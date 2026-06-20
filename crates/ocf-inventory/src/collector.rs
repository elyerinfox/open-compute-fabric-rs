//! The hardware-collection contract and a DMI-based backend.

use crate::component::{ComponentKind, HardwareComponent, MachineInventory};
use crate::exec;
use ocf_core::prelude::*;
use std::fs;
use std::path::Path;

/// Pluggable contract for discovering a machine's hardware.
///
/// A backend inspects one machine (out-of-band or via an on-host agent) and
/// returns its [`MachineInventory`]. Extends [`Provider`] so backends are named
/// and swappable through a [`Registry`].
#[async_trait]
pub trait InventoryCollector: Provider {
    /// Collect the full hardware inventory for `machine_id`.
    async fn collect(&self, machine_id: &Id) -> Result<MachineInventory>;
}

/// Collector that parses SMBIOS/DMI tables (via `dmidecode`) and Linux sysfs /
/// procfs to enumerate a machine's hardware.
///
/// `collect` gathers, on the host it runs on:
///
/// * **Baseboard serial** — `dmidecode -s baseboard-serial-number`, falling back
///   to reading `/sys/class/dmi/id/board_serial`.
/// * **CPUs** — parsed from `/proc/cpuinfo`, grouped by `physical id`.
/// * **Memory DIMMs** — `dmidecode -t 17` (memory devices), populated slots only.
/// * **NICs** — `/sys/class/net/*/address` (loopback and zero MACs skipped).
/// * **Disks** — `/sys/block/*`, with the serial read from the device sysfs node.
///
/// Each step degrades independently: a missing `dmidecode` or unreadable sysfs
/// path yields zero components of that kind rather than failing the whole scan,
/// because most of this tooling is Linux- and privilege-specific. The baseboard
/// serial is the one identity we always want, so it falls back to a synthesized
/// value derived from the machine id when neither source is readable.
pub struct DmiInventoryCollector;

impl DmiInventoryCollector {
    pub fn new() -> Self {
        DmiInventoryCollector
    }

    /// Resolve the chassis baseboard serial.
    ///
    /// Prefers `dmidecode -s baseboard-serial-number`; if that binary is missing
    /// or errors, reads `/sys/class/dmi/id/board_serial`. When neither is
    /// available (non-Linux host, no privilege) it derives a stable per-machine
    /// fallback so the inventory always has a usable hardware identity.
    async fn baseboard_serial(machine_id: &Id) -> String {
        match exec::run("dmidecode", &["-s", "baseboard-serial-number"]).await {
            Ok(out) => {
                if let Some(serial) = clean_dmi_value(&out) {
                    return serial;
                }
            }
            Err(e) => tracing::debug!(error = %e, "dmidecode baseboard serial unavailable"),
        }
        if let Ok(raw) = fs::read_to_string("/sys/class/dmi/id/board_serial") {
            if let Some(serial) = clean_dmi_value(&raw) {
                return serial;
            }
        }
        tracing::debug!(machine_id = %machine_id, "no DMI baseboard serial; synthesizing from machine id");
        format!("BB-{}", machine_id.as_str())
    }

    /// CPUs parsed from `/proc/cpuinfo`. Returns an empty vec if the file can't
    /// be read (e.g. on a non-Linux host).
    fn cpus() -> Vec<HardwareComponent> {
        match fs::read_to_string("/proc/cpuinfo") {
            Ok(text) => parse_cpuinfo(&text),
            Err(e) => {
                tracing::debug!(error = %e, "/proc/cpuinfo unavailable; no CPUs collected");
                Vec::new()
            }
        }
    }

    /// Memory DIMMs parsed from `dmidecode -t 17` (Type 17 = Memory Device).
    /// Returns an empty vec if `dmidecode` is unavailable.
    async fn memory() -> Vec<HardwareComponent> {
        match exec::run("dmidecode", &["-t", "17"]).await {
            Ok(out) => parse_dmidecode_memory(&out),
            Err(e) => {
                tracing::debug!(error = %e, "dmidecode -t 17 unavailable; no memory collected");
                Vec::new()
            }
        }
    }

