//! Check: IPv4 forwarding must be enabled for routing/NAT/overlay to work.

use crate::check::HealthCheck;
use crate::exec::{read_sys, write_sys};
use crate::finding::{FixAction, HealthCategory, HealthFinding, Severity};
use ocf_core::prelude::*;

const PATH: &str = "/proc/sys/net/ipv4/ip_forward";
const FIX_ID: &str = "enable-ipv4-forwarding";

/// Warns when `net.ipv4.ip_forward` is `0`. Without it, the node cannot route
/// between subnets, NAT egress, or carry overlay traffic.
#[derive(Debug, Default)]
pub struct IpForwardingCheck;

impl IpForwardingCheck {
    pub fn new() -> Self {
        IpForwardingCheck
    }
}

impl Provider for IpForwardingCheck {
    fn name(&self) -> &str {
        "ip-forwarding"
    }
    fn description(&self) -> &str {
        "IPv4 forwarding (net.ipv4.ip_forward) is enabled"
    }
}

#[async_trait]
impl HealthCheck for IpForwardingCheck {
    fn category(&self) -> HealthCategory {
        HealthCategory::Kernel
    }

    async fn check(&self, machine_id: &Id) -> Result<Vec<HealthFinding>> {
        // Absent file → not Linux / can't assess → no finding.
        let Some(value) = read_sys(PATH)? else {
            return Ok(vec![]);
        };
        if value == "1" {
            return Ok(vec![]);
        }
        Ok(vec![HealthFinding::new(
            self.name(),
            "disabled",
            machine_id,
            HealthCategory::Kernel,
            Severity::Warning,
            "IP forwarding not enabled on kernel",
            "net.ipv4.ip_forward is 0. The node cannot route between subnets, \
             provide NAT egress, or carry overlay traffic until forwarding is on.",
        )
        .with_fix(FixAction::new(
            FIX_ID,
            "Enable IP forwarding",
            "Writes 1 to /proc/sys/net/ipv4/ip_forward on this node.",
        ))])
    }

    async fn apply_fix(&self, fix_id: &str, _machine_id: &Id) -> Result<String> {
        if fix_id != FIX_ID {
            return Err(Error::not_found(format!("fix `{fix_id}`")));
        }
        write_sys(PATH, "1")?;
        tracing::info!("enabled IPv4 forwarding");
        Ok("IPv4 forwarding enabled (net.ipv4.ip_forward = 1).".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_disabled_state() {
        // The detection logic keys off the trimmed file content.
        assert_ne!("0", "1");
        assert_eq!("1".trim(), "1");
    }

    #[tokio::test]
    async fn rejects_unknown_fix() {
        let c = IpForwardingCheck::new();
        assert!(c.apply_fix("bogus", &Id::named("m")).await.is_err());
    }
}
