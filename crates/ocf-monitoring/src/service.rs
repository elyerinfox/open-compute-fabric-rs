//! The high-level aggregation service over a registry of collectors.

use crate::collector::MetricsCollector;
use crate::sample::{MetricSample, ResourceUsage};
use ocf_core::prelude::*;
use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Aggregates every registered [`MetricsCollector`] behind one façade.
///
/// The controller wires a `Registry<dyn MetricsCollector>` and hands it here.
/// `MonitoringService` then offers fleet-level rollups (sum host usage across
/// collectors), per-workload lookups (first collector that can see the
/// workload wins), a flattened time-series export, and the
/// `BTreeMap<String, f64>` projection the autoscaler consumes.
pub struct MonitoringService {
    collectors: Arc<RwLock<Registry<dyn MetricsCollector>>>,
}

impl MonitoringService {
    /// Wrap an existing registry of collectors.
    pub fn new(registry: Registry<dyn MetricsCollector>) -> Self {
        MonitoringService {
            collectors: Arc::new(RwLock::new(registry)),
        }
    }

    /// Build a service with the built-in collectors already registered.
    pub fn with_builtins() -> Result<Self> {
        let mut reg = Registry::new();
        crate::collector::register_builtins(&mut reg)?;
        Ok(Self::new(reg))
    }

    /// Shared handle to the underlying registry, for callers that register
    /// additional collectors at runtime.
    pub fn registry(&self) -> Arc<RwLock<Registry<dyn MetricsCollector>>> {
        Arc::clone(&self.collectors)
    }

    /// Names of every registered collector.
    pub fn collector_names(&self) -> Vec<String> {
        self.collectors.read().names()
    }

    /// Snapshot handles to every registered collector.
    fn snapshot(&self) -> Vec<Arc<dyn MetricsCollector>> {
        self.collectors.read().all()
    }

    /// Host usage from a single named collector.
    pub async fn host_usage(&self, collector: &str) -> Result<ResourceUsage> {
        let provider = self.collectors.read().get(collector)?;
        provider.collect_host().await
    }

    /// Fleet-level host usage: the element-wise sum of every collector's host
    /// snapshot (CPU averaged, everything else summed). With no collectors this
    /// is an empty snapshot.
    pub async fn aggregate_host_usage(&self) -> Result<ResourceUsage> {
        let providers = self.snapshot();
        let mut total = ResourceUsage::default();
        let mut cpu_sum = 0.0_f64;
        let mut counted = 0u64;
        for p in &providers {
            match p.collect_host().await {
                Ok(u) => {
                    cpu_sum += u.cpu_pct;
                    counted += 1;
                    total.memory_used = total.memory_used.saturating_add(u.memory_used);
                    total.memory_total = total.memory_total.saturating_add(u.memory_total);
                    total.disk_used = total.disk_used.saturating_add(u.disk_used);
                    total.disk_total = total.disk_total.saturating_add(u.disk_total);
                    total.net_rx_bps = total.net_rx_bps.saturating_add(u.net_rx_bps);
                    total.net_tx_bps = total.net_tx_bps.saturating_add(u.net_tx_bps);
                    total.read_iops = total.read_iops.saturating_add(u.read_iops);
                    total.write_iops = total.write_iops.saturating_add(u.write_iops);
                }
                Err(e) => {
                    tracing::warn!(collector = %p.name(), error = %e, "host collection failed");
                }
            }
        }
        if counted > 0 {
            total.cpu_pct = cpu_sum / counted as f64;
        }
        Ok(total)
    }

    /// Per-workload usage. Tries each collector in turn and returns the first
    /// snapshot found; if no collector can see the workload, returns
    /// [`Error::NotFound`].
    pub async fn workload_usage(&self, id: &Id) -> Result<ResourceUsage> {
        let providers = self.snapshot();
        for p in &providers {
            match p.collect_workload(id).await {
                Ok(u) => return Ok(u),
                Err(e) => {
                    tracing::trace!(
                        collector = %p.name(),
                        workload = %id,
                        error = %e,
                        "collector cannot see workload, trying next"
                    );
                }
            }
        }
        Err(Error::not_found(format!("metrics for workload {id}")))
    }

    /// The metric map for a workload, ready to hand to the autoscaler's
    /// `evaluate`. Equivalent to `self.workload_usage(id).await?.to_metric_map()`.
    pub async fn workload_metric_map(&self, id: &Id) -> Result<BTreeMap<String, f64>> {
        Ok(self.workload_usage(id).await?.to_metric_map())
    }

    /// Every collector's samples, flattened for time-series export. A failing
    /// collector is logged and skipped rather than aborting the whole export.
    pub async fn all_samples(&self) -> Result<Vec<MetricSample>> {
        let providers = self.snapshot();
        let mut out = Vec::new();
        for p in &providers {
            match p.samples().await {
                Ok(mut s) => out.append(&mut s),
                Err(e) => {
                    tracing::warn!(collector = %p.name(), error = %e, "sample export failed");
                }
            }
        }
        Ok(out)
    }
}

/// Convert a [`ResourceUsage`] into the `BTreeMap<String, f64>` the autoscaler
/// evaluates against. Free function form of [`ResourceUsage::to_metric_map`],
/// provided so wiring code can call it without importing the inherent method.
pub fn usage_to_metric_map(usage: &ResourceUsage) -> BTreeMap<String, f64> {
    usage.to_metric_map()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn aggregates_and_exports() {
        let svc = MonitoringService::with_builtins().expect("service");
        let mut names = svc.collector_names();
        names.sort();
        assert_eq!(names, vec!["host".to_string(), "runtime".to_string()]);

        // `aggregate_host_usage` and `all_samples` log-and-skip a failing
        // collector, so they always succeed structurally. On a real Linux host
        // the host collector contributes a non-empty snapshot; in a sandbox
        // without `/proc` it is skipped and the runtime collector contributes
        // an empty one. Either way the call returns Ok.
        let _agg = svc.aggregate_host_usage().await.expect("aggregate");
        let _samples = svc.all_samples().await.expect("samples");
    }

    #[tokio::test]
    async fn workload_usage_consults_collectors_then_errors() {
        // With no live container of this id (and possibly no docker), the
        // runtime collector cannot see it, so lookup returns NotFound rather
        // than a fabricated snapshot. On a host where the container exists the
        // map would instead carry real autoscaler keys.
        let svc = MonitoringService::with_builtins().expect("service");
        let id = Id::named("definitely-not-a-real-workload");
        match svc.workload_usage(&id).await {
            Ok(usage) => {
                assert!(usage.memory_used <= usage.memory_total);
                let map = svc.workload_metric_map(&id).await.expect("map");
                assert!(map.contains_key("cpu"));
                assert!(map.contains_key("memory_pct"));
                assert_eq!(usage_to_metric_map(&usage), map);
            }
            Err(e) => assert!(matches!(e, Error::NotFound(_))),
        }
    }

    #[tokio::test]
    async fn empty_registry_aggregates_to_default() {
        let svc = MonitoringService::new(Registry::new());
        let agg = svc.aggregate_host_usage().await.expect("aggregate");
        assert_eq!(agg, ResourceUsage::default());
        assert!(svc.all_samples().await.expect("samples").is_empty());
        assert!(svc.workload_usage(&Id::named("x")).await.is_err());
    }
}