    /// NICs from `/sys/class/net/*/address`. Skips loopback and all-zero MACs.
    fn nics() -> Vec<HardwareComponent> {
        collect_nics(Path::new("/sys/class/net"))
    }

    /// Block devices from `/sys/block/*`, with serials from the device node.
    fn disks() -> Vec<HardwareComponent> {
        collect_disks(Path::new("/sys/block"))
    }
}

impl Default for DmiInventoryCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for DmiInventoryCollector {
    fn name(&self) -> &str {
        "dmi"
    }
    fn description(&self) -> &str {
        "DMI/SMBIOS hardware inventory collector (dmidecode + sysfs/procfs)"
    }
}

#[async_trait]
impl InventoryCollector for DmiInventoryCollector {
    async fn collect(&self, machine_id: &Id) -> Result<MachineInventory> {
        let baseboard_serial = Self::baseboard_serial(machine_id).await;

        let mut inv = MachineInventory::new(machine_id.clone(), baseboard_serial.as_str());

        // Baseboard itself, identified by its serial. Augment with the DMI
        // board name/vendor and BIOS version when those sysfs nodes are present.
        let board_vendor = read_dmi_id("board_vendor").unwrap_or_else(|| "unknown".to_string());
        let board_name = read_dmi_id("board_name").unwrap_or_else(|| "unknown".to_string());
        let mut baseboard = HardwareComponent::new(
            ComponentKind::Baseboard,
            board_vendor,
            board_name,
            baseboard_serial.as_str(),
        );
        if let Some(bios) = read_dmi_id("bios_version") {
            baseboard = baseboard.with_attribute("bios_version", bios);
        }
        inv.components.push(baseboard);

        inv.components.extend(Self::cpus());
        inv.components.extend(Self::memory().await);
        inv.components.extend(Self::nics());
        inv.components.extend(Self::disks());

        Ok(inv)
    }
}

/// Read and trim a single `/sys/class/dmi/id/<field>` value, returning `None`
/// when the node is absent or empty.
fn read_dmi_id(field: &str) -> Option<String> {
    let raw = fs::read_to_string(format!("/sys/class/dmi/id/{field}")).ok()?;
    clean_dmi_value(&raw)
}

/// Normalize a DMI value: trim, and treat empty / placeholder strings (which
/// firmware commonly stamps in unfilled fields) as "no value".
fn clean_dmi_value(raw: &str) -> Option<String> {
    let v = raw.trim();
    if v.is_empty() {
        return None;
    }
    let lower = v.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "to be filled by o.e.m."
            | "to be filled by o.e.m"
            | "not specified"
            | "default string"
            | "none"
            | "n/a"
            | "0x00000000"
            | "00000000"
    ) {
        return None;
    }
    Some(v.to_string())
}

