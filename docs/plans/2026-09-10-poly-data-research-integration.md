# Polymarket historical-data integration and research handoff

Status: implementation requested; this specification is not implementation or research evidence.

## Goal

Use https://github.com/warproxxx/poly_data as an external historical trade source so
Monday can prioritize reproducible Polymarket research instead of duplicating
historical acquisition. Deliver a working import-to-research path in Monday.
Preserve the existing minimal Polymarket and CEX real-time recording paths.

User requested execution in the Monday Codex project on 2026-09-10.
Initial inspected Monday base: 54f7b558bc4dae80a17a9f16ebe893a6c571fb37.
Refresh the current branch and upstream source before implementation.

## Existing seams to inspect and reuse

- Acquisition: rust_hft/tools/collector.
- Existing collector modules: polymarket_research_import.rs,
  polymarket_research_normalize.rs, polymarket_research_select.rs,
  polymarket_evidence_artifact.rs, and polymarket_raw.rs.
- Existing operator entrypoint: polymarket-raw-ops.
- Prediction research: rust_hft/prediction-markets; inspect its local AGENTS.md.
- Research orchestration: rust_hft/alpha-harness and the applicable canonical
  campaign contracts. Do not bypass their evidence or holdout gates.
- Read root AGENTS.md, rust_hft/AGENTS.md, ARCHITECTURE.md, and
  docs/architecture/REPOSITORY_LAYOUT.md before choosing owning modules.

Monday remains one Rust-only production system with shared market-data, order,
and risk seams. An independently runnable prediction research path does not
mean a new platform, standalone OMS, or duplicated execution infrastructure.
Do not add the upstream Python pipeline as a Monday runtime dependency.
Consume externally produced data through a Rust adapter; do not port the entire
upstream scraper merely for language uniformity.

## Implementation

1. Pin and inspect the upstream revision and actual schemas. The initial README
   describes markets.csv, orderFilled.csv, and processed/trades.csv from chain
   OrderFilled events. Verify contract coverage, v1/v2 historical boundaries,
   maker/taker semantics, timestamp precision and intermediate mint/burn events.
   Do not assume the README's coverage claims prove completeness.
2. Add a bounded streaming Rust import at the existing acquisition seam, with
   explicit input paths, schema/version, source revision, content hashes,
   retrieval/import time, date coverage, and row/byte limits. Reuse canonical
   types only when their semantics match. Preserve decimal precision and token
   IDs. Distinguish block event time from historical information availability.
3. Validate market/outcome mappings, prices, amounts, timestamps, malformed rows,
   duplicate handling, and restart idempotency. A transaction hash alone is not
   a unique fill ID. Retain log index when available; otherwise report identity
   ambiguity rather than silently dropping legitimate same-transaction fills.
   Quarantine invalid/unmapped records with reason counts and fail research
   promotion when required coverage or identity cannot be established.
4. Produce a versioned, auditable dataset manifest and quality report using the
   existing storage/evidence contracts. Imported chain fills are trade-only:
   they do not establish L2, executable quotes, queue position, spread,
   fill probability, or a trade-completion certificate. Never fabricate those
   fields or weaken the current research-segment validator to accept them.
5. Wire the accepted dataset into an existing research-only consumer for
   historical trade/price/volume exploration. If stronger replay requires L2 or
   settlement evidence, surface missing prerequisites explicitly while allowing
   correctly labeled trade-only exploratory analysis.
6. Reuse current real-time Polymarket and corresponding CEX collectors. Document
   the minimal market subset/configuration for BTC direction research without
   stopping deployed collection or performing a production cutover in this task.
7. Provide one reproducible bounded BTC 5m or 15m research baseline, selected by
   verified coverage, with point-in-time external CEX signals, settlement labels,
   chronological splits, leakage checks, and cost assumptions. Freeze the
   hypothesis and holdout before evaluation. Historical trade prices are not
   executable prices; require quotes for execution claims. Report unsupported
   execution/queue metrics as unavailable.
8. Document exact runnable import, validation, and research commands plus required
   data/credentials. If real data or an existing resource grant is unavailable,
   finish the adapter and focused checks, and report the precise blocked run.
   Never substitute fixtures for real research evidence.

## Acceptance and delivery

- Focused tests disprove wrong direction/price mapping, malformed inputs,
  duplicate/restart mistakes, and false L2 or completion promotion.
- Representative fixtures are clearly synthetic and used only for tests.
- Import readback verifies manifest identity and repeat-import behavior.
- A real-data run, when available within existing grants, records coverage,
  input hashes, sample counts, exclusions, fees, and out-of-sample results.
  Negative or insufficient evidence is a valid research outcome.
- Run the owning focused Rust checks and required CI; follow monday-remote-build
  for remote validation, never build on ack-system nodes.
- Deliver implementation commits and evidence in this PR or a clearly linked
  implementation PR. Do not merge this handoff alone as completed integration.
- Keep live trading disabled and retain current risk/approval gates. No new
  paid subscriptions, unbounded backfills, infrastructure cutover, or new trading
  authority is part of this handoff.
