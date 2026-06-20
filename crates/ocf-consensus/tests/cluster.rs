//! Integration tests for a real, in-process 3-node Raft cluster.
//!
//! These exercise the whole stack: the in-process network routing RPCs between
//! peer `Raft` instances, the in-memory log, and the state machine applying
//! committed `KvCommand`s into each node's `ocf_store::StateStore`.

use std::sync::Arc;
use std::time::Duration;

use ocf_consensus::network::Registry;
use ocf_consensus::ReplicatedStore;
use ocf_store::MemoryStateStore;
use ocf_store::StateStore;

/// Build a `node_count`-node cluster sharing one in-process registry, initialize
/// it, and wait for a leader to be elected. Returns the nodes (index 0..n).
async fn start_cluster(node_count: u64) -> Vec<ReplicatedStore> {
    let registry = Registry::new();
    let members: Vec<u64> = (1..=node_count).collect();

    let mut nodes = Vec::new();
    for id in &members {
        let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
        let node = ReplicatedStore::start_in(*id, members.clone(), store, registry.clone())
            .await
            .expect("start node");
        nodes.push(node);
    }

    // Form the cluster from a single node.
    nodes[0]
        .initialize(members.clone())
        .await
        .expect("initialize cluster");

    // Wait for a leader on node 1.
    nodes[0]
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader elected");

    nodes
}

/// Poll until `cond` holds or `timeout` elapses. Returns whether it became true.
async fn eventually<F>(timeout: Duration, mut cond: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn three_node_cluster_replicates_a_write_to_all_nodes() {
    let nodes = start_cluster(3).await;

    // Find the leader and write through it.
    let leader_id = nodes[0]
        .leader()
        .expect("a leader id is known after election");
    let leader = nodes
        .iter()
        .find(|n| n.node_id() == leader_id)
        .expect("leader node is in the set");

    let resp = leader
        .put("workloads", "w1", b"replicated-spec".to_vec())
        .await
        .expect("leader write succeeds");
    assert!(resp.applied, "committed write must report applied");

    // The value must become readable from ALL THREE nodes' state-machine stores.
    for node in &nodes {
        let node = node.clone();
        let replicated = eventually(Duration::from_secs(10), || {
            matches!(
                node.get("workloads", "w1"),
                Ok(Some(ref v)) if v == b"replicated-spec"
            )
        })
        .await;
        assert!(
            replicated,
            "value did not replicate to node {}",
            node.node_id()
        );
    }

    for node in &nodes {
        node.shutdown().await;
    }
}

#[tokio::test]
async fn write_returns_committed_ack_and_value_is_readable() {
    let nodes = start_cluster(3).await;

    let leader_id = nodes[0].leader().expect("leader known");
    let leader = nodes
        .iter()
        .find(|n| n.node_id() == leader_id)
        .expect("leader in set");

    // A committed write returns an ack...
    let ack = leader
        .put("config", "k", b"v1".to_vec())
        .await
        .expect("write ok");
    assert!(ack.applied, "ack must indicate the entry was applied");

    // ...and the committed value is immediately readable on the leader.
    assert_eq!(
        leader.get("config", "k").expect("get ok"),
        Some(b"v1".to_vec()),
        "leader must read its own committed write"
    );

    // A delete is likewise replicated and observable everywhere.
    leader.delete("config", "k").await.expect("delete ok");
    for node in &nodes {
        let node = node.clone();
        let gone = eventually(Duration::from_secs(10), || {
            matches!(node.get("config", "k"), Ok(None))
        })
        .await;
        assert!(gone, "delete did not replicate to node {}", node.node_id());
    }

    for node in &nodes {
        node.shutdown().await;
    }
}

#[tokio::test]
async fn follower_write_is_rejected_with_leader_hint() {
    let nodes = start_cluster(3).await;

    let leader_id = nodes[0].leader().expect("leader known");
    // Pick a node that is NOT the leader.
    let follower = nodes
        .iter()
        .find(|n| n.node_id() != leader_id)
        .expect("a follower exists in a 3-node cluster");

    let err = follower
        .put("config", "x", b"y".to_vec())
        .await
        .expect_err("a follower must not accept a write");
    let msg = err.to_string();
    assert!(
        msg.contains("leader"),
        "follower write error should name the leader, got: {msg}"
    );

    for node in &nodes {
        node.shutdown().await;
    }
}
