use super::{
    artifacts::*,
    data::{
        fit_market_scaling, MarketFeatureReader, MarketTaskReader, Moments,
        UnlabeledSequenceExample,
    },
    network::*,
};
use crate::{lock_ndarray_backend, CpuAutodiffBackend, CpuBackend};
use burn::{
    module::AutodiffModule,
    nn::{
        loss::{MseLoss, Reduction},
        LinearConfig,
    },
    optim::{AdamConfig, GradientsParams, Optimizer},
    tensor::{backend::Backend, Tensor, TensorData},
};
use burn_ndarray::NdArrayDevice;
use hft_research_manifest::market_encoder::*;
use sha2::{Digest, Sha256};
use std::time::Instant;

fn diagnostics(initial: String) -> MarketFitDiagnosticsV1 {
    MarketFitDiagnosticsV1 {
        updates: 0,
        example_visits: 0,
        losses: vec![],
        gradient_norms: vec![],
        initial_encoder_values_sha256: initial,
        final_encoder_values_sha256: String::new(),
    }
}
fn scalar(loss: &Tensor<CpuAutodiffBackend, 1>) -> Result<f64, String> {
    let value = f64::from(
        loss.clone()
            .into_data()
            .into_vec::<f32>()
            .map_err(|e| e.to_string())?[0],
    );
    if !value.is_finite() || !(0.0..=1e6).contains(&value) {
        return Err("market loss exceeded bound".into());
    }
    Ok(value)
}
pub(super) fn masked_positions(
    seed: u64,
    epoch: u64,
    item: &UnlabeledSequenceExample,
) -> [bool; 60] {
    let mut ranked = (2_u64..20)
        .map(|block| {
            let mut hash = Sha256::new();
            for bytes in [
                seed.to_le_bytes(),
                epoch.to_le_bytes(),
                item.series_id.to_le_bytes(),
                item.observed_at_ms.to_le_bytes(),
                block.to_le_bytes(),
            ] {
                hash.update(bytes);
            }
            (hash.finalize().to_vec(), block as usize)
        })
        .collect::<Vec<_>>();
    ranked.sort();
    let mut mask = [false; 60];
    for (_, block) in ranked.into_iter().take(6) {
        mask[block * 3..block * 3 + 3].fill(true);
    }
    mask
}
pub(super) fn input_tensor<B: Backend<Device = NdArrayDevice>>(
    batch: &[UnlabeledSequenceExample],
    request: &MarketFitRequestV1,
    scaling: &MarketFeatureScalingV1,
    epoch: Option<u64>,
) -> Result<Tensor<B, 3>, String> {
    let n = request.spec.input.ordered_channels.len();
    let t = request.spec.input.context_rows;
    let mut values = Vec::with_capacity(batch.len() * (n + 1) * t);
    for item in batch {
        if item.inputs.len() != n * t {
            return Err("market input shape differs".into());
        }
        let mask = epoch
            .map(|e| masked_positions(request.seed, e, item))
            .unwrap_or([false; 60]);
        for c in 0..n {
            for (time, masked) in mask.iter().enumerate() {
                let raw = item.inputs[time * n + c];
                let value = ((f64::from(raw) - scaling.means[c]) / scaling.scales[c]) as f32;
                if !raw.is_finite() || !value.is_finite() {
                    return Err("nonfinite normalized market input".into());
                }
                values.push(if *masked { 0.0 } else { value });
            }
        }
        values.extend(mask.map(|m| if m { 1.0 } else { 0.0 }));
    }
    Ok(Tensor::from_data(
        TensorData::new(values, [batch.len(), n + 1, t]),
        &NdArrayDevice::Cpu,
    ))
}
pub fn pretrain_market_encoder(
    reader: &mut MarketFeatureReader,
    request: MarketFitRequestV1,
) -> Result<MarketEncoderCheckpoint, String> {
    request.validate()?;
    if reader.request() != &request.read_request() || !reader.is_at_start() {
        return Err("pretraining reader differs from request".into());
    }
    let scaling = fit_market_scaling(reader, request.min_examples, request.max_examples)?;
    let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
    let device = NdArrayDevice::Cpu;
    CpuAutodiffBackend::seed(&device, request.seed);
    let mut model = Reconstruction::<CpuAutodiffBackend>::new(&request.spec, &device);
    let mut diag = diagnostics(values_digest(&model.encoder.valid())?);
    let mut optimizer = AdamConfig::new().init();
    let mut epoch = 0;
    let mut seen = 0_u64;
    while diag.updates < request.updates {
        let batch = reader.next_batch(request.batch_size)?;
        if batch.is_empty() {
            if seen != scaling.examples {
                return Err("pretraining anchor coverage changed".into());
            }
            reader.rewind()?;
            epoch += 1;
            seen = 0;
            continue;
        }
        seen += batch.len() as u64;
        if seen > scaling.examples {
            return Err("pretraining exceeded anchors".into());
        }
        let x = input_tensor::<CpuAutodiffBackend>(&batch, &request, &scaling, Some(epoch))?;
        let n = request.spec.input.ordered_channels.len();
        let mut targets = Vec::new();
        let mut masks = Vec::new();
        for item in &batch {
            let mask = masked_positions(request.seed, epoch, item);
            for (time, masked) in mask.into_iter().enumerate() {
                for c in 0..n {
                    targets.push(
                        ((f64::from(item.inputs[time * n + c]) - scaling.means[c])
                            / scaling.scales[c]) as f32,
                    );
                    masks.push(if masked { 1.0 } else { 0.0 });
                }
            }
        }
        let shape = [batch.len(), 60, n];
        let y = Tensor::from_data(TensorData::new(targets, shape), &device);
        let m = Tensor::from_data(TensorData::new(masks, shape), &device);
        let error = model.forward(x) - y;
        let loss = (error.clone() * error * m).sum() / (batch.len() * 18 * n) as f32;
        let value = scalar(&loss)?;
        let mut gradients = GradientsParams::from_grads(loss.backward(), &model);
        let norm = clip(&model, &mut gradients)?;
        model = optimizer.step(request.learning_rate, model, gradients);
        diag.updates += 1;
        diag.example_visits += batch.len() as u64;
        diag.losses.push(value);
        diag.gradient_norms.push(norm);
    }
    reader.finish_pass()?;
    let trained = model.valid();
    let (reconstruction_diagnostics, diagnostic_elapsed_micros) =
        if reconstruction_channel_groups(&request.spec.input).is_some() {
            let started = Instant::now();
            let report = reconstruction_diagnostics(reader, &request, &scaling, &trained)?;
            (
                Some(report),
                Some(
                    u64::try_from(started.elapsed().as_micros())
                        .map_err(|_| "reconstruction diagnostic clock overflow")?,
                ),
            )
        } else {
            (None, None)
        };
    let encoder = trained.encoder;
    diag.final_encoder_values_sha256 = values_digest(&encoder)?;
    diag.validate(&request)?;
    let weights = save(&encoder)?;
    let head = trained.head;
    let head_weights = save(&head)?;
    let head_values = values_digest(&head)?;
    let mut checkpoint = MarketEncoderCheckpoint {
        model: encoder,
        weights: weights.clone(),
        reconstruction: None,
        manifest: EncoderManifest {
            schema_version: ENCODER_SCHEMA.into(),
            request,
            mask_policy: MASK_POLICY.into(),
            scaling,
            diagnostics: diag,
            weights_sha256: bytes_digest(&weights),
        },
    };
    checkpoint.reconstruction = Some(ReconstructionAudit {
        metadata: ReconstructionAuditManifest {
            schema_version: RECONSTRUCTION_AUDIT_SCHEMA.into(),
            encoder_checkpoint_sha256: checkpoint.identity()?,
            mask_policy: MASK_POLICY.into(),
            weights_sha256: bytes_digest(&head_weights),
            parameter_values_sha256: head_values,
            diagnostics: reconstruction_diagnostics,
            diagnostic_elapsed_micros,
        },
        weights: head_weights,
    });
    Ok(checkpoint)
}
#[derive(Default)]
pub(super) struct ReconstructionMse {
    count: u64,
    model_squared_error: f64,
    baseline_squared_error: f64,
}
impl ReconstructionMse {
    pub(super) fn finish(
        &self,
        group: &str,
        channels: usize,
    ) -> Result<ReconstructionGroupMse, String> {
        if self.count == 0
            || !self.model_squared_error.is_finite()
            || !self.baseline_squared_error.is_finite()
        {
            return Err("invalid reconstruction diagnostic accumulation".into());
        }
        Ok(ReconstructionGroupMse {
            group: group.into(),
            channels: channels as u64,
            masked_scalar_count: self.count,
            model_mse: self.model_squared_error / self.count as f64,
            last_visible_mse: self.baseline_squared_error / self.count as f64,
        })
    }
}
/// Output is [time,channel], matching Reconstruction::forward's [batch,time,channel].
/// The baseline holds the last UNMASKED frame across an entire adjacent masked run.
pub(super) fn observe_reconstruction(
    item: &UnlabeledSequenceExample,
    mask: &[bool; 60],
    predicted: &[f32],
    scaling: &MarketFeatureScalingV1,
    groups: &[Vec<usize>; 3],
    sums: &mut [ReconstructionMse; 3],
) -> Result<(), String> {
    if item.inputs.len() != 60 * 24
        || predicted.len() != 60 * 24
        || scaling.means.len() != 24
        || scaling.scales.len() != 24
        || mask[..6].iter().any(|v| *v)
        || mask.iter().filter(|v| **v).count() != 18
        || predicted.iter().any(|v| !v.is_finite())
    {
        return Err("reconstruction diagnostic tensor shape, mask or value differs".into());
    }
    let normalized = |time: usize, channel: usize| -> Result<f64, String> {
        let raw = item.inputs[time * 24 + channel];
        let value = ((f64::from(raw) - scaling.means[channel]) / scaling.scales[channel]) as f32;
        if !raw.is_finite() || !value.is_finite() {
            return Err("nonfinite reconstruction target".into());
        }
        Ok(f64::from(value))
    };
    let mut last_visible = 0;
    for (time, masked) in mask.iter().enumerate() {
        if !masked {
            last_visible = time;
            continue;
        }
        for (channels, sum) in groups.iter().zip(sums.iter_mut()) {
            for &channel in channels {
                let observed = normalized(time, channel)?;
                let error = f64::from(predicted[time * 24 + channel]) - observed;
                let baseline = normalized(last_visible, channel)? - observed;
                sum.count += 1;
                sum.model_squared_error += error * error;
                sum.baseline_squared_error += baseline * baseline;
            }
        }
    }
    Ok(())
}
fn diagnose_batch(
    model: &Reconstruction<CpuBackend>,
    batch: &[UnlabeledSequenceExample],
    request: &MarketFitRequestV1,
    scaling: &MarketFeatureScalingV1,
    groups: &[Vec<usize>; 3],
    sums: &mut [ReconstructionMse; 3],
) -> Result<(), String> {
    let output = model.forward(input_tensor::<CpuBackend>(
        batch,
        request,
        scaling,
        Some(0),
    )?);
    if output.dims() != [batch.len(), 60, 24] {
        return Err("reconstruction output is not batch/time/channel".into());
    }
    let values = output
        .into_data()
        .into_vec::<f32>()
        .map_err(|e| e.to_string())?;
    for (item, predicted) in batch.iter().zip(values.as_chunks::<{ 60 * 24 }>().0) {
        observe_reconstruction(
            item,
            &masked_positions(request.seed, 0, item),
            predicted,
            scaling,
            groups,
            sums,
        )?;
    }
    Ok(())
}
fn reconstruction_diagnostics(
    reader: &mut MarketFeatureReader,
    request: &MarketFitRequestV1,
    scaling: &MarketFeatureScalingV1,
    model: &Reconstruction<CpuBackend>,
) -> Result<ReconstructionDiagnostics, String> {
    let groups = reconstruction_channel_groups(&request.spec.input)
        .ok_or("unverified reconstruction channel registry")?;
    let selected = diagnostic_ordinals(scaling.examples);
    let mut selection = 0;
    let mut seen = 0;
    let mut batches = 0;
    let mut pending = Vec::with_capacity(request.batch_size);
    let mut sums: [ReconstructionMse; 3] = Default::default();
    let mut identities = Sha256::new();
    identities.update(b"monday.reconstruction-diagnostic-anchor-keys.v1");
    reader.rewind()?;
    loop {
        let batch = reader.next_batch(DIAGNOSTIC_SAMPLE_LIMIT)?;
        if batch.is_empty() {
            break;
        }
        for item in batch {
            if seen >= scaling.examples {
                return Err("diagnostic training anchor coverage increased".into());
            }
            if selected.get(selection) == Some(&seen) {
                identities.update(seen.to_le_bytes());
                identities.update(item.series_id.to_le_bytes());
                identities.update(item.observed_at_ms.to_le_bytes());
                pending.push(item);
                selection += 1;
                if pending.len() == request.batch_size {
                    diagnose_batch(model, &pending, request, scaling, &groups, &mut sums)?;
                    batches += 1;
                    pending.clear();
                }
            }
            seen += 1;
        }
    }
    reader.finish_pass()?;
    if seen != scaling.examples || selection != selected.len() {
        return Err("diagnostic training anchor coverage changed".into());
    }
    if !pending.is_empty() {
        diagnose_batch(model, &pending, request, scaling, &groups, &mut sums)?;
        batches += 1;
    }
    let count = selection as u64;
    let report = ReconstructionDiagnostics {
        schema_version: "monday.market_reconstruction_diagnostics.v1".into(),
        sampling_policy: DIAGNOSTIC_SAMPLING.into(),
        grouping_policy: DIAGNOSTIC_GROUPING.into(),
        metric_space: "train-channel-standardized-f32".into(),
        mask_epoch: 0,
        eligible_training_anchors: seen,
        scanned_training_anchors: seen,
        sampled_ordinals: selected,
        sampled_anchor_keys_sha256: format!("{:x}", identities.finalize()),
        additional_feature_passes: 1,
        forward_batch_size: request.batch_size,
        cpu_forward_batches: batches,
        cpu_forward_examples: count,
        cpu_forward_frames: count * 60,
        cpu_output_scalars: count * 60 * 24,
        additional_optimizer_updates: 0,
        groups: sums
            .iter()
            .zip(&groups)
            .zip(["price", "depth", "trade"])
            .map(|((sum, indices), name)| sum.finish(name, indices.len()))
            .collect::<Result<_, _>>()?,
    };
    report.validate(request, scaling)?;
    Ok(report)
}

