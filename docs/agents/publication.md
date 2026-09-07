# Artifact publication policy

Repository delivery and production authority are defined in ../../AGENTS.md.
Configured automatic workflows are standing automation: a merge authorized by
the user may cause an artifact to be published after its exact source passes
release admission. No additional per-run approval is required for that configured
publication. Changes to registries, recipients, production targets or authority
require the corresponding scope authorization.

Artifact publication and production deployment are separate. Publishing a runner,
controller or trading binary does not authorize running it, a collector cutover,
sealed holdout, trading, risk changes, or resuming a paused runtime.

## Admission

All production artifact paths require the same three authenticated GitHub Actions
checks for the exact source SHA, read by read-release-required-checks.sh. Do not
accept similarly named checks from another app, skipped checks, or another SHA.
ACR requires current main and retains its additional binary provenance, smoke and
artifact readback checks. GHCR main/manual publication requires current main.
Version-tag publication requires the tagged commit to belong to main history and
the same exact-source checks. The explicit ACR source-test target remains a
non-production diagnostic exception with its existing identity restrictions.

Publishing may wait a bounded time for main checks; failure, cancellation,
missing evidence at the deadline, or source drift fails closed. Reconcile those
conditions before an authorized retry. CI and publication remain separate labels.

## Rule ownership

AGENTS.md owns delivery authorization and validation policy. GitHub branch
protection owns merge enforcement; workflows and shared scripts own executable
CI/release admission. Claude entrypoints refer to AGENTS.md. Module instructions
contain local differences. Keep feature branches after merge; deletion is a
separately authorized cleanup. The repository auto-delete setting is disabled.

Review rule changes against local-only fixes, a PR-only task, a plan explicitly
including multiple merges, ambiguous publishing, CI failure, transient GitHub
failure, and production deployment. Use existing contract tests; do not turn
wording into brittle string-match tests.
