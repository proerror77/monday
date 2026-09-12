use crate::{
    evaluation::{EngineContext, ResearchRow, WalkForwardFold},
    formula_evaluator::{evaluate_ast, FormulaEvaluator, PositionEvaluationReport},
};
use alpha_domain::{
    canonical_json_hash, CexBaselineArtifactV1, CexBaselineCartNodeV1, CexBaselineFoldV1,
    CexBaselineGateV1, CexBaselineModelKindV1, CexBaselineModelV1, CexBaselinePolicyV1,
    CexBaselineRangeV1, CexFactorBankRevisionV2, CexFactorOrientationV1, CexMlpFoldObservationV1,
    CexMlpTrainingProfileV1, CexResearchContentRefV1, CexResearchHypothesisTargetV1,
    EvaluationLabelSpecV1, CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
};
use hft_research_ml::{
    train_contract_model, ContractDatasetBinding, ContractTrainingConfig, ContractTrainingRow,
    FeatureName, MlpPredictionDiagnosticsV1, MlpTargetScaleV1, PositiveDurationMs,
    PurgedWalkForwardSplit, SealedTrainingRequest, Sha256Digest, SplitId, SplitRole, Symbol,
    TimestampMs, TrainingRequest, Venue,
};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::engines::solve;

const CEX_BURN_HIDDEN_DIM: usize = 8;
const CEX_BURN_EPOCHS: usize = 8;
const CEX_BURN_LEARNING_RATE: f64 = 1e-3;
const CEX_BURN_MIN_ROWS: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct CexBaselineRun {
    pub ridge: Option<CexBaselineArtifactV1>,
    pub cart: Option<CexBaselineArtifactV1>,
    pub burn: Option<CexBaselineArtifactV1>,
    pub gate: CexBaselineGateV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CexBurnFitIdentity<'a> {
    pub symbol: &'a str,
    pub venue: &'a str,
}

#[derive(Clone, Copy)]
struct CexBurnFoldFit<'a> {
    rows: &'a [ResearchRow],
    features: &'a [Vec<f64>],
    fold: &'a WalkForwardFold,
    fold_index: usize,
    mission_id: &'a str,
    identity: CexBurnFitIdentity<'a>,
    factor_ids: &'a [String],
    horizon: &'a EvaluationLabelSpecV1,
    evaluation_policy_sha256: &'a str,
    dataset_sha256: &'a str,
    profile: Option<&'a CexMlpTrainingProfileV1>,
    purpose: &'static str,
}

#[derive(Debug)]
struct CexBurnFoldOutput {
    model: CexBaselineModelV1,
    predictions: Vec<f64>,
    observation: CexMlpFoldObservationV1,
}

pub use hft_research_manifest::model::{
    CexDecisionCostsV1, CexSupervisedDecisionPolicyV2, CexSupervisedSizingRuleV1,
};

pub use alpha_domain::frozen_model::CexSupervisedModelCandidateV2;
use alpha_domain::frozen_model::CEX_SUPERVISED_CANDIDATE_SCHEMA_V2;

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

pub fn verify_cex_baseline_artifact(
    context: &EngineContext<'_>,
    factor_bank: &CexFactorBankRevisionV2,
    artifact: &CexBaselineArtifactV1,
) -> Result<(), String> {
    artifact
        .validate()
        .map_err(|error| format!("baseline artifact validation failed: {error}"))?;
    factor_bank
        .validate()
        .map_err(|error| format!("factor bank validation failed: {error}"))?;
    if let Some(profile) = &artifact.baseline_policy.mlp_training {
        profile.validate_factor_bank(factor_bank)?;
    }
    if artifact.factor_bank_revision_id != factor_bank.revision_id
        || artifact.research_dataset != factor_bank.research_dataset
        || artifact.evaluation_policy != factor_bank.evaluation_policy
        || artifact.walk_forward_partition != factor_bank.walk_forward_partition
    {
        return Err("baseline artifact Factor Bank binding drifted".to_string());
    }
    validate_context_identity(
        context,
        factor_bank,
        &artifact.evaluation_policy,
        &artifact.target,
    )?;
    let (factor_ids, factors) = evaluate_factor_features(context, factor_bank)?;
    if artifact.factor_ids != factor_ids {
        return Err("baseline artifact factor ordering drifted".to_string());
    }
    let features = transpose_factors(&factors, context.rows().len())?;
    if artifact.folds.len() != context.folds().len() {
        return Err("baseline artifact fold count drifted".to_string());
    }
    let labels = labels(context.rows());
    let mut signals = vec![0.0; context.rows().len()];
    let mut ranges = Vec::with_capacity(artifact.folds.len());
    for (fold_index, (fold, context_fold)) in artifact.folds.iter().zip(context.folds()).enumerate()
    {
        let validation = fold.validation_range.start..fold.validation_range.end;
        if fold.train_range.start != context_fold.train.start
            || fold.train_range.end != context_fold.train.end
            || fold.purge_range.start != context_fold.purge.start
            || fold.purge_range.end != context_fold.purge.end
            || fold.validation_range.start != context_fold.validation.start
            || fold.validation_range.end != context_fold.validation.end
            || fold.embargo_range.start != context_fold.embargo.start
            || fold.embargo_range.end != context_fold.embargo.end
            || validation.end > features.len()
            || fold.predictions.len() != validation.len()
        {
            return Err(format!(
                "baseline fold {} validation range drifted",
                fold_index + 1
            ));
        }
        let predictions = match &fold.model {
            CexBaselineModelV1::Ridge { .. } => {
                let fit = fit_ridge(
                    &features,
                    &labels,
                    context_fold.train.clone(),
                    artifact.baseline_policy.ridge_l2,
                )?;
                let refit_model = CexBaselineModelV1::Ridge {
                    intercept: fit.intercept,
                    means: fit.means.clone(),
                    scales: fit.scales.clone(),
                    coefficients: fit.coefficients.clone(),
                };
                if refit_model != fold.model {
                    return Err(format!(
                        "baseline fold {} Ridge model drifted",
                        fold_index + 1
                    ));
                }
                predict_fold_ridge(&fit, &features, &validation)?
            }
            CexBaselineModelV1::ShallowCart { .. } => {
                let tree = fit_cart(
                    &features,
                    &labels,
                    context_fold.train.clone(),
                    artifact.baseline_policy.cart_max_depth,
                    artifact.baseline_policy.cart_min_leaf,
                    &factor_ids,
                )?;
                let refit_model = CexBaselineModelV1::ShallowCart { root: tree.clone() };
                if refit_model != fold.model {
                    return Err(format!(
                        "baseline fold {} CART model drifted",
                        fold_index + 1
                    ));
                }
                predict_fold_cart(&tree, &features, &validation)?
            }
            CexBaselineModelV1::BurnMlp { .. } => {
                return Err(
                    "historical Burn MLP diagnostics do not contain executable parameters".into(),
                );
            }
            CexBaselineModelV1::BurnMlpPortable { symbol, venue, .. } => {
                let refit = fit_burn_fold(CexBurnFoldFit {
                    rows: context.rows(),
                    features: &features,
                    fold: context_fold,
                    fold_index: fold.fold_index,
                    mission_id: &artifact.mission_id,
                    identity: CexBurnFitIdentity { symbol, venue },
                    factor_ids: &factor_ids,
                    horizon: &artifact.target.horizon,
                    evaluation_policy_sha256: &artifact.evaluation_policy.content_sha256,
                    dataset_sha256: &factor_bank.research_dataset.content_sha256,
                    profile: artifact.baseline_policy.mlp_training.as_ref(),
                    purpose: "verification",
                })?;
                if refit.model != fold.model
                    || fold
                        .mlp_observation
                        .as_ref()
                        .map(|value| &value.validation_prediction)
                        != Some(&refit.observation.validation_prediction)
                {
                    return Err(format!(
                        "baseline fold {} Burn MLP model drifted",
                        fold_index + 1
                    ));
                }
                refit.predictions
            }
        };
        if !predictions_equal(&predictions, &fold.predictions) {
            return Err(format!(
                "baseline fold {} predictions drifted",
                fold_index + 1
            ));
        }
        for (index, prediction) in validation.clone().zip(predictions) {
            signals[index] = prediction;
        }
        ranges.push(validation);
    }
    let evaluator = FormulaEvaluator::new(artifact.baseline_policy.evaluator_config.clone())?;
    let evaluation = evaluator.evaluate_signals(
        context.rows(),
        &signals,
        ranges,
        CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
        context.protocol(),
    )?;
    if evaluation != artifact.evaluation {
        return Err("baseline evaluation drifted".to_string());
    }
    Ok(())
}

