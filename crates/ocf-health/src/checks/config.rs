//! Configuration health checks: declarative, reusable checks for common host
//! misconfigurations (sysctls, services, swap, time sync).
//!
//! The point of this module is that adding a configuration warning is *data*,
//! not code: register a [`SysctlCheck`] or [`ServiceCheck`] with the expected
//! value and you get the finding **and** a fix button, no new type required.

use crate::check::HealthCheck;
use crate::exec::{read_sys, run, run_fix, write_sys};
use crate::finding::{FixAction, HealthCategory, HealthFinding, Severity};
use ocf_core::prelude::*;

// ===========================================================================
// SysctlCheck — "this kernel tunable should be <value>" (declarative)
// ===========================================================================

/// The expected-vs-actual rule for a sysctl value.
#[derive(Debug, Clone)]
pub enum SysctlRule {
    /// The value must equal this string exactly (e.g. a flag `"1"`).
    Equals(String),
    /// The numeric value must be at least this (e.g. a table-size minimum).
    AtLeast(i64),
}

/// Whether `current` satisfies `rule`. Pure and unit-tested.
fn sysctl_satisfied(rule: &SysctlRule, current: &str) -> bool {
    match rule {
        SysctlRule::Equals(v) => current == v,
        SysctlRule::AtLeast(min) => current.parse::<i64>().map(|n| n >= *min).unwrap_or(false),
    }
}

/// A declarative check that a `/proc/sys` tunable holds an expected value, with
/// a fix that writes the desired value.
pub struct SysctlCheck {
    name: String,
    path: String,
    rule: SysctlRule,
    fix_value: String,
    category: HealthCategory,
    severity: Severity,
    title: String,
    detail: String,
}

impl SysctlCheck {
    /// A flag-style check: the sysctl should equal `value`; the fix writes it.
    pub fn equals(
        name: impl Into<String>,
        path: impl Into<String>,
        value: impl Into<String>,
        category: HealthCategory,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        let value = value.into();
        SysctlCheck {
            name: name.into(),
            path: path.into(),
            rule: SysctlRule::Equals(value.clone()),
            fix_value: value,
            category,
            severity: Severity::Warning,
            title: title.into(),
            detail: detail.into(),
        }
    }

    /// A minimum-value check: the sysctl should be `>= min`; the fix raises it.
    pub fn at_least(
        name: impl Into<String>,
        path: impl Into<String>,
        min: i64,
        category: HealthCategory,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        SysctlCheck {
            name: name.into(),
            path: path.into(),
            rule: SysctlRule::AtLeast(min),
            fix_value: min.to_string(),
            category,
            severity: Severity::Warning,
            title: title.into(),
            detail: detail.into(),
        }
    }
}

impl Provider for SysctlCheck {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "Kernel tunable holds its expected value"
    }
}

#[async_trait]
impl HealthCheck for SysctlCheck {
    fn category(&self) -> HealthCategory {
        self.category
    }

    async fn check(&self, machine_id: &Id) -> Result<Vec<HealthFinding>> {
        let Some(current) = read_sys(&self.path)? else {
            return Ok(vec![]); // sysctl absent → can't assess
        };
        if sysctl_satisfied(&self.rule, &current) {
            return Ok(vec![]);
        }
        Ok(vec![HealthFinding::new(
            self.name(),
            "misconfigured",
            machine_id,
            self.category,
            self.severity,
            self.title.clone(),
            format!("{} (currently `{current}`, expected `{}`)", self.detail, self.fix_value),
        )
        .with_fix(FixAction::new(
            "set",
            "Apply recommended value",
            format!("Writes `{}` to {}.", self.fix_value, self.path),
        ))])
    }

    async fn apply_fix(&self, fix_id: &str, _machine_id: &Id) -> Result<String> {
        if fix_id != "set" {
            return Err(Error::not_found(format!("fix `{fix_id}`")));
        }
        write_sys(&self.path, &self.fix_value)?;
        Ok(format!("Set {} = {}.", self.path, self.fix_value))
    }
}

// ===========================================================================
// ServiceCheck — "this installed service should be running (and enabled)"
// ===========================================================================

/// A check that a systemd unit which is **installed** is active (and optionally
/// enabled at boot). It stays silent when the unit isn't installed at all — it
/// is about misconfiguration, not absence.
pub struct ServiceCheck {
    name: String,
    unit: String,
    want_enabled: bool,
    severity: Severity,
}

