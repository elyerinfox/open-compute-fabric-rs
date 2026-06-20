//! Concrete [`RuntimeProvider`](crate::provider::RuntimeProvider) backends.
//!
//! Every backend here drives a **real host binary** and reports state by
//! querying that binary, not an in-memory mirror:
//!
//! * [`DockerRuntime`] / [`PodmanRuntime`] shell out to `docker` / `podman`.
//! * [`LxcRuntime`] drives the `lxc-*` family (`lxc-create`, `lxc-start`, ...).
//! * [`QemuRuntime`] drives libvirt's `virsh` (`define`, `start`, `dump`, ...)
//!   and is migration-capable.
//!
//! All command execution and output parsing is funneled through
//! [`command::run`] so tool failures map onto a single uniform error. A missing
//! binary is therefore a *runtime* error (correct: the crate still compiles on a
//! host without these tools, including Windows).

mod docker;
mod lxc;
mod podman;
mod qemu;

pub mod command;

pub use docker::DockerRuntime;
pub use lxc::LxcRuntime;
pub use podman::PodmanRuntime;
pub use qemu::QemuRuntime;
