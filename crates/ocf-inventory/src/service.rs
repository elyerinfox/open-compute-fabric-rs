//! High-level inventory service with first-seen tracking.

use crate::collector::InventoryCollector;
use crate::component::MachineInventory;
use ocf_core::prelude::*;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Stores per-machine inventories and remembers when each component serial was
/// first observed across the whole fleet.
///
/// The `first_seen` ledger is the point of the service: a collector stamps a
/// fresh timestamp on every scan, but a part that has been seen before should
/// keep its *original* timestamp. On each [`record`](InventoryService::record)
/// the service rewrites each component's `first_seen` to the remembered value
/// (inserting the current time the first time a serial appears). This makes a
/// newly-installed part stand out and gives every part an accurate age.
pub struct InventoryService {
    /// Latest inventory per machine id.
    inventories: RwLock<HashMap<Id, MachineInventory>>,
    /// First time each component serial was ever observed.
    first_seen: RwLock<HashMap<String, DateTime<Utc>>>,
}

impl InventoryService {
    pub fn new() -> Self {
        InventoryService {
            inventories: RwLock::new(HashMap::new()),
            first_seen: RwLock::new(HashMap::new()),
        }
    }

    /// Store an inventory, reconciling every component's `first_seen` against
    /// the fleet-wide ledger, and return the reconciled inventory.
    pub fn record(&self, mut inventory: MachineInventory) -> MachineInventory {
        {
            let mut ledger = self.first_seen.write();
            let now = Utc::now();
            for component in inventory.components.iter_mut() {
                let seen = ledger
                    .entry(component.serial.clone())
                    .or_insert_with(|| component.first_seen.min(now));
                component.first_seen = *seen;
            }
        }
        self.inventories
            .write()
            .insert(inventory.machine_id.clone(), inventory.clone());
        inventory
    }

    /// Collect a machine's inventory through `collector` and record it,
    /// returning the reconciled result.
    pub async fn collect_and_record(
        &self,
        collector: &Arc<dyn InventoryCollector>,
        machine_id: &Id,
    ) -> Result<MachineInventory> {
        let inventory = collector.collect(machine_id).await?;
        Ok(self.record(inventory))
    }

    /// The most recently recorded inventory for `machine_id`.
    pub fn get(&self, machine_id: &Id) -> Result<MachineInventory> {
        self.inventories
            .read()
            .get(machine_id)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("inventory for machine {machine_id}")))
    }

    /// Every recorded machine inventory.
    pub fn list(&self) -> Vec<MachineInventory> {
        self.inventories.read().values().cloned().collect()
    }

    /// When the component with `serial` was first observed, if ever.
    pub fn first_seen(&self, serial: &str) -> Option<DateTime<Utc>> {
        self.first_seen.read().get(serial).copied()
    }
}

impl Default for InventoryService {
    fn default() -> Self {
        Self::new()
    }
}
