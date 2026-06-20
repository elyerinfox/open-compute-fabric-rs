//! Host network configuration: IP forwarding and software bridges.
//!
//! The [`NetworkManager`] contract is the host-level dataplane prerequisite the
//! rest of the fabric builds on: enabling IPv4 forwarding so a machine can route
//! between runtimes, and creating the Linux bridges that container/VM backends
//! attach to. The default [`LinuxNetworkManager`] programs the real host: it
//! writes the `/proc/sys` forwarding knobs and drives `ip link` for bridge
//! lifecycle. It owns no in-memory state — the kernel is the source of truth.

use crate::exec::run;
use ocf_core::prelude::*;

/// Host network configuration contract.
///
/// Implementations program the host kernel's forwarding flag and software
/// bridges by writing `/proc/sys/net/.../forwarding` and issuing `ip link`
/// operations.
#[async_trait]
pub trait NetworkManager: Send + Sync {
    /// Enable or disable IPv4 forwarding on the host.
    async fn set_ip_forwarding(&self, enabled: bool) -> Result<()>;

    /// Ensure a bridge named `name` exists, creating it if necessary.
    ///
    /// Idempotent: calling it for an existing bridge is a no-op success.
    async fn ensure_bridge(&self, name: &str) -> Result<()>;

    /// Delete the bridge named `name`. Errors if no such bridge exists.
    async fn delete_bridge(&self, name: &str) -> Result<()>;

    /// List the names of all bridges currently managed on the host.
    async fn list_bridges(&self) -> Result<Vec<String>>;
}

/// `/proc/sys` path for the IPv4 forwarding master switch.
const IPV4_FORWARD: &str = "/proc/sys/net/ipv4/ip_forward";
/// `/proc/sys` path for the IPv6 forwarding switch (`all` interfaces).
const IPV6_FORWARD: &str = "/proc/sys/net/ipv6/conf/all/forwarding";

/// Linux host network manager driving the real kernel.
///
/// Forwarding is toggled by writing `/proc/sys/...`; bridges are created and
/// destroyed with `ip link`. The manager is stateless — every query reads the
/// live kernel state, so it can be shared freely across async tasks.
pub struct LinuxNetworkManager;

impl LinuxNetworkManager {
    pub fn new() -> Self {
        LinuxNetworkManager
    }

    /// Whether IPv4 forwarding is currently enabled, read live from
    /// `/proc/sys/net/ipv4/ip_forward`.
    pub fn ip_forwarding_enabled(&self) -> bool {
        std::fs::read_to_string(IPV4_FORWARD)
            .map(|s| s.trim() == "1")
            .unwrap_or(false)
    }
}

impl Default for LinuxNetworkManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse the names of bridge interfaces out of `ip -o link show type bridge`.
///
/// Each line looks like `7: br-ocf: <BROADCAST,...> mtu 1500 ...`; we take the
/// second colon-separated field and strip any `@parent` suffix.
fn parse_bridge_names(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.split(':').nth(1))
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .map(|name| name.split('@').next().unwrap_or(name).trim().to_string())
        .collect()
}

#[async_trait]
impl NetworkManager for LinuxNetworkManager {
    async fn set_ip_forwarding(&self, enabled: bool) -> Result<()> {
        let value = if enabled { "1" } else { "0" };
        // IPv4 is the master switch the fabric relies on; it must succeed.
        std::fs::write(IPV4_FORWARD, value)?;
        // IPv6 forwarding is best-effort: a kernel built without IPv6 won't
        // expose the path, which we don't want to treat as a hard failure.
        if let Err(e) = std::fs::write(IPV6_FORWARD, value) {
            tracing::debug!(error = %e, path = IPV6_FORWARD, "ipv6 forwarding not set");
        }
        Ok(())
    }

    async fn ensure_bridge(&self, name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(Error::invalid("bridge name must not be empty"));
        }
        // `ip link add` fails with "File exists" if the bridge is already
        // present; treat that as success so the operation stays idempotent.
        match run("ip", &["link", "add", "name", name, "type", "bridge"]).await {
            Ok(_) => {}
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if !(msg.contains("exists") || msg.contains("file exists")) {
                    return Err(e);
                }
            }
        }
        run("ip", &["link", "set", name, "up"]).await?;
        Ok(())
    }

    async fn delete_bridge(&self, name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(Error::invalid("bridge name must not be empty"));
        }
        match run("ip", &["link", "del", name]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("cannot find") || msg.contains("does not exist") {
                    Err(Error::not_found(format!("bridge {name}")))
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn list_bridges(&self) -> Result<Vec<String>> {
        let output = run("ip", &["-o", "link", "show", "type", "bridge"]).await?;
        Ok(parse_bridge_names(&output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bridge_names_from_ip_output() {
        let output = "\
7: br-ocf: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc noqueue state UP mode DEFAULT group default qlen 1000\\    link/ether aa:bb:cc:dd:ee:ff brd ff:ff:ff:ff:ff:ff
9: docker0: <NO-CARRIER,BROADCAST,MULTICAST,UP> mtu 1500 qdisc noqueue state DOWN mode DEFAULT group default\\    link/ether 02:42:00:00:00:00 brd ff:ff:ff:ff:ff:ff
";
        assert_eq!(parse_bridge_names(output), vec!["br-ocf", "docker0"]);
    }

    #[test]
    fn parses_empty_output_to_no_bridges() {
        assert!(parse_bridge_names("").is_empty());
        assert!(parse_bridge_names("\n  \n").is_empty());
    }

    #[test]
    fn strips_at_parent_suffix() {
        let output = "12: vlan10@eth0: <BROADCAST> mtu 1500 qdisc noqueue\n";
        assert_eq!(parse_bridge_names(output), vec!["vlan10"]);
    }

    // The lifecycle tests need a real Linux host with `ip` and CAP_NET_ADMIN.
    #[tokio::test]
    #[ignore = "requires root + Linux `ip` tooling"]
    async fn bridge_lifecycle_is_idempotent() {
        let nm = LinuxNetworkManager::new();
        nm.ensure_bridge("br-ocf-test").await.unwrap();
        nm.ensure_bridge("br-ocf-test").await.unwrap(); // no-op
        assert!(nm
            .list_bridges()
            .await
            .unwrap()
            .contains(&"br-ocf-test".to_string()));
        nm.delete_bridge("br-ocf-test").await.unwrap();
        assert!(nm.delete_bridge("br-ocf-test").await.is_err());
    }

    #[tokio::test]
    #[ignore = "requires root + Linux /proc/sys"]
    async fn forwarding_toggles() {
        let nm = LinuxNetworkManager::new();
        nm.set_ip_forwarding(true).await.unwrap();
        assert!(nm.ip_forwarding_enabled());
        nm.set_ip_forwarding(false).await.unwrap();
        assert!(!nm.ip_forwarding_enabled());
    }
}
