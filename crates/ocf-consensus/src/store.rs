//! The [`ReplicatedStore`] facade — the public, ergonomic surface over a single
//! Raft node in the cluster.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ocf_core::prelude::*;
use ocf_store::StateStore;
use openraft::BasicNode;
use openraft::Config;
use openraft::Raft;

use crate::network::InProcessNetworkFactory;
use crate::network::Registry;
use crate::storage::new_storage;
use crate::types::KvCommand;
use crate::types::KvResponse;
use crate::types::TypeConfig;

/// A handle to one node of a Raft-replicated key/value control-plane store.
///
/// Writes go through Raft (`put`/`delete`); once committed they are applied into
/// every node's [`StateStore`]. Reads (`get`) are served locally from this
/// node's state-machine store — they are eventually consistent on followers and
/// linearizable-after-commit on the leader.
#[derive(Clone)]
pub struct ReplicatedStore {
    node_id: u64,
    raft: Raft<TypeConfig>,
    registry: Registry,
    store: Arc<dyn StateStore>,
}

impl ReplicatedStore {
    /// Start a Raft node `node_id` whose cluster is `peers` (the full set of
    /// member ids, including `node_id` itself), applying committed writes into
    /// `store`.
    ///
    /// All nodes in one process must share the same [`Registry`]; this
    /// convenience constructor creates a fresh per-node registry, so for a real
    /// multi-node cluster use [`ReplicatedStore::start_in`] with a shared
    /// registry instead. Exactly one node should then call
    /// [`ReplicatedStore::initialize`] to form the cluster.
    pub async fn start(
        node_id: u64,
        peers: Vec<u64>,
        store: Arc<dyn StateStore>,
    ) -> Result<Self> {
        Self::start_in(node_id, peers, store, Registry::new()).await
    }

    /// Like [`ReplicatedStore::start`] but joins the shared in-process
    /// `registry`, so several nodes built against the same registry form one
    /// cluster.
    pub async fn start_in(
        node_id: u64,
        peers: Vec<u64>,
        store: Arc<dyn StateStore>,
        registry: Registry,
    ) -> Result<Self> {
        // `peers` is the intended membership; it is recorded for callers but the
        // membership is actually established by `initialize`. Keep it referenced
        // so the signature stays honest about who the cluster is.
        let _ = &peers;

        let config = Config {
            cluster_name: "ocf-consensus".to_string(),
            // Snappy election timing so an in-process cluster forms quickly; a
            // real deployment would widen these to tolerate network latency.
            heartbeat_interval: 100,
            election_timeout_min: 300,
            election_timeout_max: 600,
            ..Default::default()
        };
        let config = config
            .validate()
            .map_err(|e| Error::internal(format!("invalid raft config: {e}")))?;
        let config = Arc::new(config);

        let (log_store, state_machine) = new_storage(store.clone());
        let network = InProcessNetworkFactory::new(registry.clone());

        let raft = Raft::new(node_id, config, network, log_store, state_machine)
            .await
            .map_err(|e| Error::internal(format!("failed to start raft node {node_id}: {e}")))?;

        registry.insert(node_id, raft.clone());

        Ok(Self {
            node_id,
            raft,
            registry,
            store,
        })
    }

    /// Initialize the cluster with `members` (id -> address). Call this on
    /// exactly one node once every member node has been started against the
    /// shared registry. Returns `Ok(())` if the cluster is already initialized.
    pub async fn initialize(&self, members: Vec<u64>) -> Result<()> {
        let nodes: BTreeMap<u64, BasicNode> = members
            .into_iter()
            .map(|id| (id, BasicNode::new(format!("in-process://{id}"))))
            .collect();

        match self.raft.initialize(nodes).await {
            Ok(()) => Ok(()),
            // An already-formed cluster reports `NotAllowed`; that is success
            // for an idempotent initialize.
            Err(e) if is_already_initialized(&e) => Ok(()),
            Err(e) => Err(Error::internal(format!(
                "failed to initialize raft cluster: {e}"
            ))),
        }
    }

    /// This node's id.
    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    /// The shared registry this node belongs to (clone it to start more nodes in
    /// the same cluster).
    pub fn registry(&self) -> Registry {
        self.registry.clone()
    }

    /// Propose a `put`. Succeeds only on the leader; on a follower it returns an
    /// error naming the current leader so the caller can redirect.
    pub async fn put(&self, collection: &str, key: &str, value: Vec<u8>) -> Result<KvResponse> {
        self.write(KvCommand::Put {
            collection: collection.to_string(),
            key: key.to_string(),
            value,
        })
        .await
    }

    /// Propose a `delete`. Leader-only, like [`ReplicatedStore::put`].
    pub async fn delete(&self, collection: &str, key: &str) -> Result<KvResponse> {
        self.write(KvCommand::Delete {
            collection: collection.to_string(),
            key: key.to_string(),
        })
        .await
    }

    /// Submit a command through Raft and wait for it to be committed + applied,
    /// returning the apply acknowledgement.
    async fn write(&self, command: KvCommand) -> Result<KvResponse> {
        match self.raft.client_write(command).await {
            Ok(resp) => Ok(resp.data),
            Err(e) => {
                // Surface a leader-redirect hint when we are not the leader.
                if let Some(leader) = self.leader() {
                    if leader != self.node_id {
                        return Err(Error::Conflict(format!(
                            "not leader; current leader is node {leader}: {e}"
                        )));
                    }
                }
                Err(Error::internal(format!("raft client_write failed: {e}")))
            }
        }
    }

    /// Read a value from this node's local state-machine store.
    ///
    /// Reads are not routed through Raft; on a follower the value reflects the
    /// last entry replicated and applied locally (eventually consistent).
    pub fn get(&self, collection: &str, key: &str) -> Result<Option<Vec<u8>>> {
        self.store.get(collection, key)
    }

    /// `true` if this node currently believes it is the leader.
    pub fn is_leader(&self) -> bool {
        self.leader() == Some(self.node_id)
    }

    /// The current leader's node id, if one is known.
    pub fn leader(&self) -> Option<u64> {
        self.raft.metrics().borrow().current_leader
    }

    /// Block (up to `timeout`) until a leader has been elected, returning its id.
    pub async fn wait_for_leader(&self, timeout: Duration) -> Result<u64> {
        let metrics = self
            .raft
            .wait(Some(timeout))
            .metrics(
                |m| m.current_leader.is_some(),
                "wait_for_leader: a leader is elected",
            )
            .await
            .map_err(|e| Error::internal(format!("timed out waiting for leader: {e}")))?;

        metrics
            .current_leader
            .ok_or_else(|| Error::internal("leader vanished after election".to_string()))
    }

    /// Gracefully shut this node's Raft task down and deregister it.
    pub async fn shutdown(&self) {
        if let Err(e) = self.raft.shutdown().await {
            tracing::warn!(node_id = self.node_id, error = %e, "raft node shutdown returned an error");
        }
        self.registry.remove(self.node_id);
    }

    /// Access the underlying state-machine store (for read-only inspection,
    /// e.g. asserting replication landed in tests).
    pub fn state_store(&self) -> Arc<dyn StateStore> {
        self.store.clone()
    }
}

/// Detect openraft's "cluster already initialized" signal so `initialize` is
/// idempotent.
fn is_already_initialized(
    e: &openraft::error::RaftError<u64, openraft::error::InitializeError<u64, BasicNode>>,
) -> bool {
    matches!(
        e,
        openraft::error::RaftError::APIError(openraft::error::InitializeError::NotAllowed(_))
    )
}
