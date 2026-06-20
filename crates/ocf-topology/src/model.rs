//! The topology resource model: `region → datacenter → rack → machine`.

use ocf_core::prelude::*;

/// A geographic region — the coarsest grouping in the fleet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub metadata: Metadata,
    /// Free-form geographic hint, e.g. `"us-east"`.
    pub locality: String,
}

impl Region {
    pub fn new(name: impl Into<String>) -> Self {
        Region {
            metadata: Metadata::named(name),
            locality: String::new(),
        }
    }

    /// The scope rooted at this region.
    pub fn scope(&self) -> Scope {
        Scope::region(self.metadata.id.clone())
    }
}

impl Resource for Region {
    fn kind(&self) -> &'static str {
        "region"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// A datacenter inside a region.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Datacenter {
    pub metadata: Metadata,
    pub region_id: Id,
    pub address: String,
}

impl Datacenter {
    pub fn new(region_id: Id, name: impl Into<String>) -> Self {
        Datacenter {
            metadata: Metadata::named(name),
            region_id,
            address: String::new(),
        }
    }

    pub fn scope(&self) -> Scope {
        Scope::datacenter(self.region_id.clone(), self.metadata.id.clone())
    }
}

impl Resource for Datacenter {
    fn kind(&self) -> &'static str {
        "datacenter"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// A rack inside a datacenter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rack {
    pub metadata: Metadata,
    pub region_id: Id,
    pub datacenter_id: Id,
    /// Number of rack units (e.g. 42U).
    pub units: u16,
}

impl Rack {
    pub fn new(region_id: Id, datacenter_id: Id, name: impl Into<String>) -> Self {
        Rack {
            metadata: Metadata::named(name),
            region_id,
            datacenter_id,
            units: 42,
        }
    }

    pub fn scope(&self) -> Scope {
        Scope::rack(
            self.region_id.clone(),
            self.datacenter_id.clone(),
            self.metadata.id.clone(),
        )
    }
}

impl Resource for Rack {
    fn kind(&self) -> &'static str {
        "rack"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// A physical (or virtual) machine — a node that actually runs workloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Machine {
    pub metadata: Metadata,
    pub region_id: Id,
    pub datacenter_id: Id,
    pub rack_id: Id,
    /// Rack unit position (1-based) where this machine is mounted.
    pub rack_position: Option<u16>,
    /// Reachable fabric address (host-to-host mesh endpoint).
    pub fabric_address: Option<String>,
    /// Total capacity advertised by this machine.
    pub capacity: ResourceSpec,
    pub state: LifecycleState,
    pub health: Health,
}

impl Machine {
    pub fn new(region_id: Id, datacenter_id: Id, rack_id: Id, name: impl Into<String>) -> Self {
        Machine {
            metadata: Metadata::named(name),
            region_id,
            datacenter_id,
            rack_id,
            rack_position: None,
            fabric_address: None,
            capacity: ResourceSpec::default(),
            state: LifecycleState::Pending,
            health: Health::Unknown,
        }
    }

    /// The fully-qualified scope identifying exactly this machine.
    pub fn scope(&self) -> Scope {
        Scope::machine(
            self.region_id.clone(),
            self.datacenter_id.clone(),
            self.rack_id.clone(),
            self.metadata.id.clone(),
        )
    }
}

impl Resource for Machine {
    fn kind(&self) -> &'static str {
        "machine"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}
