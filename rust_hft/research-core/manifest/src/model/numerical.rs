//! A reference interval and a forward-roundoff certificate for the existing
//! two-layer f32 MLP. This module never changes the model or either prediction.
use super::PortableMlpV1;
use serde::Serialize;
use std::fmt;

const UNIT_ROUNDOFF: f64 = 1.0 / 16_777_216.0;
const MIN_NORMAL: f64 = f32::MIN_POSITIVE as f64;
const MAX_FINITE: f64 = f32::MAX as f64;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MlpPredictionParityDiagnostics {
    pub backend_prediction: f64,
    pub portable_prediction: f64,
    pub absolute_difference: f64,
    pub reference_lower: f64,
    pub reference_upper: f64,
    pub error_bound: f64,
    pub exact_reference_agreement: bool,
    pub certified_lower: f64,
    pub certified_upper: f64,
    pub input_dim: usize,
    pub hidden_dim: usize,
}

#[derive(Debug, Clone)]
pub struct MlpPredictionParityError {
    pub reason: String,
    pub diagnostics: Option<MlpPredictionParityDiagnostics>,
}

impl fmt::Display for MlpPredictionParityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "MLP numerical parity rejected: {}", self.reason)?;
        if let Some(d) = &self.diagnostics {
            write!(formatter,
                "; backend={}, portable={}, absolute_difference={}, reference=[{},{}], error_bound={}, certified=[{},{}]",
                d.backend_prediction, d.portable_prediction, d.absolute_difference,
                d.reference_lower, d.reference_upper, d.error_bound,
                d.certified_lower, d.certified_upper)?;
        }
        Ok(())
    }
}

impl std::error::Error for MlpPredictionParityError {}

#[derive(Clone, Copy, Debug)]
struct Interval {
    lower: f64,
    upper: f64,
}

impl Interval {
    fn point(value: f64) -> Self {
        Self {
            lower: value,
            upper: value,
        }
    }

    fn add(self, other: Self) -> Self {
        Self {
            lower: down_add(self.lower, other.lower),
            upper: up_add(self.upper, other.upper),
        }
    }

    fn multiply(self, scalar: f64) -> Self {
        let (lower, upper) = if scalar >= 0.0 {
            (self.lower, self.upper)
        } else {
            (self.upper, self.lower)
        };
        Self {
            lower: down_mul(lower, scalar),
            upper: up_mul(upper, scalar),
        }
    }

    fn relu(self) -> Self {
        Self {
            lower: self.lower.max(0.0),
            upper: self.upper.max(0.0),
        }
    }

    fn absolute_upper(self) -> f64 {
        self.lower.abs().max(self.upper.abs())
    }
}

// Error-free TwoSum under roundTiesToEven. The checked f32/two-layer domain
// keeps every nonzero f64 intermediate and residual away from f64 underflow;
// no operation here can overflow before the f32 magnitude checks reject it.
fn sum_residual(a: f64, b: f64, sum: f64) -> f64 {
    let virtual_b = sum - a;
    (a - (sum - virtual_b)) + (b - virtual_b)
}

fn up_add(a: f64, b: f64) -> f64 {
    let value = a + b;
    if sum_residual(a, b, value) > 0.0 {
        value.next_up()
    } else {
        value
    }
}

fn down_add(a: f64, b: f64) -> f64 {
    let value = a + b;
    if sum_residual(a, b, value) < 0.0 {
        value.next_down()
    } else {
        value
    }
}

fn up_mul(a: f64, b: f64) -> f64 {
    let value = a * b;
    if a.mul_add(b, -value) > 0.0 {
        value.next_up()
    } else {
        value
    }
}

fn down_mul(a: f64, b: f64) -> f64 {
    let value = a * b;
    if a.mul_add(b, -value) < 0.0 {
        value.next_down()
    } else {
        value
    }
}

