# Agent Framework Architecture Design

Date: 2026-02-17

## Problem

The current trading system has independent decision paths (ESPN comeback + Grok signal) that don't share intelligence. Strategy development requires Rust coding, which slows iteration. There's no unified way for multiple AI models to collaborate on trade decisions.

## Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│              Claude Agent SDK (Commander)            │
│  ┌───────────┐  ┌───────────┐  ┌────────────────┐  │
│  │ ESPN Skill │  │ Poly Skill│  │ Grok X.com Skill│  │
│  │ (game data)│  │ (markets) │  │ (sentiment/inj) │  │
│  └─────┬─────┘  └─────┬─────┘  └───────┬────────┘  │
│        └───────────────┼────────────────┘            │
│                        ▼                             │
│              Structured Research Brief               │
│  (game state + stats + market + X.com intel +        │
│   risk metrics: RR ≥ 4x, EV, Kelly)                 │
└────────────────────────┬────────────────────────────┘
                         │
                    ❌ RR < 4x → PASS (no Grok call)
                    ✅ RR ≥ 4x ↓
                         ▼
┌─────────────────────────────────────────────────────┐
│                 Grok (Final Judge)                    │
│  Research Brief + Own X.com Search → TRADE / PASS    │
│  Returns: fair_value, own_fair_value, confidence     │
└────────────────────────┬────────────────────────────┘
                         ▼
┌─────────────────────────────────────────────────────┐
│              Rust Core (Unchanged)                   │
│  Coordinator → RiskGate → OrderExecutor → CLOB      │
└─────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────┐
│           OpenClaw Protocol Bridge (Inbound)          │
│  External OC agents ──query──→ Account/Position/     │
│                                System Status          │
└─────────────────────────────────────────────────────┘
```

## Three Roles

### 1. Claude Commander (Agent SDK Sidecar)

Orchestrates research using MCP tools (Skills). Collects data from ESPN, Polymarket, and Grok X.com search. Computes risk metrics (reward-to-risk, expected value, Kelly). Produces a structured Research Brief.

**Skills:**

| Skill | Input | Output | Source | Frequency |
|-------|-------|--------|--------|-----------|
| ESPN Game State | auto / game_id | `LiveGame[]` | ESPN API | 30s poll |
| Polymarket Markets | slug / query | `MarketSnapshot` | CLOB API | on-demand |
| Grok X.com Research | game_id + teams | `GrokGameIntel` | Grok API → X.com | 5min + on-demand |

Each Skill wraps existing Rust functions as MCP tools.

### 2. Grok Final Judge

Receives Claude's Research Brief. Does its own X.com search for double-confirmation. Returns structured verdict:

```json
{
  "decision": "trade" | "pass",
  "fair_value": 0.0-1.0,
  "own_fair_value": 0.0-1.0,
  "edge": 0.13,
  "confidence": 0.85,
  "reasoning": "...",
  "risk_factors": [...]
}
```

Key: `own_fair_value` is Grok's independent estimate. If it disagrees with the statistical model by >5%, Grok must explain why.

### 3. OpenClaw Protocol Bridge (Inbound Only)

For external OC agents to query ploy's system status:
- Account balance and positions
- Active agent states
- Recent trade history
- System health

**Not** for research or trade decisions. Pure read-only query interface.

## Reward-to-Risk Filter

Only trade when the reward-to-risk ratio meets threshold:

```
reward_risk_ratio = (1 - price) / price

