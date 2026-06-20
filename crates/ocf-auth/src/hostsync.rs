//! Projecting fabric identities onto local host (Linux) accounts.

use crate::exec::{parse_id_groups, run_with_stdin};
use crate::identity::Identity;
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::BTreeSet;

/// Synchronizes authenticated [`Identity`]s into real local OS accounts.
///
/// Some workloads (SSH access, file ownership, sudo policy) need an actual Unix
/// user on the host, not just a fabric-level principal. A `HostUserSync`
/// reconciles the two: given an [`Identity`] it ensures a matching local account
/// exists, and on de-provisioning it removes one.
///
/// This is a host-mutating contract: concrete backends run real account
/// commands (`useradd`/`usermod`/`userdel`) against the host.
#[async_trait]
pub trait HostUserSync: Send + Sync {
    /// Ensure a local account exists for `identity` (idempotent).
    async fn sync_user(&self, identity: &Identity) -> Result<()>;

    /// Remove the local account named `username` (idempotent — removing an
    /// absent user succeeds).
    async fn remove_user(&self, username: &str) -> Result<()>;
}

/// A [`HostUserSync`] that targets Linux user accounts via `useradd`/`usermod`/
/// `userdel`.
///
/// `sync_user` creates a missing account with `useradd` (primary group, login
/// shell, optionally a home directory) and reconciles supplementary group
/// membership with `usermod -aG`; `remove_user` runs `userdel -r`. Account
/// existence is probed with `id <user>`. A short in-memory set of the usernames
/// we have reflected onto the host is kept as an observable cache alongside the
/// real operations.
///
/// The operations are runtime `Command` invocations, so the type compiles on
/// any platform; off a Linux host the `useradd`/etc. binaries are simply absent,
/// which surfaces as [`Error::provider`] (`"linux-user-sync"`).
pub struct LinuxUserSync {
    /// Default login shell passed to `useradd -s`.
    shell: String,
    /// Whether a home directory is created (`useradd -m`).
    create_home: bool,
    /// Usernames currently reflected on the host (an observable cache).
    synced: RwLock<BTreeSet<String>>,
}

impl LinuxUserSync {
    /// Create a Linux user sync with sensible defaults (`/bin/bash`, create home).
    pub fn new() -> Self {
        LinuxUserSync {
            shell: "/bin/bash".to_string(),
            create_home: true,
            synced: RwLock::new(BTreeSet::new()),
        }
    }

    /// Override the default login shell.
    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = shell.into();
        self
    }

    /// Override whether a home directory is created.
    pub fn with_create_home(mut self, create_home: bool) -> Self {
        self.create_home = create_home;
        self
    }

    /// Whether a username is currently tracked as present on the host.
    pub fn is_synced(&self, username: &str) -> bool {
        self.synced.read().contains(username)
    }

    /// Snapshot of every username currently tracked as present.
    pub fn synced_users(&self) -> Vec<String> {
        self.synced.read().iter().cloned().collect()
    }

    /// Render the `useradd` invocation that `sync_user` executes for `identity`
    /// as a single shell-style string. Exposed so callers (and tests) can
    /// inspect the intended effect; the live path uses [`Self::useradd_args`].
    pub fn useradd_command(&self, identity: &Identity) -> String {
        let mut cmd = String::from("useradd");
        if self.create_home {
            cmd.push_str(" -m");
        }
        cmd.push_str(&format!(" -s {}", self.shell));
        if !identity.display_name.is_empty() {
            cmd.push_str(&format!(" -c \"{}\"", identity.display_name));
        }
        if !identity.groups.is_empty() {
            cmd.push_str(&format!(" -G {}", identity.groups.join(",")));
        }
        cmd.push(' ');
        cmd.push_str(&identity.username);
        cmd
    }

    /// Build the argv (excluding the `useradd` program name) that creates a
    /// local account for `identity`. This is the executed form of
    /// [`Self::useradd_command`]; kept pure so it can be unit-tested.
    pub fn useradd_args(&self, identity: &Identity) -> Vec<String> {
        let mut args = Vec::new();
        if self.create_home {
            args.push("-m".to_string());
        }
        args.push("-s".to_string());
        args.push(self.shell.clone());
        if !identity.display_name.is_empty() {
            args.push("-c".to_string());
            args.push(identity.display_name.clone());
        }
        if !identity.groups.is_empty() {
            args.push("-G".to_string());
            args.push(identity.groups.join(","));
        }
        args.push(identity.username.clone());
        args
    }
}

impl Default for LinuxUserSync {
    fn default() -> Self {
        Self::new()
    }
}

/// Does a local account named `username` exist? Probes with `id <username>`,
/// which exits 0 iff the user resolves.
async fn user_exists(username: &str) -> Result<bool> {
    let (code, _stdout, _stderr) = run_with_stdin("id", &[username], None).await?;
    Ok(code == 0)
}

/// Run an account-management command, mapping a non-zero exit onto
/// [`Error::provider`] (`"linux-user-sync"`). A missing binary already surfaces
/// as a provider error from [`run_with_stdin`].
async fn run_account_command(cmd: &str, args: &[&str]) -> Result<()> {
    let (code, _stdout, stderr) = run_with_stdin(cmd, args, None).await?;
    if code != 0 {
        return Err(Error::provider(
            "linux-user-sync",
            format!("`{cmd} {}` exited {code}: {}", args.join(" "), stderr.trim()),
        ));
    }
    Ok(())
}

