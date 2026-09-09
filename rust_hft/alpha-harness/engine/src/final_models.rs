//! Frozen-weight evaluation only. No training, search, mutation or LLM calls.
//! The canonical dispatcher owns single-use admission and publication; these
//! deterministic routines cannot publish evidence or grant runtime authority.
use crate::{
    baselines::CexSupervisedModelCandidateV2,
    evaluation::{
        evaluate_sealed_holdout, independent_selection_rows, EngineContext, PreparedDataset,
        ResearchRow,
    },
    formula_evaluator::{FormulaEvaluator, PositionEvaluationReport, ReturnAccountingBasis},
};
use alpha_domain::{
    canonical_json_hash,
    frozen_model::{
        FrozenSupervisedCandidateV1, FROZEN_SUPERVISED_CANDIDATE_SCHEMA,
        INDEPENDENT_SELECTION_EVALUATOR_VERSION,
    },
    CexBaselineArtifactV1, CexFactorBankRevisionV2, CexFactorOrientationV1,
    CexResearchContentRefV1, EvaluationProtocolV1, SEALED_HOLDOUT_EVALUATOR_VERSION,
};
use hft_factor_dsl::model_program::{
    FrozenFactorModelV1, FrozenModelFactorV1, FROZEN_FACTOR_MODEL_SCHEMA_V1,
};

fn reference<T: serde::Serialize>(id: &str, value: &T) -> Result<CexResearchContentRefV1, String> {
    Ok(CexResearchContentRefV1 {
        id: id.into(),
        content_sha256: canonical_json_hash(value).map_err(|e| e.to_string())?,
    })
}

