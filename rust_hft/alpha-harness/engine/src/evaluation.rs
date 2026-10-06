use alpha_domain::{DomainError, EvaluationProtocolV1, ResearchMission};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::BTreeMap;
use std::ops::Range;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EvaluationError {
    #[error("evaluation protocol is invalid: {0}")]
    InvalidConfiguration(DomainError),
    #[error("dataset does not contain enough rows for the requested folds")]
    InsufficientRows,
    #[error("dataset available_time is not monotonic")]
    NonMonotonicAvailability,
    #[error("dataset contains a non-finite numeric value")]
    NonFiniteValue,
    #[error("dataset feature schema is empty, inconsistent, or contains an invalid field")]
    InvalidFeatureSchema,
    #[error("dataset series boundaries are invalid")]
    InvalidSeriesTopology,
    #[error("taker spread crossing requires a finite non-negative spread_bps feature")]
    InvalidSpreadFeature,
    #[error(
        "capacity checks require positive mid_price and matching top-N bid/ask depth features"
    )]
    InvalidCapacityFeature,
    #[error("prediction label is available before its declared horizon")]
    InvalidLabelAvailability,
    #[error("training label is not available before the validation window")]
    TrainingLabelUnavailable,
    #[error("validation label reaches the sealed holdout observation window")]
    ValidationLabelReachesHoldout,
    #[error("search label reaches the independent selection observation window")]
    SearchLabelReachesSelection,
    #[error("selection label reaches the sealed holdout observation window")]
    SelectionLabelReachesHoldout,
    #[error("dataset costs do not match the bound evaluation protocol")]
    ProtocolMismatch,
    #[error("native prepared metadata does not match its verified source or protocol")]
    InvalidNativePreparedEvidence,
}

pub use hft_cex_research_input::campaign::NativeResearchRowV1 as ResearchRow;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalkForwardFold {
    pub train: Range<usize>,
    pub purge: Range<usize>,
    pub validation: Range<usize>,
    pub embargo: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalkForwardPlan {
    pub folds: Vec<WalkForwardFold>,
    pub sealed_holdout: Range<usize>,
}

pub struct EngineContext<'a> {
    rows: &'a [ResearchRow],
    folds: &'a [WalkForwardFold],
    protocol: &'a EvaluationProtocolV1,
}

impl<'a> EngineContext<'a> {
    pub fn rows(&self) -> &'a [ResearchRow] {
        self.rows
    }

    pub fn folds(&self) -> &'a [WalkForwardFold] {
        self.folds
    }

    pub fn protocol(&self) -> &EvaluationProtocolV1 {
        self.protocol
    }
}

pub struct ProposalContext<'a> {
    row_count: usize,
    fold_count: usize,
    latest_signal: Option<f64>,
    objective: Option<&'a str>,
    hypothesis_scope: Option<&'a str>,
    mutable_scope: Option<&'a [String]>,
    prompt_snapshot_id: Option<&'a str>,
}

impl ProposalContext<'_> {
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn fold_count(&self) -> usize {
        self.fold_count
    }

    pub fn latest_signal(&self) -> Option<f64> {
        self.latest_signal
    }

    pub fn objective(&self) -> Option<&str> {
        self.objective
    }

    pub fn hypothesis_scope(&self) -> Option<&str> {
        self.hypothesis_scope
    }

    pub fn mutable_scope(&self) -> Option<&[String]> {
        self.mutable_scope
    }

    pub fn prompt_snapshot_id(&self) -> Option<&str> {
        self.prompt_snapshot_id
    }
}

#[derive(Debug, Clone)]
pub struct PreparedDataset {
    rows: Vec<ResearchRow>,
    feature_names: Vec<String>,
    plan: WalkForwardPlan,
    protocol: EvaluationProtocolV1,
    partitions: alpha_domain::EvaluationRowPartitionsV1,
    withheld: Option<hft_cex_research_input::campaign::NativeDatasetMetadataV1>,
}

impl PreparedDataset {
    pub fn proposal_context(&self) -> ProposalContext<'_> {
        let research_rows = &self.rows[self.partitions.search.clone()];
        ProposalContext {
            row_count: research_rows.len(),
            fold_count: self.plan.folds.len(),
            latest_signal: research_rows.last().map(|row| row.signal),
            objective: None,
            hypothesis_scope: None,
            mutable_scope: None,
            prompt_snapshot_id: None,
        }
    }

    pub fn proposal_context_for_mission<'a>(
        &'a self,
        mission: &'a ResearchMission,
    ) -> ProposalContext<'a> {
        let mut context = self.proposal_context();
        context.objective = Some(&mission.objective);
        context.hypothesis_scope = Some(&mission.hypothesis_scope);
        context.mutable_scope = Some(&mission.mutable_scope);
        context.prompt_snapshot_id = mission.prompt_snapshot_id.as_deref();
        context
    }

    pub fn engine_context(&self) -> EngineContext<'_> {
        EngineContext {
            rows: &self.rows[self.partitions.search.clone()],
            folds: &self.plan.folds,
            protocol: &self.protocol,
        }
    }

    pub fn plan(&self) -> &WalkForwardPlan {
        &self.plan
    }

    pub fn feature_names(&self) -> &[String] {
        &self.feature_names
    }

    pub fn protocol(&self) -> &EvaluationProtocolV1 {
        &self.protocol
    }
    pub fn withheld_metadata(
        &self,
    ) -> Option<&hft_cex_research_input::campaign::NativeDatasetMetadataV1> {
        self.withheld.as_ref()
    }

    /// Calendar diagnostics can see development labels only, including mature
    /// development-tail rows excluded from fitting by the stricter purge.
    pub fn development_rows(&self) -> Option<&[ResearchRow]> {
        self.protocol
            .calendar
            .as_ref()
            .and_then(|calendar| self.rows.get(..calendar.develop_end_row))
    }

    pub fn calendar_validation_rows(&self) -> Option<&[ResearchRow]> {
        self.protocol.calendar.as_ref()?;
        self.partitions
            .selection
            .as_ref()
            .and_then(|range| self.rows.get(range.clone()))
    }
}

