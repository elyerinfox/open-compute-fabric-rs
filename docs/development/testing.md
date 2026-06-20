# Testing

> What the test suite covers, what runs anywhere, what needs a real host, and how
> to run just the slice you care about.

OCF's tests follow the project's "real backends, honest errors" rule: the logic
that can be tested in-process (parsers, argument construction, state machines,
crypto, consensus, persistence) is covered by tests that **run everywhere**, while
tests that would need a live `docker`/`ip`/`ipmitool`/etc. are marked
`#[ignore]` so they don't fail on a host without those tools. Roughly **187 tests
pass** on a plain `cargo test`.

## Run the tests

```sh
cargo test                 # whole workspace
cargo test -p ocf-fabric   # one crate
cargo test -p ocf-consensus --test cluster   # one integration test target
cargo test some_test_name  # filter by name substring
```

To also run the host-dependent tests on a machine that has the tools:

```sh
cargo test -- --ignored
```

## What runs everywhere (pure tests)

These need no host tools and run on any platform:

- **Parsers** — turning real tool output (`lsblk`, `smartctl`, `ipmitool` SDR,
  `dmidecode`, `/proc` metrics) into typed models.
- **Argument construction** — verifying the exact CLI a backend *would* invoke
  (e.g. the `docker`/`virsh`/`ip`/`nft` argument vectors) without executing it.
- **State machines** — membership transitions (`Alive → Suspect → Dead`),
  placement/scope checks, RBAC permission resolution, autoscaling rules.

## In-process integration tests that DO run

Several end-to-end tests exercise the real machinery in-process — no external
services, so they run on a normal `cargo test`:

| What it proves | Where |
|----------------|-------|
| **Encrypted fabric roundtrip** — Noise XX handshake + encrypted send/recv between two in-process nodes | [`crates/ocf-fabric/src/server.rs`](../subsystems/ocf-fabric.md), `mesh.rs` |
| **3-node Raft replication** — a write committed by a quorum is visible on all members | [`crates/ocf-consensus/tests/cluster.rs`](../subsystems/ocf-consensus.md) |
| **TCP proxy echo** — the load balancer's data-plane proxy forwards bytes end to end | [`crates/ocf-loadbalancer/src/proxy.rs`](../subsystems/ocf-loadbalancer.md) |
| **redb reopen durability** — state written, store closed, reopened, and read back intact | [`crates/ocf-store/src/redb_store.rs`](../subsystems/ocf-store.md) |

These are the load-bearing proofs that the crypto, consensus, data plane, and
persistence are real rather than mocked.

## `#[ignore]`d host tests

Tests that need a real host or tool are annotated `#[ignore]`, so the default run
skips them and stays green on any developer machine:

- Backends that must actually invoke `docker`/`podman`/`lxc`/`virsh`.
- Kernel programming through `ip`/`nft`/`systemctl`.
- Disk and inventory paths needing `lsblk`/`smartctl`/`ledctl`/`dmidecode`/`ipmitool`.
- Auth backends needing `pamtester`/`ldapwhoami`/`useradd`.

Run them deliberately on a host that has the tooling with `cargo test -- --ignored`.

## Tips

- A failing build with `No space left on device` is a disk issue, not a test
  failure — see [Building → disk-space caveat](building.md#disk-space-caveat).
- Use `cargo test -p <crate>` while iterating on one subsystem; it's far faster
  than the whole workspace.

## Next steps

- [Contributing](contributing.md) — add a provider or a subsystem (and its tests).
- [Building](building.md) — build options and feature flags.
