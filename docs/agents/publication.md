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
checks for the exact source SHA, read by read-release-required-checks.sh.
Accept success or skipped for these three checks. Reject similarly named checks
from another app or another SHA.
These checks aggregate the selected validation plan; they do not require every
product to build for each release. Monitoring and CI-policy changes select their
own contracts. The current-source aggregate must reject a missing, failed,
cancelled or skipped selected task; an unrelated task is not a release blocker.
ACR requires current main and retains its additional binary provenance, smoke and
artifact readback checks. GHCR main/manual publication requires current main.
Version-tag publication requires the tagged commit to belong to main history and
the same exact-source checks. The explicit ACR source-test target remains a
non-production diagnostic exception with its existing identity restrictions.

Automatic publication starts with Release. Completion of any required main-push
CI workflow wakes it. Pull requests do not. Release reads the three required
checks for that head SHA and calls GHCR only when each is success or skipped.
ACR wakes after Release completes and independently reads the same exact-source
checks and Prediction artifacts. It retains its native workflow, job names and
OIDC identity, which the existing Build issuer requires. GHCR failure does not
replace ACR's CI admission decision.

Release carries the original CI source in a dedicated native metadata job:
`Release source v1 [<source SHA>] [<CI run ID>/<attempt>]`.
ACR validates the exact Release attempt, its unique successful marker, and the
original main-push CI attempt before using that source. The Release controller
HEAD can differ from the original CI source. Missing or invalid markers fail
closed; ACR never substitutes the controller HEAD. This marker grants no build,
publication or runtime authority. All existing admission checks still follow.

Pending evidence exits without publishing. The final upstream completion causes
another evaluation. GHCR skips a full-SHA image tag or a successful nested
publication job, including earlier attempts. ACR requires its own successful
product publication marker; raw manifests do not prove signed release completion.
No-op runs are not publication evidence. Existing concurrency serializes each
publisher. The admitted SHA binds its checkout and OCI revision.
Manual and tag publication retain their entries and may wait a bounded time.
Failure, cancellation, missing evidence at the deadline, or source drift fails
closed. CI and publication remain separate labels.

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

## Image impact and reuse

PR/develop smoke and main security CI share an image dependency plan. On main,
the published GHCR hft image's OCI revision is the baseline for unpublished core
image impact, so a later unrelated commit cannot lose a pending publication.
Other image checks keep their per-change scope. Missing, ambiguous or unrelated
registry revision metadata fails planning instead of silently skipping work.

Automatic GHCR publication promotes the exact image saved by successful main
security CI. Producer source/run/attempt, archive hash, image ID and OCI revision
must match; missing or expired artifacts fail automatic publication. Explicit
manual/tag publication may build once when no retained artifact exists. Every
path retains required-check admission and immutable registry readback.

Runtime experiment configuration and ordinary Job manifests do not rebuild
research binaries. Scripts/templates copied into the controller image remain
image inputs; changes to that COPY boundary must update the scope mapping.

Research release v6 binds an exact product set to source, compiler/native inputs,
producer run, attempt, job, owning lockfiles, and executable hashes. The catalog
has three independently published images: CEX `research-runner`, Prediction
`prediction-research-runner`, and CEX `campaign-cycle-controller`. A producer
builds the union once; each image copies only its own executable subset and reads
those exact bytes back from the registry. Prediction's Job uses
`monday-prediction-worker`, not the CEX harness.

Controller asset changes build only its four required executables. CEX and
Prediction source changes select their owning products through the Cargo graph.
Each image has its own authenticated publication baseline, so unpublished work
survives later main commits and another product's successful publication. Old
mixed runner markers do not prove publication of the split images. The v6
migration bootstraps each product until its own readback succeeds.

Main CI keeps every product and test selected by the current source change.
When public OSS product configuration is absent, CI defers only additional builds
carried from unpublished history. An absent public policy or a missing, null or
`{}` value for `oss_by_product` confirms this condition. Invalid JSON or another
field type retains the existing carry plan.
Any nonempty product map also retains that plan, including an incomplete map.
Native publication still validates each selected product. The selector reports
pending and deferred products without advancing any publication baseline.

ACR independently recalculates pending products when binary and smoke jobs were
skipped. Pending work fails publication; it cannot become a successful no-op.
After configuration is repaired, the next main push rebuilds pending products
from their original publication baselines. A variable update alone starts no job.
If the current main push produced no research artifact, rerun all its Prediction
CI jobs to recalculate the plan. Do not use a new Prediction manual dispatch for
artifact reuse. If an earlier attempt already produced research artifacts, retain
the existing artifact admission checks and use the next main push instead.
An explicit ACR research target with `rebuild_research_runner=true` can also
recover that product. Use `research-products` to recover all three research images
from one union build. The `all` target also includes four other images.
Before a manual rebuild, the selector checks configuration presence and every
selected product's public policy. Missing settings stop before compilation.
Native signing, OIDC and TLS checks still run in each publisher.
Compatible manual builds share Prediction's dependency cache and owning-workspace
targets. Compiler, native packages, profile, flags and recipes remain cache inputs.
The manual run remains its own producer and uploads its own artifact and timings.
Dependency reuse does not admit software artifacts from another source or producer.
Every recovery keeps current-main checks, original attempt identity, native
signing, OSS admission and image readback requirements.

The separate native issuer signs each Build only after authenticated CI,
original software producer, immutable OCI programs and OSS bytes agree.
Preflight binds operator policy and TLS to the selected publication before
registry writes. Keys remain outside research services and the PG ledger.
Signed proofs use `research/builds/{build}/releases/{oci}/{proof}/`; shared
executable blobs keep their Build prefix. ACK PG projection requires separate read-only OSS credentials, completed producer
verification, current host approval and the dedicated importer role. Ordinary CI
never gets that PG identity. See the [OSS migration contract](../../deployment/aliyun/research/foundation/RELEASE_OSS.md).

Collector publication loads the successful Monorepo CI image. It checks the
producer, platform, archive hash, image ID and source before promotion. The
publisher does not compile that product. Immutable registry readback must
match the saved image ID. Missing or expired evidence blocks publication.

## Publication records

Each publisher retains a JSON record after immutable image readback. It includes
merged PR numbers and their head commits and trees, the main commit and tree,
the source image config digest, the published manifest digest, and the run attempt.
`base_image_digest_kind=oci-config` identifies the tested image before promotion.
It is not the Dockerfile FROM image. Promotion keeps that config digest and does
not issue a new signature. The existing research Build issuer remains separate.