pub fn adapt_market_encoder(
    reader: &mut MarketTaskReader,
    request: MarketAdaptationRequestV1,
    parent: Option<&MarketEncoderCheckpoint>,
) -> Result<MarketTaskModel, String> {
    request.validate()?;
    check_parent(&request, parent)?;
    if reader.features.request() != &request.fit.read_request()
        || !reader.features.is_at_start()
        || reader.target_digest() != request.target_dataset_sha256
    {
        return Err("adaptation reader differs from request".into());
    }
    let scaling = fit_market_scaling(
        &mut reader.features,
        request.fit.min_examples,
        request.fit.max_examples,
    )?;
    if parent.is_some_and(|p| p.scaling() != &scaling) {
        return Err("adaptation changed its parent normalization".into());
    }
    let mut targets = Moments::default();
    let mut count = 0_u64;
    loop {
        let batch = reader.next_batch(request.fit.batch_size)?;
        if batch.is_empty() {
            break;
        }
        for row in batch {
            targets.push(f64::from(row.target.simple_return));
            count += 1;
        }
    }
    reader.finish_pass()?;
    reader.rewind()?;
    if count != scaling.examples {
        return Err("supervised anchors differ from pretraining anchors".into());
    }
    let target_mean = targets.mean();
    let target_scale = targets.scale();
    let _guard = lock_ndarray_backend().map_err(|e| e.to_string())?;
    let device = NdArrayDevice::Cpu;
    CpuAutodiffBackend::seed(&device, request.fit.seed);
    let mut encoder = Encoder::<CpuAutodiffBackend>::new(&request.fit.spec, &device);
    if let Some(p) = parent {
        load(&mut encoder, p.weights.clone())?;
    }
    let initial = values_digest(&encoder.valid())?;
    if parent.is_some_and(|p| p.diagnostics().final_encoder_values_sha256 != initial) {
        return Err("initial adaptation weights did not inherit parent".into());
    }
    CpuAutodiffBackend::seed(&device, request.head_seed);
    let mut model = TaskNetwork {
        encoder,
        head: LinearConfig::new(request.fit.spec.hidden_channels, 1).init(&device),
    };
    let mut optimizer = AdamConfig::new().init();
    let mut diag = diagnostics(initial);
    let mut seen = 0_u64;
    while diag.updates < request.fit.updates {
        let batch = reader.next_batch(request.fit.batch_size)?;
        if batch.is_empty() {
            if seen != scaling.examples {
                return Err("adaptation coverage changed".into());
            }
            reader.finish_pass()?;
            reader.rewind()?;
            seen = 0;
            continue;
        }
        seen += batch.len() as u64;
        if seen > scaling.examples {
            return Err("adaptation exceeded anchors".into());
        }
        let features = batch.iter().map(|v| v.features.clone()).collect::<Vec<_>>();
        let labels = batch
            .iter()
            .map(|v| ((f64::from(v.target.simple_return) - target_mean) / target_scale) as f32)
            .collect::<Vec<_>>();
        if labels.iter().any(|v| !v.is_finite()) {
            return Err("nonfinite normalized market target".into());
        }
        let x = input_tensor::<CpuAutodiffBackend>(&features, &request.fit, &scaling, None)?;
        let y = Tensor::from_data(TensorData::new(labels, [batch.len(), 1]), &device);
        let head_only = diag.updates < request.head_only_updates();
        let loss = MseLoss::new().forward(model.forward(x, head_only), y, Reduction::Mean);
        let value = scalar(&loss)?;
        let mut gradients = GradientsParams::from_grads(loss.backward(), &model);
        let norm = if head_only {
            clip(&model.head, &mut gradients)?
        } else {
            clip(&model, &mut gradients)?
        };
        model = optimizer.step(request.fit.learning_rate, model, gradients);
        diag.updates += 1;
        diag.example_visits += batch.len() as u64;
        diag.losses.push(value);
        diag.gradient_norms.push(norm);
    }
    reader.finish_pass()?;
    let model = model.valid();
    diag.final_encoder_values_sha256 = values_digest(&model.encoder)?;
    if request.mode == AdaptationModeV1::LinearProbe
        && diag.final_encoder_values_sha256 != diag.initial_encoder_values_sha256
    {
        return Err("linear probe modified encoder parameters".into());
    }
    if request.mode == AdaptationModeV1::FullFineTune
        && diag.final_encoder_values_sha256 == diag.initial_encoder_values_sha256
    {
        return Err("fine-tuning completed without updating the encoder".into());
    }
    diag.validate(&request.fit)?;
    let parameter_values_sha256 = values_digest(&model)?;
    let weights = save(&model)?;
    Ok(MarketTaskModel {
        model,
        weights: weights.clone(),
        manifest: TaskManifest {
            schema_version: TASK_SCHEMA.into(),
            request,
            scaling,
            target_mean,
            target_scale,
            diagnostics: diag,
            weights_sha256: bytes_digest(&weights),
            parameter_values_sha256,
        },
    })
}