// Do not compare a possibly subnormal f32 with 0.0: DAZ can make that test
// return true for nonzero bits. Classification and subnormal promotion must
// not depend on the floating-point input flushing mode.
fn normal_or_zero(value: f32) -> bool {
    let bits = value.to_bits() & 0x7fff_ffff;
    let exponent = bits & 0x7f80_0000;
    bits == 0 || (exponent != 0 && exponent != 0x7f80_0000)
}

fn exact_f32_value(value: f32) -> f64 {
    let bits = value.to_bits();
    if bits & 0x7f80_0000 == 0 {
        let magnitude = f64::from(bits & 0x007f_ffff) * (2.0_f64).powi(-149);
        if bits >> 31 == 0 {
            magnitude
        } else {
            -magnitude
        }
    } else {
        f64::from(value)
    }
}

fn round_to_nearest() -> bool {
    use std::hint::black_box;
    // Runtime operands prevent constant folding; these distinguish nearest-even
    // from the other IEEE rounding directions without changing the environment.
    let one = black_box(1.0_f32);
    let half = black_box((2.0_f32).powi(-24));
    let three_halves = black_box(3.0_f32 * (2.0_f32).powi(-24));
    let one64 = black_box(1.0_f64);
    let half64 = black_box((2.0_f64).powi(-53));
    let three_halves64 = black_box(3.0_f64 * (2.0_f64).powi(-53));
    (one + half).to_bits() == 1.0_f32.to_bits()
        && (one + three_halves).to_bits() == 1.0_f32.to_bits() + 2
        && (one64 + half64).to_bits() == 1.0_f64.to_bits()
        && (one64 + three_halves64).to_bits() == 1.0_f64.to_bits() + 2
}

#[derive(Clone, Copy)]
struct DotError {
    relative: f64,
    underflow: f64,
    total: f64,
}

/// k counts every multiply/add, so fused multiply-add and a shallower reduction
/// are covered. Four min-normal units per operation conservatively cover two
/// flushed addition operands, rounding, and a flushed result. Original operands
/// are checked separately; hidden-activation input flushing is propagated below.
fn dot_error(absolute_sum: f64, terms: usize) -> Result<DotError, String> {
    if absolute_sum == 0.0 {
        return Ok(DotError {
            relative: 0.0,
            underflow: 0.0,
            total: 0.0,
        });
    }
    let operations = terms
        .checked_mul(2)
        .and_then(|n| n.checked_add(1))
        .ok_or("roundoff operation count overflow")?;
    let ku = operations as f64 * UNIT_ROUNDOFF;
    let denominator = (1.0 - ku).next_down();
    if denominator <= 0.0 || !absolute_sum.is_finite() {
        return Err("no finite roundoff certificate for this operation count or magnitude".into());
    }
    let gamma = (ku / denominator).next_up();
    let relative = up_mul(gamma, absolute_sum);
    let underflow = (up_mul(operations as f64, 4.0 * MIN_NORMAL) / denominator).next_up();
    let total = up_add(relative, underflow);
    if !total.is_finite() || up_add(absolute_sum, total) > MAX_FINITE {
        return Err("possible f32 intermediate overflow prevents a roundoff certificate".into());
    }
    Ok(DotError {
        relative,
        underflow,
        total,
    })
}

