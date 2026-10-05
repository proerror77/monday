//! Scientific fitting and independent refit verification.
use super::*;
use crate::{
    evaluation::{EngineContext, ResearchRow, WalkForwardFold},
    formula_evaluator::FormulaEvaluator,
};
use alpha_domain::frozen_model::CEX_SUPERVISED_CANDIDATE_SCHEMA_V2;
use alpha_domain::{
    canonical_json_hash, CexBaselineArtifactV1, CexBaselineFoldV1, CexBaselineGateV1,
    CexBaselineModelKindV1, CexBaselineModelV1, CexBaselinePolicyV1, CexBaselineRangeV1,
    CexFactorBankRevisionV2, CexMlpFoldObservationV1, CexMlpTrainingProfileV1,
    CexResearchContentRefV1, CexResearchHypothesisTargetV1, EvaluationLabelSpecV1,
    CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
};
use hft_research_ml::{
    train_contract_model, ContractDatasetBinding, ContractTrainingConfig, ContractTrainingRow,
    FeatureName, MlpPredictionDiagnosticsV1, MlpTargetScaleV1, PositiveDurationMs,
    PurgedWalkForwardSplit, SealedTrainingRequest, Sha256Digest, SplitId, SplitRole, Symbol,
    TimestampMs, TrainingRequest, Venue,
};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::classic::*;

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

/// Immutable baselines tied to the exact borrowed input context. Construction
/// performs one independent refit; downstream consumption cannot mutate the
/// models, substitute rows or deserialize a fabricated verification token.
pub struct VerifiedCexBaselineRun<'a, 'data> {
    run: CexBaselineRun,
    context: &'a EngineContext<'data>,
}

impl std::ops::Deref for VerifiedCexBaselineRun<'_, '_> {
    type Target = CexBaselineRun;
    fn deref(&self) -> &Self::Target {
        &self.run
    }
}

impl VerifiedCexBaselineRun<'_, '_> {
    pub fn evaluate_supervised_model(
        &self,
        kind: CexBaselineModelKindV1,
        policy: &CexSupervisedDecisionPolicyV2,
    ) -> Result<CexSupervisedModelEvaluationV2, String> {
        let artifact = match kind {
            CexBaselineModelKindV1::Ridge => &self.run.ridge,
            CexBaselineModelKindV1::ShallowCart => &self.run.cart,
            CexBaselineModelKindV1::BurnMlp => &self.run.burn,
        }
        .as_ref()
        .ok_or_else(|| "verified baseline model is missing".to_string())?;
        evaluate_verified_supervised_model(self.context, artifact, policy)
    }
}

pub fn prepare_cex_baselines<'a, 'data>(
    context: &'a EngineContext<'data>,
    factor_bank: &CexFactorBankRevisionV2,
    policy: &CexBaselinePolicyV1,
    mission_id: &str,
    target: CexResearchHypothesisTargetV1,
    evaluation_policy: &CexResearchContentRefV1,
    burn: Option<CexBurnFitIdentity<'_>>,
) -> Result<VerifiedCexBaselineRun<'a, 'data>, String> {
    Ok(VerifiedCexBaselineRun {
        run: evaluate_cex_baselines(
            context,
            factor_bank,
            policy,
            mission_id,
            target,
            evaluation_policy,
            burn,
        )?,
        context,
    })
}

/// Statistical evaluation is retained even for an exhausted training budget,
/// but a model that declared convergence as a stop requirement cannot advance.

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
            CexBaselineModelV1::BurnMlp { .. } | CexBaselineModelV1::BurnMlpPortable { .. } => {
                return Err(
                    "historical Burn MLP diagnostics do not contain executable parameters".into(),
                );
            }
            CexBaselineModelV1::BurnMlpPortableV2 { symbol, venue, .. } => {
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
    if policy.model_scope == alpha_domain::CexSupervisedModelScopeV1::RidgeOnly && burn.is_some() {
        return Err("Ridge-only evaluation received an MLP training identity".into());
    }
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
    if policy.model_scope == alpha_domain::CexSupervisedModelScopeV1::RidgeOnly {
        let gate = CexBaselineGateV1::ridge_only(&ridge).map_err(|e| e.to_string())?;
        return Ok(CexBaselineRun {
            ridge: Some(ridge),
            cart: None,
            burn: None,
            gate,
        });
    }
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
    evaluate_verified_supervised_model(context, artifact, decision_policy)
}

