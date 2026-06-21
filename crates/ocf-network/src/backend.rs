//! The dataplane contract and its pluggable backends.
//!
//! A [`NetworkBackend`] programs the SDN overlay on a *single* machine: it
//! creates the VXLAN device for a VPC, the namespace/bridge for a subnet,
//! installs routes, and renders firewall policies into the host's packet
//! filter. The high-level [`crate::controller::NetworkController`] fans an
//! operation out across every registered backend so a change "affects all
//! machines".
//!
//! Both concrete backends here shell out to the host's SDN tooling:
//! [`LinuxNetnsBackend`] drives iproute2 + nftables, and [`OvsBackend`] drives
//! Open vSwitch. Every command is issued idempotently so re-applying a resource
//! converges rather than failing. The commands obviously require a Linux host
//! with the relevant binaries and (usually) root; on any other platform, or
//! without the binaries, the underlying [`tokio::process::Command`] simply
//! fails at runtime and the error is surfaced as a [`Error::Provider`].

use crate::model::{
    AclAction, AclDirection, AclRule, AclScope, FirewallPolicy, Route, Subnet, Vpc,
};
use ocf_core::prelude::*;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Programs the SDN overlay dataplane on one machine.
///
/// Each method is idempotent by contract: applying the same resource twice is
/// expected to converge to the same dataplane state.
#[async_trait]
pub trait NetworkBackend: Provider {
    /// Create or update the overlay device backing a VPC (VXLAN tunnel).
    async fn apply_vpc(&self, vpc: &Vpc) -> Result<()>;

    /// Create or update a subnet's namespace, bridge, and addressing.
    async fn apply_subnet(&self, subnet: &Subnet) -> Result<()>;

    /// Install a static route into a subnet's routing table.
    async fn apply_route(&self, route: &Route) -> Result<()>;

    /// Render a firewall policy into the host packet filter.
    async fn apply_policy(&self, policy: &FirewallPolicy) -> Result<()>;

    /// Program a subnet's **egress** (outbound internet) data path.
    ///
    /// When the subnet's capability is [`EgressMode::Nat`] this enables IP
    /// forwarding and installs a source-NAT (masquerade) of the subnet CIDR out
    /// the host's default uplink, gated by a forward-filter chain that drops the
    /// subnet's traffic by default and accepts only `allowed_addresses` (the
    /// workloads that opted in). When the capability is [`EgressMode::Isolated`]
    /// the egress data path is torn down. Inbound connections are never handled
    /// here — that is the load balancer's job.
    ///
    /// The default implementation is a no-op, so backends that do not provide
    /// host-level NAT compose without change.
    async fn apply_egress(&self, subnet: &Subnet, allowed_addresses: &[String]) -> Result<()> {
        let _ = (subnet, allowed_addresses);
        Ok(())
    }

    /// Stitch a VPC's VXLAN overlay across hosts by programming the **remote
    /// VTEP peers** (`peer_vteps`: the underlay addresses of the *other* nodes).
    ///
    /// Without this a VXLAN device has no peers and encapsulated frames go
    /// nowhere; this is what makes the overlay actually multi-host. The default
    /// implementation is a no-op so non-VXLAN backends compose unchanged.
    async fn apply_vpc_peers(&self, vpc: &Vpc, peer_vteps: &[String]) -> Result<()> {
        let _ = (vpc, peer_vteps);
        Ok(())
    }
}

// ---- Shared command helpers ----------------------------------------------

/// Fragments that a tool prints to stderr when an object already exists. These
/// are treated as success so every `apply_*` is idempotent: re-running it
/// converges instead of erroring on the second pass. Kept specific (each
/// fragment ends in "exists") so an unrelated "does not exist" error is not
/// swallowed.
const IDEMPOTENT_MARKERS: &[&str] = &["file exists", "already exists"];

/// True when `stderr` only complains that the object we were creating is
/// already present — which, for an idempotent apply, is the desired state.
fn is_idempotent_stderr(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    IDEMPOTENT_MARKERS.iter().any(|m| lower.contains(m))
}

/// Run `cmd args...`, capturing output. Success (exit 0) returns `Ok`; an
/// "already exists" failure is treated as success for idempotency; any other
/// failure becomes [`Error::Provider`] tagged with `cmd` and the stderr text.
async fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| Error::provider(cmd, format!("failed to spawn `{cmd}`: {e}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if is_idempotent_stderr(&stderr) {
        return Ok(());
    }
    Err(Error::provider(cmd, stderr.trim().to_string()))
}

