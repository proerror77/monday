//! Frozen-weight evaluation only. No training, search, mutation or LLM calls.
//! The canonical dispatcher owns single-use admission and publication; these
//! deterministic routines cannot publish evidence or grant runtime authority.
use crate::{
    baselines::CexSupervisedModelCandidateV2,
    evaluation::{
        evaluate_sealed_holdout, independent_selection_rows, EngineContext, PreparedDataset,
        ResearchRow,
    },
    formula_evaluator::{
        evaluate_ast, FormulaEvaluator, PositionEvaluationReport, ReturnAccountingBasis,
    },
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
    if !candidate.evaluation.passed {
        return Err("frozen model requires passing search evidence".into());
    }
    fitted_candidate(
        context,
        bank,
        baseline,
        candidate,
        venue,
        market,
        symbol,
        mission,
        max_final_candidates,
    )
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarValidationReportV1 {
    pub schema_version: String,
    pub calendar: alpha_domain::EvaluationCalendarBindingV1,
    pub source_model: CexResearchContentRefV1,
    pub source_candidate: CexResearchContentRefV1,
    pub source_fold: CexResearchContentRefV1,
    pub last_training_time: chrono::DateTime<chrono::Utc>,
    pub max_absolute_prediction_bps: f64,
    pub median_spread_bps: Option<f64>,
    pub median_entry_cost_bps: Option<f64>,
    pub report: PositionEvaluationReport,
    pub promotion_authority: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarValidationSummaryV1 {
    pub calendar: alpha_domain::EvaluationCalendarBindingV1,
    pub source_model: CexResearchContentRefV1,
    pub source_candidate: CexResearchContentRefV1,
    pub last_training_time: chrono::DateTime<chrono::Utc>,
    pub max_absolute_prediction_bps: f64,
    pub median_spread_bps: Option<f64>,
    pub median_entry_cost_bps: Option<f64>,
    pub evaluation: alpha_domain::CandidateEvaluation,
    pub report_content_sha256: String,
}

impl CalendarValidationReportV1 {
    pub fn summary(&self) -> Result<CalendarValidationSummaryV1, String> {
        Ok(CalendarValidationSummaryV1 {
            calendar: self.calendar.clone(),
            source_model: self.source_model.clone(),
            source_candidate: self.source_candidate.clone(),
            last_training_time: self.last_training_time,
            max_absolute_prediction_bps: self.max_absolute_prediction_bps,
            median_spread_bps: self.median_spread_bps,
            median_entry_cost_bps: self.median_entry_cost_bps,
            evaluation: self.report.evaluation.clone(),
            report_content_sha256: canonical_json_hash(self).map_err(|e| e.to_string())?,
        })
    }
}

/// Report the held-out validation result even when development economics fail.
/// No refit, final precommit, holdout claim or deployable candidate is returned.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_calendar_validation(
    dataset: &PreparedDataset,
    bank: &CexFactorBankRevisionV2,
    baseline: &CexBaselineArtifactV1,
    candidate: &CexSupervisedModelCandidateV2,
    venue: &str,
    market: &str,
    symbol: &str,
    mission: &alpha_domain::ResearchMission,
) -> Result<CalendarValidationReportV1, String> {
    let calendar = dataset
        .protocol()
        .calendar
        .clone()
        .ok_or("calendar validation requires a bound calendar")?;
    if baseline.model_kind != alpha_domain::CexBaselineModelKindV1::Ridge {
        return Err("calendar H1 evaluates Ridge only".into());
    }
    let context = dataset.engine_context();
    if venue.is_empty()
        || venue.len() > 32
        || !venue
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
        || !matches!(market, "spot" | "usdm")
        || symbol.is_empty()
        || symbol.len() > 64
        || !symbol
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b"-_".contains(&b))
    {
        return Err("invalid calendar research instrument".into());
    }
    let fitted = fitted_inputs(&context, bank, baseline, candidate, mission)?;
    let development_predictions = research_predictions(context.rows(), &fitted, &bank.gp_policy)?;
    let expected = context.folds().last().ok_or("missing fitted fold")?;
    for (index, prediction) in expected.validation.clone().zip(&fitted.fold.predictions) {
        if development_predictions[index].to_bits() != prediction.to_bits() {
            return Err("research model prediction differs from fitted fold evidence".into());
        }
    }
    let last_training_row = context
        .folds()
        .last()
        .ok_or("missing fitted fold")?
        .train
        .end
        .checked_sub(1)
        .ok_or("empty training fold")?;
    let rows = independent_selection_rows(dataset)?;
    let predictions = research_predictions(rows, &fitted, &bank.gp_policy)?;
    let holding = candidate
        .decision_policy
        .holding
        .as_ref()
        .ok_or("calendar H1 requires horizon holding")?;
    let positions = crate::baselines::horizon_target_positions(
        rows,
        &predictions,
        &candidate.decision_policy,
        dataset.protocol(),
        std::iter::once(0..rows.len()),
        holding,
    )?;
    let evaluator_config = alpha_domain::frozen_model::final_evaluator_config(mission, 1)
        .map_err(|error| error.to_string())?;
    let report = FormulaEvaluator::new(evaluator_config)?
        .with_decision_policy(&candidate.decision_policy)?
        .evaluate_predictions_and_positions(
            rows,
            &predictions,
            &positions,
            std::iter::once(0..rows.len()),
            INDEPENDENT_SELECTION_EVALUATOR_VERSION,
            dataset.protocol(),
        )?;
    if report.return_accounting != ReturnAccountingBasis::HeldQuantityWithQuotedEntryExit {
        return Err("calendar H1 requires held-quantity entry/exit accounting".into());
    }
    report.evaluation.validate_reason()?;
    let mut costs = rows
        .iter()
        .map(|row| {
            let cost = crate::baselines::decision_costs(row, &dataset.protocol().costs)?;
            Ok(
                (candidate.decision_policy.round_trip_cost_multiplier * cost.one_way_cost_bps
                    + cost.funding_bps)
                    .max(0.0),
            )
        })
        .collect::<Result<Vec<f64>, String>>()?;
    costs.sort_by(f64::total_cmp);
    let mut spreads = rows
        .iter()
        .filter_map(|row| row.features.get("spread_bps").copied())
        .collect::<Vec<_>>();
    spreads.sort_by(f64::total_cmp);
    Ok(CalendarValidationReportV1 {
        schema_version: "monday.calendar_validation.v1".into(),
        calendar,
        source_model: candidate.model_artifact.clone(),
        source_candidate: reference(&candidate.artifact_id, candidate)?,
        source_fold: fitted.fold.fold_id.clone(),
        last_training_time: context.rows()[last_training_row].available_time,
        max_absolute_prediction_bps: report
            .ledger
            .iter()
            .map(|row| row.prediction.abs() * 10_000.0)
            .fold(0.0, f64::max),
        median_spread_bps: crate::label_precheck::median(&spreads),
        median_entry_cost_bps: crate::label_precheck::median(&costs),
        report,
        promotion_authority: false,
    })
}

