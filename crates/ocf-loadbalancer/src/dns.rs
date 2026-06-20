//! Dynamic DNS: publishing the records that point clients at a load balancer.
//!
//! [`DnsProvider`] is the pluggable contract for upserting and deleting records
//! in an authoritative zone. The shipped backend, [`CloudflareDns`], is **real**:
//! it drives the Cloudflare v4 DNS API over HTTPS by shelling out to the system
//! `curl` (so the crate takes no Rust HTTP dependency), authenticating with a
//! bearer token. An in-memory cache of the records it has published is kept
//! purely as a convenience for introspection and tests.

use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;
use tokio::process::Command;

/// Base URL of the Cloudflare v4 API.
const CLOUDFLARE_API: &str = "https://api.cloudflare.com/client/v4";

/// The DNS record types the fabric publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RecordType {
    A,
    Aaaa,
    Cname,
    Txt,
}

impl RecordType {
    /// The canonical DNS type label (e.g. `"AAAA"`).
    pub fn as_label(&self) -> &'static str {
        match self {
            RecordType::A => "A",
            RecordType::Aaaa => "AAAA",
            RecordType::Cname => "CNAME",
            RecordType::Txt => "TXT",
        }
    }
}

/// Pluggable contract for managing authoritative DNS records.
///
/// Extends [`Provider`] for the registry. Methods are async because a real DNS
/// API is network-bound.
#[async_trait]
pub trait DnsProvider: Provider {
    /// Create or update the record `(name, record_type)` in `zone` to `value`.
    async fn upsert_record(
        &self,
        zone: &str,
        name: &str,
        record_type: RecordType,
        value: &str,
    ) -> Result<()>;

    /// Delete the record `(name, record_type)` from `zone`.
    async fn delete_record(
        &self,
        zone: &str,
        name: &str,
        record_type: RecordType,
    ) -> Result<()>;
}

/// In-memory key identifying a single record within the convenience cache.
type RecordKey = (String, String, RecordType);

/// A Cloudflare-backed DNS provider — **real**.
///
/// Calls the Cloudflare v4 API over HTTPS via the system `curl`, authenticating
/// with `api_token`. The `records` map is a local cache of what has been
/// published through this instance; it is not the source of truth (Cloudflare
/// is) and exists only for introspection and tests.
pub struct CloudflareDns {
    /// Cloudflare API token used as the HTTPS bearer credential. Never logged.
    api_token: String,
    records: RwLock<HashMap<RecordKey, String>>,
}

impl CloudflareDns {
    pub fn new(api_token: impl Into<String>) -> Self {
        CloudflareDns {
            api_token: api_token.into(),
            records: RwLock::new(HashMap::new()),
        }
    }

    /// Snapshot of records published through this instance as
    /// `(zone, name, type, value)`. Primarily for tests and introspection.
    pub fn records(&self) -> Vec<(String, String, RecordType, String)> {
        self.records
            .read()
            .iter()
            .map(|((zone, name, ty), value)| {
                (zone.clone(), name.clone(), *ty, value.clone())
            })
            .collect()
    }

    /// The `Authorization: Bearer <token>` header for an authenticated request.
    fn auth_header(&self) -> String {
        format!("Authorization: Bearer {}", self.api_token)
    }

    /// Resolve the Cloudflare zone id for `zone` (a zone name like
    /// `example.com`). Errors if the zone is unknown to the account.
    async fn zone_id(&self, zone: &str) -> Result<String> {
        let url = format!("{CLOUDFLARE_API}/zones?name={zone}");
        let body = self
            .curl(&["-X", "GET", &url, "-H", &self.auth_header()])
            .await?;
        check_success(&body)?;
        first_id(&body).ok_or_else(|| {
            Error::provider("cloudflare", format!("zone `{zone}` not found"))
        })
    }

    /// Look up the id of the existing record `(name, record_type)` in
    /// `zone_id`, if one is already published. `None` means "create a new one".
    async fn existing_record_id(
        &self,
        zone_id: &str,
        name: &str,
        record_type: RecordType,
    ) -> Result<Option<String>> {
        let url = format!(
            "{CLOUDFLARE_API}/zones/{zone_id}/dns_records?type={}&name={name}",
            record_type.as_label()
        );
        let body = self
            .curl(&["-X", "GET", &url, "-H", &self.auth_header()])
            .await?;
        check_success(&body)?;
        Ok(first_id(&body))
    }

    /// Run `curl args...`, capturing stdout. A missing `curl` binary or a
    /// non-zero exit is mapped onto a `cloudflare` provider error. The bearer
    /// token is never included in any error text.
    async fn curl(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("curl")
            .arg("--silent")
            .arg("--show-error")
            .args(args)
            .output()
            .await
            .map_err(|e| Error::provider("cloudflare", format!("failed to spawn curl: {e}")))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = if stderr.trim().is_empty() {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            } else {
                stderr.trim().to_string()
            };
            Err(Error::provider("cloudflare", format!("curl failed: {message}")))
        }
    }
}

