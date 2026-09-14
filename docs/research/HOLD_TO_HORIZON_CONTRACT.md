# H1 holding and evidence contract

Implementation and validation are in progress under #1182. No H1 run may start
until this contract is implemented, tested, reviewed and merged.

## Clock and position

The horizon uses the signal decision clock, preserving `forward_mid_return`.
The signal occurs at `t`; its exit decision is scheduled at `t+h`. Both IOC
legs use the same frozen order-arrival latency and observed-book rule. Record
decision, order-arrival and fill times; report actual holding duration and
latency deviations instead of calling the planned duration an executed fact.

Each arm permits at most one position in the instrument. New opening signals
are ignored while holding. Entry quantity remains fixed; changing prices or
predictions cannot create rebalancing orders. At expiry, close first; another
entry is allowed at a later decision tick, after the previous position is flat.
Unfilled entry intents do not prove a position or a completed trade. An
unresolved exit prevents a new entry. No overlap or same-tick close/reopen
netting may hide costs.

Do not open a position whose full horizon would exceed a predeclared evaluation
window. The first implementation rejects an observation gap or a series
boundary inside an evaluation range; it never uses knowledge of a later gap
to avoid an earlier entry. Unavailable exit quotes, delayed
or incomplete fills and safety exits must be retained as explicit exceptions.
They cannot be presented as clean horizon-complete trades. Safety boundaries
remain active; an exception must not fabricate a fill or profitability result.

## Prices and costs

Keep the existing `2x` one-way entry cost gate, sizing rule, fees, rebates,
latency and slippage assumptions. Apply that gate at entry only. Close using the
executable side of the observed book, retaining actual IOC fills and costs.

The quantity ledger marks the same entry quantity through the episode. Its
price P&L sums to that quantity times the cumulative price change. Quote
crossing is counted once on entry and once on exit; fees apply to actual fill
notional. Do not add spread again after charging it through executable prices.
The approximate decision-time ledger and observed event replay must be labeled
separately when order-arrival latency changes execution prices.

## Data and three-arm comparison

Before any label inspection, commit the development, validation, independent
selection and sealed-test views. Label prechecks use only the authorized
development/validation view, including label-maturity boundaries. Test data
cannot select the horizon, factors, transformations or rules. This follows the
[data leakage guidance](https://scikit-learn.org/stable/common_pitfalls.html#data-leakage).

Verify data coverage and prior research usage for `2026-09-11 02:00-10:00 UTC`.
An apparently new calendar is not automatically an unseen test. Do not reuse
the already inspected `2026-09-10` eight-hour window for this H1 comparison.

Report absolute cumulative-return quantiles for 5/10/30 seconds, the unchanged
round-trip cost comparison, and counts/fractions of opportunities above cost.
The user explicitly chose to evaluate all three arms: the precheck is
descriptive and does not cancel an arm. P95 below cost is not falsification;
P95 above cost is not evidence that a model can identify the opportunities.

Fit/evaluate Ridge for all three horizons, with comparable exposure limits,
costs, data views and budgets. Account for comparing three hypotheses in the
statistical family while charging only actual work. Preserve every arm's
negative result. A weak arm does not cancel another arm; distinguish research
outcomes from shared infrastructure, identity and numerical failures.

The old holding mismatch has not been proved to cause historical zero trades.
Models that never crossed the entry threshold remain separate negative
evidence. H2, H3 and sequence-residual models are later candidates chosen from
H1 evidence, not automatic expansion of this run. Market making is excluded.

## Authority and execution

After code/validation/merge prerequisites, the user has requested new real H1
experiments. Execute only through native Campaign freeze, finalize, dispatch
submit and generated pre-holdout campaign-execute. Use authenticated preparation
reuse; keep bulk data, models, verification and readback in ACK/OSS. Retain
signed controls and cumulative accounting. The prior six-group budget is
exhausted and cannot be reset; genuine available authority must cover the new
run. A code merge, precheck or submission does not prove a terminal experiment.
