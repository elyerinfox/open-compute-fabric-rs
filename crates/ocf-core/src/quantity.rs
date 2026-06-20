//! Compute-resource quantities used by scheduling, quotas, and metrics.

use serde::{Deserialize, Serialize};

/// A request or limit for the fundamental compute resources.
///
/// CPU is expressed in **millicores** (1000 = one core) and memory/disk in
/// **bytes**, mirroring the conventions used by container runtimes so the
/// translation to a backend is lossless.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceSpec {
    /// CPU in millicores (1000 = 1 vCPU).
    #[serde(default)]
    pub cpu_millis: u64,
    /// Memory in bytes.
    #[serde(default)]
    pub memory_bytes: u64,
    /// Ephemeral/root disk in bytes.
    #[serde(default)]
    pub disk_bytes: u64,
}

impl ResourceSpec {
    pub fn new(cpu_millis: u64, memory_bytes: u64, disk_bytes: u64) -> Self {
        ResourceSpec {
            cpu_millis,
            memory_bytes,
            disk_bytes,
        }
    }

    /// Whether `self` fits within `available` on every dimension.
    pub fn fits_in(&self, available: &ResourceSpec) -> bool {
        self.cpu_millis <= available.cpu_millis
            && self.memory_bytes <= available.memory_bytes
            && self.disk_bytes <= available.disk_bytes
    }

    pub fn saturating_sub(&self, used: &ResourceSpec) -> ResourceSpec {
        ResourceSpec {
            cpu_millis: self.cpu_millis.saturating_sub(used.cpu_millis),
            memory_bytes: self.memory_bytes.saturating_sub(used.memory_bytes),
            disk_bytes: self.disk_bytes.saturating_sub(used.disk_bytes),
        }
    }
}
