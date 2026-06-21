# Using the fabric — a guided tour

> A hands-on walk through the running control plane. Every step is a real API
> call against the seeded demo fleet, with what you'll see and the
> [concept](concepts.md) it demonstrates.

**Before you start:** have the daemon running from the [Quickstart](quickstart.md)
— `cargo run -p ocfd -- serve` — listening on `http://localhost:8080`. The
examples use `curl`; piping through `python3 -m json.tool` (or `jq`) pretty-prints
the JSON.

> **One caveat up front.** Modeling, networking, the fabric, health, and platform
> all work **tool-free**. *Running* workloads needs a container runtime
> (`docker`/`podman`) or a VM backend (`qemu`); on a host without them the
> workload endpoints come back empty and the attach/placement steps are no-ops —
> everything else in this tour works regardless. Steps that need a runtime are
> marked **(needs a runtime)**.

---

## Part 1 — Explore the fleet

See the physical topology as a drill-down tree (`fleet → region → datacenter →
rack → machine`):

```sh
curl -s http://localhost:8080/api/v1/topology/tree | python3 -m json.tool
```

List the machines and their **capability labels** — node-3 is the GPU box,
node-1 has NVMe:

```sh
curl -s http://localhost:8080/api/v1/machines | python3 -m json.tool
```

