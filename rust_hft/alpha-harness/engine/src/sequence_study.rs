//! Fitted sequence comparisons. The caller must reserve Campaign authority;
//! this library neither dispatches, publishes, opens holdouts nor executes orders.
use crate::baselines::fit_ridge;
use alpha_domain::sequence_study::{SequenceStudyModelV1, SolSequenceStudyV1};
use hft_research_manifest::{model::CexBaselineModelV1, sequence::SequenceInputSpecV1};
use hft_research_ml::sequence::{
    training::{
        train_sequence_model, SequenceNeuralKindV1, SequenceTrainingRequestV1, TrainedSequenceModel,
    },
    SequenceReader,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceFitIdentityV1 {
    pub study_sha256: String,
    pub fold_id: u8,
    pub training_window_days: u8,
    pub model_kind: SequenceStudyModelV1,
    pub seed: u64,
    pub training_dataset_sha256: String,
    pub validation_dataset_sha256: String,
    pub input: SequenceInputSpecV1,
}

enum FittedModel {
    Ridge(Box<[CexBaselineModelV1; 3]>),
    Neural(Box<TrainedSequenceModel>),
}

pub struct SequenceFit {
    identity: SequenceFitIdentityV1,
    model: FittedModel,
    training_examples: u64,
}

/// A neural candidate is the predeclared two-seed mean, never the best seed.
pub struct SequenceEnsemble {
    members: Vec<SequenceFit>,
}

impl SequenceEnsemble {
    pub fn new(plan: &SolSequenceStudyV1, mut members: Vec<SequenceFit>) -> Result<Self, String> {
        plan.validate()?;
        members.sort_by_key(|member| member.identity.seed);
        let first = members.first().ok_or("empty SOL ensemble")?;
        let required_seeds = if first.identity.model_kind == SequenceStudyModelV1::Ridge {
            vec![0]
        } else {
            plan.neural_seeds.clone()
        };
        if members
            .iter()
            .map(|member| member.identity.seed)
            .collect::<Vec<_>>()
            != required_seeds
        {
            return Err("SOL ensemble has missing, duplicate or unregistered seeds".into());
        }
        for member in &members {
            let expected = identity(
                plan,
                first.identity.fold_id,
                first.identity.training_window_days,
                first.identity.model_kind,
                member.identity.seed,
            )?;
            if member.identity != expected || member.training_examples != first.training_examples {
                return Err(
                    "SOL ensemble members have different inputs or training coverage".into(),
                );
            }
        }
        Ok(Self { members })
    }

    pub fn members(&self) -> &[SequenceFit] {
        &self.members
    }

    pub fn predict(&self, inputs: &[f32]) -> Result<[f64; 3], String> {
        let mut result = [0.0; 3];
        for member in &self.members {
            for (mean, prediction) in result.iter_mut().zip(member.predict(inputs)?) {
                *mean += prediction / self.members.len() as f64;
            }
        }
        if result.iter().any(|v| !v.is_finite()) {
            return Err("non-finite SOL ensemble mean".into());
        }
        Ok(result)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SequenceFitManifestV1 {
    schema_version: String,
    identity: SequenceFitIdentityV1,
    model_sha256: String,
    neural_metadata: Option<String>,
    training_examples: u64,
    fitted_content_sha256: String,
}

fn identity(
    plan: &SolSequenceStudyV1,
    fold_id: u8,
    days: u8,
    model: SequenceStudyModelV1,
    seed: u64,
) -> Result<SequenceFitIdentityV1, String> {
    plan.validate()?;
    if !plan.models.contains(&model)
        || (model == SequenceStudyModelV1::Ridge && seed != 0)
        || (model != SequenceStudyModelV1::Ridge && !plan.neural_seeds.contains(&seed))
    {
        return Err("unregistered SOL model or seed".into());
    }
    let fold = plan
        .folds
        .iter()
        .find(|fold| fold.fold_id == fold_id && fold.training_window_days == days)
        .ok_or("unregistered SOL fold")?;
    Ok(SequenceFitIdentityV1 {
        study_sha256: plan.content_hash()?,
        fold_id,
        training_window_days: days,
        model_kind: model,
        seed,
        training_dataset_sha256: fold.train_dataset_sha256.clone(),
        validation_dataset_sha256: fold.validation_dataset_sha256.clone(),
        input: plan.input.clone(),
    })
}

pub fn fit_sequence_comparison(
    plan: &SolSequenceStudyV1,
    fold_id: u8,
    days: u8,
    model: SequenceStudyModelV1,
    seed: u64,
    reader: &mut SequenceReader,
) -> Result<SequenceFit, String> {
    let identity = identity(plan, fold_id, days, model, seed)?;
    let fold = plan
        .folds
        .iter()
        .find(|f| f.fold_id == fold_id && f.training_window_days == days)
        .expect("admitted fold");
    if reader.dataset_digest()? != identity.training_dataset_sha256
        || reader.view() != fold.train
        || reader.input_spec() != &plan.input
        || !reader.is_at_start()
    {
        return Err("SOL fitter input or time view differs from its declared fold".into());
    }
    if model == SequenceStudyModelV1::Ridge {
        let mut inputs = Vec::new();
        let mut targets: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
        loop {
            let batch = reader.next_batch(128)?;
            if batch.is_empty() {
                break;
            }
            for example in batch {
                if inputs.len() >= plan.max_training_examples {
                    return Err("SOL Ridge exceeds the frozen training-anchor cap".into());
                }
                inputs.push(
                    example
                        .inputs
                        .into_iter()
                        .map(f64::from)
                        .collect::<Vec<_>>(),
                );
                for (column, target) in targets.iter_mut().zip(example.targets) {
                    column.push(f64::from(target));
                }
            }
        }
        if inputs.len() < 64 {
            return Err("insufficient SOL training anchors".into());
        }
        let mut models = Vec::new();
        for labels in &targets {
            models.push(fit_sequence_ridge(&inputs, labels)?);
        }
        let models: [CexBaselineModelV1; 3] =
            models.try_into().map_err(|_| "missing SOL Ridge target")?;
        Ok(SequenceFit {
            identity,
            model: FittedModel::Ridge(Box::new(models)),
            training_examples: inputs.len() as u64,
        })
    } else {
        let request = neural_request(plan, &identity)?;
        let fitted = train_sequence_model(reader, request)?;
        let training_examples = fitted.scaling().examples;
        if training_examples > plan.max_training_examples as u64 {
            return Err("SOL neural fit exceeds the frozen training-anchor cap".into());
        }
        Ok(SequenceFit {
            identity,
            model: FittedModel::Neural(Box::new(fitted)),
            training_examples,
        })
    }
}

fn fit_sequence_ridge(inputs: &[Vec<f64>], labels: &[f64]) -> Result<CexBaselineModelV1, String> {
    let first = inputs.first().ok_or("empty SOL Ridge inputs")?;
    let varying = (0..first.len())
        .filter(|column| inputs.iter().any(|row| row[*column] != first[*column]))
        .collect::<Vec<_>>();
    let mut means = first.clone();
    let mut scales = vec![1.0; first.len()];
    let mut coefficients = vec![0.0; first.len()];
    let intercept = if varying.is_empty() {
        labels.iter().sum::<f64>() / labels.len() as f64
    } else {
        // Exact-zero standardized columns have zero RHS and a diagonal lambda;
        // dropping them leaves the same Ridge problem without a needless solve.
        let compact;
        let features = if varying.len() == first.len() {
            inputs
        } else {
            compact = inputs
                .iter()
                .map(|row| varying.iter().map(|column| row[*column]).collect())
                .collect::<Vec<_>>();
            &compact
        };
        let fit = fit_ridge(features, labels, 0..inputs.len(), 1e-6)?;
        for (i, column) in varying.iter().enumerate() {
            means[*column] = fit.means[i];
            scales[*column] = fit.scales[i];
            coefficients[*column] = fit.coefficients[i];
        }
        fit.intercept
    };
    let model = CexBaselineModelV1::Ridge {
        intercept,
        means,
        scales,
        coefficients,
    };
    model.validate_inference(first.len())?;
    Ok(model)
}

fn neural_request(
    plan: &SolSequenceStudyV1,
    identity: &SequenceFitIdentityV1,
) -> Result<SequenceTrainingRequestV1, String> {
    let fold = plan
        .folds
        .iter()
        .find(|f| {
            f.fold_id == identity.fold_id && f.training_window_days == identity.training_window_days
        })
        .ok_or("missing SOL fold")?;
    Ok(SequenceTrainingRequestV1 {
        model_kind: if identity.model_kind == SequenceStudyModelV1::FlattenedMlp {
            SequenceNeuralKindV1::Mlp
        } else {
            SequenceNeuralKindV1::Tcn
        },
        dataset_sha256: identity.training_dataset_sha256.clone(),
        input: plan.input.clone(),
        view: fold.train,
        channels: if identity.model_kind == SequenceStudyModelV1::PriceTcn {
            vec![0]
        } else {
            (0..plan.input.ordered_channels.len()).collect()
        },
        hidden_channels: plan.hidden_channels,
        batch_size: plan.batch_size,
        updates: plan.neural_updates,
        learning_rate: plan.learning_rate,
        seed: identity.seed,
        min_examples: 64,
    })
}

impl SequenceFit {
    pub fn fitted_content_sha256(&self) -> Result<String, String> {
        let parameters = match &self.model {
            FittedModel::Ridge(models) => {
                serde_json::to_value(models).map_err(|e| e.to_string())?
            }
            FittedModel::Neural(model) => {
                serde_json::json!({"parameters":model.parameter_digest()?, "scaling":model.scaling()})
            }
        };
        alpha_domain::canonical_json_hash(&serde_json::json!({"schema":"monday.sol_sequence_fit_values.v1", "identity":self.identity,
            "training_examples":self.training_examples, "parameters":parameters})).map_err(|e| e.to_string())
    }

    pub fn identity(&self) -> &SequenceFitIdentityV1 {
        &self.identity
    }

    pub fn predict(&self, inputs: &[f32]) -> Result<[f64; 3], String> {
        if inputs.len()
            != self.identity.input.context_rows * self.identity.input.ordered_channels.len()
            || inputs.iter().any(|v| !v.is_finite())
        {
            return Err("SOL prediction input shape or values changed".into());
        }
        match &self.model {
            FittedModel::Ridge(models) => {
                let values = inputs.iter().copied().map(f64::from).collect::<Vec<_>>();
                let mut result = [0.0; 3];
                for (i, model) in models.iter().enumerate() {
                    result[i] = model.predict(&values)?;
                }
                Ok(result)
            }
            FittedModel::Neural(model) => Ok(model.predict(inputs)?.map(f64::from)),
        }
    }

    pub fn bundle(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
        let (model, neural_metadata) = match &self.model {
            FittedModel::Ridge(models) => {
                (serde_json::to_vec(models).map_err(|e| e.to_string())?, None)
            }
            FittedModel::Neural(model) => {
                let (header, weights) = model.bundle()?;
                (
                    weights,
                    Some(String::from_utf8(header).map_err(|e| e.to_string())?),
                )
            }
        };
        let manifest = SequenceFitManifestV1 {
            schema_version: "monday.sol_sequence_fit.v1".into(),
            identity: self.identity.clone(),
            model_sha256: format!("{:x}", Sha256::digest(&model)),
            neural_metadata,
            training_examples: self.training_examples,
            fitted_content_sha256: self.fitted_content_sha256()?,
        };
        Ok((
            serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
            model,
        ))
    }

    pub fn restore(
        plan: &SolSequenceStudyV1,
        manifest: &[u8],
        expected_hash: &str,
        model: Vec<u8>,
    ) -> Result<Self, String> {
        if manifest.len() > 4 * 1024 * 1024
            || model.len() > 16 * 1024 * 1024
            || format!("{:x}", Sha256::digest(manifest)) != expected_hash
        {
            return Err("SOL fit manifest identity changed".into());
        }
        let header: SequenceFitManifestV1 =
            serde_json::from_slice(manifest).map_err(|e| e.to_string())?;
        let expected = identity(
            plan,
            header.identity.fold_id,
            header.identity.training_window_days,
            header.identity.model_kind,
            header.identity.seed,
        )?;
        if header.schema_version != "monday.sol_sequence_fit.v1"
            || header.identity != expected
            || header.training_examples < 64
            || header.training_examples > plan.max_training_examples as u64
            || format!("{:x}", Sha256::digest(&model)) != header.model_sha256
        {
            return Err("SOL fit parameters or study binding changed".into());
        }
        let fitted = match expected.model_kind {
            SequenceStudyModelV1::Ridge => {
                if header.neural_metadata.is_some() {
                    return Err("Ridge fit contains neural metadata".into());
                }
                let models: [CexBaselineModelV1; 3] =
                    serde_json::from_slice(&model).map_err(|e| e.to_string())?;
                for model in &models {
                    if !matches!(model, CexBaselineModelV1::Ridge { .. }) {
                        return Err("SOL Ridge weights contain another model kind".into());
                    }
                    model.validate_inference(
                        plan.input.context_rows * plan.input.ordered_channels.len(),
                    )?;
                }
                FittedModel::Ridge(Box::new(models))
            }
            _ => {
                let metadata = header
                    .neural_metadata
                    .ok_or("missing neural training metadata")?;
                let hash = format!("{:x}", Sha256::digest(metadata.as_bytes()));
                let restored =
                    TrainedSequenceModel::restore_bundle(metadata.as_bytes(), &hash, model)?;
                if restored.request() != &neural_request(plan, &expected)?
                    || restored.scaling().examples != header.training_examples
                {
                    return Err("SOL neural training request differs from the study".into());
                }
                FittedModel::Neural(Box::new(restored))
            }
        };
        let result = Self {
            identity: expected,
            model: fitted,
            training_examples: header.training_examples,
        };
        if result.fitted_content_sha256()? != header.fitted_content_sha256 {
            return Err("SOL fitted-value identity changed".into());
        }
        Ok(result)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequencePredictionV1 {
    pub observed_at_ms: i64,
    pub spread_bps: f64,
    pub predicted_returns: [f64; 3],
    pub observed_returns: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceValidationCoverageV1 {
    pub emitted_predictions: u64,
    pub expected_predictions: u64,
    pub first_decision_ms: Option<i64>,
    pub last_decision_ms: Option<i64>,
    /// False forbids an economic pass: missing target rows cannot erase trades
    /// that might have been entered before an unexpected future data gap.
    pub complete_decision_grid: bool,
}

pub const SOL_SEQUENCE_POSITION_POLICY: &str = "sol-fixed-notional-after-native-cost-gate-v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceEntryDecisionV1 {
    pub timestamp_us: i64,
    /// This is an opening proposal. The existing horizon replay owns actual
    /// inventory, partial fills, entry rejection and the timed close.
    pub entry_target: Option<f64>,
    pub round_trip_gate_bps: f64,
}

pub fn sequence_entry_decision(
    prediction: &SequencePredictionV1,
    costs: &alpha_domain::EvaluationCostsV1,
    declared_end_ms: i64,
) -> Result<SequenceEntryDecisionV1, String> {
    use hft_research_manifest::model::{CexDecisionCostsV1, CexSupervisedDecisionPolicyV2};
    costs.validate().map_err(|e| e.to_string())?;
    if !costs.cross_spread
        || prediction.observed_at_ms < 0
        || !prediction.spread_bps.is_finite()
        || prediction.spread_bps < 0.0
        || prediction.predicted_returns.iter().any(|v| !v.is_finite())
        || prediction.observed_at_ms >= declared_end_ms
    {
        return Err("invalid SOL entry clock, spread or prediction".into());
    }
    let one_way_cost_bps = costs.fee_bps - costs.rebate_bps
        + costs.latency_bps
        + costs.slippage_bps
        + prediction.spread_bps / 2.0;
    let round_trip_gate_bps = 2.0 * one_way_cost_bps + costs.funding_bps;
    if !round_trip_gate_bps.is_finite() || one_way_cost_bps < 0.0 {
        return Err("invalid SOL round-trip cost gate".into());
    }
    let eligible = prediction
        .observed_at_ms
        .checked_add(30000)
        .is_some_and(|end| end < declared_end_ms);
    let proposed = CexSupervisedDecisionPolicyV2::controlled_v2().target_position(
        prediction.predicted_returns[2],
        0.0,
        CexDecisionCostsV1 {
            one_way_cost_bps,
            funding_bps: costs.funding_bps,
        },
    )?;
    Ok(SequenceEntryDecisionV1 {
        timestamp_us: prediction
            .observed_at_ms
            .checked_mul(1000)
            .ok_or("SOL decision clock overflow")?,
        entry_target: (eligible && proposed != 0.0).then(|| proposed.signum()),
        round_trip_gate_bps,
    })
}

fn expected_validation_coverage(
    view: hft_research_manifest::sequence::SequenceViewV1,
) -> Result<(u64, i64), String> {
    view.validate()?;
    if view.decision_stride_ms != 1000 {
        return Err("SOL validation must retain every eligible second".into());
    }
    let duration = view.end_ms - view.decision_start_ms;
    let eligible = duration.saturating_sub(30_000).max(0);
    Ok(((eligible / 1000) as u64, view.decision_start_ms))
}

/// Development validation only. Sealed evaluation needs its own admitted path.
/// The caller stages the ledger and publishes only after this function succeeds,
/// then checks continuous
/// decision coverage before any economic verdict; omitted target rows are not
/// permission to silently discard orders preceding a future data gap.
pub fn predict_sequence_validation(
    plan: &SolSequenceStudyV1,
    fitted: &SequenceEnsemble,
    reader: &mut SequenceReader,
    mut emit: impl FnMut(SequencePredictionV1) -> Result<(), String>,
) -> Result<SequenceValidationCoverageV1, String> {
    plan.validate()?;
    let identity = &fitted.members[0].identity;
    if identity.study_sha256 != plan.content_hash()? {
        return Err("SOL validation study changed".into());
    }
    let fold = plan
        .folds
        .iter()
        .find(|fold| {
            fold.fold_id == identity.fold_id
                && fold.training_window_days == identity.training_window_days
        })
        .ok_or("unknown validation fold")?;
    if reader.view() != fold.validation
        || reader.dataset_digest()? != fold.validation_dataset_sha256
        || reader.input_spec() != &plan.input
        || !reader.is_at_start()
    {
        return Err("SOL validation reader differs from the predeclared view".into());
    }
    let (expected_predictions, mut expected_clock) = expected_validation_coverage(fold.validation)?;
    let mut coverage = SequenceValidationCoverageV1 {
        emitted_predictions: 0,
        expected_predictions,
        first_decision_ms: None,
        last_decision_ms: None,
        complete_decision_grid: expected_predictions > 0,
    };
    loop {
        let batch = reader.next_batch(128)?;
        if batch.is_empty() {
            break;
        }
        for row in batch {
            coverage.complete_decision_grid &= row.observed_at_ms == expected_clock;
            expected_clock = row
                .observed_at_ms
                .checked_add(1000)
                .ok_or("SOL validation clock overflow")?;
            coverage.first_decision_ms.get_or_insert(row.observed_at_ms);
            coverage.last_decision_ms = Some(row.observed_at_ms);
            let predicted_returns = fitted.predict(&row.inputs)?;
            emit(SequencePredictionV1 {
                observed_at_ms: row.observed_at_ms,
                spread_bps: row.spread_bps,
                predicted_returns,
                observed_returns: row.targets,
            })?;
            coverage.emitted_predictions += 1;
        }
    }
    coverage.complete_decision_grid &=
        coverage.emitted_predictions == coverage.expected_predictions;
    Ok(coverage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_research_manifest::sequence::SequenceViewV1;

    #[test]
    fn sequence_ridge_constant_column_reduction_matches_the_existing_solver() {
        let inputs = (0..80)
            .map(|i| vec![i as f64 / 80.0, 7.0, (i % 7) as f64])
            .collect::<Vec<_>>();
        let labels = inputs
            .iter()
            .map(|row| 0.0002 + row[0] * 0.001 - row[2] * 0.0001)
            .collect::<Vec<_>>();
        let existing = fit_ridge(&inputs, &labels, 0..inputs.len(), 1e-6).unwrap();
        let model = fit_sequence_ridge(&inputs, &labels).unwrap();
        for row in [[0.1, 7.0, 3.0], [0.7, 99.0, 1.0]] {
            let expected = crate::baselines::predict_ridge(&existing, &row).unwrap();
            assert!((model.predict(&row).unwrap() - expected).abs() < 1e-12);
        }
        let flat = vec![vec![1.0, 2.0]; 80];
        let model = fit_sequence_ridge(&flat, &vec![0.001; 80]).unwrap();
        assert!((model.predict(&[1.0, 2.0]).unwrap() - 0.001).abs() < 1e-12);
    }

    #[test]
    fn sequence_validation_grid_reserves_the_strict_thirty_second_tail() {
        let view = SequenceViewV1 {
            history_start_ms: 0,
            decision_start_ms: 59000,
            end_ms: 120000,
            decision_stride_ms: 1000,
        };
        assert_eq!(expected_validation_coverage(view).unwrap(), (31, 59000));
        let mut changed = view;
        changed.decision_stride_ms = 2000;
        assert!(expected_validation_coverage(changed).is_err());
        changed = view;
        changed.end_ms = 89000;
        assert_eq!(expected_validation_coverage(changed).unwrap().0, 0);
    }

    #[test]
    fn sequence_entry_uses_only_the_primary_forecast_and_current_costs() {
        let costs = alpha_domain::EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            slippage_bps: 0.0,
            cross_spread: true,
            position_notional_usd: 100.0,
            capacity_depth_levels: 5,
            max_book_depth_fraction: 0.05,
        };
        let mut row = SequencePredictionV1 {
            observed_at_ms: 1000,
            spread_bps: 1.0,
            predicted_returns: [0.1, 0.2, 0.0007],
            observed_returns: [0.0; 3],
        };
        let decision = sequence_entry_decision(&row, &costs, 100000).unwrap();
        assert_eq!(decision.round_trip_gate_bps, 6.0);
        assert_eq!(decision.entry_target, Some(1.0));
        row.observed_returns = [-100.0; 3];
        assert_eq!(
            decision,
            sequence_entry_decision(&row, &costs, 100000).unwrap()
        );
        row.predicted_returns[2] = 0.0005;
        assert_eq!(
            sequence_entry_decision(&row, &costs, 100000)
                .unwrap()
                .entry_target,
            None
        );
        row.predicted_returns[2] = -0.0007;
        assert_eq!(
            sequence_entry_decision(&row, &costs, 100000)
                .unwrap()
                .entry_target,
            Some(-1.0)
        );
        assert_eq!(
            sequence_entry_decision(&row, &costs, 31000)
                .unwrap()
                .entry_target,
            None
        );
    }
}