/// Freeze the last already-fitted search fold, including exact ordered factors.
/// Source bundle authentication and family membership are checked by the caller.
/// Recomputing validation predictions verifies portability without retraining.
#[allow(clippy::too_many_arguments)]
pub fn freeze_supervised_candidate(
    context: &EngineContext<'_>,
    bank: &CexFactorBankRevisionV2,
    baseline: &CexBaselineArtifactV1,
    candidate: &CexSupervisedModelCandidateV2,
    venue: &str,
    market: &str,
    symbol: &str,
    mission: &alpha_domain::ResearchMission,
    max_final_candidates: u32,
) -> Result<FrozenSupervisedCandidateV1, String> {
    bank.validate().map_err(|e| e.to_string())?;
    baseline.validate().map_err(|e| e.to_string())?;
    candidate.validate()?;
    let protocol = context.protocol();
    let protocol_hash = protocol.content_hash().map_err(|e| e.to_string())?;
    if candidate.model_artifact != reference(&baseline.artifact_id, baseline)?
        || candidate.factor_bank_revision_id != bank.revision_id
        || baseline.factor_bank_revision_id != bank.revision_id
        || candidate.model_kind != baseline.model_kind
        || candidate.mission_id != baseline.mission_id
        || mission.mission_id != baseline.mission_id
        || candidate.research_dataset != bank.research_dataset
        || baseline.research_dataset != bank.research_dataset
        || candidate.walk_forward_partition != bank.walk_forward_partition
        || baseline.walk_forward_partition != bank.walk_forward_partition
        || baseline.evaluation_policy != candidate.evaluation_policy
        || baseline.evaluation_policy != bank.evaluation_policy
        || baseline.evaluation_policy.content_sha256 != protocol_hash
        || baseline.target.horizon != protocol.labels
        || canonical_json_hash(&context.rows()).map_err(|e| e.to_string())?
            != bank.research_dataset.content_sha256
        || candidate
            .evaluation
            .formula_config()
            .map_err(|e| e.to_string())?
            != baseline.baseline_policy.evaluator_config
        || !candidate.evaluation.passed
    {
        return Err("frozen model source, policy or search evidence mismatch".into());
    }
    let fold = baseline
        .folds
        .last()
        .ok_or("frozen model has no fitted fold")?;
    let expected = context.folds().last().ok_or("search has no fold")?;
    if fold.train_range.start != expected.train.start
        || fold.train_range.end != expected.train.end
        || fold.validation_range.start != expected.validation.start
        || fold.validation_range.end != expected.validation.end
        || fold.embargo_range.end > context.rows().len()
    {
        return Err("frozen model fold differs from the signed search schedule".into());
    }
    let mut ids = bank
        .entries
        .iter()
        .map(|entry| entry.factor_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    if ids != baseline.factor_ids {
        return Err("frozen model column order differs from training".into());
    }
    let factors = baseline
        .factor_ids
        .iter()
        .map(|id| {
            let entry = bank
                .entries
                .iter()
                .find(|entry| &entry.factor_id == id)
                .ok_or("missing fitted factor")?;
            Ok(FrozenModelFactorV1 {
                ast: entry.canonical_ast.clone(),
                negative: entry.orientation == CexFactorOrientationV1::Negative,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let costs = &protocol.costs;
    let program = FrozenFactorModelV1 {
        schema_version: FROZEN_FACTOR_MODEL_SCHEMA_V1.into(),
        venue: venue.into(),
        market: market.into(),
        symbol: symbol.into(),
        observation_frequency_millis: protocol.labels.observation_frequency_millis,
        label_horizon_buckets: protocol.labels.horizon_buckets,
        factors,
        model: fold.model.clone(),
        decision_policy: candidate.decision_policy.clone(),
        // Decision costs are fixed to the protocol's registered bounds; realized
        // funding in the return ledger remains point-in-time observed data.
        base_costs: alpha_domain::frozen_model::frozen_decision_costs(protocol),
        cross_spread: costs.cross_spread,
    };
    let history_rows = program.validate()?.history_rows;
    for (index, expected_prediction) in expected.validation.clone().zip(&fold.predictions) {
        let start = index
            .checked_add(1)
            .and_then(|end| end.checked_sub(history_rows))
            .ok_or("frozen model history is unavailable")?;
        let rows = &context.rows()[start..=index];
        if rows.iter().any(|row| row.series_id != rows[0].series_id) {
            return Err("frozen model validation history crosses a series boundary".into());
        }
        let actual = program.predict_from_history(rows.len(), |row, field| {
            rows[row].features.get(field).copied()
        })?;
        if actual.to_bits() != expected_prediction.to_bits() {
            return Err("native frozen model prediction differs from fitted fold evidence".into());
        }
    }
    let frozen = FrozenSupervisedCandidateV1 {
        schema_version: FROZEN_SUPERVISED_CANDIDATE_SCHEMA.into(),
        artifact_id: String::new(),
        source_candidate: reference(&candidate.artifact_id, candidate)?,
        source_model: candidate.model_artifact.clone(),
        source_factor_bank: reference(&bank.revision_id, bank)?,
        source_fold: fold.fold_id.clone(),
        research_dataset: bank.research_dataset.clone(),
        evaluation_protocol_sha256: protocol_hash,
        program,
        evaluator_config: alpha_domain::frozen_model::final_evaluator_config(
            mission,
            max_final_candidates,
        )
        .map_err(|e| e.to_string())?,
    }
    .finalize()?;
    frozen.validate_fitted_origin(baseline, bank)?;
    Ok(frozen)
}

/// Evaluates only the reserved selection rows. No search context receives them.
pub fn evaluate_frozen_selection(
    candidate: &FrozenSupervisedCandidateV1,
    dataset: &PreparedDataset,
) -> Result<PositionEvaluationReport, String> {
    evaluate_frozen_rows(
        candidate,
        independent_selection_rows(dataset)?,
        dataset.protocol(),
        INDEPENDENT_SELECTION_EVALUATOR_VERSION,
    )
}

/// Called only after the canonical global holdout claim has been independently
/// read back for the chosen model precommit. The function grants no such claim.
pub fn evaluate_frozen_holdout(
    candidate: &FrozenSupervisedCandidateV1,
    dataset: &PreparedDataset,
) -> Result<PositionEvaluationReport, String> {
    evaluate_sealed_holdout(dataset, |rows| {
        evaluate_frozen_rows(
            candidate,
            rows,
            dataset.protocol(),
            SEALED_HOLDOUT_EVALUATOR_VERSION,
        )
    })
}

fn evaluate_frozen_rows(
    candidate: &FrozenSupervisedCandidateV1,
    rows: &[ResearchRow],
    protocol: &EvaluationProtocolV1,
    version: &str,
) -> Result<PositionEvaluationReport, String> {
    candidate.validate_against_protocol(protocol)?;
    let history_rows = candidate.program.validate()?.history_rows;
    let mut predictions = vec![0.0; rows.len()];
    let mut positions = vec![0.0; rows.len()];
    let mut start = 0;
    let mut previous = 0.0;
    for (index, row) in rows.iter().enumerate() {
        let gap = index > 0
            && row
                .available_time
                .signed_duration_since(rows[index - 1].available_time)
                .num_milliseconds()
                != i64::try_from(candidate.program.observation_frequency_millis)
                    .map_err(|e| e.to_string())?;
        if gap && rows[index - 1].series_id == row.series_id {
            return Err("frozen evaluation gap lacks a series boundary".into());
        }
        if index == 0 || rows[index - 1].series_id != row.series_id {
            start = index;
            previous = 0.0;
        }
        if index + 1 - start < history_rows {
            continue;
        }
        let history = &rows[start..=index];
        let prediction = candidate
            .program
            .predict_from_history(history.len(), |row, field| {
                history[row].features.get(field).copied()
            })?;
        predictions[index] = prediction;
        let spread = if candidate.program.cross_spread {
            *row.features
                .get("spread_bps")
                .ok_or("missing observed spread")?
        } else {
            0.0
        };
        let terminal = index + 1 == rows.len() || rows[index + 1].series_id != row.series_id;
        let target = if terminal {
            0.0
        } else {
            candidate
                .program
                .target_position(prediction, previous, spread)?
        };
        positions[index] = target;
        previous = target;
    }
    let report = FormulaEvaluator::new(candidate.evaluator_config.clone())?
        .evaluate_predictions_and_positions(
            rows,
            &predictions,
            &positions,
            std::iter::once(0..rows.len()),
            version,
            protocol,
        )?;
    if report.return_accounting != ReturnAccountingBasis::ObservedMidPrice {
        return Err("frozen CEX model evaluation requires observed mid-price accounting".into());
    }
    report.evaluation.validate_reason()?;
    Ok(report)
}
