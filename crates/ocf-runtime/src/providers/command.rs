//! Shared host-command execution and output parsing for the real backends.
//!
//! Every provider in this crate drives a real host binary (`docker`, `podman`,
//! `lxc-*`, `virsh`). They all go through [`run`], which spawns the process,
//! captures its output, and maps failures onto [`Error::provider`] so the
//! control plane sees a uniform error regardless of which tool failed.

use crate::workload::{RuntimeKind, Workload};
use ocf_core::prelude::*;
use tokio::process::Command;

/// The label key/value a backend stamps on every workload it creates, so that
/// `list` can recover exactly the workloads this fabric owns.
pub const OCF_LABEL: &str = "ocf=1";
/// The label key carrying the workload id (`ocf.workload=<id>`).
pub const OCF_WORKLOAD_KEY: &str = "ocf.workload";

/// Run `bin args...`, returning trimmed stdout on success.
///
/// * A missing binary (spawn failure) becomes `Error::provider(bin, <io err>)`.
/// * A non-zero exit becomes `Error::provider(bin, <stderr or stdout>)`.
///
/// This is the single choke point through which the providers touch the host,
/// so there is exactly one place that decides how tool failures are reported.
pub async fn run(bin: &str, args: &[String]) -> Result<String> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .await
        .map_err(|e| Error::provider(bin, format!("failed to spawn `{bin}`: {e}")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        Err(Error::provider(bin, message))
    }
}

/// Build the argument vector for `docker create` / `podman create`.
///
/// Shape:
/// `create --name <id> --label ocf=1 --label ocf.workload=<id>
///         [-e K=V ...] [--memory <bytes>] [--cpus <cores>] <image>`
///
/// The workload id is used as the container name so ids round-trip through
/// `docker ps`. Environment variables are emitted in sorted order (they live in
/// a `BTreeMap`) for deterministic, testable output.
pub fn container_create_args(workload: &Workload) -> Vec<String> {
    let id = workload.metadata.id.to_string();
    let mut args: Vec<String> = vec![
        "create".to_string(),
        "--name".to_string(),
        id.clone(),
        "--label".to_string(),
        OCF_LABEL.to_string(),
        "--label".to_string(),
        format!("{OCF_WORKLOAD_KEY}={id}"),
    ];

    for (k, v) in &workload.env {
        args.push("-e".to_string());
        args.push(format!("{k}={v}"));
    }

    if workload.resources.memory_bytes > 0 {
        args.push("--memory".to_string());
        args.push(workload.resources.memory_bytes.to_string());
    }

    if workload.resources.cpu_millis > 0 {
        // Docker/Podman `--cpus` takes whole cores; millicores / 1000.
        args.push("--cpus".to_string());
        args.push(format_cpus(workload.resources.cpu_millis));
    }

    args.push(workload.image.clone());
    args
}

/// Render millicores as a `--cpus` value (e.g. `1500` -> `"1.5"`, `2000` ->
/// `"2"`). Trailing zeros are trimmed so the argument is stable and minimal.
pub fn format_cpus(cpu_millis: u64) -> String {
    let cores = cpu_millis as f64 / 1000.0;
    let mut s = format!("{cores:.3}");
    while s.contains('.') && (s.ends_with('0') || s.ends_with('.')) {
        s.pop();
    }
    s
}

/// Map a `docker inspect`/`podman inspect` `.State.Status` string onto a
/// [`LifecycleState`].
///
/// Docker container states are: `created`, `running`, `paused`, `restarting`,
/// `removing`, `exited`, `dead`. We collapse them onto the fabric's lifecycle:
/// a freshly created or cleanly exited container is `Stopped`, a running one is
/// `Running`, and `dead` is `Failed`.
pub fn parse_container_status(status: &str) -> LifecycleState {
    match status.trim() {
        "running" => LifecycleState::Running,
        "restarting" => LifecycleState::Provisioning,
        "created" => LifecycleState::Stopped,
        "exited" => LifecycleState::Stopped,
        "paused" => LifecycleState::Paused,
        "removing" => LifecycleState::Stopping,
        "dead" => LifecycleState::Failed,
        _ => LifecycleState::Pending,
    }
}

/// Map a Docker/Podman `ps --format '{{.State}}'` state token onto a
/// [`LifecycleState`]. The `ps` `State` column uses the same vocabulary as
/// `inspect`'s `.State.Status`, so this defers to [`parse_container_status`].
pub fn parse_ps_state(state: &str) -> LifecycleState {
    parse_container_status(state)
}

/// Reconstruct a [`Workload`] from one line of
/// `docker ps -a --format '{{.ID}}|{{.Image}}|{{.Names}}|{{.State}}'`.
///
/// The container *name* is the workload id (that is how we created it), so ids
/// round-trip. Fields the runtime does not expose via `ps` (env, resource
/// requests, placement) come back at their defaults — the real tool owns that
/// state, and `inspect` would be used to recover the rest if a caller needed it.
pub fn workload_from_ps_line(line: &str) -> Option<Workload> {
    let mut parts = line.splitn(4, '|');
    let _container_id = parts.next()?;
    let image = parts.next()?;
    let name = parts.next()?;
    let state = parts.next()?;

    let mut workload = Workload::container(name, image);
    // The name we set on `create` is the workload id; restore it so the id the
    // caller created the workload with is the id they get back.
    workload.metadata.id = Id::named(name);
    workload.state = parse_ps_state(state);
    Some(workload)
}

