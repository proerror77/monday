---
name: polymarket-parity-high-rate-tapes-known-issue
description: Known issue — polymarket shadow parity validator cannot validate high-rate tapes (stable-read race on the live baseline spool), with follow-up fix proposal
created: 2026-08-08T01:15:00Z
updated: 2026-08-08T01:15:00Z
status: open
---

# Known Issue: Polymarket Parity Validator Fails on High-Rate Tapes

**Follow-up issue:** https://github.com/proerror77/monday/issues/747
(`bug`, `needs-triage`)

## Summary

The Polymarket shadow parity validator
(`rust_hft/tools/collector/src/polymarket_parity.rs`, invoked as
`polymarket-raw-ops verify-shadow-parity` from
`rust_hft/tools/collector/src/bin/polymarket-raw-ops.rs:319-339`) validates the
Rust shadow collector against the live baseline lane by requiring **every tape
in both spools to remain completely unchanged while it is read twice**. The
shadow gate stops and finalizes only the shadow lane before verification; the
baseline lane keeps collecting into its spool throughout. On high-rate
(high-frequency) market tapes the baseline appends or rotates during a read
pass, the stability check aborts the pass, the fixed retry budget exhausts, and
the verifier bails with `spool changed while reading parity window`. The gate
then fails closed with **no parity evidence at all**: byte, field, dedupe,
settlement, and rotation parity are unvalidated for that window, and candidate
promotion is blocked for operational (not correctness) reasons.

