# Research CI planning and compilation

Prediction CI selects checks from the current source change first. Those checks
can start while `research-image-scope` recovers pending image delivery. Binary
production and smoke wait for that recovery. The final CI gate requires both
planning stages and every selected check to succeed.

Image recovery always reads authenticated product publication history. A later
documentation change must not discard earlier unpublished software. Only a valid
offline archive allowance causes a second history read for signed Builds.
Without that allowance, `research_archive_pending_product=not-evaluated` and
`research_archive_base_sha=null`. These values mean unknown, not published.
An approved archive history failure still blocks research delivery and the gate.

The history reader retains product-specific baselines, original run attempts and
partial image successes. An image does not prove a signed Build. A successful
no-op publisher does not advance either baseline.

Use `test-main-research-scope.sh`, `test-ci-monitor-scope.sh` and
`test-acr-publish-workflow.sh` for these contracts. Test doubles prove planning
and rejection behavior; they do not measure hosted CI latency.

## Owner compilation

The producer groups selected recipes by their owning Cargo manifest. The groups
use the four existing cache targets. Each group keeps its original recipe order,
features and executable selection. Shared executables are still built once.

At most two owners compile concurrently. The scheduler divides the runner's CPU
budget between active slots. `CARGO_BUILD_JOBS` can lower that total budget, but
cannot exceed `nproc`. Available memory permits one concurrent owner per 4 GiB,
with at least one owner. The cgroup memory limit can reduce the host allowance.
This is a limit on overlap, not a hard limit on compiler memory usage.

Set `MONDAY_RESEARCH_BUILD_CONCURRENCY=1` to measure the same recipes serially.
The default is `2`. Each timing record includes requested and actual owner
concurrency, Cargo jobs, available memory, source and compilation input hashes.
Each owner writes its own timing file. The parent validates the full recipe set
before combining successful measurements in catalog order.

A compiler failure stops dispatch and terminates all unfinished owner process
groups. Cancellation does the same. Descendants which ignore TERM receive KILL.
The producer cannot package a release after either failure. Process ownership,
exact recipes and bounded overlap are exercised by
`test-research-release-products.sh` without invoking a Rust compiler.

The scheduler is inside `build-research-recipes.sh`. Existing compilation-input
hashes therefore bind its implementation. Changing it starts a new compatible
cache identity. Compare serial and concurrent runs on the same source, runner
and warmed dependency cache. A first cold run cannot establish a speedup.

Publication still consumes one producer job and one verified release bundle.
This change does not introduce matrix producers or change release provenance.
