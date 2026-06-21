//! The WireGuard **underlay**: an encrypted, kernel-datapath mesh between hosts
//! that the VXLAN overlay rides on.
//!
//! This is the answer to "encrypt the fabric for containers" without a userspace
//! per-packet pump: WireGuard is a real kernel `wireguard` interface, so once the
//! overlay's VXLAN endpoints point at the peers' WireGuard addresses, all
//! cross-host workload traffic is encrypted at line rate by the kernel.
//!
//! **Layering / isolation.** WireGuard here is a *flat* L3 underlay — every node
//! gets one WireGuard address and can reach every other. It provides encryption
//! and reachability, **not** tenant isolation. Isolation and subnetting stay in
//! the overlay: a VPC is a VXLAN VNI (separate L2 segment), subnets carve CIDRs,
//! and [`FirewallPolicy`](crate::FirewallPolicy) ACLs segment further. So this
//! change carries the existing isolation semantics, encrypted — it does not move
//! isolation into WireGuard (whose per-peer `allowed-ips` would be too coarse for
//! tenants).
//!
//! A node's WireGuard identity *is* its fabric identity: the X25519 keypair from
//! `ocf-fabric` is exactly a Curve25519 WireGuard key (base64-encoded), and the
//! peer set is the fabric membership.

use crate::backend::run;
use ocf_core::prelude::*;

/// Programs this host's WireGuard interface and its peers via `ip` + `wg`.
#[derive(Debug, Clone)]
pub struct WireguardUnderlay {
    iface: String,
    listen_port: u16,
}

impl WireguardUnderlay {
    /// A WireGuard underlay on interface `iface` (e.g. `"wg-ocf"`) listening on
    /// `listen_port` (e.g. `51820`).
    pub fn new(iface: impl Into<String>, listen_port: u16) -> Self {
        WireguardUnderlay {
            iface: iface.into(),
            listen_port: listen_port.into(),
        }
    }

    pub fn iface(&self) -> &str {
        &self.iface
    }

    /// Create and configure this node's WireGuard interface: set its private key
    /// (this node's fabric secret key, base64) and listen port, assign its
    /// `address_cidr` (e.g. `"10.255.0.1/16"`), and bring it up. Idempotent.
    pub async fn ensure_interface(
        &self,
        private_key_b64: &str,
        address_cidr: &str,
    ) -> Result<()> {
        // `ip link add <iface> type wireguard` — "File exists" treated as success.
        run("ip", &["link", "add", &self.iface, "type", "wireguard"]).await?;

        // `wg set` reads the private key from a file (not argv, to keep it off the
        // process table). Write it, apply, then remove.
        let keyfile = std::env::temp_dir().join(format!("ocf-{}.key", self.iface));
        let keypath = keyfile.to_string_lossy().to_string();
        std::fs::write(&keyfile, format!("{private_key_b64}\n"))
            .map_err(|e| Error::provider("wg", format!("write key file: {e}")))?;
        let port = self.listen_port.to_string();
        let result = run(
            "wg",
            &["set", &self.iface, "private-key", &keypath, "listen-port", &port],
        )
        .await;
        let _ = std::fs::remove_file(&keyfile);
        result?;

        // Address + up.
        run("ip", &["addr", "add", address_cidr, "dev", &self.iface]).await?;
        run("ip", &["link", "set", &self.iface, "up"]).await?;
        tracing::info!(iface = %self.iface, address = %address_cidr, "WireGuard underlay up");
        Ok(())
    }

    /// Add (or update) a peer: its WireGuard public key, real underlay endpoint
    /// (`host:port`), the WireGuard `allowed-ips` it owns, and a keepalive.
    pub async fn set_peer(
        &self,
        public_key_b64: &str,
        endpoint: &str,
        allowed_ips: &str,
        keepalive_secs: u16,
    ) -> Result<()> {
        let args = wg_set_peer_args(
            &self.iface,
            public_key_b64,
            endpoint,
            allowed_ips,
            keepalive_secs,
        );
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        run("wg", &argv).await?;
        tracing::info!(iface = %self.iface, %endpoint, "WireGuard peer programmed");
        Ok(())
    }

    /// Remove a peer by its public key.
    pub async fn remove_peer(&self, public_key_b64: &str) -> Result<()> {
        run("wg", &["set", &self.iface, "peer", public_key_b64, "remove"]).await
    }
}

