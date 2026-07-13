# Strategy Registry

Authoritative index of all trading strategies. Each strategy has a unique
numeric ID used as filename prefix.

## Config Format Migration

**Two incompatible config formats exist.** DO NOT mix them.

| Format | Used By | Schema | Example |
|--------|---------|--------|---------|
| **Legacy** | Old binary on tango-1-1 | `[entry]`/`[timing]`/`[risk]`, values ÷100 | `02-pm5d.dryrun.toml` |
| **Unified** | New `StrategyRuntime` | `[runtime]`/`[strategy]`/`[execution]`, raw values | `02-pm5d.unified.toml` |

**Key differences that cause silent failures if mixed:**
- `min_edge`: legacy `5.0` (÷100→0.05) vs unified `0.05` (raw)
- `max_entry`: legacy `85.0` (÷100→0.85) vs unified `max_entry_price = 0.85`
- `[timing]` section: legacy has it, unified puts fields in `[strategy]`
- `[risk].shares`: legacy, vs unified `[strategy].quantity`

**Migration plan:**
1. Deploy new binary with StrategyRuntime to tango-1-1
2. Switch services to use `*.unified.toml` configs
3. Verify dry-run signal rate matches backtest (~87/day)
4. Delete all legacy configs (marked `LEGACY` below)

---

## Naming Convention

```
{NN}-{strategy-name}.{mode}.toml      — legacy format
{NN}-{strategy-name}.unified.toml     — new StrategyRuntime format
```

**Modes**: `unified` (all modes via `[runtime].mode`), `default`, `dryrun`,
`live`, `canary`, `backtest`, `template`

---

## Active Strategies

| ID | Strategy | Domain | Description |
|----|----------|--------|-------------|
| 01 | momentum | Crypto | CEX lag momentum — buys PM side predicted to win when Binance moves |
| 02 | pm5-directional | Crypto | Log-normal P(S_T>=S_0) on 5-min binary options via Binance oracle |
| 02b | pm5d-three-layer | Crypto | Three-gate directional: direction → confirmation → worth-it |
| 03 | pattern-memory | Crypto | Bayesian beta-posterior on kline return pattern similarity |
| 04 | staggered-arb | Crypto | Time-separated two-leg arb (Leg1 + hedge Leg2, sum < $1) |
| 05 | split-arb | Crypto | Simultaneous YES+NO when combined < $1 |
| 06 | gamma-scalping | Crypto | Delta-neutral straddle, rebalance on vol |
| 07 | liquidity-vacuum | Crypto | Mean-reversion against panic flow (disabled template) |

## Code-Only Strategies (no config files yet)

| ID | Strategy | Domain | Description |
|----|----------|--------|-------------|
| 08 | volatility-arb | Crypto | IV vs realized vol edge (Black-Scholes) |
| 09 | deribit-prob-arb | Cross-exch | Deribit-implied P(UP) vs Polymarket |
| 10 | crypto-lob-ml | Crypto | Order book ML feature inference |
| 11 | crypto-rl-policy | Crypto | RL policy network (requires `rl` feature) |
| 12 | dump-hedge | Crypto | Panic dump detection + progressive hedge |
| 13 | nba-comeback | Sports | NBA Q3-Q4 comeback win probability |
| 14 | event-edge | Events | Arena/data-source mispricing scanner |

---

## Config Files

### S01 — Momentum
| File | Format | Notes |
|------|--------|-------|
| `01-momentum.default.toml` | LEGACY | 4 symbols, 3 shares, predictive |
| `01-momentum.live-aws.toml` | LEGACY | 3 symbols, 4 shares, confirmatory |

### S02 — PM 5-Min Directional
| File | Format | Notes |
|------|--------|-------|
| `02-pm5d.unified.toml` | **UNIFIED** | All modes via `[runtime].mode`, backtest-aligned params |
| `02-pm5d.dryrun.toml` | LEGACY — DELETE after migration | Legacy adapter format, deployed on tango-1-1 |
| `02-pm5d.live.toml` | LEGACY — DELETE after migration | Legacy `name="momentum"` with `directional_mode` |
| `02-pm5d.canary.toml` | LEGACY — DELETE after migration | BTC only variant |
| `02-pm5d.backtest-relaxed.toml` | LEGACY — DELETE after migration | Relaxed OBI (3%), loose timing |

### S02b — PM 5-Min Three-Layer Directional
| File | Format | Notes |
|------|--------|-------|
| `02-pm5d-threelayer.unified.toml` | **UNIFIED** | Three-gate entry filter on top of directional |
| `02-pm5d-threelayer.live.toml` | **UNIFIED** | Live twin of unified dry-run config; only `[runtime].mode` differs |
| `02-pm5d-threelayer.champion-dryrun.toml` | **UNIFIED** | Snapshot-profile dry-run candidate; profile `champion` |
| `02-pm5d-threelayer.obi-soft-dryrun.toml` | **UNIFIED** | Snapshot-profile dry-run candidate; profile `obi_soft` |
| `02-pm5d-threelayer.obi-hard-dryrun.toml` | **UNIFIED** | Snapshot-profile dry-run candidate; profile `obi_hard` with hard OBI confirmation |
| `02-pm5d-threelayer.continuation-soft-dryrun.toml` | **UNIFIED** | Holistic snapshot-profile dry-run candidate; profile `continuation_soft` |
| `02-pm5d-threelayer.repricing-momentum-dryrun.toml` | **UNIFIED** | BTC/ETH/SOL-only full-depth dry-run candidate; profile `repricing_momentum` |
| `02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml` | **UNIFIED** | BTC/ETH-only AutoFactor dry-run handoff candidate; profile `settlement_probability`, runtime score `mcts_mcts_spread_adjusted_external_move_select_entry_price_quality_ge_025_select_entry_capacity_ge_025`, entry score `0.10` |

### S03 — Pattern Memory
| File | Format | Notes |
|------|--------|-------|
| `03-pattern-memory.default.toml` | LEGACY | 4 symbols (BTC/ETH/SOL/XRP), corr 0.70 |

### S04 — Staggered Arb
| File | Format | Notes |
|------|--------|-------|
| `04-staggered-arb.live.toml` | LEGACY | 3 symbols, 20 shares, sum<0.92 |

### S05 — Split Arb
| File | Format | Notes |
|------|--------|-------|
| `05-split-arb.default.toml` | LEGACY | 50 shares, target sum 98c |

### S06 — Gamma Scalping
| File | Format | Notes |
|------|--------|-------|
| `06-gamma-scalping.default.toml` | LEGACY | 3 symbols, $1/leg |

### S07 — Liquidity Vacuum
| File | Format | Notes |
|------|--------|-------|
| `07-liquidity-vacuum.template.toml` | LEGACY | Disabled, reference only |
