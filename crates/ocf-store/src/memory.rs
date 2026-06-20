//! An in-memory [`StateStore`] for tests and ephemeral runs.

use crate::StateStore;
use ocf_core::error::Result;
use parking_lot::RwLock;
use std::collections::BTreeMap;

/// A non-durable store backed by a `BTreeMap`. Useful in tests and as a drop-in
/// when persistence is intentionally disabled.
#[derive(Default)]
pub struct MemoryStateStore {
    // key is "collection\u{1f}key" so a single map holds every collection while
    // preserving per-collection ordering for `list`.
    data: RwLock<BTreeMap<String, Vec<u8>>>,
}

impl MemoryStateStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn composite(collection: &str, key: &str) -> String {
    format!("{collection}\u{1f}{key}")
}

impl StateStore for MemoryStateStore {
    fn put(&self, collection: &str, key: &str, value: &[u8]) -> Result<()> {
        self.data
            .write()
            .insert(composite(collection, key), value.to_vec());
        Ok(())
    }

    fn get(&self, collection: &str, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.data.read().get(&composite(collection, key)).cloned())
    }

    fn delete(&self, collection: &str, key: &str) -> Result<()> {
        self.data.write().remove(&composite(collection, key));
        Ok(())
    }

    fn list(&self, collection: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let prefix = format!("{collection}\u{1f}");
        Ok(self
            .data
            .read()
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix(&prefix)
                    .map(|key| (key.to_string(), v.clone()))
            })
            .collect())
    }
}
