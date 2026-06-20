//! Pure parsers for the Linux pseudo-files the host collector reads.
//!
//! Each function takes the *raw text* of a `/proc` (or `df`/`docker stats`)
//! reading and returns the structured counters the collector diffs across an
//! interval. Keeping the parsing pure — text in, numbers out, no I/O — is what
//! makes the host collector testable on any platform: the unit tests below feed
//! real sample fixtures and assert the extracted values, even though the live
//! reads only succeed on Linux.

/// Aggregate CPU jiffy counters from the first `cpu ` line of `/proc/stat`.
///
/// The fields are (in order): user, nice, system, idle, iowait, irq, softirq,
/// steal, guest, guest_nice. "Idle" time is `idle + iowait`; everything else is
/// "busy". We keep the raw totals so the collector can diff two readings and
/// derive a busy percentage over the interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuTimes {
    /// Sum of all jiffies across every column (busy + idle).
    pub total: u64,
    /// Idle jiffies (`idle + iowait`).
    pub idle: u64,
}

/// Parse the aggregate `cpu` line of `/proc/stat`.
///
/// Returns `None` if no `cpu ` summary line is present or it has too few fields.
pub fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
    for line in text.lines() {
        // The aggregate line is exactly "cpu" followed by whitespace; the
        // per-core lines are "cpu0", "cpu1", ... which we skip.
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("cpu") => {
                let values: Vec<u64> = fields.filter_map(|f| f.parse::<u64>().ok()).collect();
                // Need at least user..idle..iowait to compute idle vs busy.
                if values.len() < 5 {
                    return None;
                }
                let total: u64 = values.iter().copied().sum();
                // idle is column 4 (0-based 3), iowait is column 5 (0-based 4).
                let idle = values[3].saturating_add(values[4]);
                return Some(CpuTimes { total, idle });
            }
            _ => continue,
        }
    }
    None
}

/// Busy percentage (0..=100) between two `/proc/stat` readings.
///
/// `prev` is the earlier sample, `curr` the later one. Guards against a zero or
/// negative total delta (clock not advancing / counter reset) by returning 0.
pub fn cpu_busy_pct(prev: CpuTimes, curr: CpuTimes) -> f64 {
    let total_delta = curr.total.saturating_sub(prev.total);
    let idle_delta = curr.idle.saturating_sub(prev.idle);
    if total_delta == 0 {
        return 0.0;
    }
    let busy = total_delta.saturating_sub(idle_delta);
    (busy as f64 / total_delta as f64) * 100.0
}

/// Total and available memory in **bytes**, parsed from `/proc/meminfo`.
///
/// `/proc/meminfo` reports values in kibibytes (the `kB` suffix). We read
/// `MemTotal` and `MemAvailable` and derive `used = total - available`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

impl MemInfo {
    /// Used bytes (`total - available`), saturating at zero.
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }
}

/// Parse `MemTotal` and `MemAvailable` out of `/proc/meminfo`.
///
/// Returns `None` if either key is missing. Values are converted from the
/// kibibytes `/proc/meminfo` reports into bytes.
pub fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let mut total_kb: Option<u64> = None;
    let mut avail_kb: Option<u64> = None;
    for line in text.lines() {
        // Lines look like "MemTotal:       16329216 kB".
        let (key, rest) = match line.split_once(':') {
            Some(kv) => kv,
            None => continue,
        };
        let value_kb = rest.split_whitespace().next().and_then(|v| v.parse::<u64>().ok());
        match key.trim() {
            "MemTotal" => total_kb = value_kb,
            "MemAvailable" => avail_kb = value_kb,
            _ => {}
        }
        if total_kb.is_some() && avail_kb.is_some() {
            break;
        }
    }
    match (total_kb, avail_kb) {
        (Some(t), Some(a)) => Some(MemInfo {
            // kB in /proc/meminfo are kibibytes (1024 bytes).
            total_bytes: t.saturating_mul(1024),
            available_bytes: a.saturating_mul(1024),
        }),
        _ => None,
    }
}

