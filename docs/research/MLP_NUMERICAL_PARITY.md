# MLP numerical parity certificate

The CEX check compares two evaluations of the same raw-return f32 model. It
checks a higher-precision reference and derived forward-error bounds, rather
than choosing a larger empirical tolerance. It does not change either output,
training, model parameters, update/gradient/convergence controls, costs, or the
wire format of a saved model.

## Which parameters and units are compared

`train_parsed_contract_model` performs `fold_target_inverse` before returning a
`TrainedContractModel`. That folds the training-only target scale and mean into
the output tensor using f64 arithmetic followed by a single f32 conversion.
`TrainedContractModel::predict` evaluates that post-fold tensor. Its
`export_parameters` reads the same tensor and verifies an exact semantic digest
covering tensor names, dimensions and f32 bit patterns. The portable path and
this certificate use those verified post-fold f32 values and the same converted
f32 input vector.

There is no separate inverse transform after the compared Burn forward pass.
This certificate does **not** claim that a normalized f32 forward followed by an
inverse transform is bit-identical to folding the transform before inference.
The existing training/persistence/raw-return tests cover that separate unit
contract. Export identity and frozen feature order checks remain necessary:
approximately matching predictions cannot replace them.

## Supported arithmetic and reference

The relative-roundoff model is IEEE roundTiesToEven (RN), with `u = 2^-24` for
f32. Runtime f32/f64 tie probes reject other scalar rounding directions without
changing the environment. The supported backend is the existing f32 Burn
NdArray linear/ReLU computation: ordinary multiply/add, fused multiply-add, and
reordered/tree dot-product reductions. Reduced-precision/approximate kernels
are not assumed to satisfy this contract.

Original inputs and parameters must be finite, normal f32 values or signed
zero. Their classification uses integer bits, since DAZ can make a nonzero
subnormal compare equal to zero. Nonzero subnormal original operands are
rejected. Observed subnormal outputs are promoted by decoding their bits, so
DAZ cannot silently turn the value being checked into zero.

An outward f64 interval independently evaluates the mathematical two-layer
model. Products of two f32 values are exactly representable in f64 (at most48
significand bits). Subsequent f64 addition uses the error-free TwoSum residual;
multiplication uses the fused residual `a.mul_add(b, -a*b)`. An endpoint is moved
one representable f64 step only when the residual sign requires it. Exact
operations retain point intervals; they are not padded with an arbitrary
absolute epsilon. ReLU maps both interval endpoints through `max(0, x)`.

These error-free transforms require RN and no f64 overflow/underflow. Under the
checked normal-f32, two-layer and one-million-parameter limits, f32 products and
f64 sums/residuals remain far inside the f64 normal exponent range; f32 overflow
is rejected before a later layer could approach f64 overflow. The routines are
private to this bounded domain, not general interval primitives for arbitrary
f64 input. A finite f64 reference interval is required at the final check.

## Forward-roundoff bound

For a dot product with `n` multiplied terms and a bias, use the conservative
operation count `k = 2n+1`. It counts every multiplication and accumulation,
including bias addition. FMA and shallower reductions use no more nontrivial
roundings. Define:

```
gamma(k) = k*u / (1-k*u)
tau      = f32::MIN_POSITIVE
rho      = 4*tau
R(S,k)   = gamma(k)*S
A(k)     = k*rho / (1-k*u)
E(S,k)   = R(S,k) + A(k)
```

`S` is an upper bound on the sum of absolute terms. `rho` conservatively covers
flushing of two subnormal addition operands, one arithmetic rounding, and a
subnormal result. More explicitly, two input flushes contribute at most2tau;
RN rounding is bounded by `u*|z| + tau/2`, and result flushing adds at most tau.
Accounting for the changed operands in the relative term adds at most
`2*u*tau`, giving `(3.5+2u)*tau < 4tau`. Original multiplication operands are
normal/zero; a possibly flushed hidden activation is propagated separately
below. FMA combines fewer operations and is covered by the conservative count.

