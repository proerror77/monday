# Aliyun Binance data host

The Tokyo ECS runs two public-market-data services. Neither service submits orders.

```bash
systemctl status binance-lob-archiver@spot.service
systemctl status binance-lob-archiver@usdm.service
journalctl -u 'binance-lob-archiver@*' -f
```

Polymarket public crypto market updates run separately in dry-run/no-op mode:

```bash
systemctl status polymarket-market-tape.service
journalctl -u polymarket-market-tape.service -f
```

Install the runtime config as group-readable by the unprivileged service account:

```bash
sudo install -m 0640 -g hftcollector deployment/aliyun/polymarket-market-tape.toml \
  /etc/monday/polymarket-market-tape.toml
```

The initial scope is BTC, ETH, SOL, XRP, DOGE, HYPE, and BNB 5-minute/15-minute
markets only. NBA, World Cup, general event, and weather catalogs remain disabled
until this lane is stable and explicitly expanded.

The service records normalized `MarketUpdate` NDJSON under
`/data/monday/spool/polymarket/`. It has no credential environment file and cannot
emit trading intents. The primary tape stores Polymarket quotes/lifecycle events plus
reference prices, records one full visible CLOB book per token per second, retains
every bid and ask level in each snapshot, and drops orphaned or post-expiry quotes.
The manifest separates `venue_depth_complete` from `temporal_updates_complete` so a
full-depth sampled snapshot cannot be mistaken for a raw exchange-diff tape.
Quotes timestamped after
their event end are rejected before executor or strategy evaluation, so a late quote
from an expired 5-minute/15-minute market cannot trigger a trade in the next event.
The recorder rotates the active tape in-process every hour, without disconnecting
the feed. The runner's independent six-hour restart remains as a bounded lifecycle
refresh. This keeps the local-only recovery-point window to about one hour while
avoiding unsafe copy/truncate operations and hourly WebSocket reconnect gaps. A
new tape is seeded with the active `event_discovered` records before quotes, so
token-to-event context remains independently replayable.

Closed Polymarket sessions are validated, compressed, and uploaded every five
minutes by `polymarket-market-tape-upload.timer`. The uploader ignores the active
`market-updates.ndjson`, requires contiguous sequence numbers and monotonic record
timestamps, then writes `.ndjson.zst`, `manifest.json`, and `_SUCCESS` under
`lake/raw/venue=polymarket/dataset=crypto_expiry/`. Sessions crossing UTC-hour
boundaries are split into hour partitions. Before deleting a closed source tape,
the uploader reads all three OSS objects back and verifies the compressed byte
count, SHA-256, manifest, and success marker. A bad tape does not block later
closed tapes. Failed uploads retain the closed source tape for retry and surface the failure in
`/data/monday/spool/polymarket/upload-status.json`.
Each manifest reports `event_context_complete`; legacy segments lacking a prior
token discovery are explicitly marked as requiring the previous event context.
New manifests also set `canonical=true` and list `record_id_versions`. Raw readers
must ignore any artifact with an adjacent `<data>.SUPERSEDED.json` marker; the marker
names the canonical replacement and quarantine copy. This is the fail-closed path
when the collector role can write but cannot delete an invalid historical object.

