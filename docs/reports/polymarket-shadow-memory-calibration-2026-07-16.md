# Polymarket shadow memory calibration (Tokyo)

## Scope

This report records the fail-closed calibration performed on the Tokyo collector ECS
before the Python-to-Rust reference-collector cutover. No production unit was replaced,
and the Python collector retained one PID with zero restarts throughout every probe.

## Pinned release and host

- ECS: `i-6we6afeqsvv8uo1ixmyo` (`monday-trade-data-26`)
- Host memory: 7,751,184,384 bytes total; 5,981,065,216 bytes available during the probe
- Source revision: `4f2c4c11f8082bf933c6a970dc7c09311626aa62`
- Candidate SHA-256: `01e798b186589178c50a3568df2c435a10abb5dae8f4cbfc30aa9ea0611a88ca`
- Production Python PID: `51455`; `NRestarts=0`

## Reclaim failure under the old envelope

Two formal gate invocations failed closed before cutover:

- `t-jpn6r0jipdgm2v4`: 2026-07-15 17:08:02Z to 17:12:05Z
- `t-jpn6r0kyj4lt534`: 2026-07-15 17:24:10Z to 17:28:13Z

Both reported `Rust shadow health is missing` at the 240-second initial-health
settle boundary. The second run used the production shadow unit unchanged:

- `MemoryHigh=402653184` bytes (384 MiB)
- `MemoryMax=536870912` bytes (512 MiB)
- `MemoryCurrent=440455168` bytes
- `MemoryPeak=441233408` bytes
- `memory.events high=5525`; `oom=0`; `oom_kill=0`
- memory pressure `full avg10=83.55`, `full avg60=67.32`

The first failed shadow durably wrote 4,951 records (2,476 metadata and 2,475
settlements), 28,661,316 bytes, before the gate stopped it. This proves that the old
soft limit forced sustained reclaim during cold-start persistence rather than an OOM
or process restart.

## Isolated control probes

The same binary was then run in isolated, non-production diagnostic directories
without the shadow unit's old memory envelope:

- `t-jpn6r0knahigbgg` (recovery/steady-state): 36.621 seconds, 112/112 trade polls,
  zero API errors, zero priority backlog, health policy passed.
- `t-jpn6r0ktuqz7f9c` (new empty spool): 31.425 seconds, 112/112 trade polls,
  zero API errors, zero priority backlog, health policy passed.

These probes isolate cgroup reclaim as the formal-shadow slowdown; they are not
production gate evidence and cannot authorize cutover.

## Follow-up at the 512 MiB watermark

A later formal shadow used `MemoryHigh=536870912` bytes (512 MiB) and
`MemoryMax=805306368` bytes (768 MiB). It reached a measured
`MemoryPeak=538951680` bytes. During steady observation, `memory.events high`
increased from 102 to 112 in 68 seconds while `max=0`, `oom=0`, and memory
pressure averages were zero. The lack of OOM did not make the run eligible:
the growing `high` counter proved that the working set still crossed the soft
watermark. The run was stopped and cannot authorize cutover.

## Decision

The initially reviewed service envelope was `MemoryHigh=576M` and
`MemoryMax=768M`. That July 16 calibration left 65,028,096 bytes of headroom
over the then-observed 538,951,680-byte peak without raising the hard limit.

Two July 17 formal production gates against source revision
`eb3ec638c99e763dd0db06843cefa9294aee56dd` invalidated that earlier soft-limit
assumption without showing an OOM:

- 2026-07-17 16:26:06 to 16:26:37 Asia/Shanghai: `MemoryPeak=586.1M`
- 2026-07-17 16:27:07 to 16:27:38 Asia/Shanghai: `MemoryPeak=605.8M`

Both runs failed because `memory.events high` increased while `MemoryMax=768M`
remained untouched. The updated reviewed envelope is therefore
`MemoryHigh=672M` and `MemoryMax=768M`. The new 672 MiB soft watermark restores
roughly the same safety margin over the observed peak while preserving the
existing hard limit and fail-closed gate semantics. On this 8 GB host,
simultaneous production and shadow hard limits remain 1.5 GiB. This calibration
does not replace promotion evidence: a new formal one-hour-plus-tail gate must
still prove zero restarts, current health, 112/112 polls, zero priority backlog,
and zero high/max/OOM events from its first cgroup sample before promotion.

The Rust collector also uses an independent 180-second OS-thread watchdog so
non-yielding fsync or atomic state publication cannot evade the cooperative Tokio
timeout. The health policy remains capped at 180 seconds; no acceptance threshold was
relaxed.

The production compatibility unit has a six-hour `RuntimeMaxSec` and `Restart=always`.
Consequently, a healthy long-lived legacy process can have a nonzero cumulative
systemd `NRestarts`. The production gate records the PID and restart counter at its
start and rejects any change during shadow or before cutover; it does not erase or
misclassify an earlier scheduled lifecycle refresh. The counter is reset only after
the legacy writer is stopped, so the promoted Rust process is still verified from a
zero restart baseline.

Runtime continuity is bound to the systemd `InvocationID` as well as the frozen PID
and `NRestarts` value. Immediately before stopping either the Rust shadow or the
legacy writer, the control syncs the journal and captures a cursor. It then scans
only post-cursor records, rejects a restart or new invocation, and re-reads the
restart counter after stop. This supplies evidence across the narrow interval that
PID and counter sampling alone cannot cover.

The later production cutover is complete only if `cutover.json` has an adjacent
single-line `PASSED.sha256` that verifies the exact JSON checksum. A JSON file
without that marker is not success evidence. A failed transition or any automatic
or requested rollback invalidates the pair; the remaining artifacts document the
failure or rollback but cannot authorize the Rust collector as current production.
The valid marker is atomically renamed and synced as rollback-pending before any
service mutation. If restoration fails or is interrupted, that pending state remains
fail-closed. A completed automatic recovery renames the marker to
`PASSED.invalid.sha256`; a completed requested rollback uses
`PASSED.rolled-back.sha256`. Both continue to verify the unchanged `cutover.json`,
but neither is the canonical `PASSED.sha256` required for Rust-production authority.
