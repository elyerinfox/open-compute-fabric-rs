//! Request authentication + authorization for the REST API.
//!
//! Key functionality is gated behind the fabric's RBAC. A middleware maps each
//! request to the [`Permission`] it requires (mutations and identity/admin
//! routes; ordinary reads stay open so the dashboard works), authenticates the
//! caller from the `Authorization` header (HTTP Basic or a Bearer token) via the
//! registered [`Authenticator`]s, and asks the [`RbacEngine`] whether that
//! principal may perform the action — `401` when unauthenticated, `403` when
//! denied.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header::AUTHORIZATION, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use ocf_auth::{Authenticator, Credentials};
use ocf_authz::{AccessRequest, Authorizer, Permission, RbacEngine};
use ocf_core::prelude::*;

use crate::controller::FabricController;
use crate::error::ApiError;

/// The permission a request requires, or `None` when the route is open
/// (liveness and ordinary reads). Mutations are gated by the resource they
/// touch; identity/admin routes are privileged for both read and write.
pub(crate) fn required_permission(method: &Method, path: &str) -> Option<&'static str> {
    let write = matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );

    // Liveness is always open.
    if path == "/api/v1/health" {
        return None;
    }
    // RBAC / identity management is privileged — read and write.
    if path.starts_with("/api/v1/access") {
        return Some(if write {
            Permission::SYS_MODIFY
        } else {
            Permission::SYS_READ
        });
    }
    // Admin operations (e.g. force-persist) are privileged.
    if path.starts_with("/api/v1/admin") {
        return Some(Permission::SYS_MODIFY);
    }
    // Other reads are open, so the dashboard works without credentials.
    if !write {
        return None;
    }
    // Writes, gated by the resource they touch.
    if path.starts_with("/api/v1/workloads") {
        return Some(Permission::WORKLOAD_MANAGE);
    }
    if path.starts_with("/api/v1/networks") {
        return Some(Permission::VPC_MANAGE);
    }
    if path.starts_with("/api/v1/loadbalancers") {
        return Some(Permission::LB_MANAGE);
    }
    // Everything else that mutates the host/fleet (platform updates, health
    // fixes, fabric machine failure injection, …) needs system-modify.
    Some(Permission::SYS_MODIFY)
}

/// Parse an `Authorization` header value into [`Credentials`]: `Basic` decodes
/// `user:pass`; `Bearer` is an opaque token. Unknown schemes → `None`.
pub(crate) fn parse_authorization(header: &str) -> Option<Credentials> {
    let (scheme, rest) = header.split_once(' ')?;
    match scheme.to_ascii_lowercase().as_str() {
        "basic" => {
            let decoded = base64_decode(rest.trim())?;
            let text = String::from_utf8(decoded).ok()?;
            let (user, pass) = text.split_once(':')?;
            Some(Credentials::password(user, pass))
        }
        "bearer" => Some(Credentials::token(rest.trim())),
        _ => None,
    }
}

/// Authenticate `creds` against the registered authenticators and authorize the
/// resulting principal for `perm` (fleet scope). `Unauthenticated` when no/invalid
/// credentials, `Forbidden` when the principal lacks the permission.
pub(crate) async fn check_access(
    authenticators: &Registry<dyn Authenticator>,
    rbac: &RbacEngine,
    creds: Option<Credentials>,
    perm: &str,
) -> Result<()> {
    let creds = creds.ok_or_else(|| Error::unauthenticated("authentication required"))?;
    let mut identity = None;
    for authenticator in authenticators.all() {
        if let Ok(id) = authenticator.authenticate(&creds).await {
            identity = Some(id);
            break;
        }
    }
    let identity = identity.ok_or_else(|| Error::unauthenticated("invalid credentials"))?;
    rbac.authorize(&AccessRequest::new(identity.username, perm, Scope::fleet()))
        .await
}

/// Axum middleware: gate the request on [`required_permission`].
pub(crate) async fn authz_middleware(
    State(controller): State<Arc<FabricController>>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(perm) = required_permission(request.method(), request.uri().path()) {
        let creds = request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(parse_authorization);
        if let Err(e) = check_access(&controller.authenticators, &controller.rbac, creds, perm).await
        {
            return ApiError(e).into_response();
        }
    }
    next.run(request).await
}

