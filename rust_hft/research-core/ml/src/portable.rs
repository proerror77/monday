//! Exact trained parameters at the frozen-inference seam. Optimizers stay in the trainers.
use crate::CpuBackend;
use burn::{
    module::Param,
    nn::{
        conv::{Conv1d, Conv1dConfig},
        Linear, LinearConfig,
    },
    tensor::{backend::Backend, Tensor, TensorData},
};
use burn_ndarray::NdArrayDevice;
use hft_research_manifest::portable_network::{FrozenCausalConvV1, FrozenLinearV1};

pub(crate) fn linear(model: &Linear<CpuBackend>) -> Result<FrozenLinearV1, String> {
    let [inputs, outputs] = model.weight.val().dims();
    let frozen = FrozenLinearV1 {
        inputs,
        outputs,
        weight: model
            .weight
            .val()
            .into_data()
            .into_vec::<f32>()
            .map_err(|e| e.to_string())?,
        bias: model
            .bias
            .as_ref()
            .ok_or("frozen linear bias missing")?
            .val()
            .into_data()
            .into_vec::<f32>()
            .map_err(|e| e.to_string())?,
    };
    frozen.validate(inputs, outputs)?;
    Ok(frozen)
}
pub(crate) fn convolution(model: &Conv1d<CpuBackend>) -> Result<FrozenCausalConvV1, String> {
    let [outputs, inputs, kernel] = model.weight.val().dims();
    if kernel != 3 || model.stride != 1 || model.groups != 1 {
        return Err("unsupported frozen causal convolution".into());
    }
    let frozen = FrozenCausalConvV1 {
        inputs,
        outputs,
        dilation: model.dilation,
        weight: model
            .weight
            .val()
            .into_data()
            .into_vec::<f32>()
            .map_err(|e| e.to_string())?,
        bias: model
            .bias
            .as_ref()
            .ok_or("frozen convolution bias missing")?
            .val()
            .into_data()
            .into_vec::<f32>()
            .map_err(|e| e.to_string())?,
    };
    frozen.validate(inputs, outputs, model.dilation)?;
    Ok(frozen)
}
pub(crate) fn restore_linear<B: Backend<Device = NdArrayDevice>>(
    frozen: &FrozenLinearV1,
) -> Linear<B> {
    let device = NdArrayDevice::Cpu;
    let mut model = LinearConfig::new(frozen.inputs, frozen.outputs).init(&device);
    model.weight = Param::from_tensor(Tensor::from_data(
        TensorData::new(frozen.weight.clone(), [frozen.inputs, frozen.outputs]),
        &device,
    ));
    model.bias = Some(Param::from_tensor(Tensor::from_data(
        TensorData::new(frozen.bias.clone(), [frozen.outputs]),
        &device,
    )));
    model
}
pub(crate) fn restore_convolution<B: Backend<Device = NdArrayDevice>>(
    frozen: &FrozenCausalConvV1,
) -> Conv1d<B> {
    let device = NdArrayDevice::Cpu;
    let mut model = Conv1dConfig::new(frozen.inputs, frozen.outputs, 3)
        .with_dilation(frozen.dilation)
        .init(&device);
    model.weight = Param::from_tensor(Tensor::from_data(
        TensorData::new(frozen.weight.clone(), [frozen.outputs, frozen.inputs, 3]),
        &device,
    ));
    model.bias = Some(Param::from_tensor(Tensor::from_data(
        TensorData::new(frozen.bias.clone(), [frozen.outputs]),
        &device,
    )));
    model
}
