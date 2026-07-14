# OpenClaw read-only PLOY research example

This example is retained for read-only research against a former PLOY host. It is
not a trading or deployment interface inside Monday.

Monday's `rust_hft` runtime is the only production execution authority. The wrappers
in this directory reject remote start, stop, submit, cancel, replace, and any unlisted
RPC method before opening SSH.

## Supported use

- ingest RSS/Atom sources with `bin/ingest_feeds`;
- query explicitly allowlisted market, order-book, position, and system methods;
- collect research evidence for a later typed Monday handoff.

Required environment:

- `PLOY_TRADING_HOST`, for example `ploy@1.2.3.4`;
- optional `PLOY_TRADING_SSH_OPTS`, for example
  `-i ~/.ssh/ploy -o StrictHostKeyChecking=yes`.

Examples:

```bash
./skill-ploy-rpc/bin/ployrpc system.describe
./skill-ploy-rpc/bin/ployrpc pm.search_markets '{"query":"best ai model"}'
./skill-ploy-rpc/bin/ployrpc pm.get_order_book '{"token_id":"123"}'
./skill-ploy-rpc/bin/ployctl status
./skill-ploy-rpc/bin/ployctl logs 200
```

There is no environment flag that enables writes in Monday. Historical standalone
write instructions are intentionally not reproduced here.
