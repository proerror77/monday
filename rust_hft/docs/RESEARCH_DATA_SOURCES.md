# Research Data Source Inventory

_Last reviewed: 2026-08-24_

This document records external historical market-data resources that Monday can use to bootstrap research. It is a **research inventory**, not a declaration that any source is supported by a governed Data Mission, production connector, or live execution path.

Monday should use third-party or venue-hosted archives to accelerate historical research, while retaining its own live collectors as production/evidence truth for exact receive timing, sequence integrity, order/fill telemetry, and venue-specific behavior.

## Operating principle

Use a three-layer model:

1. **Research bootstrap** — public/free datasets and downloadable venue archives.
2. **Large-scale historical research** — paid or continuous historical providers where needed.
3. **Production truth** — Monday live collectors and private execution telemetry.

Do not block research until Monday has accumulated years of self-recorded data. Do not treat historical snapshots from an external provider as proof of production latency, maker queue position, or fill probability.

---

## Polymarket

### 1. PolyOrderbooks Hugging Face: 1-second full L2 sample

Source:
- https://huggingface.co/datasets/polyorderbooks/polymarket-crypto-5min-orderbooks

Current published sample:
- 15 complete five-minute crypto markets
- BTC, ETH, SOL, XRP, BNB, DOGE, HYPE
- 1-second snapshots
- YES and NO books
- full resting bid/ask ladders
- 9,000 outcome snapshots
- winner attached
- CC BY 4.0

Best use:
- execution-aware replay
- spread/depth/OBI/microprice research
- liquidity withdrawal
- depth slope/convexity
- size-aware VWAP and slippage
- capacity estimation
- expiry-liquidity studies

Limitation:
- a 1-second snapshot does not reconstruct every intra-second trade/cancel event;
- it is not enough for true maker queue modeling, sub-second latency research, or exact fill probability.

### 2. PolyOrderbooks continuous historical API

Sources:
- https://polyorderbooks.com/
- https://polyorderbooks.com/features

Advertised capability:
- historical Polymarket order books
- 1-second snapshots
- resolved markets remain queryable
- full bid/ask ladders
- REST API

Use this as a candidate large-scale historical source. Monday should still validate sampled windows against its own collector before treating vendor data as canonical evidence.

### 3. Large public Polymarket research corpus

Source:
- https://huggingface.co/datasets/obadiaha/polymarket-crypto-5m-15m

Published coverage includes:
- BTC, ETH, SOL, XRP
- 5-minute and 15-minute markets
- millions of order-book rows
- tens of millions of trades
- top-10 order-book snapshots at 10-second cadence
- market resolutions and underlying crypto prices

Best use:
- large-sample alpha screening
- regime studies
- market-resolution joins
- coarse liquidity and lead/lag studies

Do **not** use 10-second top-10 snapshots as an execution-truth substitute for 1-second/full-event L2.

---

## Binance

### 1. Binance Public Data — official downloadable archive

Sources:
- https://data.binance.vision/
- https://github.com/binance/binance-public-data

Useful public datasets include daily/monthly:
- trades
- aggTrades
- klines
- Spot
- USD-M Futures
- COIN-M Futures

Important parser note:
- Binance Spot archive timestamps changed to microseconds from 2025-01-01 onward; historical ingestion must normalize timestamp units explicitly.

Best use:
- long-horizon trade-flow research
- taker buy/sell flow
- momentum / realized-volatility studies
- cross-sectional symbol selection
- long historical backfill

Caution:
- Binance's easily downloadable public `bookDepth` artifacts should not be assumed to be execution-grade full incremental L2. For real order-book replay, prefer a validated incremental-L2 archive or Monday self-capture.
- verify the applicable Binance data-use terms before redistribution or commercial data resale.

### 2. Tardis — Binance historical incremental L2

Source:
- https://docs.tardis.dev/historical-data-details/binance

Available research types include:
- trades
- incremental_book_L2
- book snapshots
- quotes
- raw historical WebSocket messages with local receive timestamps

Tardis documents Binance depth capture at the fastest API update speed available at collection time and reconstructs initial snapshots using REST where required.

Free bootstrap path:
- Tardis provides downloadable CSV samples for the first day of each month without an API key.

Best use:
- order-book reconstruction
- cancellation/replenishment research
- microprice/OBI
- queue/flow proxies
- lead/lag against other exchanges
- realistic taker execution simulation

---

## Bybit

### 1. Bybit official historical public market data

Sources:
- https://www.bybit.com/future-activity/developer
- https://www.bybit.com/en/learn/bybit-guide/how-to-create-a-bybit-api-key

Bybit documents downloadable historical public market data including:
- order-book snapshots
- klines
- trades
- funding rates

Use official downloads first for low-cost factor research where the required resolution is available.

### 2. Tardis — Bybit historical incremental L2

Derivatives:
- https://docs.tardis.dev/historical-data-details/bybit

Spot:
- https://docs.tardis.dev/historical-data-details/bybit-spot

Published coverage includes:
- Bybit derivatives historical data for inverse contracts from 2019-11-07
- linear contracts from 2020-05-28
- Bybit Spot from 2021-12-04
- trades
- incremental_book_L2
- quotes
- derivative ticker
- liquidations for derivatives

Tardis also exposes free first-day-of-month CSV samples.

Best use:
- perp microstructure
- liquidation propagation
- funding/OI + L2 interaction
- Binance-vs-Bybit lead/lag
- high-volatility altcoin execution research

---

## OKX

### 1. OKX official historical data — unusually useful free source

Source:
- https://www.okx.com/historical-data