/// Reconstruct every fabric-owned [`Workload`] from the raw output of
/// `docker ps -a --filter label=ocf=1 --format '...'`.
pub fn workloads_from_ps(output: &str) -> Vec<Workload> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(workload_from_ps_line)
        .collect()
}

/// Map an LXC container state (from `lxc-info -s` / `lxc-ls`) onto a
/// [`LifecycleState`].
///
/// LXC reports uppercase states: `RUNNING`, `STOPPED`, `STARTING`, `STOPPING`,
/// `FROZEN`, `ABORTING`. We collapse them onto the fabric lifecycle.
pub fn parse_lxc_state(state: &str) -> LifecycleState {
    match state.trim().to_ascii_uppercase().as_str() {
        "RUNNING" => LifecycleState::Running,
        "STOPPED" => LifecycleState::Stopped,
        "STARTING" => LifecycleState::Provisioning,
        "STOPPING" => LifecycleState::Stopping,
        "FROZEN" => LifecycleState::Paused,
        "ABORTING" => LifecycleState::Failed,
        _ => LifecycleState::Pending,
    }
}

/// Extract the `State:` field from `lxc-info -n <name>` output.
///
/// `lxc-info` prints lines like `State:          RUNNING`; we find that line and
/// return the mapped [`LifecycleState`]. If no `State:` line is present the
/// container's state is unknown, surfaced as `Pending`.
pub fn lxc_info_state(output: &str) -> LifecycleState {
    for line in output.lines() {
        if let Some(rest) = line.trim().strip_prefix("State:") {
            return parse_lxc_state(rest);
        }
    }
    LifecycleState::Pending
}

/// Reconstruct workloads from `lxc-ls -1 -f -F NAME,STATE` output.
///
/// The first line is the `NAME STATE` header (whitespace-separated columns); we
/// skip it and read `name state` pairs. LXC has no image/label concept, so the
/// reconstructed image is a placeholder and ids round-trip via the container
/// name.
pub fn workloads_from_lxc_ls(output: &str) -> Vec<Workload> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        // Drop the column header emitted by `-f` (fancy) mode.
        .filter(|l| !l.starts_with("NAME"))
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let name = cols.next()?;
            let state = cols.next().unwrap_or("");
            let mut workload = Workload::container(name, "<lxc-rootfs>");
            workload.metadata.id = Id::named(name);
            workload.state = parse_lxc_state(state);
            Some(workload)
        })
        .collect()
}

/// Map a libvirt domain state (from `virsh domstate`) onto a [`LifecycleState`].
///
/// `virsh domstate` prints one of: `running`, `idle`, `paused`, `in shutdown`,
/// `shut off`, `crashed`, `pmsuspended`.
pub fn parse_virsh_state(state: &str) -> LifecycleState {
    match state.trim() {
        "running" => LifecycleState::Running,
        "idle" => LifecycleState::Running,
        "paused" => LifecycleState::Paused,
        "pmsuspended" => LifecycleState::Paused,
        "in shutdown" => LifecycleState::Stopping,
        "shut off" => LifecycleState::Stopped,
        "crashed" => LifecycleState::Failed,
        _ => LifecycleState::Pending,
    }
}

/// Reconstruct workloads from `virsh list --all --name` output (one domain name
/// per line; blank lines separate and terminate the list).
pub fn workloads_from_virsh_list(output: &str) -> Vec<Workload> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|name| {
            let mut workload = Workload::virtual_machine(name, "<libvirt-domain>");
            workload.metadata.id = Id::named(name);
            workload
        })
        .collect()
}

