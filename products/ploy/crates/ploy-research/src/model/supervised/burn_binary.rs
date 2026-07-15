//! Pure-Rust Burn binary probability research model.
//!
//! The input contract makes event-disjoint validation and point-in-time cutoffs
//! explicit. The resulting bundle is research-only: this module has no trading,
//! deployment, or promotion authority integration.

use anyhow::{bail, ensure, Context, Result};
use burn::{
    backend::{Autodiff, NdArray},
    module::{AutodiffModule, Initializer, Module, Param},
    nn::{loss::BinaryCrossEntropyLossConfig, Linear, LinearConfig},
    optim::{AdamConfig, GradientsParams, Optimizer},
    tensor::{activation::sigmoid, backend::Backend, Int, Tensor, TensorData},
};
use burn_store::{BurnpackStore, ModuleSnapshot};
use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use crate::{
    prediction_loop::{
        current_prediction_policy_snapshot_id, validate_prediction_mission,
        validate_prediction_snapshot_sources, PredictionResearchMission,
        PREDICTION_EVENT_WINDOW_SECS,
    },
    research_snapshot::{verify_research_snapshot_integrity, ResearchSnapshot},
    FactorObservation,
};

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const BURN_VERSION: &str = "0.20.1";
const MODEL_FAMILY: &str = "burn_linear_logit";
const BACKEND_NAME: &str = "burn_ndarray_f32";
const OPTIMIZER_NAME: &str = "adam_full_batch";
const MODEL_FILE: &str = "model.bpk";
const MANIFEST_FILE: &str = "manifest.json";
const MAX_MANIFEST_BYTES: u64 = 1_048_576;
const MAX_BURNPACK_METADATA_BYTES: usize = 1_048_576;
const BURNPACK_HEADER_BYTES: usize = 10;
const BURNPACK_MAGIC: u32 = 0x4255_524e;
const BURNPACK_FORMAT_VERSION: u16 = 1;
const MAX_FEATURES: usize = 64;
const MAX_BINARY_EPOCHS: usize = 1_000;
const MAX_SELECTORS_PER_PARTITION: usize = 250_000;
const MAX_TOTAL_SELECTORS: usize = 400_000;
const MAX_FEATURE_CELLS: usize = 8_000_000;
const MAX_TOTAL_WORK: usize = 2_000_000_000;
const MAX_SNAPSHOT_OBSERVATIONS: usize = 1_000_000;
const MAX_SNAPSHOT_DERIBIT_ROWS: usize = 2_000_000;
const MAX_SNAPSHOT_BOOK_ROWS: usize = 2_000_000;
const MAX_TOTAL_SNAPSHOT_ROWS: usize = 4_000_000;
const MAX_SNAPSHOT_COVERAGE_WORK: usize = 4_000_000;
const MIN_STANDARD_DEVIATION: f64 = 1.0e-12;

type InferenceBackend = NdArray<f32>;
type TrainingBackend = Autodiff<InferenceBackend>;

fn validate_snapshot_row_budget(
    observations: usize,
    deribit_rows: usize,
    book_rows: usize,
) -> Result<(), String> {
    let total_rows = observations
        .checked_add(deribit_rows)
        .and_then(|rows| rows.checked_add(book_rows))
        .ok_or_else(|| "snapshot row count arithmetic overflow".to_string())?;
    let coverage_work = observations
        .checked_mul(2)
        .and_then(|lookups| lookups.checked_add(book_rows))
        .ok_or_else(|| "snapshot coverage work arithmetic overflow".to_string())?;
    if observations > MAX_SNAPSHOT_OBSERVATIONS {
        return Err(format!(
            "snapshot observation row count exceeds the governed limit of {MAX_SNAPSHOT_OBSERVATIONS}"
        ));
    }
    if deribit_rows > MAX_SNAPSHOT_DERIBIT_ROWS {
        return Err(format!(
            "snapshot Deribit row count exceeds the governed limit of {MAX_SNAPSHOT_DERIBIT_ROWS}"
        ));
    }
    if book_rows > MAX_SNAPSHOT_BOOK_ROWS {
        return Err(format!(
            "snapshot Polymarket book row count exceeds the governed limit of {MAX_SNAPSHOT_BOOK_ROWS}"
        ));
    }
    if total_rows > MAX_TOTAL_SNAPSHOT_ROWS {
        return Err(format!(
            "total snapshot row count exceeds the governed limit of {MAX_TOTAL_SNAPSHOT_ROWS}"
        ));
    }
    if coverage_work > MAX_SNAPSHOT_COVERAGE_WORK {
        return Err(format!(
            "snapshot coverage work exceeds the governed limit of {MAX_SNAPSHOT_COVERAGE_WORK} indexed row operations"
        ));
    }
    Ok(())
}

struct PolymarketEvidenceIndex<'a> {
    book_tokens: BTreeMap<(&'a str, &'static str), &'a str>,
    valid_book_times: BTreeMap<(&'a str, &'static str), Vec<chrono::DateTime<chrono::Utc>>>,
}

impl<'a> PolymarketEvidenceIndex<'a> {
    fn from_snapshot(snapshot: &'a ResearchSnapshot) -> Result<Self, String> {
        let observation_events = snapshot
            .observations
            .iter()
            .map(|row| row.event_id.as_str())
            .collect::<BTreeSet<_>>();
        let mut book_tokens = BTreeMap::new();
        let mut valid_book_times = BTreeMap::<_, Vec<_>>::new();
        for book in &snapshot.pm_book_snapshots {
            if !observation_events.contains(book.event_id.as_str()) {
                return Err(format!(
                    "snapshot contains Polymarket book rows outside the mission event set: {}",
                    book.event_id
                ));
            }
            let side = if book.side.eq_ignore_ascii_case("up") {
                "up"
            } else if book.side.eq_ignore_ascii_case("down") {
                "down"
            } else {
                return Err(format!(
                    "snapshot event {} has unknown Polymarket book side {}",
                    book.event_id, book.side
                ));
            };
            if book.token_id.trim().is_empty() || book.token_id.trim() != book.token_id {
                return Err(format!(
                    "snapshot event {} has an invalid Polymarket {side} token identity",
                    book.event_id
                ));
            }
            let key = (book.event_id.as_str(), side);
            if book_tokens
                .insert(key, book.token_id.as_str())
                .is_some_and(|previous| previous != book.token_id)
            {
                return Err(format!(
                    "snapshot event {} has inconsistent Polymarket {side} token identities",
                    book.event_id
                ));
            }
            if !book.bids.is_empty()
                && !book.asks.is_empty()
                && book.bids.iter().chain(&book.asks).all(|level| {
                    level.price.is_finite()
                        && level.price > 0.0
                        && level.price < 1.0
                        && level.size.is_finite()
                        && level.size > 0.0
                })
            {
                valid_book_times.entry(key).or_default().push(book.ts);
            }
        }
        for timestamps in valid_book_times.values_mut() {
            timestamps.sort_unstable();
        }
        Ok(Self {
            book_tokens,
            valid_book_times,
        })
    }

    fn has_fresh_quote(&self, row: &FactorObservation, max_age_ms: i64) -> bool {
        let max_age_secs = max_age_ms as f64 / 1_000.0;
        row.pm_lag_secs.is_finite()
            && row.pm_lag_secs >= 0.0
            && row.pm_lag_secs <= max_age_secs
            && [
                (
                    row.source_availability.polymarket_up_quote,
                    row.pm_up_bid,
                    row.pm_up_ask,
                ),
                (
                    row.source_availability.polymarket_down_quote,
                    row.pm_down_bid,
                    row.pm_down_ask,
                ),
            ]
            .into_iter()
            .all(|(available_at, bid, ask)| {
                available_at.is_some_and(|available_at| {
                    let age_ms = (row.tick_ts - available_at).num_milliseconds();
                    age_ms >= 0 && age_ms <= max_age_ms && (bid.is_finite() || ask.is_finite())
                })
            })
    }

    fn has_fresh_book(&self, row: &FactorObservation, side: &'static str, max_age_ms: i64) -> bool {
        let Some(timestamps) = self.valid_book_times.get(&(row.event_id.as_str(), side)) else {
            return false;
        };
        let upper = timestamps.partition_point(|timestamp| timestamp <= &row.tick_ts);
        let Some(timestamp) = upper.checked_sub(1).and_then(|index| timestamps.get(index)) else {
            return false;
        };
        let age_ms = (row.tick_ts - *timestamp).num_milliseconds();
        age_ms >= 0 && age_ms <= max_age_ms
    }

    fn validate_decision_row(
        &self,
        partition: &str,
        index: usize,
        row: &FactorObservation,
        max_age_ms: i64,
    ) -> Result<()> {
        ensure!(
            self.has_fresh_quote(row, max_age_ms),
            "{partition} row {index} for event {:?} at {} lacks a fresh matching Polymarket quote",
            row.event_id,
            row.tick_ts
        );
        ensure!(
            self.has_fresh_book(row, "up", max_age_ms)
                && self.has_fresh_book(row, "down", max_age_ms),
            "{partition} row {index} for event {:?} at {} lacks fresh matching nonempty Polymarket UP/DOWN full-depth book evidence",
            row.event_id,
            row.tick_ts
        );
        Ok(())
    }
}

fn validate_binary_snapshot_coverage(
    snapshot: &ResearchSnapshot,
    mission: &PredictionResearchMission,
) -> Result<(), String> {
    let requested = mission
        .symbols
        .first()
        .map(String::as_str)
        .ok_or_else(|| "prediction coverage requires one mission symbol".to_string())?;
    if snapshot.observations.is_empty() {
        return Err("prediction snapshot contains no evaluator observations".to_string());
    }
    if snapshot.manifest.row_counts.observations != snapshot.observations.len()
        || snapshot.manifest.row_counts.deribit_snapshots != snapshot.deribit_snapshots.len()
        || snapshot.manifest.row_counts.pm_book_snapshots != snapshot.pm_book_snapshots.len()
    {
        return Err(
            "snapshot manifest row counts do not match governed evaluator artifacts".to_string(),
        );
    }
    validate_snapshot_row_budget(
        snapshot.observations.len(),
        snapshot.deribit_snapshots.len(),
        snapshot.pm_book_snapshots.len(),
    )?;

    let settlement_evidence = snapshot
        .manifest
        .chainlink_oracle_settlement_evidence
        .iter()
        .map(|evidence| (evidence.event_id.as_str(), evidence))
        .collect::<BTreeMap<_, _>>();
    let polymarket_evidence = PolymarketEvidenceIndex::from_snapshot(snapshot)?;
    let max_age_ms = snapshot
        .manifest
        .max_quote_age_secs
        .max(0)
        .checked_mul(1_000)
        .ok_or_else(|| "snapshot max_quote_age_secs overflows milliseconds".to_string())?;

    let mut labels = BTreeMap::<&str, f64>::new();
    let mut quote_events = BTreeSet::new();
    let mut up_book_events = BTreeSet::new();
    let mut down_book_events = BTreeSet::new();
    for row in &snapshot.observations {
        if crate::factors::normalized_underlying_symbol(&row.symbol) != requested {
            return Err(format!(
                "snapshot observation {} belongs to {}, not isolated mission underlying {requested}",
                row.event_id, row.symbol
            ));
        }
        if row.event_window_secs != PREDICTION_EVENT_WINDOW_SECS {
            return Err(format!(
                "snapshot event {} has {}s horizon; prediction LoopRun requires {}s",
                row.event_id, row.event_window_secs, PREDICTION_EVENT_WINDOW_SECS
            ));
        }
        if !(1..=PREDICTION_EVENT_WINDOW_SECS).contains(&row.time_remaining_secs) {
            return Err(format!(
                "snapshot event {} has invalid decision-time settlement boundary {}s",
                row.event_id, row.time_remaining_secs
            ));
        }
        let evidence = settlement_evidence
            .get(row.event_id.as_str())
            .ok_or_else(|| {
                format!(
                    "snapshot event {} lacks governed settlement evidence",
                    row.event_id
                )
            })?;
        if evidence.label_available_at().is_none() {
            return Err(format!(
                "snapshot event {} lacks official label availability provenance",
                row.event_id
            ));
        }
        if (evidence.end_time - row.tick_ts).num_seconds() != row.time_remaining_secs {
            return Err(format!(
                "snapshot event {} decision-time settlement boundary disagrees with governed Chainlink evidence",
                row.event_id
            ));
        }
        if !matches!(row.settlement_up, 0.0 | 1.0) {
            return Err(format!(
                "snapshot event {} lacks an official binary settlement label",
                row.event_id
            ));
        }
        if labels
            .insert(&row.event_id, row.settlement_up)
            .is_some_and(|previous| previous != row.settlement_up)
        {
            return Err(format!(
                "snapshot event {} has inconsistent official settlement labels",
                row.event_id
            ));
        }
        if !row.chainlink_prob_up.is_finite() || !(0.0..=1.0).contains(&row.chainlink_prob_up) {
            return Err(format!(
                "snapshot observation {} at {} lacks governed Chainlink probability context",
                row.event_id, row.tick_ts
            ));
        }
        if !row.model_prob_up.is_finite() || !(0.0..=1.0).contains(&row.model_prob_up) {
            return Err(format!(
                "snapshot observation {} at {} lacks finite Binance spot probability context",
                row.event_id, row.tick_ts
            ));
        }
        if !row.obi.is_finite()
            || !row.spread_bps.is_finite()
            || !row.bid_depth_near.is_finite()
            || !row.ask_depth_near.is_finite()
            || row.bid_depth_near < 0.0
            || row.ask_depth_near < 0.0
            || row.bid_depth_near + row.ask_depth_near <= 0.0
        {
            return Err(format!(
                "snapshot observation {} at {} lacks finite Binance L2 context",
                row.event_id, row.tick_ts
            ));
        }
        if !row.cum_trade_imbalance_5m.is_finite() {
            return Err(format!(
                "snapshot observation {} at {} lacks finite Binance aggTrade context",
                row.event_id, row.tick_ts
            ));
        }
        if polymarket_evidence.has_fresh_quote(row, max_age_ms) {
            quote_events.insert(row.event_id.as_str());
        }
        if polymarket_evidence.has_fresh_book(row, "up", max_age_ms) {
            up_book_events.insert(row.event_id.as_str());
        }
        if polymarket_evidence.has_fresh_book(row, "down", max_age_ms) {
            down_book_events.insert(row.event_id.as_str());
        }
    }

    for event_id in labels.keys().copied() {
        let up_token = polymarket_evidence
            .book_tokens
            .get(&(event_id, "up"))
            .copied();
        let down_token = polymarket_evidence
            .book_tokens
            .get(&(event_id, "down"))
            .copied();
        if up_token.is_none() || down_token.is_none() || up_token == down_token {
            return Err(format!(
                "snapshot event {event_id} lacks distinct Polymarket UP/DOWN token identities"
            ));
        }
        if !quote_events.contains(event_id)
            || !up_book_events.contains(event_id)
            || !down_book_events.contains(event_id)
        {
            return Err(format!(
                "snapshot event {event_id} lacks fresh matching nonempty Polymarket UP/DOWN quote/full-depth evidence"
            ));
        }
    }
    Ok(())
}

