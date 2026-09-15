//! Descriptive label-space diagnostics on the already partitioned search view.
//! A thin percentile never cancels a member or authorizes a trading decision.
use crate::{
    baselines::CexSupervisedDecisionPolicyV2,
    evaluation::{EngineContext, PreparedDataset, ResearchRow},
};
use alpha_domain::EvaluationProtocolV1;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelSpacePrecheckV1 {
    pub schema_version: String,
    pub scope: String,
    pub horizon_millis: u64,
    pub observations: usize,
    pub excluded_unmatured_labels: usize,
    pub absolute_p50_bps: Option<f64>,
    pub absolute_p95_bps: Option<f64>,
    pub absolute_max_bps: Option<f64>,
    pub entry_cost_min_bps: Option<f64>,
    pub entry_cost_max_bps: Option<f64>,
    pub entry_cost_median_bps: Option<f64>,
    pub median_spread_bps: Option<f64>,
    pub observations_above_cost: usize,
    pub fraction_above_cost: Option<f64>,
    pub cancels_experiment: bool,
}

pub fn label_space_precheck(
    context: &EngineContext<'_>,
    policy: &CexSupervisedDecisionPolicyV2,
) -> Result<LabelSpacePrecheckV1, String> {
    policy.validate()?;
    let rows = context.rows();
    let frequency = context.protocol().labels.observation_frequency_millis;
    let horizon_millis = frequency
        .checked_mul(context.protocol().labels.horizon_buckets as u64)
        .ok_or("label horizon overflow")?;
    let mut visible = vec![false; rows.len()];
    // Never inspect independent selection or sealed-test labels. A union avoids
    // counting a row again when a later expanding train fold includes it.
    for fold in context.folds() {
        for index in fold.train.clone().chain(fold.validation.clone()) {
            visible[index] = true;
        }
    }
    let cutoff = rows.last().ok_or("empty search view")?.available_time;
    summarize(
        rows.iter()
            .zip(visible)
            .filter_map(|(row, visible)| visible.then_some(row)),
        cutoff,
        false,
        context.protocol(),
        policy,
        horizon_millis,
        "search_visible_development_and_validation_overlapping_labels",
    )
}

pub fn dataset_label_space_precheck(
    dataset: &PreparedDataset,
    policy: &CexSupervisedDecisionPolicyV2,
) -> Result<LabelSpacePrecheckV1, String> {
    match dataset.development_rows() {
        Some(rows) => development_label_space_precheck(rows, dataset.protocol(), policy),
        None => label_space_precheck(&dataset.engine_context(), policy),
    }
}

/// Filtering happens before a label or spread is inspected. Selection and
/// sealed values cannot affect this report, including its tail exclusions.
pub fn development_label_space_precheck(
    rows: &[ResearchRow],
    protocol: &EvaluationProtocolV1,
    policy: &CexSupervisedDecisionPolicyV2,
) -> Result<LabelSpacePrecheckV1, String> {
    protocol.validate().map_err(|error| error.to_string())?;
    policy.validate()?;
    let calendar = &protocol
        .calendar
        .as_ref()
        .ok_or("development precheck requires a calendar")?
        .calendar;
    let horizon = protocol
        .labels
        .observation_frequency_millis
        .checked_mul(protocol.labels.horizon_buckets as u64)
        .ok_or("label horizon overflow")?;
    summarize(
        rows.iter().filter(|row| {
            row.available_time >= calendar.start && row.available_time < calendar.develop_end
        }),
        calendar.develop_end,
        true,
        protocol,
        policy,
        horizon,
        "calendar_development_only_overlapping_labels",
    )
}

