//! The pluggable disk-management contract and its sysfs/`lsblk`/`smartctl` backend.

use crate::model::{DiskHealth, PhysicalDisk};
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;
use tokio::process::Command;

/// Pluggable contract for enumerating disks and reading their SMART health.
///
/// The default backend shells out to `lsblk`/`smartctl`; the controller depends
/// only on this trait, so an HBA-specific backend (megaraid, nvme-cli, ...) can
/// be swapped in without touching callers.
#[async_trait]
pub trait DiskManager: Provider {
    /// List the physical disks attached to `machine_id`.
    async fn list(&self, machine_id: &Id) -> Result<Vec<PhysicalDisk>>;

    /// Read the current SMART-derived health of the disk with this `serial`.
    async fn smart(&self, serial: &str) -> Result<DiskHealth>;

    /// Mark the disk with this `serial` as needing RMA (return to vendor).
    async fn mark_rma(&self, serial: &str) -> Result<()>;
}

/// `DiskManager` backed by sysfs / `lsblk` / `smartctl`.
///
/// [`list`](DiskManager::list) enumerates whole disks via `lsblk` and reads
/// SMART overall-health via `smartctl`. RMA is pure bookkeeping (the vendor
/// process happens off-host), so it mutates an in-memory record keyed by serial.
/// That same map is exposed to tests through [`seed`](Self::seed) so the rest of
/// the fabric has something concrete to drive without real hardware.
pub struct SysfsDiskManager {
    /// Bookkeeping overrides keyed by serial. On a real host this holds RMA
    /// flags applied via [`mark_rma`](DiskManager::mark_rma); in tests it also
    /// holds seeded disks so `list` has something to return without `lsblk`.
    disks: RwLock<HashMap<String, PhysicalDisk>>,
}

impl SysfsDiskManager {
    pub fn new() -> Self {
        SysfsDiskManager {
            disks: RwLock::new(HashMap::new()),
        }
    }

    /// Seed the in-memory inventory with a known disk (test / dev helper).
    pub fn seed(&self, disk: PhysicalDisk) {
        self.disks.write().insert(disk.serial.clone(), disk);
    }
}

impl Default for SysfsDiskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for SysfsDiskManager {
    fn name(&self) -> &str {
        "sysfs"
    }
    fn description(&self) -> &str {
        "Enumerates disks via lsblk and reads SMART health via smartctl."
    }
}

#[async_trait]
impl DiskManager for SysfsDiskManager {
    async fn list(&self, machine_id: &Id) -> Result<Vec<PhysicalDisk>> {
        // Start from any seeded/bookkeeping records for this machine. In tests
        // these *are* the inventory; on a real host they are usually just RMA
        // overlays, which we re-apply by serial below.
        let mut by_serial: HashMap<String, PhysicalDisk> = self
            .disks
            .read()
            .values()
            .filter(|d| &d.machine_id == machine_id)
            .map(|d| (d.serial.clone(), d.clone()))
            .collect();

        // Enumerate live disks. `lsblk` doesn't know about fabric machines, so
        // every disk it reports belongs to the host this daemon runs on, which
        // is `machine_id`. A missing/failed `lsblk` (e.g. on a CI host) is not
        // fatal here — we still return the bookkeeping view.
        match run(
            "lsblk",
            &[
                "-dn",
                "-P",
                "-b",
                "-o",
                "NAME,SERIAL,WWN,MODEL,VENDOR,SIZE",
            ],
        )
        .await
        {
            Ok(output) => {
                for disk in parse_lsblk_pairs(&output, machine_id) {
                    // A seeded record wins over the raw hardware snapshot; only
                    // add live disks we don't already track for this machine.
                    by_serial.entry(disk.serial.clone()).or_insert(disk);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "lsblk enumeration failed; returning bookkeeping view only");
            }
        }

        // Overlay RMA bookkeeping by serial: a disk flagged via `mark_rma` may
        // have been discovered live (so it isn't keyed under this machine in the
        // map above), yet its RMA verdict must still surface on the live record.
        {
            let records = self.disks.read();
            for disk in by_serial.values_mut() {
                if let Some(rma) = records.get(&disk.serial).and_then(|r| r.rma_date) {
                    if disk.rma_date.is_none() {
                        disk.rma_date = Some(rma);
                        disk.health = DiskHealth::Failing;
                    }
                }
            }
        }

        Ok(by_serial.into_values().collect())
    }

    async fn smart(&self, serial: &str) -> Result<DiskHealth> {
        // Resolve the serial to its OS device path. A bookkeeping/seeded record
        // is authoritative for its path; otherwise look it up via `lsblk`.
        let dev_path = self.resolve_dev_path(serial).await?;

        // Use `smartctl -H` and scan its overall-health line (stable across
        // smartmontools versions and dependency-free to parse).
        let output = run("smartctl", &["-H", &dev_path]).await?;
        Ok(parse_smart_health(&output))
    }