/// Source checks shared by research validation and live freezing. This private
/// value is neither a deployable candidate nor an admission token.
struct FittedInputs<'a> {
    fold: &'a alpha_domain::CexBaselineFoldV1,
    factors: Vec<FrozenModelFactorV1>,
    protocol_hash: String,
}

fn fitted_inputs<'a>(
    context: &EngineContext<'_>,
    bank: &CexFactorBankRevisionV2,
    baseline: &'a CexBaselineArtifactV1,
    candidate: &CexSupervisedModelCandidateV2,
    mission: &alpha_domain::ResearchMission,
) -> Result<FittedInputs<'a>, String> {
    bank.validate().map_err(|e| e.to_string())?;
    baseline.validate().map_err(|e| e.to_string())?;
    if !crate::baselines::baseline_training_admitted(baseline) {
        return Err("model did not satisfy its declared training convergence requirement".into());
    }
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
        || bank.walk_forward_partition.content_sha256
            != canonical_json_hash(&serde_json::json!({
                "research_dataset": &bank.research_dataset,
                "folds": context.folds(),
            }))
            .map_err(|e| e.to_string())?
        || candidate
            .evaluation
            .formula_config()
            .map_err(|e| e.to_string())?
            != baseline.baseline_policy.evaluator_config
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
        || fold.purge_range.start != expected.purge.start
        || fold.purge_range.end != expected.purge.end
        || fold.embargo_range.start != expected.embargo.start
        || fold.embargo_range.end != expected.embargo.end
        || fold.predictions.len() != expected.validation.len()
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
    Ok(FittedInputs {
        fold,
        factors,
        protocol_hash,
    })
}

