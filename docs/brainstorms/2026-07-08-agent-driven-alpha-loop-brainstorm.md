# Agent-Driven Alpha Loop Engineer Brainstorm

Date: 2026-07-08

## What We're Building

Reposition the project from a high-frequency trading execution platform into an agent-driven Alpha Loop Engineer: a system that continuously collects data, generates factor hypotheses, searches candidate DSL/program factors, evaluates them, stores them as auditable assets, promotes them through paper/shadow/live gates, monitors decay, and feeds results back into research memory.

The existing Rust HFT execution core remains valuable. It should become the controlled runtime for validated signals, not the place where research agents freely mutate behavior.

## Explicit Non-Goal

Do not use OMX/autoresearch skills or OMX-style workflow strategy for this project direction. The architecture work should use the Superpowers brainstorm/plan/implementation flow and project-local docs instead.

Also do not rebuild the whole repo. The shortest useful change is to add the missing Alpha Loop layer beside the existing HFT core, then retire or rewrite old docs that frame the project as only an HFT platform.

## Current Shape

The repo currently looks like:

- Rust HFT execution core
- Market adapters and execution gateway
- Risk-control and Sentinel-style monitoring
- ML training workspace
- LOB alpha search and labeling primitives
- Backtest app and ClickHouse-based data paths

This is a usable base, but it is not yet a durable factor lifecycle system. The missing center is Factor Factory plus Factor Bank.

## Target Shape

```text
Data Collection
  LOB / trades / OI / funding / liquidation / cross-exchange / on-chain
        ↓
Feature Store
  point-in-time fields with event_time / receive_time / available_time
        ↓
Candidate Pool
  GP / MCTS / RL / Bayesian opt / LLM proposals
        ↓
Evaluator
  IC / RankIC / ICIR / PnL / cost / DD / turnover / leakage / robustness
        ↓
Factor Bank
  definitions / versions / lineage / metrics / status / regimes / decay
        ↓
Meta Model + Allocator
  factor ensemble / sizing / regime gating / bounded weights
        ↓
Rust Runtime
  inference / risk / execution / kill switch
        ↓
Live Feedback
  attribution / slippage / decay / failures / memory updates
```

## Key Decisions

1. Keep the Rust hot path. Agents do not directly mutate live execution logic.

2. Add a new Alpha Loop layer, most likely under `ml_workspace/factor_factory/` first. Move only stabilized evaluators or live scoring logic to Rust later.

3. Treat every factor as an auditable asset. A factor needs definition, AST/program representation, lineage, data version, label version, metrics, status, and promotion history.

4. Use LLM agents as research workers: write hypotheses, explain failures, generate DSL/program candidates, propose model configs, summarize memories. They may propose live weight changes, but only through deterministic gates.

5. Run MCTS/RL in Lab. MCTS searches DSL/program structure. RL searches factor collections or allocator policies. Neither should bypass evaluator, paper/shadow, risk limits, or promotion gates.

6. Make memory operational. Store successful patterns, forbidden directions, correlation clusters, regime-specific lessons, and failure explanations in a structured Research Memory, not only prose notes.

7. Version labels and data availability. No loop is trustworthy without point-in-time availability and cost-aware labels.

## Proposed Modules

```text
ml_workspace/factor_factory/
  data/
    collectors/
    feature_store.py
    availability.py
  dsl/
    ast.py
    parser.py
    operators.py
    type_checker.py
  search/
    candidate_pool.py
    genetic.py
    mcts.py
    rl_generator.py
    bayes_opt.py
    llm_proposer.py
  evaluator/
    ic.py
    pnl.py
    triple_barrier.py
    walk_forward.py
    leakage.py
    robustness.py
  registry/
    factor_bank.py
    schema.sql
    lineage.py
    promotion.py
  memory/
    research_memory.py
    success_patterns.yaml
    failure_patterns.yaml
  workflows/
    daily_factor_mining.py
    weekly_meta_model_retrain.py
    live_decay_review.py
```

## Agent Roles

- `HypothesisAgent`: reads recent results and proposes next research directions.
- `FactorGeneratorAgent`: emits DSL/program candidates from hypotheses.
- `SearchAgent`: orchestrates GP, MCTS, RL, Bayesian search, and candidate pool deduplication.
- `EvaluatorAgent`: runs deterministic evaluation and produces metrics artifacts.
- `ValidatorAgent`: rejects leakage, low sample size, overfit, single-venue bias, or high correlation.
- `FactorBankAgent`: writes factor definitions, versions, metrics, status transitions, and lineage.
- `PortfolioAgent`: tests incremental value against the existing factor library.
- `DecayMonitorAgent`: monitors live/shadow IC, PnL, slippage, and factor decay.
- `ResearchMemoryAgent`: distills lessons back into structured memory.

Start with three workflow agents rather than all roles as separate services:

- `daily_factor_mining`
- `weekly_meta_model_retrain`
- `live_decay_review`

## Factor Lifecycle

```text
generated
  → quick_test_passed
  → full_backtest_passed
  → paper_trading
  → live_shadow
  → live_small
  → live_full
  → decayed
  → retired
```

Every transition must leave an artifact. Live transitions require hard gates: OOS performance, cost-aware PnL, drawdown, turnover, correlation, regime stability, and rollback metadata.

## MCTS / RL Placement

MCTS should operate on factor AST/program nodes:

- node: partial or complete DSL/program factor
- action: add operator, mutate feature, change window, add condition, simplify subtree
- expansion: LLM or deterministic mutation proposes children
- evaluation: deterministic evaluator scores candidates
- backpropagation: reward updates ancestors through real lineage

RL should initially avoid direct live trading. Use it for:

- formulaic alpha collection generation
- factor ensemble selection
- contextual bandit-style paper/shadow allocation

## First Implementation Slice

P0 is not a grand rewrite.

1. Remove OMX/autoresearch framing from the project plan.
2. Add a top-level architecture doc that names the product as Agent-Driven Alpha Loop Engineer.
3. Create `ml_workspace/factor_factory/` skeleton with docs and schema first.
4. Define Factor Bank schema.
5. Move or wrap the existing `lob_core/alpha_search.py` as the initial GP search backend.
6. Add a minimal `daily_factor_mining` workflow that writes candidate factors and metrics to the Factor Bank.
7. Add promotion states, but keep live promotion disabled until paper/shadow evidence exists.

## Open Questions

- Which storage should be canonical for Factor Bank first: ClickHouse, DuckDB, SQLite, or Parquet plus JSONL?
- Should DSL stay Python-first until stabilized, or should Rust own the canonical AST schema immediately?
- Which data domains are P1: LOB/trades only, or LOB plus OI/funding/CVD first?
- What is the first live-safe output: model file, factor ensemble config, or bounded allocator weight proposal?

## Recommended Next Step

Run a planning pass for P0/P1 only:

- architecture doc update
- Factor Bank schema
- factor_factory package skeleton
- daily_factor_mining minimal workflow
- explicit no-live-promotion guard

Skipped: full multi-agent runtime, live auto-weighting, and RL-to-live integration. Add them after Factor Bank and paper/shadow promotion artifacts exist.
