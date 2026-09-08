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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexBaselineModelKindV1 {
    Ridge,
    ShallowCart,
    BurnMlp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "model_kind", rename_all = "snake_case")]
pub enum CexBaselineModelV1 {
    Ridge {
        intercept: f64,
        means: Vec<f64>,
        scales: Vec<f64>,
        coefficients: Vec<f64>,
    },
    ShallowCart {
        root: CexBaselineCartNodeV1,
    },
    /// Historical diagnostic-only representation, retained for audit decoding.
    BurnMlp {
        request_semantic_sha256: String,
        semantic_model_sha256: String,
        config_sha256: String,
        trainer_version: String,
        symbol: String,
        venue: String,
        row_count: usize,
        seed: u64,
        hidden_dim: usize,
        epochs: usize,
        learning_rate: f64,
        min_rows: usize,
    },
    /// Executable fitted parameters bound to the original training tensor digest.
    BurnMlpPortable {
        request_semantic_sha256: String,
        semantic_model_sha256: String,
        config_sha256: String,
        trainer_version: String,
        symbol: String,
        venue: String,
        row_count: usize,
        seed: u64,
        hidden_dim: usize,
        epochs: usize,
        learning_rate: f64,
        min_rows: usize,
        parameters: PortableMlpV1,
    },
}

