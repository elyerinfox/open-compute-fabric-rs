# Distributed Control Plane

This is what turns a pile of subsystems into a *fleet*: durable state, encrypted
connectivity, membership with failure detection, replicated consensus, and
automatic recovery when a node drops out. Four foundation crates collaborate:

| Concern | Crate | One-liner |
|---------|-------|-----------|
| **Persistence** | [`ocf-store`](../subsystems/ocf-store.md) | Crash-safe local key/value store (redb). |
| **Connectivity** | [`ocf-fabric`](../subsystems/ocf-fabric.md) | Encrypted host-to-host mesh (X25519 + Noise XX). |
| **Membership** | [`ocf-fabric::membership`](../subsystems/ocf-fabric.md) | SWIM-style failure detector. |
| **Consensus** | [`ocf-consensus`](../subsystems/ocf-consensus.md) | Raft-replicated control-plane store (openraft). |

And the [`ocf-api` controller](../subsystems/ocf-api.md) ties them together:
state is written through Raft, applied into redb on every node, restored on boot,
and a failure-detector loop reschedules HA workloads when a node dies.

```mermaid
flowchart TB
    subgraph nodeA["Node A (leader)"]
        apiA[ocf-api controller]
        raftA[ocf-consensus<br/>Raft]
        storeA[(ocf-store<br/>redb)]
        memA[membership]
        apiA --> raftA --> storeA
        apiA --> memA
    end
    subgraph nodeB["Node B (follower)"]
        raftB[Raft]
        storeB[(redb)]
        raftB --> storeB
    end
    subgraph nodeC["Node C (follower)"]
        raftC[Raft]
        storeC[(redb)]
        raftC --> storeC
    end

    raftA <-->|"Raft RPC over<br/>Noise XX (ocf-fabric)"| raftB
    raftA <-->|"Raft RPC over<br/>Noise XX"| raftC
    raftB <-->|gossip| raftC
    memA <-->|"heartbeats / SWIM"| nodeB
    memA <-->|"heartbeats / SWIM"| nodeC
```

---

## Persistence

There are two halves to "persistent." `ocf-store` provides the first:
**node-local durability** — a single node's state survives its own reboot.

### The `StateStore` contract

```rust
pub trait StateStore: Send + Sync {
    fn put(&self, collection: &str, key: &str, value: &[u8]) -> Result<()>;
    fn get(&self, collection: &str, key: &str) -> Result<Option<Vec<u8>>>;
    fn delete(&self, collection: &str, key: &str) -> Result<()>;
    fn list(&self, collection: &str) -> Result<Vec<(String, Vec<u8>)>>;
}
```

A namespaced key/value store. `collection` acts like a table (`"workloads"`,
`"vpcs"`, `"machines"`, …); a `StateStoreExt` trait layers typed
`put_json`/`get_json`/`list_json` helpers on top. Two backends ship:

| Backend | Durability | Used for |
|---------|-----------|----------|
| `MemoryStateStore` | None (RAM) | Tests, ephemeral runs (`ocfd` with no `--data-dir`). |
| `RedbStateStore` | Crash-safe single file | `ocfd --data-dir <dir>` → `<dir>/state.redb`. |

`RedbStateStore` is verified by a test that writes, drops the database, reopens
the same file, and reads the value back — proving the reboot path.

### Restore-or-seed on boot

The controller decides at bootstrap whether to restore or seed:

```mermaid
flowchart TD
    boot["FabricController::bootstrap(config)"]
    boot --> open["Open StateStore<br/>(redb if --data-dir, else memory)"]
    open --> check{"machines collection<br/>non-empty?"}
    check -->|yes| restore["restore()<br/>reload every resource from the store"]
    check -->|no| seed["seed_demo()<br/>create a demo fleet"]
    seed --> persist["persist()<br/>write it through Raft"]
    restore --> ready["Controller ready"]
    persist --> ready
```

Because resource ids are stable (name-derived or persisted UUIDs), a restored
workload keeps the **same id** across reboots — the proof that boot restored
state rather than re-seeding it.

---

## Connectivity — the encrypted mesh

`ocf-fabric` gives every node a cryptographic identity and a real encrypted
channel to its peers. There are no plaintext control-plane links.

### Identity

A node's identity is a real **Curve25519 (X25519)** keypair (`x25519-dalek`).
`KeyPair::generate()` draws from the OS CSPRNG; `KeyPair::from_seed_name(name)`
derives a deterministic identity (for fixtures/tests) whose public key is still a
genuine X25519 point. The public-key fingerprint becomes the node's `NodeId`.

