//! Immutable fitted-model evidence and deterministic position accounting.
//! Fitting is compiled only for the scientific worker feature.
use crate::{
    evaluation::{EngineContext, ResearchRow},
    formula_evaluator::PositionEvaluationReport,
};
use alpha_domain::{canonical_json_hash, CexBaselineArtifactV1, CexBaselineModelV1};

#[cfg(feature = "fitting")]
mod fitting;
#[cfg(all(test, feature = "fitting"))]
pub(crate) use fitting::predict_ridge;
#[cfg(feature = "fitting")]
pub use fitting::{
    evaluate_cex_baselines, evaluate_cex_supervised_model, prepare_cex_baselines,
    verify_cex_baseline_artifact, CexBaselineRun, CexBurnFitIdentity, VerifiedCexBaselineRun,
};
#[cfg(feature = "fitting")]
pub(crate) use fitting::{
    evaluate_factor_features_from_entries, fit_ridge, validate_cex_context_bindings,
};

pub fn baseline_training_admitted(artifact: &CexBaselineArtifactV1) -> bool {
    artifact.folds.iter().all(|fold| match &fold.model {
        CexBaselineModelV1::BurnMlpPortableV2 { learning, .. } => {
            learning.stability.as_ref().is_none_or(|stability| {
                !stability.controls.stop_on_convergence
                    || stability.convergence.status
                        == hft_research_manifest::mlp_training::MlpConvergenceStatusV1::Converged
            })
        }
        CexBaselineModelV1::Ridge { .. } | CexBaselineModelV1::ShallowCart { .. } => true,
        _ => false,
    })
}

pub use hft_research_manifest::model::{
    CexDecisionCostsV1, CexSupervisedDecisionPolicyV2, CexSupervisedSizingRuleV1,
};

pub use alpha_domain::frozen_model::CexSupervisedModelCandidateV2;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexSupervisedModelEvaluationV2 {
    pub candidate: CexSupervisedModelCandidateV2,
    pub predictions: Vec<f64>,
    pub target_positions: Vec<f64>,
    pub report: PositionEvaluationReport,
}

impl CexSupervisedModelEvaluationV2 {
    pub fn validate(&self) -> Result<(), String> {
        self.candidate.validate()?;
        if self.candidate.predictions_sha256
            != canonical_json_hash(&self.predictions).map_err(|error| error.to_string())?
            || self.candidate.target_positions_sha256
                != canonical_json_hash(&self.target_positions).map_err(|error| error.to_string())?
            || self.candidate.evaluation != self.report.evaluation
            || self.report.return_accounting != self.candidate.return_accounting
            || self.predictions.len() != self.target_positions.len()
            || self.report.ledger.iter().any(|point| {
                point.row_index >= self.predictions.len()
                    || point.prediction.to_bits() != self.predictions[point.row_index].to_bits()
                    || point.target_position.to_bits()
                        != self.target_positions[point.row_index].to_bits()
            })
        {
            return Err("CEX supervised model evaluation evidence drifted".to_string());
        }
        Ok(())
    }
}

pub(crate) fn supervised_target_positions(
    context: &EngineContext<'_>,
    predictions: &[f64],
    policy: &CexSupervisedDecisionPolicyV2,
) -> Result<Vec<f64>, String> {
    policy.validate()?;
    if predictions.len() != context.rows().len() {
        return Err("supervised prediction length does not match dataset".to_string());
    }
    if let Some(holding) = &policy.holding {
        return horizon_target_positions(
            context.rows(),
            predictions,
            policy,
            context.protocol(),
            context.folds().iter().map(|fold| fold.validation.clone()),
            holding,
        );
    }
    let mut positions = vec![0.0; predictions.len()];
    for fold in context.folds() {
        let mut previous_position = 0.0;
        let mut previous_series_id = None;
        for index in fold.validation.clone() {
            let row = &context.rows()[index];
            if previous_series_id != Some(row.series_id) {
                previous_position = 0.0;
                previous_series_id = Some(row.series_id);
            }
            let prediction = predictions[index];
            if !prediction.is_finite() {
                return Err("supervised model prediction is not finite".to_string());
            }
            let terminal = index + 1 == fold.validation.end
                || context.rows()[index + 1].series_id != row.series_id;
            let position =
                if terminal {
                    0.0
                } else {
                    match policy.sizing_rule {
                CexSupervisedSizingRuleV1::ExcessExpectedReturnOverRoundTripCost => {
                    cost_aware_target_position(prediction, row, &context.protocol().costs, policy)?
                }
                CexSupervisedSizingRuleV1::PredictionIdentity => {
                    policy.target_position(prediction, previous_position,
                        CexDecisionCostsV1 { one_way_cost_bps: 0.0, funding_bps: 0.0 })?
                }
                CexSupervisedSizingRuleV1::HystereticExcessExpectedReturnOverRoundTripCost => {
                    hysteretic_cost_aware_target_position(
                        prediction,
                        previous_position,
                        row,
                        &context.protocol().costs,
                        policy,
                    )?
                }
            }
                };
            positions[index] = position;
            previous_position = position;
        }
    }
    Ok(positions)
}