The usual product-of-rounding-factors bound gives `gamma(k)*S`; additive errors
from at most `k` operations are amplified by at most `1/(1-k*u)`, yielding
`A(k)`. The denominator is rounded downward, divisions upward, and all
nonnegative magnitude sums/products upward. No estimated bound is rounded
inward. `1-k*u` must remain positive, and `S+E` must fit f32 to certify that
intermediate overflow is impossible for the admitted reduction orderings.

For each hidden unit, with exact real preactivation `z_j` and
`h_j = ReLU(z_j)`:

```
S1_j = |hidden_bias_j| + sum_i |input_i * hidden_weight_ij|
E1_j = E(S1_j, 2*input_dim+1)
```

Here `E(S,k)` takes an operation count; the implementation's `dot_error` accepts
`n` and computes `k` once. ReLU is1-Lipschitz, so the same absolute error bounds
the hidden activation. If the certified preactivation upper bound is nonpositive,
all admitted evaluations produce exactly zero and its propagated error is zero.
Otherwise an additional `tau` accounts for a subnormal hidden activation being
flushed when read by the output multiplication. Underflow-dominated active
arithmetic (`A > R`) is outside the supported domain and is rejected rather
than used to justify a broad tolerance.

With these effective hidden errors `D_j`, and interval upper bounds `H_j` for
`|h_j|`:

```
S2          = |output_bias| + sum_j |output_weight_j| * (H_j + D_j)
propagation = sum_j |output_weight_j| * D_j
final_error = propagation + E(S2, 2*hidden_dim+1)
```

The multiplication/addition rounding acts on the actual perturbed hidden values,
which is why `S2` includes `D_j`. The f64 reference uncertainty is retained by
expanding the reference interval endpoints by `final_error`; it is not omitted
from the comparison.

## Acceptance and rejection

Both observed outputs must independently lie in the reference interval expanded
by the finite derived bound. Equal but jointly corrupted outputs are insufficient.
Unsupported inputs, rounding directions, overflow, nonfinite bounds and
underflow-dominated arithmetic are rejected.

A bound at least as large as the ideal sum of absolute output terms is treated
as a vacuous certificate for deviations from the reference. There is a narrow
stronger acceptance rule: when the independent reference is an exact point and
**both** observed outputs equal that point, no roundoff allowance is used. This
accepts, for example, normal inputs `[1,1]`, weights `[1,-1]`, a zero bias and
ReLU producing exact zero. The reported `exact_reference_agreement` is true and
the certified interval is that point, while `error_bound` still records the
general forward bound. A wide f64 interval produced by cancellation cannot use
this exception to hide a shared wrong output.

Rejection diagnostics retain both outputs, absolute difference, reference
interval, derived bound when available, model/request identity, seed/fold,
validation row and target scale. No raw feature, label or tensor values are
logged. Certified approximation is not a claim that arbitrary unit changes are
observable on a zero signal; exact tensor/transform identity remains the unit
boundary.

## Regression evidence

A finite independently generated14-by8 model reproduced ordinary Burn/portable
f32 disagreement beyond the old heuristic threshold. The regression retains
that observed pair and also executes the current host's real Burn kernel under
the existing backend lock. It applies the production inverse-folding function,
checks exact post-fold tensor hashes with zero and nonzero target means, and
checks that portable prediction bit patterns remain unchanged.

Other cases reject deliberate output and unit perturbations, common-mode wrong
outputs, wrong dimensions, nonfinite values, unsupported subnormal operands,
possible overflow and vacuous wide-reference cases. Exact-zero and near-ReLU
boundaries have dedicated tests. On Linux amd64, an isolated child process
exercises actual DAZ/FTZ flags and non-RN rejection without changing another
thread's floating-point environment. Existing training-prefix, validation-label
isolation and train/save/load/raw-unit tests remain applicable.

These are software checks. They do not reopen or relabel the failed attempts in
Study #1170, and they do not authorize another real-data run or change its c348
source/grants.
