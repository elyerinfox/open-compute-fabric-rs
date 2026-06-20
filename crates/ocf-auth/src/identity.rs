//! The authentication vocabulary: who a principal is, and what they present.

use ocf_core::prelude::*;
use std::collections::BTreeMap;

/// A successfully authenticated principal.
///
/// An `Identity` is the *output* of authentication: it is what every backend
/// (local, PAM, Active Directory) maps its native account onto, so the rest of
/// the fabric — notably `ocf-authz` and [`crate::hostsync`] — speaks a single
/// vocabulary regardless of which directory the user actually came from.
///
/// `groups` carry directory-level group membership and feed straight into RBAC
/// group resolution; `attributes` hold any extra backend-specific claims (e.g.
/// an AD `distinguishedName`, a PAM `gecos` field) that callers may want but the
/// core model does not standardize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

impl Identity {
    /// Build a bare identity for `username`, with no groups or attributes.
    pub fn new(username: impl Into<String>) -> Self {
        Identity {
            username: username.into(),
            display_name: String::new(),
            email: String::new(),
            groups: Vec::new(),
            attributes: BTreeMap::new(),
        }
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = display_name.into();
        self
    }

    pub fn with_email(mut self, email: impl Into<String>) -> Self {
        self.email = email.into();
        self
    }

    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.groups.push(group.into());
        self
    }

    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }
}

/// What a principal presents to prove who they are.
///
/// The fabric supports interactive password login and bearer-token login; a
/// concrete [`crate::authenticator::Authenticator`] decides which variants it
/// understands and rejects the rest with [`Error::unsupported`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Credentials {
    /// A username/password pair (interactive or API basic-auth login).
    Password { username: String, password: String },
    /// An opaque bearer token (session token, API key, ...).
    Token(String),
}

impl Credentials {
    /// Construct password credentials.
    pub fn password(username: impl Into<String>, password: impl Into<String>) -> Self {
        Credentials::Password {
            username: username.into(),
            password: password.into(),
        }
    }

    /// Construct token credentials.
    pub fn token(token: impl Into<String>) -> Self {
        Credentials::Token(token.into())
    }

    /// The principal name implied by these credentials, when one is carried
    /// in-band. Token credentials are opaque and therefore yield `None`.
    pub fn username(&self) -> Option<&str> {
        match self {
            Credentials::Password { username, .. } => Some(username),
            Credentials::Token(_) => None,
        }
    }
}
