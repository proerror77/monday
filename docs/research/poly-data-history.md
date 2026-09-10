# poly_data historical fills

Monday imports externally produced CSV exports through `polymarket-raw-ops`
and consumes the verified result through `monday-prediction-research
--explore-trades`. All production implementation is Rust. The upstream scraper
is neither installed nor invoked by this integration.

## Reviewed source and coverage

The supported profile is `poly_data.order_filled.v2`, pinned to
[`136e129735c73bce3a3d59f20354417b188e47fd`](https://github.com/warproxxx/poly_data/tree/136e129735c73bce3a3d59f20354417b188e47fd).
The adapter was checked against these actual source functions:

- `update_utils/update_chain.py::_decode_log`, `_build_query` and `COLUMNS`:
  Polygon chain 137, one CTF Exchange V2 contract
  `0xe111180000d2663c0091e4f400237545b87b996b`, starting at block 84902353.
  The source describes deployment on 2026-03-31 and migration on 2026-04-28.
  Those descriptions and the 20-block tip buffer are not completeness or
  finality proofs. Other exchanges/contracts and V1 history are not covered.
- `update_utils/update_markets.py::_row` and `_token_ids`: `id` is
  `condition_id`, `clobTokenIds` contains decimal-string token IDs, and `tokens`
  carries explicit `token_id` / `outcome` pairs. Current metadata is not
  point-in-time metadata; its `winner` field is not a settlement label here.
- `update_utils/process_live.py::_processed_df`: V2 side is the **maker's**
  direction. Maker asset zero means maker buys outcome tokens; taker asset
  zero means maker sells them. Both raw amounts use six decimals. The upstream
  processed output casts price to float and removes raw asset IDs, so Monday
  deliberately requires `orderFilled.csv`, not `processed/trades.csv`.
- The inspected [`v1-final` tag, `23d9138d`](https://github.com/warproxxx/poly_data/tree/23d9138d334a3c7c7ec549bb2adf33b833334611),
  uses `update_goldsky.py` and a superficially similar eight-column CSV. It is
  a different source profile and is rejected rather than relabeled as V2.

The raw required columns are `timestamp,maker,makerAssetId,makerAmountFilled,
taker,takerAssetId,takerAmountFilled,transactionHash`. Timestamp is integer UTC
Unix seconds from the block. Upstream neither requests nor exports log index,
block hash, historical receipt time, or fees. An optional externally enriched
`logIndex` column is preserved; the source export and its provenance must still
be independently supplied. The importer does not authenticate the enrichment.

## Import and evidence contract

Input paths must be absolute, canonical regular files with caller-pinned
SHA-256 hashes. Private snapshots bind the two-pass parsing to those exact bytes.
Both files must fit the declared byte and row limits. Hard ceilings are 256 MiB
per input, one million records per input, 64 KiB per logical CSV record, and
64 MiB / one million records for the normalized artifact. CSV quoting and
multiline fields are bounded before the parser grows its record buffer.
Normalization streams to private files; the existing immutable publisher
buffers only the capped final artifact. No network requests occur during import.

Token IDs remain uint256 decimal strings. Cash and token quantities are exact
six-decimal values within Decimal's representable range. Out-of-range amounts
are quarantined. Price is a Decimal approximation of the authoritative exact
`cash_amount / token_amount` ratio; it is never an executable quote.

Records are tagged `market`, `fill`, or `quarantine`. Invalid mappings, amounts,
timestamps, out-of-window rows and intermediary mint/burn events have explicit
quarantine reasons. Quarantine records carry source-file row ordinals and
length-framed decoded-field hashes; they do not copy wallet addresses into
reports. Encoding errors, malformed quote framing, unsupported headers and
resource-limit violations fail the import without a partial publication.

Without `logIndex`, **every occurrence is retained**, including identical rows
and multiple fills within the same transaction. All such fills are marked
identity-ambiguous in the quality report. With an index, exact duplicate
`(chain, contract, transactionHash, logIndex)` records are quarantined as
duplicates. If the same identity has different payloads, **all** its occurrences
are quarantined. Conflicting token/market mappings are also removed entirely.

Publication reuses the collector's Linux-only no-clobber, fsync and final
`_SUCCESS` protocol. Each directory is `sha256=<manifest-sha256>` and contains:

- `history.ndjson`: accepted mappings/fills and quarantine records.
- `manifest.json`: source revision, input paths/hashes/bytes, declared retrieval
  and first-import times, requested interval, observed interval and quality counts.
- `_SUCCESS`: exact content SHA-256, written after both payloads are durable.

Keep the import JSON fixed for retries, including `imported_at_unix`. An exact
retry verifies the existing bytes and returns `unchanged`. A stopped publication
without `_SUCCESS` can complete its matching triplet; existing different bytes
are rejected. Different inputs, provenance or requested windows yield different
manifest identities. Separate overlapping exports are not a deduplicated union;
the consumer accepts one anchored artifact at a time.

The consumer requires the separately recorded manifest SHA-256, rehashes both
payloads, checks the marker, then reconstructs mappings, identity constraints,
row counts and quality statistics. A hash authenticates the supplied artifact
identity, not the truth or completeness of externally asserted provenance.

Every manifest explicitly leaves L2, executable quotes, settlement, trade
completion, coverage certification and research promotion false. Historical
information availability is `null`, separately from block event time and the
declared retrieval/import clocks. A missing log index or availability clock is
never manufactured. These files cannot replace existing tape triplets,
completeness certificates, Ready catalog receipts or authenticated snapshots.

## Runnable import and exploration

Build from the repository on Linux; use the existing cache. If a remote build is
needed, follow `.agents/skills/monday-remote-build/SKILL.md` and its approved
research-worker target requirements.

```sh
cd rust_hft
cargo build --locked -p hft-collector --bin polymarket-raw-ops
cd prediction-markets
cargo build --locked -p ploy-research --bin monday-prediction-research
```

Provide actual paths, the requested half-open UTC interval `[START, END)`, and
the actual export retrieval time. These example paths are placeholders; no data
comes bundled with the integration. Generate this file once and retain it for
repeat imports. Do not regenerate the first-import timestamp on retry.

```sh
MARKETS=/absolute/export/data/markets.csv
FILLS=/absolute/export/data/orderFilled.csv
OUTPUT=/absolute/research/poly-data
START=1788825600
END=1788912000
RETRIEVED=1788998400
jq -n \
  --arg markets "$MARKETS" --arg fills "$FILLS" --arg output "$OUTPUT" \
  --arg mh "$(sha256sum "$MARKETS" | cut -d ' ' -f 1)" \
  --arg fh "$(sha256sum "$FILLS" | cut -d ' ' -f 1)" \
  --argjson start "$START" --argjson end "$END" --argjson retrieved "$RETRIEVED" \
  --argjson imported "$(date +%s)" \
  '{source_schema:"poly_data.order_filled.v2",
    source_revision:"136e129735c73bce3a3d59f20354417b188e47fd",
    markets:$markets,markets_sha256:$mh,fills:$fills,fills_sha256:$fh,
    retrieved_at_unix:$retrieved,imported_at_unix:$imported,
    start_unix:$start,end_unix:$end,max_input_bytes:268435456,
    max_input_rows:100000,output_root:$output}' > poly-data-import.json

polymarket-raw-ops import-poly-data --config poly-data-import.json > first-import.json
polymarket-raw-ops import-poly-data --config poly-data-import.json > repeat-import.json
jq -e '.publication == "Unchanged"' repeat-import.json
HISTORY_DIR=$(jq -r .directory first-import.json)
MANIFEST_SHA=$(jq -r .manifest_sha256 first-import.json)
polymarket-raw-ops validate-poly-data \
  --directory "$HISTORY_DIR" --manifest-sha256 "$MANIFEST_SHA" > validated-history.json
monday-prediction-research --explore-trades "$HISTORY_DIR" "$MANIFEST_SHA" \
  > trade-exploration.json
```

Use the actual built binary paths or put them on PATH. Import is unavailable on
macOS because secure publication retains the existing Linux requirement; parsing,
quality validation and synthetic readback tests also run on macOS.

Exploration reports per-token observed row counts, ambiguous identities, exact
cash/token sums, min/max prices and volume-weighted observed price. These sums
describe exported fill events and do not establish complete economic volume.
Execution prices, queue position, spread, fill probability, net returns and
Sharpe are unavailable. `baseline_status=blocked_missing_research_evidence` is
expected for this source alone, even when the import itself is valid.

## Bounded BTC baseline contract and current blocker

The first baseline is **BTC 5m settlement probability**, with one decision per
official event at open + 60 seconds. Freeze the cohort and the following
hypothesis before viewing evaluation labels: a positive causally available
Binance spot mid-price return from open + 30s to open + 60s predicts Up with
probability 0.55; a negative return predicts 0.45; zero predicts 0.50. Compare
against constant 0.50. This fixed baseline has no fitted parameters or search
trials. Historical Polymarket fills serve descriptive coverage exploration;
they cannot supply the predictor's receipt clock or execution price.

Before any evaluation, require independently verified BTC 300-second market
contracts, UP/DOWN token bindings, Chainlink opening/expiry evidence, official
resolved labels and their availability times, and concurrent Binance spot
quotes with receive clocks. Reject signals whose availability exceeds their
decision time. Bind one authenticated chronological partition and label cutoff;
exclude events crossing its boundary. Keep the held-out artifact sealed through
selection and the existing one-time holdout protocol. Do not create a second
partition implementation or feed these CSVs directly into Mission execution.
Cap this first evidence assessment at 200 chronological events; report
insufficient data if the existing partition/evaluator minimums are not met.

Report sample counts, exclusions, Brier score and log loss on the authorized
evaluation partition. A cost-free predictive score is not P&L. Any later
economic comparison requires the bound token's executable ask, source fee
schedule, latency/slippage assumptions and sizing limits frozen in the existing
Mission. Unknown fees are not zero. With trade-only history, cost-adjusted
return, capacity, queue metrics and executable replay remain unavailable.

As of this implementation, neither the checked workspace nor the upstream Git
tree supplies real `markets.csv` / `orderFilled.csv`. No exact real cohort,
authenticated partition, matching CEX/settlement artifacts or existing run grant
has been provided for this baseline. Therefore no real import, training,
held-out evaluation or P&L result is claimed. Supply bounded external files to
run the commands above; an upstream backfill requires its own HyperSync token
and resource grant. This task does not start a backfill or buy a subscription.

## Existing real-time collection

Reuse `deployment/aliyun/polymarket-market-tape.toml` and its existing recorder.
The minimal research subset is `[strategy] symbols = ["BTCUSDT"]`, retaining
`mode = "dryrun"`, `strategy_variant = "noop"`, event-scoped quotes, the existing
reference/spot/aggregate-trade/L2 kinds and `record_market_updates_quote_sample_ms
= 0`. Keep full visible depth (`quote_depth_levels = 0` in upload policy), both
outcome tokens, Chainlink references and the shared Binance feeds. For reference
metadata/trades/settlement use the existing `polymarket-raw-ops collect-reference
--symbols BTCUSDT --market-id <verified-condition-id>` configuration within the
existing owner and limits. The subset is a documented configuration for a
future authorized transition; this PR does not edit or stop deployed collection.

## Focused checks

```sh
cd rust_hft
cargo test -p hft-collector --lib --locked poly_data
cargo test -p hft-data --lib --locked history_token_ids
cargo clippy -p hft-data -p hft-collector --lib --bin polymarket-raw-ops --locked --no-deps -- -D warnings
cd prediction-markets
cargo test -p ploy-research --lib --locked historical_trade_consumer
cargo clippy -p ploy-research --lib --bin monday-prediction-research --locked --no-deps -- -D warnings
```

Fixtures are synthetic and remain inside tests. Linux additionally exercises
the immutable publisher, exact retry, interrupted publication and corruption
rejection. No fixture is a substitute for a real research result.
