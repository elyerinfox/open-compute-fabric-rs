//! The pluggable host-to-host transport contract and its real Noise implementation.

use crate::crypto::{KeyPair, NodeId};
use crate::node::FabricNode;
use crate::wire;
use ocf_core::prelude::*;
use snow::TransportState;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

/// Pluggable contract for moving bytes between mesh nodes.
///
/// Implementations own the wire: dialing a peer ([`connect`](FabricTransport::connect)),
/// pushing a frame to it ([`send`](FabricTransport::send)), and declaring whether
/// the channel is confidential ([`is_encrypted`](FabricTransport::is_encrypted)).
/// Concrete backends are registered by name in a [`Registry`] so the mesh never
/// depends on a specific transport.
#[async_trait]
pub trait FabricTransport: Provider {
    /// Establish (or reuse) an encrypted session to `node`.
    async fn connect(&self, node: &FabricNode) -> Result<()>;

    /// Send `payload` to `node`, establishing a session first if needed.
    async fn send(&self, node: &FabricNode, payload: &[u8]) -> Result<()>;

    /// Whether this transport encrypts traffic on the wire.
    fn is_encrypted(&self) -> bool {
        true
    }
}

/// A live, post-handshake connection to one peer.
struct Conn {
    stream: TcpStream,
    transport: TransportState,
}

/// A real Noise transport: TCP + the Noise XX handshake (see [`crate::wire`]).
///
/// Each instance carries this node's static [`KeyPair`] (its mesh identity) and
/// caches one authenticated, encrypted session per peer. Dialing a peer runs a
/// genuine X25519 handshake and every `send` is sealed with ChaCha20-Poly1305 —
/// `is_encrypted()` is the truth, not a claim.
pub struct NoiseTransport {
    name: String,
    keypair: KeyPair,
    conns: Mutex<HashMap<NodeId, Arc<Mutex<Conn>>>>,
}

impl NoiseTransport {
    /// Build a transport with a fresh node identity.
    pub fn new() -> Self {
        Self::with_keypair(KeyPair::generate())
    }

    /// Build a transport that presents `keypair` as this node's identity, so the
    /// transport and the node's advertised public key are the same key.
    pub fn with_keypair(keypair: KeyPair) -> Self {
        NoiseTransport {
            name: "noise".to_string(),
            keypair,
            conns: Mutex::new(HashMap::new()),
        }
    }

    /// This transport's public identity key.
    pub fn public_key(&self) -> &crate::crypto::PublicKey {
        &self.keypair.public
    }
}

impl Default for NoiseTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for NoiseTransport {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Noise XX (X25519 + ChaCha20-Poly1305) encrypted host-to-host transport over TCP"
    }
}

#[async_trait]
impl FabricTransport for NoiseTransport {
    async fn connect(&self, node: &FabricNode) -> Result<()> {
        let endpoint = node
            .primary_endpoint()
            .ok_or_else(|| Error::invalid(format!("node {} has no endpoint", node.node_id)))?
            .to_string();

        let mut conns = self.conns.lock().await;
        if conns.contains_key(&node.node_id) {
            return Ok(());
        }

        let mut stream = TcpStream::connect(&endpoint)
            .await
            .map_err(|e| Error::provider("noise", format!("dial {endpoint}: {e}")))?;
        let transport = wire::client_handshake(&mut stream, self.keypair.secret.as_bytes()).await?;
        tracing::info!(node = %node.node_id, %endpoint, "noise session established");
        conns.insert(
            node.node_id.clone(),
            Arc::new(Mutex::new(Conn { stream, transport })),
        );
        Ok(())
    }

    async fn send(&self, node: &FabricNode, payload: &[u8]) -> Result<()> {
        // Every exchange is request/response (the server always replies); a
        // one-way send simply discards the reply.
        self.request(node, payload).await.map(|_| ())
    }
}

impl NoiseTransport {
    /// Send `payload` to `node` and return the peer's sealed response.
    ///
    /// This is the RPC primitive the Raft network layer is built on: one sealed
    /// request frame out, one sealed response frame back, over the cached Noise
    /// session.
    pub async fn request(&self, node: &FabricNode, payload: &[u8]) -> Result<Vec<u8>> {
        self.connect(node).await?;
        let conn = self
            .conns
            .lock()
            .await
            .get(&node.node_id)
            .cloned()
            .ok_or_else(|| Error::internal("noise session vanished"))?;

        let mut guard = conn.lock().await;
        let Conn { stream, transport } = &mut *guard;
        wire::send_sealed(stream, transport, payload).await?;
        let reply = wire::recv_opened(stream, transport).await?;
        tracing::debug!(
            node = %node.node_id,
            sent = payload.len(),
            recv = reply.len(),
            "noise rpc exchange"
        );
        Ok(reply)
    }
}

/// Register the built-in fabric transports into `reg`.
pub fn register_builtins(reg: &mut Registry<dyn FabricTransport>) -> Result<()> {
    reg.register("noise", Arc::new(NoiseTransport::new()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_builtins_registers_noise() {
        let mut reg: Registry<dyn FabricTransport> = Registry::new();
        register_builtins(&mut reg).unwrap();
        assert!(reg.contains("noise"));
    }

    #[tokio::test]
    async fn connect_to_unreachable_endpoint_errors() {
        let t = NoiseTransport::new();
        let node = FabricNode::from_keypair(
            &KeyPair::from_seed_name("peer"),
            // Port 1 on loopback: nothing listening.
            vec!["127.0.0.1:1".into()],
        );
        assert!(t.connect(&node).await.is_err());
    }
}
