//! Host firewall programming.
//!
//! The fabric expresses host-level packet filtering as an ordered set of
//! [`FirewallRule`]s and hands them to a pluggable [`FirewallBackend`] — either
//! `nftables` or legacy `iptables`. The `nftables` backend renders the ruleset
//! into a single `nft -f -` document and loads it atomically; the `iptables`
//! backend flushes and re-adds each rule with `iptables -A`. Both keep an
//! in-memory copy of the rules they last applied purely to answer the
//! [`FirewallBackend::rules`] accessor — the kernel holds the authoritative
//! state.

use crate::exec::run;
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// What a [`FirewallRule`] does to a matching packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirewallAction {
    Allow,
    Deny,
}

/// A single host firewall rule.
///
/// Fields left `None` match anything on that dimension (e.g. `src_cidr = None`
/// matches any source). `chain` names the hook the rule attaches to, mirroring
/// netfilter chains such as `"input"`, `"forward"`, or `"output"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallRule {
    pub id: Id,
    /// The chain/hook this rule attaches to (e.g. `"input"`, `"forward"`).
    pub chain: String,
    pub action: FirewallAction,
    /// Layer-4 protocol (e.g. `"tcp"`, `"udp"`, `"icmp"`); `None` = any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proto: Option<String>,
    /// Source CIDR; `None` = any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src_cidr: Option<String>,
    /// Destination CIDR; `None` = any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dst_cidr: Option<String>,
    /// Destination port; `None` = any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dport: Option<u16>,
}

impl FirewallRule {
    /// Construct a rule on `chain` with the given `action`; match dimensions are
    /// "any" until set with the builder methods.
    pub fn new(chain: impl Into<String>, action: FirewallAction) -> Self {
        FirewallRule {
            id: Id::new(),
            chain: chain.into(),
            action,
            proto: None,
            src_cidr: None,
            dst_cidr: None,
            dport: None,
        }
    }

    pub fn with_proto(mut self, proto: impl Into<String>) -> Self {
        self.proto = Some(proto.into());
        self
    }

    pub fn with_src(mut self, cidr: impl Into<String>) -> Self {
        self.src_cidr = Some(cidr.into());
        self
    }

    pub fn with_dst(mut self, cidr: impl Into<String>) -> Self {
        self.dst_cidr = Some(cidr.into());
        self
    }

    pub fn with_dport(mut self, port: u16) -> Self {
        self.dport = Some(port);
        self
    }
}

/// Pluggable host firewall backend contract.
///
/// Concrete backends translate a fabric ruleset into their native syntax and
/// program the kernel. Extends [`Provider`] so backends are registered by name
/// in a [`Registry`].
#[async_trait]
pub trait FirewallBackend: Provider {
    /// Replace the active ruleset with `rules` (declarative apply).
    async fn apply(&self, rules: &[FirewallRule]) -> Result<()>;

    /// Remove all rules this backend installed.
    async fn flush(&self) -> Result<()>;

    /// The rules currently installed by this backend, in application order.
    async fn rules(&self) -> Result<Vec<FirewallRule>>;
}

/// The dedicated nftables table the fabric owns, so a flush only touches our
/// rules and never the host's other tables.
const NFT_TABLE: &str = "ocf";

/// Map a fabric chain name onto an nftables base-chain hook.
///
/// Unknown chain names default to the `input` hook, which is the safe choice
/// for host-local filtering.
fn nft_hook_for(chain: &str) -> &'static str {
    match chain.to_ascii_lowercase().as_str() {
        "forward" => "forward",
        "output" => "output",
        _ => "input",
    }
}

/// Render the complete `nft -f` document for `rules`.
///
/// The document recreates the `inet ocf` table from scratch (delete-if-exists
/// then add), declares the base chains the rules reference, and appends one
/// rule line each. Loading it with `nft -f -` applies the whole set atomically.
fn build_nft_ruleset(rules: &[FirewallRule]) -> String {
    let mut out = String::new();
    // Recreate our table from scratch so apply is fully declarative. `nft -f`
    // tolerates the leading delete of a table that doesn't exist only when
    // wrapped like this is not guaranteed, so emit `add table` first (idempotent)
    // then `delete`/`add` to start clean.
    out.push_str(&format!("add table inet {NFT_TABLE}\n"));
    out.push_str(&format!("delete table inet {NFT_TABLE}\n"));
    out.push_str(&format!("add table inet {NFT_TABLE}\n"));

    // Declare every base chain referenced by the ruleset (deduplicated).
    let mut seen_hooks: Vec<&'static str> = Vec::new();
    for rule in rules {
        let hook = nft_hook_for(&rule.chain);
        if !seen_hooks.contains(&hook) {
            seen_hooks.push(hook);
            out.push_str(&format!(
                "add chain inet {NFT_TABLE} {hook} {{ type filter hook {hook} priority 0; policy accept; }}\n"
            ));
        }
    }

    for rule in rules {
        let hook = nft_hook_for(&rule.chain);
        let mut expr = String::new();
        if let Some(src) = &rule.src_cidr {
            expr.push_str(&format!("ip saddr {src} "));
        }
        if let Some(dst) = &rule.dst_cidr {
            expr.push_str(&format!("ip daddr {dst} "));
        }
        if let Some(proto) = &rule.proto {
            expr.push_str(&format!("{} ", proto.to_ascii_lowercase()));
            if let Some(dport) = rule.dport {
                expr.push_str(&format!("dport {dport} "));
            }
        } else if let Some(dport) = rule.dport {
            // No L4 proto given but a port is: default to tcp so the match is valid.
            expr.push_str(&format!("tcp dport {dport} "));
        }
        let verdict = match rule.action {
            FirewallAction::Allow => "accept",
            FirewallAction::Deny => "drop",
        };
        out.push_str(&format!(
            "add rule inet {NFT_TABLE} {hook} {expr}{verdict}\n"
        ));
    }

    out
}

