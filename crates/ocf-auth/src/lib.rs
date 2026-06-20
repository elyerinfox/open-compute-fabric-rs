//! # ocf-auth
//!
//! Authentication and host-account synchronization for the fabric.
//!
//! This crate answers two questions and nothing else: *who is this principal?*
//! and *does a matching local OS account exist?* It deliberately knows nothing
//! about what a principal is *allowed* to do — that is `ocf-authz`'s job — but
//! the [`Identity`] it produces (notably its `groups`) is exactly the input the
//! RBAC engine consumes.
//!
//! Authentication is pluggable: an [`Authenticator`] turns [`Credentials`] into
//! a verified [`Identity`], and concrete backends are registered by name in a
//! `Registry<dyn Authenticator>` ([`register_builtins`]). The default
//! [`LocalAuthenticator`] performs a real in-memory password check;
//! [`PamAuthenticator`] drives the host PAM stack via `pamtester` and
//! [`ActiveDirectoryAuthenticator`] performs a real LDAP bind via `ldapwhoami`.
//! Both shell out to host tooling, so they verify credentials for real where the
//! tools are installed and return [`ocf_core::error::Error::Provider`] where they
//! are not.
//!
//! Separately, [`HostUserSync`] projects an [`Identity`] onto a real local Unix
//! account; [`LinuxUserSync`] runs the real `useradd`/`usermod`/`userdel`
//! commands. Because the host operations are runtime `Command` invocations, the
//! crate still *compiles* on Windows and Linux alike — the binaries simply
//! aren't present off-host, which surfaces as a provider error rather than a
//! build failure.

pub mod authenticator;
pub mod exec;
pub mod hostsync;
pub mod identity;
pub mod providers;

pub use authenticator::{register_builtins, Authenticator};
pub use hostsync::{HostUserSync, LinuxUserSync};
pub use identity::{Credentials, Identity};
pub use providers::{ActiveDirectoryAuthenticator, LocalAuthenticator, PamAuthenticator};