    async fn mark_rma(&self, serial: &str) -> Result<()> {
        // RMA is bookkeeping, not an OS action: flag the in-memory record so it
        // survives subsequent `list` calls. The record may not exist yet if the
        // disk was discovered live via `lsblk`, so materialize it on demand.
        let mut disks = self.disks.write();
        let disk = disks
            .entry(serial.to_string())
            .or_insert_with(|| PhysicalDisk::new(Id::named(""), serial));
        if disk.rma_date.is_none() {
            disk.rma_date = Some(chrono::Utc::now());
            disk.health = DiskHealth::Failing;
            disk.metadata.touch();
        }
        tracing::info!(serial, "marked disk for RMA");
        Ok(())
    }
}

impl SysfsDiskManager {
    /// Resolve a serial to its `/dev/<name>` path, preferring a bookkeeping
    /// record and falling back to a fresh `lsblk` enumeration.
    async fn resolve_dev_path(&self, serial: &str) -> Result<String> {
        if let Some(path) = self
            .disks
            .read()
            .get(serial)
            .map(|d| d.dev_path.clone())
            .filter(|p| !p.is_empty())
        {
            return Ok(path);
        }

        let output = run(
            "lsblk",
            &[
                "-dn",
                "-P",
                "-b",
                "-o",
                "NAME,SERIAL,WWN,MODEL,VENDOR,SIZE",
            ],
        )
        .await?;
        parse_lsblk_pairs(&output, &Id::named(""))
            .into_iter()
            .find(|d| d.serial == serial)
            .map(|d| d.dev_path)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| Error::not_found(format!("disk serial `{serial}`")))
    }
}

/// Run `cmd args...`, returning captured stdout on success.
///
/// Maps a missing binary or a non-zero exit onto [`Error::provider`] so callers
/// get a uniform, transport-mappable failure regardless of which tool ran.
async fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| Error::provider("sysfs", format!("failed to spawn `{cmd}`: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::provider(
            "sysfs",
            format!(
                "`{cmd}` exited with {}: {}",
                output.status,
                stderr.trim()
            ),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `lsblk -P` (`KEY="value"` pairs, one disk per line) into disks owned by
/// `machine_id`.
///
/// `lsblk -P` quotes every value and emits one space-separated record per line,
/// e.g. `NAME="sda" SERIAL="S1" WWN="0x5" MODEL="ST" VENDOR="ATA" SIZE="1024"`.
fn parse_lsblk_pairs(output: &str, machine_id: &Id) -> Vec<PhysicalDisk> {
    let mut disks = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields = parse_pairs(line);
        let name = fields.get("NAME").map(String::as_str).unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let serial = fields.get("SERIAL").cloned().unwrap_or_default();

        let mut disk = PhysicalDisk::new(machine_id.clone(), serial);
        disk.dev_path = format!("/dev/{name}");
        disk.wwn = fields
            .get("WWN")
            .filter(|w| !w.is_empty())
            .cloned();
        disk.model = fields.get("MODEL").cloned().unwrap_or_default();
        disk.vendor = fields.get("VENDOR").cloned().unwrap_or_default();
        disk.size_bytes = fields
            .get("SIZE")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        disks.push(disk);
    }
    disks
}

/// Split one `lsblk -P` line into its `KEY="value"` pairs.
///
/// Values are double-quoted; a quoted value may itself contain spaces (common
/// in MODEL strings), so we scan rather than naively splitting on whitespace.
fn parse_pairs(line: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip leading separators.
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        // Read KEY up to '='.
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && bytes[i] != b' ' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            break;
        }
        let key = &line[key_start..i];
        i += 1; // consume '='
        // Expect an opening quote.
        if i >= bytes.len() || bytes[i] != b'"' {
            continue;
        }
        i += 1; // consume opening quote
        let val_start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            i += 1;
        }
        let value = &line[val_start..i];
        if i < bytes.len() {
            i += 1; // consume closing quote
        }
        fields.insert(key.to_string(), value.to_string());
    }
    fields
}

/// Map `smartctl -H` output onto a [`DiskHealth`].
///
/// `smartctl` prints an overall-health line such as
/// `SMART overall-health self-assessment test result: PASSED`
/// (or `FAILED!`). We classify on that verdict and fall back to `Unknown` when
/// the device cannot be assessed.
fn parse_smart_health(output: &str) -> DiskHealth {
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("overall-health") || lower.contains("smart health status") {
            if lower.contains("passed") || lower.contains(": ok") {
                return DiskHealth::Ok;
            }
            if lower.contains("failed") {
                return DiskHealth::Failing;
            }
        }
    }
    DiskHealth::Unknown
}

