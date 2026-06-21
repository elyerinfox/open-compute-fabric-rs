//! IP address management (IPAM): per-subnet allocation of host addresses.
//!
//! Each subnet owns a [`SubnetAllocator`] that hands out free host addresses
//! from the subnet's CIDR, skipping the network address (`.0`), the gateway
//! (`.1`, assigned to the subnet bridge), and the broadcast address. Allocation
//! is deterministic (lowest free host first), so a fresh allocator with the same
//! reservations always yields the same next address — which keeps restore
//! idempotent.
//!
//! The implementation is std-only (no IP-parsing crate): IPv4 addresses are
//! manipulated as `u32`s.

use ocf_core::error::{Error, Result};
use std::collections::BTreeSet;

/// Parse a dotted-quad IPv4 string into a `u32`. Returns `None` for anything
/// that is not exactly four `0..=255` octets.
pub fn parse_ipv4(s: &str) -> Option<u32> {
    let octets: Vec<&str> = s.trim().split('.').collect();
    if octets.len() != 4 {
        return None;
    }
    let mut acc: u32 = 0;
    for octet in octets {
        let v: u8 = octet.parse().ok()?;
        acc = (acc << 8) | u32::from(v);
    }
    Some(acc)
}

/// Render a `u32` as a dotted-quad IPv4 string.
pub fn format_ipv4(n: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        (n >> 24) & 0xff,
        (n >> 16) & 0xff,
        (n >> 8) & 0xff,
        n & 0xff
    )
}

/// Parse `a.b.c.d/p` into `(network_base_u32, prefix_len)`, validating the four
/// octets and `p <= 32`. The base is masked to the network boundary.
pub fn parse_cidr(cidr: &str) -> Option<(u32, u8)> {
    let (addr, prefix) = cidr.split_once('/')?;
    let addr = parse_ipv4(addr)?;
    let prefix: u8 = prefix.parse().ok()?;
    if prefix > 32 {
        return None;
    }
    // Mask the address down to its network boundary.
    let mask: u32 = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    Some((addr & mask, prefix))
}

/// A deterministic, in-memory allocator for one subnet's host addresses.
#[derive(Debug, Clone)]
pub struct SubnetAllocator {
    base: u32,
    prefix: u8,
    used: BTreeSet<u32>,
}

impl SubnetAllocator {
    /// Build an allocator for `cidr` (e.g. `"10.0.1.0/24"`). The gateway (`.1`)
    /// is reserved up front so it is never handed to a workload.
    pub fn new(cidr: &str) -> Result<Self> {
        let (base, prefix) = parse_cidr(cidr)
            .ok_or_else(|| Error::invalid(format!("invalid subnet CIDR `{cidr}`")))?;
        let mut used = BTreeSet::new();
        // Reserve the gateway address (.1) — it lives on the subnet bridge.
        if let Some(gw) = checked_host(base, prefix, 1) {
            used.insert(gw);
        }
        Ok(SubnetAllocator { base, prefix, used })
    }

    /// The first assignable host (`.2`: after network `.0` and gateway `.1`).
    fn first_host(&self) -> u32 {
        self.base.wrapping_add(2)
    }

    /// The last assignable host (one below broadcast). For `/31` and `/32` there
    /// is no usable range, so this is below `first_host` and allocation fails.
    fn last_host(&self) -> u32 {
        if self.prefix >= 31 {
            // No usable host range in a point-to-point or host route.
            return self.base; // < first_host → empty range
        }
        let size: u32 = 1u32 << (32 - self.prefix);
        self.base.wrapping_add(size).wrapping_sub(2) // broadcast - 1
    }

    /// Allocate the lowest free host address, returning it as a dotted quad.
    /// Fails with [`Error::Conflict`] when the subnet is exhausted.
    pub fn allocate(&mut self) -> Result<String> {
        let (lo, hi) = (self.first_host(), self.last_host());
        if hi < lo {
            return Err(Error::Conflict(format!(
                "subnet /{} has no assignable host addresses",
                self.prefix
            )));
        }
        let mut candidate = lo;
        loop {
            if !self.used.contains(&candidate) {
                self.used.insert(candidate);
                return Ok(format_ipv4(candidate));
            }
            if candidate == hi {
                return Err(Error::Conflict("subnet address pool exhausted".to_string()));
            }
            candidate = candidate.wrapping_add(1);
        }
    }

