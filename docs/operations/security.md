# Security

> The security model: an encrypted mutually-authenticated fabric, pluggable
> authentication, scoped RBAC, careful secret handling, TLS, and what is still
> being hardened.

This page is an honest account of how OCF protects the fleet — what is real
cryptography today and where the rough edges are. Per-subsystem detail lives in
[`ocf-fabric`](../subsystems/ocf-fabric.md), [`ocf-auth`](../subsystems/ocf-auth.md),
[`ocf-authz`](../subsystems/ocf-authz.md), and
[`ocf-loadbalancer`](../subsystems/ocf-loadbalancer.md).

## Asset → protection at a glance

| Asset | Protection |
|-------|------------|
| Host-to-host traffic (mesh, Raft log) | Real X25519 key agreement + **Noise XX** handshake — encrypted and **mutually authenticated** |
| Operator/API login | Pluggable `Authenticator` (PAM, Active Directory, local); local secret checked in **constant time** |
| API actions | RBAC with roles, groups, users, and **scope-bound** role bindings |
| Control-plane integrity (split-brain) | **Raft quorum** — only the side with a majority can commit |
| Cloudflare DDNS token | Held in memory, sent only as an HTTPS bearer header, **never logged**, never in error text |
| ACME / TLS material | Issued and renewed via `certbot`; certificate provider behind a contract |
| IPMI / BMC credentials | Passed to `ipmitool` as the tool requires; not logged |
| AD/LDAP bind password | Passed to `ldapwhoami`; **not logged** — see hardening note below |
| Secret keys in memory | `Debug` is `<redacted>` so keys never print in logs or panics |

## Encrypted fabric

The host-to-host mesh is **real cryptography**, not a placeholder. Each node has
an X25519 static keypair; links are established with the **Noise XX** pattern,
which gives an encrypted channel *and* mutual authentication — both ends prove
possession of their static key during the handshake, so a peer cannot be
impersonated. All inter-node traffic, including the replicated Raft log, rides
this encrypted transport. Secret keys redact themselves in `Debug` output (they
print as `<redacted>`), so a stray log line or panic message can't leak key
material. See [`ocf-fabric`](../subsystems/ocf-fabric.md) and
[`crates/ocf-fabric/src/crypto.rs`](../subsystems/ocf-fabric.md).

## Authentication

Authentication is a pluggable `Authenticator` contract with three built-in
backends ([`ocf-auth`](../subsystems/ocf-auth.md)):

- **PAM** — `pamtester`, the host's own auth stack.
- **Active Directory / LDAP** — a real LDAP simple bind via `ldapwhoami`;
  `memberOf` groups are read back and flow into RBAC.
- **Local** — an in-process user store. The supplied secret is verified with a
  **constant-time comparison**, so the check does not leak the secret through
  timing.

A successful login yields an `Identity` (user + resolved groups) that the
authorization layer consumes. Optionally, host-account sync materializes local
Linux users (`useradd`) for authenticated principals.

## Authorization (RBAC)

Authorization is Proxmox-style RBAC ([`ocf-authz`](../subsystems/ocf-authz.md)):
**roles** bundle permissions, **users** belong to **groups**, and **role
bindings** grant a role to a subject **at a scope**. Because bindings are
scope-bound, a grant can be as narrow as a single machine or as broad as the
whole fleet — the same [`Scope`](../architecture/scopes-and-placement.md)
hierarchy used for placement. A permission check passes only if some binding for
the subject (directly or via a group) grants the required permission at a scope
that contains the target.

## Secrets handling

Secrets are kept out of logs and error text on purpose:

- The **Cloudflare DDNS token** is stored in memory, sent only as an
  `Authorization: Bearer …` header over HTTPS, and is **never logged** and never
  included in any error message.
- **ACME/TLS** private material is managed by `certbot`; OCF drives issuance and
  renewal through a `CertificateProvider` contract rather than handling raw keys
  itself.
- **IPMI/BMC** credentials are handed to `ipmitool` as it requires and are not
  logged.
- In-memory **secret keys** redact in `Debug` (`<redacted>`).

## TLS termination

Public load-balancer listeners terminate TLS with certificates obtained via
**ACME** (`certbot` / Let's Encrypt), and the load balancer subsystem renews them
on schedule. See [`ocf-loadbalancer`](../subsystems/ocf-loadbalancer.md).

## Split-brain prevention

The control plane is Raft-replicated, and Raft requires a **quorum** to commit.
Under a network partition only the side with a majority of nodes can make
progress, so the fleet cannot diverge into two authoritative copies. See
[Operations → Deployment](deployment.md#raft-replication-of-the-control-plane)
and [`ocf-consensus`](../subsystems/ocf-consensus.md).

## Honest about hardening

OCF prefers a real, working integration with a noted rough edge over a stub, so a
few items are explicitly on the hardening list:

- **AD/LDAP bind password via `-w`.** The OpenLDAP `ldapwhoami` client takes the
  bind password as the `-w <password>` argument, so during the bind that
  argument is briefly visible in `ps` / `/proc` on the node running it. It is
  **never logged** by OCF. The documented hardening step is to switch to
  `-y <file>` reading the password from a private file descriptor.
- **Demo credentials.** First boot seeds a `local` `admin`/`admin` user for
  convenience; change or remove it before any real exposure.
- **Bind exposure.** `--bind` defaults to `0.0.0.0:8080` (all interfaces); bind to
  a specific interface or `127.0.0.1` and front it appropriately for production.

## Related

- [Operations → Deployment](deployment.md) — fleet formation, the data directory, HA.
- [Architecture → Distributed Control Plane](../architecture/distributed-control-plane.md) — fabric + Raft together.
