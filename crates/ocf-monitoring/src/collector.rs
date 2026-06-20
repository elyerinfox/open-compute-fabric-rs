//! The pluggable metrics-collection contract and its built-in backends.
//!
//! A [`MetricsCollector`] knows how to read resource usage for the host it runs
//! on and for individual workloads. The built-ins here read *real* counters:
//! [`HostMetricsCollector`] parses the Linux `/proc` filesystem (and shells out
//! to `df` for filesystem space), and [`RuntimeMetricsCollector`] shells out to
//! `docker stats` for per-container usage. The pure parsing logic lives in
//! [`crate::procfs`] and is unit-tested there with sample fixtures; this module
//! wires those parsers to the live reads and the two-sample interval diffs.

use crate::procfs::{
    self, cpu_busy_pct, disk_deltas, net_deltas, parse_diskstats, parse_meminfo, parse_net_dev,
    parse_proc_stat, per_second, DiskStats, NetCounters,
};
use crate::sample::{MetricSample, ResourceUsage};
use ocf_core::prelude::*;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::process::Command;

/// How long to wait between the two counter readings used to derive rates
/// (CPU busy %, net bytes/sec, disk IOPS). 100ms is long enough to see motion
/// without making `collect_host` feel sluggish.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
/// `SAMPLE_INTERVAL` expressed in milliseconds for the rate math.
const SAMPLE_INTERVAL_MS: u64 = 100;

/// Pluggable contract for reading resource usage off a host or workload.
///
/// Extends [`Provider`] so backends are swappable via a
/// [`Registry`]`<dyn MetricsCollector>`.
#[async_trait]
pub trait MetricsCollector: Provider {
    /// Whole-host resource usage (CPU, memory, disk, net, IOPS).
    async fn collect_host(&self) -> Result<ResourceUsage>;

    /// Resource usage for a single workload, by id. Backends that cannot see a
    /// given workload return [`Error::NotFound`].
    async fn collect_workload(&self, id: &Id) -> Result<ResourceUsage>;

    /// All current measurements flattened into individual [`MetricSample`]s,
    /// ready for time-series export. The default implementation flattens
    /// [`collect_host`](Self::collect_host) and tags it with this collector's
    /// `name()`; backends with per-workload visibility may override to append
    /// workload samples too.
    async fn samples(&self) -> Result<Vec<MetricSample>> {
        let mut labels = BTreeMap::new();
        labels.insert("collector".to_string(), self.name().to_string());
        labels.insert("subject".to_string(), "host".to_string());
        Ok(self.collect_host().await?.samples(&labels))
    }
}

/// Built-in host collector.
///
/// Reads real whole-host counters from the Linux `/proc` filesystem:
///
/// | Metric                  | Source                                            |
/// |-------------------------|---------------------------------------------------|
/// | `cpu_pct`               | `/proc/stat` busy delta over [`SAMPLE_INTERVAL`]  |
/// | `memory_used`/`_total`  | `/proc/meminfo` (`MemTotal`, `MemAvailable`)      |
/// | `disk_used`/`_total`    | `df -B1 /`                                         |
/// | `net_rx_bps`/`net_tx_bps` | `/proc/net/dev` byte delta over the interval    |
/// | `read_iops`/`write_iops`  | `/proc/diskstats` op delta over the interval    |
///
/// On non-Linux hosts these paths do not exist, so the `/proc` reads fail and
/// `collect_host` returns an honest [`Error::NotSupported`] rather than any
/// fabricated numbers. The crate still compiles everywhere — the failure is at
/// runtime, where `/proc` is genuinely absent.
#[derive(Debug, Clone)]
pub struct HostMetricsCollector {
    /// Logical hostname this collector reports for.
    host: String,
    /// Filesystem path whose space `df` reports (defaults to `/`).
    mount: String,
}