/// Parse `/proc/cpuinfo` into one [`HardwareComponent`] per physical CPU package.
///
/// Linux lists one block per *logical* processor. We group by `physical id`,
/// count the logical processors (threads) per package, take `cpu cores` for the
/// physical-core count, and read `model name`/`vendor_id` for identity. When a
/// kernel omits `physical id` (single-socket / some VMs), all processors fold
/// into package `0`.
pub fn parse_cpuinfo(text: &str) -> Vec<HardwareComponent> {
    use std::collections::BTreeMap;

    /// Accumulated facts for one physical package.
    #[derive(Default)]
    struct Package {
        vendor: String,
        model: String,
        cores: Option<String>,
        mhz: Option<String>,
        threads: usize,
    }

    let mut packages: BTreeMap<String, Package> = BTreeMap::new();
    // Fields for the processor block currently being parsed.
    let mut cur_phys = "0".to_string();
    let mut cur_vendor = String::new();
    let mut cur_model = String::new();
    let mut cur_cores: Option<String> = None;
    let mut cur_mhz: Option<String> = None;
    let mut saw_processor = false;

    // Commit the current block into its package bucket.
    let mut flush =
        |phys: &str, vendor: &str, model: &str, cores: &Option<String>, mhz: &Option<String>| {
            let pkg = packages.entry(phys.to_string()).or_default();
            pkg.threads += 1;
            if pkg.vendor.is_empty() && !vendor.is_empty() {
                pkg.vendor = vendor.to_string();
            }
            if pkg.model.is_empty() && !model.is_empty() {
                pkg.model = model.to_string();
            }
            if pkg.cores.is_none() {
                pkg.cores = cores.clone();
            }
            if pkg.mhz.is_none() {
                pkg.mhz = mhz.clone();
            }
        };

    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            // Blank line terminates a processor block.
            if saw_processor {
                flush(&cur_phys, &cur_vendor, &cur_model, &cur_cores, &cur_mhz);
            }
            cur_phys = "0".to_string();
            cur_vendor.clear();
            cur_model.clear();
            cur_cores = None;
            cur_mhz = None;
            saw_processor = false;
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "processor" => saw_processor = true,
            "physical id" => cur_phys = value.to_string(),
            "vendor_id" => cur_vendor = value.to_string(),
            "model name" => cur_model = value.to_string(),
            "cpu cores" => cur_cores = Some(value.to_string()),
            "cpu MHz" => {
                // Round the reported (often fractional) clock to a whole MHz.
                cur_mhz = Some(match value.split_once('.') {
                    Some((whole, _frac)) => whole.to_string(),
                    None => value.to_string(),
                });
            }
            _ => {}
        }
    }
    // Trailing block with no terminating blank line.
    if saw_processor {
        flush(&cur_phys, &cur_vendor, &cur_model, &cur_cores, &cur_mhz);
    }

    packages
        .into_iter()
        .map(|(phys, pkg)| {
            let vendor = if pkg.vendor.is_empty() {
                "unknown".to_string()
            } else {
                pkg.vendor
            };
            let model = if pkg.model.is_empty() {
                "unknown".to_string()
            } else {
                pkg.model
            };
            let serial = format!("CPU{phys}");
            let mut comp = HardwareComponent::new(ComponentKind::Cpu, vendor, model, serial)
                .with_attribute("threads", pkg.threads.to_string());
            if let Some(cores) = pkg.cores {
                comp = comp.with_attribute("cores", cores);
            }
            if let Some(mhz) = pkg.mhz {
                comp = comp.with_attribute("clock_mhz", mhz);
            }
            comp
        })
        .collect()
}

