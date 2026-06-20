//! Concrete [`crate::authenticator::Authenticator`] backends.
//!
//! All three perform a real check. [`LocalAuthenticator`] compares against an
//! in-memory account map; [`PamAuthenticator`] drives the host PAM stack via the
//! `pamtester` CLI; and [`ActiveDirectoryAuthenticator`] performs a real LDAP
//! bind via `ldapwhoami`. The directory-integrated backends shell out to host
//! tooling, returning [`ocf_core::error::Error::Provider`] where that tooling is
//! absent rather than fabricating an identity.

mod ad;
mod local;
mod pam;

pub use ad::ActiveDirectoryAuthenticator;
pub use local::LocalAuthenticator;
pub use pam::PamAuthenticator;
