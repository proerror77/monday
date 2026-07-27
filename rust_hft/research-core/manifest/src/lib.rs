//! Manifest contracts for reproducible research, evaluation, promotion, and live rollout.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const CEX_REPLAY_SNAPSHOT_SCHEMA_V1: &str = "cex-replay-snapshot-v1";
pub const CEX_REPLAY_DATASET_KIND: &str = "cex_replay_feature_dataset";
pub const CEX_REPLAY_DATASET_SCHEMA_V1: &str = "cex-replay-feature-dataset-v1";
pub const BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2: &str = "binance-lob-pit-v2";
pub const CEX_REPLAY_CLOCK_RECEIVED_AT_NS: &str = "received_at_ns";
pub const CEX_FEATURE_AVAILABILITY_POLICY: &str = "feature_available_time_equals_event_time";
pub const CEX_MODALITY_LOB: &str = "lob";
pub const CEX_MODALITY_AGGREGATE_TRADE: &str = "aggregate_trade";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("manifest id cannot be empty")]
    EmptyId,
    #[error("manifest reference kind cannot be empty")]
    EmptyKind,
    #[error("CEX replay snapshot identity is invalid: {0}")]
    InvalidCexReplaySnapshot(&'static str),
    #[error("CEX replay dataset identity is invalid: {0}")]
    InvalidCexReplayDataset(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexReplaySegmentIdentity {
    pub content_sha256: String,
    pub manifest_sha256: String,
    pub start_received_at_ns: u64,
    pub end_received_at_ns: u64,
    pub events: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexReplaySnapshotV1 {
    pub schema_version: String,
    pub venue: String,
    pub instrument_type: String,
    pub symbol: String,
    pub replay_clock: String,
    pub required_modalities: BTreeSet<String>,
    pub source_segments: Vec<CexReplaySegmentIdentity>,
    pub first_event_time: DateTime<Utc>,
    pub last_event_time: DateTime<Utc>,
    pub feature_artifact_sha256: String,
    pub feature_availability_policy: String,
    pub bucket_ms: u64,
    pub label_horizon_buckets: usize,
    pub top_depth: usize,
}

impl CexReplaySnapshotV1 {
    pub fn validate(&self) -> Result<(), ManifestError> {
        let invalid = ManifestError::InvalidCexReplaySnapshot;
        if self.schema_version != CEX_REPLAY_SNAPSHOT_SCHEMA_V1
            || self.venue != "binance"
            || !matches!(self.instrument_type.as_str(), "spot" | "usdm")
            || self.symbol.trim().is_empty()
            || self.symbol != self.symbol.to_ascii_uppercase()
            || self.replay_clock != CEX_REPLAY_CLOCK_RECEIVED_AT_NS
            || self.feature_availability_policy != CEX_FEATURE_AVAILABILITY_POLICY
            || self.bucket_ms == 0
            || self.label_horizon_buckets == 0
            || self.top_depth == 0
            || self.first_event_time > self.last_event_time
            || !valid_sha256(&self.feature_artifact_sha256)
        {
            return Err(invalid("metadata is incomplete"));
        }
        let required = BTreeSet::from([
            CEX_MODALITY_LOB.to_string(),
            CEX_MODALITY_AGGREGATE_TRADE.to_string(),
        ]);
        if self.required_modalities != required {
            return Err(invalid(
                "required modalities must be lob and aggregate_trade",
            ));
        }
        if self.source_segments.is_empty() {
            return Err(invalid("source segments are empty"));
        }
        let mut identities = BTreeSet::new();
        for segment in &self.source_segments {
            if !valid_sha256(&segment.content_sha256)
                || !valid_sha256(&segment.manifest_sha256)
                || segment.start_received_at_ns > segment.end_received_at_ns
                || segment.events == 0
                || !identities.insert((
                    segment.content_sha256.as_str(),
                    segment.manifest_sha256.as_str(),
                ))
            {
                return Err(invalid("source segment identity is invalid"));
            }
        }
        if self
            .source_segments
            .windows(2)
            .any(|pair| pair[0].end_received_at_ns > pair[1].start_received_at_ns)
        {
            return Err(invalid("source segments are out of order or overlap"));
        }
        let first_event_ns = u64::try_from(
            self.first_event_time
                .timestamp_nanos_opt()
                .ok_or_else(|| invalid("event time is out of range"))?,
        )
        .map_err(|_| invalid("event time is out of range"))?;
        let last_event_ns = u64::try_from(
            self.last_event_time
                .timestamp_nanos_opt()
                .ok_or_else(|| invalid("event time is out of range"))?,
        )
        .map_err(|_| invalid("event time is out of range"))?;
        if first_event_ns < self.source_segments[0].start_received_at_ns
            || last_event_ns
                > self
                    .source_segments
                    .last()
                    .expect("non-empty source segments have a last segment")
                    .end_received_at_ns
        {
            return Err(invalid("event range is outside source segments"));
        }
        Ok(())
    }

    pub fn sha256(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("CEX replay snapshot must serialize");
        format!("{:x}", Sha256::digest(bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexReplayDatasetManifestV1 {
    pub dataset_kind: String,
    pub schema_version: String,
    pub manifest_id: String,
    pub feature_manifest_id: String,
    pub snapshot: CexReplaySnapshotV1,
    pub snapshot_sha256: String,
}

impl CexReplayDatasetManifestV1 {
    pub fn new(
        feature_manifest_id: impl Into<String>,
        snapshot: CexReplaySnapshotV1,
    ) -> Result<Self, ManifestError> {
        let snapshot_sha256 = snapshot.sha256();
        let manifest = Self {
            dataset_kind: CEX_REPLAY_DATASET_KIND.to_string(),
            schema_version: CEX_REPLAY_DATASET_SCHEMA_V1.to_string(),
            manifest_id: format!("dataset-cex-replay-{snapshot_sha256}"),
            feature_manifest_id: feature_manifest_id.into(),
            snapshot,
            snapshot_sha256,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        let invalid = ManifestError::InvalidCexReplayDataset;
        self.snapshot.validate()?;
        if self.dataset_kind != CEX_REPLAY_DATASET_KIND
            || self.schema_version != CEX_REPLAY_DATASET_SCHEMA_V1
            || self.feature_manifest_id.trim().is_empty()
            || !valid_sha256(&self.snapshot_sha256)
            || self.snapshot_sha256 != self.snapshot.sha256()
            || self.manifest_id != format!("dataset-cex-replay-{}", self.snapshot_sha256)
        {
            return Err(invalid("metadata or digest is inconsistent"));
        }
        Ok(())
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && value == value.to_ascii_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ManifestId(String);

impl ManifestId {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        let id = Self(value);
        id.validate()?;
        Ok(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.0.trim().is_empty() {
            return Err(ManifestError::EmptyId);
        }
        Ok(())
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
        let reference = Self { id, kind };
        reference.validate()?;
        Ok(reference)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        self.id.validate()?;
        if self.kind.trim().is_empty() {
            return Err(ManifestError::EmptyKind);
        }
        Ok(())
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

impl DataManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.id.validate()
    }
}

impl FeatureManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.id.validate()?;
        self.data_manifest.validate()
    }
}

impl LabelManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.id.validate()?;
        self.feature_manifest.validate()
    }
}

impl SearchManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.id.validate()?;
        self.parent_run_ids
            .iter()
            .try_for_each(ManifestId::validate)
    }
}

impl EvaluationManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.id.validate()?;
        self.search_manifest.validate()
    }
}

impl PromotionManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.id.validate()?;
        self.evaluation_manifest.validate()
    }
}