pub(crate) fn horizon_target_positions(
    rows: &[ResearchRow],
    predictions: &[f64],
    policy: &CexSupervisedDecisionPolicyV2,
    protocol: &alpha_domain::EvaluationProtocolV1,
    ranges: impl IntoIterator<Item = std::ops::Range<usize>>,
    holding: &hft_research_manifest::model::HorizonHoldingPolicyV1,
) -> Result<Vec<f64>, String> {
    use hft_research_manifest::model::{HorizonPositionAction, HorizonPositionState};
    let duration = holding.duration_micros()?;
    let labels = &protocol.labels;
    let label_millis = u64::try_from(labels.horizon_buckets)
        .ok()
        .and_then(|h| h.checked_mul(labels.observation_frequency_millis));
    if label_millis != Some(holding.horizon_millis) || !protocol.costs.cross_spread {
        return Err(
            "horizon holding must match the prediction label and existing taker costs".into(),
        );
    }
    if predictions.len() != rows.len() || predictions.iter().any(|v| !v.is_finite()) {
        return Err("supervised model prediction is not finite".into());
    }
    let clock = |row: &ResearchRow| {
        u64::try_from(row.available_time.timestamp_micros())
            .map_err(|_| "invalid holding decision time".to_string())
    };
    let mut positions = vec![0.0; predictions.len()];
    for range in ranges {
        if range.start >= range.end || range.end > rows.len() {
            return Err("invalid holding evaluation range".into());
        }
        // A gap is a data-quality failure, not a future-known early stop. Only
        // the predeclared evaluation end may prohibit a new entry.
        if rows[range.clone()].windows(2).any(|pair| {
            pair[0].series_id != pair[1].series_id
                || pair[1]
                    .available_time
                    .signed_duration_since(pair[0].available_time)
                    .num_milliseconds()
                    != labels.observation_frequency_millis as i64
        }) {
            return Err("holding evaluation requires one continuous instrument calendar".into());
        }
        {
            let last = clock(&rows[range.end - 1])?;
            let mut state = HorizonPositionState::default();
            for index in range {
                let row = &rows[index];
                let now = clock(row)?;
                // The evaluation calendar is fixed in advance. Never open an
                // episode that would require an early close at its known end.
                let can_enter = now.checked_add(duration).is_some_and(|due| due <= last);
                let proposed = if can_enter && !state.is_holding() {
                    Some(cost_aware_target_position(
                        predictions[index],
                        row,
                        &protocol.costs,
                        policy,
                    )?)
                } else {
                    None
                };
                let action = state.advance(holding, now, can_enter, proposed)?;
                if matches!(action, HorizonPositionAction::Exit { late: true, .. }) {
                    return Err(
                        "observations cannot close the position at its declared horizon".into(),
                    );
                }
                positions[index] = action.target();
            }
            if state.is_holding() {
                return Err("evaluation ended with an incomplete horizon position".into());
            }
        }
    }
    Ok(positions)
}

pub(crate) fn decision_costs(
    row: &ResearchRow,
    costs: &alpha_domain::EvaluationCostsV1,
) -> Result<CexDecisionCostsV1, String> {
    let half_spread = if costs.cross_spread {
        row.features
            .get("spread_bps")
            .copied()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or_else(|| "cost-aware ML decision requires spread_bps".to_string())?
            / 2.0
    } else {
        0.0
    };
    Ok(CexDecisionCostsV1 {
        one_way_cost_bps: row.fee_bps.max(0.0) - costs.rebate_bps
            + row.latency_bps.max(0.0)
            + costs.slippage_bps
            + half_spread,
        funding_bps: row.funding_bps,
    })
}

fn cost_aware_target_position(
    prediction: f64,
    row: &ResearchRow,
    costs: &alpha_domain::EvaluationCostsV1,
    policy: &CexSupervisedDecisionPolicyV2,
) -> Result<f64, String> {
    policy.target_position(prediction, 0.0, decision_costs(row, costs)?)
}

fn hysteretic_cost_aware_target_position(
    prediction: f64,
    previous_position: f64,
    row: &ResearchRow,
    costs: &alpha_domain::EvaluationCostsV1,
    policy: &CexSupervisedDecisionPolicyV2,
) -> Result<f64, String> {
    policy.target_position(prediction, previous_position, decision_costs(row, costs)?)
}
