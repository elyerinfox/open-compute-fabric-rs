//! Small helper for shelling out to host authentication / account tooling.
//!
//! The directory backends (`pamtester`, `ldapwhoami`, `ldapsearch`) and the host
//! user sync (`useradd`/`usermod`/`userdel`, `id`) all drive their real binary
//! through [`run_with_stdin`]. Unlike `ocf-kernel`/`ocf-inventory`'s `exec::run`,
//! this helper returns the raw exit code alongside captured stdout/stderr instead
//! of treating a non-zero exit as an error: for an authentication probe a
//! non-zero exit means *the credential was rejected*, which is a normal outcome
//! the caller maps onto [`Error::Unauthenticated`], not a provider failure. Only
//! a genuine spawn failure (the binary isn't installed, or we're not on a host
//! that has it) is surfaced as [`Error::Provider`] tagged with the command name.
//!
//! Secrets are passed on the child's **stdin**, never as an argv element (which
//! would be visible in `ps`/`/proc`), and are never logged.

use ocf_core::prelude::*;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Run `cmd args...`, optionally writing `stdin` to the child's standard input,
/// and return `(exit_code, stdout, stderr)`.
///
/// `exit_code` is the process's exit status code, or `-1` if it was terminated
/// by a signal (no numeric code available). stdout/stderr are decoded lossily as
/// UTF-8. If the binary cannot be spawned at all this returns
/// [`Error::Provider`] tagged with `cmd`; a process that runs to completion —
/// even with a non-zero status — is reported as `Ok` so the caller can interpret
/// the exit code itself.
///
/// When `stdin` is `Some`, it is written verbatim to the child and the pipe is
/// then closed (EOF), which is how tools like `pamtester` read a password
/// through their conversation function.
pub async fn run_with_stdin(
    cmd: &str,
    args: &[&str],
    stdin: Option<&str>,
) -> Result<(i32, String, String)> {
    let mut command = Command::new(cmd);
    command
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|e| Error::provider(cmd, format!("failed to spawn `{cmd}`: {e}")))?;

    if let Some(input) = stdin {
        // Take the pipe so it is dropped (closed, signalling EOF) once we have
        // written the secret, even if the write itself fails.
        let mut sink = child
            .stdin
            .take()
            .ok_or_else(|| Error::provider(cmd, format!("`{cmd}` stdin pipe was not captured")))?;
        sink.write_all(input.as_bytes())
            .await
            .map_err(|e| Error::provider(cmd, format!("failed writing to `{cmd}` stdin: {e}")))?;
        // Explicit shutdown closes the write half so the child sees EOF.
        sink.shutdown()
            .await
            .map_err(|e| Error::provider(cmd, format!("failed closing `{cmd}` stdin: {e}")))?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| Error::provider(cmd, format!("failed waiting on `{cmd}`: {e}")))?;

    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    Ok((code, stdout, stderr))
}

/// Parse the output of `id -nG <user>` — a single line of space-separated group
/// names — into an owned `Vec<String>` in the order printed.
///
/// `id -nG` prints the primary and supplementary groups on one whitespace-joined
/// line (a trailing newline is normal); empty/blank output yields no groups.
pub fn parse_id_groups(output: &str) -> Vec<String> {
    output
        .split_whitespace()
        .map(|g| g.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_space_separated_groups() {
        let groups = parse_id_groups("alice dev docker sudo\n");
        assert_eq!(groups, vec!["alice", "dev", "docker", "sudo"]);
    }

    #[test]
    fn handles_extra_whitespace_and_blank_lines() {
        assert_eq!(parse_id_groups("  wheel   users \n"), vec!["wheel", "users"]);
        assert!(parse_id_groups("").is_empty());
        assert!(parse_id_groups("   \n").is_empty());
    }
}