/// Settlement authority accepted by this research lane.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BinarySettlementAuthority {
    OfficialResolution,
}

/// Provenance and feature-order contract for one governed binary dataset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BinaryDatasetContract {
    pub mission_id: String,
    pub mission_sha256: String,
    pub snapshot_hash: String,
    pub snapshot_contract_hash: String,
    pub symbols: Vec<String>,
    pub target_horizon_seconds: u32,
    pub max_feature_age_ms: i64,
    pub settlement_authority: BinarySettlementAuthority,
    pub feature_names: Vec<String>,
    pub feature_schema_sha256: String,
}

impl BinaryDatasetContract {
    /// Bind a five-minute BTC/SOL training projection to a validated prediction
    /// mission and a snapshot returned by [`crate::load_research_snapshot`].
    pub fn from_prediction_snapshot(
        snapshot: &ResearchSnapshot,
        mission: &PredictionResearchMission,
        feature_names: Vec<String>,
    ) -> Result<Self> {
        verify_research_snapshot_integrity(snapshot)
            .context("verify immutable prediction snapshot artifacts")?;
        validate_prediction_mission(mission, &current_prediction_policy_snapshot_id())
            .map_err(anyhow::Error::msg)
            .context("validate prediction mission contract")?;
        validate_prediction_snapshot_sources(&snapshot.manifest, &snapshot.observations)
            .map_err(anyhow::Error::msg)
            .context("validate prediction snapshot sources")?;
        validate_binary_snapshot_coverage(snapshot, mission)
            .map_err(anyhow::Error::msg)
            .context("validate prediction snapshot coverage")?;
        let manifest = &snapshot.manifest;
        ensure!(
            manifest.symbols.len() == 1
                && crate::factors::normalized_underlying_symbol(&manifest.symbols[0])
                    == mission.symbols[0],
            "prediction snapshot manifest must isolate the mission underlying"
        );
        ensure!(
            manifest.immutable_input,
            "binary ML requires an immutable research snapshot"
        );
        ensure!(
            manifest.require_official_settlement,
            "binary ML requires official settlement labels"
        );
        let snapshot_hash = manifest
            .snapshot_hash
            .clone()
            .context("loaded snapshot is missing snapshot_hash")?;
        let snapshot_contract_hash = manifest
            .snapshot_contract_hash
            .clone()
            .context("loaded snapshot is missing snapshot_contract_hash")?;
        ensure!(
            mission.data_snapshot_id == snapshot_contract_hash,
            "prediction mission data_snapshot_id does not match snapshot contract hash"
        );
        let max_feature_age_ms = manifest
            .max_quote_age_secs
            .checked_mul(1_000)
            .context("snapshot max_quote_age_secs overflows milliseconds")?;
        let feature_schema_sha256 = feature_schema_sha256(&feature_names)?;
        let contract = Self {
            mission_id: mission.mission_id.clone(),
            mission_sha256: prefixed_sha256_bytes(
                &serde_json::to_vec(mission).context("serialize prediction mission")?,
            ),
            snapshot_hash,
            snapshot_contract_hash,
            symbols: mission.symbols.clone(),
            target_horizon_seconds: PREDICTION_EVENT_WINDOW_SECS as u32,
            max_feature_age_ms,
            settlement_authority: BinarySettlementAuthority::OfficialResolution,
            feature_names,
            feature_schema_sha256,
        };
        contract.validate()?;
        Ok(contract)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            !self.mission_id.trim().is_empty() && self.mission_id.trim() == self.mission_id,
            "mission_id must be a trimmed non-empty string"
        );
        ensure!(
            is_prefixed_sha256(&self.mission_sha256),
            "mission_sha256 must use sha256:<64 lowercase hex>"
        );
        ensure!(
            is_snapshot_hash(&self.snapshot_hash),
            "snapshot_hash must use the governed 16-character lowercase snapshot digest"
        );
        ensure!(
            is_prefixed_sha256(&self.snapshot_contract_hash),
            "snapshot_contract_hash must use sha256:<64 lowercase hex>"
        );
        ensure!(
            self.symbols.len() == 1 && matches!(self.symbols[0].as_str(), "BTC" | "SOL"),
            "binary prediction dataset must isolate exactly one BTC or SOL symbol"
        );
        let mut symbols = HashSet::with_capacity(self.symbols.len());
        for symbol in &self.symbols {
            ensure!(!symbol.trim().is_empty(), "dataset symbol is empty");
            ensure!(
                symbol.trim() == symbol,
                "dataset symbol contains surrounding whitespace"
            );
            ensure!(symbols.insert(symbol), "duplicate dataset symbol: {symbol}");
        }
        ensure!(
            self.target_horizon_seconds == PREDICTION_EVENT_WINDOW_SECS as u32,
            "binary prediction target horizon must be 300 seconds"
        );
        ensure!(
            self.max_feature_age_ms > 0,
            "max feature age must be positive"
        );
        ensure!(
            self.settlement_authority == BinarySettlementAuthority::OfficialResolution,
            "unsupported settlement authority"
        );
        validate_feature_names(&self.feature_names)?;
        ensure!(
            self.feature_schema_sha256 == feature_schema_sha256(&self.feature_names)?,
            "feature schema SHA-256 does not match ordered feature names"
        );
        Ok(())
    }

    fn validate_mission_binding(&self, mission: &PredictionResearchMission) -> Result<()> {
        let mission_sha256 = prefixed_sha256_bytes(
            &serde_json::to_vec(mission).context("serialize expected prediction mission")?,
        );
        ensure!(
            self.mission_id == mission.mission_id && self.mission_sha256 == mission_sha256,
            "binary dataset contract belongs to a different prediction mission"
        );
        ensure!(
            self.snapshot_contract_hash == mission.data_snapshot_id,
            "binary dataset contract snapshot differs from prediction mission"
        );
        ensure!(
            self.symbols == mission.symbols && mission.horizon == "5m",
            "binary dataset symbol or horizon differs from prediction mission"
        );
        Ok(())
    }
}

/// Selector for one decision-time row in the governed research snapshot.
///
/// This deliberately contains no feature values, clocks, settlement timestamp,
/// or outcome. The trainer materializes all of those fields from the immutable,
/// content-addressed snapshot so callers cannot smuggle labels into features or
/// claim fictitious point-in-time availability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BinaryDecisionRow {
    pub event_id: String,
    pub decision_at_ms: i64,
}

/// Caller-selected, already separated train and validation decision rows.
///
/// The trainer validates that no `event_id` occurs in both partitions. It does
/// not silently re-split rows, and it derives every value from the bound
/// snapshot, which avoids overlapping event-window and caller-input leakage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventDisjointBinarySplit {
    pub contract: BinaryDatasetContract,
    pub train: Vec<BinaryDecisionRow>,
    pub validation: Vec<BinaryDecisionRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct MaterializedBinarySample {
    event_id: String,
    decision_at_ms: i64,
    event_end_at_ms: i64,
    label_available_at_ms: i64,
    features: Vec<f32>,
    outcome: bool,
}

#[derive(Debug, Serialize)]
struct MaterializedBinarySplit<'a> {
    contract: &'a BinaryDatasetContract,
    train: Vec<MaterializedBinarySample>,
    validation: Vec<MaterializedBinarySample>,
}

/// Deterministic full-batch training configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BinaryTrainingConfig {
    pub seed: u64,
    pub epochs: usize,
    pub learning_rate: f64,
    /// Numerical clamp used only when reporting validation log loss.
    pub log_loss_epsilon: f64,
}

impl Default for BinaryTrainingConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            epochs: 250,
            learning_rate: 0.03,
            log_loss_epsilon: 1.0e-7,
        }
    }
}

impl BinaryTrainingConfig {
    fn validate(&self) -> Result<()> {
        ensure!(self.epochs > 0, "epochs must be greater than zero");
        ensure!(
            self.epochs <= MAX_BINARY_EPOCHS,
            "epochs exceeds the governed research limit of {MAX_BINARY_EPOCHS}"
        );
        ensure!(
            self.learning_rate.is_finite() && self.learning_rate > 0.0,
            "learning_rate must be finite and positive"
        );
        ensure!(
            self.log_loss_epsilon.is_finite()
                && self.log_loss_epsilon > 0.0
                && self.log_loss_epsilon < 0.5,
            "log_loss_epsilon must be in (0, 0.5)"
        );
        Ok(())
    }
}

/// Train-only feature normalization parameters stored with the bundle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeatureNormalizer {
    pub means: Vec<f64>,
    pub standard_deviations: Vec<f64>,
}

impl FeatureNormalizer {
    fn fit(samples: &[MaterializedBinarySample], input_dim: usize) -> Self {
        let count = samples.len() as f64;
        let mut means = vec![0.0; input_dim];
        for sample in samples {
            for (index, value) in sample.features.iter().enumerate() {
                means[index] += f64::from(*value);
            }
        }
        for mean in &mut means {
            *mean /= count;
        }

        let mut variances = vec![0.0; input_dim];
        for sample in samples {
            for (index, value) in sample.features.iter().enumerate() {
                let centered = f64::from(*value) - means[index];
                variances[index] += centered * centered;
            }
        }
        let standard_deviations = variances
            .into_iter()
            .map(|sum| {
                let standard_deviation = (sum / count).sqrt();
                if standard_deviation < MIN_STANDARD_DEVIATION {
                    1.0
                } else {
                    standard_deviation
                }
            })
            .collect();

        Self {
            means,
            standard_deviations,
        }
    }

    fn validate(&self, input_dim: usize) -> Result<()> {
        ensure!(
            self.means.len() == input_dim && self.standard_deviations.len() == input_dim,
            "normalizer dimension does not match feature schema"
        );
        ensure!(
            self.means.iter().all(|value| value.is_finite()),
            "normalizer contains a non-finite mean"
        );
        ensure!(
            self.standard_deviations
                .iter()
                .all(|value| value.is_finite() && *value > 0.0),
            "normalizer contains an invalid standard deviation"
        );
        Ok(())
    }

    fn transform(&self, features: &[f32]) -> Result<Vec<f32>> {
        ensure!(
            features.len() == self.means.len(),
            "feature width {} does not match model width {}",
            features.len(),
            self.means.len()
        );
        let mut normalized = Vec::with_capacity(features.len());
        for (index, value) in features.iter().enumerate() {
            ensure!(value.is_finite(), "feature {index} is not finite");
            let value =
                ((f64::from(*value) - self.means[index]) / self.standard_deviations[index]) as f32;
            ensure!(
                value.is_finite(),
                "normalized feature {index} is not finite"
            );
            normalized.push(value);
        }
        Ok(normalized)
    }
}

/// Metrics computed once, out of sample, after training completes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BinaryOosMetrics {
    pub sample_count: usize,
    pub event_count: usize,
    pub brier_score: f64,
    pub log_loss: f64,
    pub accuracy: f64,
}

impl BinaryOosMetrics {
    fn validate(&self) -> Result<()> {
        ensure!(self.sample_count > 0, "OOS sample count must be positive");
        ensure!(self.event_count > 0, "OOS event count must be positive");
        ensure!(
            self.brier_score.is_finite() && (0.0..=1.0).contains(&self.brier_score),
            "invalid Brier score in manifest"
        );
        ensure!(
            self.log_loss.is_finite() && self.log_loss >= 0.0,
            "invalid log loss in manifest"
        );
        ensure!(
            self.accuracy.is_finite() && (0.0..=1.0).contains(&self.accuracy),
            "invalid accuracy in manifest"
        );
        Ok(())
    }
}

/// Explicitly non-authoritative artifact scope.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BinaryArtifactScope {
    ResearchOnly,
}

/// Typed JSON sidecar for a Burnpack model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BinaryModelManifest {
    pub schema_version: u32,
    pub artifact_scope: BinaryArtifactScope,
    pub model_family: String,
    pub backend: String,
    pub burn_version: String,
    pub optimizer: String,
    pub dataset_contract: BinaryDatasetContract,
    pub normalizer: FeatureNormalizer,
    pub training: BinaryTrainingConfig,
    pub dataset_sha256: String,
    pub train_sample_count: usize,
    pub train_event_count: usize,
    pub validation_metrics: BinaryOosMetrics,
    pub model_file: String,
    /// `None` only while the in-memory model has not yet been persisted.
    pub model_sha256: Option<String>,
}

