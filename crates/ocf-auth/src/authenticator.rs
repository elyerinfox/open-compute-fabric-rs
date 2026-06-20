//! The pluggable authentication contract.

use crate::identity::{Credentials, Identity};
use crate::providers::{ActiveDirectoryAuthenticator, LocalAuthenticator, PamAuthenticator};
use ocf_core::prelude::*;
use std::sync::Arc;

/// A pluggable backend that turns [`Credentials`] into a verified [`Identity`].
///
/// Backends are swappable plugins registered in a `Registry<dyn Authenticator>`:
/// the controller authenticates against a named backend (`"local"`, `"pam"`,
/// `"active-directory"`) without depending on which directory is in use. An
/// implementation that does not understand the presented credential variant
/// returns [`Error::unsupported`]; a credential mismatch returns
/// [`Error::Unauthenticated`].
#[async_trait]
pub trait Authenticator: Provider {
    /// Verify `credentials` and, on success, return the resolved identity.
    async fn authenticate(&self, credentials: &Credentials) -> Result<Identity>;
}

/// Register the built-in authentication backends into `reg`.
///
/// Seeds the in-memory [`LocalAuthenticator`] (a real password check), the
/// [`PamAuthenticator`] (host PAM via `pamtester`), and the
/// [`ActiveDirectoryAuthenticator`] (LDAP bind via `ldapwhoami`), so the
/// controller starts with a working `"local"` directory plus host/enterprise
/// directories that verify credentials for real wherever their CLI tooling is
/// installed.
pub fn register_builtins(reg: &mut Registry<dyn Authenticator>) -> Result<()> {
    reg.register("local", Arc::new(LocalAuthenticator::new()))?;
    reg.register("pam", Arc::new(PamAuthenticator::new()))?;
    reg.register(
        "active-directory",
        Arc::new(ActiveDirectoryAuthenticator::new("EXAMPLE.COM")),
    )?;
    Ok(())
}
