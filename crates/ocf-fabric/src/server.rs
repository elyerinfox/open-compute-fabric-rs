//! The fabric listener: accepts peer connections, completes the Noise handshake
//! as responder, and dispatches every decrypted frame to a handler.
//!
//! Pairing a [`FabricServer`] (inbound) with a [`crate::transport::NoiseTransport`]
//! (outbound) gives a node a real, mutually-authenticated, encrypted presence on
//! the mesh: anyone who can reach its TCP endpoint and completes the handshake
//! can deliver sealed frames, and the server learns each peer's authenticated
//! static public key.

use crate::crypto::{KeyPair, PublicKey};
use crate::wire;
use ocf_core::error::{Error, Result};
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

/// A bound fabric listener. Call [`FabricServer::run`] to start serving.
pub struct FabricServer {
    keypair: KeyPair,
    listener: TcpListener,
    local_addr: SocketAddr,
}

impl FabricServer {
    /// Bind a listener on `addr` (e.g. `"0.0.0.0:51820"` or `"127.0.0.1:0"` for
    /// an ephemeral port), presenting `keypair` as this node's identity.
    pub async fn bind(addr: impl tokio::net::ToSocketAddrs, keypair: KeyPair) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| Error::provider("noise", format!("bind: {e}")))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| Error::provider("noise", format!("local_addr: {e}")))?;
        Ok(FabricServer {
            keypair,
            listener,
            local_addr,
        })
    }

    /// The address the listener is actually bound to (resolves an ephemeral `:0`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Serve forever. For every accepted connection the handshake runs and then
    /// each decrypted request frame is passed to `handler` as
    /// `(peer_public_key, payload)`; the bytes it returns are sealed and sent
    /// back as the response. A one-way caller simply ignores the (often empty)
    /// reply.
    ///
    /// `handler` is shared across all connections; spawn `run` on a task.
    pub async fn run<F, Fut>(self, handler: F) -> Result<()>
    where
        F: Fn(PublicKey, Vec<u8>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Vec<u8>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let secret: Vec<u8> = self.keypair.secret.as_bytes().to_vec();
        tracing::info!(addr = %self.local_addr, "fabric server listening");
        loop {
            let (stream, peer) = self
                .listener
                .accept()
                .await
                .map_err(|e| Error::provider("noise", format!("accept: {e}")))?;
            let secret = secret.clone();
            let handler = Arc::clone(&handler);
            tokio::spawn(async move {
                if let Err(e) = serve_conn(stream, peer, &secret, handler).await {
                    tracing::debug!(%peer, error = %e, "fabric connection closed");
                }
            });
        }
    }
}

async fn serve_conn<F, Fut>(
    mut stream: TcpStream,
    peer: SocketAddr,
    local_secret: &[u8],
    handler: Arc<F>,
) -> Result<()>
where
    F: Fn(PublicKey, Vec<u8>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Vec<u8>> + Send + 'static,
{
    let (mut transport, remote_static) =
        wire::server_handshake(&mut stream, local_secret).await?;
    let peer_key = PublicKey::from_bytes(remote_static);
    tracing::info!(%peer, peer_key = %peer_key, "inbound noise session established");

    loop {
        match wire::recv_opened(&mut stream, &mut transport).await {
            Ok(request) => {
                let response = handler(peer_key.clone(), request).await;
                wire::send_sealed(&mut stream, &mut transport, &response).await?;
            }
            // Any read error (including a clean close) ends the connection.
            Err(_) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::FabricNode;
    use crate::transport::{FabricTransport, NoiseTransport};
    use std::time::Duration;

    #[tokio::test]
    async fn real_encrypted_roundtrip_and_mutual_auth() {
        // Stand up a server with a known identity on an ephemeral port.
        let server_kp = KeyPair::from_seed_name("server-node");
        let server = FabricServer::bind("127.0.0.1:0", server_kp.clone())
            .await
            .expect("bind");
        let addr = server.local_addr();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(server.run(move |pk, payload| {
            let tx = tx.clone();
            async move {
                let _ = tx.send((pk, payload));
                Vec::new()
            }
        }));

        // A client with its own identity dials the server and sends a sealed frame.
        let client = NoiseTransport::new();
        let client_pub = client.public_key().clone();
        let server_node =
            FabricNode::from_keypair(&server_kp, vec![addr.to_string()]);

        client
            .send(&server_node, b"hello mesh")
            .await
            .expect("send");

        let (peer_key, payload) = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");

        // The payload decrypted correctly end-to-end...
        assert_eq!(payload, b"hello mesh");
        // ...and the server authenticated the client's real static public key.
        assert_eq!(peer_key.as_bytes(), client_pub.as_bytes());
    }

    #[tokio::test]
    async fn encrypted_request_response_rpc() {
        // An echo-uppercase RPC server.
        let server_kp = KeyPair::from_seed_name("rpc-server");
        let server = FabricServer::bind("127.0.0.1:0", server_kp.clone())
            .await
            .expect("bind");
        let addr = server.local_addr();
        tokio::spawn(server.run(|_pk, req| async move {
            req.into_iter().map(|b| b.to_ascii_uppercase()).collect()
        }));

        let client = NoiseTransport::new();
        let server_node = FabricNode::from_keypair(&server_kp, vec![addr.to_string()]);

        let reply = tokio::time::timeout(
            Duration::from_secs(3),
            client.request(&server_node, b"raft-rpc"),
        )
        .await
        .expect("timed out")
        .expect("request");
        assert_eq!(reply, b"RAFT-RPC");
    }
}
