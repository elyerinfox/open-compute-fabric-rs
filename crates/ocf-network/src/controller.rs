//! The high-level network controller.
//!
//! [`NetworkController`] is the fleet-wide control plane for the SDN overlay. It
//! owns the authoritative in-memory state (VPCs, subnets, routes, policies) and
//! a [`Registry`] of dataplane [`NetworkBackend`]s. Because the overlay spans
//! the whole fleet, every mutation "affects all machines": after recording the
//! desired state the controller fans the operation out to *every* registered
//! backend so each host programs its local dataplane.

use crate::backend::NetworkBackend;
use crate::ipam::SubnetAllocator;
use crate::model::{AclScope, EgressMode, FirewallPolicy, Route, Subnet, Vpc};
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Authoritative store + fan-out controller for the network overlay.
pub struct NetworkController {
    vpcs: RwLock<HashMap<Id, Vpc>>,
    subnets: RwLock<HashMap<Id, Subnet>>,
    routes: RwLock<HashMap<Id, Route>>,
    policies: RwLock<HashMap<Id, FirewallPolicy>>,
    /// Per-subnet IP address allocators, keyed by subnet id.
    ipam: RwLock<HashMap<Id, SubnetAllocator>>,
    backends: Arc<Registry<dyn NetworkBackend>>,
}

impl NetworkController {
    /// Build a controller over the given dataplane backend registry.
    pub fn new(backends: Arc<Registry<dyn NetworkBackend>>) -> Self {
        NetworkController {
            vpcs: RwLock::new(HashMap::new()),
            subnets: RwLock::new(HashMap::new()),
            routes: RwLock::new(HashMap::new()),
            policies: RwLock::new(HashMap::new()),
            ipam: RwLock::new(HashMap::new()),
            backends,
        }
    }

    /// The backend registry this controller fans operations out to.
    pub fn backends(&self) -> &Arc<Registry<dyn NetworkBackend>> {
        &self.backends
    }

    // ---- VPCs -------------------------------------------------------------

    /// Create a VPC and program it onto every machine. Fails if the id exists.
    pub async fn create_vpc(&self, vpc: Vpc) -> Result<Vpc> {
        let id = vpc.metadata.id.clone();
        if self.vpcs.read().contains_key(&id) {
            return Err(Error::already_exists(format!("vpc {id}")));
        }
        self.vpcs.write().insert(id, vpc.clone());
        // Desired state is recorded centrally; programming each host's dataplane
        // is best-effort so a host lacking iproute2/OVS (or the control node
        // itself, on a non-Linux box) does not fail the API call. Hosts converge
        // when their backend can program.
        if let Err(e) = self
            .fan_out("apply_vpc", &vpc.metadata.name, |b| {
                let vpc = vpc.clone();
                async move { b.apply_vpc(&vpc).await }
            })
            .await
        {
            tracing::warn!(vpc = %vpc.metadata.name, error = %e, "vpc dataplane programming failed (state recorded)");
        }
        Ok(vpc)
    }

    /// Stitch a VPC's overlay across hosts by programming the remote VTEP peers
    /// (the underlay addresses of the other fleet nodes) onto every backend.
    /// Best-effort: a host that can't program FDB/tunnels logs and is skipped.
    pub async fn refresh_vpc_peers(&self, vpc_id: &Id, peer_vteps: &[String]) -> Result<()> {
        let vpc = self.get_vpc(vpc_id).await?;
        let backends = self.backends.all();
        tracing::info!(
            operation = "apply_vpc_peers",
            vpc = %vpc.metadata.name,
            peers = peer_vteps.len(),
            machines = backends.len(),
            "fanning out VXLAN peer set to all machines"
        );
        for backend in backends {
            if let Err(e) = backend.apply_vpc_peers(&vpc, peer_vteps).await {
                tracing::warn!(vpc = %vpc.metadata.name, error = %e, "VXLAN peer programming failed on a backend");
            }
        }
        Ok(())
    }