/// Build the `iptables -A` argument vector for a single rule.
///
/// Returns the args after the leading `iptables`, e.g.
/// `["-A", "INPUT", "-p", "tcp", "--dport", "22", "-j", "ACCEPT"]`.
fn iptables_args_for(rule: &FirewallRule) -> Vec<String> {
    let mut args: Vec<String> = vec!["-A".to_string(), rule.chain.to_ascii_uppercase()];
    if let Some(proto) = &rule.proto {
        args.push("-p".to_string());
        args.push(proto.to_ascii_lowercase());
    } else if rule.dport.is_some() {
        // A port match requires a protocol; default to tcp.
        args.push("-p".to_string());
        args.push("tcp".to_string());
    }
    if let Some(src) = &rule.src_cidr {
        args.push("-s".to_string());
        args.push(src.clone());
    }
    if let Some(dst) = &rule.dst_cidr {
        args.push("-d".to_string());
        args.push(dst.clone());
    }
    if let Some(dport) = rule.dport {
        args.push("--dport".to_string());
        args.push(dport.to_string());
    }
    args.push("-j".to_string());
    args.push(match rule.action {
        FirewallAction::Allow => "ACCEPT".to_string(),
        FirewallAction::Deny => "DROP".to_string(),
    });
    args
}

/// Feed `ruleset` to `nft -f -` on stdin, mapping any failure to a provider error.
async fn nft_load(ruleset: &str) -> Result<()> {
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::provider("nft", format!("failed to spawn `nft`: {e}")))?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::provider("nft", "could not open stdin for `nft`"))?;
        stdin
            .write_all(ruleset.as_bytes())
            .await
            .map_err(|e| Error::provider("nft", format!("writing ruleset to `nft`: {e}")))?;
        // Drop closes stdin so `nft` sees EOF and processes the document.
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| Error::provider("nft", format!("waiting on `nft`: {e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::provider("nft", stderr.trim().to_string()))
    }
}

/// `nftables` firewall backend.
///
/// Renders the ruleset into a single `nft -f -` document loaded atomically into
/// the dedicated `inet ocf` table. The in-memory `applied` copy only backs the
/// [`FirewallBackend::rules`] accessor.
pub struct NftablesFirewall {
    applied: RwLock<Vec<FirewallRule>>,
}

impl NftablesFirewall {
    pub const NAME: &'static str = "nftables";

    pub fn new() -> Self {
        NftablesFirewall {
            applied: RwLock::new(Vec::new()),
        }
    }
}

impl Default for NftablesFirewall {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for NftablesFirewall {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn description(&self) -> &str {
        "nftables-based host firewall"
    }
}

#[async_trait]
impl FirewallBackend for NftablesFirewall {
    async fn apply(&self, rules: &[FirewallRule]) -> Result<()> {
        let ruleset = build_nft_ruleset(rules);
        nft_load(&ruleset).await?;
        *self.applied.write() = rules.to_vec();
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        // Drop only our table; leave any host-managed tables untouched.
        run("nft", &["delete", "table", "inet", NFT_TABLE]).await?;
        self.applied.write().clear();
        Ok(())
    }

    async fn rules(&self) -> Result<Vec<FirewallRule>> {
        Ok(self.applied.read().clone())
    }
}

/// Legacy `iptables` firewall backend.
///
/// Flushes the referenced chains and re-issues one `iptables -A` per rule. The
/// in-memory `applied` copy only backs the [`FirewallBackend::rules`] accessor.
pub struct IptablesFirewall {
    applied: RwLock<Vec<FirewallRule>>,
}

impl IptablesFirewall {
    pub const NAME: &'static str = "iptables";

