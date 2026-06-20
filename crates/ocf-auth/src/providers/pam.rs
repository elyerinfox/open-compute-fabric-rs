//! Linux PAM authentication, driven through the `pamtester` CLI.

use crate::authenticator::Authenticator;
use crate::exec::{parse_id_groups, run_with_stdin};
use crate::identity::{Credentials, Identity};
use ocf_core::prelude::*;

/// Authenticates against the host's PAM stack.
///
/// This drives the host PAM stack through the `pamtester` binary, which opens a
/// real PAM transaction against a configured service (e.g. `/etc/pam.d/ocf`) and
/// runs its auth phase. That lets the fabric reuse the host's existing login
/// policy (Unix shadow, `pam_sss`, MFA modules, ...) without linking the PAM C
/// library directly, so the crate still builds on platforms without `libpam`.
///
/// `pamtester <service> <username> authenticate` is invoked and the password is
/// written to the child's **stdin** (pamtester reads it through PAM's
/// conversation function), never passed on the command line. A zero exit means
/// the credential was accepted; any non-zero exit is an authentication failure.
/// On success the user's groups are read back via `id -nG <username>`.
pub struct PamAuthenticator {
    /// The PAM service name whose stack is consulted (`pamtester`'s first arg).
    service: String,
}

impl PamAuthenticator {
    /// Create a PAM authenticator using the default `"ocf"` service.
    pub fn new() -> Self {
        Self::with_service("ocf")
    }

    /// Create a PAM authenticator bound to an explicit PAM service name.
    pub fn with_service(service: impl Into<String>) -> Self {
        PamAuthenticator {
            service: service.into(),
        }
    }

    /// The PAM service name this backend consults.
    pub fn service(&self) -> &str {
        &self.service
    }
}

impl Default for PamAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for PamAuthenticator {
    fn name(&self) -> &str {
        "pam"
    }
    fn description(&self) -> &str {
        "Host PAM stack authentication via the pamtester CLI"
    }
}

#[async_trait]
impl Authenticator for PamAuthenticator {
    async fn authenticate(&self, credentials: &Credentials) -> Result<Identity> {
        let (username, password) = match credentials {
            Credentials::Password { username, password } => (username, password),
            Credentials::Token(_) => {
                return Err(Error::unsupported(
                    "pam authenticator only accepts password credentials",
                ));
            }
        };

        tracing::info!(
            target: "ocf_auth::pam",
            service = %self.service,
            user = %username,
            "running pamtester authenticate phase",
        );

        // pamtester reads the password from its PAM conversation, which it wires
        // to stdin; the password is never placed on the command line.
        let (code, _stdout, stderr) = run_with_stdin(
            "pamtester",
            &[&self.service, username, "authenticate"],
            Some(password),
        )
        .await?;

        if code != 0 {
            return Err(Error::Unauthenticated(format!(
                "pam rejected `{username}` (service `{}`)",
                self.service
            )));
        }

        // Authentication succeeded; the stderr is pamtester's progress chatter
        // and intentionally ignored.
        let _ = stderr;

        let groups = resolve_groups(username).await;
        let mut identity = Identity::new(username);
        identity.groups = groups;
        Ok(identity)
    }
}

/// Best-effort group lookup for an authenticated user via `id -nG <username>`.
///
/// Group resolution is advisory: if `id` is missing or the user has no nss
/// entry we still return a valid (group-less) identity rather than failing an
/// otherwise-successful authentication.
async fn resolve_groups(username: &str) -> Vec<String> {
    match run_with_stdin("id", &["-nG", username], None).await {
        Ok((0, stdout, _)) => parse_id_groups(&stdout),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_ocf_service() {
        assert_eq!(PamAuthenticator::new().service(), "ocf");
        assert_eq!(PamAuthenticator::with_service("login").service(), "login");
    }

    #[tokio::test]
    async fn tokens_are_unsupported() {
        let auth = PamAuthenticator::new();
        let err = auth
            .authenticate(&Credentials::token("abc"))
            .await
            .expect_err("tokens not supported for pam");
        assert!(matches!(err, Error::NotSupported(_)));
    }

    // Requires a host with `pamtester` installed and a real PAM service; run
    // explicitly with `cargo test -- --ignored` on such a host.
    #[tokio::test]
    #[ignore = "needs pamtester and a configured PAM service on the host"]
    async fn real_pam_rejects_bad_password() {
        let auth = PamAuthenticator::with_service("login");
        let err = auth
            .authenticate(&Credentials::password("root", "definitely-not-the-password"))
            .await
            .expect_err("bad password must be rejected");
        assert!(matches!(err, Error::Unauthenticated(_)));
    }
}
