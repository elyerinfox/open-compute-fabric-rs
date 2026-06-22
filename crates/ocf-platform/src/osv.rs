//! OSV vulnerability lookups for installed packages.
//!
//! Queries the public [OSV database](https://osv.dev) batch API
//! (`POST https://api.osv.dev/v1/querybatch`) with each installed package's name,
//! version, and distro ecosystem, and reports the packages that match a known
//! vulnerability. Building the request and parsing the response are pure
//! functions (unit-tested); the HTTP call is a blocking `ureq` request run on a
//! blocking thread, so a host with no network simply gets an error and the rest
//! of the fabric keeps running.

use crate::update::InstalledPackage;
use ocf_core::prelude::*;

/// The default OSV batch-query endpoint.
pub const OSV_QUERYBATCH_URL: &str = "https://api.osv.dev/v1/querybatch";

/// OSV caps a batch query at 1000 entries; we chunk to stay under it.
const MAX_BATCH: usize = 1000;

/// An installed package that matched one or more OSV advisories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VulnerablePackage {
    pub name: String,
    pub version: String,
    /// The OSV / CVE ids affecting this package@version.
    pub vuln_ids: Vec<String>,
}

/// A client for the OSV batch-query API.
#[derive(Debug, Clone)]
pub struct OsvClient {
    url: String,
}

impl Default for OsvClient {
    fn default() -> Self {
        OsvClient {
            url: OSV_QUERYBATCH_URL.to_string(),
        }
    }
}

impl OsvClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Point the client at a different endpoint (for testing / a mirror).
    pub fn with_url(url: impl Into<String>) -> Self {
        OsvClient { url: url.into() }
    }

    /// Scan `packages` (in the given OSV `ecosystem`, e.g. `"Ubuntu"`) for known
    /// vulnerabilities, chunking to respect OSV's batch limit. Returns only the
    /// packages that matched at least one advisory.
    pub async fn scan(
        &self,
        packages: &[InstalledPackage],
        ecosystem: &str,
    ) -> Result<Vec<VulnerablePackage>> {
        let mut found = Vec::new();
        for chunk in packages.chunks(MAX_BATCH) {
            found.extend(self.query_chunk(chunk, ecosystem).await?);
        }
        Ok(found)
    }

    async fn query_chunk(
        &self,
        packages: &[InstalledPackage],
        ecosystem: &str,
    ) -> Result<Vec<VulnerablePackage>> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        let body = build_query(packages, ecosystem);
        let url = self.url.clone();
        // `ureq` is blocking — never run it on the async reactor.
        let json: serde_json::Value = tokio::task::spawn_blocking(move || {
            ureq::post(&url)
                .send_json(body)
                .map_err(|e| Error::provider("osv", format!("query failed: {e}")))?
                .into_json::<serde_json::Value>()
                .map_err(|e| Error::provider("osv", format!("decode failed: {e}")))
        })
        .await
        .map_err(|e| Error::internal(format!("osv task join: {e}")))??;
        Ok(parse_response(packages, &json))
    }
}

/// Build the OSV `querybatch` request body for `packages` in `ecosystem`.
pub fn build_query(packages: &[InstalledPackage], ecosystem: &str) -> serde_json::Value {
    let queries: Vec<serde_json::Value> = packages
        .iter()
        .map(|p| {
            serde_json::json!({
                "package": { "name": p.name, "ecosystem": ecosystem },
                "version": p.version,
            })
        })
        .collect();
    serde_json::json!({ "queries": queries })
}

/// Parse an OSV `querybatch` response. Results map **positionally** to the
/// queried `packages`; a result with a non-empty `vulns` array marks that
/// package vulnerable.
pub fn parse_response(
    packages: &[InstalledPackage],
    json: &serde_json::Value,
) -> Vec<VulnerablePackage> {
    let Some(results) = json.get("results").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, result) in results.iter().enumerate() {
        let Some(pkg) = packages.get(i) else { break };
        let ids: Vec<String> = result
            .get("vulns")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.get("id").and_then(|x| x.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if !ids.is_empty() {
            out.push(VulnerablePackage {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                vuln_ids: ids,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkgs() -> Vec<InstalledPackage> {
        vec![
            InstalledPackage { name: "openssl".into(), version: "3.0.2".into() },
            InstalledPackage { name: "bash".into(), version: "5.1".into() },
        ]
    }

    #[test]
    fn query_body_has_one_entry_per_package() {
        let body = build_query(&pkgs(), "Ubuntu");
        let queries = body["queries"].as_array().unwrap();
        assert_eq!(queries.len(), 2);
        assert_eq!(queries[0]["package"]["name"], "openssl");
        assert_eq!(queries[0]["package"]["ecosystem"], "Ubuntu");
        assert_eq!(queries[0]["version"], "3.0.2");
    }

    #[test]
    fn response_maps_vulns_positionally() {
        // openssl has a vuln, bash does not.
        let json = serde_json::json!({
            "results": [
                { "vulns": [ { "id": "CVE-2022-0778" }, { "id": "OSV-2022-1" } ] },
                {}
            ]
        });
        let vulns = parse_response(&pkgs(), &json);
        assert_eq!(vulns.len(), 1);
        assert_eq!(vulns[0].name, "openssl");
        assert_eq!(vulns[0].vuln_ids, vec!["CVE-2022-0778", "OSV-2022-1"]);
    }

    #[test]
    fn empty_or_missing_results_are_no_vulns() {
        assert!(parse_response(&pkgs(), &serde_json::json!({})).is_empty());
        assert!(parse_response(&pkgs(), &serde_json::json!({"results": []})).is_empty());
    }

    #[tokio::test]
    #[ignore = "hits the live api.osv.dev; run with `--ignored`"]
    async fn live_osv_reports_a_known_vulnerability() {
        // jinja2 2.4.1 (PyPI) has long-published advisories in OSV.
        let pkgs = vec![InstalledPackage {
            name: "jinja2".into(),
            version: "2.4.1".into(),
        }];
        let vulns = OsvClient::new().scan(&pkgs, "PyPI").await.expect("osv query");
        assert!(!vulns.is_empty(), "OSV should report vulns for jinja2 2.4.1");
        assert!(!vulns[0].vuln_ids.is_empty());
    }
}