Price $0.10 → 9.0x ✅
Price $0.15 → 5.7x ✅
Price $0.20 → 4.0x ✅ (threshold)
Price $0.25 → 3.0x ❌
```

**Filter placement:** Before Grok call (saves API cost).

```toml
[nba_comeback]
min_reward_risk_ratio = 4.0   # RR ≥ 4x
min_expected_value = 0.05     # EV ≥ 5%
kelly_fraction_cap = 0.25     # Kelly max 25%
```

## Asymmetric Fallback

- **ESPN path**: Falls back to rule-based when Grok is down (has its own statistical model)
- **Grok signal path**: No fallback (signal alone is insufficient without LLM confirmation)
- **Parse failure**: Always defaults to PASS (never trade on garbage)

## Implementation Status

### Phase 1: Unified Grok Decision Layer ✅

Already implemented in the Rust core:
- `grok_decision.rs`: Types, prompt builder, parser, risk metrics
- `sports.rs`: Both paths route through unified decision with reward-to-risk pre-filter
- `config.rs`: New fields for risk thresholds
- DB: `grok_unified_decisions` table with full audit trail
- 9 unit tests passing

### Phase 1.5: Kelly-Proportional Scaling-In ✅

Dynamic position sizing that adds to positions when price drops but fundamentals remain strong:
- `core.rs`: `PositionEntry`, `GamePosition` types, `record_position_entry()`, `can_scale_in()`, `kelly_scaling_shares()`
- `grok_decision.rs`: `EspnScaleIn` trigger variant with existing position context in Grok prompt
- `sports.rs`: Scaling loop in ESPN poll path, position recording on all order paths
- `config.rs`: 6 new scaling config fields (enabled, max_adds, min_price_drop, max_exposure, comeback_retention, min_time)
- 6 new unit tests (31 total NBA comeback tests passing)
- **Key**: Scale-in requires Grok LLM confirmation (no rule-based fallback) — concentrating risk demands oversight
- **Formula**: `kelly_fraction = edge / (1 - price)`, capped at 25%, target = fraction × max_game_exposure

### Phase 2: Agent SDK Sidecar (Future)

- Claude Agent SDK TypeScript/Python sidecar process
- MCP tools wrapping Rust functions via gRPC/HTTP
- Skills as composable tool collections + system prompts
- Natural language strategy definitions

### Phase 3: OpenClaw Bridge (Future)

- Read-only HTTP/WebSocket bridge
- OpenClaw-compatible message format
- Account/position/health query endpoints

## Files Modified (Phase 1)

| File | Changes |
|------|---------|
| `src/strategy/nba_comeback/grok_decision.rs` | +RiskMetrics, +own_fair_value, +risk metrics in prompt |
| `src/strategy/nba_comeback/mod.rs` | +RiskMetrics export |
| `src/agents/sports.rs` | +risk metrics pre-filter, +RR/EV/Kelly in intent metadata |
| `src/config.rs` | +min_reward_risk_ratio, +min_expected_value, +kelly_fraction_cap |
| `src/cli/strategy.rs` | +new config fields |
| `src/platform/agents/nba_agent.rs` | +new config fields |

## Files Modified (Phase 1.5 — Kelly Scaling-In)

| File | Changes |
|------|---------|
| `src/strategy/nba_comeback/core.rs` | +PositionEntry, +GamePosition, +game_positions state, +record_position_entry, +can_scale_in (5 guards), +kelly_scaling_shares, +6 tests |
| `src/strategy/nba_comeback/grok_decision.rs` | +EspnScaleIn trigger with position context in prompt |
| `src/strategy/nba_comeback/mod.rs` | +GamePosition, +PositionEntry re-exports |
| `src/agents/sports.rs` | +position recording on all order paths, +scaling-in loop in ESPN poll, +Grok-only confirmation for scale-in |
| `src/config.rs` | +scaling_enabled, +scaling_max_adds, +scaling_min_price_drop_pct, +scaling_max_game_exposure_usd, +scaling_min_comeback_retention, +scaling_min_time_remaining_mins |
| `src/cli/strategy.rs` | +scaling config defaults |
| `src/platform/agents/nba_agent.rs` | +scaling config defaults in test |

## Key Design Decisions

1. **4x minimum reward-to-risk**: Conservative threshold that limits entries to price ≤ $0.20. Natural fit for NBA comeback scenarios where trailing teams are heavily discounted.

2. **Pre-filter before LLM**: Calculating risk metrics is cheap (pure math). Filtering before Grok saves expensive API calls on low-value opportunities.

3. **Dual fair value**: Both Claude's statistical model and Grok's X.com-informed estimate are recorded. Agreement signals higher confidence; disagreement flags uncertainty.

4. **Kelly position sizing**: Caps at 25% of bankroll even when Kelly suggests more. Combined with the 4x RR filter, this creates a conservative risk envelope.

5. **Kelly scaling-in requires LLM confirmation**: Unlike initial entries (which can fall back to rule-based), scale-in has no fallback. Adding to an existing position concentrates risk, so Grok must approve every add.

6. **Five scaling guards**: max_adds (3), min_price_drop (5%), comeback_retention (70%), min_time_remaining (8 min), max_game_exposure ($50). All must pass before even querying Grok.
4. **Kelly position sizing**: Caps at 25% of bankroll even when Kelly suggests more. Combined with the 4x RR filter, this creates a conservative risk envelope.
