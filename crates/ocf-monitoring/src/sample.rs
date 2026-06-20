//! The metric value types: point [`MetricSample`]s for time-series export and
//! the rolled-up [`ResourceUsage`] snapshot a collector reports for a host or
//! a single workload.

use chrono::{DateTime, Utc};
use ocf_core::prelude::*;
use std::collections::BTreeMap;

/// A single point measurement, ready to be shipped to a time-series store.
///
/// `labels` carry the dimensions a backend (Prometheus, etc.) would index on,
/// e.g. `{"host": "node-1"}` or `{"workload": "<id>"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSample {
    pub name: String,
    pub value: f64,
    /// Unit of `value`, e.g. `"percent"`, `"bytes"`, `"bps"`, `"iops"`.
    pub unit: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

impl MetricSample {
    /// Build a sample stamped with the current time.
    pub fn new(
        name: impl Into<String>,
        value: f64,
        unit: impl Into<String>,
        labels: BTreeMap<String, String>,
    ) -> Self {
        MetricSample {
            name: name.into(),
            value,
            unit: unit.into(),
            timestamp: Utc::now(),
            labels,
        }
    }
}

/// A rolled-up resource-usage snapshot for one subject (a host or a workload).
///
/// CPU is a 0..=100 percentage; memory/disk are byte counts; network rates are
/// bits-per-second; IOPS are operations-per-second. These mirror the dimensions
/// the autoscaler and the frontend dashboards consume.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ResourceUsage {
    /// CPU utilization as a percentage (0..=100).
    pub cpu_pct: f64,
    pub memory_used: u64,
    pub memory_total: u64,
    pub disk_used: u64,
    pub disk_total: u64,
    pub net_rx_bps: u64,
    pub net_tx_bps: u64,
    pub read_iops: u64,
    pub write_iops: u64,
}

impl ResourceUsage {
    /// Memory utilization as a percentage (0..=100), guarding divide-by-zero.
    pub fn memory_pct(&self) -> f64 {
        ratio_pct(self.memory_used, self.memory_total)
    }

    /// Disk utilization as a percentage (0..=100), guarding divide-by-zero.
    pub fn disk_pct(&self) -> f64 {
        ratio_pct(self.disk_used, self.disk_total)
    }

    /// Flatten this snapshot into individual [`MetricSample`]s for time-series
    /// export. `labels` are attached to every emitted sample.
    pub fn samples(&self, labels: &BTreeMap<String, String>) -> Vec<MetricSample> {
        vec![
            MetricSample::new("cpu_pct", self.cpu_pct, "percent", labels.clone()),
            MetricSample::new(
                "memory_used",
                self.memory_used as f64,
                "bytes",
                labels.clone(),
            ),
            MetricSample::new(
                "memory_total",
                self.memory_total as f64,
                "bytes",
                labels.clone(),
            ),
            MetricSample::new("memory_pct", self.memory_pct(), "percent", labels.clone()),
            MetricSample::new("disk_used", self.disk_used as f64, "bytes", labels.clone()),
            MetricSample::new(
                "disk_total",
                self.disk_total as f64,
                "bytes",
                labels.clone(),
            ),
            MetricSample::new("disk_pct", self.disk_pct(), "percent", labels.clone()),
            MetricSample::new("net_rx_bps", self.net_rx_bps as f64, "bps", labels.clone()),
            MetricSample::new("net_tx_bps", self.net_tx_bps as f64, "bps", labels.clone()),
            MetricSample::new("read_iops", self.read_iops as f64, "iops", labels.clone()),
            MetricSample::new(
                "write_iops",
                self.write_iops as f64,
                "iops",
                labels.clone(),
            ),
        ]
    }

    /// Project this snapshot onto the plain `BTreeMap<String, f64>` of metric
    /// names → values that the `ocf-runtime` autoscaler evaluates against its
    /// scaling rules.
    ///
    /// Keeping this a plain map (rather than a typed struct) is deliberate: the
    /// autoscaler depends only on `BTreeMap<String, f64>`, so `ocf-runtime` does
    /// not have to depend on `ocf-monitoring`. The emitted keys are stable and
    /// match the metric names a scaling rule would reference:
    ///
    /// - `"cpu"` / `"cpu_pct"` — CPU utilization percentage
    /// - `"memory_pct"` — memory utilization percentage
    /// - `"disk_pct"` — disk utilization percentage
    /// - `"net_rx_bps"`, `"net_tx_bps"` — network throughput
    /// - `"read_iops"`, `"write_iops"` — disk IOPS
    pub fn to_metric_map(&self) -> BTreeMap<String, f64> {
        let mut m = BTreeMap::new();
        // Provide both "cpu" and "cpu_pct" so a rule can spell it either way.
        m.insert("cpu".to_string(), self.cpu_pct);
        m.insert("cpu_pct".to_string(), self.cpu_pct);
        m.insert("memory_pct".to_string(), self.memory_pct());
        m.insert("memory_used".to_string(), self.memory_used as f64);
        m.insert("memory_total".to_string(), self.memory_total as f64);
        m.insert("disk_pct".to_string(), self.disk_pct());
        m.insert("net_rx_bps".to_string(), self.net_rx_bps as f64);
        m.insert("net_tx_bps".to_string(), self.net_tx_bps as f64);
        m.insert("read_iops".to_string(), self.read_iops as f64);
        m.insert("write_iops".to_string(), self.write_iops as f64);
        m
    }
}

/// Percentage of `used` out of `total`, returning `0.0` when `total == 0`.
fn ratio_pct(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64) * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> ResourceUsage {
        ResourceUsage {
            cpu_pct: 42.0,
            memory_used: 4 * 1024 * 1024 * 1024,
            memory_total: 8 * 1024 * 1024 * 1024,
            disk_used: 50,
            disk_total: 200,
            net_rx_bps: 1_000,
            net_tx_bps: 2_000,
            read_iops: 10,
            write_iops: 20,
        }
    }

    #[test]
    fn percentages_guard_zero_total() {
        let zero = ResourceUsage::default();
        assert_eq!(zero.memory_pct(), 0.0);
        assert_eq!(zero.disk_pct(), 0.0);
    }

    #[test]
    fn percentages_are_computed() {
        let u = usage();
        assert!((u.memory_pct() - 50.0).abs() < f64::EPSILON);
        assert!((u.disk_pct() - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn metric_map_exposes_autoscaler_keys() {
        let m = usage().to_metric_map();
        assert_eq!(m.get("cpu"), Some(&42.0));
        assert_eq!(m.get("cpu_pct"), Some(&42.0));
        assert_eq!(m.get("memory_pct"), Some(&50.0));
        assert!(m.contains_key("disk_pct"));
    }

    #[test]
    fn samples_carry_labels() {
        let mut labels = BTreeMap::new();
        labels.insert("host".to_string(), "node-1".to_string());
        let samples = usage().samples(&labels);
        assert!(!samples.is_empty());
        assert!(samples
            .iter()
            .all(|s| s.labels.get("host").map(String::as_str) == Some("node-1")));
    }
}