/// Build the `wg set <iface> peer <key> endpoint <ep> allowed-ips <ips>
/// persistent-keepalive <n>` argv. Pure and unit-tested.
fn wg_set_peer_args(
    iface: &str,
    public_key_b64: &str,
    endpoint: &str,
    allowed_ips: &str,
    keepalive_secs: u16,
) -> Vec<String> {
    vec![
        "set".into(),
        iface.into(),
        "peer".into(),
        public_key_b64.into(),
        "endpoint".into(),
        endpoint.into(),
        "allowed-ips".into(),
        allowed_ips.into(),
        "persistent-keepalive".into(),
        keepalive_secs.to_string(),
    ]
}

/// Splice a workload into a subnet: create a veth pair, move one end into the
/// workload's network namespace, attach the other to the subnet bridge, and
/// assign the workload's address inside the namespace.
///
/// This is the missing "attach a container to the overlay" plumbing — with it,
/// a workload in `netns` is on the subnet bridge (and therefore on the VXLAN
/// overlay, encrypted by the WireGuard underlay). `veth_id` should be a short,
/// unique token (interface names are capped at 15 bytes).
pub async fn attach_workload_veth(
    netns: &str,
    bridge: &str,
    veth_id: &str,
    address_cidr: &str,
) -> Result<()> {
    let id: String = veth_id.chars().take(8).collect();
    let host = format!("vh-{id}"); // host side (≤ 11 chars)
    let peer = format!("vp-{id}"); // namespace side

    run("ip", &["link", "add", &host, "type", "veth", "peer", "name", &peer]).await?;
    run("ip", &["link", "set", &peer, "netns", netns]).await?;
    run("ip", &["link", "set", &host, "master", bridge]).await?;
    run("ip", &["link", "set", &host, "up"]).await?;
    run("ip", &["netns", "exec", netns, "ip", "addr", "add", address_cidr, "dev", &peer]).await?;
    run("ip", &["netns", "exec", netns, "ip", "link", "set", &peer, "up"]).await?;
    tracing::info!(%netns, %bridge, address = %address_cidr, "attached workload veth to subnet");
    Ok(())
}

/// The bridge interface name for a subnet — matches the netns backend's naming
/// (`br-<first 8 chars of subnet id>`), so the container attach lands on the same
/// bridge the subnet was realized on.
pub fn subnet_bridge_name(subnet_id: &Id) -> String {
    let short: String = subnet_id.as_str().chars().take(8).collect();
    format!("br-{short}")
}

/// Splice a **running container** onto a subnet's overlay bridge.
///
/// Exposes the container's network namespace (found from its host `pid`) under a
/// name so `ip netns exec` can address it, then runs [`attach_workload_veth`] to
/// put one end of a veth pair in the container and the other on `bridge`. This is
/// the last mile that makes a live container reach the (WireGuard-encrypted)
/// VXLAN overlay. Linux + a container runtime only.
pub async fn attach_container_to_subnet(
    container_pid: u32,
    ns_alias: &str,
    bridge: &str,
    veth_id: &str,
    address_cidr: &str,
) -> Result<()> {
    // `ip netns attach <alias> <pid>` exposes /proc/<pid>/ns/net under the alias.
    run("ip", &["netns", "attach", ns_alias, &container_pid.to_string()]).await?;
    attach_workload_veth(ns_alias, bridge, veth_id, address_cidr).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnet_bridge_name_matches_backend_naming() {
        let id = Id::named("web-subnet-1234567890");
        let name = subnet_bridge_name(&id);
        assert!(name.starts_with("br-"));
        // br- + 8 chars = 11, under the 15-byte IFNAMSIZ cap.
        assert!(name.len() <= 11);
    }

    #[test]
    fn wg_set_peer_args_are_well_formed() {
        let args = wg_set_peer_args(
            "wg-ocf",
            "abcDEF0123456789abcDEF0123456789abcDEF01234=",
            "10.0.0.2:51820",
            "10.255.0.2/32",
            25,
        );
        assert_eq!(args[0], "set");
        assert_eq!(args[1], "wg-ocf");
        assert_eq!(args[2], "peer");
        // endpoint + allowed-ips + keepalive present and ordered.
        let joined = args.join(" ");
        assert!(joined.contains("endpoint 10.0.0.2:51820"));
        assert!(joined.contains("allowed-ips 10.255.0.2/32"));
        assert!(joined.contains("persistent-keepalive 25"));
    }

    #[test]
    fn iface_accessor() {
        let wg = WireguardUnderlay::new("wg-ocf", 51820);
        assert_eq!(wg.iface(), "wg-ocf");
    }
}