/// Predict with the fitted weights and the same research factor interpreter
/// used by training. The frozen GP policy bounds fields, operators and history.
fn research_predictions(
    rows: &[ResearchRow],
    fitted: &FittedInputs<'_>,
    policy: &alpha_domain::CexGpPolicyV1,
) -> Result<Vec<f64>, String> {
    if fitted.factors.len() > hft_factor_dsl::model_program::MAX_FROZEN_FACTORS {
        return Err("research model exceeds the fitted factor limit".into());
    }
    let model = fitted.fold.model.prepare_inference(fitted.factors.len())?;
    let history_rows = fitted
        .factors
        .iter()
        .map(|factor| {
            policy
                .candidate_history_rows(&factor.ast)
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max()
        .ok_or("research model has no factors")?;
    let columns = fitted
        .factors
        .iter()
        .map(|factor| {
            let mut values = evaluate_ast(&factor.ast, rows)?;
            for value in &mut values {
                if !value.is_finite() {
                    return Err("research model factor is not finite".into());
                }
                if factor.negative {
                    *value = -*value;
                }
                if *value == 0.0 {
                    *value = 0.0;
                }
            }
            Ok(values)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut predictions = Vec::with_capacity(rows.len());
    let mut series_start = 0;
    for (index, row) in rows.iter().enumerate() {
        if index == 0 || rows[index - 1].series_id != row.series_id {
            series_start = index;
        }
        if index + 1 - series_start < history_rows {
            predictions.push(0.0);
            continue;
        }
        let features = columns
            .iter()
            .map(|column| column[index])
            .collect::<Vec<_>>();
        predictions.push(model.predict(&features)?);
    }
    Ok(predictions)
}

#[allow(clippy::too_many_arguments)]
fn fitted_candidate(
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
    let fitted = fitted_inputs(context, bank, baseline, candidate, mission)?;
    let FittedInputs {
        fold,
        factors,
        protocol_hash,
    } = fitted;
    let protocol = context.protocol();
    let expected = context.folds().last().ok_or("search has no fold")?;
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
    let prepared = program.prepare()?;
    let history_rows = prepared.history_rows();
    for (index, expected_prediction) in expected.validation.clone().zip(&fold.predictions) {
        let start = index
            .checked_add(1)
            .and_then(|end| end.checked_sub(history_rows))
            .ok_or("frozen model history is unavailable")?;
        let rows = &context.rows()[start..=index];
        if rows.iter().any(|row| row.series_id != rows[0].series_id) {
            return Err("frozen model validation history crosses a series boundary".into());
        }
        let actual = prepared.predict_from_history(rows.len(), |row, field| {
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
    let prepared = candidate.program.prepare()?;
    let history_rows = prepared.history_rows();
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
        let prediction = prepared.predict_from_history(history.len(), |row, field| {
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
    if let Some(holding) = &candidate.program.decision_policy.holding {
        positions = crate::baselines::horizon_target_positions(
            rows,
            &predictions,
            &candidate.program.decision_policy,
            protocol,
            std::iter::once(0..rows.len()),
            holding,
        )?;
    }
    let report = FormulaEvaluator::new(candidate.evaluator_config.clone())?
        .with_decision_policy(&candidate.program.decision_policy)?
        .evaluate_predictions_and_positions(
            rows,
            &predictions,
            &positions,
            std::iter::once(0..rows.len()),
            version,
            protocol,
        )?;
    let expected_basis = if candidate.program.decision_policy.holding.is_some() {
        ReturnAccountingBasis::HeldQuantityWithQuotedEntryExit
    } else {
        ReturnAccountingBasis::ObservedMidPrice
    };
    if report.return_accounting != expected_basis {
        return Err("frozen CEX model evaluation requires observed mid-price accounting".into());
    }
    report.evaluation.validate_reason()?;
    Ok(report)
}

#[cfg(test)]
#[path = "final_models_tests.rs"]
mod tests;
