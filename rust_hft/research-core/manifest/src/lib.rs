//! Manifest contracts for reproducible research, evaluation, promotion, and live rollout.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const CEX_REPLAY_SNAPSHOT_SCHEMA_V1: &str = "cex-replay-snapshot-v1";
pub const CEX_REPLAY_SNAPSHOT_SCHEMA_V2: &str = "cex-replay-snapshot-v2";
pub const CEX_REPLAY_SNAPSHOT_SCHEMA_V3: &str = "cex-replay-snapshot-v3";
pub const CEX_REPLAY_SNAPSHOT_SCHEMA_V4: &str = "cex-replay-snapshot-v4";
pub const CEX_REPLAY_DATASET_KIND: &str = "cex_replay_feature_dataset";
pub const CEX_REPLAY_DATASET_SCHEMA_V1: &str = "cex-replay-feature-dataset-v1";
pub const CEX_REPLAY_DATASET_SCHEMA_V2: &str = "cex-replay-feature-dataset-v2";
pub const CEX_REPLAY_DATASET_SCHEMA_V3: &str = "cex-replay-feature-dataset-v3";
pub const CEX_REPLAY_DATASET_SCHEMA_V4: &str = "cex-replay-feature-dataset-v4";
pub const BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2: &str = "binance-lob-pit-v2";
pub const BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3: &str = "binance-lob-pit-v3";
pub const BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V4: &str = "binance-lob-pit-v4";
pub const BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V5: &str = "binance-lob-pit-v5";
pub const BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V6: &str = "binance-lob-pit-v6";
pub const CEX_REPLAY_CLOCK_RECEIVED_AT_NS: &str = "received_at_ns";
pub const CEX_FEATURE_AVAILABILITY_POLICY: &str = "feature_available_time_equals_event_time";
pub const CEX_MODALITY_LOB: &str = "lob";
pub const CEX_MODALITY_AGGREGATE_TRADE: &str = "aggregate_trade";
pub const CEX_MODALITY_FUNDING: &str = "funding";
pub const CEX_MODALITY_OPEN_INTEREST: &str = "open_interest";
pub const CEX_DERIVATIVES_MAX_GAP_NS: u64 = 90_000_000_000;

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

/// Historical CEX replay snapshot. Kept for read-only evidence decoding; new writers use V4.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexArtifactTripletV2 {
    pub data_sha256: String,
    pub manifest_sha256: String,
    pub success_sha256: String,
}

impl CexArtifactTripletV2 {
    fn valid(&self) -> bool {
        valid_sha256(&self.data_sha256)
            && valid_sha256(&self.manifest_sha256)
            && valid_sha256(&self.success_sha256)
            && self.success_sha256 == self.data_sha256
    }
}

impl CexReplaySnapshotV1 {
    pub fn validate(&self) -> Result<(), ManifestError> {
        validate_snapshot_core(
            &self.schema_version,
            CEX_REPLAY_SNAPSHOT_SCHEMA_V1,
            &self.venue,
            &self.instrument_type,
            &self.symbol,
            &self.replay_clock,
            &self.required_modalities,
            &BTreeSet::from([
                CEX_MODALITY_LOB.to_string(),
                CEX_MODALITY_AGGREGATE_TRADE.to_string(),
            ]),
            &self.source_segments,
            self.first_event_time,
            self.last_event_time,
            &self.feature_artifact_sha256,
            &self.feature_availability_policy,
            self.bucket_ms,
            self.label_horizon_buckets,
            self.top_depth,
        )
    }

