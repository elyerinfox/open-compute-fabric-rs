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
        // Disable Nagle: this is a request/response RPC transport, so batching
        // small writes only adds latency.
        let _ = stream.set_nodelay(true);
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
    /// Stream all of `reader` to `node` over a **dedicated** encrypted connection
    /// (separate from the cached request/response session, so a multi-GB bulk
    /// transfer never blocks control-plane RPC). Returns the bytes sent.
    ///
    /// The data is chunked into pipelined Noise records (see [`wire::send_stream`]),
    /// so throughput is bounded by the cipher and the link, not by round-trips —
    /// this is what makes VM-migration-sized transfers practical over the fabric.
    /// When `compress` is set, records are zstd-compressed before sealing (a large
    /// win for memory/disk images). The receiver
    /// ([`FabricStreamServer`](crate::server::FabricStreamServer)) must run with
    /// the same `compress` flag.
    pub async fn send_stream<R>(
        &self,
        node: &FabricNode,
        reader: &mut R,
        compress: bool,
    ) -> Result<u64>
    where
        R: tokio::io::AsyncReadExt + Unpin,
    {
        let endpoint = node
            .primary_endpoint()
            .ok_or_else(|| Error::invalid(format!("node {} has no endpoint", node.node_id)))?
            .to_string();
        let mut stream = TcpStream::connect(&endpoint)
            .await
            .map_err(|e| Error::provider("noise", format!("dial {endpoint}: {e}")))?;
        let _ = stream.set_nodelay(true);
        let mut transport = wire::client_handshake(&mut stream, self.keypair.secret.as_bytes()).await?;
        let total = wire::send_stream(&mut stream, &mut transport, reader, compress).await?;
        tracing::debug!(node = %node.node_id, bytes = total, compress, "streamed bulk transfer");
        Ok(total)
    }

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

    /// Send `payload` to a `target` that isn't directly dialable, by forwarding
    /// through a `relay` node. The relay must be running a forwarding handler
    /// (see [`forward_relayed`](NoiseTransport::forward_relayed)). The end-to-end
    /// reply is returned. This is how a `Private`/NAT'd peer is reached.
    ///
    /// Note the relay sees ciphertext between itself and the target only at the
    /// transport layer — it forwards the *request bytes*, so end-to-end payload
    /// confidentiality from a relay would require an additional inner seal; today
    /// the relay is trusted fleet infrastructure.
    pub async fn request_via_relay(
        &self,
        relay: &FabricNode,
        target: &FabricNode,
        payload: &[u8],
    ) -> Result<Vec<u8>> {
        let endpoint = target
            .primary_endpoint()
            .ok_or_else(|| Error::invalid("relay target has no endpoint"))?
            .to_string();
        let envelope = RelayEnvelope {
            target_endpoint: endpoint,
            target_pubkey: target.public_key.as_bytes().to_vec(),
            payload: payload.to_vec(),
        };
        let bytes = serde_json::to_vec(&envelope)
            .map_err(|e| Error::invalid(format!("encode relay envelope: {e}")))?;
        self.request(relay, &bytes).await
    }

    /// Relay-side forwarding: decode a [`RelayEnvelope`] received as a request,
    /// dial the enclosed target, forward the inner payload, and return its reply.
    /// Wire this as a relay node's request handler.
    pub async fn forward_relayed(&self, request_bytes: &[u8]) -> Result<Vec<u8>> {
        let envelope: RelayEnvelope = serde_json::from_slice(request_bytes)
            .map_err(|e| Error::invalid(format!("decode relay envelope: {e}")))?;
        let target = FabricNode::new(
            NodeId::new("relayed-target"),
            crate::crypto::PublicKey::from_bytes(envelope.target_pubkey),
            vec![envelope.target_endpoint],
        );
        self.request(&target, &envelope.payload).await
    }
}

/// A request to a relay: forward `payload` to the node at `target_endpoint`
/// (authenticated by `target_pubkey`) and return its reply.
#[derive(serde::Serialize, serde::Deserialize)]
struct RelayEnvelope {
    target_endpoint: String,
    target_pubkey: Vec<u8>,
    payload: Vec<u8>,
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

    #[tokio::test]
    async fn relayed_request_reaches_private_target() {
        use crate::node::Reachability;
        use crate::server::FabricServer;

        // Target: a private node that only echoes (stands in for a NAT'd peer
        // reachable from the relay's network but not from the origin).
        let target_kp = KeyPair::from_seed_name("relay-target");
        let target_srv = FabricServer::bind("127.0.0.1:0", target_kp.clone())
            .await
            .expect("bind target");
        let target_addr = target_srv.local_addr();
        tokio::spawn(target_srv.run(|_pk, req| async move { req }));
        let target = FabricNode::from_keypair(&target_kp, vec![target_addr.to_string()])
            .with_reachability(Reachability::Private);

        // Relay: forwards whatever it receives to the enclosed target.
        let relay_kp = KeyPair::from_seed_name("relay-node");
        let relay_srv = FabricServer::bind("127.0.0.1:0", relay_kp.clone())
            .await
            .expect("bind relay");
        let relay_addr = relay_srv.local_addr();
        let relay_transport = Arc::new(NoiseTransport::with_keypair(relay_kp.clone()));
        let rt = relay_transport.clone();
        tokio::spawn(relay_srv.run(move |_pk, req| {
            let rt = rt.clone();
            async move { rt.forward_relayed(&req).await.unwrap_or_default() }
        }));
        let relay = FabricNode::from_keypair(&relay_kp, vec![relay_addr.to_string()])
            .with_reachability(Reachability::Relay);

        // Origin reaches the private target *through* the relay.
        let origin = NoiseTransport::new();
        let reply = origin
            .request_via_relay(&relay, &target, b"hello-private")
            .await
            .expect("relayed request");
        assert_eq!(reply, b"hello-private");
    }
}
