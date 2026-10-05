use super::network::*;
use super::training::input_tensor;
use crate::{lock_ndarray_backend, CpuBackend};
use hft_cex_research_input::market_encoder::UnlabeledSequenceExample;
use hft_research_manifest::market_encoder::*;
use hft_research_manifest::portable_market::{FrozenMarketEncoderV1, FrozenMarketTaskModelV1};
use hft_research_manifest::portable_network::FrozenLinearV1;
use serde::{Deserialize, Serialize};

pub(super) const MASK_POLICY: &str = "causal-whole-frame-3s-30pct-prefix6s-v1";
pub type MarketArtifactBytes = (Vec<u8>, Vec<u8>);
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReconstructionAuditManifest {
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

pub(super) const RECONSTRUCTION_AUDIT_SCHEMA: &str = "monday.market_reconstruction_audit.v2";
pub(super) const DIAGNOSTIC_SAMPLE_LIMIT: usize = 256;
pub(super) const DIAGNOSTIC_SAMPLING: &str = "uniform-eligible-rank-endpoints-max256-v1";
pub(super) const DIAGNOSTIC_GROUPING: &str = "sol-lob-24-price11-depth10-trade3-v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReconstructionGroupMse {
    pub group: String,
    pub channels: u64,
    pub masked_scalar_count: u64,
    pub model_mse: f64,
    pub last_visible_mse: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReconstructionDiagnostics {
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
pub(super) fn reconstruction_channel_groups(
    input: &hft_research_manifest::sequence::SequenceInputSpecV1,
) -> Option<[Vec<usize>; 3]> {
    if *input != hft_research_manifest::sequence::SequenceInputSpecV1::sol_lob() {
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
pub(super) fn diagnostic_ordinals(examples: u64) -> Vec<u64> {
    let count = examples.min(DIAGNOSTIC_SAMPLE_LIMIT as u64);
    if count < 2 {
        return (0..count).collect();
    }
    (0..count)
        .map(|i| i * (examples - 1) / (count - 1))
        .collect()
}
impl ReconstructionDiagnostics {
    pub(super) fn validate(
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
            || !hft_research_manifest::sequence::valid_sha256(&self.sampled_anchor_keys_sha256)
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
pub(super) struct ReconstructionAudit {
    pub metadata: ReconstructionAuditManifest,
    pub weights: Vec<u8>,
}
/// Immutable encoder only, with no predictive or trading interface.
pub struct MarketEncoderCheckpoint {
    pub(super) model: Encoder<CpuBackend>,
    pub(super) manifest: EncoderManifest,
    pub(super) weights: Vec<u8>,
    pub(super) reconstruction: Option<ReconstructionAudit>,
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
    pub fn portable(&self) -> Result<FrozenMarketEncoderV1, String> {
        let manifest = serde_json::to_vec(&self.manifest).map_err(|e| e.to_string())?;
        let mut frozen = FrozenMarketEncoderV1::restore(
            &manifest,
            &bytes_digest(&manifest),
            self.weights.clone(),
        )?;
        if let Some(audit) = &self.reconstruction {
            let metadata = serde_json::to_vec(&audit.metadata).map_err(|e| e.to_string())?;
            frozen.attach_reconstruction_audit(
                &metadata,
                &bytes_digest(&metadata),
                audit.weights.clone(),
            )?;
        }
        Ok(frozen)
    }
    pub fn restore(manifest: &[u8], expected: &str, weights: Vec<u8>) -> Result<Self, String> {
        let frozen = FrozenMarketEncoderV1::restore(manifest, expected, weights.clone())?;
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        Ok(Self {
            model: Encoder::from_portable(frozen.network())?,
            manifest: serde_json::from_slice(manifest).map_err(|e| e.to_string())?,
            weights,
            reconstruction: None,
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
    pub fn portable(
        &self,
        parent: Option<&MarketEncoderCheckpoint>,
    ) -> Result<FrozenMarketTaskModelV1, String> {
        let manifest = serde_json::to_vec(&self.manifest).map_err(|e| e.to_string())?;
        let parent = parent.map(MarketEncoderCheckpoint::portable).transpose()?;
        FrozenMarketTaskModelV1::restore(
            &manifest,
            &bytes_digest(&manifest),
            self.weights.clone(),
            parent.as_ref(),
        )
    }
    pub fn restore(
        manifest: &[u8],
        expected: &str,
        weights: Vec<u8>,
        parent: Option<&MarketEncoderCheckpoint>,
    ) -> Result<Self, String> {
        let frozen_parent = parent.map(MarketEncoderCheckpoint::portable).transpose()?;
        let frozen = FrozenMarketTaskModelV1::restore(
            manifest,
            expected,
            weights.clone(),
            frozen_parent.as_ref(),
        )?;
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        Ok(Self {
            model: TaskNetwork::from_portable(frozen.network())?,
            manifest: serde_json::from_slice(manifest).map_err(|e| e.to_string())?,
            weights,
        })
    }
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
