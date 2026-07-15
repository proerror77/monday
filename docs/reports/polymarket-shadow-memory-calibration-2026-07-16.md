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

## Decision

The reviewed service envelope is `MemoryHigh=512M` and `MemoryMax=768M`. On this
8 GB host, simultaneous production and shadow hard limits total 1.5 GiB. The formal
one-hour-plus-tail gate must still prove zero restarts, current health, 112/112 polls,
zero priority backlog, no growing `memory.events high`, and no OOM before promotion.

The Rust collector also uses an independent 180-second OS-thread watchdog so
non-yielding fsync or atomic state publication cannot evade the cooperative Tokio
timeout. The health policy remains capped at 180 seconds; no acceptance threshold was
relaxed.
