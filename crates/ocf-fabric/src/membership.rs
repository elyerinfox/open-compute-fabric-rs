//! SWIM-style membership and failure detection.
//!
//! Every node keeps a view of the fleet here. A peer is [`Liveness::Alive`]
//! while its heartbeats keep arriving; if they stop for `suspect_timeout` it
//! becomes [`Liveness::Suspect`] (a soft failure — it might just be a slow
//! link), and if it stays silent for a further `dead_timeout` it is declared
//! [`Liveness::Dead`]. A graceful departure is [`Liveness::Left`].
//!
//! [`Membership::tick`] is the deterministic heart of the detector: given "now",
//! it advances every member's state and returns the transitions as
//! [`MembershipEvent`]s, which the controller turns into action (evict from
//! load-balancer pools, reschedule HA workloads, ...). Keeping `tick` a pure
//! function of the current time makes the whole failure detector unit-testable
//! without sleeping.
//!
//! What's modeled: the per-member state machine, heartbeat refresh, and
//! suspicion/death timeouts. A fuller SWIM also runs indirect probes (ask k
//! peers to ping a suspect before declaring it dead) and incarnation-number
//! refutation to suppress false positives; the `incarnation` field and the seams
//! for it are here. Heartbeats themselves ride the real [`crate::transport`].

use crate::crypto::NodeId;
use crate::node::FabricNode;
use chrono::{DateTime, Duration, Utc};
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;

/// A member's liveness in the local view of the fleet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    Alive,
    Suspect,
    Dead,
    Left,
}

impl Liveness {
    /// Whether a node in this state should receive traffic / be schedulable.
    pub fn is_available(&self) -> bool {
        matches!(self, Liveness::Alive)
    }

    /// Terminal states the failure detector no longer transitions.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Liveness::Dead | Liveness::Left)
    }
}

/// The local view of one fleet member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberState {
    pub node: FabricNode,
    pub liveness: Liveness,
    pub last_heartbeat: DateTime<Utc>,
    /// SWIM incarnation number — bumped by a node to refute a suspicion about
    /// itself. Carried here for the refutation path.
    pub incarnation: u64,
    /// Last measured round-trip latency to this peer, in milliseconds. `None`
    /// until a successful probe (or for an unreachable peer). Feeds latency-aware
    /// routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<f64>,
}

/// A transition the failure detector observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event", content = "node")]
pub enum MembershipEvent {
    Joined(NodeId),
    Recovered(NodeId),
    Suspected(NodeId),
    Died(NodeId),
    Left(NodeId),
}

/// A node's membership table and failure detector.
pub struct Membership {
    local: NodeId,
    members: RwLock<HashMap<NodeId, MemberState>>,
    suspect_timeout: Duration,
    dead_timeout: Duration,
}

impl Membership {
    /// Build a detector for `local` with default timeouts (suspect after 5s of
    /// silence, dead 5s after that).
    pub fn new(local: NodeId) -> Self {
        Self::with_timeouts(local, Duration::seconds(5), Duration::seconds(5))
    }

    /// Build a detector with explicit timeouts.
    pub fn with_timeouts(local: NodeId, suspect_timeout: Duration, dead_timeout: Duration) -> Self {
        Membership {
            local,
            members: RwLock::new(HashMap::new()),
            suspect_timeout,
            dead_timeout,
        }
    }

    pub fn local(&self) -> &NodeId {
        &self.local
    }

    /// Add (or refresh) a peer as alive. Returns `Joined` for a new member or
    /// `Recovered` for one that had been suspected/declared dead.
    pub fn join(&self, node: FabricNode) -> MembershipEvent {
        let id = node.node_id.clone();
        let now = Utc::now();
        let mut members = self.members.write();
        match members.get_mut(&id) {
            Some(existing) => {
                let was_down = existing.liveness != Liveness::Alive;
                existing.node = node;
                existing.liveness = Liveness::Alive;
                existing.last_heartbeat = now;
                existing.incarnation += 1;
                if was_down {
                    MembershipEvent::Recovered(id)
                } else {
                    MembershipEvent::Joined(id)
                }
            }
            None => {
                members.insert(
                    id.clone(),
                    MemberState {
                        node,
                        liveness: Liveness::Alive,
                        last_heartbeat: now,
                        incarnation: 0,
                        rtt_ms: None,
                    },
                );
                MembershipEvent::Joined(id)
            }
        }
    }

