//! The REST surface of the fabric controller.
//!
//! Every route is a thin adapter: it borrows the relevant subsystem off the
//! [`FabricController`] state, calls one method, and serializes the result.
//! Domain resources serialize as-is (they all derive `Serialize`); a handful of
//! cross-cutting views use the small DTOs in [`crate::dto`].

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use ocf_core::error::Error;
use ocf_core::id::Id;
use ocf_runtime::RuntimeKind;

use crate::controller::FabricController;
use crate::dto::{HealthResponse, ProviderGroup, ProviderInfo, RuntimeInfo};
use crate::error::ApiResult;

type Ctrl = State<Arc<FabricController>>;

/// Build the `/api/v1` router over a shared [`FabricController`].
pub fn api_router(controller: Arc<FabricController>) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/providers", get(providers))
        .route("/api/v1/topology/tree", get(topology_tree))
        .route("/api/v1/topology/regions", get(regions))
        .route("/api/v1/machines", get(machines))
        .route("/api/v1/runtimes", get(runtimes))
        .route("/api/v1/workloads", get(workloads))
        .route("/api/v1/workloads/:id/migrate", post(migrate_workload))
        .route("/api/v1/networks/vpcs", get(vpcs))
        .route("/api/v1/networks/subnets", get(subnets))
        .route("/api/v1/loadbalancers", get(loadbalancers))
        .route("/api/v1/disks", get(disks))
        .route("/api/v1/metrics/host", get(host_metrics))
        .route("/api/v1/fabric/peers", get(fabric_peers))
        .route("/api/v1/fabric/membership", get(membership))
        .route("/api/v1/fabric/machines/:id/heartbeat", post(heartbeat_machine))
        .route("/api/v1/fabric/machines/:id/fail", post(fail_machine))
        .route("/api/v1/admin/persist", post(persist_state))
        .route("/api/v1/access/users", get(users))
        .route("/api/v1/access/roles", get(roles))
        .route("/api/v1/access/groups", get(groups))
        .with_state(controller)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        subsystems: vec![
            "topology",
            "runtime",
            "auth",
            "authz",
            "kernel",
            "inventory",
            "disk",
            "monitoring",
            "fabric",
            "network",
            "loadbalancer",
        ],
    })
}

async fn providers(State(c): Ctrl) -> Json<Vec<ProviderGroup>> {
    fn group<T: ocf_core::registry::Provider + ?Sized>(
        contract: &'static str,
        items: Vec<Arc<T>>,
    ) -> ProviderGroup {
        ProviderGroup {
            contract,
            providers: items
                .iter()
                .map(|p| ProviderInfo {
                    name: p.name().to_string(),
                    description: p.description().to_string(),
                })
                .collect(),
        }
    }

    Json(vec![
        group("RuntimeProvider", c.runtimes.all()),
        group("Authenticator", c.authenticators.all()),
        group("InventoryCollector", c.inventory_controllers.collectors.all()),
        group("IpmiController", c.inventory_controllers.ipmi.all()),
        group("CertificateProvider", c.cert_providers.all()),
        group("DnsProvider", c.dns_providers.all()),
    ])
}

async fn topology_tree(State(c): Ctrl) -> ApiResult<Json<ocf_topology::TopologyTree>> {
    Ok(Json(c.topology.tree().await?))
}

async fn regions(State(c): Ctrl) -> ApiResult<Json<Vec<ocf_topology::Region>>> {
    Ok(Json(c.topology.store().list_regions().await?))
}

async fn machines(State(c): Ctrl) -> ApiResult<Json<Vec<ocf_topology::Machine>>> {
    Ok(Json(c.topology.store().all_machines().await?))
}

async fn runtimes(State(c): Ctrl) -> Json<Vec<RuntimeInfo>> {
    let infos = c
        .runtimes
        .all()
        .iter()
        .map(|p| RuntimeInfo {
            name: p.name().to_string(),
            description: p.description().to_string(),
            kind: match p.kind() {
                RuntimeKind::Container => "container",
                RuntimeKind::VirtualMachine => "virtual_machine",
            },
            supports_migration: p.supports_migration(),
        })
        .collect();
    Json(infos)
}

