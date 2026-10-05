use crate::{CpuAutodiffBackend, CpuBackend};
use burn::{
    module::{Module, ModuleVisitor, Param},
    nn::{
        conv::{Conv1d, Conv1dConfig},
        Linear, LinearConfig,
    },
    optim::GradientsParams,
    tensor::{activation::relu, backend::Backend, Tensor},
};
use hft_research_manifest::market_encoder::MarketEncoderSpecV1;
use sha2::{Digest, Sha256};

#[derive(Module, Debug)]
pub(super) struct Encoder<B: Backend> {
    convolutions: Vec<Conv1d<B>>,
}
impl<B: Backend> Encoder<B> {
    pub(super) fn new(spec: &MarketEncoderSpecV1, device: &B::Device) -> Self {
        Self {
            convolutions: [1, 2, 4, 8, 16]
                .into_iter()
                .enumerate()
                .map(|(i, d)| {
                    Conv1dConfig::new(
                        if i == 0 {
                            spec.input.ordered_channels.len() + 1
                        } else {
                            spec.hidden_channels
                        },
                        spec.hidden_channels,
                        3,
                    )
                    .with_dilation(d)
                    .init(device)
                })
                .collect(),
        }
    }
    pub(super) fn forward(&self, mut x: Tensor<B, 3>) -> Tensor<B, 3> {
        for conv in &self.convolutions {
            let [batch, channels, _] = x.dims();
            let padding = Tensor::zeros([batch, channels, 2 * conv.dilation], &x.device());
            x = relu(conv.forward(Tensor::cat(vec![padding, x], 2)));
        }
        x
    }
}
#[derive(Module, Debug)]
pub(super) struct Reconstruction<B: Backend> {
    pub encoder: Encoder<B>,
    pub head: Linear<B>,
}
impl<B: Backend> Reconstruction<B> {
    pub(super) fn new(spec: &MarketEncoderSpecV1, device: &B::Device) -> Self {
        Self {
            encoder: Encoder::new(spec, device),
            head: LinearConfig::new(spec.hidden_channels, spec.input.ordered_channels.len())
                .init(device),
        }
    }
    pub(super) fn forward(&self, x: Tensor<B, 3>) -> Tensor<B, 3> {
        self.head.forward(self.encoder.forward(x).swap_dims(1, 2))
    }
}
#[derive(Module, Debug)]
pub(super) struct TaskNetwork<B: Backend> {
    pub encoder: Encoder<B>,
    pub head: Linear<B>,
}
impl<B: Backend> TaskNetwork<B> {
    pub(super) fn forward(&self, x: Tensor<B, 3>, head_only: bool) -> Tensor<B, 2> {
        let mut h = self.encoder.forward(x);
        if head_only {
            h = h.detach();
        }
        let [batch, channels, length] = h.dims();
        self.head.forward(
            h.slice([0..batch, 0..channels, length - 1..length])
                .reshape([batch, channels]),
        )
    }
}

