---
name: ploy-rpc
description: Remote-control a ploy trading machine over SSH (JSON-RPC tools for research + trading).
metadata:
  {
    "openclaw": { "emoji": "📈", "requires": { "anyBins": ["ssh"] } },
  }
---

# ploy-rpc (OpenClaw skill)

This skill provides simple command wrappers around a remote `ploy` instance:

- `ployctl status|start|stop|logs`
- `ployrpc <method> [params-json]`
- `ingest_feeds <feeds.json>` (RSS/Atom ingest + dedupe state)

It is designed to be safe with SSH forced-command allowlists.

## Required env vars

- `PLOY_TRADING_HOST` (example: `ploy@1.2.3.4`)

Optional:

- `PLOY_TRADING_SSH_OPTS` (example: `-i ~/.ssh/ploy -o StrictHostKeyChecking=yes`)

## Commands

```bash
./bin/ployctl status
./bin/ployctl start false true
./bin/ployctl logs 200
./bin/ployctl stop
```

```bash
./bin/ployrpc system.describe
./bin/ployrpc pm.search_markets '{"query":"best ai model end of february"}'
./bin/ployrpc event_edge.scan '{"title":"Which company has the best AI model end of February?"}'
```

### Multi-source discovery (RSS/Atom)

Copy `./config/feeds.example.json` to `./config/feeds.json`, edit URLs, then:

```bash
./bin/ingest_feeds ./config/feeds.json
```

## Trading (write operations)

Write ops depend on the trading machine having `PLOY_RPC_WRITE_ENABLED=true`.

Example:

```bash
./bin/ployrpc pm.submit_limit '{"token_id":"123","order_side":"BUY","shares":50,"limit_price":"0.72","market_side":"UP"}'
```
