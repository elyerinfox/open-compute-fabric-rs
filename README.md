# open-compute-fabric-rs

> I'm pretty upset with proxmox.

**Open Compute Fabric (OCF)** is a monolithic, contract-first fleet-management
and hypervisor control plane written in Rust. The guiding principle is that
**every capability is a trait ("contract") and every backend is a swappable
plugin** registered at runtime — so the controller never hard-depends on Docker
vs. LXC vs. QEMU, PAM vs. Active Directory, nftables vs. iptables, and so on.

📚 **[Full documentation](docs/README.md)** — start with the
[Architecture Overview](docs/architecture/overview.md), the
[Quickstart](docs/getting-started/quickstart.md), or the
[per-subsystem reference](docs/subsystems/).

## What it does (scope)

- **Runtimes** — run container (Docker/Podman/LXC) and virtualization (QEMU)
  workloads behind one `RuntimeProvider` contract; live-migrate workloads
  (memory dump/restore) on request or when flagged highly-available; autoscale
  container workloads with rule-based policies.
- **Authentication** — pluggable `Authenticator` (PAM, Active Directory, local),
  plus host-account sync that materializes local Linux users.
- **Authorization** — Proxmox-style RBAC: roles, groups, users, and role
  bindings over a hierarchical scope.
- **Topology** — model the fleet as `region → datacenter → rack → machine` and
  drill down into any resource.
- **Kernel management** — IP forwarding, bridges, firewall backends, and
  systemd service reconciliation.
- **Inventory** — hardware components with serial numbers + first-seen, and
  IPMI control for same-network targets.
- **Disks** — physical disk tracking (serial, WWN, first-seen, RMA), SMART
  health, and LED locate/fault via `ledctl`.
- **Monitoring** — host and per-runtime CPU/memory/disk/network/IOPS.
- **Fabric** — an encrypted host-to-host peer-to-peer mesh.
- **Networking** — VPCs, subnets, routes, and firewall/ACL policies pushed to
  every machine.
- **Load balancing** — TCP and application load balancers with scope-restricted
  placement, anycast, latency/geo/load routing policies, Let's Encrypt
  termination + renewal, and dynamic DNS (e.g. Cloudflare).

## Layout

```
crates/
  ocf-core          foundational contracts: Resource, Provider+Registry, Scope, …
  ocf-store         durable StateStore (redb) + in-memory backend
  ocf-consensus     openraft-replicated control-plane KV (quorum writes)
  ocf-topology      region → datacenter → rack → machine + drill-down
  ocf-runtime       container/VM runtimes, migration, autoscaling
  ocf-auth          authentication (+ host user sync)
  ocf-authz         RBAC authorization
  ocf-kernel        host kernel: networking, firewall, services
  ocf-inventory     hardware inventory + IPMI
  ocf-disk          physical disks, LED, RMA
  ocf-monitoring    host + per-runtime metrics
  ocf-fabric        encrypted host-to-host mesh (real Noise XX) + SWIM membership
  ocf-network       VPC / subnet / route / ACL overlay
  ocf-loadbalancer  TCP/ALB, TLS (ACME), DDNS
  ocf-api           axum REST API + serves the frontend
  ocfd              the monolithic binary (CLI, config, wiring)
web/                Nuxt 3 + Vite + Vue 3 + Tailwind frontend
```

## Status

The contracts and plugin wiring are real, **and so are the backends**. Every
subsystem executes the real tool — `docker`/`virsh`/`lxc`, `ip`/`nft`/`systemctl`,
`lsblk`/`smartctl`/`ledctl`, `dmidecode`/`ipmitool`, `/proc` metrics,
`pamtester`/`ldapwhoami`/`useradd`, `ovs-vsctl`, `curl` (Cloudflare), `certbot`
(ACME) — and the data plane (TCP load balancer), crypto (X25519 + Noise), and
Raft consensus (over the encrypted transport) are real in-process Rust. There
are no `// TODO(real)` stubs left. Everything compiles cross-platform; on a node
that is missing a tool, that operation degrades gracefully while the rest of the
control plane comes up. The whole workspace builds clean and **187 tests pass**.

## Build & run

```sh
# control plane
cargo build
cargo run -p ocfd -- serve            # starts the API on :8080

# frontend
cd web && npm install && npm run dev   # Nuxt dev server
```

## License

MIT — see [LICENSE](LICENSE).
