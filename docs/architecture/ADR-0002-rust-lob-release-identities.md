# ADR-0002: Separate Rust LOB artifact, runtime-contract, and controller releases

- **Status:** Accepted
- **Date:** 2026-08-27
- **Scope:** Binance Rust LOB release identity. This ADR does not authorize a
  Gate, production controller application, service restart, or cutover.

## Context

The collector release directory is keyed by the binary SHA-256, while its
`deployment/` directory contains both runtime unit/env files and transition,
recovery, and readback controllers. Replacing those bytes for the active binary
would rewrite the rollback identity. Rebuilding an unchanged binary or rerunning
the real-market Gate for every controller correction would instead couple
unrelated evidence and keep the production host in the development loop.

## Decision

The LOB delivery chain has three immutable identities:

1. **Artifact release:** binary URI and SHA-256. A new binary requires a new
   digest-addressed artifact release.
2. **Runtime contract:** SHA-256 of the eight production/shadow unit and env
   files used by the formal Gate. Gate evidence is reusable only when this hash
   and the binary SHA-256 are unchanged.
3. **Controller release:** full deployment bundle SHA-256 plus a manifest that
   binds its source revision, OSS URI, artifact identity, and runtime-contract
   identity. It is published under
   `/opt/monday/releases/binance-lob-controller/<bundle-sha256>/` without changing
   the active artifact release.

`BUNDLE_ONLY=1` continues to reject the active production digest. The new
`CONTROLLER_ONLY=1` path is publish-only: it verifies the active binary and
release metadata, recomputes both runtime contracts, rejects indirect bundle
assets, and creates the controller release once. It does not write `/etc`,
change `/opt/monday/bin`, call service lifecycle commands, or touch `/data`.

A later controller application must name one exact controller-release digest,
hold the release/cutover locks, verify the active runtime contract, allowlist
destinations, preserve production PIDs and restart counters, provide rollback,
and emit direct byte and runtime readback. Publication alone is only **Release**;
it is not **Runtime** or **Readback**.

## Consequences

- Controller fixes can pass Code, CI, Merge, and Release without fabricating a
  new binary identity or consuming a market Gate.
- The active artifact directory remains immutable and usable for rollback.
- Applying controller bytes remains fail-closed and separately reviewable.
- A change to any gated unit/env file still requires a new runtime-contract
  identity and formal Gate before cutover.

## Rejected alternatives

- **Permit active `BUNDLE_ONLY`:** rewrites historical release evidence.
- **Rebuild the same source solely for a new digest:** invents artifact churn and
  does not prove controller correctness.
- **Rerun Gate for controller-only bytes:** tests a runtime payload that did not
  change and leaves the actual controller transition unverified.
