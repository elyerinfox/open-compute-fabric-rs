//! Metadata common to every managed resource.

use crate::id::Id;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Common bookkeeping carried by every fabric resource.
///
/// `labels` are intended for selection/grouping (e.g. load-balancer target
/// selectors, autoscaler matching), while `annotations` hold non-identifying
/// metadata (free-form operator notes, provider hints, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub id: Id,
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Metadata {
    pub fn new(name: impl Into<String>) -> Self {
        let now = Utc::now();
        Metadata {
            id: Id::new(),
            name: name.into(),
            labels: BTreeMap::new(),
            annotations: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Build metadata with a stable, name-derived id.
    pub fn named(name: impl Into<String>) -> Self {
        let name = name.into();
        let now = Utc::now();
        Metadata {
            id: Id::named(name.clone()),
            name,
            labels: BTreeMap::new(),
            annotations: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    pub fn with_annotation(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.annotations.insert(key.into(), value.into());
        self
    }

    /// True if every selector entry is present and equal in this resource's labels.
    pub fn matches_labels(&self, selector: &BTreeMap<String, String>) -> bool {
        selector
            .iter()
            .all(|(k, v)| self.labels.get(k).map(|got| got == v).unwrap_or(false))
    }

    /// Stamp `updated_at` with the current time.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }
}