pub fn evaluate_cex_baselines(
    context: &EngineContext<'_>,
    factor_bank: &CexFactorBankRevisionV2,
    policy: &CexBaselinePolicyV1,
    mission_id: &str,
    target: CexResearchHypothesisTargetV1,
    evaluation_policy: &CexResearchContentRefV1,
    burn: Option<CexBurnFitIdentity<'_>>,
) -> Result<CexBaselineRun, String> {
    if mission_id.trim().is_empty() {
        return Err("baseline mission identity is empty".to_string());
    }
    factor_bank
        .validate()
        .map_err(|error| format!("factor bank validation failed: {error}"))?;
    policy
        .validate()
        .map_err(|error| format!("baseline policy validation failed: {error}"))?;
    evaluation_policy
        .validate()
        .map_err(|error| format!("evaluation policy validation failed: {error}"))?;
    if factor_bank.evaluation_policy != *evaluation_policy {
        return Err("baseline evaluation policy does not match the Factor Bank".to_string());
    }
    validate_context_identity(context, factor_bank, evaluation_policy, &target)?;
    if let Some(profile) = &policy.mlp_training {
        profile.validate_factor_bank(factor_bank)?;
        let mut ids: Vec<_> = factor_bank
            .entries
            .iter()
            .map(|entry| entry.factor_id.clone())
            .collect();
        ids.sort();
        profile.validate_inputs(context.folds().len(), &ids)?;
        if burn.is_none() {
            return Err("a bound MLP training profile requires the supervised MLP lane".into());
        }
    }
    if factor_bank.entries.is_empty() {
        let gate = CexBaselineGateV1::empty_factor_bank(mission_id, policy, factor_bank)
            .map_err(|error| format!("empty Factor Bank gate failed: {error}"))?;
        return Ok(CexBaselineRun {
            ridge: None,
            cart: None,
            burn: None,
            gate,
        });
    }
    let (factor_ids, factors) = evaluate_factor_features(context, factor_bank)?;
    let feature_rows = transpose_factors(&factors, context.rows().len())?;
    let ridge = fit_artifact(
        context,
        policy,
        mission_id,
        target.clone(),
        evaluation_policy,
        factor_bank,
        factor_ids.clone(),
        &feature_rows,
        BaselineKind::Ridge,
    )?;
    let cart = fit_artifact(
        context,
        policy,
        mission_id,
        target.clone(),
        evaluation_policy,
        factor_bank,
        factor_ids.clone(),
        &feature_rows,
        BaselineKind::ShallowCart,
    )?;
    let burn = burn
        .map(|identity| {
            fit_artifact(
                context,
                policy,
                mission_id,
                target,
                evaluation_policy,
                factor_bank,
                factor_ids,
                &feature_rows,
                BaselineKind::BurnMlp { identity },
            )
        })
        .transpose()?;
    let gate = CexBaselineGateV1::new(&ridge, &cart)
        .map_err(|error| format!("baseline gate failed: {error}"))?;
    Ok(CexBaselineRun {
        ridge: Some(ridge),
        cart: Some(cart),
        burn,
        gate,
    })
}

