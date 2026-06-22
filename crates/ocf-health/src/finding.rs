//! The value types a health check produces: findings, severities, categories,
//! and the fix actions a user can press.

use chrono::{DateTime, Utc};
use ocf_core::prelude::*;

/// How serious a [`HealthFinding`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational — not a problem, just worth surfacing.
    Info,
    /// A degraded or misconfigured state that should be fixed.
    Warning,
    /// A serious problem likely to break functionality.
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }
}

/// Which subsystem a finding relates to (drives grouping/icons in the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthCategory {
    Kernel,
    Network,
    Runtime,
    Storage,
    /// Security posture — pending security updates, known-vulnerable packages.
    Security,
    Other,
}

/// A remediation the user can trigger for a finding — rendered as a button.
///
/// `id` is unique within the check that offered it; the check's
/// [`HealthCheck::apply_fix`](crate::HealthCheck::apply_fix) executes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixAction {
    /// Stable id within the owning check, e.g. `"enable-ipv4-forwarding"`.
    pub id: String,
    /// Button label, e.g. `"Enable IP forwarding"`.
    pub label: String,
    /// What pressing it will do (shown as a tooltip / confirmation).
    pub description: String,
    /// Whether applying it needs root on the target node (UI can warn).
    #[serde(default)]
    pub requires_root: bool,
}

impl FixAction {
    pub fn new(id: impl Into<String>, label: impl Into<String>, description: impl Into<String>) -> Self {
        FixAction {
            id: id.into(),
            label: label.into(),
            description: description.into(),
            requires_root: true,
        }
    }

    pub fn without_root(mut self) -> Self {
        self.requires_root = false;
        self
    }
}

/// A single problem (or note) a check detected on a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthFinding {
    /// Stable id across runs: `"{check}:{machine}:{kind}"`. Lets the UI dedupe
    /// and the user re-check after a fix.
    pub id: String,
    /// The check that produced this finding (its provider name).
    pub check: String,
    pub category: HealthCategory,
    /// The node this finding is about.
    pub machine_id: Id,
    pub severity: Severity,
    /// Short, human title, e.g. `"IP forwarding not enabled"`.
    pub title: String,
    /// Longer explanation of the problem and its impact.
    pub detail: String,
    /// Remediations the user can press. Empty when nothing can be auto-fixed.
    #[serde(default)]
    pub fixes: Vec<FixAction>,
    pub detected_at: DateTime<Utc>,
}

impl HealthFinding {
    /// Build a finding. `kind` is a short stable discriminator unique within the
    /// check (used to form the finding id), e.g. `"disabled"`.
    pub fn new(
        check: impl Into<String>,
        kind: &str,
        machine_id: &Id,
        category: HealthCategory,
        severity: Severity,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        let check = check.into();
        HealthFinding {
            id: format!("{check}:{machine_id}:{kind}"),
            check,
            category,
            machine_id: machine_id.clone(),
            severity,
            title: title.into(),
            detail: detail.into(),
            fixes: Vec::new(),
            detected_at: Utc::now(),
        }
    }

    /// Attach a fix action (builder).
    pub fn with_fix(mut self, fix: FixAction) -> Self {
        self.fixes.push(fix);
        self
    }
}