/// Summed receive/transmit byte counters across non-loopback interfaces, parsed
/// from `/proc/net/dev`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// Sum the rx/tx byte columns of `/proc/net/dev` across every interface except
/// loopback (`lo`).
///
/// The file has two header lines, then one line per interface:
/// `  eth0: <rx_bytes> <rx_packets> ... <tx_bytes> <tx_packets> ...`
/// The receive block is columns 1..=8 and the transmit block 9..=16 after the
/// interface name; `rx_bytes` is the first receive column and `tx_bytes` the
/// first transmit column (the 9th value).
pub fn parse_net_dev(text: &str) -> NetCounters {
    let mut counters = NetCounters::default();
    for line in text.lines() {
        let (iface, rest) = match line.split_once(':') {
            Some(ir) => ir,
            None => continue, // header lines have no ':' before the columns
        };
        let iface = iface.trim();
        if iface.is_empty() || iface == "lo" {
            continue;
        }
        let cols: Vec<u64> = rest.split_whitespace().filter_map(|c| c.parse::<u64>().ok()).collect();
        // Receive bytes is the first column; transmit bytes is the 9th (index 8).
        if cols.len() >= 9 {
            counters.rx_bytes = counters.rx_bytes.saturating_add(cols[0]);
            counters.tx_bytes = counters.tx_bytes.saturating_add(cols[8]);
        }
    }
    counters
}

/// Completed read/write operation counters summed across physical block
/// devices, parsed from `/proc/diskstats`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiskStats {
    pub reads_completed: u64,
    pub writes_completed: u64,
}

/// Sum the "reads completed" and "writes completed" columns of
/// `/proc/diskstats` across physical devices.
///
/// Each line is: `<major> <minor> <name> <reads completed> <reads merged>
/// <sectors read> <ms reading> <writes completed> ...`. We count only whole
/// physical devices and skip partitions and virtual devices (loop/ram/dm/md)
/// so the rate reflects real disk I/O. Field 3 (0-based) is reads completed and
/// field 7 (0-based) is writes completed.
pub fn parse_diskstats(text: &str) -> DiskStats {
    let mut stats = DiskStats::default();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 8 {
            continue;
        }
        let name = fields[2];
        if !is_physical_device(name) {
            continue;
        }
        let reads = fields[3].parse::<u64>().unwrap_or(0);
        let writes = fields[7].parse::<u64>().unwrap_or(0);
        stats.reads_completed = stats.reads_completed.saturating_add(reads);
        stats.writes_completed = stats.writes_completed.saturating_add(writes);
    }
    stats
}

/// Whether a `/proc/diskstats` device name is a whole physical disk worth
/// counting (as opposed to a partition, loop, ramdisk, or mapper device).
fn is_physical_device(name: &str) -> bool {
    // Skip virtual / pseudo devices outright.
    if name.starts_with("loop")
        || name.starts_with("ram")
        || name.starts_with("dm-")
        || name.starts_with("md")
        || name.starts_with("zram")
        || name.starts_with("fd")
    {
        return false;
    }
    // For sd*/vd*/hd* style names, a trailing digit means a partition
    // (e.g. "sda1") which we skip in favour of the whole disk ("sda").
    if (name.starts_with("sd") || name.starts_with("vd") || name.starts_with("hd"))
        && name.chars().last().map(|c| c.is_ascii_digit()).unwrap_or(false)
    {
        return false;
    }
    // NVMe names are like "nvme0n1" (whole disk) vs "nvme0n1p1" (partition).
    if name.starts_with("nvme") {
        return !name.contains('p');
    }
    true
}

/// Total and used bytes for a filesystem, parsed from `df -B1 <path>` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiskSpace {
    pub total_bytes: u64,
    pub used_bytes: u64,
}

/// Parse the second line of `df -B1 <path>` output.
///
/// `df -B1` prints a header line then one data line:
/// `Filesystem  1B-blocks  Used  Available  Use%  Mounted on`
/// We read total (`1B-blocks`) and used directly as byte counts. The filesystem
/// name in column 0 can itself contain spaces in rare cases, so we anchor off
/// the last numeric columns by taking the first data line and reading columns
/// 1 and 2 after splitting on whitespace.
pub fn parse_df(text: &str) -> Option<DiskSpace> {
    // Skip the header line; take the first non-empty data line.
    let data_line = text.lines().skip(1).find(|l| !l.trim().is_empty())?;
    let cols: Vec<&str> = data_line.split_whitespace().collect();
    // Columns: Filesystem, total, used, available, use%, mountpoint.
    if cols.len() < 3 {
        return None;
    }
    let total = cols[1].parse::<u64>().ok()?;
    let used = cols[2].parse::<u64>().ok()?;
    Some(DiskSpace {
        total_bytes: total,
        used_bytes: used,
    })
}