### The Noise XX handshake

Every connection runs the **`Noise_XX_25519_ChaChaPoly_BLAKE2s`** pattern (the
same primitives WireGuard uses) over a tokio TCP stream, via the `snow` crate.
XX is *mutually authenticated*: both sides learn and verify each other's static
public key.

```mermaid
sequenceDiagram
    participant C as NoiseTransport (initiator)
    participant S as FabricServer (responder)
    Note over C,S: Noise_XX_25519_ChaChaPoly_BLAKE2s
    C->>S: msg 1: -> e
    S->>C: msg 2: <- e, ee, s, es
    C->>S: msg 3: -> s, se
    Note over C,S: Both hold a TransportState;<br/>each side authenticated the other's static key
    C->>S: sealed request frame (ChaCha20-Poly1305)
    S->>C: sealed response frame
```

After the three-message handshake both peers hold a `snow::TransportState` and
every subsequent frame is sealed with ChaCha20-Poly1305. The transport exposes a
`request(node, payload) -> reply` RPC primitive (length-prefixed framing) — this
is exactly what carries Raft RPCs (below). `is_encrypted()` returns `true`
because it *is* encrypted, not as a claim.

---

## Membership & failure detection

`ocf-fabric::membership` is a SWIM-style state machine. Every node keeps a view
of every other node's liveness and ages that view as heartbeats arrive or stop.

### The liveness state machine

```mermaid
stateDiagram-v2
    [*] --> Alive: join()
    Alive --> Suspect: silent ≥ suspect_timeout
    Suspect --> Alive: heartbeat()
    Suspect --> Dead: silent ≥ suspect+dead_timeout
    Alive --> Dead: force_dead()
    Alive --> Left: leave()
    Suspect --> Left: leave()
    Dead --> [*]: reap()
    Left --> [*]: reap()
```

| State | Meaning | `is_available()` |
|-------|---------|:----------------:|
| `Alive` | Heartbeats current; schedulable & routable. | ✓ |
| `Suspect` | Missed heartbeats past `suspect_timeout`; might be a slow link. | – |
| `Dead` | Silent past `suspect + dead_timeout`, or forced. | – |
| `Left` | Graceful departure. | – |

The detector's core is `tick(now) -> Vec<MembershipEvent>`: a **pure** function
of the current time that advances every member and returns the transitions
(`Joined`, `Recovered`, `Suspected`, `Died`, `Left`). Because it's pure in `now`,
the whole failure detector is deterministically unit-testable without sleeping.

### Joining a fleet

```mermaid
sequenceDiagram
    participant N as New node (ocfd)
    participant M as Membership
    participant Mesh as FabricMesh
    N->>N: bootstrap: register each machine
    N->>M: join(FabricNode) → Alive
    N->>Mesh: join(FabricNode)
    Note over N: a background loop ticks the detector every 2s
    loop every 2s
        N->>M: tick(now)
        M-->>N: events (Suspected / Died / ...)
    end
```

"Available / schedulable" means `Alive` plus current heartbeats. The membership
view is served at `GET /api/v1/fabric/membership`, and a node can be forced dead
(an operator action or hard signal) via `POST /api/v1/fabric/machines/:id/fail`.

---

## Consensus — replicated, quorum-committed state

Node-local durability survives a *reboot*; it does not survive losing the *node*.
`ocf-consensus` provides the second half: a **Raft** cluster (openraft 0.9) whose
committed writes are applied into the `StateStore` on **every** node.

### The replicated write path

```mermaid
sequenceDiagram
    participant Ctrl as Controller (any node)
    participant L as Leader Raft
    participant F1 as Follower 1
    participant F2 as Follower 2
    participant SM as State machines (all nodes)

    Ctrl->>L: ReplicatedStore.put(collection, key, value)
    Note over L: append KvCommand::Put to the log
    L->>F1: AppendEntries (over Noise XX)
    L->>F2: AppendEntries (over Noise XX)
    F1-->>L: ack
    F2-->>L: ack
    Note over L: quorum (majority) reached → commit
    L->>SM: apply KvCommand → StateStore.put
    L-->>Ctrl: KvResponse { applied: true }
```

The replicated data type is a tiny KV command:

```rust
enum KvCommand {
    Put    { collection: String, key: String, value: Vec<u8> },
    Delete { collection: String, key: String },
}
```

The Raft **state machine** applies each committed command into an
`Arc<dyn StateStore>` — so consensus and node-local durability compose: a quorum
orders and replicates the write, and redb makes it durable on each node.

