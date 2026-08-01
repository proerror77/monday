# Aliyun Binance data host

The public-data collector host described below is not the trading host. The
future Tokyo bare-ECS trading contract is documented separately in
[`TRADING_ECS_HOST.md`](TRADING_ECS_HOST.md). Trading does not run on ACK, and
staging its ACR image does not enable or start a runtime.

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
reference prices, records every quote update with the full visible CLOB book
per token, retains every bid and ask level in each snapshot, and drops
orphaned or post-expiry quotes.
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
timestamps, then writes `.ndjson.zst`, `manifest.json`, and `_SUCCESS` under the
immutable `lake/raw/venue=polymarket/dataset=crypto_expiry/date=.../hour=.../sha256=<data-sha>/`
prefix. Uploads are no-clobber, and an existing triplet must match byte for byte before
it is treated as a successful retry. Sessions crossing UTC-hour
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
same content-addressed artifact triplet under
`lake/raw/venue=polymarket/dataset=crypto_expiry_reference/`. Stable trade IDs and a
persisted overlap state prevent duplicate trade prints across polls and restarts.
An in-place v1-to-v2 migration locally isolates the old active tape, reopens completed
markets, and emits a complete v2 overlap. Historical v1 objects remain readable only
under their explicit supersession markers; there is no long-lived canonicalizer
process. After settlement,
trade polling continues for at least 30 minutes after the latest observed change and
requires three additional stable polls before a market is marked complete. Malformed
trade rows are isolated and counted by reason in `health.json` instead of blocking
valid rows.
The collector discovers the full 24-hour settlement lane on every cycle, but bounds
Data API trade requests to 200 markets per cycle. Markets in the active/finalization
window and failed retries are always selected first; the remaining historical lane
rotates by oldest successful poll so cold-start backfill cannot prevent health from
advancing. `priority_trade_backlog` must be zero for the shadow gate to accept a
health sample, while `deferred_trade_markets` makes bounded historical backfill
explicit rather than silently claiming full-cycle trade coverage. Every Data API
request, including a second pagination request for the same market, passes through a
shared start-time pacer with at least 100ms between request starts. Up to four requests
may remain in flight, and each processing chunk retains at most four market responses,
so slow I/O overlaps without creating an unbounded request or memory fan-out. An
absolute 180-second cycle deadline cancels stalled network work and fails closed.
A separate OS-thread watchdog enforces the same wall-clock deadline across synchronous
tape fsync and atomic state publication, where a cooperative Tokio timeout cannot
preempt non-yielding work. Health evidence over that duration is rejected by the
shadow gate.
The 200-request budget and the collector units' 1536MiB/2048MiB memory high/max
limits are a measured pair. The budget was raised from 112 to speed historical
trade backfill without raising concurrency: at most four requests remain in
flight and each chunk retains at most four market responses, so the measured
memory calibration below still bounds the same working set. A Tokyo cold-start
probe covered all 112 priority markets in 31.425 seconds with zero priority
backlog. The retired 384MiB high
watermark prevented health publication, while a later formal shadow reached a
538,951,680-byte peak and continued incrementing `memory.events high` under the
512MiB watermark. July 17 formal gates then measured 586.1MiB, 605.8MiB, and
601.9MiB cold-start peaks under the former 576MiB watermark without reaching
`MemoryMax=768M` or recording an OOM. The 672MiB watermark restores measured
headroom without changing that hard limit. On 2026-08-01 the tracked catalog
(6509 markets, 316k retained trade ids) outgrew the 672MiB high watermark: a
production shadow gate (`329edad2`) failed with `memory.events high` growth
during cold-start backfill. The 1536MiB/2048MiB pair doubles the former
watermark within the Tokyo host budget and is the new calibration for the
enlarged catalog. It is calibration, not promotion
evidence: the formal gate still requires zero high/max/OOM events. The health policy
pins the budget so a later default drift cannot silently invalidate that evidence.
Both reference units reserve up to 80% of one CPU so observed collector work can
complete before the same 180-second fail-closed deadline; the quota does not relax
the deadline, completeness checks, or any execution boundary.
The host, cgroup pressure, invocation IDs, and control probes are recorded in
`docs/reports/polymarket-shadow-memory-calibration-2026-07-16.md`.
The shadow gate allows a separate 60-second initial-health grace after that
deadline so a cycle completing at the boundary can finish durable health
publication before the first sample. This does not relax the 180-second health
policy: a real timeout still exits or restarts the candidate and fails the
identity checks.
The shadow unit uses `Type=exec`, so `systemctl start` returns only after the
pinned Rust executable has completed `execve`; the first PID, executable, and
command-line identity check cannot race a pre-exec service process.
`cycle_started_at` preserves the API snapshot boundary, while `updated_at` and
`last_success_at` are stamped only after tape and state durability completes;
`cycle_duration_ms` makes the 240-second gate freshness budget directly auditable.
Each append batch rolls back to its starting offset if write or fsync fails, so a retry
cannot duplicate a durable prefix, suppress a required hourly metadata seed, or leave
a partial record behind. A durable per-hour seed marker also forces Rust metadata when
cutover inherits a current-hour Python tape. Discovery is fail-closed unless every
configured asset is present; `health.json.missing_target_symbols` must remain empty.
Neither companion unit contains private keys or an execution command.
Both companion units use the same `polymarket-raw-ops` Rust binary. Its
`collect-reference` subcommand owns metadata/trade/settlement collection and its
`upload` subcommand owns validation, compression, OSS upload, and remote readback.
Retired Python collectors and uploaders are rollback inputs only and are never
reintroduced as application code. The root-only shadow-gate and cutover controls
remain part of the release bundle until the Python-to-Rust migration and its rollback
retention window are complete; they do not collect data or execute trades.

