# DuckDB cache and build budget audit

Evidence date: 2026-10-16. Times below are durations. GitHub timestamps use UTC.
PR: [#1430](https://github.com/proerror77/monday/pull/1430).
Reviewed branch head: `22c909b727c3f7f5e365b304925844e3f14dcf81`.
This includes the fingerprint diagnostics added after `073396357`.

## Actual run evidence

| Run | Source tested | Cache result | Recipe phase | Complete Prediction CI |
| --- | --- | --- | --- | --- |
| [38429350500](https://github.com/proerror77/monday/actions/runs/38429350500) | main `08b98142` | Previous-run cache restored; DuckDB build script and library fresh | 13m48s | 20m51s |
| [38767757268](https://github.com/proerror77/monday/actions/runs/38767757268) | main `7de64784` | Previous-run cache restored; first DuckDB recipe not fresh | 26m17s | 30m14s |
| [38831045248](https://github.com/proerror77/monday/actions/runs/38831045248) | PR merge `0be6aab8`, head `07339635` | Exact and compatible keys absent | 36m30s | 39m35s |
| [38836120877](https://github.com/proerror77/monday/actions/runs/38836120877) | PR merge `70eccba8`, head `22c909b7` | Exact and compatible keys absent | 26m01s | 29m19s |

The last run used EPYC 9V45 with four CPUs. Its image job took 27m24s.
The build step took 26m04s. The operator recipe took 19m21s.
The scientific worker took 4m49s and marked both DuckDB artifacts fresh.
The offline smoke step took 48s. Steps overlap or nest; do not add their durations.
The preceding PR run used EPYC 7763. These are not controlled speed comparisons.

## What directly caused the latest native compilation

Run 38836120877 logged failed reads for all three DuckDB fingerprints:

```text
target/research-core/x86_64-unknown-linux-gnu/release/.fingerprint/libduckdb-sys-52f9d69926e21174/lib-libduckdb_sys
target/research-core/x86_64-unknown-linux-gnu/release/.fingerprint/libduckdb-sys-bdeb33e435cea0fa/run-build-script-build-script-build
target/research-core/release/.fingerprint/libduckdb-sys-e0e6c3cde5c60cd6/build-script-build-script-build
```

The restore log also says `Cache not found`. This directly establishes a cold
target, rather than a dirty fingerprint caused by a dependency update.
Both PR runs requested compatibility identity
`11f7e6516ebdf509e52dba420622288cdae7e4546fb6ffa5ab64c3b01e7adf49`.
The instrumentation-only change preserved that identity.

Historical main runs used compatibility identity
`875c4a7d72813afb1feadc78bb76e0876d08ac727ec9f1ad648ef51ff634c37c`.
The identity migration intentionally separates those caches.
Successful main builds alone save cleaned dependency bytes. PR runs do not save them.
Therefore repeated PR runs cannot populate the new trusted identity.
Do not broaden writes to untrusted PR contexts to obtain a warm measurement.

## Build dependencies and real Cargo units

The downloaded `libduckdb-sys 1.10504.0` registry archive matched the lock checksum:
`94e67523a40ee3da30411e00e243990b3760d91877202ab2f39c03ab34f8dbfc`.
Its manifest unconditionally declares reqwest with blocking and rustls-tls features.
Only the non-bundled source module uses the download client.

Locked metadata at tested source `70eccba8` contains the dependency chain:

```text
libduckdb-sys 1.10504.0 build script -> reqwest 0.12.28 -> rustls 0.23.46
```

A read-only Cargo 1.98.1 unit graph confirms a host reqwest/rustls build beneath
the bundled DuckDB build script. This is an actual build dependency, not only
an unused lockfile entry. The graph command used `RUSTC_BOOTSTRAP=1` only for
the diagnostic `-Z unstable-options --unit-graph` operation. It compiled no
production bytes and changed no workflow, lockfile, cache admission or compiler flags.
Production builds continue to use the existing stable Cargo command.

Security commit `7fbeb700` changes rustls from 0.23.45 to 0.23.46.
That identity change can legitimately invalidate the DuckDB build script and its native outputs.
Run 38767757268 restored the exact cache saved by run 38429350500, then rebuilt DuckDB.
However, it did not retain Cargo dirty reasons. The dependency change supports
this explanation; the historical invalidation reason cannot be recovered from those logs.
Do not claim it was the sole reason.

A separate three-package Cargo 1.98.1 experiment tested this mechanism.
A bundled C archive had a download build dependency referenced only under the disabled cfg branch.
Cold build: native build script/library fresh=false. Unchanged build: both fresh=true.
Changing only the unused build dependency made both fresh=false and reran the C build.
Cargo reported `StaleDepFingerprint`; the archive hash stayed identical across all three phases.
The consumer executed successfully each time. This is a small mechanism reproduction,
not a DuckDB compilation benchmark or historical-run fingerprint evidence.

## Conservative boundaries

The exact suffix binds full provenance. Source and packaging changes can alter it
without preventing compatible dependency restoration.
The compatible prefix binds the compiler, builder digest, native environment,
flags, profiles, compiler helper, cleanup contract and native dependency identities.
Final executable hashes, source admission, ABI checks and offline smoke remain separate.

There is still excess scope:

- Native environment capture hashes every installed dpkg package, including packaging tools.
- One combined prefix covers the cached owners. A native change can prevent restoration for all owners.
- Cargo metadata resolves workspace feature unions rather than exact host compilation units.
  At `70eccba8`, metadata adds reqwest json, stream, h2 and http2 features.
  The DuckDB host reqwest unit uses blocking/rustls without those extra features.
  The metadata closure contains 137 package identities; the host build closure contains 113.
  The excess includes quinn, h2 and their dependencies. This proves excess key scope,
  not an observed cache miss caused by any one of those packages.
- The native identity helper and cache-layout script bytes enter the compiler contract hash.
  A semantics-preserving helper edit still migrates the identity.

These are potential miss amplifiers. They are not dirty reasons for the latest cold run.
Removing rustls from the identity alone would not stop Cargo from rebuilding it.
It would also discard a real build-dependency identity boundary.
This change therefore retains the native key, checksum and feature controls.

Safe future narrowing needs either an authenticated native artifact boundary or
a reviewed upstream change that makes download dependencies conditional.
The latter must preserve non-bundled behavior and regenerate every owning lockfile.
Exact host-unit projection also needs a supported producer interface and regression evidence.
The diagnostic unstable Cargo interface is not added to production key generation.

## CPU allocation correction

The old scheduler assigned two jobs to each owner on a four-CPU runner.
That allocation remained fixed after both faster owners exited.
In run 38831045248, Prediction finished at 00:59:59 UTC.
The research-core owner then ran alone until 01:21:07 UTC with two jobs.
This is 21m08s of nominal two-job capacity left unassigned.
It does not prove the CPUs were physically idle or quantify recoverable time.

The correction grants the final owner the admitted total budget only after every
sibling exits and the pending owner queue is empty. Each recipe reads its budget
once before Cargo starts. An atomic file replacement prevents partial reads.
Current Cargo/cc jobservers retain their initial allocation.
Therefore this correction does not accelerate an already-running DuckDB build.
It can reclaim CPUs for the following scientific-worker recipe.

The global CPU budget, memory overlap limit and one writer per owning target remain enforced.
Regression tests exercise a delayed owner, reclaim on its next recipe, total active
jobs, failure cancellation, TERM-ignoring descendants and removal of budget files.
The compiler helper and native compatibility identity remain unchanged.
These tests establish scheduling behavior, not elapsed-time improvement.

## Remaining 10–15 minute conditions

1. Obtain a compatible, successful trusted producer cache and verify restoration
   in a different run. Do not label same-run reuse as cross-run evidence.
2. Capture dirty reasons on a genuine restored-but-rebuilt case. Distinguish
   missing files, build-dependency units, features, compiler/environment and file freshness.
3. Measure the CPU correction on comparable hardware and cache state. It cannot
   reclaim CPUs inside an already-running native build.
4. Budget the full critical path: admission, setup, build, archive transfer, smoke
   and final CI gate. Historical warm Prediction CI still exceeded 15 minutes.
5. Measure publication separately only under an authorized publication task.
   A successful PR CI run does not establish merge-to-registry or production latency.

Cold native compilation already exceeds the complete target on the measured runners.
The existing 8m22s same-run dependency-warm recipe result is not complete CI.
No measured result here establishes a 10–15 minute full delivery outcome.

## Download isolation implementation

The continuation starts from PR head `f82a1ad2` and merges main `4310dc7e`.
The patch vendors the checksum-verified `libduckdb-sys 1.10504.0` package at
`rust_hft/native/libduckdb-sys`. Its C++ archive and native compiler backends match upstream bytes. Bindings keep
upstream code with one trailing blank line removed. The functional changes affect
only its manifest and downloader cfg gates.
The license and patch provenance remain beside the source.

Research-core and data-pipelines select this patch. Their lockfiles retain all
other package versions. Bundled and system linking exclude reqwest and zip.
The `download` feature retains the upstream downloader and requires the existing
`DUCKDB_DOWNLOAD_LIB` switch. Requesting a download without that feature fails.
Prediction retains its independently locked upstream `1.10501.0` package.

Research cache identities hash every native patch file and the resolved native
closure. The dev/test owner cache also binds patch bytes and source preparation.
Both cleaners retain this exact native path while deleting application artifacts.
Loop nextest uses the same owner-cache contract, with its own namespace. This
avoids Swatinem removing the newly local native dependency before a trusted save.
PR jobs still cannot save trusted dependency caches.

Fresh CI checkouts normalize native source mtimes before any target restore.
Preparation rejects dirty source or existing owning targets. Local builds retain
actual mtimes. This handles local-path freshness without hiding changed source
bytes from the compatibility identity. Native library ABI, flags, compiler,
profiles, artifact hashing and offline smoke remain enforced.

Focused checks cover system/downloader compilation, rejection without the
feature, bundled dependency exclusion, source-byte cache invalidation, cleanup
retention and source timestamp guards. Actual macOS integration checks use
`SELECT 42` with the explicit downloader and system linking. The bundled native
build and complete existing CI are reported separately; fixture and dependency
graph checks do not establish a 10–15 minute full pipeline.
