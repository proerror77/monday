//! Stage-bound fitting and immutable artifacts. The caller owns Campaign grants,
//! cumulative reservations and verified source/replay admission; this library
//! exposes no dispatch, holdout, publication or order path.
use alpha_domain::{canonical_json_hash, market_encoder_study::*};
use hft_cex_research_input::market_encoder::{MarketFeatureReader, MarketTaskReader};
use hft_research_manifest::{
    market_encoder::*, model::CexBaselineModelV1, sequence::SequenceInputSpecV1,
};
use hft_research_ml::market_encoder::{
    adapt_market_encoder, pretrain_market_encoder, MarketEncoderCheckpoint, MarketTaskModel,
};
use serde::{Deserialize, Serialize};

pub enum MarketStageInput<'a> {
    Features(&'a mut MarketFeatureReader),
    Targets(&'a mut MarketTaskReader),
}
enum Fitted {
    Encoder(Box<MarketEncoderCheckpoint>),
    Task(Box<MarketTaskModel>),
    Ridge(Box<CexBaselineModelV1>),
}
pub struct FittedMarketStage {
    study_sha256: String,
    key: MarketTrainingStageKeyV1,
    input: SequenceInputSpecV1,
    examples: u64,
    model: Fitted,
}
/// Created only after the primary and independently fitted verification stage
/// agree on learned values, scaling, task and input identities.
pub struct VerifiedMarketStage<'a> {
    primary: &'a FittedMarketStage,
    verification: &'a FittedMarketStage,
}
impl VerifiedMarketStage<'_> {
    pub fn primary(&self) -> &FittedMarketStage {
        self.primary
    }
    pub fn verification(&self) -> &FittedMarketStage {
        self.verification
    }
}
pub fn verify_market_stage_pair<'a>(
    primary: &'a FittedMarketStage,
    verification: &'a FittedMarketStage,
) -> Result<VerifiedMarketStage<'a>, String> {
    let expected = MarketTrainingStageKeyV1 {
        purpose: MarketTrainingStagePurposeV1::Verification,
        ..primary.key
    };
    if primary.key.purpose != MarketTrainingStagePurposeV1::Primary
        || verification.key != expected
        || primary.study_sha256 != verification.study_sha256
        || primary.fitted_values_digest()? != verification.fitted_values_digest()?
    {
        return Err("market independent fit differs from its primary stage".into());
    }
    Ok(VerifiedMarketStage {
        primary,
        verification,
    })
}

fn stage(
    study: &MarketEncoderStudyV1,
    key: MarketTrainingStageKeyV1,
) -> Result<MarketTrainingStageV1, String> {
    study
        .development_stages()?
        .into_iter()
        .find(|s| s.key == key)
        .ok_or("unregistered market stage".into())
}
fn train_data(
    study: &MarketEncoderStudyV1,
    key: MarketTrainingStageKeyV1,
) -> Result<&MarketDataViewV1, String> {
    stage(study, key)?;
    Ok(&study
        .folds
        .iter()
        .find(|f| f.fold_id == key.fold_id)
        .ok_or("unknown market fold")?
        .train)
}
fn read_request(study: &MarketEncoderStudyV1, data: &MarketDataViewV1) -> MarketDataReadRequestV1 {
    MarketDataReadRequestV1 {
        feature_dataset_sha256: data.features_sha256.clone(),
        qualified_anchors_sha256: data.qualified_anchors_sha256.clone(),
        input: study.input.clone(),
        view: data.view,
        anchor_end_ms: data.view.end_ms - 30_000,
    }
}
fn parent_encoder<'a>(
    study: &MarketEncoderStudyV1,
    key: MarketTrainingStageKeyV1,
    parent: Option<&'a VerifiedMarketStage<'a>>,
) -> Result<Option<&'a MarketEncoderCheckpoint>, String> {
    let expected = stage(study, key)?.parent;
    match (expected, parent) {
        (None, None) => Ok(None),
        (Some(expected), Some(parent)) => {
            if parent.primary.study_sha256 != study.content_hash()?
                || parent.primary.key != expected
            {
                return Err("market parent belongs to another study, fold or seed".into());
            }
            match &parent.primary.model {
                Fitted::Encoder(p) => Ok(Some(p)),
                _ => Err("market parent is not an encoder checkpoint".into()),
            }
        }
        _ => Err("market stage has a missing or unexpected verified parent".into()),
    }
}
fn adaptation_request(
    study: &MarketEncoderStudyV1,
    key: MarketTrainingStageKeyV1,
    parent: Option<&MarketEncoderCheckpoint>,
) -> Result<MarketAdaptationRequestV1, String> {
    let fit = study.fit_request(key)?;
    let mode = match key.kind {
        MarketTrainingStageKindV1::Scratch | MarketTrainingStageKindV1::ScratchCompute => {
            AdaptationModeV1::Scratch
        }
        MarketTrainingStageKindV1::LinearProbe => AdaptationModeV1::LinearProbe,
        MarketTrainingStageKindV1::FineTune => AdaptationModeV1::FullFineTune,
        _ => return Err("stage is not a market adaptation".into()),
    };
    let request = MarketAdaptationRequestV1 {
        fit,
        target_dataset_sha256: train_data(study, key)?.targets_sha256.clone(),
        mode,
        parent_checkpoint_sha256: parent.map(MarketEncoderCheckpoint::identity).transpose()?,
        head_seed: key.seed + 1000,
    };
    request.validate()?;
    Ok(request)
}

