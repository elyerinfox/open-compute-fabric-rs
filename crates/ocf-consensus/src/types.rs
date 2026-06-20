//! Raft type configuration and the application command/response types.
//!
//! These define what flows through the replicated log. A [`KvCommand`] is the
//! mutation a client proposes; once a quorum has committed it, every node's
//! state machine applies it into its [`ocf_store::StateStore`] and acknowledges
//! with a [`KvResponse`].

use std::io::Cursor;

use ocf_core::prelude::*;

/// A control-plane mutation replicated through the Raft log.
///
/// The command is intentionally a thin mirror of the [`ocf_store::StateStore`]
/// write surface (`put`/`delete`) so that applying a committed entry is a direct
/// translation into a store call — there is no hidden business logic in the
/// state machine, which keeps replication auditable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KvCommand {
    /// Store `value` under `key` within `collection`, overwriting any previous
    /// value.
    Put {
        collection: String,
        key: String,
        value: Vec<u8>,
    },
    /// Remove `key` from `collection`. Deleting an absent key is not an error.
    Delete { collection: String, key: String },
}

/// The acknowledgement returned to the proposer once a [`KvCommand`] has been
/// committed and applied to the local state machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvResponse {
    /// `true` when the command was applied to the state machine. A committed
    /// command is always applied, so this is the committed/ack signal callers
    /// wait on.
    pub applied: bool,
}

openraft::declare_raft_types!(
    /// The Raft type configuration for the control-plane key/value store.
    ///
    /// * `D` — [`KvCommand`], the proposed mutation.
    /// * `R` — [`KvResponse`], the apply acknowledgement.
    /// * `NodeId` — `u64`.
    /// * `Node` — [`openraft::BasicNode`] (carries a routable address; unused by
    ///   the in-process network but required by the cluster-membership types).
    /// * `Entry` — [`openraft::Entry`] over this config.
    /// * `SnapshotData` — an in-memory `Cursor<Vec<u8>>`.
    pub TypeConfig:
        D = KvCommand,
        R = KvResponse,
        NodeId = u64,
        Node = openraft::BasicNode,
        Entry = openraft::Entry<TypeConfig>,
        SnapshotData = Cursor<Vec<u8>>,
);
