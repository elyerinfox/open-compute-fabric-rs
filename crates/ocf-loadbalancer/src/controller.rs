//! The high-level load-balancer controller.
//!
//! [`LoadBalancerController`] owns the in-memory set of [`LoadBalancer`]
//! resources (behind a `parking_lot::RwLock`) and exposes async CRUD plus
//! [`LoadBalancerController::resolve`], which answers "for this load balancer and
//! this client, which backend should serve the request?" by combining the LB's
//! `placement` constraint with the policy-aware [`select_backend`] routing core.

use crate::model::{Backend, ClientContext, LoadBalancer};
use crate::routing::select_backend;
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;

/// Async CRUD + resolution over a set of load balancers.
///
/// The store is in-memory and single-node, matching the rest of the fabric's
/// default backends; a production deployment would swap the store for a durable
/// one without changing this surface.
#[derive(Default)]
pub struct LoadBalancerController {
    load_balancers: RwLock<HashMap<Id, LoadBalancer>>,
}

impl LoadBalancerController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a load balancer. Fails if one with the same id already exists.
    pub async fn create(&self, lb: LoadBalancer) -> Result<LoadBalancer> {
        let id = lb.metadata.id.clone();
        let mut store = self.load_balancers.write();
        if store.contains_key(&id) {
            return Err(Error::already_exists(format!("load balancer {id}")));
        }
        tracing::info!(lb = %id, name = %lb.metadata.name, "creating load balancer");
        store.insert(id, lb.clone());
        Ok(lb)
    }

    /// Fetch a load balancer by id.
    pub async fn get(&self, lb_id: &Id) -> Result<LoadBalancer> {
        self.load_balancers
            .read()
            .get(lb_id)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("load balancer {lb_id}")))
    }

    /// List every load balancer.
    pub async fn list(&self) -> Result<Vec<LoadBalancer>> {
        Ok(self.load_balancers.read().values().cloned().collect())
    }

    /// Replace an existing load balancer, stamping `updated_at`. Fails if it is
    /// not already present.
    pub async fn update(&self, mut lb: LoadBalancer) -> Result<LoadBalancer> {
        let id = lb.metadata.id.clone();
        let mut store = self.load_balancers.write();
        if !store.contains_key(&id) {
            return Err(Error::not_found(format!("load balancer {id}")));
        }
        lb.metadata.touch();
        tracing::info!(lb = %id, "updating load balancer");
        store.insert(id, lb.clone());
        Ok(lb)
    }

    /// Delete a load balancer by id. Fails if it does not exist.
    pub async fn delete(&self, lb_id: &Id) -> Result<()> {
        let mut store = self.load_balancers.write();
        if store.remove(lb_id).is_none() {
            return Err(Error::not_found(format!("load balancer {lb_id}")));
        }
        tracing::info!(lb = %lb_id, "deleting load balancer");
        Ok(())
    }

    /// Resolve a request to a concrete backend.
    ///
    /// Looks up the load balancer, drops any candidate the LB's `placement`
    /// scope does not admit (so a scoped LB never routes outside its scope),
    /// then applies the LB's [`RoutingPolicy`](crate::model::RoutingPolicy) via
    /// [`select_backend`]. Returns `None` when no admissible backend exists.
    pub async fn resolve(
        &self,
        lb_id: &Id,
        client: &ClientContext,
        backends: &[Backend],
    ) -> Result<Option<Backend>> {
        let lb = self.get(lb_id).await?;
        // Enforce placement: only backends the LB admits are eligible.
        let admissible: Vec<Backend> = backends
            .iter()
            .filter(|b| lb.admits(b))
            .cloned()
            .collect();
        Ok(select_backend(lb.policy, &admissible, client))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LbKind, Listener, RoutingPolicy};

    fn sample_lb() -> LoadBalancer {
        LoadBalancer::new("web", LbKind::Application, RoutingPolicy::LeastLoad)
            .with_listener(Listener::tls(443))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn crud_round_trip() {
        let controller = LoadBalancerController::new();
        let lb = controller.create(sample_lb()).await.unwrap();
        let id = lb.metadata.id.clone();

        assert_eq!(controller.list().await.unwrap().len(), 1);
        assert_eq!(controller.get(&id).await.unwrap().metadata.name, "web");

        // Duplicate create is rejected.
        assert!(controller.create(lb.clone()).await.is_err());

        let mut updated = lb.clone();
        updated.anycast = true;
        let updated = controller.update(updated).await.unwrap();
        assert!(updated.anycast);

        controller.delete(&id).await.unwrap();
        assert!(controller.get(&id).await.is_err());
        assert!(controller.delete(&id).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_respects_placement() {
        let controller = LoadBalancerController::new();
        let lb = LoadBalancer::new("eu-only", LbKind::Tcp, RoutingPolicy::LeastLoad)
            .with_placement(Scope::region("eu"));
        let lb = controller.create(lb).await.unwrap();

        let backends = vec![
            Backend::new(Id::named("us"), "us:80", Scope::region("us")).with_load(0.1),
            Backend::new(Id::named("eu"), "eu:80", Scope::region("eu")).with_load(0.9),
        ];

        // Even though `us` is less loaded, placement forbids it.
        let chosen = controller
            .resolve(&lb.metadata.id, &ClientContext::new(), &backends)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chosen.address, "eu:80");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_unknown_lb_errors() {
        let controller = LoadBalancerController::new();
        let res = controller
            .resolve(&Id::named("missing"), &ClientContext::new(), &[])
            .await;
        assert!(res.is_err());
    }
}
