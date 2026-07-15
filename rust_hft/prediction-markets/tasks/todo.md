# Monday Migration Tracker

## Goal

Migrate the imported PLOY compatibility code into Monday's canonical market-family and venue modules without creating a second execution authority or enabling live trading.

## Ownership

- Main migration session: source import, provenance, authority docs, CI, validation, and archive handoff.
- Review agents: read-only standards and spec review of the completed Monday diff.

## Tasks

- [x] Import PLOY `main` at `8ce4e0f150173a44030294101f4b1371cbdf80bc`.
- [x] Preserve the seven local-only readiness documents as stale historical material.
- [x] Replace standalone agent/session state with Monday-local instructions.
- [x] Add and validate dedicated PLOY CI at the Monday repository root.
- [x] Pass repository secret scanning and focused Rust/TypeScript validation.
- [x] Complete standards/spec review and address findings.
- [ ] Merge the Monday migration PR.
- [ ] Redirect and archive the former PLOY repository.

## Polymarket public data on Monday ECS

- [x] Add a no-op strategy that consumes market data without emitting orders.
- [x] Add a fail-closed ECS config and hardened systemd service.
- [x] Build and install the Linux runner artifact.
- [x] Enable the BTC/ETH/SOL/XRP 5-minute/15-minute service and verify event rollover, token-mapped quotes, and zero intents/fills.
- [x] Archive hourly Polymarket tapes to OSS with manifest and readback verification.
- [x] Retain full visible CLOB depth in one-second snapshots for all seven configured crypto assets.
- [x] Add a collection-only metadata/trade/settlement tape and verify a complete OSS artifact.
- [x] Port the long-lived Polymarket reference collector and raw-tape uploader to Rust, and retire their Python runtime entry points.
- [x] Coalesce Polymarket price-change batches before publishing quotes so transient crossed intermediate states are never recorded.

## Research framework cleanup

- [x] Adapt the prediction Loop to binary/digital-option factors by exposing a point-in-time Chainlink endpoint-probability component without duplicating the existing CEX model.
- [x] Split every prediction walk-forward lane by event end so unresolved events cannot cross train/test boundaries.
- [x] Run `ploy-research` library tests in the active root CI workflow.
- [x] Preserve Monday's Factor Bank, Search Protocol, artifact/experiment/factor stores, `factorctl`, active DSL, manifest, and harness contracts while moving the prediction research loop into PLOY Rust.
- [x] Pass governed mission context and bounded prior verdicts into LLM proposals, with code-enforced mutable scope.
- [x] Gate prediction OOS evidence with Brier score, log loss, calibration error, settlement PnL, and event-level capacity.
- [x] Route typed probability-blend candidates into the prediction evaluator and emit candidate-specific loop feedback.
- [x] Add separate governed BTC and SOL five-minute mission templates; reject mixed-symbol missions and unresolved provenance.
- [x] Add a resumable PLOY prediction LoopRun with mission candidate/call/time budgets and content-addressed iteration evidence.
- [x] Implement the complete authoritative prediction LoopRun and tests in `ploy-research` Rust without introducing a Python runner; retire the Binance-to-opening-reference backfill helper.
- [x] Recompute terminal provisional models from typed Brier, log-loss, ECE, settlement-PnL, and conservative-capacity metrics recorded in append-only feedback; require a separate sealed snapshot for final keep.
- [x] Recover LLM provider, model, and usage provenance from the same content-addressed response envelope after a crash.
- [x] Require explicit, non-empty Chainlink reference plus Binance spot/aggTrade/L2 snapshot surfaces, replayed by `received_at`, before a BTC/SOL LoopRun starts.
- [x] Retire the legacy Binance-to-`price_to_beat` backfill path so Binance cannot override Chainlink opening/settlement authority.
- [x] Enforce mission, symbol, horizon, and exact snapshot provenance again at the Rust evaluator boundary.
- [x] Keep feedback mission-local and return no-OOS plus conservative-capacity failure reasons for every proposed blend.
- [x] Add a governed Rust-only Burn binary probability lane with event-disjoint snapshot-bound inputs and content-addressed model evidence.

## Research framework cleanup

- [x] Complete the repository-wide Rust-only cutover: remove tracked Python and PyTorch/libtorch paths, add native Burn training for both research lanes, and pass focused Rust validation.

- [x] Adapt the prediction Loop to binary/digital-option factors by exposing a point-in-time Chainlink endpoint-probability component without duplicating the existing CEX model.
- [x] Split every prediction walk-forward lane by event end so unresolved events cannot cross train/test boundaries.
- [x] Run `ploy-research` library tests in the active root CI workflow.
- [x] Remove unused Monday research-store/search crates while preserving active DSL, manifest, and harness contracts.
- [x] Pass governed mission context and bounded prior verdicts into LLM proposals, with code-enforced mutable scope.
- [x] Gate prediction OOS evidence with Brier score, log loss, calibration error, settlement PnL, and event-level capacity.
- [x] Route typed probability-blend candidates into the prediction evaluator and emit candidate-specific loop feedback.
- [x] Add separate governed BTC and SOL five-minute mission templates; reject mixed-symbol missions and unresolved provenance.
- [x] Add a resumable PLOY prediction LoopRun with mission candidate/call/time budgets and content-addressed iteration evidence.
- [x] Move the authoritative prediction LoopRun into `ploy-research` Rust and retire the non-authoritative compatibility proposal helper.
- [x] Require explicit, non-empty Chainlink reference plus Binance spot/aggTrade/L2 snapshot surfaces, replayed by `received_at`, before a BTC/SOL LoopRun starts.
- [x] Retire the legacy Binance-to-`price_to_beat` backfill path so Binance cannot override Chainlink reference-price authority or Polymarket official resolution.
- [x] Enforce mission, symbol, horizon, and exact snapshot provenance again at the Rust evaluator boundary.
- [x] Keep feedback mission-local and return no-OOS plus conservative-capacity failure reasons for every proposed blend.

## Safety

- Live trading stays disabled.
- Nested legacy workflows are inactive and are not deployment authority.
- No database, wallet, venue, cloud host, or trading service is mutated by this migration.
