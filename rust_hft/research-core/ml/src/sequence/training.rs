//! Small TCN and flattened MLP fitting on identical bounded causal batches.
//! This module has no execution authority.
use super::{SequenceExample, SequenceReader, MAX_SEQUENCE_BATCH};
use crate::{lock_ndarray_backend, CpuAutodiffBackend, CpuBackend};
use burn::{
    module::{AutodiffModule, Module, ModuleVisitor, Param},
    nn::{
        conv::{Conv1d, Conv1dConfig},
        loss::{MseLoss, Reduction},
        Linear, LinearConfig,
    },
    optim::{AdamConfig, GradientsParams, Optimizer},
    tensor::{activation::relu, backend::Backend, Tensor, TensorData},
};
use burn_ndarray::NdArrayDevice;
use burn_store::{BurnpackStore, ModuleSnapshot};
use hft_research_manifest::sequence::{valid_sha256, SequenceInputSpecV1, SequenceViewV1};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

#[derive(Module, Debug)]
struct CausalTcn<B: Backend> {
    convolutions: Vec<Conv1d<B>>,
    output: Linear<B>,
}

#[derive(Module, Debug)]
struct FlattenedMlp<B: Backend> {
    hidden: Linear<B>,
    output: Linear<B>,
}

#[derive(Module, Debug)]
enum SequenceNetwork<B: Backend> {
    Tcn(CausalTcn<B>),
    Mlp(FlattenedMlp<B>),
}

impl<B: Backend> SequenceNetwork<B> {
    fn new(request: &SequenceTrainingRequestV1, device: &B::Device) -> Self {
        match request.model_kind {
            SequenceNeuralKindV1::Tcn => Self::Tcn(CausalTcn::new(
                request.channels.len(),
                request.hidden_channels,
                device,
            )),
            SequenceNeuralKindV1::Mlp => Self::Mlp(FlattenedMlp {
                hidden: LinearConfig::new(
                    request.channels.len() * request.input.context_rows,
                    request.hidden_channels,
                )
                .init(device),
                output: LinearConfig::new(request.hidden_channels, 3).init(device),
            }),
        }
    }

    fn forward(&self, x: Tensor<B, 3>) -> Tensor<B, 2> {
        match self {
            Self::Tcn(model) => model.forward(x),
            Self::Mlp(model) => {
                let [batch, channels, context] = x.dims();
                model.output.forward(relu(
                    model.hidden.forward(x.reshape([batch, channels * context])),
                ))
            }
        }
    }
}

impl<B: Backend> CausalTcn<B> {
    fn new(channels: usize, hidden: usize, device: &B::Device) -> Self {
        let convolutions = [1, 2, 4, 8, 16]
            .into_iter()
            .enumerate()
            .map(|(index, dilation)| {
                Conv1dConfig::new(if index == 0 { channels } else { hidden }, hidden, 3)
                    .with_dilation(dilation)
                    .init(device)
            })
            .collect();
        Self {
            convolutions,
            output: LinearConfig::new(hidden, 3).init(device),
        }
    }

