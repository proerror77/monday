# Binance Futures Local Orderbook

Ploy's current Binance LOB collector records partial depth snapshot evidence.
That is useful for directional and liquidity diagnostics, but it is not a
sequence-correct futures local book.

Promotion-grade FactorEvolve LOB research requires:

- REST or equivalent snapshot with `last_update_id`.
- Incremental diff-depth updates with first/final update IDs.
- First diff validation:
  `first_update_id <= last_update_id + 1 <= final_update_id`.
- Later diff validation:
  `previous_final_update_id + 1 == first_update_id`.
- Gap and out-of-order handling that fails closed instead of silently
  continuing.

Unsequenced partial depth must not be used as proof of queue position,
passive-fill feasibility, or exact executable depth. Runtime promotion still
requires Polymarket full-depth execution evidence and official settlement for
PM5D binary-option strategies.
