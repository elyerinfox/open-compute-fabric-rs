//! The platform façade: detected OS + the package managers, with capability
//! resolution and installation.

use crate::capability::Capability;
use crate::os::HostOs;
use crate::package::{register_builtins, PackageManager};
use ocf_core::prelude::*;
use std::sync::Arc;

/// Per-capability status, for the API / dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityStatus {
    pub name: String,
    pub binary: String,
    /// Whether the binary is present on this host.
    pub present: bool,
    /// The package that would install it under the active package manager (if
    /// there is one and a mapping exists).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
}

/// A snapshot of the host platform and its capability readiness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformStatus {
    pub os: HostOs,
    /// The package manager selected for this host, if any (`None` on a host with
    /// no supported manager — e.g. Windows/macOS).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_manager: Option<String>,
    pub capabilities: Vec<CapabilityStatus>,
}

/// Owns the detected [`HostOs`] and the [`PackageManager`] registry, and turns
/// "we need capability X" into the right install command for this host.
pub struct PlatformService {
    os: HostOs,
    managers: Registry<dyn PackageManager>,
}

impl PlatformService {
    /// Detect the host OS and register the built-in package managers.
    pub fn detect() -> Result<Self> {
        let os = HostOs::detect();
        let mut managers = Registry::new();
        register_builtins(&mut managers)?;
        tracing::info!(
            os = %os.os,
            distro = %os.distro,
            "platform detected"
        );
        Ok(PlatformService { os, managers })
    }

    pub fn new(os: HostOs, managers: Registry<dyn PackageManager>) -> Self {
        PlatformService { os, managers }
    }

    pub fn os(&self) -> &HostOs {
        &self.os
    }

    pub fn managers(&self) -> &Registry<dyn PackageManager> {
        &self.managers
    }

    /// The package manager that applies to this host, if any. The first
    /// registered manager whose [`applies_to`](PackageManager::applies_to)
    /// matches wins.
    pub fn active_manager(&self) -> Option<Arc<dyn PackageManager>> {
        self.managers
            .all()
            .into_iter()
            .find(|m| m.applies_to(&self.os))
    }

    /// Whether a capability's binary is present on this host.
    pub fn capability_present(&self, cap: &Capability) -> bool {
        cap.is_present()
    }

    /// Install the package that provides `cap` using the active package manager.
    ///
    /// Errors with `NotSupported` when the host has no package manager, or
    /// `NotFound` when the capability has no package mapping for that manager.
    pub async fn install_capability(&self, cap: &Capability) -> Result<String> {
        let pm = self.active_manager().ok_or_else(|| {
            Error::unsupported(format!(
                "no supported package manager for host OS `{}`",
                self.os.os
            ))
        })?;
        let package = cap.package_for(pm.name()).ok_or_else(|| {
            Error::not_found(format!(
                "capability `{}` has no `{}` package mapping",
                cap.name,
                pm.name()
            ))
        })?;
        pm.install(package).await
    }

    /// A status snapshot over `caps` for the API/dashboard.
    pub fn status(&self, caps: &[Capability]) -> PlatformStatus {
        let active = self.active_manager().map(|m| m.name().to_string());
        let capabilities = caps
            .iter()
            .map(|c| CapabilityStatus {
                name: c.name.clone(),
                binary: c.binary.clone(),
                present: c.is_present(),
                package: active.as_deref().and_then(|pm| c.package_for(pm).map(str::to_string)),
            })
            .collect();
        PlatformStatus {
            os: self.os.clone(),
            active_manager: active,
            capabilities,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::builtin_capabilities;

    #[test]
    fn detect_builds_a_service_with_managers() {
        let svc = PlatformService::detect().expect("detect");
        assert_eq!(svc.managers().len(), 4);
        // A status snapshot lists every capability with a present flag.
        let status = svc.status(&builtin_capabilities());
        assert_eq!(status.capabilities.len(), builtin_capabilities().len());
        assert_eq!(status.os.os, std::env::consts::OS);
    }

    #[tokio::test]
    async fn install_without_manager_is_unsupported() {
        // Force a host with no package manager (empty registry).
        let svc = PlatformService::new(
            HostOs {
                os: "plan9".into(),
                distro: String::new(),
                id_like: vec![],
                pretty: String::new(),
            },
            Registry::new(),
        );
        let cap = builtin_capabilities().into_iter().next().unwrap();
        let err = svc.install_capability(&cap).await.unwrap_err();
        assert_eq!(err.code(), "not_supported");
    }
}
