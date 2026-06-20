//! High-level disk service: reconciles backend enumeration with persistent
//! per-serial bookkeeping (first-seen and RMA dates).

use crate::led::LedControl;
use crate::manager::DiskManager;
use crate::model::{DiskHealth, LedState, PhysicalDisk};
use chrono::{DateTime, Utc};
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Durable, fleet-wide facts the fabric keeps about a disk serial, independent
/// of where the drive is currently slotted or whether it is reachable now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskRecord {
    pub serial: String,
    /// First time the fabric ever observed this serial.
    pub first_seen: DateTime<Utc>,
    /// Set once the disk has been marked for RMA.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rma_date: Option<DateTime<Utc>>,
    /// The machine the disk was most recently observed on, used to address its
    /// LED. `None` until the disk has been seen via [`DiskService::list`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_machine: Option<Id>,
}

/// Orchestrates a [`DiskManager`] backend and a [`LedControl`] backend over an
/// in-memory store keyed by serial.
///
/// The backend reports the *current* view of a machine's disks; the service
/// owns the *historical* view — `first_seen` (so a re-slotted drive keeps its
/// original sighting) and `rma_date`. Listing a machine merges the two so each
/// returned [`PhysicalDisk`] carries the canonical history, and records which
/// machine last reported each serial so the LED can later be addressed.
pub struct DiskService {
    manager: Arc<dyn DiskManager>,
    led: Arc<dyn LedControl>,
    records: RwLock<HashMap<String, DiskRecord>>,
}

impl DiskService {
    /// Build a service over the given disk-management and LED backends.
    pub fn new(manager: Arc<dyn DiskManager>, led: Arc<dyn LedControl>) -> Self {
        DiskService {
            manager,
            led,
            records: RwLock::new(HashMap::new()),
        }
    }

    /// The disk-management backend this service drives.
    pub fn manager(&self) -> &Arc<dyn DiskManager> {
        &self.manager
    }

    /// The LED backend this service drives.
    pub fn led(&self) -> &Arc<dyn LedControl> {
        &self.led
    }

    /// List the disks on `machine_id`, stamping each with the canonical
    /// `first_seen`/`rma_date` from the persistent store and registering newly
    /// seen serials (and their machine) as a side effect.
    pub async fn list(&self, machine_id: &Id) -> Result<Vec<PhysicalDisk>> {
        let mut disks = self.manager.list(machine_id).await?;
        let mut records = self.records.write();
        for disk in &mut disks {
            let record = records
                .entry(disk.serial.clone())
                .or_insert_with(|| DiskRecord {
                    serial: disk.serial.clone(),
                    first_seen: disk.first_seen,
                    rma_date: None,
                    last_machine: None,
                });
            // Earliest sighting wins; a re-slotted drive keeps its history.
            if disk.first_seen < record.first_seen {
                record.first_seen = disk.first_seen;
            }
            record.last_machine = Some(disk.machine_id.clone());

            // Project the canonical history back onto the returned disk.
            disk.first_seen = record.first_seen;
            if disk.rma_date.is_none() {
                disk.rma_date = record.rma_date;
            }
        }
        Ok(disks)
    }

    /// Read the current SMART health of `serial` from the backend.
    pub async fn smart(&self, serial: &str) -> Result<DiskHealth> {
        self.manager.smart(serial).await
    }

    /// Mark `serial` for RMA: records the RMA date in the store, tells the
    /// backend, and lights the fault LED if the disk's location is known.
    pub async fn mark_rma(&self, serial: &str) -> Result<()> {
        let now = Utc::now();
        {
            let mut records = self.records.write();
            let record = records
                .entry(serial.to_string())
                .or_insert_with(|| DiskRecord {
                    serial: serial.to_string(),
                    first_seen: now,
                    rma_date: None,
                    last_machine: None,
                });
            if record.rma_date.is_none() {
                record.rma_date = Some(now);
            }
        }

        self.manager.mark_rma(serial).await?;

        // Best-effort: light the fault LED if we know where the disk lives. A
        // disk that has never been listed (unknown location) is not an error.
        if let Some(disk) = self.find(serial).await? {
            self.led.set_led(&disk, LedState::Fault).await?;
        }
        tracing::info!(serial, "disk marked for RMA");
        Ok(())
    }

    /// Set the locator/fault LED for the disk with this `serial`.
    ///
    /// Errors with `NotFound` if the disk has never been listed (so its current
    /// machine/location is unknown and the LED cannot be addressed).
    pub async fn set_led(&self, serial: &str, state: LedState) -> Result<()> {
        let disk = self
            .find(serial)
            .await?
            .ok_or_else(|| Error::not_found(format!("disk serial `{serial}` location")))?;
        self.led.set_led(&disk, state).await
    }

    /// The persistent bookkeeping record for `serial`, if the fabric has ever
    /// seen it.
    pub fn record(&self, serial: &str) -> Option<DiskRecord> {
        self.records.read().get(serial).cloned()
    }

    /// All persistent disk records the fabric is tracking.
    pub fn records(&self) -> Vec<DiskRecord> {
        self.records.read().values().cloned().collect()
    }

    /// Resolve `serial` to its live [`PhysicalDisk`] by re-listing the machine
    /// it was last seen on. Returns `Ok(None)` when the serial's location is
    /// unknown (never listed) or the disk is no longer present there.
    async fn find(&self, serial: &str) -> Result<Option<PhysicalDisk>> {
        let machine = match self.records.read().get(serial).and_then(|r| r.last_machine.clone()) {
            Some(m) => m,
            None => return Ok(None),
        };
        let disk = self
            .manager
            .list(&machine)
            .await?
            .into_iter()
            .find(|d| d.serial == serial);
        Ok(disk)
    }
}