The one-hour shadow compares trades by event timestamp and exact stable record ID.
It retains the full common retrieval cutoff but applies a 10-minute trade-event
maturity lag, matching the measured bounded delay of the rotating Data API poller;
the mature legacy and Rust ID sets must still be exactly equal. Trade context is
joined to the latest metadata for each trade's market ID before the cutoff instead
of reusing the settlement-oriented market-end projection, and those two context
snapshots must have the same governed values even when the market is outside the
normal metadata event projection. Evidence includes both trade-ID set differences
and context-value mismatch market IDs.
Settlement parity uses each market's end time with a 15-minute lookback and a
10-minute maturity lag, because independent 30-second polling schedules can record
the same closed market on opposite sides of a wall-clock boundary. Every mature
legacy settlement and metadata record must exist in Rust with the same canonical
value. Metadata parity validates each complete raw provider row, then compares the
immutable identity, timing, outcome/token, tick, size, fee, and `negRisk` projection;
tick and size must be finite and positive, and the fee/`negRisk` flags must be
booleans. Enabled fees must be finite and non-negative; disabled fees may be
absent or null and are normalized to null before comparison. Tick, size,
`feesEnabled`, and `negRisk` are required rather than silently omitted from the
projection. Independently sampled lifecycle fields such as `active`, `closed`, and
`acceptingOrders` are not cross-lane byte-equality inputs. Additional valid Rust
records are allowed, but a legacy-only record or any governed shared-value mismatch
still fails closed. Field parity checks the governed record contract on both lanes
rather than requiring every historical provider field to be identical.

Obtain the immutable collector artifact on the ECS as a root-owned,
non-group/world-writable directory. Run the staging command from the root-owned,
exact source checkout used for that artifact; never execute a script extracted
from an unverified artifact as root. The command requires its own bytes to match
the candidate control release, then verifies and atomically publishes the complete
release. It rejects mixed source identities, wrong sidecars, unexpected or
symbolic control entries, and an existing manifest-addressed destination:

