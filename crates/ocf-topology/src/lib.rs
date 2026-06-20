//! # ocf-topology
//!
//! The fleet's structural model and a service to navigate it.
//!
//! The fleet is a tree: `region → datacenter → rack → machine`. This crate
//! provides the resource types ([`model`]), a pluggable persistence contract
//! ([`store::TopologyStore`]) with an in-memory backend, and a
//! [`TopologyService`] that assembles the tree for the frontend's drill-down
//! view.

pub mod model;
pub mod store;

pub use model::{Datacenter, Machine, Rack, Region};
pub use store::{InMemoryTopology, TopologyStore};

use ocf_core::prelude::*;
use std::sync::Arc;

/// A materialized subtree used by the UI to drill down into the fleet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyTree {
    pub regions: Vec<RegionNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionNode {
    pub region: Region,
    pub datacenters: Vec<DatacenterNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatacenterNode {
    pub datacenter: Datacenter,
    pub racks: Vec<RackNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RackNode {
    pub rack: Rack,
    pub machines: Vec<Machine>,
}

/// High-level operations over a [`TopologyStore`].
pub struct TopologyService {
    store: Arc<dyn TopologyStore>,
}

impl TopologyService {
    pub fn new(store: Arc<dyn TopologyStore>) -> Self {
        TopologyService { store }
    }

    pub fn store(&self) -> &Arc<dyn TopologyStore> {
        &self.store
    }

    /// Build the full topology tree for the drill-down view.
    pub async fn tree(&self) -> Result<TopologyTree> {
        let mut regions = Vec::new();
        for region in self.store.list_regions().await? {
            let mut datacenters = Vec::new();
            for dc in self.store.list_datacenters(&region.metadata.id).await? {
                let mut racks = Vec::new();
                for rack in self.store.list_racks(&dc.metadata.id).await? {
                    let machines = self.store.list_machines(&rack.metadata.id).await?;
                    racks.push(RackNode { rack, machines });
                }
                datacenters.push(DatacenterNode {
                    datacenter: dc,
                    racks,
                });
            }
            regions.push(RegionNode {
                region,
                datacenters,
            });
        }
        Ok(TopologyTree { regions })
    }

    /// Resolve the full [`Scope`] that locates a machine within the fleet,
    /// used by placement/authorization checks.
    pub async fn machine_scope(&self, machine_id: &Id) -> Result<Scope> {
        let m = self.store.get_machine(machine_id).await?;
        Ok(m.scope())
    }
}