pub(crate) fn validate_row_clocks(
    rows: &[ResearchRow],
    protocol: &EvaluationProtocolV1,
) -> Result<(), EvaluationError> {
    if rows
        .windows(2)
        .any(|pair| pair[0].available_time >= pair[1].available_time)
    {
        return Err(EvaluationError::NonMonotonicAvailability);
    }
    let duration = u64::try_from(protocol.labels.horizon_buckets)
        .ok()
        .and_then(|count| count.checked_mul(protocol.labels.observation_frequency_millis))
        .and_then(|value| i64::try_from(value).ok())
        .and_then(chrono::TimeDelta::try_milliseconds)
        .ok_or(EvaluationError::InvalidLabelAvailability)?;
    if rows.iter().any(|row| {
        row.available_time
            .checked_add_signed(duration)
            .is_none_or(|earliest| row.label_available_time < earliest)
    }) {
        return Err(EvaluationError::InvalidLabelAvailability);
    }
    Ok(())
}

pub fn prepare_dataset(
    rows: Vec<ResearchRow>,
    protocol: &EvaluationProtocolV1,
) -> Result<PreparedDataset, EvaluationError> {
    protocol
        .validate()
        .map_err(EvaluationError::InvalidConfiguration)?;
    if let Some(binding) = &protocol.calendar {
        let clocks = rows
            .iter()
            .map(|row| row.available_time)
            .collect::<Vec<_>>();
        if binding.calendar.resolve(&clocks, &protocol.labels).as_ref() != Ok(binding) {
            return Err(EvaluationError::ProtocolMismatch);
        }
    }
    let config = &protocol.walk_forward;
    let registered_features = validate_numeric_rows(&rows, protocol)?;
    if rows.len() <= config.sealed_holdout_rows {
        return Err(EvaluationError::InsufficientRows);
    }
    let partitions = protocol
        .row_partitions(rows.len())
        .map_err(|_| EvaluationError::InsufficientRows)?;
    let holdout_start = partitions.sealed_holdout.start;
    if let Some(selection) = &partitions.selection {
        if rows[partitions.search.clone()]
            .iter()
            .any(|row| row.label_available_time >= rows[selection.start].available_time)
        {
            return Err(EvaluationError::SearchLabelReachesSelection);
        }
        if rows[selection.clone()]
            .iter()
            .any(|row| row.label_available_time >= rows[holdout_start].available_time)
        {
            return Err(EvaluationError::SelectionLabelReachesHoldout);
        }
    }
    let folds = development_folds(
        &rows,
        protocol,
        &partitions,
        rows[holdout_start].available_time,
    )?;
    Ok(PreparedDataset {
        rows,
        feature_names: registered_features,
        plan: WalkForwardPlan {
            folds,
            sealed_holdout: holdout_start..holdout_start + config.sealed_holdout_rows,
        },
        protocol: protocol.clone(),
        partitions,
        withheld: None,
    })
}

