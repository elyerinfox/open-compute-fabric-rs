//! The load-balancer resource model and the data routing operates over.
//!
//! A [`LoadBalancer`] is a [`Resource`] describing a virtual front-end: which
//! ports it listens on, how it selects targets, and (optionally) the [`Scope`]
//! it is pinned to. A [`Backend`] is a concrete target the front-end can route
//! to, and a [`ClientContext`] carries the request-side facts (source address,
//! ingress location, geography) that the routing policy consults.

use ocf_core::prelude::*;
use std::collections::BTreeMap;

/// The kind of traffic a load balancer front-ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LbKind {
    /// Layer-4 pass-through balancing (raw TCP).
    Tcp,
    /// Layer-7 application load balancing (HTTP/HTTPS aware).
    Application,
}

/// How a load balancer chooses between healthy backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingPolicy {
    /// Cycle through backends in order.
    RoundRobin,
    /// Send to the backend reporting the lowest load.
    LeastLoad,
    /// Send to the backend with the lowest observed latency.
    Latency,
    /// Prefer a backend in the client's geography.
    Geo,
}

/// A single listening port on a load balancer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listener {
    pub port: u16,
    /// Whether the listener terminates TLS.
    #[serde(default)]
    pub tls: bool,
}

impl Listener {
    pub fn tcp(port: u16) -> Self {
        Listener { port, tls: false }
    }

    pub fn tls(port: u16) -> Self {
        Listener { port, tls: true }
    }
}

/// A virtual front-end that balances client traffic across selected targets.
///
/// `target_selector` matches backend labels the same way an autoscaler matches
/// replicas. `placement`, when set, restricts where the targets are allowed to
/// live — and, because a highly-available workload may only migrate within its
/// own scope, it also bounds where they may migrate. An unset `placement` means
/// the load balancer is fleet-wide. `anycast` advertises the same address from
/// every ingress so the fabric can steer clients to the nearest one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadBalancer {
    pub metadata: Metadata,
    pub kind: LbKind,
    #[serde(default)]
    pub listeners: Vec<Listener>,
    /// Label selector matching the backend workloads this LB fronts.
    #[serde(default)]
    pub target_selector: BTreeMap<String, String>,
    pub policy: RoutingPolicy,
    /// When set, restricts where targets may live and migrate.
    #[serde(default)]
    pub placement: Option<Scope>,
    /// Advertise the same address from every ingress (steer to nearest).
    #[serde(default)]
    pub anycast: bool,
    /// DNS hostnames that resolve to this load balancer.
    #[serde(default)]
    pub hostnames: Vec<String>,
}

impl LoadBalancer {
    /// Create a load balancer with a single policy and no listeners yet.
    pub fn new(name: impl Into<String>, kind: LbKind, policy: RoutingPolicy) -> Self {
        LoadBalancer {
            metadata: Metadata::new(name),
            kind,
            listeners: Vec::new(),
            target_selector: BTreeMap::new(),
            policy,
            placement: None,
            anycast: false,
            hostnames: Vec::new(),
        }
    }

    pub fn with_listener(mut self, listener: Listener) -> Self {
        self.listeners.push(listener);
        self
    }

    pub fn with_hostname(mut self, hostname: impl Into<String>) -> Self {
        self.hostnames.push(hostname.into());
        self
    }

    /// Pin the load balancer (and therefore its targets) to a scope.
    pub fn with_placement(mut self, scope: Scope) -> Self {
        self.placement = Some(scope);
        self
    }

    /// Add a label to the target selector — the workloads this LB fronts (the
    /// same label set an autoscaler governs). Chainable to require several.
    pub fn fronting(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.target_selector.insert(key.into(), value.into());
        self
    }

    /// Whether `backend` is allowed to serve this load balancer given its
    /// `placement` constraint. A fleet-wide LB (no placement) accepts any
    /// backend; a scoped LB accepts only backends whose scope it contains.
    pub fn admits(&self, backend: &Backend) -> bool {
        match &self.placement {
            None => true,
            Some(scope) => scope.contains(&backend.scope),
        }
    }
}

impl Resource for LoadBalancer {
    fn kind(&self) -> &'static str {
        "loadbalancer"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// A concrete target a load balancer can route a client to.
///
/// Backends are the live, per-request inputs to routing: `load` and
/// `latency_ms` are sampled continuously, `scope` locates the backend in the
/// fleet (so a scoped LB can reject out-of-scope targets), and `geo` is an
/// optional coarse region tag used by the [`RoutingPolicy::Geo`] policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backend {
    pub workload_id: Id,
    /// Reachable address (`host:port`) of the target.
    pub address: String,
    /// Where this backend lives in the fleet.
    pub scope: Scope,
    /// Current load, normalized 0.0 (idle) .. 1.0 (saturated).
    #[serde(default)]
    pub load: f64,
    /// Most recently observed round-trip latency in milliseconds.
    #[serde(default)]
    pub latency_ms: f64,
    /// Coarse geography tag (e.g. `"us-east"`), if known.
    #[serde(default)]
    pub geo: Option<String>,
}

impl Backend {
    pub fn new(workload_id: Id, address: impl Into<String>, scope: Scope) -> Self {
        Backend {
            workload_id,
            address: address.into(),
            scope,
            load: 0.0,
            latency_ms: 0.0,
            geo: None,
        }
    }

    pub fn with_load(mut self, load: f64) -> Self {
        self.load = load;
        self
    }

    pub fn with_latency(mut self, latency_ms: f64) -> Self {
        self.latency_ms = latency_ms;
        self
    }

    pub fn with_geo(mut self, geo: impl Into<String>) -> Self {
        self.geo = Some(geo.into());
        self
    }
}

/// Request-side context the routing policy consults.
///
/// `ingress_scope` is where the request entered the fabric; it lets a scoped or
/// anycast load balancer prefer backends near the point of ingress. `geo` is the
/// client's coarse geography, used by [`RoutingPolicy::Geo`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientContext {
    /// Source IP of the client, if known.
    #[serde(default)]
    pub src_ip: Option<String>,
    /// Scope through which the request entered the fabric.
    #[serde(default)]
    pub ingress_scope: Option<Scope>,
    /// Coarse client geography (e.g. `"eu-west"`), if known.
    #[serde(default)]
    pub geo: Option<String>,
}

impl ClientContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_src_ip(mut self, src_ip: impl Into<String>) -> Self {
        self.src_ip = Some(src_ip.into());
        self
    }

    pub fn with_ingress_scope(mut self, scope: Scope) -> Self {
        self.ingress_scope = Some(scope);
        self
    }

    pub fn with_geo(mut self, geo: impl Into<String>) -> Self {
        self.geo = Some(geo.into());
        self
    }
}
