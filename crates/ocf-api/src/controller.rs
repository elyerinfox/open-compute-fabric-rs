//! The [`FabricController`]: the single object that owns every subsystem.
//!
//! This is where the fabric stops being a pile of independent contracts and
//! becomes one control plane. The controller holds each subsystem's service
//! (or its plugin [`Registry`]) behind one struct; the API layer borrows from it
//! and the `ocfd` binary constructs exactly one of them.
//!
//! [`FabricController::bootstrap`] builds every subsystem with its built-in
//! providers registered, then either **restores** persisted state (if a data
//! directory holds a prior snapshot) or **seeds** a small demo fleet and
//! persists it. It also stands up the membership/failure detector so the
//! controller can react to nodes joining and dropping out
//! (see [`crate::fleet`] and [`crate::persist`]).

use std::sync::Arc;

use chrono::Duration;
use ocf_core::prelude::*;

use ocf_consensus::ReplicatedStore;
use ocf_auth::{Authenticator, LinuxUserSync, LocalAuthenticator};
use ocf_authz::{Group, RbacEngine, RoleBinding, Subject, User, ADMINISTRATOR_ROLE};
use ocf_disk::{DiskHealth, DiskService, LedControl, PhysicalDisk, SysfsDiskManager};
use ocf_fabric::{
    FabricMesh, FabricNode, FabricTransport, KeyPair, Membership, NodeId, Reachability,
};
use ocf_health::{HealthCheck, HealthService, PackageCheck};
use ocf_platform::PlatformService;
use ocf_inventory::InventoryService;
use ocf_kernel::KernelManager;
use ocf_loadbalancer::{
    CertificateProvider, DnsProvider, LbKind, Listener, LoadBalancer, LoadBalancerController,
    RoutingPolicy,
};
use ocf_monitoring::MonitoringService;
use ocf_network::{EgressMode, NetworkBackend, NetworkController, Subnet, Vpc, WireguardUnderlay};
use ocf_runtime::{NetworkAttachment, RuntimeProvider, Workload};
use ocf_store::{MemoryStateStore, RedbStateStore, StateStore};
use ocf_topology::{Datacenter, InMemoryTopology, Machine, Rack, Region, TopologyService, TopologyStore};

use crate::config::ControllerConfig;

/// One isolated WireGuard underlay plane: its interface name, the `/24` host
/// prefix it assigns overlay addresses from, and its WireGuard (UDP) listen port.
/// The fabric runs three of these so the management, workload, and load-balancer
/// data planes never share an interface or address space.
#[derive(Clone, Copy)]
pub(crate) struct WgPlane {
    pub iface: &'static str,
    pub prefix: &'static str,
    pub port: u16,
}

/// Control plane (Raft, membership, latency, streaming) — node-to-node management.
pub(crate) const WG_MGMT: WgPlane = WgPlane { iface: "wg-mgmt", prefix: "10.255.0", port: 51820 };
/// Workload data plane — the VXLAN overlay's VTEPs ride this.
pub(crate) const WG_DATA: WgPlane = WgPlane { iface: "wg-data", prefix: "10.254.0", port: 51821 };
/// Load-balancer ingress plane — LB-to-backend traffic rides this.
pub(crate) const WG_LB: WgPlane = WgPlane { iface: "wg-lb", prefix: "10.253.0", port: 51822 };

impl WgPlane {
    /// This plane's overlay address for the machine at `index` (1-based host).
    pub(crate) fn ip(&self, index: usize) -> String {
        format!("{}.{}", self.prefix, index + 1)
    }
}

/// Every subsystem of the fabric, owned in one place.
pub struct FabricController {
    pub config: ControllerConfig,
    pub node_id: String,
    pub store: Arc<dyn StateStore>,
    /// Raft-replicated control plane. Writes go through `consensus` (committed by
    /// a quorum, then applied into `store`); reads come from `store`.
    pub consensus: Arc<ReplicatedStore>,
    pub membership: Arc<Membership>,
    pub topology: TopologyService,
    pub runtimes: Registry<dyn RuntimeProvider>,
    pub authenticators: Registry<dyn Authenticator>,
    pub host_user_sync: Arc<LinuxUserSync>,
    pub rbac: Arc<RbacEngine>,
    pub kernel: KernelManager,
    pub inventory: InventoryService,
    pub inventory_controllers: InventoryController,
    pub disks: DiskService,
    pub(crate) disk_mgr: Arc<SysfsDiskManager>,
    pub monitoring: MonitoringService,
    pub fabric: FabricMesh,
    pub network: NetworkController,
    pub loadbalancers: LoadBalancerController,
    pub cert_providers: Registry<dyn CertificateProvider>,
    pub dns_providers: Registry<dyn DnsProvider>,
    /// Host OS detection + package managers (resolve/install missing tools).
    pub platform: Arc<PlatformService>,
    /// Modular fleet-health checks (kernel/runtime/network warnings + fixes).
    pub health: HealthService,
    /// Workload → subnet attachments. The runtime providers are stateless (they
    /// query `docker`/`virsh` for live state), so the rich network binding lives
    /// here, persisted, keyed by workload id.
    pub(crate) attachments: parking_lot::RwLock<std::collections::HashMap<Id, NetworkAttachment>>,
}

