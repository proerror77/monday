//! Model-independent, raw-return diagnostics and train-only target transforms.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MlpTargetScaleV1 {
    RawReturn,
    TrainStandardized,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlpTargetTransformV1 {
    pub mode: MlpTargetScaleV1,
    pub mean: f64,
    pub scale: f64,
}

impl MlpTargetTransformV1 {
    pub fn fit(mode: MlpTargetScaleV1, targets: &[f32]) -> Result<Self, String> {
        if targets.is_empty() || targets.iter().any(|v| !v.is_finite()) {
            return Err("MLP target statistics require finite training targets".into());
        }
        let (mean, scale) = match mode {
            MlpTargetScaleV1::RawReturn => (0.0, 1.0),
            MlpTargetScaleV1::TrainStandardized => {
                let mean =
                    targets.iter().map(|v| f64::from(*v)).sum::<f64>() / targets.len() as f64;
                let variance = targets
                    .iter()
                    .map(|v| (f64::from(*v) - mean).powi(2))
                    .sum::<f64>()
                    / targets.len() as f64;
                (mean, variance.sqrt())
            }
        };
        let transform = Self { mode, mean, scale };
        transform.validate()?;
        Ok(transform)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.mean.is_finite() || !self.scale.is_finite() || self.scale <= 0.0 {
            return Err(
                "MLP target transform is non-finite or has degenerate training variance".into(),
            );
        }
        if self.mode == MlpTargetScaleV1::RawReturn
            && (self.mean.to_bits() != 0.0_f64.to_bits()
                || self.scale.to_bits() != 1.0_f64.to_bits())
        {
            return Err("raw-return target transform must be the identity".into());
        }
        Ok(())
    }

    pub fn transform(&self, target: f32) -> Result<f32, String> {
        self.validate()?;
        let value = ((f64::from(target) - self.mean) / self.scale) as f32;
        if value.is_finite() {
            Ok(value)
        } else {
            Err("MLP transformed target is non-finite".into())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbsoluteValueSummaryV1 {
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

impl AbsoluteValueSummaryV1 {
    fn from_values(values: &[f64]) -> Self {
        let mut values: Vec<_> = values.iter().map(|v| v.abs()).collect();
        values.sort_by(f64::total_cmp);
        let quantile = |q: f64| {
            let index = q * (values.len() - 1) as f64;
            let lo = index.floor() as usize;
            let hi = (lo + 1).min(values.len() - 1);
            values[lo] + (values[hi] - values[lo]) * (index - lo as f64)
        };
        Self {
            p50: quantile(0.5),
            p95: quantile(0.95),
            max: *values.last().unwrap(),
        }
    }

    fn validate(&self) -> bool {
        [self.p50, self.p95, self.max]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
            && self.p50 <= self.p95
            && self.p95 <= self.max
    }
}

/// Errors are in original return units; the mean benchmark is fitted on training rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlpPredictionDiagnosticsV1 {
    pub row_count: usize,
    pub training_target_mean: f64,
    pub mse: f64,
    pub mae: f64,
    pub zero_prediction_mse: f64,
    pub zero_prediction_mae: f64,
    pub training_mean_prediction_mse: f64,
    pub training_mean_prediction_mae: f64,
    pub mse_over_zero_prediction: Option<f64>,
    pub mse_over_training_mean_prediction: Option<f64>,
    pub prediction_abs: AbsoluteValueSummaryV1,
    pub target_abs: AbsoluteValueSummaryV1,
}

fn error_ratio(error: f64, baseline: f64) -> Option<f64> {
    (baseline > 0.0)
        .then(|| error / baseline)
        .filter(|value| value.is_finite())
}

fn ratio_agrees(actual: Option<f64>, expected: Option<f64>) -> bool {
    match (actual, expected) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.is_finite()
                && b.is_finite()
                && (a == b || (a - b).abs() <= 64.0 * f64::EPSILON * a.abs().max(b.abs()))
        }
        _ => false,
    }
}

impl MlpPredictionDiagnosticsV1 {
    pub fn new(
        predictions: &[f64],
        targets: &[f64],
        training_target_mean: f64,
    ) -> Result<Self, String> {
        if predictions.is_empty()
            || predictions.len() != targets.len()
            || !training_target_mean.is_finite()
            || predictions.iter().chain(targets).any(|v| !v.is_finite())
        {
            return Err("MLP prediction diagnostics require aligned finite rows".into());
        }
        let n = predictions.len() as f64;
        let mse = predictions
            .iter()
            .zip(targets)
            .map(|(p, y)| (p - y).powi(2))
            .sum::<f64>()
            / n;
        let mae = predictions
            .iter()
            .zip(targets)
            .map(|(p, y)| (p - y).abs())
            .sum::<f64>()
            / n;
        let zero_prediction_mse = targets.iter().map(|y| y.powi(2)).sum::<f64>() / n;
        let zero_prediction_mae = targets.iter().map(|y| y.abs()).sum::<f64>() / n;
        let training_mean_prediction_mse = targets
            .iter()
            .map(|y| (y - training_target_mean).powi(2))
            .sum::<f64>()
            / n;
        let training_mean_prediction_mae = targets
            .iter()
            .map(|y| (y - training_target_mean).abs())
            .sum::<f64>()
            / n;
        let diagnostics = Self {
            row_count: predictions.len(),
            training_target_mean,
            mse,
            mae,
            zero_prediction_mse,
            zero_prediction_mae,
            training_mean_prediction_mse,
            training_mean_prediction_mae,
            mse_over_zero_prediction: error_ratio(mse, zero_prediction_mse),
            mse_over_training_mean_prediction: error_ratio(mse, training_mean_prediction_mse),
            prediction_abs: AbsoluteValueSummaryV1::from_values(predictions),
            target_abs: AbsoluteValueSummaryV1::from_values(targets),
        };
        diagnostics.validate()?;
        Ok(diagnostics)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.row_count == 0
            || !self.training_target_mean.is_finite()
            || [
                self.mse,
                self.mae,
                self.zero_prediction_mse,
                self.zero_prediction_mae,
                self.training_mean_prediction_mse,
                self.training_mean_prediction_mae,
            ]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0)
            || !ratio_agrees(
                self.mse_over_zero_prediction,
                error_ratio(self.mse, self.zero_prediction_mse),
            )
            || !ratio_agrees(
                self.mse_over_training_mean_prediction,
                error_ratio(self.mse, self.training_mean_prediction_mse),
            )
            || !self.prediction_abs.validate()
            || !self.target_abs.validate()
        {
            return Err("MLP raw-return diagnostics are invalid".into());
        }
        Ok(())
    }
}

