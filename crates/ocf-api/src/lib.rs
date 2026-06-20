//! # ocf-api
//!
//! The HTTP control surface of Open Compute Fabric.
//!
//! It exposes the [`FabricController`] — the object that owns every subsystem —
//! as a small REST API (see [`routes`]) and, optionally, serves the built Nuxt
//! frontend as static files. The `ocfd` binary constructs one controller and
//! hands it to [`serve`].

pub mod config;
pub mod controller;
pub mod dto;
pub mod error;
pub mod fleet;
pub mod persist;
pub mod routes;

pub use config::ControllerConfig;
pub use controller::FabricController;
pub use error::{ApiError, ApiResult};

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use ocf_core::error::Result;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

/// Assemble the full application router: the REST API, request tracing, a
/// permissive CORS policy (so the Nuxt dev server can call it cross-origin),
/// and — when `static_dir` is provided and exists — the built frontend with an
/// SPA fallback to `index.html`.
pub fn build_app(controller: Arc<FabricController>, static_dir: Option<PathBuf>) -> Router {
    let mut app = routes::api_router(controller);

    if let Some(dir) = static_dir {
        if dir.is_dir() {
            let index = dir.join("index.html");
            let serve = ServeDir::new(&dir).fallback(ServeFile::new(index));
            app = app.fallback_service(serve);
        } else {
            tracing::warn!(dir = %dir.display(), "static dir not found; serving API only");
        }
    }

    app.layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
}

/// Bind `addr` and serve the fabric API (and optional frontend) until the
/// process is stopped.
pub async fn serve(
    addr: SocketAddr,
    controller: Arc<FabricController>,
    static_dir: Option<PathBuf>,
) -> Result<()> {
    // Start the membership failure detector before serving requests.
    tokio::spawn(controller.clone().run_failure_detector());

    let app = build_app(controller, static_dir);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "ocf-api listening");
    axum::serve(listener, app).await?;
    Ok(())
}
