# Building

> How the workspace builds, the per-crate options, the feature flag that matters,
> and the one disk-space gotcha worth knowing.

OCF is a single Cargo workspace of 16 crates (15 libraries + the `ocfd` binary).
For the repository map see [Project Layout](project-layout.md); to install the
toolchain see [Getting Started → Installation](../getting-started/installation.md).

## Workspace layout

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
  ocf-fabric        encrypted host-to-host mesh (Noise XX) + SWIM membership
  ocf-network       VPC / subnet / route / ACL overlay
  ocf-loadbalancer  TCP/ALB, TLS (ACME), DDNS
  ocf-api           axum REST API + serves the frontend
  ocfd              the monolithic binary (CLI, config, wiring)
```

`ocf-core` sits at the base; subsystem crates depend on it; `ocf-api` depends on
all of them; `ocfd` depends on `ocf-api` + `ocf-core`. See the dependency graph in
[Project Layout](project-layout.md#dependency-graph).

## Build everything

```sh
cargo build              # debug build of the whole workspace
cargo build --release    # optimized (release profile is opt-level 2)
cargo test               # build + run the test suite
```

## Per-crate builds

To compile or test a single crate without the rest of the workspace:

```sh
cargo build -p ocf-fabric
cargo test  -p ocf-consensus
cargo run   -p ocfd -- serve
```

This is the fast inner loop when you are working on one subsystem.

## Feature flags

The only feature flag you have to get right is on **openraft**, the Raft engine
behind [`ocf-consensus`](../subsystems/ocf-consensus.md). It must be built with:

```toml
openraft = { version = "0.9", features = ["serde", "storage-v2"] }
```

`serde` is needed because the replicated commands are serialized over the fabric;
`storage-v2` selects the storage API `ocf-consensus` is written against. These are
already pinned in the workspace `Cargo.toml` and the crate's own `Cargo.toml` — if
you copy the consensus crate elsewhere, carry both features with it.

## Disk-space caveat

Incremental rebuilds of a workspace this size can fill a small `target/`. If you
hit `No space left on device` mid-build, disable incremental compilation and clear
the incremental cache, then rebuild:

```sh
rm -rf target/debug/incremental
CARGO_INCREMENTAL=0 cargo build
```

Setting `CARGO_INCREMENTAL=0` trades slightly slower edit-rebuild cycles for a
much smaller `target/`. You can also `cargo clean` to reclaim everything and start
fresh.

## Cross-platform note

The workspace builds on Linux, Windows, and macOS. The data-plane integrations are
Linux-centric and shell out to host tools at runtime, so a non-Linux build still
compiles and runs the control plane; operations needing an absent tool degrade
gracefully (see [Getting Started → Installation](../getting-started/installation.md#cross-platform-support)).

## Next steps

- [Testing](testing.md) — what runs everywhere vs. what needs real hosts.
- [Contributing](contributing.md) — adding a provider or a whole subsystem.