/// Parse `dmidecode -t 17` output into one [`HardwareComponent`] per *populated*
/// memory device. Empty slots (`Size: No Module Installed`) are skipped.
///
/// dmidecode prints a `Handle ...` line followed by an indented block of
/// `Key: Value` pairs per device; blocks are separated by blank lines.
pub fn parse_dmidecode_memory(text: &str) -> Vec<HardwareComponent> {
    let mut out = Vec::new();

    // Split into per-handle device blocks. Each device starts at a "Handle"
    // line; we collect the indented body that follows it.
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.starts_with("Handle ") {
            if let Some(b) = current.take() {
                blocks.push(b);
            }
            current = Some(Vec::new());
        } else if let Some(b) = current.as_mut() {
            b.push(line);
        }
    }
    if let Some(b) = current.take() {
        blocks.push(b);
    }

    for block in blocks {
        // Only Memory Device blocks describe DIMMs.
        let is_memory_device = block
            .iter()
            .any(|l| l.trim() == "Memory Device");
        if !is_memory_device {
            continue;
        }

        let mut size: Option<String> = None;
        let mut manufacturer: Option<String> = None;
        let mut part: Option<String> = None;
        let mut serial: Option<String> = None;
        let mut locator: Option<String> = None;
        let mut speed: Option<String> = None;
        let mut mem_type: Option<String> = None;

        for line in &block {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "Size" => size = Some(value.to_string()),
                "Manufacturer" => manufacturer = clean_dmi_value(value),
                "Part Number" => part = clean_dmi_value(value),
                "Serial Number" => serial = clean_dmi_value(value),
                "Locator" => locator = clean_dmi_value(value),
                "Speed" => speed = clean_dmi_value(value),
                "Type" => mem_type = clean_dmi_value(value),
                _ => {}
            }
        }

        // Skip unpopulated slots.
        let size = match &size {
            Some(s)
                if !s.eq_ignore_ascii_case("No Module Installed") && !s.eq_ignore_ascii_case("0") =>
            {
                s.clone()
            }
            _ => continue,
        };

        // Serial is the natural key; fall back to the slot locator, then a
        // synthesized name, so every populated DIMM is trackable.
        let serial = serial
            .or_else(|| locator.clone().map(|l| format!("DIMM-{l}")))
            .unwrap_or_else(|| "DIMM-unknown".to_string());

        let mut comp = HardwareComponent::new(
            ComponentKind::MemoryModule,
            manufacturer.unwrap_or_else(|| "unknown".to_string()),
            part.unwrap_or_else(|| "unknown".to_string()),
            serial,
        )
        .with_attribute("size", size);
        if let Some(loc) = locator {
            comp = comp.with_attribute("locator", loc);
        }
        if let Some(sp) = speed {
            comp = comp.with_attribute("speed", sp);
        }
        if let Some(t) = mem_type {
            comp = comp.with_attribute("type", t);
        }
        out.push(comp);
    }

    out
}

/// Enumerate NICs by reading each interface's MAC from `<net_dir>/<iface>/address`.
///
/// Loopback (`lo`) and interfaces with an all-zero MAC (virtual/down devices
/// that report no hardware address) are skipped. The MAC is used as the serial.
fn collect_nics(net_dir: &Path) -> Vec<HardwareComponent> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(net_dir) else {
        return out;
    };
    let mut ifaces: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    ifaces.sort();

    for iface in ifaces {
        if iface == "lo" {
            continue;
        }
        let addr_path = net_dir.join(&iface).join("address");
        let Ok(raw) = fs::read_to_string(&addr_path) else {
            continue;
        };
        let mac = raw.trim().to_string();
        if mac.is_empty() || mac == "00:00:00:00:00:00" {
            continue;
        }
        let mut comp =
            HardwareComponent::new(ComponentKind::Nic, "unknown", iface.as_str(), mac.as_str())
                .with_attribute("interface", iface.as_str())
                .with_attribute("mac", mac.as_str());

        // Link speed in Mbps, when the kernel exposes it (often -1 when down).
        if let Ok(speed) = fs::read_to_string(net_dir.join(&iface).join("speed")) {
            let speed = speed.trim();
            if let Ok(mbps) = speed.parse::<i64>() {
                if mbps > 0 {
                    comp = comp.with_attribute("speed_mbps", mbps.to_string());
                }
            }
        }
        out.push(comp);
    }
    out
}

