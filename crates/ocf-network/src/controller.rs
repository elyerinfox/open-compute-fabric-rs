//! The high-level network controller.
//!
//! [`NetworkController`] is the fleet-wide control plane for the SDN overlay. It
//! owns the authoritative in-memory state (VPCs, subnets, routes, policies) and
//! a [`Registry`] of dataplane [`NetworkBackend`]s. Because the overlay spans
//! the whole fleet, every mutation "affects all machines": after recording the
//! desired state the controller fans the operation out to *every* registered
//! backend so each host programs its local dataplane.

use crate::backend::NetworkBackend;
use crate::model::{AclScope, FirewallPolicy, Route, Subnet, Vpc};
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
        self.fan_out("apply_vpc", &vpc.metadata.name, |b| {
            let vpc = vpc.clone();
            async move { b.apply_vpc(&vpc).await }
        })
        .await?;
        Ok(vpc)
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
        self.subnets.write().insert(id, subnet.clone());
        self.fan_out("apply_subnet", &subnet.metadata.name, |b| {
            let subnet = subnet.clone();
            async move { b.apply_subnet(&subnet).await }
        })
        .await?;
        Ok(subnet)
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
}
