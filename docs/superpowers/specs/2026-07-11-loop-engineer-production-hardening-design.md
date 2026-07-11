# Loop Engineer Production Hardening Design

## Decision

Keep the existing Rust research/runtime split and harden it in place. The system is a Loop Engineer only when a bounded research goal can run, checkpoint, evaluate explicit completion criteria, retain failure evidence, create a follow-up mission, promote one immutable strategy bundle, execute that exact bundle in paper or shadow, and feed attributed runtime evidence back into the same lineage.

LLM, GP, MCTS, Bayesian optimization, ML training, and offline RL remain outside the per-tick and per-order path. They may propose hypotheses, factor ASTs, model artifacts, search priorities, and allocator targets. They may not place orders, resume trading, load an unverified artifact, or increase a hard risk cap.

## Loop Contract

The durable loop is:

1. `LoopRun` defines the overall goal, completion policy, current stage, terminal reason, and child research missions. `ResearchMission::Completed` means only that its research-local criteria passed.
2. A proposal engine emits a typed `CandidateArtifact` and evidence of budget use.
3. A deterministic evaluator returns a versioned report with trade count, net return, uncertainty, drawdown, and failure reasons.
4. The kernel checkpoints every iteration, including versioned engine-specific search state, and stops only on research completion, pause, failure, or budget exhaustion.
5. Repeated failures create immutable learning directives and bounded follow-up missions.
6. Promotion binds the candidate, dataset manifest, evaluator version, sealed result, and approval state into one content-addressed `StrategyBundle`.
7. `hft-live` verifies and loads that exact bundle before runtime construction. Paper and shadow may activate automatically; live-small remains disabled.
8. Execution events and portfolio snapshots produce append-only attribution tied to deployment, bundle, strategy, mission, and asset revision.
9. Attribution may change future lab search policy only after deterministic validation. The adopted child policy is pinned by the next mission. It never changes runtime hard caps.

`LoopRun` advances through explicit stage records: `researching`, `walkforward_kept`, `holdout_passed`, `paper_healthy`, `shadow_healthy`, and `live_small_eligible`. A run completes only when its declared target stage is reached; candidate count or budget exhaustion is never a success heuristic.

## Trust Boundaries

### Research plane

- Owns goals, data manifests, search, evaluation, lineage, memory, and proposals.
- Exposes no order, cancel, credential, wallet, or raw transaction capability.
- Treats opaque program/model/allocator JSON as lab-only until compiled into a typed bundle.

### Governance plane

- Creates deployment envelopes only from a persisted promotion record.
- Verifies every referenced hash and approval record before signing.
- Uses approvals with subject, scope, validity window, revocation state, and signer identity.

### Runtime plane

- Owns trusted keys, nonce ledger, account state, risk caps, OMS, execution, reconciliation, emergency cancellation, and attribution.
- Calculates `effective_limit = min(agent_request, strategy_cap, account_cap, venue_cap, global_hard_cap)`.
- Fails closed on unknown balances, positions, open orders, unsupported reconciliation, stale data, or invalid deployment state.
- Keeps pause, degrade, and emergency-stop controls directly available. Resume, model load, strategy replacement, and risk increases require the same validated deployment/approval path.

## Strategy Bundle

The first production-capable bundle supports only artifacts the runtime can execute deterministically:

- `Formula`: validated Factor DSL plus live feature mapping, signal threshold, order size request, and strategy risk request.
- `OnnxModel`: content-addressed model artifact plus existing deterministic DL strategy configuration.

Other candidate variants remain research-only and cannot be signed. Model files are not embedded; the bundle includes path/URI, SHA-256, size, and schema/version metadata. Formula and model bundles share the same asset, evaluation, and risk bindings.

## Accounting And Execution Safety

- Portfolio accounting handles opening, increasing, reducing, closing, and crossing long/short positions using exact decimals.
- Realized PnL is recorded only for the closed quantity. Unrealized PnL uses signed quantity and current mark.
- Drawdown is based on account equity and an initialized equity high-water mark.
- Emergency mode pauses new intents and sends cancellation commands for every locally open order. Automatic flattening is not claimed until reduce-only semantics exist for every enabled venue.
- Reconciliation detects exchange-only, local-only, status/quantity mismatches, balance failures, and client errors. Unknown state halts live execution.

## Research Integrity

- A no-trade candidate cannot pass.
- Defaults require a positive edge margin, minimum observations, minimum trades, bounded drawdown, and a multiple-testing-adjusted score.
- Sealed holdout remains one-time and promotion-only.
- Data missions reject partial candles, duplicate timestamps, interval gaps, invalid OHLC relationships, negative volume, stale acquisition, and manifest mismatches.
- Source catalog entries distinguish advertised connector capability from implemented governed acquisition capability.

## Security And Delivery

- No key, password, token, credential file, or populated environment file is tracked or copied into image build contexts.
- Exposed credentials are treated as compromised. Current-tree removal is implemented here; provider rotation and public-history rewrite are separate operator actions because they are external or destructive.
- Dependency audit, secret scan, focused package tests, feature-matrix checks, image build, and manifest validation fail closed in CI.
- The production image runs `hft-live`, contains its health-check dependency, and uses the actual `/readiness` endpoint.

## Acceptance Criteria

1. Current source tree and Docker build context contain no private key or populated credential files.
2. `cargo audit` parses project policy and returns no unacknowledged vulnerability.
3. Focused default and feature-matrix checks compile.
4. Portfolio invariant tests cover long, short, partial close, and position crossing.
5. Emergency action sends cancellation commands and rejects subsequent intents.
6. Reconciliation errors halt live execution instead of returning success or empty state.
7. A zero-signal or no-trade candidate fails evaluation.
8. Promotion cannot sign an envelope that is not bound to its persisted candidate, evaluation, dataset, bundle, and approval.
9. Paper/shadow activation loads the referenced formula or ONNX strategy before runtime build.
10. Activation is reported only after runtime start succeeds.
11. Fill/reject/cancel/PnL attribution is non-empty and tied to deployment and strategy bundle.
12. Research mission completion uses explicit criteria and persists a completion reason; overall `LoopRun` completion uses a target stage.
13. MCTS/Bayesian checkpoint-resume restores versioned search state rather than only usage counters.
14. Learning directives retain source evidence, pin an adopted child policy into the next mission, and cannot mutate runtime limits.
15. Direct resume/model/risk-increase controls cannot bypass the deployment workflow.
16. Kubernetes and container artifacts run the same `hft-live` handoff path tested locally.
17. Live-small remains fail-closed until real venue reconciliation and reduce-only exit acceptance tests pass.

## Loop Engineer Verdict

The architecture becomes a bounded Loop Engineer after criteria 7-15 pass: it then pursues explicit staged goals, evaluates its own evidence, retains failures, resumes exact search state, and changes future research behavior. It is not a self-authorizing trader. Capital authority, hard limits, emergency control, and artifact loading remain deterministic Rust responsibilities.