impl FabricController {
    /// Build the controller, restoring persisted state or seeding a demo fleet.
    pub async fn bootstrap(config: ControllerConfig) -> Result<Self> {
        // --- durable state store -------------------------------------------
        let store: Arc<dyn StateStore> = match &config.data_dir {
            Some(dir) => {
                std::fs::create_dir_all(dir)
                    .map_err(|e| Error::internal(format!("create data dir: {e}")))?;
                Arc::new(RedbStateStore::open(dir.join("state.redb"))?)
            }
            None => Arc::new(MemoryStateStore::new()),
        };

        // --- subsystems (built empty; populated below) ----------------------
        let topology = TopologyService::new(Arc::new(InMemoryTopology::new()));

        let mut runtimes = Registry::<dyn RuntimeProvider>::new();
        ocf_runtime::register_builtins(&mut runtimes)?;

        let mut authenticators = Registry::<dyn Authenticator>::new();
        ocf_auth::register_builtins(&mut authenticators)?;
        authenticators.register_or_replace(
            "local",
            Arc::new(LocalAuthenticator::with_admin("admin", "admin")),
        );
        let host_user_sync = Arc::new(LinuxUserSync::new());

        let rbac = Arc::new(RbacEngine::with_defaults());
        let kernel = KernelManager::with_defaults()?;

        let mut inv_collectors = Registry::new();
        let mut ipmi_controllers = Registry::new();
        ocf_inventory::register_builtins(&mut inv_collectors, &mut ipmi_controllers)?;
        let inventory_controllers = InventoryController {
            collectors: inv_collectors,
            ipmi: ipmi_controllers,
        };
        let inventory = InventoryService::new();

        let disk_mgr = Arc::new(SysfsDiskManager::new());
        let mut led_reg = Registry::<dyn LedControl>::new();
        ocf_disk::register_led_builtins(&mut led_reg)?;
        let led = led_reg.get("ledctl")?;
        let disks = DiskService::new(disk_mgr.clone(), led);

        let monitoring = MonitoringService::with_builtins()?;

        let mut transports = Registry::<dyn FabricTransport>::new();
        ocf_fabric::register_builtins(&mut transports)?;
        let transport = transports.get("noise")?;
        let fabric = FabricMesh::new(transport);

        let mut net_backends = Registry::<dyn NetworkBackend>::new();
        ocf_network::register_builtins(&mut net_backends)?;
        let network = NetworkController::new(Arc::new(net_backends));

        let mut cert_providers = Registry::<dyn CertificateProvider>::new();
        let mut dns_providers = Registry::<dyn DnsProvider>::new();
        ocf_loadbalancer::register_builtins(&mut cert_providers, &mut dns_providers)?;

        // Platform: detect the host OS + register package managers, so health
        // checks can offer OS-aware "install missing tool" fixes.
        let platform = Arc::new(PlatformService::detect()?);
        let mut health_reg = Registry::<dyn HealthCheck>::new();
        ocf_health::register_builtins(&mut health_reg)?;
        health_reg.register(
            "packages",
            Arc::new(PackageCheck::new(
                platform.clone(),
                ocf_platform::builtin_capabilities(),
            )),
        )?;
        let health = HealthService::new(health_reg);
        let loadbalancers = LoadBalancerController::new();

        let membership = Arc::new(Membership::with_timeouts(
            NodeId::new(config.node_id.clone()),
            Duration::seconds(config.suspect_timeout_secs.max(1)),
            Duration::seconds(config.dead_timeout_secs.max(1)),
        ));

        // Raft-replicated control plane. This node forms (or, with peers, joins)
        // a cluster whose committed writes are applied into `store`. A
        // single-node deployment is a quorum of one — every write is still
        // ordered through the Raft log before it lands.
        let raft_id = raft_node_id(&config.node_id);
        let consensus = Arc::new(ReplicatedStore::start(raft_id, vec![raft_id], store.clone()).await?);
        consensus.initialize(vec![raft_id]).await?;
        consensus
            .wait_for_leader(std::time::Duration::from_secs(10))
            .await?;

        let controller = FabricController {
            node_id: config.node_id.clone(),
            config,
            store,
            consensus,
            membership,
            topology,
            runtimes,
            authenticators,
            host_user_sync,
            rbac,
            kernel,
            inventory,
            inventory_controllers,
            disks,
            disk_mgr,
            monitoring,
            fabric,
            network,
            loadbalancers,
            cert_providers,
            dns_providers,
            platform,
            health,
            attachments: parking_lot::RwLock::new(std::collections::HashMap::new()),
        };

        // --- restore-or-seed ------------------------------------------------
        let persisted = !controller.store.list("machines")?.is_empty();
        if persisted {
            tracing::info!("restoring fabric state from durable store");
            controller.restore().await?;
        } else {
            tracing::info!("no persisted state; seeding demo fleet");
            controller.seed_demo().await?;
            controller.persist().await?;
        }

        // --- membership / mesh ----------------------------------------------
        controller.init_membership().await?;

        // --- encrypted underlay + overlay across hosts ----------------------
        // Bring up the WireGuard underlay first, then point the VXLAN overlay's
        // VTEPs at the WireGuard addresses so workload traffic is encrypted.
        controller.program_wireguard().await;
        controller.program_vxlan_peers().await;

        Ok(controller)
    }

