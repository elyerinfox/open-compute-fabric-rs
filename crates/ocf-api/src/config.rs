//! Controller configuration passed in by the `ocfd` binary.

use std::path::PathBuf;

/// How a node should bring itself up: its identity, where to persist state, and
/// who to contact to join the fleet.
#[derive(Clone, Debug)]
pub struct ControllerConfig {
    /// This node's stable identity in the fleet.
    pub node_id: String,
    /// Directory for durable state. `None` runs fully in-memory (state is lost
    /// on restart); `Some(dir)` persists to `dir/state.redb` and reloads on boot.
    pub data_dir: Option<PathBuf>,
    /// Seed peers to contact when joining the mesh (`host:port`).
    pub seeds: Vec<String>,
    /// Seconds of heartbeat silence before a peer is suspected.
    pub suspect_timeout_secs: i64,
    /// Additional seconds after suspicion before a peer is declared dead.
    pub dead_timeout_secs: i64,
    /// TCP port for the fabric control channel (RPC, ping/latency probing),
    /// reached over the `wg-mgmt` overlay. Distinct from the WireGuard UDP ports
    /// (51820/51821/51822).
    pub fabric_control_port: u16,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        ControllerConfig {
            node_id: "node-local".to_string(),
            data_dir: None,
            seeds: Vec::new(),
            suspect_timeout_secs: 5,
            dead_timeout_secs: 5,
            fabric_control_port: 51900,
        }
    }
}