impl PortableMlpV1 {
    /// Check both already-computed outputs against an independent outward-rounded
    /// f64 reference to the same post-inverse-transform f32 tensors and inputs.
    /// The caller must retain the exact export/tensor digest check. This is an
    /// arithmetic certificate, not a replacement for parameter or unit identity.
    pub fn verify_prediction_pair(
        &self,
        features: &[f32],
        backend_prediction: f32,
        portable_prediction: f32,
    ) -> Result<MlpPredictionParityDiagnostics, MlpPredictionParityError> {
        let unsupported = |reason: String| MlpPredictionParityError {
            reason: format!(
                "{reason}; backend={backend_prediction}, portable={portable_prediction}"
            ),
            diagnostics: None,
        };
        self.validate().map_err(&unsupported)?;
        if features.len() != self.input_dim {
            return Err(unsupported(
                "feature dimension differs from the model".into(),
            ));
        }
        if !backend_prediction.is_finite() || !portable_prediction.is_finite() {
            return Err(unsupported("prediction is non-finite".into()));
        }
        if !round_to_nearest() {
            return Err(unsupported(
                "roundTiesToEven f32/f64 arithmetic is required".into(),
            ));
        }
        // This checked support domain avoids an unknown DAZ mode turning an
        // original tiny operand into zero before multiplication by a large one.
        if features
            .iter()
            .chain(&self.hidden_weight)
            .chain(&self.hidden_bias)
            .chain(&self.output_weight)
            .chain(std::iter::once(&self.output_bias))
            .any(|v| !normal_or_zero(*v))
        {
            return Err(unsupported(
                "non-finite or nonzero subnormal operand is outside the certified domain".into(),
            ));
        }
        let mut reference = Interval::point(f64::from(self.output_bias));
        let mut ideal_output_magnitude = f64::from(self.output_bias).abs();
        let mut actual_output_magnitude = ideal_output_magnitude;
        let mut propagated_error = 0.0;
        for hidden in 0..self.hidden_dim {
            let mut value = Interval::point(f64::from(self.hidden_bias[hidden]));
            let mut magnitude = f64::from(self.hidden_bias[hidden]).abs();
            for (input, feature) in features.iter().enumerate() {
                // Two finite f32 operands have at most 48 product significand
                // bits and a range fully representable in f64: this product is exact.
                let product = f64::from(*feature)
                    * f64::from(self.hidden_weight[input * self.hidden_dim + hidden]);
                value = value.add(Interval::point(product));
                magnitude = up_add(magnitude, product.abs());
            }
            let error = dot_error(magnitude, self.input_dim).map_err(&unsupported)?;
            let hidden_error = if up_add(value.upper, error.total) <= 0.0 {
                // ReLU is exactly zero for every certified f32 evaluation.
                0.0
            } else {
                if error.underflow > error.relative {
                    return Err(unsupported(
                        "underflow-dominated hidden arithmetic is outside the certified domain"
                            .into(),
                    ));
                }
                // ReLU is 1-Lipschitz; its result may be flushed when read by
                // the output multiplication even if the first layer retained it.
                up_add(error.total, MIN_NORMAL)
            };
            let value = value.relu();
            let weight = f64::from(self.output_weight[hidden]);
            reference = reference.add(value.multiply(weight));
            let ideal_term = up_mul(value.absolute_upper(), weight.abs());
            ideal_output_magnitude = up_add(ideal_output_magnitude, ideal_term);
            actual_output_magnitude = up_add(
                actual_output_magnitude,
                up_mul(up_add(value.absolute_upper(), hidden_error), weight.abs()),
            );
            propagated_error = up_add(propagated_error, up_mul(hidden_error, weight.abs()));
        }
        let output_error =
            dot_error(actual_output_magnitude, self.hidden_dim).map_err(&unsupported)?;
        if output_error.underflow > output_error.relative {
            return Err(unsupported(
                "underflow-dominated output arithmetic is outside the certified domain".into(),
            ));
        }
        let bound = up_add(propagated_error, output_error.total);
        let backend = exact_f32_value(backend_prediction);
        let portable = exact_f32_value(portable_prediction);
        // A point interval proves the exact mathematical output. In particular,
        // two exact zeros at a ReLU boundary need no loose roundoff allowance.
        // A wide interval from cancellation cannot use this exception.
        let exact_reference_agreement = reference.lower == reference.upper
            && backend == reference.lower
            && portable == reference.lower;
        if !bound.is_finite() || !reference.lower.is_finite() || !reference.upper.is_finite() {
            return Err(unsupported(
                "non-finite error bound cannot certify numerical parity".into(),
            ));
        }
        let lower = if exact_reference_agreement {
            reference.lower
        } else {
            down_add(reference.lower, -bound)
        };
        let upper = if exact_reference_agreement {
            reference.upper
        } else {
            up_add(reference.upper, bound)
        };
        let diagnostics = MlpPredictionParityDiagnostics {
            backend_prediction: backend,
            portable_prediction: portable,
            absolute_difference: (backend - portable).abs(),
            reference_lower: reference.lower,
            reference_upper: reference.upper,
            error_bound: bound,
            exact_reference_agreement,
            certified_lower: lower,
            certified_upper: upper,
            input_dim: self.input_dim,
            hidden_dim: self.hidden_dim,
        };
        if bound > 0.0 && bound >= ideal_output_magnitude && !exact_reference_agreement {
            return Err(MlpPredictionParityError {
                reason: "vacuous error bound cannot certify deviations from the reference".into(),
                diagnostics: Some(diagnostics),
            });
        }
        if [backend, portable]
            .iter()
            .any(|value| *value < lower || *value > upper)
        {
            return Err(MlpPredictionParityError {
                reason: "an output is outside the independently certified interval".into(),
                diagnostics: Some(diagnostics),
            });
        }
        Ok(diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PORTABLE_MLP_SCHEMA_V1;

    fn identity() -> PortableMlpV1 {
        PortableMlpV1 {
            schema_version: PORTABLE_MLP_SCHEMA_V1.into(),
            input_dim: 1,
            hidden_dim: 1,
            hidden_weight: vec![1.0],
            hidden_bias: vec![0.0],
            output_weight: vec![1.0],
            output_bias: 0.0,
        }
    }

    #[test]
    fn exact_relu_zero_uses_exact_reference_without_a_loose_allowance() {
        let mut model = identity();
        model.input_dim = 2;
        model.hidden_weight = vec![1.0, -1.0];
        let details = model.verify_prediction_pair(&[1.0, 1.0], 0.0, 0.0).unwrap();
        assert!(details.exact_reference_agreement);
        assert_eq!(
            (details.reference_lower, details.reference_upper),
            (0.0, 0.0)
        );
        assert_eq!(
            (details.certified_lower, details.certified_upper),
            (0.0, 0.0)
        );
        assert!(model
            .verify_prediction_pair(&[1.0, 1.0], 1e-12, 1e-12)
            .is_err());
        assert!(model
            .verify_prediction_pair(&[1.0, 1.0], 0.0, 1e-12)
            .is_err());
    }

    #[test]
    fn wide_reference_cannot_hide_a_common_wrong_output() {
        let mut model = identity();
        model.input_dim = 2;
        model.hidden_weight = vec![1.0, -1.0];
        model.hidden_bias[0] = 1.0;
        // The exact real hidden value is 1; f64 interval arithmetic cannot
        // retain it tightly at this scale. A shared wrong value is not proof.
        let error = model
            .verify_prediction_pair(&[1e30, 1e30], 1e10, 1e10)
            .unwrap_err();
        assert!(error.reason.contains("vacuous"));
    }

    #[test]
    fn relu_boundary_with_other_signal_keeps_a_finite_certificate() {
        let model = PortableMlpV1 {
            schema_version: PORTABLE_MLP_SCHEMA_V1.into(),
            input_dim: 2,
            hidden_dim: 2,
            hidden_weight: vec![1.0, 0.0, -1.0, 1.0],
            hidden_bias: vec![0.0, 0.5],
            output_weight: vec![0.0001, 0.0001],
            output_bias: 0.0,
        };
        for first in [1.0_f32.next_down(), 1.0, 1.0_f32.next_up()] {
            let features = [first, 1.0];
            let prediction = model.predict(&features).unwrap();
            let proof = model
                .verify_prediction_pair(&features, prediction, prediction)
                .unwrap();
            assert!(proof.error_bound.is_finite());
        }
    }

    #[test]
    fn parameter_shape_nonfinite_and_uncertifiable_domain_still_reject() {
        let model = identity();
        assert!(model.verify_prediction_pair(&[], 1.0, 1.0).is_err());
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(model.verify_prediction_pair(&[value], 1.0, 1.0).is_err());
            assert!(model.verify_prediction_pair(&[1.0], value, 1.0).is_err());
            assert!(model.verify_prediction_pair(&[1.0], 1.0, value).is_err());
            let mut invalid = model.clone();
            invalid.hidden_weight[0] = value;
            assert!(invalid.verify_prediction_pair(&[1.0], 1.0, 1.0).is_err());
        }
        for bits in [1, 0x007f_ffff, 0x8000_0001, 0x807f_ffff] {
            let tiny = f32::from_bits(bits);
            assert!(!normal_or_zero(tiny));
            assert!(model.verify_prediction_pair(&[tiny], 0.0, 0.0).is_err());
            let mut invalid = model.clone();
            invalid.output_weight[0] = tiny;
            assert!(invalid.verify_prediction_pair(&[1.0], 0.0, 0.0).is_err());
        }
        assert!(normal_or_zero(0.0) && normal_or_zero(-0.0));
        assert!(normal_or_zero(f32::MIN_POSITIVE));
        let error = model
            .verify_prediction_pair(&[f32::MAX], f32::MAX, f32::MAX)
            .unwrap_err();
        assert!(error.reason.contains("overflow"));
        let mut tiny = model;
        tiny.hidden_weight[0] = f32::MIN_POSITIVE;
        assert!(tiny
            .verify_prediction_pair(&[0.5], 0.0, 0.0)
            .unwrap_err()
            .reason
            .contains("underflow"));
    }

    #[test]
    fn independent_reference_rejects_matching_corruption_and_unit_drift() {
        let model = identity();
        let expected = model.predict(&[0.125]).unwrap();
        model
            .verify_prediction_pair(&[0.125], expected, expected)
            .unwrap();
        for wrong in [expected + 0.01, expected * 10_000.0, expected / 0.0001] {
            let error = model
                .verify_prediction_pair(&[0.125], wrong, expected)
                .unwrap_err();
            let details = error.diagnostics.as_ref().unwrap();
            assert!(details.absolute_difference > details.error_bound);
            assert!(error.to_string().contains("absolute_difference="));
            assert!(error.to_string().contains("reference=["));
            assert!(model
                .verify_prediction_pair(&[0.125], wrong, wrong)
                .is_err());
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn daz_and_rounding_domain_checks_run_in_an_isolated_process() {
        const CHILD: &str = "MONDAY_PARITY_FP_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("model::numerical::tests::daz_and_rounding_domain_checks_run_in_an_isolated_process")
                .arg("--test-threads=1").env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        // Only this subprocess changes its thread-local FP control word.
        #[allow(deprecated)]
        unsafe {
            use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
            let original = _mm_getcsr();
            _mm_setcsr((original & !0x6000) | 0x8040); // RN, DAZ and FTZ.
            assert!(round_to_nearest());
            let tiny = std::hint::black_box(f32::from_bits(1));
            assert!(!normal_or_zero(tiny));
            assert_eq!(exact_f32_value(tiny), (2.0_f64).powi(-149));
            let model = identity();
            assert!(model.verify_prediction_pair(&[tiny], 0.0, 0.0).is_err());
            let mut zero = model.clone();
            zero.output_weight[0] = 0.0;
            assert!(zero.verify_prediction_pair(&[1.0], tiny, tiny).is_err());
            // Round-toward-zero cannot use an RN-derived certificate.
            _mm_setcsr((original & !0x6000) | 0x6000);
            assert!(!round_to_nearest());
            assert!(model.verify_prediction_pair(&[1.0], 1.0, 1.0).is_err());
            _mm_setcsr(original);
        }
    }
}
