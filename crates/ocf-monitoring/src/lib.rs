//! # ocf-monitoring
//!
//! Host and per-runtime resource metrics for the fabric.
//!
//! Metrics flow through three pieces:
//!
//! * The value types ([`sample`]) — point [`MetricSample`]s for time-series
//!   export and the rolled-up [`ResourceUsage`] snapshot reported per host or
//!   per workload.
//! * A pluggable collection contract ([`collector::MetricsCollector`]) with
//!   built-in [`HostMetricsCollector`] and [`RuntimeMetricsCollector`] backends
//!   that read *real* counters: the host backend parses the Linux `/proc`
//!   filesystem (and `df`); the runtime backend shells out to `docker stats`.
//!   The pure parsing logic lives in [`procfs`] and is unit-tested there.
//! * A [`MonitoringService`] that aggregates every registered collector and
//!   projects a snapshot into the `BTreeMap<String, f64>` the `ocf-runtime`
//!   autoscaler consumes (so that crate need not depend on this one).

pub mod collector;
pub mod procfs;
pub mod sample;
pub mod service;

pub use collector::{
    register_builtins, HostMetricsCollector, MetricsCollector, RuntimeMetricsCollector,
};
pub use sample::{MetricSample, ResourceUsage};
pub use service::{usage_to_metric_map, MonitoringService};
