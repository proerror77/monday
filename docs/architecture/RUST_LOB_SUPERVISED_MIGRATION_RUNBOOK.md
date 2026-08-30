# Rust LOB supervised migration checklist

**Status:** operator checklist only. It does not authorize production change or
deletion of historical evidence.

## Frozen identities

Freeze identities in the execution record immediately before the run; never
write one run's changing SHA values into this durable checklist.

| Role | Source of truth |
| --- | --- |
| Current controller `C0` | exact active-controller readback |
| Candidate controller `C1` | staged immutable `release.json` digest |
| Current payload/runtime `P0/R0` | active release plus live installed-byte readback |
| Candidate payload/runtime `P1/R1` | exact fields in `C1/release.json` |
| Candidate source/control bytes | exact fields and `deployment.sha256` in `C1` |

Transition only as the complete pair `C0/P0/R0 -> C1/P1/R1`. Keep old
receipts and rollback artifacts read-only until step 6 passes. `C1` remains
staged-only until a new supervised observation record is read back. Any prior
or failed Gate receipt is evidence only and never authorizes a cutover; any
identity movement after freezing is a stop condition.

## Six steps

### 1. Freeze

- Stop writers to Gate/Cutover/Restore/Readback; preserve the frozen diff and
  failed receipts.
- Read back active C/P/R/source plus both production PID, executable digest,
  `NRestarts`, `InvocationID`, cgroup and slice.
- **Stop:** any mismatch or non-running lane. Safe state is the frozen active
  `C0/P0/R0` with `C1` inactive.

### 2. Preflight resources

- Read real-host `MemAvailable`, production aggregate anonymous memory and the
  Shadow phase maximum.
- Calculate `required = 1 GiB reserve + phase_max + (candidate MemoryMax - production_anon)`.
- Require `production_anon <= 3.5 GiB`, `MemAvailable >= required`, no OOM, and
  no active recovery/compression/upload worker. The only timers that may be
  paused are this exact six-unit cold allowlist:
  `binance-usdm-reference-upload.timer`, `bybit-options-upload.timer`,
  `polymarket-reference-upload.timer`, `polymarket-market-tape-upload.timer`,
  `polymarket-market-tape-upload-watchdog.timer`, and
  `binance-lob-archiver-recovery@spot.timer`.
- Record each allowlisted timer's `LoadState`, `ActiveState`, `SubState`,
  `UnitFileState`, and `Result` before pausing it; record its paired oneshot
  service as well. Later restore `ActiveState` and `UnitFileState` independently
  to their exact before-state, and require the paired oneshot service to return
  to its recorded quiescent state. Never pause a
  Collector (`binance-lob-archiver-production@spot.service`,
  `binance-lob-archiver-production@usdm.service`,
  `binance-usdm-reference-collector.service`,
  `bybit-options-archiver.service`, `polymarket-market-tape.service`, or
  `polymarket-reference-collector.service`), `monday-collector-health.timer`,
  any fee timer (`binance-fee-snapshot-spot.timer`,
  `binance-fee-snapshot-usdm.timer`, or `binance-fee-upload.timer`), or the
  elapsed `binance-lob-archiver-recovery@usdm.timer` (or its service).
- **Stop:** any failed/unreadable input. Do not change slices or start Shadow.

### 3. Verify the current envelope

- Read and record the current production values without changing them. A stable
  V2 source must already match its signed `MemoryHigh=3 GiB` and
  `MemoryMax=3.5 GiB`; the one direct v1-to-V2 bootstrap may instead record an
  unlimited legacy aggregate. Resource admission still reserves growth to the
  signed candidate 3.5 GiB limit.
- Gate may create only its run-scoped Shadow `MemoryMax=1.5 GiB` slice. The
  signed production slice remains inactive until Cutover.
