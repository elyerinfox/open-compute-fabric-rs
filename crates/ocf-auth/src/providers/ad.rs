//! Active Directory / LDAP authentication, driven through the OpenLDAP CLIs.

use crate::authenticator::Authenticator;
use crate::exec::run_with_stdin;
use crate::identity::{Credentials, Identity};
use ocf_core::prelude::*;

/// Authenticates against an Active Directory domain over LDAP(S).
///
/// This performs a real LDAP *bind* using the OpenLDAP `ldapwhoami` client: a
/// successful simple bind with the user's `username@domain` UPN and password
/// proves the credential. On success the user's `memberOf` groups are read back
/// with `ldapsearch` (best effort) to populate the resolved [`Identity`]; that
/// group membership flows directly into the RBAC engine in `ocf-authz`.
///
/// Shelling out to the CLI (rather than linking an LDAP C library) keeps the
/// crate buildable on platforms without an LDAP client — the binaries simply
/// aren't present off-host, which surfaces as a provider error.
///
/// Note on the password: `ldapwhoami` accepts the bind password via `-w`, so it
/// is passed as an argument here as the tooling requires. A hardening step is to
/// switch to `-y <file>` reading from a private fd; the simple-bind shape is
/// unchanged.
pub struct ActiveDirectoryAuthenticator {
    /// The AD domain, e.g. `"EXAMPLE.COM"`, used to build the bind UPN.
    domain: String,
    /// Domain-controller LDAP URLs (`ldap://` / `ldaps://`) tried in order.
    servers: Vec<String>,
}

impl ActiveDirectoryAuthenticator {
    /// Create an AD authenticator for `domain` with no explicit servers. With no
    /// server configured the bind targets the default `ldap://<domain>` URI
    /// (lower-cased), which lets DNS resolve a domain controller.
    pub fn new(domain: impl Into<String>) -> Self {
        ActiveDirectoryAuthenticator {
            domain: domain.into(),
            servers: Vec::new(),
        }
    }

    /// Create an AD authenticator for `domain` with an explicit server list.
    pub fn with_servers(domain: impl Into<String>, servers: Vec<String>) -> Self {
        ActiveDirectoryAuthenticator {
            domain: domain.into(),
            servers,
        }
    }

    /// The configured domain.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// The configured domain-controller URLs.
    pub fn servers(&self) -> &[String] {
        &self.servers
    }

    /// The LDAP URI to bind against: the first configured server, or a default
    /// derived from the domain (`ldap://<lower-cased-domain>`).
    fn ldap_uri(&self) -> String {
        match self.servers.first() {
            Some(uri) => uri.clone(),
            None => format!("ldap://{}", self.domain.to_ascii_lowercase()),
        }
    }

    /// Build the bind principal for `username`: an AD user-principal-name of the
    /// form `username@DOMAIN`. If the caller already passed a UPN/DN (it contains
    /// `@` or looks like a DN with `=`), it is used verbatim.
    fn bind_upn(&self, username: &str) -> String {
        if username.contains('@') || username.contains('=') {
            username.to_string()
        } else {
            format!("{username}@{}", self.domain)
        }
    }
}

impl Provider for ActiveDirectoryAuthenticator {
    fn name(&self) -> &str {
        "active-directory"
    }
    fn description(&self) -> &str {
        "Active Directory / LDAP bind authentication via ldapwhoami"
    }
}

#[async_trait]
impl Authenticator for ActiveDirectoryAuthenticator {
    async fn authenticate(&self, credentials: &Credentials) -> Result<Identity> {
        let (username, password) = match credentials {
            Credentials::Password { username, password } => (username, password),
            Credentials::Token(_) => {
                return Err(Error::unsupported(
                    "active-directory authenticator only accepts password credentials",
                ));
            }
        };

        let uri = self.ldap_uri();
        let upn = self.bind_upn(username);

        tracing::info!(
            target: "ocf_auth::active_directory",
            domain = %self.domain,
            uri = %uri,
            bind_dn = %upn,
            "performing LDAP simple bind",
        );

        // ldapwhoami -x (simple bind) -H <uri> -D <bind-dn> -w <password>. The
        // password is supplied via -w as the OpenLDAP CLI requires; no stdin is
        // piped for the bind itself.
        let (code, _stdout, stderr) = run_with_stdin(
            "ldapwhoami",
            &["-x", "-H", &uri, "-D", &upn, "-w", password],
            None,
        )
        .await?;

        if code != 0 {
            return Err(Error::Unauthenticated(format!(
                "ldap bind failed for `{upn}`: {}",
                stderr.trim()
            )));
        }

        let groups = self.resolve_groups(&uri, &upn, password, username).await;
        let mut identity = Identity::new(username);
        identity.groups = groups;
        Ok(identity)
    }
}