    /// This node's id as a machine id, for tagging the health findings it
    /// produces about its own host.
    pub fn node_machine_id(&self) -> Id {
        Id::from(self.node_id.clone())
    }

    /// Collect every workload across every registered runtime backend, overlaying
    /// the stored network attachment (the providers are stateless and don't carry
    /// it).
    pub async fn all_workloads(&self) -> Vec<Workload> {
        let mut out = Vec::new();
        for provider in self.runtimes.all() {
            if let Ok(mut wls) = provider.list().await {
                out.append(&mut wls);
            }
        }
        let attachments = self.attachments.read();
        for w in &mut out {
            if let Some(att) = attachments.get(&w.metadata.id) {
                w.network = Some(att.clone());
            }
        }
        out
    }

    /// Collect every disk across every machine in the topology. A machine whose
    /// disks cannot be enumerated (no `lsblk`, not reachable) is skipped rather
    /// than failing the whole sweep.
    pub async fn all_disks(&self) -> Result<Vec<PhysicalDisk>> {
        let mut out = Vec::new();
        for machine in self.topology.store().all_machines().await? {
            match self.disks.list(&machine.metadata.id).await {
                Ok(mut disks) => out.append(&mut disks),
                Err(e) => tracing::warn!(machine = %machine.metadata.name, error = %e,
                    "could not enumerate disks"),
            }
        }
        Ok(out)
    }

    /// The subnet addresses of workloads that have opted in to egress on
    /// `subnet_id` — the allow-list the egress data path is programmed with.
    /// Read from the persisted attachment store (the runtime providers are
    /// stateless and don't carry the binding).
    pub fn subnet_egress_allowed(&self, subnet_id: &Id) -> Vec<String> {
        self.attachments
            .read()
            .values()
            .filter(|a| &a.subnet_id == subnet_id && a.egress)
            .filter_map(|a| a.address.clone())
            .collect()
    }

