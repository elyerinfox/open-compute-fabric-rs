//! # ocf-loadbalancer
//!
//! Traffic front-ends for the fabric: TCP / application load balancing, TLS
//! certificate issuance, and dynamic DNS.
//!
//! The crate is organized around three pluggable concerns and one stateful
//! controller:
//!
//! * [`model`] — the [`LoadBalancer`] resource (with its `placement` scope,
//!   `anycast` flag, hostnames, and target selector), plus the [`Backend`] and
//!   [`ClientContext`] inputs that routing consumes.
//! * [`routing`] — [`select_backend`], the pure, policy- and ingress-aware
//!   function that picks a backend for a request.
//! * [`proxy`] — [`TcpLoadBalancer`], the real layer-4 data plane that accepts
//!   client TCP connections, routes each via [`select_backend`], and splices
//!   bytes through to the chosen backend.
//! * [`certs`] — the [`CertificateProvider`] contract and the
//!   [`LetsEncryptProvider`] (ACME) backend, which drives the system `certbot`.
//! * [`dns`] — the [`DnsProvider`] contract and the [`CloudflareDns`] backend,
//!   which calls the Cloudflare v4 API over HTTPS via the system `curl`.
//! * [`controller`] — [`LoadBalancerController`], async CRUD over load balancers
//!   plus [`LoadBalancerController::resolve`].
//!
//! A load balancer's `placement` scope restricts where its targets may live —
//! and, since a highly-available target may only migrate within its own scope,
//! where they may migrate. The controller enforces this when resolving traffic:
//! a scoped load balancer never routes to a backend outside its scope.
//!
//! The certificate and DNS backends here are real: they shell out to the
//! installed `certbot` and `curl` respectively. On a host where those binaries
//! are absent (or the credentials are wrong), the calls fail with a provider
//! error rather than fabricating success.

pub mod certs;
pub mod controller;
pub mod dns;
pub mod model;
pub mod proxy;
pub mod routing;

pub use certs::{Certificate, CertificateProvider, CertbotMode, LetsEncryptProvider};
pub use controller::LoadBalancerController;
pub use dns::{CloudflareDns, DnsProvider, RecordType};
pub use model::{
    Backend, ClientContext, LbKind, Listener, LoadBalancer, RoutingPolicy,
};
pub use proxy::TcpLoadBalancer;
pub use routing::select_backend;

use ocf_core::prelude::*;

/// Register every built-in provider this crate ships into both registries.
///
/// Convenience over calling [`certs::register_builtins`] and
/// [`dns::register_builtins`] separately: wires the default
/// [`LetsEncryptProvider`] and [`CloudflareDns`] in one call.
pub fn register_builtins(
    certs: &mut Registry<dyn CertificateProvider>,
    dns: &mut Registry<dyn DnsProvider>,
) -> Result<()> {
    certs::register_builtins(certs)?;
    dns::register_builtins(dns)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_builtins_populates_both_registries() {
        let mut cert_reg: Registry<dyn CertificateProvider> = Registry::new();
        let mut dns_reg: Registry<dyn DnsProvider> = Registry::new();
        register_builtins(&mut cert_reg, &mut dns_reg).unwrap();
        assert!(cert_reg.contains("letsencrypt"));
        assert!(dns_reg.contains("cloudflare"));
    }
}