impl ServiceCheck {
    pub fn new(name: impl Into<String>, unit: impl Into<String>, want_enabled: bool) -> Self {
        ServiceCheck {
            name: name.into(),
            unit: unit.into(),
            want_enabled,
            severity: Severity::Warning,
        }
    }
}

impl Provider for ServiceCheck {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "An installed service is running and enabled"
    }
}

#[async_trait]
impl HealthCheck for ServiceCheck {
    fn category(&self) -> HealthCategory {
        HealthCategory::Runtime
    }

    async fn check(&self, machine_id: &Id) -> Result<Vec<HealthFinding>> {
        // `is-enabled` both tells us whether the unit exists and its boot state.
        let enabled = run("systemctl", &["is-enabled", &self.unit]).await;
        if !enabled.ran {
            return Ok(vec![]); // no systemctl → can't assess
        }
        let combined = format!("{}{}", enabled.stdout, enabled.stderr).to_lowercase();
        if combined.contains("no such file") || combined.contains("not found") {
            return Ok(vec![]); // unit not installed → not our concern
        }
        let is_enabled = enabled.stdout.trim() == "enabled";

        let active = run("systemctl", &["is-active", &self.unit]).await;
        let is_active = active.stdout.trim() == "active";

        if is_active && (!self.want_enabled || is_enabled) {
            return Ok(vec![]);
        }

        let (title, detail) = if !is_active {
            (
                format!("Service `{}` is installed but not running", self.unit),
                format!(
                    "The `{}` unit exists but is not active. Features depending on it \
                     will not work until it is started.",
                    self.unit
                ),
            )
        } else {
            (
                format!("Service `{}` is not enabled at boot", self.unit),
                format!(
                    "The `{}` unit is running now but is not enabled, so it will not \
                     come back after a reboot.",
                    self.unit
                ),
            )
        };

        Ok(vec![HealthFinding::new(
            self.name(),
            "misconfigured",
            machine_id,
            HealthCategory::Runtime,
            self.severity,
            title,
            detail,
        )
        .with_fix(FixAction::new(
            "enable-now",
            format!("Start & enable {}", self.unit),
            format!("Runs `systemctl enable --now {}`.", self.unit),
        ))])
    }

    async fn apply_fix(&self, fix_id: &str, _machine_id: &Id) -> Result<String> {
        if fix_id != "enable-now" {
            return Err(Error::not_found(format!("fix `{fix_id}`")));
        }
        run_fix("systemctl", &["enable", "--now", &self.unit]).await?;
        Ok(format!("Started and enabled `{}`.", self.unit))
    }
}

// ===========================================================================
// SwapCheck — swap should be off for predictable scheduling
// ===========================================================================

/// Warns when swap is enabled (unpredictable scheduling / latency for compute
/// nodes), with a fix that runs `swapoff -a`.
#[derive(Debug, Default)]
pub struct SwapCheck;

impl SwapCheck {
    pub fn new() -> Self {
        SwapCheck
    }
}

/// Whether `/proc/swaps` content shows an active swap area (any line past the
/// header). Pure and unit-tested.
fn swap_active(proc_swaps: &str) -> bool {
    proc_swaps.lines().skip(1).any(|l| !l.trim().is_empty())
}

impl Provider for SwapCheck {
    fn name(&self) -> &str {
        "swap-disabled"
    }
    fn description(&self) -> &str {
        "Swap is disabled (predictable scheduling)"
    }
}

#[async_trait]
impl HealthCheck for SwapCheck {
    fn category(&self) -> HealthCategory {
        HealthCategory::Kernel
    }

    async fn check(&self, machine_id: &Id) -> Result<Vec<HealthFinding>> {
        let Some(swaps) = read_sys("/proc/swaps")? else {
            return Ok(vec![]);
        };
        if !swap_active(&swaps) {
            return Ok(vec![]);
        }
        Ok(vec![HealthFinding::new(
            self.name(),
            "enabled",
            machine_id,
            HealthCategory::Kernel,
            Severity::Info,
            "Swap is enabled",
            "Active swap can cause unpredictable scheduling and latency for \
             compute workloads. Disabling it gives more deterministic behavior.",
        )
        .with_fix(FixAction::new(
            "disable",
            "Disable swap",
            "Runs `swapoff -a` on this node (does not edit /etc/fstab).",
        ))])
    }