#[allow(clippy::too_many_arguments)]
fn summarize<'a>(
    rows: impl Iterator<Item = &'a ResearchRow>,
    cutoff: DateTime<Utc>,
    exclusive: bool,
    protocol: &EvaluationProtocolV1,
    policy: &CexSupervisedDecisionPolicyV2,
    horizon_millis: u64,
    scope: &str,
) -> Result<LabelSpacePrecheckV1, String> {
    let mut magnitudes = Vec::new();
    let mut costs = Vec::new();
    let mut spreads = Vec::new();
    let mut above = 0;
    let mut excluded = 0;
    for row in rows {
        if row.label_available_time > cutoff || (exclusive && row.label_available_time == cutoff) {
            excluded += 1;
            continue;
        }
        let cost = crate::baselines::decision_costs(row, &protocol.costs)?;
        let threshold =
            (policy.round_trip_cost_multiplier * cost.one_way_cost_bps + cost.funding_bps).max(0.0);
        let magnitude = row.label.abs() * 10_000.0;
        if !magnitude.is_finite() || !threshold.is_finite() {
            return Err("non-finite precheck label or cost".into());
        }
        above += usize::from(magnitude > threshold);
        magnitudes.push(magnitude);
        costs.push(threshold);
        if let Some(spread) = row.features.get("spread_bps") {
            if !spread.is_finite() || *spread < 0.0 {
                return Err("invalid precheck spread".into());
            }
            spreads.push(*spread);
        }
    }
    magnitudes.sort_by(f64::total_cmp);
    costs.sort_by(f64::total_cmp);
    spreads.sort_by(f64::total_cmp);
    let quantile = |p: f64| {
        (!magnitudes.is_empty())
            .then(|| magnitudes[((magnitudes.len() as f64 * p).ceil() as usize).saturating_sub(1)])
    };
    Ok(LabelSpacePrecheckV1 {
        schema_version: "cex-label-space-precheck-v1".into(),
        scope: scope.into(),
        horizon_millis,
        observations: magnitudes.len(),
        excluded_unmatured_labels: excluded,
        absolute_p50_bps: quantile(0.5),
        absolute_p95_bps: quantile(0.95),
        absolute_max_bps: magnitudes.last().copied(),
        entry_cost_min_bps: costs.first().copied(),
        entry_cost_max_bps: costs.last().copied(),
        entry_cost_median_bps: median(&costs),
        median_spread_bps: median(&spreads),
        observations_above_cost: above,
        fraction_above_cost: (!magnitudes.is_empty())
            .then(|| above as f64 / magnitudes.len() as f64),
        cancels_experiment: false,
    })
}

pub(crate) fn median(sorted: &[f64]) -> Option<f64> {
    (!sorted.is_empty())
        .then(|| sorted[(sorted.len() - 1) / 2] / 2.0 + sorted[sorted.len() / 2] / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::{prepare_dataset, ResearchRow};
    use alpha_domain::{
        EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationProtocolV1, EvaluationWalkForwardV1,
    };
    #[test]
    fn thin_precheck_preserves_rare_tails_and_excludes_selection_and_holdout() {
        let protocol = EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 20,
                validation_rows: 20,
                fold_count: 2,
                purge_rows: 5,
                embargo_rows: 5,
                sealed_holdout_rows: 20,
            },
            EvaluationCostsV1 {
                fee_bps: 2.0,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.0,
                slippage_bps: 0.0,
                cross_spread: true,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            },
            EvaluationLabelSpecV1 {
                horizon_buckets: 5,
                observation_frequency_millis: 1000,
            },
        )
        .unwrap()
        .with_independent_selection(10)
        .unwrap();
        let mut rows: Vec<_> = (0..160)
            .map(|i| ResearchRow {
                series_id: 1,
                available_time: chrono::DateTime::from_timestamp(i, 0).unwrap(),
                label_available_time: chrono::DateTime::from_timestamp(i + 5, 0).unwrap(),
                signal: 0.0,
                features: std::collections::BTreeMap::from([("spread_bps".into(), 2.0)]),
                label: if i == 10 { 0.0015 } else { 0.0001 },
                fee_bps: 2.0,
                funding_bps: 0.0,
                pit_funding: true,
                latency_bps: 0.0,
            })
            .collect();
        let policy = CexSupervisedDecisionPolicyV2::hold_to_horizon_v3(5000).unwrap();
        let original = prepare_dataset(rows.clone(), &protocol).unwrap();
        let first = label_space_precheck(&original.engine_context(), &policy).unwrap();
        assert!(!first.cancels_experiment);
        assert!(first.absolute_p95_bps < first.entry_cost_min_bps);
        assert_eq!(first.observations_above_cost, 1);
        assert!(first.fraction_above_cost.unwrap() > 0.0);
        let partitions = protocol.row_partitions(rows.len()).unwrap();
        for index in partitions
            .selection
            .unwrap()
            .chain(partitions.sealed_holdout)
        {
            rows[index].label = 987.0;
        }
        let changed = prepare_dataset(rows, &protocol).unwrap();
        assert_eq!(
            first,
            label_space_precheck(&changed.engine_context(), &policy).unwrap()
        );
    }
}
