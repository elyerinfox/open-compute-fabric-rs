# CLI Reference

`ocfd` is the Open Compute Fabric daemon — a single monolithic binary that
builds the entire control plane (every subsystem with its built-in plugin
providers registered) and either serves the REST API + frontend or prints its
registered providers. See [ocfd](../subsystems/ocfd.md) for the binary's
internals.

```
ocfd [GLOBAL OPTIONS] <COMMAND>
```

Every global option also reads an environment variable, so the same invocation
works as flags, env vars, or a mix. Flags win over env vars; env vars win over
defaults. The full set of knobs (including the per-provider env vars the
subsystems read) is catalogued in the [Configuration reference](configuration.md).

## Commands

| Command | What it does |
|---------|--------------|
| [`serve`](#ocfd-serve) | Build the controller and serve the REST API (and the frontend, if built). |
| [`providers`](#ocfd-providers) | Build the controller and print every registered pluggable provider. |

## Global options

These apply to both subcommands and may appear before or after the command.

| Flag | Env var | Default | Description |
|------|---------|---------|-------------|
| `--node-id <ID>` | `OCF_NODE_ID` | `node-local` | This node's stable identity in the fleet. Also seeds the Raft node id. |
| `--data-dir <DIR>` | `OCF_DATA_DIR` | _(unset)_ | Directory for durable state. Omit to run fully in-memory (state lost on restart); set it to persist to `<DIR>/state.redb` and reload on boot. |
| `--seed <PEER>` | `OCF_SEEDS` | _(empty)_ | Seed peer(s) to contact when joining the mesh. Comma-separated; may be repeated. |

> `--seed` accepts a comma-separated list (`--seed a:51820,b:51820`) and may also
> be given multiple times. Via the environment, set `OCF_SEEDS` to the
> comma-separated list.

---

## `ocfd serve`

Start the controller and serve the API. On first boot with an empty (or unset)
data directory the controller seeds a small demo fleet and persists it; on a
subsequent boot it restores the persisted state. The membership failure detector
starts before the listener accepts requests.

```
ocfd serve [OPTIONS]
```

| Flag | Env var | Default | Description |
|------|---------|---------|-------------|
| `--bind <ADDR>` | `OCF_BIND` | `0.0.0.0:8080` | Socket address to bind the HTTP API on. |
| `--static-dir <DIR>` | `OCF_STATIC_DIR` | _(unset)_ | Directory of built frontend assets to serve (e.g. `web/.output/public`). When set and present, the frontend is served with an SPA fallback to `index.html`; when absent, the API is served alone. |

**Examples**

Serve in-memory on the default port (state is lost on restart):

```bash
ocfd serve
```

Serve with durable state and the built frontend:

```bash
ocfd --data-dir ./data serve --bind 127.0.0.1:8080 --static-dir web/.output/public
```

All-environment form (e.g. in a container):

```bash
export OCF_NODE_ID=node-a
export OCF_DATA_DIR=/var/lib/ocf
export OCF_BIND=0.0.0.0:8080
export OCF_STATIC_DIR=/srv/ocf/public
ocfd serve
```

Join an existing fleet by pointing at seed peers:

```bash
ocfd --node-id node-b --data-dir ./data-b --seed node-a.fabric:51820 serve
```

The API is then available under `/api/v1` (see the [REST API reference](rest-api.md)).

---

## `ocfd providers`

Build the controller and print every pluggable provider registered across all
subsystems, grouped by contract — the most direct demonstration that the whole
control plane is plugin-driven. Useful as a smoke test that the binary builds a
healthy controller without binding a port.

```
ocfd providers
```

**Example**

```bash
ocfd providers
```

```text
RuntimeProvider:
  - docker           Docker/OCI container runtime (drives the docker CLI)
  - qemu             QEMU/KVM virtual machine runtime (drives virsh/libvirt)
Authenticator:
  - local            In-memory username/password authenticator
InventoryCollector:
  - sysfs            Linux sysfs/DMI hardware inventory collector
IpmiController:
  - ipmitool         BMC controller (drives the ipmitool CLI)
CertificateProvider:
  - letsencrypt      ACME / Let's Encrypt certificate provider (drives certbot)
DnsProvider:
  - cloudflare       Cloudflare authoritative DNS provider (Cloudflare v4 API over HTTPS)
```

> The exact lines reflect whatever providers are registered; the descriptions
> come from each provider's own `Provider::description()`.

---

## Other built-in flags

`ocfd` is built with `clap`, so it also accepts the standard:

| Flag | Description |
|------|-------------|
| `-h`, `--help` | Print help (works at the top level and per subcommand, e.g. `ocfd serve --help`). |
| `-V`, `--version` | Print the binary version. |

Logging verbosity is controlled by the `RUST_LOG` environment variable (the
daemon uses `tracing-subscriber`'s `EnvFilter`, defaulting to `info`); see the
[Configuration reference](configuration.md#logging).

## See also

- [Configuration reference](configuration.md) — every flag and environment variable in one table
- [Getting Started → Quickstart](../getting-started/quickstart.md) — build and run end to end
- [Operations → Deployment](../operations/deployment.md) — multi-node clusters, seeds, the data directory
- [ocfd subsystem](../subsystems/ocfd.md) — the binary's internals
