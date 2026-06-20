//! The topology persistence contract and an in-memory implementation.

use crate::model::{Datacenter, Machine, Rack, Region};
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;

/// Pluggable persistence contract for the fleet topology.
///
/// The default backend is in-memory; a production deployment swaps in an
/// etcd/Postgres-backed implementation without touching callers.
#[async_trait]
pub trait TopologyStore: Send + Sync {
    async fn put_region(&self, region: Region) -> Result<()>;
    async fn get_region(&self, id: &Id) -> Result<Region>;
    async fn list_regions(&self) -> Result<Vec<Region>>;

    async fn put_datacenter(&self, dc: Datacenter) -> Result<()>;
    async fn list_datacenters(&self, region_id: &Id) -> Result<Vec<Datacenter>>;

    async fn put_rack(&self, rack: Rack) -> Result<()>;
    async fn list_racks(&self, datacenter_id: &Id) -> Result<Vec<Rack>>;

    async fn put_machine(&self, machine: Machine) -> Result<()>;
    async fn get_machine(&self, id: &Id) -> Result<Machine>;
    async fn list_machines(&self, rack_id: &Id) -> Result<Vec<Machine>>;
    async fn all_machines(&self) -> Result<Vec<Machine>>;
}

/// A simple thread-safe, in-memory topology store. Suitable for a single-node
/// controller and for tests.
#[derive(Default)]
pub struct InMemoryTopology {
    regions: RwLock<HashMap<Id, Region>>,
    datacenters: RwLock<HashMap<Id, Datacenter>>,
    racks: RwLock<HashMap<Id, Rack>>,
    machines: RwLock<HashMap<Id, Machine>>,
}

impl InMemoryTopology {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl TopologyStore for InMemoryTopology {
    async fn put_region(&self, region: Region) -> Result<()> {
        self.regions
            .write()
            .insert(region.metadata.id.clone(), region);
        Ok(())
    }

    async fn get_region(&self, id: &Id) -> Result<Region> {
        self.regions
            .read()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("region {id}")))
    }

    async fn list_regions(&self) -> Result<Vec<Region>> {
        Ok(self.regions.read().values().cloned().collect())
    }

    async fn put_datacenter(&self, dc: Datacenter) -> Result<()> {
        self.datacenters
            .write()
            .insert(dc.metadata.id.clone(), dc);
        Ok(())
    }

    async fn list_datacenters(&self, region_id: &Id) -> Result<Vec<Datacenter>> {
        Ok(self
            .datacenters
            .read()
            .values()
            .filter(|d| &d.region_id == region_id)
            .cloned()
            .collect())
    }

    async fn put_rack(&self, rack: Rack) -> Result<()> {
        self.racks.write().insert(rack.metadata.id.clone(), rack);
        Ok(())
    }

    async fn list_racks(&self, datacenter_id: &Id) -> Result<Vec<Rack>> {
        Ok(self
            .racks
            .read()
            .values()
            .filter(|r| &r.datacenter_id == datacenter_id)
            .cloned()
            .collect())
    }

    async fn put_machine(&self, machine: Machine) -> Result<()> {
        self.machines
            .write()
            .insert(machine.metadata.id.clone(), machine);
        Ok(())
    }

    async fn get_machine(&self, id: &Id) -> Result<Machine> {
        self.machines
            .read()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("machine {id}")))
    }

    async fn list_machines(&self, rack_id: &Id) -> Result<Vec<Machine>> {
        Ok(self
            .machines
            .read()
            .values()
            .filter(|m| &m.rack_id == rack_id)
            .cloned()
            .collect())
    }

    async fn all_machines(&self) -> Result<Vec<Machine>> {
        Ok(self.machines.read().values().cloned().collect())
    }
}
