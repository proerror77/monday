//! Frozen sequence request, receipt and portable inference. No fitting backend.
use crate::{
    portable_network::FrozenNetworkV1,
    sequence::{valid_sha256, SequenceInputSpecV1, SequenceViewV1},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
const MAX_SEQUENCE_BATCH: usize = 256;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequenceNeuralKindV1 {
    Mlp,
    Tcn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceTrainingRequestV1 {
    pub model_kind: SequenceNeuralKindV1,
    pub dataset_sha256: String,
    pub input: SequenceInputSpecV1,
    pub view: SequenceViewV1,
    /// Ordered indices for the same-information or price-only input ablation.
    pub channels: Vec<usize>,
    pub hidden_channels: usize,
    pub batch_size: usize,
    pub updates: usize,
    pub learning_rate: f64,
    pub seed: u64,
    pub min_examples: u64,
}

impl SequenceTrainingRequestV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.input.validate()?;
        self.view.validate()?;
        if !valid_sha256(&self.dataset_sha256)
            || (self.model_kind == SequenceNeuralKindV1::Tcn && self.input.context_rows > 63)
            || self.channels.is_empty()
            || self
                .channels
                .iter()
                .any(|i| *i >= self.input.ordered_channels.len())
            || self.channels.windows(2).any(|p| p[0] >= p[1])
            || !(2..=64).contains(&self.hidden_channels)
            || !(1..=MAX_SEQUENCE_BATCH).contains(&self.batch_size)
            || !(1..=16384).contains(&self.updates)
            || self.min_examples < 2
            || !self.learning_rate.is_finite()
            || !(0.0..=0.01).contains(&self.learning_rate)
            || self.learning_rate == 0.0
        {
            return Err("invalid bounded sequence training request".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceScalingV1 {
    pub means: Vec<f64>,
    pub scales: Vec<f64>,
    pub target_means: [f64; 3],
    pub target_scales: [f64; 3],
    pub examples: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceTrainingDiagnosticsV1 {
    pub completed_updates: usize,
    pub examples_seen: u64,
    pub first_training_decision_ms: i64,
    pub last_training_decision_ms: i64,
    /// Pre-update batch loss; not a convergence certificate or validation metric.
    pub batch_losses: Vec<f64>,
    pub raw_gradient_l2: Vec<f64>,
    pub stop_reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenSequenceModelV1 {
    request: SequenceTrainingRequestV1,
    scaling: SequenceScalingV1,
    diagnostics: SequenceTrainingDiagnosticsV1,
    network: FrozenNetworkV1,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    schema_version: String,
    weights_sha256: String,
    request: SequenceTrainingRequestV1,
    scaling: SequenceScalingV1,
    diagnostics: SequenceTrainingDiagnosticsV1,
}

impl FrozenSequenceModelV1 {
    pub fn new(
        request: SequenceTrainingRequestV1,
        scaling: SequenceScalingV1,
        diagnostics: SequenceTrainingDiagnosticsV1,
        network: FrozenNetworkV1,
    ) -> Result<Self, String> {
        let model = Self {
            request,
            scaling,
            diagnostics,
            network,
        };
        model.validate()?;
        Ok(model)
    }
    pub fn validate(&self) -> Result<(), String> {
        let Self {
            request,
            scaling,
            diagnostics,
            network,
        } = self;
        request.validate()?;
        network.validate(
            request.channels.len(),
            request.input.context_rows,
            request.hidden_channels,
            Some(3),
        )?;
        if matches!(network, FrozenNetworkV1::Mlp { .. })
            != (request.model_kind == SequenceNeuralKindV1::Mlp)
            || scaling.means.len() != request.channels.len()
            || scaling.scales.len() != request.channels.len()
            || scaling
                .means
                .iter()
                .chain(&scaling.target_means)
                .any(|v| !v.is_finite())
            || scaling
                .scales
                .iter()
                .chain(&scaling.target_scales)
                .any(|v| !v.is_finite() || *v <= 0.0)
            || scaling.examples < request.min_examples
            || diagnostics.completed_updates != request.updates
            || diagnostics.batch_losses.len() != request.updates
            || diagnostics.raw_gradient_l2.len() != request.updates
            || diagnostics.examples_seen < request.updates as u64
            || diagnostics.examples_seen > (request.updates * request.batch_size) as u64
            || diagnostics.first_training_decision_ms < request.view.decision_start_ms
            || diagnostics.last_training_decision_ms < diagnostics.first_training_decision_ms
            || diagnostics.last_training_decision_ms >= request.view.end_ms
            || diagnostics
                .batch_losses
                .iter()
                .any(|v| !v.is_finite() || *v < 0.0 || *v > 1_000_000.0)
            || diagnostics
                .raw_gradient_l2
                .iter()
                .any(|v| !v.is_finite() || *v < 0.0 || *v > 100.0)
            || diagnostics.stop_reason != "fixed_update_budget_completed"
        {
            return Err("sequence weights, normalization or training receipt is invalid".into());
        }
        Ok(())
    }
    pub fn request(&self) -> &SequenceTrainingRequestV1 {
        &self.request
    }
    pub fn scaling(&self) -> &SequenceScalingV1 {
        &self.scaling
    }
    pub fn diagnostics(&self) -> &SequenceTrainingDiagnosticsV1 {
        &self.diagnostics
    }
    pub fn network(&self) -> &FrozenNetworkV1 {
        &self.network
    }
    pub fn parameter_digest(&self) -> Result<String, String> {
        self.validate()?;
        let mut prefix = b"monday.sequence-neural-parameters.v1".to_vec();
        prefix.extend(
            serde_json::to_vec(&(
                self.request.model_kind,
                self.request.channels.len(),
                self.request.hidden_channels,
                self.request.input.context_rows,
            ))
            .map_err(|e| e.to_string())?,
        );
        Ok(self.network.parameter_digest(&prefix))
    }
    /// Time-major raw features. Targets are absent from this interface.
    pub fn predict(&self, inputs: &[f32]) -> Result<[f32; 3], String> {
        self.validate()?;
        let request = &self.request;
        let columns = request.input.ordered_channels.len();
        if inputs.len() != columns * request.input.context_rows
            || inputs.iter().any(|v| !v.is_finite())
        {
            return Err("sequence inference shape or values differ from training".into());
        }
        let mut normalized =
            Vec::with_capacity(request.channels.len() * request.input.context_rows);
        for (channel, index) in request.channels.iter().enumerate() {
            for time in 0..request.input.context_rows {
                normalized.push(
                    ((f64::from(inputs[time * columns + index]) - self.scaling.means[channel])
                        / self.scaling.scales[channel]) as f32,
                );
            }
        }
        let predicted = self
            .network
            .predict(&normalized, request.input.context_rows)?;
        let raw = std::array::from_fn(|i| {
            (f64::from(predicted[i]) * self.scaling.target_scales[i] + self.scaling.target_means[i])
                as f32
        });
        if raw.iter().any(|v| !v.is_finite()) {
            return Err("sequence raw-return prediction is non-finite".into());
        }
        Ok(raw)
    }
    pub fn bundle(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.validate()?;
        let weights = serde_json::to_vec(&self.network).map_err(|e| e.to_string())?;
        let header = Bundle {
            schema_version: "monday.sequence_portable_bundle.v1".into(),
            weights_sha256: format!("{:x}", Sha256::digest(&weights)),
            request: self.request.clone(),
            scaling: self.scaling.clone(),
            diagnostics: self.diagnostics.clone(),
        };
        Ok((
            serde_json::to_vec(&header).map_err(|e| e.to_string())?,
            weights,
        ))
    }
    pub fn restore_bundle(
        manifest: &[u8],
        expected: &str,
        weights: Vec<u8>,
    ) -> Result<Self, String> {
        if manifest.len() > 2 * 1024 * 1024
            || weights.len() > 16 * 1024 * 1024
            || !valid_sha256(expected)
            || format!("{:x}", Sha256::digest(manifest)) != expected
        {
            return Err("sequence model manifest checksum or size mismatch".into());
        }
        let header: Bundle = serde_json::from_slice(manifest).map_err(|e| e.to_string())?;
        if header.schema_version != "monday.sequence_portable_bundle.v1"
            || format!("{:x}", Sha256::digest(&weights)) != header.weights_sha256
        {
            return Err("sequence portable model binding changed".into());
        }
        Self::new(
            header.request,
            header.scaling,
            header.diagnostics,
            serde_json::from_slice(&weights).map_err(|e| e.to_string())?,
        )
    }
}
