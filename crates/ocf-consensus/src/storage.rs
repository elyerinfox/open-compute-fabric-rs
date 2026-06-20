//! In-memory Raft log storage plus a state machine that applies committed
//! entries into an [`ocf_store::StateStore`].
//!
//! This implements openraft 0.9's split storage traits:
//!
//! * [`RaftLogStorage`] — the replicated log (vote, committed marker, entries).
//!   The log lives in memory; persistence of the *log* itself is out of scope
//!   for this in-process cluster (see the crate-level note). It is modelled on
//!   openraft's `raft-kv-memstore` example for 0.9.
//! * [`RaftStateMachine`] — applies each committed [`KvCommand`] into the
//!   supplied `StateStore`, so replicated writes land in exactly the durable
//!   abstraction the rest of the fabric reads from.
//!
//! Snapshots capture the full state-machine store contents so a lagging or
//! freshly-joined node can be caught up in one shot.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt::Debug;
use std::io::Cursor;
use std::ops::RangeBounds;
use std::sync::Arc;

use ocf_store::StateStore;
use openraft::storage::LogFlushed;
use openraft::storage::LogState;
use openraft::storage::RaftLogStorage;
use openraft::storage::RaftStateMachine;
use openraft::storage::Snapshot;
use openraft::Entry;
use openraft::EntryPayload;
use openraft::LogId;
use openraft::OptionalSend;
use openraft::RaftLogReader;
use openraft::RaftSnapshotBuilder;
use openraft::SnapshotMeta;
use openraft::StorageError;
use openraft::StorageIOError;
use openraft::StoredMembership;
use openraft::Vote;
use parking_lot_shim::Mutex;
use serde::Deserialize;
use serde::Serialize;

use crate::types::KvCommand;
use crate::types::KvResponse;
use crate::types::TypeConfig;

// The fabric house style uses `parking_lot`, but this crate's dependency budget
// is the openraft set only. openraft already pulls in `tokio`, whose sync
// `Mutex` is for async hold-across-await use; for these short, non-awaiting
// critical sections a `std::sync::Mutex` is correct. This tiny shim adapts
// `std::sync::Mutex` to the `parking_lot`-style `.lock()` (no poisoning) the
// rest of the code expects, so a poisoned lock can never panic non-test code.
mod parking_lot_shim {
    pub struct Mutex<T>(std::sync::Mutex<T>);

    impl<T> Mutex<T> {
        pub fn new(value: T) -> Self {
            Self(std::sync::Mutex::new(value))
        }

        /// Lock the mutex, recovering the guard even if a previous holder
        /// panicked (poison is ignored — our critical sections never leave
        /// inconsistent state).
        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }
}

type NodeId = u64;

/// The serialized form of a snapshot: every `(collection, key, value)` triple in
/// the state-machine store, plus the metadata openraft needs to place it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotPayload {
    last_applied: Option<LogId<NodeId>>,
    last_membership: StoredMembership<NodeId, openraft::BasicNode>,
    /// `collection -> (key -> value)`.
    data: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
}

/// Shared, mutable Raft log + state-machine bookkeeping.
///
/// `LogStore` and `StateMachineStore` are thin handles over this so the same
/// data is visible to both the log-storage and state-machine trait impls (they
/// are constructed as a pair and handed separately to `Raft::new`).
struct Inner {
    /// The replicated log, indexed by log index.
    log: BTreeMap<u64, Entry<TypeConfig>>,
    /// The last persisted vote.
    vote: Option<Vote<NodeId>>,
    /// The last committed log id (optionally persisted; informational here).
    committed: Option<LogId<NodeId>>,
    /// The highest log id purged after being applied + snapshotted.
    last_purged: Option<LogId<NodeId>>,

    /// State machine: last applied log id.
    sm_last_applied: Option<LogId<NodeId>>,
    /// State machine: last applied membership config.
    sm_last_membership: StoredMembership<NodeId, openraft::BasicNode>,
    /// Collections this state machine has written to, so snapshots can
    /// enumerate the store (which only supports per-collection `list`).
    sm_collections: BTreeSet<String>,

    /// The most recently built or installed snapshot, returned by
    /// `get_current_snapshot`.
    current_snapshot: Option<StoredSnapshot>,
    /// Monotonic counter feeding unique snapshot ids.
    snapshot_idx: u64,
}