/// Digests that must be stored in a trusted research registry after persistence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BinaryBundleDigest {
    pub manifest_sha256: String,
    pub model_sha256: String,
}

impl BinaryBundleDigest {
    fn validate(&self) -> Result<()> {
        ensure!(
            is_sha256(&self.manifest_sha256),
            "expected manifest SHA-256 is invalid"
        );
        ensure!(
            is_sha256(&self.model_sha256),
            "expected Burnpack SHA-256 is invalid"
        );
        Ok(())
    }
}

impl BinaryModelManifest {
    fn validate(&self, persisted: bool) -> Result<()> {
        ensure!(
            self.schema_version == MANIFEST_SCHEMA_VERSION,
            "unsupported binary model manifest schema {}",
            self.schema_version
        );
        ensure!(
            self.artifact_scope == BinaryArtifactScope::ResearchOnly,
            "binary model artifact is not research-only"
        );
        ensure!(
            self.model_family == MODEL_FAMILY,
            "unsupported model family"
        );
        ensure!(self.backend == BACKEND_NAME, "unsupported Burn backend");
        ensure!(
            self.burn_version == BURN_VERSION,
            "unsupported Burn artifact version"
        );
        ensure!(self.optimizer == OPTIMIZER_NAME, "unsupported optimizer");
        self.dataset_contract.validate()?;
        self.normalizer
            .validate(self.dataset_contract.feature_names.len())?;
        self.training.validate()?;
        ensure!(
            is_sha256(&self.dataset_sha256),
            "invalid dataset SHA-256 in manifest"
        );
        ensure!(
            self.train_sample_count > 0 && self.train_event_count > 0,
            "training counts must be positive"
        );
        self.validation_metrics.validate()?;
        ensure!(self.model_file == MODEL_FILE, "unexpected model file path");
        if persisted {
            ensure!(
                self.model_sha256.as_deref().is_some_and(is_sha256),
                "persisted manifest is missing a valid model SHA-256"
            );
        } else if let Some(hash) = &self.model_sha256 {
            ensure!(is_sha256(hash), "invalid model SHA-256 in manifest");
        }
        Ok(())
    }
}

#[derive(Module, Debug)]
struct BurnBinaryLinear<B: Backend> {
    projection: Linear<B>,
}

impl<B: Backend> BurnBinaryLinear<B> {
    fn forward(&self, features: Tensor<B, 2>) -> Tensor<B, 2> {
        self.projection.forward(features)
    }

    fn zeros(input_dim: usize, device: &B::Device) -> Self {
        Self {
            projection: LinearConfig::new(input_dim, 1)
                .with_initializer(Initializer::Zeros)
                .init(device),
        }
    }
}

impl BurnBinaryLinear<TrainingBackend> {
    fn from_seed(
        input_dim: usize,
        seed: u64,
        device: &<TrainingBackend as Backend>::Device,
    ) -> Self {
        // Generate weights from a local RNG so determinism does not depend on a
        // process-global backend RNG or concurrent research jobs.
        let mut rng = StdRng::seed_from_u64(seed);
        let bound = (1.0 / input_dim as f32).sqrt();
        let weights = (0..input_dim)
            .map(|_| rng.gen_range(-bound..bound))
            .collect::<Vec<f32>>();
        let bias = vec![rng.gen_range(-bound..bound)];
        Self {
            projection: Linear {
                weight: Param::from_data(TensorData::new(weights, [input_dim, 1]), device),
                bias: Some(Param::from_data(TensorData::new(bias, [1]), device)),
            },
        }
    }
}

/// Trained research model and its evidence manifest.
///
/// This type intentionally does not implement `StrategyModel`, `SignalSource`,
/// deployment, or promotion traits.
#[derive(Debug)]
pub struct BinaryProbabilityModel {
    model: BurnBinaryLinear<InferenceBackend>,
    manifest: BinaryModelManifest,
}

impl BinaryProbabilityModel {
    pub fn manifest(&self) -> &BinaryModelManifest {
        &self.manifest
    }

    /// Produce probabilities only from exact decision rows in the immutable,
    /// governed snapshot bound to this model's dataset contract.
    pub fn predict_probabilities(
        &self,
        snapshot: &ResearchSnapshot,
        mission: &PredictionResearchMission,
        selectors: &[BinaryDecisionRow],
    ) -> Result<Vec<f64>> {
        ensure!(!selectors.is_empty(), "prediction batch is empty");
        validate_selector_partition("prediction", selectors)?;
        validate_prediction_budget(
            selectors.len(),
            self.manifest.dataset_contract.feature_names.len(),
        )?;
        self.manifest
            .dataset_contract
            .validate_mission_binding(mission)?;
        let expected_contract = BinaryDatasetContract::from_prediction_snapshot(
            snapshot,
            mission,
            self.manifest.dataset_contract.feature_names.clone(),
        )?;
        ensure!(
            self.manifest.dataset_contract == expected_contract,
            "prediction snapshot contract does not match the trained model"
        );
        let governed_rows = governed_observation_index(snapshot)?;
        let polymarket_evidence = PolymarketEvidenceIndex::from_snapshot(snapshot)
            .map_err(anyhow::Error::msg)
            .context("index prediction Polymarket evidence")?;
        let feature_rows = materialize_governed_feature_rows(
            "prediction",
            selectors,
            &self.manifest.dataset_contract,
            &governed_rows,
            &polymarket_evidence,
        )?;
        predict_feature_rows(&self.model, &self.manifest.normalizer, &feature_rows)
    }

    /// Persist a Burnpack plus typed JSON manifest in a new directory.
    /// Existing directories are never overwritten.
    pub fn save_bundle(&mut self, bundle_dir: &Path) -> Result<BinaryBundleDigest> {
        self.manifest.validate(false)?;
        ensure!(
            !bundle_dir.exists(),
            "refusing to overwrite existing model bundle {}",
            bundle_dir.display()
        );
        fs::create_dir(bundle_dir)
            .with_context(|| format!("create binary model bundle {}", bundle_dir.display()))?;

        let write_result = self.write_bundle(bundle_dir);
        if write_result.is_err() {
            let _ = fs::remove_dir_all(bundle_dir);
        }
        write_result
    }

    fn write_bundle(&mut self, bundle_dir: &Path) -> Result<BinaryBundleDigest> {
        let model_path = bundle_dir.join(MODEL_FILE);
        let mut store = BurnpackStore::from_file(&model_path)
            .auto_extension(false)
            .metadata("artifact_scope", "research_only")
            .metadata("dataset_sha256", self.manifest.dataset_sha256.clone())
            .metadata(
                "snapshot_contract_hash",
                self.manifest
                    .dataset_contract
                    .snapshot_contract_hash
                    .clone(),
            )
            .metadata(
                "mission_sha256",
                self.manifest.dataset_contract.mission_sha256.clone(),
            )
            .metadata(
                "feature_schema_sha256",
                self.manifest.dataset_contract.feature_schema_sha256.clone(),
            )
            .metadata("manifest_schema", MANIFEST_SCHEMA_VERSION.to_string());
        self.model
            .save_into(&mut store)
            .context("write Burnpack model")?;

        let mut persisted_manifest = self.manifest.clone();
        persisted_manifest.model_sha256 = Some(sha256_file(&model_path)?);
        persisted_manifest.validate(true)?;

        let manifest_path = bundle_dir.join(MANIFEST_FILE);
        let mut manifest_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&manifest_path)
            .with_context(|| format!("create {}", manifest_path.display()))?;
        let manifest_bytes = serde_json::to_vec_pretty(&persisted_manifest)
            .context("serialize binary model manifest")?;
        manifest_file
            .write_all(&manifest_bytes)
            .with_context(|| format!("write {}", manifest_path.display()))?;
        manifest_file
            .sync_all()
            .with_context(|| format!("sync {}", manifest_path.display()))?;

        let digest = BinaryBundleDigest {
            manifest_sha256: sha256_file(&manifest_path)?,
            model_sha256: persisted_manifest
                .model_sha256
                .clone()
                .expect("persisted manifest validation requires model SHA-256"),
        };
        self.manifest = persisted_manifest;
        Ok(digest)
    }

    /// Load a bundle fail-closed. `expected_bundle` must come from a trusted
    /// research registry, not from another file inside `bundle_dir`.
    pub fn load_bundle(
        bundle_dir: &Path,
        expected_bundle: &BinaryBundleDigest,
        expected_mission: &PredictionResearchMission,
    ) -> Result<Self> {
        expected_bundle.validate()?;
        let manifest_path = bundle_dir.join(MANIFEST_FILE);
        let metadata = fs::metadata(&manifest_path)
            .with_context(|| format!("stat {}", manifest_path.display()))?;
        ensure!(
            metadata.len() <= MAX_MANIFEST_BYTES,
            "binary model manifest exceeds size limit"
        );
        ensure!(
            sha256_file(&manifest_path)? == expected_bundle.manifest_sha256,
            "manifest SHA-256 does not match trusted registry digest"
        );
        let manifest_bytes = fs::read(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let manifest: BinaryModelManifest =
            serde_json::from_slice(&manifest_bytes).context("parse binary model manifest")?;
        manifest.validate(true)?;
        manifest
            .dataset_contract
            .validate_mission_binding(expected_mission)?;

        let model_path = bundle_dir.join(&manifest.model_file);
        let actual_hash = sha256_file(&model_path)?;
        ensure!(
            actual_hash == expected_bundle.model_sha256,
            "Burnpack SHA-256 does not match trusted registry digest"
        );
        ensure!(
            manifest.model_sha256.as_deref() == Some(actual_hash.as_str()),
            "Burnpack SHA-256 does not match manifest"
        );
        validate_burnpack_metadata(&model_path, &manifest)?;

        let device = Default::default();
        let mut model = BurnBinaryLinear::<InferenceBackend>::zeros(
            manifest.dataset_contract.feature_names.len(),
            &device,
        );
        let mut store = BurnpackStore::from_file(&model_path).auto_extension(false);
        let applied = model.load_from(&mut store).context("load Burnpack model")?;
        ensure!(
            applied.is_success(),
            "Burnpack did not fully apply to the binary model: {applied:?}"
        );

        Ok(Self { model, manifest })
    }
}

/// Train a deterministic Burn binary probability model and evaluate it once on
/// the caller-selected event-disjoint validation partition. Feature values and
/// labels are always materialized from the bound governed snapshot.
pub fn train_event_disjoint_binary(
    snapshot: &ResearchSnapshot,
    mission: &PredictionResearchMission,
    split: &EventDisjointBinarySplit,
    config: BinaryTrainingConfig,
) -> Result<BinaryProbabilityModel> {
    config.validate()?;
    validate_total_selector_budget(split)?;
    validate_selector_partition("train", &split.train)?;
    validate_selector_partition("validation", &split.validation)?;
    split.contract.validate()?;
    split.contract.validate_mission_binding(mission)?;
    let input_dim = split.contract.feature_names.len();
    validate_training_budget(
        split.train.len(),
        split.validation.len(),
        input_dim,
        config.epochs,
    )?;
    let expected_contract = BinaryDatasetContract::from_prediction_snapshot(
        snapshot,
        mission,
        split.contract.feature_names.clone(),
    )?;
    ensure!(
        split.contract == expected_contract,
        "binary dataset contract does not match the validated prediction snapshot"
    );
    let materialized = materialize_split(split, snapshot)?;
    validate_split(&materialized, input_dim)?;

    let normalizer = FeatureNormalizer::fit(&materialized.train, input_dim);
    normalizer.validate(input_dim)?;
    let train_features = normalized_flattened_features(&materialized.train, &normalizer)?;
    let train_labels = materialized
        .train
        .iter()
        .map(|sample| i64::from(sample.outcome))
        .collect::<Vec<i64>>();

    let device = Default::default();
    TrainingBackend::seed(&device, config.seed);
    let mut model = BurnBinaryLinear::<TrainingBackend>::from_seed(input_dim, config.seed, &device);
    let features = Tensor::<TrainingBackend, 2>::from_data(
        TensorData::new(train_features, [materialized.train.len(), input_dim]),
        &device,
    );
    let labels = Tensor::<TrainingBackend, 2, Int>::from_data(
        TensorData::new(train_labels, [materialized.train.len(), 1]),
        &device,
    );
    let loss = BinaryCrossEntropyLossConfig::new()
        .with_logits(true)
        .init(&device);
    let mut optimizer = AdamConfig::new().init();

    // Validation data is deliberately not materialized or read in this loop.
    for _ in 0..config.epochs {
        let logits = model.forward(features.clone());
        let objective = loss.forward(logits, labels.clone());
        let gradients = GradientsParams::from_grads(objective.backward(), &model);
        model = optimizer.step(config.learning_rate, model, gradients);
    }

    let model = model.valid();
    let validation_rows = materialized
        .validation
        .iter()
        .map(|sample| sample.features.clone())
        .collect::<Vec<_>>();
    let validation_probabilities = predict_feature_rows(&model, &normalizer, &validation_rows)?;
    let validation_metrics = compute_metrics(
        &materialized.validation,
        &validation_probabilities,
        config.log_loss_epsilon,
    )?;

    let manifest = BinaryModelManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        artifact_scope: BinaryArtifactScope::ResearchOnly,
        model_family: MODEL_FAMILY.to_owned(),
        backend: BACKEND_NAME.to_owned(),
        burn_version: BURN_VERSION.to_owned(),
        optimizer: OPTIMIZER_NAME.to_owned(),
        dataset_contract: split.contract.clone(),
        normalizer,
        training: config,
        dataset_sha256: sha256_bytes(
            &serde_json::to_vec(&materialized).context("serialize governed binary split")?,
        ),
        train_sample_count: materialized.train.len(),
        train_event_count: event_count(&materialized.train),
        validation_metrics,
        model_file: MODEL_FILE.to_owned(),
        model_sha256: None,
    };
    manifest.validate(false)?;

    Ok(BinaryProbabilityModel { model, manifest })
}

