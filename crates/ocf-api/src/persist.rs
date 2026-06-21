//! Snapshot persistence: write the declarative control-plane state into the
//! durable [`StateStore`](ocf_store::StateStore) and reload it on boot.
//!
//! This is **node-local** durability: a single node's reboot preserves its
//! view of the fleet. (Surviving the loss of the node itself — fleet-global
//! consistency — is the Raft layer's job; see `ocf-consensus`.) State is
//! snapshotted per resource kind into a named collection, keyed by id, so a
//! restart restores the exact same resources (same ids) rather than re-seeding.

use crate::controller::FabricController;
use ocf_core::prelude::*;
use ocf_store::StateStoreExt;

use ocf_authz::{Group, Role, RoleBinding, User};
use ocf_disk::PhysicalDisk;
use ocf_loadbalancer::LoadBalancer;
use ocf_network::{Subnet, Vpc};
use ocf_runtime::{NetworkAttachment, RuntimeKind, Workload};
use ocf_topology::{Datacenter, Machine, Rack, Region};

impl FabricController {
    /// Persist one resource by proposing it through Raft. Once committed by a
    /// quorum, the state-machine applies it into this (and every) node's
    /// [`StateStore`].
    async fn persist_put<T: Serialize>(&self, collection: &str, key: &str, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        self.consensus.put(collection, key, bytes).await?;
        Ok(())
    }

    /// Write the entire control-plane state to the durable, replicated store.
    pub async fn persist(&self) -> Result<()> {
        let tree = self.topology.tree().await?;
        for rn in &tree.regions {
            self.persist_put("regions", rn.region.metadata.id.as_str(), &rn.region)
                .await?;
            for dn in &rn.datacenters {
                self.persist_put("datacenters", dn.datacenter.metadata.id.as_str(), &dn.datacenter)
                    .await?;
                for rk in &dn.racks {
                    self.persist_put("racks", rk.rack.metadata.id.as_str(), &rk.rack)
                        .await?;
                    for m in &rk.machines {
                        self.persist_put("machines", m.metadata.id.as_str(), m).await?;
                    }
                }
            }
        }

        for w in self.all_workloads().await {
            self.persist_put("workloads", w.metadata.id.as_str(), &w).await?;
        }

        for vpc in self.network.list_vpcs().await? {
            self.persist_put("vpcs", vpc.metadata.id.as_str(), &vpc).await?;
            for subnet in self.network.list_subnets(&vpc.metadata.id).await? {
                self.persist_put("subnets", subnet.metadata.id.as_str(), &subnet)
                    .await?;
            }
        }

        for lb in self.loadbalancers.list().await? {
            self.persist_put("loadbalancers", lb.metadata.id.as_str(), &lb)
                .await?;
        }

        for u in self.rbac.list_users() {
            self.persist_put("rbac_users", &u.username, &u).await?;
        }
        for r in self.rbac.list_roles() {
            self.persist_put("rbac_roles", &r.metadata.name, &r).await?;
        }
        for g in self.rbac.list_groups() {
            self.persist_put("rbac_groups", &g.metadata.name, &g).await?;
        }
        for b in self.rbac.list_bindings() {
            self.persist_put("rbac_bindings", b.id.as_str(), &b).await?;
        }

        for d in self.all_disks().await? {
            self.persist_put("disks", &d.serial, &d).await?;
        }

        // Workload → subnet attachments (the IP/egress binding lives here, not in
        // the stateless runtime providers).
        let attachments = self.attachments.read().clone();
        for (wid, att) in &attachments {
            self.persist_put("workload_networks", wid.as_str(), att).await?;
        }

        tracing::info!("fabric state committed through raft and persisted");
        Ok(())
    }

    /// Reload the control-plane state from the durable store into the live
    /// subsystems. Mirrors the demo seed, but from persisted data, so resource
    /// ids are preserved across a reboot.
    pub async fn restore(&self) -> Result<()> {
        let store = &self.store;
        let topo = self.topology.store();

        for r in store.list_json::<Region>("regions")? {
            topo.put_region(r).await?;
        }
        for d in store.list_json::<Datacenter>("datacenters")? {
            topo.put_datacenter(d).await?;
        }
        for rk in store.list_json::<Rack>("racks")? {
            topo.put_rack(rk).await?;
        }
        for m in store.list_json::<Machine>("machines")? {
            topo.put_machine(m).await?;
        }

        for w in store.list_json::<Workload>("workloads")? {
            // The persisted snapshot records kind but not which container
            // backend; containers are restored onto `docker`, VMs onto `qemu`.
            // Best-effort: a node lacking the runtime logs and continues.
            let provider = match w.kind {
                RuntimeKind::Container => self.runtimes.get("docker")?,
                RuntimeKind::VirtualMachine => self.runtimes.get("qemu")?,
            };
            crate::controller::spawn_workload(&provider, w).await;
        }

        // Re-program the SDN dataplane; tolerate nodes without ip/ovs.
        for vpc in store.list_json::<Vpc>("vpcs")? {
            if let Err(e) = self.network.create_vpc(vpc).await {
                tracing::warn!(error = %e, "vpc restore: dataplane programming failed");
            }
        }
        for subnet in store.list_json::<Subnet>("subnets")? {
            if let Err(e) = self.network.create_subnet(subnet).await {
                tracing::warn!(error = %e, "subnet restore: dataplane programming failed");
            }
        }

        for lb in store.list_json::<LoadBalancer>("loadbalancers")? {
            self.loadbalancers.create(lb).await?;
        }

        for u in store.list_json::<User>("rbac_users")? {
            self.rbac.put_user(u);
        }
        for r in store.list_json::<Role>("rbac_roles")? {
            self.rbac.put_role(r);
        }
        for g in store.list_json::<Group>("rbac_groups")? {
            self.rbac.put_group(g);
        }
        for b in store.list_json::<RoleBinding>("rbac_bindings")? {
            self.rbac.add_binding(b);
        }

        for d in store.list_json::<PhysicalDisk>("disks")? {
            self.disk_mgr.seed(d);
        }

        // Workload → subnet attachments. The store keys by workload id; reserve
        // each restored address in IPAM so the pool reflects what's already
        // assigned (subnets/allocators were restored just above).
        for (wid, bytes) in store.list("workload_networks")? {
            match serde_json::from_slice::<NetworkAttachment>(&bytes) {
                Ok(att) => {
                    if let Some(addr) = &att.address {
                        let _ = self.network.reserve_address(&att.subnet_id, addr);
                    }
                    self.attachments.write().insert(Id::from(wid), att);
                }
                Err(e) => tracing::warn!(workload = %wid, error = %e, "skipping undecodable attachment"),
            }
        }

        tracing::info!("fabric state restored from durable store");
        Ok(())
    }
}