```bash
set -euo pipefail
artifact_dir=$(pwd -P)
source_tree=/root/monday-release-source
stage_command="$source_tree/deployment/aliyun/polymarket-raw-ops-cutover.sh"
source_revision=$(sudo git -C "$source_tree" rev-parse HEAD)
[[ -z $(sudo git -C "$source_tree" status --porcelain --untracked-files=no) ]]
candidate_dir=$(sudo "$stage_command" stage "$artifact_dir" "$source_revision")

candidate_sha=$(jq -er '.candidate.sha256' \
  "$candidate_dir/polymarket-raw-ops-release.json")
gate_control="$candidate_dir/polymarket-raw-ops-gate-control.sh"
sudo "$gate_control" install
gate_status=$(sudo "$gate_control" start \
  "$candidate_dir/polymarket-raw-ops" "$candidate_sha" "$source_revision")
gate_invocation=$(jq -er '.systemd_invocation_id' <<<"$gate_status")

# Run this status command after the supervised systemd Gate becomes terminal.
gate_terminal=$(sudo "$gate_control" status "$candidate_sha" "$gate_invocation")
jq -e '.terminal_state == "passed"' <<<"$gate_terminal" >/dev/null
gate_receipt="/data/monday/evidence/polymarket-gate-jobs/$candidate_sha/$gate_invocation/receipt.json"
pinned_control_dir="/opt/monday/releases/polymarket-raw-ops/$candidate_sha/control"
sudo "$pinned_control_dir/polymarket-raw-ops-cutover.sh" \
  cutover "$candidate_sha" "$gate_receipt"
```

Staging does not replace active global controls or production units. The Gate pins
the verified controls under the immutable candidate release; only a successful
cutover may install them globally.

The Rust shadow unit must complete its configured observation window and publish
fresh fail-closed health before the reference collector is promoted. Evidence binds
the candidate binary digest, source revision, symbol set, settled market payloads,
and upload readback. Any stale health, missing symbol, sequence gap, or identity
mismatch blocks promotion. Live execution remains disabled; this lane only collects
and archives public market data.
The gate detects either the legacy Python writer or the active immutable Rust
release. A legacy-Python Gate does not wait for health publication or freeze the
writer identity; the persistent legacy spool is the parity input and Python remains
untouched for rollback. Cutover binds the current canonical Python identity only
for its short transition. An active immutable Rust baseline still freezes its PID,
systemd restart counter, and `InvocationID` throughout the Gate. After the baseline
writer is stopped, cutover explicitly resets the inherited counter and requires the
new Rust process to remain at zero for post-start verification.
PID and `NRestarts` are not sufficient across the final stop boundary, so the gate
also freezes each unit's systemd `InvocationID`. Immediately before stopping the
shadow or baseline writer, the control captures a synced journal cursor; after the
stop it scans only records after that cursor, rejects evidence of a new invocation
or restart, and rechecks the frozen restart counter. This closes the race between
the final live identity check and `systemctl stop`.

A cutover is successful only when its evidence directory contains `cutover.json`
and an adjacent, single-line `PASSED.sha256` that verifies exactly that JSON with
`sha256sum --check --strict`. Either file by itself is provisional and must not be
treated as promotion evidence. Any failed transition or automatic or requested
rollback invalidates that success pair; the retained failed/rollback artifacts are
for forensic review only and cannot authorize Rust production. Before rollback can
change any service, the marker is atomically renamed to
`PASSED.rollback-pending.sha256` and synced. A failed or interrupted restore therefore
leaves an explicit pending marker rather than stale Rust-production authorization;
successful automatic recovery finalizes it as `PASSED.invalid.sha256`, while a
requested rollback finalizes it as `PASSED.rolled-back.sha256`. Each renamed marker
continues to verify the unchanged `cutover.json`; none can substitute for the exact
canonical `PASSED.sha256` required to authorize Rust production.

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
even if the runtime stalls. The watchdog remains armed across ordinary session
reconnects, but global shutdown disarms it before shutdown becomes visible to
session tasks. This prevents bounded final segment compression (up to
`ZSTD_TIMEOUT_SECONDS`) from being mistaken for a market-data stall. Low disk
space emits a warning but does not pause collection. Successfully uploaded segments are deleted
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
cores and less than 1GiB of service memory. The 80% CPU quota and 3.5GiB memory
limit on each service preserve host headroom during bursts.