pub fn fit_market_stage(
    study: &MarketEncoderStudyV1,
    key: MarketTrainingStageKeyV1,
    input: MarketStageInput<'_>,
    parent: Option<&VerifiedMarketStage<'_>>,
) -> Result<FittedMarketStage, String> {
    let study_sha256 = study.content_hash()?;
    let parent = parent_encoder(study, key, parent)?;
    let data = train_data(study, key)?;
    let (model, examples) = match (key.kind, input) {
        (MarketTrainingStageKindV1::Pretrain, MarketStageInput::Features(reader)) => {
            let request = study.fit_request(key)?;
            let fitted = pretrain_market_encoder(reader, request)?;
            let examples = fitted.scaling().examples;
            (Fitted::Encoder(Box::new(fitted)), examples)
        }
        (MarketTrainingStageKindV1::Ridge, MarketStageInput::Targets(reader)) => {
            if reader.feature_request() != &read_request(study, data)
                || reader.target_digest() != data.targets_sha256
                || !reader.is_at_start()
            {
                return Err("market Ridge input differs from its fixed training view".into());
            }
            let scaling = reader.fit_feature_scaling(64, study.training.max_training_examples)?;
            let mut inputs = Vec::new();
            let mut targets = Vec::new();
            loop {
                let batch = reader.next_batch(128)?;
                if batch.is_empty() {
                    break;
                }
                for row in batch {
                    if inputs.len() as u64 >= study.training.max_training_examples {
                        return Err("market Ridge exceeds sample budget".into());
                    }
                    inputs.push(
                        row.features
                            .inputs
                            .into_iter()
                            .map(f64::from)
                            .collect::<Vec<_>>(),
                    );
                    targets.push(f64::from(row.target.simple_return));
                }
            }
            reader.finish_pass()?;
            if inputs.len() < 64 {
                return Err("insufficient market Ridge anchors".into());
            }
            let model = fit_market_ridge(&inputs, &targets, &scaling)?;
            (Fitted::Ridge(Box::new(model)), inputs.len() as u64)
        }
        (kind, MarketStageInput::Targets(reader))
            if !matches!(
                kind,
                MarketTrainingStageKindV1::Pretrain | MarketTrainingStageKindV1::Ridge
            ) =>
        {
            let request = adaptation_request(study, key, parent)?;
            let fitted = adapt_market_encoder(reader, request, parent)?;
            let examples = fitted.scaling().examples;
            (Fitted::Task(Box::new(fitted)), examples)
        }
        _ => return Err("market stage received the wrong input role".into()),
    };
    Ok(FittedMarketStage {
        study_sha256,
        key,
        input: study.input.clone(),
        examples,
        model,
    })
}

