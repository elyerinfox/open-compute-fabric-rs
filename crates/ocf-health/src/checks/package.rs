//! Check: required host tools must be installed — and if not, offer an
//! OS-aware install fix via [`ocf_platform`].
//!
//! This is the bridge between fleet health and cross-OS package management. The
//! check probes each [`Capability`]'s binary; for any that's missing it emits a
//! finding whose fix resolves the host's package manager and installs the right
//! package (`apt-get install -y nftables` on Debian, `dnf install -y nftables`
//! on Fedora, …). On a host with no supported package manager (e.g. Windows) the
//! check stays silent rather than nagging about tools it can't install.

use crate::check::HealthCheck;
use crate::finding::{FixAction, HealthCategory, HealthFinding, Severity};
use ocf_core::prelude::*;
use ocf_platform::{Capability, PlatformService};
use std::sync::Arc;

/// Fix-id prefix: `"install-<capability>"`.
const FIX_PREFIX: &str = "install-";

/// Warns about missing required host tools and offers to install them through
/// the host's package manager.
pub struct PackageCheck {
    platform: Arc<PlatformService>,
    capabilities: Vec<Capability>,
}

impl PackageCheck {
    pub fn new(platform: Arc<PlatformService>, capabilities: Vec<Capability>) -> Self {
        PackageCheck {
            platform,
            capabilities,
        }
    }

    fn capability(&self, name: &str) -> Option<&Capability> {
        self.capabilities.iter().find(|c| c.name == name)
    }
}

impl Provider for PackageCheck {
    fn name(&self) -> &str {
        "packages"
    }
    fn description(&self) -> &str {
        "Required host tools are installed (cross-OS package check)"
    }
}

#[async_trait]
impl HealthCheck for PackageCheck {
    fn category(&self) -> HealthCategory {
        HealthCategory::Other
    }

    async fn check(&self, machine_id: &Id) -> Result<Vec<HealthFinding>> {
        // No package manager for this host (Windows/macOS/unknown) → we can't
        // install anything, so don't report missing tools we can't fix.
        let Some(pm) = self.platform.active_manager() else {
            return Ok(vec![]);
        };

        let mut findings = Vec::new();
        for cap in &self.capabilities {
            if self.platform.capability_present(cap) {
                continue;
            }
            let mut finding = HealthFinding::new(
                self.name(),
                &cap.name,
                machine_id,
                HealthCategory::Other,
                Severity::Warning,
                format!("`{}` is not installed", cap.name),
                format!(
                    "The `{}` tool (binary `{}`) is missing on this {} host, so the \
                     features that depend on it are unavailable.",
                    cap.name, cap.binary, self.platform.os().os
                ),
            );
            // Only offer the install button when there's a package mapping for the
            // active manager; otherwise the finding is informational.
            if let Some(package) = cap.package_for(pm.name()) {
                finding = finding.with_fix(FixAction::new(
                    format!("{FIX_PREFIX}{}", cap.name),
                    format!("Install {}", cap.name),
                    format!("Runs the {} install for `{package}` on this node.", pm.name()),
                ));
            }
            findings.push(finding);
        }
        Ok(findings)
    }

    async fn apply_fix(&self, fix_id: &str, _machine_id: &Id) -> Result<String> {
        let cap_name = fix_id
            .strip_prefix(FIX_PREFIX)
            .ok_or_else(|| Error::not_found(format!("fix `{fix_id}`")))?;
        let cap = self
            .capability(cap_name)
            .ok_or_else(|| Error::not_found(format!("capability `{cap_name}`")))?;
        self.platform.install_capability(cap).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocf_platform::builtin_capabilities;

    fn check() -> PackageCheck {
        let platform = Arc::new(PlatformService::detect().expect("platform"));
        PackageCheck::new(platform, builtin_capabilities())
    }

    #[tokio::test]
    async fn unknown_fix_id_is_rejected() {
        let c = check();
        assert!(c.apply_fix("not-an-install", &Id::named("m")).await.is_err());
        // A well-formed fix for an unknown capability is also rejected.
        assert!(c.apply_fix("install-nonsense", &Id::named("m")).await.is_err());
    }

    #[tokio::test]
    async fn check_runs_cleanly() {
        // On a host without a package manager this is empty; on Linux it may list
        // missing tools. Either way the sweep must complete.
        let c = check();
        let _ = c.check(&Id::named("node-local")).await.expect("check ran");
    }
}