/// Enumerate block devices under `<block_dir>` (`/sys/block`), reading each
/// device's serial and size from sysfs.
///
/// Pseudo block devices (loop, ram, zram, device-mapper, etc.) are skipped. The
/// serial is read from `device/serial` (or `serial`) and falls back to the
/// device name so the disk is still trackable.
fn collect_disks(block_dir: &Path) -> Vec<HardwareComponent> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(block_dir) else {
        return out;
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();

    for name in names {
        if name.starts_with("loop")
            || name.starts_with("ram")
            || name.starts_with("zram")
            || name.starts_with("dm-")
            || name.starts_with("md")
            || name.starts_with("sr")
        {
            continue;
        }
        let dev = block_dir.join(&name);

        // Serial: prefer the device node's serial, then a top-level one.
        let serial = fs::read_to_string(dev.join("device").join("serial"))
            .ok()
            .and_then(|s| clean_dmi_value(&s))
            .or_else(|| {
                fs::read_to_string(dev.join("serial"))
                    .ok()
                    .and_then(|s| clean_dmi_value(&s))
            })
            .unwrap_or_else(|| name.clone());

        let vendor = fs::read_to_string(dev.join("device").join("vendor"))
            .ok()
            .and_then(|s| clean_dmi_value(&s))
            .unwrap_or_else(|| "unknown".to_string());
        let model = fs::read_to_string(dev.join("device").join("model"))
            .ok()
            .and_then(|s| clean_dmi_value(&s))
            .unwrap_or_else(|| name.clone());

        let mut comp = HardwareComponent::new(ComponentKind::Disk, vendor, model, serial)
            .with_attribute("device", name.as_str());

        // Size: sysfs reports the capacity in 512-byte sectors.
        if let Ok(sectors) = fs::read_to_string(dev.join("size")) {
            if let Ok(sectors) = sectors.trim().parse::<u64>() {
                comp = comp.with_attribute("size_bytes", (sectors * 512).to_string());
            }
        }
        // Rotational flag distinguishes spinning disks (1) from SSD/NVMe (0).
        if let Ok(rot) = fs::read_to_string(dev.join("queue").join("rotational")) {
            let interface = if rot.trim() == "0" { "ssd" } else { "rotational" };
            comp = comp.with_attribute("media", interface);
        }
        out.push(comp);
    }
    out
}

