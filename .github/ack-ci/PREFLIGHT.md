# Cross-lane preflight proposal

This change is local and has not been activated. Private executor changes are
required together with the public relay patch. Keep existing executions paused
until the controller owner accepts the protocol and scoped validation results.

Monorepo's `Research preflight` relay consumes `ci-research-preflight` success.
Rust depends on that job for ACK scope. Cross-workflow Clippy and research binaries
are guarded by private admission, execution revalidation and public receipt
consumption; public workflows do not dispatch private work.

Collector-scoped instances of these four profiles use `monday.ack_execution_receipt.v2` at the existing receipt
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
remain required. Other profiles keep v1. Binary v2 receipts retain the independently
verified software download descriptor consumed by the existing download checks.

The approved quick recipe and v2 migration cover collector scope only. Other
research scope does not schedule preflight and retains existing v1 receipts and
full gates. The relay derives scope from source; private compiler recipes recheck
committed scope before compilation, with no control-plane credentials available. There is no cross-commit compiled target reuse: only workspace registry/git
downloads and immutable toolchain image layers are reused; targets retain exact SHA
isolation. No runner, storage, permission or budget change is included.

Local checks (no compilation/network dispatch):

- `.github/scripts/test-ack-preflight-relay.sh`: signed quick/heavy consumption,
  verified binary download, producer rerun rejection, bounded receipt timeout.
- `.github/scripts/test-preflight-workflow-gate.sh`: dependency edges, non-ACK skip,
  failed/cancelled prerequisite rejection and required full Rust gate.
- `.github/scripts/test-classify-ack-research-job.sh`: profile routing.

Private matching contracts additionally cover signature tampering, missing/expired
proof, source/base/manifest/scope mismatch, API failure, cancellation, timeout,
new head and fresh matching reruns, plus checks before and after actual execution
through local command substitutes. Real ACK/signing/API end-to-end acceptance
remains with the existing controller owner before resuming work.
