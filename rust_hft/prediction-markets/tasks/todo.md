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
- [x] Enforce the compatibility live-gateway boundary from parsed Rust syntax and keep every execution fake inside exact `#[cfg(test)]` modules.
- [x] Remove retired `ployctl trading principal/readiness` parsing and usage until a canonical Monday read-only preflight exists.
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

### Unified Monday prediction research control plane (2026-07-16)

- [x] Add the versioned Mission v3 typed task, authority, immutable identity,
  and fail-closed checkpoint/admission contract without reinterpreting v2
  artifacts (#321).
- [x] Build the prediction data audit directly from externally verified artifacts.
- [x] Aggregate non-overlapping Polymarket evidence artifacts while requiring every member to remain BTC/SOL slot-complete; allow gaps between event-local cohorts.
- [x] Verify sealed Polymarket evidence into semantic read-only typed projections.
- [x] Project verified Binance tapes into deterministic research updates, source clocks, and LOB snapshots.
- [x] Bind immutable Polymarket evidence to external content and manifest SHA-256 anchors before semantic consumption.
- [x] Bind selected-event trades to an event-local collector completion proof from raw tape through sealed research evidence.
- [x] Project verified Polymarket evidence into availability-safe research carriers without replaying discovery metadata or settlement labels before observation.
- [x] Persist and independently verify the immutable Ready catalog plus event
  cohort partition artifact without reconstructing the partition (#365).
- [ ] Wire the verified-artifact ResearchSnapshot adapter into the snapshot CLI and complete its cloud alpha-harness E2E.
- [ ] Project causally valid in-event Chainlink ticks without allowing them to replace the pre-open five-minute strike (#304).
- [x] Promote snapshot, prediction LoopRun, and event evaluator to precompiled
  Monday binaries; remove runtime `cargo run --example` and research-policy
  coupling to the legacy OMS/runtime workspace.
- [x] Route snapshot and mission transport through `alpha-harness prediction`,
  including signed inputs, outer hashes, safe ZIP extraction, immutable result
  publication, and cross-Job paused-state resume.
- [x] Build the binaries into the shared Monday research image and validate the
  restricted ACK Job in prediction-market CI without adding an OMS, risk,
  reconciliation, or venue execution path.

- [x] Adapt the prediction Loop to binary/digital-option factors by exposing a point-in-time Chainlink endpoint-probability component without duplicating the existing CEX model.
- [x] Split every prediction walk-forward lane by event end so unresolved events cannot cross train/test boundaries.
- [x] Run `ploy-research` library tests in the active root CI workflow.
- [x] Preserve Monday's Factor Bank, Search Protocol, artifact/experiment/factor stores, `factorctl`, active DSL, manifest, and harness contracts while keeping the event-specific loop inside the prediction-market module.
- [x] Pass governed mission context and bounded prior verdicts into LLM proposals, with code-enforced mutable scope.
- [x] Gate prediction OOS evidence with Brier score, log loss, calibration error, settlement PnL, and event-level capacity.
- [x] Route typed probability-blend candidates into the prediction evaluator and emit candidate-specific loop feedback.
- [x] Add separate governed BTC and SOL five-minute mission templates; reject mixed-symbol missions and unresolved provenance.
- [x] Pin a canonical reviewed Linux prediction-policy graph and fail closed when its lockfile or policy path-manifest inputs change; preserve package, checksum, and feature evidence, exclude host/proc-macro runtime dependencies, then pin the v5 identity in both mission templates.
- [x] Add a resumable Monday prediction LoopRun with mission candidate/call/time budgets and content-addressed iteration evidence.
- [x] Implement the complete authoritative prediction LoopRun and tests in `ploy-research` Rust without introducing a Python runner; retire the Binance-to-opening-reference backfill helper.
- [x] Recompute terminal provisional models from typed Brier, log-loss, ECE, settlement-PnL, and conservative-capacity metrics recorded in append-only feedback; require a separate sealed snapshot for final keep.
- [x] Recover LLM provider, model, and usage provenance from the same content-addressed response envelope after a crash.
- [x] Require explicit, non-empty Chainlink reference plus Binance spot/aggTrade/L2 snapshot surfaces, replayed by `received_at`, before a BTC/SOL LoopRun starts.
- [x] Retire the legacy Binance-to-`price_to_beat` backfill path so Binance cannot override Chainlink opening/settlement authority.
- [x] Enforce mission, symbol, horizon, and exact snapshot provenance again at the Rust evaluator boundary.
- [x] Keep feedback mission-local and return no-OOS plus conservative-capacity failure reasons for every proposed blend.
- [x] Add a governed Rust-only Burn binary probability lane with event-disjoint snapshot-bound inputs and content-addressed model evidence.
- [x] Bind the settlement trainer to a manifest-sealed shared time boundary so overlapping event lifetimes cannot cross train and validation cohorts.
- [x] Preserve UP/DOWN AutoFactor registry identity and export side-isolated Alpha Zoo snapshots without exposing them to legacy replay/runtime consumers.
- [ ] Add separate UP/DOWN token repricing, fill, slippage, and markout evaluators without changing the settlement target.
- [ ] Add an immutable all-update-tick research tape for token microstructure; keep the one-second L2 tape as the settlement baseline.
- [ ] Expand governed product-family missions beyond BTC/SOL five-minute only after verified 15-minute and one-hour evidence exists end to end.
- [x] Complete the repository-wide Rust-only cutover: remove tracked Python and PyTorch/libtorch paths, add native Burn training for both research lanes, and pass focused Rust validation.
- [x] Remove unused Monday research-store/search crates while preserving active DSL, manifest, and harness contracts.

## Safety

- Live trading stays disabled.
- Nested legacy workflows are inactive and are not deployment authority.
- No database, wallet, venue, cloud host, or trading service is mutated by this migration.
