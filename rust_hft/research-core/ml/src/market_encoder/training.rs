use super::{
    artifacts::*,
    data::{
        fit_market_scaling, MarketFeatureReader, MarketTaskReader, Moments,
        UnlabeledSequenceExample,
    },
    network::*,
};
use crate::{lock_ndarray_backend, CpuAutodiffBackend};
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
    let encoder = model.encoder.valid();
    diag.final_encoder_values_sha256 = values_digest(&encoder)?;
    diag.validate(&request)?;
    let weights = save(&encoder)?;
    let head = model.head.valid();
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
            schema_version: "monday.market_reconstruction_audit.v1".into(),
            encoder_checkpoint_sha256: checkpoint.identity()?,
            mask_policy: MASK_POLICY.into(),
            weights_sha256: bytes_digest(&head_weights),
            parameter_values_sha256: head_values,
        },
        weights: head_weights,
    });
    Ok(checkpoint)
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
