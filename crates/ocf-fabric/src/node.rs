//! The mesh's view of a participating node.

use crate::crypto::{KeyPair, NodeId, PublicKey};
use chrono::{DateTime, Utc};
use ocf_core::prelude::*;

/// How a node can be reached on the fabric.
///
/// This is what lets the mesh handle members that aren't publicly addressable:
/// a `Private` node can make outbound connections but can't be dialed directly,
/// so traffic to it is routed through a `Relay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    /// Directly dialable from anywhere in the fleet (a public/routable endpoint).
    #[default]
    Public,
    /// Behind NAT / no inbound: reachable only via a relay (or its own outbound
    /// connections). Not directly dialable.
    Private,
    /// Directly dialable *and* willing to forward traffic for `Private` peers.
    Relay,
}

/// A node advertised in the fabric mesh.
///
/// This is the membership record a peer needs in order to reach another node:
/// its mesh-level [`NodeId`], the (optional) fleet [`Id`] of the backing
/// machine, its public key, the endpoints it can be dialed on, how it can be
/// reached, and when it was last seen alive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FabricNode {
    pub node_id: NodeId,
    /// The fleet machine this node runs on, if known.
    pub machine_id: Option<Id>,
    pub public_key: PublicKey,
    /// Dialable mesh endpoints, e.g. `"10.0.0.4:51820"`.
    pub endpoints: Vec<String>,
    /// How this node can be reached (direct vs relay-only). Defaults to `Public`.
    #[serde(default)]
    pub reachability: Reachability,
    pub last_seen: DateTime<Utc>,
}

impl FabricNode {
    /// Construct a node record from an identity and its endpoints.
    pub fn new(
        node_id: NodeId,
        public_key: PublicKey,
        endpoints: Vec<String>,
    ) -> Self {
        FabricNode {
            node_id,
            machine_id: None,
            public_key,
            endpoints,
            reachability: Reachability::Public,
            last_seen: Utc::now(),
        }
    }

    /// Build a node from a [`KeyPair`], deriving the [`NodeId`] from its
    /// public-key fingerprint.
    pub fn from_keypair(keypair: &KeyPair, endpoints: Vec<String>) -> Self {
        FabricNode::new(keypair.node_id(), keypair.public.clone(), endpoints)
    }

    /// Associate this node with a fleet machine.
    pub fn with_machine(mut self, machine_id: Id) -> Self {
        self.machine_id = Some(machine_id);
        self
    }

    /// Set how this node is reachable (direct vs relay-only).
    pub fn with_reachability(mut self, reachability: Reachability) -> Self {
        self.reachability = reachability;
        self
    }

    /// Whether this node can be dialed directly (`Public` or `Relay`).
    pub fn is_directly_dialable(&self) -> bool {
        !matches!(self.reachability, Reachability::Private)
    }

    /// Whether this node can relay traffic for others.
    pub fn is_relay(&self) -> bool {
        matches!(self.reachability, Reachability::Relay)
    }

    /// Refresh the liveness timestamp to now.
    pub fn touch(&mut self) {
        self.last_seen = Utc::now();
    }

    /// The first advertised endpoint, if any — the address a transport dials.
    pub fn primary_endpoint(&self) -> Option<&str> {
        self.endpoints.first().map(|s| s.as_str())
    }
}

/// The host's stable machine id from the OS (`/etc/machine-id`, falling back to
/// `/var/lib/dbus/machine-id`), if present. This is a unique, reboot-stable id
/// independent of hostname — ideal as a node's permanent fabric identity.
pub fn detect_machine_id() -> Option<String> {
    for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(contents) = std::fs::read_to_string(path) {
            let id = contents.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// The host's name, for a **friendly** node label (not its identity). Tries the
/// `HOSTNAME`/`COMPUTERNAME` env, then `/etc/hostname`, then the kernel hostname.
pub fn detect_hostname() -> Option<String> {
    for var in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(h) = std::env::var(var) {
            let h = h.trim();
            if !h.is_empty() {
                return Some(h.to_string());
            }
        }
    }
    for path in ["/etc/hostname", "/proc/sys/kernel/hostname"] {
        if let Ok(contents) = std::fs::read_to_string(path) {
            let h = contents.trim();
            if !h.is_empty() {
                return Some(h.to_string());
            }
        }
    }
    None
}

/// Resolve this node's stable, **unique** identity **without the operator naming
/// it**: an explicit override → the OS machine id → a UUID generated once and
/// persisted under the data directory → the hostname. The result is what derives
/// the node's keypair, Raft id, and membership id, so it must be stable across
/// reboots and unique per host.
pub fn resolve_machine_id(explicit: Option<String>, data_dir: Option<&std::path::Path>) -> String {
    if let Some(id) = explicit.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return id;
    }
    if let Some(id) = detect_machine_id() {
        return id;
    }
    if let Some(dir) = data_dir {
        let path = dir.join("machine-id");
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let id = existing.trim();
            if !id.is_empty() {
                return id.to_string();
            }
        }
        let generated = uuid::Uuid::new_v4().to_string();
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(&path, &generated);
        return generated;
    }
    detect_hostname().unwrap_or_else(|| "node".to_string())
}

/// Best-effort detection of this host's primary reachable IPv4 — the source
/// address the kernel would use toward the default route. This is the
/// **router-assigned LAN address** on a host behind a home/office router
/// (`192.168.x.x`, `10.x.x.x`, …), and the right interface even with several NICs.
///
/// It opens a connected UDP socket toward a routable target and reads back the
/// chosen local address; **no packets are sent** (UDP `connect` only selects a
/// route). Returns `None` when there's no usable route (offline, or no default
/// gateway) — the operator then sets the address explicitly.
pub fn detect_local_address() -> Option<String> {
    use std::net::UdpSocket;
    // Any routable destination works; we never transmit to it.
    for target in ["8.8.8.8:80", "1.1.1.1:80"] {
        let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if sock.connect(target).is_ok() {
            if let Ok(local) = sock.local_addr() {
                let ip = local.ip();
                if !ip.is_loopback() && !ip.is_unspecified() {
                    return Some(ip.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_keypair_derives_node_id() {
        let kp = KeyPair::from_seed_name("n1");
        let node = FabricNode::from_keypair(&kp, vec!["10.0.0.1:7777".into()]);
        assert_eq!(node.node_id, kp.node_id());
        assert_eq!(node.primary_endpoint(), Some("10.0.0.1:7777"));
    }

    #[test]
    fn machine_id_explicit_wins_and_is_stable() {
        // An explicit override is returned verbatim.
        assert_eq!(resolve_machine_id(Some("abc-123".into()), None), "abc-123");
        // With a data dir, the resolved id is stable across calls (OS machine-id
        // if present, else a persisted uuid) — never empty, never changing.
        let dir = std::env::temp_dir().join(format!("ocf-mid-{}", uuid::Uuid::new_v4()));
        let first = resolve_machine_id(None, Some(&dir));
        let second = resolve_machine_id(None, Some(&dir));
        assert!(!first.is_empty());
        assert_eq!(first, second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_local_address_is_sane_or_none() {
        // Never panics; when it finds an address it is a real, non-loopback IP.
        if let Some(addr) = detect_local_address() {
            let ip: std::net::IpAddr = addr.parse().expect("a parseable IP");
            assert!(!ip.is_loopback(), "should not be loopback: {addr}");
            assert!(!ip.is_unspecified(), "should not be 0.0.0.0: {addr}");
        }
    }
}
