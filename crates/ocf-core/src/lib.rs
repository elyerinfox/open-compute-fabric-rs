//! # ocf-core
//!
//! Foundational contracts and domain types for **Open Compute Fabric**.
//!
//! Everything in the fabric is built on three ideas that live here:
//!
//! 1. **Resources** ([`resource::Resource`]) — the abstract base every managed
//!    object implements, carrying [`metadata::Metadata`].
//! 2. **Providers + Registry** ([`registry`]) — the generic plugin system.
//!    Each subsystem declares a provider contract (a trait) and registers
//!    swappable concrete backends in a [`registry::Registry`].
//! 3. **Scope** ([`scope::Scope`]) — the hierarchical
//!    `fleet → region → datacenter → rack → machine` coordinate reused by both
//!    authorization and placement.
//!
//! Subsystem crates depend only on `ocf-core`, never on each other's
//! implementations, which is what keeps the fabric pluggable.

pub mod error;
pub mod health;
pub mod id;
pub mod metadata;
pub mod quantity;
pub mod registry;
pub mod resource;
pub mod scope;

// Re-export the async-trait attribute so subsystem crates can write
// `#[ocf_core::async_trait]` without taking their own dependency on it.
pub use async_trait::async_trait;

/// Glob-import surface for subsystem crates: `use ocf_core::prelude::*;`.
pub mod prelude {
    pub use crate::async_trait;
    pub use crate::error::{Error, Result};
    pub use crate::health::{Health, LifecycleState};
    pub use crate::id::Id;
    pub use crate::metadata::Metadata;
    pub use crate::quantity::ResourceSpec;
    pub use crate::registry::{Provider, Registry};
    pub use crate::resource::Resource;
    pub use crate::scope::{Scope, ScopeLevel};
    pub use serde::{Deserialize, Serialize};
}
