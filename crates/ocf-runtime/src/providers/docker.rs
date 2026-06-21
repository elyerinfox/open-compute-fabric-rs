//! Docker container backend.
//!
//! Drives the real `docker` CLI. State is owned by the Docker engine and read
//! back from it (`docker inspect`, `docker ps`) — this backend keeps no shadow
//! copy. Containers are not migratable, so it inherits the trait's default
//! refusals for `dump_memory`/`restore`.

use crate::provider::RuntimeProvider;
use crate::providers::command::{
    self, container_create_args, parse_container_status, workloads_from_ps, OCF_LABEL,
};
use crate::workload::{RuntimeKind, Workload};
use ocf_core::prelude::*;

/// The `docker` binary this backend shells out to.
const BIN: &str = "docker";

/// A [`RuntimeProvider`] backed by the Docker engine.
#[derive(Default)]
pub struct DockerRuntime;

impl DockerRuntime {
    pub fn new() -> Self {
        Self
    }
}

impl Provider for DockerRuntime {
    fn name(&self) -> &str {
        "docker"
    }
    fn description(&self) -> &str {
        "Docker engine container backend"
    }
}

#[async_trait]
impl RuntimeProvider for DockerRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Container
    }

    async fn create(&self, workload: &Workload) -> Result<()> {
        command::require_kind(workload, RuntimeKind::Container, self.name())?;
        // `docker create --name <id> --label ocf=1 --label ocf.workload=<id>
        //   [-e K=V ...] [--memory <bytes>] [--cpus <cores>] <image>`
        let args = container_create_args(workload);
        command::run(BIN, &args).await.map(|_| ())
    }

    async fn start(&self, id: &Id) -> Result<()> {
        // `docker start <id>`
        let args = vec!["start".to_string(), id.to_string()];
        command::run(BIN, &args).await.map(|_| ())
    }

    async fn stop(&self, id: &Id) -> Result<()> {
        // `docker stop <id>`
        let args = vec!["stop".to_string(), id.to_string()];
        command::run(BIN, &args).await.map(|_| ())
    }

    async fn delete(&self, id: &Id) -> Result<()> {
        // `docker rm -f <id>`
        let args = vec!["rm".to_string(), "-f".to_string(), id.to_string()];
        command::run(BIN, &args).await.map(|_| ())
    }

    async fn status(&self, id: &Id) -> Result<LifecycleState> {
        // `docker inspect -f '{{.State.Status}}' <id>`
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
        // `docker inspect -f '{{.State.Pid}}' <id>` — 0 means not running.
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
        // `docker ps -a --filter label=ocf=1
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