You'll see three machines (`node-1`, `node-2`, `node-3`) with `metadata.labels`
like `gpu=true`, `nvme=true`, and a `fabric.reachability` label. *Concept:*
[scope & the domain model](concepts.md#the-domain-model).

---

## Part 2 — The software-defined network

The seed creates one VPC (`tenant-a`) with two subnets. List them:

```sh
curl -s http://localhost:8080/api/v1/networks/vpcs    | python3 -m json.tool
curl -s http://localhost:8080/api/v1/networks/subnets | python3 -m json.tool
```

You'll see VPC `tenant-a` and subnets `web` (`10.0.1.0/24`) and `db`
(`10.0.2.0/24`). Each VPC is an **isolated VXLAN VNI**; each subnet is a CIDR
realized on every host and stitched across them by the overlay. *Concept:*
[Networking](concepts.md#networking-vpcs-subnets-overlay-ingress--egress).

Turn on **outbound internet (NAT)** for the `web` subnet:

```sh
curl -s -X POST http://localhost:8080/api/v1/networks/subnets/web/egress \
  -H 'content-type: application/json' -d '{"mode":"nat"}' | python3 -m json.tool
```

The subnet's `egress` flips to `nat`. A workload still has to **opt in**
individually (Part 5) — the subnet capability and the per-workload opt-in are two
separate controls. Set it back with `{"mode":"isolated"}`.

---

## Part 3 — The fabric (the part you can fully see tool-free)

**Membership** — every node, its liveness, measured RTT, and reachability:

```sh
curl -s http://localhost:8080/api/v1/fabric/membership | python3 -m json.tool
```

Each member shows `reachability` (`public` / `private` / `relay`) and `rtt_ms`
(the measured round-trip; `null` on a single host since peers aren't reachable).

**The three WireGuard planes** — management, workload, and load-balancer, each its
own isolated encrypted underlay:

```sh
curl -s http://localhost:8080/api/v1/fabric/wireguard | python3 -m json.tool
```

You'll see `wg-mgmt` (`10.255.0.x`, control), `wg-data` (`10.254.0.x`, workload
VXLAN), and `wg-lb` (`10.253.0.x`, LB) — each with this node's address and its
peers' WireGuard public keys (derived from their fabric identity). *Concept:*
[three planes](concepts.md#the-fabric-one-identity-three-encrypted-planes).

**Routing** — the planned path from this node to each peer, weighed by RTT:

```sh
curl -s http://localhost:8080/api/v1/fabric/routes | python3 -m json.tool
```

In the seeded demo node-1 is a `relay` and node-2 is `private`, so the route to
node-2 comes back **`relayed` via node-1** — the fabric's "fastest path" decision
made visible. *Concept:*
[topology intelligence](concepts.md#topology-intelligence-latency-reachability-routing).

---

## Part 4 — Health & platform

Ask each node what's wrong and how to fix it:

```sh
curl -s http://localhost:8080/api/v1/health/findings | python3 -m json.tool
```

Each finding has a `severity`, a message (e.g. "ip_forward not enabled",
"netfilter module not loaded"), and — when remediable — a `fix` you can apply:

```sh
# Apply a finding's fix (use the check/fix ids from the findings above)
curl -s -X POST http://localhost:8080/api/v1/health/fix \
  -H 'content-type: application/json' \
  -d '{"check":"ip_forwarding","fix":"enable"}' | python3 -m json.tool
```

See the host OS and how a missing package would be installed for *this* distro:

```sh
curl -s http://localhost:8080/api/v1/platform | python3 -m json.tool
```

*Concept:* [health & platform](concepts.md#health--platform). (On a host without
the underlying tools, fixes report an honest error rather than pretending.)

---

## Part 5 — Workloads, placement & egress  **(needs a runtime)**

List the workloads (containers + the HA VM). With a runtime present you'll see
`web-1`, `web-2`, `web-3`, `db-1`, and `gpu-job`:

```sh
curl -s http://localhost:8080/api/v1/workloads | python3 -m json.tool
```

**Capability placement** — `gpu-job` requires `gpu=true`, so it can only land on
node-3. Ask where it can run:

```sh
curl -s http://localhost:8080/api/v1/workloads/gpu-job/candidates | python3 -m json.tool
```

The `candidates` list contains only node-3 — placement filters by capability +
scope + capacity. *Concept:*
[placement](concepts.md#workloads--placement).

**Attach a workload to a subnet with egress** — IPAM assigns an address, and the
workload opts in to the NAT you enabled in Part 2:

```sh
curl -s -X POST http://localhost:8080/api/v1/workloads/web-1/network \
  -H 'content-type: application/json' \
  -d '{"subnet_id":"web","egress":true}' | python3 -m json.tool
```

The response shows the IPAM-assigned `address`; the host programs the masquerade
rule for it. Detach (and release the address) with `DELETE` on the same path.

---

## Part 6 — Load balancing & autoscaling association  **(needs a runtime for live backends)**

List the load balancers — the internet-facing ingress:

```sh
curl -s http://localhost:8080/api/v1/loadbalancers | python3 -m json.tool
```

`web-https` (L7, TLS :443, `app.example.com`) fronts `target_selector =
{app: web}`, and `db-tcp` (L4, :5432) balances database connections. The
`target_selector` is the link to workloads: the same label set an **autoscaler**
governs is the set the LB fronts.

Resolve a load balancer's **live backend set** from its selector — the matching
workloads addressed on the `wg-lb` plane:

```sh
curl -s http://localhost:8080/api/v1/loadbalancers/web-https/backends | python3 -m json.tool
```

With workloads running you get one backend per `app=web` workload, addressed at
its hosting node's `wg-lb` IP, with measured RTT stamped (feeding the `Latency`
policy). *Concept:* [LB ↔ workloads](concepts.md#networking-vpcs-subnets-overlay-ingress--egress).

---

## Part 7 — Resilience: fail a node, watch HA reschedule

Simulate a node failure and watch the membership detector react:

```sh
# Mark node-2 as failed
curl -s -X POST http://localhost:8080/api/v1/fabric/machines/node-2/fail | python3 -m json.tool

# Re-read membership — node-2 ages toward suspect/dead
curl -s http://localhost:8080/api/v1/fabric/membership | python3 -m json.tool
```

When a node is declared dead, its **highly-available** workloads (like the `db-1`
VM) are rescheduled onto a surviving node that still satisfies their constraints;
non-HA workloads are stopped. Bring a node back with the `heartbeat` endpoint.
*Concept:* [fleet membership & consensus](concepts.md#the-fleet-membership-consensus-persistence).

---

## Part 8 — Make it durable

Everything so far was in-memory. Run with a data directory and the state is
Raft-committed and reloaded on restart:

```sh
cargo run -p ocfd -- serve --data-dir ./ocf-data
```

Restart the daemon and re-read any endpoint — your VPCs, subnets, egress settings,
and attachments survive. *Concept:*
[replicated & durable](concepts.md#the-fleet-membership-consensus-persistence).
See [Configuration](configuration.md) and
[Operations → Deployment](../operations/deployment.md) for multi-node fleets.

---

## Where to go next

- **The web UI** — everything above is also clickable at `http://localhost:3000`
  (`cd web && npm run dev`). See [Frontend → Overview](../frontend/overview.md).
- **The full API** — every endpoint and its shapes:
  [Reference → REST API](../reference/rest-api.md).
- **Go deep** — [Architecture overview](../architecture/overview.md) and the
  [subsystem docs](../subsystems/).
- **What changed recently** — the [changelog](../changelog/).
