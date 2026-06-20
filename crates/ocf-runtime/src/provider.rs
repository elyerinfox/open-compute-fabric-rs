//! The pluggable runtime backend contract.

use crate::workload::{MemorySnapshot, RuntimeKind, Workload};
use ocf_core::prelude::*;

/// A swappable backend that actually runs workloads.
///
/// Each concrete backend (Docker, Podman, LXC, QEMU, ...) implements this
/// contract and is registered into a `Registry<dyn RuntimeProvider>`. The
/// controller talks only to this trait, so adding a backend never touches the
/// control plane.
///
/// Migration support is opt-in: the two `dump_memory`/`restore` methods default
/// to [`Error::unsupported`], and a backend that *can* checkpoint overrides them
/// and reports `supports_migration() == true`.
#[async_trait]
pub trait RuntimeProvider: Provider {
    /// Whether this backend runs containers or virtual machines.
    fn kind(&self) -> RuntimeKind;

    /// Whether this backend can checkpoint and restore live memory.
    ///
    /// Defaults to `false`; migration-capable backends override it (and the
    /// `dump_memory`/`restore` pair below).
    fn supports_migration(&self) -> bool {
        false
    }

    /// Provision a workload (does not start it). Idempotent per id.
    async fn create(&self, workload: &Workload) -> Result<()>;

    /// Start a previously-created workload.
    async fn start(&self, id: &Id) -> Result<()>;

    /// Stop a running workload, leaving it provisioned.
    async fn stop(&self, id: &Id) -> Result<()>;

    /// Delete a workload, releasing its resources.
    async fn delete(&self, id: &Id) -> Result<()>;

    /// Report the current lifecycle state of a workload.
    async fn status(&self, id: &Id) -> Result<LifecycleState>;

    /// List every workload this backend currently manages.
    async fn list(&self) -> Result<Vec<Workload>>;

    /// Capture a live memory checkpoint of a running workload.
    ///
    /// Non-migratable backends inherit the default, which refuses the
    /// operation rather than pretending to succeed.
    async fn dump_memory(&self, id: &Id) -> Result<MemorySnapshot> {
        let _ = id;
        Err(Error::unsupported(format!(
            "backend `{}` does not support memory checkpointing",
            self.name()
        )))
    }

    /// Restore a workload from a memory checkpoint produced by `dump_memory`.
    ///
    /// Non-migratable backends inherit the default refusal.
    async fn restore(&self, snapshot: &MemorySnapshot) -> Result<()> {
        let _ = snapshot;
        Err(Error::unsupported(format!(
            "backend `{}` does not support memory restore",
            self.name()
        )))
    }
}
