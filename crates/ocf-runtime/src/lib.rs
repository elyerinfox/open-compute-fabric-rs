//! # ocf-runtime
//!
//! The fabric's compute plane: run containers and virtual machines, migrate
//! them between nodes, and autoscale them.
//!
//! Everything is built on the pluggable [`RuntimeProvider`] contract. A
//! [`Workload`] ([`workload`]) is the backend-agnostic description of a unit of
//! compute; a concrete provider ([`providers`]) turns it into a running
//! container or VM by driving the real host tooling. Two cross-cutting services
//! sit on top:
//!
//! * [`Migrator`] ([`migration`]) orchestrates a `dump → transfer → restore`
//!   live migration between two migration-capable providers, honoring the
//!   workload's placement [`Scope`].
//! * [`evaluate`] ([`autoscaler`]) decides a container [`Autoscaler`]'s desired
//!   replica count from an externally-supplied metric map, keeping this crate
//!   independent of `ocf-monitoring`.
//!
//! Each backend shells out to its real tool: `docker`/`podman` for containers,
//! the `lxc-*` family for LXC, and libvirt's `virsh` for QEMU/KVM VMs. The crate
//! still compiles everywhere (a missing tool is a *runtime* error), but a
//! lifecycle call now provisions an actual container or domain. Swapping a
//! backend for a different tool is a matter of replacing a provider, not
//! touching the control plane.

pub mod autoscaler;
pub mod migration;
pub mod provider;
pub mod providers;
pub mod workload;

pub use autoscaler::{evaluate, AutoscaleDecision, Autoscaler, Comparison, ScalingRule};
pub use migration::{MigrationReport, Migrator};
pub use provider::RuntimeProvider;
pub use providers::{DockerRuntime, LxcRuntime, PodmanRuntime, QemuRuntime};
pub use workload::{MemorySnapshot, RuntimeKind, Workload};

use ocf_core::prelude::*;
use std::sync::Arc;

/// Register the built-in runtime backends into `reg`.
///
/// Registers `docker`, `podman`, `lxc` (containers) and `qemu` (a
/// migration-capable VM backend). A deployment may register additional
/// backends, or `register_or_replace` these, without changing any caller.
pub fn register_builtins(reg: &mut Registry<dyn RuntimeProvider>) -> Result<()> {
    reg.register("docker", Arc::new(DockerRuntime::new()))?;
    reg.register("podman", Arc::new(PodmanRuntime::new()))?;
    reg.register("lxc", Arc::new(LxcRuntime::new()))?;
    reg.register("qemu", Arc::new(QemuRuntime::new()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_register_expected_backends() {
        let mut reg: Registry<dyn RuntimeProvider> = Registry::new();
        register_builtins(&mut reg).expect("register builtins");
        assert_eq!(reg.len(), 4);
        assert!(reg.contains("docker"));
        assert!(reg.contains("podman"));
        assert!(reg.contains("lxc"));
        assert!(reg.contains("qemu"));
    }

    // The lifecycle/migration tests below drive real `docker`/`virsh` and a
    // running daemon, so they are `#[ignore]`d by default and only run when a
    // developer opts in on a host with the tooling (`cargo test -- --ignored`).

    #[tokio::test]
    #[ignore = "requires a running Docker daemon"]
    async fn container_lifecycle_round_trips() {
        let docker = DockerRuntime::new();
        let wl = Workload::container("web", "nginx:1.27");
        let id = wl.metadata.id.clone();

        docker.create(&wl).await.expect("create");
        // A freshly `docker create`d container is in state `created` => Stopped.
        assert_eq!(docker.status(&id).await.unwrap(), LifecycleState::Stopped);
        docker.start(&id).await.expect("start");
        assert_eq!(docker.status(&id).await.unwrap(), LifecycleState::Running);
        docker.stop(&id).await.expect("stop");
        assert_eq!(docker.status(&id).await.unwrap(), LifecycleState::Stopped);
        docker.delete(&id).await.expect("delete");
        // After `docker rm -f`, `docker inspect` fails => status is an error.
        assert!(docker.status(&id).await.is_err());
    }

    #[tokio::test]
    async fn containers_refuse_migration() {
        // Migration capability is a compile-time/trait property — no daemon
        // needed to assert that containers refuse `dump_memory`.
        let docker = DockerRuntime::new();
        assert!(!docker.supports_migration());
        let id = Id::named("web");
        assert!(docker.dump_memory(&id).await.is_err());
    }

    #[tokio::test]
    #[ignore = "requires libvirt/virsh and a defined, running domain"]
    async fn qemu_migration_moves_workload_to_target() {
        let source = Arc::new(QemuRuntime::new());
        let target = Arc::new(QemuRuntime::new());
        assert!(source.supports_migration());

        let mut wl = Workload::virtual_machine("db", "debian-12.qcow2");
        wl.resources = ResourceSpec::new(2000, 4 * 1024 * 1024 * 1024, 0);
        let id = wl.metadata.id.clone();
        source.create(&wl).await.unwrap();
        source.start(&id).await.unwrap();

        let migrator = Migrator::new(source.clone(), target.clone());
        let target_node = Id::named("node-b");
        let report = migrator
            .migrate(&wl, target_node.clone(), &Scope::fleet())
            .await
            .expect("migrate");

        // The snapshot carries the real size of the saved memory image.
        assert!(report.snapshot.bytes_len > 0);
        assert_eq!(report.target_node, target_node);
        // The workload now lives on the target and is gone from the source.
        assert_eq!(target.status(&id).await.unwrap(), LifecycleState::Running);
        assert!(source.status(&id).await.is_err());
    }

    #[tokio::test]
    async fn migration_refuses_out_of_placement_scope() {
        // The placement-scope check happens before any provider call, so this
        // exercises real orchestration logic without needing a daemon.
        let source = Arc::new(QemuRuntime::new());
        let target = Arc::new(QemuRuntime::new());

        // Confine the workload to region "us"; attempt to land it in "eu".
        let wl = Workload::virtual_machine("db", "debian-12.qcow2")
            .within(Scope::region("us"));

        let migrator = Migrator::new(source, target);
        let err = migrator
            .migrate(&wl, Id::named("node-eu"), &Scope::region("eu"))
            .await
            .expect_err("must refuse cross-scope migration");
        assert_eq!(err.code(), "forbidden");
    }
}
