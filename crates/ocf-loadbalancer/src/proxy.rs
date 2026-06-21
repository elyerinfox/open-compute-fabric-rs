//! The real layer-4 data plane: a TCP load balancer.
//!
//! [`TcpLoadBalancer`] binds a [`tokio::net::TcpListener`] and, for every
//! accepted client connection, picks a [`Backend`] via the same policy-aware
//! [`select_backend`] routing core the controller uses, dials that backend with
//! [`TcpStream::connect`], and splices the two sockets together with
//! [`tokio::io::copy_bidirectional`]. This is the live forwarding path for
//! [`LbKind::Tcp`](crate::model::LbKind::Tcp): real bytes in, real bytes out, no
//! stub.
//!
//! The set of backends is supplied up front (a `Vec<Backend>`); a richer
//! deployment would refresh it from health checks, but keeping it fixed here
//! makes the data plane self-contained and easy to reason about.

use crate::model::{Backend, ClientContext, RoutingPolicy};
use crate::routing::select_backend;
use ocf_core::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

/// A bound TCP load balancer. Call [`TcpLoadBalancer::run`] to start serving.
///
/// Each accepted connection is routed independently: the client's peer address
/// becomes the [`ClientContext`] source IP, `policy` selects among `backends`,
/// and bytes are then piped bidirectionally until either side closes.
pub struct TcpLoadBalancer {
    listener: TcpListener,
    local_addr: SocketAddr,
    policy: RoutingPolicy,
    backends: Arc<Vec<Backend>>,
}

impl TcpLoadBalancer {
    /// Bind a load balancer on `addr` (e.g. `"0.0.0.0:8080"`, or `"127.0.0.1:0"`
    /// for an ephemeral port) that forwards to `backends` under `policy`.
    pub async fn bind(
        addr: impl tokio::net::ToSocketAddrs,
        policy: RoutingPolicy,
        backends: Vec<Backend>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| Error::provider("tcp_lb", format!("bind: {e}")))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| Error::provider("tcp_lb", format!("local_addr: {e}")))?;
        Ok(TcpLoadBalancer {
            listener,
            local_addr,
            policy,
            backends: Arc::new(backends),
        })
    }

    /// The address the listener is actually bound to (resolves an ephemeral `:0`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Serve forever, accepting connections and forwarding each to a selected
    /// backend on its own task.
    ///
    /// An individual connection that fails (no backend available, the backend
    /// refuses, or the splice errors) is logged and dropped; the listener keeps
    /// accepting. `run` only returns `Err` if accepting itself fails fatally.
    pub async fn run(self) -> Result<()> {
        tracing::info!(addr = %self.local_addr, "tcp load balancer listening");
        loop {
            let (client, peer) = self
                .listener
                .accept()
                .await
                .map_err(|e| Error::provider("tcp_lb", format!("accept: {e}")))?;
            let policy = self.policy;
            let backends = Arc::clone(&self.backends);
            tokio::spawn(async move {
                if let Err(e) = proxy_connection(client, peer, policy, &backends).await {
                    tracing::warn!(peer = %peer, error = %e, "tcp load balancer: connection failed");
                }
            });
        }
    }
}

/// Route one accepted client connection to a backend and splice the two sockets.
async fn proxy_connection(
    mut client: TcpStream,
    peer: SocketAddr,
    policy: RoutingPolicy,
    backends: &[Backend],
) -> Result<()> {
    let ctx = ClientContext::new().with_src_ip(peer.ip().to_string());
    let backend = select_backend(policy, backends, &ctx)
        .ok_or_else(|| Error::provider("tcp_lb", "no backend available"))?;

    let mut upstream = TcpStream::connect(&backend.address)
        .await
        .map_err(|e| Error::provider("tcp_lb", format!("connect {}: {e}", backend.address)))?;

    tracing::debug!(peer = %peer, backend = %backend.address, "tcp load balancer: forwarding");

    tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .map_err(|e| Error::provider("tcp_lb", format!("relay {}: {e}", backend.address)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A self-contained echo server: accepts one connection, reads `expect`
    /// bytes, writes them straight back, then returns. Bound on an ephemeral
    /// port; its address is returned so a backend can point at it.
    async fn spawn_echo() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Serve any number of connections for the lifetime of the test.
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(_) => return,
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    loop {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                if sock.write_all(&buf[..n]).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn proxy_round_trips_bytes_through_backend() {
        // Real echo backend on a real ephemeral port.
        let echo_addr = spawn_echo().await;
        let backend = Backend::new(
            Id::named("echo"),
            echo_addr.to_string(),
            Scope::fleet(),
        );

        // Real load balancer in front of it.
        let lb = TcpLoadBalancer::bind(
            "127.0.0.1:0",
            RoutingPolicy::RoundRobin,
            vec![backend],
        )
        .await
        .unwrap();
        let lb_addr = lb.local_addr();
        tokio::spawn(async move {
            let _ = lb.run().await;
        });

        // A real client connects *through* the proxy and expects an echo.
        let mut client = TcpStream::connect(lb_addr).await.unwrap();
        let payload = b"hello through the load balancer";
        client.write_all(payload).await.unwrap();

        let mut received = vec![0u8; payload.len()];
        client.read_exact(&mut received).await.unwrap();
        assert_eq!(&received, payload);
    }
}
