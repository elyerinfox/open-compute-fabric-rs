//! Fleet membership and drop-out handling.
//!
//! This is the live answer to "how do we join, stay available, and react when a
//! node drops out". On boot every machine is registered into the
//! [`Membership`](ocf_fabric::Membership) detector and the mesh. A background
//! task ticks the detector; when a peer goes silent past its timeouts it is
//! suspected and then declared dead, and [`FabricController::handle_node_dead`]
//! reschedules that node's **highly-available** workloads onto a surviving node
//! within their placement scope — and stops the rest.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::Utc;
use ocf_core::prelude::*;
use ocf_fabric::{Liveness, MembershipEvent, NodeId};
use ocf_topology::Machine;

use crate::controller::{node_for_machine, FabricController};

impl FabricController {
    /// Register every topology machine into the membership detector and the
    /// mesh as an alive peer.
    pub async fn init_membership(&self) -> Result<()> {
        for machine in self.topology.store().all_machines().await? {
            let node = node_for_machine(&machine);
            self.membership.join(node.clone());
            self.fabric.join(node)?;
        }
        tracing::info!(
            members = self.membership.members().len(),
            "membership initialized"
        );
        Ok(())
    }

    /// Record a heartbeat for the member backing `machine_id`. Returns whether a
    /// previously-suspected node was revived.
    pub fn heartbeat_machine(&self, machine_id: &Id) -> bool {
        match self.membership.member_for_machine(machine_id) {
            Some(member) => self.membership.heartbeat(&member.node.node_id).is_some(),
            None => false,
        }
    }

    /// Force a node to be considered failed (operator action / hard signal), and
    /// immediately run drop-out handling. Returns the rescheduled workloads.
    pub async fn fail_machine(&self, machine_id: &Id) -> Result<Vec<String>> {
        let member = self
            .membership
            .member_for_machine(machine_id)
            .ok_or_else(|| Error::not_found(format!("member for machine {machine_id}")))?;
        self.membership.force_dead(&member.node.node_id);
        let moved = self.handle_node_dead(machine_id).await?;
        let _ = self.persist().await;
        Ok(moved)
    }

    /// Reschedule the dead node's highly-available workloads onto a surviving,
    /// in-scope node; stop the rest. Returns a human-readable list of moves.
    pub async fn handle_node_dead(&self, dead_machine: &Id) -> Result<Vec<String>> {
        let machines = self.topology.store().all_machines().await?;
        let alive: Vec<Id> = self
            .membership
            .alive()
            .into_iter()
            .filter_map(|n| n.machine_id)
            .filter(|id| id != dead_machine)
            .collect();

        let mut moves = Vec::new();
        for provider in self.runtimes.all() {
            let workloads = provider.list().await.unwrap_or_default();
            for wl in workloads {
                if wl.node.as_ref() != Some(dead_machine) {
                    continue;
                }
                if wl.highly_available {
                    match pick_target(&wl.placement, &alive, &machines) {
                        Some(target) => {
                            let mut moved = wl.clone();
                            moved.node = Some(target.clone());
                            let id = wl.metadata.id.clone();
                            // Re-place on the surviving node (delete + recreate
                            // models the live migration the Migrator performs).
                            provider.delete(&id).await?;
                            provider.create(&moved).await?;
                            provider.start(&moved.metadata.id).await?;
                            tracing::warn!(
                                workload = %wl.metadata.name,
                                from = %dead_machine,
                                to = %target,
                                "rescheduled HA workload off dead node"
                            );
                            moves.push(format!("{} -> {}", wl.metadata.name, target));
                        }
                        None => {
                            tracing::error!(
                                workload = %wl.metadata.name,
                                "no in-scope surviving node to reschedule HA workload"
                            );
                        }
                    }
                } else {
                    // Non-HA workloads are simply lost with the node.
                    provider.stop(&wl.metadata.id).await.ok();
                    tracing::warn!(
                        workload = %wl.metadata.name,
                        "non-HA workload lost with dead node"
                    );
                }
            }
        }
        Ok(moves)
    }

    /// Run the failure detector forever, ticking every two seconds and acting on
    /// transitions. Spawn this on a task; it never returns.
    pub async fn run_failure_detector(self: Arc<Self>) {
        let mut interval = tokio::time::interval(StdDuration::from_secs(2));
        loop {
            interval.tick().await;
            // In a multi-node fleet each peer's ocfd heartbeats this node over the
            // encrypted transport and those refreshes keep it Alive; a peer that
            // goes silent then ages Alive -> Suspect -> Dead via the state machine
            // below. In this single-process deployment the seeded machines have no
            // live agents, so currently-alive members are refreshed here and
            // failures are injected through POST /fabric/machines/:id/fail.
            for node in self.membership.alive() {
                self.membership.heartbeat(&node.node_id);
            }
            for event in self.membership.tick(Utc::now()) {
                match event {
                    MembershipEvent::Suspected(id) => {
                        tracing::warn!(node = %id, "peer suspected (heartbeats stalled)");
                    }
                    MembershipEvent::Died(id) => {
                        tracing::error!(node = %id, "peer declared dead");
                        self.on_node_dead(&id).await;
                    }
                    _ => {}
                }
            }
        }
    }

    /// Map a dead node id back to its machine and run drop-out handling.
    async fn on_node_dead(&self, node_id: &NodeId) {
        let machine_id = self
            .membership
            .members()
            .into_iter()
            .find(|m| &m.node.node_id == node_id)
            .and_then(|m| m.node.machine_id);
        if let Some(machine_id) = machine_id {
            match self.handle_node_dead(&machine_id).await {
                Ok(moves) if !moves.is_empty() => {
                    tracing::info!(?moves, "rescheduled HA workloads after node death");
                    let _ = self.persist().await;
                }
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "drop-out handling failed"),
            }
        }
    }
}

/// Choose a surviving machine that satisfies a workload's placement scope.
///
/// A workload with no `placement` may land on any alive node; a scoped workload
/// may only land where its placement [`Scope`] contains the candidate machine —
/// exactly the migration restriction the fabric promises.
fn pick_target(
    placement: &Option<Scope>,
    alive: &[Id],
    machines: &[Machine],
) -> Option<Id> {
    alive.iter().find(|id| {
        let Some(machine) = machines.iter().find(|m| &m.metadata.id == *id) else {
            return false;
        };
        match placement {
            None => true,
            Some(scope) => scope.contains(&machine.scope()),
        }
    }).cloned()
}

/// A lightweight view of one member for the API.
#[derive(Serialize)]
pub struct MemberView {
    pub node_id: String,
    pub machine_id: Option<String>,
    pub liveness: Liveness,
    pub last_heartbeat: String,
}

impl FabricController {
    /// Snapshot the membership table for the API.
    pub fn membership_view(&self) -> Vec<MemberView> {
        self.membership
            .members()
            .into_iter()
            .map(|m| MemberView {
                node_id: m.node.node_id.to_string(),
                machine_id: m.node.machine_id.map(|id| id.to_string()),
                liveness: m.liveness,
                last_heartbeat: m.last_heartbeat.to_rfc3339(),
            })
            .collect()
    }
}