impl HostMetricsCollector {
    pub fn new(host: impl Into<String>) -> Self {
        HostMetricsCollector {
            host: host.into(),
            mount: "/".to_string(),
        }
    }

    /// Override the filesystem path used for disk-space reporting.
    pub fn with_mount(mut self, mount: impl Into<String>) -> Self {
        self.mount = mount.into();
        self
    }
}

impl Default for HostMetricsCollector {
    fn default() -> Self {
        HostMetricsCollector::new("localhost")
    }
}

impl Provider for HostMetricsCollector {
    fn name(&self) -> &str {
        "host"
    }
    fn description(&self) -> &str {
        "Whole-host resource metrics (reads /proc and df on Linux)"
    }
}

/// Read a `/proc` (or other) file into a string, mapping the absence of the
/// path onto an honest [`Error::NotSupported`] (the case on non-Linux hosts)
/// and any other I/O failure onto [`Error::Io`].
fn read_proc(path: &str) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::unsupported(format!(
            "host metrics require {path}, which is not present on this platform"
        ))),
        Err(e) => Err(Error::Io(format!("reading {path}: {e}"))),
    }
}

#[async_trait]
impl MetricsCollector for HostMetricsCollector {
    async fn collect_host(&self) -> Result<ResourceUsage> {
        tracing::debug!(host = %self.host, "collecting host metrics from /proc");

        // First reading of every rate counter.
        let cpu_t0 = parse_proc_stat(&read_proc("/proc/stat")?)
            .ok_or_else(|| Error::Io("no aggregate cpu line in /proc/stat".to_string()))?;
        let net_t0: NetCounters = parse_net_dev(&read_proc("/proc/net/dev")?);
        let disk_t0: DiskStats = parse_diskstats(&read_proc("/proc/diskstats")?);

        // Let the counters advance before reading them again.
        tokio::time::sleep(SAMPLE_INTERVAL).await;

        // Second reading.
        let cpu_t1 = parse_proc_stat(&read_proc("/proc/stat")?)
            .ok_or_else(|| Error::Io("no aggregate cpu line in /proc/stat".to_string()))?;
        let net_t1 = parse_net_dev(&read_proc("/proc/net/dev")?);
        let disk_t1 = parse_diskstats(&read_proc("/proc/diskstats")?);

        // Point-in-time gauges.
        let mem = parse_meminfo(&read_proc("/proc/meminfo")?)
            .ok_or_else(|| Error::Io("missing MemTotal/MemAvailable in /proc/meminfo".to_string()))?;
        let disk_space = self.read_disk_space().await?;

        // Derive rates from the two readings.
        let net_d = net_deltas(net_t0, net_t1);
        let disk_d = disk_deltas(disk_t0, disk_t1);

        Ok(ResourceUsage {
            cpu_pct: cpu_busy_pct(cpu_t0, cpu_t1),
            memory_used: mem.used_bytes(),
            memory_total: mem.total_bytes,
            disk_used: disk_space.used_bytes,
            disk_total: disk_space.total_bytes,
            net_rx_bps: per_second(net_d.rx_bytes, SAMPLE_INTERVAL_MS),
            net_tx_bps: per_second(net_d.tx_bytes, SAMPLE_INTERVAL_MS),
            read_iops: per_second(disk_d.reads_completed, SAMPLE_INTERVAL_MS),
            write_iops: per_second(disk_d.writes_completed, SAMPLE_INTERVAL_MS),
        })
    }

    async fn collect_workload(&self, id: &Id) -> Result<ResourceUsage> {
        // A host collector has no per-workload visibility; that is the runtime
        // collector's job.
        Err(Error::unsupported(format!(
            "host collector does not expose per-workload metrics for {id}"
        )))
    }

    async fn samples(&self) -> Result<Vec<MetricSample>> {
        let mut labels = BTreeMap::new();
        labels.insert("collector".to_string(), self.name().to_string());
        labels.insert("host".to_string(), self.host.clone());
        labels.insert("subject".to_string(), "host".to_string());
        Ok(self.collect_host().await?.samples(&labels))
    }
}