    pub async fn get_vpc(&self, id: &Id) -> Result<Vpc> {
        self.vpcs
            .read()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("vpc {id}")))
    }

    pub async fn list_vpcs(&self) -> Result<Vec<Vpc>> {
        Ok(self.vpcs.read().values().cloned().collect())
    }

    /// Delete a VPC. Refuses while any subnet still references it.
    pub async fn delete_vpc(&self, id: &Id) -> Result<()> {
        if self
            .subnets
            .read()
            .values()
            .any(|s| &s.vpc_id == id)
        {
            return Err(Error::Conflict(format!(
                "vpc {id} still has subnets"
            )));
        }
        if self.vpcs.write().remove(id).is_none() {
            return Err(Error::not_found(format!("vpc {id}")));
        }
        tracing::info!(vpc = %id, "deleted vpc");
        Ok(())
    }

    // ---- Subnets ----------------------------------------------------------

    /// Create a subnet (validating its VPC exists) and program it everywhere.
    pub async fn create_subnet(&self, subnet: Subnet) -> Result<Subnet> {
        if !self.vpcs.read().contains_key(&subnet.vpc_id) {
            return Err(Error::not_found(format!("vpc {}", subnet.vpc_id)));
        }
        let id = subnet.metadata.id.clone();
        if self.subnets.read().contains_key(&id) {
            return Err(Error::already_exists(format!("subnet {id}")));
        }
        self.subnets.write().insert(id.clone(), subnet.clone());
        // Stand up the subnet's IP allocator (the gateway .1 is auto-reserved).
        match SubnetAllocator::new(&subnet.cidr) {
            Ok(alloc) => {
                self.ipam.write().insert(id, alloc);
            }
            Err(e) => {
                tracing::warn!(subnet = %subnet.metadata.name, error = %e, "could not build IPAM allocator");
            }
        }
        // Best-effort dataplane programming (see `create_vpc`): record desired
        // state regardless of whether each host can program it right now.
        if let Err(e) = self
            .fan_out("apply_subnet", &subnet.metadata.name, |b| {
                let subnet = subnet.clone();
                async move { b.apply_subnet(&subnet).await }
            })
            .await
        {
            tracing::warn!(subnet = %subnet.metadata.name, error = %e, "subnet dataplane programming failed (state recorded)");
        }
        // Program egress NAT for a Nat-capable subnet. Best-effort: a host that
        // can't program NAT should not fail subnet creation. No workloads have
        // attached yet, so the opt-in allow-list is empty (default-deny).
        if subnet.egress.provides_egress() {
            if let Err(e) = self.fan_out_egress(&subnet, &[]).await {
                tracing::warn!(
                    subnet = %subnet.metadata.name,
                    error = %e,
                    "egress NAT programming failed (subnet still created)"
                );
            }
        }
        Ok(subnet)
    }

    /// Change a subnet's egress capability and re-program the dataplane with the
    /// given opted-in workload addresses. Returns the updated subnet.
    pub async fn set_subnet_egress(
        &self,
        subnet_id: &Id,
        mode: EgressMode,
        allowed: &[String],
    ) -> Result<Subnet> {
        let subnet = {
            let mut subnets = self.subnets.write();
            let s = subnets
                .get_mut(subnet_id)
                .ok_or_else(|| Error::not_found(format!("subnet {subnet_id}")))?;
            s.egress = mode;
            s.metadata.touch();
            s.clone()
        };
        // The capability change is recorded; programming is best-effort so a host
        // without nft/iproute2 doesn't fail the API call.
        if let Err(e) = self.fan_out_egress(&subnet, allowed).await {
            tracing::warn!(subnet = %subnet.metadata.name, error = %e, "egress reprogramming failed (capability recorded)");
        }
        Ok(subnet)
    }

    /// Re-program a subnet's egress data path with the current set of opted-in
    /// workload addresses (called when a workload attaches, detaches, or changes
    /// its egress opt-in). Looks the subnet up and fans `apply_egress` out.
    pub async fn refresh_subnet_egress(
        &self,
        subnet_id: &Id,
        allowed: &[String],
    ) -> Result<()> {
        let subnet = self.get_subnet(subnet_id).await?;
        self.fan_out_egress(&subnet, allowed).await
    }

    // ---- IPAM -------------------------------------------------------------

    /// Allocate the next free host address in `subnet_id`. Fails if the subnet is
    /// unknown or its address pool is exhausted.
    pub fn allocate_address(&self, subnet_id: &Id) -> Result<String> {
        let mut ipam = self.ipam.write();
        let alloc = ipam
            .get_mut(subnet_id)
            .ok_or_else(|| Error::not_found(format!("subnet {subnet_id} IPAM")))?;
        alloc.allocate()
    }

    /// Mark `address` as in use in `subnet_id` (used when restoring persisted
    /// attachments so the pool reflects already-assigned addresses).
    pub fn reserve_address(&self, subnet_id: &Id, address: &str) -> Result<()> {
        let mut ipam = self.ipam.write();
        let alloc = ipam
            .get_mut(subnet_id)
            .ok_or_else(|| Error::not_found(format!("subnet {subnet_id} IPAM")))?;
        alloc.reserve(address)
    }

    /// Return `address` to `subnet_id`'s free pool. No-op if the subnet or
    /// address is unknown.
    pub fn release_address(&self, subnet_id: &Id, address: &str) {
        if let Some(alloc) = self.ipam.write().get_mut(subnet_id) {
            alloc.release(address);
        }
    }

    pub async fn get_subnet(&self, id: &Id) -> Result<Subnet> {
        self.subnets
            .read()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("subnet {id}")))
    }

    pub async fn list_subnets(&self, vpc_id: &Id) -> Result<Vec<Subnet>> {
        Ok(self
            .subnets
            .read()
            .values()
            .filter(|s| &s.vpc_id == vpc_id)
            .cloned()
            .collect())
    }

    // ---- Routes -----------------------------------------------------------

    /// Add a route (validating its subnet exists) and install it everywhere.
    pub async fn add_route(&self, route: Route) -> Result<Route> {
        if !self.subnets.read().contains_key(&route.subnet_id) {
            return Err(Error::not_found(format!("subnet {}", route.subnet_id)));
        }
        self.routes
            .write()
            .insert(route.id.clone(), route.clone());
        let label = route.dest_cidr.clone();
        self.fan_out("apply_route", &label, |b| {
            let route = route.clone();
            async move { b.apply_route(&route).await }
        })
        .await?;
        Ok(route)
    }

    pub async fn list_routes(&self, subnet_id: &Id) -> Result<Vec<Route>> {
        Ok(self
            .routes
            .read()
            .values()
            .filter(|r| &r.subnet_id == subnet_id)
            .cloned()
            .collect())
    }

    // ---- Firewall policies ------------------------------------------------

    /// Apply (create or replace) a firewall policy, validating its scope target
    /// exists, then render it onto every machine.
    pub async fn apply_policy(&self, policy: FirewallPolicy) -> Result<FirewallPolicy> {
        match &policy.scope {
            AclScope::Vpc(vpc_id) => {
                if !self.vpcs.read().contains_key(vpc_id) {
                    return Err(Error::not_found(format!("vpc {vpc_id}")));
                }
            }
            AclScope::Subnet(subnet_id) => {
                if !self.subnets.read().contains_key(subnet_id) {
                    return Err(Error::not_found(format!("subnet {subnet_id}")));
                }
            }
        }
        self.policies
            .write()
            .insert(policy.id.clone(), policy.clone());
        let label = policy.id.to_string();
        self.fan_out("apply_policy", &label, |b| {
            let policy = policy.clone();
            async move { b.apply_policy(&policy).await }
        })
        .await?;
        Ok(policy)
    }

    pub async fn list_policies(&self) -> Result<Vec<FirewallPolicy>> {
        Ok(self.policies.read().values().cloned().collect())
    }

    pub async fn delete_policy(&self, id: &Id) -> Result<()> {
        if self.policies.write().remove(id).is_none() {
            return Err(Error::not_found(format!("policy {id}")));
        }
        tracing::info!(policy = %id, "deleted firewall policy");
        Ok(())
    }

    // ---- Fan-out ----------------------------------------------------------

    /// Run `op` against every registered backend, logging the fleet-wide
    /// fan-out. The first backend failure aborts and is surfaced to the caller.
    async fn fan_out<F, Fut>(&self, op: &str, target: &str, op_fn: F) -> Result<()>
    where
        F: Fn(Arc<dyn NetworkBackend>) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let backends = self.backends.all();
        tracing::info!(
            operation = op,
            target = target,
            machines = backends.len(),
            "fanning out network change to all machines"
        );
        for backend in backends {
            op_fn(backend).await?;
        }
        Ok(())
    }

    /// Fan a subnet's egress data path out to every backend. Separate from
    /// [`Self::fan_out`] because egress takes the subnet plus the opt-in
    /// allow-list rather than a single-resource closure.
    async fn fan_out_egress(&self, subnet: &Subnet, allowed: &[String]) -> Result<()> {
        let backends = self.backends.all();
        tracing::info!(
            operation = "apply_egress",
            subnet = %subnet.metadata.name,
            mode = ?subnet.egress,
            allowed = allowed.len(),
            machines = backends.len(),
            "fanning out egress change to all machines"
        );
        for backend in backends {
            backend.apply_egress(subnet, allowed).await?;
        }
        Ok(())
    }
}