`ReplicatedStore` is the facade: `put`/`delete` are leader-only (a follower
returns a `Conflict` error naming the current leader so the caller can redirect);
`get` reads the local state machine; `wait_for_leader`, `is_leader`, and
`leader()` expose cluster status.

### Raft over the encrypted fabric

openraft is transport-agnostic. OCF provides two `RaftNetwork` implementations:

```mermaid
flowchart LR
    subgraph inproc["network.rs — in-process"]
        reg["Registry of peer Raft handles"]
    end
    subgraph fab["fabric_net.rs — cross-host"]
        f["FabricRaftNetworkFactory"]
    end
    raft["openraft Raft core"]
    raft --> inproc
    raft --> fab
    inproc -.single process / tests.-> peers1["peer Raft (same process)"]
    fab -->|"serialize RPC → NoiseTransport.request()"| noise["Noise XX over TCP"]
    noise --> peers2["peer FabricServer → serve_raft → peer Raft"]
```

| Network | Where | Used for |
|---------|-------|----------|
| `InProcessNetwork` (`network.rs`) | Same process | Single-host clusters, tests. |
| `FabricRaftNetwork` (`fabric_net.rs`) | Across hosts | Real multi-node fleets — each Raft RPC is serialized and sent over the encrypted Noise transport. |

A test (`three_node_cluster_replicates_over_encrypted_fabric`) stands up three
Raft nodes over real Noise/TCP, elects a leader, and confirms a write replicates
to all three — consensus genuinely running over the encrypted mesh.

### How the controller uses it

The `FabricController` embeds a `ReplicatedStore`. Its `persist()` routes **every**
mutation through Raft (`consensus.put(...)`), so control-plane writes are
quorum-committed before they land in redb; `restore()` reads them back on boot.
A single-node deployment is simply a quorum of one — every write is still ordered
through the Raft log.

---

## Dropping out — failure & recovery

When a node dies, the failure detector fires and the controller recovers the
workloads that asked to be recovered.

```mermaid
flowchart TD
    silent["Peer stops heartbeating"]
    silent --> tick["failure detector tick(now)"]
    tick --> suspect["→ Suspect (warn)"]
    suspect --> dead["→ Dead (after dead_timeout)"]
    dead --> handle["handle_node_dead(machine)"]
    handle --> loopw{"for each workload on the dead node"}
    loopw -->|highly_available| pick["pick a surviving machine<br/>whose scope satisfies placement"]
    pick -->|found| resched["delete on dead node →<br/>create + start on survivor"]
    pick -->|none in scope| logwarn["log: cannot place in scope"]
    loopw -->|not HA| lost["stop / mark lost"]
    resched --> persist["persist() through Raft"]
```

Recovery rules:

| Workload | On node death |
|----------|---------------|
| `highly_available = true` | Rescheduled onto a surviving machine **within its `placement` scope** ([Scopes & Placement](scopes-and-placement.md)). If no in-scope survivor exists, it's logged, not force-placed. |
| `highly_available = false` | Marked lost with the node. |

A graceful `Left` drains the node from routing/LB pools the same way, without
waiting for a timeout.

### Split-brain safety

Because control-plane writes require a Raft **majority**, a partitioned minority
cannot commit changes — it can't reschedule the same workload onto two nodes at
once. Quorum is the guard against split-brain.

---

## End-to-end: a node reboots, then a node dies

```mermaid
sequenceDiagram
    participant N as Node
    participant Store as redb
    participant Raft
    participant Mem as Membership

    Note over N: --- reboot ---
    N->>Store: open state.redb
    N->>N: machines present → restore()
    N->>Raft: start, become/await leader
    N->>Mem: register machines as Alive
    Note over N: control plane serving (same resource ids as before)

    Note over N: --- a peer dies ---
    Mem->>N: tick → Died(peer)
    N->>N: handle_node_dead(peer)
    N->>Raft: persist rescheduled workloads (quorum)
    Raft->>Store: apply → durable on every node
```

## Cross-references

- [`ocf-store`](../subsystems/ocf-store.md) · [`ocf-fabric`](../subsystems/ocf-fabric.md) · [`ocf-consensus`](../subsystems/ocf-consensus.md) · [`ocf-api`](../subsystems/ocf-api.md)
- [Scopes & Placement](scopes-and-placement.md) — the constraint HA rescheduling respects.
- [Operations → Deployment](../operations/deployment.md) — running a multi-node cluster.
- [Operations → Security](../operations/security.md) — the cryptographic model.
