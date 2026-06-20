//! Horizontal autoscaling for container workloads.
//!
//! An [`Autoscaler`] selects a set of replicas by label and adjusts their count
//! against a list of [`ScalingRule`]s. Evaluation is a pure function of the
//! current replica count and an externally-supplied metric map, so this crate
//! stays independent of `ocf-monitoring`: the caller decides where the numbers
//! come from.

use ocf_core::prelude::*;
use std::collections::BTreeMap;

/// How a [`ScalingRule`] compares an observed metric to its threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    /// Fires when the metric is strictly greater than the threshold.
    Gt,
    /// Fires when the metric is strictly less than the threshold.
    Lt,
}

impl Comparison {
    /// Apply the comparison to an observed `value` and a `threshold`.
    pub fn matches(&self, value: f64, threshold: f64) -> bool {
        match self {
            Comparison::Gt => value > threshold,
            Comparison::Lt => value < threshold,
        }
    }
}

/// A single scaling rule: when `metric` crosses `threshold` in the direction
/// `comparison`, adjust the replica count by `change`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalingRule {
    /// The metric key looked up in the supplied metric map (e.g. `"cpu_pct"`).
    pub metric: String,
    /// Whether the rule fires above or below `threshold`.
    pub comparison: Comparison,
    /// The value the metric is compared against.
    pub threshold: f64,
    /// Replica delta to apply when the rule fires (e.g. `+1`, `-1`).
    pub change: i32,
}

impl ScalingRule {
    pub fn new(
        metric: impl Into<String>,
        comparison: Comparison,
        threshold: f64,
        change: i32,
    ) -> Self {
        ScalingRule {
            metric: metric.into(),
            comparison,
            threshold,
            change,
        }
    }

    /// Whether this rule fires for the given metric map.
    pub fn fires(&self, metrics: &BTreeMap<String, f64>) -> bool {
        metrics
            .get(&self.metric)
            .map(|v| self.comparison.matches(*v, self.threshold))
            .unwrap_or(false)
    }
}

/// A horizontal autoscaler over a label-selected set of container replicas.
///
/// Implements [`Resource`] so it is stored, audited, and served like any other
/// fabric object. Autoscaling is a container-only concern; VMs scale vertically
/// out of band.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Autoscaler {
    pub metadata: Metadata,
    /// Label selector identifying the replicas this autoscaler governs.
    #[serde(default)]
    pub selector: BTreeMap<String, String>,
    /// Lower clamp on replica count.
    pub min_replicas: u32,
    /// Upper clamp on replica count.
    pub max_replicas: u32,
    /// Rules evaluated against the supplied metrics, in order.
    #[serde(default)]
    pub rules: Vec<ScalingRule>,
}

impl Autoscaler {
    pub fn new(name: impl Into<String>, min_replicas: u32, max_replicas: u32) -> Self {
        Autoscaler {
            metadata: Metadata::new(name),
            selector: BTreeMap::new(),
            min_replicas,
            max_replicas,
            rules: Vec::new(),
        }
    }

    /// Builder: add a selector label the autoscaler matches on.
    pub fn with_selector(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.selector.insert(key.into(), value.into());
        self
    }

    /// Builder: append a scaling rule.
    pub fn with_rule(mut self, rule: ScalingRule) -> Self {
        self.rules.push(rule);
        self
    }
}

impl Resource for Autoscaler {
    fn kind(&self) -> &'static str {
        "autoscaler"
    }
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// The outcome of evaluating an [`Autoscaler`] against current metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoscaleDecision {
    /// The replica count the autoscaler wants, already clamped to
    /// `[min_replicas, max_replicas]`.
    pub desired_replicas: u32,
    /// A human-readable explanation of how the decision was reached.
    pub reason: String,
}

/// Evaluate an autoscaler: sum the deltas of every firing rule, apply them to
/// `current_replicas`, and clamp the result into the autoscaler's bounds.
///
/// The metric map is supplied by the caller (typically derived from
/// `ocf-monitoring`'s `ResourceUsage`), keeping this crate dependency-free.
pub fn evaluate(
    autoscaler: &Autoscaler,
    current_replicas: u32,
    metrics: &BTreeMap<String, f64>,
) -> AutoscaleDecision {
    let mut delta: i64 = 0;
    let mut fired: Vec<String> = Vec::new();

    for rule in &autoscaler.rules {
        if rule.fires(metrics) {
            delta += rule.change as i64;
            fired.push(format!(
                "{} {} {} => {:+}",
                rule.metric,
                match rule.comparison {
                    Comparison::Gt => ">",
                    Comparison::Lt => "<",
                },
                rule.threshold,
                rule.change
            ));
        }
    }

    // Apply the delta to the current count without underflow, then clamp.
    let proposed = (current_replicas as i64 + delta)
        .max(autoscaler.min_replicas as i64)
        .min(autoscaler.max_replicas as i64);
    let desired = proposed as u32;

    let reason = if fired.is_empty() {
        format!("no rule fired; holding at {current_replicas} replica(s)")
    } else {
        format!(
            "fired [{}]; {current_replicas} -> {desired} (clamped to [{}, {}])",
            fired.join(", "),
            autoscaler.min_replicas,
            autoscaler.max_replicas
        )
    };

    AutoscaleDecision {
        desired_replicas: desired,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(cpu: f64) -> BTreeMap<String, f64> {
        let mut m = BTreeMap::new();
        m.insert("cpu_pct".to_string(), cpu);
        m
    }

    #[test]
    fn scales_up_when_over_threshold() {
        let a = Autoscaler::new("web", 1, 5)
            .with_rule(ScalingRule::new("cpu_pct", Comparison::Gt, 80.0, 1));
        let d = evaluate(&a, 2, &metrics(95.0));
        assert_eq!(d.desired_replicas, 3);
    }

    #[test]
    fn scales_down_when_under_threshold() {
        let a = Autoscaler::new("web", 1, 5)
            .with_rule(ScalingRule::new("cpu_pct", Comparison::Lt, 20.0, -1));
        let d = evaluate(&a, 2, &metrics(5.0));
        assert_eq!(d.desired_replicas, 1);
    }

    #[test]
    fn clamps_to_max() {
        let a = Autoscaler::new("web", 1, 3)
            .with_rule(ScalingRule::new("cpu_pct", Comparison::Gt, 80.0, 5));
        let d = evaluate(&a, 2, &metrics(95.0));
        assert_eq!(d.desired_replicas, 3);
    }

    #[test]
    fn clamps_to_min_and_never_underflows() {
        let a = Autoscaler::new("web", 2, 5)
            .with_rule(ScalingRule::new("cpu_pct", Comparison::Lt, 20.0, -10));
        let d = evaluate(&a, 1, &metrics(5.0));
        assert_eq!(d.desired_replicas, 2);
    }

    #[test]
    fn holds_when_nothing_fires() {
        let a = Autoscaler::new("web", 1, 5)
            .with_rule(ScalingRule::new("cpu_pct", Comparison::Gt, 80.0, 1));
        let d = evaluate(&a, 2, &metrics(50.0));
        assert_eq!(d.desired_replicas, 2);
        assert!(d.reason.contains("no rule fired"));
    }
}
