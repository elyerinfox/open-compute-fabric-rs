//! A Raft network that carries RPCs over the **encrypted fabric transport**.
//!
//! This is the real, cross-host counterpart to [`crate::network`]'s in-process
//! router. Each Raft RPC (append-entries, vote, install-snapshot) is serialized,
//! sent to the target node's fabric endpoint over a Noise session
//! ([`ocf_fabric::NoiseTransport::request`]), and the typed result is
//! deserialized on return — so a real multi-node cluster forms over
//! authenticated, encrypted TCP.
//!
//! Wiring per node: bind a [`FabricServer`], run [`serve_raft`] against a
//! [`RaftHandle`] cell, build the local `Raft` with a [`FabricRaftNetworkFactory`]
//! (sharing the node's [`NoiseTransport`] identity), then publish the `Raft`
//! handle into the cell so the server can dispatch inbound RPCs to it.

use std::sync::Arc;

use openraft::error::{InstallSnapshotError, NetworkError, RPCError, RaftError, RemoteError};
use openraft::network::{RaftNetwork, RaftNetworkFactory, RPCOption};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::{BasicNode, Raft};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use ocf_fabric::{FabricNode, FabricServer, NoiseTransport, NodeId as FabricNodeId, PublicKey};

use crate::types::TypeConfig;

type NodeId = u64;

/// A cell holding a node's live [`Raft`] handle once it has been constructed.
///
/// The server needs the handle to dispatch inbound RPCs, but the handle is built
/// *after* the network; the cell bridges that ordering. Inbound RPCs that arrive
/// before the handle is published are answered with a transient "not ready"
/// error, which openraft retries.
pub type RaftHandle = Arc<OnceCell<Raft<TypeConfig>>>;

/// The request envelope sent over the wire.
#[derive(Serialize, Deserialize)]
enum RpcRequest {
    Append(AppendEntriesRequest<TypeConfig>),
    Vote(VoteRequest<NodeId>),
    Snapshot(InstallSnapshotRequest<TypeConfig>),
}

/// The response envelope: the peer's *typed* `Result`, serialized verbatim so the
/// caller can reconstruct the exact success value or `RaftError` and map it as if
/// the call had been local.
#[derive(Serialize, Deserialize)]
enum RpcResponse {
    Append(Result<AppendEntriesResponse<NodeId>, RaftError<NodeId>>),
    Vote(Result<VoteResponse<NodeId>, RaftError<NodeId>>),
    Snapshot(Result<InstallSnapshotResponse<NodeId>, RaftError<NodeId, InstallSnapshotError>>),
    /// The receiving node's Raft handle was not yet published.
    NotReady,
}

/// Serve inbound Raft RPCs for one node: handshake (handled by [`FabricServer`]),
/// then dispatch each decrypted request to the local `Raft` published in `cell`.
pub async fn serve_raft(server: FabricServer, cell: RaftHandle) {
    let result = server
        .run(move |_peer_key, bytes| {
            let cell = cell.clone();
            async move {
                let response = dispatch(&cell, &bytes).await;
                serde_json::to_vec(&response).unwrap_or_default()
            }
        })
        .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "fabric raft server stopped");
    }
}

async fn dispatch(cell: &RaftHandle, bytes: &[u8]) -> RpcResponse {
    let raft = match cell.get() {
        Some(raft) => raft,
        None => return RpcResponse::NotReady,
    };
    let request: RpcRequest = match serde_json::from_slice(bytes) {
        Ok(req) => req,
        Err(e) => {
            // A malformed frame is treated as a transient transport fault; the
            // caller maps `NotReady` onto a retryable network error.
            tracing::warn!(error = %e, "undecodable raft rpc frame");
            return RpcResponse::NotReady;
        }
    };
    match request {
        RpcRequest::Append(rpc) => RpcResponse::Append(raft.append_entries(rpc).await),
        RpcRequest::Vote(rpc) => RpcResponse::Vote(raft.vote(rpc).await),
        RpcRequest::Snapshot(rpc) => RpcResponse::Snapshot(raft.install_snapshot(rpc).await),
    }
}

/// Builds [`FabricRaftNetwork`] clients that dial peers over a shared
/// [`NoiseTransport`] identity.
#[derive(Clone)]
pub struct FabricRaftNetworkFactory {
    transport: Arc<NoiseTransport>,
}

impl FabricRaftNetworkFactory {
    /// Create a factory whose clients send from `transport`'s node identity.
    pub fn new(transport: Arc<NoiseTransport>) -> Self {
        Self { transport }
    }
}

impl RaftNetworkFactory<TypeConfig> for FabricRaftNetworkFactory {
    type Network = FabricRaftNetwork;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> Self::Network {
        FabricRaftNetwork {
            transport: self.transport.clone(),
            target,
            addr: node.addr.clone(),
        }
    }
}

/// A network client that delivers RPCs to one peer over the encrypted transport.
pub struct FabricRaftNetwork {
    transport: Arc<NoiseTransport>,
    target: NodeId,
    addr: String,
}

impl FabricRaftNetwork {
    /// The peer as a dialable fabric node. The Noise XX handshake learns the
    /// peer's static key during connection, so only the endpoint is needed here.
    fn peer_node(&self) -> FabricNode {
        FabricNode::new(
            FabricNodeId::from(self.addr.as_str()),
            PublicKey::from_bytes(Vec::new()),
            vec![self.addr.clone()],
        )
    }

