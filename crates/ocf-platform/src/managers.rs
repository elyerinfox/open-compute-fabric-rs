//! The built-in package managers: apt, dnf, pacman, apk.
//!
//! Each maps the [`PackageManager`] contract onto its CLI. Detection is by the
//! host's distro/`ID_LIKE` with the install driver's presence as a fallback (so
//! a minimal image without `/etc/os-release` is still recognised). Note that the
//! *install* driver and the *query* command can differ (apt installs with
//! `apt-get` but queries with `dpkg`).

use crate::os::{binary_available, HostOs};
use crate::package::{probe, run, PackageManager};
use ocf_core::prelude::*;

/// Shared `is_installed` body: run a query command; a missing query binary is a
/// provider error, otherwise success/failure of the query is the answer.
async fn query_installed(manager: &str, cmd: &str, args: &[&str]) -> Result<bool> {
    let (ran, success, _out, _err) = probe(cmd, args).await;
    if !ran {
        return Err(Error::provider(
            manager,
            format!("query tool `{cmd}` not available"),
        ));
    }
    Ok(success)
}

// ---- apt (Debian/Ubuntu) --------------------------------------------------

#[derive(Debug, Default)]
pub struct AptPackageManager;
impl AptPackageManager {
    pub fn new() -> Self {
        AptPackageManager
    }
}
impl Provider for AptPackageManager {
    fn name(&self) -> &str {
        "apt"
    }
    fn description(&self) -> &str {
        "Debian/Ubuntu APT package manager (apt-get / dpkg)"
    }
}
#[async_trait]
impl PackageManager for AptPackageManager {
    fn applies_to(&self, os: &HostOs) -> bool {
        os.matches("debian") || os.matches("ubuntu") || binary_available("apt-get")
    }
    async fn is_installed(&self, package: &str) -> Result<bool> {
        query_installed("apt", "dpkg", &["-s", package]).await
    }
    async fn install(&self, package: &str) -> Result<String> {
        run("apt-get", &["install", "-y", package]).await?;
        tracing::info!(manager = "apt", package, "installed package");
        Ok(format!("Installed `{package}` via apt-get."))
    }
}

// ---- dnf (RHEL/Fedora) ----------------------------------------------------

#[derive(Debug, Default)]
pub struct DnfPackageManager;
impl DnfPackageManager {
    pub fn new() -> Self {
        DnfPackageManager
    }
}
impl Provider for DnfPackageManager {
    fn name(&self) -> &str {
        "dnf"
    }
    fn description(&self) -> &str {
        "RHEL/Fedora DNF package manager (dnf / rpm)"
    }
}
#[async_trait]
impl PackageManager for DnfPackageManager {
    fn applies_to(&self, os: &HostOs) -> bool {
        os.matches("fedora")
            || os.matches("rhel")
            || os.matches("centos")
            || binary_available("dnf")
    }
    async fn is_installed(&self, package: &str) -> Result<bool> {
        query_installed("dnf", "rpm", &["-q", package]).await
    }
    async fn install(&self, package: &str) -> Result<String> {
        run("dnf", &["install", "-y", package]).await?;
        tracing::info!(manager = "dnf", package, "installed package");
        Ok(format!("Installed `{package}` via dnf."))
    }
}

// ---- pacman (Arch) --------------------------------------------------------

#[derive(Debug, Default)]
pub struct PacmanPackageManager;
impl PacmanPackageManager {
    pub fn new() -> Self {
        PacmanPackageManager
    }
}
impl Provider for PacmanPackageManager {
    fn name(&self) -> &str {
        "pacman"
    }
    fn description(&self) -> &str {
        "Arch Linux pacman package manager"
    }
}
#[async_trait]
impl PackageManager for PacmanPackageManager {
    fn applies_to(&self, os: &HostOs) -> bool {
        os.matches("arch")
            || os.matches("archlinux")
            || os.matches("manjaro")
            || binary_available("pacman")
    }
    async fn is_installed(&self, package: &str) -> Result<bool> {
        query_installed("pacman", "pacman", &["-Q", package]).await
    }
    async fn install(&self, package: &str) -> Result<String> {
        run("pacman", &["-S", "--noconfirm", package]).await?;
        tracing::info!(manager = "pacman", package, "installed package");
        Ok(format!("Installed `{package}` via pacman."))
    }
}

// ---- apk (Alpine) ---------------------------------------------------------

#[derive(Debug, Default)]
pub struct ApkPackageManager;
impl ApkPackageManager {
    pub fn new() -> Self {
        ApkPackageManager
    }
}
impl Provider for ApkPackageManager {
    fn name(&self) -> &str {
        "apk"
    }
    fn description(&self) -> &str {
        "Alpine apk package manager"
    }
}
#[async_trait]
impl PackageManager for ApkPackageManager {
    fn applies_to(&self, os: &HostOs) -> bool {
        os.matches("alpine") || binary_available("apk")
    }
    async fn is_installed(&self, package: &str) -> Result<bool> {
        query_installed("apk", "apk", &["info", "-e", package]).await
    }
    async fn install(&self, package: &str) -> Result<String> {
        run("apk", &["add", package]).await?;
        tracing::info!(manager = "apk", package, "installed package");
        Ok(format!("Installed `{package}` via apk."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(distro: &str, like: &[&str]) -> HostOs {
        HostOs {
            os: "linux".into(),
            distro: distro.into(),
            id_like: like.iter().map(|s| s.to_string()).collect(),
            pretty: String::new(),
        }
    }

    #[test]
    fn apt_applies_to_debian_family() {
        let apt = AptPackageManager::new();
        assert!(apt.applies_to(&os("ubuntu", &["debian"])));
        assert!(apt.applies_to(&os("debian", &[])));
    }

    #[test]
    fn managers_detect_their_distros() {
        assert!(DnfPackageManager::new().applies_to(&os("centos", &["rhel", "fedora"])));
        assert!(PacmanPackageManager::new().applies_to(&os("arch", &[])));
        assert!(ApkPackageManager::new().applies_to(&os("alpine", &[])));
    }

    #[test]
    fn provider_names_are_stable() {
        assert_eq!(AptPackageManager::new().name(), "apt");
        assert_eq!(DnfPackageManager::new().name(), "dnf");
        assert_eq!(PacmanPackageManager::new().name(), "pacman");
        assert_eq!(ApkPackageManager::new().name(), "apk");
    }
}
