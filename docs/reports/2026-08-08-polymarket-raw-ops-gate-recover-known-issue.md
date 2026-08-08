---
name: polymarket-raw-ops-gate-recover-known-issue
description: Known issue — polymarket raw-ops gate recover admission conflates systemd bookkeeping state with containment, refusing failed-state units with no governed remediation, with follow-up fix proposal
created: 2026-08-08T01:18:36Z
updated: 2026-08-08T01:18:36Z
status: open
---

# Known Issue: Polymarket Raw-Ops Gate `recover` Refuses Failed-State Contained Units

**Follow-up issue:** https://github.com/proerror77/monday/issues/748
(`bug`, `needs-triage`)

## Summary

The `recover` action of the Polymarket raw-ops gate control plane
(`deployment/aliyun/polymarket-raw-ops-gate-control.sh:727-738`, added by #637 /
PR #644, retargeted to the healthy Gamma closed-200 probe by PR #717) admits a
recovery gate only when the contained baseline collector and all four
uploader units/timers report systemd `ActiveState` exactly `inactive`
(`polymarket-raw-ops-gate-control.sh:676-679` and `:717-725`). A unit in
`failed` state — no managed process, restart budget exhausted or `Restart=no`,
i.e. fully contained — is refused with `recovery requires the direct bootstrap
baseline to be stopped` or `recovery requires inactive uploader/timer`.

`failed` is the state the production units are most likely to occupy in
exactly the conditions `recover` was built for: the action's own contract
(#637) is "when, and only when, the direct Rust bootstrap reference collector
has been contained after the known closed-lane Gamma tagged-500 failure", and
the 2026-08-05/06 disk-full incident
(`docs/reviews/2026-08-07-incident-remediation-diff-review.md`) left ample
opportunity for crash-loop exhaustion and failed oneshot uploaders (the
pre-#5 `NoSuchKey` upload failures). The control plane offers no governed way
forward from that refusal: the only remediation is an unrecorded manual
`systemctl reset-failed`/`systemctl stop` on production units outside the
control lock — or, worse, manual stop/start experimentation on the production
collector to reach `inactive`, an ungoverned runtime transition with no health
verification during an incident.

The defect is in the admission predicate, not in the safety posture: the gate
remains fail-closed throughout, so there is no false-promotion risk. See
[What is sound](#what-is-sound-verified) and
[Why this is a defect](#why-this-is-a-defect-not-a-missing-enhancement).

## Evidence

### Primary defect: `ActiveState == "inactive"` is used as the containment test

- Baseline admission (`polymarket-raw-ops-gate-control.sh:676-682`):
  `recovery_baseline` dies unless `ActiveState` is exactly `inactive`, then
  separately requires `MainPID == 0`. The containment fact is already
  established by `MainPID == 0` plus the exact identity binding that follows
  (fragment `:683-686`, drop-ins `:687-689`, effective ExecStart `:690-693`,
  restart counter and invocation ID `:694-700`, direct secure binary and
  digest `:701-707`). The `inactive` string adds no safety — it selects a
  systemd bookkeeping state, not a runtime property. A `failed` unit has no
  managed process and systemd will not start it on its own (the restart
  budget is exhausted or `Restart=no`), which is why `systemctl is-active
  --quiet` exits non-zero for it.
- Uploader admission (`polymarket-raw-ops-gate-control.sh:717-725`):
  `verify_recovery_uploaders_stopped` applies the same `inactive`-only test to
  `polymarket-reference-upload.service`/`.timer` and
  `polymarket-market-tape-upload.service`/`.timer`. A failed oneshot uploader
  — the common post-incident residue — is contained but refused.
- The downstream machinery binds the *recorded* snapshot, not the `inactive`
  constant: `verify_contained_recovery_baseline` in the gate
  (`deployment/aliyun/polymarket-raw-ops-shadow-gate.sh:524-551`) compares the
  live `ActiveState` to the snapshot value (`:529-531`), and the cutover's
  `verify_contained_bootstrap_recovery`
  (`deployment/aliyun/polymarket-raw-ops-cutover.sh:955-997`, live comparison
  at `:976-979`, invoked at `:1689` and again at `:1849`) does the same. The
  literal `"inactive"` appears only in binding predicates — gate
  `verify_recovery_binding` (`polymarket-raw-ops-shadow-gate.sh:501`), gate
  policy `contained_bootstrap_recovery`
  (`deployment/aliyun/polymarket-shadow-gate-policy.jq:80`), and cutover
  (`polymarket-raw-ops-cutover.sh:964`) — so the recovery chain is
  mechanically compatible with any recorded quiescent state; the refusal is an
  admission-time artifact.
- The harness pins the intended semantics
  (`deployment/aliyun/test-polymarket-raw-ops-control-plane.sh:434-488`): an
  active baseline, active uploader, wrong ExecStart, stale/missing/wrong
  candidate probe are all rejected, and recovery must never
  start/stop/restart/enable/disable the contained baseline (`:481-488`). It
  never exercises a `failed`-state baseline or uploader, so the refusal of the
  primary post-failure state is currently untested in either direction.

### Secondary gap A: admission preconditions run outside the control lock and leave no failure evidence

`recover_gate` (`polymarket-raw-ops-gate-control.sh:727-738`) evaluates the
probe, baseline, and uploader preconditions *before* `start_gate` acquires
`CONTROL_LOCK` (`:764-765`). The window is closed downstream — the gate
re-verifies admission freshness and baseline identity at start
(`polymarket-raw-ops-shadow-gate.sh:1433-1438`) and repeatedly through the
gate (`:1663`, `:1693`, `:1871`, `:1956`, `:2111`), and the cutover re-verifies
twice — so no unsafe promotion can result. But a refused or interleaved
admission produces nothing durable: `die` writes only to stderr. During
incident response — the only time `recover` runs — there is no immutable
record of admission attempts, refusals, or their reasons. The governed-restore
precedent takes the host-wide locks *first*
(`deployment/aliyun/host-rust-lob-restore.sh:517-526`) and writes
`recovery.json` with `result=failed`, the failing step, and the reason on
every failure path (`host-rust-lob-restore.sh:202-241`, `:306-322`).

### Secondary gap B: baseline identity is bound from systemctl-loaded values only

`recovery_baseline` reads `FragmentPath`/`ExecStart`/counters via
`systemctl show`, i.e. the unit as loaded at the last `daemon-reload`; the
on-disk fragment bytes are never compared (contrast the restore bar, which
`cmp`s every installed unit/env asset against the gated bundle,
`host-rust-lob-restore.sh:444-456`). A unit file edited without a
`daemon-reload` would be bound into the recovery evidence in its stale loaded
form. This is a residual risk, not a live hole: promotion is neutralized
because the cutover installs its own unit assets atomically rather than
inheriting the stale file, and any `daemon-reload` before gate start surfaces
the drift at the in-gate identity check.

## What is sound (verified)

The recover path's fail-closed core is intact; this document does not allege a
safety defect:

- Probe handling: canonicalization and containment under the exact
  per-candidate evidence root (`polymarket-raw-ops-gate-control.sh:647-650`),
  schema-exact binding to candidate/source with the bounded Gamma closed-200
  contract (`:652-662`), and a 900-second freshness budget enforced both at
  admission (`:663-668`) and again at gate start
  (`polymarket-raw-ops-shadow-gate.sh:514-522`, called at `:1433-1435`).
- Baseline binding: direct, root-owned, non-symlink, executable binary with
  digest distinct from the candidate
  (`polymarket-raw-ops-gate-control.sh:701-707`); candidate binary digest
  re-verified in `start_gate` (`:747-750`).
- In-gate containment: uploader inactivity re-checked at every baseline
  identity verification (`polymarket-raw-ops-shadow-gate.sh:547-550`), which
  runs at gate start and repeatedly through the gate (`:1437`, `:1663`,
  `:1693`, `:1871`, `:1956`, `:2111`).
- Evidence handling: per-invocation immutable receipts and pass markers
  serialized through `commit.lock` with staged/committed states
  (`polymarket-raw-ops-gate-control.sh:405-485`); a passed recovery gate
  embeds the recovery evidence in `gate.json`, which the policy pins via
  `recovery_matches_gate` (`polymarket-shadow-gate-policy.jq:92-95`, `:294`).
- Cutover: contained-recovery promotion re-verifies the baseline twice and
  rolls back transactionally to the recorded stopped state
  (`polymarket-raw-ops-cutover.sh:1128-1136`, `:1279-1299`).

## Impact

- **Operational:** after a crash-loop or failed-oneshot containment — the
  modal post-incident state — `recover` cannot admit a gate. The only paths
  forward are ungoverned: manual `systemctl reset-failed`/`stop` on production
  units outside `CONTROL_LOCK` with no evidence artifact, or manual stop/start
  of the production collector to force `inactive`, an unrecorded production
  runtime transition with no health verification, taken under incident
  pressure. These are exactly the untracked host mutations the control plane
  exists to eliminate and that the 2026-08-07 monitoring remediation (#7, PR
  #735) now watches for.
- **Safety:** none. Every refusal is fail-closed; the defect blocks admission,
  it cannot weaken a gate, skip a probe check, or promote a candidate.
- **Evidence:** refused admissions are invisible after the fact (stderr only),
  so incident reviews cannot reconstruct recovery-gate admission history.

## Relation to the governed-restore precedent

`deployment/aliyun/host-rust-lob-restore.sh` (merged 2026-08-07, PR #734) is
the bar for this class of recovery action, and the recover path currently
falls short of it in three specific ways:

1. **Quiescence test.** Restore treats any not-active unit as quiescent via
   `systemctl is-active --quiet` (`host-rust-lob-restore.sh:425-428`), which
   accepts `failed`; recover pins the literal string `inactive`.
2. **Governed remediation.** Restore performs `systemctl reset-failed` as a
   named STEP inside the governed flow (`host-rust-lob-restore.sh:472`) before
   starting; recover has no remediation step at all.
3. **Serialization and evidence.** Restore holds the host release lock and the
   shadow-gate lock before any preflight (`host-rust-lob-restore.sh:517-526`)
   and writes immutable evidence on success *and* failure
   (`host-rust-lob-restore.sh:202-241`, `:306-322`); recover checks
   preconditions before its lock and records nothing on refusal.

## Why this is a defect, not a missing enhancement

`recover` already has one job at admission: prove the baseline is contained
and exactly identified. The containment fact it needs is "no managed process
plus exact identity", and it already verifies that (`MainPID == 0`, fragment,
drop-ins, ExecStart, binary digest). The `ActiveState == "inactive"` test is
therefore not a missing layer of rigor — it is the wrong predicate, selecting
a bookkeeping state that the modal post-failure scenario does not produce,
while every downstream consumer merely requires that the recorded state match
the live state. An action that cannot be lawfully invoked in the conditions
its own contract (#637) scopes it to, and whose refusal pushes operators
toward ungoverned production mutations, is broken behavior with a focused
reproduction, not a feature request.

## Proposed follow-up fix direction

One PR, one behavior, scoped to the recover admission path:

1. **Admit containment, not bookkeeping state.** Accept `inactive` or `failed`
   for the baseline collector and the four uploader units/timers, keeping
   `MainPID == 0` and every exact-identity check unchanged.
2. **Governed reset, restore-style.** For units observed `failed`, run
   `systemctl reset-failed` as a governed step, then re-read every snapshot
   field (state, MainPID, fragment, drop-ins, ExecStart, restarts, invocation,
   binary digest) so the recorded baseline snapshot reflects the post-reset
   state (`inactive`). The downstream binding predicates
   (`polymarket-raw-ops-shadow-gate.sh:501`,
   `polymarket-shadow-gate-policy.jq:80`,
   `polymarket-raw-ops-cutover.sh:964`) then keep their exact `"inactive"`
   contract unchanged — preferred over widening four binding sites to accept
   `"failed"`, which would also stay racy against any manual reset between
   admission and gate start.
3. **Serialize admission.** Acquire `CONTROL_LOCK` at the top of
   `recover_gate` so precondition reads, the governed reset, and the gate
   start are one critical section (mirroring
   `host-rust-lob-restore.sh:517-526`).
4. **Durable admission evidence.** Write an immutable admission record
   (accepted/refused, exact candidate/baseline/probe identities, refusal
   reason) under the gate evidence root on every `recover` invocation,
   mirroring `recovery.json` (`host-rust-lob-restore.sh:202-241`).
5. **Optional, stacked:** compare the on-disk baseline unit fragment bytes
   against the expected installed asset (or `daemon-reload` and re-read)
   before binding, closing secondary gap B.

Fail-closed semantics must not move: active/activating/deactivating units,
nonzero MainPID, any identity drift, active uploader/timer, and stale,
missing, or mis-bound probes must keep refusing.

## Acceptance criteria for the follow-up PR

- New harness cases in
  `deployment/aliyun/test-polymarket-raw-ops-control-plane.sh`: a `failed`
  baseline and each `failed` uploader unit are admitted after a governed
  `reset-failed`; the recorded baseline snapshot reads `inactive` with
  `MainPID == 0`; the gate binding, gate policy, and cutover recovery binding
  pass unchanged end-to-end on the existing recovery evidence path.
- Regression: `active`, `activating`, and `deactivating` baselines, active
  uploaders/timers, and every existing refusal case (probe stale/missing/wrong
  candidate, ExecStart/fragment/drop-in/binary drift, active gate for the same
  candidate) still refuse; the existing assertion that recovery never
  start/stop/restart/enable/disables the contained baseline
  (`test-polymarket-raw-ops-control-plane.sh:481-488`) is preserved and
  extended to prove `reset-failed` happens only inside the control lock.
- Admission evidence: both an admitted and a refused `recover` invocation
  write an immutable record with exact identities and reason, asserted in the
  harness.
- `deployment/aliyun/test-polymarket-raw-ops-control-plane.sh` passes in full;
  `shellcheck` and `bash -n` clean on touched scripts; `git diff --check`
  clean.
- Boundaries: no changes to the parity validator (owned by issue #747 and its
  own write-up); no weakening of probe freshness, identity binding, or
  `MainPID == 0` containment; no cloud or host mutation — runtime application
  of any fix remains a separately authorized cutover.

## Out of scope

The parity validator defect and its write-up are owned by a separate
follow-up (issue #747,
`docs/reports/2026-08-08-polymarket-parity-high-rate-tapes-known-issue.md`).
Production runtime mutation is not part of this document; no cloud or host
state was changed while preparing it.
