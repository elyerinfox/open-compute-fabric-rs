//! # ocf-platform
//!
//! Host **operating-system detection** and **pluggable package managers**, so the
//! fabric can resolve and install missing capabilities across operating systems.
//!
//! The data plane shells out to host tools (`nft`, `ip`, `smartctl`, …). Whether
//! those tools are present — and how to install them — differs by OS and distro.
//! This crate provides the missing abstraction:
//!
//! * [`HostOs`] — what OS/distro this is ([`os`], from `/etc/os-release`).
//! * [`Capability`] — what the fabric needs (a binary, an optional kernel
//!   module) and its **package name per package manager** ([`capability`]). The
//!   package name can differ across distros (`iproute2` vs `iproute`), which is
//!   the whole point of the map.
//! * [`PackageManager`] — the pluggable contract ([`package`]) with built-in
//!   [`AptPackageManager`], [`DnfPackageManager`], [`PacmanPackageManager`], and
//!   [`ApkPackageManager`].
//! * [`PlatformService`] — detects the OS, selects the applicable package
//!   manager, and turns "install capability X" into the right command for this
//!   host ([`service`]).
//!
//! A host with no supported package manager (Windows, macOS) simply has no active
//! manager — capabilities are reported as unsatisfiable rather than guessed at.
//! This is what [`ocf-health`](../ocf_health/index.html)'s `PackageCheck` uses to
//! offer an OS-aware *"Install X"* fix button for a missing tool.

pub mod capability;
pub mod managers;
pub mod os;
pub mod osv;
pub mod package;
pub mod service;
pub mod update;

pub use capability::{builtin_capabilities, Capability};
pub use managers::{
    ApkPackageManager, AptPackageManager, DnfPackageManager, PacmanPackageManager,
};
pub use os::{binary_available, HostOs};
pub use osv::{OsvClient, VulnerablePackage};
pub use package::{register_builtins, PackageManager};
pub use service::{CapabilityStatus, PlatformService, PlatformStatus, UpdateSummary};
pub use update::{InstalledPackage, PackageUpdate};

#[cfg(test)]
mod tests {
    use super::*;
    use ocf_core::prelude::*;

    #[test]
    fn register_builtins_registers_four_managers() {
        let mut reg: Registry<dyn PackageManager> = Registry::new();
        register_builtins(&mut reg).expect("register builtins");
        assert_eq!(reg.len(), 4);
        for name in ["apt", "dnf", "pacman", "apk"] {
            assert!(reg.contains(name), "missing manager {name}");
        }
    }
}