    pub fn new() -> Self {
        IptablesFirewall {
            applied: RwLock::new(Vec::new()),
        }
    }
}

impl Default for IptablesFirewall {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for IptablesFirewall {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn description(&self) -> &str {
        "legacy iptables host firewall"
    }
}

#[async_trait]
impl FirewallBackend for IptablesFirewall {
    async fn apply(&self, rules: &[FirewallRule]) -> Result<()> {
        // Flush every chain the ruleset references before re-adding, so apply is
        // declarative rather than additive. Deduplicate chains first.
        let mut chains: Vec<String> = Vec::new();
        for rule in rules {
            let chain = rule.chain.to_ascii_uppercase();
            if !chains.contains(&chain) {
                chains.push(chain);
            }
        }
        for chain in &chains {
            run("iptables", &["-F", chain]).await?;
        }
        for rule in rules {
            let args = iptables_args_for(rule);
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run("iptables", &arg_refs).await?;
        }
        *self.applied.write() = rules.to_vec();
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        // Flush the chains we last applied; fall back to a global `-F` if we have
        // no record (e.g. flush before any apply).
        let applied = self.applied.read().clone();
        if applied.is_empty() {
            run("iptables", &["-F"]).await?;
        } else {
            let mut chains: Vec<String> = Vec::new();
            for rule in &applied {
                let chain = rule.chain.to_ascii_uppercase();
                if !chains.contains(&chain) {
                    chains.push(chain);
                }
            }
            for chain in &chains {
                run("iptables", &["-F", chain]).await?;
            }
        }
        self.applied.write().clear();
        Ok(())
    }

    async fn rules(&self) -> Result<Vec<FirewallRule>> {
        Ok(self.applied.read().clone())
    }
}

/// Register the built-in firewall backends (`nftables`, `iptables`).
pub fn register_builtins(reg: &mut Registry<dyn FirewallBackend>) -> Result<()> {
    reg.register(NftablesFirewall::NAME, Arc::new(NftablesFirewall::new()))?;
    reg.register(IptablesFirewall::NAME, Arc::new(IptablesFirewall::new()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nft_ruleset_recreates_table_and_emits_rule() {
        let rules = vec![FirewallRule::new("input", FirewallAction::Allow)
            .with_proto("tcp")
            .with_dport(22)];
        let doc = build_nft_ruleset(&rules);
        assert!(doc.contains("delete table inet ocf"));
        assert!(doc.contains("add table inet ocf"));
        assert!(doc.contains("hook input"));
        assert!(doc.contains("tcp dport 22 accept"));
    }

    #[test]
    fn nft_ruleset_maps_deny_to_drop_and_chains_to_hooks() {
        let rules = vec![
            FirewallRule::new("forward", FirewallAction::Deny).with_src("10.0.0.0/8"),
            FirewallRule::new("output", FirewallAction::Allow),
        ];
        let doc = build_nft_ruleset(&rules);
        assert!(doc.contains("hook forward"));
        assert!(doc.contains("hook output"));
        assert!(doc.contains("ip saddr 10.0.0.0/8 drop"));
    }

    #[test]
    fn nft_port_without_proto_defaults_to_tcp() {
        let rules = vec![FirewallRule::new("input", FirewallAction::Allow).with_dport(443)];
        let doc = build_nft_ruleset(&rules);
        assert!(doc.contains("tcp dport 443 accept"));
    }

    #[test]
    fn iptables_args_render_full_match() {
        let rule = FirewallRule::new("input", FirewallAction::Allow)
            .with_proto("tcp")
            .with_src("192.168.0.0/16")
            .with_dport(8080);
        let args = iptables_args_for(&rule);
        assert_eq!(
            args,
            vec![
                "-A", "INPUT", "-p", "tcp", "-s", "192.168.0.0/16", "--dport", "8080", "-j",
                "ACCEPT"
            ]
        );
    }

    #[test]
    fn iptables_deny_maps_to_drop_and_defaults_proto_for_port() {
        let rule = FirewallRule::new("forward", FirewallAction::Deny).with_dport(53);
        let args = iptables_args_for(&rule);
        // Port present but no proto -> tcp inserted, action DROP.
        assert_eq!(
            args,
            vec!["-A", "FORWARD", "-p", "tcp", "--dport", "53", "-j", "DROP"]
        );
    }

    #[test]
    fn register_builtins_registers_both() {
        let mut reg: Registry<dyn FirewallBackend> = Registry::new();
        register_builtins(&mut reg).unwrap();
        assert!(reg.contains(NftablesFirewall::NAME));
        assert!(reg.contains(IptablesFirewall::NAME));
    }

    // Requires a real host with nft installed and CAP_NET_ADMIN.
    #[tokio::test]
    #[ignore = "requires root + nftables"]
    async fn nftables_apply_and_flush() {
        let fw = NftablesFirewall::new();
        let rules = vec![FirewallRule::new("input", FirewallAction::Allow)
            .with_proto("tcp")
            .with_dport(22)];
        fw.apply(&rules).await.unwrap();
        assert_eq!(fw.rules().await.unwrap().len(), 1);
        fw.flush().await.unwrap();
        assert!(fw.rules().await.unwrap().is_empty());
    }
}
