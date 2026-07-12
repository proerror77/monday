use alpha_domain::OnnxModelCandidate;
use alpha_engine::{
    evaluation::{evaluate_sealed_holdout, EngineContext, PreparedDataset, ResearchRow},
    formula_evaluator::FormulaEvaluator,
    CandidateEvaluation,
};
use hft_infer_onnx::OnnxPredictor;
use std::path::Path;

pub struct OnnxEvaluator {
    policy: FormulaEvaluator,
}

impl OnnxEvaluator {
    pub fn for_mission(mission: &alpha_domain::ResearchMission) -> Result<Self, String> {
        Ok(Self {
            policy: FormulaEvaluator::for_mission(mission)?,
        })
    }

    pub fn evaluate(
        &self,
        model: &OnnxModelCandidate,
        model_path: &Path,
        context: &EngineContext<'_>,
    ) -> Result<CandidateEvaluation, String> {
        let predictor = load_predictor(model, model_path)?;
        let signals = infer_signals(model, context.rows(), |input| predictor.infer(input))?;
        self.policy.evaluate_onnx_signals(
            context.rows(),
            &signals,
            context.folds().iter().map(|fold| fold.validation.clone()),
            false,
        )
    }

    pub fn evaluate_sealed(
        &self,
        model: &OnnxModelCandidate,
        model_path: &Path,
        dataset: &PreparedDataset,
    ) -> Result<CandidateEvaluation, String> {
        let predictor = load_predictor(model, model_path)?;
        evaluate_sealed_holdout(dataset, |rows| {
            let signals = infer_signals(model, rows, |input| predictor.infer(input))?;
            self.policy
                .evaluate_onnx_signals(rows, &signals, std::iter::once(0..rows.len()), true)
        })
    }
}

fn load_predictor(model: &OnnxModelCandidate, model_path: &Path) -> Result<OnnxPredictor, String> {
    let shape = input_shape(model)?;
    let checksum = model
        .artifact
        .checksum
        .as_deref()
        .ok_or_else(|| "ONNX candidate checksum is missing".to_string())?;
    OnnxPredictor::load_verified(model_path, checksum, shape).map_err(|error| error.to_string())
}

fn input_shape(model: &OnnxModelCandidate) -> Result<(usize, usize, usize, usize), String> {
    model.validate().map_err(|error| error.to_string())?;
    let [input] = model.inputs.as_slice() else {
        return Err("governed ONNX evaluation supports one input tensor".to_string());
    };
    let [Some(batch), Some(channels), Some(window), Some(levels)] = input.dimensions.as_slice()
    else {
        return Err("ONNX input must have static [1, 4, window, levels] shape".to_string());
    };
    if *batch != 1 || *channels != 4 || *window == 0 || *levels == 0 {
        return Err("ONNX input must have static [1, 4, window, levels] shape".to_string());
    }
    if !matches!(
        model.output.dimensions.as_slice(),
        [Some(1)] | [Some(1), Some(1)]
    ) {
        return Err("ONNX output must contain one signal".to_string());
    }
    Ok((*batch, *channels, *window, *levels))
}

fn infer_signals<E: std::fmt::Display>(
    model: &OnnxModelCandidate,
    rows: &[ResearchRow],
    infer: impl Fn(&[f32]) -> Result<Vec<f32>, E>,
) -> Result<Vec<f64>, String> {
    let (_, channels, window, levels) = input_shape(model)?;
    let mut signals = Vec::with_capacity(rows.len());
    let channel_names = [
        "bid_price_offset",
        "bid_log_size",
        "ask_price_offset",
        "ask_log_size",
    ];
    debug_assert_eq!(channels, channel_names.len());
    for row in rows {
        let mut input = Vec::with_capacity(channels * window * levels);
        for channel in channel_names {
            for time in 0..window {
                for level in 0..levels {
                    let name = format!("{channel}.t{time}.l{level}");
                    let value = row
                        .features
                        .get(&name)
                        .copied()
                        .ok_or_else(|| format!("ONNX research feature is missing: {name}"))?;
                    input.push(value as f32);
                }
            }
        }
        let output = infer(&input).map_err(|error| format!("ONNX inference failed: {error}"))?;
        let [signal] = output.as_slice() else {
            return Err("ONNX inference must return exactly one signal".to_string());
        };
        if !signal.is_finite() {
            return Err("ONNX inference returned a non-finite signal".to_string());
        }
        signals.push(f64::from(*signal));
    }
    Ok(signals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::{TensorElementType, TensorSpec, LOB_ONNX_PREPROCESSING_VERSION};
    use chrono::{Duration, Utc};
    use hft_research_manifest::ArtifactRef;
    use std::collections::BTreeMap;

    fn model() -> OnnxModelCandidate {
        OnnxModelCandidate {
            artifact: ArtifactRef {
                uri: "model.onnx".to_string(),
                content_type: "application/onnx".to_string(),
                checksum: Some("a".repeat(64)),
            },
            byte_len: 1,
            opset: 17,
            preprocessing_version: LOB_ONNX_PREPROCESSING_VERSION.to_string(),
            inputs: vec![TensorSpec {
                name: "lob".to_string(),
                element_type: TensorElementType::Float32,
                dimensions: vec![Some(1), Some(4), Some(1), Some(1)],
            }],
            output: TensorSpec {
                name: "signal".to_string(),
                element_type: TensorElementType::Float32,
                dimensions: vec![Some(1), Some(1)],
            },
        }
    }

    #[test]
    fn input_mapping_matches_runtime_channel_time_level_order() {
        let row = ResearchRow {
            available_time: Utc::now(),
            signal: 0.0,
            features: BTreeMap::from([
                ("ask_log_size.t0.l0".to_string(), 4.0),
                ("ask_price_offset.t0.l0".to_string(), 3.0),
                ("bid_log_size.t0.l0".to_string(), 2.0),
                ("bid_price_offset.t0.l0".to_string(), 1.0),
            ]),
            label: 1.0,
            fee_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
        };
        let signals = infer_signals(&model(), &[row], |input| {
            assert_eq!(input, &[1.0, 2.0, 3.0, 4.0]);
            Ok::<_, String>(vec![0.5])
        })
        .unwrap();
        assert_eq!(signals, vec![0.5]);
    }

    #[test]
    fn model_signal_uses_the_same_governed_cost_and_edge_policy() {
        let start = Utc::now();
        let rows = (0..500)
            .map(|index| {
                let signal = if index % 2 == 0 { 1.0 } else { -1.0 };
                ResearchRow {
                    available_time: start + Duration::seconds(index),
                    signal: 0.0,
                    features: BTreeMap::from([
                        ("ask_log_size.t0.l0".to_string(), 0.0),
                        ("ask_price_offset.t0.l0".to_string(), 0.0),
                        ("bid_log_size.t0.l0".to_string(), 0.0),
                        ("bid_price_offset.t0.l0".to_string(), signal),
                    ]),
                    label: signal * 0.01,
                    fee_bps: 0.0,
                    funding_bps: 0.0,
                    latency_bps: 0.0,
                }
            })
            .collect::<Vec<_>>();
        let signals =
            infer_signals(&model(), &rows, |input| Ok::<_, String>(vec![input[0]])).unwrap();
        let policy = FormulaEvaluator::for_trials(1).unwrap();
        let evaluation = policy
            .evaluate_onnx_signals(&rows, &signals, std::iter::once(0..rows.len()), false)
            .unwrap();
        assert!(evaluation.passed);
    }
}
