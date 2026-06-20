//! The authorization contract ([`Authorizer`]) and its in-memory RBAC
//! implementation ([`RbacEngine`]).
//!
//! The engine answers a single question — "may this user do this thing here?" —
//! by walking the model: resolve the user's effective groups, gather every
//! [`RoleBinding`] whose subject covers the user, keep those whose scope
//! *contains* the requested scope, and allow if any bound role holds the
//! requested permission (or the wildcard).

use crate::model::{AccessRequest, Group, Role, RoleBinding, User};
use crate::permission::{read_only_permissions, Permission};
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::{BTreeSet, HashMap};

/// The name of the seeded all-powerful role.
pub const ADMINISTRATOR_ROLE: &str = "Administrator";
/// The name of the seeded read-only role.
pub const AUDITOR_ROLE: &str = "Auditor";

/// The authorization contract: decide whether an [`AccessRequest`] is allowed.
///
/// It is async and object-safe so the API layer can hold an
/// `Arc<dyn Authorizer>` and swap the in-memory [`RbacEngine`] for an external
/// policy backend without touching callers.
#[async_trait]
pub trait Authorizer: Send + Sync {
    /// Return `Ok(true)` if the request is permitted, `Ok(false)` otherwise.
    async fn is_allowed(&self, request: &AccessRequest) -> Result<bool>;

    /// Convenience: turn a denied request into a [`Error::Forbidden`].
    async fn authorize(&self, request: &AccessRequest) -> Result<()> {
        if self.is_allowed(request).await? {
            Ok(())
        } else {
            Err(Error::forbidden(format!(
                "{} may not {} at {:?}",
                request.username, request.permission, request.scope
            )))
        }
    }
}

/// In-memory RBAC engine: thread-safe stores of roles, groups, users and
/// bindings plus the evaluation logic.
///
/// Stores are keyed for stable lookup: roles and groups by their (unique) name,
/// users by username, bindings by their [`Id`]. All access goes through a
/// [`RwLock`], so the engine is `Send + Sync` and cheap to share behind an
/// `Arc`.
#[derive(Default)]
pub struct RbacEngine {
    roles: RwLock<HashMap<String, Role>>,
    groups: RwLock<HashMap<String, Group>>,
    users: RwLock<HashMap<String, User>>,
    bindings: RwLock<HashMap<Id, RoleBinding>>,
}

impl RbacEngine {
    /// An empty engine with no roles, groups, users or bindings.
    pub fn new() -> Self {
        Self::default()
    }

    /// An engine seeded with the built-in `Administrator` (wildcard) and
    /// `Auditor` (read-only) roles.
    ///
    /// This is the RBAC analogue of the `register_builtins` helpers other
    /// subsystems expose: it gives a fresh controller a sane starting policy.
    pub fn with_defaults() -> Self {
        let engine = Self::new();
        // new() yields an empty store, so these inserts cannot collide.
        engine.put_role(Role::with_permissions(
            ADMINISTRATOR_ROLE,
            [Permission::wildcard()],
        ));
        engine.put_role(Role::with_permissions(
            AUDITOR_ROLE,
            read_only_permissions(),
        ));
        tracing::info!(
            "seeded default RBAC roles: {ADMINISTRATOR_ROLE} (wildcard), {AUDITOR_ROLE} (read-only)"
        );
        engine
    }

    // --- Role store ----------------------------------------------------------

    /// Insert or replace a role, keyed by its name.
    pub fn put_role(&self, role: Role) {
        self.roles.write().insert(role.name().to_string(), role);
    }

    /// Fetch a role by name.
    pub fn get_role(&self, name: &str) -> Result<Role> {
        self.roles
            .read()
            .get(name)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("role {name}")))
    }

    /// All roles.
    pub fn list_roles(&self) -> Vec<Role> {
        self.roles.read().values().cloned().collect()
    }

    // --- Group store ---------------------------------------------------------

    /// Insert or replace a group, keyed by its name.
    pub fn put_group(&self, group: Group) {
        self.groups.write().insert(group.name().to_string(), group);
    }

    /// Fetch a group by name.
    pub fn get_group(&self, name: &str) -> Result<Group> {
        self.groups
            .read()
            .get(name)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("group {name}")))
    }

    /// All groups.
    pub fn list_groups(&self) -> Vec<Group> {
        self.groups.read().values().cloned().collect()
    }

    // --- User store ----------------------------------------------------------

    /// Insert or replace a user, keyed by username.
    pub fn put_user(&self, user: User) {
        self.users.write().insert(user.username.clone(), user);
    }

    /// Fetch a user by username.
    pub fn get_user(&self, username: &str) -> Result<User> {
        self.users
            .read()
            .get(username)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("user {username}")))
    }

    /// All users.
    pub fn list_users(&self) -> Vec<User> {
        self.users.read().values().cloned().collect()
    }

    // --- Binding store -------------------------------------------------------

    /// Add a role binding, returning its id.
    pub fn add_binding(&self, binding: RoleBinding) -> Id {
        let id = binding.id.clone();
        self.bindings.write().insert(id.clone(), binding);
        id
    }

    /// Remove a binding by id. Returns `NotFound` if it was not present.
    pub fn remove_binding(&self, id: &Id) -> Result<()> {
        self.bindings
            .write()
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| Error::not_found(format!("binding {id}")))
    }

    /// All bindings.
    pub fn list_bindings(&self) -> Vec<RoleBinding> {
        self.bindings.read().values().cloned().collect()
    }

    // --- Evaluation ----------------------------------------------------------

    /// Resolve the full set of group names a user belongs to.
    ///
    /// Membership is the union of two declarations: the groups listed on the
    /// [`User`] record and the groups whose `members` list names the user. This
    /// keeps the relationship authoritative from either side.
    fn effective_groups(&self, username: &str) -> BTreeSet<String> {
        let mut groups = BTreeSet::new();
        if let Some(user) = self.users.read().get(username) {
            groups.extend(user.groups.iter().cloned());
        }
        for group in self.groups.read().values() {
            if group.contains_member(username) {
                groups.insert(group.name().to_string());
            }
        }
        groups
    }
}