/// Register the built-in [`DiskManager`] backends.
pub fn register_builtins(reg: &mut Registry<dyn DiskManager>) -> Result<()> {
    reg.register("sysfs", std::sync::Arc::new(SysfsDiskManager::new()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsblk_pairs_with_spaces_in_model() {
        let line = r#"NAME="sda" SERIAL="S1ABC" WWN="0x5000c500" MODEL="ST1000 NM0033" VENDOR="ATA" SIZE="1000204886016""#;
        let disks = parse_lsblk_pairs(line, &Id::named("m1"));
        assert_eq!(disks.len(), 1);
        let d = &disks[0];
        assert_eq!(d.dev_path, "/dev/sda");
        assert_eq!(d.serial, "S1ABC");
        assert_eq!(d.wwn.as_deref(), Some("0x5000c500"));
        assert_eq!(d.model, "ST1000 NM0033");
        assert_eq!(d.vendor, "ATA");
        assert_eq!(d.size_bytes, 1_000_204_886_016);
        assert_eq!(d.machine_id, Id::named("m1"));
    }

    #[test]
    fn parses_multiple_lsblk_lines_and_skips_blanks() {
        let out = "NAME=\"sda\" SERIAL=\"A\" WWN=\"\" MODEL=\"M\" VENDOR=\"V\" SIZE=\"512\"\n\nNAME=\"nvme0n1\" SERIAL=\"B\" WWN=\"0x1\" MODEL=\"N\" VENDOR=\"\" SIZE=\"1024\"\n";
        let disks = parse_lsblk_pairs(out, &Id::named("m1"));
        assert_eq!(disks.len(), 2);
        // Empty WWN becomes None.
        let sda = disks.iter().find(|d| d.dev_path == "/dev/sda").expect("sda");
        assert_eq!(sda.wwn, None);
        let nvme = disks
            .iter()
            .find(|d| d.dev_path == "/dev/nvme0n1")
            .expect("nvme");
        assert_eq!(nvme.wwn.as_deref(), Some("0x1"));
        assert_eq!(nvme.size_bytes, 1024);
    }

    #[test]
    fn lsblk_line_without_name_is_skipped() {
        let line = r#"SERIAL="A" SIZE="512""#;
        assert!(parse_lsblk_pairs(line, &Id::named("m1")).is_empty());
    }

    #[test]
    fn smart_passed_is_ok() {
        let out = "SMART overall-health self-assessment test result: PASSED\n";
        assert_eq!(parse_smart_health(out), DiskHealth::Ok);
    }

    #[test]
    fn smart_failed_is_failing() {
        let out = "SMART overall-health self-assessment test result: FAILED!\n";
        assert_eq!(parse_smart_health(out), DiskHealth::Failing);
    }

    #[test]
    fn smart_sas_health_status_ok() {
        // SAS drives report a slightly different phrasing.
        let out = "SMART Health Status: OK\n";
        assert_eq!(parse_smart_health(out), DiskHealth::Ok);
    }

    #[test]
    fn smart_unparseable_is_unknown() {
        let out = "smartctl: unable to detect device type\n";
        assert_eq!(parse_smart_health(out), DiskHealth::Unknown);
    }

    #[tokio::test]
    async fn seeded_list_returns_seeded_disk_without_lsblk() {
        let mgr = SysfsDiskManager::new();
        let mut disk = PhysicalDisk::new(Id::named("m1"), "SEED-1");
        disk.dev_path = "/dev/sdz".to_string();
        disk.health = DiskHealth::Ok;
        mgr.seed(disk);

        let disks = mgr.list(&Id::named("m1")).await.expect("list");
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].serial, "SEED-1");
    }

    #[tokio::test]
    async fn mark_rma_materializes_and_flags_record() {
        let mgr = SysfsDiskManager::new();
        mgr.mark_rma("UNSEEN").await.expect("rma");
        let disks = mgr.list(&Id::named("")).await.expect("list");
        let d = disks.iter().find(|d| d.serial == "UNSEEN").expect("rma disk");
        assert!(d.is_rma());
        assert_eq!(d.health, DiskHealth::Failing);
    }

    #[tokio::test]
    #[ignore = "requires real lsblk on the host"]
    async fn real_lsblk_enumerates() {
        let mgr = SysfsDiskManager::new();
        let disks = mgr.list(&Id::named("local")).await.expect("list");
        // On a real host with at least one disk this should be non-empty.
        assert!(!disks.is_empty());
    }

    #[tokio::test]
    #[ignore = "requires real smartctl and a real device"]
    async fn real_smart_reads_health() {
        let mgr = SysfsDiskManager::new();
        let disks = mgr.list(&Id::named("local")).await.expect("list");
        let serial = &disks[0].serial;
        let _health = mgr.smart(serial).await.expect("smart");
    }
}
