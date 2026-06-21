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
use ocf_fabric::{
    plan_route, FabricNode, FabricServer, KeyPair, Liveness, MembershipEvent, NoiseTransport,
    NodeId, Reachability, RoutePlan,
};
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

    /// The machines that satisfy a workload's placement constraints — scope,
    /// required node capabilities (`node_selector`), and capacity. This is the
    /// candidate set a scheduler (or the operator) chooses from.
    pub async fn candidate_nodes(&self, workload: &ocf_runtime::Workload) -> Vec<Machine> {
        let machines = self
            .topology
            .store()
            .all_machines()
            .await
            .unwrap_or_default();
        machines
            .into_iter()
            .filter(|m| machine_satisfies(workload, m))
            .collect()
    }

    /// Choose a node for a workload: the first machine satisfying its scope,
    /// capability, and capacity constraints. `None` when nothing qualifies.
    pub async fn schedule(&self, workload: &ocf_runtime::Workload) -> Option<Id> {
        self.candidate_nodes(workload)
            .await
            .into_iter()
            .next()
            .map(|m| m.metadata.id)
    }

    /// This node's fabric keypair (its identity == its WireGuard/fabric key).
    fn node_keypair(&self) -> KeyPair {
        KeyPair::from_seed_name(&self.node_id)
    }

    /// The planned route from this node to every other member: direct when the
    /// peer is dialable, relayed (through the lowest-RTT relay) when it is
    /// private, weighed by measured RTT. This is the fabric's "fastest path"
    /// answer made observable.
    pub fn routes_view(&self) -> Vec<RouteView> {
        let self_id = self.node_keypair().node_id();
        let members = self.membership.members();
        let relays: Vec<FabricNode> = members
            .iter()
            .filter(|m| m.node.is_relay() && m.node.node_id != self_id)
            .map(|m| m.node.clone())
            .collect();
        let relay_refs: Vec<&FabricNode> = relays.iter().collect();
        members
            .iter()
            .filter(|m| m.node.node_id != self_id)
            .map(|m| {
                let plan = plan_route(&m.node, &relay_refs, |id| self.membership.rtt(id));
                let (route, via, cost_ms) = match plan {
                    RoutePlan::Direct { cost_ms, .. } => ("direct", None, Some(cost_ms)),
                    RoutePlan::Relayed { relay, cost_ms, .. } => {
                        ("relayed", Some(relay.to_string()), Some(cost_ms))
                    }
                    RoutePlan::Unreachable => ("unreachable", None, None),
                };
                RouteView {
                    target: m.node.node_id.to_string(),
                    machine_id: m.node.machine_id.as_ref().map(|id| id.to_string()),
                    reachability: reachability_str(m.node.reachability).into(),
                    route: route.into(),
                    via,
                    cost_ms,
                }
            })
            .collect()
    }

    /// The measured-latency view from this node: `node_id -> last RTT (ms)`.
    ///
    /// This is the bridge between the fabric's latency map and the **load
    /// balancer's `Latency` policy**: they operate at different layers (this is
    /// node↔node network distance; the LB distributes requests across a service's
    /// backends), but a backend's network latency *is* the measured RTT to the
    /// node hosting it. Stamping `Backend.latency_ms` from this map turns the LB's
    /// `Latency`/`Geo` policies from static numbers into real measurements, while
    /// `RoundRobin`/`LeastLoad` remain (correctly) latency-agnostic.
    pub fn latency_map(&self) -> std::collections::BTreeMap<String, f64> {
        self.membership
            .members()
            .into_iter()
            .filter_map(|m| m.rtt_ms.map(|r| (m.node.node_id.to_string(), r)))
            .collect()
    }

    /// Run the fabric **control channel**: a ping server that echoes, plus a
    /// prober that periodically times a round-trip to every alive peer and
    /// records the **measured RTT** in membership. This is the real latency view
    /// the load balancer's `Latency` policy and weighted routing consume. Spawn
    /// on a task. On a single-host deployment peers are unreachable, so RTT stays
    /// `None` — the mechanism is the same on a real fleet.
    pub async fn run_latency_services(self: Arc<Self>) {
        let port = self.config.fabric_control_port;
        match FabricServer::bind(("0.0.0.0", port), self.node_keypair()).await {
            Ok(srv) => {
                tokio::spawn(srv.run(|_pk, req| async move { req }));
            }
            Err(e) => {
                tracing::warn!(error = %e, "fabric control server bind failed; latency probing off");
            }
        }
        let client = NoiseTransport::with_keypair(self.node_keypair());
        let mut interval = tokio::time::interval(StdDuration::from_secs(5));
        loop {
            interval.tick().await;
            let machines = self
                .topology
                .store()
                .all_machines()
                .await
                .unwrap_or_default();
            for member in self.membership.members() {
                if !member.liveness.is_available() {
                    continue;
                }
                let Some(mid) = &member.node.machine_id else { continue };
                let Some(addr) = machines
                    .iter()
                    .find(|m| &m.metadata.id == mid)
                    .and_then(|m| m.fabric_address.clone())
                else {
                    continue;
                };
                let target = FabricNode::new(
                    member.node.node_id.clone(),
                    member.node.public_key.clone(),
                    vec![format!("{addr}:{port}")],
                );
                let t = std::time::Instant::now();
                if client.request(&target, b"ping").await.is_ok() {
                    let rtt = t.elapsed().as_secs_f64() * 1000.0;
                    self.membership.record_rtt(&member.node.node_id, rtt);
                }
            }
        }
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
                    match pick_target(&wl, &alive, &machines) {
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
    workload: &ocf_runtime::Workload,
    alive: &[Id],
    machines: &[Machine],
) -> Option<Id> {
    alive
        .iter()
        .find(|id| {
            let Some(machine) = machines.iter().find(|m| &m.metadata.id == *id) else {
                return false;
            };
            machine_satisfies(workload, machine)
        })
        .cloned()
}

/// Whether `machine` satisfies *all* of `workload`'s placement constraints: the
/// scope (`placement`), the required node capabilities (`node_selector` matched
/// against the machine's labels), and capacity (the request fits the machine).
pub(crate) fn machine_satisfies(workload: &ocf_runtime::Workload, machine: &Machine) -> bool {
    workload.permits_placement(&machine.scope())
        && machine.metadata.matches_labels(&workload.node_selector)
        && workload.resources.fits_in(&machine.capacity)
}

/// Render a [`Reachability`] as a stable lowercase string for the API.
fn reachability_str(r: Reachability) -> &'static str {
    match r {
        Reachability::Public => "public",
        Reachability::Private => "private",
        Reachability::Relay => "relay",
    }
}

