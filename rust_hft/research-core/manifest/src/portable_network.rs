//! Frozen f32 inference. No optimizer, tensor backend or artifact loader.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenLinearV1 {
    pub inputs: usize,
    pub outputs: usize,
    /// Row-major [inputs, outputs], matching the trained linear module.
    pub weight: Vec<f32>,
    pub bias: Vec<f32>,
}
impl FrozenLinearV1 {
    pub fn parameter_digest(&self, prefix: &[u8]) -> String {
        let mut hash = Sha256::new();
        hash.update(prefix);
        for (index, shape, values) in [
            (0_u64, vec![self.inputs, self.outputs], &self.weight),
            (1, vec![self.outputs], &self.bias),
        ] {
            hash.update(index.to_le_bytes());
            hash.update((shape.len() as u64).to_le_bytes());
            for dimension in shape {
                hash.update((dimension as u64).to_le_bytes());
            }
            for value in values {
                hash.update(value.to_bits().to_le_bytes());
            }
        }
        format!("{:x}", hash.finalize())
    }
    pub fn validate(&self, inputs: usize, outputs: usize) -> Result<(), String> {
        if self.inputs != inputs
            || self.outputs != outputs
            || inputs == 0
            || outputs == 0
            || self.weight.len() != inputs.checked_mul(outputs).ok_or("linear shape overflow")?
            || self.bias.len() != outputs
            || self.weight.len() > 4_000_000
            || self.weight.iter().chain(&self.bias).any(|v| !v.is_finite())
        {
            return Err("invalid frozen linear shape or values".into());
        }
        Ok(())
    }
    fn forward(&self, x: &[f32]) -> Result<Vec<f32>, String> {
        self.validate(x.len(), self.outputs)?;
        let mut y = vec![0.0_f32; self.outputs];
        for (input, value) in x.iter().enumerate() {
            for (output, result) in y.iter_mut().enumerate() {
                *result += value * self.weight[input * self.outputs + output];
            }
        }
        for (result, bias) in y.iter_mut().zip(&self.bias) {
            *result += bias;
        }
        finite(&y)?;
        Ok(y)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenCausalConvV1 {
    pub inputs: usize,
    pub outputs: usize,
    pub dilation: usize,
    /// Row-major [outputs, inputs, 3]. Padding is exclusively on the left.
    pub weight: Vec<f32>,
    pub bias: Vec<f32>,
}
impl FrozenCausalConvV1 {
    pub fn validate(&self, inputs: usize, outputs: usize, dilation: usize) -> Result<(), String> {
        if self.inputs != inputs
            || self.outputs != outputs
            || self.dilation != dilation
            || inputs == 0
            || outputs == 0
            || self.weight.len() > 4_000_000
            || self.weight.len()
                != inputs
                    .checked_mul(outputs)
                    .and_then(|v| v.checked_mul(3))
                    .ok_or("convolution shape overflow")?
            || self.bias.len() != outputs
            || self.weight.iter().chain(&self.bias).any(|v| !v.is_finite())
        {
            return Err("invalid frozen causal convolution".into());
        }
        Ok(())
    }
    fn forward(&self, x: &[f32], context: usize) -> Result<Vec<f32>, String> {
        if x.len() != self.inputs * context {
            return Err("causal input shape changed".into());
        }
        let mut y = vec![0.0_f32; self.outputs * context];
        for output in 0..self.outputs {
            for time in 0..context {
                let mut value = 0.0_f32;
                for input in 0..self.inputs {
                    for kernel in 0..3 {
                        if let Some(source) = time.checked_sub((2 - kernel) * self.dilation) {
                            value += x[input * context + source]
                                * self.weight[(output * self.inputs + input) * 3 + kernel];
                        }
                    }
                }
                y[output * context + time] = (value + self.bias[output]).max(0.0);
            }
        }
        finite(&y)?;
        Ok(y)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FrozenNetworkV1 {
    Mlp {
        hidden: FrozenLinearV1,
        output: FrozenLinearV1,
    },
    Tcn {
        convolutions: Vec<FrozenCausalConvV1>,
        output: Option<FrozenLinearV1>,
    },
}
impl FrozenNetworkV1 {
    pub fn validate(
        &self,
        channels: usize,
        context: usize,
        hidden: usize,
        outputs: Option<usize>,
    ) -> Result<(), String> {
        if !(1..=512).contains(&context)
            || !(1..=256).contains(&channels)
            || !(2..=64).contains(&hidden)
        {
            return Err("frozen network dimensions exceed admitted bounds".into());
        }
        match self {
            Self::Mlp {
                hidden: layer,
                output,
            } => {
                layer.validate(channels * context, hidden)?;
                output.validate(hidden, outputs.ok_or("MLP requires an output head")?)?;
            }
            Self::Tcn {
                convolutions,
                output,
            } => {
                if convolutions.len() != 5 {
                    return Err("causal network must have five layers".into());
                }
                for (index, (conv, dilation)) in
                    convolutions.iter().zip([1, 2, 4, 8, 16]).enumerate()
                {
                    conv.validate(if index == 0 { channels } else { hidden }, hidden, dilation)?;
                }
                match (output, outputs) {
                    (Some(head), Some(n)) => head.validate(hidden, n)?,
                    (None, None) => (),
                    _ => return Err("frozen causal output head changed".into()),
                }
            }
        }
        Ok(())
    }
    /// Inputs are channel-major, oldest first within each channel.
    pub fn predict(&self, x: &[f32], context: usize) -> Result<Vec<f32>, String> {
        finite(x)?;
        if context == 0 || !x.len().is_multiple_of(context) {
            return Err("frozen network context changed".into());
        }
        let (hidden, outputs) = match self {
            Self::Mlp { hidden, output } => (hidden.outputs, Some(output.outputs)),
            Self::Tcn {
                convolutions,
                output,
            } => (
                convolutions.last().ok_or("empty causal network")?.outputs,
                output.as_ref().map(|head| head.outputs),
            ),
        };
        self.validate(x.len() / context, context, hidden, outputs)?;
        match self {
            Self::Mlp { hidden, output } => output.forward(
                &hidden
                    .forward(x)?
                    .into_iter()
                    .map(|v| v.max(0.0))
                    .collect::<Vec<_>>(),
            ),
            Self::Tcn {
                convolutions,
                output,
            } => {
                let mut state = x.to_vec();
                for conv in convolutions {
                    state = conv.forward(&state, context)?;
                }
                let last = convolutions.last().ok_or("empty causal network")?;
                let final_state = (0..last.outputs)
                    .map(|channel| state[channel * context + context - 1])
                    .collect::<Vec<_>>();
                match output {
                    Some(head) => head.forward(&final_state),
                    None => Ok(final_state),
                }
            }
        }
    }
    /// The traversal and bit encoding match the scientific parameter receipt.
    pub fn parameter_digest(&self, prefix: &[u8]) -> String {
        let mut hash = Sha256::new();
        hash.update(prefix);
        let mut index = 0_u64;
        let mut parameter = |shape: &[usize], values: &[f32]| {
            hash.update(index.to_le_bytes());
            index += 1;
            hash.update((shape.len() as u64).to_le_bytes());
            for dimension in shape {
                hash.update((*dimension as u64).to_le_bytes());
            }
            for value in values {
                hash.update(value.to_bits().to_le_bytes());
            }
        };
        let mut linear = |layer: &FrozenLinearV1| {
            parameter(&[layer.inputs, layer.outputs], &layer.weight);
            parameter(&[layer.outputs], &layer.bias);
        };
        match self {
            Self::Mlp { hidden, output } => {
                linear(hidden);
                linear(output);
            }
            Self::Tcn {
                convolutions,
                output,
            } => {
                for conv in convolutions {
                    parameter(&[conv.outputs, conv.inputs, 3], &conv.weight);
                    parameter(&[conv.outputs], &conv.bias);
                }
                if let Some(layer) = output {
                    parameter(&[layer.inputs, layer.outputs], &layer.weight);
                    parameter(&[layer.outputs], &layer.bias);
                }
            }
        }
        format!("{:x}", hash.finalize())
    }
    pub fn encoder(&self) -> Result<Self, String> {
        match self {
            Self::Tcn { convolutions, .. } => Ok(Self::Tcn {
                convolutions: convolutions.clone(),
                output: None,
            }),
            _ => Err("MLP has no reusable encoder".into()),
        }
    }
}
fn finite(values: &[f32]) -> Result<(), String> {
    if values.iter().any(|value| !value.is_finite()) {
        Err("nonfinite frozen network value".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn causal_layers_use_delayed_context_and_reject_shape_drift() {
        let mut convolutions = Vec::new();
        for (i, dilation) in [1, 2, 4, 8, 16].into_iter().enumerate() {
            let mut weight = vec![0.0; 12];
            weight[if i == 0 { 0 } else { 2 }] = 1.0;
            convolutions.push(FrozenCausalConvV1 {
                inputs: if i == 0 { 1 } else { 2 },
                outputs: 2,
                dilation,
                weight: if i == 0 { weight[..6].to_vec() } else { weight },
                bias: vec![0.0; 2],
            });
        }
        let model = FrozenNetworkV1::Tcn {
            convolutions,
            output: Some(FrozenLinearV1 {
                inputs: 2,
                outputs: 3,
                weight: vec![1.0, 2.0, 3.0, 0.0, 0.0, 0.0],
                bias: vec![0.0; 3],
            }),
        };
        model.validate(1, 3, 2, Some(3)).unwrap();
        assert_eq!(
            model.predict(&[10.0, 20.0, 30.0], 3).unwrap(),
            [10.0, 20.0, 30.0]
        );
        assert_eq!(
            model.predict(&[10.0, 20.0, 999.0], 3).unwrap(),
            [10.0, 20.0, 30.0]
        );
        assert_eq!(
            model.predict(&[11.0, 20.0, 30.0], 3).unwrap(),
            [11.0, 22.0, 33.0]
        );
        assert!(model.validate(2, 3, 2, Some(3)).is_err());
        assert!(model.predict(&[f32::NAN, 2.0, 3.0], 3).is_err());
    }
}