/// Like [`run`], but feeds `stdin` to the child's standard input. Used for
/// `nft -f -`, which reads a ruleset from stdin and swaps it in atomically.
async fn run_stdin(cmd: &str, args: &[&str], stdin: &str) -> Result<()> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::provider(cmd, format!("failed to spawn `{cmd}`: {e}")))?;

    if let Some(mut sink) = child.stdin.take() {
        sink.write_all(stdin.as_bytes())
            .await
            .map_err(|e| Error::provider(cmd, format!("failed to write stdin: {e}")))?;
        // Drop closes the pipe so the child sees EOF and can finish reading.
        drop(sink);
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| Error::provider(cmd, format!("failed to wait on `{cmd}`: {e}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if is_idempotent_stderr(&stderr) {
        return Ok(());
    }
    Err(Error::provider(cmd, stderr.trim().to_string()))
}

/// First 8 characters of an id, for building Linux interface names. Linux caps
/// `IFNAMSIZ` at 15 bytes, so e.g. `br-<8>` (11 bytes) stays comfortably under.
fn short_id(id: &Id) -> String {
    id.as_str().chars().take(8).collect()
}

/// Derive a usable gateway host address for a CIDR by taking the network's
/// first host (`.1`). Returns `None` if the CIDR is not a parseable IPv4 `a.b.c.d/p`
/// — in which case the caller skips address assignment rather than guessing.
///
/// Kept deliberately simple (std-only, no IP-parsing crate): split on `/`,
/// validate four octets, replace the final octet with `1`, and re-attach the
/// prefix length.
fn gateway_from_cidr(cidr: &str) -> Option<String> {
    let (addr, prefix) = cidr.split_once('/')?;
    let octets: Vec<&str> = addr.split('.').collect();
    if octets.len() != 4 {
        return None;
    }
    for octet in &octets {
        // Each octet must parse as a u8 (0-255) for this to be a valid IPv4.
        octet.parse::<u8>().ok()?;
    }
    let prefix_len: u8 = prefix.parse().ok()?;
    if prefix_len > 32 {
        return None;
    }
    Some(format!(
        "{}.{}.{}.1/{}",
        octets[0], octets[1], octets[2], prefix_len
    ))
}

// ---- Linux netns / iproute2 / nftables backend ---------------------------

/// Backend driving the Linux networking stack directly: `ip netns`, `ip link`
/// (VXLAN), bridges, `ip route`, and `nft`.
///
/// This is the default single-host backend, programmed entirely through
/// iproute2 and nftables. It requires a Linux host and (in practice) root.
#[derive(Debug, Default)]
pub struct LinuxNetnsBackend;

impl LinuxNetnsBackend {
    pub fn new() -> Self {
        LinuxNetnsBackend
    }
}

impl Provider for LinuxNetnsBackend {
    fn name(&self) -> &str {
        "linux-netns"
    }
    fn description(&self) -> &str {
        "Linux network-namespace + VXLAN overlay backend (iproute2/netlink)"
    }
}

#[async_trait]
impl NetworkBackend for LinuxNetnsBackend {
    async fn apply_vpc(&self, vpc: &Vpc) -> Result<()> {
        let dev = format!("vxlan{}", vpc.vni);
        let vni = vpc.vni.to_string();
        tracing::info!(
            backend = self.name(),
            vpc = %vpc.metadata.name,
            vni = vpc.vni,
            cidr = %vpc.cidr,
            "creating VXLAN device for VPC"
        );
        // `ip link add vxlan{vni} type vxlan id {vni} dstport 4789 nolearning`
        // Re-running yields "File exists", which `run` treats as success.
        run(
            "ip",
            &[
                "link", "add", &dev, "type", "vxlan", "id", &vni, "dstport", "4789", "nolearning",
            ],
        )
        .await?;
        // `ip link set vxlan{vni} up`
        run("ip", &["link", "set", &dev, "up"]).await?;
        Ok(())
    }

    async fn apply_subnet(&self, subnet: &Subnet) -> Result<()> {
        let bridge = format!("br-{}", short_id(&subnet.metadata.id));
        tracing::info!(
            backend = self.name(),
            subnet = %subnet.metadata.name,
            netns = %subnet.netns,
            cidr = %subnet.cidr,
            bridge = %bridge,
            "creating netns + bridge for subnet"
        );
        // `ip netns add {netns}` (idempotent: "File exists" -> success)
        run("ip", &["netns", "add", &subnet.netns]).await?;
        // `ip link add br-{shortid} type bridge`
        run("ip", &["link", "add", &bridge, "type", "bridge"]).await?;
        // `ip link set br-{shortid} up`
        run("ip", &["link", "set", &bridge, "up"]).await?;
        // Assign the subnet gateway address (first host of the CIDR) to the
        // bridge, when the CIDR is a parseable IPv4 prefix.
        if let Some(gw) = gateway_from_cidr(&subnet.cidr) {
            // `ip addr add {gw} dev br-{shortid}` (idempotent on re-apply)
            run("ip", &["addr", "add", &gw, "dev", &bridge]).await?;
        }
        Ok(())
    }

    async fn apply_route(&self, route: &Route) -> Result<()> {
        tracing::info!(
            backend = self.name(),
            subnet = %route.subnet_id,
            dest = %route.dest_cidr,
            next_hop = %route.next_hop,
            "installing route"
        );
        // `ip route replace {dest_cidr} via {next_hop}` — `replace` is
        // inherently idempotent (creates or updates the entry).
        //
        // The Route model has no netns handle, so the route is installed in the
        // host's main table. (When a netns context is available it would be
        // `ip netns exec {netns} ip route replace ...`.)
        run(
            "ip",
            &["route", "replace", &route.dest_cidr, "via", &route.next_hop],
        )
        .await?;
        Ok(())
    }

    async fn apply_policy(&self, policy: &FirewallPolicy) -> Result<()> {
        tracing::info!(
            backend = self.name(),
            policy = %policy.id,
            rule_count = policy.rules.len(),
            "rendering firewall policy into nftables"
        );
        // Build a single self-contained ruleset and swap it in atomically via
        // `nft -f -`. The table is flushed first so re-applying a policy is
        // idempotent (it fully replaces the previous render of this policy).
        let ruleset = render_nftables(policy);
        run_stdin("nft", &["-f", "-"], &ruleset).await?;
        Ok(())
    }

    async fn apply_egress(&self, subnet: &Subnet, allowed_addresses: &[String]) -> Result<()> {
        program_host_egress(self.name(), subnet, allowed_addresses).await
    }

    async fn apply_vpc_peers(&self, vpc: &Vpc, peer_vteps: &[String]) -> Result<()> {
        let dev = format!("vxlan{}", vpc.vni);
        tracing::info!(
            backend = self.name(),
            vpc = %vpc.metadata.name,
            dev = %dev,
            peers = peer_vteps.len(),
            "programming VXLAN remote VTEP peers (FDB head-end replication)"
        );
        for peer in peer_vteps {
            // Head-end replication: an all-zeros (BUM) FDB entry per remote VTEP
            // floods broadcast/unknown-unicast to every peer. Delete-then-append
            // makes re-applying converge instead of accumulating duplicates.
            let del = vxlan_fdb_args("del", &dev, peer);
            let del_args: Vec<&str> = del.iter().map(String::as_str).collect();
            let _ = run("bridge", &del_args).await; // ignore "not found" on first apply
            let add = vxlan_fdb_args("append", &dev, peer);
            let add_args: Vec<&str> = add.iter().map(String::as_str).collect();
            run("bridge", &add_args).await?;
        }
        Ok(())
    }
}

/// Build the `bridge fdb <op> 00:00:00:00:00:00 dev <dev> dst <peer>` argv for a
/// VXLAN head-end-replication flood entry. `op` is `append` or `del`.
fn vxlan_fdb_args(op: &str, dev: &str, peer: &str) -> Vec<String> {
    vec![
        "fdb".into(),
        op.into(),
        "00:00:00:00:00:00".into(),
        "dev".into(),
        dev.into(),
        "dst".into(),
        peer.into(),
    ]
}

/// Program (or tear down) a subnet's egress NAT on the host via nftables.
///
/// Shared by both backends: source NAT is a host-netfilter concern, the same
/// whether the L2 dataplane is a Linux bridge or OVS. When the subnet is
/// [`EgressMode::Nat`] this:
///
/// 1. enables IPv4 forwarding (`/proc/sys/net/ipv4/ip_forward`),
/// 2. resolves the host's default uplink interface (`ip route show default`),
/// 3. installs an `inet ocf_egr_<id>` table with a forward-filter chain
///    (default-deny the subnet, accept only `allowed`) and a postrouting
///    masquerade of the subnet CIDR out the uplink.
///
/// When the subnet is [`EgressMode::Isolated`] the table is removed.
async fn program_host_egress(
    backend: &str,
    subnet: &Subnet,
    allowed: &[String],
) -> Result<()> {
    let table = egress_table_name(subnet);

    if !subnet.egress.provides_egress() {
        tracing::info!(backend, subnet = %subnet.metadata.name, "tearing down subnet egress");
        // add-then-delete is idempotent whether or not the table existed.
        run_stdin("nft", &["-f", "-"], &render_egress_teardown(&table)).await?;
        return Ok(());
    }

    // 1. Forwarding must be on for the forward hook to route subnet traffic.
    enable_ip_forwarding()?;

    // 2. The uplink is the device on the host's default route.
    let uplink = default_uplink().await?;

    tracing::info!(
        backend,
        subnet = %subnet.metadata.name,
        cidr = %subnet.cidr,
        %uplink,
        allowed = allowed.len(),
        "programming subnet egress NAT"
    );

    // 3. Install the masquerade + opt-in forward filter atomically.
    let script = render_egress_nftables(subnet, allowed, &uplink, &table);
    run_stdin("nft", &["-f", "-"], &script).await?;
    Ok(())
}

/// Enable IPv4 forwarding by writing `/proc/sys/net/ipv4/ip_forward`. A no-op if
/// it is already `1`. Errors (e.g. not Linux, or not permitted) surface as a
/// provider error since NAT cannot route without forwarding.
fn enable_ip_forwarding() -> Result<()> {
    const PATH: &str = "/proc/sys/net/ipv4/ip_forward";
    if let Ok(cur) = std::fs::read_to_string(PATH) {
        if cur.trim() == "1" {
            return Ok(());
        }
    }
    std::fs::write(PATH, "1")
        .map_err(|e| Error::provider("ip_forward", format!("enable forwarding: {e}")))
}

/// Resolve the host's default-route uplink interface via `ip route show default`.
async fn default_uplink() -> Result<String> {
    let out = run_capture("ip", &["route", "show", "default"]).await?;
    parse_default_uplink(&out)
        .ok_or_else(|| Error::provider("ip", "no default-route uplink found".to_string()))
}

/// Stable nftables table name for a subnet's egress data path. Sanitized to the
/// nft identifier charset (alphanumerics + `_`).
fn egress_table_name(subnet: &Subnet) -> String {
    let sanitized: String = short_id(&subnet.metadata.id)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("ocf_egr_{sanitized}")
}

/// Parse the `dev <iface>` interface out of `ip route show default` output.
///
/// Example input `default via 10.0.0.1 dev eth0 proto dhcp metric 100` → `eth0`.
/// Returns the first default route's device, or `None` if absent.
fn parse_default_uplink(ip_route_output: &str) -> Option<String> {
    for line in ip_route_output.lines() {
        let line = line.trim();
        if !line.starts_with("default") {
            continue;
        }
        let mut toks = line.split_whitespace();
        while let Some(tok) = toks.next() {
            if tok == "dev" {
                return toks.next().map(|s| s.to_string());
            }
        }
    }
    None
}

/// Render the teardown script that removes a subnet's egress table.
fn render_egress_teardown(table: &str) -> String {
    format!("add table inet {table}\ndelete table inet {table}\n")
}

/// Render a subnet's egress NAT + opt-in filter as an `nft -f -` script.
///
/// The table holds two chains:
/// * `forward` (filter) — accepts established/related return traffic, accepts
///   each opted-in `allowed` source address, then **drops** the rest of the
///   subnet CIDR. Non-subnet traffic falls through the default `accept`.
/// * `postrouting` (nat) — masquerades the subnet CIDR out `uplink`. Only
///   traffic the forward chain accepted ever reaches it.
fn render_egress_nftables(
    subnet: &Subnet,
    allowed: &[String],
    uplink: &str,
    table: &str,
) -> String {
    let cidr = &subnet.cidr;
    let mut out = String::new();
    out.push_str(&format!("add table inet {table}\n"));
    out.push_str(&format!("delete table inet {table}\n"));
    out.push_str(&format!("table inet {table} {{\n"));

    // Forward filter: default-deny the subnet, allow only opted-in addresses.
    out.push_str("  chain forward {\n");
    out.push_str("    type filter hook forward priority filter; policy accept;\n");
    out.push_str("    ct state established,related accept\n");
    for addr in allowed {
        out.push_str(&format!("    ip saddr {addr} accept\n"));
    }
    out.push_str(&format!("    ip saddr {cidr} drop\n"));
    out.push_str("  }\n");

    // Source NAT: masquerade the subnet out the uplink.
    out.push_str("  chain postrouting {\n");
    out.push_str("    type nat hook postrouting priority srcnat; policy accept;\n");
    out.push_str(&format!(
        "    ip saddr {cidr} oifname \"{uplink}\" masquerade\n"
    ));
    out.push_str("  }\n");

    out.push_str("}\n");
    out
}

/// Run `cmd args...` and return captured stdout on success.
async fn run_capture(cmd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| Error::provider(cmd, format!("failed to spawn `{cmd}`: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::provider(cmd, stderr.trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Stable nftables table name for a policy: scoped by VPC/subnet so each
/// policy's render is isolated from every other policy's.
///
/// nftables identifiers allow only alphanumerics and underscores, so the short
/// id is sanitized — any other character (e.g. a UUID/name hyphen) becomes `_`.
fn nft_table_name(policy: &FirewallPolicy) -> String {
    let (scope, id) = match &policy.scope {
        AclScope::Vpc(id) => ("vpc", id),
        AclScope::Subnet(id) => ("subnet", id),
    };
    let sanitized: String = short_id(id)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("ocf_{scope}_{sanitized}")
}

/// nftables hook for a direction: ingress traffic is filtered on `input`,
/// egress on `output`.
fn nft_hook(direction: AclDirection) -> &'static str {
    match direction {
        AclDirection::Ingress => "input",
        AclDirection::Egress => "output",
    }
}

/// nftables `saddr`/`daddr` keyword for a direction: ingress matches the remote
/// *source* address, egress the remote *destination*.
fn nft_addr_keyword(direction: AclDirection) -> &'static str {
    match direction {
        AclDirection::Ingress => "saddr",
        AclDirection::Egress => "daddr",
    }
}

/// Render a single [`AclRule`] as one nftables rule line (no leading indent).
///
/// Example: a Deny ingress tcp from `0.0.0.0/0` port 22 renders as
/// `ip saddr 0.0.0.0/0 tcp dport 22 drop`. A `cidr` of `0.0.0.0/0` and a proto
/// of `any` are treated as wildcards and omitted from the match.
fn nft_rule_line(rule: &AclRule) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Address match (skip the all-addresses wildcard).
    if rule.cidr != "0.0.0.0/0" && !rule.cidr.is_empty() {
        parts.push(format!("ip {} {}", nft_addr_keyword(rule.direction), rule.cidr));
    }

    // Protocol + optional port match. `any` proto is a wildcard.
    let proto = rule.proto.to_ascii_lowercase();
    let is_l4 = proto == "tcp" || proto == "udp";
    if proto != "any" && !proto.is_empty() {
        if is_l4 {
            // For tcp/udp emit the L4 keyword so a port match can attach to it.
            if let Some(port) = rule.port {
                parts.push(format!("{proto} dport {port}"));
            } else {
                parts.push(format!("ip protocol {proto}"));
            }
        } else {
            // icmp and friends: match on the protocol field.
            parts.push(format!("ip protocol {proto}"));
        }
    } else if let Some(port) = rule.port {
        // Port specified without a concrete L4 proto: default to tcp.
        parts.push(format!("tcp dport {port}"));
    }

    let verdict = match rule.action {
        AclAction::Allow => "accept",
        AclAction::Deny => "drop",
    };
    parts.push(verdict.to_string());
    parts.join(" ")
}

/// Render a whole [`FirewallPolicy`] into a `nft -f -` script.
///
/// The script uses nftables' atomic replace idiom: `add table` (a no-op if it
/// already exists, but guarantees the following `delete` has a target), then
/// `delete table` to clear any prior render, then a literal `table { … }` block
/// recreating it with one filter chain per direction in use and the policy's
/// rules in order. Feeding the whole script to `nft -f -` applies it atomically,
/// so re-applying a policy converges rather than erroring.
fn render_nftables(policy: &FirewallPolicy) -> String {
    let table = nft_table_name(policy);
    let mut out = String::new();

    // Ensure the table exists, then delete it, so the recreate below is a clean
    // replace whether or not a previous render was present.
    out.push_str(&format!("add table inet {table}\n"));
    out.push_str(&format!("delete table inet {table}\n"));
    out.push_str(&format!("table inet {table} {{\n"));

    // A chain is emitted only for directions the policy actually uses, each as
    // a base filter chain with a default `accept` policy (rules add drops).
    let needs_input = policy
        .rules
        .iter()
        .any(|r| r.direction == AclDirection::Ingress);
    let needs_output = policy
        .rules
        .iter()
        .any(|r| r.direction == AclDirection::Egress);

    for (direction, needed) in [
        (AclDirection::Ingress, needs_input),
        (AclDirection::Egress, needs_output),
    ] {
        if !needed {
            continue;
        }
        let hook = nft_hook(direction);
        out.push_str(&format!("  chain {hook} {{\n"));
        out.push_str(&format!(
            "    type filter hook {hook} priority 0; policy accept;\n"
        ));
        for rule in policy.rules.iter().filter(|r| r.direction == direction) {
            out.push_str(&format!("    {}\n", nft_rule_line(rule)));
        }
        out.push_str("  }\n");
    }

    out.push_str("}\n");
    out
}

// ---- Open vSwitch backend ------------------------------------------------

/// Backend targeting Open vSwitch (OVS) for the overlay dataplane.
///
/// OVS is preferred at scale (programmable flows, OpenFlow). Bridges and VXLAN
/// ports are managed with `ovs-vsctl`; routes and ACLs are programmed as
/// OpenFlow flows with `ovs-ofctl`.
#[derive(Debug, Default)]
pub struct OvsBackend;

impl OvsBackend {
    pub fn new() -> Self {
        OvsBackend
    }
}

impl Provider for OvsBackend {
    fn name(&self) -> &str {
        "ovs"
    }
    fn description(&self) -> &str {
        "Open vSwitch overlay backend (OpenFlow-programmed VXLAN)"
    }
}

#[async_trait]
impl NetworkBackend for OvsBackend {
    async fn apply_vpc(&self, vpc: &Vpc) -> Result<()> {
        let bridge = format!("ovs-{}", vpc.vni);
        let port = format!("vxlan{}", vpc.vni);
        tracing::info!(
            backend = self.name(),
            vpc = %vpc.metadata.name,
            vni = vpc.vni,
            bridge = %bridge,
            "adding OVS bridge + VXLAN port for VPC"
        );
        // `ovs-vsctl --may-exist add-br ovs-{vni}` (idempotent)
        run("ovs-vsctl", &["--may-exist", "add-br", &bridge]).await?;
        // `ovs-vsctl --may-exist add-port ovs-{vni} vxlan{vni} \
        //    -- set interface vxlan{vni} type=vxlan options:key={vni} options:remote_ip=flow`
        let key_opt = format!("options:key={}", vpc.vni);
        run(
            "ovs-vsctl",
            &[
                "--may-exist",
                "add-port",
                &bridge,
                &port,
                "--",
                "set",
                "interface",
                &port,
                "type=vxlan",
                &key_opt,
                "options:remote_ip=flow",
            ],
        )
        .await?;
        Ok(())
    }

    async fn apply_subnet(&self, subnet: &Subnet) -> Result<()> {
        // The Subnet model carries no VNI, so the OVS bridge it belongs to
        // cannot be derived here directly. Best-effort: add an internal port
        // named for the subnet's short id onto the integration bridge `br-int`,
        // which is the conventional OVS tenant bridge.
        let bridge = "br-int";
        let port = format!("ovs-{}", short_id(&subnet.metadata.id));
        tracing::info!(
            backend = self.name(),
            subnet = %subnet.metadata.name,
            cidr = %subnet.cidr,
            bridge = bridge,
            port = %port,
            "adding OVS internal port for subnet"
        );
        // Ensure the integration bridge exists, then add the internal port.
        run("ovs-vsctl", &["--may-exist", "add-br", bridge]).await?;
        // `ovs-vsctl --may-exist add-port br-int {port} \
        //    -- set interface {port} type=internal`
        run(
            "ovs-vsctl",
            &[
                "--may-exist",
                "add-port",
                bridge,
                &port,
                "--",
                "set",
                "interface",
                &port,
                "type=internal",
            ],
        )
        .await?;
        Ok(())
    }

    async fn apply_route(&self, route: &Route) -> Result<()> {
        let bridge = "br-int";
        tracing::info!(
            backend = self.name(),
            dest = %route.dest_cidr,
            next_hop = %route.next_hop,
            bridge = bridge,
            "programming OVS flow for route"
        );
        // `ovs-ofctl add-flow br-int "<cookie>,ip,nw_dst=<dest>,actions=..."`.
        // Traffic to `dest_cidr` is steered to the next hop: rewrite its source
        // to the gateway and hand off to OVS `normal` L2/L3 forwarding, which
        // resolves the next hop via the bridge's MAC table. The cookie encodes
        // the next hop so re-applying with the same hop replaces the flow
        // (idempotent) while a changed hop is distinguishable.
        let flow = format!(
            "cookie={},priority=100,ip,nw_dst={},actions=mod_nw_src:{},normal",
            next_hop_cookie(&route.next_hop),
            route.dest_cidr,
            route.next_hop
        );
        run("ovs-ofctl", &["add-flow", bridge, &flow]).await?;
        Ok(())
    }

    async fn apply_policy(&self, policy: &FirewallPolicy) -> Result<()> {
        let bridge = "br-int";
        tracing::info!(
            backend = self.name(),
            policy = %policy.id,
            rule_count = policy.rules.len(),
            bridge = bridge,
            "programming OVS flows for firewall policy"
        );
        // One add-flow per rule, in declaration order, descending priority so
        // earlier rules win. `add-flow` replaces a flow with the same match, so
        // re-applying the policy converges.
        let base_priority: u32 = 1000;
        for (i, rule) in policy.rules.iter().enumerate() {
            let priority = base_priority.saturating_sub(i as u32);
            let flow = ovs_flow_line(rule, priority);
            run("ovs-ofctl", &["add-flow", bridge, &flow]).await?;
        }
        Ok(())
    }

    async fn apply_egress(&self, subnet: &Subnet, allowed_addresses: &[String]) -> Result<()> {
        // Source NAT to the uplink is a host-netfilter concern, identical whether
        // the L2 dataplane is a Linux bridge or OVS — so the egress path is the
        // same nftables masquerade as the netns backend.
        program_host_egress(self.name(), subnet, allowed_addresses).await
    }

    async fn apply_vpc_peers(&self, vpc: &Vpc, peer_vteps: &[String]) -> Result<()> {
        let bridge = format!("ovs-{}", vpc.vni);
        let key_opt = format!("options:key={}", vpc.vni);
        tracing::info!(
            backend = self.name(),
            vpc = %vpc.metadata.name,
            bridge = %bridge,
            peers = peer_vteps.len(),
            "adding OVS VXLAN tunnel ports for remote VTEP peers"
        );
        for peer in peer_vteps {
            // One concrete-remote VXLAN port per peer forms the mesh. `--may-exist`
            // keeps it idempotent.
            let port = format!("vx{}-{}", vpc.vni, ovs_peer_suffix(peer));
            let remote_opt = format!("options:remote_ip={peer}");
            run(
                "ovs-vsctl",
                &[
                    "--may-exist",
                    "add-port",
                    &bridge,
                    &port,
                    "--",
                    "set",
                    "interface",
                    &port,
                    "type=vxlan",
                    &key_opt,
                    &remote_opt,
                ],
            )
            .await?;
        }
        Ok(())
    }
}

/// A short, interface-name-safe suffix derived from a peer VTEP address (its
/// dotted quad with `.` → `_`), for naming per-peer OVS tunnel ports.
fn ovs_peer_suffix(peer: &str) -> String {
    let sanitized: String = peer
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    sanitized.chars().take(10).collect()
}

/// Derive a stable OpenFlow cookie (a hex literal) from a next-hop address.
///
/// OVS cookies must be numeric, so the address string is folded into a 64-bit
/// value with FNV-1a (std-only, no hashing crate) and rendered as `0x…`. A
/// given next hop always maps to the same cookie, which keeps the route flow
/// idempotent across re-applies.
fn next_hop_cookie(next_hop: &str) -> String {
    // FNV-1a 64-bit.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in next_hop.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("0x{hash:x}")
}

/// Render an [`AclRule`] as an `ovs-ofctl add-flow` flow specification.
///
/// Example: a Deny ingress tcp from `10.0.0.0/8` port 22 at priority 1000 ->
/// `priority=1000,ip,nw_src=10.0.0.0/8,tcp,tp_dst=22,actions=drop`. A wildcard
/// `cidr` (`0.0.0.0/0`) or `any` proto drops that match field.
fn ovs_flow_line(rule: &AclRule, priority: u32) -> String {
    let mut matches: Vec<String> = vec![format!("priority={priority}"), "ip".to_string()];

    // Address match keyed by direction (ingress=source, egress=destination).
    if rule.cidr != "0.0.0.0/0" && !rule.cidr.is_empty() {
        let key = match rule.direction {
            AclDirection::Ingress => "nw_src",
            AclDirection::Egress => "nw_dst",
        };
        matches.push(format!("{key}={}", rule.cidr));
    }

    // Protocol + optional L4 port.
    let proto = rule.proto.to_ascii_lowercase();
    let is_l4 = proto == "tcp" || proto == "udp";
    if proto != "any" && !proto.is_empty() {
        matches.push(proto.clone());
        if is_l4 {
            if let Some(port) = rule.port {
                matches.push(format!("tp_dst={port}"));
            }
        }
    } else if let Some(port) = rule.port {
        // Port without a concrete proto: default to tcp.
        matches.push("tcp".to_string());
        matches.push(format!("tp_dst={port}"));
    }

    let action = match rule.action {
        AclAction::Allow => "normal",
        AclAction::Deny => "drop",
    };
    format!("{},actions={action}", matches.join(","))
}

/// Register the built-in dataplane backends into `reg`.
pub fn register_builtins(reg: &mut Registry<dyn NetworkBackend>) -> Result<()> {
    reg.register("linux-netns", Arc::new(LinuxNetnsBackend::new()))?;
    reg.register("ovs", Arc::new(OvsBackend::new()))?;
    Ok(())
}

// ---- Test-only no-op backend ---------------------------------------------

/// A backend that programs nothing and always succeeds.
///
/// Used by the controller's CRUD/integrity unit tests so they exercise the
/// in-memory state machine and fan-out plumbing *without* requiring root, a
/// Linux host, or the iproute2/OVS binaries. The real backends shell out to
/// host tooling that is unavailable on a dev box, so the tests register this
/// instead via [`register_null`].
#[cfg(test)]
#[derive(Debug, Default)]
pub struct NullBackend;

#[cfg(test)]
impl NullBackend {
    pub fn new() -> Self {
        NullBackend
    }
}

#[cfg(test)]
impl Provider for NullBackend {
    fn name(&self) -> &str {
        "null"
    }
    fn description(&self) -> &str {
        "No-op test backend (programs nothing)"
    }
}

#[cfg(test)]
#[async_trait]
impl NetworkBackend for NullBackend {
    async fn apply_vpc(&self, _vpc: &Vpc) -> Result<()> {
        Ok(())
    }
    async fn apply_subnet(&self, _subnet: &Subnet) -> Result<()> {
        Ok(())
    }
    async fn apply_route(&self, _route: &Route) -> Result<()> {
        Ok(())
    }
    async fn apply_policy(&self, _policy: &FirewallPolicy) -> Result<()> {
        Ok(())
    }
}

/// Register the test-only [`NullBackend`] under `"null"`.
#[cfg(test)]
pub fn register_null(reg: &mut Registry<dyn NetworkBackend>) -> Result<()> {
    reg.register("null", Arc::new(NullBackend::new()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EgressMode;

    #[test]
    fn short_id_truncates_to_eight_chars() {
        let id = Id::named("0123456789abcdef");
        assert_eq!(short_id(&id), "01234567");
        // Interface name `br-<short>` must fit Linux's 15-byte IFNAMSIZ limit.
        assert!(format!("br-{}", short_id(&id)).len() <= 15);
    }

    #[test]
    fn short_id_handles_short_input() {
        let id = Id::named("abc");
        assert_eq!(short_id(&id), "abc");
    }

    #[test]
    fn gateway_from_cidr_derives_first_host() {
        assert_eq!(
            gateway_from_cidr("10.0.1.0/24").as_deref(),
            Some("10.0.1.1/24")
        );
        assert_eq!(
            gateway_from_cidr("192.168.5.0/26").as_deref(),
            Some("192.168.5.1/26")
        );
    }

    #[test]
    fn gateway_from_cidr_rejects_garbage() {
        assert_eq!(gateway_from_cidr("not-a-cidr"), None);
        assert_eq!(gateway_from_cidr("10.0.0.0"), None);
        assert_eq!(gateway_from_cidr("10.0.0.0/40"), None);
        assert_eq!(gateway_from_cidr("10.0.0.999/24"), None);
        assert_eq!(gateway_from_cidr("10.0.0/24"), None);
    }

    #[test]
    fn idempotent_stderr_recognizes_exists() {
        assert!(is_idempotent_stderr(
            "RTNETLINK answers: File exists"
        ));
        assert!(is_idempotent_stderr("ovs-vsctl: bridge already exists"));
        assert!(!is_idempotent_stderr("Operation not permitted"));
    }

    #[test]
    fn nft_rule_line_deny_ingress_tcp_port() {
        let rule = AclRule::new(
            AclAction::Deny,
            AclDirection::Ingress,
            "tcp",
            "10.0.0.0/8",
            Some(22),
        );
        assert_eq!(
            nft_rule_line(&rule),
            "ip saddr 10.0.0.0/8 tcp dport 22 drop"
        );
    }

    #[test]
    fn nft_rule_line_allow_egress_any_proto_any_addr() {
        let rule = AclRule::new(
            AclAction::Allow,
            AclDirection::Egress,
            "any",
            "0.0.0.0/0",
            None,
        );
        // Wildcard address + any proto + no port => bare verdict.
        assert_eq!(nft_rule_line(&rule), "accept");
    }

    #[test]
    fn nft_rule_line_icmp_uses_protocol_match() {
        let rule = AclRule::new(
            AclAction::Allow,
            AclDirection::Ingress,
            "icmp",
            "10.1.0.0/16",
            None,
        );
        assert_eq!(
            nft_rule_line(&rule),
            "ip saddr 10.1.0.0/16 ip protocol icmp accept"
        );
    }

    #[test]
    fn render_nftables_scopes_table_and_chains() {
        let policy = FirewallPolicy::new(AclScope::Vpc(Id::named("vpc-abcdefgh-extra")))
            .with_rule(AclRule::new(
                AclAction::Deny,
                AclDirection::Ingress,
                "tcp",
                "0.0.0.0/0",
                Some(22),
            ))
            .with_rule(AclRule::new(
                AclAction::Allow,
                AclDirection::Egress,
                "any",
                "0.0.0.0/0",
                None,
            ));
        let script = render_nftables(&policy);
        // Table name is scoped by the (short, sanitized) VPC id: the first 8
        // chars are "vpc-abcd", and the hyphen is sanitized to an underscore.
        assert!(script.contains("table inet ocf_vpc_vpc_abcd {"));
        // Both an input chain (ingress rule) and an output chain (egress rule).
        assert!(script.contains("hook input"));
        assert!(script.contains("hook output"));
        assert!(script.contains("tcp dport 22 drop"));
        // The atomic-replace idiom: `add table` then `delete table` first, so a
        // re-apply cleanly replaces the prior render (idempotent).
        assert!(script.starts_with("add table inet ocf_vpc_vpc_abcd\n"));
        assert!(script.contains("delete table inet ocf_vpc_vpc_abcd\n"));
    }

    #[test]
    fn render_nftables_omits_unused_direction() {
        // Only ingress rules => no output chain emitted.
        let policy = FirewallPolicy::new(AclScope::Subnet(Id::named("subnet-1")))
            .with_rule(AclRule::new(
                AclAction::Deny,
                AclDirection::Ingress,
                "tcp",
                "0.0.0.0/0",
                Some(80),
            ));
        let script = render_nftables(&policy);
        assert!(script.contains("hook input"));
        assert!(!script.contains("hook output"));
    }

    #[test]
    fn ovs_flow_line_deny_ingress_tcp() {
        let rule = AclRule::new(
            AclAction::Deny,
            AclDirection::Ingress,
            "tcp",
            "10.0.0.0/8",
            Some(22),
        );
        assert_eq!(
            ovs_flow_line(&rule, 1000),
            "priority=1000,ip,nw_src=10.0.0.0/8,tcp,tp_dst=22,actions=drop"
        );
    }

    #[test]
    fn ovs_flow_line_allow_egress_wildcard() {
        let rule = AclRule::new(
            AclAction::Allow,
            AclDirection::Egress,
            "any",
            "0.0.0.0/0",
            None,
        );
        assert_eq!(ovs_flow_line(&rule, 900), "priority=900,ip,actions=normal");
    }

    #[test]
    fn next_hop_cookie_is_stable_and_hex() {
        let a = next_hop_cookie("10.0.1.1");
        let b = next_hop_cookie("10.0.1.1");
        let c = next_hop_cookie("10.0.1.2");
        assert_eq!(a, b, "same next hop must yield the same cookie");
        assert_ne!(a, c, "different next hops should differ");
        assert!(a.starts_with("0x"));
        assert!(a[2..].chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    fn nat_subnet() -> Subnet {
        Subnet::new(Id::named("vpc-a"), "web", "10.0.1.0/24", "ns-web")
            .with_egress(EgressMode::Nat)
    }

    #[test]
    fn parse_default_uplink_extracts_dev() {
        let out = "default via 10.0.0.1 dev eth0 proto dhcp metric 100\n\
                   10.0.1.0/24 dev br-web proto kernel scope link";
        assert_eq!(parse_default_uplink(out).as_deref(), Some("eth0"));
    }

    #[test]
    fn parse_default_uplink_none_without_default_route() {
        let out = "10.0.1.0/24 dev br-web proto kernel scope link";
        assert_eq!(parse_default_uplink(out), None);
        assert_eq!(parse_default_uplink(""), None);
    }

    #[test]
    fn egress_table_name_is_sanitized_and_stable() {
        let s = nat_subnet();
        let t = egress_table_name(&s);
        assert!(t.starts_with("ocf_egr_"));
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        assert_eq!(t, egress_table_name(&s), "must be stable for a subnet");
    }

    #[test]
    fn render_egress_nftables_masquerades_and_gates_opt_in() {
        let subnet = nat_subnet();
        let allowed = vec!["10.0.1.5".to_string(), "10.0.1.6".to_string()];
        let table = egress_table_name(&subnet);
        let script = render_egress_nftables(&subnet, &allowed, "eth0", &table);

        // Atomic replace idiom.
        assert!(script.contains(&format!("add table inet {table}")));
        assert!(script.contains(&format!("delete table inet {table}")));
        // Masquerade of the subnet CIDR out the uplink.
        assert!(script.contains("type nat hook postrouting priority srcnat"));
        assert!(script.contains("ip saddr 10.0.1.0/24 oifname \"eth0\" masquerade"));
        // Opt-in: established/related + each allowed addr accepted, rest dropped.
        assert!(script.contains("ct state established,related accept"));
        assert!(script.contains("ip saddr 10.0.1.5 accept"));
        assert!(script.contains("ip saddr 10.0.1.6 accept"));
        assert!(script.contains("ip saddr 10.0.1.0/24 drop"));
    }

    #[test]
    fn render_egress_nftables_default_deny_with_no_allowed() {
        let subnet = nat_subnet();
        let table = egress_table_name(&subnet);
        let script = render_egress_nftables(&subnet, &[], "eth0", &table);
        // With nobody opted in, the only saddr rule for the subnet is the drop.
        assert!(script.contains("ip saddr 10.0.1.0/24 drop"));
        assert!(!script.contains("accept\n    ip saddr 10.0.1.0/24 accept"));
    }

    #[test]
    fn render_egress_teardown_removes_table() {
        let t = "ocf_egr_abc12345";
        let s = render_egress_teardown(t);
        assert_eq!(s, format!("add table inet {t}\ndelete table inet {t}\n"));
    }

    #[tokio::test]
    async fn null_backend_apply_egress_is_noop() {
        let b = NullBackend::new();
        let subnet = nat_subnet();
        assert!(b.apply_egress(&subnet, &[]).await.is_ok());
    }

    #[test]
    fn vxlan_fdb_args_build_flood_entry() {
        let args = vxlan_fdb_args("append", "vxlan1001", "10.0.0.2");
        assert_eq!(
            args,
            vec![
                "fdb", "append", "00:00:00:00:00:00", "dev", "vxlan1001", "dst", "10.0.0.2"
            ]
        );
        let del = vxlan_fdb_args("del", "vxlan1001", "10.0.0.2");
        assert_eq!(del[1], "del");
    }

    #[test]
    fn ovs_peer_suffix_is_ifname_safe() {
        let s = ovs_peer_suffix("10.0.0.2");
        assert_eq!(s, "10_0_0_2");
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        // Capped so `vx{vni}-{suffix}` stays within IFNAMSIZ.
        assert!(ovs_peer_suffix("255.255.255.255").len() <= 10);
    }

    #[tokio::test]
    async fn null_backend_apply_vpc_peers_is_noop() {
        let b = NullBackend::new();
        let vpc = Vpc::new("v", "10.0.0.0/16", 1001);
        assert!(b.apply_vpc_peers(&vpc, &["10.0.0.2".into()]).await.is_ok());
    }
}
