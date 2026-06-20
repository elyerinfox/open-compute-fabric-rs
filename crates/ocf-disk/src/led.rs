//! Drive-bay locator LED control.

use crate::model::{LedState, PhysicalDisk};
use ocf_core::prelude::*;
use tokio::process::Command;

/// Contract for driving a disk's enclosure locator/fault LED.
///
/// Wraps `ledctl` (from `ledmon`), which addresses drives by device path or by
/// enclosure/slot. Used by the "blink the bay so a tech can find the failed
/// drive" workflow and by array-rebuild status.
#[async_trait]
pub trait LedControl: Provider {
    /// Set `disk`'s locator/fault LED to `state`.
    async fn set_led(&self, disk: &PhysicalDisk, state: LedState) -> Result<()>;
}

/// [`LedControl`] backed by `ledctl` (from `ledmon`).
///
/// Issues `ledctl <verb>=/dev/<dev>` where `<verb>` is the IBPI pattern for the
/// requested [`LedState`].
pub struct LedctlControl;

impl LedctlControl {
    pub fn new() -> Self {
        LedctlControl
    }

    /// The `ledctl` IBPI pattern name corresponding to a [`LedState`].
    fn ibpi(state: LedState) -> &'static str {
        match state {
            LedState::Normal => "normal",
            LedState::Locate => "locate",
            LedState::Fault => "failure",
            LedState::Rebuild => "rebuild",
        }
    }
}

impl Default for LedctlControl {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for LedctlControl {
    fn name(&self) -> &str {
        "ledctl"
    }
    fn description(&self) -> &str {
        "Drives enclosure locator/fault LEDs via ledctl/ledmon."
    }
}

#[async_trait]
impl LedControl for LedctlControl {
    async fn set_led(&self, disk: &PhysicalDisk, state: LedState) -> Result<()> {
        // `ledctl` addresses the drive by its host device node. Without one we
        // cannot drive the LED (a drive behind an opaque RAID HBA has no node).
        if disk.dev_path.is_empty() {
            return Err(Error::invalid(format!(
                "disk serial `{}` has no device path to address its LED",
                disk.serial
            )));
        }

        // e.g. `ledctl locate=/dev/sda`, `ledctl failure=/dev/sda`.
        let pattern = Self::ibpi(state);
        let arg = format!("{pattern}={}", disk.dev_path);
        run("ledctl", &[&arg]).await?;
        tracing::info!(serial = %disk.serial, dev = %disk.dev_path, pattern, "set drive LED via ledctl");
        Ok(())
    }
}

/// Run `cmd args...`, discarding stdout on success.
///
/// Maps a missing binary or a non-zero exit onto [`Error::provider`].
async fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| Error::provider("ledctl", format!("failed to spawn `{cmd}`: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::provider(
            "ledctl",
            format!("`{cmd}` exited with {}: {}", output.status, stderr.trim()),
        ));
    }
    Ok(())
}

/// Register the built-in [`LedControl`] backends.
pub fn register_builtins(reg: &mut Registry<dyn LedControl>) -> Result<()> {
    reg.register("ledctl", std::sync::Arc::new(LedctlControl::new()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ibpi_maps_every_state() {
        assert_eq!(LedctlControl::ibpi(LedState::Normal), "normal");
        assert_eq!(LedctlControl::ibpi(LedState::Locate), "locate");
        assert_eq!(LedctlControl::ibpi(LedState::Fault), "failure");
        assert_eq!(LedctlControl::ibpi(LedState::Rebuild), "rebuild");
    }

    #[tokio::test]
    async fn set_led_without_dev_path_is_invalid() {
        let led = LedctlControl::new();
        let disk = PhysicalDisk::new(Id::named("m1"), "NO-PATH");
        // dev_path is empty by default.
        let err = led.set_led(&disk, LedState::Locate).await.unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
    }

    #[tokio::test]
    #[ignore = "requires real ledctl and an enclosure"]
    async fn real_set_led() {
        let led = LedctlControl::new();
        let mut disk = PhysicalDisk::new(Id::named("local"), "REAL");
        disk.dev_path = "/dev/sda".to_string();
        led.set_led(&disk, LedState::Locate).await.expect("set led");
    }
}
