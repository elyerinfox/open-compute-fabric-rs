//! An in-process Raft network: RPCs are routed directly to peer [`Raft`]
//! instances held in a shared registry.
//!
//! This lets a real multi-node Raft cluster run inside a single process (used by
//! the tests and for single-host clusters). Every node registers its `Raft`
//! handle into a shared [`Registry`]; a [`RaftNetworkFactory`] hands out
//! per-target [`InProcessNetwork`] clients that look the peer up and invoke its
//! receiving-side handler directly.
//!
//! For a cross-host cluster, use [`crate::fabric_net`] instead, which carries the
//! same RPCs over `ocf-fabric`'s encrypted Noise transport.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use openraft::error::InstallSnapshotError;
use openraft::error::NetworkError;
use openraft::error::RPCError;
use openraft::error::RaftError;
use openraft::error::RemoteError;
use openraft::network::RPCOption;
use openraft::network::RaftNetwork;
use openraft::network::RaftNetworkFactory;
use openraft::raft::AppendEntriesRequest;
use openraft::raft::AppendEntriesResponse;
use openraft::raft::InstallSnapshotRequest;
use openraft::raft::InstallSnapshotResponse;
use openraft::raft::VoteRequest;
use openraft::raft::VoteResponse;
use openraft::BasicNode;
use openraft::Raft;

use crate::types::TypeConfig;

type NodeId = u64;

/// A shared, process-wide registry of running Raft nodes, keyed by node id.
///
/// Nodes insert their [`Raft`] handle here once constructed; the network factory
/// reads from it to route RPCs. Cloning a `Registry` shares the same underlying
/// map (it is an `Arc`), so every node and every network client see the same
/// cluster.
#[derive(Clone, Default)]
pub struct Registry {
    nodes: Arc<Mutex<HashMap<NodeId, Raft<TypeConfig>>>>,
}

impl Registry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a node's Raft handle.
    pub fn insert(&self, id: NodeId, raft: Raft<TypeConfig>) {
        self.lock().insert(id, raft);
    }

    /// Remove a node from the registry (e.g. on shutdown).
    pub fn remove(&self, id: NodeId) {
        self.lock().remove(&id);
    }

    /// Look up a node's Raft handle, cloning it (cloning a `Raft` is cheap).
    fn get(&self, id: NodeId) -> Option<Raft<TypeConfig>> {
        self.lock().get(&id).cloned()
    }

    /// Lock the inner map, recovering from poisoning rather than panicking.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<NodeId, Raft<TypeConfig>>> {
        self.nodes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A [`RaftNetworkFactory`] producing in-process clients backed by a [`Registry`].
#[derive(Clone)]
pub struct InProcessNetworkFactory {
    registry: Registry,
}

impl InProcessNetworkFactory {
    /// Create a factory over a shared registry.
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }
}

impl RaftNetworkFactory<TypeConfig> for InProcessNetworkFactory {
    type Network = InProcessNetwork;

    async fn new_client(&mut self, target: NodeId, _node: &BasicNode) -> Self::Network {
        InProcessNetwork {
            target,
            registry: self.registry.clone(),
        }
    }
}

/// A network client that delivers RPCs to a single target node by calling its
/// receiving-side [`Raft`] handler in process.
pub struct InProcessNetwork {
    target: NodeId,
    registry: Registry,
}

impl InProcessNetwork {
    /// Resolve the target's Raft handle, or a transport-style `Unreachable`
    /// error if it is not currently registered (e.g. shut down).
    fn peer<E>(&self) -> Result<Raft<TypeConfig>, RPCError<NodeId, BasicNode, E>>
    where
        E: std::error::Error,
    {
        match self.registry.get(self.target) {
            Some(raft) => Ok(raft),
            None => Err(RPCError::Network(NetworkError::new(&PeerGone(self.target)))),
        }
    }
}

/// Error used when a target node is absent from the registry.
#[derive(Debug)]
struct PeerGone(NodeId);

impl std::fmt::Display for PeerGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "peer {} is not present in the in-process registry", self.0)
    }
}

impl std::error::Error for PeerGone {}

impl RaftNetwork<TypeConfig> for InProcessNetwork {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        let peer = self.peer()?;
        peer.append_entries(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        let peer = self.peer()?;
        peer.vote(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, BasicNode, RaftError<NodeId, InstallSnapshotError>>,
    > {
        let peer = self.peer()?;
        peer.install_snapshot(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}