/// A snapshot retained in memory: its metadata and the serialized bytes.
#[derive(Clone)]
struct StoredSnapshot {
    meta: SnapshotMeta<NodeId, openraft::BasicNode>,
    data: Vec<u8>,
}

/// Handle to the replicated log. Implements [`RaftLogStorage`].
#[derive(Clone)]
pub struct LogStore {
    inner: Arc<Mutex<Inner>>,
}

/// Handle to the state machine. Implements [`RaftStateMachine`] and applies
/// committed commands into the shared [`StateStore`].
#[derive(Clone)]
pub struct StateMachineStore {
    inner: Arc<Mutex<Inner>>,
    store: Arc<dyn StateStore>,
}

/// Build a fresh log-storage + state-machine pair backed by `store`.
///
/// The two share the same in-memory log/vote/snapshot state; only the state
/// machine holds the durable [`StateStore`] it applies into.
pub fn new_storage(store: Arc<dyn StateStore>) -> (LogStore, StateMachineStore) {
    let inner = Arc::new(Mutex::new(Inner {
        log: BTreeMap::new(),
        vote: None,
        committed: None,
        last_purged: None,
        sm_last_applied: None,
        sm_last_membership: StoredMembership::default(),
        sm_collections: BTreeSet::new(),
        current_snapshot: None,
        snapshot_idx: 0,
    }));
    let log = LogStore {
        inner: inner.clone(),
    };
    let sm = StateMachineStore { inner, store };
    (log, sm)
}

impl StateMachineStore {
    /// Apply a single command into the durable store, recording the collection
    /// so snapshots can later enumerate it.
    fn apply_command(&self, inner: &mut Inner, cmd: &KvCommand) -> Result<(), StorageError<NodeId>> {
        match cmd {
            KvCommand::Put {
                collection,
                key,
                value,
            } => {
                self.store
                    .put(collection, key, value)
                    .map_err(|e| StorageIOError::write_state_machine(&e))?;
                inner.sm_collections.insert(collection.clone());
            }
            KvCommand::Delete { collection, key } => {
                self.store
                    .delete(collection, key)
                    .map_err(|e| StorageIOError::write_state_machine(&e))?;
                inner.sm_collections.insert(collection.clone());
            }
        }
        Ok(())
    }

    /// Serialize the entire state-machine store into a snapshot payload.
    fn build_payload(&self, inner: &Inner) -> Result<SnapshotPayload, StorageError<NodeId>> {
        let mut data = BTreeMap::new();
        for collection in &inner.sm_collections {
            let entries = self
                .store
                .list(collection)
                .map_err(|e| StorageIOError::read_state_machine(&e))?;
            let map: BTreeMap<String, Vec<u8>> = entries.into_iter().collect();
            data.insert(collection.clone(), map);
        }
        Ok(SnapshotPayload {
            last_applied: inner.sm_last_applied,
            last_membership: inner.sm_last_membership.clone(),
            data,
        })
    }

