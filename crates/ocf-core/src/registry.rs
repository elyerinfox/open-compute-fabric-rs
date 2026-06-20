//! The generic plugin system.
//!
//! Pluggability in the fabric is realized with two pieces:
//!
//! * A [`Provider`] supertrait that every pluggable contract extends. It only
//!   requires a unique `name` and a description.
//! * A generic [`Registry`] that stores named providers of *any* trait object
//!   `dyn T`. Subsystems instantiate `Registry<dyn RuntimeProvider>`,
//!   `Registry<dyn Authenticator>`, etc. — one registry per contract.
//!
//! Concrete implementations live in their own crates and are registered at
//! startup, so the controller never depends on a specific backend.

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// The minimal contract every pluggable provider satisfies.
pub trait Provider: Send + Sync {
    /// Globally unique identifier within its registry (e.g. `"docker"`).
    fn name(&self) -> &str;

    /// Human-facing description shown in the UI / `--list-providers`.
    fn description(&self) -> &str {
        ""
    }
}

/// A thread-safe registry of named providers of trait object type `T`.
///
/// `T` is typically an unsized trait object such as `dyn RuntimeProvider`.
/// Providers are stored behind `Arc` so they can be shared across async tasks.
pub struct Registry<T: ?Sized> {
    providers: HashMap<String, Arc<T>>,
}

impl<T: ?Sized> Registry<T> {
    pub fn new() -> Self {
        Registry {
            providers: HashMap::new(),
        }
    }

    /// Register `provider` under `name`. Fails if the name is already taken.
    pub fn register(&mut self, name: impl Into<String>, provider: Arc<T>) -> Result<()> {
        let name = name.into();
        if self.providers.contains_key(&name) {
            return Err(Error::AlreadyExists(format!("provider `{name}`")));
        }
        self.providers.insert(name, provider);
        Ok(())
    }

    /// Register, replacing any existing provider with the same name.
    pub fn register_or_replace(&mut self, name: impl Into<String>, provider: Arc<T>) {
        self.providers.insert(name.into(), provider);
    }

    /// Look up a provider by name.
    pub fn get(&self, name: &str) -> Result<Arc<T>> {
        self.providers
            .get(name)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("provider `{name}`")))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.providers.contains_key(name)
    }

    /// All registered provider names, unordered.
    pub fn names(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// All registered providers.
    pub fn all(&self) -> Vec<Arc<T>> {
        self.providers.values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl<T: ?Sized> Default for Registry<T> {
    fn default() -> Self {
        Registry::new()
    }
}