fn validate_total_selector_budget(split: &EventDisjointBinarySplit) -> Result<()> {
    let total = split
        .train
        .len()
        .checked_add(split.validation.len())
        .context("total selector count overflow")?;
    ensure!(
        total <= MAX_TOTAL_SELECTORS,
        "total selector count exceeds the governed limit of {MAX_TOTAL_SELECTORS}"
    );
    Ok(())
}

fn checked_feature_cells(rows: usize, features: usize, scope: &str) -> Result<usize> {
    rows.checked_mul(features)
        .with_context(|| format!("{scope} feature cell count overflow"))
}

fn validate_training_budget(
    train_rows: usize,
    validation_rows: usize,
    features: usize,
    epochs: usize,
) -> Result<()> {
    let total_rows = train_rows
        .checked_add(validation_rows)
        .context("total training row count overflow")?;
    let total_cells = checked_feature_cells(total_rows, features, "total")?;
    ensure!(
        total_cells <= MAX_FEATURE_CELLS,
        "feature cell count exceeds the governed limit of {MAX_FEATURE_CELLS}"
    );

    let train_cells = checked_feature_cells(train_rows, features, "training")?;
    let optimization_work = train_cells
        .checked_mul(epochs)
        .context("training epoch work overflow")?;
    let validation_cells = checked_feature_cells(validation_rows, features, "validation")?;
    let total_work = optimization_work
        .checked_add(validation_cells)
        .context("total training work overflow")?;
    ensure!(
        total_work <= MAX_TOTAL_WORK,
        "total training work exceeds the governed limit of {MAX_TOTAL_WORK} feature-cell steps"
    );
    Ok(())
}

fn validate_prediction_budget(rows: usize, features: usize) -> Result<()> {
    let cells = checked_feature_cells(rows, features, "prediction")?;
    ensure!(
        cells <= MAX_FEATURE_CELLS,
        "prediction feature cell count exceeds the governed limit of {MAX_FEATURE_CELLS}"
    );
    Ok(())
}

fn validate_selector_partition(partition: &str, selectors: &[BinaryDecisionRow]) -> Result<()> {
    ensure!(
        selectors.len() <= MAX_SELECTORS_PER_PARTITION,
        "{partition} selector count exceeds the governed limit of {MAX_SELECTORS_PER_PARTITION}"
    );
    let mut unique = HashSet::with_capacity(selectors.len());
    for (index, selector) in selectors.iter().enumerate() {
        ensure!(
            !selector.event_id.trim().is_empty() && selector.event_id.trim() == selector.event_id,
            "{partition} selector {index} has an invalid event_id"
        );
        ensure!(
            unique.insert((selector.event_id.as_str(), selector.decision_at_ms)),
            "{partition} contains duplicate decision selector {:?} at {}",
            selector.event_id,
            selector.decision_at_ms
        );
    }
    Ok(())
}

fn validate_feature_names(feature_names: &[String]) -> Result<()> {
    ensure!(!feature_names.is_empty(), "feature schema is empty");
    ensure!(
        feature_names.len() <= MAX_FEATURES,
        "feature schema exceeds {MAX_FEATURES} columns"
    );
    let mut unique = HashSet::with_capacity(feature_names.len());
    for name in feature_names {
        ensure!(!name.trim().is_empty(), "feature name is empty");
        ensure!(
            name.trim() == name,
            "feature name contains surrounding whitespace: {name:?}"
        );
        ensure!(
            unique.insert(name.as_str()),
            "duplicate feature name: {name}"
        );
        ensure!(
            registered_feature_accessor(name).is_some(),
            "feature {name:?} is not a registered point-in-time binary feature"
        );
    }
    Ok(())
}

type RegisteredFeatureAccessor = fn(&FactorObservation) -> f64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeatureSource {
    Spot,
    Lob,
    AggregateTrade,
    PolymarketUpQuote,
    PolymarketDownQuote,
    ChainlinkReference,
    ChainlinkOpen,
}

impl FeatureSource {
    fn name(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Lob => "lob",
            Self::AggregateTrade => "aggregate_trade",
            Self::PolymarketUpQuote => "polymarket_up_quote",
            Self::PolymarketDownQuote => "polymarket_down_quote",
            Self::ChainlinkReference => "chainlink_reference",
            Self::ChainlinkOpen => "chainlink_open",
        }
    }

    fn available_at(self, row: &FactorObservation) -> Option<chrono::DateTime<chrono::Utc>> {
        match self {
            Self::Spot => row.source_availability.spot,
            Self::Lob => row.source_availability.lob,
            Self::AggregateTrade => row.source_availability.aggregate_trade,
            Self::PolymarketUpQuote => row.source_availability.polymarket_up_quote,
            Self::PolymarketDownQuote => row.source_availability.polymarket_down_quote,
            Self::ChainlinkReference => row.source_availability.chainlink_reference,
            Self::ChainlinkOpen => row.source_availability.chainlink_open,
        }
    }

    fn is_event_static(self) -> bool {
        self == Self::ChainlinkOpen
    }
}

/// Closed registry of decision-time fields that the binary trainer is allowed
/// to project from a governed snapshot. Settlement and `future_*` fields are
/// intentionally absent.
fn registered_feature_accessor(name: &str) -> Option<RegisteredFeatureAccessor> {
    match name {
        "time_remaining_secs" => Some(|row| row.time_remaining_secs as f64),
        "signed_distance_to_beat" => Some(|row| row.signed_distance_to_beat),
        "abs_distance_to_beat" => Some(|row| row.abs_distance_to_beat),
        "drift_10s" => Some(|row| row.drift_10s),
        "drift_30s" => Some(|row| row.drift_30s),
        "flip_age_secs" => Some(|row| row.flip_age_secs),
        "post_flip_drift" => Some(|row| row.post_flip_drift),
        "sigma_horizon" => Some(|row| row.sigma_horizon),
        "fair_prob_up" => Some(|row| row.fair_prob_up),
        "fair_prob_up_clean" => Some(|row| row.fair_prob_up_clean),
        "prob_disagreement" => Some(|row| row.prob_disagreement),
        "implied_sigma_horizon" => Some(|row| row.implied_sigma_horizon),
        "vol_gap" => Some(|row| row.vol_gap),
        "distance_over_sigma" => Some(|row| row.distance_over_sigma),
        "model_prob_up" => Some(|row| row.model_prob_up),
        "chainlink_prob_up" => Some(|row| row.chainlink_prob_up),
        "model_edge_up" => Some(|row| row.model_edge_up),
        "reward_risk_up" => Some(|row| row.reward_risk_up),
        "reward_risk_down" => Some(|row| row.reward_risk_down),
        "obi" => Some(|row| row.obi),
        "spread_bps" => Some(|row| row.spread_bps),
        "microprice_offset_bps" => Some(|row| row.microprice_offset_bps),
        "bid_depth_near" => Some(|row| row.bid_depth_near),
        "ask_depth_near" => Some(|row| row.ask_depth_near),
        "depth_ratio" => Some(|row| row.depth_ratio),
        "depth_imbalance" => Some(|row| row.depth_imbalance),
        "depth_far_ratio" => Some(|row| row.depth_far_ratio),
        "depth_acceleration" => Some(|row| row.depth_acceleration),
        "obi_10" => Some(|row| row.obi_10),
        "pm_up_bid" => Some(|row| row.pm_up_bid),
        "pm_up_ask" => Some(|row| row.pm_up_ask),
        "pm_up_bid_size" => Some(|row| row.pm_up_bid_size),
        "pm_up_ask_size" => Some(|row| row.pm_up_ask_size),
        "pm_down_bid" => Some(|row| row.pm_down_bid),
        "pm_down_ask" => Some(|row| row.pm_down_ask),
        "pm_down_bid_size" => Some(|row| row.pm_down_bid_size),
        "pm_down_ask_size" => Some(|row| row.pm_down_ask_size),
        "pm_lag_secs" => Some(|row| row.pm_lag_secs),
        "cum_obi_delta_5m" => Some(|row| row.cum_obi_delta_5m),
        "cum_depth_delta_5m" => Some(|row| row.cum_depth_delta_5m),
        "cum_mprice_drift_5m" => Some(|row| row.cum_mprice_drift_5m),
        "cum_trade_imbalance_5m" => Some(|row| row.cum_trade_imbalance_5m),
        "cex_bar_return_30s" => Some(|row| row.cex_bar_return_30s),
        "cex_bar_return_60s" => Some(|row| row.cex_bar_return_60s),
        "cex_bar_volume_ratio_30s" => Some(|row| row.cex_bar_volume_ratio_30s),
        "cex_bar_volume_trend_3" => Some(|row| row.cex_bar_volume_trend_3),
        "cex_signed_volume_ratio_30s" => Some(|row| row.cex_signed_volume_ratio_30s),
        "cex_consecutive_up_bars" => Some(|row| row.cex_consecutive_up_bars),
        "cex_consecutive_down_bars" => Some(|row| row.cex_consecutive_down_bars),
        "cex_breakout_volume_score" => Some(|row| row.cex_breakout_volume_score),
        _ => None,
    }
}

fn registered_feature_sources(name: &str) -> Option<&'static [FeatureSource]> {
    use FeatureSource::{
        AggregateTrade, ChainlinkOpen, ChainlinkReference, Lob, PolymarketDownQuote,
        PolymarketUpQuote, Spot,
    };
    match name {
        "time_remaining_secs" => Some(&[]),
        "signed_distance_to_beat"
        | "abs_distance_to_beat"
        | "distance_over_sigma"
        | "model_prob_up" => Some(&[Spot, ChainlinkOpen]),
        "drift_10s" | "drift_30s" | "flip_age_secs" | "post_flip_drift" | "sigma_horizon" => {
            Some(&[Spot])
        }
        "fair_prob_up" | "fair_prob_up_clean" | "prob_disagreement" | "reward_risk_up"
        | "reward_risk_down" | "pm_lag_secs" => Some(&[PolymarketUpQuote, PolymarketDownQuote]),
        "implied_sigma_horizon" | "vol_gap" | "model_edge_up" => {
            Some(&[Spot, ChainlinkOpen, PolymarketUpQuote, PolymarketDownQuote])
        }
        "chainlink_prob_up" => Some(&[Spot, ChainlinkOpen, ChainlinkReference]),
        "obi"
        | "spread_bps"
        | "microprice_offset_bps"
        | "bid_depth_near"
        | "ask_depth_near"
        | "depth_ratio"
        | "depth_imbalance"
        | "depth_far_ratio"
        | "depth_acceleration"
        | "obi_10"
        | "cum_obi_delta_5m"
        | "cum_depth_delta_5m"
        | "cum_mprice_drift_5m" => Some(&[Lob]),
        "pm_up_bid" | "pm_up_ask" | "pm_up_bid_size" | "pm_up_ask_size" => {
            Some(&[PolymarketUpQuote])
        }
        "pm_down_bid" | "pm_down_ask" | "pm_down_bid_size" | "pm_down_ask_size" => {
            Some(&[PolymarketDownQuote])
        }
        "cum_trade_imbalance_5m" => Some(&[AggregateTrade]),
        "cex_bar_return_30s"
        | "cex_bar_return_60s"
        | "cex_bar_volume_ratio_30s"
        | "cex_bar_volume_trend_3"
        | "cex_signed_volume_ratio_30s"
        | "cex_consecutive_up_bars"
        | "cex_consecutive_down_bars"
        | "cex_breakout_volume_score" => Some(&[Spot, AggregateTrade]),
        _ => None,
    }
}

fn validate_feature_freshness(
    row: &FactorObservation,
    name: &str,
    max_feature_age_ms: i64,
) -> Result<()> {
    let sources = registered_feature_sources(name)
        .with_context(|| format!("feature {name:?} has no registered source clock"))?;
    for source in sources {
        let source_name = source.name();
        let available_at = source.available_at(row).with_context(|| {
            format!(
                "snapshot feature {name:?} is missing {source_name} availability for event {:?} at {}",
                row.event_id, row.tick_ts
            )
        })?;
        ensure!(
            available_at <= row.tick_ts,
            "snapshot feature {name:?} uses future {source_name} availability for event {:?} at {}",
            row.event_id,
            row.tick_ts
        );
        if !source.is_event_static() {
            let age_ms = (row.tick_ts - available_at).num_milliseconds();
            ensure!(
                age_ms <= max_feature_age_ms,
                "snapshot feature {name:?} {source_name} source is stale by {age_ms}ms for event {:?} at {} (max {max_feature_age_ms}ms)",
                row.event_id,
                row.tick_ts
            );
        }
    }
    Ok(())
}

fn extract_registered_feature(
    row: &FactorObservation,
    name: &str,
    max_feature_age_ms: i64,
) -> Result<f32> {
    let accessor = registered_feature_accessor(name)
        .with_context(|| format!("feature {name:?} is not registered for binary research"))?;
    validate_feature_freshness(row, name, max_feature_age_ms)?;
    let value = accessor(row);
    ensure!(
        value.is_finite(),
        "snapshot feature {name:?} is non-finite for event {:?} at {}",
        row.event_id,
        row.tick_ts
    );
    let value = value as f32;
    ensure!(
        value.is_finite(),
        "snapshot feature {name:?} exceeds f32 range for event {:?} at {}",
        row.event_id,
        row.tick_ts
    );
    Ok(value)
}

