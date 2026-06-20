//! Hierarchical placement / authorization scope.
//!
//! A [`Scope`] names a position in the fleet topology tree
//! (`fleet → region → datacenter → rack → machine`). It is reused in two
//! places:
//!
//! * **Authorization** — a role binding is granted at a scope and applies to
//!   that node and everything beneath it.
//! * **Placement** — a load balancer or a highly-available workload may be
//!   restricted to a scope, which also bounds where it is allowed to migrate.

use crate::id::Id;
use serde::{Deserialize, Serialize};

/// A path from the fleet root down to (at most) a single machine.
///
/// A field being `None` means "any / unscoped at this level". `Scope::default()`
/// (all `None`) is the whole fleet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub region: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub datacenter: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rack: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub machine: Option<Id>,
}

/// The granularity of a [`Scope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeLevel {
    Fleet = 0,
    Region = 1,
    Datacenter = 2,
    Rack = 3,
    Machine = 4,
}

impl Scope {
    /// The entire fleet.
    pub fn fleet() -> Self {
        Scope::default()
    }

    pub fn region(region: impl Into<Id>) -> Self {
        Scope {
            region: Some(region.into()),
            ..Default::default()
        }
    }

    pub fn datacenter(region: impl Into<Id>, datacenter: impl Into<Id>) -> Self {
        Scope {
            region: Some(region.into()),
            datacenter: Some(datacenter.into()),
            ..Default::default()
        }
    }

    pub fn rack(region: impl Into<Id>, datacenter: impl Into<Id>, rack: impl Into<Id>) -> Self {
        Scope {
            region: Some(region.into()),
            datacenter: Some(datacenter.into()),
            rack: Some(rack.into()),
            ..Default::default()
        }
    }

    pub fn machine(
        region: impl Into<Id>,
        datacenter: impl Into<Id>,
        rack: impl Into<Id>,
        machine: impl Into<Id>,
    ) -> Self {
        Scope {
            region: Some(region.into()),
            datacenter: Some(datacenter.into()),
            rack: Some(rack.into()),
            machine: Some(machine.into()),
        }
    }

    /// The most specific level set on this scope.
    pub fn level(&self) -> ScopeLevel {
        if self.machine.is_some() {
            ScopeLevel::Machine
        } else if self.rack.is_some() {
            ScopeLevel::Rack
        } else if self.datacenter.is_some() {
            ScopeLevel::Datacenter
        } else if self.region.is_some() {
            ScopeLevel::Region
        } else {
            ScopeLevel::Fleet
        }
    }

    /// True if `self` is an ancestor of (or equal to) `other` — i.e. every level
    /// constrained by `self` matches `other`. A grant at `self` therefore covers
    /// `other`.
    pub fn contains(&self, other: &Scope) -> bool {
        fn level_ok(parent: &Option<Id>, child: &Option<Id>) -> bool {
            match parent {
                None => true,
                Some(p) => child.as_ref() == Some(p),
            }
        }
        level_ok(&self.region, &other.region)
            && level_ok(&self.datacenter, &other.datacenter)
            && level_ok(&self.rack, &other.rack)
            && level_ok(&self.machine, &other.machine)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_contains_everything() {
        let dc = Scope::datacenter("us", "dc1");
        assert!(Scope::fleet().contains(&dc));
        assert_eq!(Scope::fleet().level(), ScopeLevel::Fleet);
    }

    #[test]
    fn region_does_not_contain_other_region() {
        let a = Scope::region("us");
        let b = Scope::region("eu");
        assert!(!a.contains(&b));
        assert!(a.contains(&Scope::datacenter("us", "dc1")));
    }
}