fn evaluate_verified_supervised_model(
    context: &EngineContext<'_>,
    artifact: &CexBaselineArtifactV1,
    decision_policy: &CexSupervisedDecisionPolicyV2,
) -> Result<CexSupervisedModelEvaluationV2, String> {
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
    let evaluator = FormulaEvaluator::new(artifact.baseline_policy.evaluator_config.clone())?
        .with_decision_policy(decision_policy)?;
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

#[derive(Debug, Clone, Copy)]
enum BaselineKind<'a> {
    Ridge,
    ShallowCart,
    BurnMlp { identity: CexBurnFitIdentity<'a> },
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
    let learning_rate = profile.map_or(CEX_BURN_LEARNING_RATE, |profile| profile.learning_rate());
    let optimization = profile
        .and_then(|profile| profile.optimization_controls())
        .cloned();
    let config = ContractTrainingConfig {
        input_dim: factor_ids.len(),
        hidden_dim: CEX_BURN_HIDDEN_DIM,
        epochs,
        learning_rate,
        min_rows: CEX_BURN_MIN_ROWS,
        seed,
        target_scale,
        optimization,
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
    let request_semantic_sha256 = request
        .semantic_sha256()
        .map_err(|error| format!("Burn MLP semantic request failed: {error}"))?;
    crate::research_event(
        "alpha-engine-mlp",
        "mlp_fold_fit_started",
        json!({
            "mission_id":mission_id,"fold_index":fold_index,"purpose":purpose,
            "symbol":identity.symbol,"venue":identity.venue,"seed":seed,
            "updates_requested":epochs,"learning_rate":learning_rate,"target_scale":target_scale,
            "request_semantic_sha256":request_semantic_sha256.as_str(),
        }),
    );
    let trained = train_contract_model(&rows_artifact, &sealed).map_err(|error| {
        crate::research_event(
            "alpha-engine-mlp", "mlp_fold_fit_failed",
            json!({"mission_id":mission_id,"fold_index":fold_index,"purpose":purpose,
                "symbol":identity.symbol,"venue":identity.venue,"seed":seed,
                "updates_requested":epochs,"learning_rate":learning_rate,"target_scale":target_scale,
                "cause":error.to_string()}),
        );
        format!("Burn MLP training failed: fold={fold_index}, seed={seed}, learning_rate={learning_rate}, purpose={purpose}: {error}")
    })?;
    crate::research_event(
        "alpha-engine-mlp",
        "mlp_fold_fit_completed",
        json!({
            "mission_id": mission_id, "fold_index": fold_index, "purpose": purpose,
            "symbol": identity.symbol, "venue": identity.venue, "seed": seed,
            "updates_requested": epochs, "updates_completed": trained.diagnostics().learning.updates_completed,
            "exit_reason": trained.diagnostics().learning.exit_reason,
            "target_scale": target_scale, "learning_rate":learning_rate,
            "row_count": trained.diagnostics().row_count,
            "convergence": trained.diagnostics().learning.stability.as_ref().map(|s| &s.convergence),
            "clipped_updates": trained.diagnostics().learning.stability.as_ref().map(|s| s.clipped_updates),
            "max_raw_gradient_l2": trained.diagnostics().learning.stability.as_ref().map(|s| s.raw_gradient_l2_history.iter().copied().fold(0.0_f64,f64::max)),
            "max_applied_gradient_l2": trained.diagnostics().learning.stability.as_ref().map(|s| s.applied_gradient_l2_history.iter().copied().fold(0.0_f64,f64::max)),
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
        if let Err(error) =
            parameters.verify_prediction_pair(&feature_row, backend_prediction, prediction)
        {
            crate::research_event(
                "alpha-engine-mlp",
                "mlp_portable_parity_failed",
                json!({
                    "mission_id":mission_id,"fold_index":fold_index,"purpose":purpose,
                    "seed":seed,"learning_rate":learning_rate,"validation_row":index,
                    "request_semantic_sha256":trained.diagnostics().request_semantic_sha256,
                    "semantic_model_sha256":trained.diagnostics().semantic_model_sha256,
                    "target_scale":trained.diagnostics().learning.target_transform.scale,
                    "backend_prediction":backend_prediction,"portable_prediction":prediction,
                    "numerical_diagnostics":&error.diagnostics,"cause":error.to_string(),
                }),
            );
            return Err(format!(
                "Burn MLP portable inference rejected at validation row {index}: {error}"
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
        model: CexBaselineModelV1::BurnMlpPortableV2 {
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
            learning_rate,
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

fn range(range: &std::ops::Range<usize>) -> CexBaselineRangeV1 {
    CexBaselineRangeV1 {
        start: range.start,
        end: range.end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn horizon_targets_keep_entry_gate_and_ignore_signals_until_expiry() {
        for seconds in [5, 10, 30] {
            let policy = CexSupervisedDecisionPolicyV2::hold_to_horizon_v3(seconds * 1000).unwrap();
            let rows: Vec<_> = (0..=seconds + 2)
                .map(|i| ResearchRow {
                    series_id: 1,
                    available_time: chrono::DateTime::from_timestamp(i as i64, 0).unwrap(),
                    label_available_time: chrono::DateTime::from_timestamp((i + seconds) as i64, 0)
                        .unwrap(),
                    signal: 0.0,
                    label: 0.0,
                    fee_bps: 2.0,
                    funding_bps: 0.0,
                    pit_funding: true,
                    latency_bps: 0.0,
                    features: std::collections::BTreeMap::from([
                        ("spread_bps".into(), 2.0),
                        ("mid_price".into(), 100.0),
                    ]),
                })
                .collect();
            let protocol = alpha_domain::EvaluationProtocolV1::new(
                alpha_domain::EvaluationWalkForwardV1 {
                    initial_train_rows: 60,
                    validation_rows: 60,
                    fold_count: 2,
                    purge_rows: seconds as usize,
                    embargo_rows: seconds as usize,
                    sealed_holdout_rows: 60,
                },
                alpha_domain::EvaluationCostsV1 {
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
                    horizon_buckets: seconds as usize,
                    observation_frequency_millis: 1000,
                },
            )
            .unwrap();
            let mut predictions = vec![-0.002; rows.len()];
            predictions[0] = 0.002;
            let evaluate = |rows: &[ResearchRow], predictions: &[f64]| {
                horizon_target_positions(
                    rows,
                    predictions,
                    &policy,
                    &protocol,
                    std::iter::once(0..rows.len()),
                    policy.holding.as_ref().unwrap(),
                )
            };
            let positions = evaluate(&rows, &predictions).unwrap();
            assert!(positions[0] > 0.0);
            assert!(positions[..seconds as usize]
                .iter()
                .all(|p| *p == positions[0]));
            assert!(positions[seconds as usize..].iter().all(|p| *p == 0.0));
            assert!(evaluate(&rows, &vec![0.0001; rows.len()])
                .unwrap()
                .iter()
                .all(|p| *p == 0.0));
            let mut gap = rows.clone();
            gap[1].series_id = 2;
            assert!(evaluate(&gap, &predictions).is_err());
        }
    }

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
    fn ridge_preserves_feature_units_and_target_units_with_train_only_statistics() {
        let features: Vec<_> = (0..8)
            .map(|i| vec![i as f64, (i % 2) as f64, 2.0])
            .collect();
        let labels: Vec<_> = features
            .iter()
            .map(|x| 0.0001 + x[0] * 0.0002 - x[1] * 0.00003)
            .collect();
        let transformed: Vec<_> = features
            .iter()
            .map(|x| vec![x[0] * 1e6 + 50.0, x[1] * 0.001 - 10.0, -7.0])
            .collect();
        let fitted = fit_ridge(&features, &labels, 0..6, 1e-6).unwrap();
        let scaled_features = fit_ridge(&transformed, &labels, 0..6, 1e-6).unwrap();
        let bps_labels: Vec<_> = labels.iter().map(|y| y * 10_000.0).collect();
        let scaled_target = fit_ridge(&features, &bps_labels, 0..6, 1e-6).unwrap();
        assert!((fitted.means[0] - 2.5).abs() < 1e-12);
        let mut changed = features.clone();
        changed[7] = vec![1e9, 1e9, 1e9];
        let mut changed_labels = labels.clone();
        changed_labels[7] = 1e9;
        assert_eq!(
            fitted,
            fit_ridge(&changed, &changed_labels, 0..6, 1e-6).unwrap()
        );
        for i in 0..8 {
            let expected = predict_ridge(&fitted, &features[i]).unwrap();
            assert!(
                (expected - predict_ridge(&scaled_features, &transformed[i]).unwrap()).abs()
                    < 1e-12
            );
            assert!(
                (expected - predict_ridge(&scaled_target, &features[i]).unwrap() / 10_000.0).abs()
                    < 1e-12
            );
            assert!((expected - labels[i]).abs() < 1e-9);
        }
        let portable = CexBaselineModelV1::Ridge {
            intercept: fitted.intercept,
            means: fitted.means,
            scales: fitted.scales,
            coefficients: fitted.coefficients,
        };
        let loaded: CexBaselineModelV1 =
            serde_json::from_slice(&serde_json::to_vec(&portable).unwrap()).unwrap();
        assert_eq!(
            portable.predict(&features[7]).unwrap().to_bits(),
            loaded.predict(&features[7]).unwrap().to_bits()
        );
        let policy = CexSupervisedDecisionPolicyV2::hold_to_horizon_v3(5000).unwrap();
        assert!(
            policy
                .target_position(
                    loaded.predict(&features[7]).unwrap(),
                    0.0,
                    CexDecisionCostsV1 {
                        one_way_cost_bps: 2.5,
                        funding_bps: 0.0
                    }
                )
                .unwrap()
                > 0.0
        );
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
        let CexBaselineModelV1::BurnMlpPortableV2 {
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

        let CexBaselineModelV1::BurnMlpPortableV2 { seed, learning, .. } = &left_model else {
            unreachable!()
        };
        let mut shorter_guarded: Option<
            hft_research_manifest::mlp_training::MlpLearningDiagnosticsV1,
        > = None;
        for mode in [
            MlpTargetScaleV1::RawReturn,
            MlpTargetScaleV1::TrainStandardized,
        ] {
            let budgets = if mode == MlpTargetScaleV1::TrainStandardized {
                vec![8, 64, 4096, 8192]
            } else {
                vec![8, 64]
            };
            for updates in budgets {
                let profile = CexMlpTrainingProfileV1 {
                    schema_version: "cex-mlp-training-profile-v1".into(),
                    updates,
                    target_scale: mode,
                    optimization: (updates > 256).then(|| alpha_domain::mlp_training::CexMlpOptimizationV1 {
                        learning_rate:0.0003,
                        controls:hft_research_manifest::mlp_training::MlpOptimizationControlsV1::default(),
                    }),
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
                let CexBaselineModelV1::BurnMlpPortableV2 {
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
                treatment
                    .model
                    .validate_inference(factor_ids.len())
                    .unwrap();
                observed
                    .validate_optimization(profile.optimization_controls())
                    .unwrap();
                if updates == 4096 {
                    shorter_guarded = Some((**observed).clone());
                } else if updates == 8192 {
                    let shorter = shorter_guarded.as_ref().unwrap();
                    assert_eq!(&observed.loss_history[..4097], shorter.loss_history);
                    assert_eq!(
                        &observed.stability.as_ref().unwrap().raw_gradient_l2_history[..4096],
                        shorter.stability.as_ref().unwrap().raw_gradient_l2_history
                    );
                    assert_eq!(
                        &observed
                            .stability
                            .as_ref()
                            .unwrap()
                            .applied_gradient_l2_history[..4096],
                        shorter
                            .stability
                            .as_ref()
                            .unwrap()
                            .applied_gradient_l2_history
                    );
                    // Full histories remain serializable without truncation; native
                    // round readers admit the expanded, bounded MLP evidence.
                    assert!(
                        serde_json::to_vec_pretty(&treatment.model).unwrap().len()
                            < 64 * 1024 * 1024
                    );
                }
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
            CexBaselineModelV1::BurnMlpPortableV2 { row_count: 12, .. }
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
            CexBaselineModelV1::BurnMlpPortableV2 { row_count: 8, .. }
        ));
        // These labels fall inside Burn's extra embargo and must not be fitted.
        for row in &mut multi_step_rows[8..12] {
            row.label = 9.9;
        }
        assert_eq!(fit_multi_step(&multi_step_rows).unwrap().model, multi_model);
    }
}