fn validate_split(split: &MaterializedBinarySplit<'_>, input_dim: usize) -> Result<()> {
    ensure!(!split.train.is_empty(), "training partition is empty");
    ensure!(
        !split.validation.is_empty(),
        "validation partition is empty"
    );
    let train_events = validate_partition("train", &split.train, input_dim)?;
    let validation_events = validate_partition("validation", &split.validation, input_dim)?;
    if let Some(overlap) = train_events.intersection(&validation_events).next() {
        bail!("event-disjoint split violation: event {overlap:?} appears in train and validation");
    }
    let latest_training_settlement = split
        .train
        .iter()
        .map(|sample| sample.label_available_at_ms)
        .max()
        .expect("non-empty training partition was checked above");
    let earliest_validation_decision = split
        .validation
        .iter()
        .map(|sample| sample.decision_at_ms)
        .min()
        .expect("non-empty validation partition was checked above");
    ensure!(
        latest_training_settlement <= earliest_validation_decision,
        "OOS cutoff violation: a training event settles after validation decisions begin"
    );
    ensure!(
        split.train.iter().any(|sample| sample.outcome)
            && split.train.iter().any(|sample| !sample.outcome),
        "training partition must contain both binary outcomes"
    );
    Ok(())
}

fn validate_partition<'a>(
    partition: &str,
    samples: &'a [MaterializedBinarySample],
    input_dim: usize,
) -> Result<HashSet<&'a str>> {
    let mut events = HashSet::new();
    let mut event_contracts: HashMap<&str, (i64, i64, bool)> = HashMap::new();
    for (row, sample) in samples.iter().enumerate() {
        ensure!(
            !sample.event_id.trim().is_empty(),
            "{partition} row {row} has an empty event_id"
        );
        ensure!(
            sample.event_id.trim() == sample.event_id,
            "{partition} row {row} event_id contains surrounding whitespace"
        );
        ensure!(
            sample.features.len() == input_dim,
            "{partition} row {row} feature width {} does not match schema width {input_dim}",
            sample.features.len()
        );
        ensure!(
            sample.features.iter().all(|value| value.is_finite()),
            "{partition} row {row} contains a non-finite feature"
        );
        ensure!(
            sample.decision_at_ms < sample.event_end_at_ms,
            "settlement cutoff violation in {partition} row {row}: decision is not before settlement"
        );
        let event_id = sample.event_id.as_str();
        if let Some((event_end_at_ms, label_available_at_ms, outcome)) =
            event_contracts.get(event_id)
        {
            ensure!(
                *event_end_at_ms == sample.event_end_at_ms
                    && *label_available_at_ms == sample.label_available_at_ms
                    && *outcome == sample.outcome,
                "inconsistent settlement contract for event {event_id:?} in {partition}"
            );
        } else {
            event_contracts.insert(
                event_id,
                (
                    sample.event_end_at_ms,
                    sample.label_available_at_ms,
                    sample.outcome,
                ),
            );
        }
        events.insert(event_id);
    }
    Ok(events)
}

type GovernedObservationIndex<'a> = HashMap<(&'a str, i64), &'a FactorObservation>;

fn governed_observation_index(snapshot: &ResearchSnapshot) -> Result<GovernedObservationIndex<'_>> {
    let mut governed_rows = HashMap::with_capacity(snapshot.observations.len());
    for row in &snapshot.observations {
        let decision_at_ms = row.tick_ts.timestamp_millis();
        ensure!(
            governed_rows
                .insert((row.event_id.as_str(), decision_at_ms), row)
                .is_none(),
            "bound prediction snapshot contains duplicate decision row {:?} at {decision_at_ms}",
            row.event_id
        );
    }
    Ok(governed_rows)
}

fn materialize_governed_feature_row(
    partition: &str,
    index: usize,
    selector: &BinaryDecisionRow,
    contract: &BinaryDatasetContract,
    governed_rows: &GovernedObservationIndex<'_>,
    polymarket_evidence: &PolymarketEvidenceIndex<'_>,
) -> Result<Vec<f32>> {
    ensure!(
        !selector.event_id.trim().is_empty() && selector.event_id.trim() == selector.event_id,
        "{partition} row {index} has an invalid event_id"
    );
    let snapshot_row = governed_rows
        .get(&(selector.event_id.as_str(), selector.decision_at_ms))
        .copied()
        .with_context(|| {
            format!(
                "{partition} row {index} is not an exact decision row in the bound prediction snapshot"
            )
        })?;
    polymarket_evidence.validate_decision_row(
        partition,
        index,
        snapshot_row,
        contract.max_feature_age_ms,
    )?;
    contract
        .feature_names
        .iter()
        .map(|name| extract_registered_feature(snapshot_row, name, contract.max_feature_age_ms))
        .collect()
}

fn materialize_governed_feature_rows(
    partition: &str,
    selectors: &[BinaryDecisionRow],
    contract: &BinaryDatasetContract,
    governed_rows: &GovernedObservationIndex<'_>,
    polymarket_evidence: &PolymarketEvidenceIndex<'_>,
) -> Result<Vec<Vec<f32>>> {
    selectors
        .iter()
        .enumerate()
        .map(|(index, selector)| {
            materialize_governed_feature_row(
                partition,
                index,
                selector,
                contract,
                governed_rows,
                polymarket_evidence,
            )
        })
        .collect()
}

fn materialize_split<'a>(
    split: &'a EventDisjointBinarySplit,
    snapshot: &ResearchSnapshot,
) -> Result<MaterializedBinarySplit<'a>> {
    let governed_rows = governed_observation_index(snapshot)?;
    let polymarket_evidence = PolymarketEvidenceIndex::from_snapshot(snapshot)
        .map_err(anyhow::Error::msg)
        .context("index training Polymarket evidence")?;
    let governed_events = snapshot
        .manifest
        .chainlink_oracle_settlement_evidence
        .iter()
        .map(|evidence| (evidence.event_id.as_str(), evidence))
        .collect::<HashMap<_, _>>();

    let materialize_partition =
        |partition: &str, selectors: &[BinaryDecisionRow]| -> Result<Vec<_>> {
            selectors
                .iter()
                .enumerate()
                .map(|(index, selector)| {
                    let features = materialize_governed_feature_row(
                        partition,
                        index,
                        selector,
                        &split.contract,
                        &governed_rows,
                        &polymarket_evidence,
                    )?;
                    let settlement_evidence = governed_events
                        .get(selector.event_id.as_str())
                        .copied()
                        .with_context(|| {
                            format!("{partition} row {index} has no governed settlement evidence")
                        })?;
                    let event_end_at_ms = settlement_evidence.end_time.timestamp_millis();
                    let label_available_at_ms = settlement_evidence
                        .label_available_at()
                        .with_context(|| {
                            format!(
                            "{partition} row {index} lacks official label availability provenance"
                        )
                        })?
                        .timestamp_millis();
                    let outcome = settlement_evidence
                        .official_outcome_up
                        .context("governed settlement evidence is missing official outcome")?;
                    Ok(MaterializedBinarySample {
                        event_id: selector.event_id.clone(),
                        decision_at_ms: selector.decision_at_ms,
                        event_end_at_ms,
                        label_available_at_ms,
                        features,
                        outcome,
                    })
                })
                .collect()
        };

    Ok(MaterializedBinarySplit {
        contract: &split.contract,
        train: materialize_partition("train", &split.train)?,
        validation: materialize_partition("validation", &split.validation)?,
    })
}

fn normalized_flattened_features(
    samples: &[MaterializedBinarySample],
    normalizer: &FeatureNormalizer,
) -> Result<Vec<f32>> {
    let capacity = checked_feature_cells(samples.len(), normalizer.means.len(), "normalized")?;
    ensure!(
        capacity <= MAX_FEATURE_CELLS,
        "feature cell count exceeds the governed limit of {MAX_FEATURE_CELLS}"
    );
    let mut flattened = Vec::with_capacity(capacity);
    for sample in samples {
        flattened.extend(normalizer.transform(&sample.features)?);
    }
    Ok(flattened)
}

fn predict_feature_rows(
    model: &BurnBinaryLinear<InferenceBackend>,
    normalizer: &FeatureNormalizer,
    feature_rows: &[Vec<f32>],
) -> Result<Vec<f64>> {
    ensure!(!feature_rows.is_empty(), "prediction batch is empty");
    let capacity = checked_feature_cells(feature_rows.len(), normalizer.means.len(), "prediction")?;
    ensure!(
        capacity <= MAX_FEATURE_CELLS,
        "prediction feature cell count exceeds the governed limit of {MAX_FEATURE_CELLS}"
    );
    let mut flattened = Vec::with_capacity(capacity);
    for features in feature_rows {
        flattened.extend(normalizer.transform(features)?);
    }

    let device = Default::default();
    let features = Tensor::<InferenceBackend, 2>::from_data(
        TensorData::new(flattened, [feature_rows.len(), normalizer.means.len()]),
        &device,
    );
    let probabilities = sigmoid(model.forward(features))
        .into_data()
        .to_vec::<f32>()
        .context("read Burn binary probabilities")?;
    ensure!(
        probabilities.len() == feature_rows.len(),
        "Burn output row count does not match prediction input"
    );
    probabilities
        .into_iter()
        .enumerate()
        .map(|(row, probability)| {
            ensure!(
                probability.is_finite() && (0.0..=1.0).contains(&probability),
                "invalid probability at row {row}"
            );
            Ok(f64::from(probability))
        })
        .collect()
}

fn compute_metrics(
    samples: &[MaterializedBinarySample],
    probabilities: &[f64],
    epsilon: f64,
) -> Result<BinaryOosMetrics> {
    ensure!(
        samples.len() == probabilities.len() && !samples.is_empty(),
        "metric input lengths are invalid"
    );
    let mut squared_error = 0.0;
    let mut negative_log_likelihood = 0.0;
    let mut correct = 0usize;
    for (sample, probability) in samples.iter().zip(probabilities) {
        ensure!(
            probability.is_finite() && (0.0..=1.0).contains(probability),
            "metric input contains an invalid probability"
        );
        let label = f64::from(sample.outcome);
        squared_error += (probability - label).powi(2);
        let clamped = probability.clamp(epsilon, 1.0 - epsilon);
        negative_log_likelihood -= label * clamped.ln() + (1.0 - label) * (1.0 - clamped).ln();
        if (*probability >= 0.5) == sample.outcome {
            correct += 1;
        }
    }
    let count = samples.len() as f64;
    Ok(BinaryOosMetrics {
        sample_count: samples.len(),
        event_count: event_count(samples),
        brier_score: squared_error / count,
        log_loss: negative_log_likelihood / count,
        accuracy: correct as f64 / count,
    })
}

fn event_count(samples: &[MaterializedBinarySample]) -> usize {
    samples
        .iter()
        .map(|sample| sample.event_id.as_str())
        .collect::<HashSet<_>>()
        .len()
}

#[derive(Deserialize)]
struct BurnpackMetadataView {
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

fn validate_burnpack_metadata(path: &Path, manifest: &BinaryModelManifest) -> Result<()> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut header = [0_u8; BURNPACK_HEADER_BYTES];
    file.read_exact(&mut header)
        .with_context(|| format!("read Burnpack header {}", path.display()))?;
    ensure!(
        u32::from_le_bytes(header[0..4].try_into().expect("fixed header range")) == BURNPACK_MAGIC,
        "invalid Burnpack magic"
    );
    ensure!(
        u16::from_le_bytes(header[4..6].try_into().expect("fixed header range"))
            == BURNPACK_FORMAT_VERSION,
        "unsupported Burnpack format version"
    );
    let metadata_size =
        u32::from_le_bytes(header[6..10].try_into().expect("fixed header range")) as usize;
    ensure!(
        metadata_size <= MAX_BURNPACK_METADATA_BYTES,
        "Burnpack metadata exceeds research size limit"
    );
    let mut metadata_bytes = vec![0_u8; metadata_size];
    file.read_exact(&mut metadata_bytes)
        .with_context(|| format!("read Burnpack metadata {}", path.display()))?;
    let metadata: BurnpackMetadataView =
        ciborium::de::from_reader(metadata_bytes.as_slice()).context("decode Burnpack metadata")?;

    let expected = [
        ("artifact_scope", "research_only".to_owned()),
        ("dataset_sha256", manifest.dataset_sha256.clone()),
        (
            "snapshot_contract_hash",
            manifest.dataset_contract.snapshot_contract_hash.clone(),
        ),
        (
            "mission_sha256",
            manifest.dataset_contract.mission_sha256.clone(),
        ),
        (
            "feature_schema_sha256",
            manifest.dataset_contract.feature_schema_sha256.clone(),
        ),
        ("manifest_schema", MANIFEST_SCHEMA_VERSION.to_string()),
    ];
    for (key, expected_value) in expected {
        ensure!(
            metadata.metadata.get(key) == Some(&expected_value),
            "Burnpack metadata {key:?} does not match the trusted manifest"
        );
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_digest(hasher.finalize().as_slice())
}

fn prefixed_sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_bytes(bytes))
}