impl HostMetricsCollector {
    /// Read filesystem space for `self.mount` via `df -B1 <mount>`.
    ///
    /// `-B1` forces byte units so the parsed columns are already in bytes. A
    /// missing `df` (non-Linux, or a stripped image) surfaces as
    /// [`Error::NotSupported`]; a non-zero exit as [`Error::Io`].
    async fn read_disk_space(&self) -> Result<procfs::DiskSpace> {
        let output = Command::new("df")
            .arg("-B1")
            .arg(&self.mount)
            .output()
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Error::unsupported(format!(
                        "host disk metrics require `df`, which is not available: {e}"
                    ))
                } else {
                    Error::Io(format!("spawning df: {e}"))
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Io(format!(
                "df failed for {}: {}",
                self.mount,
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        procfs::parse_df(&stdout)
            .ok_or_else(|| Error::Io(format!("could not parse df output for {}", self.mount)))
    }
}

/// The `docker` binary the runtime collector shells out to. Matches the
/// container name convention used by `ocf-runtime` (workload id == container
/// name), so a workload id can be passed straight to `docker stats`.
const DOCKER_BIN: &str = "docker";

/// Built-in per-runtime collector.
///
/// Reads real per-container usage by shelling out to
/// `docker stats --no-stream`. The container id is the workload id (that is how
/// `ocf-runtime` names containers), so a workload id maps directly to a
/// `docker stats <id>` query. Docker reports CPU% and memory usage/limit
/// directly; network and block figures are cumulative lifetime totals (docker
/// exposes no instantaneous rate), surfaced as-is. Docker reports no IOPS, so
/// `read_iops`/`write_iops` stay zero for this backend.
///
/// If `docker` is not installed (or the container does not exist) the relevant
/// call returns [`Error::NotFound`] rather than any fabricated snapshot.
#[derive(Debug, Clone, Default)]
pub struct RuntimeMetricsCollector;

impl RuntimeMetricsCollector {
    pub fn new() -> Self {
        RuntimeMetricsCollector
    }
}

impl Provider for RuntimeMetricsCollector {
    fn name(&self) -> &str {
        "runtime"
    }
    fn description(&self) -> &str {
        "Per-workload resource metrics (reads `docker stats`)"
    }
}

#[async_trait]
impl MetricsCollector for RuntimeMetricsCollector {
    async fn collect_host(&self) -> Result<ResourceUsage> {
        // A runtime collector reports per-workload usage, not whole-host usage;
        // host-level rollups come from the host collector. Reporting an empty
        // snapshot keeps it a no-op contributor to fleet aggregation.
        Ok(ResourceUsage::default())
    }

    async fn collect_workload(&self, id: &Id) -> Result<ResourceUsage> {
        tracing::debug!(workload = %id, "collecting workload metrics from docker stats");

        // `docker stats --no-stream --format '<CPUPerc>|<MemUsage>|<NetIO>|<BlockIO>' <id>`
        // A pipe-delimited template avoids needing a JSON parser dependency.
        let format = "{{.CPUPerc}}|{{.MemUsage}}|{{.NetIO}}|{{.BlockIO}}";
        let output = Command::new(DOCKER_BIN)
            .arg("stats")
            .arg("--no-stream")
            .arg("--format")
            .arg(format)
            .arg(id.as_str())
            .output()
            .await
            .map_err(|e| {
                // Docker missing/not found => the workload simply cannot be seen.
                Error::not_found(format!(
                    "docker not available to read metrics for workload {id}: {e}"
                ))
            })?;

        if !output.status.success() {
            // Non-zero exit (e.g. no such container) => not found for this id.
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::not_found(format!(
                "docker stats found no metrics for workload {id}: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.lines().next().unwrap_or("").trim();
        if line.is_empty() {
            return Err(Error::not_found(format!(
                "docker stats returned no rows for workload {id}"
            )));
        }

        let stats = procfs::parse_docker_stats(line).ok_or_else(|| {
            Error::Io(format!("could not parse docker stats output for {id}: {line:?}"))
        })?;

        Ok(ResourceUsage {
            cpu_pct: stats.cpu_pct,
            memory_used: stats.mem_used_bytes,
            memory_total: stats.mem_total_bytes,
            // Docker stats exposes no per-container filesystem usage; only the
            // cumulative block-I/O byte totals below. Leave disk space unset.
            disk_used: 0,
            disk_total: 0,
            // NetIO/BlockIO are cumulative lifetime byte totals from docker, not
            // instantaneous rates; surfaced here as the best available signal.
            net_rx_bps: stats.net_rx_bytes,
            net_tx_bps: stats.net_tx_bytes,
            // Docker reports block I/O in bytes, not operations, so there is no
            // honest IOPS figure to report for a container.
            read_iops: 0,
            write_iops: 0,
        })
    }
}

/// Register the built-in metrics collectors into `reg`.
///
/// Mirrors every subsystem crate's `register_builtins`: registers the default
/// backends so a freshly-constructed controller has working collectors.
pub fn register_builtins(reg: &mut Registry<dyn MetricsCollector>) -> Result<()> {
    reg.register(
        "host",
        std::sync::Arc::new(HostMetricsCollector::default()) as std::sync::Arc<dyn MetricsCollector>,
    )?;
    reg.register(
        "runtime",
        std::sync::Arc::new(RuntimeMetricsCollector::new())
            as std::sync::Arc<dyn MetricsCollector>,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // These integration-flavoured tests exercise the live reads. On a Linux
    // host with `/proc` they return real numbers; on other platforms (or in a
    // sandbox without `/proc`) `collect_host` returns an honest error. Either
    // outcome is acceptable — what must never happen is a fabricated success.
    #[tokio::test]
    async fn host_collector_real_read_or_honest_error() {
        let c = HostMetricsCollector::default();
        match c.collect_host().await {
            Ok(u) => {
                // Real reading: values must be internally consistent.
                assert!(u.cpu_pct >= 0.0 && u.cpu_pct <= 100.0);
                assert!(u.memory_used <= u.memory_total);
                assert!(u.disk_used <= u.disk_total);
                assert!(!c.samples().await.expect("samples").is_empty());
            }
            Err(e) => {
                // No /proc (e.g. Windows / sandbox): must be NotSupported/Io,
                // never a silent fabricated success.
                assert!(matches!(e, Error::NotSupported(_) | Error::Io(_)));
            }
        }
    }

    #[tokio::test]
    async fn host_collector_has_no_workload_view() {
        let c = HostMetricsCollector::default();
        assert!(c.collect_workload(&Id::named("w1")).await.is_err());
    }

    #[tokio::test]
    async fn runtime_host_usage_is_empty() {
        let c = RuntimeMetricsCollector::new();
        assert_eq!(c.collect_host().await.unwrap(), ResourceUsage::default());
    }

    #[tokio::test]
    async fn runtime_collector_errors_without_docker_or_container() {
        // Without a running container named "definitely-not-a-real-workload"
        // (and possibly without docker at all), this must be an error, never a
        // fabricated snapshot.
        let c = RuntimeMetricsCollector::new();
        let r = c
            .collect_workload(&Id::named("definitely-not-a-real-workload"))
            .await;
        assert!(r.is_err());
        if let Err(e) = r {
            assert!(matches!(e, Error::NotFound(_) | Error::Io(_)));
        }
    }

    #[test]
    fn builtins_register() {
        let mut reg: Registry<dyn MetricsCollector> = Registry::new();
        register_builtins(&mut reg).expect("register");
        assert!(reg.contains("host"));
        assert!(reg.contains("runtime"));
    }
}