/// The planned route from this node to one peer.
#[derive(Serialize)]
pub struct RouteView {
    pub target: String,
    pub machine_id: Option<String>,
    pub reachability: String,
    /// `"direct"`, `"relayed"`, or `"unreachable"`.
    pub route: String,
    /// The relay's node id when `route == "relayed"`.
    pub via: Option<String>,
    /// Estimated path cost in ms (`None` when unreachable).
    pub cost_ms: Option<f64>,
}

/// A lightweight view of one member for the API.
#[derive(Serialize)]
pub struct MemberView {
    pub node_id: String,
    pub machine_id: Option<String>,
    pub liveness: Liveness,
    /// How this peer is reached: `"public"`, `"private"`, or `"relay"`.
    pub reachability: String,
    /// Last measured round-trip latency to this peer in ms (`None` until probed).
    pub rtt_ms: Option<f64>,
    pub last_heartbeat: String,
}

/// One node's WireGuard peer entry, for the API.
#[derive(Serialize)]
pub struct WireguardPeerView {
    pub name: String,
    pub wg_ip: String,
    pub endpoint: Option<String>,
    pub public_key: String,
}

/// The computed WireGuard underlay mesh: this node and its peers. Lets you
/// inspect the encrypted-overlay wiring even on a host where the kernel
/// programming can't run.
#[derive(Serialize)]
pub struct WireguardView {
    pub iface: String,
    pub node: String,
    pub node_ip: String,
    pub public_key: String,
    pub vxlan_rides_wireguard: bool,
    pub peers: Vec<WireguardPeerView>,
}

