//! Check: pending OS package updates, with a **security** warning.
//!
//! Lists the host's available package updates via [`ocf_platform`] and, when any
//! address security issues, raises a `Warning` with one-press fixes to apply the
//! security updates (or all of them). On a host without a supported package
//! manager it stays silent.

use crate::check::HealthCheck;
use crate::finding::{FixAction, HealthCategory, HealthFinding, Severity};
use ocf_core::prelude::*;
use ocf_platform::PlatformService;
use std::sync::Arc;

const FIX_SECURITY: &str = "apply-security-updates";
const FIX_ALL: &str = "apply-all-updates";

/// Warns when package updates — especially security ones — are pending.
pub struct SecurityUpdateCheck {
    platform: Arc<PlatformService>,
}

impl SecurityUpdateCheck {
    pub fn new(platform: Arc<PlatformService>) -> Self {
        SecurityUpdateCheck { platform }
    }
}

impl Provider for SecurityUpdateCheck {
    fn name(&self) -> &str {
        "security-updates"
    }
    fn description(&self) -> &str {
        "Pending OS package updates, flagging security updates"
    }
}

#[async_trait]
impl HealthCheck for SecurityUpdateCheck {
    fn category(&self) -> HealthCategory {
        HealthCategory::Security
    }

    async fn check(&self, machine_id: &Id) -> Result<Vec<HealthFinding>> {
        // Best-effort: a query failure (no root to refresh, etc.) is silent.
        let summary = match self.platform.available_updates().await {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        if summary.manager.is_none() || summary.total == 0 {
            return Ok(Vec::new());
        }

        let finding = if summary.security > 0 {
            HealthFinding::new(
                self.name(),
                "security",
                machine_id,
                HealthCategory::Security,
                Severity::Warning,
                format!("{} security update(s) available", summary.security),
                format!(
                    "{} of {} pending package updates address security issues. Apply them promptly.",
                    summary.security, summary.total
                ),
            )
            .with_fix(FixAction::new(
                FIX_SECURITY,
                "Apply security updates",
                "Install only the pending security updates.",
            ))
            .with_fix(FixAction::new(
                FIX_ALL,
                "Apply all updates",
                "Install all pending package updates.",
            ))
        } else {
            HealthFinding::new(
                self.name(),
                "available",
                machine_id,
                HealthCategory::Security,
                Severity::Info,
                format!("{} package update(s) available", summary.total),
                format!(
                    "{} package updates are pending (none flagged as security).",
                    summary.total
                ),
            )
            .with_fix(FixAction::new(
                FIX_ALL,
                "Apply all updates",
                "Install all pending package updates.",
            ))
        };
        Ok(vec![finding])
    }

    async fn apply_fix(&self, fix_id: &str, _machine_id: &Id) -> Result<String> {
        match fix_id {
            FIX_SECURITY => self.platform.apply_updates(true).await,
            FIX_ALL => self.platform.apply_updates(false).await,
            _ => Err(Error::not_found(format!(
                "check `{}` does not offer fix `{fix_id}`",
                self.name()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_cleanly_and_rejects_unknown_fix() {
        let platform = Arc::new(PlatformService::detect().expect("platform"));
        let check = SecurityUpdateCheck::new(platform);
        // On a host without a package manager this is empty; either way it runs.
        let _ = check.check(&Id::named("node-local")).await.expect("check ran");
        assert!(check.apply_fix("nope", &Id::named("m")).await.is_err());
    }
}
