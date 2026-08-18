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

Closed Polymarket sessions are validated, compressed, and uploaded five minutes
after the prior run finishes by `polymarket-market-tape-upload.timer`. The uploader
ignores the active
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

A watchdog guards both upload lanes against silent stalls (issue #655: the
market upload timer once sat inactive for 19 hours until the spool disk nearly
filled, and the reference upload timer was later found silently dead as well).
`polymarket-market-tape-upload-watchdog.timer` runs the oneshot
`polymarket-market-tape-upload-watchdog.service` every two minutes. Each run
logs one journal INFO line (tag `polymarket-upload-watchdog`) with, for each of
the market (`/data/monday/spool/polymarket`) and reference
(`/data/monday/spool/polymarket-reference`) spools, the pending rotated-tape
count and the oldest rotated-tape age, plus `/data` free gigabytes. For each
lane independently: if the upload timer (`polymarket-market-tape-upload.timer`
or `polymarket-reference-upload.timer`) is not active, the watchdog starts it
and logs a WARNING with the previous state; if the upload service is inactive
while a rotated tape is older than 90 minutes, it starts the service with
`--no-block` and logs why. A failed start is logged as an ERROR naming the
lane, the remaining lane is still checked, and the run exits nonzero at the
end so the failure surfaces in the oneshot unit result. The watchdog only ever
starts units — it never stops or disables anything and never modifies tape
files. It runs as root because it needs `systemctl start`; the remaining
hardening matches the other units.

Governed cutovers stop both upload timers and services on purpose, so the
watchdog honors the runtime suppression file
`/run/monday/polymarket-upload-watchdog.suppress`: while it exists, every run
logs one `suppressed` INFO line and performs no remediation on either lane.
The file lives under `/run` and never survives a reboot. Any controller that
stops the upload timers MUST create the file before stopping them and remove
it immediately after the cutover concludes:

```bash
sudo install -D /dev/null /run/monday/polymarket-upload-watchdog.suppress
sudo rm -f /run/monday/polymarket-upload-watchdog.suppress
```

Install and enable it alongside the upload units. This is a governed runtime
change with one named controller:

- Controller: `CONTROLLER_NAME` (one named operator; no concurrent writers).
- Target: host `monday-trade-data-26`, units
  `polymarket-market-tape-upload-watchdog.service` and
  `polymarket-market-tape-upload-watchdog.timer`.
- Source identity: this repository at commit `SOURCE_REVISION`, files
  `deployment/aliyun/polymarket-market-tape-upload-watchdog.sh`,
  `deployment/aliyun/polymarket-market-tape-upload-watchdog.service`, and
  `deployment/aliyun/polymarket-market-tape-upload-watchdog.timer`.
- Stop rules: abort if `/data` is not mounted, if either existing upload timer
  is failed rather than merely inactive, or if the post-install readback does
  not match; rollback rather than retry in place.
- Rollback: `sudo systemctl disable --now polymarket-market-tape-upload-watchdog.timer`,
  then remove the three installed files and `sudo systemctl daemon-reload`.

```bash
sudo install -m 0755 deployment/aliyun/polymarket-market-tape-upload-watchdog.sh \
  /opt/monday/bin/polymarket-market-tape-upload-watchdog.sh
sudo install -m 0644 deployment/aliyun/polymarket-market-tape-upload-watchdog.service \
  deployment/aliyun/polymarket-market-tape-upload-watchdog.timer \
  /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now polymarket-market-tape-upload-watchdog.timer
```

Post-install readback (required before the change is considered live):

```bash
systemctl is-active polymarket-market-tape-upload-watchdog.timer
journalctl -t polymarket-upload-watchdog -n 5
```

## Durable monitoring

The 2026-08-05/06 disk-full incident was silent because the only on-host
monitor (`polymarket-market-tape-upload-watchdog.sh`) self-heals but never
alerts a human: every governed collector stopped, uploads failed, and
delay-gate trips accumulated with nobody notified. This host now has a durable,
read-only health monitor with two alert channels: a Cloud Monitor disk alarm
(primary) and a scheduled GitHub Actions workflow (fallback).