#[async_trait]
impl DnsProvider for CloudflareDns {
    async fn upsert_record(
        &self,
        zone: &str,
        name: &str,
        record_type: RecordType,
        value: &str,
    ) -> Result<()> {
        if zone.is_empty() || name.is_empty() {
            return Err(Error::invalid("dns record requires a zone and name"));
        }

        let zone_id = self.zone_id(zone).await?;
        let payload = record_json(name, record_type, value);
        let auth = self.auth_header();

        // Create when the record is new, update in place when it already exists.
        let body = match self.existing_record_id(&zone_id, name, record_type).await? {
            None => {
                let url = format!("{CLOUDFLARE_API}/zones/{zone_id}/dns_records");
                self.curl(&[
                    "-X", "POST", &url,
                    "-H", &auth,
                    "-H", "Content-Type: application/json",
                    "-d", &payload,
                ])
                .await?
            }
            Some(record_id) => {
                let url = format!("{CLOUDFLARE_API}/zones/{zone_id}/dns_records/{record_id}");
                self.curl(&[
                    "-X", "PUT", &url,
                    "-H", &auth,
                    "-H", "Content-Type: application/json",
                    "-d", &payload,
                ])
                .await?
            }
        };
        check_success(&body)?;

        tracing::info!(
            zone = %zone,
            name = %name,
            record_type = record_type.as_label(),
            "cloudflare: upserted DNS record"
        );
        self.records.write().insert(
            (zone.to_string(), name.to_string(), record_type),
            value.to_string(),
        );
        Ok(())
    }

    async fn delete_record(
        &self,
        zone: &str,
        name: &str,
        record_type: RecordType,
    ) -> Result<()> {
        if zone.is_empty() || name.is_empty() {
            return Err(Error::invalid("dns record requires a zone and name"));
        }

        let zone_id = self.zone_id(zone).await?;
        // Nothing to delete remotely if Cloudflare has no such record.
        if let Some(record_id) =
            self.existing_record_id(&zone_id, name, record_type).await?
        {
            let url = format!("{CLOUDFLARE_API}/zones/{zone_id}/dns_records/{record_id}");
            let body = self
                .curl(&["-X", "DELETE", &url, "-H", &self.auth_header()])
                .await?;
            check_success(&body)?;
        }

        tracing::info!(
            zone = %zone,
            name = %name,
            record_type = record_type.as_label(),
            "cloudflare: deleted DNS record"
        );
        self.records
            .write()
            .remove(&(zone.to_string(), name.to_string(), record_type));
        Ok(())
    }
}

impl Provider for CloudflareDns {
    fn name(&self) -> &str {
        "cloudflare"
    }
    fn description(&self) -> &str {
        "Cloudflare authoritative DNS provider (Cloudflare v4 API over HTTPS)"
    }
}

/// Build the JSON body for a create/update DNS record request, by hand so the
/// crate needs no JSON serializer. `value` and `name` are escaped for the few
/// characters that are illegal inside a JSON string.
fn record_json(name: &str, record_type: RecordType, value: &str) -> String {
    format!(
        "{{\"type\":\"{}\",\"name\":\"{}\",\"content\":\"{}\",\"ttl\":1,\"proxied\":false}}",
        record_type.as_label(),
        json_escape(name),
        json_escape(value),
    )
}

/// Escape the characters that must not appear literally inside a JSON string.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Minimal check that a Cloudflare response reported success. We deliberately
/// scan for the literal `"success":true` (tolerating insignificant whitespace
/// after the colon) rather than pulling in a JSON parser.
fn check_success(body: &str) -> Result<()> {
    if contains_success_true(body) {
        Ok(())
    } else {
        Err(Error::provider("cloudflare", body.trim().to_string()))
    }
}

/// True if `body` contains a `"success": true` member (whitespace-tolerant).
fn contains_success_true(body: &str) -> bool {
    for (idx, _) in body.match_indices("\"success\"") {
        let rest = &body[idx + "\"success\"".len()..];
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        if rest.trim_start().starts_with("true") {
            return true;
        }
    }
    false
}

/// Extract the value of the first `"id":"..."` member in `body`, if any. The
/// Cloudflare list endpoints return the matching object(s) first, so the first
/// id is the record/zone we asked about.
fn first_id(body: &str) -> Option<String> {
    let key = "\"id\"";
    let idx = body.find(key)?;
    let rest = &body[idx + key.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Register the built-in DNS providers into `reg`.
pub fn register_builtins(reg: &mut Registry<dyn DnsProvider>) -> Result<()> {
    reg.register(
        "cloudflare",
        std::sync::Arc::new(CloudflareDns::new(
            std::env::var("CLOUDFLARE_API_TOKEN").unwrap_or_default(),
        )),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn empty_zone_is_rejected() {
        let dns = CloudflareDns::new("token");
        assert!(dns
            .upsert_record("", "www", RecordType::A, "203.0.113.7")
            .await
            .is_err());
    }

    #[test]
    fn success_scan_matches_real_shapes() {
        assert!(contains_success_true(r#"{"result":null,"success":true}"#));
        assert!(contains_success_true(r#"{"success": true,"errors":[]}"#));
        assert!(!contains_success_true(r#"{"success":false,"errors":["bad"]}"#));
        assert!(!contains_success_true(r#"{"result":[]}"#));
    }

    #[test]
    fn first_id_extracts_zone_id() {
        let body = r#"{"result":[{"id":"abc123","name":"example.com"}],"success":true}"#;
        assert_eq!(first_id(body).as_deref(), Some("abc123"));
        // An empty result list has no id.
        assert_eq!(first_id(r#"{"result":[],"success":true}"#), None);
    }

    #[test]
    fn record_json_is_well_formed_and_escaped() {
        let json = record_json("www", RecordType::A, "203.0.113.7");
        assert!(json.contains("\"type\":\"A\""));
        assert!(json.contains("\"name\":\"www\""));
        assert!(json.contains("\"content\":\"203.0.113.7\""));
        // A value containing a quote is escaped, keeping the JSON valid.
        let escaped = record_json("h", RecordType::Txt, "a\"b");
        assert!(escaped.contains("a\\\"b"));
    }
}