/// Register the default inventory collectors.
pub fn register_builtins(reg: &mut Registry<dyn InventoryCollector>) -> Result<()> {
    reg.register("dmi", std::sync::Arc::new(DmiInventoryCollector::new()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cpuinfo_groups_by_physical_package() {
        // Two logical processors on one physical package (1 core, 2 threads).
        let sample = "\
processor\t: 0
vendor_id\t: GenuineIntel
model name\t: Intel(R) Xeon(R) Gold 6338 CPU @ 2.00GHz
physical id\t: 0
cpu cores\t: 32
cpu MHz\t\t: 2000.000

processor\t: 1
vendor_id\t: GenuineIntel
model name\t: Intel(R) Xeon(R) Gold 6338 CPU @ 2.00GHz
physical id\t: 0
cpu cores\t: 32
cpu MHz\t\t: 2000.000

processor\t: 2
vendor_id\t: GenuineIntel
model name\t: Intel(R) Xeon(R) Gold 6338 CPU @ 2.00GHz
physical id\t: 1
cpu cores\t: 32
cpu MHz\t\t: 1999.998
";
        let cpus = parse_cpuinfo(sample);
        assert_eq!(cpus.len(), 2, "two physical packages");

        let cpu0 = &cpus[0];
        assert_eq!(cpu0.kind, ComponentKind::Cpu);
        assert_eq!(cpu0.serial, "CPU0");
        assert_eq!(cpu0.vendor, "GenuineIntel");
        assert_eq!(cpu0.model, "Intel(R) Xeon(R) Gold 6338 CPU @ 2.00GHz");
        assert_eq!(cpu0.attributes.get("threads").map(String::as_str), Some("2"));
        assert_eq!(cpu0.attributes.get("cores").map(String::as_str), Some("32"));
        assert_eq!(
            cpu0.attributes.get("clock_mhz").map(String::as_str),
            Some("2000")
        );

        let cpu1 = &cpus[1];
        assert_eq!(cpu1.serial, "CPU1");
        assert_eq!(cpu1.attributes.get("threads").map(String::as_str), Some("1"));
        // Fractional MHz is rounded down to a whole number.
        assert_eq!(
            cpu1.attributes.get("clock_mhz").map(String::as_str),
            Some("1999")
        );
    }

    #[test]
    fn parse_cpuinfo_folds_into_package_zero_without_physical_id() {
        // ARM / some VMs omit `physical id`; all processors should fold into 0.
        let sample = "\
processor\t: 0
model name\t: Cortex-A72

processor\t: 1
model name\t: Cortex-A72
";
        let cpus = parse_cpuinfo(sample);
        assert_eq!(cpus.len(), 1);
        assert_eq!(cpus[0].serial, "CPU0");
        assert_eq!(cpus[0].attributes.get("threads").map(String::as_str), Some("2"));
        // No vendor_id in the sample -> recorded as unknown rather than panicking.
        assert_eq!(cpus[0].vendor, "unknown");
    }

    #[test]
    fn parse_cpuinfo_empty_is_empty() {
        assert!(parse_cpuinfo("").is_empty());
    }

    #[test]
    fn parse_memory_keeps_only_populated_slots() {
        let sample = "\
# dmidecode 3.3
Handle 0x0010, DMI type 17, 92 bytes
Memory Device
\tArray Handle: 0x000F
\tSize: 32 GB
\tLocator: DIMM_A0
\tType: DDR4
\tSpeed: 3200 MT/s
\tManufacturer: Samsung
\tSerial Number: 12345678
\tPart Number: M393A4K40DB3-CWE

Handle 0x0011, DMI type 17, 92 bytes
Memory Device
\tArray Handle: 0x000F
\tSize: No Module Installed
\tLocator: DIMM_A1
\tManufacturer: NO DIMM
\tSerial Number: NO DIMM

Handle 0x0012, DMI type 17, 92 bytes
Memory Device
\tArray Handle: 0x000F
\tSize: 32 GB
\tLocator: DIMM_B0
\tType: DDR4
\tSpeed: 3200 MT/s
\tManufacturer: Samsung
\tSerial Number: 87654321
\tPart Number: M393A4K40DB3-CWE
";
        let dimms = parse_dmidecode_memory(sample);
        assert_eq!(dimms.len(), 2, "only the two populated slots");

        let d0 = &dimms[0];
        assert_eq!(d0.kind, ComponentKind::MemoryModule);
        assert_eq!(d0.vendor, "Samsung");
        assert_eq!(d0.model, "M393A4K40DB3-CWE");
        assert_eq!(d0.serial, "12345678");
        assert_eq!(d0.attributes.get("size").map(String::as_str), Some("32 GB"));
        assert_eq!(
            d0.attributes.get("locator").map(String::as_str),
            Some("DIMM_A0")
        );
        assert_eq!(
            d0.attributes.get("speed").map(String::as_str),
            Some("3200 MT/s")
        );
        assert_eq!(d0.attributes.get("type").map(String::as_str), Some("DDR4"));

        assert_eq!(dimms[1].serial, "87654321");
    }

    #[test]
    fn parse_memory_falls_back_to_locator_for_serial() {
        // A populated DIMM whose serial firmware left as a placeholder.
        let sample = "\
Handle 0x0010, DMI type 17, 92 bytes
Memory Device
\tSize: 16 GB
\tLocator: DIMM_C1
\tManufacturer: Micron
\tSerial Number: Not Specified
\tPart Number: MTA18ASF
";
        let dimms = parse_dmidecode_memory(sample);
        assert_eq!(dimms.len(), 1);
        assert_eq!(dimms[0].serial, "DIMM-DIMM_C1");
    }

    #[test]
    fn parse_memory_empty_is_empty() {
        assert!(parse_dmidecode_memory("").is_empty());
        // A non-memory dmidecode dump produces nothing.
        assert!(parse_dmidecode_memory(
            "Handle 0x0001, DMI type 4, 48 bytes\nProcessor Information\n\tSocket: CPU0\n"
        )
        .is_empty());
    }

    #[test]
    fn clean_dmi_value_drops_placeholders() {
        assert_eq!(clean_dmi_value("  ABC123 \n"), Some("ABC123".to_string()));
        assert_eq!(clean_dmi_value(""), None);
        assert_eq!(clean_dmi_value("   "), None);
        assert_eq!(clean_dmi_value("To Be Filled By O.E.M."), None);
        assert_eq!(clean_dmi_value("Not Specified"), None);
        assert_eq!(clean_dmi_value("Default string"), None);
    }

    #[test]
    fn collect_nics_reads_macs_and_skips_loopback() {
        // Build a fake /sys/class/net under a temp dir.
        let root = std::env::temp_dir().join(format!("ocf-net-{}", uuid::Uuid::new_v4()));
        let net = root.join("net");
        for (iface, mac) in [
            ("lo", "00:00:00:00:00:00"),
            ("eth0", "0c:42:a1:00:00:01"),
            ("eth1", "00:00:00:00:00:00"),
        ] {
            let dir = net.join(iface);
            fs::create_dir_all(&dir).expect("mkdir");
            fs::write(dir.join("address"), format!("{mac}\n")).expect("write addr");
        }
        fs::write(net.join("eth0").join("speed"), "25000\n").expect("write speed");

        let nics = collect_nics(&net);
        let _ = fs::remove_dir_all(&root);

        // Only eth0: lo is skipped by name, eth1 by its all-zero MAC.
        assert_eq!(nics.len(), 1);
        assert_eq!(nics[0].kind, ComponentKind::Nic);
        assert_eq!(nics[0].serial, "0c:42:a1:00:00:01");
        assert_eq!(nics[0].model, "eth0");
        assert_eq!(
            nics[0].attributes.get("speed_mbps").map(String::as_str),
            Some("25000")
        );
    }

    #[test]
    fn collect_disks_reads_serial_and_size_and_skips_pseudo() {
        let root = std::env::temp_dir().join(format!("ocf-blk-{}", uuid::Uuid::new_v4()));
        let block = root.join("block");

        // A real NVMe-ish device.
        let nvme = block.join("nvme0n1");
        fs::create_dir_all(nvme.join("device")).expect("mkdir dev");
        fs::create_dir_all(nvme.join("queue")).expect("mkdir queue");
        fs::write(nvme.join("device").join("serial"), "SN-ABC123\n").expect("serial");
        fs::write(nvme.join("device").join("model"), "INTEL SSDPE2KE016T8\n").expect("model");
        fs::write(nvme.join("size"), "3125627568\n").expect("size");
        fs::write(nvme.join("queue").join("rotational"), "0\n").expect("rot");

        // Pseudo devices that must be skipped.
        for pseudo in ["loop0", "ram0", "dm-0"] {
            fs::create_dir_all(block.join(pseudo)).expect("mkdir pseudo");
        }

        let disks = collect_disks(&block);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(disks.len(), 1);
        let d = &disks[0];
        assert_eq!(d.kind, ComponentKind::Disk);
        assert_eq!(d.serial, "SN-ABC123");
        assert_eq!(d.model, "INTEL SSDPE2KE016T8");
        assert_eq!(d.attributes.get("device").map(String::as_str), Some("nvme0n1"));
        assert_eq!(
            d.attributes.get("size_bytes").map(String::as_str),
            Some((3125627568u64 * 512).to_string()).as_deref()
        );
        assert_eq!(d.attributes.get("media").map(String::as_str), Some("ssd"));
    }

    /// Exercises the real host. Ignored by default: requires Linux + privileges
    /// and produces host-dependent output.
    #[tokio::test]
    #[ignore = "requires a real Linux host with dmidecode/sysfs"]
    async fn collect_on_real_host() {
        let collector = DmiInventoryCollector::new();
        let inv = collector
            .collect(&Id::named("localhost"))
            .await
            .expect("collect");
        assert!(inv.component_count() > 0);
    }
}