    /// Attach a workload to a subnet: allocate it an address (IPAM), record the
    /// binding with its egress opt-in, re-program the subnet's egress allow-list,
    /// and persist. Returns the resulting attachment.
    pub async fn attach_workload(
        &self,
        workload_id: &Id,
        subnet_id: &Id,
        egress: bool,
    ) -> Result<NetworkAttachment> {
        // Validate the subnet exists.
        let _ = self.network.get_subnet(subnet_id).await?;

        // Release any prior address for this workload before re-allocating.
        if let Some(prev) = self.attachments.read().get(workload_id).cloned() {
            self.network.release_address(&prev.subnet_id, prev.address.as_deref().unwrap_or(""));
        }

        // Allocate a fresh address from the subnet's pool.
        let address = self.network.allocate_address(subnet_id)?;
        let attachment = NetworkAttachment::new(subnet_id.clone())
            .with_egress(egress)
            .with_address(address);
        self.attachments
            .write()
            .insert(workload_id.clone(), attachment.clone());

        // Re-program egress for the subnet with the new allow-list.
        let allowed = self.subnet_egress_allowed(subnet_id);
        if let Err(e) = self.network.refresh_subnet_egress(subnet_id, &allowed).await {
            tracing::warn!(error = %e, "egress refresh after attach failed (binding recorded)");
        }

        // Best-effort: splice the running container onto the subnet's overlay
        // bridge (Linux + container runtime only).
        self.splice_workload_overlay(workload_id, subnet_id, &attachment).await;

        let _ = self.persist().await;
        Ok(attachment)
    }

    /// Splice a workload's container onto its subnet's overlay bridge: resolve the
    /// container's host PID from whichever runtime holds it, then create a veth
    /// pair into the container's netns and onto the subnet bridge with the IPAM
    /// address. Best-effort — a VM workload, a host without `ip`, or a stopped
    /// container simply logs and is skipped.
    async fn splice_workload_overlay(
        &self,
        workload_id: &Id,
        subnet_id: &Id,
        attachment: &NetworkAttachment,
    ) {
        let Some(address) = &attachment.address else { return };
        // Find the runtime holding this workload and its container PID.
        let mut pid = None;
        for provider in self.runtimes.all() {
            if let Ok(Some(p)) = provider.host_pid(workload_id).await {
                pid = Some(p);
                break;
            }
        }
        let Some(pid) = pid else { return };
        let subnet = match self.network.get_subnet(subnet_id).await {
            Ok(s) => s,
            Err(_) => return,
        };
        let prefix = subnet.cidr.split_once('/').map(|(_, p)| p).unwrap_or("24");
        let cidr = format!("{address}/{prefix}");
        let bridge = ocf_network::subnet_bridge_name(subnet_id);
        let alias = format!("ocf-{}", workload_id.as_str().chars().take(8).collect::<String>());
        if let Err(e) = ocf_network::attach_container_to_subnet(
            pid,
            &alias,
            &bridge,
            workload_id.as_str(),
            &cidr,
        )
        .await
        {
            tracing::warn!(workload = %workload_id, error = %e, "overlay attach failed (best-effort)");
        } else {
            tracing::info!(workload = %workload_id, %cidr, bridge = %bridge, "container attached to overlay");
        }
    }

    /// Detach a workload from its subnet: release its address and re-program the
    /// subnet's egress allow-list. No-op if the workload has no attachment.
    pub async fn detach_workload(&self, workload_id: &Id) -> Result<()> {
        let removed = self.attachments.write().remove(workload_id);
        if let Some(att) = removed {
            self.network
                .release_address(&att.subnet_id, att.address.as_deref().unwrap_or(""));
            let allowed = self.subnet_egress_allowed(&att.subnet_id);
            if let Err(e) = self.network.refresh_subnet_egress(&att.subnet_id, &allowed).await {
                tracing::warn!(error = %e, "egress refresh after detach failed");
            }
            let _ = self.persist().await;
        }
        Ok(())
    }

