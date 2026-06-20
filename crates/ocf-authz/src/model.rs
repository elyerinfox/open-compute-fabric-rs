//! The RBAC resource model: [`Role`], [`Group`], [`User`], and the
//! [`RoleBinding`] / [`Subject`] / [`AccessRequest`] glue that connects them.
//!
//! The model is deliberately small:
//!
//! * A [`User`] belongs to zero or more [`Group`]s (named by username/group
//!   name, so identities can be managed independently of this store).
//! * A [`Role`] is a named bundle of [`Permission`]s.
//! * A [`RoleBinding`] grants a [`Role`] to a [`Subject`] (a user or a group)
//!   at a [`Scope`]. The binding applies to that scope and everything beneath
//!   it (see [`Scope::contains`]).
//! * An [`AccessRequest`] is the question the [`Authorizer`](crate::engine::Authorizer)
//!   answers: "may `username` do `permission` at `scope`?".

use crate::permission::Permission;
use ocf_core::prelude::*;
use std::collections::BTreeSet;

/// A named bundle of permissions.
///
/// Roles are referenced from a [`RoleBinding`] by name (their metadata name),
/// which is how the seeded `Administrator` / `Auditor` roles are wired up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    pub metadata: Metadata,
    pub permissions: BTreeSet<Permission>,
}

impl Role {
    /// A role with no permissions.
    pub fn new(name: impl Into<String>) -> Self {
        Role {
            metadata: Metadata::named(name),
            permissions: BTreeSet::new(),
        }
    }

    /// A role seeded with the given permissions.
    pub fn with_permissions<I, P>(name: impl Into<String>, permissions: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<Permission>,
    {
        Role {
            metadata: Metadata::named(name),
            permissions: permissions.into_iter().map(Into::into).collect(),
        }
    }

    /// Builder-style: add a permission and return the role.
    pub fn grant(mut self, permission: impl Into<Permission>) -> Self {
        self.permissions.insert(permission.into());
        self
    }

    /// True if this role holds `requested` directly or via the wildcard.
    pub fn holds(&self, requested: &Permission) -> bool {
        self.permissions.iter().any(|held| held.grants(requested))
    }
}

impl Resource for Role {
    fn kind(&self) -> &'static str {
        "role"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// A named collection of users, referenced by username.
///
/// Membership is stored as usernames (not [`Id`]s) so groups can reference
/// identities that live in an external directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub metadata: Metadata,
    /// Usernames of the members of this group.
    pub members: Vec<String>,
}

impl Group {
    pub fn new(name: impl Into<String>) -> Self {
        Group {
            metadata: Metadata::named(name),
            members: Vec::new(),
        }
    }

    /// Builder-style: add a member username and return the group.
    pub fn with_member(mut self, username: impl Into<String>) -> Self {
        self.members.push(username.into());
        self
    }

    /// True if `username` is a member of this group.
    pub fn contains_member(&self, username: &str) -> bool {
        self.members.iter().any(|m| m == username)
    }
}

impl Resource for Group {
    fn kind(&self) -> &'static str {
        "group"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// A principal that can be granted roles.
///
/// `groups` lists the names of the groups this user is statically a member of.
/// The engine additionally resolves dynamic membership from each [`Group`]'s
/// own `members` list, so either side may declare the relationship.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub metadata: Metadata,
    pub username: String,
    /// Names of groups this user belongs to.
    pub groups: Vec<String>,
}

impl User {
    pub fn new(username: impl Into<String>) -> Self {
        let username = username.into();
        User {
            metadata: Metadata::named(username.clone()),
            username,
            groups: Vec::new(),
        }
    }

    /// Builder-style: add a group name and return the user.
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.groups.push(group.into());
        self
    }
}

impl Resource for User {
    fn kind(&self) -> &'static str {
        "user"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// The principal a [`RoleBinding`] grants a role to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    /// A single user, identified by username.
    User(String),
    /// Every member of a group, identified by group name.
    Group(String),
}

impl Subject {
    /// A user subject.
    pub fn user(username: impl Into<String>) -> Self {
        Subject::User(username.into())
    }

    /// A group subject.
    pub fn group(group: impl Into<String>) -> Self {
        Subject::Group(group.into())
    }
}

/// Grants a [`Role`] to a [`Subject`] at a [`Scope`].
///
/// The binding applies at `scope` and everything beneath it: a binding scoped
/// to a region authorizes actions on every datacenter, rack and machine in that
/// region (decided by [`Scope::contains`]). `role` is the *name* of the role
/// (its metadata name), matching how roles are stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleBinding {
    pub id: Id,
    pub subject: Subject,
    /// Name of the bound role.
    pub role: String,
    pub scope: Scope,
}

impl RoleBinding {
    /// Create a binding with a fresh id.
    pub fn new(subject: Subject, role: impl Into<String>, scope: Scope) -> Self {
        RoleBinding {
            id: Id::new(),
            subject,
            role: role.into(),
            scope,
        }
    }

    /// True if this binding's subject covers `username`, given the set of group
    /// names the user belongs to.
    pub fn applies_to(&self, username: &str, user_groups: &BTreeSet<String>) -> bool {
        match &self.subject {
            Subject::User(u) => u == username,
            Subject::Group(g) => user_groups.contains(g),
        }
    }
}

/// The question put to an [`Authorizer`](crate::engine::Authorizer): may
/// `username` perform `permission` at `scope`?
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessRequest {
    pub username: String,
    pub permission: Permission,
    pub scope: Scope,
}

impl AccessRequest {
    pub fn new(
        username: impl Into<String>,
        permission: impl Into<Permission>,
        scope: Scope,
    ) -> Self {
        AccessRequest {
            username: username.into(),
            permission: permission.into(),
            scope,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_holds_via_wildcard() {
        let admin = Role::new("admin").grant(Permission::wildcard());
        assert!(admin.holds(&Permission::from(Permission::SYS_MODIFY)));
    }

    #[test]
    fn role_holds_exact_only() {
        let r = Role::with_permissions("r", [Permission::WORKLOAD_READ]);
        assert!(r.holds(&Permission::from(Permission::WORKLOAD_READ)));
        assert!(!r.holds(&Permission::from(Permission::WORKLOAD_CREATE)));
    }

    #[test]
    fn binding_applies_to_user_and_group() {
        let mut groups = BTreeSet::new();
        groups.insert("ops".to_string());

        let direct = RoleBinding::new(Subject::user("alice"), "admin", Scope::fleet());
        assert!(direct.applies_to("alice", &BTreeSet::new()));
        assert!(!direct.applies_to("bob", &BTreeSet::new()));

        let via_group = RoleBinding::new(Subject::group("ops"), "admin", Scope::fleet());
        assert!(via_group.applies_to("alice", &groups));
        assert!(!via_group.applies_to("alice", &BTreeSet::new()));
    }
}
