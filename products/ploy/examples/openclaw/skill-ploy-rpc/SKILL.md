---
name: ploy-rpc-read-only
description: Query allowlisted PLOY research and market-data methods over SSH without remote mutations.
metadata:
  {
    "openclaw": { "emoji": "🔎", "requires": { "anyBins": ["ssh"] } },
  }
---

# PLOY read-only research skill

This historical compatibility skill is restricted to research reads. Monday's
`rust_hft` runtime owns production execution; this skill cannot start or stop a
remote runtime and cannot submit, cancel, or replace orders.

## Required environment

- `PLOY_TRADING_HOST`, for example `ploy@1.2.3.4`.
- Optional `PLOY_TRADING_SSH_OPTS`, for example
  `-i ~/.ssh/ploy -o StrictHostKeyChecking=yes`.

## Commands

```bash
./bin/ployctl status
./bin/ployctl logs 200
./bin/ployrpc system.describe
./bin/ployrpc pm.search_markets '{"query":"best ai model"}'
./bin/ployrpc pm.get_event_details '{"market_id":"example"}'
./bin/ployrpc pm.get_order_book '{"token_id":"123"}'
./bin/ployrpc event_edge.scan '{"title":"Example event"}'
```

Feed ingestion remains local and non-executing:

```bash
./bin/ingest_feeds ./config/feeds.json
```

All unlisted RPC methods fail closed before SSH. Treat the output as research evidence,
not as permission to trade or mutate a host.
