//! # ocf-inventory
//!
//! Hardware inventory and out-of-band machine control.
//!
//! This crate answers two questions about the metal: *what is in this machine?*
//! and *how do I power and read it without the host OS?*
//!
//! * [`InventoryCollector`] discovers a machine's hardware (modeled as
//!   [`HardwareComponent`]s categorized by [`ComponentKind`]) and returns a
//!   [`MachineInventory`]. The built-in [`DmiInventoryCollector`] parses
//!   SMBIOS/DMI (`dmidecode`) plus Linux sysfs/procfs.
//! * [`IpmiController`] drives a machine's BMC over IPMI to query/set chassis
//!   power and read [`Sensor`]s. The built-in [`LanplusIpmi`] shells out to
//!   `ipmitool -I lanplus`. **IPMI only works when the caller is on the same
//!   physical management network as the target BMC** — see the [`ipmi`] module
//!   docs.
//! * [`InventoryService`] keeps the latest inventory per machine and a
//!   fleet-wide ledger of when each component serial was `first_seen`, so a
//!   newly-installed part is detectable and every part has an accurate age.
//!
//! Both contracts extend [`Provider`] and ship `register_builtins`, so backends
//! are swappable through a [`Registry`].
//!
//! [`Provider`]: ocf_core::registry::Provider
//! [`Registry`]: ocf_core::registry::Registry

pub mod collector;
pub mod component;
pub mod exec;
pub mod ipmi;
pub mod service;

pub use collector::{DmiInventoryCollector, InventoryCollector};
pub use component::{ComponentKind, HardwareComponent, MachineInventory};
pub use ipmi::{IpmiController, IpmiTarget, LanplusIpmi, PowerState, Sensor};
pub use service::InventoryService;

use ocf_core::prelude::*;

/// Register the default inventory collectors **and** IPMI controllers.
///
/// A convenience over the per-module `register_builtins` for wiring code that
/// just wants every default backend in both registries.
pub fn register_builtins(
    collectors: &mut Registry<dyn InventoryCollector>,
    controllers: &mut Registry<dyn IpmiController>,
) -> Result<()> {
    collector::register_builtins(collectors)?;
    ipmi::register_builtins(controllers)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // The collector degrades gracefully off-Linux: hardware-specific sources
    // (dmidecode, /proc, /sys) yield no components, but the baseboard identity
    // is always synthesized, so the inventory is never empty. These tests assert
    // only that host-independent invariant; per-source parsing is unit-tested
    // with fixtures in `collector` / `ipmi`, and real-host behavior lives behind
    // `#[ignore]` tests there.

    #[tokio::test]
    async fn dmi_collector_always_reports_baseboard_identity() {
        let collector = DmiInventoryCollector::new();
        let machine = Id::named("machine-1");
        let inv = collector.collect(&machine).await.expect("collect");
        assert_eq!(inv.machine_id, machine);
        // Baseboard is always present (its serial is the chassis identity).
        assert_eq!(inv.components_of(ComponentKind::Baseboard).count(), 1);
        let baseboard = inv
            .components_of(ComponentKind::Baseboard)
            .next()
            .expect("baseboard");
        assert_eq!(baseboard.serial, inv.baseboard_serial);
    }

    #[tokio::test]
    async fn service_preserves_first_seen_across_rescans() {
        let svc = InventoryService::new();
        let collector: Arc<dyn InventoryCollector> = Arc::new(DmiInventoryCollector::new());
        let machine = Id::named("machine-1");

        let first = svc
            .collect_and_record(&collector, &machine)
            .await
            .expect("first scan");
        // The baseboard is the one component guaranteed on every host.
        let serial = first.components[0].serial.clone();
        let first_seen = svc.first_seen(&serial).expect("ledger entry");

        // A later scan must not move the remembered first_seen timestamp.
        let second = svc
            .collect_and_record(&collector, &machine)
            .await
            .expect("second scan");
        assert_eq!(second.components[0].first_seen, first_seen);
        assert_eq!(svc.first_seen(&serial), Some(first_seen));
    }

    #[test]
    fn register_builtins_registers_defaults() {
        let mut collectors = Registry::<dyn InventoryCollector>::new();
        let mut controllers = Registry::<dyn IpmiController>::new();
        register_builtins(&mut collectors, &mut controllers).expect("register");
        assert!(collectors.contains("dmi"));
        assert!(controllers.contains("lanplus"));
    }
}