fn feature_schema_sha256(feature_names: &[String]) -> Result<String> {
    validate_feature_names(feature_names)?;
    let bytes = serde_json::to_vec(feature_names).context("serialize ordered feature schema")?;
    Ok(prefixed_sha256_bytes(&bytes))
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("write to String cannot fail");
    }
    encoded
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_snapshot_hash(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_prefixed_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(is_sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        prediction_loop::{
            research_brief_snapshot_id, PredictionSearchBudget, PREDICTION_LOOP_TARGET,
            PREDICTION_MISSION_SCHEMA_VERSION, REQUIRED_BINANCE_DATA_REQUIREMENTS,
            REQUIRED_BINANCE_SOURCE_SURFACES, REQUIRED_CHAINLINK_SOURCE_SURFACE,
            REQUIRED_POLYMARKET_SOURCE_SURFACE,
        },
        research_snapshot::{
            load_research_snapshot, write_research_snapshot, ResearchSnapshotArtifacts,
            ResearchSnapshotInputArtifact, ResearchSnapshotPhaseTiming,
            ResearchSnapshotPmBookSource, ResearchSnapshotRowCounts, ResearchSnapshotSourceSurface,
        },
        FactorObservation, FactorSourceAvailability, ResearchPmBookLevel, ResearchPmBookSnapshot,
    };
    use chrono::{Duration, TimeZone, Utc};

    struct TestContext {
        mission: PredictionResearchMission,
        snapshot: ResearchSnapshot,
        split: EventDisjointBinarySplit,
    }

    #[derive(Clone)]
    struct TestDecision {
        event_id: String,
        decision_at_ms: i64,
        settlement_at_ms: i64,
        features: [f64; 2],
        outcome: bool,
    }

    fn sample(partition: &str, index: usize, outcome: bool) -> TestDecision {
        let base = 1_700_000_000_000_i64 + index as i64 * 60_000;
        let direction = if outcome { 1.0 } else { -1.0 };
        let decision_at_ms = base + 1_000;
        let settlement_at_ms = base + 50_000;
        TestDecision {
            event_id: format!("{partition}-event-{index}"),
            decision_at_ms,
            settlement_at_ms,
            features: [direction + index as f64 * 0.001, direction * 0.5],
            outcome,
        }
    }

    fn selector(sample: &TestDecision) -> BinaryDecisionRow {
        BinaryDecisionRow {
            event_id: sample.event_id.clone(),
            decision_at_ms: sample.decision_at_ms,
        }
    }

    fn feature_names() -> Vec<String> {
        vec!["cex_bar_return_30s".to_owned(), "obi".to_owned()]
    }

    fn mission() -> PredictionResearchMission {
        let mut mission = PredictionResearchMission {
            schema_version: PREDICTION_MISSION_SCHEMA_VERSION.to_owned(),
            mission_id: "polymarket-btc-5m-burn-v1".to_owned(),
            lane: "prediction_market".to_owned(),
            objective: "Estimate official BTC five-minute settlement probability.".to_owned(),
            hypothesis_scope:
                "Test Binance flow and Polymarket depth with official settlement labels.".to_owned(),
            mutable_scope: vec!["probability_blend_weights".to_owned()],
            data_snapshot_id: format!("sha256:{}", "1".repeat(64)),
            target: PREDICTION_LOOP_TARGET.to_owned(),
            symbols: vec!["BTC".to_owned()],
            horizon: "5m".to_owned(),
            prompt_snapshot_id: String::new(),
            search_policy_snapshot_id: current_prediction_policy_snapshot_id(),
            search_budget: PredictionSearchBudget {
                max_candidates: 6,
                max_llm_calls: 2,
                max_seconds: 900,
            },
        };
        mission.prompt_snapshot_id = research_brief_snapshot_id(&mission);
        mission
    }

    fn observation(sample: &TestDecision) -> FactorObservation {
        let tick_ts = Utc
            .timestamp_millis_opt(sample.decision_at_ms)
            .single()
            .expect("valid test timestamp");
        FactorObservation {
            event_id: sample.event_id.clone(),
            symbol: "BTCUSDT".to_owned(),
            tick_ts,
            source_availability: FactorSourceAvailability {
                spot: Some(tick_ts),
                lob: Some(tick_ts),
                aggregate_trade: Some(tick_ts),
                polymarket_up_quote: Some(tick_ts),
                polymarket_down_quote: Some(tick_ts),
                chainlink_reference: Some(tick_ts),
                chainlink_open: Some(tick_ts - Duration::minutes(5)),
            },
            event_window_secs: PREDICTION_EVENT_WINDOW_SECS,
            time_remaining_secs: (sample.settlement_at_ms - sample.decision_at_ms) / 1_000,
            signed_distance_to_beat: 0.0,
            abs_distance_to_beat: 0.0,
            drift_10s: 0.0,
            drift_30s: 0.0,
            flip_age_secs: 0.0,
            post_flip_drift: 0.0,
            sigma_horizon: 0.01,
            fair_prob_up: 0.5,
            fair_prob_up_clean: 0.5,
            prob_disagreement: 0.0,
            implied_sigma_horizon: 0.01,
            vol_gap: 0.0,
            distance_over_sigma: 0.0,
            model_prob_up: 0.5,
            chainlink_prob_up: 0.5,
            model_edge_up: 0.0,
            reward_risk_up: 1.0,
            reward_risk_down: 1.0,
            obi: sample.features[1],
            spread_bps: 1.0,
            microprice_offset_bps: 0.0,
            bid_depth_near: 10.0,
            ask_depth_near: 10.0,
            depth_ratio: 1.0,
            depth_imbalance: 0.0,
            depth_far_ratio: 1.0,
            depth_acceleration: 0.0,
            obi_10: 0.0,
            pm_up_bid: 0.49,
            pm_up_ask: 0.51,
            pm_up_bid_size: 10.0,
            pm_up_ask_size: 10.0,
            pm_down_bid: 0.49,
            pm_down_ask: 0.51,
            pm_down_bid_size: 10.0,
            pm_down_ask_size: 10.0,
            pm_lag_secs: 0.0,
            settlement_up: f64::from(sample.outcome),
            future_up_ask_change_30s: None,
            future_up_ask_change_60s: None,
            cum_obi_delta_5m: 0.0,
            cum_depth_delta_5m: 0.0,
            cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0,
            cex_bar_return_30s: sample.features[0],
            cex_bar_return_60s: 0.0,
            cex_bar_volume_ratio_30s: 0.0,
            cex_bar_volume_trend_3: 0.0,
            cex_signed_volume_ratio_30s: 0.0,
            cex_consecutive_up_bars: 0.0,
            cex_consecutive_down_bars: 0.0,
            cex_breakout_volume_score: 0.0,
        }
    }

    fn books(sample: &TestDecision) -> Vec<ResearchPmBookSnapshot> {
        let tick_ts = Utc
            .timestamp_millis_opt(sample.decision_at_ms)
            .single()
            .expect("valid test timestamp");
        [("up", "up"), ("down", "down")]
            .into_iter()
            .map(|(token_suffix, side)| ResearchPmBookSnapshot {
                event_id: sample.event_id.clone(),
                token_id: format!("{}-{token_suffix}", sample.event_id),
                side: side.to_owned(),
                ts: tick_ts,
                bids: vec![ResearchPmBookLevel {
                    price: 0.49,
                    size: 10.0,
                }],
                asks: vec![ResearchPmBookLevel {
                    price: 0.51,
                    size: 10.0,
                }],
            })
            .collect()
    }

    fn snapshot(samples: &[TestDecision]) -> ResearchSnapshot {
        let generated_at = Utc
            .timestamp_millis_opt(samples[0].decision_at_ms)
            .single()
            .expect("valid test timestamp");
        let observations = samples.iter().map(observation).collect::<Vec<_>>();
        let pm_book_snapshots = samples.iter().flat_map(books).collect::<Vec<_>>();
        let chainlink_oracle_settlement_evidence = samples
            .iter()
            .map(|sample| {
                let end_time = Utc
                    .timestamp_millis_opt(sample.settlement_at_ms)
                    .single()
                    .expect("valid settlement timestamp");
                let start_time = end_time - Duration::seconds(PREDICTION_EVENT_WINDOW_SECS);
                let close_price = if sample.outcome { 101.0 } else { 99.0 };
                crate::ChainlinkOracleSettlementEvidence {
                    event_id: sample.event_id.clone(),
                    symbol: "BTC".to_owned(),
                    policy_version: crate::GOVERNED_CHAINLINK_BOUNDARY_POLICY_VERSION.to_owned(),
                    start_time,
                    end_time,
                    open: Some(crate::ChainlinkOracleBoundaryEvidence {
                        boundary_ts: start_time,
                        price: 100.0,
                        source_ts: start_time,
                        received_at: start_time,
                        confirmation_source_ts: start_time,
                        confirmation_received_at: start_time,
                    }),
                    close: Some(crate::ChainlinkOracleBoundaryEvidence {
                        boundary_ts: end_time,
                        price: close_price,
                        source_ts: end_time,
                        received_at: end_time,
                        confirmation_source_ts: end_time,
                        confirmation_received_at: end_time,
                    }),
                    chainlink_outcome_up: Some(sample.outcome),
                    official_outcome_up: Some(sample.outcome),
                    official_outcome_available_at: Some(end_time),
                    reasons: Vec::new(),
                }
            })
            .collect::<Vec<_>>();
        let chainlink_oracle_settlement_audit =
            crate::ChainlinkOracleSettlementAudit::from_evidence(
                &chainlink_oracle_settlement_evidence,
            );
        ResearchSnapshot {
            manifest: crate::ResearchSnapshotManifest {
                schema_version: crate::RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_owned(),
                snapshot_hash: None,
                snapshot_contract_hash: None,
                generated_at,
                git_sha: None,
                symbols: vec!["BTCUSDT".to_owned()],
                start: generated_at - Duration::days(1),
                end: generated_at + Duration::days(2),
                history_start: generated_at - Duration::days(3),
                lob_sample_secs: 1,
                pm_book_sample_secs: Some(1),
                observation_sample_secs: 1,
                max_quote_age_secs: 30,
                stake_usd: 15.0,
                require_official_settlement: true,
                chainlink_oracle_settlement_audit: Some(chainlink_oracle_settlement_audit),
                chainlink_oracle_settlement_evidence,
                immutable_input: true,
                source_kind: "unit_test".to_owned(),
                optimizer_data_dir: Some("unit-test-immutable-source".to_owned()),
                source_surfaces: REQUIRED_BINANCE_SOURCE_SURFACES
                    .iter()
                    .copied()
                    .chain([
                        REQUIRED_CHAINLINK_SOURCE_SURFACE,
                        REQUIRED_POLYMARKET_SOURCE_SURFACE,
                    ])
                    .map(|name| ResearchSnapshotSourceSurface {
                        name: name.to_owned(),
                        role: match name {
                            REQUIRED_CHAINLINK_SOURCE_SURFACE => {
                                "opening_reference_and_expiry_price_source".to_owned()
                            }
                            REQUIRED_POLYMARKET_SOURCE_SURFACE => {
                                "execution_depth_context".to_owned()
                            }
                            _ => "binance_prediction_context".to_owned(),
                        },
                        gate_category: if name == REQUIRED_POLYMARKET_SOURCE_SURFACE {
                            "required_for_execution".to_owned()
                        } else {
                            "required_for_prediction".to_owned()
                        },
                        raw_full_fidelity: false,
                        snapshot_sampled: true,
                        sample_secs: Some(1),
                        row_count: Some(samples.len()),
                        notes: "point-in-time governed test surface".to_owned(),
                    })
                    .collect(),
                input_artifacts: vec![ResearchSnapshotInputArtifact {
                    name: "unit".to_owned(),
                    path: "unit".to_owned(),
                    content_hash: None,
                    row_count: Some(samples.len()),
                }],
                data_requirements: REQUIRED_BINANCE_DATA_REQUIREMENTS
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                data_audit_status: Some("ok".to_owned()),
                data_audit_report: Some("audit.json".to_owned()),
                include_deribit: false,
                artifacts: ResearchSnapshotArtifacts::default(),
                row_counts: ResearchSnapshotRowCounts {
                    observations: observations.len(),
                    deribit_snapshots: 0,
                    pm_book_snapshots: pm_book_snapshots.len(),
                },
                phase_timings: vec![ResearchSnapshotPhaseTiming {
                    phase: "unit".to_owned(),
                    elapsed_ms: 1,
                    rows: Some(samples.len()),
                }],
                quality_flags: vec![],
                pm_book_source: ResearchSnapshotPmBookSource::default(),
            },
            observations,
            deribit_snapshots: vec![],
            pm_book_snapshots,
        }
    }

    fn context() -> TestContext {
        let train = (0..12)
            .map(|index| sample("train", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let validation = (15..21)
            .map(|index| sample("validation", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let all_samples = train.iter().chain(&validation).cloned().collect::<Vec<_>>();
        let mut mission = mission();
        let temp = tempfile::tempdir().unwrap();
        let written = write_research_snapshot(temp.path(), snapshot(&all_samples)).unwrap();
        mission.data_snapshot_id = written.snapshot_contract_hash.unwrap();
        let snapshot = load_research_snapshot(temp.path()).unwrap();
        let contract =
            BinaryDatasetContract::from_prediction_snapshot(&snapshot, &mission, feature_names())
                .expect("valid governed test contract");
        TestContext {
            mission,
            snapshot,
            split: EventDisjointBinarySplit {
                contract,
                train: train.iter().map(selector).collect(),
                validation: validation.iter().map(selector).collect(),
            },
        }
    }

    fn reseal_snapshot(
        snapshot: ResearchSnapshot,
        mut mission: PredictionResearchMission,
    ) -> (ResearchSnapshot, PredictionResearchMission) {
        let temp = tempfile::tempdir().unwrap();
        let written = write_research_snapshot(temp.path(), snapshot).unwrap();
        mission.data_snapshot_id = written.snapshot_contract_hash.unwrap();
        (load_research_snapshot(temp.path()).unwrap(), mission)
    }

    fn reseal_context(mut context: TestContext) -> TestContext {
        (context.snapshot, context.mission) = reseal_snapshot(context.snapshot, context.mission);
        context.split.contract = BinaryDatasetContract::from_prediction_snapshot(
            &context.snapshot,
            &context.mission,
            feature_names(),
        )
        .expect("resealed context must have a valid governed contract");
        context
    }

    fn add_fresh_polymarket_sibling(
        context: &mut TestContext,
        selected: &BinaryDecisionRow,
    ) -> BinaryDecisionRow {
        let selected_index = context
            .snapshot
            .observations
            .iter()
            .position(|row| {
                row.event_id == selected.event_id
                    && row.tick_ts.timestamp_millis() == selected.decision_at_ms
            })
            .expect("selected observation must exist");
        let selected_tick = context.snapshot.observations[selected_index].tick_ts;
        let fresh_tick = selected_tick + Duration::seconds(1);

        let mut fresh = context.snapshot.observations[selected_index].clone();
        fresh.tick_ts = fresh_tick;
        fresh.time_remaining_secs -= 1;
        fresh.source_availability.spot = Some(fresh_tick);
        fresh.source_availability.lob = Some(fresh_tick);
        fresh.source_availability.aggregate_trade = Some(fresh_tick);
        fresh.source_availability.polymarket_up_quote = Some(fresh_tick);
        fresh.source_availability.polymarket_down_quote = Some(fresh_tick);
        fresh.source_availability.chainlink_reference = Some(fresh_tick);
        fresh.pm_lag_secs = 0.0;

        let stale = &mut context.snapshot.observations[selected_index];
        stale.source_availability.polymarket_up_quote = Some(selected_tick - Duration::seconds(31));
        stale.source_availability.polymarket_down_quote =
            Some(selected_tick - Duration::seconds(31));
        stale.pm_lag_secs = 31.0;

        for book in context
            .snapshot
            .pm_book_snapshots
            .iter_mut()
            .filter(|book| book.event_id == selected.event_id)
        {
            book.ts = fresh_tick;
        }
        context.snapshot.observations.push(fresh);
        context.snapshot.manifest.row_counts.observations += 1;

        BinaryDecisionRow {
            event_id: selected.event_id.clone(),
            decision_at_ms: fresh_tick.timestamp_millis(),
        }
    }

    fn test_config() -> BinaryTrainingConfig {
        BinaryTrainingConfig {
            seed: 7,
            epochs: 120,
            learning_rate: 0.05,
            log_loss_epsilon: 1.0e-7,
        }
    }

    #[test]
    fn accepts_snapshot_written_and_loaded_by_governed_snapshot_api() {
        let train = (0..12)
            .map(|index| sample("train", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let validation = (15..21)
            .map(|index| sample("validation", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let all_samples = train.iter().chain(&validation).cloned().collect::<Vec<_>>();
        let temp = tempfile::tempdir().unwrap();
        let written = write_research_snapshot(temp.path(), snapshot(&all_samples)).unwrap();
        let loaded = load_research_snapshot(temp.path()).unwrap();
        let mut mission = mission();
        mission.data_snapshot_id = written.snapshot_contract_hash.clone().unwrap();

        assert!(written
            .snapshot_hash
            .as_deref()
            .is_some_and(|hash| hash.len() == 16));
        BinaryDatasetContract::from_prediction_snapshot(&loaded, &mission, feature_names())
            .expect("a snapshot emitted by the governed writer must enter the Burn lane");
    }

    #[test]
    fn rejects_event_overlap_between_train_and_validation() {
        let mut context = context();
        context.split.validation[0] = context.split.train[0].clone();

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("overlapping event must fail");
        assert!(error.to_string().contains("event-disjoint split violation"));
    }

    #[test]
    fn rejects_duplicate_selector_in_either_partition() {
        for partition in ["train", "validation"] {
            let mut context = context();
            match partition {
                "train" => context.split.train.push(context.split.train[0].clone()),
                "validation" => context
                    .split
                    .validation
                    .push(context.split.validation[0].clone()),
                _ => unreachable!(),
            }

            let error = train_event_disjoint_binary(
                &context.snapshot,
                &context.mission,
                &context.split,
                test_config(),
            )
            .expect_err("an exact selector duplicate must fail closed");
            assert!(error.to_string().contains("duplicate decision selector"));
        }
    }

    #[test]
    fn selected_train_and_validation_rows_require_their_own_polymarket_evidence() {
        for partition in ["train", "validation"] {
            let mut context = context();
            let selected = match partition {
                "train" => context.split.train[0].clone(),
                "validation" => context.split.validation[0].clone(),
                _ => unreachable!(),
            };
            add_fresh_polymarket_sibling(&mut context, &selected);
            let context = reseal_context(context);

            let error = train_event_disjoint_binary(
                &context.snapshot,
                &context.mission,
                &context.split,
                test_config(),
            )
            .expect_err("another fresh row must not cover stale selected PM evidence");
            let message = format!("{error:#}");
            assert!(message.contains(&format!("{partition} row 0")));
            assert!(message.contains("Polymarket"));
        }
    }

    #[test]
    fn selected_prediction_row_requires_its_own_polymarket_evidence() {
        let mut context = context();
        let stale_selector = context.split.validation[0].clone();
        let fresh_selector = add_fresh_polymarket_sibling(&mut context, &stale_selector);
        context.split.validation[0] = fresh_selector;
        let context = reseal_context(context);
        let model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect("fresh sibling selector must remain trainable");

        let error = model
            .predict_probabilities(&context.snapshot, &context.mission, &[stale_selector])
            .expect_err("another fresh row must not cover stale prediction PM evidence");
        let message = format!("{error:#}");
        assert!(message.contains("prediction row 0"));
        assert!(message.contains("Polymarket"));
    }

    #[test]
    fn selected_row_requires_its_own_nonempty_full_depth_books() {
        let mut context = context();
        let selected = context.split.train[0].clone();
        add_fresh_polymarket_sibling(&mut context, &selected);
        let selected_row = context
            .snapshot
            .observations
            .iter_mut()
            .find(|row| {
                row.event_id == selected.event_id
                    && row.tick_ts.timestamp_millis() == selected.decision_at_ms
            })
            .expect("selected observation must exist");
        selected_row.source_availability.polymarket_up_quote = Some(selected_row.tick_ts);
        selected_row.source_availability.polymarket_down_quote = Some(selected_row.tick_ts);
        selected_row.pm_lag_secs = 0.0;
        let context = reseal_context(context);

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("another fresh row must not cover selected full-depth books");
        let message = format!("{error:#}");
        assert!(message.contains("train row 0"));
        assert!(message.contains("full-depth book evidence"));
    }

    #[test]
    fn selected_row_requires_fresh_quotes_for_both_polymarket_sides() {
        for missing_up_quote in [true, false] {
            let mut context = context();
            let selected = context.split.train[0].clone();
            add_fresh_polymarket_sibling(&mut context, &selected);
            let selected_row = context
                .snapshot
                .observations
                .iter_mut()
                .find(|row| {
                    row.event_id == selected.event_id
                        && row.tick_ts.timestamp_millis() == selected.decision_at_ms
                })
                .expect("selected observation must exist");
            selected_row.source_availability.polymarket_up_quote = if missing_up_quote {
                None
            } else {
                Some(selected_row.tick_ts - Duration::seconds(31))
            };
            selected_row.source_availability.polymarket_down_quote = Some(selected_row.tick_ts);
            selected_row.pm_lag_secs = 0.0;
            for book in context
                .snapshot
                .pm_book_snapshots
                .iter_mut()
                .filter(|book| book.event_id == selected.event_id)
            {
                book.ts = selected_row.tick_ts;
            }
            let context = reseal_context(context);

            let error = train_event_disjoint_binary(
                &context.snapshot,
                &context.mission,
                &context.split,
                test_config(),
            )
            .expect_err("one fresh PM side must not cover a missing or stale opposite quote");
            let message = format!("{error:#}");
            assert!(message.contains("train row 0"));
            assert!(message.contains("fresh matching Polymarket quote"));
        }
    }

    #[test]
    fn rejects_missing_source_and_non_finite_snapshot_rows() {
        let mut missing_lob = context();
        missing_lob.snapshot.observations[0].source_availability.lob = None;
        let missing_lob = reseal_context(missing_lob);
        let error = train_event_disjoint_binary(
            &missing_lob.snapshot,
            &missing_lob.mission,
            &missing_lob.split,
            test_config(),
        )
        .expect_err("missing L2 availability must fail");
        assert!(format!("{error:#}").contains("missing lob availability"));

        let mut non_finite = context();
        non_finite.snapshot.observations[0].cex_bar_return_30s = f64::MAX;
        let non_finite = reseal_context(non_finite);
        let error = train_event_disjoint_binary(
            &non_finite.snapshot,
            &non_finite.mission,
            &non_finite.split,
            test_config(),
        )
        .expect_err("non-finite registered feature must fail");
        assert!(error.to_string().contains("snapshot feature"));
    }

    #[test]
    fn rejects_unregistered_label_features_and_caller_supplied_values() {
        let context = context();
        for forbidden in ["settlement_up", "future_up_ask_change_30s", "unknown"] {
            let error = BinaryDatasetContract::from_prediction_snapshot(
                &context.snapshot,
                &context.mission,
                vec![forbidden.to_owned()],
            )
            .expect_err("labels and unknown fields must not enter the feature registry");
            assert!(error.to_string().contains("not a registered point-in-time"));
        }

        let selector = serde_json::json!({
            "event_id": context.split.train[0].event_id,
            "decision_at_ms": context.split.train[0].decision_at_ms,
            "features": [1.0],
            "outcome": true
        });
        assert!(serde_json::from_value::<BinaryDecisionRow>(selector).is_err());
    }

    #[test]
    fn rejects_training_events_settling_after_validation_begins() {
        let mut context = context();
        let validation_start = context.split.validation[0].decision_at_ms;
        let train_selector = context.split.train.last().unwrap().clone();
        let settlement_at = Utc
            .timestamp_millis_opt(validation_start + 1_000)
            .single()
            .expect("valid governed settlement timestamp");
        let row = context
            .snapshot
            .observations
            .iter_mut()
            .find(|row| row.event_id == train_selector.event_id)
            .unwrap();
        row.time_remaining_secs = (settlement_at - row.tick_ts).num_seconds();
        let evidence = context
            .snapshot
            .manifest
            .chainlink_oracle_settlement_evidence
            .iter_mut()
            .find(|evidence| evidence.event_id == train_selector.event_id)
            .expect("training event settlement evidence");
        evidence.end_time = settlement_at;
        evidence.start_time = settlement_at - Duration::seconds(PREDICTION_EVENT_WINDOW_SECS);
        let open = evidence.open.as_mut().expect("opening boundary evidence");
        open.boundary_ts = evidence.start_time;
        open.source_ts = evidence.start_time;
        open.received_at = evidence.start_time;
        open.confirmation_source_ts = evidence.start_time;
        open.confirmation_received_at = evidence.start_time;
        let close = evidence.close.as_mut().expect("closing boundary evidence");
        close.boundary_ts = evidence.end_time;
        close.source_ts = evidence.end_time;
        close.received_at = evidence.end_time;
        close.confirmation_source_ts = evidence.end_time;
        close.confirmation_received_at = evidence.end_time;
        context.snapshot.manifest.chainlink_oracle_settlement_audit =
            Some(crate::ChainlinkOracleSettlementAudit::from_evidence(
                &context
                    .snapshot
                    .manifest
                    .chainlink_oracle_settlement_evidence,
            ));
        let context = reseal_context(context);

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("future training label must not validate past decisions");
        assert!(error.to_string().contains("OOS cutoff violation"));
    }

    #[test]
    fn rejects_training_label_available_after_validation_begins() {
        let train = (0..12)
            .map(|index| sample("train", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let validation = (15..21)
            .map(|index| sample("validation", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let validation_start = validation[0].decision_at_ms;
        let all_samples = train.iter().chain(&validation).cloned().collect::<Vec<_>>();
        let mut raw_snapshot = snapshot(&all_samples);
        let last_train_event = &train.last().unwrap().event_id;
        let evidence = raw_snapshot
            .manifest
            .chainlink_oracle_settlement_evidence
            .iter_mut()
            .find(|evidence| &evidence.event_id == last_train_event)
            .unwrap();
        evidence.official_outcome_available_at = Some(
            Utc.timestamp_millis_opt(validation_start + 1_000)
                .single()
                .unwrap(),
        );
        let temp = tempfile::tempdir().unwrap();
        let written = write_research_snapshot(temp.path(), raw_snapshot).unwrap();
        let loaded = load_research_snapshot(temp.path()).unwrap();
        let mut mission = mission();
        mission.data_snapshot_id = written.snapshot_contract_hash.unwrap();
        let contract =
            BinaryDatasetContract::from_prediction_snapshot(&loaded, &mission, feature_names())
                .unwrap();
        let split = EventDisjointBinarySplit {
            contract,
            train: train.iter().map(selector).collect(),
            validation: validation.iter().map(selector).collect(),
        };

        let error = train_event_disjoint_binary(&loaded, &mission, &split, test_config())
            .expect_err("labels unavailable at validation start must not enter training");
        assert!(error.to_string().contains("OOS cutoff violation"));
    }

    #[test]
    fn feature_materialization_enforces_source_availability_and_age() {
        let sample = sample("availability", 0, true);
        let mut row = observation(&sample);

        row.source_availability.lob = None;
        let missing = extract_registered_feature(&row, "obi", 30_000)
            .expect_err("missing source availability must fail closed");
        assert!(missing.to_string().contains("missing lob availability"));

        let mut row = observation(&sample);
        row.source_availability.lob = Some(row.tick_ts + Duration::milliseconds(1));
        let future = extract_registered_feature(&row, "obi", 30_000)
            .expect_err("future source availability must fail closed");
        assert!(future.to_string().contains("future lob availability"));

        let mut row = observation(&sample);
        row.source_availability.aggregate_trade = Some(row.tick_ts - Duration::seconds(31));
        let stale = extract_registered_feature(&row, "cex_bar_return_30s", 30_000)
            .expect_err("stale source availability must fail closed");
        assert!(stale
            .to_string()
            .contains("aggregate_trade source is stale"));

        let mut row = observation(&sample);
        row.source_availability.chainlink_reference = Some(row.tick_ts - Duration::seconds(31));
        let combination = extract_registered_feature(&row, "chainlink_prob_up", 30_000)
            .expect_err("one stale dependency in a combined feature must fail closed");
        assert!(combination
            .to_string()
            .contains("chainlink_reference source is stale"));

        extract_registered_feature(&row, "signed_distance_to_beat", 30_000)
            .expect("event-static Chainlink open may predate the rolling freshness window");
    }

    #[test]
    fn rejects_snapshot_without_official_label_availability() {
        let train = (0..12)
            .map(|index| sample("train", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let validation = (15..21)
            .map(|index| sample("validation", index, index % 2 == 0))
            .collect::<Vec<_>>();
        let all_samples = train.iter().chain(&validation).cloned().collect::<Vec<_>>();
        let mut raw_snapshot = snapshot(&all_samples);
        raw_snapshot.manifest.chainlink_oracle_settlement_evidence[0]
            .official_outcome_available_at = None;
        let temp = tempfile::tempdir().unwrap();
        let written = write_research_snapshot(temp.path(), raw_snapshot).unwrap();
        let loaded = load_research_snapshot(temp.path()).unwrap();
        let mut mission = mission();
        mission.data_snapshot_id = written.snapshot_contract_hash.unwrap();

        let error =
            BinaryDatasetContract::from_prediction_snapshot(&loaded, &mission, feature_names())
                .expect_err("missing official label availability must fail closed");
        assert!(format!("{error:#}").contains("official label availability"));
    }

    #[test]
    fn rejects_rows_not_in_the_bound_snapshot() {
        let mut context = context();
        context.split.train[0].event_id = "unbound-event".to_owned();

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("unbound decision row must fail");
        assert!(error
            .to_string()
            .contains("not an exact decision row in the bound prediction snapshot"));
    }

    #[test]
    fn rejects_snapshot_mutated_after_contract_creation() {
        let mut context = context();
        context.snapshot.observations[0].cex_bar_return_30s += 7.0;

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("training must reverify the immutable snapshot contract");
        assert!(format!("{error:#}").contains("evaluator contract hash mismatch"));
    }

    #[test]
    fn rejects_snapshot_without_isolated_symbol_or_official_authority() {
        let mut wrong_symbol = context();
        wrong_symbol.snapshot.manifest.symbols = vec!["SOLUSDT".to_owned()];
        for observation in &mut wrong_symbol.snapshot.observations {
            observation.symbol = "SOLUSDT".to_owned();
        }
        for evidence in &mut wrong_symbol
            .snapshot
            .manifest
            .chainlink_oracle_settlement_evidence
        {
            evidence.symbol = "SOL".to_owned();
        }
        let (wrong_symbol_snapshot, wrong_symbol_mission) =
            reseal_snapshot(wrong_symbol.snapshot, wrong_symbol.mission);
        let error = BinaryDatasetContract::from_prediction_snapshot(
            &wrong_symbol_snapshot,
            &wrong_symbol_mission,
            feature_names(),
        )
        .expect_err("cross-symbol snapshot must fail at contract creation");
        assert!(format!("{error:#}").contains("not isolated mission underlying"));

        let mut unofficial = context();
        unofficial.snapshot.manifest.require_official_settlement = false;
        let (unofficial_snapshot, unofficial_mission) =
            reseal_snapshot(unofficial.snapshot, unofficial.mission);
        let error = BinaryDatasetContract::from_prediction_snapshot(
            &unofficial_snapshot,
            &unofficial_mission,
            feature_names(),
        )
        .expect_err("unofficial labels must fail at contract creation");
        assert!(format!("{error:#}").contains("official settlement labels"));
    }

    #[test]
    fn training_is_seeded_and_reports_oos_metrics() {
        let context = context();
        let model_a = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let model_b = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let probabilities_a = model_a
            .predict_probabilities(
                &context.snapshot,
                &context.mission,
                &context.split.validation,
            )
            .unwrap();
        let probabilities_b = model_b
            .predict_probabilities(
                &context.snapshot,
                &context.mission,
                &context.split.validation,
            )
            .unwrap();

        assert_eq!(probabilities_a, probabilities_b);
        assert_eq!(model_a.manifest().training.seed, 7);
        assert_eq!(
            model_a.manifest().artifact_scope,
            BinaryArtifactScope::ResearchOnly
        );
        assert_eq!(model_a.manifest().validation_metrics.sample_count, 6);
        assert_eq!(model_a.manifest().validation_metrics.event_count, 6);
        assert!(model_a.manifest().validation_metrics.brier_score < 0.25);
        assert!(model_a.manifest().validation_metrics.log_loss.is_finite());
        assert!(model_a.manifest().validation_metrics.accuracy >= 0.5);
        assert!(model_a.manifest().model_sha256.is_none());
    }

    #[test]
    fn rejects_epoch_budget_above_governed_limit() {
        let context = context();
        let mut config = test_config();
        config.epochs = 1_001;

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            config,
        )
        .expect_err("epoch count above the governed bound must fail before training");
        assert!(error.to_string().contains("epochs exceeds"));
    }

    #[test]
    fn rejects_selector_partition_above_governed_limit() {
        let mut context = context();
        context.split.train = vec![context.split.train[0].clone(); MAX_SELECTORS_PER_PARTITION + 1];

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("selector count above the governed bound must fail before allocation");
        assert!(error.to_string().contains("train selector count exceeds"));
    }

    #[test]
    fn rejects_total_selector_budget_above_governed_limit() {
        let mut context = context();
        let partition_size = MAX_TOTAL_SELECTORS / 2 + 1;
        context.split.train = vec![context.split.train[0].clone(); partition_size];
        context.split.validation = vec![context.split.validation[0].clone(); partition_size];

        let error = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .expect_err("combined selector count above the governed bound must fail first");
        assert!(error.to_string().contains("total selector count exceeds"));
    }

    #[test]
    fn training_budget_uses_checked_governed_arithmetic() {
        let overflow = validate_training_budget(usize::MAX, 1, 2, 1)
            .expect_err("row arithmetic overflow must fail closed");
        assert!(format!("{overflow:#}").contains("overflow"));

        let cells = validate_training_budget(MAX_FEATURE_CELLS + 1, 0, 1, 1)
            .expect_err("feature matrix above the governed bound must fail closed");
        assert!(cells.to_string().contains("feature cell count exceeds"));

        let work = validate_training_budget(100_001, 1, 20, MAX_BINARY_EPOCHS)
            .expect_err("training work above the governed bound must fail closed");
        assert!(work.to_string().contains("total training work exceeds"));

        let too_many_features = (0..=MAX_FEATURES)
            .map(|index| format!("feature_{index}"))
            .collect::<Vec<_>>();
        let feature_count = validate_feature_names(&too_many_features)
            .expect_err("feature count above the governed bound must fail first");
        assert!(feature_count.to_string().contains("feature schema exceeds"));

        let prediction = validate_prediction_budget(MAX_FEATURE_CELLS + 1, 1)
            .expect_err("prediction cell count must fail before materialization");
        assert!(prediction
            .to_string()
            .contains("prediction feature cell count exceeds"));
    }

    #[test]
    fn snapshot_coverage_budget_uses_checked_governed_arithmetic() {
        let overflow = validate_snapshot_row_budget(usize::MAX, 1, 1)
            .expect_err("snapshot row arithmetic overflow must fail closed");
        assert!(overflow.contains("overflow"));

        let observations = validate_snapshot_row_budget(MAX_SNAPSHOT_OBSERVATIONS + 1, 0, 0)
            .expect_err("unselected observations must still be bounded");
        assert!(observations.contains("observation row count exceeds"));

        let books = validate_snapshot_row_budget(0, 0, MAX_SNAPSHOT_BOOK_ROWS + 1)
            .expect_err("unselected book rows must still be bounded");
        assert!(books.contains("Polymarket book row count exceeds"));
    }

    #[test]
    fn normalizer_is_fit_from_train_partition_only() {
        let mut context = context();
        let validation_ids = context
            .split
            .validation
            .iter()
            .map(|row| row.event_id.as_str())
            .collect::<HashSet<_>>();
        for observation in &mut context.snapshot.observations {
            if validation_ids.contains(observation.event_id.as_str()) {
                observation.cex_bar_return_30s += 10_000.0;
            }
        }
        let context = reseal_context(context);
        let materialized = materialize_split(&context.split, &context.snapshot).unwrap();
        let expected_train_mean = materialized
            .train
            .iter()
            .map(|sample| f64::from(sample.features[0]))
            .sum::<f64>()
            / materialized.train.len() as f64;

        let model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        assert_eq!(model.manifest().normalizer.means[0], expected_train_mean);
    }

    #[test]
    fn prediction_requires_the_bound_snapshot_and_exact_selector() {
        let context = context();
        let model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let unknown = vec![BinaryDecisionRow {
            event_id: "unbound-event".to_owned(),
            decision_at_ms: context.split.validation[0].decision_at_ms,
        }];
        let error = model
            .predict_probabilities(&context.snapshot, &context.mission, &unknown)
            .expect_err("selector outside the governed snapshot must fail");
        assert!(error.to_string().contains("not an exact decision row"));

        let mut mutated = context.snapshot.clone();
        mutated.observations[0].obi += 0.25;
        let error = model
            .predict_probabilities(&mutated, &context.mission, &context.split.validation)
            .expect_err("snapshot mutation must fail immutable-contract verification");
        assert!(format!("{error:#}").contains("evaluator contract hash mismatch"));
    }

    #[test]
    fn burnpack_and_typed_manifest_round_trip_predictions() {
        let context = context();
        let mut model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let before = model
            .predict_probabilities(
                &context.snapshot,
                &context.mission,
                &context.split.validation,
            )
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("binary-research-model");

        let digest = model.save_bundle(&bundle).unwrap();
        let burnpack = fs::read(bundle.join(MODEL_FILE)).unwrap();
        assert_eq!(
            u32::from_le_bytes(burnpack[..4].try_into().unwrap()),
            0x4255_524e
        );
        let manifest: BinaryModelManifest =
            serde_json::from_slice(&fs::read(bundle.join(MANIFEST_FILE)).unwrap()).unwrap();
        assert_eq!(manifest.schema_version, MANIFEST_SCHEMA_VERSION);
        assert!(manifest.model_sha256.as_deref().is_some_and(is_sha256));

        let loaded =
            BinaryProbabilityModel::load_bundle(&bundle, &digest, &context.mission).unwrap();
        let after = loaded
            .predict_probabilities(
                &context.snapshot,
                &context.mission,
                &context.split.validation,
            )
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(loaded.manifest(), &manifest);
    }

    #[test]
    fn load_rejects_tampered_burnpack() {
        let context = context();
        let mut model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("binary-research-model");
        let digest = model.save_bundle(&bundle).unwrap();
        let model_path = bundle.join(MODEL_FILE);
        let mut bytes = fs::read(&model_path).unwrap();
        let last = bytes.last_mut().unwrap();
        *last ^= 0x01;
        fs::write(model_path, bytes).unwrap();

        let error = BinaryProbabilityModel::load_bundle(&bundle, &digest, &context.mission)
            .expect_err("tampered Burnpack must fail");
        assert!(error.to_string().contains("trusted registry digest"));
    }

    #[test]
    fn load_rejects_tampered_manifest_and_cross_mission_bundle() {
        let context = context();
        let mut model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("binary-research-model");
        let digest = model.save_bundle(&bundle).unwrap();

        let mut other_mission = context.mission.clone();
        other_mission.mission_id = "polymarket-btc-5m-other-mission".to_owned();
        let error = BinaryProbabilityModel::load_bundle(&bundle, &digest, &other_mission)
            .expect_err("cross-mission bundle must fail");
        assert!(error.to_string().contains("different prediction mission"));

        let manifest_path = bundle.join(MANIFEST_FILE);
        let mut manifest_bytes = fs::read(&manifest_path).unwrap();
        manifest_bytes.push(b'\n');
        fs::write(&manifest_path, manifest_bytes).unwrap();
        let error = BinaryProbabilityModel::load_bundle(&bundle, &digest, &context.mission)
            .expect_err("tampered manifest must fail");
        assert!(error
            .to_string()
            .contains("manifest SHA-256 does not match trusted registry digest"));
    }

    #[test]
    fn load_rejects_burnpack_metadata_that_disagrees_with_manifest() {
        let context = context();
        let mut model = train_event_disjoint_binary(
            &context.snapshot,
            &context.mission,
            &context.split,
            test_config(),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("binary-research-model");
        model.save_bundle(&bundle).unwrap();

        let model_path = bundle.join(MODEL_FILE);
        let mut burnpack = fs::read(&model_path).unwrap();
        let metadata_size = u32::from_le_bytes(burnpack[6..10].try_into().unwrap()) as usize;
        let metadata = &burnpack[BURNPACK_HEADER_BYTES..BURNPACK_HEADER_BYTES + metadata_size];
        let marker = b"research_only";
        let marker_start = metadata
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("artifact_scope metadata marker");
        burnpack[BURNPACK_HEADER_BYTES + marker_start + marker.len() - 1] = b'x';
        fs::write(&model_path, burnpack).unwrap();

        let manifest_path = bundle.join(MANIFEST_FILE);
        let mut manifest: BinaryModelManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let model_sha256 = sha256_file(&model_path).unwrap();
        manifest.model_sha256 = Some(model_sha256.clone());
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let digest = BinaryBundleDigest {
            manifest_sha256: sha256_file(&manifest_path).unwrap(),
            model_sha256,
        };

        let error = BinaryProbabilityModel::load_bundle(&bundle, &digest, &context.mission)
            .expect_err("Burnpack metadata mismatch must fail");
        assert!(error.to_string().contains("artifact_scope"));
    }
}