## Binance USD-M reference collector lane

The `binance-usdm-reference-collector` publishes a complete snapshot of every
active Binance USD-M perpetual contract every 30 seconds: exchange metadata,
mark/index/funding observations, and open interest, all anchored to the
official `https://fapi.binance.com` server clock. Each poll is published as an
atomic no-clobber batch triplet (`reference.ndjson`,
`reference.ndjson.manifest.json`, `reference.ndjson._SUCCESS`) under
`/data/monday/spool/binance-usdm-reference/lake/raw/venue=binance_usdm/dataset=reference/date=.../hour=.../batch=<observed-at-ns>/`
and is verified by canonical readback before the batch is visible. The lane
has no credentials and no execution path; it is public market data only.

Promotion follows the same three-operation contract as the LOB archiver:
install a digest-addressed release under
`/opt/monday/releases/binance-usdm-reference-collector/<candidate-sha256>/`,
pass the one-hour isolated shadow gate
(`binance-usdm-reference-collector-shadow@<candidate-sha256>.service` observed
by `binance-usdm-reference-shadow-gate.sh`), then promote with
`binance-usdm-reference-cutover.sh <candidate-sha256>`. The shadow bundle
(`binance-usdm-reference-control.tar.gz`) keeps exactly the three gate assets;
the production units ship in the separate
`binance-usdm-reference-production-control.tar.gz` bundle, so production
assets can never invalidate a completed shadow gate.

The cutover is green-field only: it refuses any existing production unit,
symlink, or canonical lake artifact. It revalidates the release identity, the
uploader sidecar digest, the production bundle manifest, and exactly one
immutable `PASSED.sha256` shadow gate before touching systemd. It then
installs the production units, points
`/opt/monday/bin/binance-usdm-reference-collector` and
`/opt/monday/bin/binance-usdm-reference-upload` at the digest-addressed
release, starts the collector without enabling it, requires fresh verifier-
checked health, proves one OSS round trip by draining the spool with the
candidate uploader, requires fresh post-drain health, and only then enables
`binance-usdm-reference-collector.service` and
`binance-usdm-reference-upload.timer`. Failure after the transition starts
disables and runtime-masks every lane unit and removes candidate symlinks.
Evidence is append-only under
`/data/monday/evidence/binance-usdm-reference-cutovers/` and is valid only as
the `cutover.json` plus single-line `PASSED.sha256` pair verified with
`sha256sum --check --strict`.

The production uploader runs every five minutes from
`binance-usdm-reference-upload.timer`. It re-verifies each local batch with
the canonical artifact verifier, uploads the triplet to
`oss://monday-lob-apne1-1045353359/lake/raw/venue=binance_usdm/dataset=reference/...`
with `aliyun ossutil cp --ignore-existing` under the `ecs-role` profile,
downloads all three objects back, and re-runs the verifier on the remote
bytes before deleting the local batch. An existing remote triplet must match
byte for byte; a conflicting object fails closed and the local batch is
retained. A bad batch never blocks later batches, and failures surface in
`/data/monday/spool/binance-usdm-reference/upload-status.json`.

