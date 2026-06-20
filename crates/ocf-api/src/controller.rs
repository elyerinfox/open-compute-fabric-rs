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
use ocf_fabric::{FabricMesh, FabricNode, FabricTransport, KeyPair, Membership, NodeId};
use ocf_inventory::InventoryService;
use ocf_kernel::KernelManager;
use ocf_loadbalancer::{
    CertificateProvider, DnsProvider, LbKind, Listener, LoadBalancer, LoadBalancerController,
    RoutingPolicy,
};
use ocf_monitoring::MonitoringService;
use ocf_network::{NetworkBackend, NetworkController, Subnet, Vpc};
use ocf_runtime::{RuntimeProvider, Workload};
use ocf_store::{MemoryStateStore, RedbStateStore, StateStore};
use ocf_topology::{Datacenter, InMemoryTopology, Machine, Rack, Region, TopologyService, TopologyStore};

use crate::config::ControllerConfig;

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

        Ok(controller)
    }

    /// Collect every workload across every registered runtime backend.
    pub async fn all_workloads(&self) -> Vec<Workload> {
        let mut out = Vec::new();
        for provider in self.runtimes.all() {
            if let Ok(mut wls) = provider.list().await {
                out.append(&mut wls);
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
    network
        .create_subnet(Subnet::new(vpc_id.clone(), "web", "10.0.1.0/24", "ns-web"))
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
                .with_hostname("app.example.com"),
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
    FabricNode::from_keypair(
        &KeyPair::from_seed_name(&machine.metadata.name),
        vec![format!("{endpoint}:51820")],
    )
    .with_machine(machine.metadata.id.clone())
}
