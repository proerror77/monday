//! Manifest contracts for reproducible research, evaluation, promotion, and live rollout.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("manifest id cannot be empty")]
    EmptyId,
    #[error("manifest reference kind cannot be empty")]
    EmptyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ManifestId(String);

impl ManifestId {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ManifestError::EmptyId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestRef {
    pub id: ManifestId,
    pub kind: String,
}

impl ManifestRef {
    pub fn new(id: ManifestId, kind: impl Into<String>) -> Result<Self, ManifestError> {
        let kind = kind.into();
        if kind.trim().is_empty() {
            return Err(ManifestError::EmptyKind);
        }
        Ok(Self { id, kind })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub uri: String,
    pub content_type: String,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataManifest {
    pub id: ManifestId,
    pub sources: Vec<String>,
    pub symbols: Vec<String>,
    pub time_range: TimeRange,
    pub artifact_refs: Vec<ArtifactRef>,
    pub schema_versions: BTreeMap<String, String>,
    pub quality_summary: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureManifest {
    pub id: ManifestId,
    pub data_manifest: ManifestRef,
    pub feature_set_id: String,
    pub operators: Vec<String>,
    pub windows: Vec<String>,
    pub normalization: String,
    pub availability_policy: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabelManifest {
    pub id: ManifestId,
    pub feature_manifest: ManifestRef,
    pub horizon: String,
    pub barrier_config: BTreeMap<String, f64>,
    pub fee_bps: f64,
    pub slippage_bps: f64,
    pub funding_cost_bps: f64,
    pub label_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchManifest {
    pub id: ManifestId,
    pub engine: String,
    pub seed: Option<u64>,
    pub model_or_prompt_version: Option<String>,
    pub search_space: BTreeMap<String, String>,
    pub parent_run_ids: Vec<ManifestId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationManifest {
    pub id: ManifestId,
    pub search_manifest: ManifestRef,
    pub evaluator_version: String,
    pub metrics: BTreeMap<String, f64>,
    pub costs: BTreeMap<String, f64>,
    pub walk_forward_split: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionManifest {
    pub id: ManifestId,
    pub asset_id: String,
    pub evaluation_manifest: ManifestRef,
    pub gate_results: BTreeMap<String, bool>,
    pub approval_mode: String,
    pub rollout_limits: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveRolloutManifest {
    pub id: ManifestId,
    pub promotion_manifest: ManifestRef,
    pub runtime_config_ref: String,
    pub risk_policy_ref: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub attribution: BTreeMap<String, f64>,
    pub rollback_result: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessManifest {
    pub id: ManifestId,
    pub harness_version: String,
    pub agents: Vec<String>,
    pub prompt_versions: BTreeMap<String, String>,
    pub tool_permissions: BTreeMap<String, Vec<String>>,
    pub evaluator_versions: BTreeMap<String, String>,
    pub memory_snapshot_ref: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_manifest_id() {
        assert_eq!(ManifestId::new("  ").unwrap_err(), ManifestError::EmptyId);
    }

    #[test]
    fn builds_manifest_ref() {
        let id = ManifestId::new("data-20260708").unwrap();
        let reference = ManifestRef::new(id, "data_manifest").unwrap();
        assert_eq!(reference.kind, "data_manifest");
        assert_eq!(reference.id.as_str(), "data-20260708");
    }
}
