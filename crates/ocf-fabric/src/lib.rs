//! # ocf-fabric
//!
//! The host-to-host encrypted mesh that fabric nodes use to reach each other.
//!
//! A node has an identity ([`crypto`]): a real X25519 [`KeyPair`] whose
//! public-key fingerprint becomes its [`NodeId`]. Membership records
//! ([`FabricNode`]) carry that identity plus dialable endpoints and liveness.
//! Bytes move over a pluggable [`FabricTransport`]; the built-in
//! [`NoiseTransport`] is a **real** Noise XX transport (X25519 + ChaCha20-Poly1305
//! over TCP), and [`FabricServer`] is the matching listener. The [`FabricMesh`]
//! holds membership and fans broadcasts out across the transport, while
//! [`membership::Membership`] runs SWIM-style heartbeat/failure detection.
//!
//! What is real here: key generation, the mutually-authenticated handshake, frame
//! sealing, and the membership state machine. What is still simplified: peer
//! discovery is seed-driven and gossip dissemination is basic (see
//! [`membership`]). The same `FabricTransport` trait is the seam a production
//! WireGuard data plane would slot into.

pub mod crypto;
pub mod membership;
pub mod mesh;
pub mod node;
pub mod routing;
pub mod server;
pub mod transport;
pub mod wire;

pub use crypto::{fingerprint, KeyPair, NodeId, PublicKey, SecretKey};
pub use membership::{Liveness, Membership, MembershipEvent};
pub use mesh::FabricMesh;
pub use node::{FabricNode, Reachability};
pub use routing::RouteGraph;
pub use server::{FabricServer, FabricStreamServer};
pub use transport::{register_builtins, FabricTransport, NoiseTransport};
