# Staggered Arb Strategy: Core Idea, Entry/Exit, State Machine

## Core idea
- Trade prediction-market binary pairs (`UP`, `DOWN`) as a two-leg cycle.
- Buy Leg1 on the predicted side early in the event.
- Buy Leg2 on the opposite side when sum/edge conditions are met.
- Treat cycle as complete (`merge` close reason) once both legs are filled.
- Keep the strategy in strict single-cycle mode: do not open a new cycle while one is in-flight.

## State machine
1. `Idle`
2. `EntryGateEval`
3. `Leg1Submit` (live) / `Leg1Filled` (paper)
4. `Leg1Filled`
5. `Leg2GateEval`
6. `Leg2Submit` (live) / `Merged` or `ForcedComplete` (paper)
7. `Merged` or `ForcedComplete`
8. back to `Idle`

## Live-path overlay

The coarse state machine above describes the strategy's position lifecycle, not
the lower-level exchange lifecycle.

- Paper mode collapses `Leg1Submit` and `Leg2Submit` into immediate synthetic
  fills, so the strategy transitions almost directly between `Leg1Filled`,
  `Merged`, `ForcedComplete`, or `Settled`.
- Live mode inserts an order-tracking overlay between each submit state and the
  eventual position transition.
- That overlay exists so the strategy can survive delayed acknowledgements,
  partial fills, cancels, and late terminal exchange updates without losing the
  event lock or position lock too early.

## Live order-track lifecycle

The live path has more states than the coarse
`Leg1Submit -> Leg1Filled -> Leg2Submit` summary above because each submitted
order is tracked as a `LiveOrderTrack`.

- On `SubmitIntent`, the strategy creates a local order track keyed by the
  client order id and marks either the Leg1 event lock or the Leg2 position
  lock as pending.
- Each `LiveOrderTrack` stores the event/position identity, leg number,
  submitted price and shares, optional `exchange_order_id`, optional
  `cancel_requested_at`, and `acknowledged_filled_qty`.
- Early exchange updates attach the exchange order id and advance cumulative
  fill progress without immediately releasing the pending lock.
- Partial fills stay pending while cumulative shares are updated in place.
- Leg1 partial-fill cancel/remainder flows keep the track alive until the final
  cancel/fill result decides whether the cycle promotes to `Leg1Filled`, is
  abandoned, or must wait for a late terminal update.
- Stale live orders are cancelled after roughly 30 seconds if no terminal update
  arrives.
- Orphaned tracks are archived after roughly 90 seconds so late exchange updates
  can still reconcile without blocking new cycles forever.
- Terminal live updates clear `pending_leg1_events` or
  `pending_leg2_positions`, archive or remove the track, and then allow the
  next cycle to start safely.

## Foreground vs managed live path

There are two live execution surfaces, but both now route through coordinator
ingress rather than direct exchange execution.

- Foreground live: `cli strategy ... --foreground` runs through
  `src/cli/strategy/runtime_ops/foreground.rs` and
  `foreground_submit.rs`. It requires a `deployment_id`, converts
  `StrategyOrderIntent` into coordinator ingress payloads, and submits through
  the same risk/admission path as other live traffic. It is operator-driven and
  does not own managed-runtime persistence or deployment bootstrap.
- Managed live: deployment-driven runtime execution runs through
  `src/coordinator/strategy_runtime.rs`,
  `src/coordinator/strategy_runtime/actions/order_commands.rs`,
  `src/strategy/runtime_order.rs`, and deployment admission in
  `src/coordinator/admission/deployments.rs`. It persists runtime orders,
  converts strategy intents into canonical `OrderIntent`s, and submits through
  the managed runtime control loop.

Operationally the difference is:

- Foreground live is a local operator surface for running one strategy process
  interactively with the same coordinator ingress checks.
- Managed live is deployment-owned: runtime startup, deployment resolution,
  runtime-order persistence, and order-update reconciliation all live in the
  coordinator-managed strategy runtime.

## Entry gates (Leg1)
- Balance pause gate (`balance_pause_until`) when recent balance failures happened.
- Active cycle lock (`has_active_cycle`).
- Time remaining gate (`min_time_remaining_secs`).
- Entry timing gate (`entry_after_start_max_secs`).
- Market quote presence (`up_ask`, `down_ask`) and min ask floor (`min_ask_price`).
- Pair sum gates:
- `current_sum >= min_entry_sum`
- `current_sum < max_initial_sum`
- Probability and directional conviction:
- valid `open_price` anchor (`s0`)
- `|p_hat - 0.5| >= direction_threshold`
- Price displacement gate from event open anchor.
- Greeks confirmation gates (if enabled): gamma/theta/delta/vega/d2/fair-value consistency.
- Binance L2 OBI confirmation gate (freshness + direction alignment).
- Leg1 price cap (`max_leg1_price`).
- Leg2 feasibility gate (`merge_target_sum - leg1_price > 0`).
- Cooldown gate (`cooldown_secs`).
- Concurrency and duplication gates:
- `max_concurrent_positions`
- no duplicate open event
- no duplicate pending Leg1 event
- Per-event limit gate (`max_trades_per_event`).
- Sizing / reserve gates:
- non-zero shares
- reserve guard (`min_balance_usd`)

## Exit gates (Leg2)
- Skip if Leg2 order is already pending.
- Final-minute no-trade gate (`no_trade_last_secs`).
- Opposite-side quote presence and min ask floor.
- Minimum delay gate (`min_leg2_delay_secs`).
- Close triggers:
- `current_sum <= merge_target_sum`
- fee-adjusted profit `>= min_profit_target`
- any fee-adjusted positive profit
- forced stop-loss (`max_leg1_loss`)
- forced timeout (`wait_deadline`)
- forced time safety (`time_remaining < min_time_remaining_secs`)

## Expiry and settlement branch

Live cycles do not only terminate via `Merged` or `ForcedComplete`.

- If the event expires while a live cycle is still open, lifecycle handlers can
  push the position into a settlement path.
- That path records a settled/live-settlement outcome and clears in-flight
  tracking so the strategy does not strand pending state across the expiry
  boundary.
- Paper mode collapses this faster because fills are simulated immediately, but
  live mode depends on order-update reconciliation and expiry-driven cleanup.

## Runtime observability (new)
The strategy state metrics now expose gate counters:
- Entry counters: `entry_gate_*`
- Leg2 counters: `leg2_gate_*`

Useful keys include:
- `entry_gate_entry_accepted`
- `entry_gate_active_cycle_lock`
- `entry_gate_entry_window_expired`
- `entry_gate_direction_strength_below_threshold`
- `entry_gate_obi_not_confirmed`
- `entry_gate_leg1_price_above_cap`
- `entry_gate_reserve_guard`
- `leg2_gate_final_minute_block`
- `leg2_gate_min_leg2_delay`
- `leg2_gate_leg2_order_pending`

These counters are cumulative since strategy start and are meant to identify the dominant bottleneck reducing trade count.