/// Deterministic learning evidence. Runtime duration is recorded separately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlpLearningDiagnosticsV1 {
    pub schema_version: String,
    pub updates_requested: usize,
    pub updates_completed: usize,
    pub initial_parameters_sha256: String,
    pub target_transform: MlpTargetTransformV1,
    /// Initial loss, then one loss per completed update, in optimization target units.
    pub loss_history: Vec<f64>,
    pub max_gradient_abs: f64,
    pub max_parameter_abs: f64,
    pub training_prediction: MlpPredictionDiagnosticsV1,
    pub exit_reason: String,
}

impl MlpLearningDiagnosticsV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.target_transform.validate()?;
        self.training_prediction.validate()?;
        if self.target_transform.mode == MlpTargetScaleV1::TrainStandardized
            && (!ratio_agrees(
                Some(self.target_transform.mean),
                Some(self.training_prediction.training_target_mean),
            ) || !ratio_agrees(
                Some(self.target_transform.scale.powi(2)),
                Some(self.training_prediction.training_mean_prediction_mse),
            ))
        {
            return Err(
                "MLP target transform is detached from training-only target statistics".into(),
            );
        }
        if self.schema_version != "mlp-learning-diagnostics-v1"
            || self.updates_requested == 0
            || self.updates_completed != self.updates_requested
            || self.updates_completed.checked_add(1) != Some(self.loss_history.len())
            || !crate::valid_sha256(&self.initial_parameters_sha256)
            || self.loss_history.iter().any(|v| !v.is_finite() || *v < 0.0)
            || !self.max_gradient_abs.is_finite()
            || self.max_gradient_abs < 0.0
            || !self.max_parameter_abs.is_finite()
            || self.max_parameter_abs < 0.0
            || self.exit_reason != "fixed_update_budget_completed"
        {
            return Err(
                "MLP update counts, loss curve or numerical diagnostics are invalid".into(),
            );
        }
        Ok(())
    }
}
