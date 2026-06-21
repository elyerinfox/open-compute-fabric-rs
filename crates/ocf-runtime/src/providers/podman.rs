//! Podman container backend.
//!
//! Identical to the Docker backend but driving the daemonless `podman` CLI,
//! whose subcommands and `--format` placeholders are Docker-compatible. State is
//! owned by Podman and read back from it; containers are not migratable.

use crate::provider::RuntimeProvider;
use crate::providers::command::{
    self, container_create_args, parse_container_status, workloads_from_ps, OCF_LABEL,
};
use crate::workload::{RuntimeKind, Workload};
use ocf_core::prelude::*;

/// The `podman` binary this backend shells out to.
const BIN: &str = "podman";

/// A [`RuntimeProvider`] backed by Podman (daemonless containers).
#[derive(Default)]
pub struct PodmanRuntime;

impl PodmanRuntime {
    pub fn new() -> Self {
        Self
    }
}

impl Provider for PodmanRuntime {
    fn name(&self) -> &str {
        "podman"
    }
    fn description(&self) -> &str {
        "Podman daemonless container backend"
    }
}

#[async_trait]
impl RuntimeProvider for PodmanRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Container
    }

    async fn create(&self, workload: &Workload) -> Result<()> {
        command::require_kind(workload, RuntimeKind::Container, self.name())?;
        // `podman create --name <id> --label ocf=1 --label ocf.workload=<id>
        //   [-e K=V ...] [--memory <bytes>] [--cpus <cores>] <image>`
        let args = container_create_args(workload);
        command::run(BIN, &args).await.map(|_| ())
    }

    async fn start(&self, id: &Id) -> Result<()> {
        // `podman start <id>`
        let args = vec!["start".to_string(), id.to_string()];
        command::run(BIN, &args).await.map(|_| ())
    }

    async fn stop(&self, id: &Id) -> Result<()> {
        // `podman stop <id>`
        let args = vec!["stop".to_string(), id.to_string()];
        command::run(BIN, &args).await.map(|_| ())
    }

    async fn delete(&self, id: &Id) -> Result<()> {
        // `podman rm -f <id>`
        let args = vec!["rm".to_string(), "-f".to_string(), id.to_string()];
        command::run(BIN, &args).await.map(|_| ())
    }

    async fn status(&self, id: &Id) -> Result<LifecycleState> {
        // `podman inspect -f '{{.State.Status}}' <id>`
        let args = vec![
            "inspect".to_string(),
            "-f".to_string(),
            "{{.State.Status}}".to_string(),
            id.to_string(),
        ];
        let out = command::run(BIN, &args).await?;
        Ok(parse_container_status(&out))
    }

    async fn host_pid(&self, id: &Id) -> Result<Option<u32>> {
        // `podman inspect -f '{{.State.Pid}}' <id>` — 0 means not running.
        let args = vec![
            "inspect".to_string(),
            "-f".to_string(),
            "{{.State.Pid}}".to_string(),
            id.to_string(),
        ];
        let out = command::run(BIN, &args).await?;
        Ok(out.trim().parse::<u32>().ok().filter(|p| *p != 0))
    }

    async fn list(&self) -> Result<Vec<Workload>> {
        // `podman ps -a --filter label=ocf=1
        //   --format '{{.ID}}|{{.Image}}|{{.Names}}|{{.State}}'`
        let args = vec![
            "ps".to_string(),
            "-a".to_string(),
            "--filter".to_string(),
            format!("label={OCF_LABEL}"),
            "--format".to_string(),
            "{{.ID}}|{{.Image}}|{{.Names}}|{{.State}}".to_string(),
        ];
        let out = command::run(BIN, &args).await?;
        Ok(workloads_from_ps(&out))
    }
}
