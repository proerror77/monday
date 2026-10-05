//! Frozen market encoder, task head and reconstruction receipts. No training backend.
use crate::{
    market_encoder::*,
    portable_network::{FrozenLinearV1, FrozenNetworkV1},
};
use serde::{Deserialize, Serialize};
pub const ENCODER_PORTABLE_SCHEMA: &str = "monday.market_encoder_portable.v1";
pub const TASK_PORTABLE_SCHEMA: &str = "monday.market_task_portable.v1";
pub const MASK_POLICY: &str = "causal-whole-frame-3s-30pct-prefix6s-v1";
pub type MarketArtifactBytes = (Vec<u8>, Vec<u8>);
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncoderManifest {
    pub schema_version: String,
    pub request: MarketFitRequestV1,
    pub mask_policy: String,
    pub scaling: MarketFeatureScalingV1,
    pub diagnostics: MarketFitDiagnosticsV1,
    pub weights_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconstructionAuditManifest {
    pub schema_version: String,
    pub encoder_checkpoint_sha256: String,
    pub mask_policy: String,
    pub weights_sha256: String,
    pub parameter_values_sha256: String,
    pub diagnostics: Option<ReconstructionDiagnostics>,
    /// Observed wall time for the extra scan and CPU forward passes; not CPU-seconds
    /// and deliberately excluded from independent numerical-fit comparison.
    pub diagnostic_elapsed_micros: Option<u64>,
}

pub const RECONSTRUCTION_AUDIT_SCHEMA: &str = "monday.market_reconstruction_audit.v2";
pub const DIAGNOSTIC_SAMPLE_LIMIT: usize = 256;
pub const DIAGNOSTIC_SAMPLING: &str = "uniform-eligible-rank-endpoints-max256-v1";
pub const DIAGNOSTIC_GROUPING: &str = "sol-lob-24-price11-depth10-trade3-v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconstructionGroupMse {
    pub group: String,
    pub channels: u64,
    pub masked_scalar_count: u64,
    pub model_mse: f64,
    pub last_visible_mse: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconstructionDiagnostics {
    pub schema_version: String,
    pub sampling_policy: String,
    pub grouping_policy: String,
    pub metric_space: String,
    pub mask_epoch: u64,
    pub eligible_training_anchors: u64,
    pub scanned_training_anchors: u64,
    pub sampled_ordinals: Vec<u64>,
    pub sampled_anchor_keys_sha256: String,
    pub additional_feature_passes: u64,
    pub forward_batch_size: usize,
    pub cpu_forward_batches: u64,
    pub cpu_forward_examples: u64,
    pub cpu_forward_frames: u64,
    pub cpu_output_scalars: u64,
    pub additional_optimizer_updates: u64,
    pub groups: Vec<ReconstructionGroupMse>,
}
/// Only the verified registry is classified. Generic ML fixtures with other
/// channels never acquire misleading SOL price/depth/trade labels.
pub fn reconstruction_channel_groups(
    input: &crate::sequence::SequenceInputSpecV1,
) -> Option<[Vec<usize>; 3]> {
    if *input != crate::sequence::SequenceInputSpecV1::sol_lob() {
        return None;
    }
    let mut groups: [Vec<usize>; 3] = Default::default();
    for (i, name) in input.ordered_channels.iter().enumerate() {
        let group = if name == "mid_return_1" || name.ends_with("_distance_bps") {
            0
        } else if name.ends_with("_log_quantity") {
            1
        } else if name.starts_with("aggregate_trade_") {
            2
        } else {
            return None;
        };
        groups[group].push(i);
    }
    (groups.iter().map(Vec::len).collect::<Vec<_>>() == [11, 10, 3]).then_some(groups)
}
pub fn diagnostic_ordinals(examples: u64) -> Vec<u64> {
    let count = examples.min(DIAGNOSTIC_SAMPLE_LIMIT as u64);
    if count < 2 {
        return (0..count).collect();
    }
    (0..count)
        .map(|i| i * (examples - 1) / (count - 1))
        .collect()
}
impl ReconstructionDiagnostics {
    pub fn validate(
        &self,
        request: &MarketFitRequestV1,
        scaling: &MarketFeatureScalingV1,
    ) -> Result<(), String> {
        let groups = reconstruction_channel_groups(&request.spec.input)
            .ok_or("reconstruction diagnostic registry changed")?;
        let expected = diagnostic_ordinals(scaling.examples);
        let count = expected.len() as u64;
        if self.schema_version != "monday.market_reconstruction_diagnostics.v1"
            || self.sampling_policy != DIAGNOSTIC_SAMPLING
            || self.grouping_policy != DIAGNOSTIC_GROUPING
            || self.metric_space != "train-channel-standardized-f32"
            || self.mask_epoch != 0
            || self.eligible_training_anchors != scaling.examples
            || self.scanned_training_anchors != scaling.examples
            || self.sampled_ordinals != expected
            || !crate::sequence::valid_sha256(&self.sampled_anchor_keys_sha256)
            || self.additional_feature_passes != 1
            || self.forward_batch_size != request.batch_size
            || self.cpu_forward_batches != count.div_ceil(request.batch_size as u64)
            || self.cpu_forward_examples != count
            || self.cpu_forward_frames != count * 60
            || self.cpu_output_scalars != count * 60 * 24
            || self.additional_optimizer_updates != 0
            || self.groups.len() != 3
        {
            return Err(
                "reconstruction diagnostics changed their sampling, training view or compute bound"
                    .into(),
            );
        }
        for ((metric, indices), name) in self
            .groups
            .iter()
            .zip(groups)
            .zip(["price", "depth", "trade"])
        {
            if metric.group != name
                || metric.channels != indices.len() as u64
                || metric.masked_scalar_count != count * 18 * indices.len() as u64
                || !metric.model_mse.is_finite()
                || metric.model_mse < 0.0
                || !metric.last_visible_mse.is_finite()
                || metric.last_visible_mse < 0.0
            {
                return Err("reconstruction diagnostic group count or MSE is invalid".into());
            }
        }
        Ok(())
    }
}
pub struct ReconstructionAudit {
    pub metadata: ReconstructionAuditManifest,
    weights: Vec<u8>,
}
/// Immutable encoder only, with no predictive or trading interface.
pub struct FrozenMarketEncoderV1 {
    model: FrozenNetworkV1,
    manifest: EncoderManifest,
    weights: Vec<u8>,
    reconstruction: Option<ReconstructionAudit>,
}
impl FrozenMarketEncoderV1 {
    pub fn network(&self) -> &FrozenNetworkV1 {
        &self.model
    }
    /// Last causal hidden state; no task head or decision policy is applied.
    pub fn encode(&self, inputs: &[f32]) -> Result<Vec<f32>, String> {
        self.model.predict(
            &normalized_inputs(inputs, &self.manifest.request, &self.manifest.scaling)?,
            self.manifest.request.spec.input.context_rows,
        )
    }
    /// Auxiliary reconstruction head is separate from the reusable encoder.
    pub fn reconstruction_bundle(&self) -> Result<Option<MarketArtifactBytes>, String> {
        self.reconstruction
            .as_ref()
            .map(|audit| {
                Ok((
                    serde_json::to_vec(&audit.metadata).map_err(|e| e.to_string())?,
                    audit.weights.clone(),
                ))
            })
            .transpose()
    }
    pub fn reconstruction_parameter_digest(&self) -> Option<&str> {
        self.reconstruction
            .as_ref()
            .map(|audit| audit.metadata.parameter_values_sha256.as_str())
    }
    /// The bounded, deterministic diagnostic evidence compared by independent P fits.
    /// The auxiliary wall clock is not a model value or a selection criterion.
    pub fn reconstruction_diagnostics_digest(&self) -> Result<Option<String>, String> {
        self.reconstruction
            .as_ref()
            .and_then(|a| a.metadata.diagnostics.as_ref())
            .map(digest)
            .transpose()
    }
    pub fn attach_reconstruction_audit(
        &mut self,
        metadata: &[u8],
        expected: &str,
        weights: Vec<u8>,
    ) -> Result<(), String> {
        if metadata.len() > 64 * 1024
            || weights.len() > 64 * 1024
            || bytes_digest(metadata) != expected
        {
            return Err("reconstruction audit size or external identity differs".into());
        }
        let audit: ReconstructionAuditManifest =
            serde_json::from_slice(metadata).map_err(|e| e.to_string())?;
        if audit.schema_version != RECONSTRUCTION_AUDIT_SCHEMA
            || audit.encoder_checkpoint_sha256 != self.identity()?
            || audit.mask_policy != MASK_POLICY
            || audit.weights_sha256 != bytes_digest(&weights)
        {
            return Err("reconstruction audit belongs to another encoder or objective".into());
        }
        let is_sol = reconstruction_channel_groups(&self.manifest.request.spec.input).is_some();
        if audit.diagnostics.is_some() != is_sol
            || audit.diagnostic_elapsed_micros.is_some() != is_sol
        {
            return Err("reconstruction audit omitted or mislabeled its SOL diagnostics".into());
        }
        if let Some(report) = &audit.diagnostics {
            report.validate(&self.manifest.request, &self.manifest.scaling)?;
        }
        let head: FrozenLinearV1 = serde_json::from_slice(&weights).map_err(|e| e.to_string())?;
        head.validate(
            self.manifest.request.spec.hidden_channels,
            self.manifest.request.spec.input.ordered_channels.len(),
        )?;
        if head.parameter_digest(b"monday.market-parameter-values.v1")
            != audit.parameter_values_sha256
        {
            return Err("reconstruction head values differ".into());
        }
        self.reconstruction = Some(ReconstructionAudit {
            metadata: audit,
            weights,
        });
        Ok(())
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
        if meta.schema_version != ENCODER_PORTABLE_SCHEMA
            || meta.mask_policy != MASK_POLICY
            || bytes_digest(&weights) != meta.weights_sha256
        {
            return Err("encoder manifest or weight binding differs".into());
        }
        let model: FrozenNetworkV1 = serde_json::from_slice(&weights).map_err(|e| e.to_string())?;
        model.validate(
            meta.request.spec.input.ordered_channels.len() + 1,
            meta.request.spec.input.context_rows,
            meta.request.spec.hidden_channels,
            None,
        )?;
        if !matches!(model, FrozenNetworkV1::Tcn { .. })
            || model.parameter_digest(b"monday.market-parameter-values.v1")
                != meta.diagnostics.final_encoder_values_sha256
        {
            return Err("encoder values differ from receipt".into());
        }
        Ok(Self {
            model,
            manifest: meta,
            weights,
            reconstruction: None,
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskManifest {
    pub schema_version: String,
    pub request: MarketAdaptationRequestV1,
    pub scaling: MarketFeatureScalingV1,
    pub target_mean: f64,
    pub target_scale: f64,
    pub diagnostics: MarketFitDiagnosticsV1,
    pub weights_sha256: String,
    pub parameter_values_sha256: String,
}
pub struct FrozenMarketTaskModelV1 {
    model: FrozenNetworkV1,
    manifest: TaskManifest,
    weights: Vec<u8>,
}
impl FrozenMarketTaskModelV1 {
    pub fn network(&self) -> &FrozenNetworkV1 {
        &self.model
    }
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
        let normalized =
            normalized_inputs(inputs, &self.manifest.request.fit, &self.manifest.scaling)?;
        let y = self.model.predict(
            &normalized,
            self.manifest.request.fit.spec.input.context_rows,
        )?[0];
        let raw = (f64::from(y) * self.manifest.target_scale + self.manifest.target_mean) as f32;
        if !raw.is_finite() {
            return Err("nonfinite raw market prediction".into());
        }
        Ok(raw)
    }
    pub fn bundle(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
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
        parent: Option<&FrozenMarketEncoderV1>,
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
        if meta.schema_version != TASK_PORTABLE_SCHEMA
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
        let model: FrozenNetworkV1 = serde_json::from_slice(&weights).map_err(|e| e.to_string())?;
        model.validate(
            meta.request.fit.spec.input.ordered_channels.len() + 1,
            meta.request.fit.spec.input.context_rows,
            meta.request.fit.spec.hidden_channels,
            Some(1),
        )?;
        if !matches!(model, FrozenNetworkV1::Tcn { .. })
            || model.parameter_digest(b"monday.market-parameter-values.v1")
                != meta.parameter_values_sha256
            || model
                .encoder()?
                .parameter_digest(b"monday.market-parameter-values.v1")
                != meta.diagnostics.final_encoder_values_sha256
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
pub fn check_parent(
    request: &MarketAdaptationRequestV1,
    parent: Option<&FrozenMarketEncoderV1>,
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

fn normalized_inputs(
    inputs: &[f32],
    request: &MarketFitRequestV1,
    scaling: &MarketFeatureScalingV1,
) -> Result<Vec<f32>, String> {
    let channels = request.spec.input.ordered_channels.len();
    let context = request.spec.input.context_rows;
    if inputs.len() != channels * context {
        return Err("market input shape differs".into());
    }
    let mut values = Vec::with_capacity((channels + 1) * context);
    for channel in 0..channels {
        for time in 0..context {
            let raw = inputs[time * channels + channel];
            let value =
                ((f64::from(raw) - scaling.means[channel]) / scaling.scales[channel]) as f32;
            if !raw.is_finite() || !value.is_finite() {
                return Err("nonfinite normalized market input".into());
            }
            values.push(value);
        }
    }
    values.extend(std::iter::repeat_n(0.0, context));
    Ok(values)
}
