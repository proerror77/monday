# Spot LOB recovery-queue drain failure and the production restart feedback loop

- Evidence for issue: #1241
- Host: Tokyo ECS `i-6we6afeqsvv8uo1ixmyo` (`ap-northeast-1`), role `market-data-collector`
- Observation window: 2026-09-28 23:14 CST – 2026-09-29 07:10 CST
- Collection mode: read-only host inspection plus two recorded runtime mutations (below). No code was changed.

## Summary

The spot channel dies on a market-data stall, systemd restarts it, and each restart asks the recovery queue to drain.
That drain cannot complete: it exits on an application assertion and leaves a `.failed` job behind. The spot queue has
therefore grown monotonically to 23 failed jobs with no `.ready` or `.running` work. Stopping the recovery timer removes
the independent 15-minute cadence but **does not** stop the growth, because the `isolate` handoff inside every production
restart starts a drain by itself.

## The loop

1. `binance-lob-archiver-production@spot.service` exits `status=75/TEMPFAIL`:
   `process watchdog exiting after market-data stall silent_ms=180876..199534 queue_saturated=false`.
2. systemd restarts the unit (`Restart=always`), which runs `ExecStartPre` `monday-rust-lob-recovery-queue isolate spot`.
3. `isolate` completes and its `phase=handoff` executes
   `systemctl start --no-block binance-lob-archiver-recovery@spot.service`
   (`host-rust-lob-recovery-queue.sh:670`; `queue_unit="$RECOVERY_SERVICE@$MARKET.service"` at lines 607 and 719 — verified).
4. The drain takes a job and dies on an application assertion; the job becomes `.failed`.

Observed three times: 23:18:54, 00:26:07, 01:21:02 CST. Each produced exactly one new failed job.

The handoff is conditional: `isolate_market` exits 0 without starting recovery when
`needs_recovery_isolation` is false (`host-rust-lob-recovery-queue.sh:705`; the predicate is
`has_incomplete_parts || has_undrained_complete_segments`). Those branches are not taken here.

### Correcting two claims that were initially recorded

- `rust-lob-control-plane-lib.sh:338` is **not** the start call; those lines are the body of
  `monday_rust_lob_recovery_service_units()`. The authoritative start is
  `host-rust-lob-recovery-queue.sh:670`. (A second call site exists at line 1173, inside `resume_market`.)
- `production@spot.service` has **no** systemd dependency on the recovery unit: `Wants`, `Requires`, `After`, `Before`,
  `BindsTo`, `PartOf` and `TriggeredBy` contain no recovery reference. The only linkage is the `ExecStartPre` command.
- The recovery timer is **not** the source of the 23:29:15 and later starts. `LastTriggerUSec` stayed frozen at
  `2026-09-28 23:22:10`, and the timer journal has no entries after that. At 23:22:10 the unit was already active
  (23:09:21–23:25:41), so that firing did not start a new run.

## The two application assertions

Journal identifier `monday-rust-lob-recovery-queue`, unit `binance-lob-archiver-recovery@spot.service`:

```
Error: --recover-parts-only did not recover every non-empty input part
Error: recovery parts contain no complete stream-coverage catalog
```

Each is followed by `detached recovery of incomplete parts failed`, then
`Main process exited, code=exited, status=1/FAILURE`, then `Failed with result 'exit-code'`.

Adjacent strings present in the same binary but not emitted in this window:

```
--recover-parts-only refuses residual .part.corrupt artifacts
recovery stream coverage does not prove the complete catalog
```

Source path embedded in the binary: `tools/collector/src/bin/binance-lob-archiver.rs`.

A no-op run — no ready job — finishes in about 4 seconds with exit 0 (observed at 22:51:05 and 23:06:34).
A historically green `recovery@spot.service` therefore does not demonstrate that recovery works.

## Where the evidence lives, and its retention limit

- The Rust `Error:` lines exist **only in the systemd journal**.
- `<id>.failed` directories contain no `*error*`, no `*log*`, no `result.json`.
- `evidence/recoveries/lob-queue/<id>/result.json` exists but carries only
  `result='failed'`, `step='recover-upload'`, `message='drain failed'`, `market='spot'`.
  The Rust text is absent.
