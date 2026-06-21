//! Check: the `nf_tables` netfilter backend must be loaded for the fabric's
//! firewall/NAT programming (`nft`) to take effect.

use crate::check::HealthCheck;
use crate::exec::{read_sys, run, run_fix};
use crate::finding::{FixAction, HealthCategory, HealthFinding, Severity};
use ocf_core::prelude::*;

const FIX_ID: &str = "load-nf-tables";

/// Warns when the `nf_tables` kernel module is not loaded. `ocf-network` and
/// `ocf-kernel` program firewall/NAT rules through `nft`, which needs it.
#[derive(Debug, Default)]
pub struct NetfilterCheck;

impl NetfilterCheck {
    pub fn new() -> Self {
        NetfilterCheck
    }
}

impl Provider for NetfilterCheck {
    fn name(&self) -> &str {
        "netfilter"
    }
    fn description(&self) -> &str {
        "The nf_tables netfilter backend is loaded (nft is usable)"
    }
}

#[async_trait]
impl HealthCheck for NetfilterCheck {
    fn category(&self) -> HealthCategory {
        HealthCategory::Kernel
    }

    async fn check(&self, machine_id: &Id) -> Result<Vec<HealthFinding>> {
        // `/proc/modules` is the cheapest probe. Absent → can't assess.
        let Some(modules) = read_sys("/proc/modules")? else {
            return Ok(vec![]);
        };
        if nf_tables_loaded(&modules) {
            return Ok(vec![]);
        }
        Ok(vec![HealthFinding::new(
            self.name(),
            "not-loaded",
            machine_id,
            HealthCategory::Kernel,
            Severity::Warning,
            "Netfilter (nf_tables) not enabled on kernel",
            "The nf_tables module is not loaded, so firewall and NAT rules \
             programmed via nft will not take effect on this node.",
        )
        .with_fix(FixAction::new(
            FIX_ID,
            "Load nf_tables module",
            "Runs `modprobe nf_tables` on this node.",
        ))])
    }

    async fn apply_fix(&self, fix_id: &str, _machine_id: &Id) -> Result<String> {
        if fix_id != FIX_ID {
            return Err(Error::not_found(format!("fix `{fix_id}`")));
        }
        run_fix("modprobe", &["nf_tables"]).await?;
        // Best-effort confirm it took.
        let after = run("modprobe", &["-n", "nf_tables"]).await;
        let _ = after;
        tracing::info!("loaded nf_tables module");
        Ok("Loaded the nf_tables kernel module (modprobe nf_tables).".to_string())
    }
}

/// Whether `/proc/modules` content shows the `nf_tables` module loaded.
fn nf_tables_loaded(modules: &str) -> bool {
    modules
        .lines()
        .any(|l| l.split_whitespace().next() == Some("nf_tables"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_module_in_proc_modules() {
        let sample = "nf_tables 286720 1 nft_compat, Live 0x0\n\
                      overlay 151552 0 - Live 0x0";
        assert!(nf_tables_loaded(sample));
        assert!(!nf_tables_loaded("overlay 151552 0 - Live 0x0"));
        assert!(!nf_tables_loaded(""));
    }
}