This is a structural defect, not a tuning problem; see
[Why this is structural](#why-this-is-structural).

## Evidence

### Primary defect: all-or-nothing stable-read of a live spool

- `FileFingerprint` (`polymarket_parity.rs:79-98`) captures device, inode,
  byte size, and mtime to nanosecond precision. Any append to a tape — even
  rows far outside the comparison window — changes the fingerprint.
- `stream_stable_rows` (`polymarket_parity.rs:236-289`) compares the
  fingerprint before opening (`:246-247`), after opening (`:250-251`), at end
  of file (`:285-286`), and around every row error (`:272-274`, `:279-281`);
  a missing trailing newline also returns "changed" (`:265-266`). Any change
  yields `Ok(None)`, i.e. "tape moved, start over".
- `load_rows` (`polymarket_parity.rs:345-419`) reads **all** tapes in the
  spool in a first pass (`:358`), re-reads all of them in a second metadata
  pass (`:385`), and re-enumerates the directory between passes (`:371`,
  `:406`). The whole two-pass cycle is retried at most 5 times with 20 ms
  sleeps (`:352`, `:373`, `:407`) and then fails closed
  (`:418`: `bail!("{}: {last_reason}", ...)` with
  `last_reason = "spool changed while reading parity window"`).
- The gate stops, freezes, and finalizes only the **shadow** collector
  (`deployment/aliyun/polymarket-raw-ops-shadow-gate.sh:1887-1916`), then runs
  the verifier against `--legacy-spool "$LEGACY_SPOOL"` (`:1922-1926`) while
  `LEGACY_RUNTIME_STABILITY_REQUIRED=true` (`:25`) requires the baseline unit
  to be active and healthy for the entire gate
  (`verify_runtime_identity`, `:399-417`, checks `systemctl is-active`). The
  baseline spool is therefore a live, continuously appended directory at
  verification time — by design of the gate, not by accident.
- The gate policy requires all parity checks true and non-empty legacy metrics
  in `legacy_overlap` mode
  (`deployment/aliyun/polymarket-shadow-gate-policy.jq:329-337`, `:370-373`),
  so a verifier bail is never masked: it always fails the gate.

Per-attempt failure probability is the probability that at least one append or
rotation lands anywhere in a two-pass read of the whole spool. As tape rate
rises, the mean inter-append interval falls below the pass duration and that
probability approaches 1; 5 retries do not change the asymptote.

### Amplifier A: read cost scales with total spool bytes, not the window

`tape_paths` (`polymarket_parity.rs:162-189`) enumerates every closed segment
plus the active tape, and both passes parse every row of every tape as JSON;
`retain_primary_row` (`:291-343`) discards out-of-window rows only **after**
the full parse. Rows appended after the comparison cutoff are likewise parsed
and held to full schema validity (kind allowlist `:226-229`, gapless sequence
`:205-218`), so a post-cutoff row the running baseline has written but the
verifier's schema predates is a hard error, not a retry. On a host with upload
backlog — exactly the condition of the 2026-08-05/06 disk-full incident
(`docs/reviews/2026-08-07-incident-remediation-diff-review.md`) — spool
retention is large, each pass is slow, and the race window per attempt widens
further.

### Amplifier B: memory scales with tape rate

Every in-window row is retained in a `Vec<TapeRow>` (`:359-361`, extended at
`:410`), and `trade_map` clones every trade's full `serde_json::Value` into a
`BTreeMap` (`:668`); metadata and settlement maps behave similarly
(`:569-596`, `:704-738`). Peak verifier memory is O(in-window rows × row size)
on the 7.75 GiB collector host
(`docs/reports/polymarket-shadow-memory-calibration-2026-07-16.md`), and the
verifier runs inline in the gate shell (`polymarket-raw-ops-shadow-gate.sh:1932`)
rather than under the shadow unit's memory envelope. On sufficiently high-rate
windows the verifier can be OOM-killed before producing evidence even when the
read race is won.

### Ranked candidates considered

1. **Stable-read race on the live baseline spool (structural, primary).**
   Breaks validation outright on high-rate tapes. Detailed above.
2. **O(spool) read cost and post-cutoff full-schema parsing (structural,
   amplifier).** Same root design — the read scope is "the whole spool, both
   passes" instead of "the comparison window" — so it is fixed by the same
   redesign, not separately.
3. **O(window) in-memory retention with full value clones (structural,
   secondary).** Degrades rather than breaks: OOM-kill on very high-rate
   windows. Fixed by streaming/digest comparison; independently shippable.
4. **Fixed 600 s trade maturity lag / 601 s gate tail (incidental).**
   `TRADE_MATURITY_LAG_SECONDS` (`polymarket_parity.rs:37`) and
   `PARITY_TAIL_SECONDS` (`polymarket-raw-ops-shadow-gate.sh:12`) are policy
   constants about provider eventual consistency, not tape rate. A tuning
   knob, not the defect.
5. **Within-lane duplicate `record_id` fails `dedupe_parity`
   (`polymarket_parity.rs:667-673`, `:1042-1048`).** Reviewed and judged
   fail-closed by design: collectors keep `trade_seen` state and must dedupe;
   a duplicate is genuine collector evidence, not a validator artifact.

## Impact

On high-rate tapes the shadow gate loses **all** parity guarantees for the
affected window — byte identity, field presence, dedupe, trade coverage and
contract, settlement, and rotation — because the verifier cannot complete a
stable read of the live baseline spool and bails before writing evidence. The
failure is fail-closed, so there is no false promotion risk; the cost is that
candidate releases cannot be promoted while baseline tape rates are high (or
while spool retention is large after an upload backlog), and gate failures
present as operational noise (`spool changed while reading parity window`)
rather than as a validator limitation. The break threshold is reached when the
baseline lane's append/rotation interval approaches the two-pass full-spool
read duration; at current reference-lane rates (7 symbols, 5/15-minute
markets) the race is rare, which is why it has not been observed in
production. This is a code-level structural analysis with a deterministic
synthetic reproducer (appending writer fixture); it has **not** been observed
against a live high-rate tape, and no production gate failure is attributed to
it here.

## Why this is structural

The validator's core protocol is "read the entire spool twice and require the
world not to move". No constant tuning removes the race:

- More retries or longer sleeps only multiply an attempt whose failure
  probability already tends to 1 as tape rate grows.
- The gate contract itself keeps the baseline running
  (`LEGACY_RUNTIME_STABILITY_REQUIRED=true`), so the writer cannot be
  quiesced without weakening an independent gate guarantee.
- Pass duration grows with spool size (Amplifier A), so the race gets worse
  exactly when the system is under stress (upload backlog, high-rate tapes).

The defect is in the read model — global quiescence over unbounded scope —
not in any threshold. The fix must change what is read and how stability is
judged, which is a redesign of `load_rows`/`stream_stable_rows`, not a
parameter change.

## Proposed follow-up fix direction

One PR, one behavior, scoped to the validator:

1. **Append-tolerant stability.** Tapes are append-only; treat growth as safe.
   Snapshot `(device, inode, size)` at pass start, read only bytes below the
   snapshot size, and afterwards verify identity is unchanged
   (same device/inode, size ≥ snapshot, no rename) instead of requiring equal
   size and mtime. Truncation, replacement, and indirection keep failing
   closed exactly as today.
2. **Window-scoped reads.** Select candidate tapes by rotation timestamp/name
   and mtime overlap with the comparison window before parsing, and stop
   parsing a tape once `recorded_at` exceeds the cutoff (rows are
   time-ordered per tape). Read cost becomes O(window), decoupling gate
   latency from spool retention.
3. **Do not impose full schema validity on rows outside the comparison
   window.** Parse enough to sequence-check and skip them; only in-window
   rows get contract validation. This removes the post-cutoff hard-error
   exposure described in Amplifier A.
4. **Bounded memory (secondary, may be a stacked PR).** Replace retained
   `Vec<TapeRow>` + cloned `BTreeMap`s with a streaming per-lane digest/set
   comparison (sorted `record_id` merge or per-identity hash accumulation),
   so peak memory no longer scales with in-window trade count.

Fail-closed semantics are the contract and must not move: sequence gaps,
truncation, symlink/indirection, schema violations inside the window, and
duplicate identities must still fail; the `monday.polymarket_shadow_parity.v1`
evidence metrics consumed by `polymarket-shadow-gate-policy.jq` stay
compatible, or schema and policy are versioned together in the same change.

## Acceptance criteria for the follow-up PR

- New focused test: a writer appends valid rows to the legacy spool's active
  tape at ≥ 100 rows/second while `compare()` runs; verification succeeds
  within the existing attempt budget and produces passing evidence.
- New focused test: a spool containing large out-of-window segments plus a
  small in-window set completes with rows-parsed / wall-time bounded by the
  window, not the spool size (assertion on parsed-row counts or a deterministic
  fixture bound).
- Regression tests proving fail-closed behavior is unchanged: mid-read
  truncation, rename/replacement, symlink swap, sequence gap, in-window schema
  violation, and duplicate `record_id` still error or fail parity.
- All existing tests in `rust_hft/tools/collector/src/polymarket_parity.rs`
  pass unchanged (`cargo test -p hft-collector --locked`, scoped to the
  collector crate per `AGENTS.md`), plus scoped Clippy clean.
- Evidence schema: `monday.polymarket_shadow_parity.v1` metrics consumed by
  the gate policy are unchanged, or any change ships with the matching
  `polymarket-shadow-gate-policy.jq` update in the same PR.
- No changes to gate recover logic (owned by a separate follow-up) and no
  changes to collector write paths.

## Out of scope

Gate recover logic and its own write-up are owned by another follow-up item
and are intentionally untouched here. Production runtime mutation is not part
of this document; no cloud or host state was changed while preparing it.
