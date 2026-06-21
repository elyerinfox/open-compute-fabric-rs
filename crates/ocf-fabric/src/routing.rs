//! Weighted path selection across the mesh.
//!
//! The fabric is a direct full-mesh between *directly-dialable* nodes, so for a
//! `Public` peer "the fastest path" is simply the direct link. This module is
//! what makes the question meaningful for the harder cases:
//!
//! * a `Private` peer (behind NAT, no inbound) can't be dialed directly, so it
//!   must be reached **through a relay**;
//! * when several routes exist (direct, or via different relays), they are
//!   **weighed by measured RTT** and the lowest-cost one is chosen, preferring a
//!   direct path.
//!
//! [`plan_route`] is a pure function of the target, the available relays, and a
//! measured-RTT lookup, so it is exhaustively unit-testable.

use crate::node::{FabricNode, Reachability};
use crate::NodeId;

/// Assumed RTT (ms) for a reachable peer we haven't measured yet — high enough
/// that a *measured* peer is always preferred, low enough not to overflow sums.
pub const UNMEASURED_COST_MS: f64 = 1_000.0;

/// Extra cost (ms) charged to a relayed path for the second, unmeasured hop
/// (relay → target) plus forwarding overhead. Keeps a direct path preferred when
/// both are viable.
pub const RELAY_PENALTY_MS: f64 = 50.0;

/// The chosen way to reach a target node.
#[derive(Debug, Clone, PartialEq)]
pub enum RoutePlan {
    /// Dial the target directly at `endpoint`. `cost_ms` is the (measured or
    /// assumed) RTT.
    Direct { endpoint: String, cost_ms: f64 },
    /// Reach the target by forwarding through `relay` (dialed at
    /// `relay_endpoint`). `cost_ms` is the estimated total path cost.
    Relayed {
        relay: NodeId,
        relay_endpoint: String,
        target: NodeId,
        cost_ms: f64,
    },
    /// No viable route: the target is private and no relay can reach it.
    Unreachable,
}

impl RoutePlan {
    /// The estimated cost of this plan (`Unreachable` = infinity).
    pub fn cost_ms(&self) -> f64 {
        match self {
            RoutePlan::Direct { cost_ms, .. } => *cost_ms,
            RoutePlan::Relayed { cost_ms, .. } => *cost_ms,
            RoutePlan::Unreachable => f64::INFINITY,
        }
    }
}

/// Plan the lowest-cost route to `target`, weighing measured RTTs and preferring
/// a direct path.
///
/// * If `target` is directly dialable (`Public`/`Relay`), the direct route costs
///   the measured RTT to it (or [`UNMEASURED_COST_MS`]).
/// * Each relay offers a relayed route costing `RTT(relay) + RELAY_PENALTY_MS`.
/// * A direct route wins ties and is only beaten by a *strictly cheaper* relay
///   (rare) — so we don't relay a reachable peer needlessly.
///
/// `rtt(node)` returns the measured RTT (ms) to a node, or `None` if unknown.
pub fn plan_route<F>(target: &FabricNode, relays: &[&FabricNode], rtt: F) -> RoutePlan
where
    F: Fn(&NodeId) -> Option<f64>,
{
    let direct = if target.is_directly_dialable() {
        target.primary_endpoint().map(|e| RoutePlan::Direct {
            endpoint: e.to_string(),
            cost_ms: rtt(&target.node_id).unwrap_or(UNMEASURED_COST_MS),
        })
    } else {
        None
    };

    let best_relay = relays
        .iter()
        .filter(|r| r.node_id != target.node_id)
        .filter(|r| matches!(r.reachability, Reachability::Relay))
        .filter_map(|r| {
            r.primary_endpoint().map(|e| RoutePlan::Relayed {
                relay: r.node_id.clone(),
                relay_endpoint: e.to_string(),
                target: target.node_id.clone(),
                cost_ms: rtt(&r.node_id).unwrap_or(UNMEASURED_COST_MS) + RELAY_PENALTY_MS,
            })
        })
        .min_by(|a, b| {
            a.cost_ms()
                .partial_cmp(&b.cost_ms())
                .unwrap_or(std::cmp::Ordering::Greater)
        });

    match (direct, best_relay) {
        // Prefer direct unless a relay is strictly cheaper.
        (Some(d), Some(r)) => {
            if r.cost_ms() < d.cost_ms() {
                r
            } else {
                d
            }
        }
        (Some(d), None) => d,
        (None, Some(r)) => r,
        (None, None) => RoutePlan::Unreachable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KeyPair;

    fn node(name: &str, reach: Reachability) -> FabricNode {
        FabricNode::from_keypair(&KeyPair::from_seed_name(name), vec![format!("{name}:51820")])
            .with_reachability(reach)
    }

    #[test]
    fn public_target_routes_direct() {
        let target = node("pub", Reachability::Public);
        let plan = plan_route(&target, &[], |_| Some(3.0));
        assert!(matches!(plan, RoutePlan::Direct { cost_ms, .. } if cost_ms == 3.0));
    }

    #[test]
    fn private_target_routes_via_relay() {
        let target = node("priv", Reachability::Private);
        let relay = node("relay", Reachability::Relay);
        let relays = [&relay];
        let plan = plan_route(&target, &relays, |id| {
            if *id == relay.node_id {
                Some(2.0)
            } else {
                None
            }
        });
        match plan {
            RoutePlan::Relayed { relay: r, cost_ms, .. } => {
                assert_eq!(r, relay.node_id);
                assert_eq!(cost_ms, 2.0 + RELAY_PENALTY_MS);
            }
            other => panic!("expected relayed, got {other:?}"),
        }
    }

    #[test]
    fn private_target_with_no_relay_is_unreachable() {
        let target = node("priv", Reachability::Private);
        let plan = plan_route(&target, &[], |_| None);
        assert_eq!(plan, RoutePlan::Unreachable);
    }

    #[test]
    fn lowest_rtt_relay_wins() {
        let target = node("priv", Reachability::Private);
        let near = node("near", Reachability::Relay);
        let far = node("far", Reachability::Relay);
        let relays = [&near, &far];
        let plan = plan_route(&target, &relays, |id| {
            if *id == near.node_id {
                Some(1.0)
            } else if *id == far.node_id {
                Some(40.0)
            } else {
                None
            }
        });
        match plan {
            RoutePlan::Relayed { relay, .. } => assert_eq!(relay, near.node_id),
            other => panic!("expected relayed via near, got {other:?}"),
        }
    }

    #[test]
    fn direct_preferred_over_relay_on_ties() {
        // Target is public (direct possible) and a relay exists at equal cost.
        let target = node("pub", Reachability::Public);
        let relay = node("relay", Reachability::Relay);
        let relays = [&relay];
        // Direct RTT 10; relay RTT 10 + penalty 50 = 60. Direct wins.
        let plan = plan_route(&target, &relays, |_| Some(10.0));
        assert!(matches!(plan, RoutePlan::Direct { .. }));
    }
}
