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
            // Disable Nagle on the inbound side too (request/response RPC).
            let _ = stream.set_nodelay(true);
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

/// A bound listener for **bulk streamed** transfers (separate from the
/// request/response [`FabricServer`], so large transfers don't share a socket
/// with control RPC). Each connection's decrypted record stream is drained into
/// a sink the caller supplies per peer (a file for a VM image, `tokio::io::sink`
/// to discard, a channel, …).
pub struct FabricStreamServer {
    keypair: KeyPair,
    listener: TcpListener,
    local_addr: SocketAddr,
}

impl FabricStreamServer {
    /// Bind a streaming listener on `addr`, presenting `keypair` as the identity.
    pub async fn bind(addr: impl tokio::net::ToSocketAddrs, keypair: KeyPair) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| Error::provider("noise", format!("bind: {e}")))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| Error::provider("noise", format!("local_addr: {e}")))?;
        Ok(FabricStreamServer {
            keypair,
            listener,
            local_addr,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Serve forever. For each connection: handshake, then drain the decrypted
    /// record stream into the sink `make_sink(peer_public_key)` returns.
    /// `compress` must match the sender (records are zstd-decompressed when set).
    pub async fn run<F, W>(self, compress: bool, make_sink: F) -> Result<()>
    where
        F: Fn(PublicKey) -> W + Send + Sync + 'static,
        W: tokio::io::AsyncWriteExt + Unpin + Send + 'static,
    {
        let make_sink = Arc::new(make_sink);
        let secret: Vec<u8> = self.keypair.secret.as_bytes().to_vec();
        tracing::info!(addr = %self.local_addr, compress, "fabric stream server listening");
        loop {
            let (mut stream, peer) = self
                .listener
                .accept()
                .await
                .map_err(|e| Error::provider("noise", format!("accept: {e}")))?;
            let _ = stream.set_nodelay(true);
            let secret = secret.clone();
            let make_sink = Arc::clone(&make_sink);
            tokio::spawn(async move {
                match wire::server_handshake(&mut stream, &secret).await {
                    Ok((mut transport, remote_static)) => {
                        let peer_key = PublicKey::from_bytes(remote_static);
                        let mut sink = make_sink(peer_key);
                        if let Err(e) =
                            wire::recv_stream(&mut stream, &mut transport, &mut sink, compress).await
                        {
                            tracing::debug!(%peer, error = %e, "stream receive ended");
                        }
                    }
                    Err(e) => tracing::debug!(%peer, error = %e, "stream handshake failed"),
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
    async fn streamed_transfer_roundtrips_large_payload() {
        use std::sync::Arc as StdArc;
        use tokio::sync::Mutex as TokioMutex;

        // A stream server that collects received bytes into a shared buffer.
        let server_kp = KeyPair::from_seed_name("stream-server");
        let server = FabricStreamServer::bind("127.0.0.1:0", server_kp.clone())
            .await
            .expect("bind");
        let addr = server.local_addr();
        let sink_buf: StdArc<TokioMutex<Vec<u8>>> = StdArc::new(TokioMutex::new(Vec::new()));
        let buf_for_server = sink_buf.clone();
        tokio::spawn(server.run(false, move |_pk| {
            // Each connection writes into the shared buffer via a cursor-like sink.
            let buf = buf_for_server.clone();
            VecSink { buf }
        }));

        // Stream ~5 MB (spans many 64 KB records) from a client.
        let payload: Vec<u8> = (0..5_000_000).map(|i| (i % 251) as u8).collect();
        let client = NoiseTransport::new();
        let server_node = FabricNode::from_keypair(&server_kp, vec![addr.to_string()]);
        let mut reader = std::io::Cursor::new(payload.clone());
        let sent = client
            .send_stream(&server_node, &mut reader, false)
            .await
            .expect("send_stream");
        assert_eq!(sent, payload.len() as u64);

        // The receiver reassembled the exact bytes.
        for _ in 0..50 {
            if sink_buf.lock().await.len() == payload.len() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(*sink_buf.lock().await, payload);
    }

    #[tokio::test]
    async fn compressed_stream_roundtrips_exactly() {
        use std::sync::Arc as StdArc;
        use tokio::sync::Mutex as TokioMutex;

        let server_kp = KeyPair::from_seed_name("zstd-server");
        let server = FabricStreamServer::bind("127.0.0.1:0", server_kp.clone())
            .await
            .expect("bind");
        let addr = server.local_addr();
        let sink_buf: StdArc<TokioMutex<Vec<u8>>> = StdArc::new(TokioMutex::new(Vec::new()));
        let buf_for_server = sink_buf.clone();
        // compress = true on the receive side.
        tokio::spawn(server.run(true, move |_pk| {
            let buf = buf_for_server.clone();
            VecSink { buf }
        }));

        // Highly compressible data (long runs) across many records.
        let mut payload = Vec::with_capacity(3_000_000);
        for i in 0..3_000_000u32 {
            payload.push(((i / 4096) % 7) as u8); // long constant runs → compresses well
        }
        let client = NoiseTransport::new();
        let node = FabricNode::from_keypair(&server_kp, vec![addr.to_string()]);
        let mut reader = std::io::Cursor::new(payload.clone());
        let sent = client
            .send_stream(&node, &mut reader, true)
            .await
            .expect("send_stream compressed");
        assert_eq!(sent, payload.len() as u64);

        for _ in 0..50 {
            if sink_buf.lock().await.len() == payload.len() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Decompressed bytes must equal the original exactly.
        assert_eq!(*sink_buf.lock().await, payload);
    }

    /// A tiny `AsyncWrite` that appends into a shared `Vec` (test sink).
    struct VecSink {
        buf: std::sync::Arc<tokio::sync::Mutex<Vec<u8>>>,
    }
    impl tokio::io::AsyncWrite for VecSink {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            data: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            // Lock is uncontended in the test; try_lock keeps poll non-blocking.
            if let Ok(mut g) = self.buf.try_lock() {
                g.extend_from_slice(data);
                std::task::Poll::Ready(Ok(data.len()))
            } else {
                _cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn measures_and_records_peer_rtt() {
        use crate::membership::Membership;
        use crate::NodeId;
        use std::time::Instant;

        // A peer that answers pings (echo).
        let kp = KeyPair::from_seed_name("rtt-peer");
        let server = FabricServer::bind("127.0.0.1:0", kp.clone()).await.expect("bind");
        let addr = server.local_addr();
        tokio::spawn(server.run(|_pk, req| async move { req }));
        let node = FabricNode::from_keypair(&kp, vec![addr.to_string()]);
        let node_id = node.node_id.clone();

        let membership = Membership::new(NodeId::new("self"));
        membership.join(node.clone());

        // Time a ping round-trip and record it — the latency-probe mechanism.
        let client = NoiseTransport::new();
        let t = Instant::now();
        client.request(&node, b"ping").await.expect("ping");
        let rtt_ms = t.elapsed().as_secs_f64() * 1000.0;
        membership.record_rtt(&node_id, rtt_ms);

        let recorded = membership.rtt(&node_id).expect("rtt recorded");
        assert!(recorded >= 0.0 && recorded < 5000.0, "sane loopback RTT: {recorded}");
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
