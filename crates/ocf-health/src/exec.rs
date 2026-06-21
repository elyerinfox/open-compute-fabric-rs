//! Shared probe helpers: read a sysctl/proc file and run a command, mapping
//! "not present / not this platform" onto soft signals so checks can decide
//! whether the absence is itself a finding.

use ocf_core::error::{Error, Result};
use tokio::process::Command;

/// Read a `/proc`/`/sys` file's trimmed contents. `Ok(None)` means the file does
/// not exist (e.g. not Linux, or the feature isn't present) — distinct from a
/// real read error.
pub fn read_sys(path: &str) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(format!("read {path}: {e}"))),
    }
}

/// Write a value to a `/proc`/`/sys` file (used by fixes like enabling
/// forwarding). Errors surface as a provider error tagged with the path.
pub fn write_sys(path: &str, value: &str) -> Result<()> {
    std::fs::write(path, value)
        .map_err(|e| Error::provider("sysfs", format!("write {path}: {e}")))
}

/// The result of running a probe command.
pub struct CmdOutput {
    pub ran: bool,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run `cmd args...`, capturing output. A missing binary yields `ran == false`
/// (the host can't be probed this way) rather than an error, so a check can
/// treat "tool absent" as "can't assess".
pub async fn run(cmd: &str, args: &[&str]) -> CmdOutput {
    match Command::new(cmd).args(args).output().await {
        Ok(out) => CmdOutput {
            ran: true,
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(_) => CmdOutput {
            ran: false,
            success: false,
            stdout: String::new(),
            stderr: String::new(),
        },
    }
}

/// Run `cmd args...` for a **fix**, where a missing binary or non-zero exit is a
/// real failure (the user asked us to remediate). Returns stdout on success.
pub async fn run_fix(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| Error::provider(cmd, format!("failed to spawn `{cmd}`: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(Error::provider(
            cmd,
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}