pub fn evaluate_cex_supervised_model(
    context: &EngineContext<'_>,
    factor_bank: &CexFactorBankRevisionV2,
    artifact: &CexBaselineArtifactV1,
    decision_policy: &CexSupervisedDecisionPolicyV2,
) -> Result<CexSupervisedModelEvaluationV2, String> {
    verify_cex_baseline_artifact(context, factor_bank, artifact)?;
    decision_policy.validate()?;
    let mut predictions = vec![0.0; context.rows().len()];
    let mut assigned = vec![false; context.rows().len()];
    for fold in &artifact.folds {
        for (index, prediction) in
            (fold.validation_range.start..fold.validation_range.end).zip(&fold.predictions)
        {
            if assigned[index] {
                return Err("supervised model validation ranges overlap".to_string());
            }
            predictions[index] = *prediction;
            assigned[index] = true;
        }
    }
    let target_positions = supervised_target_positions(context, &predictions, decision_policy)?;
    let evaluator = FormulaEvaluator::new(artifact.baseline_policy.evaluator_config.clone())?;
    let report = evaluator.evaluate_predictions_and_positions(
        context.rows(),
        &predictions,
        &target_positions,
        context.folds().iter().map(|fold| fold.validation.clone()),
        CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
        context.protocol(),
    )?;
    let model_sha256 = canonical_json_hash(artifact).map_err(|error| error.to_string())?;
    let candidate = CexSupervisedModelCandidateV2 {
        schema_version: CEX_SUPERVISED_CANDIDATE_SCHEMA_V2.to_string(),
        artifact_id: String::new(),
        mission_id: artifact.mission_id.clone(),
        model_artifact: CexResearchContentRefV1 {
            id: artifact.artifact_id.clone(),
            content_sha256: model_sha256,
        },
        model_kind: artifact.model_kind,
        factor_bank_revision_id: artifact.factor_bank_revision_id.clone(),
        research_dataset: artifact.research_dataset.clone(),
        walk_forward_partition: artifact.walk_forward_partition.clone(),
        evaluation_policy: artifact.evaluation_policy.clone(),
        decision_policy: decision_policy.clone(),
        predictions_sha256: canonical_json_hash(&predictions).map_err(|error| error.to_string())?,
        target_positions_sha256: canonical_json_hash(&target_positions)
            .map_err(|error| error.to_string())?,
        return_accounting: report.return_accounting,
        evaluation: report.evaluation.clone(),
        deployment_authority: false,
        order_submission_authority: false,
    }
    .finalize()?;
    let evaluation = CexSupervisedModelEvaluationV2 {
        candidate,
        predictions,
        target_positions,
        report,
    };
    evaluation.validate()?;
    Ok(evaluation)
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

fn decision_costs(
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

#[derive(Debug, Clone, Copy)]
enum BaselineKind<'a> {
    Ridge,
    ShallowCart,
    BurnMlp { identity: CexBurnFitIdentity<'a> },
}

fn validate_context_identity(
    context: &EngineContext<'_>,
    factor_bank: &CexFactorBankRevisionV2,
    evaluation_policy: &CexResearchContentRefV1,
    target: &CexResearchHypothesisTargetV1,
) -> Result<(), String> {
    if target.name.trim().is_empty() || target.horizon != context.protocol().labels {
        return Err("baseline target identity is invalid".to_string());
    }
    validate_cex_context_bindings(context, factor_bank, evaluation_policy)
}

pub(crate) fn validate_cex_context_bindings(
    context: &EngineContext<'_>,
    factor_bank: &CexFactorBankRevisionV2,
    evaluation_policy: &CexResearchContentRefV1,
) -> Result<(), String> {
    if factor_bank.research_dataset.validate().is_err()
        || factor_bank.walk_forward_partition.validate().is_err()
    {
        return Err("Factor Bank context identity is invalid".to_string());
    }
    let dataset_hash = canonical_json_hash(&context.rows())
        .map_err(|error| format!("research dataset identity failed: {error}"))?;
    if dataset_hash != factor_bank.research_dataset.content_sha256 {
        return Err("baseline research dataset identity drifted".to_string());
    }
    let partition_hash = canonical_json_hash(&json!({
        "research_dataset": &factor_bank.research_dataset,
        "folds": context.folds(),
    }))
    .map_err(|error| format!("walk-forward partition identity failed: {error}"))?;
    if partition_hash != factor_bank.walk_forward_partition.content_sha256 {
        return Err("baseline walk-forward partition identity drifted".to_string());
    }
    if context
        .protocol()
        .content_hash()
        .map_err(|error| format!("evaluation protocol identity failed: {error}"))?
        != evaluation_policy.content_sha256
    {
        return Err("baseline evaluation protocol identity drifted".to_string());
    }
    for fold in context.folds() {
        validate_fold_range(fold, context.rows().len())?;
    }
    Ok(())
}

fn validate_fold_range(fold: &WalkForwardFold, row_count: usize) -> Result<(), String> {
    if fold.train.start >= fold.train.end
        || fold.embargo.start > fold.embargo.end
        || fold.purge.start > fold.purge.end
        || fold.validation.start >= fold.validation.end
        || fold.train.end > fold.purge.start
        || fold.purge.end > fold.validation.start
        || fold.validation.end > fold.embargo.start
        || fold.embargo.end > row_count
    {
        return Err("walk-forward fold range is invalid".to_string());
    }
    Ok(())
}

fn evaluate_factor_features(
    context: &EngineContext<'_>,
    factor_bank: &CexFactorBankRevisionV2,
) -> Result<(Vec<String>, Vec<Vec<f64>>), String> {
    let mut entries = factor_bank.entries.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.factor_id.cmp(&right.factor_id));
    let factor_ids = entries
        .iter()
        .map(|entry| entry.factor_id.clone())
        .collect::<Vec<_>>();
    let factors = evaluate_factor_features_from_entries(context, &entries)?;
    Ok((factor_ids, factors))
}

pub(crate) fn evaluate_factor_features_from_entries(
    context: &EngineContext<'_>,
    entries: &[&alpha_domain::CexFactorBankEntryV1],
) -> Result<Vec<Vec<f64>>, String> {
    entries
        .iter()
        .map(|entry| {
            let mut values = evaluate_ast(&entry.canonical_ast, context.rows())?;
            if values.iter().any(|value| !value.is_finite()) {
                return Err(format!(
                    "factor {} produced a non-finite value",
                    entry.factor_id
                ));
            }
            if entry.orientation == CexFactorOrientationV1::Negative {
                for value in &mut values {
                    *value = normalize_zero(-*value);
                }
            }
            Ok(values)
        })
        .collect::<Result<Vec<Vec<f64>>, String>>()
}

fn transpose_factors(factors: &[Vec<f64>], row_count: usize) -> Result<Vec<Vec<f64>>, String> {
    if factors.is_empty() || factors.iter().any(|factor| factor.len() != row_count) {
        return Err("Factor Bank feature matrix is invalid".to_string());
    }
    let mut rows = vec![Vec::with_capacity(factors.len()); row_count];
    for factor in factors {
        for (row, value) in rows.iter_mut().zip(factor) {
            row.push(normalize_zero(*value));
        }
    }
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
fn fit_artifact(
    context: &EngineContext<'_>,
    policy: &CexBaselinePolicyV1,
    mission_id: &str,
    target: CexResearchHypothesisTargetV1,
    evaluation_policy: &CexResearchContentRefV1,
    factor_bank: &CexFactorBankRevisionV2,
    factor_ids: Vec<String>,
    features: &[Vec<f64>],
    kind: BaselineKind<'_>,
) -> Result<CexBaselineArtifactV1, String> {
    if let Some(profile) = &policy.mlp_training {
        profile.validate_inputs(context.folds().len(), &factor_ids)?;
    }
    let evaluator = FormulaEvaluator::new(policy.evaluator_config.clone())?;
    let mut folds = Vec::with_capacity(context.folds().len());
    let mut signals = vec![0.0; context.rows().len()];
    for (fold_index, fold) in context.folds().iter().enumerate() {
        let (model, predictions, mlp_observation) = match kind {
            BaselineKind::Ridge => {
                let labels = labels(context.rows());
                let fit = fit_ridge(features, &labels, fold.train.clone(), policy.ridge_l2)?;
                let predictions = predict_fold_ridge(&fit, features, &fold.validation)?;
                (
                    CexBaselineModelV1::Ridge {
                        intercept: fit.intercept,
                        means: fit.means,
                        scales: fit.scales,
                        coefficients: fit.coefficients,
                    },
                    predictions,
                    None,
                )
            }
            BaselineKind::ShallowCart => {
                let labels = labels(context.rows());
                let tree = fit_cart(
                    features,
                    &labels,
                    fold.train.clone(),
                    policy.cart_max_depth,
                    policy.cart_min_leaf,
                    &factor_ids,
                )?;
                let predictions = predict_fold_cart(&tree, features, &fold.validation)?;
                (
                    CexBaselineModelV1::ShallowCart { root: tree },
                    predictions,
                    None,
                )
            }
            BaselineKind::BurnMlp { identity } => {
                let result = fit_burn_fold(CexBurnFoldFit {
                    rows: context.rows(),
                    features,
                    fold,
                    fold_index: fold_index + 1,
                    mission_id,
                    identity,
                    factor_ids: &factor_ids,
                    horizon: &target.horizon,
                    evaluation_policy_sha256: &evaluation_policy.content_sha256,
                    dataset_sha256: &factor_bank.research_dataset.content_sha256,
                    profile: policy.mlp_training.as_ref(),
                    purpose: "training",
                })?;
                (result.model, result.predictions, Some(result.observation))
            }
        };
        for (index, prediction) in fold.validation.clone().zip(&predictions) {
            signals[index] = *prediction;
        }
        let mut recorded_fold = CexBaselineFoldV1::new(
            fold_index + 1,
            range(&fold.train),
            range(&fold.purge),
            range(&fold.validation),
            range(&fold.embargo),
            predictions,
            model,
        )
        .map_err(|error| format!("baseline fold validation failed: {error}"))?;
        recorded_fold.mlp_observation = mlp_observation;
        folds.push(recorded_fold);
    }
    let evaluation = evaluator.evaluate_signals(
        context.rows(),
        &signals,
        context.folds().iter().map(|fold| fold.validation.clone()),
        CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
        context.protocol(),
    )?;
    let model_kind = match kind {
        BaselineKind::Ridge => CexBaselineModelKindV1::Ridge,
        BaselineKind::ShallowCart => CexBaselineModelKindV1::ShallowCart,
        BaselineKind::BurnMlp { .. } => CexBaselineModelKindV1::BurnMlp,
    };
    let artifact = CexBaselineArtifactV1::new(
        mission_id.to_string(),
        factor_bank.revision_id.clone(),
        factor_ids,
        target,
        factor_bank.research_dataset.clone(),
        factor_bank.walk_forward_partition.clone(),
        evaluation_policy.clone(),
        policy.clone(),
        model_kind,
        folds,
        evaluation,
    )
    .map_err(|error| format!("baseline artifact validation failed: {error}"))?;
    verify_cex_baseline_artifact(context, factor_bank, &artifact)?;
    Ok(artifact)
}

fn fit_burn_fold(fit: CexBurnFoldFit<'_>) -> Result<CexBurnFoldOutput, String> {
    let CexBurnFoldFit {
        rows,
        features,
        fold,
        fold_index,
        mission_id,
        identity,
        factor_ids,
        horizon,
        evaluation_policy_sha256,
        dataset_sha256,
        profile,
        purpose,
    } = fit;
    if fold.train.end > rows.len()
        || fold.validation.end > rows.len()
        || fold.train.end > features.len()
        || fold.validation.end > features.len()
    {
        return Err("Burn MLP fold range exceeds the research matrix".to_string());
    }
    let horizon_ms = horizon
        .horizon_buckets
        .checked_mul(
            usize::try_from(horizon.observation_frequency_millis)
                .map_err(|_| "Burn MLP label horizon overflowed".to_string())?,
        )
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| "Burn MLP label horizon overflowed".to_string())?;
    if horizon_ms <= 0 {
        return Err("Burn MLP label horizon must be positive".to_string());
    }
    // Burn requires a full horizon after the last training label matures.
    // A protocol may purge only one horizon, so reserve the remaining rows
    // inside its training range instead of inventing a later validation time.
    let required_gap_rows = horizon
        .horizon_buckets
        .checked_mul(2)
        .and_then(|rows| rows.checked_sub(1))
        .ok_or("Burn MLP training gap overflowed")?;
    let extra_purge_rows =
        required_gap_rows.saturating_sub(fold.validation.start.saturating_sub(fold.train.end));
    let training_end = fold.train.end.saturating_sub(extra_purge_rows);
    let mut training_rows = Vec::with_capacity(training_end.saturating_sub(fold.train.start));
    for index in fold.train.start..training_end {
        let observed_at_ms = timestamp_ms(rows[index].available_time)?;
        let label_available_at_ms = timestamp_ms(rows[index].label_available_time)?;
        let feature_row = features[index]
            .iter()
            .map(|value| {
                if value.is_finite() {
                    Ok(*value as f32)
                } else {
                    Err(format!("Burn MLP row {index} has a non-finite feature"))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !rows[index].label.is_finite() {
            return Err(format!("Burn MLP row {index} has a non-finite label"));
        }
        training_rows.push(ContractTrainingRow {
            observed_at_ms: TimestampMs::new(observed_at_ms),
            feature_max_available_at_ms: TimestampMs::new(observed_at_ms),
            label_available_at_ms: TimestampMs::new(label_available_at_ms),
            features: feature_row,
            forward_return: rows[index].label as f32,
        });
    }
    if training_rows.len() < CEX_BURN_MIN_ROWS {
        return Err(format!(
            "Burn MLP fold {fold_index} has {} training rows; {CEX_BURN_MIN_ROWS} are required",
            training_rows.len()
        ));
    }
    let first_observed = training_rows[0].observed_at_ms.get();
    let cutoff_ms = training_rows
        .iter()
        .map(|row| row.label_available_at_ms.get())
        .max()
        .ok_or_else(|| format!("Burn MLP fold {fold_index} lost its training rows"))?;
    let next_split_start_ms = timestamp_ms(rows[fold.validation.start].available_time)?;
    // The generic trainer's embargo is the actual gap to the next split,
    // not the campaign's post-validation embargo row range.
    let embargo_ms = next_split_start_ms
        .checked_sub(cutoff_ms)
        .and_then(|gap| u64::try_from(gap).ok())
        .filter(|gap| *gap > 0)
        .ok_or("Burn MLP training labels are not available before validation")?;
    let ordered_features = factor_ids
        .iter()
        .map(|factor_id| {
            FeatureName::new(factor_id.clone())
                .map_err(|error| format!("Burn MLP factor id is invalid: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let dataset = ContractDatasetBinding::new(
        parse_sha256(dataset_sha256)?,
        sha256_json(&factor_ids)?,
        parse_sha256(evaluation_policy_sha256)?,
        ordered_features,
        Symbol::new(identity.symbol)
            .map_err(|error| format!("Burn MLP symbol is invalid: {error}"))?,
        Venue::new(identity.venue)
            .map_err(|error| format!("Burn MLP venue is invalid: {error}"))?,
        PositiveDurationMs::new(
            u64::try_from(horizon_ms)
                .map_err(|_| "Burn MLP label horizon overflowed".to_string())?,
        )
        .map_err(|error| format!("Burn MLP horizon is invalid: {error}"))?,
    )
    .map_err(|error| format!("Burn MLP dataset binding failed: {error}"))?;
    if let Some(profile) = profile {
        profile.validate()?;
        if profile.initialization.expected_factor_ids != factor_ids {
            return Err("Burn MLP factor order differs from the paired training profile".into());
        }
    }
    let seed = match profile {
        Some(profile) => profile.seed_for_fold(fold_index)?,
        None => burn_fold_seed(mission_id, fold_index, factor_ids),
    };
    let epochs = profile.map_or(CEX_BURN_EPOCHS, |profile| profile.updates);
    let target_scale = profile.map_or(MlpTargetScaleV1::RawReturn, |profile| profile.target_scale);
    let config = ContractTrainingConfig {
        input_dim: factor_ids.len(),
        hidden_dim: CEX_BURN_HIDDEN_DIM,
        epochs,
        learning_rate: CEX_BURN_LEARNING_RATE,
        min_rows: CEX_BURN_MIN_ROWS,
        seed,
        target_scale,
    };
    let rows_artifact = serde_json::to_vec(&training_rows)
        .map_err(|error| format!("Burn MLP training rows failed to serialize: {error}"))?;
    let split = PurgedWalkForwardSplit::new(
        SplitId::new(format!("cex-burn-fold-{fold_index}-train"))
            .map_err(|error| format!("Burn MLP split id is invalid: {error}"))?,
        SplitRole::Train,
        TimestampMs::new(first_observed),
        TimestampMs::new(cutoff_ms),
        TimestampMs::new(next_split_start_ms),
        PositiveDurationMs::new(
            u64::try_from(horizon_ms)
                .map_err(|_| "Burn MLP label horizon overflowed".to_string())?,
        )
        .map_err(|error| format!("Burn MLP purge is invalid: {error}"))?,
        PositiveDurationMs::new(embargo_ms)
            .map_err(|error| format!("Burn MLP embargo is invalid: {error}"))?,
    )
    .map_err(|error| format!("Burn MLP split failed: {error}"))?;
    let request = TrainingRequest::new(
        Sha256Digest::of_bytes(&rows_artifact),
        dataset,
        split,
        config,
    )
    .map_err(|error| format!("Burn MLP training request failed: {error}"))?;
    let request_bytes = serde_json::to_vec(&request)
        .map_err(|error| format!("Burn MLP training request failed to serialize: {error}"))?;
    let request_digest = Sha256Digest::of_bytes(&request_bytes);
    let sealed = SealedTrainingRequest::from_bytes(&request_bytes, &request_digest)
        .map_err(|error| format!("Burn MLP training request failed to seal: {error}"))?;
    let trained = train_contract_model(&rows_artifact, &sealed)
        .map_err(|error| format!("Burn MLP training failed: {error}"))?;
    crate::research_event(
        "alpha-engine-mlp",
        "mlp_fold_fit_completed",
        json!({
            "mission_id": mission_id, "fold_index": fold_index, "purpose": purpose,
            "symbol": identity.symbol, "venue": identity.venue, "seed": seed,
            "updates_requested": epochs, "updates_completed": trained.diagnostics().learning.updates_completed,
            "target_scale": target_scale, "row_count": trained.diagnostics().row_count,
            "training_elapsed_millis": trained.training_elapsed_millis(),
            "initial_parameters_sha256": trained.diagnostics().learning.initial_parameters_sha256,
            "semantic_model_sha256": trained.diagnostics().semantic_model_sha256,
            "request_semantic_sha256": trained.diagnostics().request_semantic_sha256,
            "training_mse": trained.diagnostics().mse,
            "mse_over_zero_prediction": trained.diagnostics().learning.training_prediction.mse_over_zero_prediction,
        }),
    );
    let parameters = trained
        .export_parameters()
        .map_err(|error| format!("Burn MLP parameters failed to export: {error}"))?;
    let mut predictions =
        Vec::with_capacity(fold.validation.end.saturating_sub(fold.validation.start));
    for index in fold.validation.clone() {
        let feature_row = features[index]
            .iter()
            .map(|value| {
                if value.is_finite() {
                    Ok(*value as f32)
                } else {
                    Err(format!(
                        "Burn MLP validation row {index} has a non-finite feature"
                    ))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let backend_prediction = trained
            .predict(&feature_row)
            .map_err(|error| format!("Burn MLP validation row {index} failed: {error}"))?;
        let prediction = parameters.predict(&feature_row)?;
        let output_scale = trained.diagnostics().learning.target_transform.scale as f32;
        let tolerance = (1e-6_f32 * output_scale + 1e-6 * backend_prediction.abs()).max(1e-10);
        if (prediction - backend_prediction).abs() > tolerance {
            return Err(format!(
                "Burn MLP portable inference differs at validation row {index}"
            ));
        }
        if !prediction.is_finite() {
            return Err(format!(
                "Burn MLP validation row {index} produced a non-finite prediction"
            ));
        }
        predictions.push(normalize_zero(f64::from(prediction)));
    }
    let diagnostics = trained.diagnostics();
    let targets: Vec<_> = fold
        .validation
        .clone()
        .map(|index| rows[index].label)
        .collect();
    let observation = CexMlpFoldObservationV1 {
        validation_prediction: MlpPredictionDiagnosticsV1::new(
            &predictions,
            &targets,
            diagnostics
                .learning
                .training_prediction
                .training_target_mean,
        )?,
    };
    Ok(CexBurnFoldOutput {
        model: CexBaselineModelV1::BurnMlpPortable {
            parameters,
            request_semantic_sha256: diagnostics.request_semantic_sha256.as_str().to_string(),
            semantic_model_sha256: diagnostics.semantic_model_sha256.as_str().to_string(),
            config_sha256: diagnostics.config_sha256.as_str().to_string(),
            trainer_version: diagnostics.trainer_version.clone(),
            symbol: identity.symbol.to_string(),
            venue: identity.venue.to_string(),
            row_count: diagnostics.row_count,
            seed,
            hidden_dim: CEX_BURN_HIDDEN_DIM,
            epochs,
            learning_rate: CEX_BURN_LEARNING_RATE,
            min_rows: CEX_BURN_MIN_ROWS,
            learning: Box::new(diagnostics.learning.clone()),
        },
        predictions,
        observation,
    })
}

fn timestamp_ms(time: chrono::DateTime<chrono::Utc>) -> Result<i64, String> {
    Ok(time.timestamp_millis())
}

fn parse_sha256(value: &str) -> Result<Sha256Digest, String> {
    Sha256Digest::try_from(value.to_string()).map_err(|error| error.to_string())
}

fn sha256_json(value: &impl serde::Serialize) -> Result<Sha256Digest, String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("Burn MLP identity hash failed: {error}"))?;
    Ok(Sha256Digest::of_bytes(&bytes))
}

fn burn_fold_seed(mission_id: &str, fold_index: usize, factor_ids: &[String]) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(mission_id.as_bytes());
    hasher.update(fold_index.to_le_bytes());
    for factor_id in factor_ids {
        hasher.update(factor_id.as_bytes());
        hasher.update([0xff]);
    }
    let digest = hasher.finalize();
    let mut seed = [0_u8; 8];
    seed.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(seed)
}

fn labels(rows: &[crate::evaluation::ResearchRow]) -> Vec<f64> {
    rows.iter().map(|row| row.label).collect()
}

fn range(range: &std::ops::Range<usize>) -> CexBaselineRangeV1 {
    CexBaselineRangeV1 {
        start: range.start,
        end: range.end,
    }
}

fn predict_fold_ridge(
    model: &RidgeFit,
    features: &[Vec<f64>],
    range: &std::ops::Range<usize>,
) -> Result<Vec<f64>, String> {
    range
        .clone()
        .map(|index| predict_ridge(model, &features[index]))
        .collect()
}

fn predict_fold_cart(
    model: &CartNode,
    features: &[Vec<f64>],
    range: &std::ops::Range<usize>,
) -> Result<Vec<f64>, String> {
    range
        .clone()
        .map(|index| predict_cart(model, &features[index]))
        .collect()
}

fn predictions_equal(left: &[f64], right: &[f64]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.to_bits() == right.to_bits())
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RidgeFit {
    pub(crate) intercept: f64,
    pub(crate) means: Vec<f64>,
    pub(crate) scales: Vec<f64>,
    pub(crate) coefficients: Vec<f64>,
}

pub(crate) type CartNode = CexBaselineCartNodeV1;

pub(crate) fn fit_ridge(
    features: &[Vec<f64>],
    labels: &[f64],
    rows: std::ops::Range<usize>,
    lambda: f64,
) -> Result<RidgeFit, String> {
    if !lambda.is_finite() || lambda <= 0.0 || labels.len() != features.len() {
        return Err("invalid Ridge inputs".to_string());
    }
    let width = features.first().map_or(0, Vec::len);
    if width == 0
        || features.iter().any(|row| row.len() != width)
        || rows.start >= rows.end
        || rows.end > features.len()
        || rows.end > labels.len()
        || features
            .iter()
            .flat_map(|row| row.iter())
            .chain(labels.iter())
            .any(|value| !value.is_finite())
    {
        return Err("invalid Ridge inputs".to_string());
    }

    let count = rows.end - rows.start;
    let mut means = vec![0.0; width];
    for row in &features[rows.clone()] {
        for (mean, value) in means.iter_mut().zip(row) {
            *mean += *value;
        }
    }
    for mean in &mut means {
        *mean /= count as f64;
        if !mean.is_finite() {
            return Err("Ridge mean is non-finite".to_string());
        }
    }
    let mut scales = vec![0.0; width];
    for row in &features[rows.clone()] {
        for ((scale, mean), value) in scales.iter_mut().zip(&means).zip(row) {
            *scale += (*value - *mean).powi(2);
        }
    }
    for scale in &mut scales {
        *scale = (*scale / count as f64).sqrt();
        if !scale.is_finite() {
            return Err("Ridge scale is non-finite".to_string());
        }
        if *scale <= f64::EPSILON {
            *scale = 1.0;
        }
    }

    let target_mean = labels[rows.clone()].iter().sum::<f64>() / count as f64;
    if !target_mean.is_finite() {
        return Err("Ridge target mean is non-finite".to_string());
    }
    let mut matrix = vec![vec![0.0; width]; width];
    let mut values = vec![0.0; width];
    for index in rows {
        let standardized = features[index]
            .iter()
            .zip(&means)
            .zip(&scales)
            .map(|((value, mean), scale)| (*value - *mean) / *scale)
            .collect::<Vec<_>>();
        let centered_target = labels[index] - target_mean;
        for feature in 0..width {
            values[feature] += standardized[feature] * centered_target;
            for other in 0..width {
                matrix[feature][other] += standardized[feature] * standardized[other];
            }
        }
    }
    for (index, row) in matrix.iter_mut().enumerate() {
        row[index] += lambda;
    }
    let coefficients = solve(matrix, values)?;
    if coefficients.iter().any(|value| !value.is_finite()) {
        return Err("Ridge coefficients are non-finite".to_string());
    }
    let intercept = target_mean;
    if !intercept.is_finite() {
        return Err("Ridge intercept is non-finite".to_string());
    }
    Ok(RidgeFit {
        intercept: normalize_zero(intercept),
        means: means.into_iter().map(normalize_zero).collect(),
        scales: scales.into_iter().map(normalize_zero).collect(),
        coefficients: coefficients.into_iter().map(normalize_zero).collect(),
    })
}

pub(crate) fn predict_ridge(model: &RidgeFit, features: &[f64]) -> Result<f64, String> {
    hft_research_manifest::model::predict_standardized_ridge(
        model.intercept,
        &model.means,
        &model.scales,
        &model.coefficients,
        features,
    )
}

pub(crate) fn fit_cart(
    features: &[Vec<f64>],
    labels: &[f64],
    rows: std::ops::Range<usize>,
    max_depth: usize,
    min_leaf: usize,
    factor_ids: &[String],
) -> Result<CartNode, String> {
    if labels.len() != features.len()
        || rows.start >= rows.end
        || rows.end > features.len()
        || rows.end > labels.len()
        || max_depth == 0
        || min_leaf == 0
    {
        return Err("invalid CART inputs".to_string());
    }
    let width = features[rows.start].len();
    if width == 0
        || factor_ids.len() != width
        || features[rows.clone()]
            .iter()
            .any(|row| row.len() != width || row.iter().any(|value| !value.is_finite()))
        || labels[rows.clone()].iter().any(|value| !value.is_finite())
    {
        return Err("invalid CART inputs".to_string());
    }
    let indices = rows.collect::<Vec<_>>();
    Ok(fit_cart_node(
        features, labels, &indices, 0, max_depth, min_leaf, factor_ids,
    ))
}

fn fit_cart_node(
    features: &[Vec<f64>],
    labels: &[f64],
    indices: &[usize],
    depth: usize,
    max_depth: usize,
    min_leaf: usize,
    factor_ids: &[String],
) -> CartNode {
    let value = indices.iter().map(|index| labels[*index]).sum::<f64>() / indices.len() as f64;
    let parent_loss = sse(labels, indices, value);
    if depth >= max_depth || indices.len() < min_leaf.saturating_mul(2) {
        return CartNode::Leaf {
            value: normalize_zero(value),
            sample_count: indices.len(),
        };
    }

    let mut best: Option<(f64, usize, f64)> = None;
    for feature_index in 0..factor_ids.len() {
        let mut sorted = indices.to_vec();
        sorted.sort_by(|left, right| {
            features[*left][feature_index]
                .total_cmp(&features[*right][feature_index])
                .then_with(|| left.cmp(right))
        });
        let total_sum = indices.iter().map(|index| labels[*index]).sum::<f64>();
        let total_sum_sq = indices
            .iter()
            .map(|index| labels[*index] * labels[*index])
            .sum::<f64>();
        let mut left_sum = 0.0;
        let mut left_sum_sq = 0.0;
        for split in 0..sorted.len() - 1 {
            let index = sorted[split];
            left_sum += labels[index];
            left_sum_sq += labels[index] * labels[index];
            let left_value = features[index][feature_index];
            let right_value = features[sorted[split + 1]][feature_index];
            if left_value == right_value {
                continue;
            }
            let threshold = left_value;
            let left_count = split + 1;
            let right_count = sorted.len() - left_count;
            if left_count < min_leaf || right_count < min_leaf {
                continue;
            }
            let right_sum = total_sum - left_sum;
            let right_sum_sq = total_sum_sq - left_sum_sq;
            let loss = sse_from_sums(left_sum, left_sum_sq, left_count)
                + sse_from_sums(right_sum, right_sum_sq, right_count);
            if !loss.is_finite() || loss >= parent_loss {
                continue;
            }
            let replace = best
                .as_ref()
                .is_none_or(|(best_loss, best_feature, best_threshold)| {
                    loss.total_cmp(best_loss)
                        .then_with(|| factor_ids[feature_index].cmp(&factor_ids[*best_feature]))
                        .then_with(|| threshold.total_cmp(best_threshold))
                        .is_lt()
                });
            if replace {
                best = Some((loss, feature_index, threshold));
            }
        }
    }
    let Some((_, feature_index, threshold)) = best else {
        return CartNode::Leaf {
            value: normalize_zero(value),
            sample_count: indices.len(),
        };
    };
    let (left, right): (Vec<_>, Vec<_>) = indices
        .iter()
        .copied()
        .partition(|index| features[*index][feature_index] <= threshold);
    CartNode::Split {
        feature_index,
        threshold: normalize_zero(threshold),
        left: Box::new(fit_cart_node(
            features,
            labels,
            &left,
            depth + 1,
            max_depth,
            min_leaf,
            factor_ids,
        )),
        right: Box::new(fit_cart_node(
            features,
            labels,
            &right,
            depth + 1,
            max_depth,
            min_leaf,
            factor_ids,
        )),
    }
}

fn sse(labels: &[f64], indices: &[usize], mean: f64) -> f64 {
    indices
        .iter()
        .map(|index| (labels[*index] - mean).powi(2))
        .sum()
}

fn sse_from_sums(sum: f64, sum_sq: f64, count: usize) -> f64 {
    let raw = sum_sq - (sum * sum / count as f64);
    let tolerance = f64::EPSILON * 64.0 * sum_sq.abs().max(1.0);
    if raw >= 0.0 {
        raw
    } else if raw >= -tolerance {
        0.0
    } else {
        f64::NAN
    }
}

pub(crate) fn predict_cart(model: &CartNode, features: &[f64]) -> Result<f64, String> {
    model.predict(features)
}

fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn ridge_is_deterministic_and_constant_columns_are_safe() {
        let features = vec![
            vec![0.0, 2.0],
            vec![1.0, 2.0],
            vec![2.0, 2.0],
            vec![3.0, 2.0],
        ];
        let labels = vec![0.0, 1.0, 2.0, 3.0];
        let left = fit_ridge(&features, &labels, 0..3, 1.0e-6).unwrap();
        let right = fit_ridge(&features, &labels, 0..3, 1.0e-6).unwrap();
        assert_eq!(left, right);
        assert_eq!(left.scales[1], 1.0);
        assert!((predict_ridge(&left, &features[3]).unwrap() - 3.0).abs() < 1.0e-3);
    }

    #[test]
    fn cart_uses_stable_threshold_routing_and_tie_breaking() {
        let features = vec![
            vec![0.0, 0.0],
            vec![1.0, 0.0],
            vec![2.0, 1.0],
            vec![3.0, 1.0],
        ];
        let labels = vec![0.0, 0.0, 1.0, 1.0];
        let model = fit_cart(
            &features,
            &labels,
            0..4,
            1,
            1,
            &["a".to_string(), "b".to_string()],
        )
        .unwrap();
        assert_eq!(
            predict_cart(&model, &[1.0, 0.0]).unwrap(),
            0.0,
            "<= threshold must route left"
        );
        assert_eq!(predict_cart(&model, &[2.0, 1.0]).unwrap(), 1.0);
        assert_eq!(sse_from_sums(1.0, 0.99999999999999, 1), 0.0);
    }

    #[test]
    fn supervised_policy_trades_only_excess_edge_and_keeps_fractional_size() {
        let row = crate::evaluation::ResearchRow {
            series_id: 1,
            available_time: Utc::now(),
            label_available_time: Utc::now() + chrono::Duration::seconds(1),
            signal: 0.0,
            features: std::collections::BTreeMap::from([("spread_bps".to_string(), 2.0)]),
            label: 0.0,
            fee_bps: 2.0,
            funding_bps: 1.0,
            pit_funding: true,
            latency_bps: 1.0,
        };
        let costs = alpha_domain::EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 1.0,
            latency_bps: 1.0,
            slippage_bps: 1.0,
            cross_spread: true,
            position_notional_usd: 0.0,
            capacity_depth_levels: 0,
            max_book_depth_fraction: 0.0,
        };
        let policy = CexSupervisedDecisionPolicyV2::controlled_v2();

        assert_eq!(
            cost_aware_target_position(0.0009, &row, &costs, &policy).unwrap(),
            0.0
        );
        let long = cost_aware_target_position(0.002, &row, &costs, &policy).unwrap();
        let short = cost_aware_target_position(-0.002, &row, &costs, &policy).unwrap();
        assert!(long > 0.0 && long < 1.0);
        assert_eq!(short, -long);
    }

    #[test]
    fn registered_supervised_position_policies_have_distinct_bounded_behavior() {
        let row = crate::evaluation::ResearchRow {
            series_id: 1,
            available_time: Utc::now(),
            label_available_time: Utc::now() + chrono::Duration::seconds(1),
            signal: 0.0,
            features: std::collections::BTreeMap::from([("spread_bps".to_string(), 2.0)]),
            label: 0.0,
            fee_bps: 2.0,
            funding_bps: 1.0,
            pit_funding: true,
            latency_bps: 1.0,
        };
        let costs = alpha_domain::EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 1.0,
            latency_bps: 1.0,
            slippage_bps: 1.0,
            cross_spread: true,
            position_notional_usd: 0.0,
            capacity_depth_levels: 0,
            max_book_depth_fraction: 0.0,
        };
        let controlled = CexSupervisedDecisionPolicyV2::controlled_v2();
        let identity = CexSupervisedDecisionPolicyV2::prediction_identity_v2();
        let hysteretic = CexSupervisedDecisionPolicyV2::hysteretic_cost_aware_v2();
        let spot_identity = identity.clone().with_long_only(true);

        assert_eq!(
            cost_aware_target_position(0.0009, &row, &costs, &controlled).unwrap(),
            0.0
        );
        assert_eq!(
            0.0009_f64.clamp(-identity.max_abs_position, identity.max_abs_position),
            0.0009
        );
        assert_eq!(
            cost_aware_target_position(-0.002, &row, &costs, &spot_identity).unwrap(),
            0.0
        );
        assert_eq!(
            spot_identity
                .target_position(
                    0.0009,
                    0.0,
                    CexDecisionCostsV1 {
                        one_way_cost_bps: 0.0,
                        funding_bps: 0.0,
                    }
                )
                .unwrap(),
            0.0009
        );
        let entered = cost_aware_target_position(0.002, &row, &costs, &hysteretic).unwrap();
        assert!(entered > 0.0);
        assert_eq!(
            hysteretic_cost_aware_target_position(0.0007, entered, &row, &costs, &hysteretic,)
                .unwrap(),
            entered
        );
        assert_eq!(
            hysteretic_cost_aware_target_position(0.0004, entered, &row, &costs, &hysteretic,)
                .unwrap(),
            0.0
        );
        assert_eq!(
            hysteretic_cost_aware_target_position(-0.0007, -entered, &row, &costs, &hysteretic,)
                .unwrap(),
            -entered
        );
    }

    #[test]
    fn burn_mlp_is_deterministic_and_ignores_validation_labels() {
        let rows = (0..20)
            .map(|index| ResearchRow {
                series_id: 1,
                available_time: chrono::DateTime::<Utc>::from_timestamp(index as i64, 0).unwrap(),
                label_available_time: chrono::DateTime::<Utc>::from_timestamp(index as i64, 0)
                    .unwrap()
                    + chrono::Duration::seconds(1),
                signal: 0.0,
                features: std::collections::BTreeMap::new(),
                label: (index as f64 - 10.0) / 1_000.0,
                fee_bps: 2.0,
                funding_bps: 0.0,
                pit_funding: true,
                latency_bps: 0.0,
            })
            .collect::<Vec<_>>();
        let features = rows
            .iter()
            .enumerate()
            .map(|(index, _)| vec![index as f64 / 20.0])
            .collect::<Vec<_>>();
        let fold = WalkForwardFold {
            train: 0..12,
            purge: 12..13,
            validation: 13..16,
            embargo: 16..20,
        };
        let horizon = EvaluationLabelSpecV1 {
            horizon_buckets: 1,
            observation_frequency_millis: 1_000,
        };
        let dataset_sha = "a".repeat(64);
        let evaluation_sha = "b".repeat(64);
        let identity = CexBurnFitIdentity {
            symbol: "BTCUSDT",
            venue: "binance-usdm",
        };
        let factor_ids = ["cex-factor-1".to_string()];
        let fit = |rows: &[ResearchRow]| {
            fit_burn_fold(CexBurnFoldFit {
                rows,
                features: &features,
                fold: &fold,
                fold_index: 1,
                mission_id: "cex-mission-burn",
                identity,
                factor_ids: &factor_ids,
                horizon: &horizon,
                evaluation_policy_sha256: &evaluation_sha,
                dataset_sha256: &dataset_sha,
                profile: None,
                purpose: "test",
            })
        };
        let CexBurnFoldOutput {
            model: left_model,
            predictions: left_predictions,
            ..
        } = fit(&rows).unwrap();
        let CexBurnFoldOutput {
            model: right_model,
            predictions: right_predictions,
            ..
        } = fit(&rows).unwrap();
        assert_eq!(left_model, right_model);
        assert_eq!(left_predictions, right_predictions);
        assert_eq!(left_predictions.len(), 3);
        let reloaded: CexBaselineModelV1 =
            serde_json::from_slice(&serde_json::to_vec(&left_model).unwrap()).unwrap();
        let CexBaselineModelV1::BurnMlpPortable {
            parameters,
            semantic_model_sha256,
            ..
        } = &reloaded
        else {
            panic!("training must retain executable parameters");
        };
        assert_eq!(
            parameters.semantic_sha256().unwrap(),
            *semantic_model_sha256
        );
        for (row, expected) in (13..16).zip(&left_predictions) {
            assert_eq!(
                reloaded.predict(&features[row]).unwrap().to_bits(),
                expected.to_bits()
            );
            let features = features[row]
                .iter()
                .map(|value| *value as f32)
                .collect::<Vec<_>>();
            assert_eq!(
                normalize_zero(f64::from(parameters.predict(&features).unwrap())).to_bits(),
                expected.to_bits()
            );
        }

        let mut mutated = rows.clone();
        for row in &mut mutated[13..16] {
            row.label = 9.9;
        }
        let mutated_model = fit(&mutated).unwrap().model;
        assert_eq!(left_model, mutated_model);

        let CexBaselineModelV1::BurnMlpPortable { seed, learning, .. } = &left_model else {
            unreachable!()
        };
        for mode in [
            MlpTargetScaleV1::RawReturn,
            MlpTargetScaleV1::TrainStandardized,
        ] {
            for updates in [8, 64] {
                let profile = CexMlpTrainingProfileV1 {
                    schema_version: "cex-mlp-training-profile-v1".into(),
                    updates,
                    target_scale: mode,
                    initialization: alpha_domain::mlp_training::CexMlpInitializationV1 {
                        fold_seeds: vec![*seed],
                        expected_factor_ids: factor_ids.to_vec(),
                        expected_factor_columns_sha256: "a".repeat(64),
                    },
                };
                let paired_fit = |input: &[ResearchRow], profile: &CexMlpTrainingProfileV1| {
                    fit_burn_fold(CexBurnFoldFit {
                        rows: input,
                        features: &features,
                        fold: &fold,
                        fold_index: 1,
                        mission_id: "different-mission-for-paired-treatment",
                        identity,
                        factor_ids: &factor_ids,
                        horizon: &horizon,
                        evaluation_policy_sha256: &evaluation_sha,
                        dataset_sha256: &dataset_sha,
                        profile: Some(profile),
                        purpose: "test",
                    })
                };
                let treatment = paired_fit(&rows, &profile).unwrap();
                let CexBaselineModelV1::BurnMlpPortable {
                    learning: observed,
                    epochs,
                    ..
                } = &treatment.model
                else {
                    unreachable!()
                };
                assert_eq!(
                    observed.initial_parameters_sha256,
                    learning.initial_parameters_sha256
                );
                assert_eq!(*epochs, updates);
                assert_eq!(observed.updates_completed, updates);
                assert_eq!(observed.target_transform.mode, mode);
                let changed_validation = paired_fit(&mutated, &profile).unwrap();
                assert_eq!(treatment.model, changed_validation.model);
                assert_eq!(treatment.predictions, changed_validation.predictions);
                assert_ne!(
                    treatment.observation.validation_prediction.mse,
                    changed_validation.observation.validation_prediction.mse
                );
                let mut bad = profile.clone();
                bad.initialization.expected_factor_ids[0] = "different-factor".into();
                assert!(paired_fit(&rows, &bad)
                    .unwrap_err()
                    .contains("factor order"));
            }
        }

        // Actual validation time and actual label maturity must drive the split.
        // Neither may be replaced by a convenient synthetic horizon offset.
        let mut delayed = rows.clone();
        delayed[11].label_available_time = delayed[13].available_time;
        assert!(fit(&delayed)
            .unwrap_err()
            .contains("not available before validation"));
        let mut early_validation = rows.clone();
        early_validation[13].available_time = rows[12].available_time;
        assert!(fit(&early_validation)
            .unwrap_err()
            .contains("not available before validation"));
        assert!(matches!(
            left_model,
            CexBaselineModelV1::BurnMlpPortable { row_count: 12, .. }
        ));

        let mut multi_step_rows = rows.clone();
        for row in &mut multi_step_rows {
            row.label_available_time = row.available_time + chrono::Duration::seconds(5);
        }
        let multi_step_fold = WalkForwardFold {
            train: 0..12,
            purge: 12..17,
            validation: 17..20,
            embargo: 20..20,
        };
        let multi_step_horizon = EvaluationLabelSpecV1 {
            horizon_buckets: 5,
            observation_frequency_millis: 1_000,
        };
        let fit_multi_step = |rows: &[ResearchRow]| {
            fit_burn_fold(CexBurnFoldFit {
                rows,
                features: &features,
                fold: &multi_step_fold,
                fold_index: 1,
                mission_id: "cex-mission-burn",
                identity,
                factor_ids: &factor_ids,
                horizon: &multi_step_horizon,
                evaluation_policy_sha256: &evaluation_sha,
                dataset_sha256: &dataset_sha,
                profile: None,
                purpose: "test",
            })
        };
        let CexBurnFoldOutput {
            model: multi_model,
            predictions,
            ..
        } = fit_multi_step(&multi_step_rows).unwrap();
        assert_eq!(predictions.len(), 3);
        assert!(matches!(
            multi_model,
            CexBaselineModelV1::BurnMlpPortable { row_count: 8, .. }
        ));
        // These labels fall inside Burn's extra embargo and must not be fitted.
        for row in &mut multi_step_rows[8..12] {
            row.label = 9.9;
        }
        assert_eq!(fit_multi_step(&multi_step_rows).unwrap().model, multi_model);
    }
}
