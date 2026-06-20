//! # ocf-disk
//!
//! Physical-disk inventory, SMART health, drive-bay LED control, and RMA
//! tracking for the fabric.
//!
//! The model ([`model`]) is a [`PhysicalDisk`] resource carrying a SMART-derived
//! [`DiskHealth`] and an enclosure [`LedState`]. Two pluggable contracts drive
//! the hardware: [`manager::DiskManager`] enumerates disks and reads SMART
//! (default backend [`manager::SysfsDiskManager`], shelling out to `lsblk` and
//! `smartctl`), and [`led::LedControl`] drives the locator/fault LED (default
//! backend [`led::LedctlControl`], shelling out to `ledctl`). RMA is bookkeeping
//! kept in memory, since the vendor-return process happens off-host.
//!
//! [`service::DiskService`] sits above the backends and owns the *historical*
//! view of a drive — keyed by serial, it tracks `first_seen` (so a re-slotted
//! drive keeps its original sighting) and `rma_date`, merging that history onto
//! every disk it lists.

pub mod led;
pub mod manager;
pub mod model;
pub mod service;

pub use led::{LedControl, LedctlControl};
pub use manager::{DiskManager, SysfsDiskManager};
pub use model::{DiskHealth, LedState, PhysicalDisk};
pub use service::{DiskRecord, DiskService};

use ocf_core::prelude::*;

/// Register every built-in disk-management backend into `reg`.
///
/// Mirrors the per-crate `register_builtins` convention. LED backends are
/// registered separately via [`led::register_builtins`].
pub fn register_builtins(reg: &mut Registry<dyn DiskManager>) -> Result<()> {
    manager::register_builtins(reg)
}

/// Register every built-in LED-control backend into `reg`.
pub fn register_led_builtins(reg: &mut Registry<dyn LedControl>) -> Result<()> {
    led::register_builtins(reg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    fn machine() -> Id {
        Id::named("machine-1")
    }

    fn seeded_manager() -> Arc<SysfsDiskManager> {
        let mgr = SysfsDiskManager::new();
        let mut disk = PhysicalDisk::new(machine(), "SERIAL-ABC");
        disk.dev_path = "/dev/sda".to_string();
        disk.model = "MODEL-X".to_string();
        disk.health = DiskHealth::Ok;
        mgr.seed(disk);
        Arc::new(mgr)
    }

    /// Test-only [`LedControl`] that records the last `(serial, state)` it was
    /// asked to drive instead of shelling out to `ledctl`. This keeps the
    /// service-level tests free of real hardware while still asserting the
    /// service drives the LED at the right moments.
    #[derive(Default)]
    struct RecordingLed {
        last: Mutex<Option<(String, LedState)>>,
    }

    impl Provider for RecordingLed {
        fn name(&self) -> &str {
            "recording"
        }
    }

    #[async_trait]
    impl LedControl for RecordingLed {
        async fn set_led(&self, disk: &PhysicalDisk, state: LedState) -> Result<()> {
            *self.last.lock() = Some((disk.serial.clone(), state));
            Ok(())
        }
    }

    #[tokio::test]
    async fn manager_lists_seeded_disk() {
        let mgr = seeded_manager();
        let disks = mgr.list(&machine()).await.expect("list");
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].serial, "SERIAL-ABC");
    }

    #[tokio::test]
    async fn mark_rma_sets_date_and_health() {
        let mgr = seeded_manager();
        mgr.mark_rma("SERIAL-ABC").await.expect("rma");
        let disks = mgr.list(&machine()).await.expect("list");
        assert!(disks[0].is_rma());
        assert_eq!(disks[0].health, DiskHealth::Failing);
    }

    #[tokio::test]
    async fn service_tracks_first_seen_by_serial() {
        let mgr = seeded_manager();
        let svc = DiskService::new(mgr, Arc::new(RecordingLed::default()));

        let first = svc.list(&machine()).await.expect("list");
        let original_first_seen = first[0].first_seen;

        // A later listing must preserve the original first_seen for the serial.
        let again = svc.list(&machine()).await.expect("list again");
        assert_eq!(again[0].first_seen, original_first_seen);

        let record = svc.record("SERIAL-ABC").expect("record");
        assert_eq!(record.first_seen, original_first_seen);
        assert_eq!(record.last_machine, Some(machine()));
    }

    #[tokio::test]
    async fn service_mark_rma_records_date_and_lights_led() {
        let mgr = seeded_manager();
        let led = Arc::new(RecordingLed::default());
        let svc = DiskService::new(mgr, led.clone());

        svc.list(&machine()).await.expect("list"); // learn the disk's location
        svc.mark_rma("SERIAL-ABC").await.expect("rma");

        let record = svc.record("SERIAL-ABC").expect("record");
        assert!(record.rma_date.is_some());

        // The fault LED was driven for the right serial.
        assert_eq!(
            led.last.lock().clone(),
            Some(("SERIAL-ABC".to_string(), LedState::Fault))
        );

        // Listing reflects the RMA date projected from the store.
        let disks = svc.list(&machine()).await.expect("list");
        assert!(disks[0].rma_date.is_some());
    }

    #[tokio::test]
    async fn set_led_unknown_disk_is_not_found() {
        let mgr = seeded_manager();
        let svc = DiskService::new(mgr, Arc::new(RecordingLed::default()));
        // Never listed, so location is unknown.
        assert!(svc.set_led("SERIAL-ABC", LedState::Locate).await.is_err());
    }

    #[test]
    fn builtins_register() {
        let mut mgr_reg: Registry<dyn DiskManager> = Registry::new();
        register_builtins(&mut mgr_reg).expect("disk builtins");
        assert!(mgr_reg.contains("sysfs"));

        let mut led_reg: Registry<dyn LedControl> = Registry::new();
        register_led_builtins(&mut led_reg).expect("led builtins");
        assert!(led_reg.contains("ledctl"));
    }
}
