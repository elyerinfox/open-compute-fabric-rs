//! LXC system-container backend.
//!
//! Drives the real `lxc-*` family of tools (`lxc-create`, `lxc-start`,
//! `lxc-stop`, `lxc-destroy`, `lxc-info`, `lxc-ls`). LXC owns container state;
//! this backend reads it back via `lxc-info`/`lxc-ls` rather than mirroring it.
//! LXC containers are not migratable here, so the trait's default
//! `dump_memory`/`restore` refusals apply.

use crate::provider::RuntimeProvider;
use crate::providers::command::{self, lxc_info_state, workloads_from_lxc_ls};
use crate::workload::{RuntimeKind, Workload};
use ocf_core::prelude::*;

/// A [`RuntimeProvider`] backed by LXC system containers.
#[derive(Default)]
pub struct LxcRuntime;

impl LxcRuntime {
    pub fn new() -> Self {
        Self
    }
}

impl Provider for LxcRuntime {
    fn name(&self) -> &str {
        "lxc"
    }
    fn description(&self) -> &str {
        "LXC system-container backend"
    }
}

#[async_trait]
impl RuntimeProvider for LxcRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Container
    }

    async fn create(&self, workload: &Workload) -> Result<()> {
        command::require_kind(workload, RuntimeKind::Container, self.name())?;
        // `lxc-create -n <id> -t <image>` — the workload image names the LXC
        // template/rootfs to build the container from.
        let args = vec![
            "-n".to_string(),
            workload.metadata.id.to_string(),
            "-t".to_string(),
            workload.image.clone(),
        ];
        command::run("lxc-create", &args).await.map(|_| ())
    }

    async fn start(&self, id: &Id) -> Result<()> {
        // `lxc-start -n <id> -d` (daemonized).
        let args = vec!["-n".to_string(), id.to_string(), "-d".to_string()];
        command::run("lxc-start", &args).await.map(|_| ())
    }

    async fn stop(&self, id: &Id) -> Result<()> {
        // `lxc-stop -n <id>`.
        let args = vec!["-n".to_string(), id.to_string()];
        command::run("lxc-stop", &args).await.map(|_| ())
    }

    async fn delete(&self, id: &Id) -> Result<()> {
        // `lxc-destroy -n <id> -f` (force: stop first if still running).
        let args = vec!["-n".to_string(), id.to_string(), "-f".to_string()];
        command::run("lxc-destroy", &args).await.map(|_| ())
    }

    async fn status(&self, id: &Id) -> Result<LifecycleState> {
        // `lxc-info -n <id> -s` prints a `State: <STATE>` line.
        let args = vec!["-n".to_string(), id.to_string(), "-s".to_string()];
        let out = command::run("lxc-info", &args).await?;
        Ok(lxc_info_state(&out))
    }

    async fn list(&self) -> Result<Vec<Workload>> {
        // `lxc-ls -1 -f -F NAME,STATE` lists every container with its state.
        let args = vec![
            "-1".to_string(),
            "-f".to_string(),
            "-F".to_string(),
            "NAME,STATE".to_string(),
        ];
        let out = command::run("lxc-ls", &args).await?;
        Ok(workloads_from_lxc_ls(&out))
    }
}
