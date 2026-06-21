//! # ocfd — the Open Compute Fabric daemon
//!
//! A single monolithic binary that builds the entire fabric control plane
//! ([`ocf_api::FabricController`]) — every subsystem with its built-in plugin
//! providers registered — and either serves the REST API + frontend or prints
//! its registered providers.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use ocf_api::{ControllerConfig, FabricController};

/// Open Compute Fabric daemon.
#[derive(Parser)]
#[command(name = "ocfd", version, about)]
struct Cli {
    /// This node's stable identity in the fleet.
    #[arg(long, env = "OCF_NODE_ID", default_value = "node-local", global = true)]
    node_id: String,
    /// Directory for durable state; omit to run fully in-memory.
    #[arg(long, env = "OCF_DATA_DIR", global = true)]
    data_dir: Option<PathBuf>,
    /// Seed peer(s) to contact when joining the mesh (comma-separated).
    #[arg(long = "seed", env = "OCF_SEEDS", value_delimiter = ',', global = true)]
    seeds: Vec<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the controller and serve the API (and the frontend, if built).
    Serve {
        /// Address to bind the HTTP API on.
        #[arg(long, env = "OCF_BIND", default_value = "0.0.0.0:8080")]
        bind: SocketAddr,
        /// Directory of built frontend assets to serve (e.g. `web/.output/public`).
        #[arg(long, env = "OCF_STATIC_DIR")]
        static_dir: Option<PathBuf>,
    },
    /// Print every pluggable provider registered across all subsystems.
    Providers,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let config = ControllerConfig {
        node_id: cli.node_id.clone(),
        data_dir: cli.data_dir.clone(),
        seeds: cli.seeds.clone(),
        ..Default::default()
    };

    tracing::info!(node_id = %config.node_id, "building fabric controller");
    let controller = Arc::new(FabricController::bootstrap(config).await?);

    match cli.command {
        Command::Serve { bind, static_dir } => {
            ocf_api::serve(bind, controller, static_dir).await?;
        }
        Command::Providers => {
            print_providers(&controller);
        }
    }
    Ok(())
}

/// Walk every subsystem registry and print the registered providers, which is
/// the most direct demonstration that the whole control plane is plugin-driven.
fn print_providers(c: &FabricController) {
    fn show<T: ocf_core::registry::Provider + ?Sized>(
        contract: &str,
        items: Vec<Arc<T>>,
    ) {
        println!("{contract}:");
        if items.is_empty() {
            println!("  (none)");
        }
        for p in items {
            println!("  - {:<16} {}", p.name(), p.description());
        }
    }

    show("RuntimeProvider", c.runtimes.all());
    show("Authenticator", c.authenticators.all());
    show("InventoryCollector", c.inventory_controllers.collectors.all());
    show("IpmiController", c.inventory_controllers.ipmi.all());
    show("CertificateProvider", c.cert_providers.all());
    show("DnsProvider", c.dns_providers.all());
    show("HealthCheck", c.health.checks().all());
    show("PackageManager", c.platform.managers().all());
}
