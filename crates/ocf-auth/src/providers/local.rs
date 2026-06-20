//! The built-in in-memory authenticator with a real password check.

use crate::authenticator::Authenticator;
use crate::identity::{Credentials, Identity};
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;

/// A stored account: the secret to compare against plus the identity to mint.
#[derive(Debug, Clone)]
struct Account {
    password: String,
    identity: Identity,
}

/// An authenticator backed by an in-memory username → account map.
///
/// Unlike the directory-integrated backends this one performs a *real* check:
/// it looks the user up and compares the presented password. It is the default
/// `"local"` directory and is what the controller falls back to before any
/// enterprise directory is configured. Accounts live only in memory, so this is
/// intended for bootstrap/break-glass and tests rather than a user database of
/// record.
///
/// The secret is verified with a **constant-time** comparison so the check does
/// not leak password length/content through timing. This is the bootstrap /
/// break-glass directory; the directories of record are the real PAM and Active
/// Directory backends.
pub struct LocalAuthenticator {
    accounts: RwLock<HashMap<String, Account>>,
}

/// Constant-time byte-slice equality. Compares every byte regardless of where a
/// mismatch occurs, so verification time does not depend on the secret.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

impl LocalAuthenticator {
    /// Create an empty local authenticator.
    pub fn new() -> Self {
        LocalAuthenticator {
            accounts: RwLock::new(HashMap::new()),
        }
    }

    /// Create a local authenticator seeded with a single administrator account,
    /// convenient for first-boot / break-glass access.
    pub fn with_admin(username: impl Into<String>, password: impl Into<String>) -> Self {
        let auth = Self::new();
        let username = username.into();
        let identity = Identity::new(username.clone())
            .with_display_name("Administrator")
            .with_group("administrators");
        auth.set_account(username, password, identity);
        auth
    }

    /// Insert or overwrite the account `username`, deriving a default identity
    /// (the identity simply carries the username, no groups).
    pub fn add_user(&self, username: impl Into<String>, password: impl Into<String>) {
        let username = username.into();
        let identity = Identity::new(username.clone());
        self.set_account(username, password, identity);
    }

    /// Insert or overwrite the account `username` with an explicit identity,
    /// e.g. to attach groups consumed by RBAC.
    pub fn set_account(
        &self,
        username: impl Into<String>,
        password: impl Into<String>,
        identity: Identity,
    ) {
        let username = username.into();
        self.accounts.write().insert(
            username,
            Account {
                password: password.into(),
                identity,
            },
        );
    }

    /// Remove an account. Returns `true` if one was present.
    pub fn remove_user(&self, username: &str) -> bool {
        self.accounts.write().remove(username).is_some()
    }

    /// Number of stored accounts.
    pub fn len(&self) -> usize {
        self.accounts.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.read().is_empty()
    }
}

impl Default for LocalAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for LocalAuthenticator {
    fn name(&self) -> &str {
        "local"
    }
    fn description(&self) -> &str {
        "In-memory username/password directory with a real password check"
    }
}

#[async_trait]
impl Authenticator for LocalAuthenticator {
    async fn authenticate(&self, credentials: &Credentials) -> Result<Identity> {
        let (username, password) = match credentials {
            Credentials::Password { username, password } => (username, password),
            Credentials::Token(_) => {
                return Err(Error::unsupported(
                    "local authenticator only accepts password credentials",
                ));
            }
        };

        let accounts = self.accounts.read();
        let account = accounts
            .get(username)
            .ok_or_else(|| Error::Unauthenticated(format!("unknown user `{username}`")))?;

        if !constant_time_eq(account.password.as_bytes(), password.as_bytes()) {
            return Err(Error::Unauthenticated(format!(
                "invalid password for `{username}`"
            )));
        }

        Ok(account.identity.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn correct_password_authenticates() {
        let auth = LocalAuthenticator::new();
        auth.set_account(
            "alice",
            "s3cret",
            Identity::new("alice").with_group("dev"),
        );

        let id = auth
            .authenticate(&Credentials::password("alice", "s3cret"))
            .await
            .expect("should authenticate");
        assert_eq!(id.username, "alice");
        assert_eq!(id.groups, vec!["dev".to_string()]);
    }

    #[tokio::test]
    async fn wrong_password_is_rejected() {
        let auth = LocalAuthenticator::new();
        auth.add_user("bob", "hunter2");

        let err = auth
            .authenticate(&Credentials::password("bob", "nope"))
            .await
            .expect_err("should reject");
        assert!(matches!(err, Error::Unauthenticated(_)));
    }

    #[tokio::test]
    async fn unknown_user_is_rejected() {
        let auth = LocalAuthenticator::new();
        let err = auth
            .authenticate(&Credentials::password("ghost", "x"))
            .await
            .expect_err("should reject");
        assert!(matches!(err, Error::Unauthenticated(_)));
    }

    #[tokio::test]
    async fn tokens_are_unsupported() {
        let auth = LocalAuthenticator::with_admin("root", "toor");
        let err = auth
            .authenticate(&Credentials::token("abc"))
            .await
            .expect_err("tokens not supported locally");
        assert!(matches!(err, Error::NotSupported(_)));
    }
}