impl CexBaselineModelV1 {
    pub fn kind(&self) -> CexBaselineModelKindV1 {
        match self {
            Self::Ridge { .. } => CexBaselineModelKindV1::Ridge,
            Self::ShallowCart { .. } => CexBaselineModelKindV1::ShallowCart,
            Self::BurnMlp { .. } | Self::BurnMlpPortable { .. } => CexBaselineModelKindV1::BurnMlp,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node_kind", rename_all = "snake_case")]
pub enum CexBaselineCartNodeV1 {
    Leaf {
        value: f64,
        sample_count: usize,
    },
    Split {
        feature_index: usize,
        threshold: f64,
        left: Box<Self>,
        right: Box<Self>,
    },
}

/// Shared, bounded inference for fitted model records. Historical diagnostics
/// remain decodable but cannot be used as an executable model.
impl CexBaselineModelV1 {
    pub fn validate_inference(&self, features: usize) -> Result<(), String> {
        if features == 0 || features > MAX_PARAMETERS {
            return Err("model input dimension is invalid".into());
        }
        match self {
            Self::Ridge {
                intercept,
                means,
                scales,
                coefficients,
            } => validate_ridge(*intercept, means, scales, coefficients, features),
            Self::ShallowCart { root } => root.validate_inference(features),
            Self::BurnMlpPortable {
                parameters,
                semantic_model_sha256,
                hidden_dim,
                ..
            } => {
                parameters.validate()?;
                if parameters.input_dim != features
                    || parameters.hidden_dim != *hidden_dim
                    || parameters.semantic_sha256()? != *semantic_model_sha256
                {
                    return Err("portable MLP training identity or dimensions differ".into());
                }
                Ok(())
            }
            Self::BurnMlp { .. } => Err("historical Burn diagnostics are not executable".into()),
        }
    }

    pub fn predict(&self, features: &[f64]) -> Result<f64, String> {
        self.validate_inference(features.len())?;
        if features.iter().any(|value| !value.is_finite()) {
            return Err("model prediction input is non-finite".into());
        }
        match self {
            Self::Ridge {
                intercept,
                means,
                scales,
                coefficients,
            } => predict_standardized_ridge(*intercept, means, scales, coefficients, features),
            Self::ShallowCart { root } => root.predict(features),
            Self::BurnMlpPortable { parameters, .. } => {
                let features: Vec<f32> = features.iter().map(|value| *value as f32).collect();
                let prediction = f64::from(parameters.predict(&features)?);
                Ok(if prediction == 0.0 { 0.0 } else { prediction })
            }
            Self::BurnMlp { .. } => Err("historical Burn diagnostics are not executable".into()),
        }
    }
}

fn validate_ridge(
    intercept: f64,
    means: &[f64],
    scales: &[f64],
    coefficients: &[f64],
    width: usize,
) -> Result<(), String> {
    if width == 0
        || width > MAX_PARAMETERS
        || means.len() != width
        || scales.len() != width
        || coefficients.len() != width
        || !intercept.is_finite()
        || means
            .iter()
            .chain(scales)
            .chain(coefficients)
            .any(|v| !v.is_finite())
        || scales.iter().any(|v| *v <= 0.0)
    {
        return Err("invalid Ridge prediction inputs".into());
    }
    Ok(())
}

/// The same scalar arithmetic is used during fitting verification and after
/// artifact reload; borrowing parameters avoids copying a model for each row.
pub fn predict_standardized_ridge(
    intercept: f64,
    means: &[f64],
    scales: &[f64],
    coefficients: &[f64],
    features: &[f64],
) -> Result<f64, String> {
    validate_ridge(intercept, means, scales, coefficients, features.len())?;
    if features.iter().any(|v| !v.is_finite()) {
        return Err("invalid Ridge prediction inputs".into());
    }
    let prediction = intercept
        + features
            .iter()
            .zip(means)
            .zip(scales)
            .zip(coefficients)
            .map(|(((value, mean), scale), coefficient)| coefficient * (value - mean) / scale)
            .sum::<f64>();
    if !prediction.is_finite() {
        return Err("Ridge prediction is non-finite".into());
    }
    Ok(if prediction == 0.0 { 0.0 } else { prediction })
}

impl CexBaselineCartNodeV1 {
    pub fn validate_inference(&self, features: usize) -> Result<(), String> {
        if features == 0 || features > MAX_PARAMETERS {
            return Err("invalid CART input dimension".into());
        }
        let mut pending = vec![(self, 0usize)];
        let mut nodes = 0;
        while let Some((node, depth)) = pending.pop() {
            nodes += 1;
            if nodes > MAX_PARAMETERS || depth > 64 {
                return Err("CART model exceeds inference bounds".into());
            }
            match node {
                Self::Leaf {
                    value,
                    sample_count,
                } => {
                    if !value.is_finite() || *sample_count == 0 {
                        return Err("invalid CART leaf".into());
                    }
                }
                Self::Split {
                    feature_index,
                    threshold,
                    left,
                    right,
                } => {
                    if *feature_index >= features || !threshold.is_finite() {
                        return Err("invalid CART model".into());
                    }
                    pending.push((left, depth + 1));
                    pending.push((right, depth + 1));
                }
            }
        }
        Ok(())
    }

    pub fn predict(&self, features: &[f64]) -> Result<f64, String> {
        self.validate_inference(features.len())?;
        if features.iter().any(|value| !value.is_finite()) {
            return Err("invalid CART prediction inputs".into());
        }
        let mut node = self;
        loop {
            match node {
                Self::Leaf { value, .. } => return Ok(if *value == 0.0 { 0.0 } else { *value }),
                Self::Split {
                    feature_index,
                    threshold,
                    left,
                    right,
                } => {
                    node = if features[*feature_index] <= *threshold {
                        left
                    } else {
                        right
                    };
                }
            }
        }
    }
}

const CEX_SUPERVISED_DECISION_POLICY_SCHEMA_V2: &str = "cex-supervised-decision-policy-v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexSupervisedSizingRuleV1 {
    ExcessExpectedReturnOverRoundTripCost,
    PredictionIdentity,
    HystereticExcessExpectedReturnOverRoundTripCost,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexSupervisedDecisionPolicyV2 {
    pub schema_version: String,
    pub round_trip_cost_multiplier: f64,
    pub sizing_rule: CexSupervisedSizingRuleV1,
    pub max_abs_position: f64,
}

impl CexSupervisedDecisionPolicyV2 {
    pub fn controlled_v2() -> Self {
        Self {
            schema_version: CEX_SUPERVISED_DECISION_POLICY_SCHEMA_V2.to_string(),
            round_trip_cost_multiplier: 2.0,
            sizing_rule: CexSupervisedSizingRuleV1::ExcessExpectedReturnOverRoundTripCost,
            max_abs_position: 1.0,
        }
    }

    pub fn prediction_identity_v2() -> Self {
        Self {
            schema_version: CEX_SUPERVISED_DECISION_POLICY_SCHEMA_V2.to_string(),
            round_trip_cost_multiplier: 0.0,
            sizing_rule: CexSupervisedSizingRuleV1::PredictionIdentity,
            max_abs_position: 1.0,
        }
    }

    pub fn hysteretic_cost_aware_v2() -> Self {
        Self {
            schema_version: CEX_SUPERVISED_DECISION_POLICY_SCHEMA_V2.to_string(),
            round_trip_cost_multiplier: 2.0,
            sizing_rule: CexSupervisedSizingRuleV1::HystereticExcessExpectedReturnOverRoundTripCost,
            max_abs_position: 1.0,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let admitted = match self.sizing_rule {
            CexSupervisedSizingRuleV1::ExcessExpectedReturnOverRoundTripCost
            | CexSupervisedSizingRuleV1::HystereticExcessExpectedReturnOverRoundTripCost => {
                self.round_trip_cost_multiplier.to_bits() == 2.0_f64.to_bits()
            }
            CexSupervisedSizingRuleV1::PredictionIdentity => {
                self.round_trip_cost_multiplier.to_bits() == 0.0_f64.to_bits()
            }
        };
        if self.schema_version != CEX_SUPERVISED_DECISION_POLICY_SCHEMA_V2
            || !admitted
            || self.max_abs_position.to_bits() != 1.0_f64.to_bits()
        {
            return Err("CEX supervised decision policy drifted".to_string());
        }
        Ok(())
    }

    pub fn content_hash(&self) -> Result<String, String> {
        self.validate()?;
        serde_json::to_vec(self)
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexDecisionCostsV1 {
    pub one_way_cost_bps: f64,
    pub funding_bps: f64,
}

impl CexSupervisedDecisionPolicyV2 {
    /// Shared decision semantics for evaluation and the native strategy actor.
    /// Reset previous_position to zero at a series boundary or disconnect.
    pub fn target_position(
        &self,
        prediction: f64,
        previous_position: f64,
        costs: CexDecisionCostsV1,
    ) -> Result<f64, String> {
        self.validate()?;
        if !prediction.is_finite()
            || !previous_position.is_finite()
            || previous_position.abs() > self.max_abs_position
            || !costs.one_way_cost_bps.is_finite()
            || !costs.funding_bps.is_finite()
            || costs.funding_bps < 0.0
        {
            return Err("invalid supervised decision input".into());
        }
        if self.sizing_rule == CexSupervisedSizingRuleV1::PredictionIdentity {
            return Ok(prediction.clamp(-self.max_abs_position, self.max_abs_position));
        }
        let edge = |multiplier: f64| -> Result<f64, String> {
            let value =
                (multiplier * costs.one_way_cost_bps + costs.funding_bps).max(0.0) / 10_000.0;
            if value.is_finite() {
                Ok(value)
            } else {
                Err("supervised decision cost overflow".into())
            }
        };
        let minimum_edge = edge(self.round_trip_cost_multiplier)?;
        let absolute = prediction.abs();
        let proposed = if absolute <= minimum_edge || absolute <= f64::EPSILON {
            0.0
        } else {
            prediction.signum()
                * ((absolute - minimum_edge) / (absolute + minimum_edge).max(f64::EPSILON))
                    .clamp(0.0, self.max_abs_position)
        };
        if self.sizing_rule
            == CexSupervisedSizingRuleV1::HystereticExcessExpectedReturnOverRoundTripCost
            && previous_position.abs() > f64::EPSILON
            && prediction.signum() == previous_position.signum()
            && absolute > edge(self.round_trip_cost_multiplier / 2.0)?
        {
            Ok(previous_position)
        } else {
            Ok(proposed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitted_models_preserve_wire_shape_and_predict_without_a_trainer() {
        let ridge_json = r#"{"model_kind":"ridge","intercept":0.5,"means":[1.0,2.0],"scales":[2.0,4.0],"coefficients":[3.0,5.0]}"#;
        let ridge: CexBaselineModelV1 = serde_json::from_str(ridge_json).unwrap();
        assert_eq!(serde_json::to_string(&ridge).unwrap(), ridge_json);
        assert_eq!(ridge.predict(&[3.0, 6.0]).unwrap(), 8.5);
        assert!(ridge.predict(&[3.0]).is_err());
        assert!(ridge.predict(&[f64::NAN, 6.0]).is_err());
        let cart = CexBaselineModelV1::ShallowCart {
            root: CexBaselineCartNodeV1::Split {
                feature_index: 0,
                threshold: 1.0,
                left: Box::new(CexBaselineCartNodeV1::Leaf {
                    value: -0.25,
                    sample_count: 3,
                }),
                right: Box::new(CexBaselineCartNodeV1::Leaf {
                    value: 0.75,
                    sample_count: 3,
                }),
            },
        };
        let restored: CexBaselineModelV1 =
            serde_json::from_slice(&serde_json::to_vec(&cart).unwrap()).unwrap();
        assert_eq!(restored.predict(&[1.0]).unwrap(), -0.25);
        assert_eq!(restored.predict(&[1.01]).unwrap(), 0.75);
        let bad = CexBaselineCartNodeV1::Split {
            feature_index: 0,
            threshold: 1.0,
            left: Box::new(CexBaselineCartNodeV1::Leaf {
                value: 0.0,
                sample_count: 1,
            }),
            right: Box::new(CexBaselineCartNodeV1::Leaf {
                value: f64::NAN,
                sample_count: 1,
            }),
        };
        assert!(
            bad.predict(&[0.0]).is_err(),
            "invalid unused branches must also be rejected"
        );
    }

    #[test]
    fn shared_decision_policy_preserves_identity_and_hysteresis() {
        let policy = CexSupervisedDecisionPolicyV2::hysteretic_cost_aware_v2();
        let costs = CexDecisionCostsV1 {
            one_way_cost_bps: 5.0,
            funding_bps: 1.0,
        };
        let entered = policy.target_position(0.002, 0.0, costs).unwrap();
        assert!(entered > 0.0 && entered < 1.0);
        assert_eq!(
            policy.target_position(0.0007, entered, costs).unwrap(),
            entered
        );
        assert_eq!(policy.target_position(0.0004, entered, costs).unwrap(), 0.0);
        assert_eq!(
            policy.target_position(-0.0007, -entered, costs).unwrap(),
            -entered
        );
        assert_eq!(policy.target_position(0.0007, 0.0, costs).unwrap(), 0.0);
        assert!(policy.target_position(f64::NAN, 0.0, costs).is_err());
        assert!(policy.target_position(0.1, 2.0, costs).is_err());
        let identity = CexSupervisedDecisionPolicyV2::prediction_identity_v2();
        assert_eq!(identity.target_position(2.0, 0.0, costs).unwrap(), 1.0);
        assert_eq!(identity.target_position(-0.25, 0.0, costs).unwrap(), -0.25);
        let json = r#"{"schema_version":"cex-supervised-decision-policy-v2","round_trip_cost_multiplier":0.0,"sizing_rule":"prediction_identity","max_abs_position":1.0}"#;
        assert_eq!(serde_json::to_string(&identity).unwrap(), json);
        assert_eq!(
            identity.content_hash().unwrap(),
            format!("{:x}", Sha256::digest(json.as_bytes()))
        );
    }

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
