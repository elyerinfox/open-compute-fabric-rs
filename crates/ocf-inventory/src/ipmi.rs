//! Out-of-band power and sensor control over IPMI.
//!
//! IPMI talks to a machine's Baseboard Management Controller (BMC), which is a
//! tiny always-on computer wired to the chassis power rails and sensor bus. It
//! lets the controller power a machine on/off and read temperatures/voltages
//! even when the host OS is dead.
//!
//! **Network requirement.** A BMC is reachable only over the out-of-band
//! management LAN it sits on. There is no routing/overlay involved: the caller
//! must already be on the *same physical network* (the management VLAN) as the
//! target's BMC for any of these operations to work. The fabric does not — and
//! cannot — tunnel IPMI for you. See the `LanplusIpmi` docs.

use crate::exec;
use ocf_core::prelude::*;

/// Connection parameters for a single machine's BMC.
///
/// `channel` selects the BMC LAN channel (commonly `1`). Credentials are kept
/// as plain fields here; a real deployment would source them from a secret
/// store rather than inline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpmiTarget {
    /// The BMC's address on the management network.
    pub address: String,
    pub username: String,
    pub password: String,
    /// IPMI LAN channel number (typically `1`).
    pub channel: u8,
}

impl IpmiTarget {
    pub fn new(
        address: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        IpmiTarget {
            address: address.into(),
            username: username.into(),
            password: password.into(),
            channel: 1,
        }
    }

    /// The leading `ipmitool` arguments shared by every command against this
    /// target: lanplus transport, host, credentials. Returned owned so the
    /// caller can append the verb (e.g. `chassis power status`).
    fn base_args(&self) -> Vec<String> {
        vec![
            "-I".to_string(),
            "lanplus".to_string(),
            "-H".to_string(),
            self.address.clone(),
            "-U".to_string(),
            self.username.clone(),
            "-P".to_string(),
            self.password.clone(),
        ]
    }
}

/// Chassis power state as reported by the BMC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerState {
    On,
    Off,
    /// State could not be determined (BMC unreachable / unknown response).
    Unknown,
}

/// A single sensor reading from the BMC's Sensor Data Repository (SDR).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sensor {
    /// Sensor name, e.g. `"CPU0 Temp"`, `"PSU1 Voltage"`, `"Fan1"`.
    pub name: String,
    pub value: f64,
    /// Engineering unit, e.g. `"degrees C"`, `"Volts"`, `"RPM"`.
    pub unit: String,
    /// BMC-reported health for this sensor (within thresholds or not).
    pub health: Health,
}

impl Sensor {
    pub fn new(name: impl Into<String>, value: f64, unit: impl Into<String>, health: Health) -> Self {
        Sensor {
            name: name.into(),
            value,
            unit: unit.into(),
            health,
        }
    }
}

/// Out-of-band power and sensor control for one machine's BMC.
///
/// Extends [`Provider`] so transports (lanplus, redfish, ...) are named and
/// swappable. All methods take an [`IpmiTarget`] so a single controller can
/// serve many machines.
#[async_trait]
pub trait IpmiController: Provider {
    async fn power_status(&self, target: &IpmiTarget) -> Result<PowerState>;
    async fn power_on(&self, target: &IpmiTarget) -> Result<()>;
    async fn power_off(&self, target: &IpmiTarget) -> Result<()>;
    async fn power_cycle(&self, target: &IpmiTarget) -> Result<()>;
    /// Read the BMC's current sensor values (temperatures, voltages, fans).
    async fn sensors(&self, target: &IpmiTarget) -> Result<Vec<Sensor>>;
}

/// IPMI 2.0 controller using the `lanplus` interface (RMCP+ over UDP/623).
///
/// This drives the real `ipmitool -I lanplus -H <addr> -U <user> -P <pass> ...`
/// binary against the target BMC and parses its output. `ipmitool` must be
/// installed on the host running the fabric.
///
/// **Same-network requirement.** RMCP+ is unrouted management-LAN traffic: this
/// only works when the controller host is on the same physical/management
/// network as the target BMC. The fabric does not tunnel it. If `ipmitool` is
/// missing or the BMC is unreachable, calls return an [`Error::Provider`] tagged
/// `ipmitool`.
pub struct LanplusIpmi;

impl LanplusIpmi {
    pub fn new() -> Self {
        LanplusIpmi
    }

    /// Run `ipmitool <base args> <verb...>` against `target`, returning stdout.
    async fn ipmitool(&self, target: &IpmiTarget, verb: &[&str]) -> Result<String> {
        let mut owned = target.base_args();
        owned.extend(verb.iter().map(|s| s.to_string()));
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        exec::run("ipmitool", &args).await
    }
}