The companion `polymarket-reference-collector.service` polls the official Gamma and
Data APIs every 30 seconds. It writes complete Gamma market payloads (including
volume, tick size, minimum order size, fee fields, token/outcome mappings, and status),
all public taker and maker trade prints, and closed-market settlement payloads to
`/data/monday/spool/polymarket-reference/`. Its independent uploader publishes the
same hash-bound artifact triplet under
`lake/raw/venue=polymarket/dataset=crypto_expiry_reference/`. Stable trade IDs and a
persisted overlap state prevent duplicate trade prints across polls and restarts.
An in-place v1-to-v2 migration locally isolates the old active tape, reopens completed
markets, and emits a complete v2 overlap; the canonicalizer builds a v2 union before
historical v1 artifacts receive supersession markers. After settlement,
trade polling continues for at least 30 minutes after the latest observed change and
requires three additional stable polls before a market is marked complete. Malformed
trade rows are isolated and counted by reason in `health.json` instead of blocking
valid rows.
Each append batch rolls back to its starting offset if write or fsync fails, so a retry
cannot duplicate a durable prefix or leave a partial record behind.
Neither companion unit contains private keys or an execution command.
The Python companion is a transitional parity lane: the existing Rust
`collect-pm-trades` command requires PostgreSQL and cannot yet emit the stateless raw
OSS contract used by this data-only ECS. Replace it only after a Rust shadow service
produces byte/field, deduplication, settlement, rotation, and OSS-readback parity.

Install the uploader beside the existing tape service:

```bash
sudo install -m 0755 deployment/aliyun/polymarket_market_tape_upload.py \
  /opt/monday/bin/polymarket_market_tape_upload.py
sudo install -m 0640 deployment/aliyun/polymarket-market-tape-upload.env \
  /etc/monday/polymarket-market-tape-upload.env
sudo install -m 0644 deployment/aliyun/polymarket-market-tape-upload.service \
  /etc/systemd/system/polymarket-market-tape-upload.service
sudo install -m 0644 deployment/aliyun/polymarket-market-tape-upload.timer \
  /etc/systemd/system/polymarket-market-tape-upload.timer
sudo systemctl daemon-reload
sudo systemctl enable --now polymarket-market-tape-upload.timer
```

Install the companion reference lane:

```bash
sudo install -m 0755 deployment/aliyun/polymarket_reference_collector.py \
  /opt/monday/bin/polymarket_reference_collector.py
sudo install -m 0644 deployment/aliyun/polymarket-reference-{collector,upload}.service \
  /etc/systemd/system/
sudo install -m 0644 deployment/aliyun/polymarket-reference-upload.timer \
  /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now polymarket-reference-collector.service \
  polymarket-reference-upload.timer
```

Each service opens bounded WebSocket shards, records every diff, fetches a REST
Top-100 snapshot, validates sequence continuity, writes replay checkpoints, compresses
hourly segments, and uploads `.jsonl.zst`, `manifest.json`, and `_SUCCESS` to OSS.
`health.json` under `/data/monday/spool/binance-lob/<market>/` reports freshness,
symbol coverage, bridge state, gap count, free disk space, and whether the 20GiB
warning threshold is active. It also reports pending upload count, last upload
success/error, and an upload warning so OSS failures cannot look fully healthy.
A silent WebSocket shard fails after
`STALL_TIMEOUT_SECONDS`. Receiver cleanup is bounded to five seconds so a stuck
WebSocket close handshake cannot block reconnection. A separate process watchdog
exits after 180 seconds without any market-data frame, allowing systemd to recover
even if the asyncio loop deadlocks. Low disk space emits a warning but does not
pause collection. Successfully uploaded segments are deleted
from the local spool immediately. Pending segments are retained when OSS upload
fails so the collector never creates a silent data hole merely to reclaim space.
The shared pending-diff budget still bounds initialization bursts. Services restart
every six hours to refresh the active-symbol catalog.

The snapshot bridge timeout is 120 seconds after the last initial snapshot request
finishes. This gives the full-market queue time to apply the tail of Spot and USD-M
snapshots without turning normal initialization backlog into a reconnect loop.

Current coverage:

- `spot`: every active Binance spot symbol; `TRD_GRP_261` symbols are tagged as
  tokenized securities in manifests.
- `usdm`: every active Binance USD-M perpetual contract.

The July 13 full-market probe measured about 1,450 events/s and 0.46MiB/s on the
wire. With both production services synchronized, the host used about 1.2 CPU
cores and less than 1GiB of service memory. The 80% CPU quota and 3.2GiB memory
limit on each service preserve host headroom during bursts.

