//! Capabilities: what the fabric needs, decoupled from what it's *called* on
//! each operating system.
//!
//! A [`Capability`] names a thing the data plane needs (e.g. "nftables"), the
//! **binary** that proves it's present (`nft`), an optional kernel **module**,
//! and the **package name per package manager** — which can differ across
//! distros (`iproute2` on apt/pacman but `iproute` on dnf). This map is what
//! lets a single "Install nftables" fix run the right command on each host.

use crate::os::binary_available;
use ocf_core::prelude::*;
use std::collections::BTreeMap;

/// A host capability the fabric depends on, with its per-package-manager names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    /// Stable capability name, e.g. `"nftables"`.
    pub name: String,
    /// The executable whose presence on `PATH` proves the capability.
    pub binary: String,
    /// An optional kernel module the capability also needs loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// Package name keyed by package-manager name (`"apt"`, `"dnf"`, …).
    pub packages: BTreeMap<String, String>,
}

impl Capability {
    pub fn new(name: impl Into<String>, binary: impl Into<String>) -> Self {
        Capability {
            name: name.into(),
            binary: binary.into(),
            module: None,
            packages: BTreeMap::new(),
        }
    }

    pub fn with_module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    /// Map this capability to `package` under package manager `pm`.
    pub fn pkg(mut self, pm: impl Into<String>, package: impl Into<String>) -> Self {
        self.packages.insert(pm.into(), package.into());
        self
    }

    /// The package name for package manager `pm`, if mapped.
    pub fn package_for(&self, pm: &str) -> Option<&str> {
        self.packages.get(pm).map(String::as_str)
    }

    /// Whether the capability's binary is present on this host's `PATH`.
    pub fn is_present(&self) -> bool {
        binary_available(&self.binary)
    }
}

/// The capabilities the fabric's data plane relies on, with their package names
/// across the common Linux package managers. Note the deliberate name
/// differences (`iproute2` vs `iproute`, `openvswitch-switch` vs `openvswitch`)
/// — the whole point of the map.
pub fn builtin_capabilities() -> Vec<Capability> {
    vec![
        Capability::new("nftables", "nft")
            .with_module("nf_tables")
            .pkg("apt", "nftables")
            .pkg("dnf", "nftables")
            .pkg("pacman", "nftables")
            .pkg("apk", "nftables"),
        Capability::new("iproute2", "ip")
            .pkg("apt", "iproute2")
            .pkg("dnf", "iproute")
            .pkg("pacman", "iproute2")
            .pkg("apk", "iproute2"),
        Capability::new("smartmontools", "smartctl")
            .pkg("apt", "smartmontools")
            .pkg("dnf", "smartmontools")
            .pkg("pacman", "smartmontools")
            .pkg("apk", "smartmontools"),
        Capability::new("ipmitool", "ipmitool")
            .pkg("apt", "ipmitool")
            .pkg("dnf", "ipmitool")
            .pkg("pacman", "ipmitool")
            .pkg("apk", "ipmitool"),
        Capability::new("openvswitch", "ovs-vsctl")
            .pkg("apt", "openvswitch-switch")
            .pkg("dnf", "openvswitch")
            .pkg("pacman", "openvswitch")
            .pkg("apk", "openvswitch"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_differ_per_manager() {
        let caps = builtin_capabilities();
        let iproute = caps.iter().find(|c| c.name == "iproute2").unwrap();
        // Same capability, different package name on dnf.
        assert_eq!(iproute.package_for("apt"), Some("iproute2"));
        assert_eq!(iproute.package_for("dnf"), Some("iproute"));
        assert_eq!(iproute.package_for("pacman"), Some("iproute2"));

        let ovs = caps.iter().find(|c| c.name == "openvswitch").unwrap();
        assert_eq!(ovs.package_for("apt"), Some("openvswitch-switch"));
        assert_eq!(ovs.package_for("dnf"), Some("openvswitch"));
    }

    #[test]
    fn nftables_carries_module() {
        let caps = builtin_capabilities();
        let nft = caps.iter().find(|c| c.name == "nftables").unwrap();
        assert_eq!(nft.module.as_deref(), Some("nf_tables"));
        assert_eq!(nft.binary, "nft");
    }
}