#[async_trait]
impl Authorizer for RbacEngine {
    async fn is_allowed(&self, request: &AccessRequest) -> Result<bool> {
        let user_groups = self.effective_groups(&request.username);

        // Snapshot the bindings and roles we need under their locks, then
        // evaluate without holding any lock across an await point.
        let candidate_roles: Vec<String> = {
            let bindings = self.bindings.read();
            bindings
                .values()
                .filter(|b| b.applies_to(&request.username, &user_groups))
                // A binding only counts if its scope is an ancestor of (or
                // equal to) the requested scope.
                .filter(|b| b.scope.contains(&request.scope))
                .map(|b| b.role.clone())
                .collect()
        };

        let allowed = {
            let roles = self.roles.read();
            candidate_roles.iter().any(|role_name| {
                roles
                    .get(role_name)
                    .map(|role| role.holds(&request.permission))
                    .unwrap_or(false)
            })
        };

        if allowed {
            tracing::debug!(
                user = %request.username,
                permission = %request.permission,
                "access granted"
            );
        } else {
            tracing::debug!(
                user = %request.username,
                permission = %request.permission,
                "access denied"
            );
        }
        Ok(allowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AccessRequest, Subject};

    fn req(user: &str, perm: &str, scope: Scope) -> AccessRequest {
        AccessRequest::new(user, Permission::from(perm), scope)
    }

    #[tokio::test]
    async fn administrator_can_do_anything_fleet_wide() {
        let engine = RbacEngine::with_defaults();
        engine.put_user(User::new("root"));
        engine.add_binding(RoleBinding::new(
            Subject::user("root"),
            ADMINISTRATOR_ROLE,
            Scope::fleet(),
        ));

        assert!(engine
            .is_allowed(&req("root", Permission::SYS_MODIFY, Scope::datacenter("us", "dc1")))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn auditor_is_read_only() {
        let engine = RbacEngine::with_defaults();
        engine.put_user(User::new("watcher"));
        engine.add_binding(RoleBinding::new(
            Subject::user("watcher"),
            AUDITOR_ROLE,
            Scope::fleet(),
        ));

        assert!(engine
            .is_allowed(&req("watcher", Permission::AUDIT_READ, Scope::fleet()))
            .await
            .unwrap());
        assert!(!engine
            .is_allowed(&req("watcher", Permission::WORKLOAD_CREATE, Scope::fleet()))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn binding_scope_bounds_authority() {
        let engine = RbacEngine::with_defaults();
        engine.put_user(User::new("regional"));
        engine.add_binding(RoleBinding::new(
            Subject::user("regional"),
            ADMINISTRATOR_ROLE,
            Scope::region("us"),
        ));

        // Within the granted region: allowed.
        assert!(engine
            .is_allowed(&req(
                "regional",
                Permission::SYS_MODIFY,
                Scope::datacenter("us", "dc1")
            ))
            .await
            .unwrap());
        // A different region: denied.
        assert!(!engine
            .is_allowed(&req("regional", Permission::SYS_MODIFY, Scope::region("eu")))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn group_membership_grants_access_from_either_side() {
        let engine = RbacEngine::with_defaults();

        // User declares its own group membership.
        engine.put_user(User::new("alice").with_group("ops"));
        // Group declares bob as a member.
        engine.put_user(User::new("bob"));
        engine.put_group(Group::new("ops").with_member("bob"));

        engine.add_binding(RoleBinding::new(
            Subject::group("ops"),
            AUDITOR_ROLE,
            Scope::fleet(),
        ));

        for user in ["alice", "bob"] {
            assert!(engine
                .is_allowed(&req(user, Permission::AUDIT_READ, Scope::fleet()))
                .await
                .unwrap());
        }
    }

    #[tokio::test]
    async fn unknown_user_with_no_bindings_is_denied() {
        let engine = RbacEngine::with_defaults();
        assert!(!engine
            .is_allowed(&req("nobody", Permission::WORKLOAD_READ, Scope::fleet()))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn authorize_maps_denial_to_forbidden() {
        let engine = RbacEngine::new();
        let err = engine
            .authorize(&req("nobody", Permission::SYS_MODIFY, Scope::fleet()))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "forbidden");
    }
}
