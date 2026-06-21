//! Node identity and keying material for the encrypted mesh.
//!
//! Keys are **real Curve25519 (X25519)** keypairs — the same static keys the
//! Noise transport ([`crate::transport`]) uses for its handshake, so a node's
//! mesh identity and its transport identity are one and the same. Keys are
//! stored as raw bytes with a hex view for display/serialization.
//!
//! [`KeyPair::generate`] uses the OS CSPRNG; [`KeyPair::from_seed_name`] derives
//! a deterministic identity from a name (handy for fixtures and tests) — the
//! private scalar is a name-seeded expansion and the public key is derived from
//! it by real X25519 base-point multiplication, so it interoperates with Noise.

use ocf_core::prelude::*;
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};

/// A stable identifier for a node participating in the fabric mesh.
///
/// Distinct from [`ocf_core::id::Id`]: a node id is the mesh-level handle a peer
/// is addressed by, and is typically derived from (or pinned to) the node's
/// public key fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn new(id: impl Into<String>) -> Self {
        NodeId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for NodeId {
    fn from(s: String) -> Self {
        NodeId(s)
    }
}

impl From<&str> for NodeId {
    fn from(s: &str) -> Self {
        NodeId(s.to_string())
    }
}

/// A public key, stored as raw bytes and viewable as hex.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub Vec<u8>);

impl PublicKey {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        PublicKey(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Lowercase hex rendering, suitable for fingerprints and logs.
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    /// The key as a **WireGuard public key**: standard base64 of the 32-byte
    /// Curve25519 point. Our identity keys are X25519, which is exactly
    /// WireGuard's key type, so a node's fabric identity *is* its WireGuard
    /// identity — no separate keyset.
    pub fn to_wireguard_key(&self) -> String {
        base64(&self.0)
    }
}

impl std::fmt::Display for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A secret key, stored as raw bytes.
///
/// Serialization is supported so the controller can persist a node's identity to
/// its data directory; keep it out of any cross-node wire format.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretKey(pub Vec<u8>);

impl SecretKey {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        SecretKey(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The key as a **WireGuard private key**: standard base64 of the 32-byte
    /// Curve25519 scalar. Feed to `wg set <iface> private-key`.
    pub fn to_wireguard_key(&self) -> String {
        base64(&self.0)
    }
}

// Hand-rolled Debug so secret bytes never leak into logs.
impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SecretKey").field(&"<redacted>").finish()
    }
}

/// A public/secret keypair identifying a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyPair {
    pub public: PublicKey,
    pub secret: SecretKey,
}

impl KeyPair {
    /// Construct a keypair from existing key material.
    pub fn new(public: PublicKey, secret: SecretKey) -> Self {
        KeyPair { public, secret }
    }

    /// Generate a fresh X25519 keypair from the OS CSPRNG.
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        Self::from_static_secret(secret)
    }

    /// Derive a deterministic X25519 keypair from a stable name, so the same
    /// name always yields the same identity (useful for tests and fixtures).
    ///
    /// The private scalar is a name-seeded 32-byte expansion; the public key is
    /// its real X25519 base-point product, so the identity is a genuine
    /// Curve25519 key that the Noise transport accepts.
    pub fn from_seed_name(name: &str) -> Self {
        let mut scalar = [0u8; 32];
        let lo = expand_to_32(&seed16(name.as_bytes()));
        scalar.copy_from_slice(&lo);
        Self::from_static_secret(StaticSecret::from(scalar))
    }

    /// Build a [`KeyPair`] from existing 32-byte private key material.
    pub fn from_private_bytes(private: [u8; 32]) -> Self {
        Self::from_static_secret(StaticSecret::from(private))
    }

    fn from_static_secret(secret: StaticSecret) -> Self {
        let public = X25519Public::from(&secret);
        KeyPair {
            public: PublicKey::from_bytes(public.to_bytes().to_vec()),
            secret: SecretKey::from_bytes(secret.to_bytes().to_vec()),
        }
    }

    /// Derive a [`NodeId`] from the public key fingerprint.
    pub fn node_id(&self) -> NodeId {
        NodeId(fingerprint(&self.public))
    }
}

/// A short, stable fingerprint of a public key (first 8 bytes, hex).
pub fn fingerprint(public: &PublicKey) -> String {
    let take = public.0.len().min(8);
    to_hex(&public.0[..take])
}

/// Expand a 16-byte seed into a 32-byte pseudo-key by concatenating the bytes
/// with their byte-reversed copy. Deterministic and dependency-free.
fn expand_to_32(seed: &[u8; 16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(seed);
    out.extend(seed.iter().rev());
    out
}

/// Deterministically derive a 16-byte seed from arbitrary input using eight
/// FNV-1a passes (each with a distinct round byte) to fill the buffer.
///
/// This drives only [`KeyPair::from_seed_name`], which mints **deterministic
/// test/fixture identities** from a name — the resulting bytes are clamped and
/// used as a real X25519 private scalar. Production identities come from
/// [`KeyPair::generate`], which uses the OS CSPRNG; this name expansion is never
/// on that path, so it does not need to be a cryptographic KDF.
fn seed16(input: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (chunk, pair) in out.chunks_mut(2).enumerate() {
        let h = fnv1a(input, chunk as u8);
        pair[0] = (h & 0xff) as u8;
        pair[1] = ((h >> 8) & 0xff) as u8;
    }
    out
}

/// A 64-bit FNV-1a hash with a per-round seed byte folded in up front.
fn fnv1a(input: &[u8], round: u8) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET ^ (round as u64);
    for &b in input {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Lowercase hex encoding without pulling in an external crate.
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Standard base64 encoding (RFC 4648, with `=` padding) — used for WireGuard
/// key formatting. A 32-byte key encodes to the 44-char form `wg` expects.
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(A[(b0 >> 2) as usize] as char);
        out.push(A[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_yields_32_byte_keys() {
        let kp = KeyPair::generate();
        assert_eq!(kp.public.as_bytes().len(), 32);
        assert_eq!(kp.secret.as_bytes().len(), 32);
    }

    #[test]
    fn seeded_keypair_is_deterministic() {
        let a = KeyPair::from_seed_name("node-a");
        let b = KeyPair::from_seed_name("node-a");
        assert_eq!(a.public, b.public);
        assert_eq!(a.node_id(), b.node_id());
    }

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 vectors.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn wireguard_keys_are_44_char_base64() {
        // A 32-byte X25519 key renders to WireGuard's 44-char base64 form.
        let kp = KeyPair::from_seed_name("wg-node");
        let pk = kp.public.to_wireguard_key();
        let sk = kp.secret.to_wireguard_key();
        assert_eq!(pk.len(), 44);
        assert!(pk.ends_with('='));
        assert_eq!(sk.len(), 44);
        // Stable: same identity → same WG key.
        assert_eq!(pk, KeyPair::from_seed_name("wg-node").public.to_wireguard_key());
    }

    #[test]
    fn hex_roundtrips_known_value() {
        assert_eq!(PublicKey::from_bytes(vec![0x0a, 0xff]).to_hex(), "0aff");
    }

    #[test]
    fn secret_debug_is_redacted() {
        let s = SecretKey::from_bytes(vec![1, 2, 3]);
        assert!(format!("{s:?}").contains("redacted"));
    }
}
