//! Check: Docker daemon experimental features.

use crate::check::HealthCheck;
use crate::exec::{run, run_fix};
use crate::finding::{FixAction, HealthCategory, HealthFinding, Severity};
use ocf_core::prelude::*;

const DAEMON_JSON: &str = "/etc/docker/daemon.json";
const FIX_ID: &str = "enable-docker-experimental";

/// Warns when the Docker daemon does not have experimental features enabled.
/// Some fabric runtime features (certain `docker` subcommands) require it.
#[derive(Debug, Default)]
pub struct DockerExperimentalCheck;

impl DockerExperimentalCheck {
    pub fn new() -> Self {
        DockerExperimentalCheck
    }
}

impl Provider for DockerExperimentalCheck {
    fn name(&self) -> &str {
        "docker-experimental"
    }
    fn description(&self) -> &str {
        "The Docker daemon has experimental features enabled"
    }
}

#[async_trait]
impl HealthCheck for DockerExperimentalCheck {
    fn category(&self) -> HealthCategory {
        HealthCategory::Runtime
    }

    async fn check(&self, machine_id: &Id) -> Result<Vec<HealthFinding>> {
        // `docker info` reports the daemon's experimental flag. If docker isn't
        // installed/usable, we can't assess → no finding.
        let out = run("docker", &["info", "--format", "{{.ExperimentalBuild}}"]).await;
        if !out.ran || !out.success {
            return Ok(vec![]);
        }
        if out.stdout.trim().eq_ignore_ascii_case("true") {
            return Ok(vec![]);
        }
        Ok(vec![HealthFinding::new(
            self.name(),
            "disabled",
            machine_id,
            HealthCategory::Runtime,
            Severity::Info,
            "Docker experimental features not enabled",
            "The Docker daemon is running without experimental features. Some \
             container operations the fabric may use require them.",
        )
        .with_fix(FixAction::new(
            FIX_ID,
            "Enable Docker experimental",
            "Sets \"experimental\": true in /etc/docker/daemon.json and restarts dockerd.",
        ))])
    }

    async fn apply_fix(&self, fix_id: &str, _machine_id: &Id) -> Result<String> {
        if fix_id != FIX_ID {
            return Err(Error::not_found(format!("fix `{fix_id}`")));
        }
        // Read-modify-write the daemon config so we don't clobber other settings.
        let merged = merge_experimental(&std::fs::read_to_string(DAEMON_JSON).unwrap_or_default())?;
        std::fs::write(DAEMON_JSON, &merged)
            .map_err(|e| Error::provider("docker", format!("write {DAEMON_JSON}: {e}")))?;
        // Restart the daemon so the change takes effect.
        run_fix("systemctl", &["restart", "docker"]).await?;
        tracing::info!("enabled docker experimental features and restarted dockerd");
        Ok("Set experimental=true in /etc/docker/daemon.json and restarted dockerd.".to_string())
    }
}

/// Merge `"experimental": true` into an existing daemon.json document (or an
/// empty one), preserving any other keys. Pure and unit-tested.
fn merge_experimental(existing: &str) -> Result<String> {
    let mut doc: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(existing)
            .map_err(|e| Error::provider("docker", format!("parse daemon.json: {e}")))?
    };
    if !doc.is_object() {
        doc = serde_json::json!({});
    }
    doc["experimental"] = serde_json::Value::Bool(true);
    serde_json::to_string_pretty(&doc)
        .map_err(|e| Error::provider("docker", format!("encode daemon.json: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_into_empty() {
        let out = merge_experimental("").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["experimental"], serde_json::json!(true));
    }

    #[test]
    fn merge_preserves_existing_keys() {
        let out = merge_experimental(r#"{"log-driver":"json-file","experimental":false}"#).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["experimental"], serde_json::json!(true));
        assert_eq!(v["log-driver"], serde_json::json!("json-file"));
    }

    #[test]
    fn merge_rejects_garbage() {
        assert!(merge_experimental("not json at all {{{").is_err());
    }
}
