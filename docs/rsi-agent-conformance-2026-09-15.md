# RSI Agent 符合性审计 / RSIAgent Conformance Audit

**Date:** 2026-09-15
**Audit baseline SHA:** `5bb8e8d800683ad8028045c3c9fcd392548cb1b1` (`main`)
**Authoritative criteria:** [@mylifcc status 2099785613973115334](https://x.com/mylifcc/status/2099785613973115334) and same-author replies `2099785616880030196` … `2099785632633803039`
**Paper named in the thread:** [arXiv:2609.15364](https://arxiv.org/abs/2609.15364) RSIAgent
**Scope:** analysis only. No RSI system is implemented in this pass.

**RSI in Monday = Recursive Self Improvement of the research / agent loop.**
Ignore trading Relative Strength Index (`RsiCalculator` in `rust_hft/strategy-framework`). Ignore collector `research_memory.rs` (process/cgroup RSS) and architecture “causal history” (PIT lagged features) unless they store condition / action / outcome knowledge — they do not.

## Verdict

**部分符合 / Partially conforms.**

- **Confidence medium-high** that Monday already has a governed explore → independently score → freeze → sealed eval → bounded reuse loop.
- **Confidence high** that Monday is **not** the thread’s training-free “Scale Experience” product: Campaigns still fit model weights; Memory is not condition/action/outcome operational knowledge.

A reader who requires every bullet below as a package should treat the verdict as **does not conform**. A reader who only asks whether Actor self-scoring and holdout pollution are already forbidden should treat those two as **has**.

---

## Post criteria (author thread)

1. Scale Experience without changing model weights; explore the environment, accumulate and verify experience.
2. Loop: try task → act → observe → record failure → write **validated** experience into Memory; the model is unchanged, the experience library changes.
3. Three roles: Curriculum (what to practice next), Actor (operate the env), Verifier (check correctness). Actor cannot self-score; only Verifier-passed experience enters Memory.
4. Memory stores condition / action / outcome / what fails — **not** chat logs.
5. After exploration: **FREEZE** Memory, clear task env + conversation history, then formal eval with **no** Memory writes. Memory needs versions + snapshots (avoid pollution).
6. Post-task pipeline: finish → distill → verify → write Memory → reuse.

---

## Scorecard / 逐条对照

Labels are `has` / `partial` / `missing` against the **thread** meaning, not against a renamed Monday artifact.

| # | Criterion | Monday | Why |
| --- | --- | --- | --- |
| 1 | Scale Experience; weights frozen; explore, accumulate, verify | **partial** | Exploration + verify exist. Weights **do** change during Campaign search. |
| 2 | try → act → observe → record failure → write validated Memory; model unchanged | **partial** | Failure recording and Factor Bank writes exist. Model/library both change. |
| 3 | Curriculum / Actor / Verifier; no self-score; Verifier gate into Memory | **partial** | Verifier + no self-score: **has**. Autonomous Curriculum: **missing**. |
| 4 | Memory = condition / action / outcome / what fails, not chat logs | **missing** | No ACO Memory product. Factor Bank is AST + metrics. Chat logs are not the store either. |
| 5 | Freeze Memory, clear env/history, eval with no writes; versions + snapshots | **partial** | Freeze / holdout isolation / content-addressed snapshots: **has**. Clearing conversation Memory: N/A (no that Memory). Eval freeze is model/input freeze, not experience-library freeze. |
| 6 | finish → distill → verify → write Memory → reuse | **partial** | Bounded `campaign-learn` / `mission learn`. Not after every task; not ACO Memory. |

### 1. Scale Experience without changing model weights — **partial**

**Has (explore / accumulate / verify, in the research domain):**

- Search explores formula/model space: GP, Factor-Bank subset MCTS, Bayesian (`rust_hft/alpha-harness/README.md`, `rust_hft/ARCHITECTURE.md`).
- Attempts accumulate as `CexFactorScreeningAttemptV2`; only `Accepted` become `CexFactorBankEntryV1` (`rust_hft/alpha-harness/domain/src/lib.rs`, `build_factor_bank` in `rust_hft/alpha-harness/app/src/mission_runner.rs`).
- Independent evaluator; generators get label-free proposal context (`docs/superpowers/specs/2026-07-11-loop-engineer-production-hardening-design.md`, `rust_hft/alpha-harness/README.md`).

**Missing vs the thread:**

- Pre-holdout Campaigns **train** Ridge / CART / Burn MLP and bind portable weights (`docs/architecture/RUST_ONLY_RESEARCH.md`). That is Scale Model inside a loop, not “模型权重一行不改”.
- Final evaluation “freezes the already fitted winner’s actual model weights … it does not retrain or refit” (`docs/architecture/RUST_ONLY_RESEARCH.md`) — exam hygiene **after** training, not Scale Experience instead of training.
- Environment is LOB/feature research under a signed plan, not an agent practicing a new tool/UI world (`docs/research/CAMPAIGN_WORKFLOW.md`).

### 2. try → act → observe → record failure → validated Memory; model unchanged — **partial**

**Has:**

- GP iterations try candidates, evaluator observes metrics, rejections record `CexFactorRejectionCodeV1` (`CoverageGateFailed`, `PredictiveGateFailed`, `TradingGateFailed`, …) (`rust_hft/alpha-harness/domain/src/lib.rs`).
- Discarded evaluations still enter the atomic Factor Bank as rejected attempts (`mission_runner.rs` tests).
- Repeated failures → `LearningDirective` + follow-up (`rust_hft/alpha-harness/engine/src/learning.rs`, `CexCampaignLearningDirectiveV1` in `rust_hft/alpha-harness/app/src/mission_render.rs`).

**Missing:**

- The thread’s invariant “变的是经验库，不是模型” fails: Campaign search **fits weights** (`docs/architecture/RUST_ONLY_RESEARCH.md`).
- “Memory” here is Factor Bank / policy revision, not a validated experience library of the thread’s kind (see §4).

### 3. Curriculum / Actor / Verifier; Actor cannot self-score — **partial**

| Role | Thread | Monday | Label |
| --- | --- | --- | --- |
| Curriculum | decides what to practice next | Signed `research-plan`, grant, `allowed_search_policy_revisions`; `campaign-learn` only emits a failure-class delta from that allowlist | **partial** (human/grant Curriculum, not an autonomous role) |
| Actor | operates the environment | GP / MCTS / Bayesian / optional LLM proposer emit candidates; no order path | **partial** (operates search, not a live env) |
| Verifier | checks env, artifacts, results | Deterministic evaluator + Factor Bank gates + independent readback; Job Complete is not completion | **has** |

**Has (no self-score; Verifier gate):**

- “Candidate generators receive a label-free proposal context; validation labels remain evaluator-only” (`docs/superpowers/specs/2026-07-11-loop-engineer-production-hardening-design.md`).
- Production seam is `campaign-freeze` → `campaign-finalize` → `dispatch submit` → generated `campaign-execute`. Direct `mission execute` / `loop run` are diagnostics (`AGENTS.md`, `rust_hft/alpha-harness/README.md`).
- Coordinator cannot issue a grant, open holdout, or authorize trading (`docs/research/CAMPAIGN_WORKFLOW.md`).
- Only `Accepted` screening attempts become Factor Bank entries (`CexFactorBankRevisionV2::new` in `rust_hft/alpha-harness/domain/src/lib.rs`).

**Missing:**

- No agent role that autonomously chooses the next practice task. H1 “cannot automatically change the entry policy or create H2/H3” (`docs/research/HOLD_TO_HORIZON_CONTRACT.md`). Holding comparisons and MLP paired diagnostics refuse automatic follow-up (`rust_hft/alpha-harness/app/src/mission_campaign.rs`).

### 4. Memory stores condition / action / outcome / what fails — **missing**

Thread Memory is operational knowledge: 「什么条件下」「做什么动作」「会产生什么结果」「什么方法会失败」. Not chat logs.

Monday stores:

- Factor AST + orientation + source features + screening/evaluation evidence (`CexFactorBankEntryV1`, `CexFactorScreeningAttemptV2` in `rust_hft/alpha-harness/domain/src/lib.rs`).
- Append-only `{event_id, mission_id, payload, created_at}` (`MemoryRecord` in `rust_hft/alpha-harness/store/src/lib.rs`) — checkpoint/learning events, not ACO triples.
- Learning directives bind parent hashes, `failure_class`, rollback and next search-policy revision (`rust_hft/alpha-harness/app/src/mission_render.rs`) — policy lineage.

There is **no** typed condition / action / outcome / failure-method record, no applicability predicate, no invalidation condition. Chat logs are also not the Memory store. Do not rename Factor Bank or `research_memory` into this product.

### 5. Freeze Memory, clear env/history, eval with no writes; versions + snapshots — **partial**

**Has (pollution control and versioning, mapped onto research freeze/holdout):**

- `campaign-freeze` writes `cex-campaign-freeze-v1` with input hashes, image identity, holdout id (`freeze` in `rust_hft/alpha-harness/app/src/mission_campaign.rs`).
- Search cannot open sealed holdout. `independent_selection_withheld` means the window is reserved, not evaluated (`docs/architecture/RUST_ONLY_RESEARCH.md`, `rust_hft/alpha-harness/domain/src/campaign_control.rs`).
- Final evaluation is a second authorization on a closed family: freeze accepts no seed/research plan, does not read reserved selection or sealed rows, does not retrain; holdout claim is create-once (`docs/architecture/RUST_ONLY_RESEARCH.md`).
- H1 calendar commits develop / validation / sealed-test **before** label inspection (`docs/research/HOLD_TO_HORIZON_CONTRACT.md`).
- Factor Bank `revision_id`, freeze plans, ledgers, preparation snapshots are content-addressed / append-only (`docs/research/CAMPAIGN_WORKFLOW.md`, `docs/research/CAMPAIGN_PREPARATION_REUSE.md`).

**Missing vs the thread:**

- Freeze is **inputs / fitted weights / holdout**, not a frozen **experience Memory** that the Actor wrote during exploration.
- There is no conversation-history Memory to clear. Eval “no Memory writes” is approximated by “no holdout writes / no retraining”, not “Memory frozen then exam”.

### 6. finish → distill → verify → write Memory → reuse — **partial**

**Has:**

- `mission campaign-learn`: terminal negative result → classify `failure_class` → typed learning directive + next plan, or `NoImprovement` / `FixedComparisonComplete` (`learn` in `rust_hft/alpha-harness/app/src/mission_campaign.rs`).
- `mission learn` / `close_learning_loop`: repeated failures → one idempotent follow-up + `LearningDirective`; child search policy adopted only after deterministic validation (`rust_hft/alpha-harness/engine/src/learning.rs`, `rust_hft/alpha-harness/README.md`).
- Runtime attribution is signed; research may open a directive from verified events and must not mutate runtime caps (`docs/architecture/governance-contract-inventory.md`, `rust_hft/ARCHITECTURE.md`).
- Factor Bank accepted entries are reused by later supervised / subset search in the **same** Campaign lineage (`mission_runner.rs`).

**Missing:**

- Distillation is not the default after every task; it is bounded, often operator-triggered, and refused for holding comparisons and paired MLP (`mission_campaign.rs`).
- Reuse target is search-policy delta / Factor Bank AST, not Verifier-passed ACO Memory.
- Optional `--llm-critic` explains failures; it is not a Memory writer (`rust_hft/alpha-harness/README.md`).
- The 2026-07-08 harness loop that would rewrite prompts / evaluators / memory retrieval is **superseded** (`docs/superpowers/specs/2026-07-08-agentic-alpha-harness-mvp-design.md`). Do not cite it as current RSI.

---

## Gaps

1. Object of improvement differs: thread = frozen model + growing verified experience; Monday = factors, search policy, **and fitted weights** under grants.
2. No condition/action/outcome Memory (bullet 4 is **missing**). Name collisions (`research_memory`, trading RSI) will keep producing false hits.
3. Curriculum is grant/plan, not an autonomous “what to practice next” role.
4. Freeze/holdout is strong exam hygiene, but it freezes research identities, not an exploration Memory snapshot.
5. Post-task distill→reuse exists only as bounded learning directives, not as the thread’s default pipeline.

---

## Top 5 next steps (no speculative rewrite)

1. Keep the term note in agent/domain docs: Monday RSI ≠ trading RSI; collector `research_memory` ≠ experience Memory; Factor Bank ≠ RSIAgent Memory.
2. Map three layers onto **existing** artifacts only: GP/MCTS traces = raw trajectories; `CexFactorScreeningAttemptV2` = candidates; Factor Bank entries + `LearningDirective` = verified long-term. List missing ACO / invalidation fields as a schema gap.
3. If ACO knowledge is wanted, add a research-only typed record bound to the same dataset/protocol hashes as Factor Bank, written only after evaluator verdict, never during sealed holdout. Actor/LLM proposer must not insert the row.
4. Keep Curriculum fail-closed: any “next practice” proposer still goes through freeze / grant / holdout isolation. Preserve existing `campaign-learn` refusals (holding, MLP follow-up, no holdout open).
5. Smallest C1 test: reuse a **frozen** Factor Bank + **frozen** weights on a new calendar versus refitting. Do not claim thread conformance until that Campaign evidence exists.

---

## Method

- Criteria are the six bullets above, taken from the `@mylifcc` thread (not third-party replies). Paper used only to name RSIAgent.
- Paths cited were read at baseline `5bb8e8d8`. This file is Code-on-branch evidence, not CI / merge / runtime / ACK Campaign result.