    /// Record a heartbeat from `id` at the current time, reviving a suspected
    /// member. Returns `Some(Recovered)` if the heartbeat brought a non-alive
    /// member back, `None` otherwise (including unknown members).
    pub fn heartbeat(&self, id: &NodeId) -> Option<MembershipEvent> {
        self.heartbeat_at(id, Utc::now())
    }

    /// Heartbeat with an explicit timestamp (for tests).
    pub fn heartbeat_at(&self, id: &NodeId, now: DateTime<Utc>) -> Option<MembershipEvent> {
        let mut members = self.members.write();
        let member = members.get_mut(id)?;
        if member.liveness == Liveness::Left {
            return None;
        }
        let recovered = member.liveness != Liveness::Alive;
        member.last_heartbeat = now;
        member.liveness = Liveness::Alive;
        recovered.then(|| MembershipEvent::Recovered(id.clone()))
    }

    /// Immediately declare `id` dead (e.g. an operator-forced eviction or a
    /// hard failure signal), bypassing the suspicion timeout. Returns `Died`
    /// if this changed a live/suspect member, `None` otherwise.
    pub fn force_dead(&self, id: &NodeId) -> Option<MembershipEvent> {
        let mut members = self.members.write();
        let member = members.get_mut(id)?;
        if member.liveness.is_terminal() {
            return None;
        }
        member.liveness = Liveness::Dead;
        Some(MembershipEvent::Died(id.clone()))
    }

    /// Find the member backing fleet machine `machine_id`, if any.
    pub fn member_for_machine(&self, machine_id: &Id) -> Option<MemberState> {
        self.members
            .read()
            .values()
            .find(|m| m.node.machine_id.as_ref() == Some(machine_id))
            .cloned()
    }

    /// Record a measured round-trip latency (ms) to `id`. No-op for unknown
    /// members.
    pub fn record_rtt(&self, id: &NodeId, rtt_ms: f64) {
        if let Some(m) = self.members.write().get_mut(id) {
            m.rtt_ms = Some(rtt_ms);
        }
    }

    /// The last measured RTT (ms) to `id`, if any.
    pub fn rtt(&self, id: &NodeId) -> Option<f64> {
        self.members.read().get(id).and_then(|m| m.rtt_ms)
    }

    /// Gracefully mark `id` as having left the fleet.
    pub fn leave(&self, id: &NodeId) -> Result<MembershipEvent> {
        let mut members = self.members.write();
        let member = members
            .get_mut(id)
            .ok_or_else(|| Error::not_found(format!("member {id}")))?;
        member.liveness = Liveness::Left;
        Ok(MembershipEvent::Left(id.clone()))
    }

    /// Advance the failure detector to `now`, returning every state transition.
    ///
    /// A member silent past `suspect_timeout` becomes `Suspect`; one silent past
    /// `suspect_timeout + dead_timeout` becomes `Dead`. Terminal members are
    /// skipped. Pure in `now`, so callers drive it from a timer.
    pub fn tick(&self, now: DateTime<Utc>) -> Vec<MembershipEvent> {
        let mut events = Vec::new();
        let mut members = self.members.write();
        for (id, member) in members.iter_mut() {
            if member.liveness.is_terminal() {
                continue;
            }
            let silent = now - member.last_heartbeat;
            if silent >= self.suspect_timeout + self.dead_timeout {
                if member.liveness != Liveness::Dead {
                    member.liveness = Liveness::Dead;
                    events.push(MembershipEvent::Died(id.clone()));
                }
            } else if silent >= self.suspect_timeout && member.liveness == Liveness::Alive {
                member.liveness = Liveness::Suspect;
                events.push(MembershipEvent::Suspected(id.clone()));
            }
        }
        events
    }