impl Default for LanplusIpmi {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for LanplusIpmi {
    fn name(&self) -> &str {
        "lanplus"
    }
    fn description(&self) -> &str {
        "IPMI 2.0 lanplus (RMCP+) power/sensor control via ipmitool"
    }
}

#[async_trait]
impl IpmiController for LanplusIpmi {
    async fn power_status(&self, target: &IpmiTarget) -> Result<PowerState> {
        // `ipmitool ... chassis power status` -> "Chassis Power is on".
        let out = self
            .ipmitool(target, &["chassis", "power", "status"])
            .await?;
        Ok(parse_power_status(&out))
    }

    async fn power_on(&self, target: &IpmiTarget) -> Result<()> {
        self.ipmitool(target, &["chassis", "power", "on"]).await?;
        Ok(())
    }

    async fn power_off(&self, target: &IpmiTarget) -> Result<()> {
        // Hard power-off. Use `soft` for an ACPI graceful shutdown instead.
        self.ipmitool(target, &["chassis", "power", "off"]).await?;
        Ok(())
    }

    async fn power_cycle(&self, target: &IpmiTarget) -> Result<()> {
        self.ipmitool(target, &["chassis", "power", "cycle"]).await?;
        Ok(())
    }

    async fn sensors(&self, target: &IpmiTarget) -> Result<Vec<Sensor>> {
        // `ipmitool ... sdr` emits one `name | value unit | status` row per
        // sensor in its default, pipe-delimited list format.
        let out = self.ipmitool(target, &["sdr"]).await?;
        Ok(parse_sdr(&out))
    }
}

/// Parse `ipmitool chassis power status` output into a [`PowerState`].
///
/// The canonical line is `Chassis Power is on` / `... is off`; we match case-
/// insensitively and tolerate surrounding noise, defaulting to `Unknown`.
pub fn parse_power_status(output: &str) -> PowerState {
    let lower = output.to_ascii_lowercase();
    if lower.contains("power is on") {
        PowerState::On
    } else if lower.contains("power is off") {
        PowerState::Off
    } else {
        PowerState::Unknown
    }
}

/// Parse the default `ipmitool sdr` list format into [`Sensor`] records.
///
/// Each populated line looks like:
///
/// ```text
/// CPU0 Temp        | 48 degrees C      | ok
/// PSU1 Voltage     | 12.10 Volts       | ok
/// Fan1             | 6200 RPM          | ok
/// FAN3             | no reading        | ns
/// ```
///
/// Three pipe-delimited fields: name, "value unit", and status. We split the
/// value field into a leading number and a trailing unit, map the IPMI status
/// token to [`Health`], and skip rows with no numeric reading (`no reading`,
/// `disabled`, etc.).
pub fn parse_sdr(output: &str) -> Vec<Sensor> {
    let mut sensors = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('|').map(str::trim).collect();
        if fields.len() < 3 {
            continue;
        }
        let name = fields[0];
        let reading = fields[1];
        let status = fields[2];
        if name.is_empty() {
            continue;
        }

        let Some((value, unit)) = split_reading(reading) else {
            // No numeric reading (e.g. "no reading", "disabled") — skip.
            continue;
        };

        sensors.push(Sensor::new(name, value, unit, status_to_health(status)));
    }
    sensors
}

/// Split a `"<number> <unit>"` reading into its numeric value and unit string.
/// Returns `None` when the leading token isn't a number.
fn split_reading(reading: &str) -> Option<(f64, String)> {
    let reading = reading.trim();
    let mut parts = reading.splitn(2, char::is_whitespace);
    let num = parts.next()?.trim();
    let value: f64 = num.parse().ok()?;
    let unit = parts.next().unwrap_or("").trim().to_string();
    Some((value, unit))
}

/// Map an `ipmitool` SDR status token to a coarse [`Health`].
///
/// `ok` is healthy; threshold breaches (`nc` non-critical, `cr` critical,
/// `nr` non-recoverable, or any `*c`/`*r` flag word) are degraded/unhealthy;
/// `ns` (no state) and anything unrecognized is unknown.
fn status_to_health(status: &str) -> Health {
    match status.to_ascii_lowercase().as_str() {
        "ok" => Health::Healthy,
        "nc" => Health::Degraded,
        "cr" | "nr" => Health::Unhealthy,
        "ns" | "" => Health::Unknown,
        other => {
            // Some firmwares emit longer words ("lnc", "unc", "lcr", "ucr",
            // "lnr", "unr"). Classify by the trailing severity letter.
            if other.ends_with("nc") {
                Health::Degraded
            } else if other.ends_with("cr") || other.ends_with("nr") {
                Health::Unhealthy
            } else {
                Health::Unknown
            }
        }
    }
}