pub(super) fn values_digest<M: Module<CpuBackend>>(model: &M) -> Result<String, String> {
    struct Visitor {
        hash: Sha256,
        error: Option<String>,
        index: u64,
    }
    impl ModuleVisitor<CpuBackend> for Visitor {
        fn visit_float<const D: usize>(&mut self, p: &Param<Tensor<CpuBackend, D>>) {
            self.hash.update(self.index.to_le_bytes());
            self.index += 1;
            let x = p.val();
            self.hash.update((D as u64).to_le_bytes());
            for dim in x.dims() {
                self.hash.update((dim as u64).to_le_bytes());
            }
            match x.into_data().into_vec::<f32>() {
                Ok(values) => {
                    for v in values {
                        if !v.is_finite() {
                            self.error = Some("nonfinite market parameter".into());
                        }
                        self.hash.update(v.to_bits().to_le_bytes());
                    }
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
    }
    let mut visitor = Visitor {
        hash: Sha256::new(),
        error: None,
        index: 0,
    };
    visitor.hash.update(b"monday.market-parameter-values.v1");
    model.visit(&mut visitor);
    if let Some(e) = visitor.error {
        return Err(e);
    }
    Ok(format!("{:x}", visitor.hash.finalize()))
}

pub(super) fn clip<M: Module<CpuAutodiffBackend>>(
    model: &M,
    gradients: &mut GradientsParams,
) -> Result<f64, String> {
    struct Visitor<'a> {
        grads: &'a mut GradientsParams,
        norm: f64,
        count: usize,
        scale: f32,
        error: Option<String>,
    }
    impl ModuleVisitor<CpuAutodiffBackend> for Visitor<'_> {
        fn visit_float<const D: usize>(&mut self, p: &Param<Tensor<CpuAutodiffBackend, D>>) {
            let Some(g) = self.grads.get::<CpuBackend, D>(p.id) else {
                self.error = Some("missing trainable market gradient".into());
                return;
            };
            match g.clone().into_data().into_vec::<f32>() {
                Ok(values) => {
                    for v in values {
                        if !v.is_finite() {
                            self.error = Some("nonfinite market gradient".into());
                        }
                        self.norm = self.norm.hypot(f64::from(v));
                    }
                }
                Err(e) => self.error = Some(e.to_string()),
            }
            if self.scale != 1.0 {
                self.grads.register(p.id, g * self.scale);
            }
            self.count += 1;
        }
    }
    let mut visitor = Visitor {
        grads: gradients,
        norm: 0.0,
        count: 0,
        scale: 1.0,
        error: None,
    };
    model.visit(&mut visitor);
    if let Some(e) = visitor.error {
        return Err(e);
    }
    if visitor.count != visitor.grads.len() || !visitor.norm.is_finite() || visitor.norm > 100.0 {
        return Err("market gradient exceeded bound".into());
    }
    let norm = visitor.norm;
    if norm > 1.0 {
        visitor.scale = (1.0 / norm) as f32;
        model.visit(&mut visitor);
        if let Some(e) = visitor.error {
            return Err(e);
        }
        let mut check = Visitor {
            grads: visitor.grads,
            norm: 0.0,
            count: 0,
            scale: 1.0,
            error: None,
        };
        model.visit(&mut check);
        if check.error.is_some() || (check.norm - 1.0).abs() > 8.0 * f64::from(f32::EPSILON) {
            return Err("market gradient clipping failed".into());
        }
    }
    Ok(norm)
}

impl Encoder<CpuBackend> {
    pub(super) fn portable(
        &self,
    ) -> Result<hft_research_manifest::portable_network::FrozenNetworkV1, String> {
        Ok(
            hft_research_manifest::portable_network::FrozenNetworkV1::Tcn {
                convolutions: self
                    .convolutions
                    .iter()
                    .map(crate::portable::convolution)
                    .collect::<Result<_, _>>()?,
                output: None,
            },
        )
    }
}
impl TaskNetwork<CpuBackend> {
    pub(super) fn portable(
        &self,
    ) -> Result<hft_research_manifest::portable_network::FrozenNetworkV1, String> {
        Ok(
            hft_research_manifest::portable_network::FrozenNetworkV1::Tcn {
                convolutions: self
                    .encoder
                    .convolutions
                    .iter()
                    .map(crate::portable::convolution)
                    .collect::<Result<_, _>>()?,
                output: Some(crate::portable::linear(&self.head)?),
            },
        )
    }
    pub(super) fn from_portable(
        frozen: &hft_research_manifest::portable_network::FrozenNetworkV1,
    ) -> Result<Self, String> {
        match frozen {
            hft_research_manifest::portable_network::FrozenNetworkV1::Tcn {
                convolutions,
                output: Some(head),
            } => Ok(Self {
                encoder: Encoder {
                    convolutions: convolutions
                        .iter()
                        .map(crate::portable::restore_convolution)
                        .collect(),
                },
                head: crate::portable::restore_linear(head),
            }),
            _ => Err("task portable shape changed".into()),
        }
    }
}

impl<B: Backend<Device = burn_ndarray::NdArrayDevice>> Encoder<B> {
    pub(super) fn from_portable(
        frozen: &hft_research_manifest::portable_network::FrozenNetworkV1,
    ) -> Result<Self, String> {
        match frozen {
            hft_research_manifest::portable_network::FrozenNetworkV1::Tcn {
                convolutions,
                output: None,
            } => Ok(Self {
                convolutions: convolutions
                    .iter()
                    .map(crate::portable::restore_convolution)
                    .collect(),
            }),
            _ => Err("encoder portable shape changed".into()),
        }
    }
}