    /// Remove members that are `Dead` or `Left` from the table, returning their ids.
    pub fn reap(&self) -> Vec<NodeId> {
        let mut reaped = Vec::new();
        self.members.write().retain(|id, m| {
            if m.liveness.is_terminal() {
                reaped.push(id.clone());
                false
            } else {
                true
            }
        });
        reaped
    }

    /// The current liveness of `id`, if known.
    pub fn liveness(&self, id: &NodeId) -> Option<Liveness> {
        self.members.read().get(id).map(|m| m.liveness)
    }

    /// Snapshot of every member.
    pub fn members(&self) -> Vec<MemberState> {
        self.members.read().values().cloned().collect()
    }

    /// Just the alive members' node records — the schedulable / routable set.
    pub fn alive(&self) -> Vec<FabricNode> {
        self.members
            .read()
            .values()
            .filter(|m| m.liveness == Liveness::Alive)
            .map(|m| m.node.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::KeyPair;

    fn node(name: &str) -> FabricNode {
        FabricNode::from_keypair(&KeyPair::from_seed_name(name), vec![format!("{name}:1")])
    }

    fn membership() -> Membership {
        Membership::with_timeouts(
            NodeId::new("self"),
            Duration::seconds(5),
            Duration::seconds(5),
        )
    }

    #[test]
    fn alive_then_suspect_then_dead() {
        let m = membership();
        let peer = node("peer");
        let id = peer.node_id.clone();
        assert_eq!(m.join(peer), MembershipEvent::Joined(id.clone()));
        assert_eq!(m.liveness(&id), Some(Liveness::Alive));

        let t0 = Utc::now();
        // Before the suspect timeout: nothing happens.
        assert!(m.tick(t0 + Duration::seconds(3)).is_empty());

        // Past suspect timeout: suspected.
        let ev = m.tick(t0 + Duration::seconds(6));
        assert_eq!(ev, vec![MembershipEvent::Suspected(id.clone())]);

        // Past suspect+dead: declared dead.
        let ev = m.tick(t0 + Duration::seconds(11));
        assert_eq!(ev, vec![MembershipEvent::Died(id.clone())]);
        assert_eq!(m.liveness(&id), Some(Liveness::Dead));

        // Dead is terminal: no further events, and reap removes it.
        assert!(m.tick(t0 + Duration::seconds(30)).is_empty());
        assert_eq!(m.reap(), vec![id.clone()]);
        assert_eq!(m.liveness(&id), None);
    }

    #[test]
    fn heartbeat_revives_a_suspect() {
        let m = membership();
        let peer = node("peer");
        let id = peer.node_id.clone();
        m.join(peer);

        let t0 = Utc::now();
        m.tick(t0 + Duration::seconds(6)); // -> suspect
        assert_eq!(m.liveness(&id), Some(Liveness::Suspect));

        let ev = m.heartbeat_at(&id, t0 + Duration::seconds(7));
        assert_eq!(ev, Some(MembershipEvent::Recovered(id.clone())));
        assert_eq!(m.liveness(&id), Some(Liveness::Alive));
    }

    #[test]
    fn records_and_reads_rtt() {
        let m = membership();
        let peer = node("peer");
        let id = peer.node_id.clone();
        m.join(peer);
        assert_eq!(m.rtt(&id), None);
        m.record_rtt(&id, 1.42);
        assert_eq!(m.rtt(&id), Some(1.42));
        // Unknown member is a no-op.
        m.record_rtt(&NodeId::new("ghost"), 9.0);
        assert_eq!(m.rtt(&NodeId::new("ghost")), None);
    }

    #[test]
    fn graceful_leave_is_terminal() {
        let m = membership();
        let peer = node("peer");
        let id = peer.node_id.clone();
        m.join(peer);
        assert_eq!(m.leave(&id).unwrap(), MembershipEvent::Left(id.clone()));
        assert_eq!(m.liveness(&id), Some(Liveness::Left));
        assert!(m.tick(Utc::now() + Duration::seconds(100)).is_empty());
    }
}
