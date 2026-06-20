//! Health and lifecycle vocabulary shared by all stateful resources.

use serde::{Deserialize, Serialize};

/// Coarse health signal used by monitoring, the API, and the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Unknown,
    Healthy,
    Degraded,
    Unhealthy,
}

impl Default for Health {
    fn default() -> Self {
        Health::Unknown
    }
}

/// Generic lifecycle of a provisioned resource (a workload, a load balancer,
/// a network, ...). Not every resource visits every state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Pending,
    Provisioning,
    Running,
    Paused,
    Stopping,
    Stopped,
    Migrating,
    Failed,
    Terminated,
}

impl LifecycleState {
    /// Whether the resource is doing useful work right now.
    pub fn is_active(&self) -> bool {
        matches!(self, LifecycleState::Running | LifecycleState::Migrating)
    }

    /// Whether the resource has reached a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, LifecycleState::Terminated | LifecycleState::Failed)
    }
}
