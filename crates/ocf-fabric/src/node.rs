//! The mesh's view of a participating node.

use crate::crypto::{KeyPair, NodeId, PublicKey};
use chrono::{DateTime, Utc};
use ocf_core::prelude::*;

/// A node advertised in the fabric mesh.
///
/// This is the membership record a peer needs in order to reach another node:
/// its mesh-level [`NodeId`], the (optional) fleet [`Id`] of the backing
/// machine, its public key, the endpoints it can be dialed on, and when it was
/// last seen alive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FabricNode {
    pub node_id: NodeId,
    /// The fleet machine this node runs on, if known.
    pub machine_id: Option<Id>,
    pub public_key: PublicKey,
    /// Dialable mesh endpoints, e.g. `"10.0.0.4:51820"`.
    pub endpoints: Vec<String>,
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

    /// Refresh the liveness timestamp to now.
    pub fn touch(&mut self) {
        self.last_seen = Utc::now();
    }

    /// The first advertised endpoint, if any — the address a transport dials.
    pub fn primary_endpoint(&self) -> Option<&str> {
        self.endpoints.first().map(|s| s.as_str())
    }
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
}