- Obtain cgroup paths only from `systemctl show ... ControlGroup`; read back the
  parent, both production children and the empty Shadow slice. The exact
  production paths (literal `\x2d` spelling) are:
  - slice unit `system-binance\x2dlob\x2darchiver\x2dproduction.slice`,
    `ControlGroup=/system.slice/system-binance\x2dlob\x2darchiver\x2dproduction.slice`,
    filesystem path `/sys/fs/cgroup/system.slice/system-binance\x2dlob\x2darchiver\x2dproduction.slice`;
  - Spot child `/sys/fs/cgroup/system.slice/system-binance\x2dlob\x2darchiver\x2dproduction.slice/binance-lob-archiver-production@spot.service`;
  - USD-M child `/sys/fs/cgroup/system.slice/system-binance\x2dlob\x2darchiver\x2dproduction.slice/binance-lob-archiver-production@usdm.service`.
- The run-scoped Shadow aggregate must be named
  `mondayrustlobgate<digits>.slice` (digits-only run suffix), with
  `ControlGroup=/mondayrustlobgate<digits>.slice` and filesystem path
  `/sys/fs/cgroup/mondayrustlobgate<digits>.slice`. Read the value back from
  `systemctl show` rather than constructing it from an assumed hierarchy, and
  require its `cgroup.procs` to contain no PID before startup.
- **Stop:** any topology, identity, child-limit, OOM, or stable-envelope
  mismatch. Remove the empty Shadow slice, verify `C0/P0/R0`, then stop.

### 4. Observe Shadow

- Pause only named cold timers. Never stop a live Collector or segment writer.
- Start `C1/P1/R1` without changing `controller/active`; verify PID, digests and
  actual cgroup.
- Require Spot/USD-M synced health, stable session/PID/restarts, zero current
  and cumulative gaps, complete sealed segments, and OSS
  `data/manifest/_SUCCESS` triplets. Host pressure is evidence; candidate-slice
  limits and production data health are authoritative.
- Before entering step 5, run the Gate shipped by exact `C1`; the operator may
  only read back its create-once receipt and must not write or edit it. Read back
  the controller directory
  `/opt/monday/releases/binance-lob-controller/<C1>/`
  (`sha256sum release.json` must equal the frozen `C1`), and
  Gate receipt
  `/data/monday/evidence/shadow-gates/<C1>/<R1>/runs/<run_id>/gate.json`
  plus its create-once `PASSED.sha256`. Require `passed=true`, verify the
  receipt bytes against `PASSED.sha256`, and validate the exact receipt with
  `monday_validate_v2_gate`. Verify the release with
  `monday_verify_controller_release` so `release.json.sha256` and
  `deployment.sha256` are both covered. The receipt must bind
  the frozen `C1/P1/R1` and source/control-byte identities. An old, edited, or failed
  receipt cannot satisfy this requirement. If exact `C1` cannot produce this
  PASSED receipt, stop, publish a new controller, and re-freeze this table.
- **Stop:** any restart, drift, gap, stale health, limit breach or missing
  triplet. Stop Shadow, restore timers, verify production unchanged.

### 5. Cut over

- A named operator confirms step 4, takes the cutover lock, stops the frozen
  production pair, and atomically switches the complete pair to `C1/P1/R1`.
- Install the signed production assets from `C1`, clear legacy runtime
  overrides, reload systemd, and verify the 3 GiB/3.5 GiB slice before starting
  either candidate lane.
- Start both lanes; read back PID, executable digest, `NRestarts`,
  `InvocationID`, cgroup, slice and active links.
- **Stop:** any mismatch. Restore complete `C0/P0/R0`; never half-repair.

### 6. Read back and close

- Independently verify active `C1/P1/R1`, both processes, production
  `3 GiB/3.5 GiB` limits, health continuity, real 300-second production
  segments, and post-cutover OSS triplets.
- Restore each of the six allowlisted cold timers to its independently recorded
  before-state: active versus inactive and enabled versus disabled. Read back
  both states and the paired oneshot's quiescent state. Record
  Code, CI, Merge, Release, Runtime, Cutover and Readback separately.
- **Stop:** incomplete readback means incomplete migration; preserve rollback
  and evidence.

After step 6, and only after a separate validator-parity PR has passed, perform
an independent cleanup review. Remove only fixtures or legacy paths proven
obsolete and unreachable; retain old receipts, manifests, rollback artifacts,
and the historical evidence chain until migration completion and retention
approval. Do not delete old evidence as part of this migration, and do not mix
cleanup, validator convergence, or policy changes in the migration change.