    fn forward(&self, mut x: Tensor<B, 3>) -> Tensor<B, 2> {
        // Explicit left padding: symmetric convolution padding would expose future positions.
        for conv in &self.convolutions {
            let [batch, channels, _] = x.dims();
            let padding = Tensor::zeros([batch, channels, 2 * conv.dilation], &x.device());
            x = relu(conv.forward(Tensor::cat(vec![padding, x], 2)));
        }
        let [batch, channels, length] = x.dims();
        self.output.forward(
            x.slice([0..batch, 0..channels, length - 1..length])
                .reshape([batch, channels]),
        )
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

#[derive(Default, Clone, Copy)]
struct Moments {
    count: u64,
    mean: f64,
    m2: f64,
}
impl Moments {
    fn push(&mut self, value: f64) {
        self.count += 1;
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        self.m2 += delta * (value - self.mean);
    }
    fn scale(&self) -> f64 {
        let sd = (self.m2 / self.count.max(1) as f64).max(0.0).sqrt();
        if sd > 0.0 {
            sd
        } else {
            1.0
        }
    }
}

fn fit_scaling(
    reader: &mut SequenceReader,
    request: &SequenceTrainingRequestV1,
) -> Result<SequenceScalingV1, String> {
    let mut features = vec![Moments::default(); request.channels.len()];
    let mut targets = [Moments::default(); 3];
    let mut examples = 0;
    loop {
        let batch = reader.next_batch(request.batch_size)?;
        if batch.is_empty() {
            break;
        }
        for example in batch {
            for frame in example
                .inputs
                .chunks_exact(request.input.ordered_channels.len())
            {
                for (moment, index) in features.iter_mut().zip(&request.channels) {
                    moment.push(frame[*index] as f64);
                }
            }
            for (moment, target) in targets.iter_mut().zip(example.targets) {
                moment.push(target as f64);
            }
            examples += 1;
        }
    }
    if examples < request.min_examples {
        return Err("insufficient mature sequence training examples".into());
    }
    reader.rewind()?;
    Ok(SequenceScalingV1 {
        means: features.iter().map(|m| m.mean).collect(),
        scales: features.iter().map(Moments::scale).collect(),
        target_means: targets.map(|m| m.mean),
        target_scales: targets.map(|m| m.scale()),
        examples,
    })
}

fn input_values(
    examples: &[SequenceExample],
    request: &SequenceTrainingRequestV1,
    scaling: &SequenceScalingV1,
) -> Result<Vec<f32>, String> {
    let columns = request.input.ordered_channels.len();
    let mut result =
        Vec::with_capacity(examples.len() * request.channels.len() * request.input.context_rows);
    for example in examples {
        if example.inputs.len() != columns * request.input.context_rows {
            return Err("sequence inference shape differs from training".into());
        }
        for (channel, index) in request.channels.iter().enumerate() {
            for time in 0..request.input.context_rows {
                let value = ((f64::from(example.inputs[time * columns + index])
                    - scaling.means[channel])
                    / scaling.scales[channel]) as f32;
                if !value.is_finite() {
                    return Err("sequence normalized feature is non-finite".into());
                }
                result.push(value);
            }
        }
    }
    Ok(result)
}

struct GradientVisitor<'a> {
    gradients: &'a mut GradientsParams,
    norm: f64,
    count: usize,
    scale: f32,
    error: Option<String>,
}
impl ModuleVisitor<CpuAutodiffBackend> for GradientVisitor<'_> {
    fn visit_float<const D: usize>(&mut self, param: &Param<Tensor<CpuAutodiffBackend, D>>) {
        let Some(gradient) = self.gradients.get::<CpuBackend, D>(param.id) else {
            self.error = Some("sequence parameter gradient is missing".into());
            return;
        };
        match gradient.clone().into_data().into_vec::<f32>() {
            Ok(values) => {
                for value in values {
                    if !value.is_finite() {
                        self.error = Some("sequence gradient is non-finite".into());
                    }
                    self.norm = self.norm.hypot(f64::from(value));
                }
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        if self.scale != 1.0 {
            self.gradients.register(param.id, gradient * self.scale);
        }
        self.count += 1;
    }
}

fn control_gradients(
    model: &SequenceNetwork<CpuAutodiffBackend>,
    gradients: &mut GradientsParams,
) -> Result<f64, String> {
    let mut visitor = GradientVisitor {
        gradients,
        norm: 0.0,
        count: 0,
        scale: 1.0,
        error: None,
    };
    model.visit(&mut visitor);
    if let Some(error) = visitor.error {
        return Err(error);
    }
    if visitor.count != visitor.gradients.len() || !visitor.norm.is_finite() || visitor.norm > 100.0
    {
        return Err("sequence raw gradient exceeded bound".into());
    }
    let norm = visitor.norm;
    if norm > 1.0 {
        visitor.scale = (1.0 / norm) as f32;
        model.visit(&mut visitor);
        if let Some(error) = visitor.error {
            return Err(error);
        }
        let mut applied = GradientVisitor {
            gradients: visitor.gradients,
            norm: 0.0,
            count: 0,
            scale: 1.0,
            error: None,
        };
        model.visit(&mut applied);
        if applied.error.is_some() || (applied.norm - 1.0).abs() > 8.0 * f64::from(f32::EPSILON) {
            return Err("sequence global gradient clipping bound failed".into());
        }
    }
    Ok(norm)
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

fn sample_batch(
    reader: &mut SequenceReader,
    seen: &mut u64,
    total: u64,
    take: u64,
    size: usize,
) -> Result<Vec<SequenceExample>, String> {
    let mut batch = Vec::with_capacity(size);
    while batch.len() < size {
        let next = reader.next_batch(1)?;
        let Some(example) = next.into_iter().next() else {
            if *seen != total {
                return Err("sequence training coverage changed after scaling".into());
            }
            break;
        };
        if *seen >= total {
            return Err("sequence training examples exceeded admitted coverage".into());
        }
        // Evenly cover the complete recent window, rather than spending a small
        // update budget on only the earliest rows of a multi-day dataset.
        let before = u128::from(*seen) * u128::from(take) / u128::from(total);
        *seen += 1;
        let after = u128::from(*seen) * u128::from(take) / u128::from(total);
        if after > before {
            batch.push(example);
        }
    }
    Ok(batch)
}

pub struct TrainedSequenceModel {
    model: SequenceNetwork<CpuBackend>,
    request: SequenceTrainingRequestV1,
    scaling: SequenceScalingV1,
    diagnostics: SequenceTrainingDiagnosticsV1,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SequenceBundleV1 {
    schema_version: String,
    weights_sha256: String,
    request: SequenceTrainingRequestV1,
    scaling: SequenceScalingV1,
    diagnostics: SequenceTrainingDiagnosticsV1,
}

pub fn train_sequence_model(
    reader: &mut SequenceReader,
    request: SequenceTrainingRequestV1,
) -> Result<TrainedSequenceModel, String> {
    request.validate()?;
    if reader.dataset.digest()? != request.dataset_sha256
        || reader.view != request.view
        || reader.input_spec() != &request.input
    {
        return Err("sequence training request differs from admitted dataset or view".into());
    }
    if reader.shard_index != 0 || reader.shard.is_some() {
        return Err("sequence training reader must start at its admitted beginning".into());
    }
    let scaling = fit_scaling(reader, &request)?;
    let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
    let device = NdArrayDevice::Cpu;
    CpuAutodiffBackend::seed(&device, request.seed);
    let mut model = SequenceNetwork::<CpuAutodiffBackend>::new(&request, &device);
    let mut optimizer = AdamConfig::new().init();
    let mut diagnostics = SequenceTrainingDiagnosticsV1 {
        completed_updates: 0,
        examples_seen: 0,
        first_training_decision_ms: i64::MAX,
        last_training_decision_ms: 0,
        batch_losses: Vec::new(),
        raw_gradient_l2: Vec::new(),
        stop_reason: "fixed_update_budget_completed".into(),
    };
    let mut epoch_seen = 0_u64;
    let mut epoch_take = scaling
        .examples
        .min((request.updates * request.batch_size) as u64);
    while diagnostics.completed_updates < request.updates {
        let batch = sample_batch(
            reader,
            &mut epoch_seen,
            scaling.examples,
            epoch_take,
            request.batch_size,
        )?;
        if batch.is_empty() {
            reader.rewind()?;
            epoch_seen = 0;
            epoch_take = scaling.examples.min(
                ((request.updates - diagnostics.completed_updates) * request.batch_size) as u64,
            );
            continue;
        }
        let values = input_values(&batch, &request, &scaling)?;
        let mut targets = Vec::with_capacity(batch.len() * 3);
        for example in &batch {
            for i in 0..3 {
                let value = ((f64::from(example.targets[i]) - scaling.target_means[i])
                    / scaling.target_scales[i]) as f32;
                if !value.is_finite() {
                    return Err("sequence normalized target is non-finite".into());
                }
                targets.push(value);
            }
        }
        let x = Tensor::from_data(
            TensorData::new(
                values,
                [
                    batch.len(),
                    request.channels.len(),
                    request.input.context_rows,
                ],
            ),
            &device,
        );
        let y = Tensor::from_data(TensorData::new(targets, [batch.len(), 3]), &device);
        let loss = MseLoss::new().forward(model.forward(x), y, Reduction::Mean);
        let scalar = loss
            .clone()
            .into_data()
            .into_vec::<f32>()
            .map_err(|e| e.to_string())?[0] as f64;
        if !scalar.is_finite() || scalar < 0.0 || scalar > 1_000_000.0 {
            return Err("sequence loss exceeded finite bound".into());
        }
        let mut gradients = GradientsParams::from_grads(loss.backward(), &model);
        let norm = control_gradients(&model, &mut gradients)?;
        model = optimizer.step(request.learning_rate, model, gradients);
        diagnostics.batch_losses.push(scalar);
        diagnostics.raw_gradient_l2.push(norm);
        diagnostics.examples_seen += batch.len() as u64;
        diagnostics.first_training_decision_ms = diagnostics
            .first_training_decision_ms
            .min(batch[0].observed_at_ms);
        diagnostics.last_training_decision_ms = diagnostics
            .last_training_decision_ms
            .max(batch.last().expect("nonempty batch").observed_at_ms);
        diagnostics.completed_updates += 1;
    }
    reader.finish_pass()?;
    let trained = TrainedSequenceModel {
        model: model.valid(),
        request,
        scaling,
        diagnostics,
    };
    // A finite loss before the final update does not certify the resulting weights.
    trained.verify_parameters()?;
    Ok(trained)
}

impl TrainedSequenceModel {
    /// Refits compare fitted values rather than container metadata or Param IDs.
    pub fn parameter_digest(&self) -> Result<String, String> {
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        struct Visitor {
            digest: Sha256,
            index: u64,
            error: Option<String>,
        }
        impl ModuleVisitor<CpuBackend> for Visitor {
            fn visit_float<const D: usize>(&mut self, param: &Param<Tensor<CpuBackend, D>>) {
                let tensor = param.val();
                self.digest.update(self.index.to_le_bytes());
                self.digest.update((D as u64).to_le_bytes());
                for dimension in tensor.dims() {
                    self.digest.update((dimension as u64).to_le_bytes());
                }
                match tensor.into_data().into_vec::<f32>() {
                    Ok(values) => {
                        for value in values {
                            if !value.is_finite() {
                                self.error = Some("non-finite sequence parameter".into());
                            }
                            self.digest.update(value.to_bits().to_le_bytes());
                        }
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
                self.index += 1;
            }
        }
        let mut visitor = Visitor {
            digest: Sha256::new(),
            index: 0,
            error: None,
        };
        visitor
            .digest
            .update(b"monday.sequence-neural-parameters.v1");
        visitor.digest.update(
            serde_json::to_vec(&(
                self.request.model_kind,
                self.request.channels.len(),
                self.request.hidden_channels,
                self.request.input.context_rows,
            ))
            .map_err(|e| e.to_string())?,
        );
        self.model.visit(&mut visitor);
        if let Some(error) = visitor.error {
            return Err(error);
        }
        Ok(format!("{:x}", visitor.digest.finalize()))
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
    fn verify_parameters(&self) -> Result<(), String> {
        struct Finite {
            error: bool,
        }
        impl ModuleVisitor<CpuBackend> for Finite {
            fn visit_float<const D: usize>(&mut self, param: &Param<Tensor<CpuBackend, D>>) {
                self.error |= param
                    .val()
                    .into_data()
                    .into_vec::<f32>()
                    .map_or(true, |values| values.iter().any(|v| !v.is_finite()));
            }
        }
        let mut check = Finite { error: false };
        self.model.visit(&mut check);
        if check.error {
            Err("sequence parameters are non-finite".into())
        } else {
            Ok(())
        }
    }

    /// Labels are intentionally absent from the inference API.
    pub fn predict(&self, inputs: &[f32]) -> Result<[f32; 3], String> {
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        let example = SequenceExample {
            observed_at_ms: 0,
            spread_bps: 0.0,
            inputs: inputs.to_vec(),
            targets: [0.0; 3],
        };
        let values = input_values(&[example], &self.request, &self.scaling)?;
        let x = Tensor::from_data(
            TensorData::new(
                values,
                [
                    1,
                    self.request.channels.len(),
                    self.request.input.context_rows,
                ],
            ),
            &NdArrayDevice::Cpu,
        );
        let normalized = self
            .model
            .forward(x)
            .into_data()
            .into_vec::<f32>()
            .map_err(|e| e.to_string())?;
        let raw = std::array::from_fn(|i| {
            (f64::from(normalized[i]) * self.scaling.target_scales[i]
                + self.scaling.target_means[i]) as f32
        });
        if raw.iter().any(|v| !v.is_finite()) {
            return Err("sequence raw-return prediction is non-finite".into());
        }
        Ok(raw)
    }

    pub fn weights(&self) -> Result<Vec<u8>, String> {
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        self.verify_parameters()?;
        let mut store = BurnpackStore::from_bytes(None);
        self.model
            .save_into(&mut store)
            .map_err(|e| e.to_string())?;
        Ok(store.get_bytes().map_err(|e| e.to_string())?.to_vec())
    }

    /// Returns (manifest bytes, Burnpack bytes). The manifest hash must be pinned
    /// by the caller's Campaign result, rather than supplied by this same file.
    pub fn bundle(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
        let weights = self.weights()?;
        let manifest = SequenceBundleV1 {
            schema_version: "monday.sequence_neural_bundle.v1".into(),
            weights_sha256: format!("{:x}", Sha256::digest(&weights)),
            request: self.request.clone(),
            scaling: self.scaling.clone(),
            diagnostics: self.diagnostics.clone(),
        };
        Ok((
            serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
            weights,
        ))
    }

    pub fn restore_bundle(
        manifest: &[u8],
        expected_manifest_sha256: &str,
        weights: Vec<u8>,
    ) -> Result<Self, String> {
        if manifest.len() > 2 * 1024 * 1024
            || !valid_sha256(expected_manifest_sha256)
            || format!("{:x}", Sha256::digest(manifest)) != expected_manifest_sha256
        {
            return Err("sequence model manifest checksum mismatch".into());
        }
        let bundle: SequenceBundleV1 =
            serde_json::from_slice(manifest).map_err(|e| e.to_string())?;
        if bundle.schema_version != "monday.sequence_neural_bundle.v1" {
            return Err("unsupported sequence model bundle".into());
        }
        Self::restore(
            weights,
            &bundle.weights_sha256,
            bundle.request,
            bundle.scaling,
            bundle.diagnostics,
        )
    }

    fn restore(
        weights: Vec<u8>,
        expected_sha256: &str,
        request: SequenceTrainingRequestV1,
        scaling: SequenceScalingV1,
        diagnostics: SequenceTrainingDiagnosticsV1,
    ) -> Result<Self, String> {
        request.validate()?;
        if weights.len() > 16 * 1024 * 1024
            || !valid_sha256(expected_sha256)
            || format!("{:x}", Sha256::digest(&weights)) != expected_sha256
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
        let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
        let mut model = SequenceNetwork::<CpuBackend>::new(&request, &NdArrayDevice::Cpu);
        let mut store =
            BurnpackStore::from_bytes(Some(burn::tensor::Bytes::from_bytes_vec(weights)));
        let applied = model.load_from(&mut store).map_err(|e| e.to_string())?;
        if !applied.is_success()
            || !applied.missing.is_empty()
            || !applied.unused.is_empty()
            || applied.applied.is_empty()
        {
            return Err("sequence weight load is incomplete".into());
        }
        let result = Self {
            model,
            request,
            scaling,
            diagnostics,
        };
        result.verify_parameters()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causal_tcn_uses_delayed_context_instead_of_only_the_last_row() {
        let _guard = lock_ndarray_backend().unwrap();
        let device = NdArrayDevice::Cpu;
        let mut model = CausalTcn::<CpuBackend>::new(1, 2, &device);
        for (index, conv) in model.convolutions.iter_mut().enumerate() {
            let shape = conv.weight.val().dims();
            let mut values = vec![0.0; shape.iter().product()];
            values[if index == 0 { 0 } else { 2 }] = 1.0;
            conv.weight =
                Param::from_tensor(Tensor::from_data(TensorData::new(values, shape), &device));
            conv.bias = Some(Param::from_tensor(Tensor::zeros([2], &device)));
        }
        model.output.weight = Param::from_tensor(Tensor::from_data(
            TensorData::new(vec![1.0, 2.0, 3.0, 0.0, 0.0, 0.0], [2, 3]),
            &device,
        ));
        model.output.bias = Some(Param::from_tensor(Tensor::zeros([3], &device)));
        let output = |values| {
            model
                .forward(Tensor::from_data(
                    TensorData::new(values, [1, 1, 3]),
                    &device,
                ))
                .into_data()
                .into_vec::<f32>()
                .unwrap()
        };
        assert_eq!(output(vec![10.0, 20.0, 30.0]), [10.0, 20.0, 30.0]);
        assert_eq!(output(vec![11.0, 20.0, 30.0]), [11.0, 22.0, 33.0]);
        assert_eq!(output(vec![10.0, 20.0, 999.0]), [10.0, 20.0, 30.0]);
    }
}