The 3072MiB high and 3584MiB max watermarks are a measured pair, calibrated the
same way as the Polymarket reference collector limits above. On 2026-07-28 the
USD-M production archiver stopped writing data while its cgroup sat at the
former 2500MiB high watermark: `/proc` showed `__mem_cgroup_handle_over_high`,
so memory-high reclaim throttling stalled the process in D state at about
2.75GiB RSS without ever reaching `MemoryMax=3200M` or recording an OOM. That
throttled footprint understates real demand because the initial full-catalog
snapshot sync allocates above the steady-state working set. The 3072MiB
watermark restores measured headroom above the 2.75GiB footprint, and the
3584MiB hard limit covers initial-sync and catalog-growth overshoot. Host
budget: the 7.5GiB host reserves about 1GiB for the OS and journald, the
Polymarket tape and reference collector cap at 512MiB and 768MiB, and the
short-lived upload units are transient, leaving about 5.25GiB of steady-state
budget for both archivers. Both instances share this template, so the new caps
formally oversubscribe that budget at the hard limit; this is accepted because
the watermarks are caps rather than reservations, the Spot instance currently
measures healthy below the former watermark, and `MemoryMax` remains the OOM
backstop protecting the OS. If the Spot working set later grows to the USD-M
level, the host needs a resize rather than a further raise. It is calibration,
not promotion evidence: the production gate still requires CPU accounting and
peak memory to stay inside the systemd limits.

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
does not overwrite production configuration or the production symlink. A
pre-existing artifact directory is reusable only when its binary, deployment
assets, artifact URI, bundle digest, bundle URI, and source revision all match
exactly; otherwise installation fails instead of rewriting historical release
evidence. First installation is assembled in a sibling directory and renamed
into place only after all identity checks pass.

The committed shadow environments use `SYMBOLS=ALL`, ten-minute segments, the
isolated spools below, and isolated OSS datasets:

| Market | Shadow spool | Shadow dataset |
| --- | --- | --- |
| Spot | `/data/monday/spool/binance-lob-rust-shadow/spot` | `spot_all_rust_shadow` |
| USD-M | `/data/monday/spool/binance-lob-rust-shadow/usdm` | `usdm_perpetual_all_rust_shadow` |

Each session proves the exact expected subscription set on every WebSocket
shard with `LIST_SUBSCRIPTIONS` before it requests snapshots. A
`binance.market_tape.v2` candidate declares its per-symbol stream-type list
(`depth@100ms`, `aggTrade`, `trade`, `bookTicker`, plus USD-M-only
`forceOrder`) in the manifest and every `session_start` row, and coverage is
verified against that declared list. A `binance.market_tape.v1` candidate
keeps the legacy depth-plus-`aggTrade` pair and never carries the new
families, so the same gate can still gate a v1 binary during the transition.
Every segment also retains the sorted stream list returned for each shard in a
SHA-bound `stream_coverage` row, so canonical readback can recompute the exact
catalog (symbols x declared stream types for v2, symbols x 2 for v1) instead
of trusting a boolean alone. The resulting checkpoints, health, and manifests
carry the derived coverage summary; health also publishes an explicit
`full_stream_coverage_verified` decision so deploy policies can pin
full-family coverage without weakening the depth-only readiness fields (a v1
collector never publishes the field, and its absence stays acceptable so a
rollback to a v1 binary remains possible during the transition). A
symbol that receives no depth or trade event during
a segment is complete only when it has an unchanged two-sided snapshot-backed
checkpoint and verified stream coverage; the collector never invents a diff or
trade for a static symbol. Every segment must still contain at least one real
`agg_trade` for its market dataset, and a v2 segment must additionally carry
`raw_trade` and `book_ticker` events for the same scope.

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
- every discovered symbol has a ready two-sided snapshot, exact WebSocket stream
  coverage is verified, and sequence gaps remain zero;
- neither session nor catalog membership changes, health never stops advancing
  for more than 120 seconds, and the persistent upload-failure count is unchanged;
- queue, disk, and upload warnings are false, while the persistent upload-failure
  count does not increase during normal segment rotations;
- CPU accounting and peak memory stay inside the systemd limits;
- after stop, the candidate's `--upload-only` drain leaves no partial,
  temporary, corrupt, compressed, success-marker, or cleanup-marker artifact;
- for each market, at least two manifests opened after health settles and the
  observation starts are downloaded from OSS with their data object and
  reproduce the manifest SHA-256; each manifest contains real aggregate trades,
  complete checkpoint coverage, and either sequence-checked diffs or explicit
  static-symbol evidence derived from the verified subscription set.

