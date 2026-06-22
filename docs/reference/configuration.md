# Configuration Reference

Every configuration knob `ocfd` reads, in one place: the command-line flags and
their environment variables, the per-provider environment variables the
subsystems consult, the logging filter, and the on-disk data directory layout.

There are two layers of configuration:

1. **Daemon configuration** — passed to `ocfd` as CLI flags (each with an
   environment-variable equivalent). These shape how the node brings itself up
   and what it serves. See the [CLI reference](cli.md) for usage.
2. **Provider configuration** — environment variables individual subsystems read
   at startup when registering their built-in providers (credentials for the
   real DNS / ACME backends, etc.).

Precedence for the daemon flags is **flag > environment variable > default**.

## Daemon configuration

| Knob | Flag | Env var | Default | Read by | Description |
|------|------|---------|---------|---------|-------------|
| Node id | `--node-id` | `OCF_NODE_ID` | _(auto)_ | `ocfd` (global) | Override this node's stable identity. **Omit to auto-derive** it from the host — `/etc/machine-id`, else a UUID generated once and persisted under the data dir, else the hostname — so the operator never has to name a node and two nodes never collide. Drives the keypair, Raft id, and membership id. |
| Node name | `--node-name` | `OCF_NODE_NAME` | _(hostname)_ | `ocfd` (global) | A friendly display name for this node. Cosmetic — changing it doesn't change the identity. |
| Machine id | `--machine-id` | `OCF_MACHINE_ID` | _(auto)_ | `ocfd` (global) | Override the host machine id used to derive the node identity (rarely needed). |
| Data directory | `--data-dir` | `OCF_DATA_DIR` | _(unset → in-memory)_ | `ocfd` (global) | Directory for durable state. Unset runs fully in-memory; set persists to `<DIR>/state.redb` and restores on boot. |
| Seed peers | `--seed` | `OCF_SEEDS` | _(empty)_ | `ocfd` (global) | Comma-separated seed peer(s) (`host:port`) to contact when joining the mesh. |
| Fabric address | `--fabric-address` | `OCF_FABRIC_ADDRESS` | _(auto-detect)_ | `ocfd` (global) | This node's reachable address peers dial it at (its WireGuard endpoint / control address). Unset auto-detects the host's **primary LAN/route address** (the router-assigned address behind NAT); set it for static setups or when there's no default gateway. |
| Bind address | `--bind` | `OCF_BIND` | `0.0.0.0:8080` | `ocfd serve` | Socket address the HTTP API binds. |
| Static dir | `--static-dir` | `OCF_STATIC_DIR` | _(unset)_ | `ocfd serve` | Directory of built frontend assets to serve (e.g. `web/.output/public`). |

> The membership failure-detector timeouts (`suspect_timeout_secs`,
> `dead_timeout_secs`, both default `5`) live in `ControllerConfig` but are not
> wired to a CLI flag in this build — the daemon constructs the controller with
> the defaults plus the flags above. See
> [ocf-fabric](../subsystems/ocf-fabric.md) for the detector itself.

## Provider configuration

These environment variables are read by individual subsystems when they register
their built-in providers during controller bootstrap. They are **optional**: a
missing variable falls back as noted, and the affected provider simply fails at
call time (with a clear provider error) rather than at boot — consistent with the
project's "honest error" stance (see [Architecture → Overview](../architecture/overview.md)).

| Env var | Default when unset | Read by | Description |
|---------|--------------------|---------|-------------|
| `CLOUDFLARE_API_TOKEN` | empty string | [`ocf-loadbalancer`](../subsystems/ocf-loadbalancer.md) — `dns::register_builtins` | Bearer token for the Cloudflare v4 DNS API used by the `cloudflare` `DnsProvider`. Never logged. With it unset, DNS upserts/deletes fail with a `cloudflare` provider error. |
| `ACME_ACCOUNT_EMAIL` | `admin@example.com` | [`ocf-loadbalancer`](../subsystems/ocf-loadbalancer.md) — `certs::register_builtins` | Account contact email passed to `certbot` (`-m`) by the `letsencrypt` `CertificateProvider`. |

## Logging

| Env var | Default | Description |
|---------|---------|-------------|
| `RUST_LOG` | `info` | Standard `tracing-subscriber` `EnvFilter` directive controlling log verbosity (e.g. `RUST_LOG=debug`, `RUST_LOG=ocf_fabric=trace,info`). When unset, the daemon defaults to `info`. |

## Data directory layout

When `--data-dir` / `OCF_DATA_DIR` is set, the controller creates the directory
(recursively, if needed) and opens a [redb](../subsystems/ocf-store.md) database
inside it for durable state.

```text
<data-dir>/
└── state.redb        # redb-backed StateStore: the persisted snapshot of fleet state
```

- The directory is created on boot if it does not exist.
- On first boot the controller finds an empty store, **seeds** a small demo
  fleet, and persists it; on subsequent boots it finds the persisted machines and
  **restores** instead.
- Writes flow through Raft consensus and are applied into this store; `POST
  /api/v1/admin/persist` snapshots the current state here on demand (see the
  [REST API reference](rest-api.md#admin)).
- With **no** data directory the node uses an in-memory store and all state is
  lost on restart.

See [ocf-store](../subsystems/ocf-store.md) for the store contract and the redb
backend, and [Operations → Deployment](../operations/deployment.md) for how the
data directory factors into running a real cluster.

## See also

- [CLI reference](cli.md) — invocation and examples for every flag
- [Getting Started → Configuration](../getting-started/configuration.md) — a guided walkthrough
- [ocf-store](../subsystems/ocf-store.md) — the durable state store
- [Operations → Security](../operations/security.md) — handling the provider credentials above
