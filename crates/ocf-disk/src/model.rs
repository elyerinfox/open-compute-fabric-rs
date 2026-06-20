//! The physical-disk resource model and its health / LED vocabulary.

use chrono::{DateTime, Utc};
use ocf_core::prelude::*;

/// Coarse SMART-derived health signal for a physical disk.
///
/// Mirrors the way `smartctl`/`smartmontools` summarize a drive: a healthy
/// `Ok`, a `Warning` for pre-fail attributes trending bad (reallocated
/// sectors, pending sectors, high temperature), a `Failing` drive that has
/// tripped the SMART overall-health assessment, and `Unknown` when the drive
/// cannot be queried (no SMART support, behind an unsupported HBA, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskHealth {
    Ok,
    Warning,
    Failing,
    Unknown,
}

impl Default for DiskHealth {
    fn default() -> Self {
        DiskHealth::Unknown
    }
}

impl DiskHealth {
    /// Whether this health state warrants operator attention (warning or worse).
    pub fn is_actionable(&self) -> bool {
        matches!(self, DiskHealth::Warning | DiskHealth::Failing)
    }
}

/// State of a drive-bay locator LED.
///
/// These map onto the SES/enclosure LED states `ledctl` drives: `Normal` (off),
/// `Locate` (the technician "find this drive" blink), `Fault` (solid fault), and
/// `Rebuild` (array rebuild in progress).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedState {
    Normal,
    Locate,
    Fault,
    Rebuild,
}

impl Default for LedState {
    fn default() -> Self {
        LedState::Normal
    }
}

/// A physical disk attached to a machine.
///
/// Implements [`Resource`] so the API serializer, audit log, and topology
/// indexer can treat it uniformly. The drive is identified for the lifetime of
/// the fleet by its `serial` (a `mark_rma` and `first_seen` are tracked against
/// the serial, since a disk keeps its serial as it moves between slots and
/// machines).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhysicalDisk {
    pub metadata: Metadata,
    /// The machine this disk is currently attached to.
    pub machine_id: Id,
    /// OS device path, e.g. `/dev/sda` or `/dev/nvme0n1`. Best-effort; a disk
    /// behind a RAID HBA may not have a host device node.
    pub dev_path: String,
    /// Drive serial number — the stable fleet-wide identity of the disk.
    pub serial: String,
    /// World Wide Name (SAS/SATA WWN), when reported.
    pub wwn: Option<String>,
    pub model: String,
    pub vendor: String,
    pub size_bytes: u64,
    pub health: DiskHealth,
    /// When the fabric first observed this serial.
    pub first_seen: DateTime<Utc>,
    /// Set once the disk has been marked for RMA (return to vendor).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rma_date: Option<DateTime<Utc>>,
    /// Enclosure identifier (SES enclosure) the disk lives in, when known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub enclosure: Option<String>,
    /// Physical slot within the enclosure, when known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub slot: Option<u32>,
}

impl PhysicalDisk {
    /// Construct a disk record for `serial` on `machine_id`.
    ///
    /// `first_seen` is stamped to now; callers that already track an earlier
    /// sighting (via [`crate::service::DiskService`]) should overwrite it.
    pub fn new(machine_id: Id, serial: impl Into<String>) -> Self {
        let serial = serial.into();
        PhysicalDisk {
            metadata: Metadata::new(serial.clone()),
            machine_id,
            dev_path: String::new(),
            serial,
            wwn: None,
            model: String::new(),
            vendor: String::new(),
            size_bytes: 0,
            health: DiskHealth::Unknown,
            first_seen: Utc::now(),
            rma_date: None,
            enclosure: None,
            slot: None,
        }
    }

    /// Whether this disk has been marked for RMA.
    pub fn is_rma(&self) -> bool {
        self.rma_date.is_some()
    }
}

impl Resource for PhysicalDisk {
    fn kind(&self) -> &'static str {
        "disk"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}