impl LiveRolloutManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.id.validate()?;
        self.promotion_manifest.validate()
    }
}

impl HarnessManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.id.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cex_snapshot() -> CexReplaySnapshotV1 {
        CexReplaySnapshotV1 {
            schema_version: CEX_REPLAY_SNAPSHOT_SCHEMA_V1.to_string(),
            venue: "binance".to_string(),
            instrument_type: "usdm".to_string(),
            symbol: "BTCUSDT".to_string(),
            replay_clock: CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
            required_modalities: BTreeSet::from([
                CEX_MODALITY_LOB.to_string(),
                CEX_MODALITY_AGGREGATE_TRADE.to_string(),
            ]),
            source_segments: vec![CexReplaySegmentIdentity {
                content_sha256: "1".repeat(64),
                manifest_sha256: "2".repeat(64),
                start_received_at_ns: 1_783_987_200_000_000_000,
                end_received_at_ns: 1_783_987_210_000_000_000,
                events: 100,
            }],
            first_event_time: DateTime::parse_from_rfc3339("2026-07-14T00:00:02Z")
                .unwrap()
                .with_timezone(&Utc),
            last_event_time: DateTime::parse_from_rfc3339("2026-07-14T00:00:04Z")
                .unwrap()
                .with_timezone(&Utc),
            feature_artifact_sha256: "3".repeat(64),
            feature_availability_policy: CEX_FEATURE_AVAILABILITY_POLICY.to_string(),
            bucket_ms: 1_000,
            label_horizon_buckets: 5,
            top_depth: 5,
        }
    }

    #[test]
    fn cex_replay_snapshot_digest_is_deterministic_and_identity_sensitive() {
        let snapshot = cex_snapshot();
        snapshot.validate().unwrap();

        assert_eq!(snapshot.sha256(), snapshot.clone().sha256());
        assert_eq!(snapshot.sha256().len(), 64);
        let mut other_symbol = snapshot.clone();
        other_symbol.symbol = "SOLUSDT".to_string();
        other_symbol.validate().unwrap();
        assert_ne!(snapshot.sha256(), other_symbol.sha256());
    }

    #[test]
    fn cex_replay_snapshot_rejects_overlapping_segments() {
        let mut snapshot = cex_snapshot();
        snapshot.source_segments.push(CexReplaySegmentIdentity {
            content_sha256: "4".repeat(64),
            manifest_sha256: "5".repeat(64),
            start_received_at_ns: 1_783_987_209_000_000_000,
            end_received_at_ns: 1_783_987_211_000_000_000,
            events: 10,
        });

        assert_eq!(
            snapshot.validate().unwrap_err(),
            ManifestError::InvalidCexReplaySnapshot("source segments are out of order or overlap")
        );
    }

    #[test]
    fn cex_replay_dataset_identity_binds_snapshot_digest() {
        let snapshot = cex_snapshot();
        let manifest =
            CexReplayDatasetManifestV1::new("dataset-feature-sha", snapshot.clone()).unwrap();
        let mut different_tape = snapshot;
        different_tape.source_segments[0].manifest_sha256 = "4".repeat(64);
        let different =
            CexReplayDatasetManifestV1::new("dataset-feature-sha", different_tape).unwrap();

        assert_ne!(manifest.manifest_id, different.manifest_id);
        manifest.validate().unwrap();
    }

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

    #[test]
    fn validate_rejects_deserialized_empty_manifest_id() {
        let id = ManifestId(" ".to_string());
        assert_eq!(id.validate().unwrap_err(), ManifestError::EmptyId);
    }

    #[test]
    fn validate_rejects_deserialized_empty_manifest_ref_kind() {
        let reference = ManifestRef {
            id: ManifestId::new("data-1").unwrap(),
            kind: " ".to_string(),
        };

        assert_eq!(reference.validate().unwrap_err(), ManifestError::EmptyKind);
    }

    #[test]
    fn validate_rejects_nested_manifest_ref() {
        let manifest = FeatureManifest {
            id: ManifestId::new("feature-1").unwrap(),
            data_manifest: ManifestRef {
                id: ManifestId::new("data-1").unwrap(),
                kind: " ".to_string(),
            },
            feature_set_id: "features".to_string(),
            operators: vec![],
            windows: vec![],
            normalization: "none".to_string(),
            availability_policy: "close".to_string(),
        };

        assert_eq!(manifest.validate().unwrap_err(), ManifestError::EmptyKind);
    }
}
