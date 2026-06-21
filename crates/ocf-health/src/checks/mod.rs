//! The built-in health checks. Each module is one self-contained warning with
//! its own detection probe and fix action.

pub mod bridge_netfilter;
pub mod config;
pub mod docker;
pub mod ip_forwarding;
pub mod netfilter;
pub mod package;

pub use bridge_netfilter::BridgeNetfilterCheck;
pub use config::{ServiceCheck, SwapCheck, SysctlCheck, TimeSyncCheck};
pub use docker::DockerExperimentalCheck;
pub use ip_forwarding::IpForwardingCheck;
pub use netfilter::NetfilterCheck;
pub use package::PackageCheck;
