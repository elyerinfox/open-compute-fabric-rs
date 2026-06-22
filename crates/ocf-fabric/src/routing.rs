//! Graph-aware packet routing across the mesh.
//!
//! The fabric is a direct full-mesh between *directly-dialable* nodes, but a
//! `Private` (NAT'd) node can't be dialed and two private nodes can't reach each
//! other at all — they must go **through a relay**. [`RouteGraph`] models the
//! fabric as a weighted graph (nodes with [`Reachability`], edges between any
//! pair that can WireGuard-peer, weighted by measured RTT) and computes the
//! **shortest path** between any two nodes with Dijkstra. From that it derives,
//! per destination, the **next-hop relay** to send through — so different
//! destinations can use different relays, each chosen by the *whole*
//! `self → relay → dest` cost, and multi-hop relay chains fall out naturally.
//!
//! Edges only exist where two nodes can directly peer: any pair that is **not
//! both private**. The cost of an edge is the measured RTT in either direction,
//! or [`DEFAULT_EDGE_MS`] until a measurement is known (so a node with a real
//! latency view routes optimally, and one without still finds *a* path).

use crate::node::Reachability;
use crate::NodeId;
use std::cmp::Ordering;
use std::collections::HashMap;

/// Assumed edge RTT (ms) until a real measurement is observed. High enough that a
/// measured edge always wins, low enough that sums of a few stay finite.
pub const DEFAULT_EDGE_MS: f64 = 1_000.0;

/// A weighted reachability graph of the fabric, for shortest-path routing.
#[derive(Debug, Default, Clone)]
pub struct RouteGraph {
    reach: HashMap<NodeId, Reachability>,
    /// Directed measured RTT (ms) between nodes; either direction satisfies an edge.
    rtt: HashMap<(NodeId, NodeId), f64>,
}

impl RouteGraph {
    pub fn new() -> Self {
        RouteGraph::default()
    }

    /// Add (or update) a node and its reachability.
    pub fn add_node(&mut self, id: NodeId, reach: Reachability) {
        self.reach.insert(id, reach);
    }

    /// Record a measured RTT (ms) from `from` to `to` — one observed edge weight.
    pub fn observe_rtt(&mut self, from: NodeId, to: NodeId, rtt_ms: f64) {
        self.rtt.insert((from, to), rtt_ms);
    }

    fn node_ids(&self) -> Vec<NodeId> {
        self.reach.keys().cloned().collect()
    }

    /// Whether `a` and `b` can hold a direct WireGuard tunnel: both known, and not
    /// *both* private (a NAT'd pair has no direct path).
    fn can_link(&self, a: &NodeId, b: &NodeId) -> bool {
        match (self.reach.get(a), self.reach.get(b)) {
            (Some(ra), Some(rb)) => {
                !(matches!(ra, Reachability::Private) && matches!(rb, Reachability::Private))
            }
            _ => false,
        }
    }

    /// The cost of the edge `a–b`, or `None` when they can't directly peer.
    fn edge_cost(&self, a: &NodeId, b: &NodeId) -> Option<f64> {
        if a == b || !self.can_link(a, b) {
            return None;
        }
        let measured = self
            .rtt
            .get(&(a.clone(), b.clone()))
            .or_else(|| self.rtt.get(&(b.clone(), a.clone())))
            .copied();
        Some(measured.unwrap_or(DEFAULT_EDGE_MS))
    }

    /// The shortest path from `src` to `dst` (Dijkstra) as the ordered hops
    /// **after** `src` (so the last element is `dst`). `Some(vec![])` when
    /// `src == dst`; `None` when `dst` is unreachable.
    pub fn path(&self, src: &NodeId, dst: &NodeId) -> Option<Vec<NodeId>> {
        if !self.reach.contains_key(src) || !self.reach.contains_key(dst) {
            return None;
        }
        if src == dst {
            return Some(Vec::new());
        }
        let nodes = self.node_ids();
        let mut dist: HashMap<NodeId, f64> =
            nodes.iter().map(|n| (n.clone(), f64::INFINITY)).collect();
        let mut prev: HashMap<NodeId, NodeId> = HashMap::new();
        let mut done: HashMap<NodeId, bool> = nodes.iter().map(|n| (n.clone(), false)).collect();
        dist.insert(src.clone(), 0.0);

        loop {
            // Cheapest unvisited node.
            let next = nodes
                .iter()
                .filter(|n| !done[*n])
                .min_by(|a, b| dist[*a].partial_cmp(&dist[*b]).unwrap_or(Ordering::Equal))
                .cloned();
            let Some(u) = next else { break };
            if dist[&u].is_infinite() {
                break; // remaining nodes are unreachable
            }
            done.insert(u.clone(), true);
            if &u == dst {
                break;
            }
            for v in &nodes {
                if done[v] {
                    continue;
                }
                if let Some(w) = self.edge_cost(&u, v) {
                    let cand = dist[&u] + w;
                    if cand < dist[v] {
                        dist.insert(v.clone(), cand);
                        prev.insert(v.clone(), u.clone());
                    }
                }
            }
        }

        if dist[dst].is_infinite() {
            return None;
        }
        let mut hops = Vec::new();
        let mut cur = dst.clone();
        while &cur != src {
            hops.push(cur.clone());
            cur = prev.get(&cur)?.clone();
        }
        hops.reverse();
        Some(hops)
    }