impl ActiveDirectoryAuthenticator {
    /// Best-effort `memberOf` lookup for an authenticated user.
    ///
    /// Searches the directory (binding as the just-authenticated user) for the
    /// user's entry and extracts `memberOf` DNs. Group resolution is advisory:
    /// any failure yields an empty group list rather than failing an otherwise
    /// successful authentication.
    async fn resolve_groups(
        &self,
        uri: &str,
        bind_dn: &str,
        password: &str,
        username: &str,
    ) -> Vec<String> {
        let base = domain_to_base_dn(&self.domain);
        let filter = format!("(sAMAccountName={username})");
        let result = run_with_stdin(
            "ldapsearch",
            &[
                "-x", "-LLL", "-H", uri, "-D", bind_dn, "-w", password, "-b", &base, &filter,
                "memberOf",
            ],
            None,
        )
        .await;

        match result {
            Ok((0, stdout, _)) => parse_member_of(&stdout),
            _ => Vec::new(),
        }
    }
}

/// Convert a dotted DNS domain (`EXAMPLE.COM`) into an LDAP base DN
/// (`DC=EXAMPLE,DC=COM`), the search base AD uses for the domain root.
fn domain_to_base_dn(domain: &str) -> String {
    domain
        .split('.')
        .filter(|part| !part.is_empty())
        .map(|part| format!("DC={part}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Extract group names from LDIF `memberOf:` lines.
///
/// `ldapsearch -LLL` emits one `memberOf: <dn>` attribute per group; we keep the
/// leaf `CN=<name>` as the group identity (matching how AD groups surface to
/// RBAC), falling back to the full DN when no `CN=` component is present.
fn parse_member_of(ldif: &str) -> Vec<String> {
    let mut groups = Vec::new();
    for line in ldif.lines() {
        let line = line.trim();
        let Some(dn) = line.strip_prefix("memberOf:") else {
            continue;
        };
        let dn = dn.trim();
        if dn.is_empty() {
            continue;
        }
        let name = leaf_cn(dn).unwrap_or_else(|| dn.to_string());
        groups.push(name);
    }
    groups
}

/// Pull the first `CN=<value>` component out of a distinguished name.
fn leaf_cn(dn: &str) -> Option<String> {
    dn.split(',').find_map(|comp| {
        let comp = comp.trim();
        comp.strip_prefix("CN=")
            .or_else(|| comp.strip_prefix("cn="))
            .map(|v| v.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_upn_from_bare_username() {
        let ad = ActiveDirectoryAuthenticator::new("EXAMPLE.COM");
        assert_eq!(ad.bind_upn("alice"), "alice@EXAMPLE.COM");
    }

    #[test]
    fn passes_through_existing_upn_or_dn() {
        let ad = ActiveDirectoryAuthenticator::new("EXAMPLE.COM");
        assert_eq!(ad.bind_upn("alice@OTHER.COM"), "alice@OTHER.COM");
        assert_eq!(
            ad.bind_upn("CN=Alice,DC=example,DC=com"),
            "CN=Alice,DC=example,DC=com"
        );
    }

    #[test]
    fn default_uri_derives_from_domain() {
        let ad = ActiveDirectoryAuthenticator::new("EXAMPLE.COM");
        assert_eq!(ad.ldap_uri(), "ldap://example.com");
    }

    #[test]
    fn explicit_server_wins() {
        let ad = ActiveDirectoryAuthenticator::with_servers(
            "EXAMPLE.COM",
            vec!["ldaps://dc1.example.com".to_string()],
        );
        assert_eq!(ad.ldap_uri(), "ldaps://dc1.example.com");
    }

    #[test]
    fn domain_becomes_base_dn() {
        assert_eq!(domain_to_base_dn("EXAMPLE.COM"), "DC=EXAMPLE,DC=COM");
        assert_eq!(domain_to_base_dn("ad.corp.example.com"), "DC=ad,DC=corp,DC=example,DC=com");
    }

    #[test]
    fn parses_member_of_to_leaf_cn() {
        let ldif = "\
dn: CN=Alice,OU=Users,DC=example,DC=com
memberOf: CN=admins,OU=Groups,DC=example,DC=com
memberOf: CN=developers,OU=Groups,DC=example,DC=com
";
        assert_eq!(parse_member_of(ldif), vec!["admins", "developers"]);
    }

    #[test]
    fn member_of_falls_back_to_full_dn() {
        let ldif = "memberOf: OU=weird,DC=example,DC=com\n";
        assert_eq!(parse_member_of(ldif), vec!["OU=weird,DC=example,DC=com"]);
    }

    #[tokio::test]
    async fn tokens_are_unsupported() {
        let ad = ActiveDirectoryAuthenticator::new("EXAMPLE.COM");
        let err = ad
            .authenticate(&Credentials::token("abc"))
            .await
            .expect_err("tokens not supported for ad");
        assert!(matches!(err, Error::NotSupported(_)));
    }

    // Requires a reachable domain controller / LDAP server; run explicitly with
    // `cargo test -- --ignored` against a real directory.
    #[tokio::test]
    #[ignore = "needs ldapwhoami and a reachable LDAP/AD server"]
    async fn real_bind_rejects_bad_password() {
        let ad = ActiveDirectoryAuthenticator::with_servers(
            "EXAMPLE.COM",
            vec!["ldap://localhost".to_string()],
        );
        let err = ad
            .authenticate(&Credentials::password("alice", "definitely-wrong"))
            .await
            .expect_err("bad password must be rejected");
        assert!(matches!(err, Error::Unauthenticated(_)));
    }
}
