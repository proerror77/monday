# H1 holding and evidence contract

The holding implementation and focused regression checks are tracked by #1182
and PR #1183. H1 starts only after reviewed code and current-source CI are
merged, with its workflow and signed budget prerequisites satisfied.

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
Unfilled entry intents do not prove a position or a completed trade. Native
replay retains each pre-holding opening signal, with entry disabled near the
known window end. A zero-fill IOC clears the tentative episode; a later signal
uses its own clock and expiry. FormulaStrategy clears a rejected entry only
after a terminal execution receipt bound to its own acknowledged order.
An
unresolved exit prevents a new entry. No overlap or same-tick close/reopen
netting may hide costs.

Do not open a position whose full horizon would exceed a predeclared evaluation
window. The first implementation rejects an observation gap or a series
boundary inside an evaluation range; it never uses knowledge of a later gap
to avoid an earlier entry. Unavailable exit quotes, delayed
or incomplete fills and safety exits must be retained as explicit exceptions.
Holding summaries retain incomplete exit orders and delayed exit decisions;
either blocks replay promotion even if cleanup eventually flattens inventory.
A completed cleanup cannot open another position on that same tick. The next
eligible tick may use a fresh opening signal.
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

Before any label inspection, commit the development, independent validation
and sealed-test views. H1 label prechecks use only development, including
label-maturity boundaries. Test data
cannot select the horizon, factors, transformations or rules. This follows the
[data leakage guidance](https://scikit-learn.org/stable/common_pitfalls.html#data-leakage).

Verify data coverage and prior research usage for `2026-09-11 02:00-10:00 UTC`.
An apparently new calendar is not automatically an unseen test. Do not reuse
the already inspected `2026-09-10` eight-hour window for this H1 comparison.

Declare `calendar` on each Ridge research plan before inspection: `start`,
`develop_end`, `validation_end`, and `end` are UTC timestamps. For this H1 they
are September 11 at 02:00, 06:00, 08:00, and 10:00 respectively. Intervals are
half-open and use feature decision availability. Native admission resolves the
row boundaries from actual clocks, preserving warm-up and label-tail exclusions;
the worker resolves them again. A row count cannot substitute for this calendar.
Admission allows at most two initial buckets (bucket alignment plus the previous
book sample) and `h + 1` trailing buckets for label maturity/end alignment. It
rejects any larger endpoint truncation. The retained calendar must also have every
interior observation bucket; a gap fails calendar admission before any fitting.
The H1 plan requires the exact registered
5/10/30-second purge/embargo tuples, seeds 7/11, unchanged snapshot GP templates,
and the complete 138-trial comparison-family correction.

Freeze verifies complete raw blobs that overlap the requested receive window,
including boundary blobs. The PIT materializer then emits only window-contained
decisions whose labels mature before the window end, retaining full source hashes.
It must not discard a five-minute boundary blob because its final receive timestamp
is a millisecond beyond the nominal window, or quietly accept the resulting gap.

Three expanding search folds remain entirely inside development. Their equal
validation lengths are derived from the available development rows, preserving
the declared horizon-specific purge/embargo. The final already-fitted Ridge fold
is evaluated on the separate 06:00–08:00 validation view, with its training cutoff
recorded. This does not refit on validation, add a model trial, or inspect sealed
labels. Retain this report even when the development economic gate failed.

Before `campaign-prepare` or `campaign-freeze`, run the ACK-native
`mission campaign-precheck --feature FEATURES --materialization MATERIALIZATION
--research-plan PLAN --output REPORT --research-plan-out CHECKED_PLAN`.
It reports only development labels mature strictly before `develop_end`, with
absolute-return quantiles, count/fraction above each row's unchanged round-trip
cost, median spread, and median/min/max entry threshold. It emits an immutable
report and a plan binding the report's source/input/calendar/cost identities.
Preparation recomputes the report once during input admission; an authenticated
preparation snapshot can reuse that verified report without reopening inputs.

The new root grant must explicitly bind `fixed_calendar_validation_pre_holdout`
view exposure. A prior grant reserving an unreported independent-selection view
cannot authorize this report. Reporting validation does not grant final evaluation;
the sealed 08:00–10:00 view stays closed. Calendar negatives cannot produce a
selected pre-holdout candidate. The result bundle retains `calendar-validation.json`
and independent readback reevaluates it from the same fitted weights and inputs.

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

A completed fixed holding comparison returns `fixed_comparison_complete` from
learning. It cannot automatically change the entry policy or create H2/H3.

Fill prices, quantity and holding-time evidence here come from event replay.
Live runtime activation follows its separately enforced execution contract.
