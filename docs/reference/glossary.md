# Glossary

An alphabetical reference for the terms used throughout Open Compute Fabric.
Each entry links to the subsystem or architecture document where the concept is
developed in full.

### ACME

The protocol (RFC 8555) used to obtain and renew TLS certificates from a
certificate authority such as Let's Encrypt. The fabric's `letsencrypt`
[`CertificateProvider`](#certificateprovider) drives the system `certbot` to run
the ACME flow. See [ocf-loadbalancer](../subsystems/ocf-loadbalancer.md).

### Anycast

Advertising the same load-balancer address from every ingress point so the
fabric can steer each client to the nearest one. A boolean flag on a
[load balancer](#load-balancer). See [ocf-loadbalancer](../subsystems/ocf-loadbalancer.md).

### Authenticator

The pluggable contract that establishes *who* a caller is (as opposed to
[RBAC](#rbac), which decides what they may do). The built-in `local`
authenticator validates username/password in memory. See
[ocf-auth](../subsystems/ocf-auth.md).

### Autoscaler

The component that horizontally scales container [workloads](#workload) by
evaluating scaling rules against a metric map (CPU, memory, IOPS, …). Only
container workloads are eligible. See [ocf-runtime](../subsystems/ocf-runtime.md).

### Backend (load balancer)

A concrete target a [load balancer](#load-balancer) can route a client to,
carrying its address, [scope](#scope), current load, latency, and geography —
the live inputs a [routing policy](#routing-policy) consults. See
[ocf-loadbalancer](../subsystems/ocf-loadbalancer.md).

### Binding

See [Role Binding](#role-binding).

### Datacenter

The second level of the fleet hierarchy: a datacenter inside a
[region](#region), containing [racks](#rack). See
[Scopes & Placement](../architecture/scopes-and-placement.md) and
[ocf-topology](../subsystems/ocf-topology.md).

### DDNS (Dynamic DNS)

Programmatically publishing the DNS records that point clients at a
[load balancer](#load-balancer). The built-in `cloudflare`
[`DnsProvider`](#dnsprovider) drives the Cloudflare v4 API over HTTPS. See
[ocf-loadbalancer](../subsystems/ocf-loadbalancer.md).

### Dead

The terminal [liveness](#liveness) state a [member](#membership) reaches after
staying silent past both the suspect and dead timeouts (or being forced dead by
an operator). Triggers drop-out handling — rescheduling its HA workloads. See
[ocf-fabric](../subsystems/ocf-fabric.md).

### DnsProvider

The pluggable contract for upserting and deleting authoritative DNS records.
Built-in: `cloudflare`. See [ocf-loadbalancer](../subsystems/ocf-loadbalancer.md).

### CertificateProvider

The pluggable contract for issuing and renewing TLS certificates and deciding
when renewal is due. Built-in: `letsencrypt` (drives `certbot` over
[ACME](#acme)). See [ocf-loadbalancer](../subsystems/ocf-loadbalancer.md).

### Fabric

The encrypted host-to-host mesh that connects the fleet's nodes, plus, by
extension, the project as a whole ("Open Compute Fabric"). Carries heartbeats and
migration checkpoints over a [Noise](#noise) transport. See
[ocf-fabric](../subsystems/ocf-fabric.md) and
[Distributed Control Plane](../architecture/distributed-control-plane.md).

### Fleet

The entire managed estate — every [region](#region), [datacenter](#datacenter),
[rack](#rack), and [machine](#machine). The root and broadest [scope](#scope)
(`Scope::fleet()`), which contains everything beneath it. See
[Scopes & Placement](../architecture/scopes-and-placement.md).

### Group

A named collection of [users](#user), referenced by username, that can be the
[subject](#subject) of a [role binding](#role-binding). See
[ocf-authz](../subsystems/ocf-authz.md).

### Heartbeat

A periodic liveness signal from a [member](#membership). A heartbeat refreshes a
member to [alive](#liveness) and revives a [suspect](#suspect--dead). Exposed via
`POST /api/v1/fabric/machines/:id/heartbeat`. See
[ocf-fabric](../subsystems/ocf-fabric.md).

### Health

A coarse health signal (`unknown`, `healthy`, `degraded`, `unhealthy`) carried by
stateful resources such as [machines](#machine), distinct from their
[lifecycle](#lifecycle-state). See [Domain Model](../architecture/domain-model.md).

### Id

An opaque, stable string identifier for a resource, either randomly generated or
derived from a human-meaningful name. Serializes transparently as a string. See
[ocf-core](../subsystems/ocf-core.md) and
[Domain Model](../architecture/domain-model.md).

### IPMI

The Intelligent Platform Management Interface — the out-of-band BMC protocol used
to query and control server hardware (power, sensors). The `ipmitool`
[`IpmiController`](#provider) drives it. See
[ocf-inventory](../subsystems/ocf-inventory.md).

### Leader

In [Raft](#raft), the single elected node that orders writes for a term. The
controller waits for a leader before serving. See
[ocf-consensus](../subsystems/ocf-consensus.md).

### LED / ledctl

The drive-bay locator/fault LEDs (`normal`, `locate`, `fault`, `rebuild`) and the
`ledctl` tool that drives them, used to physically find or flag a
[disk](#physical-disk). See [ocf-disk](../subsystems/ocf-disk.md).

### Lifecycle State

The generic provisioning lifecycle of a resource (`pending`, `provisioning`,
`running`, `paused`, `stopping`, `stopped`, `migrating`, `failed`, `terminated`).
Distinct from [health](#health). See
[Domain Model](../architecture/domain-model.md).

### Liveness

A [member's](#membership) state in the local view of the fleet: `alive`,
`suspect`, `dead`, or `left`. Only `alive` nodes are schedulable/routable. See
[ocf-fabric](../subsystems/ocf-fabric.md).

### Load Balancer

A virtual front-end that balances client traffic across selected
[backends](#backend-load-balancer): either a `Tcp` (layer-4) or an
`Application` (layer-7) kind, with [listeners](#listener), a
[routing policy](#routing-policy), and optional [placement](#placement) and
[anycast](#anycast). See [ocf-loadbalancer](../subsystems/ocf-loadbalancer.md).

### Listener

A single listening port on a [load balancer](#load-balancer), with a flag for
whether it terminates TLS. See [ocf-loadbalancer](../subsystems/ocf-loadbalancer.md).

### Machine

The leaf of the fleet hierarchy: a physical (or virtual) node that actually runs
[workloads](#workload), located within a [rack](#rack). Carries capacity, state,
and health. See [ocf-topology](../subsystems/ocf-topology.md).

### Membership

Each node's view of the fleet's members and their [liveness](#liveness), driven
by a [SWIM](#swim)-style failure detector. Exposed via
`GET /api/v1/fabric/membership`. See [ocf-fabric](../subsystems/ocf-fabric.md).

### Metadata

The common bookkeeping every resource carries: [id](#id), name, labels,
annotations, and created/updated timestamps. Labels drive selection (e.g.
load-balancer target selectors). See
[Domain Model](../architecture/domain-model.md).

### Migration

Moving a running [workload](#workload) from one node to another, ideally live
(memory snapshot shipped over the [fabric](#fabric)). Only migration-capable
runtimes (e.g. QEMU) support it; a scoped workload may only migrate within its
[placement](#placement). See [ocf-runtime](../subsystems/ocf-runtime.md).

### Noise

The Noise Protocol Framework, used by the fabric transport for its encrypted
handshake. Built on real [X25519](#x25519) static keys, so a node's mesh identity
and transport identity are the same. See [ocf-fabric](../subsystems/ocf-fabric.md)
and [Operations → Security](../operations/security.md).

### Permission

A single authorization verb (e.g. `workload.create`, `vpc.read`) that a
[role](#role) holds and an access request is checked against. The wildcard `*`
grants everything. See [ocf-authz](../subsystems/ocf-authz.md).

### Physical Disk

A drive attached to a [machine](#machine), identified fleet-wide by its serial,
with a [SMART](#smart)-derived [health](#health) signal, [LED](#led--ledctl)
state, and RMA tracking. See [ocf-disk](../subsystems/ocf-disk.md).

### Placement

A [scope](#scope) restriction on where a [workload](#workload) or
[load balancer](#load-balancer) target may live — and, because an HA target may
only migrate within its own scope, where it may [migrate](#migration). `None`
means fleet-wide. See [Scopes & Placement](../architecture/scopes-and-placement.md).

### Provider

A pluggable implementation of a fabric contract, identified by a `name` and
`description` and held in a [registry](#registry). Examples:
[`RuntimeProvider`](#workload), [`Authenticator`](#authenticator),
[`DnsProvider`](#dnsprovider), `IpmiController`. See
[Contracts & Plugins](../architecture/contracts-and-plugins.md).

### Quorum

The majority of [Raft](#raft) nodes that must agree before a write is committed.
A single-node deployment is a quorum of one. See
[ocf-consensus](../subsystems/ocf-consensus.md).

### Rack

The third level of the fleet hierarchy: a rack inside a [datacenter](#datacenter),
holding [machines](#machine) at numbered rack positions. See
[ocf-topology](../subsystems/ocf-topology.md).

### Raft

The consensus algorithm that replicates the control plane: writes are ordered
through a Raft log and committed by a [quorum](#quorum) before being applied into
the [state store](#statestore). See [ocf-consensus](../subsystems/ocf-consensus.md)
and [Distributed Control Plane](../architecture/distributed-control-plane.md).

### RBAC

Role-Based Access Control: the authorization model of [roles](#role),
[groups](#group), [users](#user), and [role bindings](#role-binding) that decides
whether a user may perform a [permission](#permission) at a [scope](#scope). See
[ocf-authz](../subsystems/ocf-authz.md).

### Region

The coarsest grouping in the fleet hierarchy: a geographic region containing
[datacenters](#datacenter). See [ocf-topology](../subsystems/ocf-topology.md).

### Registry

A typed collection of [providers](#provider) for one contract, keyed by name —
the mechanism that makes every subsystem plugin-driven. See
[ocf-core](../subsystems/ocf-core.md) and
[Contracts & Plugins](../architecture/contracts-and-plugins.md).

### ReplicatedStore

The [Raft](#raft)-replicated control plane: writes go through it (committed by a
[quorum](#quorum), then applied into the [state store](#statestore)); reads come
from the store. See [ocf-consensus](../subsystems/ocf-consensus.md).

### Resource

The common trait every managed object implements (a [workload](#workload), a
[machine](#machine), a [VPC](#vpc), …), giving it a `kind` and shared
[metadata](#metadata) so the API, audit log, and indexers treat them uniformly.
See [Domain Model](../architecture/domain-model.md).

### ResourceSpec

A request or limit for the fundamental compute resources: CPU in **millicores**
(1000 = one core), memory and disk in **bytes**. See
[Domain Model](../architecture/domain-model.md).

### Role

A named bundle of [permissions](#permission), granted to a [subject](#subject) by
a [role binding](#role-binding). Seeded roles: `Administrator` (wildcard) and
`Auditor` (read-only). See [ocf-authz](../subsystems/ocf-authz.md).

### Role Binding

The grant that connects a [role](#role) to a [subject](#subject) at a
[scope](#scope); it applies to that scope and everything beneath it. See
[ocf-authz](../subsystems/ocf-authz.md).

### Routing Policy

How a [load balancer](#load-balancer) chooses among healthy
[backends](#backend-load-balancer): `round_robin`, `least_load`, `latency`, or
`geo`. See [ocf-loadbalancer](../subsystems/ocf-loadbalancer.md).

### Runtime

A [workload](#workload) execution backend — a `RuntimeProvider` that turns a
backend-agnostic workload into a real container or VM (Docker, QEMU, …). See
[ocf-runtime](../subsystems/ocf-runtime.md).

### Scope

A path from the fleet root down to at most a single machine
(`fleet → region → datacenter → rack → machine`), reused for both
[placement](#placement) and authorization. A scope "contains" everything beneath
it. See [Scopes & Placement](../architecture/scopes-and-placement.md).

### SMART

Self-Monitoring, Analysis and Reporting Technology — the drive self-diagnostics
that back a [disk's](#physical-disk) [health](#health) signal (`ok`, `warning`,
`failing`, `unknown`). See [ocf-disk](../subsystems/ocf-disk.md).

### StateStore

The durable key/value persistence contract underpinning the control plane, with
an in-memory backend and a [redb](#redb)-backed backend
(`<data-dir>/state.redb`). See [ocf-store](../subsystems/ocf-store.md).

### redb

The embedded, pure-Rust key/value database that backs the durable
[state store](#statestore) on disk. See [ocf-store](../subsystems/ocf-store.md).

### Subject

The principal a [role binding](#role-binding) grants a role to — either a single
[user](#user) or every member of a [group](#group). See
[ocf-authz](../subsystems/ocf-authz.md).

### Subnet

A range carved out of a [VPC](#vpc), realized on a host inside a Linux network
namespace (`netns`). See [ocf-network](../subsystems/ocf-network.md).

### Suspect / Dead

Intermediate and terminal failure states in [membership](#membership): a member
silent past the suspect timeout becomes `suspect` (a soft, possibly transient
failure); silent further, it becomes [`dead`](#dead). A [heartbeat](#heartbeat)
revives a suspect. See [ocf-fabric](../subsystems/ocf-fabric.md).

### SWIM

The gossip-style membership/failure-detection approach (Scalable
Weakly-consistent Infection-style process group Membership) the
[membership](#membership) detector models — heartbeat refresh plus
suspicion/death timeouts, with seams for indirect probes and incarnation-number
refutation. See [ocf-fabric](../subsystems/ocf-fabric.md).

### User

A principal that can be granted [roles](#role), belonging to zero or more
[groups](#group). See [ocf-authz](../subsystems/ocf-authz.md).

### VPC (Virtual Private Cloud)

An isolated tenant network domain, identified by a [VNI](#vni) and carrying one or
more [subnets](#subnet). See [ocf-network](../subsystems/ocf-network.md).

### VNI (VXLAN Network Identifier)

The VXLAN identifier that keeps one [VPC](#vpc)'s overlay traffic separated from
every other VPC on the same physical [fabric](#fabric). See
[ocf-network](../subsystems/ocf-network.md).

### Workload

A unit of compute the fabric schedules onto a [machine](#machine) — a container
or a virtual machine — backend-agnostic until a [runtime](#runtime) realizes it.
Carries resources, lifecycle state, [placement](#placement), and an
HA flag. See [ocf-runtime](../subsystems/ocf-runtime.md).

### X25519

The Curve25519 Diffie-Hellman function used for the fabric's real keypairs; a
node's static X25519 key is both its mesh identity and its
[Noise](#noise) transport key. See [ocf-fabric](../subsystems/ocf-fabric.md) and
[Operations → Security](../operations/security.md).

## See also

- [Architecture → Overview](../architecture/overview.md) — how these pieces fit together
- [Domain Model](../architecture/domain-model.md) — the core types (Resource, Metadata, Id, …)
- [Subsystems](../subsystems/) — per-crate detail for every term above
