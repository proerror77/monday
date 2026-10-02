# ACK validation and receipt contract

The public reader and private executor are deployed together. New requests must
use their reviewed source, command manifest and exact run/attempt/job identities.

Monorepo's `Research preflight` relay consumes `ci-research-preflight` success.
Rust depends on that job for collector ACK scope. Tests and strict Clippy run
in one `ci-rust` batch, using the selected `loop_packages` rather than always
testing all seven Loop packages. Security verifies that same signed batch and
its Monorepo producer attempt/job; it does not request another compiler. The
weekly Security audit has no Monorepo sibling and retains its fixed Clippy-only
profile. Research binaries remain guarded by private admission and proof
consumption; public workflows do not dispatch private work.

The `ci-rust` batch always binds a run attempt. Collector preflight and
collector-scoped binary/weekly-Clippy profiles also use `monday.ack_execution_receipt.v2` at the existing receipt
branch's `<run>/<attempt>/<job>/<checkout>/receipt.json` and `.sig` path. The
controller must commit the producer's exact run attempt and numeric job ID, sign
its independently verified terminal, wait for its public job success, then commit
heavy requests referencing that receipt's SHA256 and run/attempt/job IDs. Dispatch
once per selected job attempt under the existing global lock. Missing prerequisites
reject immediately; there is no auto-dispatch or wait loop inside private admission.

Heavy acceptance requires matching source/head/base, scope, command manifest,
signature, current run attempts, successful producer job, current live source,
and unexpired evidence. A producer workflow can remain running after its quick
job succeeds, avoiding a dependency cycle with Rust. Cancelled/failed/rerun
producers and changed source fail closed. Public metadata GETs require no additional
token permission; API errors/rate limits prevent acceptance. Existing full gates
remain required. Other profiles keep v1. Static layout, source-policy and `cargo fmt` checks
run on GitHub without compiling research code or waiting on an ACK receipt. Binary v2 receipts retain the independently
verified software download descriptor consumed by the existing download checks.

The quick preflight recipe covers collector scope only. Other research scope
does not schedule preflight, but its batch still binds the exact attempt and
lists all selected test and Clippy stage results. A missing, duplicated or failed
stage cannot satisfy either consumer. Trusted negative receipts terminate the
wait before success-only preflight/artifact requirements. The relay derives scope from source; private compiler recipes recheck
committed scope before compilation, with no control-plane credentials available. There is no cross-commit compiled target reuse: only workspace registry/git
downloads and immutable toolchain image layers are reused; targets retain exact SHA
isolation. No runner, storage, permission or budget change is included.

Local checks (no compilation/network dispatch):

- `.github/scripts/test-ack-preflight-relay.sh`: signed quick/heavy consumption,
  verified binary download, producer rerun rejection, bounded receipt timeout.
- `.github/scripts/test-preflight-workflow-gate.sh`: dependency edges, non-ACK skip,
  failed/cancelled prerequisite rejection and required full Rust gate.
- `.github/scripts/test-classify-ack-research-job.sh`: profile routing.
- `.github/scripts/test-ack-rust-batch.sh`: stage/package coverage, producer
  attempt/job drift and public static checks.

Private matching contracts additionally cover signature tampering, missing/expired
proof, source/base/manifest/scope mismatch, API failure, cancellation, timeout,
new head and fresh matching reruns, plus checks before and after actual execution
through local command substitutes. Real ACK/signing/API end-to-end acceptance
remains with the existing controller owner before resuming work.
