---
name: regime-detector
description: Detects the current market regime (HighVol, LowVol, Trending, Ranging) from BTC price data and recommends strategy adjustments.
version: 1.0.0
user-invocable: true
metadata: {"openclaw":{"requires":{"bins":["curl","jq"]}}}
---

# Market Regime Detector

Detect the current market regime by analyzing BTC/USDT price data from Binance, then recommend strategy adjustments via governance policy updates.

## Regime Definitions

| Regime | BTC 60m Range/Avg | ADX Proxy | Recommended Crypto Mode | Kelly | Max Intent % |
|--------|-------------------|-----------|------------------------|-------|-------------|
| **HighVol** | > 2.5% | any | vol_straddle | 0.15 | 50% |
| **LowVol** | < 0.8% | < 20 | arb_only | 0.30 | 100% |
| **Trending** | 1.0-2.5% | > 25 | directional | 0.25 | 100% |
| **Ranging** | 0.8-2.5% | < 25 | arb_only | 0.20 | 75% |

## Step 1: Gather Price Data

### 60-minute klines (1m interval, 60 candles)
```bash
curl -sf 'https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=60' | jq '[.[] | {open: (.[1] | tonumber), high: (.[2] | tonumber), low: (.[3] | tonumber), close: (.[4] | tonumber), volume: (.[5] | tonumber)}]'
```

### 5-minute klines for trend detection (12 candles = 1 hour)
```bash
curl -sf 'https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=5m&limit=12' | jq '[.[] | {open: (.[1] | tonumber), close: (.[4] | tonumber), direction: (if (.[4] | tonumber) > (.[1] | tonumber) then "up" else "down" end)}]'
```

### Quick volatility summary
```bash
curl -sf 'https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=60' | jq '
  [.[].[-3] | tonumber] as $closes |
  ($closes | add / length) as $avg |
  ($closes | max) as $high |
  ($closes | min) as $low |
  {
    avg_price: ($avg | round),
    range_usd: (($high - $low) | . * 100 | round / 100),
    range_pct: ((($high - $low) / $avg * 100) | . * 100 | round / 100),
    current: ($closes | last | round),
    candles: ($closes | length)
  }
'
```

## Step 2: Compute Indicators

From the gathered data, calculate:

1. **Volatility (range_pct)**: `(high - low) / avg * 100` over 60 1m candles
2. **ADX Proxy**: Count directional consistency in 5m candles.
   - Count how many 5m candles close in the same direction as the majority
   - If > 8/12 candles same direction, ADX proxy > 25 (trending)
   - If < 7/12, ADX proxy < 25 (ranging)
3. **Trend Direction**: Net direction of 5m candles (more up = bullish, more down = bearish)

## Step 3: Classify Regime

Apply the classification rules from the table above:
- `range_pct > 2.5` → **HighVol**
- `range_pct < 0.8` → **LowVol**
- `range_pct between 0.8-2.5 AND adx_proxy > 25` → **Trending**
- `range_pct between 0.8-2.5 AND adx_proxy <= 25` → **Ranging**

## Step 4: Confirmation Rule

**IMPORTANT**: Only trigger a regime change if the SAME regime is detected on two consecutive checks. This prevents whipsawing on transient spikes.

Check memory for the previous regime reading:
- If this is the first check, record the regime but do NOT act.
- If this matches the previous reading AND differs from current governance metadata, proceed to Step 5.
- If this differs from the previous reading, record it and wait for next check.

Write to memory: `regime_reading: {regime} at {timestamp}, confirmed: {true/false}`

## Step 5: Apply Regime Policy

If regime is confirmed and different from current policy, update governance:

```bash
# First read current policy
CURRENT=$(curl -sf $PLOY_API_BASE/api/governance/policy -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN")

# Update with new regime settings
curl -sf -X PUT $PLOY_API_BASE/api/governance/policy \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{
    "block_new_intents": false,
    "blocked_domains": [],
    "max_intent_notional_usd": <FROM_REGIME_TABLE>,
    "max_total_notional_usd": <SCALED_BY_MAX_INTENT_PCT>,
    "updated_by": "openclaw.regime-detector",
    "reason": "Regime change: <OLD> → <NEW> (range_pct=X.X%, adx_proxy=Y)"
  }' | jq .
```

## Step 6: Report

Output a summary:
```
Regime: <REGIME> (confidence: <0.0-1.0>)
BTC 60m: range $X ($Y%) | avg $Z
Trend: <direction> (X/12 candles aligned)
Action: <updated governance / no change / waiting for confirmation>
```
