//! Model-independent, raw-return diagnostics and train-only target transforms.
use serde::{Deserialize, Serialize};

pub const MAX_MLP_TRAINING_UPDATES: usize = 16_384;
/// Accounts for the shared scale's f32 rounding and each f32 multiplication.
pub const MLP_GRADIENT_CLIP_RELATIVE_TOLERANCE: f64 = 8.0 * f32::EPSILON as f64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlpConvergencePolicyV1 {
    pub minimum_updates: usize,
    pub window_updates: usize,
    pub comparisons: usize,
    pub max_relative_window_change: f64,
    pub max_relative_tail_range: f64,
}

impl Default for MlpConvergencePolicyV1 {
    fn default() -> Self {
        Self {
            minimum_updates: 2048,
            window_updates: 256,
            comparisons: 3,
            max_relative_window_change: 0.001,
            max_relative_tail_range: 0.005,
        }
    }
}

impl MlpConvergencePolicyV1 {
    pub fn validate(&self) -> Result<(), String> {
        let tail_updates = self
            .comparisons
            .checked_add(1)
            .and_then(|windows| windows.checked_mul(self.window_updates));
        if self.minimum_updates == 0
            || self.minimum_updates > MAX_MLP_TRAINING_UPDATES
            || self.window_updates == 0
            || self.comparisons == 0
            || tail_updates.is_none_or(|count| count > MAX_MLP_TRAINING_UPDATES)
            || [
                self.max_relative_window_change,
                self.max_relative_tail_range,
            ]
            .iter()
            .any(|v| !v.is_finite() || !(0.0..1.0).contains(v))
        {
            return Err("MLP training-only convergence policy is invalid".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlpOptimizationControlsV1 {
    pub global_gradient_clip_l2: f64,
    pub max_raw_gradient_l2: f64,
    pub max_loss_growth_ratio: f64,
    pub convergence: MlpConvergencePolicyV1,
}

impl Default for MlpOptimizationControlsV1 {
    fn default() -> Self {
        Self {
            global_gradient_clip_l2: 1.0,
            max_raw_gradient_l2: 100.0,
            max_loss_growth_ratio: 10.0,
            convergence: MlpConvergencePolicyV1::default(),
        }
    }
}

impl MlpOptimizationControlsV1 {
    pub fn validate_for_updates(&self, updates: usize) -> Result<(), String> {
        self.validate()?;
        if updates == 0 || updates > MAX_MLP_TRAINING_UPDATES {
            return Err("MLP update budget must be between 1 and 16384".into());
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        self.convergence.validate()?;
        if !self.global_gradient_clip_l2.is_finite()
            || self.global_gradient_clip_l2 < f64::from(f32::MIN_POSITIVE)
            || self.global_gradient_clip_l2 > f64::from(f32::MAX).sqrt()
            || !self.max_raw_gradient_l2.is_finite()
            || self.max_raw_gradient_l2 < self.global_gradient_clip_l2
            || !self.max_loss_growth_ratio.is_finite()
            || self.max_loss_growth_ratio < 1.0
        {
            return Err("MLP optimization controls are invalid".into());
        }
        Ok(())
    }

    pub fn validate_loss(&self, initial: f64, current: f64) -> Result<(), String> {
        if !initial.is_finite() || initial < 0.0 || !current.is_finite() || current < 0.0 {
            return Err("MLP optimization loss is non-finite or negative".into());
        }
        // Division avoids overflowing the declared bound. An initially zero loss
        // must stay zero; there is no unrecorded absolute tolerance.
        if (initial == 0.0 && current != 0.0)
            || (initial > 0.0 && current / initial > self.max_loss_growth_ratio)
        {
            return Err("MLP optimization loss exceeded the frozen growth limit".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MlpConvergenceStatusV1 {
    Converged,
    BudgetExhaustedNotConverged,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlpConvergenceDiagnosticsV1 {
    pub status: MlpConvergenceStatusV1,
    /// Chronological non-overlapping windows of post-update training losses.
    pub tail_window_means: Vec<f64>,
    /// Absolute adjacent mean change divided by max(previous mean, 1e-12).
    pub relative_window_changes: Vec<f64>,
    /// All tail windows' (maximum - minimum) / max(tail mean, 1e-12).
    pub relative_tail_range: Option<f64>,
}

impl MlpConvergenceDiagnosticsV1 {
    pub fn from_loss_history(
        policy: &MlpConvergencePolicyV1,
        losses: &[f64],
    ) -> Result<Self, String> {
        policy.validate()?;
        if losses.is_empty()
            || losses.len() > MAX_MLP_TRAINING_UPDATES + 1
            || losses.iter().any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err("MLP convergence requires a finite training loss history".into());
        }
        let updates = losses.len() - 1;
        let tail_updates = (policy.comparisons + 1) * policy.window_updates;
        let mut result = Self {
            status: MlpConvergenceStatusV1::BudgetExhaustedNotConverged,
            tail_window_means: Vec::new(),
            relative_window_changes: Vec::new(),
            relative_tail_range: None,
        };
        if updates < tail_updates {
            return Ok(result);
        }
        let tail = &losses[losses.len() - tail_updates..];
        // Sum scaled values so a finite loss cannot overflow its window mean.
        result.tail_window_means = tail
            .chunks_exact(policy.window_updates)
            .map(|window| {
                window
                    .iter()
                    .map(|v| v / policy.window_updates as f64)
                    .sum()
            })
            .collect();
        result.relative_window_changes = result
            .tail_window_means
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs() / pair[0].max(1e-12))
            .collect();
        let mean = tail.iter().map(|v| v / tail_updates as f64).sum::<f64>();
        let min = tail.iter().copied().fold(f64::INFINITY, f64::min);
        let max = tail.iter().copied().fold(0.0, f64::max);
        let range = (max - min) / mean.max(1e-12);
        result.relative_tail_range = Some(range);
        if updates >= policy.minimum_updates
            && result
                .relative_window_changes
                .iter()
                .all(|v| *v <= policy.max_relative_window_change)
            && range <= policy.max_relative_tail_range
        {
            result.status = MlpConvergenceStatusV1::Converged;
        }
        Ok(result)
    }

    fn agrees(&self, expected: &Self) -> bool {
        self.status == expected.status
            && self.tail_window_means.len() == expected.tail_window_means.len()
            && self.relative_window_changes.len() == expected.relative_window_changes.len()
            && self
                .tail_window_means
                .iter()
                .zip(&expected.tail_window_means)
                .chain(
                    self.relative_window_changes
                        .iter()
                        .zip(&expected.relative_window_changes),
                )
                .all(|(&actual, &expected)| ratio_agrees(Some(actual), Some(expected)))
            && ratio_agrees(self.relative_tail_range, expected.relative_tail_range)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlpStabilityDiagnosticsV1 {
    pub controls: MlpOptimizationControlsV1,
    pub raw_gradient_l2_history: Vec<f64>,
    pub applied_gradient_l2_history: Vec<f64>,
    pub clipped_updates: usize,
    pub convergence: MlpConvergenceDiagnosticsV1,
}

impl MlpStabilityDiagnosticsV1 {
    pub fn validate(&self, losses: &[f64], updates: usize) -> Result<(), String> {
        self.controls.validate()?;
        if updates == 0
            || updates > MAX_MLP_TRAINING_UPDATES
            || updates.checked_add(1) != Some(losses.len())
            || self.raw_gradient_l2_history.len() != updates
            || self.applied_gradient_l2_history.len() != updates
        {
            return Err("MLP stability evidence has inconsistent update counts".into());
        }
        for &loss in losses {
            self.controls.validate_loss(losses[0], loss)?;
        }
        let clip = self.controls.global_gradient_clip_l2;
        for (&raw, &applied) in self
            .raw_gradient_l2_history
            .iter()
            .zip(&self.applied_gradient_l2_history)
        {
            let expected = raw.min(clip);
            if !raw.is_finite()
                || raw < 0.0
                || raw > self.controls.max_raw_gradient_l2
                || !applied.is_finite()
                || applied < 0.0
                || (applied - expected).abs() > expected * MLP_GRADIENT_CLIP_RELATIVE_TOLERANCE
            {
                return Err("MLP global gradient evidence violates its frozen controls".into());
            }
        }
        if self.clipped_updates
            != self
                .raw_gradient_l2_history
                .iter()
                .filter(|v| **v > clip)
                .count()
            || !self
                .convergence
                .agrees(&MlpConvergenceDiagnosticsV1::from_loss_history(
                    &self.controls.convergence,
                    losses,
                )?)
        {
            return Err("MLP clipping or convergence evidence is not reproducible".into());
        }
        Ok(())
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stability: Option<MlpStabilityDiagnosticsV1>,
}

impl MlpLearningDiagnosticsV1 {
    pub fn validate_optimization(
        &self,
        expected: Option<&MlpOptimizationControlsV1>,
    ) -> Result<(), String> {
        self.validate()?;
        if self.stability.as_ref().map(|value| &value.controls) != expected {
            return Err(
                "MLP optimization diagnostics are detached from the sealed configuration".into(),
            );
        }
        Ok(())
    }

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
            || self.updates_requested > MAX_MLP_TRAINING_UPDATES
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
        if let Some(stability) = &self.stability {
            stability.validate(&self.loss_history, self.updates_completed)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod optimization_tests {
    use super::*;

    fn policy() -> MlpConvergencePolicyV1 {
        MlpConvergencePolicyV1 {
            minimum_updates: 8,
            window_updates: 2,
            comparisons: 3,
            ..MlpConvergencePolicyV1::default()
        }
    }

    #[test]
    fn convergence_uses_all_post_update_tail_windows_and_minimum_budget() {
        let mut losses = vec![1000.0];
        losses.extend([1.0; 8]);
        let stable = MlpConvergenceDiagnosticsV1::from_loss_history(&policy(), &losses).unwrap();
        assert_eq!(stable.status, MlpConvergenceStatusV1::Converged);
        assert_eq!(stable.tail_window_means, vec![1.0; 4]);
        assert_eq!(stable.relative_tail_range, Some(0.0));
        let mut insufficient = policy();
        insufficient.minimum_updates = 9;
        assert_eq!(
            MlpConvergenceDiagnosticsV1::from_loss_history(&insufficient, &losses)
                .unwrap()
                .status,
            MlpConvergenceStatusV1::BudgetExhaustedNotConverged
        );
        assert!(
            MlpConvergenceDiagnosticsV1::from_loss_history(&policy(), &losses[..8])
                .unwrap()
                .tail_window_means
                .is_empty()
        );
        let descent = (0..9).map(|index| 9.0 - index as f64).collect::<Vec<_>>();
        assert_eq!(
            MlpConvergenceDiagnosticsV1::from_loss_history(&policy(), &descent)
                .unwrap()
                .status,
            MlpConvergenceStatusV1::BudgetExhaustedNotConverged
        );
        // All window means agree, but an earlier tail window oscillates.
        losses[1] = 0.5;
        losses[2] = 1.5;
        let oscillating =
            MlpConvergenceDiagnosticsV1::from_loss_history(&policy(), &losses).unwrap();
        assert_eq!(oscillating.relative_window_changes, vec![0.0; 3]);
        assert_eq!(oscillating.relative_tail_range, Some(1.0));
        assert_eq!(
            oscillating.status,
            MlpConvergenceStatusV1::BudgetExhaustedNotConverged
        );
    }

    #[test]
    fn stability_rejects_edited_norms_counts_convergence_and_loss_growth() {
        let losses = vec![1.0; 9];
        let controls = MlpOptimizationControlsV1 {
            convergence: policy(),
            ..Default::default()
        };
        let evidence = MlpStabilityDiagnosticsV1 {
            convergence: MlpConvergenceDiagnosticsV1::from_loss_history(
                &controls.convergence,
                &losses,
            )
            .unwrap(),
            controls,
            raw_gradient_l2_history: vec![2.0; 8],
            applied_gradient_l2_history: vec![1.0; 8],
            clipped_updates: 8,
        };
        evidence.validate(&losses, 8).unwrap();
        let mut changed = evidence.clone();
        changed.applied_gradient_l2_history[0] = 1.1;
        assert!(changed.validate(&losses, 8).is_err());
        let mut changed = evidence.clone();
        changed.raw_gradient_l2_history[0] = 101.0;
        assert!(changed.validate(&losses, 8).is_err());
        let mut changed = evidence.clone();
        changed.clipped_updates = 0;
        assert!(changed.validate(&losses, 8).is_err());
        let mut changed = evidence.clone();
        changed.convergence.tail_window_means[0] = 2.0;
        assert!(changed.validate(&losses, 8).is_err());
        let mut changed = evidence.clone();
        changed.convergence.status = MlpConvergenceStatusV1::BudgetExhaustedNotConverged;
        assert!(changed.validate(&losses, 8).is_err());
        let mut changed_losses = losses;
        changed_losses[8] = 11.0;
        assert!(evidence
            .validate(&changed_losses, 8)
            .unwrap_err()
            .contains("growth limit"));
        assert!(evidence.controls.validate_loss(0.0, 0.001).is_err());
        assert!(evidence.controls.validate_loss(1.0, f64::NAN).is_err());
        assert!(evidence
            .controls
            .validate_for_updates(MAX_MLP_TRAINING_UPDATES + 1)
            .is_err());
    }
}
