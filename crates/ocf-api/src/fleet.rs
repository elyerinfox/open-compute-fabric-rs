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
use std::collections::BTreeMap;

use ocf_core::prelude::*;
use ocf_fabric::{
    FabricNode, FabricServer, KeyPair, Liveness, MembershipEvent, NoiseTransport, NodeId,
    Reachability,
};

/// A node's request to be admitted to the Raft cluster, sent to a seed's control
/// channel as `join <json>`.
#[derive(serde::Serialize, serde::Deserialize)]
struct JoinRequest {
    raft_id: u64,
    raft_addr: String,
}
use ocf_loadbalancer::{Backend, LoadBalancer};
use ocf_runtime::Workload;
use ocf_topology::Machine;

use crate::controller::{node_for_machine, FabricController, WG_DATA, WG_LB, WG_MGMT};

impl FabricController {
    /// Register every topology machine into the membership detector and the
    /// mesh as an alive peer.
    ///
    /// The control plane is **unified over the `wg-mgmt` underlay**: each peer's
    /// dialable endpoint is its management overlay address (`10.255.0.x`), not its
    /// physical address — so Raft, membership gossip, and the latency prober all
    /// flow over the encrypted management WireGuard, isolated from the workload
    /// (`wg-data`) and load-balancer (`wg-lb`) planes.
    pub async fn init_membership(&self) -> Result<()> {
        let machines = self.topology.store().all_machines().await?;
        let mut sorted: Vec<&Machine> = machines.iter().collect();
        sorted.sort_by(|a, b| a.metadata.name.cmp(&b.metadata.name));
        for machine in &machines {
            let mut node = node_for_machine(machine);
            if let Some(i) = sorted
                .iter()
                .position(|m| m.metadata.name == machine.metadata.name)
            {
                node.endpoints = vec![self.mgmt_endpoint(i)];
            }
            self.membership.join(node.clone());
            self.fabric.join(node)?;
        }
        tracing::info!(
            members = self.membership.members().len(),
            "membership initialized (control plane on wg-mgmt)"
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

    /// The planned route from this node to every other member, computed over the
    /// fleet [`RouteGraph`](ocf_fabric::RouteGraph): `direct` when the peer can be
    /// dialed/reverse-connected, `relayed` (through the per-destination next-hop
    /// relay, with the full `hops` path) when it must bounce, or `unreachable`.
    /// This is the graph's packet-forwarding decision, made observable.
    pub async fn routes_view(&self) -> Vec<RouteView> {
        let graph = self.route_graph().await;
        let machines = self
            .topology
            .store()
            .all_machines()
            .await
            .unwrap_or_default();
        let name_of = |nid: &NodeId| {
            machines
                .iter()
                .find(|m| KeyPair::from_seed_name(&m.metadata.name).node_id() == *nid)
                .map(|m| m.metadata.name.clone())
        };
        let me = self.node_keypair().node_id();
        self.membership
            .members()
            .into_iter()
            .filter(|m| m.node.node_id != me)
            .map(|m| {
                let dest = m.node.node_id.clone();
                let (route, via, hops) = match graph.path(&me, &dest) {
                    None => ("unreachable", None, Vec::new()),
                    Some(h) if h.len() <= 1 => ("direct", None, Vec::new()),
                    Some(h) => {
                        let names: Vec<String> = h.iter().filter_map(&name_of).collect();
                        ("relayed", name_of(&h[0]), names)
                    }
                };
                RouteView {
                    target: m.node.node_id.to_string(),
                    machine_id: m.node.machine_id.as_ref().map(|id| id.to_string()),
                    reachability: reachability_str(m.node.reachability).into(),
                    route: route.into(),
                    via,
                    hops,
                }
            })
            .collect()
    }

    /// Resolve a load balancer's live backend set from its `target_selector`:
    /// the workloads matching the selector, addressed on the **wg-lb** plane at
    /// their hosting node, with measured RTT stamped. As an autoscaler with the
    /// same selector adds or removes replicas, this set follows — making the LB ↔
    /// autoscaling-group association live, over the isolated load-balancer underlay.
    pub async fn resolve_lb_backends(&self, lb: &LoadBalancer) -> Vec<Backend> {
        let plan = self.machine_plan().await;
        let machines = self
            .topology
            .store()
            .all_machines()
            .await
            .unwrap_or_default();
        let mut workloads = Vec::new();
        for provider in self.runtimes.all() {
            workloads.extend(provider.list().await.unwrap_or_default());
        }

        let node_index = |id: &Id| {
            plan.iter()
                .find(|(mid, _, _, _)| mid == id)
                .map(|(_, _, i, _)| *i)
        };
        let node_lb_addr = |id: &Id| node_index(id).map(|i| WG_LB.ip(i));
        let node_scope = |id: &Id| {
            machines
                .iter()
                .find(|m| &m.metadata.id == id)
                .map(|m| m.scope())
                .unwrap_or_else(Scope::fleet)
        };
        let node_latency = |id: &Id| {
            machines
                .iter()
                .find(|m| &m.metadata.id == id)
                .and_then(|m| self.membership.rtt(&KeyPair::from_seed_name(&m.metadata.name).node_id()))
        };

        lb_backends_for(
            &workloads,
            &lb.target_selector,
            node_lb_addr,
            node_scope,
            node_latency,
        )
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

    /// Run the fabric **control channel**: a control server that answers `ping`
    /// (for RTT timing) and `latency` (returns this node's measured latency map),
    /// plus a prober that periodically (1) times a round-trip to every alive peer
    /// and records the **measured RTT** in membership, and (2) fetches each peer's
    /// latency map so this node can build a fleet-wide [`RouteGraph`](ocf_fabric::RouteGraph)
    /// for graph-aware routing. Spawn on a task. On a single-host deployment peers
    /// are unreachable, so the maps stay empty — the mechanism is the same on a
    /// real fleet.
    pub async fn run_latency_services(self: Arc<Self>) {
        let port = self.config.fabric_control_port;
        let ctl = self.clone();
        match FabricServer::bind(("0.0.0.0", port), self.node_keypair()).await {
            Ok(srv) => {
                tokio::spawn(srv.run(move |_pk, req| {
                    let ctl = ctl.clone();
                    async move {
                        if req == b"latency" {
                            // Share our measured latencies so peers can build the graph.
                            serde_json::to_vec(&ctl.membership.latency_snapshot()).unwrap_or_default()
                        } else if let Some(body) = req.strip_prefix(b"join ") {
                            // A node asking to join the Raft cluster.
                            ctl.handle_join(body).await
                        } else {
                            req // `ping` → echo (for RTT timing)
                        }
                    }
                }));
            }
            Err(e) => {
                tracing::warn!(error = %e, "fabric control server bind failed; latency probing off");
            }
        }
        let client = NoiseTransport::with_keypair(self.node_keypair());
        let self_id = self.node_keypair().node_id();
        let mut interval = tokio::time::interval(StdDuration::from_secs(5));
        loop {
            interval.tick().await;
            for member in self.membership.members() {
                if !member.liveness.is_available() || member.node.node_id == self_id {
                    continue;
                }
                // The member's endpoint is already its wg-mgmt control address, so
                // the probe (and its RTT) reflect the management plane directly.
                let t = std::time::Instant::now();
                if client.request(&member.node, b"ping").await.is_ok() {
                    let rtt = t.elapsed().as_secs_f64() * 1000.0;
                    self.membership.record_rtt(&member.node.node_id, rtt);
                }
                // Pull the peer's own latency map to feed the fleet routing graph.
                if let Ok(bytes) = client.request(&member.node, b"latency").await {
                    if let Ok(map) =
                        serde_json::from_slice::<std::collections::BTreeMap<String, f64>>(&bytes)
                    {
                        self.peer_latency
                            .write()
                            .insert(member.node.node_id.clone(), map);
                    }
                }
            }
        }
    }

    /// This node's Raft endpoint other nodes dial it on — its `wg-mgmt` overlay
    /// address (so Raft rides the encrypted management plane and reaches NAT'd
    /// nodes), plus the Raft port.
    pub(crate) async fn raft_endpoint(&self) -> String {
        let plan = self.machine_plan().await;
        let ip = plan
            .iter()
            .find(|(_, name, _, _)| name == &self.node_id)
            .map(|(_, _, i, _)| WG_MGMT.ip(*i))
            .unwrap_or_else(|| "127.0.0.1".to_string());
        format!("{ip}:{}", self.config.fabric_raft_port)
    }

    /// Handle an inbound `join` request from a node that wants into the Raft
    /// cluster. Only the **leader** can change membership: it adds the joiner as a
    /// learner (catch-up) then promotes it to a voter. A non-leader replies
    /// `notleader` so the joiner tries another seed.
    async fn handle_join(&self, body: &[u8]) -> Vec<u8> {
        let req: JoinRequest = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(_) => return b"error".to_vec(),
        };
        if !self.consensus.is_leader() {
            return b"notleader".to_vec();
        }
        if let Err(e) = self.consensus.add_learner(req.raft_id, req.raft_addr.clone()).await {
            tracing::warn!(error = %e, raft_id = req.raft_id, "join: add_learner failed");
            return b"error".to_vec();
        }
        let mut voters: std::collections::BTreeSet<u64> =
            self.consensus.voters().into_iter().collect();
        voters.insert(req.raft_id);
        if let Err(e) = self.consensus.change_membership(voters).await {
            tracing::warn!(error = %e, raft_id = req.raft_id, "join: change_membership failed");
            return b"error".to_vec();
        }
        tracing::info!(raft_id = req.raft_id, addr = %req.raft_addr, "admitted node to the Raft cluster");
        b"ok".to_vec()
    }

    /// Join an existing Raft cluster via the configured seeds: advertise this
    /// node's Raft endpoint to each seed's control channel until the leader admits
    /// it. Spawned on a task for a node that booted with `seeds` set. Best-effort
    /// with retry; the leader's `change_membership` is itself quorum-committed, so
    /// joining can't create a second authority.
    pub async fn join_cluster(self: Arc<Self>) {
        let raft_id = crate::controller::raft_node_id(&self.node_id);
        let addr = self.raft_endpoint().await;
        let request = match serde_json::to_vec(&JoinRequest {
            raft_id,
            raft_addr: addr.clone(),
        }) {
            Ok(b) => {
                let mut p = b"join ".to_vec();
                p.extend_from_slice(&b);
                p
            }
            Err(_) => return,
        };
        let client = NoiseTransport::with_keypair(self.node_keypair());
        let mut interval = tokio::time::interval(StdDuration::from_secs(3));
        loop {
            interval.tick().await;
            for seed in &self.config.seeds {
                let seed_node = FabricNode::new(
                    NodeId::new(seed.clone()),
                    ocf_fabric::PublicKey::from_bytes(Vec::new()),
                    vec![seed.clone()],
                );
                match client.request(&seed_node, &request).await {
                    Ok(resp) if resp == b"ok" => {
                        tracing::info!(%addr, "joined Raft cluster via seed {seed}");
                        return;
                    }
                    Ok(resp) if resp == b"notleader" => continue, // try the next seed
                    Ok(_) => continue,
                    Err(_) => continue, // seed unreachable; retry
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
            let mut topology_changed = false;
            for event in self.membership.tick(Utc::now()) {
                match event {
                    MembershipEvent::Suspected(id) => {
                        tracing::warn!(node = %id, "peer suspected (heartbeats stalled)");
                    }
                    MembershipEvent::Died(id) => {
                        tracing::error!(node = %id, "peer declared dead");
                        self.on_node_dead(&id).await;
                        topology_changed = true;
                    }
                    MembershipEvent::Recovered(_)
                    | MembershipEvent::Joined(_)
                    | MembershipEvent::Left(_) => {
                        topology_changed = true;
                    }
                }
            }
            // Membership changed — re-program WireGuard so a private node fails
            // over to another live relay (and a recovered/joined relay is used).
            // ensure_interface / set_peer are idempotent, so this safely converges.
            if topology_changed {
                self.program_wireguard().await;
            }
        }
    }

    /// Map a dead node id back to its machine and run drop-out handling.
    ///
    /// **Quorum-gated:** only the Raft **leader** auto-reschedules. In a network
    /// partition each side's failure detector fires, but a minority partition has
    /// no leader (Raft needs a quorum to elect one), so it does nothing — exactly
    /// one node, in the majority, performs the reschedule. This is what stops two
    /// partition halves from both restarting the same HA workload (split-brain).
    /// A quorum-of-one node is always its own leader, so the single-node path is
    /// unaffected.
    async fn on_node_dead(&self, node_id: &NodeId) {
        if !self.consensus.is_leader() {
            tracing::warn!(
                node = %node_id,
                leader = ?self.consensus.leader(),
                "peer dead but this node is not the Raft leader — deferring reschedule to the leader"
            );
            return;
        }
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

/// Resolve a load balancer's `target_selector` to its live backend set: the
/// scheduled workloads whose labels match, each addressed at its hosting node's
/// **wg-lb** address, scoped to that node, with measured RTT stamped (for the
/// `Latency` policy). An empty selector resolves to no backends. This is the
/// live LB ↔ workloads/autoscaling-group association — the same label set an
/// autoscaler governs is the set the LB fronts.
pub(crate) fn lb_backends_for(
    workloads: &[Workload],
    selector: &BTreeMap<String, String>,
    node_lb_addr: impl Fn(&Id) -> Option<String>,
    node_scope: impl Fn(&Id) -> Scope,
    node_latency: impl Fn(&Id) -> Option<f64>,
) -> Vec<Backend> {
    if selector.is_empty() {
        return Vec::new();
    }
    workloads
        .iter()
        .filter(|w| w.metadata.matches_labels(selector))
        .filter_map(|w| {
            let node = w.node.as_ref()?;
            let addr = node_lb_addr(node)?; // skip unscheduled / unknown nodes
            let mut backend = Backend::new(w.metadata.id.clone(), addr, node_scope(node));
            if let Some(rtt) = node_latency(node) {
                backend = backend.with_latency(rtt);
            }
            Some(backend)
        })
        .collect()
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
    /// The next-hop relay's name when `route == "relayed"`.
    pub via: Option<String>,
    /// The full path (machine names from this node to the target) when relayed.
    pub hops: Vec<String>,
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

/// One peer's address on one WireGuard plane, for the API.
#[derive(Serialize)]
pub struct WireguardPeerView {
    pub name: String,
    pub wg_ip: String,
    pub reachability: String,
    /// The pinned WireGuard endpoint, or `null` when it is **roam-learned**
    /// (a NAT'd peer that reverse-connects).
    pub endpoint: Option<String>,
    /// `persistent-keepalive` seconds toward this peer (`0` = off; non-zero holds
    /// *our* NAT mapping open when we reverse-connect).
    pub keepalive: u16,
    pub public_key: String,
}

/// One isolated WireGuard plane (control / workload / load-balancer).
#[derive(Serialize)]
pub struct WireguardPlaneView {
    pub iface: String,
    pub purpose: String,
    pub node_ip: String,
    pub port: u16,
    pub peers: Vec<WireguardPeerView>,
}

/// The three computed WireGuard underlay planes: this node and its peers on each.
/// Lets you inspect the isolated-overlay wiring even where the kernel programming
/// can't run.
#[derive(Serialize)]
pub struct WireguardView {
    pub node: String,
    pub reachability: String,
    pub public_key: String,
    pub planes: Vec<WireguardPlaneView>,
}

impl FabricController {
    /// The computed WireGuard planes (this node + peers per plane). Control rides
    /// `wg-mgmt`, the VXLAN workload overlay `wg-data`, the LB `wg-lb` — three
    /// isolated encrypted underlays. Each peer shows the **reachability-aware**
    /// config: a pinned endpoint for dialable peers, `null` (roam-learned) for a
    /// NAT'd peer that reverse-connects, and keepalive when we hold a mapping open.
    pub async fn wireguard_status(&self) -> WireguardView {
        use crate::controller::{reachability_from_machine, wg_direct_endpoint_keepalive};

        let plan = self.machine_plan().await;
        let machines = self
            .topology
            .store()
            .all_machines()
            .await
            .unwrap_or_default();
        let reach_of = |name: &str| {
            machines
                .iter()
                .find(|m| m.metadata.name == name)
                .map(reachability_from_machine)
                .unwrap_or(ocf_fabric::Reachability::Public)
        };
        let self_reach = reach_of(&self.node_id);
        let my_kp = ocf_fabric::KeyPair::from_seed_name(&self.node_id);
        let self_idx = plan
            .iter()
            .find(|(_, name, _, _)| name == &self.node_id)
            .map(|(_, _, i, _)| *i);

        let planes = [
            (WG_MGMT, "control"),
            (WG_DATA, "workload"),
            (WG_LB, "load-balancer"),
        ]
        .into_iter()
        .map(|(plane, purpose)| {
            let node_ip = self_idx
                .map(|i| plane.ip(i))
                .unwrap_or_else(|| format!("{}.254", plane.prefix));
            let peers = plan
                .iter()
                .filter(|(_, name, _, _)| name != &self.node_id)
                .map(|(_, name, index, addr)| {
                    let peer_reach = reach_of(name);
                    let (endpoint, keepalive) = wg_direct_endpoint_keepalive(
                        self_reach,
                        peer_reach,
                        addr.as_deref(),
                        plane.port,
                    );
                    WireguardPeerView {
                        name: name.clone(),
                        wg_ip: plane.ip(*index),
                        reachability: reachability_str(peer_reach).into(),
                        endpoint,
                        keepalive,
                        public_key: ocf_fabric::KeyPair::from_seed_name(name)
                            .public
                            .to_wireguard_key(),
                    }
                })
                .collect();
            WireguardPlaneView {
                iface: plane.iface.to_string(),
                purpose: purpose.to_string(),
                node_ip,
                port: plane.port,
                peers,
            }
        })
        .collect();

        WireguardView {
            node: self.node_id.clone(),
            reachability: reachability_str(self_reach).into(),
            public_key: my_kp.public.to_wireguard_key(),
            planes,
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

    #[test]
    fn lb_backends_resolve_from_selector_on_wg_lb() {
        use super::lb_backends_for;
        use std::collections::BTreeMap;

        let mut web = Workload::container("web-1", "img");
        web.metadata.labels.insert("app".into(), "web".into());
        web.node = Some(Id::named("node-a"));

        let mut db = Workload::container("db-1", "img");
        db.metadata.labels.insert("app".into(), "db".into());
        db.node = Some(Id::named("node-b"));

        let mut unscheduled = Workload::container("web-2", "img");
        unscheduled.metadata.labels.insert("app".into(), "web".into());
        unscheduled.node = None; // not placed → excluded

        let workloads = vec![web, db, unscheduled];
        let mut selector = BTreeMap::new();
        selector.insert("app".to_string(), "web".to_string());

        let backends = lb_backends_for(
            &workloads,
            &selector,
            |id| (id.as_str() == "node-a").then(|| "10.253.0.1:0".to_string()),
            |_| Scope::fleet(),
            |_| Some(2.5),
        );

        // Only the scheduled, matching workload, addressed on the wg-lb plane.
        assert_eq!(backends.len(), 1);
        assert_eq!(backends[0].address, "10.253.0.1:0");
        assert_eq!(backends[0].latency_ms, 2.5);

        // Empty selector → no backends.
        assert!(lb_backends_for(&workloads, &BTreeMap::new(), |_| None, |_| Scope::fleet(), |_| None).is_empty());
    }
}
