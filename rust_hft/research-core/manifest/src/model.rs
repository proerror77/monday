//! Frozen inference parameters shared by training, evaluation and runtime.
//! The tensor digest is identical to the trainer's existing semantic digest.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PORTABLE_MLP_SCHEMA_V1: &str = "monday.portable_return_mlp.v1";
const MAX_PARAMETERS: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableMlpV1 {
    pub schema_version: String,
    pub input_dim: usize,
    pub hidden_dim: usize,
    /// Row-major [input_dim, hidden_dim], matching Burn Linear weights.
    pub hidden_weight: Vec<f32>,
    pub hidden_bias: Vec<f32>,
    pub output_weight: Vec<f32>,
    pub output_bias: f32,
}

impl PortableMlpV1 {
    pub fn validate(&self) -> Result<(), String> {
        let weights = self
            .input_dim
            .checked_mul(self.hidden_dim)
            .ok_or("MLP dimension overflow")?;
        let count = self
            .hidden_dim
            .checked_mul(2)
            .and_then(|biases| weights.checked_add(biases))
            .and_then(|count| count.checked_add(1))
            .ok_or("MLP parameter count overflow")?;
        if self.schema_version != PORTABLE_MLP_SCHEMA_V1
            || self.input_dim == 0
            || self.hidden_dim == 0
            || count > MAX_PARAMETERS
            || self.hidden_weight.len() != weights
            || self.hidden_bias.len() != self.hidden_dim
            || self.output_weight.len() != self.hidden_dim
            || self
                .hidden_weight
                .iter()
                .chain(&self.hidden_bias)
                .chain(&self.output_weight)
                .any(|weight| !weight.is_finite())
            || !self.output_bias.is_finite()
        {
            return Err("portable MLP shape, parameters or schema is invalid".into());
        }
        Ok(())
    }

    pub fn semantic_sha256(&self) -> Result<String, String> {
        self.validate()?;
        let mut hasher = Sha256::new();
        hasher.update(b"contract-return-regressor:burn-mlp-adam-mse-v1");
        for (name, dimensions, values) in [
            (
                "hidden.weight",
                vec![self.input_dim, self.hidden_dim],
                self.hidden_weight.as_slice(),
            ),
            (
                "hidden.bias",
                vec![self.hidden_dim],
                self.hidden_bias.as_slice(),
            ),
            (
                "output.weight",
                vec![self.hidden_dim, 1],
                self.output_weight.as_slice(),
            ),
            (
                "output.bias",
                vec![1],
                std::slice::from_ref(&self.output_bias),
            ),
        ] {
            hasher.update((name.len() as u64).to_le_bytes());
            hasher.update(name.as_bytes());
            hasher.update((dimensions.len() as u64).to_le_bytes());
            for dimension in dimensions {
                hasher.update((dimension as u64).to_le_bytes());
            }
            hasher.update((values.len() as u64).to_le_bytes());
            for value in values {
                hasher.update(value.to_bits().to_le_bytes());
            }
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Deterministic scalar f32 inference. New evidence uses these exact
    /// arithmetic semantics, rather than depending on a backend's GEMM kernel.
    pub fn predict(&self, features: &[f32]) -> Result<f32, String> {
        self.validate()?;
        if features.len() != self.input_dim || features.iter().any(|value| !value.is_finite()) {
            return Err("portable MLP feature shape or values are invalid".into());
        }
        let mut output = 0.0_f32;
        for hidden in 0..self.hidden_dim {
            let mut value = 0.0_f32;
            for (input, feature) in features.iter().enumerate() {
                value += feature * self.hidden_weight[input * self.hidden_dim + hidden];
            }
            value += self.hidden_bias[hidden];
            if !value.is_finite() {
                return Err("portable MLP hidden activation is non-finite".into());
            }
            output += value.max(0.0) * self.output_weight[hidden];
        }
        output += self.output_bias;
        if !output.is_finite() {
            return Err("portable MLP inference is non-finite".into());
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_mlp_roundtrip_binds_weights_and_validates_inference() {
        let model = PortableMlpV1 {
            schema_version: PORTABLE_MLP_SCHEMA_V1.into(),
            input_dim: 2,
            hidden_dim: 2,
            hidden_weight: vec![1.0, -1.0, 2.0, 1.0],
            hidden_bias: vec![0.5, 0.0],
            output_weight: vec![2.0, -1.0],
            output_bias: 0.25,
        };
        let decoded: PortableMlpV1 =
            serde_json::from_slice(&serde_json::to_vec(&model).unwrap()).unwrap();
        assert_eq!(decoded, model);
        assert_eq!(decoded.predict(&[1.0, 2.0]).unwrap(), 10.25);
        assert_eq!(
            decoded.semantic_sha256().unwrap(),
            model.semantic_sha256().unwrap()
        );
        let mut changed = decoded.clone();
        changed.hidden_weight[0] = 2.0;
        assert_ne!(
            changed.semantic_sha256().unwrap(),
            model.semantic_sha256().unwrap()
        );
        changed.hidden_bias.clear();
        assert!(changed.validate().is_err());
        assert!(decoded.predict(&[f32::NAN, 1.0]).is_err());
        assert!(decoded.predict(&[1.0]).is_err());
        let mut too_large = model;
        too_large.input_dim = usize::MAX;
        assert!(too_large.validate().is_err());
    }
}