    /// Serialize `req`, exchange it over the transport, and deserialize the
    /// response envelope. Transport / codec failures become `Unreachable`.
    async fn exchange<E>(&self, req: &RpcRequest) -> Result<RpcResponse, RPCError<NodeId, BasicNode, E>>
    where
        E: std::error::Error,
    {
        let bytes = serde_json::to_vec(req).map_err(|e| net_err(format!("encode: {e}")))?;
        let reply = self
            .transport
            .request(&self.peer_node(), &bytes)
            .await
            .map_err(|e| net_err(format!("transport: {e}")))?;
        serde_json::from_slice(&reply).map_err(|e| net_err(format!("decode: {e}")))
    }
}

impl RaftNetwork<TypeConfig> for FabricRaftNetwork {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        match self.exchange(&RpcRequest::Append(rpc)).await? {
            RpcResponse::Append(Ok(resp)) => Ok(resp),
            RpcResponse::Append(Err(e)) => Err(RPCError::RemoteError(RemoteError::new(self.target, e))),
            RpcResponse::NotReady => Err(net_err("peer raft not ready")),
            _ => Err(net_err("unexpected response kind")),
        }
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        match self.exchange(&RpcRequest::Vote(rpc)).await? {
            RpcResponse::Vote(Ok(resp)) => Ok(resp),
            RpcResponse::Vote(Err(e)) => Err(RPCError::RemoteError(RemoteError::new(self.target, e))),
            RpcResponse::NotReady => Err(net_err("peer raft not ready")),
            _ => Err(net_err("unexpected response kind")),
        }
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, BasicNode, RaftError<NodeId, InstallSnapshotError>>,
    > {
        match self.exchange(&RpcRequest::Snapshot(rpc)).await? {
            RpcResponse::Snapshot(Ok(resp)) => Ok(resp),
            RpcResponse::Snapshot(Err(e)) => {
                Err(RPCError::RemoteError(RemoteError::new(self.target, e)))
            }
            RpcResponse::NotReady => Err(net_err("peer raft not ready")),
            _ => Err(net_err("unexpected response kind")),
        }
    }
}

/// Build a transport-level (`Unreachable`/`Network`) RPC error from a message.
fn net_err<E>(msg: impl Into<String>) -> RPCError<NodeId, BasicNode, E>
where
    E: std::error::Error,
{
    RPCError::Network(NetworkError::new(&FabricRpcError(msg.into())))
}

#[derive(Debug)]
struct FabricRpcError(String);

impl std::fmt::Display for FabricRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FabricRpcError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::new_storage;
    use crate::types::KvCommand;
    use ocf_fabric::KeyPair;
    use ocf_store::{MemoryStateStore, StateStore};
    use openraft::Config;
    use std::collections::BTreeMap;
    use std::time::Duration;

    /// Bring up one Raft node served over a real Noise/TCP fabric endpoint.
    async fn start_node(
        id: u64,
        store: Arc<dyn StateStore>,
    ) -> (Raft<TypeConfig>, String, RaftHandle) {
        let kp = KeyPair::generate();
        let server = FabricServer::bind("127.0.0.1:0", kp.clone())
            .await
            .expect("bind fabric server");
        let addr = server.local_addr().to_string();

        let cell: RaftHandle = Arc::new(OnceCell::new());
        tokio::spawn(serve_raft(server, cell.clone()));

        let transport = Arc::new(NoiseTransport::with_keypair(kp));
        let factory = FabricRaftNetworkFactory::new(transport);

        let config = Config {
            cluster_name: "ocf-fabric-raft".to_string(),
            heartbeat_interval: 200,
            election_timeout_min: 600,
            election_timeout_max: 1200,
            ..Default::default()
        }
        .validate()
        .expect("valid config");

        let (log_store, state_machine) = new_storage(store);
        let raft = Raft::new(id, Arc::new(config), factory, log_store, state_machine)
            .await
            .expect("build raft");
        cell.set(raft.clone()).map_err(|_| ()).expect("publish raft handle");
        (raft, addr, cell)
    }

    #[tokio::test]
    async fn three_node_cluster_replicates_over_encrypted_fabric() {
        let stores: Vec<Arc<dyn StateStore>> = (0..3)
            .map(|_| Arc::new(MemoryStateStore::new()) as Arc<dyn StateStore>)
            .collect();

        let mut rafts = Vec::new();
        let mut members: BTreeMap<u64, BasicNode> = BTreeMap::new();
        let mut cells = Vec::new();
        for id in 1..=3u64 {
            let (raft, addr, cell) = start_node(id, stores[(id - 1) as usize].clone()).await;
            members.insert(id, BasicNode::new(addr));
            rafts.push(raft);
            cells.push(cell); // keep the handles alive for the duration of the test
        }

        // Form the cluster from node 1.
        rafts[0]
            .initialize(members.clone())
            .await
            .expect("initialize cluster");

        // Wait for an election to complete over the encrypted transport.
        let metrics = rafts[0]
            .wait(Some(Duration::from_secs(15)))
            .metrics(|m| m.current_leader.is_some(), "a leader is elected")
            .await
            .expect("leader elected");
        let leader_id = metrics.current_leader.expect("leader id");

        // Write on the leader.
        let leader = rafts[(leader_id - 1) as usize].clone();
        leader
            .client_write(KvCommand::Put {
                collection: "workloads".to_string(),
                key: "db-1".to_string(),
                value: b"running".to_vec(),
            })
            .await
            .expect("committed write");

        // The committed value must replicate to ALL three nodes' state machines.
        for (i, store) in stores.iter().enumerate() {
            let mut replicated = false;
            for _ in 0..50 {
                if store.get("workloads", "db-1").expect("get") == Some(b"running".to_vec()) {
                    replicated = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(replicated, "node {} never received the replicated write", i + 1);
        }

        let _ = &cells;
        for raft in rafts {
            let _ = raft.shutdown().await;
        }
    }
}
