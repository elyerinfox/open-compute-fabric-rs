//! # ocf-store
//!
//! Durable state for the control plane.
//!
//! Every subsystem that needs to survive a reboot writes through a
//! [`StateStore`] — a small, namespaced key/value contract. Two backends ship:
//!
//! * [`RedbStateStore`] — a single-file embedded database ([redb]) that persists
//!   to disk and is crash-safe. This is what `ocfd` uses for node-local state.
//! * [`MemoryStateStore`] — a `RwLock<HashMap>` for tests and ephemeral runs.
//!
//! Values are opaque bytes; the [`StateStoreExt`] extension adds typed
//! `*_json` helpers so callers can persist any `serde` type without thinking
//! about encoding. A `collection` namespaces keys (think "table" — `"workloads"`,
//! `"vpcs"`, ...), so one store holds the whole control plane.
//!
//! Node-local durability is one half of fleet persistence; the other half —
//! replicating this state across nodes so it survives losing the node itself —
//! lives in `ocf-consensus` (Raft). A `StateStore` is exactly the seam that a
//! Raft state machine applies committed entries into.
//!
//! [redb]: https://docs.rs/redb

mod memory;
mod redb_store;

pub use memory::MemoryStateStore;
pub use redb_store::RedbStateStore;

use ocf_core::error::Result;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// A durable, namespaced key/value store.
///
/// Implementations must be crash-consistent: a `put` that returns `Ok` is
/// readable after a process restart. Methods are synchronous because the
/// backends are local (embedded DB / memory); callers in async contexts should
/// treat them as fast, non-blocking IO.
pub trait StateStore: Send + Sync {
    /// Store `value` under `key` within `collection`, overwriting any previous
    /// value.
    fn put(&self, collection: &str, key: &str, value: &[u8]) -> Result<()>;

    /// Fetch the value for `key` within `collection`, if present.
    fn get(&self, collection: &str, key: &str) -> Result<Option<Vec<u8>>>;

    /// Remove `key` from `collection`. Removing an absent key is not an error.
    fn delete(&self, collection: &str, key: &str) -> Result<()>;

    /// Every `(key, value)` pair in `collection`, in key order.
    fn list(&self, collection: &str) -> Result<Vec<(String, Vec<u8>)>>;
}

/// Typed convenience helpers layered over any [`StateStore`].
pub trait StateStoreExt: StateStore {
    fn put_json<T: Serialize>(&self, collection: &str, key: &str, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        self.put(collection, key, &bytes)
    }

    fn get_json<T: DeserializeOwned>(&self, collection: &str, key: &str) -> Result<Option<T>> {
        match self.get(collection, key)? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    /// Deserialize every value in `collection`. Entries that fail to decode are
    /// skipped with a warning rather than failing the whole load.
    fn list_json<T: DeserializeOwned>(&self, collection: &str) -> Result<Vec<T>> {
        let mut out = Vec::new();
        for (key, bytes) in self.list(collection)? {
            match serde_json::from_slice::<T>(&bytes) {
                Ok(v) => out.push(v),
                Err(e) => {
                    tracing::warn!(collection, key, error = %e, "skipping undecodable state entry");
                }
            }
        }
        Ok(out)
    }
}

impl<S: StateStore + ?Sized> StateStoreExt for S {}
