//! # ocf-network
//!
//! The fabric's internal software-defined network (SDN) overlay.
//!
//! Tenants get isolated [`Vpc`]s (VXLAN-separated address domains), carved into
//! [`Subnet`]s that are realized on hosts inside network namespaces, wired
//! together with [`Route`]s and protected by [`FirewallPolicy`]s of ordered
//! [`AclRule`]s. The resource model lives in [`model`].
//!
//! Pluggability follows the fabric convention: the dataplane is programmed
//! through the [`NetworkBackend`] contract ([`backend`]), with in-tree
//! [`LinuxNetnsBackend`] (iproute2 + nftables) and [`OvsBackend`] (Open
//! vSwitch) implementations. The high-level [`NetworkController`]
//! ([`controller`]) owns the authoritative in-memory state and, because the
//! overlay is fleet-wide, fans every mutation out across every registered
//! backend so a change "affects all machines".
//!
//! Each backend shells out to the host's SDN tooling (`ip netns`, VXLAN links,
//! `nft`, `ovs-vsctl`/`ovs-ofctl`), so it requires a Linux host with those
//! binaries and, in practice, root. Every command is issued idempotently so
//! re-applying a resource converges instead of failing.

pub mod backend;
pub mod controller;
pub mod ipam;
pub mod model;

pub use backend::{register_builtins, LinuxNetnsBackend, NetworkBackend, OvsBackend};
pub use controller::NetworkController;
pub use ipam::SubnetAllocator;
pub use model::{
    AclAction, AclDirection, AclRule, AclScope, EgressMode, FirewallPolicy, Route, Subnet, Vpc,
};

#[cfg(test)]
mod tests {
    use super::*;
    use ocf_core::prelude::*;
    use std::sync::Arc;

    // The CRUD/integrity tests exercise the controller's in-memory state and
    // fan-out plumbing, not the dataplane. The real backends shell out to
    // iproute2/OVS, which need root and a Linux host and would fail on a dev
    // box, so these tests register the no-op `NullBackend` instead.
    fn controller() -> NetworkController {
        let mut reg: Registry<dyn NetworkBackend> = Registry::new();
        backend::register_null(&mut reg).expect("register null backend");
        NetworkController::new(Arc::new(reg))
    }

    #[test]
    fn builtins_register_both_backends() {
        let mut reg: Registry<dyn NetworkBackend> = Registry::new();
        register_builtins(&mut reg).expect("register builtins");
        assert!(reg.contains("linux-netns"));
        assert!(reg.contains("ovs"));
        assert_eq!(reg.len(), 2);
    }

    #[tokio::test]
    async fn vpc_subnet_route_lifecycle() {
        let ctl = controller();
        let vpc = ctl
            .create_vpc(Vpc::new("tenant-a", "10.0.0.0/16", 100))
            .await
            .expect("create vpc");

        // A subnet under a missing VPC is rejected.
        let orphan = Subnet::new(Id::new(), "orphan", "10.9.0.0/24", "ns-orphan");
        assert!(ctl.create_subnet(orphan).await.is_err());

        let subnet = ctl
            .create_subnet(Subnet::new(
                vpc.metadata.id.clone(),
                "web",
                "10.0.1.0/24",
                "ns-web",
            ))
            .await
            .expect("create subnet");

        let route = ctl
            .add_route(Route::new(
                subnet.metadata.id.clone(),
                "0.0.0.0/0",
                "10.0.1.1",
            ))
            .await
            .expect("add route");
        assert_eq!(route.subnet_id, subnet.metadata.id);

        let routes = ctl
            .list_routes(&subnet.metadata.id)
            .await
            .expect("list routes");
        assert_eq!(routes.len(), 1);

        // VPC cannot be deleted while a subnet references it.
        assert!(ctl.delete_vpc(&vpc.metadata.id).await.is_err());
    }

    #[tokio::test]
    async fn subnet_egress_capability_and_refresh() {
        let ctl = controller();
        let vpc = ctl
            .create_vpc(Vpc::new("tenant-egr", "10.2.0.0/16", 102))
            .await
            .expect("create vpc");

        // A Nat-capable subnet is created with egress programmed (NullBackend
        // no-ops, so creation succeeds and records the capability).
        let subnet = ctl
            .create_subnet(
                Subnet::new(vpc.metadata.id.clone(), "public", "10.2.1.0/24", "ns-pub")
                    .with_egress(EgressMode::Nat),
            )
            .await
            .expect("create nat subnet");
        assert_eq!(subnet.egress, EgressMode::Nat);

        // Re-programming with an opted-in workload address fans out cleanly.
        ctl.refresh_subnet_egress(&subnet.metadata.id, &["10.2.1.5".to_string()])
            .await
            .expect("refresh egress");

        // Toggling the capability updates the stored subnet.
        let updated = ctl
            .set_subnet_egress(&subnet.metadata.id, EgressMode::Isolated, &[])
            .await
            .expect("disable egress");
        assert_eq!(updated.egress, EgressMode::Isolated);
        let reread = ctl.get_subnet(&subnet.metadata.id).await.unwrap();
        assert_eq!(reread.egress, EgressMode::Isolated);

        // Refreshing egress for an unknown subnet errors.
        assert!(ctl
            .refresh_subnet_egress(&Id::new(), &[])
            .await
            .is_err());
    }

    #[tokio::test]
    async fn policy_scope_must_exist() {
        let ctl = controller();
        let policy = FirewallPolicy::new(AclScope::Vpc(Id::new())).with_rule(AclRule::new(
            AclAction::Deny,
            AclDirection::Ingress,
            "tcp",
            "0.0.0.0/0",
            Some(22),
        ));
        assert!(ctl.apply_policy(policy).await.is_err());

        let vpc = ctl
            .create_vpc(Vpc::new("tenant-b", "10.1.0.0/16", 101))
            .await
            .expect("create vpc");
        let policy =
            FirewallPolicy::new(AclScope::Vpc(vpc.metadata.id.clone())).with_rule(AclRule::new(
                AclAction::Allow,
                AclDirection::Ingress,
                "tcp",
                "10.1.0.0/16",
                Some(443),
            ));
        let stored = ctl.apply_policy(policy).await.expect("apply policy");
        assert_eq!(stored.rules.len(), 1);
        assert_eq!(ctl.list_policies().await.expect("list").len(), 1);
    }
}