- `recovery-input/receipt.json` is the input inventory, not an error log.

Journal retention is about 3.5 days (`journalctl --disk-usage` = 3.9 G; the recovery unit's journal begins
2026-09-25T23:42:17). Of the 23 failed jobs, only 4 still have a retrievable reason.

## Queue state

`find /data/monday/spool/binance-lob-recovery/spot -maxdepth 1 -name '*.failed' | wc -l` = **23**;
`.running` = 0; `.ready` = 0; `failed` is the only suffix present.

Growth during the window: 20 (baseline 23:24) → 21 (23:29) → 22 (23:37) → 23 (00:26) → unchanged to 07:08.

### Job inputs for the three jobs from this window

| completed CST | job | inputs present in spool | journal error |
|---|---|---|---|
| 23:25:41 | `20260928T150730Z-spot-db78a31857cb-3322263` | 3 × `.jsonl.zst` (137703062, 69311366, 138447032) + manifests; 1 × `.jsonl.part.corrupt` (**940310528**) | did not recover every non-empty input part |
| 23:30:57 | `20260928T152906Z-spot-db78a31857cb-3338789` | `.jsonl.part` 725517634; `.jsonl.part` 67221295; `.jsonl.zst.tmp` 7029859; `.jsonl.part` **0 bytes** | no complete stream-coverage catalog |
| 00:26 (failed 00:30:00) | `20260928T162614Z-spot-db78a31857cb-3375726` | not enumerated | no complete stream-coverage catalog |

The second job ran 3 min 21.5 s and consumed 44.268 s CPU; the third, 10 min 8 s and 2 min 14 s CPU (that one ended
`Deactivated successfully` with `upload-only: uploaded=2 pending=0` while still logging two `ossutil cp` failures).

Across the whole queue there are 8 `*.part.corrupt` files, 514 MB – 1095 MB, dated 09-08 … 09-28.
`segment_artifacts()` in `host-rust-lob-recovery-queue.sh` (from line 275) counts `*.part.corrupt` as a segment artifact.

## Live mutations performed (recorded for reconstructability)

1. `2026-09-28T23:29:15 CST` — `systemctl reset-failed binance-lob-archiver-production@spot.service`, then `start`.
   Preconditions checked: no drain process held the queue lock; the lock file was released. Result: `active`,
   `NRestarts=0`, `ExecMainStatus=0`. No rollback required.
2. `2026-09-28T23:32:38 CST` — `systemctl stop` and `systemctl disable binance-lob-archiver-recovery@spot.timer`.
   Current: `is-active=inactive`, `is-enabled=enabled`, `NextElapseUSecMonotonic=infinity`.
   Rollback: `systemctl enable --now binance-lob-archiver-recovery@spot.timer`.

Verified effect of mutation 2: the drains at 00:26:39 and 01:21:51 occurred with the timer inactive and were started by
`isolate` handoffs, in the same second as the corresponding production starts. The timer's own cadence was removed;
queue growth was not.

`/opt/monday/bin/monday-collector-health.sh` now exits **1** with `ok:false`, including
`binance-lob-archiver-recovery@spot.timer: timer not active ... while service ...production@spot.service is active and enabled`.
That breach is the intentional deviation of mutation 2 and clears when the timer is restored.

## Readback at 2026-09-29T07:08 CST

| item | value |
|---|---|
| `production@spot.service` | `active`/`running`, `NRestarts=2`, `Result=success`, `ExecMainStatus=0`, `ActiveEnterTimestamp=2026-09-29 01:21:51`, `Restart=always` |
| `production@usdm.service` | `active`/`running`, `NRestarts=0` — untouched |
| spot freshness | newest `part` age 0 s; `health.json` age 2 s |
| spot upload status | `last_success_at` 2026-09-28T23:11:57Z, `last_error=null`, `failure_count=18` |
| usdm upload status | `last_success_at` 2026-09-28T23:08:50Z, `last_error=null`, `failure_count=411` |
| start-limit policy | `Restart=always`, `StartLimitBurst=5`, `StartLimitIntervalUSec=2h` |