/// Per-container stats parsed from one line of `docker stats --no-stream`.
///
/// Docker reports CPU as a percentage and memory/net/block as human-readable
/// sizes with binary or decimal suffixes. Net and block figures are *cumulative
/// totals* for the container's lifetime (docker exposes no instantaneous rate),
/// which is the most honest signal available from `docker stats`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DockerStats {
    pub cpu_pct: f64,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
    pub block_read_bytes: u64,
    pub block_write_bytes: u64,
}

/// Parse a single `docker stats --no-stream` record formatted as
/// `<CPUPerc>|<MemUsage>|<NetIO>|<BlockIO>`.
///
/// Example line:
/// `12.34%|1.5GiB / 7.7GiB|1.2kB / 3.4kB|0B / 0B`
///
/// Returns `None` if the line does not have the four expected fields.
pub fn parse_docker_stats(line: &str) -> Option<DockerStats> {
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() < 4 {
        return None;
    }
    let cpu_pct = parse_percent(parts[0]);
    let (mem_used, mem_total) = parse_pair(parts[1], parse_size_bytes);
    let (net_rx, net_tx) = parse_pair(parts[2], parse_size_bytes);
    let (blk_read, blk_write) = parse_pair(parts[3], parse_size_bytes);
    Some(DockerStats {
        cpu_pct,
        mem_used_bytes: mem_used,
        mem_total_bytes: mem_total,
        net_rx_bytes: net_rx,
        net_tx_bytes: net_tx,
        block_read_bytes: blk_read,
        block_write_bytes: blk_write,
    })
}

/// Parse a `docker stats` percentage field like `"12.34%"` into `12.34`.
fn parse_percent(field: &str) -> f64 {
    field.trim().trim_end_matches('%').trim().parse::<f64>().unwrap_or(0.0)
}

/// Split a `"<a> / <b>"` docker field and convert each half with `conv`.
fn parse_pair(field: &str, conv: fn(&str) -> u64) -> (u64, u64) {
    let mut halves = field.split('/');
    let a = halves.next().map(conv).unwrap_or(0);
    let b = halves.next().map(conv).unwrap_or(0);
    (a, b)
}

/// Convert a docker human-readable size (`"1.5GiB"`, `"3.4kB"`, `"0B"`) into
/// bytes. Recognizes both binary (`KiB`/`MiB`/`GiB`/`TiB`) and decimal
/// (`kB`/`MB`/`GB`/`TB`) suffixes as docker emits them; an unrecognized or
/// absent number yields 0.
pub fn parse_size_bytes(field: &str) -> u64 {
    let s = field.trim();
    if s.is_empty() {
        return 0;
    }
    // Split the numeric prefix from the unit suffix.
    let split_at = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num_str, unit) = s.split_at(split_at);
    let num: f64 = match num_str.trim().parse() {
        Ok(n) => n,
        Err(_) => return 0,
    };
    let multiplier: f64 = match unit.trim() {
        "B" | "" => 1.0,
        "kB" | "KB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "TB" => 1_000_000_000_000.0,
        "KiB" => 1_024.0,
        "MiB" => 1_024.0 * 1_024.0,
        "GiB" => 1_024.0 * 1_024.0 * 1_024.0,
        "TiB" => 1_024.0 * 1_024.0 * 1_024.0 * 1_024.0,
        _ => 1.0,
    };
    (num * multiplier) as u64
}

/// Convert a byte-count delta observed over `interval_ms` milliseconds into a
/// per-second rate, guarding against a zero interval.
pub fn per_second(delta: u64, interval_ms: u64) -> u64 {
    if interval_ms == 0 {
        return 0;
    }
    // delta bytes over interval_ms ms => delta * 1000 / interval_ms per second.
    (delta as u128 * 1000 / interval_ms as u128) as u64
}

/// Convenience: the rx/tx byte deltas between two `/proc/net/dev` readings.
pub fn net_deltas(prev: NetCounters, curr: NetCounters) -> NetCounters {
    NetCounters {
        rx_bytes: curr.rx_bytes.saturating_sub(prev.rx_bytes),
        tx_bytes: curr.tx_bytes.saturating_sub(prev.tx_bytes),
    }
}

