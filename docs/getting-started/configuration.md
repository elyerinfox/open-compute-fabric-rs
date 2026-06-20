# Configuration

> The handful of flags you actually reach for when running `ocfd` — identity,
> persistence, peers, bind address, and serving the UI.

`ocfd` is configured entirely through CLI flags, each of which also reads an
environment variable. The flags below are the practical ones; for the exhaustive
list (every flag, every variable, defaults) see
[Reference → Configuration](../reference/configuration.md) and
[Reference → CLI](../reference/cli.md).

Global flags apply to every subcommand and go **before** the subcommand;
`serve`-specific flags go after it:

```sh
ocfd --node-id node-a --data-dir /var/lib/ocf serve --bind 0.0.0.0:8080
```

## Choosing a node id

```sh
ocfd --node-id node-a serve          # or: OCF_NODE_ID=node-a ocfd serve
```

`--node-id` (env `OCF_NODE_ID`, default `node-local`) is this node's **stable
identity** in the fleet. It must be unique per node: it keys membership, and a
distinct, non-zero Raft node id is derived from it (FNV-1a hash), so two nodes
sharing a name would collide in the cluster. Pick it once and keep it stable
across restarts.

## Persisting state with `--data-dir`

```sh
ocfd --data-dir /var/lib/ocf serve   # or: OCF_DATA_DIR=/var/lib/ocf
```

By default `ocfd` runs **fully in-memory**: state is seeded on boot and lost on
restart. Pass `--data-dir` (env `OCF_DATA_DIR`) to persist to disk. The directory
is created if needed and holds:

- **`state.redb`** — the durable key/value snapshot of control-plane state
  (machines, workloads, networks, load balancers, RBAC), via
  [`ocf-store`](../subsystems/ocf-store.md).

On the next boot, if persisted machines are found the controller **restores** that
state instead of reseeding the demo fleet. So with `--data-dir`, the fleet you
build survives a reboot; without it, every restart starts from the demo seed.

## Joining peers with `--seed`

```sh
ocfd --node-id node-b --seed node-a.internal:51820 serve
# multiple, comma-separated:
ocfd --seed node-a:51820,node-c:51820 serve   # or: OCF_SEEDS=node-a:51820,node-c:51820
```

`--seed` (env `OCF_SEEDS`, comma-separated) lists the `host:port` mesh endpoints
to contact when joining the fabric. The fabric mesh listens on port **`51820`**
by convention. Seeds are how a new node finds the existing fleet; an empty seed
list (the default) means this node forms its own single-node cluster. See
[Operations → Deployment](../operations/deployment.md) for how a multi-node fleet
forms.

## Binding the API with `--bind`

```sh
ocfd serve --bind 127.0.0.1:8080     # or: OCF_BIND=127.0.0.1:8080
```

`--bind` (env `OCF_BIND`, default `0.0.0.0:8080`) is the address the HTTP API
listens on. Use `127.0.0.1` to keep it local, or a specific interface address to
control exposure.

## Serving the built frontend with `--static-dir`

```sh
ocfd serve --static-dir web/.output/public   # or: OCF_STATIC_DIR=web/.output/public
```

`--static-dir` (env `OCF_STATIC_DIR`) points at a directory of built frontend
assets. When set and the directory exists, `ocf-api` serves those static files
with an SPA fallback to `index.html`, so the UI is same-origin with the API (no
separate dev server, no CORS proxy). Build the assets with `cd web && npm run
build` first — see [Operations → Deployment](../operations/deployment.md). If the
directory is missing, the API serves itself and logs a warning.

## Quick reference

| Flag | Env | Default | Scope | Purpose |
|------|-----|---------|-------|---------|
| `--node-id` | `OCF_NODE_ID` | `node-local` | global | Stable fleet identity |
| `--data-dir` | `OCF_DATA_DIR` | _(in-memory)_ | global | Persist state to `dir/state.redb` |
| `--seed` | `OCF_SEEDS` | _(none)_ | global | Comma-separated peer `host:port`s to join |
| `--bind` | `OCF_BIND` | `0.0.0.0:8080` | `serve` | HTTP API listen address |
| `--static-dir` | `OCF_STATIC_DIR` | _(API only)_ | `serve` | Built frontend assets to serve |

## Next steps

- [Operations → Deployment](../operations/deployment.md) — single- vs multi-node, the data directory, HA.
- [Reference → Configuration](../reference/configuration.md) — the complete list.
