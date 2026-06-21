//! The SDN overlay resource model: VPC, subnet, route, ACL rule, and policy.
//!
//! A [`Vpc`] is an isolated layer-2/3 domain identified by a VXLAN network
//! identifier (`vni`). Each VPC carries one or more [`Subnet`]s, every subnet is
//! realized on a host inside a network namespace, and [`Route`]s steer traffic
//! between them. Access is governed by [`FirewallPolicy`]s, each a bundle of
//! ordered [`AclRule`]s attached to either a whole VPC or a single subnet via an
//! [`AclScope`].

use ocf_core::prelude::*;

/// An isolated virtual private cloud — a tenant's private network domain.
///
/// The `vni` is the VXLAN Network Identifier that keeps overlay traffic for this
/// VPC separated from every other VPC sharing the same physical fabric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vpc {
    pub metadata: Metadata,
    /// The VPC's address space in CIDR notation, e.g. `"10.0.0.0/16"`.
    pub cidr: String,
    /// VXLAN Network Identifier isolating this VPC's overlay traffic.
    pub vni: u32,
}

impl Vpc {
    pub fn new(name: impl Into<String>, cidr: impl Into<String>, vni: u32) -> Self {
        Vpc {
            metadata: Metadata::named(name),
            cidr: cidr.into(),
            vni,
        }
    }
}

impl Resource for Vpc {
    fn kind(&self) -> &'static str {
        "vpc"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// Whether a subnet provides outbound (egress) internet access.
///
/// This is the subnet-level *capability* — the "public vs private subnet"
/// distinction. A workload only actually reaches the internet when its subnet is
/// [`EgressMode::Nat`] **and** the workload itself opts in
/// (`NetworkAttachment::egress`); see [`crate::controller`] and the workload
/// model in `ocf-runtime`. Inbound connections are not handled here at all —
/// those are the load balancer's responsibility (`ocf-loadbalancer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressMode {
    /// No outbound internet routing; the subnet is internal-only (default).
    #[default]
    Isolated,
    /// Outbound internet via source NAT (masquerade) out the host's uplink.
    Nat,
}

impl EgressMode {
    /// Whether this mode provides outbound internet routing.
    pub fn provides_egress(&self) -> bool {
        matches!(self, EgressMode::Nat)
    }
}

/// A subnet carved out of a [`Vpc`], realized on a host inside a netns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subnet {
    pub metadata: Metadata,
    /// The owning VPC.
    pub vpc_id: Id,
    /// The subnet's address range in CIDR notation, e.g. `"10.0.1.0/24"`.
    pub cidr: String,
    /// Name of the Linux network namespace that hosts this subnet's dataplane.
    pub netns: String,
    /// Outbound internet capability for this subnet. Defaults to
    /// [`EgressMode::Isolated`] so existing/persisted subnets stay internal-only.
    #[serde(default)]
    pub egress: EgressMode,
}

impl Subnet {
    pub fn new(
        vpc_id: Id,
        name: impl Into<String>,
        cidr: impl Into<String>,
        netns: impl Into<String>,
    ) -> Self {
        Subnet {
            metadata: Metadata::named(name),
            vpc_id,
            cidr: cidr.into(),
            netns: netns.into(),
            egress: EgressMode::Isolated,
        }
    }

    /// Builder: set the subnet's egress capability.
    pub fn with_egress(mut self, egress: EgressMode) -> Self {
        self.egress = egress;
        self
    }
}

impl Resource for Subnet {
    fn kind(&self) -> &'static str {
        "subnet"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// A static route installed for a [`Subnet`]: send `dest_cidr` via `next_hop`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub id: Id,
    /// The subnet whose routing table this entry belongs to.
    pub subnet_id: Id,
    /// Destination prefix in CIDR notation, e.g. `"0.0.0.0/0"` for a default route.
    pub dest_cidr: String,
    /// Next-hop gateway address.
    pub next_hop: String,
}

impl Route {
    pub fn new(
        subnet_id: Id,
        dest_cidr: impl Into<String>,
        next_hop: impl Into<String>,
    ) -> Self {
        Route {
            id: Id::new(),
            subnet_id,
            dest_cidr: dest_cidr.into(),
            next_hop: next_hop.into(),
        }
    }
}

/// Whether an [`AclRule`] permits or drops the matching traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AclAction {
    Allow,
    Deny,
}

/// The direction of traffic an [`AclRule`] matches, relative to the protected
/// resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AclDirection {
    Ingress,
    Egress,
}

/// A single access-control rule. `port == None` matches every port; `cidr` of
/// `"0.0.0.0/0"` matches every address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AclRule {
    pub id: Id,
    pub action: AclAction,
    pub direction: AclDirection,
    /// Transport protocol, e.g. `"tcp"`, `"udp"`, `"icmp"`, or `"any"`.
    pub proto: String,
    /// The remote address range this rule matches, in CIDR notation.
    pub cidr: String,
    /// The port this rule matches; `None` means "any port".
    pub port: Option<u16>,
}

impl AclRule {
    pub fn new(
        action: AclAction,
        direction: AclDirection,
        proto: impl Into<String>,
        cidr: impl Into<String>,
        port: Option<u16>,
    ) -> Self {
        AclRule {
            id: Id::new(),
            action,
            direction,
            proto: proto.into(),
            cidr: cidr.into(),
            port,
        }
    }
}

/// What a [`FirewallPolicy`] attaches to — either an entire VPC or one subnet.
///
/// A policy scoped to a VPC applies to every subnet within it; a policy scoped
/// to a subnet applies to just that subnet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AclScope {
    Vpc(Id),
    Subnet(Id),
}

/// An ordered bundle of [`AclRule`]s applied at an [`AclScope`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallPolicy {
    pub id: Id,
    pub scope: AclScope,
    pub rules: Vec<AclRule>,
}

impl FirewallPolicy {
    pub fn new(scope: AclScope) -> Self {
        FirewallPolicy {
            id: Id::new(),
            scope,
            rules: Vec::new(),
        }
    }

    /// Append a rule, returning `self` for fluent construction.
    pub fn with_rule(mut self, rule: AclRule) -> Self {
        self.rules.push(rule);
        self
    }
}