    /// The deterministic per-machine plan: `(machine_id, name, index,
    /// fabric_address)` with machines sorted by name. The `index` indexes every
    /// plane's address (`WgPlane::ip`), and `fabric_address` is the real underlay
    /// address a WireGuard `endpoint` points at.
    pub(crate) async fn machine_plan(&self) -> Vec<(Id, String, usize, Option<String>)> {
        let mut machines = match self.topology.store().all_machines().await {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        machines.sort_by(|a, b| a.metadata.name.cmp(&b.metadata.name));
        machines
            .into_iter()
            .enumerate()
            .map(|(i, m)| (m.metadata.id.clone(), m.metadata.name, i, m.fabric_address))
            .collect()
    }

    /// This node's index in the plan (for address assignment), if present.
    fn self_index(&self, plan: &[(Id, String, usize, Option<String>)]) -> Option<usize> {
        plan.iter()
            .find(|(_, name, _, _)| name == &self.node_id)
            .map(|(_, _, i, _)| *i)
    }

    /// Bring up one WireGuard plane on this node and program every other node as a
    /// peer on it. The WireGuard `endpoint` is the peer's real underlay address;
    /// the `allowed-ips` is the peer's overlay address *on this plane only*, so the
    /// three planes stay isolated. Best-effort.
    async fn program_plane(&self, plane: WgPlane, plan: &[(Id, String, usize, Option<String>)]) {
        let wg = WireguardUnderlay::new(plane.iface, plane.port);
        let my_kp = KeyPair::from_seed_name(&self.node_id);
        let my_ip = self
            .self_index(plan)
            .map(|i| plane.ip(i))
            .unwrap_or_else(|| format!("{}.254", plane.prefix));
        match wg
            .ensure_interface(&my_kp.secret.to_wireguard_key(), &format!("{my_ip}/16"))
            .await
        {
            Ok(mode) => tracing::info!(plane = plane.iface, %mode, "wireguard plane up"),
            Err(e) => tracing::warn!(plane = plane.iface, error = %e, "wireguard interface setup failed"),
        }
        for (_, name, index, addr) in plan {
            if name == &self.node_id {
                continue;
            }
            let Some(addr) = addr else { continue };
            let peer_pub = KeyPair::from_seed_name(name).public.to_wireguard_key();
            let endpoint = format!("{addr}:{}", plane.port);
            let allowed = format!("{}/32", plane.ip(*index));
            if let Err(e) = wg.set_peer(&peer_pub, &endpoint, &allowed, 25).await {
                tracing::warn!(plane = plane.iface, peer = %name, error = %e, "wireguard peer failed");
            }
        }
    }

    /// Bring up all three isolated WireGuard underlays (management, workload,
    /// load-balancer) and program peers on each. The control plane dials the
    /// `wg-mgmt` addresses, VXLAN VTEPs the `wg-data` addresses, and the LB the
    /// `wg-lb` addresses — so the planes never share an interface or address space.
    pub async fn program_wireguard(&self) {
        let plan = self.machine_plan().await;
        if plan.is_empty() {
            return;
        }
        for plane in [WG_MGMT, WG_DATA, WG_LB] {
            self.program_plane(plane, &plan).await;
        }
        tracing::info!(
            peers = plan.len().saturating_sub(1),
            "wireguard planes programmed (mgmt/data/lb)"
        );
    }

    /// The VXLAN VTEP addresses of the *other* nodes — their **wg-data** overlay
    /// addresses, so workload traffic rides the isolated workload underlay (never
    /// the management plane). Excludes this node.
    pub async fn peer_vteps(&self) -> Vec<String> {
        self.machine_plan()
            .await
            .into_iter()
            .filter(|(_, name, _, _)| name != &self.node_id)
            .map(|(_, _, index, _)| WG_DATA.ip(index))
            .collect()
    }

    /// The control-plane address (on `wg-mgmt`) the node at `index` is reached at.
    pub(crate) fn mgmt_endpoint(&self, index: usize) -> String {
        format!("{}:{}", WG_MGMT.ip(index), self.config.fabric_control_port)
    }

    /// Program every VPC's VXLAN overlay with the current peer VTEP set (the
    /// WireGuard addresses), so the overlay is stitched across hosts and
    /// encrypted. Best-effort.
    pub async fn program_vxlan_peers(&self) {
        let peers = self.peer_vteps().await;
        if peers.is_empty() {
            return;
        }
        let vpcs = match self.network.list_vpcs().await {
            Ok(v) => v,
            Err(_) => return,
        };
        for vpc in vpcs {
            let _ = self.network.refresh_vpc_peers(&vpc.metadata.id, &peers).await;
        }
    }

    /// Set a subnet's egress capability (`Nat` or `Isolated`), re-program the
    /// dataplane with the current opted-in workload addresses, and persist.
    pub async fn set_subnet_egress(&self, subnet_id: &Id, mode: EgressMode) -> Result<Subnet> {
        let allowed = self.subnet_egress_allowed(subnet_id);
        let subnet = self
            .network
            .set_subnet_egress(subnet_id, mode, &allowed)
            .await?;
        let _ = self.persist().await;
        Ok(subnet)
    }

    /// Seed a small, illustrative fleet (used on first boot only).
    ///
    /// Steps that touch real dataplane tooling (runtimes, network programming)
    /// are best-effort: on a node without those tools the control plane still
    /// comes up, it just has no demo workloads/networks.
    async fn seed_demo(&self) -> Result<()> {
        let fleet = seed_topology(self.topology.store()).await?;
        seed_workloads(&self.runtimes, &fleet).await?;
        seed_disks(&self.disk_mgr, &fleet);
        if let Err(e) = seed_network(&self.network).await {
            tracing::warn!(error = %e, "network demo seed skipped (dataplane unavailable)");
        }
        seed_loadbalancers(&self.loadbalancers).await?;
        seed_rbac(&self.rbac);
        Ok(())
    }
}

/// Derive a stable, non-zero numeric Raft node id from the configured node name
/// (FNV-1a), so each node in a cluster has a distinct id.
fn raft_node_id(node_id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in node_id.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash | 1 // never zero
}

/// The two inventory registries, grouped so the controller carries one field.
pub struct InventoryController {
    pub collectors: Registry<dyn ocf_inventory::InventoryCollector>,
    pub ipmi: Registry<dyn ocf_inventory::IpmiController>,
}

// --- seeding helpers -------------------------------------------------------

/// Each machine's id paired with its name.
pub(crate) struct Fleet {
    pub machines: Vec<(Id, String)>,
}

async fn seed_topology(store: &Arc<dyn TopologyStore>) -> Result<Fleet> {
    let mut region = Region::new("us-east");
    region.locality = "us-east".to_string();
    let region_id = region.metadata.id.clone();
    store.put_region(region).await?;

    let dc = Datacenter::new(region_id.clone(), "dc-1");
    let dc_id = dc.metadata.id.clone();
    store.put_datacenter(dc).await?;

    let rack = Rack::new(region_id.clone(), dc_id.clone(), "rack-a1");
    let rack_id = rack.metadata.id.clone();
    store.put_rack(rack).await?;

    let mut machines = Vec::new();
    for i in 1..=3 {
        let name = format!("node-{i}");
        let mut m = Machine::new(region_id.clone(), dc_id.clone(), rack_id.clone(), &name);
        m.rack_position = Some(i as u16);
        m.fabric_address = Some(format!("10.0.0.{}", i));
        m.capacity = ResourceSpec::new(32_000, 128 * 1024 * 1024 * 1024, 2 * 1024_u64.pow(4));
        m.state = LifecycleState::Running;
        m.health = Health::Healthy;
        // Capability labels: node-3 is the GPU/NVMe box; node-1 has NVMe.
        if i == 3 {
            m.metadata.labels.insert("gpu".into(), "true".into());
            m.metadata.labels.insert("nvme".into(), "true".into());
        } else if i == 1 {
            m.metadata.labels.insert("nvme".into(), "true".into());
        }
        // Reachability demo: node-1 is a relay; node-2 is private (NAT'd) and is
        // therefore reached through the relay; node-3 is public.
        let reach = match i {
            1 => "relay",
            2 => "private",
            _ => "public",
        };
        m.metadata
            .labels
            .insert("fabric.reachability".into(), reach.into());
        machines.push((m.metadata.id.clone(), name));
        store.put_machine(m).await?;
    }

    Ok(Fleet { machines })
}

async fn seed_workloads(runtimes: &Registry<dyn RuntimeProvider>, fleet: &Fleet) -> Result<()> {
    let docker = runtimes.get("docker")?;
    for (i, image) in ["nginx:1.27", "redis:7"].iter().enumerate() {
        let mut wl = Workload::container(format!("web-{}", i + 1), *image);
        wl.metadata.labels.insert("app".into(), "web".into());
        wl.node = fleet.machines.get(i).map(|(id, _)| id.clone());
        wl.resources = ResourceSpec::new(500, 256 * 1024 * 1024, 0);
        spawn_workload(&docker, wl).await;
    }

    let qemu = runtimes.get("qemu")?;
    let mut vm = Workload::virtual_machine("db-1", "debian-12.qcow2");
    vm.highly_available = true;
    vm.node = fleet.machines.get(2).map(|(id, _)| id.clone());
    vm.resources = ResourceSpec::new(4000, 8 * 1024 * 1024 * 1024, 64 * 1024 * 1024 * 1024);
    spawn_workload(&qemu, vm).await;

    // A workload that requires the GPU capability — only schedulable on node-3.
    let mut gpu = Workload::container("gpu-job", "cuda:12")
        .requires("gpu", "true")
        .highly_available(true);
    gpu.resources = ResourceSpec::new(2000, 4 * 1024 * 1024 * 1024, 0);
    gpu.node = fleet.machines.get(2).map(|(id, _)| id.clone()); // node-3
    spawn_workload(&docker, gpu).await;
    Ok(())
}

/// Best-effort create+start: a node that lacks the backing runtime (no
/// `docker`/`virsh` installed) logs and moves on rather than aborting boot.
pub(crate) async fn spawn_workload(provider: &Arc<dyn RuntimeProvider>, wl: Workload) {
    let id = wl.metadata.id.clone();
    if let Err(e) = provider.create(&wl).await {
        tracing::warn!(workload = %wl.metadata.name, backend = provider.name(), error = %e,
            "could not create workload (backing runtime unavailable?)");
        return;
    }
    if let Err(e) = provider.start(&id).await {
        tracing::warn!(workload = %wl.metadata.name, error = %e, "could not start workload");
    }
}

fn seed_disks(mgr: &SysfsDiskManager, fleet: &Fleet) {
    for (mi, (machine_id, _)) in fleet.machines.iter().enumerate() {
        for d in 0..2 {
            let serial = format!("S/N-{}{}{:04}", (b'A' + mi as u8) as char, d, mi * 10 + d);
            let mut disk = PhysicalDisk::new(machine_id.clone(), serial);
            disk.dev_path = format!("/dev/sd{}", (b'a' + d as u8) as char);
            disk.model = "OCF-NVMe-3840".to_string();
            disk.vendor = "OpenCompute".to_string();
            disk.size_bytes = 3_840_000_000_000;
            disk.health = if mi == 2 && d == 1 {
                DiskHealth::Warning
            } else {
                DiskHealth::Ok
            };
            disk.enclosure = Some("enc-0".to_string());
            disk.slot = Some((mi * 2 + d) as u32);
            mgr.seed(disk);
        }
    }
}

async fn seed_network(network: &NetworkController) -> Result<()> {
    let vpc = network
        .create_vpc(Vpc::new("tenant-a", "10.0.0.0/16", 1001))
        .await?;
    let vpc_id = vpc.metadata.id.clone();
    // The "web" subnet is public (NAT egress); "db" stays internal-only.
    network
        .create_subnet(
            Subnet::new(vpc_id.clone(), "web", "10.0.1.0/24", "ns-web")
                .with_egress(EgressMode::Nat),
        )
        .await?;
    network
        .create_subnet(Subnet::new(vpc_id, "db", "10.0.2.0/24", "ns-db"))
        .await?;
    Ok(())
}

async fn seed_loadbalancers(controller: &LoadBalancerController) -> Result<()> {
    controller
        .create(
            LoadBalancer::new("web-https", LbKind::Application, RoutingPolicy::Latency)
                .with_listener(Listener::tls(443))
                .with_hostname("app.example.com")
                // Fronts the `app=web` workloads — the same label set an
                // autoscaler would govern; backends resolve on the wg-lb plane.
                .fronting("app", "web"),
        )
        .await?;
    controller
        .create(
            LoadBalancer::new("db-tcp", LbKind::Tcp, RoutingPolicy::LeastLoad)
                .with_listener(Listener::tcp(5432)),
        )
        .await?;
    Ok(())
}

fn seed_rbac(rbac: &RbacEngine) {
    rbac.put_user(User::new("admin").with_group("admins"));
    rbac.put_group(Group::new("admins").with_member("admin"));
    rbac.add_binding(RoleBinding::new(
        Subject::group("admins"),
        ADMINISTRATOR_ROLE,
        Scope::fleet(),
    ));
}

/// Build a [`FabricNode`] record for a topology machine (stable identity from
/// its name, dialable at its fabric address).
pub(crate) fn node_for_machine(machine: &Machine) -> FabricNode {
    let endpoint = machine
        .fabric_address
        .clone()
        .unwrap_or_else(|| format!("{}.fabric", machine.metadata.name));
    // Reachability is declared by the `fabric.reachability` label (relay/private),
    // defaulting to public — so a NAT'd or relay node is just a labeled machine.
    let reachability = match machine
        .metadata
        .labels
        .get("fabric.reachability")
        .map(String::as_str)
    {
        Some("relay") => Reachability::Relay,
        Some("private") => Reachability::Private,
        _ => Reachability::Public,
    };
    FabricNode::from_keypair(
        &KeyPair::from_seed_name(&machine.metadata.name),
        vec![format!("{endpoint}:51820")],
    )
    .with_machine(machine.metadata.id.clone())
    .with_reachability(reachability)
}
