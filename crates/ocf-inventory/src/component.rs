//! The hardware inventory model: components and a per-machine inventory.

use ocf_core::prelude::*;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;

/// The category of a discovered hardware component.
///
/// `Other` is the escape hatch for anything the collector recognizes but the
/// model does not yet name explicitly (TPMs, BMCs, fans, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Cpu,
    MemoryModule,
    Nic,
    Disk,
    Psu,
    Baseboard,
    Gpu,
    Other,
}

/// A single discovered piece of hardware.
///
/// `serial` is the natural key the [`crate::service::InventoryService`] uses to
/// track when a component was `first_seen`: a part keeps its original
/// `first_seen` timestamp across re-scans even as other attributes change, so a
/// replaced part is detected as new.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareComponent {
    pub kind: ComponentKind,
    pub vendor: String,
    pub model: String,
    pub serial: String,
    /// When this exact part (by serial) was first observed in the fleet.
    pub first_seen: DateTime<Utc>,
    /// Free-form, collector-specific facts (capacity, speed, firmware, ...).
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

impl HardwareComponent {
    /// Build a component, stamping `first_seen` with the current time. The
    /// service overwrites this with the remembered timestamp for known serials.
    pub fn new(
        kind: ComponentKind,
        vendor: impl Into<String>,
        model: impl Into<String>,
        serial: impl Into<String>,
    ) -> Self {
        HardwareComponent {
            kind,
            vendor: vendor.into(),
            model: model.into(),
            serial: serial.into(),
            first_seen: Utc::now(),
            attributes: BTreeMap::new(),
        }
    }

    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }
}

/// The full hardware inventory of a single machine.
///
/// `machine_id` ties the inventory back to a `ocf-topology` machine, while
/// `baseboard_serial` is the stable hardware identity of the chassis itself
/// (useful when a machine record is re-created but the metal is the same).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineInventory {
    pub metadata: Metadata,
    pub machine_id: Id,
    pub baseboard_serial: String,
    #[serde(default)]
    pub components: Vec<HardwareComponent>,
}

impl MachineInventory {
    pub fn new(machine_id: Id, baseboard_serial: impl Into<String>) -> Self {
        let baseboard_serial = baseboard_serial.into();
        MachineInventory {
            metadata: Metadata::named(format!("inventory-{baseboard_serial}")),
            machine_id,
            baseboard_serial,
            components: Vec::new(),
        }
    }

    /// All components of a given kind.
    pub fn components_of(&self, kind: ComponentKind) -> impl Iterator<Item = &HardwareComponent> {
        self.components.iter().filter(move |c| c.kind == kind)
    }

    /// Total number of discovered components.
    pub fn component_count(&self) -> usize {
        self.components.len()
    }
}

impl Resource for MachineInventory {
    fn kind(&self) -> &'static str {
        "machine_inventory"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}