/// Register the default IPMI controllers.
pub fn register_builtins(reg: &mut Registry<dyn IpmiController>) -> Result<()> {
    reg.register("lanplus", std::sync::Arc::new(LanplusIpmi::new()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_status_parses_on_off_unknown() {
        assert_eq!(parse_power_status("Chassis Power is on\n"), PowerState::On);
        assert_eq!(parse_power_status("Chassis Power is off\n"), PowerState::Off);
        // Case-insensitive and tolerant of surrounding text.
        assert_eq!(
            parse_power_status("  CHASSIS POWER IS ON  "),
            PowerState::On
        );
        assert_eq!(
            parse_power_status("Error: Unable to establish IPMI v2"),
            PowerState::Unknown
        );
        assert_eq!(parse_power_status(""), PowerState::Unknown);
    }

    #[test]
    fn sdr_parses_temperatures_voltages_and_fans() {
        let sample = "\
CPU0 Temp        | 48 degrees C      | ok
CPU1 Temp        | 51 degrees C      | ok
Inlet Temp       | 24 degrees C      | ok
PSU1 Voltage     | 12.10 Volts       | ok
Fan1             | 6200 RPM          | ok
Fan2             | 6150 RPM          | nc
FAN3             | no reading        | ns
DIMM Status      | 0x00              | ok
";
        let sensors = parse_sdr(sample);
        // "no reading" is skipped; the 0x00 hex value is non-numeric -> skipped.
        assert_eq!(sensors.len(), 6);

        let cpu0 = &sensors[0];
        assert_eq!(cpu0.name, "CPU0 Temp");
        assert_eq!(cpu0.value, 48.0);
        assert_eq!(cpu0.unit, "degrees C");
        assert_eq!(cpu0.health, Health::Healthy);

        let psu = sensors.iter().find(|s| s.name == "PSU1 Voltage").unwrap();
        assert_eq!(psu.value, 12.10);
        assert_eq!(psu.unit, "Volts");

        let fan1 = sensors.iter().find(|s| s.name == "Fan1").unwrap();
        assert_eq!(fan1.value, 6200.0);
        assert_eq!(fan1.unit, "RPM");

        let fan2 = sensors.iter().find(|s| s.name == "Fan2").unwrap();
        assert_eq!(fan2.health, Health::Degraded);
    }

    #[test]
    fn sdr_ignores_blank_and_malformed_lines() {
        let sample = "\ngarbage without pipes\nOnly One | Field\nValid Temp | 30 degrees C | ok\n";
        let sensors = parse_sdr(sample);
        assert_eq!(sensors.len(), 1);
        assert_eq!(sensors[0].name, "Valid Temp");
        assert_eq!(sensors[0].value, 30.0);
    }

    #[test]
    fn status_token_maps_to_health() {
        assert_eq!(status_to_health("ok"), Health::Healthy);
        assert_eq!(status_to_health("nc"), Health::Degraded);
        assert_eq!(status_to_health("cr"), Health::Unhealthy);
        assert_eq!(status_to_health("nr"), Health::Unhealthy);
        assert_eq!(status_to_health("ns"), Health::Unknown);
        // Longer firmware flags classified by severity suffix.
        assert_eq!(status_to_health("unc"), Health::Degraded);
        assert_eq!(status_to_health("lcr"), Health::Unhealthy);
        assert_eq!(status_to_health("lnr"), Health::Unhealthy);
        assert_eq!(status_to_health("whatever"), Health::Unknown);
    }

    #[test]
    fn split_reading_handles_units_and_non_numbers() {
        assert_eq!(split_reading("48 degrees C"), Some((48.0, "degrees C".to_string())));
        assert_eq!(split_reading("12.10 Volts"), Some((12.10, "Volts".to_string())));
        assert_eq!(split_reading("6200 RPM"), Some((6200.0, "RPM".to_string())));
        // Unitless number.
        assert_eq!(split_reading("100"), Some((100.0, "".to_string())));
        assert_eq!(split_reading("no reading"), None);
        assert_eq!(split_reading("0x00"), None);
    }

    #[test]
    fn base_args_builds_lanplus_invocation() {
        let target = IpmiTarget::new("10.0.0.1", "ADMIN", "secret");
        assert_eq!(
            target.base_args(),
            vec!["-I", "lanplus", "-H", "10.0.0.1", "-U", "ADMIN", "-P", "secret"]
        );
    }

    /// Exercises a real BMC. Ignored by default: requires `ipmitool`, a reachable
    /// BMC on the management network, and real credentials supplied via env.
    #[tokio::test]
    #[ignore = "requires ipmitool and a reachable BMC on the management LAN"]
    async fn power_status_on_real_bmc() {
        let addr = std::env::var("OCF_TEST_BMC_ADDR").expect("OCF_TEST_BMC_ADDR");
        let user = std::env::var("OCF_TEST_BMC_USER").expect("OCF_TEST_BMC_USER");
        let pass = std::env::var("OCF_TEST_BMC_PASS").expect("OCF_TEST_BMC_PASS");
        let ipmi = LanplusIpmi::new();
        let target = IpmiTarget::new(addr, user, pass);
        let state = ipmi.power_status(&target).await.expect("status");
        assert_ne!(state, PowerState::Unknown);
    }
}