The 23:18 lockout reached restart counter 9 and `Start request repeated too quickly`.

## Artifact identities

| identity | value |
|---|---|
| archiver binary sha256 | `db78a31857cb038316689f02e90b8d382ea1dd37e14fcbf32c90381ca737eea8` |
| controller sha256 | `0a73962457712d52eeab9dc4008cafb6b5e890f59d2a325ceab77669a81024ad` |
| `deployment_source_revision` (controller `release.json`; also written into `result.json`) | `a149fc658a6d515bbb38eec505eeee1dc184bb5f` |
| `runtime_contract_sha256` | `ca26ddef081aa45262cef7ecf0e8e77bf2145db32121fec9eed1c2db524abea8` |
| `deployment_bundle_sha256` | `8d8d48dda364df59a7167297d83981a1423b852c70f9dad6a5b6efec005cb6d8` |

The job-directory suffix `db78a31857cb` matches the binary sha256 prefix.
The payload `release.json` (`monday.rust_lob_payload_release.v1`) records a different
`deployment_source_revision` = `0e4872eeb59467e1a3b0bfcaf396768cc9ffa18a`; recovery jobs record the controller value.

## Evidence labels

**Verified by direct readback.** The two `Error:` strings; the 23/0/0 queue counts; the job inputs in the table above;
the absence of systemd dependencies; `isolate` reaching `phase=handoff` in the same second as the recovery `Starting`;
`LastTriggerUSec` frozen at 23:22:10 while drains continued; the timer stop at 23:32:38; `Restart=always` with
`StartLimitBurst=5` / `StartLimitIntervalUSec=2h`; the identity table; the `exit 1` self-check; all readback values.

**Inferred.** That a residual `.part.corrupt` (or a 0-byte `.part`) is what makes `--recover-parts-only` conclude
"did not recover every non-empty input part". The binary string, the `segment_artifacts` treatment and the presence of a
940 MB `.part.corrupt` in the first job's spool are consistent with it, but the predicate was not isolated by experiment.
Also inferred: that the recurring `NoSuchKey` verify loop below stems from un-uploaded-part records whose source files were
renamed away by an earlier `isolate`.

**Unknown.** Whether the 23 failed jobs may be replayed and in what order. Why the `NextElapseUSecMonotonic` of the usdm
recovery timer is `infinity` (no experiment was run; not attributed here). Who authored the `reset-failed` mutation —
`reset-failed` appears in no unit, sudo or sshd journal, so the operator attribution comes from the operator, not from the host.

## Related observation, deliberately not closed here

`1823` occurrences of `Error Code: NoSuchKey` / `Http Status Code: 404` between 2026-09-28 19:00 and 07:08, roughly every
5 minutes, on both spot and usdm:

```
ERROR: failed to execute /var/lib/hft-collector/.aliyun/ossutil [cp oss://.../date=2026-09-28/hour=22|23/part-*.jsonl.zst
  /data/monday/spool/binance-lob[-recovery]/.../.oss-verify.<hex>/data ...]: exit status 2
```

Both channels currently have a fresh `last_success_at` and `last_error=null`, so this is not evidence that current
ingestion is failing. Independent confirmation was not possible: `ossutil ls` as `hftcollector` returned
`SigningContext.Credentials is null or empty`, so the bucket contents remain unverified from outside the archiver.
This looks like a distinct defect and warrants its own issue rather than closure under #1241.

## Method note

The host evidence in this report was collected read-only by two independent passes: an orchestrator pass and a separate
`grok-4.7` CLI pass, with each pass then re-running the other's commands. Four claims from the orchestrator pass were
corrected as a result (the self-check exit code, which had been read through a `| head` pipeline that replaced `$?`; the
timer stop time; an over-general "every restart starts a drain" claim; and the `lib.sh:338` citation above). One claim from
the CLI pass was corrected in the other direction: its search window was one second too narrow to find a
`frame_age_ms=Some(189914)` producer diagnostic that is present at 23:18:54. The stall figure used above
(`silent_ms=180876..199534`) is the watchdog's own measure and is the authoritative one.