impl FabricController {
    /// The computed WireGuard mesh (this node + peers, with their WG keys and
    /// underlay endpoints). VXLAN VTEPs point at these WireGuard addresses.
    pub async fn wireguard_status(&self) -> WireguardView {
        let plan = self.wireguard_plan().await;
        let my_kp = ocf_fabric::KeyPair::from_seed_name(&self.node_id);
        let node_ip = plan
            .iter()
            .find(|(_, name, _, _)| name == &self.node_id)
            .map(|(_, _, ip, _)| ip.clone())
            .unwrap_or_else(|| "10.255.0.254".to_string());
        let peers = plan
            .into_iter()
            .filter(|(_, name, _, _)| name != &self.node_id)
            .map(|(_, name, wg_ip, endpoint)| WireguardPeerView {
                public_key: ocf_fabric::KeyPair::from_seed_name(&name).public.to_wireguard_key(),
                name,
                wg_ip,
                endpoint,
            })
            .collect();
        WireguardView {
            iface: self.wireguard.iface().to_string(),
            node: self.node_id.clone(),
            node_ip,
            public_key: my_kp.public.to_wireguard_key(),
            vxlan_rides_wireguard: true,
            peers,
        }
    }

    /// Snapshot the membership table for the API.
    pub fn membership_view(&self) -> Vec<MemberView> {
        self.membership
            .members()
            .into_iter()
            .map(|m| MemberView {
                node_id: m.node.node_id.to_string(),
                machine_id: m.node.machine_id.map(|id| id.to_string()),
                liveness: m.liveness,
                reachability: reachability_str(m.node.reachability).into(),
                rtt_ms: m.rtt_ms,
                last_heartbeat: m.last_heartbeat.to_rfc3339(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::machine_satisfies;
    use ocf_core::prelude::*;
    use ocf_runtime::Workload;
    use ocf_topology::Machine;

    fn machine(name: &str, labels: &[(&str, &str)]) -> Machine {
        let mut m = Machine::new(Id::named("r"), Id::named("d"), Id::named("rk"), name);
        m.capacity = ResourceSpec::new(8000, 16 * 1024 * 1024 * 1024, 100 * 1024 * 1024 * 1024);
        for (k, v) in labels {
            m.metadata.labels.insert(k.to_string(), v.to_string());
        }
        m
    }

    #[test]
    fn node_selector_restricts_to_capable_nodes() {
        let gpu = machine("gpu-box", &[("gpu", "true"), ("nvme", "true")]);
        let plain = machine("plain", &[]);
        let wl = Workload::container("job", "img").requires("gpu", "true");
        assert!(machine_satisfies(&wl, &gpu), "gpu node should match");
        assert!(!machine_satisfies(&wl, &plain), "plain node should be excluded");

        // Multiple required flags: all must be present.
        let wl2 = Workload::container("job2", "img")
            .requires("gpu", "true")
            .requires("nvme", "true");
        assert!(machine_satisfies(&wl2, &gpu));
        let gpu_only = machine("gpu-only", &[("gpu", "true")]);
        assert!(!machine_satisfies(&wl2, &gpu_only));
    }

    #[test]
    fn no_selector_allows_any_node() {
        let any = Workload::container("any", "img");
        assert!(machine_satisfies(&any, &machine("plain", &[])));
    }

    #[test]
    fn capacity_filters_too() {
        let small = {
            let mut m = machine("small", &[]);
            m.capacity = ResourceSpec::new(100, 1024 * 1024, 0);
            m
        };
        let mut big = Workload::container("big", "img");
        big.resources = ResourceSpec::new(4000, 8 * 1024 * 1024 * 1024, 0);
        assert!(!machine_satisfies(&big, &small), "should not fit");
    }
}
