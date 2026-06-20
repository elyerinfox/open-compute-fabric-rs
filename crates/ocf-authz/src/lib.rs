//! # ocf-authz
//!
//! Role-based access control for the fabric.
//!
//! Authorization is a small, scope-aware RBAC model:
//!
//! * [`Permission`]s ([`permission`]) are the verbs a decision is made against,
//!   with a wildcard (`"*"`) that grants everything.
//! * [`Role`]s, [`Group`]s and [`User`]s ([`model`]) are the resources, tied
//!   together by [`RoleBinding`]s that grant a role to a [`Subject`] at a
//!   [`Scope`](ocf_core::scope::Scope).
//! * The [`Authorizer`] contract ([`engine`]) answers a single
//!   [`AccessRequest`], and [`RbacEngine`] is the in-memory implementation.
//!
//! A grant at a scope covers everything beneath it: the engine keeps only the
//! bindings whose scope `contains` the requested scope, then allows the request
//! if any bound role holds the permission (or the wildcard).
//!
//! [`RbacEngine::with_defaults`] seeds the conventional `Administrator`
//! (wildcard) and `Auditor` (read-only) roles, mirroring the `register_builtins`
//! pattern used by the pluggable subsystems.

pub mod engine;
pub mod model;
pub mod permission;

pub use engine::{Authorizer, RbacEngine, ADMINISTRATOR_ROLE, AUDITOR_ROLE};
pub use model::{AccessRequest, Group, Role, RoleBinding, Subject, User};
pub use permission::{read_only_permissions, Permission};
