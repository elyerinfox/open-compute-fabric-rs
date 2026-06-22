//! The built-in package managers: apt, dnf, pacman, apk.
//!
//! Each maps the [`PackageManager`] contract onto its CLI. Detection is by the
//! host's distro/`ID_LIKE` with the install driver's presence as a fallback (so
//! a minimal image without `/etc/os-release` is still recognised). Note that the
//! *install* driver and the *query* command can differ (apt installs with
//! `apt-get` but queries with `dpkg`).

use crate::os::{binary_available, HostOs};
use crate::package::{probe, run, PackageManager};
use crate::update::{
    parse_apk_installed, parse_apk_updates, parse_apt_upgradable, parse_dnf_security_names,
    parse_dnf_upgrades, parse_dpkg_installed, parse_pacman_installed, parse_pacman_updates,
    parse_rpm_installed, InstalledPackage, PackageUpdate,
};
use ocf_core::prelude::*;

/// Run an upgrade-style command from owned argv (for variadic package lists).
async fn run_owned(cmd: &str, args: &[String]) -> Result<String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run(cmd, &refs).await
}

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
    async fn list_updates(&self) -> Result<Vec<PackageUpdate>> {
        let _ = probe("apt-get", &["update", "-qq"]).await; // refresh (root); else stale cache
        let (ran, _ok, out, _err) = probe("apt", &["list", "--upgradable"]).await;
        if !ran {
            return Err(Error::provider("apt", "`apt` not available"));
        }
        Ok(parse_apt_upgradable(&out))
    }
    async fn apply_updates(&self, security_only: bool) -> Result<String> {
        let _ = probe("apt-get", &["update", "-qq"]).await;
        if security_only {
            let names: Vec<String> = self
                .list_updates()
                .await?
                .into_iter()
                .filter(|u| u.security)
                .map(|u| u.name)
                .collect();
            if names.is_empty() {
                return Ok("No security updates to apply.".to_string());
            }
            let mut argv = vec![
                "install".to_string(),
                "-y".to_string(),
                "--only-upgrade".to_string(),
            ];
            argv.extend(names.iter().cloned());
            run_owned("apt-get", &argv).await?;
            Ok(format!("Applied {} security update(s) via apt-get.", names.len()))
        } else {
            run("apt-get", &["upgrade", "-y"]).await?;
            Ok("Applied available updates via apt-get.".to_string())
        }
    }
    async fn list_installed_packages(&self) -> Result<Vec<InstalledPackage>> {
        let (ran, _ok, out, _err) =
            probe("dpkg-query", &["-W", "-f", "${Package} ${Version}\n"]).await;
        if !ran {
            return Err(Error::provider("apt", "`dpkg-query` not available"));
        }
        Ok(parse_dpkg_installed(&out))
    }
    fn osv_ecosystem(&self, os: &HostOs) -> Option<String> {
        // OSV uses "Ubuntu" / "Debian" for these distros.
        if os.matches("ubuntu") {
            Some("Ubuntu".to_string())
        } else {
            Some("Debian".to_string())
        }
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
    async fn list_updates(&self) -> Result<Vec<PackageUpdate>> {
        let (sran, _ok, sec_out, _e) =
            probe("dnf", &["-q", "updateinfo", "list", "security"]).await;
        let security_names = if sran {
            parse_dnf_security_names(&sec_out)
        } else {
            Vec::new()
        };
        let (ran, _ok2, list_out, _e2) = probe("dnf", &["-q", "list", "--upgrades"]).await;
        if !ran {
            return Err(Error::provider("dnf", "`dnf` not available"));
        }
        Ok(parse_dnf_upgrades(&list_out, &security_names))
    }
    async fn apply_updates(&self, security_only: bool) -> Result<String> {
        if security_only {
            run("dnf", &["upgrade", "--security", "-y"]).await?;
            Ok("Applied security updates via dnf.".to_string())
        } else {
            run("dnf", &["upgrade", "-y"]).await?;
            Ok("Applied available updates via dnf.".to_string())
        }
    }
    async fn list_installed_packages(&self) -> Result<Vec<InstalledPackage>> {
        let (ran, _ok, out, _e) = probe("rpm", &["-qa", "--qf", "%{NAME} %{VERSION}\n"]).await;
        if !ran {
            return Err(Error::provider("dnf", "`rpm` not available"));
        }
        Ok(parse_rpm_installed(&out))
    }
    fn osv_ecosystem(&self, os: &HostOs) -> Option<String> {
        // OSV tracks Rocky/AlmaLinux/RHEL under those ecosystem names.
        if os.matches("rocky") {
            Some("Rocky Linux".to_string())
        } else if os.matches("almalinux") {
            Some("AlmaLinux".to_string())
        } else {
            Some("Red Hat".to_string())
        }
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
    async fn list_updates(&self) -> Result<Vec<PackageUpdate>> {
        // `checkupdates` (pacman-contrib) is root-free and uses a temp DB; fall
        // back to `pacman -Qu` (needs an already-synced DB).
        let (cran, cok, cout, _e) = probe("checkupdates", &[]).await;
        if cran && cok {
            return Ok(parse_pacman_updates(&cout));
        }
        let (ran, _ok, out, _e2) = probe("pacman", &["-Qu"]).await;
        if !ran {
            return Err(Error::provider("pacman", "`pacman` not available"));
        }
        Ok(parse_pacman_updates(&out))
    }
    async fn apply_updates(&self, _security_only: bool) -> Result<String> {
        // Arch is rolling — no security-only path; a full sync upgrade.
        run("pacman", &["-Syu", "--noconfirm"]).await?;
        Ok("Applied available updates via pacman.".to_string())
    }
    async fn list_installed_packages(&self) -> Result<Vec<InstalledPackage>> {
        let (ran, _ok, out, _e) = probe("pacman", &["-Q"]).await;
        if !ran {
            return Err(Error::provider("pacman", "`pacman` not available"));
        }
        Ok(parse_pacman_installed(&out))
    }
    // OSV does not track Arch Linux as an ecosystem → `osv_ecosystem` stays None.
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
    async fn list_updates(&self) -> Result<Vec<PackageUpdate>> {
        let _ = probe("apk", &["update"]).await; // refresh the index (best-effort)
        let (ran, _ok, out, _e) = probe("apk", &["version", "-l", "<"]).await;
        if !ran {
            return Err(Error::provider("apk", "`apk` not available"));
        }
        Ok(parse_apk_updates(&out))
    }
    async fn apply_updates(&self, _security_only: bool) -> Result<String> {
        run("apk", &["upgrade"]).await?;
        Ok("Applied available updates via apk.".to_string())
    }
    async fn list_installed_packages(&self) -> Result<Vec<InstalledPackage>> {
        let (ran, _ok, out, _e) = probe("apk", &["info", "-v"]).await;
        if !ran {
            return Err(Error::provider("apk", "`apk` not available"));
        }
        Ok(parse_apk_installed(&out))
    }
    fn osv_ecosystem(&self, _os: &HostOs) -> Option<String> {
        Some("Alpine".to_string())
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
