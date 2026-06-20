//! Host service (daemon) supervision.
//!
//! The fabric keeps a set of host services in a desired state — the agent, the
//! fabric mesh, the metrics exporter — and reconciles drift. [`ServiceManager`]
//! is the contract; the default [`SystemdServiceManager`] drives the real
//! `systemctl`. It tracks only the *desired* state of the units the fabric
//! manages (its intent); the live state is always read back from systemd.

use crate::exec::run;
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::BTreeMap;
use tokio::process::Command;

/// Observed (or desired) state of a host service/unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Running,
    Stopped,
    Failed,
    Unknown,
}

/// The outcome of reconciling a single service back to its desired state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub service: String,
    pub desired: ServiceState,
    pub previous: ServiceState,
    /// Whether a corrective action was taken.
    pub changed: bool,
}

/// Host service supervision contract.
///
/// Implementations report and converge the state of host units. The bundled
/// backend wraps `systemctl`; production deployments could swap in an
/// OpenRC/runit backend without changing callers.
#[async_trait]
pub trait ServiceManager: Send + Sync {
    /// Report the current state of `name`.
    async fn status(&self, name: &str) -> Result<ServiceState>;

    /// Drive `name` toward `desired` (start/stop as needed). Idempotent.
    async fn ensure(&self, name: &str, desired: ServiceState) -> Result<()>;

    /// Detect services whose live state has drifted from their desired state
    /// and correct them, returning a report per managed service.
    async fn reconcile(&self) -> Result<Vec<ReconcileReport>>;
}

/// Map a systemd `ActiveState` (from `systemctl is-active` or `show -p
/// ActiveState`) onto a [`ServiceState`].
///
/// `active` → Running, `inactive`/`deactivating` → Stopped, `failed` → Failed,
/// and anything else (including `activating`, `unknown`, or a missing unit) →
/// Unknown.
fn parse_active_state(raw: &str) -> ServiceState {
    // Accept both the bare word and the `ActiveState=active` form.
    let value = raw
        .trim()
        .rsplit('=')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match value.as_str() {
        "active" => ServiceState::Running,
        "inactive" | "deactivating" => ServiceState::Stopped,
        "failed" => ServiceState::Failed,
        _ => ServiceState::Unknown,
    }
}

/// The `systemctl` verb that drives a unit toward `desired`, or `None` for
/// states the fabric can't actively converge on (`Failed`/`Unknown`).
fn systemctl_verb(desired: ServiceState) -> Option<&'static str> {
    match desired {
        ServiceState::Running => Some("start"),
        ServiceState::Stopped => Some("stop"),
        ServiceState::Failed | ServiceState::Unknown => None,
    }
}

/// `systemd`-backed host service manager.
///
/// Drives `systemctl` to query and converge units. The only state held here is
/// the fabric's *intent*: the desired state of each unit it manages, which
/// `reconcile` compares against the live state read back from systemd.
pub struct SystemdServiceManager {
    /// `unit name -> desired state` for every unit the fabric manages.
    desired: RwLock<BTreeMap<String, ServiceState>>,
}

impl SystemdServiceManager {
    pub fn new() -> Self {
        SystemdServiceManager {
            desired: RwLock::new(BTreeMap::new()),
        }
    }

    /// Query the live state of `name` from systemd via
    /// `systemctl is-active <name>`.
    ///
    /// `is-active` deliberately exits non-zero for inactive/failed/missing units
    /// while still printing the state on stdout, so we invoke it directly and
    /// parse stdout regardless of exit status. Only a spawn failure (e.g.
    /// `systemctl` isn't installed) is a hard error.
    async fn live_state(&self, name: &str) -> Result<ServiceState> {
        let output = Command::new("systemctl")
            .args(["is-active", name])
            .output()
            .await
            .map_err(|e| {
                Error::provider("systemctl", format!("failed to spawn `systemctl`: {e}"))
            })?;
        Ok(parse_active_state(&String::from_utf8_lossy(&output.stdout)))
    }
}

impl Default for SystemdServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ServiceManager for SystemdServiceManager {
    async fn status(&self, name: &str) -> Result<ServiceState> {
        if name.is_empty() {
            return Err(Error::invalid("service name must not be empty"));
        }
        self.live_state(name).await
    }

    async fn ensure(&self, name: &str, desired: ServiceState) -> Result<()> {
        if name.is_empty() {
            return Err(Error::invalid("service name must not be empty"));
        }
        let Some(action) = systemctl_verb(desired) else {
            return Err(Error::invalid(format!(
                "cannot drive service {name} to {desired:?}"
            )));
        };
        run("systemctl", &[action, name]).await?;
        self.desired.write().insert(name.to_string(), desired);
        Ok(())
    }

    async fn reconcile(&self) -> Result<Vec<ReconcileReport>> {
        // Snapshot intent so we don't hold the lock across awaits.
        let managed: Vec<(String, ServiceState)> = self
            .desired
            .read()
            .iter()
            .map(|(name, desired)| (name.clone(), *desired))
            .collect();

        let mut reports = Vec::with_capacity(managed.len());
        for (name, desired) in managed {
            let previous = self.live_state(&name).await?;
            // Only converge on a drift when the desired state has an actionable
            // verb (`start`/`stop`); `Failed`/`Unknown` desired states can't be
            // driven, so they're reported but never acted on.
            let action = systemctl_verb(desired).filter(|_| previous != desired);
            let changed = action.is_some();
            if let Some(action) = action {
                run("systemctl", &[action, &name]).await?;
            }
            reports.push(ReconcileReport {
                service: name,
                desired,
                previous,
                changed,
            });
        }
        Ok(reports)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_is_active_words() {
        assert_eq!(parse_active_state("active"), ServiceState::Running);
        assert_eq!(parse_active_state("active\n"), ServiceState::Running);
        assert_eq!(parse_active_state("inactive"), ServiceState::Stopped);
        assert_eq!(parse_active_state("deactivating"), ServiceState::Stopped);
        assert_eq!(parse_active_state("failed"), ServiceState::Failed);
        assert_eq!(parse_active_state("activating"), ServiceState::Unknown);
        assert_eq!(parse_active_state("nonsense"), ServiceState::Unknown);
    }

    #[test]
    fn parses_show_property_form() {
        assert_eq!(
            parse_active_state("ActiveState=active"),
            ServiceState::Running
        );
        assert_eq!(
            parse_active_state("ActiveState=failed\n"),
            ServiceState::Failed
        );
    }

    #[tokio::test]
    async fn ensure_rejects_unactionable_state() {
        // Pure validation, reached before any systemctl invocation.
        let svc = SystemdServiceManager::new();
        assert!(svc.ensure("x", ServiceState::Failed).await.is_err());
        assert!(svc.ensure("", ServiceState::Running).await.is_err());
    }

    // Requires a real systemd host with a controllable unit.
    #[tokio::test]
    #[ignore = "requires systemd + a controllable unit"]
    async fn ensure_sets_status() {
        let svc = SystemdServiceManager::new();
        svc.ensure("ocf-agent", ServiceState::Running)
            .await
            .unwrap();
        assert_eq!(
            svc.status("ocf-agent").await.unwrap(),
            ServiceState::Running
        );
    }

    #[tokio::test]
    #[ignore = "requires systemd + a controllable unit"]
    async fn reconcile_fixes_drift() {
        let svc = SystemdServiceManager::new();
        svc.ensure("ocf-agent", ServiceState::Running)
            .await
            .unwrap();
        let reports = svc.reconcile().await.unwrap();
        assert_eq!(reports.len(), 1);
    }
}
