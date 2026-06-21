//! The pluggable health-check contract.

use crate::finding::{HealthCategory, HealthFinding};
use ocf_core::prelude::*;

/// A modular fleet-health check.
///
/// Each check inspects the **local node** and reports zero or more
/// [`HealthFinding`]s — a finding being present means something is wrong (or
/// noteworthy). A check that can't assess the host (e.g. the probe file/binary
/// is absent, or the node isn't Linux) returns an empty vector rather than
/// guessing. Checks extend [`Provider`] so they register by name in a
/// [`Registry`], which is what makes the warning set modular: adding a new
/// warning is adding a new `HealthCheck` implementation.
///
/// A finding may advertise [`FixAction`](crate::FixAction)s; the same check
/// executes them via [`apply_fix`](HealthCheck::apply_fix), so detection and
/// remediation live together.
#[async_trait]
pub trait HealthCheck: Provider {
    /// The subsystem this check relates to (for UI grouping).
    fn category(&self) -> HealthCategory;

    /// Inspect `machine_id`'s host and report findings (empty == healthy /
    /// not assessable).
    async fn check(&self, machine_id: &Id) -> Result<Vec<HealthFinding>>;

    /// Apply a fix this check advertised, returning a human-readable outcome.
    ///
    /// The default refuses unknown fixes; checks override it to remediate.
    async fn apply_fix(&self, fix_id: &str, machine_id: &Id) -> Result<String> {
        let _ = machine_id;
        Err(Error::not_found(format!(
            "check `{}` does not offer fix `{fix_id}`",
            self.name()
        )))
    }
}
