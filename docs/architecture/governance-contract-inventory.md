# Monday V2 governance-contract inventory

**Status:** Phase 1 runtime-admission extraction complete; attribution and bundle extraction pending

**Baseline:** integrated against `main` on 2026-08-21; Git history records the
exact source revision.

**Scope:** identify the smallest contract families that can leave
`alpha-harness/domain` without changing wire format, hashes, signatures, or
runtime behavior.

This document is an inventory, not a new authority. The extracted governance
crate is now the sole source of truth for runtime admission; the old
`alpha-domain` definitions and exports are removed.

## 1. Contract families

| Family | Current source | Runtime/research consumers | First extraction boundary |
| --- | --- | --- | --- |
| Runtime admission | `governance-contracts/src/lib.rs:1-511` | `apps/live/src/deployment_envelope.rs`, `apps/live/src/main.rs`, `alpha-harness/store/src/lib.rs`, `alpha-harness/app/src/governance.rs` | extracted as `hft-governance-contracts`; no `alpha-domain` re-export remains |
| Runtime attribution | `alpha-harness/domain/src/lib.rs:3467-3755`, `alpha-harness/domain/src/runtime_latency_evidence.rs` | `apps/live/src/runtime_attribution.rs`, `apps/live/src/main.rs`, `alpha-harness/store/src/lib.rs`, `alpha-harness/engine/src/learning.rs` | attribution enums, event, signed/verified event, signing/verification, and the runtime-stage health predicate; keep learning policy outside the runtime crate |
| Promotion and bundle identity | `alpha-harness/domain/src/lib.rs:4705-5060` | `apps/live/src/deployment_envelope.rs`, `alpha-harness/store/src/lib.rs` | `StrategyBundleArtifact`, `StrategyBundle`, and `PromotionRecord`; this family must remain separate from runtime admission because its artifact payload is research-owned |
| Canonical hashing | `alpha-harness/domain/src/lib.rs:5469-5498` | nearly every research artifact plus admission and attribution | do not move in the first extraction; first prove whether a tiny hash utility can be shared without making the governance crate depend on research types |

## 2. Ownership decisions

### Runtime admission is governance-owned

`DeploymentEnvelope` is the signed boundary consumed by `hft-live`. The
research plane may produce it, but it cannot widen the runtime policy, consume
the nonce, resume a paused runtime, or submit an order. `RuntimePolicyDocument`
in `apps/live/src/deployment_envelope.rs` is the runtime-side JSON input that
binds to `RuntimeEnvelopePolicy`; it should move with the admission contract in
a later slice, while `ActivationRequest` and `SystemConfigActivationAdapter`
remain runtime implementation types.

The following are one contract family and must move together so the signed
wire identity does not split:

- `AllowedIntentType`
- `ApprovalClass`
- `RuntimeApprovalEvidence`
- `LiveSmallEligibilityEvidence`
- `DeploymentEnvelope`
- `SignedDeploymentEnvelope`
- `VerifiedDeploymentEnvelope`
- `RuntimeEnvelopePolicy`
- `sign_envelope`, `verify_envelope`, `deployment_scope_hash`

`GovernanceError` now owns the admission errors. `DomainError` retains research
and attribution errors; `hft-live` keeps an explicit error boundary for each so
the runtime path cannot silently convert research failures into admission
failures.

### Runtime attribution is a feedback contract, not a research command

`RuntimeAttributionEvent` is append-only evidence emitted by Paper, Shadow, and
the guarded LiveSmall path. The research engine may ingest verified events to
open a `LearningDirective`, but it must not mutate runtime configuration from
the event. `runtime_latency_evidence.rs` validates the signed event log and
therefore stays adjacent to this family during extraction.

`alpha-harness/engine/src/learning.rs` remains a consumer. Its failure-class
mapping is research policy and must not be moved into the runtime contract
crate.

### Promotion and bundle identity is the seam between research and runtime

`StrategyBundle` and `PromotionRecord` bind candidate, dataset manifest,
evaluator, sealed evidence, and runtime-loadable artifact hashes. They are
produced by the research store and read by `hft-live`; neither side may own the
other side's validation policy.

`StrategyBundleArtifact` currently embeds `FactorAst`, ONNX candidate data, and
the CEX four-stage candidate. This is the main extraction blocker: moving the
bundle family without first deciding ownership of those artifact DTOs would
create a new wrapper/shim and preserve the existing coupling.

## 3. Regression-vector inventory

The first extraction is allowed only if these checks remain green with the
same JSON/hash/signature vectors:

| Check | Location | Protects |
| --- | --- | --- |
| Runtime envelope round-trip and tamper rejection | `alpha-harness/domain/src/lib.rs:7130-7355` | canonical payload, signature, key, expiry, nonce, binding, limits, instrument and approval checks through the governance re-export |
| Runtime admission authority checks | `apps/live/src/deployment_envelope.rs:999-1080` | LiveSmall fail-closed behavior, sealed CEX cost restrictions, capacity cap |
| Runtime attribution signature and health | `alpha-harness/domain/src/lib.rs:6113-6248` | signed feedback, event scope, cost coverage, reconciliation truth |
| Runtime event attribution | `apps/live/src/runtime_attribution.rs:1422-2321` | order/fill identity, deduplication, mark freshness, stream-gap failure, final drain |
| Runtime latency evidence | `alpha-harness/domain/src/runtime_latency_evidence.rs:282-522` | signed log readback and LiveSmall-only evidence admission |
| Bundle/promotion integrity | `alpha-harness/domain/src/lib.rs:7300-7780` and `alpha-harness/store/src/lib.rs` bundle tests | bundle hash, legacy readback, promotion binding, artifact eligibility |

The owning package checks remain the smallest required validation:

```text
cargo test -p hft-governance-contracts --locked
cargo test -p alpha-domain --locked
cargo test -p hft-live --no-default-features --test deployment_artifacts --locked
cargo test -p hft-live --no-default-features --test deployment_envelope --locked
cargo metadata --locked --no-deps
```

No extraction may be declared complete from a compile-only result.

Integration baseline: `alpha-domain` passed `73/73` unit tests and `0` doc-tests;
the governance crate and runtime consumer checks also passed.

## 4. Migration order

1. ~~Extract runtime admission contracts and their canonical hash/signature
   helpers~~ (complete); the new crate preserves the existing JSON field order,
   hashes, signatures, and fail-closed checks.
2. ~~Migrate the research store and alpha CLI envelope read/write and approval
   paths~~ (complete); both consume the governance crate directly.
3. Extract runtime attribution contracts and migrate the live observer and
   signed-log verifier; keep `engine::learning` as a consumer.
4. Extract promotion/bundle identity only after artifact DTO ownership is
   resolved and the store can read both old and new records through an explicit
   versioned boundary.
5. Remove the old exports only after package tests, hash vectors, and runtime
   deployment-envelope tests pass from the new owner.

## 5. Stop rules

- Any change to a V1 JSON field, canonical payload, content hash, signature
  input, approval scope hash, nonce behavior, or runtime error boundary stops
  the extraction.
- A new crate may not depend on `alpha-harness/engine`, `alpha-harness/store`,
  `risk`, `OMS`, or execution adapters.
- The extraction must not add an order path, risk-limit mutation, runtime
  resume path, LLM authority, or LiveSmall enablement.
- No production Gate, deployment, cutover, credential change, or runtime
  readback is part of this inventory slice.

## Completion

The exact first contract family, consumers, tests, and blockers are named. The
runtime-admission implementation now lives in `hft-governance-contracts`; no
production deployment, cutover, or LiveSmall enablement was performed.
