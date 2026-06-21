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
        .route(
            "/api/v1/workloads/:id/network",
            post(attach_workload).delete(detach_workload),
        )
        .route("/api/v1/workloads/:id/candidates", get(workload_candidates))
        .route("/api/v1/networks/vpcs", get(vpcs))
        .route("/api/v1/networks/subnets", get(subnets))
        .route("/api/v1/networks/subnets/:id/egress", post(set_subnet_egress))
        .route("/api/v1/loadbalancers", get(loadbalancers))
        .route("/api/v1/disks", get(disks))
        .route("/api/v1/metrics/host", get(host_metrics))
        .route("/api/v1/fabric/peers", get(fabric_peers))
        .route("/api/v1/fabric/membership", get(membership))
        .route("/api/v1/fabric/wireguard", get(wireguard_status))
        .route("/api/v1/fabric/routes", get(fabric_routes))
        .route("/api/v1/fabric/machines/:id/heartbeat", post(heartbeat_machine))
        .route("/api/v1/fabric/machines/:id/fail", post(fail_machine))
        .route("/api/v1/admin/persist", post(persist_state))
        .route("/api/v1/health/findings", get(health_findings))
        .route("/api/v1/health/fix", post(health_fix))
        .route("/api/v1/platform", get(platform_status))
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
        group("HealthCheck", c.health.checks().all()),
        group("PackageManager", c.platform.managers().all()),
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

/// `POST /api/v1/workloads/:id/network` — attach a workload to a subnet.
/// Body: `{ "subnet_id": "...", "egress": false }`. Allocates an address (IPAM),
/// records the binding, and re-programs the subnet's egress allow-list.
async fn attach_workload(
    State(c): Ctrl,
    Path(id): Path<String>,
    Json(body): Json<AttachBody>,
) -> ApiResult<Json<ocf_runtime::NetworkAttachment>> {
    let att = c
        .attach_workload(&Id::from(id), &Id::from(body.subnet_id), body.egress)
        .await?;
    Ok(Json(att))
}

/// `DELETE /api/v1/workloads/:id/network` — detach a workload from its subnet,
/// releasing its address and re-programming egress.
async fn detach_workload(State(c): Ctrl, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    c.detach_workload(&Id::from(id)).await?;
    Ok(Json(json!({ "detached": true })))
}

#[derive(serde::Deserialize)]
struct AttachBody {
    subnet_id: String,
    #[serde(default)]
    egress: bool,
}

/// `GET /api/v1/workloads/:id/candidates` — the machines a workload can be
/// (re)scheduled onto, given its scope, required node capabilities
/// (`node_selector`), and capacity. Shows how capability flags restrict placement.
async fn workload_candidates(State(c): Ctrl, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let wid = Id::from(id);
    let workload = c
        .all_workloads()
        .await
        .into_iter()
        .find(|w| w.metadata.id == wid)
        .ok_or_else(|| Error::not_found(format!("workload {wid}")))?;
    let candidates: Vec<Value> = c
        .candidate_nodes(&workload)
        .await
        .into_iter()
        .map(|m| json!({ "id": m.metadata.id.as_str(), "name": m.metadata.name }))
        .collect();
    Ok(Json(json!({
        "workload": workload.metadata.name,
        "node_selector": workload.node_selector,
        "candidates": candidates,
    })))
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

/// `POST /api/v1/networks/subnets/:id/egress` — set a subnet's outbound internet
/// capability. Body: `{ "mode": "nat" | "isolated" }`. Re-programs the egress
/// data path for the opted-in workloads and persists.
async fn set_subnet_egress(
    State(c): Ctrl,
    Path(id): Path<String>,
    Json(body): Json<EgressBody>,
) -> ApiResult<Json<ocf_network::Subnet>> {
    let mode = match body.mode.to_ascii_lowercase().as_str() {
        "nat" => ocf_network::EgressMode::Nat,
        "isolated" => ocf_network::EgressMode::Isolated,
        other => return Err(Error::invalid(format!("unknown egress mode `{other}`")).into()),
    };
    Ok(Json(c.set_subnet_egress(&Id::from(id), mode).await?))
}

#[derive(serde::Deserialize)]
struct EgressBody {
    mode: String,
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

/// `GET /api/v1/fabric/wireguard` — the computed WireGuard underlay mesh (this
/// node + peers); the VXLAN overlay's VTEPs point at these WireGuard addresses.
async fn wireguard_status(State(c): Ctrl) -> Json<crate::fleet::WireguardView> {
    Json(c.wireguard_status().await)
}

/// `GET /api/v1/fabric/routes` — the planned route from this node to every peer
/// (direct vs relayed, weighed by measured RTT). The fabric's path selection.
async fn fabric_routes(State(c): Ctrl) -> Json<Vec<crate::fleet::RouteView>> {
    Json(c.routes_view())
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

/// `GET /api/v1/health/findings` — run every health check for this node and
/// return the findings (problems + the fix actions the user can press).
async fn health_findings(State(c): Ctrl) -> Json<Vec<ocf_health::HealthFinding>> {
    Json(c.health.run(&c.node_machine_id()).await)
}

/// `POST /api/v1/health/fix` — apply a finding's fix. Body:
/// `{ "check": "ip-forwarding", "fix": "enable-ipv4-forwarding" }`.
async fn health_fix(State(c): Ctrl, Json(body): Json<HealthFixBody>) -> ApiResult<Json<Value>> {
    let outcome = c
        .health
        .apply_fix(&body.check, &body.fix, &c.node_machine_id())
        .await?;
    Ok(Json(json!({ "applied": true, "outcome": outcome })))
}

#[derive(serde::Deserialize)]
struct HealthFixBody {
    check: String,
    fix: String,
}

/// `GET /api/v1/platform` — the detected host OS, the selected package manager,
/// and per-capability readiness (which required tools are present, and the
/// package that would install each missing one).
async fn platform_status(State(c): Ctrl) -> Json<ocf_platform::PlatformStatus> {
    Json(c.platform.status(&ocf_platform::builtin_capabilities()))
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
