# Aliyun Binance data host

The Tokyo ECS runs the Rust Binance LOB archiver as two public-market-data
services. Neither service has trading credentials or submits orders.
The legacy Python collector, its systemd template, and its deployment tests have
been removed; the Binance collector deployment lane is Rust-only.

The current Rust production cutover completed on 2026-07-15 after a 3,672-second
full-catalog shadow with zero restarts and sequence gaps plus Spot and USD-M OSS
round-trip verification. The live evidence is stored under
`/data/monday/evidence/cutovers/`; the rollout below is the required procedure for
future collector releases and host replacements.

```bash
systemctl status binance-lob-archiver-production@spot.service
systemctl status binance-lob-archiver-production@usdm.service
journalctl -u 'binance-lob-archiver-production@*' -f
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

## Rust-only collector release workflow

The Binance collector deployment lane is Rust-only. The legacy Python collector,
its systemd unit, and its deployment tests are removed. A release now has three
separate operations:

1. install a digest-pinned candidate without touching production;
2. run a candidate-specific one-hour full-catalog shadow gate;
3. cut over only by consuming that gate's immutable evidence.

All host operations go through Alibaba Cloud Assistant from the local Alibaba
Cloud CLI. The scripts reject regions other than Tokyo
(`ap-northeast-1`), use the configured `default` CLI profile, and never put a
credential in command content. The ECS side uses `MondayLobEcsRole`.

### 1. Install a candidate

The artifact must be a Linux x86-64 Rust binary produced by the approved build,
uploaded to private OSS, and identified by its exact SHA-256. Do not upload a
macOS `target/release` binary. The ACR collector image is a durable container
publication, but the current bare ECS collector consumes the separately pinned
OSS binary.

Run the committed installer from a clean checkout at `SOURCE_REVISION`:

```bash
set -euo pipefail
INSTANCE_ID=i-REPLACE \
ARTIFACT_OSS_URI=oss://monday-lob-apne1-1045353359/releases/binance-lob-archiver/REPLACE/binance-lob-archiver \
ARTIFACT_SHA256=REPLACE_WITH_64_HEX_DIGEST \
SOURCE_REVISION=REPLACE_WITH_GIT_SHA \
./deployment/aliyun/deploy-rust-lob-release.sh
```

The installer verifies that `SOURCE_REVISION` is the clean current `HEAD`,
uploads a digest-addressed deployment bundle, waits for Cloud Assistant, verifies
both OSS objects on the host, runs the binary self-test, and requires the
`--upload-only` capability. It installs only the isolated shadow unit/env files
and the shadow symlink. Production unit/env files remain staged under:

```text
/opt/monday/releases/binance-lob-archiver/<artifact-sha256>/deployment/
```

Candidate installation refuses an unmounted `/data`, an active shadow, a digest
mismatch, or a concurrent release operation. It does not start any service and
does not overwrite production configuration or the production symlink.

The committed shadow environments use `SYMBOLS=ALL`, ten-minute segments, the
isolated spools below, and isolated OSS datasets:

| Market | Shadow spool | Shadow dataset |
| --- | --- | --- |
| Spot | `/data/monday/spool/binance-lob-rust-shadow/spot` | `spot_all_rust_shadow` |
| USD-M | `/data/monday/spool/binance-lob-rust-shadow/usdm` | `usdm_perpetual_all_rust_shadow` |

### 2. Run the one-hour full-catalog gate

Start the gate through the same CLI wrapper:

```bash
set -euo pipefail
ACTION=gate \
INSTANCE_ID=i-REPLACE \
ARTIFACT_SHA256=REPLACE_WITH_64_HEX_DIGEST \
./deployment/aliyun/invoke-rust-lob-operation.sh
```

The host gate owns the complete transition. It verifies the candidate and
`SYMBOLS=ALL`, drains any previous isolated shadow data, restarts both units,
waits for initial full-catalog health, freezes both session IDs and catalog
digests, and then uses monotonic time to observe at least 3,600 seconds. It fails unless all of
these are true for the entire candidate run:

- both units stay active with `NRestarts=0`;
- Spot has at least 1,000 symbols and USD-M at least 400;
- every discovered symbol has a ready snapshot and sequence gaps remain zero;
- neither session nor catalog membership changes, health never stops advancing
  for more than 90 seconds, and the persistent upload-failure count is unchanged;
- pending uploads are zero and queue, disk, and upload warnings are false;
- CPU accounting and peak memory stay inside the systemd limits;
- after stop, the candidate's `--upload-only` drain leaves no partial,
  temporary, corrupt, compressed, success-marker, or cleanup-marker artifact;
- for each market, at least two manifests created after gate start are downloaded
  from OSS with their data object and reproduce the manifest SHA-256.

A successful production gate writes:

```text
/data/monday/evidence/shadow-gates/<artifact-sha256>/<deployment-bundle-sha256>/gate.json
/data/monday/evidence/shadow-gates/<artifact-sha256>/<deployment-bundle-sha256>/PASSED.sha256
```

The marker hashes exactly that `gate.json`. Evidence also binds the clean source
revision and deployment-bundle SHA-256, so unit or env changes cannot consume an
older gate for the same binary. A short test override is available
only for script testing; it writes `passed=false` and never creates
`PASSED.sha256`, so it cannot authorize cutover.

### 3. Cut over or roll back

After the production gate succeeds, invoke the cutover with the same immutable
artifact digest:

```bash
set -euo pipefail
ACTION=cutover \
INSTANCE_ID=i-REPLACE \
ARTIFACT_SHA256=REPLACE_WITH_64_HEX_DIGEST \
./deployment/aliyun/invoke-rust-lob-operation.sh
```

The host cutover revalidates the binary, release metadata, staged deployment
files, gate JSON, marker hash, duration, full-catalog counts, and OSS round trips.
Only then does it disable and stop the current production units. After production
is stopped, it installs the target production unit/env files.

The drain is bootstrap-safe: it runs the digest-pinned target binary directly
against the canonical production env, so the first upgrade does not depend on
the old production binary supporting `--upload-only`. A new host is accepted
only when the canonical spool contains no segment artifact. The script then
atomically changes the production symlink and starts both services without
enabling them. It verifies fresh full-catalog health, no warnings, zero restarts,
and each process's `/proc/<pid>/exe` resolving to the requested release; only a
verified candidate is enabled for reboot.

Any failure after production stops triggers a fail-closed Rust-to-Rust restore of
the previous digest-addressed binary and its staged production assets. Rollback
removes candidate health, starts the old units while disabled, requires health
written after that restart, and verifies full catalog, zero restarts, and the old
`/proc/<pid>/exe` targets before enabling. If a safe restore cannot be proved,
both production units remain disabled and masked. Cutover evidence
is written under `/data/monday/evidence/cutovers/`.

Rollback uses the same `ACTION=cutover` operation with a previously installed,
previously gated artifact digest. There is no Python fallback and no manual
symlink shortcut.

### Upload cleanup and failure rules

After all three OSS objects upload successfully, the Rust collector atomically
writes an uploaded-cleanup marker. Restart recovery consumes that marker first,
removes local data/manifest/success artifacts idempotently, fsyncs the directory,
and removes the marker last. An interrupted or invalid cleanup marker makes
`--upload-only` fail closed. Normal collection and upload-only drain also share
an exclusive per-spool process lock, so they cannot mutate one market spool
concurrently even if an operator bypasses the systemd transition mask. Recursive
spool scans reject root, directory, and file symlinks rather than crossing into
another market or filesystem.

Do not manually delete a spool, repoint a release symlink, start a second
canonical writer, or bypass `PASSED.sha256`. Do not open general SSH for a
release. When a Cloud Assistant deadline expires, the local wrapper requests
cancellation and waits for a terminal invocation state; host-side `flock`
prevents a retry from racing an earlier operation.
