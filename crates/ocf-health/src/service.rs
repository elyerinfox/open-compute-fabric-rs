//! The aggregation service: run every registered check and route fix requests.

use crate::check::HealthCheck;
use crate::finding::{HealthFinding, Severity};
use ocf_core::prelude::*;
use std::collections::BTreeMap;

/// Runs the registered [`HealthCheck`]s for a node and applies their fixes.
///
/// This is the façade the controller and API talk to. It owns a [`Registry`] of
/// checks; `run` fans every check out (a failing check is logged and skipped,
/// never aborting the sweep) and `apply_fix` routes a remediation to the check
/// that owns it.
pub struct HealthService {
    checks: Registry<dyn HealthCheck>,
}

impl HealthService {
    pub fn new(checks: Registry<dyn HealthCheck>) -> Self {
        HealthService { checks }
    }

    /// Build a service with the built-in checks registered.
    pub fn with_builtins() -> Result<Self> {
        let mut reg = Registry::new();
        crate::register_builtins(&mut reg)?;
        Ok(Self::new(reg))
    }

    /// The underlying check registry (for introspection / `providers`).
    pub fn checks(&self) -> &Registry<dyn HealthCheck> {
        &self.checks
    }

    /// Run every check against `machine_id`, returning all findings sorted by
    /// descending severity. A check that errors is logged and skipped.
    pub async fn run(&self, machine_id: &Id) -> Vec<HealthFinding> {
        let mut findings = Vec::new();
        for check in self.checks.all() {
            match check.check(machine_id).await {
                Ok(mut f) => findings.append(&mut f),
                Err(e) => {
                    tracing::warn!(check = %check.name(), error = %e, "health check failed");
                }
            }
        }
        // Most severe first, then by title for stable ordering.
        findings.sort_by(|a, b| {
            b.severity
                .cmp(&a.severity)
                .then_with(|| a.title.cmp(&b.title))
        });
        findings
    }

    /// A `{severity -> count}` summary for a dashboard badge.
    pub async fn summary(&self, machine_id: &Id) -> BTreeMap<String, usize> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for f in self.run(machine_id).await {
            *counts.entry(f.severity.as_str().to_string()).or_insert(0) += 1;
        }
        counts
    }

    /// Apply `fix_id` from `check` against `machine_id`, returning its outcome.
    pub async fn apply_fix(
        &self,
        check: &str,
        fix_id: &str,
        machine_id: &Id,
    ) -> Result<String> {
        let provider = self.checks.get(check)?;
        provider.apply_fix(fix_id, machine_id).await
    }

    /// Highest severity currently present (for a coarse node-health rollup).
    pub async fn worst_severity(&self, machine_id: &Id) -> Option<Severity> {
        self.run(machine_id).await.into_iter().map(|f| f.severity).max()
    }
}
