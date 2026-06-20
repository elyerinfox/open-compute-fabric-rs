//! # ocf-consensus
//!
//! Raft-replicated control-plane state for **Open Compute Fabric**, built on
//! [openraft] 0.9.
//!
//! Node-local durability is provided by `ocf-store`'s [`StateStore`]; this crate
//! is the other half of fleet persistence — it replicates writes across nodes so
//! the control plane survives losing any single node. A [`ReplicatedStore`] is a
//! handle to one Raft node: writes (`put`/`delete`) are proposed through the
//! Raft log, and once a quorum commits them every node's state machine applies
//! them into its local [`StateStore`]. Reads are served from the local store.
//!
//! ## Pieces
//!
//! * [`types`] — the [`TypeConfig`], the replicated [`KvCommand`], and the
//!   [`KvResponse`] acknowledgement.
//! * [`storage`] — an in-memory Raft log ([`storage::LogStore`]) and a state
//!   machine ([`storage::StateMachineStore`]) that applies committed commands
//!   into a [`StateStore`], including snapshot build/install over the store.
//! * [`network`] — an in-process [`network::Registry`] of peer Raft handles plus
//!   a [`network::InProcessNetworkFactory`] that routes RPCs between them, so a
//!   real multi-node cluster runs in one process (used for single-host clusters
//!   and tests).
//! * [`fabric_net`] — the real cross-host transport: a [`fabric_net::FabricRaftNetworkFactory`]
//!   that carries every RPC over `ocf-fabric`'s encrypted Noise transport, plus
//!   [`fabric_net::serve_raft`] for the receiving side.
//! * [`store`] — the [`ReplicatedStore`] facade.
//!
//! ## Example
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use ocf_consensus::{ReplicatedStore, network::Registry};
//! # use ocf_store::{MemoryStateStore, StateStore};
//! # async fn run() -> ocf_core::error::Result<()> {
//! let registry = Registry::new();
//! let store: Arc<dyn StateStore> = Arc::new(MemoryStateStore::new());
//! let node = ReplicatedStore::start_in(1, vec![1], store, registry).await?;
//! node.initialize(vec![1]).await?;
//! node.put("workloads", "w1", b"spec".to_vec()).await?;
//! assert_eq!(node.get("workloads", "w1")?, Some(b"spec".to_vec()));
//! # Ok(())
//! # }
//! ```
//!
//! [openraft]: https://docs.rs/openraft/0.9
//! [`StateStore`]: ocf_store::StateStore

pub mod fabric_net;
pub mod network;
pub mod storage;
pub mod store;
pub mod types;

pub use store::ReplicatedStore;
pub use types::{KvCommand, KvResponse, TypeConfig};