    /// Mark `addr` as in use (e.g. restoring a previously-assigned address).
    /// Errors if `addr` is unparseable or outside this subnet.
    pub fn reserve(&mut self, addr: &str) -> Result<()> {
        let n = parse_ipv4(addr)
            .ok_or_else(|| Error::invalid(format!("invalid address `{addr}`")))?;
        if !self.contains(n) {
            return Err(Error::invalid(format!(
                "address `{addr}` is outside this subnet"
            )));
        }
        self.used.insert(n);
        Ok(())
    }

    /// Return `addr` to the free pool. Unparseable or already-free addresses are
    /// ignored.
    pub fn release(&mut self, addr: &str) {
        if let Some(n) = parse_ipv4(addr) {
            // Never release the gateway.
            if checked_host(self.base, self.prefix, 1) != Some(n) {
                self.used.remove(&n);
            }
        }
    }

    /// Whether `n` falls within this subnet's address range.
    fn contains(&self, n: u32) -> bool {
        let mask: u32 = if self.prefix == 0 {
            0
        } else {
            u32::MAX << (32 - self.prefix)
        };
        (n & mask) == self.base
    }

    /// Count of currently-allocated addresses (including the reserved gateway).
    pub fn used_count(&self) -> usize {
        self.used.len()
    }
}

/// Compute `base + offset` if it lands inside the subnet, else `None`.
fn checked_host(base: u32, prefix: u8, offset: u32) -> Option<u32> {
    if prefix >= 32 {
        return None;
    }
    let size: u32 = 1u32 << (32 - prefix);
    if offset < size {
        Some(base.wrapping_add(offset))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_roundtrips() {
        for s in ["0.0.0.0", "10.0.1.5", "192.168.255.1", "255.255.255.255"] {
            assert_eq!(format_ipv4(parse_ipv4(s).unwrap()), s);
        }
        assert_eq!(parse_ipv4("10.0.0"), None);
        assert_eq!(parse_ipv4("10.0.0.256"), None);
        assert_eq!(parse_ipv4("nope"), None);
    }

    #[test]
    fn parse_cidr_masks_to_network() {
        // A host bit set in the address is masked off to the network base.
        assert_eq!(parse_cidr("10.0.1.37/24"), Some((parse_ipv4("10.0.1.0").unwrap(), 24)));
        assert_eq!(parse_cidr("10.0.0.0/8"), Some((parse_ipv4("10.0.0.0").unwrap(), 8)));
        assert_eq!(parse_cidr("10.0.0.0/33"), None);
        assert_eq!(parse_cidr("garbage"), None);
    }

    #[test]
    fn allocates_from_two_skipping_network_and_gateway() {
        let mut a = SubnetAllocator::new("10.0.1.0/24").unwrap();
        // .1 is the reserved gateway, so first allocation is .2.
        assert_eq!(a.allocate().unwrap(), "10.0.1.2");
        assert_eq!(a.allocate().unwrap(), "10.0.1.3");
        assert_eq!(a.allocate().unwrap(), "10.0.1.4");
    }

    #[test]
    fn reserve_then_allocate_skips_reserved() {
        let mut a = SubnetAllocator::new("10.0.1.0/24").unwrap();
        a.reserve("10.0.1.2").unwrap();
        a.reserve("10.0.1.3").unwrap();
        // .2 and .3 are taken, so the next free is .4.
        assert_eq!(a.allocate().unwrap(), "10.0.1.4");
        // Out-of-subnet reservation is rejected.
        assert!(a.reserve("10.9.9.9").unwrap_err().code() == "invalid_argument");
    }

    #[test]
    fn release_returns_address_to_pool() {
        let mut a = SubnetAllocator::new("10.0.1.0/24").unwrap();
        let first = a.allocate().unwrap(); // .2
        let _second = a.allocate().unwrap(); // .3
        a.release(&first);
        // .2 is free again, so it's handed out next (lowest-first).
        assert_eq!(a.allocate().unwrap(), "10.0.1.2");
        // Releasing the gateway is a no-op (it stays reserved).
        a.release("10.0.1.1");
        assert!(a.reserve("10.0.1.1").is_ok()); // already reserved, still fine
    }

    #[test]
    fn small_subnet_exhausts() {
        // /30 has hosts .1 (gw, reserved) .2 (broadcast is .3). Usable: just .2.
        let mut a = SubnetAllocator::new("10.0.1.0/30").unwrap();
        assert_eq!(a.allocate().unwrap(), "10.0.1.2");
        assert!(a.allocate().unwrap_err().code() == "conflict");
    }

    #[test]
    fn point_to_point_has_no_hosts() {
        let mut a = SubnetAllocator::new("10.0.1.0/31").unwrap();
        assert!(a.allocate().is_err());
    }
}
