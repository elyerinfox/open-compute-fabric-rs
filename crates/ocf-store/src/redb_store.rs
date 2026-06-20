//! A durable [`StateStore`] backed by an embedded [redb] database.
//!
//! [redb]: https://docs.rs/redb

use crate::StateStore;
use ocf_core::error::{Error, Result};
use redb::{Database, TableDefinition};
use std::path::Path;

/// One table holds every collection; keys are `"collection\u{1f}key"` so a
/// range scan over the `"collection\u{1f}".."collection\u{20}"` interval lists
/// exactly one collection.
const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("ocf_state");

/// A crash-safe, single-file key/value store.
///
/// Opening a path creates the database if it does not yet exist, so a node's
/// first boot and every subsequent boot take the same code path.
pub struct RedbStateStore {
    db: Database,
}

fn redb_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::internal(format!("redb {context}: {e}"))
}

fn composite(collection: &str, key: &str) -> String {
    format!("{collection}\u{1f}{key}")
}

impl RedbStateStore {
    /// Open (or create) the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path).map_err(|e| redb_err("open", e))?;
        // Materialize the table so reads on a fresh database don't fail.
        let wtx = db.begin_write().map_err(|e| redb_err("begin_write", e))?;
        {
            wtx.open_table(TABLE).map_err(|e| redb_err("open_table", e))?;
        }
        wtx.commit().map_err(|e| redb_err("commit", e))?;
        Ok(RedbStateStore { db })
    }
}

impl StateStore for RedbStateStore {
    fn put(&self, collection: &str, key: &str, value: &[u8]) -> Result<()> {
        let composite = composite(collection, key);
        let wtx = self.db.begin_write().map_err(|e| redb_err("begin_write", e))?;
        {
            let mut table = wtx.open_table(TABLE).map_err(|e| redb_err("open_table", e))?;
            table
                .insert(composite.as_str(), value)
                .map_err(|e| redb_err("insert", e))?;
        }
        wtx.commit().map_err(|e| redb_err("commit", e))?;
        Ok(())
    }

    fn get(&self, collection: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let composite = composite(collection, key);
        let rtx = self.db.begin_read().map_err(|e| redb_err("begin_read", e))?;
        let table = rtx.open_table(TABLE).map_err(|e| redb_err("open_table", e))?;
        let got = table
            .get(composite.as_str())
            .map_err(|e| redb_err("get", e))?;
        Ok(got.map(|g| g.value().to_vec()))
    }

    fn delete(&self, collection: &str, key: &str) -> Result<()> {
        let composite = composite(collection, key);
        let wtx = self.db.begin_write().map_err(|e| redb_err("begin_write", e))?;
        {
            let mut table = wtx.open_table(TABLE).map_err(|e| redb_err("open_table", e))?;
            table
                .remove(composite.as_str())
                .map_err(|e| redb_err("remove", e))?;
        }
        wtx.commit().map_err(|e| redb_err("commit", e))?;
        Ok(())
    }

    fn list(&self, collection: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let lo = format!("{collection}\u{1f}");
        let hi = format!("{collection}\u{20}");
        let rtx = self.db.begin_read().map_err(|e| redb_err("begin_read", e))?;
        let table = rtx.open_table(TABLE).map_err(|e| redb_err("open_table", e))?;
        let mut out = Vec::new();
        let range = table
            .range(lo.as_str()..hi.as_str())
            .map_err(|e| redb_err("range", e))?;
        for entry in range {
            let (k, v) = entry.map_err(|e| redb_err("range_entry", e))?;
            let full = k.value();
            let key = full.strip_prefix(&lo).unwrap_or(full).to_string();
            out.push((key, v.value().to_vec()));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StateStoreExt;
    use std::process;

    fn temp_path(tag: &str) -> std::path::PathBuf {
        // Process-unique path under the OS temp dir; no extra crate needed.
        let mut p = std::env::temp_dir();
        p.push(format!("ocf-store-test-{}-{}.redb", process::id(), tag));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn survives_reopen() {
        let path = temp_path("reopen");

        // Write, then drop the database to flush and release the file.
        {
            let store = RedbStateStore::open(&path).expect("open");
            store.put("workloads", "web-1", b"running").expect("put");
            store
                .put_json("vpcs", "tenant-a", &vec![10u8, 0, 0, 0])
                .expect("put_json");
        }

        // Reopen the same file: state must still be there ("reboot").
        {
            let store = RedbStateStore::open(&path).expect("reopen");
            assert_eq!(
                store.get("workloads", "web-1").expect("get"),
                Some(b"running".to_vec())
            );
            let cidr: Vec<u8> = store.get_json("vpcs", "tenant-a").expect("get_json").unwrap();
            assert_eq!(cidr, vec![10u8, 0, 0, 0]);
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn list_is_scoped_to_collection() {
        let path = temp_path("list");
        let store = RedbStateStore::open(&path).expect("open");
        store.put("a", "1", b"x").unwrap();
        store.put("a", "2", b"y").unwrap();
        store.put("b", "1", b"z").unwrap();

        let mut a = store.list("a").unwrap();
        a.sort();
        assert_eq!(
            a,
            vec![
                ("1".to_string(), b"x".to_vec()),
                ("2".to_string(), b"y".to_vec())
            ]
        );
        assert_eq!(store.list("b").unwrap().len(), 1);

        store.delete("a", "1").unwrap();
        assert_eq!(store.list("a").unwrap().len(), 1);

        let _ = std::fs::remove_file(&path);
    }
}
