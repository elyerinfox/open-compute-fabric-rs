//! # ocf-health
//!
//! A **modular fleet-health system**. Each node runs a set of pluggable
//! [`HealthCheck`]s; a check inspects the host and emits [`HealthFinding`]s —
//! e.g. *"IP forwarding not enabled on kernel"*, *"Netfilter (nf_tables) not
//! enabled on kernel"*, *"Docker experimental features not enabled"*. Every
//! finding carries one or more [`FixAction`]s the user can press in the
//! dashboard, and the same check that detected the problem also remediates it
//! ([`HealthCheck::apply_fix`]).
//!
//! Modularity comes from the same plugin pattern as the rest of the fabric: a
//! check extends [`Provider`](ocf_core::registry::Provider) and registers by
//! name into a [`Registry`](ocf_core::registry::Registry). Adding a new warning
//! is adding a new `HealthCheck` — nothing else changes.
//!
//! ## Pieces
//!
//! * [`finding`] — [`Severity`], [`HealthCategory`], [`FixAction`], [`HealthFinding`].
//! * [`check`] — the [`HealthCheck`] contract.
//! * [`checks`] — the built-in checks (ip-forwarding, netfilter, bridge-netfilter, docker-experimental).
//! * [`service`] — [`HealthService`], which runs checks and routes fixes.
//!
//! Probes are real: they read `/proc`/`/sys` and run host tools, and fixes write
//! sysctls or run `modprobe`/`systemctl`. A check that cannot assess the host
//! (not Linux, tool absent) reports *nothing* rather than guessing, so the
//! dashboard only shows genuine findings.

pub mod check;
pub mod checks;
pub mod exec;
pub mod finding;
pub mod service;

pub use check::HealthCheck;
pub use checks::{
    BridgeNetfilterCheck, DockerExperimentalCheck, IpForwardingCheck, NetfilterCheck, PackageCheck,
    SecurityUpdateCheck, ServiceCheck, SwapCheck, SysctlCheck, TimeSyncCheck, VulnerabilityCheck,
};
pub use finding::{FixAction, HealthCategory, HealthFinding, Severity};
pub use service::HealthService;

use ocf_core::prelude::*;
use std::sync::Arc;

/// Register the built-in health checks into `reg`: capability checks (kernel
/// flags, modules, runtime config) plus the declarative configuration checks.
///
/// A deployment can register additional checks (custom warnings) or
/// `register_or_replace` these without touching the controller.
pub fn register_builtins(reg: &mut Registry<dyn HealthCheck>) -> Result<()> {
    // Kernel / runtime capability checks.
    reg.register("ip-forwarding", Arc::new(IpForwardingCheck::new()))?;
    reg.register("netfilter", Arc::new(NetfilterCheck::new()))?;
    reg.register("bridge-netfilter", Arc::new(BridgeNetfilterCheck::new()))?;
    reg.register("docker-experimental", Arc::new(DockerExperimentalCheck::new()))?;

    // Configuration checks — declared as data via the reusable check types.
    register_config_checks(reg)?;
    Ok(())
}

/// Register the default set of configuration checks. Each is a parameterized
/// [`SysctlCheck`]/[`ServiceCheck`] (or a small bespoke check), so adding a
/// configuration warning here is a one-line registration.
fn register_config_checks(reg: &mut Registry<dyn HealthCheck>) -> Result<()> {
    reg.register(
        "ipv6-forwarding",
        Arc::new(SysctlCheck::equals(
            "ipv6-forwarding",
            "/proc/sys/net/ipv6/conf/all/forwarding",
            "1",
            HealthCategory::Network,
            "IPv6 forwarding not enabled on kernel",
            "net.ipv6.conf.all.forwarding is not 1; IPv6 routing/NAT/overlay won't work.",
        )),
    )?;
    reg.register(
        "conntrack-max",
        Arc::new(SysctlCheck::at_least(
            "conntrack-max",
            "/proc/sys/net/netfilter/nf_conntrack_max",
            262_144,
            HealthCategory::Network,
            "Connection-tracking table is small",
            "nf_conntrack_max is low; a busy NAT/load-balancer node can exhaust it and drop connections.",
        )),
    )?;
    reg.register(
        "inotify-instances",
        Arc::new(SysctlCheck::at_least(
            "inotify-instances",
            "/proc/sys/fs/inotify/max_user_instances",
            512,
            HealthCategory::Kernel,
            "inotify instance limit is low",
            "fs.inotify.max_user_instances is low; many watchers (containers, log tailers) can hit the limit.",
        )),
    )?;
    reg.register("swap-disabled", Arc::new(SwapCheck::new()))?;
    reg.register("time-sync", Arc::new(TimeSyncCheck::new()))?;
    reg.register(
        "service-docker",
        Arc::new(ServiceCheck::new("service-docker", "docker", true)),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_register_expected_checks() {
        let mut reg: Registry<dyn HealthCheck> = Registry::new();
        register_builtins(&mut reg).expect("register builtins");
        // Capability checks + configuration checks.
        for name in [
            "ip-forwarding",
            "netfilter",
            "bridge-netfilter",
            "docker-experimental",
            "ipv6-forwarding",
            "conntrack-max",
            "inotify-instances",
            "swap-disabled",
            "time-sync",
            "service-docker",
        ] {
            assert!(reg.contains(name), "missing check {name}");
        }
        assert_eq!(reg.len(), 10);
    }

    #[tokio::test]
    async fn service_runs_without_panicking() {
        // On a non-Linux dev box every probe is inconclusive → no findings, but
        // the sweep must complete cleanly.
        let svc = HealthService::with_builtins().expect("service");
        let findings = svc.run(&Id::named("node-local")).await;
        // We can't assert a count (platform-dependent), only that it ran.
        let _ = findings;
        // An unknown fix routes to a not-found error, not a panic.
        assert!(svc
            .apply_fix("ip-forwarding", "nope", &Id::named("node-local"))
            .await
            .is_err());
        assert!(svc
            .apply_fix("no-such-check", "x", &Id::named("node-local"))
            .await
            .is_err());
    }
}