fn validate_numeric_rows(
    rows: &[ResearchRow],
    protocol: &EvaluationProtocolV1,
) -> Result<Vec<String>, EvaluationError> {
    if rows.iter().any(|row| {
        [
            row.signal,
            row.label,
            row.fee_bps,
            row.funding_bps,
            row.latency_bps,
        ]
        .iter()
        .any(|value| !value.is_finite())
            || row.features.values().any(|value| !value.is_finite())
    }) {
        return Err(EvaluationError::NonFiniteValue);
    }
    if rows.iter().any(|row| {
        row.fee_bps.to_bits() != protocol.costs.fee_bps.to_bits()
            || if row.pit_funding {
                row.funding_bps < 0.0 || row.funding_bps > protocol.costs.funding_bps
            } else {
                row.funding_bps.to_bits() != protocol.costs.funding_bps.to_bits()
            }
            || row.latency_bps.to_bits() != protocol.costs.latency_bps.to_bits()
    }) {
        return Err(EvaluationError::ProtocolMismatch);
    }
    let feature_names = rows
        .first()
        .map(|row| row.features.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if feature_names
        .iter()
        .any(|name| name.trim().is_empty() || name == "signal")
        || rows
            .iter()
            .any(|row| row.features.keys().cloned().collect::<Vec<_>>() != feature_names)
    {
        return Err(EvaluationError::InvalidFeatureSchema);
    }
    if rows.first().is_some_and(|row| row.series_id != 1)
        || rows.windows(2).any(|window| match window[1].series_id {
            id if id == window[0].series_id => false,
            id if id == window[0].series_id + 1 => false,
            _ => true,
        })
    {
        return Err(EvaluationError::InvalidSeriesTopology);
    }
    if protocol.costs.cross_spread
        && rows.iter().any(|row| {
            row.features
                .get("spread_bps")
                .is_none_or(|spread| *spread < 0.0)
        })
    {
        return Err(EvaluationError::InvalidSpreadFeature);
    }
    if protocol.costs.capacity_enabled() {
        let bid_depth = format!("bid_depth_top{}", protocol.costs.capacity_depth_levels);
        let ask_depth = format!("ask_depth_top{}", protocol.costs.capacity_depth_levels);
        if rows.iter().any(|row| {
            ["mid_price", bid_depth.as_str(), ask_depth.as_str()]
                .iter()
                .any(|feature| {
                    row.features
                        .get(*feature)
                        .is_none_or(|value| !value.is_finite() || *value <= 0.0)
                })
        }) {
            return Err(EvaluationError::InvalidCapacityFeature);
        }
    }
    let mut registered_features = vec!["signal".to_string()];
    registered_features.extend(feature_names);
    validate_row_clocks(rows, protocol)?;
    Ok(registered_features)
}

fn development_folds(
    rows: &[ResearchRow],
    protocol: &EvaluationProtocolV1,
    partitions: &alpha_domain::EvaluationRowPartitionsV1,
    holdout_start_time: DateTime<Utc>,
) -> Result<Vec<WalkForwardFold>, EvaluationError> {
    let config = &protocol.walk_forward;
    let fold_step = config
        .validation_rows
        .checked_add(config.embargo_rows)
        .ok_or(EvaluationError::InvalidConfiguration(
            DomainError::InvalidEvaluationProtocol,
        ))?;
    let mut folds = Vec::with_capacity(config.fold_count);
    for fold_index in 0..config.fold_count {
        let train_end = fold_index
            .checked_mul(fold_step)
            .and_then(|offset| config.initial_train_rows.checked_add(offset))
            .ok_or(EvaluationError::InsufficientRows)?;
        let validation_start = train_end
            .checked_add(config.purge_rows)
            .ok_or(EvaluationError::InsufficientRows)?;
        let validation_end = validation_start
            .checked_add(config.validation_rows)
            .ok_or(EvaluationError::InsufficientRows)?;
        let embargo_end = validation_end
            .checked_add(config.embargo_rows)
            .ok_or(EvaluationError::InsufficientRows)?;
        if embargo_end > partitions.search.end {
            return Err(EvaluationError::InsufficientRows);
        }
        if rows[..train_end]
            .iter()
            .any(|row| row.label_available_time >= rows[validation_start].available_time)
        {
            return Err(EvaluationError::TrainingLabelUnavailable);
        }
        if rows[validation_start..validation_end]
            .iter()
            .any(|row| row.label_available_time >= holdout_start_time)
        {
            return Err(EvaluationError::ValidationLabelReachesHoldout);
        }
        folds.push(WalkForwardFold {
            train: 0..train_end,
            purge: train_end..validation_start,
            validation: validation_start..validation_end,
            embargo: validation_end..embargo_end,
        });
    }
    Ok(folds)
}

/// Actual development bytes with original full-data schedule. No synthetic suffix rows.
pub fn prepare_native_campaign_dataset(
    input: &hft_cex_research_input::campaign::VerifiedCampaignPreparedInputsV1,
    protocol: &EvaluationProtocolV1,
) -> Result<PreparedDataset, EvaluationError> {
    protocol
        .validate()
        .map_err(EvaluationError::InvalidConfiguration)?;
    let metadata = input.original_metadata();
    // The opaque importer already bound protocol bytes and native ResearchRow
    // content. Reuse that immutable proof; only verify this caller's protocol.
    if serde_json::from_str::<EvaluationProtocolV1>(&metadata.protocol_json)
        .ok()
        .as_ref()
        != Some(protocol)
    {
        return Err(EvaluationError::InvalidNativePreparedEvidence);
    }
    let partitions = protocol
        .row_partitions(metadata.total_rows)
        .map_err(|_| EvaluationError::InvalidNativePreparedEvidence)?;
    if partitions.search != metadata.search_rows
        || partitions.selection != metadata.selection.as_ref().map(|p| p.original_rows.clone())
        || partitions.sealed_holdout != metadata.holdout.original_rows
        || input.rows().len() != metadata.visible_rows.end
        || metadata.visible_rows.start != 0
        || protocol
            .calendar
            .as_ref()
            .is_some_and(|c| c.develop_end_row > metadata.visible_rows.end)
    {
        return Err(EvaluationError::InvalidNativePreparedEvidence);
    }
    let rows = input.rows().to_vec();
    let feature_names = validate_numeric_rows(&rows, protocol)?;
    if rows.first().is_none_or(|r| {
        r.available_time.timestamp_nanos_opt() != Some(metadata.development_window.start_ns)
    }) || rows.iter().any(|r| {
        r.available_time
            .timestamp_nanos_opt()
            .is_none_or(|t| t >= metadata.development_window.end_ns)
    }) {
        return Err(EvaluationError::InvalidNativePreparedEvidence);
    }
    if let Some(selection) = &metadata.selection {
        let boundary = DateTime::from_timestamp_nanos(selection.window.start_ns);
        if rows[partitions.search.clone()]
            .iter()
            .any(|r| r.label_available_time >= boundary)
        {
            return Err(EvaluationError::SearchLabelReachesSelection);
        }
    }
    let folds = development_folds(
        &rows,
        protocol,
        &partitions,
        DateTime::from_timestamp_nanos(metadata.holdout.window.start_ns),
    )?;
    Ok(PreparedDataset {
        rows,
        feature_names,
        plan: WalkForwardPlan {
            folds,
            sealed_holdout: partitions.sealed_holdout.clone(),
        },
        protocol: protocol.clone(),
        partitions,
        withheld: Some(metadata.clone()),
    })
}

/// Add the independently authorized selection partition to a development-only
/// native dataset. The sealed rows remain absent and inaccessible.
pub fn attach_native_selection_rows(
    mut dataset: PreparedDataset,
    selection_rows: Vec<ResearchRow>,
) -> Result<PreparedDataset, EvaluationError> {
    let metadata = dataset
        .withheld
        .as_ref()
        .ok_or(EvaluationError::InvalidNativePreparedEvidence)?;
    let selection = metadata
        .selection
        .as_ref()
        .ok_or(EvaluationError::InvalidNativePreparedEvidence)?;
    if dataset.rows.len() != selection.original_rows.start
        || selection_rows.len() != selection.original_rows.len()
        || dataset.partitions.selection.as_ref() != Some(&selection.original_rows)
        || dataset.partitions.sealed_holdout.start != metadata.holdout.original_rows.start
    {
        return Err(EvaluationError::InvalidNativePreparedEvidence);
    }
    validate_partition_clock(&selection_rows, &selection.window)?;
    let holdout_start = DateTime::from_timestamp_nanos(metadata.holdout.window.start_ns);
    if dataset.rows[..dataset.partitions.search.end]
        .iter()
        .any(|row| {
            row.label_available_time >= DateTime::from_timestamp_nanos(selection.window.start_ns)
        })
        || selection_rows
            .iter()
            .any(|row| row.label_available_time >= holdout_start)
    {
        return Err(EvaluationError::SelectionLabelReachesHoldout);
    }
    dataset.rows.extend(selection_rows);
    dataset.feature_names = validate_numeric_rows(&dataset.rows, &dataset.protocol)?;
    dataset.plan.folds = development_folds(
        &dataset.rows,
        &dataset.protocol,
        &dataset.partitions,
        holdout_start,
    )?;
    Ok(dataset)
}

/// Attach sealed data only after the caller has frozen a candidate and admitted
/// the one-time holdout claim. This function itself has no I/O or claim power;
/// callers supply bytes only after that authority transition.
pub fn attach_native_sealed_holdout_rows(
    mut dataset: PreparedDataset,
    sealed_rows: Vec<ResearchRow>,
) -> Result<PreparedDataset, EvaluationError> {
    let metadata = dataset
        .withheld
        .as_ref()
        .ok_or(EvaluationError::InvalidNativePreparedEvidence)?;
    let holdout = &metadata.holdout;
    let selection_end = metadata
        .selection
        .as_ref()
        .ok_or(EvaluationError::InvalidNativePreparedEvidence)?
        .original_rows
        .end;
    if dataset.rows.len() != selection_end
        || sealed_rows.len() != metadata.total_rows.saturating_sub(selection_end)
        || dataset.partitions.sealed_holdout != holdout.original_rows
    {
        return Err(EvaluationError::InvalidNativePreparedEvidence);
    }
    let holdout_offset = holdout.original_rows.start.saturating_sub(selection_end);
    let holdout_rows = sealed_rows
        .get(holdout_offset..)
        .ok_or(EvaluationError::InvalidNativePreparedEvidence)?;
    validate_partition_clock(holdout_rows, &holdout.window)?;
    dataset.rows.extend(sealed_rows);
    if dataset.rows.len() != metadata.total_rows {
        return Err(EvaluationError::InvalidNativePreparedEvidence);
    }
    dataset.feature_names = validate_numeric_rows(&dataset.rows, &dataset.protocol)?;
    if let Some(binding) = &dataset.protocol.calendar {
        let clocks = dataset
            .rows
            .iter()
            .map(|row| row.available_time)
            .collect::<Vec<_>>();
        if binding
            .calendar
            .resolve(&clocks, &dataset.protocol.labels)
            .as_ref()
            != Ok(binding)
        {
            return Err(EvaluationError::ProtocolMismatch);
        }
    }
    validate_row_clocks(&dataset.rows, &dataset.protocol)?;
    Ok(dataset)
}

fn validate_partition_clock(
    rows: &[ResearchRow],
    window: &hft_cex_research_input::data::Window,
) -> Result<(), EvaluationError> {
    let Some(first) = rows.first() else {
        return Err(EvaluationError::InvalidNativePreparedEvidence);
    };
    if first.available_time.timestamp_nanos_opt() != Some(window.start_ns)
        || rows.iter().any(|row| {
            row.available_time
                .timestamp_nanos_opt()
                .is_none_or(|time| time < window.start_ns || time >= window.end_ns)
        })
        || rows
            .windows(2)
            .any(|pair| pair[0].available_time >= pair[1].available_time)
    {
        return Err(EvaluationError::InvalidNativePreparedEvidence);
    }
    Ok(())
}

/// Final evaluation has a separate entrypoint. Search/proposal contexts never
/// acquire this view; its caller must hold the durable final dispatch authority.
pub(crate) fn independent_selection_rows(
    dataset: &PreparedDataset,
) -> Result<&[ResearchRow], String> {
    let range = dataset
        .partitions
        .selection
        .as_ref()
        .ok_or("independent selection was not reserved")?;
    dataset.rows.get(range.clone()).ok_or_else(|| {
        "independent selection bytes are withheld; separate final authority/input is required"
            .to_string()
    })
}

pub fn evaluate_sealed_holdout<T>(
    dataset: &PreparedDataset,
    evaluator: impl FnOnce(&[ResearchRow]) -> Result<T, String>,
) -> Result<T, String> {
    let rows = dataset
        .rows
        .get(dataset.plan.sealed_holdout.clone())
        .ok_or("sealed holdout bytes are withheld; separate final authority/input is required")?;
    evaluator(rows)
}

pub(crate) fn contiguous_series_ranges(rows: &[ResearchRow]) -> Vec<Range<usize>> {
    if rows.is_empty() {
        return vec![];
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < rows.len() {
        let series_id = rows[start].series_id;
        let mut end = start + 1;
        while end < rows.len() && rows[end].series_id == series_id {
            end += 1;
        }
        ranges.push(start..end);
        start = end;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::{EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationWalkForwardV1};
    use chrono::Duration;
    use hft_cex_research_input::{
        campaign::{NativeDatasetMetadataV1, OpaqueWithheldPartitionV1},
        data::Window,
    };

    fn rows(count: usize) -> Vec<ResearchRow> {
        let start = Utc::now();
        (0..count)
            .map(|index| ResearchRow {
                series_id: 1,
                available_time: start + Duration::seconds(index as i64),
                label_available_time: start
                    + Duration::seconds(index as i64)
                    + chrono::Duration::seconds(1),
                signal: index as f64,
                features: BTreeMap::new(),
                label: index as f64 * 0.01,
                fee_bps: 1.0,
                funding_bps: 0.1,
                pit_funding: false,
                latency_bps: 0.2,
            })
            .collect()
    }

    fn protocol() -> EvaluationProtocolV1 {
        EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 20,
                validation_rows: 5,
                fold_count: 3,
                purge_rows: 2,
                embargo_rows: 1,
                sealed_holdout_rows: 10,
            },
            EvaluationCostsV1 {
                fee_bps: 1.0,
                rebate_bps: 0.0,
                funding_bps: 0.1,
                latency_bps: 0.2,
                slippage_bps: 0.0,
                cross_spread: false,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            },
            EvaluationLabelSpecV1 {
                horizon_buckets: 1,
                observation_frequency_millis: 1_000,
            },
        )
        .unwrap()
    }

    fn partial_native_fixture() -> (PreparedDataset, Vec<ResearchRow>) {
        let protocol = protocol().with_independent_selection(7).unwrap();
        let all_rows = rows(61);
        let mut dataset = prepare_dataset(all_rows.clone(), &protocol).unwrap();
        let partitions = dataset.partitions.clone();
        let selection = partitions.selection.clone().unwrap();
        let holdout = partitions.sealed_holdout.clone();
        let ns = |index: usize| {
            all_rows[index]
                .available_time
                .timestamp_nanos_opt()
                .unwrap()
        };
        let metadata = NativeDatasetMetadataV1 {
            total_rows: all_rows.len(),
            original_window: Window {
                start_ns: ns(0),
                end_ns: ns(60) + 1,
            },
            original_rows_sha256: "a".repeat(64),
            protocol_json: serde_json::to_string(&protocol).unwrap(),
            protocol_sha256: "b".repeat(64),
            search_rows: partitions.search.clone(),
            visible_rows: 0..selection.start,
            development_window: Window {
                start_ns: ns(0),
                end_ns: ns(selection.start - 1) + 1,
            },
            authorized_context_end_ns: ns(selection.start) - 1,
            selection: Some(OpaqueWithheldPartitionV1 {
                original_rows: selection.clone(),
                window: Window {
                    start_ns: ns(selection.start),
                    end_ns: ns(selection.end),
                },
                source_content_sha256: "c".repeat(64),
            }),
            holdout: OpaqueWithheldPartitionV1 {
                original_rows: holdout.clone(),
                window: Window {
                    start_ns: ns(holdout.start),
                    end_ns: ns(holdout.end - 1) + 1,
                },
                source_content_sha256: "d".repeat(64),
            },
        };
        dataset.rows.truncate(selection.start);
        dataset.withheld = Some(metadata);
        (dataset, all_rows)
    }

    #[test]
    fn native_final_rows_are_staged_and_sealed_rows_stay_unavailable_until_attached() {
        let (dataset, all_rows) = partial_native_fixture();
        assert!(independent_selection_rows(&dataset)
            .unwrap_err()
            .contains("withheld"));
        assert!(evaluate_sealed_holdout(&dataset, |_| Ok(())).is_err());

        let selection_ready = attach_native_selection_rows(dataset, all_rows[42..49].to_vec())
            .expect("independent selection rows");
        assert_eq!(
            independent_selection_rows(&selection_ready).unwrap().len(),
            7
        );
        let called = std::cell::Cell::new(false);
        assert!(evaluate_sealed_holdout(&selection_ready, |_| {
            called.set(true);
            Ok(())
        })
        .is_err());
        assert!(
            !called.get(),
            "selection access must not invoke the sealed evaluator"
        );

        let sealed_ready =
            attach_native_sealed_holdout_rows(selection_ready, all_rows[49..].to_vec())
                .expect("embargo context and sealed rows after final authority transition");
        assert_eq!(
            evaluate_sealed_holdout(&sealed_ready, |rows| Ok(rows.len())).unwrap(),
            10
        );
    }

    #[test]
    fn native_final_rows_reject_bad_selection_extent_and_cross_partition_labels() {
        let (dataset, all_rows) = partial_native_fixture();
        assert!(attach_native_selection_rows(dataset, all_rows[42..51].to_vec()).is_err());

        let (dataset, mut all_rows) = partial_native_fixture();
        all_rows[48].label_available_time = all_rows[51].available_time;
        assert!(attach_native_selection_rows(dataset, all_rows[42..49].to_vec()).is_err());
    }

    #[test]
    fn absent_withheld_bytes_cannot_invoke_selection_or_holdout_callback() {
        let protocol = protocol().with_independent_selection(7).unwrap();
        let mut dataset = prepare_dataset(rows(61), &protocol).unwrap();
        let original = dataset.partitions.clone();
        dataset.rows.truncate(original.search.end);
        assert_eq!(dataset.engine_context().rows().len(), original.search.len());
        assert_eq!(dataset.plan.sealed_holdout, original.sealed_holdout);
        assert!(independent_selection_rows(&dataset)
            .unwrap_err()
            .contains("withheld"));
        let called = std::cell::Cell::new(false);
        let result = evaluate_sealed_holdout(&dataset, |_| {
            called.set(true);
            Ok(())
        });
        assert!(result.unwrap_err().contains("withheld"));
        assert!(!called.get());
    }

    #[test]
    fn independent_selection_is_inaccessible_to_search_and_proposals() {
        let protocol = protocol().with_independent_selection(7).unwrap();
        let partitions = protocol.row_partitions(61).unwrap();
        assert_eq!(partitions.search, 0..40);
        assert_eq!(partitions.selection, Some(42..49));
        assert_eq!(partitions.sealed_holdout, 51..61);
        let original = prepare_dataset(rows(61), &protocol).unwrap();
        // Preserve clocks, then poison every withheld signal and label.
        let mut changed_rows = original.rows.clone();
        for row in &mut changed_rows[40..] {
            row.signal = 987654.0;
            row.label = -987654.0;
        }
        let changed = prepare_dataset(changed_rows, &protocol).unwrap();
        assert_eq!(original.engine_context().rows().len(), 40);
        assert_eq!(
            original.engine_context().rows(),
            changed.engine_context().rows()
        );
        assert_eq!(original.proposal_context().row_count(), 40);
        assert_eq!(
            original.proposal_context().latest_signal(),
            changed.proposal_context().latest_signal()
        );
        assert_eq!(original.engine_context().folds().len(), 3);
    }

    #[test]
    fn independent_selection_rejects_overlap_delayed_labels_and_insufficient_rows() {
        let protocol = protocol().with_independent_selection(7).unwrap();
        assert!(matches!(
            prepare_dataset(rows(60), &protocol),
            Err(EvaluationError::InsufficientRows)
        ));
        let mut input = rows(61);
        input[39].label_available_time = input[42].available_time;
        assert!(matches!(
            prepare_dataset(input, &protocol),
            Err(EvaluationError::SearchLabelReachesSelection)
        ));
        let mut input = rows(61);
        input[48].label_available_time = input[51].available_time;
        assert!(matches!(
            prepare_dataset(input, &protocol),
            Err(EvaluationError::SelectionLabelReachesHoldout)
        ));
        let prepared = prepare_dataset(rows(61), &protocol).unwrap();
        assert_eq!(
            evaluate_sealed_holdout(&prepared, |rows| Ok(rows.len())).unwrap(),
            10
        );
    }

    #[test]
    fn delayed_training_labels_cannot_enter_a_validation_window() {
        let mut input = rows(500);
        let config = protocol();
        let prepared = prepare_dataset(input.clone(), &config).unwrap();
        let fold = &prepared.engine_context().folds()[0];
        input[fold.train.end - 1].label_available_time =
            input[fold.validation.start].available_time;
        assert!(matches!(
            prepare_dataset(input, &config),
            Err(EvaluationError::TrainingLabelUnavailable)
        ));
        let mut input = rows(500);
        input[0].label_available_time = input[0].available_time;
        assert!(matches!(
            prepare_dataset(input, &config),
            Err(EvaluationError::InvalidLabelAvailability)
        ));
    }

    #[test]
    fn frozen_selection_uses_only_its_reserved_rows_and_holdout_is_separate() {
        use alpha_domain::{frozen_model::*, CexResearchContentRefV1, FormulaEvaluatorConfig};
        use hft_factor_dsl::{model_program::*, FactorAst, FactorTerminal};
        use hft_research_manifest::model::{
            CexBaselineModelV1, CexDecisionCostsV1, CexSupervisedDecisionPolicyV2,
        };
        let protocol = protocol().with_independent_selection(7).unwrap();
        let mut input = rows(61);
        for (index, row) in input.iter_mut().enumerate() {
            row.features.insert(
                "book_imbalance".into(),
                if index % 2 == 0 { 0.5 } else { -0.5 },
            );
            row.features
                .insert("mid_price".into(), 100.0 + index as f64 * 0.1);
        }
        let reference = CexResearchContentRefV1 {
            id: "test-source".into(),
            content_sha256: "a".repeat(64),
        };
        let candidate = FrozenSupervisedCandidateV1 {
            schema_version: FROZEN_SUPERVISED_CANDIDATE_SCHEMA.into(),
            artifact_id: String::new(),
            source_candidate: reference.clone(),
            source_model: reference.clone(),
            source_factor_bank: reference.clone(),
            source_fold: reference.clone(),
            research_dataset: reference,
            evaluation_protocol_sha256: protocol.content_hash().unwrap(),
            evaluator_config: FormulaEvaluatorConfig {
                min_validation_rows: 2,
                min_trades: 1,
                ..FormulaEvaluatorConfig::default()
            },
            program: FrozenFactorModelV1 {
                schema_version: FROZEN_FACTOR_MODEL_SCHEMA_V1.into(),
                venue: "binance".into(),
                market: "usdm".into(),
                symbol: "BTCUSDT".into(),
                observation_frequency_millis: 1000,
                label_horizon_buckets: 1,
                factors: vec![FrozenModelFactorV1 {
                    ast: FactorAst::Terminal(FactorTerminal::Field("book_imbalance".into())),
                    negative: false,
                }],
                model: CexBaselineModelV1::Ridge {
                    intercept: 0.0,
                    means: vec![0.0],
                    scales: vec![1.0],
                    coefficients: vec![1.0],
                },
                decision_policy: CexSupervisedDecisionPolicyV2::prediction_identity_v2(),
                base_costs: CexDecisionCostsV1 {
                    one_way_cost_bps: 1.2,
                    funding_bps: 0.1,
                },
                cross_spread: false,
            },
        }
        .finalize()
        .unwrap();
        let prepared = prepare_dataset(input.clone(), &protocol).unwrap();
        let selection =
            crate::final_models::evaluate_frozen_selection(&candidate, &prepared).unwrap();
        let holdout = crate::final_models::evaluate_frozen_holdout(&candidate, &prepared).unwrap();
        assert_eq!(selection.evaluation.metrics.row_count, 7);
        assert_eq!(holdout.evaluation.metrics.row_count, 10);
        assert_eq!(
            selection.evaluation.evaluator_version,
            INDEPENDENT_SELECTION_EVALUATOR_VERSION
        );
        let range = protocol
            .row_partitions(input.len())
            .unwrap()
            .selection
            .unwrap();
        assert_eq!(
            selection.ledger.first().unwrap().available_time,
            input[range.start].available_time
        );
        assert_eq!(selection.ledger.last().unwrap().target_position, 0.0);
        for row in &mut input[prepared.plan().sealed_holdout.clone()] {
            row.features.insert("book_imbalance".into(), -0.75);
            row.features.insert("mid_price".into(), 999.0);
            row.label = -100.0;
        }
        let poisoned = prepare_dataset(input, &protocol).unwrap();
        assert_eq!(
            crate::final_models::evaluate_frozen_selection(&candidate, &poisoned).unwrap(),
            selection
        );
        assert_ne!(
            crate::final_models::evaluate_frozen_holdout(&candidate, &poisoned).unwrap(),
            holdout
        );
        let mut wrong_clock = candidate.clone();
        wrong_clock.program.observation_frequency_millis = 2000;
        wrong_clock = wrong_clock.finalize().unwrap();
        assert!(crate::final_models::evaluate_frozen_selection(&wrong_clock, &prepared).is_err());
    }

    #[test]
    fn validation_labels_cannot_read_into_sealed_holdout() {
        let mut input = rows(50);
        let config = protocol();
        let prepared = prepare_dataset(input.clone(), &config).unwrap();
        let last_validation = prepared.plan().folds.last().unwrap().validation.end - 1;
        input[last_validation].label_available_time =
            input[prepared.plan().sealed_holdout.start].available_time;
        assert!(matches!(
            prepare_dataset(input, &config),
            Err(EvaluationError::ValidationLabelReachesHoldout)
        ));
    }

    #[test]
    fn walk_forward_builds_purged_embargoed_folds() {
        let dataset = prepare_dataset(rows(50), &protocol()).unwrap();
        assert_eq!(dataset.plan().folds.len(), 3);
        assert_eq!(dataset.plan().folds[0].train, 0..20);
        assert_eq!(dataset.plan().folds[0].purge, 20..22);
        assert_eq!(dataset.plan().folds[0].validation, 22..27);
        assert_eq!(dataset.plan().folds[0].embargo, 27..28);
        assert_eq!(dataset.plan().folds[1].train, 0..26);
        assert_eq!(dataset.plan().folds[1].purge, 26..28);
        assert_eq!(dataset.plan().folds[1].validation, 28..33);
        assert!(dataset.plan().folds[0].embargo.end <= dataset.plan().folds[1].validation.start);
        assert_eq!(dataset.plan().sealed_holdout, 40..50);
    }

    #[test]
    fn walk_forward_rejects_purge_shorter_than_label_horizon() {
        let mut protocol = protocol();
        protocol.labels.horizon_buckets = 3;

        assert_eq!(
            prepare_dataset(rows(50), &protocol).unwrap_err(),
            EvaluationError::InvalidConfiguration(DomainError::InvalidEvaluationProtocol)
        );
    }

    #[test]
    fn walk_forward_rejects_overflowing_fold_schedule() {
        let mut protocol = protocol();
        protocol.walk_forward.fold_count = usize::MAX;

        assert_eq!(
            prepare_dataset(rows(50), &protocol).unwrap_err(),
            EvaluationError::InvalidConfiguration(DomainError::InvalidEvaluationProtocol)
        );
    }

    #[test]
    fn engine_context_cannot_read_sealed_holdout_rows() {
        let dataset = prepare_dataset(rows(50), &protocol()).unwrap();
        let context = dataset.engine_context();
        assert_eq!(context.rows().len(), 40);
        assert_eq!(
            evaluate_sealed_holdout(&dataset, |rows| Ok(rows.len())).unwrap(),
            10
        );
    }

    #[test]
    fn proposal_context_exposes_only_label_free_metadata() {
        let dataset = prepare_dataset(rows(50), &protocol()).unwrap();
        let context = dataset.proposal_context();

        assert_eq!(context.row_count(), 40);
        assert_eq!(context.fold_count(), 3);
        assert_eq!(context.latest_signal(), Some(39.0));
    }

    #[test]
    fn training_context_identity_is_independent_of_holdout_artifact() {
        let original_rows = rows(50);
        let mut mutated_rows = original_rows.clone();
        for row in &mut mutated_rows[40..] {
            row.signal *= -1.0;
            row.label *= -1.0;
        }
        let original = prepare_dataset(original_rows, &protocol()).unwrap();
        let mutated = prepare_dataset(mutated_rows, &protocol()).unwrap();

        assert_eq!(original.proposal_context().row_count(), 40);
        assert_eq!(mutated.proposal_context().row_count(), 40);
        assert_eq!(
            original.engine_context().rows(),
            mutated.engine_context().rows()
        );
    }

    #[test]
    fn walk_forward_rejects_non_monotonic_availability() {
        let mut rows = rows(50);
        rows.swap(1, 2);
        assert_eq!(
            prepare_dataset(rows, &protocol()).unwrap_err(),
            EvaluationError::NonMonotonicAvailability
        );
    }

    #[test]
    fn walk_forward_rejects_duplicate_availability() {
        let mut rows = rows(50);
        rows[2].available_time = rows[1].available_time;
        assert_eq!(
            prepare_dataset(rows, &protocol()).unwrap_err(),
            EvaluationError::NonMonotonicAvailability
        );
    }

    #[test]
    fn walk_forward_rejects_non_contiguous_series_ids() {
        let mut input = rows(50);
        input[0].series_id = 2;
        assert_eq!(
            prepare_dataset(input, &protocol()).unwrap_err(),
            EvaluationError::InvalidSeriesTopology
        );

        let mut skipped = rows(50);
        skipped[25].series_id = 3;
        assert_eq!(
            prepare_dataset(skipped, &protocol()).unwrap_err(),
            EvaluationError::InvalidSeriesTopology
        );
    }

    #[test]
    fn walk_forward_rejects_feature_schema_drift() {
        let mut rows = rows(50);
        rows[0].features.insert("lob_imbalance".to_string(), 0.1);

        assert_eq!(
            prepare_dataset(rows, &protocol()).unwrap_err(),
            EvaluationError::InvalidFeatureSchema
        );
    }

    #[test]
    fn dataset_rejects_costs_that_do_not_match_the_bound_protocol() {
        let mut fee_rows = rows(50);
        fee_rows[0].fee_bps = 2.0;

        assert_eq!(
            prepare_dataset(fee_rows, &protocol()).unwrap_err(),
            EvaluationError::ProtocolMismatch
        );

        let mut funding_rows = rows(50);
        funding_rows[0].funding_bps = 0.0;
        assert_eq!(
            prepare_dataset(funding_rows.clone(), &protocol()).unwrap_err(),
            EvaluationError::ProtocolMismatch
        );
        funding_rows[0].pit_funding = true;
        prepare_dataset(funding_rows, &protocol()).unwrap();
    }

    #[test]
    fn spread_crossing_fails_closed_without_non_negative_spread_rows() {
        let mut input = rows(50);
        for row in &mut input {
            row.features.insert("spread_bps".to_string(), 0.2);
        }
        input[0].features.insert("spread_bps".to_string(), -0.1);
        let mut protocol = protocol();
        protocol.costs.cross_spread = true;

        assert_eq!(
            prepare_dataset(input, &protocol).unwrap_err(),
            EvaluationError::InvalidSpreadFeature
        );
    }

    #[test]
    fn spread_crossing_fails_closed_for_missing_or_non_finite_spread() {
        let mut missing = rows(50);
        let mut non_finite = rows(50);
        for row in &mut missing {
            row.features.remove("spread_bps");
        }
        non_finite[0]
            .features
            .insert("spread_bps".to_string(), f64::NAN);
        let mut protocol = protocol();
        protocol.costs.cross_spread = true;

        assert_eq!(
            prepare_dataset(missing, &protocol).unwrap_err(),
            EvaluationError::InvalidSpreadFeature
        );
        assert_eq!(
            prepare_dataset(non_finite, &protocol).unwrap_err(),
            EvaluationError::NonFiniteValue
        );
    }

    #[test]
    fn capacity_check_fails_closed_without_matching_positive_depth_rows() {
        let mut input = rows(50);
        for row in &mut input {
            row.features.insert("mid_price".to_string(), 60_000.0);
            row.features.insert("bid_depth_top5".to_string(), 10.0);
            row.features.insert("ask_depth_top5".to_string(), 10.0);
        }
        input[0].features.insert("ask_depth_top5".to_string(), 0.0);
        let mut protocol = protocol();
        protocol.costs.position_notional_usd = 10_000.0;
        protocol.costs.capacity_depth_levels = 5;
        protocol.costs.max_book_depth_fraction = 0.1;

        assert_eq!(
            prepare_dataset(input, &protocol).unwrap_err(),
            EvaluationError::InvalidCapacityFeature
        );
    }
}
