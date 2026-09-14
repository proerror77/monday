use super::{predict_validated_ridge, CexBaselineCartNodeV1, CexBaselineModelV1, PortableMlpV1};

/// An admitted, immutable snapshot for a batch of predictions. Construction
/// checks the complete source evidence; subsequent calls check the current row.
/// Changing source parameters or diagnostics requires preparing a new snapshot.
#[derive(Debug, Clone)]
pub struct PreparedCexBaselineModel {
    input_dim: usize,
    parameters: Parameters,
}

#[derive(Debug, Clone)]
enum Parameters {
    Ridge {
        intercept: f64,
        means: Vec<f64>,
        scales: Vec<f64>,
        coefficients: Vec<f64>,
    },
    Cart(CexBaselineCartNodeV1),
    Mlp(PortableMlpV1),
}

impl PreparedCexBaselineModel {
    pub(super) fn new(model: &CexBaselineModelV1, input_dim: usize) -> Result<Self, String> {
        model.validate_inference(input_dim)?;
        let parameters = match model {
            CexBaselineModelV1::Ridge {
                intercept,
                means,
                scales,
                coefficients,
            } => Parameters::Ridge {
                intercept: *intercept,
                means: means.clone(),
                scales: scales.clone(),
                coefficients: coefficients.clone(),
            },
            CexBaselineModelV1::ShallowCart { root } => Parameters::Cart(root.clone()),
            CexBaselineModelV1::BurnMlpPortableV2 { parameters, .. } => {
                Parameters::Mlp(parameters.clone())
            }
            CexBaselineModelV1::BurnMlp { .. } | CexBaselineModelV1::BurnMlpPortable { .. } => {
                return Err("historical Burn records are audit-only".into());
            }
        };
        Ok(Self {
            input_dim,
            parameters,
        })
    }

    pub fn predict(&self, features: &[f64]) -> Result<f64, String> {
        if features.len() != self.input_dim || features.iter().any(|value| !value.is_finite()) {
            return Err("prepared model prediction input is invalid".into());
        }
        match &self.parameters {
            Parameters::Ridge {
                intercept,
                means,
                scales,
                coefficients,
            } => predict_validated_ridge(*intercept, means, scales, coefficients, features),
            Parameters::Cart(root) => root.predict_validated(features),
            Parameters::Mlp(parameters) => {
                let features: Vec<f32> = features.iter().map(|value| *value as f32).collect();
                let prediction = f64::from(parameters.predict_validated(&features)?);
                Ok(if prediction == 0.0 { 0.0 } else { prediction })
            }
        }
    }
}