/// Minimal standard base64 decode (for the Basic credential blob); ignores `=`
/// padding and rejects non-alphabet characters.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let s = s.trim().trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        buf = (buf << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocf_auth::{Identity, LocalAuthenticator};
    use ocf_authz::{Group, RoleBinding, User};

    #[test]
    fn reads_are_open_writes_and_admin_are_gated() {
        let get = Method::GET;
        let post = Method::POST;
        let del = Method::DELETE;
        // Liveness + ordinary reads: open.
        assert_eq!(required_permission(&get, "/api/v1/health"), None);
        assert_eq!(required_permission(&get, "/api/v1/machines"), None);
        assert_eq!(required_permission(&get, "/api/v1/platform/updates"), None);
        // Mutations: gated by resource.
        assert_eq!(
            required_permission(&post, "/api/v1/workloads/w1/migrate"),
            Some(Permission::WORKLOAD_MANAGE)
        );
        assert_eq!(
            required_permission(&del, "/api/v1/workloads/w1/network"),
            Some(Permission::WORKLOAD_MANAGE)
        );
        assert_eq!(
            required_permission(&post, "/api/v1/networks/subnets/s/egress"),
            Some(Permission::VPC_MANAGE)
        );
        assert_eq!(
            required_permission(&post, "/api/v1/platform/updates/apply"),
            Some(Permission::SYS_MODIFY)
        );
        assert_eq!(
            required_permission(&post, "/api/v1/health/fix"),
            Some(Permission::SYS_MODIFY)
        );
        assert_eq!(
            required_permission(&post, "/api/v1/fabric/machines/m/fail"),
            Some(Permission::SYS_MODIFY)
        );
        // Identity/admin: privileged even for reads.
        assert_eq!(
            required_permission(&get, "/api/v1/access/users"),
            Some(Permission::SYS_READ)
        );
        assert_eq!(
            required_permission(&post, "/api/v1/admin/persist"),
            Some(Permission::SYS_MODIFY)
        );
    }

    #[test]
    fn parses_basic_and_bearer() {
        // base64("admin:s3cret") = YWRtaW46czNjcmV0
        let creds = parse_authorization("Basic YWRtaW46czNjcmV0").unwrap();
        assert_eq!(creds, Credentials::password("admin", "s3cret"));
        let token = parse_authorization("Bearer abc.def").unwrap();
        assert_eq!(token, Credentials::token("abc.def"));
        assert!(parse_authorization("Weird xyz").is_none());
        assert!(parse_authorization("Basic !!!notbase64").is_none());
    }

    /// An authenticator that accepts anything as a fixed identity (test helper).
    struct AnyUser(&'static str);
    impl Provider for AnyUser {
        fn name(&self) -> &str {
            "test-any"
        }
    }
    #[async_trait]
    impl Authenticator for AnyUser {
        async fn authenticate(&self, _c: &Credentials) -> Result<Identity> {
            Ok(Identity::new(self.0))
        }
    }

    fn admin_rbac() -> RbacEngine {
        let rbac = RbacEngine::with_defaults();
        rbac.put_user(User::new("admin").with_group("admins"));
        rbac.put_group(Group::new("admins").with_member("admin"));
        rbac.add_binding(RoleBinding::new(
            ocf_authz::Subject::group("admins"),
            ocf_authz::ADMINISTRATOR_ROLE,
            Scope::fleet(),
        ));
        rbac
    }

    #[tokio::test]
    async fn no_credentials_is_unauthenticated() {
        let mut authn = Registry::<dyn Authenticator>::new();
        authn
            .register("local", Arc::new(LocalAuthenticator::with_admin("admin", "admin")))
            .unwrap();
        let err = check_access(&authn, &admin_rbac(), None, Permission::SYS_MODIFY)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "unauthenticated");
    }

    #[tokio::test]
    async fn admin_is_authorized_but_others_are_forbidden() {
        let mut authn = Registry::<dyn Authenticator>::new();
        authn
            .register("local", Arc::new(LocalAuthenticator::with_admin("admin", "admin")))
            .unwrap();
        let rbac = admin_rbac();
        // The admin passes a privileged write.
        check_access(
            &authn,
            &rbac,
            Some(Credentials::password("admin", "admin")),
            Permission::SYS_MODIFY,
        )
        .await
        .expect("admin authorized");

        // A principal that authenticates but isn't bound to a role is forbidden.
        let mut nobody = Registry::<dyn Authenticator>::new();
        nobody.register("any", Arc::new(AnyUser("nobody"))).unwrap();
        let err = check_access(
            &nobody,
            &rbac,
            Some(Credentials::token("whatever")),
            Permission::SYS_MODIFY,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "forbidden");
    }
}
