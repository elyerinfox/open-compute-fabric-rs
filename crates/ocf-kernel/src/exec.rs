//! Small helper for shelling out to host tooling.
//!
//! Every OS-touching subsystem (`ip`, `nft`, `iptables`, `systemctl`) drives the
//! real binary through [`run`]. It captures stdout/stderr and maps any failure
//! — including a missing binary — onto [`Error::Provider`] tagged with the
//! command name, so callers get a uniform error surface and never panic.

use ocf_core::prelude::*;
use tokio::process::Command;

/// Run `cmd args...`, returning captured stdout on success.
///
/// On a non-zero exit, the captured stderr (falling back to stdout) becomes the
/// provider error message. If the binary can't be spawned at all (e.g. it isn't
/// installed, or we're not on Linux), that spawn error is likewise reported as a
/// provider error tagged with `cmd`.
pub async fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| Error::provider(cmd, format!("failed to spawn `{cmd}`: {e}")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = if stderr.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        Err(Error::provider(
            cmd,
            format!("`{cmd} {}` exited {code}: {message}", args.join(" ")),
        ))
    }
}
