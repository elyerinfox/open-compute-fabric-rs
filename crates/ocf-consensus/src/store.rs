//! The [`ReplicatedStore`] facade — the public, ergonomic surface over a single
//! Raft node in the cluster.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use ocf_core::prelude::*;
use ocf_fabric::{FabricServer, NoiseTransport};
use ocf_store::StateStore;
use openraft::BasicNode;
use openraft::Config;
use openraft::Raft;
use tokio::sync::OnceCell;

use crate::fabric_net::{serve_raft, FabricRaftNetworkFactory, RaftHandle};
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

    /// Start a Raft node whose RPCs ride the **encrypted fabric** (cross-host),
    /// instead of the in-process router. `transport` carries this node's Noise
    /// identity; `server` is a bound [`FabricServer`] that this call begins serving
    /// inbound Raft RPCs on. Use [`initialize_cluster`](Self::initialize_cluster) /
    /// [`add_learner`](Self::add_learner) / [`change_membership`](Self::change_membership)
    /// to form the cluster, addressing peers by their fabric endpoints.
    pub async fn start_fabric(
        node_id: u64,
        store: Arc<dyn StateStore>,
        transport: Arc<NoiseTransport>,
        server: FabricServer,
    ) -> Result<Self> {
        // Wider election timing than the in-process cluster: real RPCs cross an
        // encrypted network, so allow more slack before calling an election.
        let config = Config {
            cluster_name: "ocf-consensus".to_string(),
            heartbeat_interval: 250,
            election_timeout_min: 1500,
            election_timeout_max: 3000,
            ..Default::default()
        };
        let config = Arc::new(
            config
                .validate()
                .map_err(|e| Error::internal(format!("invalid raft config: {e}")))?,
        );

        let (log_store, state_machine) = new_storage(store.clone());
        let network = FabricRaftNetworkFactory::new(transport);
        let raft = Raft::new(node_id, config, network, log_store, state_machine)
            .await
            .map_err(|e| Error::internal(format!("failed to start raft node {node_id}: {e}")))?;

        // Publish the handle so the server can dispatch inbound RPCs to it.
        let cell: RaftHandle = Arc::new(OnceCell::new());
        let _ = cell.set(raft.clone());
        tokio::spawn(serve_raft(server, cell));

        Ok(Self {
            node_id,
            raft,
            registry: Registry::new(), // unused on the fabric path
            store,
        })
    }

    /// Initialize a fabric cluster with `members` (id → fabric endpoint). Call on
    /// exactly one node; idempotent. Peers address each other by these endpoints.
    pub async fn initialize_cluster(&self, members: Vec<(u64, String)>) -> Result<()> {
        let nodes: BTreeMap<u64, BasicNode> = members
            .into_iter()
            .map(|(id, addr)| (id, BasicNode::new(addr)))
            .collect();
        match self.raft.initialize(nodes).await {
            Ok(()) => Ok(()),
            Err(e) if is_already_initialized(&e) => Ok(()),
            Err(e) => Err(Error::internal(format!("initialize cluster: {e}"))),
        }
    }

    /// Add `id` (reachable at fabric endpoint `addr`) as a learner — the leader
    /// streams it the log/snapshot until it catches up. Leader-only.
    pub async fn add_learner(&self, id: u64, addr: String) -> Result<()> {
        self.raft
            .add_learner(id, BasicNode::new(addr), true)
            .await
            .map_err(|e| Error::internal(format!("add_learner {id}: {e}")))?;
        Ok(())
    }

    /// Set the cluster's voter set (promotes caught-up learners / drops members).
    /// Leader-only; the change is itself committed by a quorum.
    pub async fn change_membership(&self, voters: BTreeSet<u64>) -> Result<()> {
        self.raft
            .change_membership(voters, false)
            .await
            .map_err(|e| Error::internal(format!("change_membership: {e}")))?;
        Ok(())
    }

    /// The current voter ids in the committed membership config.
    pub fn voters(&self) -> Vec<u64> {
        self.raft
            .metrics()
            .borrow()
            .membership_config
            .membership()
            .voter_ids()
            .collect()
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

#[cfg(test)]
mod fabric_cluster_tests {
    use super::*;
    use ocf_fabric::KeyPair;
    use ocf_store::MemoryStateStore;

    /// Bring up one Raft node served over a real loopback Noise/TCP fabric
    /// endpoint, the way the daemon does. Returns the store and its fabric address.
    async fn start_node(id: u64) -> (ReplicatedStore, String) {
        let kp = KeyPair::generate();
        let server = FabricServer::bind("127.0.0.1:0", kp.clone())
            .await
            .expect("bind");
        let addr = server.local_addr().to_string();
        let transport = Arc::new(NoiseTransport::with_keypair(kp));
        let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
        let rs = ReplicatedStore::start_fabric(id, store, transport, server)
            .await
            .expect("start_fabric");
        (rs, addr)
    }

    #[tokio::test]
    async fn fabric_cluster_forms_and_replicates() {
        let (n1, a1) = start_node(1).await;
        let (n2, a2) = start_node(2).await;
        let (n3, a3) = start_node(3).await;
        n1.initialize_cluster(vec![(1, a1), (2, a2), (3, a3)])
            .await
            .expect("init cluster");
        let leader_id = n1.wait_for_leader(Duration::from_secs(10)).await.expect("leader");

        let nodes = [&n1, &n2, &n3];
        let leader = nodes.iter().find(|n| n.node_id() == leader_id).unwrap();
        leader.put("kv", "k1", b"v1".to_vec()).await.expect("write on leader");

        // The write replicates to every node's state machine.
        for n in nodes {
            let mut ok = false;
            for _ in 0..60 {
                if n.get("kv", "k1").unwrap() == Some(b"v1".to_vec()) {
                    ok = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(ok, "node {} did not replicate the write", n.node_id());
        }
        for n in [n1, n2, n3] {
            n.shutdown().await;
        }
    }

    #[tokio::test]
    async fn follower_write_is_redirected_not_a_second_authority() {
        let (n1, a1) = start_node(1).await;
        let (n2, a2) = start_node(2).await;
        let (n3, a3) = start_node(3).await;
        n1.initialize_cluster(vec![(1, a1), (2, a2), (3, a3)])
            .await
            .expect("init");
        let leader_id = n1.wait_for_leader(Duration::from_secs(10)).await.expect("leader");
        let follower = [&n1, &n2, &n3]
            .into_iter()
            .find(|n| n.node_id() != leader_id)
            .unwrap();
        // A follower refuses the write (redirects to the leader) — there is never
        // a second writer.
        assert!(follower.put("kv", "x", b"y".to_vec()).await.is_err());
        for n in [n1, n2, n3] {
            n.shutdown().await;
        }
    }

    #[tokio::test]
    async fn minority_partition_cannot_commit() {
        let (n1, a1) = start_node(1).await;
        // A 3-member cluster whose other two members are unreachable: node 1 is a
        // minority and can never gather a quorum.
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            n1.initialize_cluster(vec![
                (1, a1),
                (2, "127.0.0.1:9".to_string()),
                (3, "127.0.0.1:10".to_string()),
            ]),
        )
        .await;
        // No quorum → no leader is elected and writes cannot commit. This is the
        // split-brain guarantee: a partitioned minority is inert.
        assert!(n1.wait_for_leader(Duration::from_secs(2)).await.is_err());
        assert!(n1.put("kv", "k", b"v".to_vec()).await.is_err());
        n1.shutdown().await;
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
