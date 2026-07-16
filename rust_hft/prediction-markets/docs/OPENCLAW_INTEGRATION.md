# OpenClaw integration inside Monday

OpenClaw is a research and operator-assistance surface for PLOY. It is not an
execution authority and must not control a former standalone trading host.

## Authority boundary

- Monday `rust_hft` owns production risk, OMS, reconciliation, cancellation,
  replacement, and execution.
- PLOY live execution is disabled by the production daemon gateway.
- OpenClaw may collect evidence, inspect read-only snapshots, and propose typed
  candidates for a separately reviewed Monday handoff.
- OpenClaw must not deploy, resume, start, stop, submit, cancel, replace, or call
  retired `ploy rpc` write methods.

## Retained compatibility example

`examples/openclaw/skill-ploy-rpc` is a read-only compatibility example for querying
explicitly allowlisted methods on a former PLOY host. Its wrappers reject every
unlisted RPC method and every remote-control mutation before opening SSH.

Supported examples:

```bash
./bin/ployctl status
./bin/ployctl logs 200
./bin/ployrpc system.describe
./bin/ployrpc pm.search_markets '{"query":"example"}'
./bin/ployrpc pm.get_event_details '{"market_id":"example"}'
./bin/ployrpc pm.get_order_book '{"token_id":"123"}'
```

The former RSS/Atom feed-ingestion helper was removed in the Rust-only
consolidation. This compatibility example currently provides no feed-ingestion
command. Restoring that capability requires a typed Rust collector with source,
clock, freshness, and evidence contracts; `ployctl` and `ployrpc` do not silently
fall back to another runtime.

## Current development direction

New agent integrations should consume Monday-approved read models or typed research
contracts. Any future write-capable handoff requires a dedicated architecture and
security review in Monday; it cannot be enabled by an environment flag in PLOY.

The nested PLOY deployment workflows and standalone host runbooks are historical
source material and are not active Monday entrypoints.