### Health script contract

`deployment/aliyun/monday-collector-health.sh` is a POSIX `sh`, read-only
monitor. It never starts, stops, enables, or disables a unit and never modifies
tape files or `upload-status.json`. It emits one JSON snapshot (or a human
`ok:`/`breach:` summary) and exits nonzero when any breach is present. It logs
to journald tag `monday-collector-health`. Run with `--json` for machine output
and `--dry-run` to avoid reading or writing the persistent delta state.

The monitor has six hard gates. Each is a breach: it fails closed into
the `monitor-collector-host` workflow issue and blocks `ok:true`.

| Hard gate | Breach condition |
| --- | --- |
| 1. Status file | `upload-status.json` missing, a symlink, or unparseable on the mandated lanes (`binance-lob` spot/usdm, `binance-fee`) |
| 2. Upload freshness | `last_success_at` missing/unparseable, or older than the lane bound (LOB 7200s, fee 600s, usdm-reference 1200s, polymarket 7200s, bybit 5400s; each just above the lane's upload cadence — the polymarket lanes rotate tapes hourly, so the 5-minute upload timer is not a heartbeat). On the polymarket lanes a pending rotated tape with a last success older than 1800s breaches earlier: an upload stall with a live backlog must alert within 30 minutes |
| 3. Pending backlog | pending count over the lane limit, or oldest pending artifact older than the lane age bound, using each collector's own pending definition (LOB `*.manifest.json`, fee/usdm-reference `lake/raw/**/batch=*`, polymarket rotated `market-updates.*.ndjson` tapes, bybit marked `.ndjson` without `.uploaded.json`) |
| 4. Upload failures | `last_error_at`/`last_error` present, or a `failure_count` increase since the previous poll (prior counts live under `/var/lib/monday-collector-health`) |
| 5. `/data` disk | free < 15% (used >= 85%) via `df -Pk /data` — the 2026-08-17/18 incidents reached 100% twice, so the critical watermark pages a human instead of only warning |
| 6. Polymarket upload timers | `polymarket-market-tape-upload.timer` or `polymarket-reference-upload.timer` not active (waiting) while its collector service (`polymarket-market-tape.service` / `polymarket-reference-collector.service`) is active — a stopped timer with a running collector silently strands rotated tapes until the disk fills |

The raw-ops Gate template has no `[Install]` section, so `static` is the
healthy installed state only when no Gate instance is active, the control lock
is free, and the Gate runtime root has no residual `*.env` (uninspectable
state remains a breach). State-persistence failures also stay breaches (gate 4
delta detection depends on that state).

Every other check is a warning — reported in the JSON `warnings` array and as
`warning:` lines, never blocking `ok:true`:

| Warning | Condition |
| --- | --- |
| `/data` disk | free < 25% (warn) via `df -Pk /data`; free < 15% is hard gate 5 above |
| Governed services | `binance-lob-archiver-production@spot/usdm`, `binance-usdm-reference-collector`, `bybit-options-archiver` active AND enabled AND `Result==success`, plus a restart-rate delta > 1 since the last poll |
| Upload lane units | upload/watchdog/fee timers active AND enabled; their oneshot services' last `Result==success` |
| `health.json` | missing/unparseable, wall-clock age of `updated_at_ns` > 300s, or `sequence_gaps` > 0 (spot + usdm spools) |
| Delay-gate trips | > 0 journald `source-to-receive delay exceeds the governed limit` lines per Binance unit in the last 15 minutes |
| Fee snapshot failures | > 0 `Failed with result` journald lines per fee snapshot unit in the last 10 minutes |
| `/data` mount | `mountpoint -q /data` fails (the monitor must DETECT a missing mount, not gate on it) |

The persistent-service check deliberately does not warn on `NRestarts > 0`:
both Binance archivers restart every six hours by design
(`RuntimeMaxSec=21600`). Crash loops are detected through `Result != success`
or an `NRestarts` delta greater than one between consecutive five-minute polls.

Test/override environment for fixtures and containers:
`MONDAY_COLLECTOR_SPOOL_ROOT` (default `/data/monday/spool`) and
`MONDAY_COLLECTOR_STATE_DIR` (default `/var/lib/monday-collector-health`).
`test-monday-collector-health.sh` is the self-contained contract test.

### Install and timer deploy

This is a governed runtime change with one named controller, following the same
template as the upload watchdog above:

- Controller: `CONTROLLER_NAME` (one named operator; no concurrent writers).
- Target: host `monday-trade-data-26`, units
  `monday-collector-health.service` and `monday-collector-health.timer`.
- Source identity: this repository at commit `SOURCE_REVISION`, files
  `deployment/aliyun/monday-collector-health.sh`,
  `deployment/aliyun/monday-collector-health.service`, and
  `deployment/aliyun/monday-collector-health.timer`.
- Stop rules: abort if the installed files or the script do not match the
  source, or if the post-install readback does not match; rollback rather than
  retry in place.
- Rollback:
  `sudo systemctl disable --now monday-collector-health.timer`, then remove the
  three installed files, `sudo rm -rf /var/lib/monday-collector-health`, and
  `sudo systemctl daemon-reload`.

```bash
sudo install -m 0755 deployment/aliyun/monday-collector-health.sh \
  /opt/monday/bin/monday-collector-health.sh
sudo install -m 0644 deployment/aliyun/monday-collector-health.service \
  deployment/aliyun/monday-collector-health.timer \
  /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now monday-collector-health.timer
```

Post-install readback (required before the change is considered live):

```bash
systemctl is-active monday-collector-health.timer
/opt/monday/bin/monday-collector-health.sh --json
systemctl status monday-collector-health.service --no-pager -n 5
```

The service must NOT add `ConditionPathIsMountPoint=/data`: the whole point of
the mount check is to detect and alert when `/data` is missing.

### Data completeness check

`deployment/aliyun/data-completeness-check.sh` (#882) is a POSIX `sh`,
read-only OSS reconciliation: it compares EXPECTED vs ACTUAL hour partitions
in the `lake/raw/` lake for every production dataset and fails closed (exit 1)
on any missing partition, triplet violation, or OSS listing failure. It guards
against silent hour-level holes like the 2026-08-14 audit findings (a ~36-hour
USD-M gap, scattered Bybit single-hour losses, Polymarket data stopping ~12
hours before the reported outage).

Governed datasets and their completeness rules:

| Dataset | Lake prefix | Rule |
| --- | --- | --- |
| `binance-spot` | `venue=binance/market=spot/dataset=spot_all/shard=all/` | hour present; each `*.jsonl.zst` carries `.manifest.json` + `._SUCCESS` |
| `binance-usdm` | `venue=binance/market=usdm/dataset=usdm_perpetual_all/shard=all/` | same triplet |
| `bybit-options` | `venue=bybit/market=option/dataset=options_quotes/` | hour present; each `*.ndjson.zst` carries its `.zst`-stripped `.manifest.json` (no `_SUCCESS` by design) |
| `polymarket-crypto-expiry` | `venue=polymarket/dataset=crypto_expiry/` | hour present; triplet |
| `binance-usdm-reference` | `venue=binance_usdm/dataset=reference/` | hour presence only (batch-partitioned, listed with `-d`) |

An hour is expected once it has ended and the per-dataset grace lag has passed
(default 1 hour: the current hour is still collecting and the previous hour may
still be in flight). Configuration: `COMPLETENESS_WINDOW_DAYS` (default 2),
`COMPLETENESS_GRACE_HOURS` plus per-dataset
`COMPLETENESS_GRACE_HOURS_{SPOT,USDM,BYBIT,POLYMARKET,REFERENCE}`, and the
usual `OSS_BUCKET`/`OSS_ENDPOINT`/`OSS_REGION`/`ALIYUN_PROFILE`. The JSON
report (`--json`, or `--output FILE`) carries per-dataset `expected_hours`,
`present_hours`, `missing_partitions`, `triplet_violations`,
`latest_landed_hour`, and `lag_seconds`.
`test-data-completeness-check.sh` is the self-contained offline contract test
(stubbed `aliyun ossutil ls` over a fixture lake).

Install (a governed runtime change, same controller/target/rollback template
as the health monitor above):

```bash
sudo install -m 0755 deployment/aliyun/data-completeness-check.sh \
  /opt/monday/bin/data-completeness-check.sh
sudo install -m 0644 deployment/aliyun/data-completeness-check.service \
  deployment/aliyun/data-completeness-check.timer \
  /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now data-completeness-check.timer
```

The timer runs hourly at :35 (after the :23 upload sweep); the oneshot writes
its JSON report to `/var/lib/monday-data-completeness/report.json`. Rollback:
`sudo systemctl disable --now data-completeness-check.timer`, remove the three
installed files, `sudo rm -rf /var/lib/monday-data-completeness`, and
`sudo systemctl daemon-reload`.

### Cloud Monitor disk alarm (PRIMARY)

The on-host timer and the GitHub workflow cannot alert if the host is
unreachable or the repository cannot run, so the primary alert must live in
Aliyun Cloud Monitor on the ECS itself. Create a metric alarm for instance
`i-6we6afeqsvv8uo1ixmyo` on the root-disk utilization metric with two
threshold tiers (an alarm group is configured per tier):

- warn: disk free < 25% — about 5.6 hours of runway at the 2026-08-05/06 fill
  rate (~120-130 GiB/day) before the 10% threshold.
- critical: disk free < 10% — collection failure territory.

Representative alarm configuration (adjust `ContactGroups` and the webhook, and
apply through the Cloud Monitor console so field names match the current API
version):

```json
{
  "ruleName": "monday-collector-disk-warn",
  "instanceId": "i-6we6afeqsvv8uo1ixmyo",
  "metricName": "DiskUtilization",
  "statistics": "Average",
  "period": 300,
  "comparisonOperator": "GreaterThanThreshold",
  "threshold": 75,
  "evaluationCount": 1,
  "contactGroups": ["monday-oncall"],
  "notifyType": "warning",
  "recoverNotify": true,
  "level": "WARN"
}
```

```json
{
  "ruleName": "monday-collector-disk-critical",
  "instanceId": "i-6we6afeqsvv8uo1ixmyo",
  "metricName": "DiskUtilization",
  "statistics": "Average",
  "period": 300,
  "comparisonOperator": "GreaterThanThreshold",
  "threshold": 90,
  "evaluationCount": 1,
  "contactGroups": ["monday-oncall"],
  "notifyType": "critical",
  "recoverNotify": true,
  "level": "CRITICAL"
}
```

The `monday-oncall` contact group must contain both an email address and a
DingTalk webhook robot, and recovery notification must be enabled so a resolved
incident is visible. Cloud Monitor is the PRIMARY channel because it fires even
when every GitHub and repository path is down.

Live alarm identity (created 2026-08-08 via `PutResourceMetricRule`,
readback-verified with `DescribeMetricRuleList`; both rules enabled,
`AlertState: OK`, recovery notification on):

- `monday-collector-disk-warn` — `acs_ecs_dashboard/diskusage_utilization`,
  instance `i-6we6afeqsvv8uo1ixmyo`, Average > 75% for 3 consecutive 60s
  periods, Warn level, contact group `云账号报警联系人`.
- `monday-collector-disk-critical` — same metric and scope, Average > 90% for
  3 consecutive 60s periods, Critical level, same contact group.

Deviation from the representative JSON above: period 300 is not a supported
alarm period for `diskusage_utilization` (the metric supports 15/60/900), so
the live rules evaluate 60s × 3 consecutive periods.

Open gaps: (1) the CloudMonitor guest agent does not report
`diskusage_utilization` for this instance (zero datapoints in the 7 days before
2026-08-08), so the alarms cannot fire until the agent is installed on the
host; (2) the `monday-oncall` contact group with email + DingTalk webhook does
not exist yet — the rules currently notify only `云账号报警联系人` and have no
webhook.

### GitHub Actions workflow (fallback)

`.github/workflows/monitor-collector-host.yml` runs every 15 minutes. It
refreshes `origin/main`, invokes `/opt/monday/bin/monday-collector-health.sh
--json` on the host through Cloud Assistant (the same RunShellScript/Base64
invoke pattern as `invoke-rust-lob-operation.sh`), decodes the JSON, and when
`ok:false` opens or appends a single `needs-triage` GitHub issue (deduped by
open-issue search) following the `docs/agents/issue-tracker.md` lifecycle. It
also opens an issue when the invocation itself cannot return a snapshot.

Prerequisites: repository secrets `ALIYUN_ACCESS_KEY_ID` and
`ALIYUN_ACCESS_KEY_SECRET` whose RAM policy is scoped to
RunCommand/DescribeInvocationResults on `i-6we6afeqsvv8uo1ixmyo` in
`ap-northeast-1` (no broader ECS or OSS grants). See the workflow header.

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
shared start-time pacer with at least 125ms between request starts. Up to four requests
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
When the shadow defers trade emission until settlement plus the 1800-second
finalization lag plus stable polls (post-#680 semantics) while the baseline
emits trades continuously, no mature shadow trade can exist inside the gate
window, so the trade coverage, field, and byte trio is unsatisfiable by
construction. The gate then adjudicates that trio instead of requiring it from
the verifier: settlement, metadata, rotation, asset, and dedupe parity must
still pass, the shadow must not emit any trade the baseline lacks, the shadow
finalization pipeline must demonstrably advance during the observation (a
growing stable-poll maximum — zero until the lag elapses — or a growing settled
maximum, from running-maxima samples of the bounded fresh-spool state), and the
canonical upload still runs. Emission modes are classified by the
collect-reference CLI contract: #680 removed the --max-retained-trade-ids flag
exactly when deferred finalization replaced per-poll emission, and any probe
uncertainty classifies continuous so full trade parity applies. The
verdict records the applied trade parity mode, both detected emission modes,
the raw verifier checks, and the reason (issue #868). When both sides share
the same emission semantics, full trade parity applies unchanged.
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
`binance-usdm-reference-cutover.sh <candidate-sha256> <controller>`. The
shadow bundle (`binance-usdm-reference-control.tar.gz`) keeps exactly the
three gate assets;
the production units ship in the separate
`binance-usdm-reference-production-control.tar.gz` bundle, so production
assets can never invalidate a completed shadow gate.

The cutover accepts either an empty new host or one healthy digest-addressed
production release. On an upgrade it first binds the independently released
old collector and uploader, destination environment, and rollback assets by
digest, stops the writer, and drains every
V2 artifact before switching schemas. It revalidates the candidate release
identity, the uploader sidecar digest, the production bundle manifest, and
exactly one immutable `PASSED.sha256` shadow gate before touching systemd. It then
installs the production units, points
`/opt/monday/bin/binance-usdm-reference-collector` and
`/opt/monday/bin/binance-usdm-reference-upload` at the digest-addressed
release, starts the collector without enabling it, requires fresh verifier-
checked health, proves one OSS round trip by draining the spool with the
candidate uploader, requires fresh post-drain health, and only then enables
`binance-usdm-reference-collector.service` and
`binance-usdm-reference-upload.timer`. Failure after the transition starts
disables and runtime-masks every lane unit. On an upgrade it drains any
possible V3 output before restoring V2; if that drain cannot be proven empty,
production remains fail-closed. On a new host it removes candidate symlinks.
The named controller and both release/rollback identities are recorded in
cutover evidence.
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

## Bybit Options collector lane

The `bybit-options-archiver` is the repo-governed Bybit v5 options market-data
collector. It subscribes to the public Bybit v5 options public-trade and ticker
WebSocket feeds, discovers the full option symbol catalog, and writes immutable
hourly segments of canonical quote events under
`/data/monday/spool/bybit-options/lake/raw/venue=bybit/market=option/dataset=options_quotes/...`
(`.ndjson` while active, `.zst` after compression). The uploader publishes the
compressed segments to
`oss://monday-lob-apne1-1045353359/lake/raw/venue=bybit/market=option/...` under
the `ecs-role` profile and verifies each object by readback before recycling the
source segment.

The lane was brought back after the 2026-08-05/06 Aliyun disk-full incident
caused by the unmanaged Bybit options archiver. Three defects caused unbounded
local growth and were fixed in this governed lane:

1. **No spool cap and no low-disk gate.** `MIN_FREE_GB` (default `20.0`) and
   `BYBIT_OPTIONS_SPOOL_MAX_BYTES` (default `53687091200`, 50 GiB) are enforced
   by the writer before opening or rotating a segment and by the uploader before
   compressing. Both bail out fail-closed, and `disk_free_gb`, `disk_warning`,
   and `spool_warning` are surfaced in `health.json`.
2. **Uploader never recycled the source segment.** The uploader now writes a
   `.uploaded.json` marker only after verified OSS readback and then recycles
   the raw `.ndjson`. The `.zst` is retained locally as a bounded fallback and
   swept after `BYBIT_OPTIONS_LOCAL_ZST_RETENTION_SECONDS` (default 2 days).
3. **Bare WS connect and fixed 2 s reconnect.** The WebSocket handshake now
   carries a `monday-bybit-options-archiver/<rev>` User-Agent plus Origin and
   app_id headers per Bybit v5 requirements, and reconnects use bounded
   exponential backoff `(backoff*2).min(30)`, reset to 1 s on success.

### Units and environment

- `bybit-options-archiver.service` — the collector, `RuntimeMaxSec=21600`,
  `AssertPathIsMountPoint=/data`,
  `ReadWritePaths=/data/monday/spool/bybit-options`,
  `CPUQuota=80%`, `MemoryHigh=1G`/`MemoryMax=1536M`.
- `bybit-options-upload.service` — `--upload-only` oneshot uploader run by
  `bybit-options-upload.timer` (5-minute cadence), same fail-closed env.
- Governed env baked into both units: `MIN_FREE_GB=20.0`,
  `BYBIT_OPTIONS_SPOOL_MAX_BYTES=53687091200`,
  `BYBIT_OPTIONS_SPOOL_DIR=/data/monday/spool/bybit-options`,
  `BYBIT_OPTIONS_LOCAL_ZST_RETENTION_SECONDS=172800`, and the OSS identity. The
  deploy lane refuses a rendered unit that drops `MIN_FREE_GB` or the spool cap.

### Promotion (staging -> shadow gate -> cutover)

1. **Stage** a digest-addressed release with
   `bybit-options-archiver-deploy.sh install <artifact-dir> <source-revision>`.
   The script stages `/opt/monday/releases/bybit-options-archiver/<sha>/`
   (binary + deployment bundle + `release.json`), renders the systemd units, and
   points `/opt/monday/bin/bybit-options-archiver-shadow` at the candidate. It
   never starts production.
2. **Gate** the candidate for at least one hour with
   `host-bybit-options-shadow-gate.sh <candidate-sha256>`. The shadow runs as a
   transient `bybit-options-shadow.service` against the isolated
   `/data/monday/spool/bybit-options-shadow`, settles to full-catalog health,
   and is observed for the full duration against the runtime health policy
   (`bybit-options-runtime-health-policy.jq`), monotonic freshness
   (`bybit_options_observe_health_freshness`), zero restarts, and a fail-closed
   drain. Passing evidence is append-only under
   `/data/monday/evidence/bybit-options-shadow-gates/<sha>/<bundle>/runs/<id>/`
   as `gate.json` plus single-line `PASSED.sha256`, validated by
   `bybit-options-shadow-gate-policy.jq`.
3. **Cut over** with `host-bybit-options-cutover.sh <candidate-sha256>`. The
   cutover revalidates the release identity, the deployment bundle digest, and
   exactly one immutable production-eligible `PASSED.sha256`, then stops the
   previous release (or starts green-field), drains the canonical spool with the
   candidate uploader, renders and installs the candidate units, clears stale
   health, starts the collector, and requires fresh full-catalog health before
   enabling the unit and the upload timer. Failure after the transition starts
   restores the previous release (or disables and runtime-masks the lane) and
   writes `cutover.json` evidence under
   `/data/monday/evidence/bybit-options-cutovers/`.

The lane is fail-closed: the collector and uploader stop writing when the spool
mount drops below `MIN_FREE_GB` or pending raw bytes reach
`BYBIT_OPTIONS_SPOOL_MAX_BYTES`, and no release can be promoted without a
full-duration shadow gate and a verified deployment bundle. Do not run the
unmanaged legacy binary, do not start `bybit-options-archiver.service` before
its gate, and never delete a spool by hand.

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

### Oversized all-market segments: slice then materialize

Production `spot_all`/`usdm_all` hour segments can decompress past the 2 GiB
market-tape seal bound, which keeps them out of
`binance-replay-parquet-materializer`. The `binance-market-tape-slicer` binary
(hft-collector) rewrites one digest-verified segment into disjoint
symbol-subset segments: session rows are rewritten to the subset scope,
per-slice manifests and digests are recomputed, and every slice is re-sealed
and re-verified under the unchanged strict market-tape gate while it is still
staged — only a verified slice is published, in data -> manifest -> _SUCCESS
order. The 2 GiB bound is a deliberate resource limit and is not raised.
`deployment/aliyun/binance-lob-slice-materialize.sh` is the batch driver: it
recursively enumerates `date=/hour=` partitions under the governed lake
prefix, downloads each segment triplet, slices it for a requested symbol set,
and materializes every slice with the unchanged materializer into
content-addressed canonical parquet plus a `slice-materialization-run.json`
evidence manifest. State under `WORK_DIR/state/` makes reruns resumable
(completed segment/symbol pairs are skipped; a changed symbol set re-slices),
and any download, slice, or materialize failure, any symbol left pending, an
empty enumeration, or a run-manifest publish failure fails the run.

## Rust-only collector release workflow

The Binance collector deployment lane is Rust-only. The legacy Python collector,
its systemd unit, and its deployment tests are removed. A release now has three
separate operations:

1. install a digest-pinned candidate without touching production;
2. validate sealed-triplet evidence, then run the candidate-specific correctness
   Shadow (`--correctness`, fixed 300 seconds after the 900-second bootstrap);
3. only after correctness passes, run the default 1,800-second stability Shadow;
4. cut over only by consuming the formal Gate's immutable evidence.

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

For a script/policy-only change that does not rebuild the binary, run the
installer with `BUNDLE_ONLY=1`. It keeps the artifact identity and verifies the
installed binary SHA, archives the prior `release.json` as
`release.json.prev.<sha256>`, and atomically replaces `deployment/` plus the
`deployment_bundle_*` fields and `deployment_source_revision` of `release.json`.
The binary is never replaced in this mode.

The committed shadow environments use `SYMBOLS=ALL`, five-minute segments, the
isolated spools below, and isolated OSS datasets:

| Market | Shadow spool | Shadow dataset |
| --- | --- | --- |
| Spot | `/data/monday/spool/binance-lob-rust-shadow/spot` | `spot_all_rust_shadow` |
| USD-M | `/data/monday/spool/binance-lob-rust-shadow/usdm` | `usdm_perpetual_all_rust_shadow` |

The sealed-triplet preflight binds candidate source/bundle/build identity and
independently verifies one or more latest Spot and USD-M data/manifest/`_SUCCESS`
triplets with the strict continuity verifiers. It does not claim to replay raw
frames; the preflight is a candidate/format check, while cross-segment
continuity and the required two new post-observation triplets per market remain
correctness-mode evidence:
the merged exact-frame parser E2E remains the parser evidence. The controller
must supply the reviewed corpus receipt's expected replay identity as the
preflight's third argument; the preflight never generates that trust anchor
from the corpus it is about to verify. Correctness mode uses only its run-scoped
`SEGMENT_SECONDS=90` override (two complete 90-second post-bootstrap segments
fit inside the fixed 300-second observation); committed environments remain at
600 seconds and stability/Gate behavior is unchanged.

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

### 2. Run the 15-minute full-catalog gate

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
digests, and then uses monotonic time to observe at least 900 seconds. It fails unless all of
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

### 4. Restore a stopped, already-gated production release

If an already-gated, already-cutover release was later stopped and disabled (for
example, during a disk-full incident), `ACTION=cutover` cannot bring it back:
`host-rust-lob-cutover.sh` refuses an active==2/enabled==2 host with an
"ambiguous production state" error and there is no governed restore path.
`host-rust-lob-restore.sh` closes that gap. It is fail-closed: it never rewrites
the production symlink and never touches the digest-addressed release or its
deployment assets, so the restored runtime is byte-identical to the cutover
artifact.

Invoke it with the immutable artifact digest that is already on disk and already
gated:

```bash
set -euo pipefail
ACTION=restore \
INSTANCE_ID=i-REPLACE \
ARTIFACT_SHA256=REPLACE_WITH_64_HEX_DIGEST \
./deployment/aliyun/invoke-rust-lob-operation.sh
```

Before starting anything the host restore requires all of the following:

1. `sha256($PRODUCTION_LINK)` matches `ARTIFACT_SHA256` and the link resolves to
   `$RELEASE_ROOT/<sha256>/binance-lob-archiver`.
2. Exactly one immutable passed shadow gate exists for that
   `<sha256>/<deployment_bundle_sha256>` and it still satisfies the gate policy.
3. No production unit is active (a running restore is refused, never preempted).
4. The production symlink exists (a missing symlink is refused, never recreated).
5. The canonical spool path is a direct directory tree under `/data` (no symlink
   escapes) and the spot/usdm subdirectories exist.
6. The canonical spool contains no segment artifacts, unless the operator forces
   with `MONDAY_ALLOW_RESTORE_WITH_PENDING=1`.
7. The installed production unit/env files match the gated deployment bundle
   `cmp`-for-`cmp` and the production unit still declares `RuntimeMaxSec=21600`.

The restore then clears stale health, starts the production units while disabled,
waits for fresh full-catalog health written after the restart with a new session
and zero restarts, verifies each `/proc/<pid>/exe` still resolves to the
candidate release, enables production for reboot, and re-verifies health. A
unique recovery evidence directory is created under
`/data/monday/evidence/recoveries/<ts>-<sha:0:12>-<pid>/` and holds the previous
and post-restart health snapshots, a copy of the gated `gate.json` +
`PASSED.sha256`, and immutable `recovery.json` + `verification.json`.

If the restored units never reach verified health, the host restore performs a
fail-closed rollback: it disables and stops production, applies the runtime
transition mask to the production, upload, and legacy units, verifies the host
is fail-closed, preserves rollback health evidence, and records
`recovery.json` with `result: failed` and `rollback_result`. A failed restore
never leaves production active, enabled, or unmasked.

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