/// Reject a workload whose [`RuntimeKind`] does not match what this backend runs.
pub fn require_kind(workload: &Workload, expected: RuntimeKind, backend: &str) -> Result<()> {
    if workload.kind != expected {
        return Err(Error::invalid(format!(
            "{backend} backend only runs {}s, got {}",
            expected.as_str(),
            workload.kind.as_str()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_covers_docker_states() {
        assert_eq!(parse_container_status("running"), LifecycleState::Running);
        assert_eq!(parse_container_status("exited"), LifecycleState::Stopped);
        assert_eq!(parse_container_status("created"), LifecycleState::Stopped);
        assert_eq!(parse_container_status("paused"), LifecycleState::Paused);
        assert_eq!(
            parse_container_status("restarting"),
            LifecycleState::Provisioning
        );
        assert_eq!(parse_container_status("removing"), LifecycleState::Stopping);
        assert_eq!(parse_container_status("dead"), LifecycleState::Failed);
        // Whitespace is tolerated and unknown tokens fall back to Pending.
        assert_eq!(parse_container_status("  running\n"), LifecycleState::Running);
        assert_eq!(parse_container_status("bogus"), LifecycleState::Pending);
    }

    #[test]
    fn create_args_use_id_as_name_and_stamp_labels() {
        let mut wl = Workload::container("web", "nginx:1.27");
        wl.metadata.id = Id::named("web-1");
        let args = container_create_args(&wl);

        let id = "web-1".to_string();
        // Leading fixed prefix: create --name <id> --label ocf=1 --label ocf.workload=<id>
        assert_eq!(
            args[..7],
            [
                "create".to_string(),
                "--name".to_string(),
                id.clone(),
                "--label".to_string(),
                "ocf=1".to_string(),
                "--label".to_string(),
                "ocf.workload=web-1".to_string(),
            ]
        );
        // Image is always the final argument.
        assert_eq!(args.last().unwrap(), "nginx:1.27");
        // No resources requested => no --memory / --cpus flags.
        assert!(!args.iter().any(|a| a == "--memory"));
        assert!(!args.iter().any(|a| a == "--cpus"));
    }

    #[test]
    fn create_args_emit_env_memory_and_cpus() {
        let mut wl = Workload::container("web", "nginx:1.27");
        wl.metadata.id = Id::named("web-1");
        wl.env.insert("A".to_string(), "1".to_string());
        wl.env.insert("B".to_string(), "2".to_string());
        wl.resources = ResourceSpec::new(1500, 512 * 1024 * 1024, 0);
        let args = container_create_args(&wl);

        // Env emitted in sorted (BTreeMap) order, each as `-e K=V`.
        let env_idx = args.iter().position(|a| a == "-e").unwrap();
        assert_eq!(args[env_idx + 1], "A=1");
        assert_eq!(args[env_idx + 3], "B=2");

        let mem_idx = args.iter().position(|a| a == "--memory").unwrap();
        assert_eq!(args[mem_idx + 1], (512 * 1024 * 1024).to_string());

        let cpu_idx = args.iter().position(|a| a == "--cpus").unwrap();
        assert_eq!(args[cpu_idx + 1], "1.5");
    }

    #[test]
    fn format_cpus_trims_trailing_zeros() {
        assert_eq!(format_cpus(2000), "2");
        assert_eq!(format_cpus(1500), "1.5");
        assert_eq!(format_cpus(250), "0.25");
        assert_eq!(format_cpus(1000), "1");
    }

    #[test]
    fn workloads_reconstructed_from_ps_output() {
        let out = "abc123|nginx:1.27|web-1|running\n\
                   def456|redis:7|cache-1|exited\n";
        let workloads = workloads_from_ps(out);
        assert_eq!(workloads.len(), 2);

        assert_eq!(workloads[0].metadata.id.as_str(), "web-1");
        assert_eq!(workloads[0].image, "nginx:1.27");
        assert_eq!(workloads[0].state, LifecycleState::Running);

        assert_eq!(workloads[1].metadata.id.as_str(), "cache-1");
        assert_eq!(workloads[1].image, "redis:7");
        assert_eq!(workloads[1].state, LifecycleState::Stopped);
    }

    #[test]
    fn lxc_state_and_info_parse() {
        assert_eq!(parse_lxc_state("RUNNING"), LifecycleState::Running);
        assert_eq!(parse_lxc_state("stopped"), LifecycleState::Stopped);
        assert_eq!(parse_lxc_state("FROZEN"), LifecycleState::Paused);

        let info = "Name:           web-1\nState:          RUNNING\nPID:    4242\n";
        assert_eq!(lxc_info_state(info), LifecycleState::Running);
        assert_eq!(lxc_info_state("Name: x\n"), LifecycleState::Pending);
    }

    #[test]
    fn lxc_ls_reconstructs_workloads() {
        let out = "NAME   STATE\nweb-1  RUNNING\ncache-1 STOPPED\n";
        let wls = workloads_from_lxc_ls(out);
        assert_eq!(wls.len(), 2);
        assert_eq!(wls[0].metadata.id.as_str(), "web-1");
        assert_eq!(wls[0].state, LifecycleState::Running);
        assert_eq!(wls[1].metadata.id.as_str(), "cache-1");
        assert_eq!(wls[1].state, LifecycleState::Stopped);
    }

    #[test]
    fn virsh_state_and_list_parse() {
        assert_eq!(parse_virsh_state("running"), LifecycleState::Running);
        assert_eq!(parse_virsh_state("shut off"), LifecycleState::Stopped);
        assert_eq!(parse_virsh_state("paused"), LifecycleState::Paused);
        assert_eq!(parse_virsh_state("crashed"), LifecycleState::Failed);

        let out = "db-1\n\nweb-1\n";
        let wls = workloads_from_virsh_list(out);
        assert_eq!(wls.len(), 2);
        assert_eq!(wls[0].metadata.id.as_str(), "db-1");
        assert_eq!(wls[0].kind, RuntimeKind::VirtualMachine);
        assert_eq!(wls[1].metadata.id.as_str(), "web-1");
    }
}