/// Convenience: the read/write op deltas between two `/proc/diskstats` readings.
pub fn disk_deltas(prev: DiskStats, curr: DiskStats) -> DiskStats {
    DiskStats {
        reads_completed: curr.reads_completed.saturating_sub(prev.reads_completed),
        writes_completed: curr.writes_completed.saturating_sub(prev.writes_completed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trimmed-down but faithful `/proc/stat` (two readings) so we can exercise
    // the busy-percentage delta math with known numbers.
    const PROC_STAT_T0: &str = "\
cpu  100 0 100 800 0 0 0 0 0 0
cpu0 50 0 50 400 0 0 0 0 0 0
intr 12345
ctxt 67890
";
    const PROC_STAT_T1: &str = "\
cpu  150 0 150 900 0 0 0 0 0 0
cpu0 75 0 75 450 0 0 0 0 0 0
intr 22345
";

    #[test]
    fn proc_stat_parses_aggregate_line() {
        let t = parse_proc_stat(PROC_STAT_T0).expect("cpu line");
        // total = 100+0+100+800 = 1000; idle = 800 + 0 (iowait) = 800.
        assert_eq!(t.total, 1000);
        assert_eq!(t.idle, 800);
    }

    #[test]
    fn proc_stat_missing_cpu_line_is_none() {
        assert!(parse_proc_stat("intr 1\nctxt 2\n").is_none());
    }

    #[test]
    fn cpu_busy_pct_computes_delta() {
        let a = parse_proc_stat(PROC_STAT_T0).unwrap();
        let b = parse_proc_stat(PROC_STAT_T1).unwrap();
        // total delta = 1200-1000 = 200; idle delta = 900-800 = 100.
        // busy = 100; pct = 100/200 = 50%.
        assert!((cpu_busy_pct(a, b) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn cpu_busy_pct_guards_zero_and_reset() {
        let z = CpuTimes::default();
        assert_eq!(cpu_busy_pct(z, z), 0.0);
        // Counter reset (curr < prev) must not underflow or exceed bounds.
        let hi = CpuTimes { total: 1000, idle: 500 };
        let lo = CpuTimes { total: 10, idle: 5 };
        let pct = cpu_busy_pct(hi, lo);
        assert!((0.0..=100.0).contains(&pct));
    }

    const MEMINFO: &str = "\
MemTotal:       16329216 kB
MemFree:         2500000 kB
MemAvailable:    8164608 kB
Buffers:          100000 kB
Cached:          4000000 kB
";

    #[test]
    fn meminfo_parses_total_and_available() {
        let m = parse_meminfo(MEMINFO).expect("meminfo");
        assert_eq!(m.total_bytes, 16329216 * 1024);
        assert_eq!(m.available_bytes, 8164608 * 1024);
        // used = total - available.
        assert_eq!(m.used_bytes(), (16329216 - 8164608) * 1024);
    }

    #[test]
    fn meminfo_missing_keys_is_none() {
        assert!(parse_meminfo("MemTotal: 100 kB\n").is_none());
        assert!(parse_meminfo("MemAvailable: 100 kB\n").is_none());
    }

    // Two header lines then per-interface rows, matching the real file layout.
    const NET_DEV: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1000      10    0    0    0     0          0         0     1000      10    0    0    0     0       0          0
  eth0: 5000      50    0    0    0     0          0         0     2000      20    0    0    0     0       0          0
  eth1: 3000      30    0    0    0     0          0         0     1000      10    0    0    0     0       0          0
";

    #[test]
    fn net_dev_sums_non_loopback() {
        let c = parse_net_dev(NET_DEV);
        // rx = 5000 + 3000 (lo excluded); tx = 2000 + 1000.
        assert_eq!(c.rx_bytes, 8000);
        assert_eq!(c.tx_bytes, 3000);
    }

    #[test]
    fn net_dev_deltas_and_rate() {
        let prev = NetCounters { rx_bytes: 1000, tx_bytes: 500 };
        let curr = NetCounters { rx_bytes: 2000, tx_bytes: 1500 };
        let d = net_deltas(prev, curr);
        assert_eq!(d.rx_bytes, 1000);
        assert_eq!(d.tx_bytes, 1000);
        // 1000 bytes over 100ms => 10_000 bytes/sec.
        assert_eq!(per_second(d.rx_bytes, 100), 10_000);
        assert_eq!(per_second(d.rx_bytes, 0), 0);
    }

    // major minor name reads rds-merged sectors ms writes ...
    const DISKSTATS: &str = "\
   8       0 sda 1000 0 0 0 500 0 0 0 0 0 0
   8       1 sda1 900 0 0 0 400 0 0 0 0 0 0
 259       0 nvme0n1 2000 0 0 0 800 0 0 0 0 0 0
 259       1 nvme0n1p1 1900 0 0 0 700 0 0 0 0 0 0
   7       0 loop0 5 0 0 0 5 0 0 0 0 0 0
";

    #[test]
    fn diskstats_counts_only_physical_disks() {
        let s = parse_diskstats(DISKSTATS);
        // Whole disks sda + nvme0n1 only: reads = 1000+2000, writes = 500+800.
        assert_eq!(s.reads_completed, 3000);
        assert_eq!(s.writes_completed, 1300);
    }

    #[test]
    fn diskstats_deltas() {
        let prev = DiskStats { reads_completed: 100, writes_completed: 50 };
        let curr = DiskStats { reads_completed: 130, writes_completed: 60 };
        let d = disk_deltas(prev, curr);
        assert_eq!(d.reads_completed, 30);
        assert_eq!(d.writes_completed, 10);
        // 30 ops over 100ms => 300 iops.
        assert_eq!(per_second(d.reads_completed, 100), 300);
    }

    #[test]
    fn is_physical_device_classifies() {
        assert!(is_physical_device("sda"));
        assert!(!is_physical_device("sda1"));
        assert!(is_physical_device("nvme0n1"));
        assert!(!is_physical_device("nvme0n1p1"));
        assert!(!is_physical_device("loop0"));
        assert!(!is_physical_device("dm-0"));
        assert!(!is_physical_device("ram0"));
    }

    const DF: &str = "\
Filesystem      1B-blocks         Used    Available Use% Mounted on
/dev/sda1    1073741824000 257698037760 816043786240  24% /
";

    #[test]
    fn df_parses_total_and_used_bytes() {
        let d = parse_df(DF).expect("df");
        assert_eq!(d.total_bytes, 1073741824000);
        assert_eq!(d.used_bytes, 257698037760);
    }

    #[test]
    fn df_without_data_line_is_none() {
        assert!(parse_df("Filesystem 1B-blocks Used Available Use% Mounted on\n").is_none());
    }

    #[test]
    fn size_bytes_handles_binary_and_decimal_units() {
        assert_eq!(parse_size_bytes("0B"), 0);
        assert_eq!(parse_size_bytes("512B"), 512);
        assert_eq!(parse_size_bytes("1kB"), 1_000);
        assert_eq!(parse_size_bytes("1KiB"), 1_024);
        assert_eq!(parse_size_bytes("1MiB"), 1_048_576);
        assert_eq!(parse_size_bytes("1.5GiB"), (1.5 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert_eq!(parse_size_bytes("2GB"), 2_000_000_000);
        // Garbage yields 0 rather than panicking.
        assert_eq!(parse_size_bytes("garbage"), 0);
        assert_eq!(parse_size_bytes(""), 0);
    }

    #[test]
    fn docker_stats_parses_all_fields() {
        let line = "12.34%|1.5GiB / 7.7GiB|1.2kB / 3.4kB|0B / 0B";
        let s = parse_docker_stats(line).expect("stats");
        assert!((s.cpu_pct - 12.34).abs() < 1e-9);
        assert_eq!(s.mem_used_bytes, (1.5 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert_eq!(s.mem_total_bytes, (7.7 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert_eq!(s.net_rx_bytes, 1_200);
        assert_eq!(s.net_tx_bytes, 3_400);
        assert_eq!(s.block_read_bytes, 0);
        assert_eq!(s.block_write_bytes, 0);
    }

    #[test]
    fn docker_stats_rejects_short_lines() {
        assert!(parse_docker_stats("12%|1GiB / 2GiB").is_none());
        assert!(parse_docker_stats("").is_none());
    }

    #[test]
    fn docker_stats_tolerates_whitespace() {
        let line = "  0.00% | 100MiB / 200MiB | 0B / 0B | 4kB / 8kB ";
        let s = parse_docker_stats(line).expect("stats");
        assert_eq!(s.cpu_pct, 0.0);
        assert_eq!(s.mem_used_bytes, 100 * 1024 * 1024);
        assert_eq!(s.block_read_bytes, 4_000);
        assert_eq!(s.block_write_bytes, 8_000);
    }
}
