use super::data::UnlabeledSequenceExample;
use super::network::*;
use super::training::input_tensor;
use crate::{lock_ndarray_backend, CpuBackend};
use burn::nn::LinearConfig;
use burn_ndarray::NdArrayDevice;
use hft_research_manifest::market_encoder::*;
use serde::{Deserialize, Serialize};

pub(super) const MASK_POLICY: &str = "causal-whole-frame-3s-30pct-prefix6s-v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EncoderManifest {
    pub schema_version: String,
    pub request: MarketFitRequestV1,
    pub mask_policy: String,
    pub scaling: MarketFeatureScalingV1,
    pub diagnostics: MarketFitDiagnosticsV1,
    pub weights_sha256: String,
}
/// Immutable encoder only, with no predictive or trading interface.
pub struct MarketEncoderCheckpoint {
    pub(super) model: Encoder<CpuBackend>,
    pub(super) manifest: EncoderManifest,
    pub(super) weights: Vec<u8>,
}
impl MarketEncoderCheckpoint {
    /// Last causal hidden state; no task head or decision policy is applied.
    pub fn encode(&self, inputs: &[f32]) -> Result<Vec<f32>, String> {
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        let example = UnlabeledSequenceExample {
            series_id: 0,
            observed_at_ms: 0,
            inputs: inputs.to_vec(),
        };
        let x = input_tensor::<CpuBackend>(
            &[example],
            &self.manifest.request,
            &self.manifest.scaling,
            None,
        )?;
        let hidden = self.model.forward(x);
        let [batch, channels, length] = hidden.dims();
        let values = hidden
            .slice([0..batch, 0..channels, length - 1..length])
            .into_data()
            .into_vec::<f32>()
            .map_err(|e| e.to_string())?;
        if values.iter().any(|x| !x.is_finite()) {
            return Err("nonfinite market representation".into());
        }
        Ok(values)
    }
    pub fn request(&self) -> &MarketFitRequestV1 {
        &self.manifest.request
    }
    pub fn scaling(&self) -> &MarketFeatureScalingV1 {
        &self.manifest.scaling
    }
    pub fn diagnostics(&self) -> &MarketFitDiagnosticsV1 {
        &self.manifest.diagnostics
    }
    pub fn identity(&self) -> Result<String, String> {
        digest(&self.manifest)
    }
    pub fn bundle(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        let weights = self.weights.clone();
        if bytes_digest(&weights) != self.manifest.weights_sha256 {
            return Err("encoder serialization changed".into());
        }
        Ok((
            serde_json::to_vec(&self.manifest).map_err(|e| e.to_string())?,
            weights,
        ))
    }
    pub fn restore(manifest: &[u8], expected: &str, weights: Vec<u8>) -> Result<Self, String> {
        verify_bytes(manifest, expected, &weights)?;
        let meta: EncoderManifest = serde_json::from_slice(manifest).map_err(|e| e.to_string())?;
        meta.request.validate()?;
        meta.scaling.validate(&meta.request)?;
        meta.diagnostics.validate(&meta.request)?;
        if meta.schema_version != ENCODER_SCHEMA
            || meta.mask_policy != MASK_POLICY
            || bytes_digest(&weights) != meta.weights_sha256
        {
            return Err("encoder manifest or weight binding differs".into());
        }
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        let mut model = Encoder::<CpuBackend>::new(&meta.request.spec, &NdArrayDevice::Cpu);
        load(&mut model, weights.clone())?;
        if values_digest(&model)? != meta.diagnostics.final_encoder_values_sha256 {
            return Err("encoder values differ from receipt".into());
        }
        Ok(Self {
            model,
            manifest: meta,
            weights,
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskManifest {
    pub schema_version: String,
    pub request: MarketAdaptationRequestV1,
    pub scaling: MarketFeatureScalingV1,
    pub target_mean: f64,
    pub target_scale: f64,
    pub diagnostics: MarketFitDiagnosticsV1,
    pub weights_sha256: String,
    pub parameter_values_sha256: String,
}
pub struct MarketTaskModel {
    pub(super) model: TaskNetwork<CpuBackend>,
    pub(super) manifest: TaskManifest,
    pub(super) weights: Vec<u8>,
}
impl MarketTaskModel {
    pub fn request(&self) -> &MarketAdaptationRequestV1 {
        &self.manifest.request
    }
    pub fn diagnostics(&self) -> &MarketFitDiagnosticsV1 {
        &self.manifest.diagnostics
    }
    pub fn scaling(&self) -> &MarketFeatureScalingV1 {
        &self.manifest.scaling
    }
    pub fn target_scaling(&self) -> (f64, f64) {
        (self.manifest.target_mean, self.manifest.target_scale)
    }
    pub fn parameter_digest(&self) -> &str {
        &self.manifest.parameter_values_sha256
    }
    pub fn predict(&self, inputs: &[f32]) -> Result<f32, String> {
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        let example = UnlabeledSequenceExample {
            series_id: 0,
            observed_at_ms: 0,
            inputs: inputs.to_vec(),
        };
        let x = input_tensor::<CpuBackend>(
            &[example],
            &self.manifest.request.fit,
            &self.manifest.scaling,
            None,
        )?;
        let y = self
            .model
            .forward(x, false)
            .into_data()
            .into_vec::<f32>()
            .map_err(|e| e.to_string())?[0];
        let raw = (f64::from(y) * self.manifest.target_scale + self.manifest.target_mean) as f32;
        if !raw.is_finite() {
            return Err("nonfinite raw market prediction".into());
        }
        Ok(raw)
    }
    pub fn bundle(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        let weights = self.weights.clone();
        if bytes_digest(&weights) != self.manifest.weights_sha256 {
            return Err("market task serialization changed".into());
        }
        Ok((
            serde_json::to_vec(&self.manifest).map_err(|e| e.to_string())?,
            weights,
        ))
    }
    /// Inherited models require the verified parent, not only its claimed hash.
    pub fn restore(
        manifest: &[u8],
        expected: &str,
        weights: Vec<u8>,
        parent: Option<&MarketEncoderCheckpoint>,
    ) -> Result<Self, String> {
        verify_bytes(manifest, expected, &weights)?;
        let meta: TaskManifest = serde_json::from_slice(manifest).map_err(|e| e.to_string())?;
        meta.request.validate()?;
        meta.scaling.validate(&meta.request.fit)?;
        meta.diagnostics.validate(&meta.request.fit)?;
        check_parent(&meta.request, parent)?;
        if let Some(p) = parent {
            if meta.scaling != p.manifest.scaling
                || meta.diagnostics.initial_encoder_values_sha256
                    != p.manifest.diagnostics.final_encoder_values_sha256
            {
                return Err("task did not inherit its declared parent scaling and values".into());
            }
        }
        if meta.schema_version != TASK_SCHEMA
            || bytes_digest(&weights) != meta.weights_sha256
            || !meta.target_mean.is_finite()
            || !meta.target_scale.is_finite()
            || meta.target_scale <= 0.0
            || (meta.request.mode == AdaptationModeV1::LinearProbe
                && meta.diagnostics.initial_encoder_values_sha256
                    != meta.diagnostics.final_encoder_values_sha256)
        {
            return Err("invalid market task artifact or frozen encoder".into());
        }
        if meta.request.mode == AdaptationModeV1::FullFineTune
            && meta.diagnostics.initial_encoder_values_sha256
                == meta.diagnostics.final_encoder_values_sha256
        {
            return Err("fine-tuning artifact did not update encoder values".into());
        }
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        let device = NdArrayDevice::Cpu;
        let mut model = TaskNetwork {
            encoder: Encoder::<CpuBackend>::new(&meta.request.fit.spec, &device),
            head: LinearConfig::new(meta.request.fit.spec.hidden_channels, 1).init(&device),
        };
        load(&mut model, weights.clone())?;
        if values_digest(&model)? != meta.parameter_values_sha256
            || values_digest(&model.encoder)? != meta.diagnostics.final_encoder_values_sha256
        {
            return Err("task values differ from receipt".into());
        }
        Ok(Self {
            model,
            manifest: meta,
            weights,
        })
    }
}
fn verify_bytes(manifest: &[u8], expected: &str, weights: &[u8]) -> Result<(), String> {
    if manifest.len() > 2 * 1024 * 1024
        || weights.len() > 16 * 1024 * 1024
        || bytes_digest(manifest) != expected
    {
        return Err("market bundle size or external manifest hash mismatch".into());
    }
    Ok(())
}
pub(super) fn check_parent(
    request: &MarketAdaptationRequestV1,
    parent: Option<&MarketEncoderCheckpoint>,
) -> Result<(), String> {
    match (request.mode, parent) {
        (AdaptationModeV1::Scratch, None) => Ok(()),
        (AdaptationModeV1::Scratch, Some(_)) | (_, None) => {
            Err("market adaptation parent is missing or unexpected".into())
        }
        (_, Some(parent)) => {
            let p = parent.request();
            let f = &request.fit;
            if request.parent_checkpoint_sha256.as_deref() != Some(parent.identity()?.as_str())
                || p.spec != f.spec
                || p.feature_dataset_sha256 != f.feature_dataset_sha256
                || p.qualified_anchors_sha256 != f.qualified_anchors_sha256
                || p.view != f.view
                || p.anchor_end_ms != f.anchor_end_ms
                || p.seed != f.seed
                || p.min_examples != f.min_examples
                || p.max_examples != f.max_examples
            {
                return Err(
                    "market parent has a different input, seed, scaling view or cutoff".into(),
                );
            }
            Ok(())
        }
    }
}
