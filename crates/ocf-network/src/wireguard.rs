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

/// How a WireGuard interface is realized on this host.
///
/// The kernel datapath (`wireguard` module, mainline since Linux 5.6) is fastest
/// and always preferred; a **userspace** implementation is the fallback for older
/// kernels, locked-down hosts, or platforms without the module. Either way the
/// interface is driven identically through `wg` + `ip`, so only its creation
/// differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireguardMode {
    /// The in-kernel `wireguard` module.
    Kernel,
    /// A userspace implementation, by binary name.
    Userspace(&'static str),
}

impl std::fmt::Display for WireguardMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireguardMode::Kernel => write!(f, "kernel"),
            WireguardMode::Userspace(bin) => write!(f, "userspace:{bin}"),
        }
    }
}

/// Userspace WireGuard backends we know how to start, in preference order.
/// `boringtun` is Cloudflare's pure-Rust implementation; `wireguard-go` is the Go
/// reference. Each, invoked as `<bin> <iface>`, creates a userspace interface
/// named `<iface>` that `wg`/`ip` then drive exactly like a kernel one.
const USERSPACE_BACKENDS: &[&str] = &["boringtun", "boringtun-cli", "wireguard-go"];

/// Whether `bin` is on `PATH` — a side-effect-free check (no spawning), so we
/// never accidentally invoke an unknown binary while probing.
fn binary_available(bin: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in path.split(sep).filter(|d| !d.is_empty()) {
        let candidate = std::path::Path::new(dir).join(bin);
        if candidate.is_file() {
            return true;
        }
        if cfg!(windows) {
            for ext in ["exe", "cmd", "bat"] {
                if candidate.with_extension(ext).is_file() {
                    return true;
                }
            }
        }
    }
    false
}

/// Programs this host's WireGuard interface and its peers via `ip` + `wg`,
/// preferring the kernel datapath and falling back to userspace.
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

    /// Create the interface, **preferring the kernel datapath and falling back to
    /// a userspace implementation** when the `wireguard` kernel module is absent.
    /// Returns the [`WireguardMode`] actually used. Honest error when neither a
    /// kernel module nor a userspace backend is available.
    async fn create_interface(&self) -> Result<WireguardMode> {
        // Best-effort: load the module if it exists but isn't loaded yet (needs
        // root; absent `modprobe` just fails harmlessly).
        let _ = run("modprobe", &["wireguard"]).await;

        // 1. Kernel datapath. `run` treats "File exists" as success, so an
        //    interface created on a previous run is fine.
        if run("ip", &["link", "add", &self.iface, "type", "wireguard"])
            .await
            .is_ok()
        {
            return Ok(WireguardMode::Kernel);
        }

        // 2. Userspace fallback: the backend creates an interface named `<iface>`,
        //    after which `wg`/`ip` drive it identically.
        for bin in USERSPACE_BACKENDS {
            if binary_available(bin) {
                run(bin, &[&self.iface]).await?;
                tracing::info!(iface = %self.iface, backend = bin, "created userspace WireGuard interface");
                return Ok(WireguardMode::Userspace(bin));
            }
        }

        Err(Error::provider(
            "wireguard",
            format!(
                "interface `{}`: no kernel `wireguard` module and no userspace backend on PATH \
                 (install the wireguard kernel module, or `boringtun` / `wireguard-go`)",
                self.iface
            ),
        ))
    }

    /// Create and configure this node's WireGuard interface: bring it up (kernel
    /// or userspace — see [`create_interface`](Self::create_interface)), set its
    /// private key (this node's fabric secret key, base64) and listen port, assign
    /// its `address_cidr` (e.g. `"10.255.0.1/16"`), and raise it. Returns the
    /// realized [`WireguardMode`]. Idempotent.
    pub async fn ensure_interface(
        &self,
        private_key_b64: &str,
        address_cidr: &str,
    ) -> Result<WireguardMode> {
        let mode = self.create_interface().await?;

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
        tracing::info!(iface = %self.iface, address = %address_cidr, mode = %mode, "WireGuard underlay up");
        Ok(mode)
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

    #[test]
    fn mode_renders_kernel_and_userspace() {
        assert_eq!(WireguardMode::Kernel.to_string(), "kernel");
        assert_eq!(
            WireguardMode::Userspace("boringtun").to_string(),
            "userspace:boringtun"
        );
    }

    #[test]
    fn boringtun_is_the_preferred_userspace_backend() {
        // Pure Rust first, Go reference last.
        assert_eq!(USERSPACE_BACKENDS.first(), Some(&"boringtun"));
        assert!(USERSPACE_BACKENDS.contains(&"wireguard-go"));
    }

    #[test]
    fn binary_available_is_false_for_a_nonexistent_binary() {
        assert!(!binary_available("ocf-definitely-not-a-real-binary-xyz-123"));
    }
}
