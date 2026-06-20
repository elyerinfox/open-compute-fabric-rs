//! The [`Permission`] vocabulary: the verbs an authorization decision is made
//! against.
//!
//! A permission is just a stable string token (e.g. `"workload.create"`). Roles
//! hold a set of them; an [`AccessRequest`](crate::model::AccessRequest) carries
//! the single one being checked. The special wildcard [`Permission::ALL`] (`"*"`)
//! matches every permission and is what makes the seeded `Administrator` role
//! all-powerful.

use ocf_core::prelude::*;

/// A single authorization verb, e.g. `workload.create`.
///
/// Permissions are compared by their string value. The wildcard `"*"` is
/// special-cased by the engine to match any requested permission — see
/// [`Permission::is_wildcard`] and [`Permission::grants`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Permission(pub String);

impl Permission {
    /// The wildcard permission. A role holding this grants every permission.
    pub const WILDCARD: &'static str = "*";

    // --- Workloads / runtime -------------------------------------------------
    /// Create a workload (container or VM).
    pub const WORKLOAD_CREATE: &'static str = "workload.create";
    /// Read / list workloads.
    pub const WORKLOAD_READ: &'static str = "workload.read";
    /// Start, stop, migrate or otherwise mutate a workload.
    pub const WORKLOAD_MANAGE: &'static str = "workload.manage";
    /// Delete a workload.
    pub const WORKLOAD_DELETE: &'static str = "workload.delete";

    // --- Networking ----------------------------------------------------------
    /// Read VPC / subnet / route / ACL state.
    pub const VPC_READ: &'static str = "vpc.read";
    /// Manage the VPC overlay (create/update/delete VPCs, subnets, routes, ACLs).
    pub const VPC_MANAGE: &'static str = "vpc.manage";

    // --- Load balancing ------------------------------------------------------
    /// Read load-balancer state.
    pub const LB_READ: &'static str = "lb.read";
    /// Manage load balancers, listeners, certificates and DNS records.
    pub const LB_MANAGE: &'static str = "lb.manage";

    // --- Host / system -------------------------------------------------------
    /// Read host / kernel / inventory / disk state.
    pub const SYS_READ: &'static str = "sys.read";
    /// Modify host-level configuration (kernel, firewall, services, disks).
    pub const SYS_MODIFY: &'static str = "sys.modify";

    // --- Audit / observability ----------------------------------------------
    /// Read audit logs and monitoring data.
    pub const AUDIT_READ: &'static str = "audit.read";

    /// Build a permission from any string-like token.
    pub fn new(token: impl Into<String>) -> Self {
        Permission(token.into())
    }

    /// The wildcard permission (`"*"`), matching everything.
    pub fn wildcard() -> Self {
        Permission(Self::WILDCARD.to_string())
    }

    /// The raw token backing this permission.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True if this is the wildcard permission (`"*"`).
    pub fn is_wildcard(&self) -> bool {
        self.0 == Self::WILDCARD
    }

    /// True if holding `self` is sufficient to satisfy a request for `requested`.
    ///
    /// The wildcard grants everything; otherwise the tokens must match exactly.
    pub fn grants(&self, requested: &Permission) -> bool {
        self.is_wildcard() || self == requested
    }
}

impl From<&str> for Permission {
    fn from(s: &str) -> Self {
        Permission(s.to_string())
    }
}

impl From<String> for Permission {
    fn from(s: String) -> Self {
        Permission(s)
    }
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The read-only permission set used to seed the `Auditor` role.
pub fn read_only_permissions() -> Vec<Permission> {
    [
        Permission::WORKLOAD_READ,
        Permission::VPC_READ,
        Permission::LB_READ,
        Permission::SYS_READ,
        Permission::AUDIT_READ,
    ]
    .into_iter()
    .map(Permission::from)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_grants_everything() {
        let star = Permission::wildcard();
        assert!(star.is_wildcard());
        assert!(star.grants(&Permission::from(Permission::WORKLOAD_CREATE)));
        assert!(star.grants(&Permission::from("anything.at.all")));
    }

    #[test]
    fn exact_match_required_without_wildcard() {
        let read = Permission::from(Permission::WORKLOAD_READ);
        assert!(read.grants(&Permission::from(Permission::WORKLOAD_READ)));
        assert!(!read.grants(&Permission::from(Permission::WORKLOAD_CREATE)));
    }

    #[test]
    fn read_only_set_is_all_reads() {
        for p in read_only_permissions() {
            assert!(p.as_str().ends_with(".read"));
        }
    }
}
