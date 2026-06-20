//! In-memory mesh membership and broadcast fan-out.

use crate::crypto::NodeId;
use crate::node::FabricNode;
use crate::transport::FabricTransport;
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// The fabric mesh: who the peers are and how to reach them.
///
/// Membership is held in an in-memory [`RwLock`] map keyed by [`NodeId`].
/// [`broadcast`](FabricMesh::broadcast) fans a payload out to every peer by
/// driving the configured [`FabricTransport`]. This is the single-node
/// controller view; a production deployment would gossip membership instead.
pub struct FabricMesh {
    transport: Arc<dyn FabricTransport>,
    peers: RwLock<HashMap<NodeId, FabricNode>>,
}

impl FabricMesh {
    /// Create an empty mesh that sends over `transport`.
    pub fn new(transport: Arc<dyn FabricTransport>) -> Self {
        FabricMesh {
            transport,
            peers: RwLock::new(HashMap::new()),
        }
    }

    /// The transport this mesh sends over.
    pub fn transport(&self) -> &Arc<dyn FabricTransport> {
        &self.transport
    }

    /// Add (or refresh) a peer in the mesh.
    ///
    /// Re-joining with the same [`NodeId`] replaces the prior record, which is
    /// how a peer updates its endpoints or liveness.
    pub fn join(&self, node: FabricNode) -> Result<()> {
        tracing::info!(node = %node.node_id, "node joining fabric mesh");
        self.peers.write().insert(node.node_id.clone(), node);
        Ok(())
    }

    /// Remove a peer from the mesh. Idempotent: leaving an absent node is `Ok`.
    pub fn leave(&self, node_id: &NodeId) -> Result<()> {
        tracing::info!(node = %node_id, "node leaving fabric mesh");
        self.peers.write().remove(node_id);
        Ok(())
    }

    /// All current peers, unordered.
    pub fn peers(&self) -> Vec<FabricNode> {
        self.peers.read().values().cloned().collect()
    }

    /// Look up a single peer by id.
    pub fn peer(&self, node_id: &NodeId) -> Result<FabricNode> {
        self.peers
            .read()
            .get(node_id)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("fabric peer {node_id}")))
    }

    /// Number of peers currently in the mesh.
    pub fn len(&self) -> usize {
        self.peers.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.read().is_empty()
    }

    /// Send `payload` to a single peer over the transport.
    pub async fn send_to(&self, node_id: &NodeId, payload: &[u8]) -> Result<()> {
        let node = self.peer(node_id)?;
        self.transport.send(&node, payload).await
    }

    /// Fan `payload` out to every peer over the transport.
    ///
    /// Returns the number of peers the payload was delivered to. A failure to
    /// reach one peer aborts the broadcast and surfaces the error, so callers
    /// can decide on retry/quorum semantics.
    pub async fn broadcast(&self, payload: &[u8]) -> Result<usize> {
        let targets = self.peers();
        tracing::info!(
            peers = targets.len(),
            bytes = payload.len(),
            "broadcasting to fabric mesh"
        );
        let mut delivered = 0usize;
        for node in targets {
            self.transport.send(&node, payload).await?;
            delivered += 1;
        }
        Ok(delivered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::KeyPair;
    use crate::transport::NoiseTransport;

    /// A transport that records deliveries without touching the network, so the
    /// mesh's fan-out logic can be tested independently of real sockets.
    #[derive(Default)]
    struct CountingTransport {
        sent: parking_lot::Mutex<usize>,
    }

    impl Provider for CountingTransport {
        fn name(&self) -> &str {
            "counting"
        }
    }

    #[async_trait]
    impl FabricTransport for CountingTransport {
        async fn connect(&self, _node: &FabricNode) -> Result<()> {
            Ok(())
        }
        async fn send(&self, _node: &FabricNode, _payload: &[u8]) -> Result<()> {
            *self.sent.lock() += 1;
            Ok(())
        }
    }

    fn node(name: &str) -> FabricNode {
        FabricNode::from_keypair(
            &KeyPair::from_seed_name(name),
            vec![format!("10.0.0.1:{}", 7000)],
        )
    }

    fn mesh() -> FabricMesh {
        FabricMesh::new(Arc::new(NoiseTransport::new()))
    }

    #[test]
    fn join_leave_membership() {
        let m = mesh();
        let n = node("a");
        let id = n.node_id.clone();
        m.join(n).unwrap();
        assert_eq!(m.len(), 1);
        assert!(m.peer(&id).is_ok());
        m.leave(&id).unwrap();
        assert!(m.is_empty());
        // Leaving twice is idempotent.
        m.leave(&id).unwrap();
    }

    #[test]
    fn rejoin_replaces_record() {
        let m = mesh();
        m.join(node("a")).unwrap();
        m.join(node("a")).unwrap();
        assert_eq!(m.len(), 1);
    }

    #[tokio::test]
    async fn broadcast_reaches_all_peers() {
        let transport = Arc::new(CountingTransport::default());
        let m = FabricMesh::new(transport.clone());
        m.join(node("a")).unwrap();
        m.join(node("b")).unwrap();
        let delivered = m.broadcast(b"ping").await.unwrap();
        assert_eq!(delivered, 2);
        assert_eq!(*transport.sent.lock(), 2);
    }

    #[tokio::test]
    async fn send_to_unknown_peer_errors() {
        let m = mesh();
        let id = NodeId::new("ghost");
        let err = m.send_to(&id, b"x").await.unwrap_err();
        assert_eq!(err.code(), "not_found");
    }
}
