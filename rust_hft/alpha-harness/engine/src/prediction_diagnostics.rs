//! Descriptive H1 attribution from already evaluated predictions and labels.
//! No fitting, threshold selection, policy changes or promotion authority.
use crate::{
    evaluation::{independent_selection_rows, PreparedDataset, ResearchRow},
    final_models::CalendarValidationReportV1,
    formula_evaluator::PositionEvaluationPoint,
};
use alpha_domain::{canonical_json_hash, EvaluationProtocolV1};
use hft_research_manifest::model::{CexSupervisedDecisionPolicyV2, CexSupervisedSizingRuleV1};
use serde::{Deserialize, Serialize};

pub const CALENDAR_DIAGNOSTICS_FILE: &str = "calendar-prediction-diagnostics.json";
const BPS: f64 = 10_000.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarPredictionDiagnosticsV1 {
    pub schema_version: String,
    pub scope: String,
    pub source_validation_sha256: String,
    pub decision_policy_sha256: String,
    pub counts: DecisionCounts,
    pub prediction: PredictionCalibration,
    pub cost_ratio_buckets: Vec<CostRatioBucket>,
    pub training_performed: bool,
    pub promotion_authority: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionCounts {
    pub evaluated_rows: usize,
    pub horizon_eligible_rows: usize,
    pub tail_rows: usize,
    /// Overlapping observations, never a count of executable trades.
    pub eligible_labels_above_cost: usize,
    pub eligible_predictions_above_cost: usize,
    pub eligible_nonzero_entry_signals: usize,
    pub eligible_signals_blocked_by_direction: usize,
    pub nonzero_position_rows: usize,
    pub zero_cost_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionCalibration {
    pub mean_prediction_bps: f64,
    pub mean_label_bps: f64,
    pub prediction_stddev_bps: f64,
    pub label_stddev_bps: f64,
    pub mae_bps: f64,
    pub mse_bps_squared: f64,
    pub zero_prediction_mse_bps_squared: f64,
    pub mse_over_zero_prediction: Option<f64>,
    /// Descriptive covariance / prediction variance; not a fitted trade rule.
    pub calibration_slope: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostRatioBucket {
    pub lower_inclusive: f64,
    pub upper_exclusive: Option<f64>,
    pub observations: usize,
    /// Prediction direction times forward label; not executable P&L.
    pub mean_directional_label_bps: Option<f64>,
    pub mean_entry_cost_bps: Option<f64>,
    pub directionally_correct_labels_above_cost: usize,
}

pub fn calendar_prediction_diagnostics(
    dataset: &PreparedDataset,
    validation: &CalendarValidationReportV1,
    policy: &CexSupervisedDecisionPolicyV2,
) -> Result<CalendarPredictionDiagnosticsV1, String> {
    if dataset.protocol().calendar.as_ref() != Some(&validation.calendar)
        || validation.report.evaluation.evaluation_protocol.as_ref() != Some(dataset.protocol())
        || validation.promotion_authority
    {
        return Err("prediction diagnostics require the original calendar validation".into());
    }
    let (counts, prediction, cost_ratio_buckets) = summarize(
        independent_selection_rows(dataset)?,
        &validation.report.ledger,
        dataset.protocol(),
        policy,
    )?;
    Ok(CalendarPredictionDiagnosticsV1 {
        schema_version: "monday.calendar_prediction_diagnostics.v1".into(),
        scope: "independent_calendar_validation_overlapping_labels".into(),
        source_validation_sha256: canonical_json_hash(validation).map_err(|e| e.to_string())?,
        decision_policy_sha256: policy.content_hash()?,
        counts,
        prediction,
        cost_ratio_buckets,
        training_performed: false,
        promotion_authority: false,
    })
}

type Summary = (DecisionCounts, PredictionCalibration, Vec<CostRatioBucket>);

fn summarize(
    rows: &[ResearchRow],
    ledger: &[PositionEvaluationPoint],
    protocol: &EvaluationProtocolV1,
    policy: &CexSupervisedDecisionPolicyV2,
) -> Result<Summary, String> {
    policy.validate()?;
    let holding = policy
        .holding
        .as_ref()
        .ok_or("diagnostics require horizon holding")?;
    if rows.is_empty()
        || rows.len() != ledger.len()
        || policy.sizing_rule != CexSupervisedSizingRuleV1::ExcessExpectedReturnOverRoundTripCost
        || protocol
            .labels
            .observation_frequency_millis
            .checked_mul(protocol.labels.horizon_buckets as u64)
            != Some(holding.horizon_millis)
    {
        return Err("invalid H1 prediction diagnostic inputs".into());
    }
    let duration =
        i64::try_from(holding.duration_micros()?).map_err(|_| "diagnostic horizon overflow")?;
    let end = rows.last().unwrap().available_time.timestamp_micros();
    let mut counts = DecisionCounts {
        evaluated_rows: rows.len(),
        ..Default::default()
    };
    let bounds = [0.0, 0.25, 0.5, 1.0, 2.0];
    let mut buckets: Vec<_> = bounds
        .iter()
        .enumerate()
        .map(|(i, lower)| CostRatioBucket {
            lower_inclusive: *lower,
            upper_exclusive: bounds.get(i + 1).copied(),
            observations: 0,
            mean_directional_label_bps: None,
            mean_entry_cost_bps: None,
            directionally_correct_labels_above_cost: 0,
        })
        .collect();
    let mut samples = Vec::with_capacity(rows.len());
    for (index, (row, point)) in rows.iter().zip(ledger).enumerate() {
        if point.row_index != index
            || point.available_time != row.available_time
            || point.series_id != row.series_id
            || !row.label.is_finite()
            || !point.prediction.is_finite()
            || !point.target_position.is_finite()
        {
            return Err("prediction diagnostics row identity or value mismatch".into());
        }
        let prediction = point.prediction * BPS;
        let label = row.label * BPS;
        let costs = crate::baselines::decision_costs(row, &protocol.costs)?;
        let threshold = (policy.round_trip_cost_multiplier * costs.one_way_cost_bps
            + costs.funding_bps)
            .max(0.0);
        if !prediction.is_finite() || !label.is_finite() || !threshold.is_finite() {
            return Err("prediction diagnostic units overflowed".into());
        }
        samples.push((prediction, label));
        counts.nonzero_position_rows += usize::from(point.target_position.abs() > f64::EPSILON);
        let eligible = row
            .available_time
            .timestamp_micros()
            .checked_add(duration)
            .is_some_and(|due| due <= end);
        if !eligible {
            counts.tail_rows += 1;
            continue;
        }
        counts.horizon_eligible_rows += 1;
        counts.eligible_labels_above_cost += usize::from(label.abs() > threshold);
        // Keep the exact fractional-return comparison used by the decision policy.
        let above =
            point.prediction.abs() > threshold / BPS && point.prediction.abs() > f64::EPSILON;
        counts.eligible_predictions_above_cost += usize::from(above);
        let signal = policy.target_position(point.prediction, 0.0, costs)?;
        counts.eligible_nonzero_entry_signals += usize::from(signal.abs() > f64::EPSILON);
        counts.eligible_signals_blocked_by_direction +=
            usize::from(above && policy.long_only && point.prediction < 0.0);
        if threshold == 0.0 {
            counts.zero_cost_rows += 1;
            continue;
        }
        let ratio = prediction.abs() / threshold;
        let i = bounds
            .partition_point(|lower| *lower <= ratio)
            .saturating_sub(1);
        let bucket = &mut buckets[i];
        bucket.observations += 1;
        let directional = if prediction == 0.0 {
            0.0
        } else {
            prediction.signum() * label
        };
        *bucket.mean_directional_label_bps.get_or_insert(0.0) += directional;
        *bucket.mean_entry_cost_bps.get_or_insert(0.0) += threshold;
        bucket.directionally_correct_labels_above_cost += usize::from(directional > threshold);
    }
    for bucket in &mut buckets {
        if let Some(v) = &mut bucket.mean_directional_label_bps {
            *v /= bucket.observations as f64;
        }
        if let Some(v) = &mut bucket.mean_entry_cost_bps {
            *v /= bucket.observations as f64;
        }
    }
    let n = samples.len() as f64;
    let mean_prediction = samples.iter().map(|x| x.0 / n).sum::<f64>();
    let mean_label = samples.iter().map(|x| x.1 / n).sum::<f64>();
    let variance = samples
        .iter()
        .map(|x| (x.0 - mean_prediction).powi(2) / n)
        .sum::<f64>();
    let label_variance = samples
        .iter()
        .map(|x| (x.1 - mean_label).powi(2) / n)
        .sum::<f64>();
    let covariance = samples
        .iter()
        .map(|x| (x.0 - mean_prediction) * (x.1 - mean_label) / n)
        .sum::<f64>();
    let mse = samples.iter().map(|x| (x.0 - x.1).powi(2) / n).sum::<f64>();
    let zero_mse = samples.iter().map(|x| x.1.powi(2) / n).sum::<f64>();
    let mae = samples.iter().map(|x| (x.0 - x.1).abs() / n).sum::<f64>();
    let finite_ratio = |a: f64, b: f64| (b > 0.0).then(|| a / b).filter(|v| v.is_finite());
    if [
        mean_prediction,
        mean_label,
        variance,
        label_variance,
        covariance,
        mse,
        zero_mse,
        mae,
    ]
    .iter()
    .any(|v| !v.is_finite())
        || buckets.iter().any(|b| {
            b.mean_directional_label_bps.is_some_and(|v| !v.is_finite())
                || b.mean_entry_cost_bps.is_some_and(|v| !v.is_finite())
        })
    {
        return Err("prediction diagnostic statistic overflowed".into());
    }
    Ok((
        counts,
        PredictionCalibration {
            mean_prediction_bps: mean_prediction,
            mean_label_bps: mean_label,
            prediction_stddev_bps: variance.sqrt(),
            label_stddev_bps: label_variance.sqrt(),
            mae_bps: mae,
            mse_bps_squared: mse,
            zero_prediction_mse_bps_squared: zero_mse,
            mse_over_zero_prediction: finite_ratio(mse, zero_mse),
            calibration_slope: finite_ratio(covariance, variance),
        },
        buckets,
    ))
}

#[cfg(test)]
#[path = "prediction_diagnostics_tests.rs"]
mod tests;