    async fn apply_fix(&self, fix_id: &str, _machine_id: &Id) -> Result<String> {
        if fix_id != "disable" {
            return Err(Error::not_found(format!("fix `{fix_id}`")));
        }
        run_fix("swapoff", &["-a"]).await?;
        Ok("Disabled all swap (swapoff -a). Edit /etc/fstab to persist.".to_string())
    }
}

// ===========================================================================
// TimeSyncCheck — a time-sync service should be active (Raft/TLS need it)
// ===========================================================================

/// Candidate time-sync units, in preference order.
const TIMESYNC_UNITS: &[&str] = &["chronyd", "chrony", "systemd-timesyncd", "ntpd", "ntp"];

/// Warns when no time-synchronization service is active. Clock skew breaks Raft
/// elections and TLS validation, so this matters fleet-wide.
#[derive(Debug, Default)]
pub struct TimeSyncCheck;

impl TimeSyncCheck {
    pub fn new() -> Self {
        TimeSyncCheck
    }
}

impl Provider for TimeSyncCheck {
    fn name(&self) -> &str {
        "time-sync"
    }
    fn description(&self) -> &str {
        "A time-synchronization service is active"
    }
}

#[async_trait]
impl HealthCheck for TimeSyncCheck {
    fn category(&self) -> HealthCategory {
        HealthCategory::Other
    }

    async fn check(&self, machine_id: &Id) -> Result<Vec<HealthFinding>> {
        let mut systemctl_present = false;
        for unit in TIMESYNC_UNITS {
            let out = run("systemctl", &["is-active", unit]).await;
            if !out.ran {
                return Ok(vec![]); // no systemctl → can't assess
            }
            systemctl_present = true;
            if out.stdout.trim() == "active" {
                return Ok(vec![]); // something is keeping time
            }
        }
        if !systemctl_present {
            return Ok(vec![]);
        }
        Ok(vec![HealthFinding::new(
            self.name(),
            "absent",
            machine_id,
            HealthCategory::Other,
            Severity::Warning,
            "No time synchronization service active",
            "No chrony/systemd-timesyncd/ntp service is running. Clock skew \
             breaks Raft leader elections and TLS certificate validation across \
             the fleet.",
        )
        .with_fix(FixAction::new(
            "enable-timesyncd",
            "Enable time sync",
            "Runs `systemctl enable --now systemd-timesyncd`.",
        ))])
    }

    async fn apply_fix(&self, fix_id: &str, _machine_id: &Id) -> Result<String> {
        if fix_id != "enable-timesyncd" {
            return Err(Error::not_found(format!("fix `{fix_id}`")));
        }
        run_fix("systemctl", &["enable", "--now", "systemd-timesyncd"]).await?;
        Ok("Enabled systemd-timesyncd for clock synchronization.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysctl_equals_and_at_least() {
        assert!(sysctl_satisfied(&SysctlRule::Equals("1".into()), "1"));
        assert!(!sysctl_satisfied(&SysctlRule::Equals("1".into()), "0"));
        assert!(sysctl_satisfied(&SysctlRule::AtLeast(512), "1024"));
        assert!(sysctl_satisfied(&SysctlRule::AtLeast(512), "512"));
        assert!(!sysctl_satisfied(&SysctlRule::AtLeast(512), "128"));
        assert!(!sysctl_satisfied(&SysctlRule::AtLeast(512), "garbage"));
    }

    #[test]
    fn swap_detection() {
        let none = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n";
        assert!(!swap_active(none));
        let on = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
                  /dev/sda2\t\t\t\tpartition\t8388604\t\t0\t\t-2\n";
        assert!(swap_active(on));
    }

    #[tokio::test]
    async fn checks_reject_unknown_fixes() {
        let s = SysctlCheck::equals(
            "x",
            "/proc/sys/none",
            "1",
            HealthCategory::Kernel,
            "t",
            "d",
        );
        assert!(s.apply_fix("nope", &Id::named("m")).await.is_err());
        assert!(SwapCheck::new().apply_fix("nope", &Id::named("m")).await.is_err());
        assert!(ServiceCheck::new("svc", "docker", true)
            .apply_fix("nope", &Id::named("m"))
            .await
            .is_err());
        assert!(TimeSyncCheck::new().apply_fix("nope", &Id::named("m")).await.is_err());
    }
}
