# RSI Agent 符合性审计 / RSIAgent Conformance Audit

**Date:** 2026-09-15
**Audit baseline SHA:** `5bb8e8d800683ad8028045c3c9fcd392548cb1b1` (`main`)
**X source:** [https://x.com/mylifcc/status/2099785613973115334](https://x.com/mylifcc/status/2099785613973115334)
**Author thread IDs:** `2099785613973115334` → `2099785616880030196` → `2099785618830365041` → `2099785621384622450` → `2099785623615963252` → `2099785625641820624` → `2099785627940376589` → `2099785630171705506` → `2099785632633803039`
**Paper cited by the author:** [arXiv:2609.15364](https://arxiv.org/abs/2609.15364) *RSIAgent: Autonomous Exploration for Recursive Self-improvement in New Environments*
**Scope:** analysis only. No RSI system was implemented in this pass.
**RSI meaning here:** Recursive Self Improvement of the research / agent loop. Trading Relative Strength Index (`RsiCalculator` in `rust_hft/strategy-framework`) is out of scope.

## Verdict

**部分符合 / Partially conforms.** Confidence: **medium-high** on architecture overlap; **high** that Monday is not the RSIAgent paper's training-free LLM explorer.

Monday already implements a governed explore → independently score → freeze → sealed evaluate → bounded reuse loop for CEX research. That loop matches several RSIAgent *system* rules (Actor cannot self-score; freeze before exam; versioned snapshots; only verified artifacts re-enter the next round). It does **not** match the paper's *product*: a training-free multi-agent that autonomously explores a new environment, writes action-condition-consequence operational memory, and improves without updating model weights.

---

## 1. Post criteria / 帖文清单

Extracted from `@mylifcc` (`lifcc`) status `2099785613973115334` and the same-author replies under that conversation. Third-party replies (`@AstroHanRay`, `@ajs6888`) are not treated as criteria. Paper abstract is used only to name two items the thread implies but does not spell out (training-free multi-agent; broad-then-deep).

| # | Criterion (EN) | 帖文要点 (ZH) |
| --- | --- | --- |
| C1 | Weights frozen. Scale Experience, not Scale Model. | 模型权重一行不改。竞争力看做过多少、记住什么、哪些经验经过验证。 |
| C2 | Give the agent an environment and let it practice first. | 给 Agent 一个环境，让它先练。 |
| C3 | Try tasks, execute, observe, record failures. | 尝试任务、执行动作、观察结果、记录失败。 |
| C4 | Write useful experience into Memory so the next similar task does not start from zero. | 有效经验写进 Memory；下次类似问题不用从零猜。变的是经验库，不是模型。 |
| C5 | Three roles: Curriculum (what to practice next), Actor (operate / complete), Verifier (check correctness). | Curriculum / Actor / Verifier。 |
| C6 | Actor cannot self-score. Verifier inspects environment, artifacts, and outcomes. | Actor 不能自己给自己打分。 |
| C7 | Only verified experience may enter Memory. | 通过检查的经验才有资格进入 Memory。 |
| C8 | Memory is operational knowledge, not chat logs: condition → action → consequence, including methods that fail. | 「什么条件下」「做什么动作」「会产生什么结果」「什么方法会失败」。 |
| C9 | After exploration, freeze Memory, clear task env and dialogue, then run formal eval. Eval must not keep writing Memory. | 探索结束冻结 Memory；评测阶段不继续改 Memory。一边考试一边写答案库会污染。 |
| C10 | Memory needs versions and snapshots. | Memory 需要版本，也需要快照。 |
| C11 | Three Memory layers: raw trajectories, candidate experience, verified long-term memory. | 原始轨迹、候选经验、验证过的长期记忆。 |
| C12 | A long-term item must carry applicability conditions, verification evidence, environment version, and invalidation conditions. | 适用条件、验证证据、环境版本、失效条件。Verifier 判错会被反复复用。 |
| C13 | Loop: task end → extract experience → verify → write Memory → reuse next time. | 任务结束 → 提炼经验 → 验证 → 写入 Memory → 下次复用。 |
| C14 | (Paper) Training-free multi-agent RSI via autonomous memory construction. | 论文：不更新参数，靠自主建记忆做递归自我改进。 |
| C15 | (Paper) Broad-then-deep exploration of environment structure, hard cases, hidden constraints. | 论文：先广后深的环境探索。 |

Author's explicit non-claim: OSWorld / Agents' Last Exam gains are partial scores; do not read the thread as "open-source model beat GPT-6".

---

## 2. What Monday already has / 仓库已有对应

Term traps (do not confuse):

- `rust_hft/tools/collector/src/research_memory.rs` observes **process/cgroup RSS**, not agent experience.
- `alpha-store` table `research_memory` is an **append-only event log** (checkpoint forks, learning records), not action-condition-consequence knowledge.
- `RsiCalculator` is a **price oscillator**. Ignore it for this audit.
- Architecture "causal evaluator / causal history" is **point-in-time lagged features**, not RSIAgent operational memory.

### C1 — frozen weights vs Scale Experience — partial / mismatch

Monday **does train**. Campaigns fit Ridge / CART / Burn MLP and bind portable weights (`docs/architecture/RUST_ONLY_RESEARCH.md`). That is Scale Model inside a governed loop, not RSIAgent's "weights unchanged".

The matching slice is **later freeze**: final evaluation "freezes the already fitted winner's actual model weights and checks their native prediction origin; it does not retrain or refit" (`docs/architecture/RUST_ONLY_RESEARCH.md`). Search policy / Factor Bank / learning directives can change **without** a new live permission class.

### C2 / C15 — environment practice / broad-then-deep — weak analog

Practice environment is **LOB/feature research**, not OSWorld. Search engines (GP, Factor-Bank subset MCTS, Bayesian) explore formula/model space under a signed plan (`rust_hft/alpha-harness/README.md`, `rust_hft/ARCHITECTURE.md`).

`gp_factor_bank_then_supervised_ml` and `gp_then_factor_bank_subset_mcts` are a **broad-then-narrow search**, not autonomous exploration of a new UI/tool environment.

Curriculum is **human-declared**: `campaign-freeze --research-plan`, signed grants, bounded `allowed_search_policy_revisions`. The coordinator "cannot issue a grant, renew an expired budget, open a holdout or authorize trading" (`docs/research/CAMPAIGN_WORKFLOW.md`).

### C3 / C7 / C11 — record failures; only verified artifacts reuse — strong analog

GP iterations become `CexFactorScreeningAttemptV2` with `Accepted` / `Rejected`, rejection codes, and optional evaluation evidence. Only `Accepted` attempts become `CexFactorBankEntryV1` (`rust_hft/alpha-harness/domain/src/lib.rs`, `build_factor_bank` in `rust_hft/alpha-harness/app/src/mission_runner.rs`).

That is two durable layers (all attempts vs accepted entries), not the post's three Memory layers. Discarded full evaluations can still enter the atomic Factor Bank as rejected attempts (tests in `mission_runner.rs`).

### C5 / C6 — Curriculum / Actor / Verifier; no self-score — strong analog on Verifier

| RSIAgent role | Monday nearest owner |
| --- | --- |
| Curriculum | Signed research plan + generation allowlist + `campaign-learn` failure-class follow-up. Not an autonomous "what to practice next" agent. |
| Actor | GP / MCTS / Bayesian / optional LLM proposer emit candidates. Label-free proposal context (`docs/superpowers/specs/2026-07-11-loop-engineer-production-hardening-design.md`, `rust_hft/alpha-harness/README.md`). |
| Verifier | Deterministic evaluator and Factor Bank screening gates. Labels are evaluator-only. Independent readback re-checks hashes; Job Complete is not completion (`docs/research/CAMPAIGN_WORKFLOW.md`). |

Loop Engineer design: "Candidate generators receive a label-free proposal context; validation labels remain evaluator-only." Production CEX seam is `campaign-freeze` → `campaign-finalize` → `dispatch submit` → generated `campaign-execute`; `mission execute` / `loop run` are diagnostics, not alternate completion (`AGENTS.md`, `rust_hft/alpha-harness/README.md`).

### C8 / C12 — operational ACO memory — gap

Factor Bank stores canonical `FactorAst`, orientation, source features, and screening/evaluation evidence bound to dataset and walk-forward identities. It does **not** store 「条件 → 动作 → 后果」 triples, applicability predicates, or invalidation conditions.

`alpha-store::MemoryRecord` is `{event_id, mission_id, payload, created_at}` (`rust_hft/alpha-harness/store/src/lib.rs`). Learning directives bind parent campaign hashes, `failure_class`, rollback policy, and next search-policy revision (`CexCampaignLearningDirectiveV1` in `rust_hft/alpha-harness/app/src/mission_render.rs`). That is **policy lineage**, not environment-operational knowledge.

### C9 / C10 — freeze, then exam; versions / snapshots — strong analog

- `campaign-freeze` writes `cex-campaign-freeze-v1` with content-addressed inputs, image identity, holdout id (`rust_hft/alpha-harness/app/src/mission_campaign.rs`).
- Pre-holdout search cannot open sealed holdout. `independent_selection_withheld` means the selection window is reserved, not evaluated (`docs/architecture/RUST_ONLY_RESEARCH.md`, `rust_hft/alpha-harness/domain/src/campaign_control.rs`).
- Final evaluation is a **second authorization** on a closed family. Freeze for final eval accepts no seed/research plan and does not read reserved selection or sealed rows. Worker does not retrain. Holdout claim is create-once (`docs/architecture/RUST_ONLY_RESEARCH.md`).
- H1 calendar commits develop / independent validation / sealed-test **before label inspection** (`docs/research/HOLD_TO_HORIZON_CONTRACT.md`).
- Factor Bank revisions, strategy bundles, ledgers, and preparation snapshots are content-addressed / append-only.

This is the closest structural match to "freeze Memory, clear exam contamination, version the snapshot."

### C4 / C13 — extract → verify → write → reuse — partial

Implemented, but **bounded and often operator-triggered**, not "every task autonomously distills experience":

- `mission campaign-learn` classifies a **negative** parent result, emits a typed learning directive and next research plan, or stops (`FollowUp` / `NoImprovement` / `FixedComparisonComplete`). Holding comparisons and MLP paired diagnostics refuse automatic follow-up (`rust_hft/alpha-harness/app/src/mission_campaign.rs`).
- `mission learn` / `close_learning_loop` clusters repeated failures, writes `LearningDirective`, creates one idempotent follow-up, and may pin an adopted child search policy after deterministic validation (`rust_hft/alpha-harness/engine/src/learning.rs`). Optional `--llm-critic` is a bounded explanation, not a Memory writer.
- Runtime attribution is signed, append-only; research may open a learning directive from verified events and **must not** mutate runtime caps (`docs/architecture/governance-contract-inventory.md`, `rust_hft/ARCHITECTURE.md`).
- Workflow recovery reuses completed members and fail-closes on identity drift; it does **not** add an identical automatic retry loop (`docs/research/CAMPAIGN_WORKFLOW.md`).

Loop Engineer design called this a durable RSI-of-the-loop: "Repeated failures create immutable learning directives and bounded follow-up missions" (`docs/superpowers/specs/2026-07-11-loop-engineer-production-hardening-design.md`). The 2026-07-08 "Harness Self-Improvement Loop" that would rewrite prompts / evaluators / memory retrieval **after regression** is marked superseded and is not the production CEX Campaign seam.

### C14 — training-free autonomous multi-agent — does not match

Monday's production improvement path updates **fitted models and/or search-policy revisions** under grants. LLM is a bounded proposer/critic, not Curriculum+Actor+Verifier exploring a new environment. Direct `loop run` is diagnostic.

---

## 3. Criteria scorecard / 对照表

| ID | RSIAgent rule | Monday | Evidence |
| --- | --- | --- | --- |
| C1 | No weight updates | **Mismatch** (train in search; freeze only at final eval) | `RUST_ONLY_RESEARCH.md` |
| C2 | Autonomous env practice | **Weak analog** (signed Campaign over market data) | `CAMPAIGN_WORKFLOW.md`, `alpha-harness/README.md` |
| C3 | Record failures | **Has** | Factor Bank attempts; `LearningDirective` |
| C4 | Reuse Memory next time | **Partial** (Factor Bank + pinned search policy) | `mission_runner.rs`, `mission_campaign.rs` `learn` |
| C5 | Curriculum / Actor / Verifier | **Partial** (Verifier yes; Curriculum human-signed) | `ARCHITECTURE.md`, research plans |
| C6 | Actor cannot self-score | **Has** | Label-free generators; independent evaluator / readback |
| C7 | Verify before Memory write | **Has** (for Factor Bank / promotion) | Screening verdicts; sealed holdout one-shot |
| C8 | ACO operational knowledge | **Gap** | Factor AST + metrics, not condition-action-consequence |
| C9 | Freeze Memory before exam | **Has** (research freeze / holdout isolation) | `campaign-freeze`; final-evaluation grant |
| C10 | Versions and snapshots | **Has** | Content hashes, freeze plan, ledgers, Factor Bank `revision_id` |
| C11 | Three-layer Memory | **Partial** (attempts vs entries; no raw-trajectory Memory product) | `CexFactorBankRevisionV2` |
| C12 | Applicability + invalidation | **Partial** (dataset/protocol binding; no invalidation conditions) | Factor Bank + evaluation protocol hashes |
| C13 | Distill after every task | **Partial** (bounded `campaign-learn` / `mission learn`) | `engine/src/learning.rs` |
| C14 | Training-free multi-agent RSI | **Does not match** | Burn/Ridge training; LLM not the loop authority |
| C15 | Broad-then-deep env RSI | **Weak analog** | GP then subset MCTS; not env structure discovery |

---

## 4. Gaps / 缺口

1. **Different object of improvement.** RSIAgent improves a frozen LLM via verified environmental memory. Monday improves **factors, search policy, and fitted models** under grants. Calling Monday "already RSIAgent" collapses those objects.
2. **No ACO Memory product.** Nothing in-tree stores reusable 「条件 / 动作 / 后果 / 失败方法」 with invalidation conditions. Do not rename RSS `research_memory` or Factor Bank ASTs into that product.
3. **Curriculum is not autonomous.** Next practice is a signed plan or a failure-class delta from an allowlist. H1 fixed holding comparison "cannot automatically change the entry policy or create H2/H3" (`HOLD_TO_HORIZON_CONTRACT.md`).
4. **Training-free claim fails.** Pre-holdout Campaigns train. Final eval freezes weights; that is exam hygiene, not Scale Experience instead of Scale Model.
5. **Three-layer Memory is incomplete.** Attempts + accepted entries ≈ candidate + verified layers. Raw trajectories (search traces, GP lineage) exist as mission lineage, not a Memory API with env version and expiry.
6. **Harness prompt/evaluator self-rewrite** from the superseded 2026-07-08 spec is not the live CEX path. Do not cite that spec as current RSI.
7. **Name collisions** (`research_memory`, RSI oscillator, "causal" features) will keep producing false positives unless docs call them out.

---

## 5. Top 5 next steps / 若要补齐的五步

No speculative rewrite. Each step is a named mapping or a missing field on an existing contract.

1. **Publish a term note in agent/domain docs:** Monday RSI = recursive self-improvement of the research loop; collector `research_memory` = RSS; Factor Bank ≠ RSIAgent Memory; ignore `RsiCalculator`. Stops false implementation hunts.
2. **Inventory three Memory layers onto existing artifacts, without a new system:** GP/MCTS traces = raw trajectories; `CexFactorScreeningAttemptV2` = candidate experience; Factor Bank entries + `LearningDirective` = verified long-term. List missing fields per C12 (applicability, env version, invalidation) as a schema gap, not a greenfield Memory service.
3. **If ACO knowledge is desired, add a typed research-only record** bound to the same dataset/protocol hashes as Factor Bank, written only after evaluator verdict, never during sealed holdout. Do not store chat logs. Do not let the Actor (search engine / LLM proposer) insert the row.
4. **Keep Curriculum fail-closed.** Any "what to practice next" proposer must emit a candidate plan that still goes through freeze / grant / holdout isolation. Do not give `campaign-learn` authority to open holdout, change H2/H3, or train a new MLP follow-up (those refusals already exist; preserve them).
5. **Measure Scale Experience separately from Scale Model.** A follow-up experiment that *only* reuses a frozen Factor Bank + frozen weights on a new calendar, versus refitting, is the smallest test of C1. Do not claim RSIAgent conformance until that comparison exists as Campaign evidence.

---

## 6. Method / 方法与限制

- X API read of conversation `2099785613973115334` on 2026-09-15 (author thread complete in one page plus the original status). Image OCR was not used (media fetch failed); criteria come from `note_tweet` / post text.
- Paper used only as title/abstract confirmation of C14–C15. Full PDF was not required for the author's checklist.
- Repo pass: `docs/research/*`, `docs/architecture/RUST_ONLY_RESEARCH.md`, `rust_hft/ARCHITECTURE.md`, `alpha-harness` Campaign / Factor Bank / learning / store, Loop Engineer spec. Trading RSI files were opened only to confirm they are oscillators.
- This report is Code-on-branch evidence. It is not CI, merge, release, runtime, or a real ACK Campaign result.

**Limitation:** "Partially conforms" is a system-pattern judgment. A reader who requires C1+C8+C14 as a package should treat the verdict as **does not conform**. A reader who asks whether Monday already froze search, isolated holdout, and forbade self-scoring should treat C6/C7/C9/C10 as **already built**.
