//! Backend selection: the policy- and ingress-aware routing core.
//!
//! [`select_backend`] is a pure function — given a policy, the currently known
//! backends, and the client's context, it returns the backend a request should
//! be routed to (or `None` when there are no candidates). It is deliberately
//! side-effect free so it can be unit-tested exhaustively and called from both
//! the controller and the API layer.

use crate::model::{Backend, ClientContext, RoutingPolicy};
use parking_lot::Mutex;

/// Monotonic counter backing the round-robin policy.
///
/// `select_backend` is stateless on its arguments, so the round-robin cursor
/// lives here in a process-global counter. It only needs to advance fairly; it
/// does not need to be tied to a particular load balancer.
static ROUND_ROBIN: Mutex<usize> = Mutex::new(0);

/// Choose a backend for a request under `policy`.
///
/// All policies are *ingress-aware*: when the client's `ingress_scope` is known,
/// backends whose scope is contained by that ingress scope are preferred, and
/// the policy only falls back to the full set if no backend is in-scope. This is
/// what lets an anycast / scoped load balancer keep traffic local to the ingress
/// while still degrading gracefully.
///
/// Within the preferred set, the policy decides:
/// * [`RoutingPolicy::RoundRobin`] — next backend by a rotating index.
/// * [`RoutingPolicy::LeastLoad`] — the backend with the smallest `load`.
/// * [`RoutingPolicy::Latency`] — the backend with the smallest `latency_ms`.
/// * [`RoutingPolicy::Geo`] — a backend whose `geo` matches the client's `geo`,
///   falling back to latency when geography is unknown or unmatched.
///
/// Returns `None` only when `backends` is empty.
pub fn select_backend(
    policy: RoutingPolicy,
    backends: &[Backend],
    client: &ClientContext,
) -> Option<Backend> {
    if backends.is_empty() {
        return None;
    }

    // Prefer backends local to the ingress scope; fall back to all of them.
    let candidates = ingress_local_candidates(backends, client);

    let chosen = match policy {
        RoutingPolicy::RoundRobin => pick_round_robin(&candidates),
        RoutingPolicy::LeastLoad => pick_min_by(&candidates, |b| b.load),
        RoutingPolicy::Latency => pick_min_by(&candidates, |b| b.latency_ms),
        RoutingPolicy::Geo => pick_geo(&candidates, client),
    };

    chosen.cloned()
}

/// The subset of `backends` whose scope is contained by the client's ingress
/// scope, or all of them when no ingress scope is set or none are in-scope.
fn ingress_local_candidates<'a>(
    backends: &'a [Backend],
    client: &ClientContext,
) -> Vec<&'a Backend> {
    if let Some(ingress) = &client.ingress_scope {
        let local: Vec<&Backend> = backends
            .iter()
            .filter(|b| ingress.contains(&b.scope))
            .collect();
        if !local.is_empty() {
            return local;
        }
    }
    backends.iter().collect()
}

/// Advance the shared round-robin cursor and return the selected backend.
fn pick_round_robin<'a>(candidates: &[&'a Backend]) -> Option<&'a Backend> {
    if candidates.is_empty() {
        return None;
    }
    let mut cursor = ROUND_ROBIN.lock();
    let index = *cursor % candidates.len();
    *cursor = cursor.wrapping_add(1);
    Some(candidates[index])
}

/// Return the backend minimizing `metric`. Ties keep the earliest candidate.
fn pick_min_by<'a>(
    candidates: &[&'a Backend],
    metric: impl Fn(&Backend) -> f64,
) -> Option<&'a Backend> {
    candidates
        .iter()
        .copied()
        .min_by(|a, b| {
            // f64 has no total order; treat NaN as "worst" so it never wins.
            metric(a)
                .partial_cmp(&metric(b))
                .unwrap_or(std::cmp::Ordering::Greater)
        })
}