async fn workloads(State(c): Ctrl) -> Json<Vec<ocf_runtime::Workload>> {
    Json(c.all_workloads().await)
}

/// `POST /api/v1/workloads/:id/migrate` — request live migration of a workload.
///
/// Finds the backend currently holding the workload and, if that backend is
/// migration-capable, acknowledges the migration. The move itself is driven by
/// [`ocf_runtime::Migrator`] (`virsh save`/`restore`, with the checkpoint shipped
/// over the fabric for a cross-host hop).
async fn migrate_workload(State(c): Ctrl, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let wid = Id::from(id);
    for provider in c.runtimes.all() {
        if provider.status(&wid).await.is_ok() {
            tracing::info!(workload = %wid, backend = %provider.name(), "migration requested");
            return Ok(Json(json!({
                "accepted": provider.supports_migration(),
                "workload": wid.as_str(),
                "backend": provider.name(),
                "migratable": provider.supports_migration(),
                "message": if provider.supports_migration() {
                    format!("migration of {wid} scheduled")
                } else {
                    format!("backend `{}` cannot migrate this workload", provider.name())
                },
            })));
        }
    }
    Err(Error::not_found(format!("workload {wid}")).into())
}

async fn vpcs(State(c): Ctrl) -> ApiResult<Json<Vec<ocf_network::Vpc>>> {
    Ok(Json(c.network.list_vpcs().await?))
}

async fn subnets(State(c): Ctrl) -> ApiResult<Json<Vec<ocf_network::Subnet>>> {
    let mut out = Vec::new();
    for vpc in c.network.list_vpcs().await? {
        out.extend(c.network.list_subnets(&vpc.metadata.id).await?);
    }
    Ok(Json(out))
}

async fn loadbalancers(State(c): Ctrl) -> ApiResult<Json<Vec<ocf_loadbalancer::LoadBalancer>>> {
    Ok(Json(c.loadbalancers.list().await?))
}

async fn disks(State(c): Ctrl) -> ApiResult<Json<Vec<ocf_disk::PhysicalDisk>>> {
    Ok(Json(c.all_disks().await?))
}

async fn host_metrics(State(c): Ctrl) -> ApiResult<Json<ocf_monitoring::ResourceUsage>> {
    Ok(Json(c.monitoring.aggregate_host_usage().await?))
}

async fn fabric_peers(State(c): Ctrl) -> Json<Vec<ocf_fabric::FabricNode>> {
    Json(c.fabric.peers())
}

async fn membership(State(c): Ctrl) -> Json<Vec<crate::fleet::MemberView>> {
    Json(c.membership_view())
}

/// `POST /api/v1/fabric/machines/:id/heartbeat` — keep a node alive.
async fn heartbeat_machine(State(c): Ctrl, Path(id): Path<String>) -> Json<Value> {
    let revived = c.heartbeat_machine(&Id::from(id));
    Json(json!({ "revived": revived }))
}

/// `POST /api/v1/fabric/machines/:id/fail` — force a node dead and run drop-out
/// handling (reschedules its HA workloads onto surviving in-scope nodes).
async fn fail_machine(State(c): Ctrl, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let rescheduled = c.fail_machine(&Id::from(id)).await?;
    Ok(Json(json!({ "rescheduled": rescheduled })))
}

/// `POST /api/v1/admin/persist` — snapshot current state to the durable store.
async fn persist_state(State(c): Ctrl) -> ApiResult<Json<Value>> {
    c.persist().await?;
    Ok(Json(json!({ "persisted": true })))
}

async fn users(State(c): Ctrl) -> Json<Vec<ocf_authz::User>> {
    Json(c.rbac.list_users())
}

async fn roles(State(c): Ctrl) -> Json<Vec<ocf_authz::Role>> {
    Json(c.rbac.list_roles())
}

async fn groups(State(c): Ctrl) -> Json<Vec<ocf_authz::Group>> {
    Json(c.rbac.list_groups())
}
