//! Resource identifiers.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// An opaque, stable identifier for a fabric resource.
///
/// Ids are either randomly generated (`Id::new`) or derived from a
/// human-meaningful name (`Id::named`). They serialize transparently as a
/// string so they read naturally in JSON payloads and URLs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(String);

impl Id {
    /// Generate a fresh random identifier.
    pub fn new() -> Self {
        Id(Uuid::new_v4().to_string())
    }

    /// Build an identifier from a stable, human-meaningful name.
    pub fn named(name: impl Into<String>) -> Self {
        Id(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl Default for Id {
    fn default() -> Self {
        Id::new()
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Id {
    fn from(s: String) -> Self {
        Id(s)
    }
}

impl From<&str> for Id {
    fn from(s: &str) -> Self {
        Id(s.to_string())
    }
}

impl AsRef<str> for Id {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