    /// Replace the state-machine store contents with those in `payload`.
    fn restore_payload(
        &self,
        inner: &mut Inner,
        payload: SnapshotPayload,
    ) -> Result<(), StorageError<NodeId>> {
        // Clear collections we currently know about, then load the snapshot's.
        let known: Vec<String> = inner.sm_collections.iter().cloned().collect();
        for collection in known {
            let existing = self
                .store
                .list(&collection)
                .map_err(|e| StorageIOError::read_state_machine(&e))?;
            for (key, _) in existing {
                self.store
                    .delete(&collection, &key)
                    .map_err(|e| StorageIOError::write_state_machine(&e))?;
            }
        }
        inner.sm_collections.clear();

        for (collection, kv) in payload.data {
            for (key, value) in kv {
                self.store
                    .put(&collection, &key, &value)
                    .map_err(|e| StorageIOError::write_state_machine(&e))?;
            }
            inner.sm_collections.insert(collection);
        }
        inner.sm_last_applied = payload.last_applied;
        inner.sm_last_membership = payload.last_membership;
        Ok(())
    }
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError<NodeId>> {
        let inner = self.inner.lock();
        let entries = inner
            .log
            .range(range)
            .map(|(_, entry)| entry.clone())
            .collect();
        Ok(entries)
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<NodeId>> {
        let inner = self.inner.lock();
        let last = inner
            .log
            .iter()
            .next_back()
            .map(|(_, entry)| entry.log_id)
            .or(inner.last_purged);
        Ok(LogState {
            last_purged_log_id: inner.last_purged,
            last_log_id: last,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.inner.lock().vote = Some(*vote);
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        Ok(self.inner.lock().vote)
    }

    async fn save_committed(
        &mut self,
        committed: Option<LogId<NodeId>>,
    ) -> Result<(), StorageError<NodeId>> {
        self.inner.lock().committed = committed;
        Ok(())
    }

    async fn read_committed(&mut self) -> Result<Option<LogId<NodeId>>, StorageError<NodeId>> {
        Ok(self.inner.lock().committed)
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<TypeConfig>,
    ) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        {
            let mut inner = self.inner.lock();
            for entry in entries {
                inner.log.insert(entry.log_id.index, entry);
            }
        }
        // The log is in memory, so it is durable the instant `insert` returns.
        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        let mut inner = self.inner.lock();
        let keys: Vec<u64> = inner.log.range(log_id.index..).map(|(k, _)| *k).collect();
        for k in keys {
            inner.log.remove(&k);
        }
        Ok(())
    }

    async fn purge(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        let mut inner = self.inner.lock();
        let keys: Vec<u64> = inner.log.range(..=log_id.index).map(|(k, _)| *k).collect();
        for k in keys {
            inner.log.remove(&k);
        }
        if inner.last_purged < Some(log_id) {
            inner.last_purged = Some(log_id);
        }
        Ok(())
    }
}

impl RaftSnapshotBuilder<TypeConfig> for StateMachineStore {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        let (payload, meta, bytes) = {
            let mut inner = self.inner.lock();
            let payload = self.build_payload(&inner)?;
            let bytes = serde_json::to_vec(&payload)
                .map_err(|e| StorageIOError::write_snapshot(None, &e))?;

            inner.snapshot_idx += 1;
            let snapshot_id = match payload.last_applied {
                Some(last) => format!("{}-{}-{}", last.leader_id, last.index, inner.snapshot_idx),
                None => format!("--{}", inner.snapshot_idx),
            };
            let meta = SnapshotMeta {
                last_log_id: payload.last_applied,
                last_membership: payload.last_membership.clone(),
                snapshot_id,
            };
            inner.current_snapshot = Some(StoredSnapshot {
                meta: meta.clone(),
                data: bytes.clone(),
            });
            (payload, meta, bytes)
        };
        let _ = payload;
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(bytes)),
        })
    }
}

impl RaftStateMachine<TypeConfig> for StateMachineStore {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> Result<
        (
            Option<LogId<NodeId>>,
            StoredMembership<NodeId, openraft::BasicNode>,
        ),
        StorageError<NodeId>,
    > {
        let inner = self.inner.lock();
        Ok((inner.sm_last_applied, inner.sm_last_membership.clone()))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<KvResponse>, StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let mut responses = Vec::new();
        let mut inner = self.inner.lock();
        for entry in entries {
            inner.sm_last_applied = Some(entry.log_id);
            match entry.payload {
                EntryPayload::Blank => {
                    responses.push(KvResponse { applied: false });
                }
                EntryPayload::Normal(ref cmd) => {
                    self.apply_command(&mut inner, cmd)?;
                    responses.push(KvResponse { applied: true });
                }
                EntryPayload::Membership(ref membership) => {
                    inner.sm_last_membership =
                        StoredMembership::new(Some(entry.log_id), membership.clone());
                    responses.push(KvResponse { applied: false });
                }
            }
        }
        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<Cursor<Vec<u8>>>, StorageError<NodeId>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<NodeId, openraft::BasicNode>,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<NodeId>> {
        let bytes = snapshot.into_inner();
        let payload: SnapshotPayload = serde_json::from_slice(&bytes)
            .map_err(|e| StorageIOError::read_snapshot(Some(meta.signature()), &e))?;

        let mut inner = self.inner.lock();
        self.restore_payload(&mut inner, payload)?;
        inner.current_snapshot = Some(StoredSnapshot {
            meta: meta.clone(),
            data: bytes,
        });
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> {
        let inner = self.inner.lock();
        match &inner.current_snapshot {
            Some(snap) => Ok(Some(Snapshot {
                meta: snap.meta.clone(),
                snapshot: Box::new(Cursor::new(snap.data.clone())),
            })),
            None => Ok(None),
        }
    }
}
