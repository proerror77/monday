# ploy-sidecar

NBA comeback research sidecar and deployment-aware operator client powered by the Claude Agent SDK.

It orchestrates a research loop every few minutes while reading the new trading-platform control plane exposed by `ployd`.

## Architecture

```
Claude Sidecar (Sonnet/Opus)
├── espn MCP       → Live NBA scores, quarter, clock
├── polymarket MCP → Market search and snapshots
├── WebSearch      → X.com sentiment, injuries, momentum
└── ploy-backend MCP
    ├── get_system_status    → ployd health + uptime
    ├── get_trading_state    → canonical trading snapshots
    ├── list/get_deployment  → deployment resources
    ├── apply/control deployment resources
    └── submit_paper_intent  → paper-only intent ingress
```

The sidecar does research and operator inspection. The platform control plane owns deployment lifecycle and trading state.

## Installation

```sh
cd ploy-sidecar
npm install
```

Requires Node.js 22+.

## Configuration

Copy `.env.example` to `.env`:

```sh
cp .env.example .env
```

For MiniMax, you can start from:

```sh
cp .env.minimax.example .env
```

| Variable | Default | Description |
|---|---|---|
| `ANTHROPIC_API_KEY` | — | Anthropic-compatible API key (required) |
| `ANTHROPIC_BASE_URL` | — | Optional Anthropic-compatible base URL (MiniMax examples: `https://api.minimaxi.com/anthropic` or `https://api.minimax.io/anthropic`) |
| `ANTHROPIC_CUSTOM_HEADERS` | — | Optional custom headers, one per line in `Header: Value` format (example: `Authorization: Bearer <key>`) |
| `MINIMAX_ANTHROPIC_MODEL` | `MiniMax-M2.5` | Optional MiniMax model id used for automatic alias mapping when `ANTHROPIC_BASE_URL` points to MiniMax |
| `PLOY_API_URL` | `http://localhost:8081` | `ployd` control-plane URL |
| `PLOY_API_KEY` | — | Bearer token (optional) |
| `PLOY_API_ADMIN_TOKEN` | — | Admin token for deployment apply/control requests |
| `PLOY_SIDECAR_AUTH_TOKEN` | — | Optional sidecar token header for control-plane requests |
| `SIDECAR_MODEL` | `sonnet` | Model name or alias (`sonnet`, `opus`, `haiku`, or a full model id like `claude-opus-4-6` / `MiniMax-M2.5`) |
| `SIDECAR_POLL_INTERVAL_SECS` | `300` | Scan interval (seconds) |
| `SIDECAR_MAX_BUDGET_USD` | `1.00` | Max Claude cost per scan cycle |
| `SIDECAR_DRY_RUN` | `true` | Keep the sidecar in recommendation-only mode |

## MiniMax M2.5 (Anthropic-Compatible)

If you want to use **MiniMax M2.5** via their **Anthropic-compatible** endpoint (instead of Claude models), set:

```sh
export ANTHROPIC_BASE_URL="https://api.minimaxi.com/anthropic"
export ANTHROPIC_API_KEY="YOUR_MINIMAX_API_KEY"
export SIDECAR_MODEL="MiniMax-M2.5"
# Optional for Anthropic-compatible providers that require explicit Authorization:
export ANTHROPIC_CUSTOM_HEADERS=$'Authorization: Bearer YOUR_MINIMAX_API_KEY'
```

When `ANTHROPIC_BASE_URL` points to MiniMax, the sidecar now auto-applies:

- `Authorization: Bearer ...` header (if `ANTHROPIC_CUSTOM_HEADERS` is unset)
- alias mapping for `opus` / `sonnet` / `haiku` to `MINIMAX_ANTHROPIC_MODEL` (default `MiniMax-M2.5`)

If you get `invalid api key` on one MiniMax domain, switch to the other domain above (accounts are often region-bound).

If you prefer to keep using model aliases like `opus` in configs, you can also map Claude aliases to MiniMax by setting:

```sh
export ANTHROPIC_DEFAULT_OPUS_MODEL="MiniMax-M2.5"
export ANTHROPIC_DEFAULT_SONNET_MODEL="MiniMax-M2.5"   # optional
export ANTHROPIC_DEFAULT_HAIKU_MODEL="MiniMax-M2.5"    # optional
```

## Usage

Start the platform daemon first:

```sh
# Terminal 1 — trading-platform daemon
cargo run -p ployd
cargo run -p ployctl -- system status
```

Then start the sidecar:

```sh
# Terminal 2 — TypeScript sidecar
npm run dev
```

### Development mode (dry-run, verbose output)

```sh
SIDECAR_DRY_RUN=true SIDECAR_POLL_INTERVAL_SECS=60 npm run dev
```

### Production

```sh
npm run build
node dist/index.js
```

## Decision Pipeline

Each scan cycle:

1. **Platform inspection** — fetch `/api/system/status`, `/api/deployments`, and `/api/trading/state`
2. **ESPN scan** — fetch today's live NBA games
3. **Filter** — Q3 or early Q4 games with 1–15 point deficit
4. **Market lookup** — find corresponding Polymarket market
5. **Risk check** — reward-to-risk ≥ 4x (price ≤ $0.20), EV ≥ 5%
6. **X.com research** — injuries, momentum, betting sentiment via WebSearch
7. **Recommendation** — emit research findings plus any deployment-resource action actually taken

## Control-Plane Endpoints

The sidecar now aligns to the new trading-platform operator surface:

```
GET  /api/system/status             Platform health snapshot
GET  /api/trading/state             Canonical trading-state snapshots
GET  /api/deployments               Deployment summaries
GET  /api/deployments/:id           Deployment resource details
PUT  /api/deployments/:id           Apply/update a deployment resource
POST /api/deployments/:id/control   Set desired_state = running|paused|stopped
POST /api/deployments/:id/intents   Submit a paper trading intent
```

Legacy `/api/sidecar/*`, `/api/config`, and `enable/disable` deployment mutations are not part of the default workspace control plane on this branch.

## Output

Each scan produces structured JSON:

```json
{
  "scan_summary": { "games_scanned": 4, "in_progress_games": 2, "comeback_candidates": 1 },
  "opportunities": [
    {
      "trailing_team": "LAL",
      "deficit": 8,
      "quarter": 3,
      "market_price": 0.18,
      "reward_risk_ratio": 4.56,
      "expected_value": 0.07,
      "action": "TRADE",
      "grok_decision": "trade",
      "confidence": "high",
      "reasoning": "..."
    }
  ],
  "operator_actions": [
    {
      "kind": "deployment_control",
      "target": "example.paper",
      "status": "not_needed",
      "details": "paper deployment already running"
    }
  ]
}
```

## File Structure

```
ploy-sidecar/
├── src/
│   ├── index.ts              Main loop (Claude Commander)
│   ├── tools/
│   │   ├── espn.ts           ESPN MCP server
│   │   ├── polymarket.ts     Polymarket MCP server
│   │   └── ploy-backend.ts   Ploy Rust backend MCP server
│   ├── schemas/
│   │   └── output.ts         Structured output JSON schema
├── .env.example
└── package.json
```