A successful production gate writes:

```text
/data/monday/evidence/shadow-gates/<artifact-sha256>/<deployment-bundle-sha256>/runs/<run-id>/run.json
/data/monday/evidence/shadow-gates/<artifact-sha256>/<deployment-bundle-sha256>/runs/<run-id>/gate.json
/data/monday/evidence/shadow-gates/<artifact-sha256>/<deployment-bundle-sha256>/runs/<run-id>/PASSED.sha256
```

Every invocation gets a new append-only run directory; prior gate evidence is
never deleted or replaced. The marker hashes exactly that run's `gate.json`.
Evidence also binds the clean source revision and deployment-bundle SHA-256, so
unit or env changes cannot consume an older gate for the same binary. A second
production gate for an identity that already has a passing run is refused, and
cutover requires exactly one immutable passing run. A short test override is
available only for script testing; it writes `passed=false` and never creates
`PASSED.sha256`, so it cannot authorize cutover.

For a one-time upgrade from the pre-release layout, where the running Rust
binary is still a regular file instead of a digest-addressed symlink, use
`host-rust-lob-adopt-production-release.sh` through Cloud Assistant before the
cutover. Pin both the running binary digest and the already gated candidate
digest. The helper never starts, stops, restarts, enables, or disables a unit.
It verifies fresh full-catalog production health and stable PIDs/restart counts,
copies the byte-identical running binary and current rollback assets into an
adopted release, installs an inactive/non-installable rollback-compatibility
upload unit, atomically replaces the regular path with the identical release
symlink, and writes immutable adoption evidence. Any failure restores the
original regular binary and the upload unit's original absent state. This helper
is intentionally not part of the candidate deployment bundle, so using it does
not mutate or invalidate an already completed shadow gate. It is not a general
manual-symlink escape hatch and refuses partial, drifted, unhealthy, or already
modern release layouts.

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
is stopped, it installs the target production unit/env files. Deleted legacy
Python instance units must be inactive and disabled before the transition; they
are included in the transition mask so they cannot become a second canonical
writer.

The drain is bootstrap-safe: it runs the digest-pinned target binary directly
against the canonical production env, so the first upgrade does not depend on
the old production binary supporting `--upload-only`. A new host is accepted
only when the canonical spool contains no segment artifact. The script then
atomically changes the production symlink and starts both services without
enabling them. It verifies fresh full-catalog health, no warnings, zero restarts,
and each process's `/proc/<pid>/exe` resolving to the requested release; only a
verified candidate is enabled for reboot.

Any failure after production stops triggers a fail-closed Rust-to-Rust restore of
the previous digest-addressed binary. Before production stops, its deployment
assets are copied into the unique cutover evidence directory and covered by a
SHA-256 manifest; mutable `/etc` files are never written back into an old
digest-addressed release. Rollback verifies that snapshot before use, removes
candidate health, starts the old units while disabled, requires health written
after that restart, and verifies full catalog, zero restarts, and the old
`/proc/<pid>/exe` targets before enabling. If a safe restore cannot be proved,
both production units remain disabled and masked. Cutover evidence is written
under `/data/monday/evidence/cutovers/`.

Rollback uses the same `ACTION=cutover` operation with a previously installed,
previously gated artifact digest. There is no Python fallback and no manual
symlink shortcut.

### Upload cleanup and failure rules

After all three OSS objects upload successfully, the Rust collector atomically
writes an uploaded-cleanup marker. Restart recovery consumes that marker first,
derives the only permitted data/manifest/success names from the marker's segment
name, validates all three before deleting any file, removes them idempotently,
fsyncs the directory, and removes the marker last. Cleanup temp files use
exclusive creation and refuse symlinks or other non-regular stale paths. An
interrupted or invalid cleanup marker makes
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

Full-catalog symbol discovery has a 15-second HTTP request timeout, so a stalled
Binance `exchangeInfo` response fails startup instead of leaving an active but
idle service until the systemd runtime limit.
