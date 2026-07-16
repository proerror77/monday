# PM5D Strategy State Special Cases

Date: 2026-04-22

Phase 3 extracted the duplicated PM5D helpers that were behavior-identical across
multiple strategy implementations:

- `strategies/common/guards.rs`: active order detection.
- `strategies/common/settlement.rs`: explicit settlement plus spot/price-to-beat fallback.
- `strategies/common/event.rs`: basic event window with UP/DOWN token helpers.
- `strategies/common/quote.rs`: basic bid/ask/timestamp quote state.
- `strategies/common/holding.rs`: basic token/direction/entry-time holding state.

The remaining local state shapes are deliberate special cases, not accidental
Phase 3 leftovers.

## Event Window Special Cases

- `directional.rs` and `directional_bayes.rs` keep local `EventWindow` because
  their entry logic is anchored on `open_price`, not only `price_to_beat`. The
  open price can be initialized from `price_to_beat` or backfilled from the first
  spot tick when an event arrives before spot data.
- `three_layer.rs` keeps local `EventWindow` because its event state is tightly
  coupled to regime and LOB confirmation scoring. It uses the shared settlement
  fallback but should not be forced into the basic event shape until the
  three-layer scoring boundary is redesigned.

## Quote State Special Cases

- `directional.rs` and `directional_bayes.rs` keep local quote state while their
  event/open-price flow remains local.
- `three_layer.rs` keeps local quote state because it combines PM quote data with
  LOB, microprice, and drift-confirmation state.
- `mean_reversion.rs` keeps local quote state because it has no timestamp field
  and is paired with return-buffer/volatility state.

## Holding State Special Cases

- `diff_enhanced.rs` keeps local holding state because it tracks `entry_price`,
  `peak_diff`, and `peak_prob` for trailing exits.
- `prob_chase.rs` keeps local holding state because it tracks `entry_deviation`
  for probability chase exits.
- `sweep.rs` keeps local holding state because its direction enum and tail-window
  expiry behavior are local to the strategy.

Future cleanup should migrate these only when the shared type can represent the
strategy-specific behavior without adding dead fields or weakening tests.
