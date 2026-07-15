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
Data API trade requests to 112 markets per cycle. Markets in the active/finalization
window and failed retries are always selected first; the remaining historical lane
rotates by oldest successful poll so cold-start backfill cannot prevent health from
advancing. `priority_trade_backlog` must be zero for the shadow gate to accept a
health sample, while `deferred_trade_markets` makes bounded historical backfill
explicit rather than silently claiming full-cycle trade coverage. Every Data API
request, including a second pagination request for the same market, passes through a
shared start-time pacer with at least 100ms between request starts. Up to four requests
may remain in flight, and each processing chunk retains at most four market responses,
so slow I/O overlaps without creating an unbounded request or memory fan-out. An
absolute 180-second cycle deadline cancels stalled network work and fails closed;
health evidence over that duration is rejected by the shadow gate.
The 112-request budget and the collector units' 384MiB/512MiB memory high/max
limits are a measured pair: a clean Tokyo cold start covered all 112 priority
markets in 45.091 seconds with zero priority backlog. The health policy pins the
budget so a later default drift cannot silently invalidate that evidence.
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
`cycle_duration_ms` makes the 90-second gate freshness budget directly auditable.
Each append batch rolls back to its starting offset if write or fsync fails, so a retry
cannot duplicate a durable prefix, suppress a required hourly metadata seed, or leave
a partial record behind. A durable per-hour seed marker also forces Rust metadata when
cutover inherits a current-hour Python tape. Discovery is fail-closed unless every
configured asset is present; `health.json.missing_target_symbols` must remain empty.
Neither companion unit contains private keys or an execution command.
After cutover, both companion units use the same `polymarket-raw-ops` Rust binary. Its
`collect-reference` subcommand owns metadata/trade/settlement collection and its
`upload` subcommand owns validation, compression, OSS upload, and remote readback.
The former Python collector and uploader remain installed, but inactive, until the
rollback retention window closes. The one-off canonicalizer is not a runtime service.

Obtain `polymarket-raw-ops` and its SHA-256 from the immutable collector build
artifact and verify the digest from the extracted artifact directory. Do not install
it over the active runtime or replace any production unit manually:

```bash
sha256sum -c polymarket-raw-ops.sha256
candidate_sha=$(awk '{print $1}' polymarket-raw-ops.sha256)
source_revision=$(git rev-parse HEAD)
```

Install the control bundle without changing the active Python units:

```bash
sudo install -d -m 0755 /opt/monday/control/polymarket-raw-ops
sudo install -m 0755 \
  deployment/aliyun/polymarket-raw-ops-{shadow-gate,cutover}.sh \
  /opt/monday/control/polymarket-raw-ops/
sudo install -m 0644 \
  deployment/aliyun/polymarket-legacy-health-policy.jq \
  deployment/aliyun/polymarket-rust-health-policy.jq \
  deployment/aliyun/polymarket-shadow-gate-policy.jq \
  deployment/aliyun/polymarket-reference-collector-shadow@.service \
  deployment/aliyun/polymarket-reference-{collector,upload}.service \
  deployment/aliyun/polymarket-reference-upload.timer \
  deployment/aliyun/polymarket-market-tape-upload.{service,timer} \
  /opt/monday/control/polymarket-raw-ops/
```

Run the isolated Rust shadow while the active unit is still the Python collector.
The gate cannot produce production-eligible evidence before 3600 continuous seconds
plus a verified five-minute comparison tail inside one UTC hour. Both lanes are bounded
to the same successful-poll cutoff (with a safety lag), so new Python rows written after
the Rust shadow stops cannot create a false mismatch. Every invocation gets a unique
run spool beneath the candidate digest, so a failed or expired gate can be rerun without
reusing prior tape data.
Evidence binds both the candidate binary digest and the exact control-bundle digest;
cutover refuses either identity if the bundle changes after shadowing. Control assets
and evidence must remain root-owned and non-writable by the service account, and a
gate older than 24 hours must be rerun.
The gate also writes a content-addressed, root-owned snapshot of the six non-secret
OSS uploader settings beside the candidate binary. Cutover renders both Rust upload
units against that immutable snapshot, so a later edit to the live legacy env file
cannot change the destination represented by the gate evidence.
It fails unless the seven assets have field and stable metadata-contract value parity,
identical non-duplicated in-window trade IDs, settlement parity, an hourly rotation,
fresh fail-closed health, and exact candidate process identity. The candidate must also
upload and read back both a closed reference segment and a deterministic market-tape
fixture under the isolated `crypto_expiry_reference_rust_shadow` and
`crypto_expiry_market_rust_shadow` datasets. Parity comparison itself runs through the
candidate's Rust `verify-shadow-parity` subcommand; the control bundle contains no
separate Python verifier:

```bash
gate_json=$(sudo /opt/monday/control/polymarket-raw-ops/polymarket-raw-ops-shadow-gate.sh \
  ./polymarket-raw-ops "$candidate_sha" "$source_revision")
```

Only the cutover command may replace the active unit. It verifies the immutable gate,
snapshots the installed Python units and scripts, drains with the Python uploader,
stops the Python collector, installs the content-addressed Rust release, and performs
an explicit `systemctl restart`. Before enabling either upload timer it verifies the
new MainPID, `/proc/<pid>/exe` digest, command line, fresh fail-closed health, journal,
and both one-shot upload services. Any failed step automatically restores and restarts
the snapshotted Python runtime:

```bash
cutover_json=$(sudo /opt/monday/control/polymarket-raw-ops/polymarket-raw-ops-cutover.sh \
  cutover "$candidate_sha" "$gate_json")
```

Keep the returned cutover evidence directory for the rollback window. A later manual
rollback uses the same checksum-verified snapshot and confirms the Python PID and
command line after restart:

```bash
cutover_dir=$(dirname "$cutover_json")
sudo /opt/monday/control/polymarket-raw-ops/polymarket-raw-ops-cutover.sh \
  rollback "$cutover_dir"
```

Never use `enable --now` as a substitute for this path: it does not prove that an
already-active Python process was replaced by the gated Rust artifact.

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
- queue, disk, and upload warnings are false, while the persistent upload-failure
  count does not increase during normal segment rotations;
- CPU accounting and peak memory stay inside the systemd limits;
- after stop, the candidate's `--upload-only` drain leaves no partial,
  temporary, corrupt, compressed, success-marker, or cleanup-marker artifact;
- for each market, at least two manifests created after gate start are downloaded
  from OSS with their data object and reproduce the manifest SHA-256.

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