    pub fn sha256(&self) -> String {
        snapshot_sha256(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexInstrumentRulesV2 {
    pub tick_size: String,
    pub step_size: String,
    pub min_notional: String,
    pub available_at: DateTime<Utc>,
    pub valid_through: DateTime<Utc>,
    pub evidence: Vec<CexArtifactTripletV2>,
}

impl CexInstrumentRulesV2 {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !positive_decimal(&self.tick_size)
            || !positive_decimal(&self.step_size)
            || !positive_decimal(&self.min_notional)
            || self.available_at > self.valid_through
            || !valid_evidence_set(&self.evidence)
        {
            return Err(ManifestError::InvalidCexReplaySnapshot(
                "instrument rules are invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexFeeScheduleV2 {
    pub runtime_account_id: String,
    pub account_fingerprint: String,
    pub maker_buy_fee_bps: String,
    pub maker_sell_fee_bps: String,
    pub taker_buy_fee_bps: String,
    pub taker_sell_fee_bps: String,
    pub available_at: DateTime<Utc>,
    pub valid_through: DateTime<Utc>,
    pub evidence: Vec<CexArtifactTripletV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexPitSeriesEvidenceV2 {
    pub evidence: Vec<CexArtifactTripletV2>,
    pub first_available_at: DateTime<Utc>,
    pub last_available_at: DateTime<Utc>,
    pub observations: u64,
    pub max_gap_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexDerivativesReferenceV2 {
    pub funding: CexPitSeriesEvidenceV2,
    pub open_interest: CexPitSeriesEvidenceV2,
    pub evaluation_funding_bps_per_bucket: String,
}

/// Historical snapshot with realized LiveSmall latency evidence. Read-only after V3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexLatencyCostV2 {
    pub method: String,
    pub venue: String,
    pub symbol: String,
    pub runtime_account_id: String,
    pub account_fingerprint: String,
    pub evidence: CexArtifactTripletV2,
    pub first_observed_at: DateTime<Utc>,
    pub last_observed_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub observations: u64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub p50_cost_bps: String,
    pub p95_cost_bps: String,
    pub p99_cost_bps: String,
}

/// Historical CEX replay snapshot. Kept for immutable evidence readback; new writers use V4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexReplaySnapshotV2 {
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
    pub instrument_rules: CexInstrumentRulesV2,
    pub fee_schedule: CexFeeScheduleV2,
    pub derivatives_reference: Option<CexDerivativesReferenceV2>,
    pub latency_cost: CexLatencyCostV2,
}

impl CexReplaySnapshotV2 {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !valid_cex_symbol(&self.symbol) {
            return Err(ManifestError::InvalidCexReplaySnapshot(
                "symbol is not canonical",
            ));
        }
        let mut required = BTreeSet::from([
            CEX_MODALITY_LOB.to_string(),
            CEX_MODALITY_AGGREGATE_TRADE.to_string(),
        ]);
        if self.instrument_type == "usdm" {
            required.insert(CEX_MODALITY_FUNDING.to_string());
            required.insert(CEX_MODALITY_OPEN_INTEREST.to_string());
        }
        validate_snapshot_core(
            &self.schema_version,
            CEX_REPLAY_SNAPSHOT_SCHEMA_V2,
            &self.venue,
            &self.instrument_type,
            &self.symbol,
            &self.replay_clock,
            &self.required_modalities,
            &required,
            &self.source_segments,
            self.first_event_time,
            self.last_event_time,
            &self.feature_artifact_sha256,
            &self.feature_availability_policy,
            self.bucket_ms,
            self.label_horizon_buckets,
            self.top_depth,
        )?;
        let invalid = ManifestError::InvalidCexReplaySnapshot;
        let label_available_through = u64::try_from(self.label_horizon_buckets)
            .ok()
            .and_then(|horizon| self.bucket_ms.checked_mul(horizon))
            .and_then(|offset| i64::try_from(offset).ok())
            .and_then(chrono::TimeDelta::try_milliseconds)
            .and_then(|offset| self.last_event_time.checked_add_signed(offset))
            .ok_or_else(|| invalid("label availability time overflows"))?;
        self.instrument_rules.validate()?;
        if self.instrument_rules.available_at > self.first_event_time
            || self.instrument_rules.valid_through < label_available_through
            || !valid_runtime_account_id(&self.fee_schedule.runtime_account_id)
            || !valid_sha256(&self.fee_schedule.account_fingerprint)
            || !nonnegative_decimal(&self.fee_schedule.maker_buy_fee_bps)
            || !nonnegative_decimal(&self.fee_schedule.maker_sell_fee_bps)
            || !nonnegative_decimal(&self.fee_schedule.taker_buy_fee_bps)
            || !nonnegative_decimal(&self.fee_schedule.taker_sell_fee_bps)
            || !valid_evidence_set(&self.fee_schedule.evidence)
            || self.fee_schedule.available_at > self.first_event_time
            || self.fee_schedule.available_at > self.fee_schedule.valid_through
            || self.fee_schedule.valid_through < label_available_through
        {
            return Err(invalid("PIT rules or fee evidence is invalid"));
        }
        match (&self.instrument_type[..], &self.derivatives_reference) {
            ("usdm", Some(reference))
                if pit_series_covers(
                    &reference.funding,
                    self.first_event_time,
                    label_available_through,
                ) && pit_series_covers(
                    &reference.open_interest,
                    self.first_event_time,
                    label_available_through,
                ) && nonnegative_decimal(&reference.evaluation_funding_bps_per_bucket) => {}
            ("spot", None) => {}
            _ => return Err(invalid("derivatives reference evidence is invalid")),
        }
        if self.latency_cost.method != "verified_order_lifecycle_realized_slippage"
            || self.latency_cost.venue != self.venue
            || self.latency_cost.symbol != self.symbol
            || self.latency_cost.runtime_account_id != self.fee_schedule.runtime_account_id
            || self.latency_cost.account_fingerprint != self.fee_schedule.account_fingerprint
            || !self.latency_cost.evidence.valid()
            || self.latency_cost.first_observed_at > self.latency_cost.last_observed_at
            || self.latency_cost.last_observed_at > self.first_event_time
            || self.latency_cost.available_at < self.latency_cost.last_observed_at
            || self.latency_cost.available_at > self.first_event_time
            || self.latency_cost.observations == 0
            || self.latency_cost.p50_ns > self.latency_cost.p95_ns
            || self.latency_cost.p95_ns > self.latency_cost.p99_ns
            || !ordered_nonnegative_decimals([
                &self.latency_cost.p50_cost_bps,
                &self.latency_cost.p95_cost_bps,
                &self.latency_cost.p99_cost_bps,
            ])
            || (self.latency_cost.observations == 1
                && (self.latency_cost.p50_ns != self.latency_cost.p99_ns
                    || !equal_decimals(
                        &self.latency_cost.p50_cost_bps,
                        &self.latency_cost.p99_cost_bps,
                    )))
        {
            return Err(invalid("measured latency cost evidence is invalid"));
        }
        Ok(())
    }

    pub fn sha256(&self) -> String {
        snapshot_sha256(self)
    }
}

/// Historical account-bound snapshot. Kept for immutable evidence readback; new writers use V4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexReplaySnapshotV3 {
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
    pub instrument_rules: CexInstrumentRulesV2,
    pub fee_schedule: CexFeeScheduleV2,
    pub derivatives_reference: Option<CexDerivativesReferenceV2>,
}

impl CexReplaySnapshotV3 {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !valid_cex_symbol(&self.symbol) {
            return Err(ManifestError::InvalidCexReplaySnapshot(
                "symbol is not canonical",
            ));
        }
        let mut required = BTreeSet::from([
            CEX_MODALITY_LOB.to_string(),
            CEX_MODALITY_AGGREGATE_TRADE.to_string(),
        ]);
        if self.instrument_type == "usdm" {
            required.insert(CEX_MODALITY_FUNDING.to_string());
            required.insert(CEX_MODALITY_OPEN_INTEREST.to_string());
        }
        validate_snapshot_core(
            &self.schema_version,
            CEX_REPLAY_SNAPSHOT_SCHEMA_V3,
            &self.venue,
            &self.instrument_type,
            &self.symbol,
            &self.replay_clock,
            &self.required_modalities,
            &required,
            &self.source_segments,
            self.first_event_time,
            self.last_event_time,
            &self.feature_artifact_sha256,
            &self.feature_availability_policy,
            self.bucket_ms,
            self.label_horizon_buckets,
            self.top_depth,
        )?;
        let invalid = ManifestError::InvalidCexReplaySnapshot;
        let label_available_through = u64::try_from(self.label_horizon_buckets)
            .ok()
            .and_then(|horizon| self.bucket_ms.checked_mul(horizon))
            .and_then(|offset| i64::try_from(offset).ok())
            .and_then(chrono::TimeDelta::try_milliseconds)
            .and_then(|offset| self.last_event_time.checked_add_signed(offset))
            .ok_or_else(|| invalid("label availability time overflows"))?;
        self.instrument_rules.validate()?;
        if self.instrument_rules.available_at > self.first_event_time
            || self.instrument_rules.valid_through < label_available_through
            || !valid_runtime_account_id(&self.fee_schedule.runtime_account_id)
            || !valid_sha256(&self.fee_schedule.account_fingerprint)
            || !nonnegative_decimal(&self.fee_schedule.maker_buy_fee_bps)
            || !nonnegative_decimal(&self.fee_schedule.maker_sell_fee_bps)
            || !nonnegative_decimal(&self.fee_schedule.taker_buy_fee_bps)
            || !nonnegative_decimal(&self.fee_schedule.taker_sell_fee_bps)
            || !valid_evidence_set(&self.fee_schedule.evidence)
            || self.fee_schedule.available_at > self.first_event_time
            || self.fee_schedule.available_at > self.fee_schedule.valid_through
            || self.fee_schedule.valid_through < label_available_through
        {
            return Err(invalid("PIT rules or fee evidence is invalid"));
        }
        match (&self.instrument_type[..], &self.derivatives_reference) {
            ("usdm", Some(reference))
                if pit_series_covers(
                    &reference.funding,
                    self.first_event_time,
                    label_available_through,
                ) && pit_series_covers(
                    &reference.open_interest,
                    self.first_event_time,
                    label_available_through,
                ) && nonnegative_decimal(&reference.evaluation_funding_bps_per_bucket) => {}
            ("spot", None) => {}
            _ => return Err(invalid("derivatives reference evidence is invalid")),
        }
        Ok(())
    }

    pub fn sha256(&self) -> String {
        snapshot_sha256(self)
    }
}

/// Credential-free USD-M research snapshot. Account-specific fees belong to runtime calibration;
/// reproducible research costs are declared by the content-hashed Mission evaluation policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexReplaySnapshotV4 {
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
    pub instrument_rules: CexInstrumentRulesV2,
    pub derivatives_reference: Option<CexDerivativesReferenceV2>,
}

impl CexReplaySnapshotV4 {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !valid_cex_symbol(&self.symbol) {
            return Err(ManifestError::InvalidCexReplaySnapshot(
                "symbol is not canonical",
            ));
        }
        if self.instrument_type != "usdm" {
            return Err(ManifestError::InvalidCexReplaySnapshot(
                "credential-free replay snapshot supports USD-M only",
            ));
        }
        let required = BTreeSet::from([
            CEX_MODALITY_LOB.to_string(),
            CEX_MODALITY_AGGREGATE_TRADE.to_string(),
            CEX_MODALITY_FUNDING.to_string(),
            CEX_MODALITY_OPEN_INTEREST.to_string(),
        ]);
        validate_snapshot_core(
            &self.schema_version,
            CEX_REPLAY_SNAPSHOT_SCHEMA_V4,
            &self.venue,
            &self.instrument_type,
            &self.symbol,
            &self.replay_clock,
            &self.required_modalities,
            &required,
            &self.source_segments,
            self.first_event_time,
            self.last_event_time,
            &self.feature_artifact_sha256,
            &self.feature_availability_policy,
            self.bucket_ms,
            self.label_horizon_buckets,
            self.top_depth,
        )?;
        let invalid = ManifestError::InvalidCexReplaySnapshot;
        let label_available_through = u64::try_from(self.label_horizon_buckets)
            .ok()
            .and_then(|horizon| self.bucket_ms.checked_mul(horizon))
            .and_then(|offset| i64::try_from(offset).ok())
            .and_then(chrono::TimeDelta::try_milliseconds)
            .and_then(|offset| self.last_event_time.checked_add_signed(offset))
            .ok_or_else(|| invalid("label availability time overflows"))?;
        self.instrument_rules.validate()?;
        if self.instrument_rules.available_at > self.first_event_time
            || self.instrument_rules.valid_through < label_available_through
        {
            return Err(invalid("PIT instrument rules evidence is invalid"));
        }
        match &self.derivatives_reference {
            Some(reference)
                if pit_series_covers(
                    &reference.funding,
                    self.first_event_time,
                    label_available_through,
                ) && pit_series_covers(
                    &reference.open_interest,
                    self.first_event_time,
                    label_available_through,
                ) && nonnegative_decimal(&reference.evaluation_funding_bps_per_bucket) => {}
            _ => return Err(invalid("derivatives reference evidence is invalid")),
        }
        Ok(())
    }

    pub fn sha256(&self) -> String {
        snapshot_sha256(self)
    }
}

fn pit_series_covers(
    evidence: &CexPitSeriesEvidenceV2,
    first_event_time: DateTime<Utc>,
    required_through: DateTime<Utc>,
) -> bool {
    let span_ns = evidence
        .last_available_at
        .signed_duration_since(evidence.first_available_at)
        .num_nanoseconds()
        .and_then(|span| u64::try_from(span).ok());
    valid_evidence_set(&evidence.evidence)
        && evidence.observations > 0
        && usize::try_from(evidence.observations).ok() == Some(evidence.evidence.len())
        && evidence.max_gap_ns <= CEX_DERIVATIVES_MAX_GAP_NS
        && evidence.first_available_at <= first_event_time
        && evidence.first_available_at <= evidence.last_available_at
        && evidence.last_available_at >= required_through
        && span_ns.is_some_and(|span| {
            evidence
                .max_gap_ns
                .checked_mul(evidence.observations - 1)
                .is_some_and(|covered| span <= covered)
        })
}

#[allow(clippy::too_many_arguments)]
fn validate_snapshot_core(
    schema_version: &str,
    expected_schema: &str,
    venue: &str,
    instrument_type: &str,
    symbol: &str,
    replay_clock: &str,
    required_modalities: &BTreeSet<String>,
    expected_modalities: &BTreeSet<String>,
    source_segments: &[CexReplaySegmentIdentity],
    first_event_time: DateTime<Utc>,
    last_event_time: DateTime<Utc>,
    feature_artifact_sha256: &str,
    feature_availability_policy: &str,
    bucket_ms: u64,
    label_horizon_buckets: usize,
    top_depth: usize,
) -> Result<(), ManifestError> {
    let invalid = ManifestError::InvalidCexReplaySnapshot;
    if schema_version != expected_schema
        || venue != "binance"
        || !matches!(instrument_type, "spot" | "usdm")
        || symbol.trim().is_empty()
        || symbol != symbol.to_ascii_uppercase()
        || replay_clock != CEX_REPLAY_CLOCK_RECEIVED_AT_NS
        || feature_availability_policy != CEX_FEATURE_AVAILABILITY_POLICY
        || bucket_ms == 0
        || label_horizon_buckets == 0
        || top_depth == 0
        || first_event_time > last_event_time
        || !valid_sha256(feature_artifact_sha256)
        || source_segments.is_empty()
    {
        return Err(invalid("metadata is incomplete"));
    }
    if required_modalities != expected_modalities {
        return Err(invalid("required modalities do not match the schema"));
    }
    let mut identities = BTreeSet::new();
    for segment in source_segments {
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
    if source_segments
        .windows(2)
        .any(|pair| pair[0].end_received_at_ns > pair[1].start_received_at_ns)
    {
        return Err(invalid("source segments are out of order or overlap"));
    }
    let first_event_ns = u64::try_from(
        first_event_time
            .timestamp_nanos_opt()
            .ok_or_else(|| invalid("event time is out of range"))?,
    )
    .map_err(|_| invalid("event time is out of range"))?;
    let last_event_ns = u64::try_from(
        last_event_time
            .timestamp_nanos_opt()
            .ok_or_else(|| invalid("event time is out of range"))?,
    )
    .map_err(|_| invalid("event time is out of range"))?;
    let final_segment_end_ns = source_segments
        .last()
        .expect("non-empty source segments have a last segment")
        .end_received_at_ns;
    if first_event_ns < source_segments[0].start_received_at_ns
        || last_event_ns > final_segment_end_ns
    {
        return Err(invalid("event range is outside source segments"));
    }
    let horizon_buckets = u64::try_from(label_horizon_buckets)
        .map_err(|_| invalid("label horizon is out of range"))?;
    let last_label_available_ns = bucket_ms
        .checked_mul(1_000_000)
        .and_then(|bucket_ns| bucket_ns.checked_mul(horizon_buckets))
        .and_then(|horizon_ns| last_event_ns.checked_add(horizon_ns))
        .ok_or_else(|| invalid("label horizon is out of range"))?;
    if last_label_available_ns > final_segment_end_ns {
        return Err(invalid("label availability is outside source segments"));
    }
    Ok(())
}

fn snapshot_sha256<T: Serialize>(snapshot: &T) -> String {
    let bytes = serde_json::to_vec(snapshot).expect("CEX replay snapshot must serialize");
    format!("{:x}", Sha256::digest(bytes))
}

/// Historical CEX replay dataset. Kept for read-only evidence decoding; new writers use V4.
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

/// Historical dataset manifest. Kept for immutable evidence readback; new writers use V4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexReplayDatasetManifestV2 {
    pub dataset_kind: String,
    pub schema_version: String,
    pub manifest_id: String,
    pub feature_manifest_id: String,
    pub snapshot: CexReplaySnapshotV2,
    pub snapshot_sha256: String,
}

impl CexReplayDatasetManifestV2 {
    pub fn validate(&self) -> Result<(), ManifestError> {
        let invalid = ManifestError::InvalidCexReplayDataset;
        self.snapshot.validate()?;
        if self.dataset_kind != CEX_REPLAY_DATASET_KIND
            || self.schema_version != CEX_REPLAY_DATASET_SCHEMA_V2
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

/// Historical account-bound dataset. Kept for immutable evidence readback; new writers use V4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexReplayDatasetManifestV3 {
    pub dataset_kind: String,
    pub schema_version: String,
    pub manifest_id: String,
    pub feature_manifest_id: String,
    pub snapshot: CexReplaySnapshotV3,
    pub snapshot_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexReplayDatasetManifestV4 {
    pub dataset_kind: String,
    pub schema_version: String,
    pub manifest_id: String,
    pub feature_manifest_id: String,
    pub snapshot: CexReplaySnapshotV4,
    pub snapshot_sha256: String,
}

impl CexReplayDatasetManifestV4 {
    pub fn new(
        feature_manifest_id: impl Into<String>,
        snapshot: CexReplaySnapshotV4,
    ) -> Result<Self, ManifestError> {
        let snapshot_sha256 = snapshot.sha256();
        let manifest = Self {
            dataset_kind: CEX_REPLAY_DATASET_KIND.to_string(),
            schema_version: CEX_REPLAY_DATASET_SCHEMA_V4.to_string(),
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
            || self.schema_version != CEX_REPLAY_DATASET_SCHEMA_V4
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

impl CexReplayDatasetManifestV3 {
    pub fn new(
        feature_manifest_id: impl Into<String>,
        snapshot: CexReplaySnapshotV3,
    ) -> Result<Self, ManifestError> {
        let snapshot_sha256 = snapshot.sha256();
        let manifest = Self {
            dataset_kind: CEX_REPLAY_DATASET_KIND.to_string(),
            schema_version: CEX_REPLAY_DATASET_SCHEMA_V3.to_string(),
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
            || self.schema_version != CEX_REPLAY_DATASET_SCHEMA_V3
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

fn valid_evidence_set(evidence: &[CexArtifactTripletV2]) -> bool {
    !evidence.is_empty()
        && evidence.iter().all(CexArtifactTripletV2::valid)
        && evidence
            .iter()
            .enumerate()
            .all(|(index, item)| !evidence[..index].contains(item))
}

fn valid_cex_symbol(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_runtime_account_id(value: &str) -> bool {
    !value.trim().is_empty()
}

fn positive_decimal(value: &str) -> bool {
    value
        .parse::<Decimal>()
        .is_ok_and(|value| value > Decimal::ZERO)
}

fn nonnegative_decimal(value: &str) -> bool {
    !value.starts_with('-')
        && value
            .parse::<Decimal>()
            .is_ok_and(|value| value >= Decimal::ZERO)
}

fn ordered_nonnegative_decimals(values: [&str; 3]) -> bool {
    let parsed = values.map(|value| value.parse::<Decimal>().ok());
    matches!(parsed, [Some(p50), Some(p95), Some(p99)]
        if values.iter().all(|value| !value.starts_with('-'))
            && p50 >= Decimal::ZERO
            && p50 <= p95
            && p95 <= p99)
}

fn equal_decimals(left: &str, right: &str) -> bool {
    left.parse::<Decimal>().ok() == right.parse::<Decimal>().ok()
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

    fn triplet(byte: char) -> CexArtifactTripletV2 {
        CexArtifactTripletV2 {
            data_sha256: byte.to_string().repeat(64),
            manifest_sha256: byte.to_string().repeat(64),
            success_sha256: byte.to_string().repeat(64),
        }
    }

    fn cex_snapshot() -> CexReplaySnapshotV4 {
        CexReplaySnapshotV4 {
            schema_version: CEX_REPLAY_SNAPSHOT_SCHEMA_V4.to_string(),
            venue: "binance".to_string(),
            instrument_type: "usdm".to_string(),
            symbol: "BTCUSDT".to_string(),
            replay_clock: CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
            required_modalities: BTreeSet::from([
                CEX_MODALITY_LOB.to_string(),
                CEX_MODALITY_AGGREGATE_TRADE.to_string(),
                CEX_MODALITY_FUNDING.to_string(),
                CEX_MODALITY_OPEN_INTEREST.to_string(),
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
            instrument_rules: CexInstrumentRulesV2 {
                tick_size: "0.1".to_string(),
                step_size: "0.001".to_string(),
                min_notional: "5".to_string(),
                available_at: DateTime::parse_from_rfc3339("2026-07-14T00:00:01Z")
                    .unwrap()
                    .with_timezone(&Utc),
                valid_through: DateTime::parse_from_rfc3339("2026-07-14T00:00:09Z")
                    .unwrap()
                    .with_timezone(&Utc),
                evidence: vec![triplet('4')],
            },
            derivatives_reference: Some(CexDerivativesReferenceV2 {
                funding: CexPitSeriesEvidenceV2 {
                    evidence: vec![triplet('6'), triplet('a')],
                    first_available_at: DateTime::parse_from_rfc3339("2026-07-14T00:00:01Z")
                        .unwrap()
                        .with_timezone(&Utc),
                    last_available_at: DateTime::parse_from_rfc3339("2026-07-14T00:00:09Z")
                        .unwrap()
                        .with_timezone(&Utc),
                    observations: 2,
                    max_gap_ns: 8_000_000_000,
                },
                open_interest: CexPitSeriesEvidenceV2 {
                    evidence: vec![triplet('7'), triplet('b')],
                    first_available_at: DateTime::parse_from_rfc3339("2026-07-14T00:00:01Z")
                        .unwrap()
                        .with_timezone(&Utc),
                    last_available_at: DateTime::parse_from_rfc3339("2026-07-14T00:00:09Z")
                        .unwrap()
                        .with_timezone(&Utc),
                    observations: 2,
                    max_gap_ns: 8_000_000_000,
                },
                evaluation_funding_bps_per_bucket: "0".to_string(),
            }),
        }
    }

    fn historical_cex_snapshot_v3() -> CexReplaySnapshotV3 {
        let mut value = serde_json::to_value(cex_snapshot()).unwrap();
        value["schema_version"] = serde_json::json!(CEX_REPLAY_SNAPSHOT_SCHEMA_V3);
        value["fee_schedule"] = serde_json::json!({
            "runtime_account_id": "desk/main",
            "account_fingerprint": "9".repeat(64),
            "maker_buy_fee_bps": "2",
            "maker_sell_fee_bps": "2",
            "taker_buy_fee_bps": "5",
            "taker_sell_fee_bps": "5",
            "available_at": "2026-07-14T00:00:01Z",
            "valid_through": "2026-07-14T00:00:09Z",
            "evidence": [triplet('5')]
        });
        serde_json::from_value(value).unwrap()
    }

    fn historical_cex_snapshot_v2() -> CexReplaySnapshotV2 {
        let mut value = serde_json::to_value(historical_cex_snapshot_v3()).unwrap();
        value["schema_version"] = serde_json::json!(CEX_REPLAY_SNAPSHOT_SCHEMA_V2);
        value["latency_cost"] = serde_json::json!({
            "method": "verified_order_lifecycle_realized_slippage",
            "venue": "binance",
            "symbol": "BTCUSDT",
            "runtime_account_id": "desk/main",
            "account_fingerprint": "9".repeat(64),
            "evidence": triplet('8'),
            "first_observed_at": "2026-07-14T00:00:00Z",
            "last_observed_at": "2026-07-14T00:00:01Z",
            "available_at": "2026-07-14T00:00:01Z",
            "observations": 100,
            "p50_ns": 1_000_000,
            "p95_ns": 2_000_000,
            "p99_ns": 3_000_000,
            "p50_cost_bps": "0.1",
            "p95_cost_bps": "0.2",
            "p99_cost_bps": "0.3"
        });
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn historical_v2_snapshot_and_dataset_remain_readable() {
        let snapshot = historical_cex_snapshot_v2();
        snapshot.validate().unwrap();
        let snapshot_sha256 = snapshot.sha256();
        let manifest = CexReplayDatasetManifestV2 {
            dataset_kind: CEX_REPLAY_DATASET_KIND.to_string(),
            schema_version: CEX_REPLAY_DATASET_SCHEMA_V2.to_string(),
            manifest_id: format!("dataset-cex-replay-{snapshot_sha256}"),
            feature_manifest_id: "dataset-feature-sha".to_string(),
            snapshot,
            snapshot_sha256,
        };

        manifest.validate().unwrap();
        let decoded: CexReplayDatasetManifestV2 =
            serde_json::from_value(serde_json::to_value(&manifest).unwrap()).unwrap();
        decoded.validate().unwrap();
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
    fn credential_free_snapshot_has_no_account_identity_or_fee_schedule() {
        let value = serde_json::to_value(cex_snapshot()).unwrap();

        assert!(value.get("fee_schedule").is_none());
        let encoded = serde_json::to_string(&value).unwrap();
        assert!(!encoded.contains("runtime_account_id"));
        assert!(!encoded.contains("account_fingerprint"));
    }

    #[test]
    fn credential_free_snapshot_rejects_spot() {
        let mut snapshot = cex_snapshot();
        snapshot.instrument_type = "spot".to_string();
        snapshot.required_modalities = BTreeSet::from([
            CEX_MODALITY_LOB.to_string(),
            CEX_MODALITY_AGGREGATE_TRADE.to_string(),
        ]);
        snapshot.derivatives_reference = None;

        assert_eq!(
            snapshot.validate().unwrap_err(),
            ManifestError::InvalidCexReplaySnapshot(
                "credential-free replay snapshot supports USD-M only"
            )
        );
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
    fn cex_replay_snapshot_rejects_label_horizon_past_authenticated_tape() {
        let mut snapshot = cex_snapshot();
        snapshot.source_segments[0].end_received_at_ns = 1_783_987_208_000_000_000;

        assert_eq!(
            snapshot.validate().unwrap_err(),
            ManifestError::InvalidCexReplaySnapshot(
                "label availability is outside source segments"
            )
        );
    }

    #[test]
    fn cex_replay_dataset_identity_binds_snapshot_digest() {
        let snapshot = cex_snapshot();
        let manifest =
            CexReplayDatasetManifestV4::new("dataset-feature-sha", snapshot.clone()).unwrap();
        let mut different_tape = snapshot;
        different_tape.source_segments[0].manifest_sha256 = "4".repeat(64);
        let different =
            CexReplayDatasetManifestV4::new("dataset-feature-sha", different_tape).unwrap();

        assert_ne!(manifest.manifest_id, different.manifest_id);
        manifest.validate().unwrap();
    }

    #[test]
    fn cex_replay_snapshot_rejects_stale_open_interest_tail() {
        let mut snapshot = cex_snapshot();
        snapshot
            .derivatives_reference
            .as_mut()
            .unwrap()
            .open_interest
            .last_available_at = DateTime::parse_from_rfc3339("2026-07-14T00:00:08Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            snapshot.validate().unwrap_err(),
            ManifestError::InvalidCexReplaySnapshot("derivatives reference evidence is invalid")
        );
    }

    #[test]
    fn cex_replay_snapshot_rejects_derivatives_gap_over_limit() {
        let mut snapshot = cex_snapshot();
        snapshot
            .derivatives_reference
            .as_mut()
            .unwrap()
            .funding
            .max_gap_ns = CEX_DERIVATIVES_MAX_GAP_NS + 1;

        assert_eq!(
            snapshot.validate().unwrap_err(),
            ManifestError::InvalidCexReplaySnapshot("derivatives reference evidence is invalid")
        );
    }

    #[test]
    fn cex_replay_snapshot_rejects_empty_evidence_set() {
        let mut snapshot = cex_snapshot();
        snapshot.instrument_rules.evidence.clear();

        assert_eq!(
            snapshot.validate().unwrap_err(),
            ManifestError::InvalidCexReplaySnapshot("instrument rules are invalid")
        );
    }

    #[test]
    fn cex_replay_snapshot_rejects_unbound_success_marker() {
        let mut snapshot = cex_snapshot();
        snapshot.instrument_rules.evidence[0].success_sha256 = "9".repeat(64);

        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn cex_replay_snapshot_rejects_noncanonical_symbol() {
        let mut snapshot = cex_snapshot();
        snapshot.symbol = "BTC-USDT".to_string();

        assert_eq!(
            snapshot.validate().unwrap_err(),
            ManifestError::InvalidCexReplaySnapshot("symbol is not canonical")
        );
    }

    #[test]
    fn cex_replay_snapshot_rejects_duplicate_series_evidence() {
        let mut snapshot = cex_snapshot();
        let funding = &mut snapshot.derivatives_reference.as_mut().unwrap().funding;
        funding.evidence[1] = funding.evidence[0].clone();

        assert_eq!(
            snapshot.validate().unwrap_err(),
            ManifestError::InvalidCexReplaySnapshot("derivatives reference evidence is invalid")
        );
    }

    #[test]
    fn cex_replay_snapshot_rejects_fee_expiry_before_last_label() {
        let mut snapshot = historical_cex_snapshot_v3();
        snapshot.fee_schedule.valid_through = snapshot.last_event_time;

        assert_eq!(
            snapshot.validate().unwrap_err(),
            ManifestError::InvalidCexReplaySnapshot("PIT rules or fee evidence is invalid")
        );
    }

    #[test]
    fn cex_replay_snapshot_rejects_impossible_series_coverage() {
        let mut snapshot = cex_snapshot();
        let funding = &mut snapshot.derivatives_reference.as_mut().unwrap().funding;
        funding.observations = 1;

        assert_eq!(
            snapshot.validate().unwrap_err(),
            ManifestError::InvalidCexReplaySnapshot("derivatives reference evidence is invalid")
        );
    }

    #[test]
    fn cex_replay_snapshot_rejects_negative_decimal_underflow() {
        let mut snapshot = historical_cex_snapshot_v3();
        snapshot.fee_schedule.maker_buy_fee_bps = "-1e-400".to_string();

        assert_eq!(
            snapshot.validate().unwrap_err(),
            ManifestError::InvalidCexReplaySnapshot("PIT rules or fee evidence is invalid")
        );
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