    /// The next hop to send through to reach `dst`: the first node on the shortest
    /// path when it is **multi-hop** (a relay), or `None` when `dst` is directly
    /// reachable (one hop) or unreachable.
    pub fn next_relay(&self, src: &NodeId, dst: &NodeId) -> Option<NodeId> {
        let hops = self.path(src, dst)?;
        if hops.len() <= 1 {
            None
        } else {
            Some(hops[0].clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> NodeId {
        NodeId::new(s)
    }

    fn graph(nodes: &[(&str, Reachability)]) -> RouteGraph {
        let mut g = RouteGraph::new();
        for (id, r) in nodes {
            g.add_node(n(id), *r);
        }
        g
    }

    #[test]
    fn direct_between_public_nodes() {
        let g = graph(&[("a", Reachability::Public), ("b", Reachability::Public)]);
        assert_eq!(g.path(&n("a"), &n("b")), Some(vec![n("b")]));
        assert_eq!(g.next_relay(&n("a"), &n("b")), None); // direct
    }

    #[test]
    fn private_to_public_is_direct() {
        // A private node can dial a public one (reverse-connect): a direct edge.
        let g = graph(&[("p", Reachability::Private), ("pub", Reachability::Public)]);
        assert_eq!(g.next_relay(&n("p"), &n("pub")), None);
    }

    #[test]
    fn two_private_nodes_route_through_the_relay() {
        let g = graph(&[
            ("a", Reachability::Private),
            ("b", Reachability::Private),
            ("r", Reachability::Relay),
        ]);
        // No direct a–b edge; the only path is a → r → b.
        assert_eq!(g.path(&n("a"), &n("b")), Some(vec![n("r"), n("b")]));
        assert_eq!(g.next_relay(&n("a"), &n("b")), Some(n("r")));
    }

    #[test]
    fn two_private_nodes_with_no_relay_are_unreachable() {
        let g = graph(&[("a", Reachability::Private), ("b", Reachability::Private)]);
        assert_eq!(g.path(&n("a"), &n("b")), None);
        assert_eq!(g.next_relay(&n("a"), &n("b")), None);
    }

    #[test]
    fn picks_the_relay_with_the_lowest_total_cost() {
        // Two relays; per-destination the graph weighs self→relay + relay→dest.
        let mut g = graph(&[
            ("a", Reachability::Private),
            ("b", Reachability::Private),
            ("r1", Reachability::Relay),
            ("r2", Reachability::Relay),
        ]);
        // a is close to r1 (1) and far from r2 (50); but r1→b is far (100) while
        // r2→b is near (2). Totals: via r1 = 1+100 = 101; via r2 = 50+2 = 52.
        g.observe_rtt(n("a"), n("r1"), 1.0);
        g.observe_rtt(n("a"), n("r2"), 50.0);
        g.observe_rtt(n("r1"), n("b"), 100.0);
        g.observe_rtt(n("r2"), n("b"), 2.0);
        // Graph-aware: r2 wins on total cost even though r1 is the nearer relay.
        assert_eq!(g.next_relay(&n("a"), &n("b")), Some(n("r2")));
    }

    #[test]
    fn different_destinations_use_different_relays() {
        // a reaches b best via r1, and c best via r2 — multiple relays in use.
        let mut g = graph(&[
            ("a", Reachability::Private),
            ("b", Reachability::Private),
            ("c", Reachability::Private),
            ("r1", Reachability::Relay),
            ("r2", Reachability::Relay),
        ]);
        g.observe_rtt(n("a"), n("r1"), 1.0);
        g.observe_rtt(n("a"), n("r2"), 1.0);
        g.observe_rtt(n("r1"), n("b"), 1.0);
        g.observe_rtt(n("r2"), n("b"), 99.0);
        g.observe_rtt(n("r1"), n("c"), 99.0);
        g.observe_rtt(n("r2"), n("c"), 1.0);
        assert_eq!(g.next_relay(&n("a"), &n("b")), Some(n("r1")));
        assert_eq!(g.next_relay(&n("a"), &n("c")), Some(n("r2")));
    }
}
