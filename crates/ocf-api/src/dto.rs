//! Small response shapes the API returns that aren't domain resources.

use serde::Serialize;

/// `GET /api/v1/health`
#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub subsystems: Vec<&'static str>,
}

/// One registered plugin provider, as surfaced by `GET /api/v1/providers`.
#[derive(Serialize)]
pub struct ProviderInfo {
    pub name: String,
    pub description: String,
}

/// A registry's worth of providers, grouped by the contract they implement.
#[derive(Serialize)]
pub struct ProviderGroup {
    pub contract: &'static str,
    pub providers: Vec<ProviderInfo>,
}

/// One runtime backend, as surfaced by `GET /api/v1/runtimes`.
#[derive(Serialize)]
pub struct RuntimeInfo {
    pub name: String,
    pub description: String,
    pub kind: &'static str,
    pub supports_migration: bool,
}