/// Prefer a backend whose geography matches the client's; otherwise fall back
/// to the lowest-latency backend so a request is still served.
fn pick_geo<'a>(candidates: &[&'a Backend], client: &ClientContext) -> Option<&'a Backend> {
    if let Some(client_geo) = &client.geo {
        let matched: Vec<&Backend> = candidates
            .iter()
            .copied()
            .filter(|b| b.geo.as_deref() == Some(client_geo.as_str()))
            .collect();
        if !matched.is_empty() {
            return pick_min_by(&matched, |b| b.latency_ms);
        }
    }
    pick_min_by(candidates, |b| b.latency_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocf_core::prelude::*;

    fn backend(addr: &str, scope: Scope) -> Backend {
        Backend::new(Id::named(addr), addr, scope)
    }

    #[test]
    fn empty_backends_yields_none() {
        let chosen = select_backend(
            RoutingPolicy::RoundRobin,
            &[],
            &ClientContext::new(),
        );
        assert!(chosen.is_none());
    }

    #[test]
    fn least_load_picks_lowest() {
        let backends = vec![
            backend("a", Scope::fleet()).with_load(0.9),
            backend("b", Scope::fleet()).with_load(0.1),
            backend("c", Scope::fleet()).with_load(0.5),
        ];
        let chosen =
            select_backend(RoutingPolicy::LeastLoad, &backends, &ClientContext::new()).unwrap();
        assert_eq!(chosen.address, "b");
    }

    #[test]
    fn latency_picks_fastest() {
        let backends = vec![
            backend("a", Scope::fleet()).with_latency(40.0),
            backend("b", Scope::fleet()).with_latency(12.0),
        ];
        let chosen =
            select_backend(RoutingPolicy::Latency, &backends, &ClientContext::new()).unwrap();
        assert_eq!(chosen.address, "b");
    }

    #[test]
    fn geo_prefers_matching_region() {
        let backends = vec![
            backend("a", Scope::fleet()).with_geo("us-east").with_latency(5.0),
            backend("b", Scope::fleet()).with_geo("eu-west").with_latency(50.0),
        ];
        let client = ClientContext::new().with_geo("eu-west");
        let chosen = select_backend(RoutingPolicy::Geo, &backends, &client).unwrap();
        assert_eq!(chosen.address, "b");
    }

    #[test]
    fn geo_falls_back_to_latency_when_unmatched() {
        let backends = vec![
            backend("a", Scope::fleet()).with_geo("us-east").with_latency(50.0),
            backend("b", Scope::fleet()).with_geo("us-west").with_latency(5.0),
        ];
        let client = ClientContext::new().with_geo("ap-south");
        let chosen = select_backend(RoutingPolicy::Geo, &backends, &client).unwrap();
        assert_eq!(chosen.address, "b");
    }

    #[test]
    fn ingress_scope_keeps_traffic_local() {
        let us = backend("us", Scope::region("us")).with_latency(99.0);
        let eu = backend("eu", Scope::region("eu")).with_latency(1.0);
        let backends = vec![us, eu];
        // Even though `eu` is faster, an ingress in `us` should stay local.
        let client = ClientContext::new().with_ingress_scope(Scope::region("us"));
        let chosen = select_backend(RoutingPolicy::Latency, &backends, &client).unwrap();
        assert_eq!(chosen.address, "us");
    }

    #[test]
    fn ingress_scope_falls_back_when_none_local() {
        let eu = backend("eu", Scope::region("eu")).with_load(0.2);
        let backends = vec![eu];
        let client = ClientContext::new().with_ingress_scope(Scope::region("us"));
        // No backend in `us`; selection still returns the out-of-scope one.
        let chosen = select_backend(RoutingPolicy::LeastLoad, &backends, &client).unwrap();
        assert_eq!(chosen.address, "eu");
    }

    #[test]
    fn round_robin_rotates() {
        let backends = vec![
            backend("a", Scope::fleet()),
            backend("b", Scope::fleet()),
        ];
        let first =
            select_backend(RoutingPolicy::RoundRobin, &backends, &ClientContext::new()).unwrap();
        let second =
            select_backend(RoutingPolicy::RoundRobin, &backends, &ClientContext::new()).unwrap();
        assert_ne!(first.address, second.address);
    }
}
