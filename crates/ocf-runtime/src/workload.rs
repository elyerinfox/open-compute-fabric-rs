//! The runtime resource model: workloads and their memory snapshots.

use ocf_core::prelude::*;
use std::collections::BTreeMap;

/// What kind of execution sandbox a workload runs in.
///
/// The distinction matters to placement and migration: only
/// [`RuntimeKind::VirtualMachine`] backends in this crate model live memory
/// migration, and only [`RuntimeKind::Container`] workloads are eligible for
/// horizontal autoscaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    /// An OS-level container (Docker, Podman, LXC, ...).
    Container,
    /// A full virtual machine (QEMU/KVM, ...).
    VirtualMachine,
}

impl RuntimeKind {
    /// A stable lowercase discriminator, handy for logs and labels.
    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeKind::Container => "container",
            RuntimeKind::VirtualMachine => "virtual_machine",
        }
    }
}

/// A unit of compute the fabric schedules onto a node.
///
/// A `Workload` is the runtime's primary [`Resource`]. It is backend-agnostic:
/// the same struct describes a Docker container or a QEMU VM, and a concrete
/// [`crate::provider::RuntimeProvider`] turns it into a real container/VM by
/// driving the backing tool. `placement`, when set, bounds both where the
/// workload may run and where it may migrate, per the fleet [`Scope`] semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workload {
    pub metadata: Metadata,
    /// Whether this runs as a container or a virtual machine.
    pub kind: RuntimeKind,
    /// The backing image/template reference (e.g. `"nginx:1.27"`).
    pub image: String,
    /// Requested compute resources.
    pub resources: ResourceSpec,
    /// Current lifecycle position.
    pub state: LifecycleState,
    /// The node this workload is currently placed on, if scheduled.
    pub node: Option<Id>,
    /// Whether the fabric should keep this workload running across node loss
    /// (eligible for live migration to a surviving node within `placement`).
    pub highly_available: bool,
    /// Optional placement restriction. `None` means the whole fleet is fair
    /// game; a set [`Scope`] confines both initial placement and migration.
    pub placement: Option<Scope>,
    /// Environment variables injected into the workload.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl Workload {
    /// Create a new container workload from an image reference.
    pub fn container(name: impl Into<String>, image: impl Into<String>) -> Self {
        Workload {
            metadata: Metadata::new(name),
            kind: RuntimeKind::Container,
            image: image.into(),
            resources: ResourceSpec::default(),
            state: LifecycleState::Pending,
            node: None,
            highly_available: false,
            placement: None,
            env: BTreeMap::new(),
        }
    }

    /// Create a new virtual-machine workload from a template/image reference.
    pub fn virtual_machine(name: impl Into<String>, image: impl Into<String>) -> Self {
        Workload {
            metadata: Metadata::new(name),
            kind: RuntimeKind::VirtualMachine,
            image: image.into(),
            resources: ResourceSpec::default(),
            state: LifecycleState::Pending,
            node: None,
            highly_available: false,
            placement: None,
            env: BTreeMap::new(),
        }
    }

    /// Builder: set the requested compute resources.
    pub fn with_resources(mut self, resources: ResourceSpec) -> Self {
        self.resources = resources;
        self
    }

    /// Builder: place the workload on a specific node.
    pub fn on_node(mut self, node: impl Into<Id>) -> Self {
        self.node = Some(node.into());
        self
    }

    /// Builder: confine the workload to a placement [`Scope`].
    pub fn within(mut self, placement: Scope) -> Self {
        self.placement = Some(placement);
        self
    }

    /// Builder: mark the workload as highly available (migration-eligible).
    pub fn highly_available(mut self, ha: bool) -> Self {
        self.highly_available = ha;
        self
    }

    /// Whether the workload's `placement` scope permits running at `target`.
    ///
    /// An unscoped workload (`placement == None`) may run anywhere; a scoped
    /// one may only run where its scope `contains` the target node's scope.
    pub fn permits_placement(&self, target: &Scope) -> bool {
        match &self.placement {
            None => true,
            Some(scope) => scope.contains(target),
        }
    }
}

impl Resource for Workload {
    fn kind(&self) -> &'static str {
        "workload"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// A handle to a captured workload memory image.
///
/// This is deliberately *not* the memory itself: `blob_ref` points at where a
/// real checkpoint (CRIU image, QEMU `savevm` blob, ...) would live, and
/// `bytes_len` records its size. The migration path moves this handle between
/// providers; an honest backend would also ship the referenced bytes over the
/// fabric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySnapshot {
    /// The workload this snapshot belongs to.
    pub workload_id: Id,
    /// The node the snapshot was captured on.
    pub node: Option<Id>,
    /// Size of the (notional) checkpoint blob in bytes.
    pub bytes_len: u64,
    /// An opaque reference to where the checkpoint blob lives.
    pub blob_ref: String,
}

impl MemorySnapshot {
    /// Build a fresh snapshot handle with a unique blob reference.
    pub fn new(workload_id: Id, node: Option<Id>, bytes_len: u64) -> Self {
        let blob_ref = format!("ocf-snapshot://{}", uuid::Uuid::new_v4());
        MemorySnapshot {
            workload_id,
            node,
            bytes_len,
            blob_ref,
        }
    }
}