fn fit_market_ridge(
    inputs: &[Vec<f64>],
    targets: &[f64],
    scaling: &MarketFeatureScalingV1,
) -> Result<CexBaselineModelV1, String> {
    let first = inputs.first().ok_or("empty market Ridge inputs")?;
    let width = first.len();
    let channels = scaling.means.len();
    if channels == 0
        || scaling.scales.len() != channels
        || !width.is_multiple_of(channels)
        || inputs.len() != targets.len()
        || inputs.len() < 2
        || inputs.iter().any(|row| row.len() != width)
        || inputs
            .iter()
            .flatten()
            .chain(targets)
            .chain(&scaling.means)
            .any(|x| !x.is_finite())
        || scaling.scales.iter().any(|x| !x.is_finite() || *x <= 0.0)
    {
        return Err("invalid frozen market Ridge scaling or input".into());
    }
    let means = (0..width)
        .map(|j| scaling.means[j % channels])
        .collect::<Vec<_>>();
    let scales = (0..width)
        .map(|j| scaling.scales[j % channels])
        .collect::<Vec<_>>();
    let varying = (0..width)
        .filter(|j| inputs.iter().any(|row| row[*j] != first[*j]))
        .collect::<Vec<_>>();
    let column_means = varying
        .iter()
        .map(|j| inputs.iter().map(|row| row[*j]).sum::<f64>() / inputs.len() as f64)
        .collect::<Vec<_>>();
    let target_mean = targets.iter().sum::<f64>() / targets.len() as f64;
    let mut coefficients = vec![0.0; width];
    let mut intercept = target_mean;
    if !varying.is_empty() {
        let n = varying.len();
        let mut matrix = vec![vec![0.0; n]; n];
        let mut rhs = vec![0.0; n];
        for (row, target) in inputs.iter().zip(targets) {
            // Center only to solve an unpenalized intercept. The L2 penalty uses
            // the shared channel scale, never a new scale for each lag column.
            let centered = varying
                .iter()
                .zip(&column_means)
                .map(|(j, mean)| (row[*j] - mean) / scales[*j])
                .collect::<Vec<_>>();
            for j in 0..n {
                rhs[j] += centered[j] * (target - target_mean);
                for k in 0..n {
                    matrix[j][k] += centered[j] * centered[k];
                }
            }
        }
        for (j, row) in matrix.iter_mut().enumerate() {
            row[j] += 1e-6;
        }
        let fitted = crate::engines::solve(matrix, rhs)?;
        for ((j, mean), coefficient) in varying.iter().zip(column_means).zip(fitted) {
            coefficients[*j] = coefficient;
            intercept -= coefficient * (mean - means[*j]) / scales[*j];
        }
    }
    let model = CexBaselineModelV1::Ridge {
        intercept,
        means,
        scales,
        coefficients,
    };
    model.validate_inference(width)?;
    Ok(model)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedReconstruction {
    metadata: String,
    weights_hex: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StageManifest {
    schema_version: String,
    study_sha256: String,
    key: MarketTrainingStageKeyV1,
    model_sha256: String,
    neural_metadata: Option<String>,
    reconstruction: Option<EmbeddedReconstruction>,
    examples: u64,
    fitted_values_sha256: String,
}
impl FittedMarketStage {
    pub fn key(&self) -> MarketTrainingStageKeyV1 {
        self.key
    }
    pub fn study_sha256(&self) -> &str {
        &self.study_sha256
    }
    pub fn fitted_values_digest(&self) -> Result<String, String> {
        let values = match &self.model {
            Fitted::Encoder(p) => {
                serde_json::json!({"parameters":p.diagnostics().final_encoder_values_sha256,
                    "reconstruction_parameters":p.reconstruction_parameter_digest().ok_or("pretraining stage omitted its reconstruction head")?,
                    "reconstruction_diagnostics":p.reconstruction_diagnostics_digest()?.ok_or("pretraining stage omitted its bounded reconstruction diagnostics")?,
                    "scaling":p.scaling(),"request":p.request()})
            }
            Fitted::Task(m) => {
                serde_json::json!({"parameters":m.parameter_digest(),"scaling":m.scaling(),"target_scaling":m.target_scaling(),"request":m.request()})
            }
            Fitted::Ridge(m) => serde_json::to_value(m).map_err(|e| e.to_string())?,
        };
        canonical_json_hash(&serde_json::json!({"schema":"monday.market_stage_values.v1","study":self.study_sha256,
            "fold":self.key.fold_id,"kind":self.key.kind,"seed":self.key.seed,"examples":self.examples,"values":values})).map_err(|e|e.to_string())
    }
    fn predict(&self, inputs: &[f32]) -> Result<f64, String> {
        if inputs.len() != self.input.context_rows * self.input.ordered_channels.len()
            || inputs.iter().any(|v| !v.is_finite())
        {
            return Err("market prediction input shape or values changed".into());
        }
        match &self.model {
            Fitted::Encoder(_) => Err("pretrained encoder has no return head".into()),
            Fitted::Task(m) => m.predict(inputs).map(f64::from),
            Fitted::Ridge(m) => {
                m.predict(&inputs.iter().copied().map(f64::from).collect::<Vec<_>>())
            }
        }
    }
    pub fn bundle(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
        let (weights, neural_metadata) = match &self.model {
            Fitted::Encoder(p) => {
                let (metadata, weights) = p.bundle()?;
                (
                    weights,
                    Some(String::from_utf8(metadata).map_err(|e| e.to_string())?),
                )
            }
            Fitted::Task(m) => {
                let (metadata, weights) = m.bundle()?;
                (
                    weights,
                    Some(String::from_utf8(metadata).map_err(|e| e.to_string())?),
                )
            }
            Fitted::Ridge(m) => (serde_json::to_vec(m).map_err(|e| e.to_string())?, None),
        };
        let reconstruction = match &self.model {
            Fitted::Encoder(p) => {
                let (metadata, weights) = p
                    .reconstruction_bundle()?
                    .ok_or("missing reconstruction audit")?;
                Some(EmbeddedReconstruction {
                    metadata: String::from_utf8(metadata).map_err(|e| e.to_string())?,
                    weights_hex: hex::encode(weights),
                })
            }
            _ => None,
        };
        let header = StageManifest {
            schema_version: "monday.market_stage_fit.v1".into(),
            study_sha256: self.study_sha256.clone(),
            key: self.key,
            model_sha256: bytes_digest(&weights),
            neural_metadata,
            reconstruction,
            examples: self.examples,
            fitted_values_sha256: self.fitted_values_digest()?,
        };
        Ok((
            serde_json::to_vec(&header).map_err(|e| e.to_string())?,
            weights,
        ))
    }
    pub fn restore(
        study: &MarketEncoderStudyV1,
        key: MarketTrainingStageKeyV1,
        manifest: &[u8],
        expected_manifest_sha256: &str,
        weights: Vec<u8>,
        parent: Option<&VerifiedMarketStage<'_>>,
    ) -> Result<Self, String> {
        if manifest.len() > 4 * 1024 * 1024
            || weights.len() > 16 * 1024 * 1024
            || bytes_digest(manifest) != expected_manifest_sha256
        {
            return Err("market stage bundle size or external identity differs".into());
        }
        let header: StageManifest = serde_json::from_slice(manifest).map_err(|e| e.to_string())?;
        let parent = parent_encoder(study, key, parent)?;
        if header.schema_version != "monday.market_stage_fit.v1"
            || header.key != key
            || header.study_sha256 != study.content_hash()?
            || header.model_sha256 != bytes_digest(&weights)
            || header.examples < 64
            || header.examples > study.training.max_training_examples
            || (header.reconstruction.is_some()
                != (key.kind == MarketTrainingStageKindV1::Pretrain))
        {
            return Err("market stage artifact drifted from study or coverage".into());
        }
        let model = match key.kind {
            MarketTrainingStageKindV1::Ridge => {
                if header.neural_metadata.is_some() {
                    return Err("Ridge stage has neural metadata".into());
                }
                let model: CexBaselineModelV1 =
                    serde_json::from_slice(&weights).map_err(|e| e.to_string())?;
                if !matches!(model, CexBaselineModelV1::Ridge { .. }) {
                    return Err("market Ridge stage contains another model kind".into());
                }
                model.validate_inference(
                    study.input.context_rows * study.input.ordered_channels.len(),
                )?;
                Fitted::Ridge(Box::new(model))
            }
            kind => {
                let metadata = header
                    .neural_metadata
                    .ok_or("missing market neural metadata")?;
                let digest = bytes_digest(metadata.as_bytes());
                if kind == MarketTrainingStageKindV1::Pretrain {
                    let mut p =
                        MarketEncoderCheckpoint::restore(metadata.as_bytes(), &digest, weights)?;
                    let audit = header
                        .reconstruction
                        .ok_or("missing reconstruction audit")?;
                    if audit.weights_hex.len() > 128 * 1024 {
                        return Err("reconstruction audit exceeds byte limit".into());
                    }
                    p.attach_reconstruction_audit(
                        audit.metadata.as_bytes(),
                        &bytes_digest(audit.metadata.as_bytes()),
                        hex::decode(audit.weights_hex).map_err(|e| e.to_string())?,
                    )?;
                    if p.request() != &study.fit_request(key)?
                        || p.scaling().examples != header.examples
                    {
                        return Err("encoder artifact changed its frozen training request".into());
                    }
                    Fitted::Encoder(Box::new(p))
                } else {
                    let m =
                        MarketTaskModel::restore(metadata.as_bytes(), &digest, weights, parent)?;
                    if m.request() != &adaptation_request(study, key, parent)?
                        || m.scaling().examples != header.examples
                    {
                        return Err("task artifact changed its frozen adaptation request".into());
                    }
                    Fitted::Task(Box::new(m))
                }
            }
        };
        let result = Self {
            study_sha256: header.study_sha256,
            key,
            input: study.input.clone(),
            examples: header.examples,
            model,
        };
        if result.fitted_values_digest() != Ok(header.fitted_values_sha256) {
            return Err("market fitted values changed".into());
        }
        Ok(result)
    }
}

/// Fixed seed mean; callers cannot select only the better seed.
pub struct MarketStudyEnsemble<'a> {
    members: Vec<&'a FittedMarketStage>,
}
impl<'a> MarketStudyEnsemble<'a> {
    pub fn new(
        study: &MarketEncoderStudyV1,
        verified: Vec<&'a VerifiedMarketStage<'a>>,
    ) -> Result<Self, String> {
        study.validate()?;
        let mut members = verified.into_iter().map(|v| v.primary).collect::<Vec<_>>();
        members.sort_by_key(|m| m.key.seed);
        let first = *members.first().ok_or("empty market ensemble")?;
        if first.key.kind == MarketTrainingStageKindV1::Pretrain {
            return Err("encoder-only stages cannot form a return ensemble".into());
        }
        let seeds = if first.key.kind == MarketTrainingStageKindV1::Ridge {
            vec![0]
        } else {
            study.seeds.clone()
        };
        if members.iter().map(|m| m.key.seed).collect::<Vec<_>>() != seeds {
            return Err("market ensemble omitted, duplicated or selected a seed".into());
        }
        for m in &members {
            if m.study_sha256 != study.content_hash()?
                || m.key.fold_id != first.key.fold_id
                || m.key.kind != first.key.kind
                || m.examples != first.examples
            {
                return Err("market ensemble mixes studies, folds or training coverage".into());
            }
            if let (Fitted::Task(a), Fitted::Task(b)) = (&first.model, &m.model) {
                if a.scaling() != b.scaling() || a.target_scaling() != b.target_scaling() {
                    return Err("market ensemble normalization differs between seeds".into());
                }
            }
        }
        Ok(Self { members })
    }
    pub fn entry_decision(
        &self,
        study: &MarketEncoderStudyV1,
        prediction: &MarketPredictionV1,
    ) -> Result<crate::sequence_study::SequenceEntryDecisionV1, String> {
        let first = self.members[0];
        if first.study_sha256 != study.content_hash()? {
            return Err("market entry costs differ from fitted study".into());
        }
        let view = study
            .folds
            .iter()
            .find(|fold| fold.fold_id == first.key.fold_id)
            .ok_or("missing market entry fold")?
            .validation
            .data
            .view;
        if prediction.observed_at_ms < view.decision_start_ms {
            return Err("market entry precedes its validation view".into());
        }
        crate::sequence_study::sequence_entry_from_return(
            prediction.observed_at_ms,
            prediction.spread_bps,
            prediction.predicted_return,
            &study.costs,
            view.end_ms,
        )
    }
    pub fn predict(&self, inputs: &[f32]) -> Result<f64, String> {
        let values = self.predict_members(inputs)?;
        let mean = values.iter().map(|(_, value)| value).sum::<f64>() / values.len() as f64;
        if !mean.is_finite() {
            return Err("nonfinite market ensemble prediction".into());
        }
        Ok(mean)
    }
    pub fn predict_members(&self, inputs: &[f32]) -> Result<Vec<(u64, f64)>, String> {
        self.members
            .iter()
            .map(|m| Ok((m.key.seed, m.predict(inputs)?)))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketPredictionV1 {
    pub observed_at_ms: i64,
    pub spread_bps: f64,
    pub predicted_return: f64,
    pub member_returns: Vec<(u64, f64)>,
    pub observed_return: f32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketPredictionCoverageV1 {
    pub expected: u64,
    pub emitted: u64,
    pub complete: bool,
}
/// Only the declared development validation partition is accepted here.
/// Emitted rows must be staged until the call succeeds. Incomplete coverage
/// forbids an economic pass, including omitted anchors before a future outage.
pub fn predict_market_validation(
    study: &MarketEncoderStudyV1,
    ensemble: &MarketStudyEnsemble<'_>,
    reader: &mut MarketTaskReader,
    mut emit: impl FnMut(MarketPredictionV1) -> Result<(), String>,
) -> Result<MarketPredictionCoverageV1, String> {
    study.validate()?;
    let first = ensemble.members[0];
    if first.study_sha256 != study.content_hash()? {
        return Err("market ensemble study changed".into());
    }
    let data = &study
        .folds
        .iter()
        .find(|f| f.fold_id == first.key.fold_id)
        .ok_or("unknown market validation fold")?
        .validation
        .data;
    if reader.feature_request() != &read_request(study, data)
        || reader.target_digest() != data.targets_sha256
        || !reader.is_at_start()
    {
        return Err("market validation reader drifted or exposes a withheld view".into());
    }
    let expected = ((data.view.end_ms - data.view.decision_start_ms - 30_000) / 1000) as u64;
    let mut report = MarketPredictionCoverageV1 {
        expected,
        emitted: 0,
        complete: true,
    };
    let mut next = data.view.decision_start_ms;
    loop {
        let batch = reader.next_batch(128)?;
        if batch.is_empty() {
            break;
        }
        for row in batch {
            if row.features.observed_at_ms != next {
                report.complete = false;
            }
            next = row
                .features
                .observed_at_ms
                .checked_add(1000)
                .ok_or("market prediction clock overflow")?;
            let member_returns = ensemble.predict_members(&row.features.inputs)?;
            let predicted_return = member_returns.iter().map(|(_, value)| value).sum::<f64>()
                / member_returns.len() as f64;
            if !predicted_return.is_finite() {
                return Err("nonfinite market ensemble prediction".into());
            }
            emit(MarketPredictionV1 {
                observed_at_ms: row.features.observed_at_ms,
                spread_bps: row.target.spread_bps,
                predicted_return,
                member_returns,
                observed_return: row.target.simple_return,
            })?;
            report.emitted += 1;
        }
    }
    reader.finish_pass()?;
    report.complete &= report.emitted == report.expected;
    Ok(report)
}

#[cfg(test)]
#[path = "market_encoder_study/tests.rs"]
mod tests;
