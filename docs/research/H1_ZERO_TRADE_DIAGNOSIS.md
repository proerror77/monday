# H1 zero-trade diagnosis and next experiment decision

## Conclusion from the completed study

The real study `h1-hf-hold-20260912-0500-1000-r3` completed under source
`d32941bd2cb608c207d88a3aa1ab038f1d8b45fb`. Its three Ridge arms and both
seeds reached independent calendar validation. All six runs produced zero
entries and no selected candidate. This is a retained negative result, not a
training failure or a successful trading strategy.

[Terminal evidence and resource cleanup](https://github.com/proerror77/monday/issues/1182#issuecomment-5692551149)
records the result hashes and closure. The calendar was September 12,
05:00–08:00 UTC development, 08:00–09:00 validation and 09:00–10:00 sealed.
The approved five-hour calendar superseded the older eight-hour proposal.
There was no imputation in this slice. Sealed evaluation and live trading
were not opened.

| Horizon | Development absolute-label P95, bp | Development labels above their own row cost | Development forecast maximum, bp | Independent-validation forecast maximum, bp | Validation IC |
|---|---:|---:|---:|---:|---:|
| 5 seconds | 0.932753 | 10 / 10,795 (0.0926%) | 0.974169 | 0.139572 | 0.044015 |
| 10 seconds | 1.516028 | 20 / 10,790 (0.1854%) | 0.578735 | 0.189252 | 0.062644 |
| 30 seconds | 2.513473 | 48 / 10,770 (0.4457%) | 1.128682 | 0.545998 | 0.112340 |

The unchanged entry threshold is about 5.01 bp. In development its minimum
was 5.012932 bp. The contract has a 5 bp base threshold before the observed
spread: two sides of 2 bp fees plus two sides of 0.5 bp latency cost, with no
rebate or extra slippage. Each validation forecast maximum is below that base
threshold. The same is true of the development forecast maxima. This proves
the immediate zero-entry cause: no forecast crossed the existing cost gate.
It does not prove why the conditional forecasts were small.

The labels in this table overlap in time. Forty-eight rows are not 48
independent or executable trades. Absolute forward mid-price return is an
ex-post opportunity description, not a signal or realized P&L. A low P95
cannot rule out a rare-event strategy; a positive IC cannot establish a net
edge. Both seeds used the same market period and produced the same reported
validation metrics, so they are not independent market replications.

## Checks on the fitting and prediction path

Source inspection and focused regression tests establish:

- The target is the fractional `forward_mid_return` label. Ridge fits this
  target directly; it has no target standardization transform to invert.
- Feature means and population standard deviations use the training fold only.
  The intercept is the training target mean. Validation rows do not contribute
  to the fitted means, scales or coefficients.
- The governed baseline uses `ridge_l2 = 1e-6`. The solver adds this to the
  diagonal of the summed standardized Gram matrix, not a mean-loss matrix.
  A nonconstant standardized column's diagonal is its training sample count.
  This does not support assuming a large global shrinkage penalty; correlated
  directions still require separate analysis.
- Evaluation and portable inference use the same standardized Ridge predictor.
  Feature-unit changes and target conversion to bp followed by inverse
  conversion preserve predictions. JSON save/load preserves prediction bits.
- A controlled linear target exceeding 5 bp remains capable of passing the
  unchanged native entry gate after fitting and loading. There is no blanket
  amplitude clamp preventing all Ridge trades.

These checks do not establish that the real model is correctly calibrated or
that its factors anticipate the rare moves. No Ridge coefficient, return unit,
entry threshold, cost assumption or holding rule is changed by this correction.
Normalizing and then restoring a Ridge target is not a justified way to enlarge
its forecasts.

## Reporting correction

The reporting change is tracked by [#1196](https://github.com/proerror77/monday/issues/1196).

The old lightweight calendar report exposed IC, maximum forecast and median
cost. It did not retain a paired forecast/label calibration summary, a per-row
cost-gate count, or the distinction between an eligible signal and a tail row.
Those omissions made zero-trade attribution unnecessarily ambiguous.

New calendar runs emit `results/calendar-prediction-diagnostics.json`:

- source validation hash and decision-policy hash;
- evaluated, horizon-eligible and known-tail row counts;
- eligible labels and forecasts above each row's entry cost, nonzero opening
  signals, and forecasts blocked by a long-only direction constraint;
- nonzero position rows, explicitly separate from opening signals and fills;
- means, standard deviations, MAE and MSE in explicit bp units, MSE relative to
  the zero predictor, and a descriptive calibration slope;
- fixed forecast/cost-ratio buckets `[0,.25)`, `[.25,.5)`, `[.5,1)`, `[1,2)`
  and `[2,infinity)`. Each retains its observation count, mean directional
  forward label, mean entry threshold and directionally correct labels above
  cost. These are overlapping conditional observations, not strategy returns.

Calibration uses all already-admitted validation rows. Gate and bucket counts
use only rows whose full holding horizon fits the known evaluation endpoint.
The exact entry gate is strict; equality with cost does not enter. Ratio
buckets are descriptive, so the `[1,2)` bucket includes equality. Undefined
calibration or zero-baseline ratios remain null. Zero-cost eligible rows are
counted separately and excluded from cost-ratio buckets.

No fit, threshold search or promotion is performed. The native independent
readback reconstructs the diagnostic from the admitted rows and fitted-weight
validation; missing or altered diagnostics fail the new producer's readback.
The lightweight Campaign report copies the verified small artifact. Original
R3 bundles are immutable and retain their original format and reader source;
this change neither rewrites them nor pretends their missing diagnostics exist.

## What remains unknown

The archived lightweight R3 summaries do not contain the paired per-row
forecast/label information needed to calculate calibration or rare-event
anticipation retrospectively. The new diagnostics have passed controlled
software tests; no new real-data diagnostic result is claimed here.

A bounded ACK-only readback can compute these statistics from the existing
frozen inputs and fitted models without new model trials. Its resource window,
input identities, exposed view and output archive must be recorded separately.
A larger IC alone must not select another horizon or authorize H2.

## Next experiment decision

1. **First deliver the real-data attribution.** Reuse the immutable fitted
   models and admitted views in ACK. Report calibration, per-row gate counts
   and rare-event buckets without changing predictions. If units or prediction
   parity fail, stop and repair that implementation before comparing models.
2. **If implementation checks pass, choose one new hypothesis.** For example,
   assess whether a longer fixed horizon offers predictable moves beyond the
   same costs, or whether point-in-time order-flow conditioning identifies
   the rare current-horizon moves. These are alternatives for review, not an
   approved search over both. Record all inspected alternatives.
3. **Freeze the target and evaluation before another run.** Specify instrument,
   horizon, opening/holding behavior, unchanged cost model, feature availability,
   baseline, independent time splits, trial family, stop rules and finite resource
   window. Use multiple new market periods; already inspected validation cannot
   become a fresh test. Keep the original sealed view closed.
4. **Authorize the concrete run only after this decision.** Original accounting
   remains 138 newly consumed plus 46 old pending, or 184 total. No refund,
   clock reset, training rerun, H2, market-making or live activation is implied
   by this code and diagnosis change.
