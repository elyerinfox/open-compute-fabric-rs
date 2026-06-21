//! The pluggable package-manager contract and shared command helpers.

use crate::os::HostOs;
use ocf_core::prelude::*;
use tokio::process::Command;

/// A host package manager (apt, dnf, pacman, apk, …).
///
/// Pluggable like every fabric contract: it extends [`Provider`] and registers
/// by name. [`applies_to`](PackageManager::applies_to) lets the platform pick
/// the right manager for the detected OS, and `is_installed`/`install` shell out
/// to it. Adding support for another OS is adding another `PackageManager`.
#[async_trait]
pub trait PackageManager: Provider {
    /// Whether this manager is the right one for `os` (by distro/ID_LIKE, or by
    /// the presence of its own driver binary as a fallback).
    fn applies_to(&self, os: &HostOs) -> bool;

    /// Whether `package` is currently installed.
    async fn is_installed(&self, package: &str) -> Result<bool>;

    /// Install `package`, returning a human-readable outcome. Typically needs
    /// root; a permission/other failure surfaces as a provider error.
    async fn install(&self, package: &str) -> Result<String>;
}

/// Run `cmd args...`, returning `(ran, success, stdout, stderr)`. A missing
/// binary yields `ran == false` rather than an error, so a probe can treat
/// "manager absent" as "doesn't apply".
pub(crate) async fn probe(cmd: &str, args: &[&str]) -> (bool, bool, String, String) {
    match Command::new(cmd).args(args).output().await {
        Ok(out) => (
            true,
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ),
        Err(_) => (false, false, String::new(), String::new()),
    }
}

/// Run an install-style command where a missing binary or non-zero exit is a
/// real failure. Returns stdout (trimmed) on success.
pub(crate) async fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| Error::provider(cmd, format!("failed to spawn `{cmd}`: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(Error::provider(
            cmd,
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

/// Register the built-in package managers into `reg`.
pub fn register_builtins(reg: &mut Registry<dyn PackageManager>) -> Result<()> {
    use crate::managers::{AptPackageManager, ApkPackageManager, DnfPackageManager, PacmanPackageManager};
    use std::sync::Arc;
    reg.register("apt", Arc::new(AptPackageManager::new()))?;
    reg.register("dnf", Arc::new(DnfPackageManager::new()))?;
    reg.register("pacman", Arc::new(PacmanPackageManager::new()))?;
    reg.register("apk", Arc::new(ApkPackageManager::new()))?;
    Ok(())
}
