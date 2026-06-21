//! Check: `br_netfilter` must be loaded and `bridge-nf-call-iptables` enabled so
//! that bridged (container/VM) traffic traverses the host firewall.

use crate::check::HealthCheck;
use crate::exec::{read_sys, run_fix, write_sys};
use crate::finding::{FixAction, HealthCategory, HealthFinding, Severity};
use ocf_core::prelude::*;

const PATH: &str = "/proc/sys/net/bridge/bridge-nf-call-iptables";
const FIX_ID: &str = "enable-bridge-nf";

/// Warns when bridged traffic bypasses the firewall — either `br_netfilter` is
/// not loaded (the sysctl is absent) or `bridge-nf-call-iptables` is `0`.
#[derive(Debug, Default)]
pub struct BridgeNetfilterCheck;

impl BridgeNetfilterCheck {
    pub fn new() -> Self {
        BridgeNetfilterCheck
    }
}

impl Provider for BridgeNetfilterCheck {
    fn name(&self) -> &str {
        "bridge-netfilter"
    }
    fn description(&self) -> &str {
        "Bridged traffic traverses the host firewall (br_netfilter)"
    }
}

#[async_trait]
impl HealthCheck for BridgeNetfilterCheck {
    fn category(&self) -> HealthCategory {
        HealthCategory::Network
    }

    async fn check(&self, machine_id: &Id) -> Result<Vec<HealthFinding>> {
        // If the sysctl exists and is "1", we're good. If it is "0", warn. If the
        // path is absent we can only flag it on Linux (where /proc exists); on a
        // non-Linux host /proc itself is absent, so we treat that as can't-assess.
        if read_sys("/proc/sys").ok().flatten().is_none() && read_sys("/proc/modules")?.is_none() {
            return Ok(vec![]);
        }
        let value = read_sys(PATH)?;
        if !bridge_nf_needs_fix(value.as_deref()) {
            return Ok(vec![]);
        }
        Ok(vec![HealthFinding::new(
            self.name(),
            "disabled",
            machine_id,
            HealthCategory::Network,
            Severity::Warning,
            "Bridge netfilter not calling iptables",
            "br_netfilter is not loaded or bridge-nf-call-iptables is 0, so \
             traffic across Linux bridges (containers/VMs) bypasses the host \
             firewall and NAT rules.",
        )
        .with_fix(FixAction::new(
            FIX_ID,
            "Enable bridge netfilter",
            "Runs `modprobe br_netfilter` and sets bridge-nf-call-iptables=1.",
        ))])
    }

    async fn apply_fix(&self, fix_id: &str, _machine_id: &Id) -> Result<String> {
        if fix_id != FIX_ID {
            return Err(Error::not_found(format!("fix `{fix_id}`")));
        }
        run_fix("modprobe", &["br_netfilter"]).await?;
        write_sys(PATH, "1")?;
        tracing::info!("enabled bridge netfilter");
        Ok("Loaded br_netfilter and set bridge-nf-call-iptables = 1.".to_string())
    }
}

/// Whether the bridge-nf sysctl value warrants a fix: a present `"1"` is fine;
/// `"0"` (disabled) or absent (module not loaded) both need remediation.
fn bridge_nf_needs_fix(value: Option<&str>) -> bool {
    !matches!(value, Some("1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_value_needs_fix() {
        assert!(!bridge_nf_needs_fix(Some("1")));
        assert!(bridge_nf_needs_fix(Some("0")));
        assert!(bridge_nf_needs_fix(None));
    }
}