#[async_trait]
impl HostUserSync for LinuxUserSync {
    async fn sync_user(&self, identity: &Identity) -> Result<()> {
        if identity.username.is_empty() {
            return Err(Error::invalid("cannot sync a user with an empty username"));
        }

        let username = identity.username.as_str();

        if user_exists(username).await? {
            // Account already present: reconcile supplementary groups.
            tracing::info!(
                target: "ocf_auth::hostsync",
                user = %username,
                "local account exists; reconciling group membership",
            );
            if !identity.groups.is_empty() {
                let groups = identity.groups.join(",");
                run_account_command("usermod", &["-aG", &groups, username]).await?;
            }
        } else {
            // Account missing: create it, then ensure group membership. We build
            // the argv via `useradd_args` so the executed command matches
            // `useradd_command`.
            let owned_args = self.useradd_args(identity);
            let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();
            tracing::info!(
                target: "ocf_auth::hostsync",
                user = %username,
                command = %self.useradd_command(identity),
                "creating local linux account",
            );
            run_account_command("useradd", &args).await?;

            // `useradd -G` already set supplementary groups, but re-apply with
            // `usermod -aG` to be robust if the create path omitted them.
            if !identity.groups.is_empty() {
                let groups = identity.groups.join(",");
                run_account_command("usermod", &["-aG", &groups, username]).await?;
            }
        }

        self.synced.write().insert(identity.username.clone());
        Ok(())
    }

    async fn remove_user(&self, username: &str) -> Result<()> {
        if username.is_empty() {
            return Err(Error::invalid("cannot remove a user with an empty username"));
        }

        // Idempotent: removing an absent user succeeds without invoking userdel.
        if user_exists(username).await? {
            tracing::info!(
                target: "ocf_auth::hostsync",
                user = %username,
                command = %format!("userdel -r {username}"),
                "removing local linux account",
            );
            run_account_command("userdel", &["-r", username]).await?;
        } else {
            tracing::info!(
                target: "ocf_auth::hostsync",
                user = %username,
                "local account already absent; nothing to remove",
            );
        }

        self.synced.write().remove(username);
        Ok(())
    }
}

/// Read a user's current supplementary groups via `id -nG <username>`. Exposed
/// for callers that want to diff desired vs. actual membership before syncing.
pub async fn current_groups(username: &str) -> Result<Vec<String>> {
    let (code, stdout, stderr) = run_with_stdin("id", &["-nG", username], None).await?;
    if code != 0 {
        return Err(Error::provider(
            "linux-user-sync",
            format!("`id -nG {username}` exited {code}: {}", stderr.trim()),
        ));
    }
    Ok(parse_id_groups(&stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_username_is_rejected_on_sync() {
        let sync = LinuxUserSync::new();
        let err = sync
            .sync_user(&Identity::new(""))
            .await
            .expect_err("empty username invalid");
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn empty_username_is_rejected_on_remove() {
        let sync = LinuxUserSync::new();
        let err = sync
            .remove_user("")
            .await
            .expect_err("empty username invalid");
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn useradd_command_reflects_identity() {
        let sync = LinuxUserSync::new();
        let id = Identity::new("dave")
            .with_display_name("Dave")
            .with_group("docker")
            .with_group("sudo");
        let cmd = sync.useradd_command(&id);
        assert!(cmd.contains("useradd"));
        assert!(cmd.contains("-G docker,sudo"));
        assert!(cmd.ends_with("dave"));
    }

    #[test]
    fn useradd_args_match_command_shape() {
        let sync = LinuxUserSync::new();
        let id = Identity::new("dave")
            .with_display_name("Dave Lister")
            .with_group("docker")
            .with_group("sudo");
        let args = sync.useradd_args(&id);
        assert_eq!(
            args,
            vec![
                "-m".to_string(),
                "-s".to_string(),
                "/bin/bash".to_string(),
                "-c".to_string(),
                "Dave Lister".to_string(),
                "-G".to_string(),
                "docker,sudo".to_string(),
                "dave".to_string(),
            ]
        );
    }

    #[test]
    fn useradd_args_omit_home_when_disabled() {
        let sync = LinuxUserSync::new().with_create_home(false).with_shell("/sbin/nologin");
        let args = sync.useradd_args(&Identity::new("svc"));
        assert_eq!(
            args,
            vec![
                "-s".to_string(),
                "/sbin/nologin".to_string(),
                "svc".to_string(),
            ]
        );
        assert!(!args.contains(&"-m".to_string()));
    }

    // Requires root and `useradd`/`usermod`/`userdel`/`id` on a Linux host; run
    // explicitly with `cargo test -- --ignored` there.
    #[tokio::test]
    #[ignore = "needs root and useradd/usermod/userdel on a Linux host"]
    async fn real_sync_then_remove() {
        let sync = LinuxUserSync::new();
        let id = Identity::new("ocf-test-user").with_group("users");
        sync.sync_user(&id).await.expect("sync ok");
        assert!(sync.is_synced("ocf-test-user"));
        sync.remove_user("ocf-test-user").await.expect("remove ok");
        assert!(!sync.is_synced("ocf-test-user"));
    }
}