OKX currently documents downloadable:
- tick-level trade history from 2021-09
- perpetual funding rates from 2022-03
- high-resolution L2 order-book data from 2023-03
- OHLC candles from 2023-07
- borrowing rates from 2021-12

This should be one of Monday's highest-priority free sources because official historical L2 is directly available.

Best use:
- multi-month L2 execution research without waiting for self-capture
- spread/depth/OBI/microprice
- funding + order-book regime studies
- cross-venue lead/lag with Binance/Bybit

### 2. Tardis — OKX historical incremental L2

Spot:
- https://docs.tardis.dev/historical-data-details/okex

Futures:
- https://docs.tardis.dev/historical-data-details/okex-futures

Swap:
- https://docs.tardis.dev/historical-data-details/okex-swap

Tardis provides historical incremental L2, trades, quotes, derivative ticker, liquidations, mark/index/open-interest channels depending on instrument and period.

---

## Multi-venue historical provider: Tardis

Overview:
- https://docs.tardis.dev/historical-data-details

Useful supported venues include:
- Binance Spot / USD-M / COIN-M
- Bybit Spot / Derivatives / Options
- OKX Spot / Futures / Swap / Options
- Deribit
- Hyperliquid
- Bitget
- Gate.io
- Kraken
- Coinbase and others

Why this matters for Monday:
- a normalized multi-venue adapter can enable the same feature and replay engine across CEXs;
- raw exchange-format replay remains available where venue semantics matter;
- local timestamps make receive-time/lead-lag research possible, subject to provider collection topology and clock assumptions.

Do not blindly merge venues by event timestamp. Preserve at least:
- exchange event timestamp
- provider/local receive timestamp when available
- sequence/update IDs
- venue
- instrument
- source/provider
- schema version

---

## Recommended Monday research tiers

| Research question | First source | Upgrade source | Production truth |
|---|---|---|---|
| Polymarket L2 execution replay | PolyOrderbooks HF sample | PolyOrderbooks continuous history | Monday Polymarket WS + user/order telemetry |
| Polymarket large-sample alpha screening | public HF corpora | continuous historical API | Monday self-capture validation |
| Binance long-horizon trade/flow factors | Binance Public Data | Tardis if L2 required | Monday Binance collector |
| Binance true L2 microstructure | Tardis sample/full history | Tardis full history | Monday Binance LOB collector |
| Bybit perp microstructure | Bybit official download | Tardis incremental L2 | Monday Bybit collector |
| OKX L2 research | OKX official historical L2 | Tardis incremental L2 | Monday OKX collector |
| cross-venue lead/lag | official trades + aligned samples | Tardis normalized/raw multi-venue | synchronized Monday live collectors |
| maker fill / queue / latency | not reliable from coarse snapshots | event-level incremental L2 only | Monday private order/fill telemetry |

---

## Suggested ingestion contract

External historical sources should adapt into a provider-neutral research interface rather than leak provider schemas through the research stack.

Conceptually:

```rust
trait HistoricalMarketDataProvider {
    fn instruments(&self) -> Result<Vec<Instrument>>;
    fn trades(&self, request: TradeHistoryRequest) -> Result<TradeStream>;
    fn book_events(&self, request: BookHistoryRequest) -> Result<BookEventStream>;
    fn funding(&self, request: FundingHistoryRequest) -> Result<FundingStream>;
}
```

The canonical research event model should preserve enough provenance to distinguish snapshots from event-level deltas.

Minimum provenance fields:
- `venue`
- `instrument`
- `event_time`
- `receive_time` when available
- `sequence_id` / update range when available
- `source_provider`
- `source_uri`
- `source_resolution`
- `source_schema_version`
- `ingested_at`
- raw artifact checksum

Recommended book-event classes:
- `Snapshot`
- `Delta`
- `Trade`
- `Quote`
- `Funding`
- `OpenInterest`
- `Liquidation`
- `MarkPrice`

Do not infer `Delta` from coarse snapshots and then present it as observed event truth.

---

## Immediate research sequence

1. **Polymarket execution unit tests**
   - import the 1-second full-L2 HF sample;
   - implement size-aware ladder walking;
   - measure spread, depth, OBI, microprice, slippage, capacity, and time-to-expiry liquidity.

2. **OKX free L2 pilot**
   - download official L2 history;
   - run the same canonical book/replay/features pipeline used for Polymarket;
   - verify parser and sequence/completeness assumptions.

3. **Binance / Bybit Tardis free-sample pilot**
   - use first-day-of-month free incremental-L2 samples;
   - prove one normalized replay path across Binance, Bybit, and OKX;
   - compare OBI/microprice/depth-withdrawal features across venues.

4. **Long-history alpha screening**
   - use Binance Public Data trades/aggTrades/klines for broad symbol/time coverage;
   - join funding/OI/liquidation sources where available;
   - use event-level L2 only on shortlisted regimes/symbols to control data cost.

5. **Shadow validation**
   - compare historical-provider features with Monday's live self-capture on overlapping windows;
   - quantify timestamp, depth, missing-event, and replay divergences before trusting a provider for execution research.

---

## Data-quality gates

Every imported archive should produce an evidence manifest with at least:
- source URL/provider
- requested and observed coverage window
- venue/instrument
- expected vs observed row/event counts where knowable
- checksum/hash
- timestamp unit and timezone
- duplicate count
- gap count
- crossed-book count
- sequence-gap count for event streams
- parser/schema version

A dataset is research input, not evidence of production support. Missing events must remain missing; do not synthesize completeness for execution studies.

## Strategic conclusion

Monday no longer needs to wait for self-collected history before beginning market-microstructure and execution research.

Use external historical data aggressively for **research iteration**. Keep Monday collectors for **production truth, exact event ordering, latency, fills, and validation**.
