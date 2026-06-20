//! # ocf-kernel
//!
//! Host-kernel control plane: the low-level knobs the fabric turns on each
//! machine. It bundles three contracts:
//!
//! * [`network::NetworkManager`] — IPv4 forwarding and software bridges.
//! * [`firewall::FirewallBackend`] — pluggable host packet filtering
//!   (`nftables` / `iptables`).
//! * [`service::ServiceManager`] — supervision and drift reconciliation of host
//!   daemons (`systemd`).
//!
//! Every OS-touching operation shells out to the real host tooling
//! (`ip`/`nft`/`iptables`/`systemctl`) or writes the relevant `/proc` knob. The
//! crate still *compiles* on any platform — `std::process` / `std::fs` are
//! cross-platform — but the commands only succeed on a Linux host that has the
//! tools installed; a missing binary or `/proc` path surfaces as a runtime
//! error, never a panic.
//!
//! [`KernelManager`] is the facade the controller wires up — it owns one
//! [`network::NetworkManager`], a [`Registry`] of firewall backends, and one
//! [`service::ServiceManager`].

pub mod firewall;
pub mod network;
pub mod service;

pub(crate) mod exec;

pub use firewall::{
    register_builtins, FirewallAction, FirewallBackend, FirewallRule, IptablesFirewall,
    NftablesFirewall,
};
pub use network::{LinuxNetworkManager, NetworkManager};
pub use service::{ReconcileReport, ServiceManager, ServiceState, SystemdServiceManager};

use ocf_core::prelude::*;
use std::sync::Arc;

/// Facade bundling the host-kernel subsystems for a single machine.
///
/// Holds the [`NetworkManager`], a [`Registry`] of [`FirewallBackend`]s (so the
/// active backend is selectable at runtime), and the [`ServiceManager`]. The
/// controller constructs one of these per managed host and drives it through the
/// helper methods below.
pub struct KernelManager {
    network: Arc<dyn NetworkManager>,
    firewalls: Registry<dyn FirewallBackend>,
    services: Arc<dyn ServiceManager>,
    /// Name of the firewall backend to use for [`KernelManager::apply_firewall`].
    active_firewall: String,
}

impl KernelManager {
    /// Construct a facade over the given subsystems, selecting `active_firewall`
    /// as the backend used by [`apply_firewall`](KernelManager::apply_firewall).
    pub fn new(
        network: Arc<dyn NetworkManager>,
        firewalls: Registry<dyn FirewallBackend>,
        services: Arc<dyn ServiceManager>,
        active_firewall: impl Into<String>,
    ) -> Self {
        KernelManager {
            network,
            firewalls,
            services,
            active_firewall: active_firewall.into(),
        }
    }

    /// Construct a facade with the bundled defaults: a [`LinuxNetworkManager`],
    /// the built-in firewall backends (with `nftables` active), and a
    /// [`SystemdServiceManager`].
    pub fn with_defaults() -> Result<Self> {
        let mut firewalls: Registry<dyn FirewallBackend> = Registry::new();
        register_builtins(&mut firewalls)?;
        Ok(KernelManager::new(
            Arc::new(LinuxNetworkManager::new()),
            firewalls,
            Arc::new(SystemdServiceManager::new()),
            NftablesFirewall::NAME,
        ))
    }

    pub fn network(&self) -> &Arc<dyn NetworkManager> {
        &self.network
    }

    pub fn firewalls(&self) -> &Registry<dyn FirewallBackend> {
        &self.firewalls
    }

    pub fn services(&self) -> &Arc<dyn ServiceManager> {
        &self.services
    }

    /// The name of the currently active firewall backend.
    pub fn active_firewall(&self) -> &str {
        &self.active_firewall
    }

    /// Select a different registered firewall backend by name. Errors if no such
    /// backend is registered.
    pub fn set_active_firewall(&mut self, name: impl Into<String>) -> Result<()> {
        let name = name.into();
        if !self.firewalls.contains(&name) {
            return Err(Error::not_found(format!("firewall backend `{name}`")));
        }
        self.active_firewall = name;
        Ok(())
    }

    /// Resolve the active firewall backend from the registry.
    pub fn firewall(&self) -> Result<Arc<dyn FirewallBackend>> {
        self.firewalls.get(&self.active_firewall)
    }

    /// Apply `rules` using the active firewall backend.
    pub async fn apply_firewall(&self, rules: &[FirewallRule]) -> Result<()> {
        self.firewall()?.apply(rules).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure wiring: construction, defaults, and firewall-backend selection — none
    /// of which touch the host.
    #[test]
    fn defaults_wire_up() {
        let mut km = KernelManager::with_defaults().unwrap();
        assert_eq!(km.active_firewall(), NftablesFirewall::NAME);
        assert!(km.firewall().is_ok());

        km.set_active_firewall(IptablesFirewall::NAME).unwrap();
        assert_eq!(km.active_firewall(), IptablesFirewall::NAME);
        assert!(km.set_active_firewall("does-not-exist").is_err());
    }

    /// End-to-end flow against the real host; needs root + Linux tooling.
    #[tokio::test]
    #[ignore = "requires root + Linux network/firewall/systemd tooling"]
    async fn defaults_wire_up_and_apply() {
        let km = KernelManager::with_defaults().unwrap();
        km.network().set_ip_forwarding(true).await.unwrap();
        km.network().ensure_bridge("br-ocf").await.unwrap();
        assert!(km
            .network()
            .list_bridges()
            .await
            .unwrap()
            .contains(&"br-ocf".to_string()));

        let rules = vec![FirewallRule::new("input", FirewallAction::Allow).with_dport(443)];
        km.apply_firewall(&rules).await.unwrap();
        assert_eq!(km.firewall().unwrap().rules().await.unwrap().len(), 1);

        km.services()
            .ensure("ocf-agent", ServiceState::Running)
            .await
            .unwrap();
    }
}