## Backtesting storage boundary

Use OSS raw segments as the immutable source of truth, not as the repeated query
format. Validate `_SUCCESS`, SHA-256, snapshots, and sequence-gap status, then
materialize selected partitions as Parquet. A research host caches those Parquet
files locally and uses DuckDB for filtering, joins, feature datasets, run lineage,
and result metadata. The Rust `apps/replay` / `apps/backtest` path owns order-book
replay, fees, latency, slippage, fills, and capacity simulation.

This is a deterministic replay contract for the state captured from a Top-100
snapshot seed plus sequence-checked diffs; it is not a complete venue-depth
contract. Deep unchanged levels can move into the visible range after enough
near-book deletions without appearing in a diff, so manifests explicitly set
`venue_depth_complete=false`. Top-20/Top-50 imbalance research must validate the
acceptable snapshot age and churn window for each experiment. Strategies that
require guaranteed depth completeness or market-impact modeling need a separately
benchmarked deeper or periodically refreshed snapshot dataset.

ClickHouse is optional for always-on shared analytics, dashboards, and derived
realtime features. It is not required for the first backtest pipeline and should
not duplicate the complete raw OSS tape.

## Rust collector shadow rollout

The Rust replacement runs beside the Python collector first. Its service name,
spool directory, and OSS dataset are deliberately separate, so installing or
starting it neither stops the Python services nor overwrites their objects:

| Market | Service | Local spool | OSS dataset |
| --- | --- | --- | --- |
| Spot | `binance-lob-archiver-rust@spot` | `/data/monday/spool/binance-lob-rust-shadow/spot` | `spot_all_rust_shadow` |
| USD-M | `binance-lob-archiver-rust@usdm` | `/data/monday/spool/binance-lob-rust-shadow/usdm` | `usdm_perpetual_all_rust_shadow` |

Both shadow examples start with `BTCUSDT` only. Keep that bounded scope through
two successful segment rotations and uploads; switch `SYMBOLS=ALL` only after
health, replay continuity, and resource use pass the shadow gate.

Build and verify the binary from `rust_hft/`:

```bash
cargo build --release --locked --no-default-features \
  -p hft-collector --bin binance-lob-archiver
target/release/binance-lob-archiver --self-test
```

Install the binary and the Rust-only service templates without changing the
running Python units:

```bash
sudo install -D -m 0755 target/release/binance-lob-archiver \
  /opt/monday/bin/binance-lob-archiver
sudo install -d -m 0750 -o hftcollector -g hftcollector \
  /data/monday/spool/binance-lob-rust-shadow/{spot,usdm}
sudo install -m 0644 ../deployment/aliyun/binance-lob-archiver-rust@.service \
  /etc/systemd/system/binance-lob-archiver-rust@.service
sudo install -m 0640 ../deployment/aliyun/binance-lob-archiver-rust-spot.env \
  /etc/monday/binance-lob-archiver-rust-spot.env
sudo install -m 0640 ../deployment/aliyun/binance-lob-archiver-rust-usdm.env \
  /etc/monday/binance-lob-archiver-rust-usdm.env
sudo systemctl daemon-reload
```

Starting shadow collection is a separate, explicit operation:

```bash
sudo systemctl start binance-lob-archiver-rust@spot.service
sudo systemctl start binance-lob-archiver-rust@usdm.service
jq . /data/monday/spool/binance-lob-rust-shadow/{spot,usdm}/health.json
```

The container image is built from the Rust workspace root and includes pinned
Alibaba Cloud CLI checksums plus `zstd`:

```bash
docker build -f deployment/docker/Dockerfile.binance-lob-archiver \
  -t monday/binance-lob-archiver:shadow .
```

For container use, mount the shadow spool read-write and provide an ECS RAM-role
Alibaba Cloud CLI profile. Do not inject long-lived access keys into the image.
