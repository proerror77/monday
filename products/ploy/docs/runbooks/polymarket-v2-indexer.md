# Polymarket V2 Indexer Sidecar

> **Historical standalone PLOY operations.** Host, systemd, secret, and nested
> workflow commands below are retained as migration evidence only. They do not
> define a current Monday deployment or execution path.

This runbook integrates the Envio `polymarket-v2-indexer` as a sidecar data
source for Ploy.

The sidecar is for chain-truth reconciliation, collateral flow checks, and
research labels. It is not a realtime strategy feed. PM5D and other live
strategies must continue to use the CLOB/orderbook and internal runtime paths
for execution decisions.

## Source

- Repository: `https://github.com/enviodev/polymarket-v2-indexer`
- Checked reference during integration: `7ad70d9`
- Stack: Envio HyperIndex, TypeScript, GraphQL, PostgreSQL

Indexed contracts on Polygon:

| Surface | Address | Ploy use |
| --- | --- | --- |
| CTFExchange V2 standard | `0xe111180000d2663c0091e4f400237545b87b996b` | OrderFilled, OrdersMatched, fees |
| CTFExchange V2 neg-risk | `0xe2222d279d744050d28e00520010520000310f59` | Neg-risk order fills and matches |
| CTFExchange V2 third instance | `0xe2222d002000ba0053cef3375333610f64600036` | Track separately until venue semantics are confirmed |
| PolyUSD | `0xc011a7e12a19f7b1f670d46f03b03f3342e82dfb` | pUSD transfers, wrap, unwrap |
| Rewards | `0xdd8db71ce3be8d71ff148b2163d64da181a29e8b` | Sponsorship/rewards research, not wired by default |

## Database Boundary

Migration `040_polymarket_v2_indexer_events.sql` creates only sidecar tables:

- `polymarket_v2_order_fills`
- `polymarket_v2_order_matches`
- `polymarket_v2_fee_events`
- `polymarket_v2_polyusd_events`
- `polymarket_v2_indexer_sync_state`
- `polymarket_v2_indexer_health`

It intentionally does not alter:

- `strategy_runtime_orders`
- `strategy_runtime_fills`
- `clob_quote_ticks`
- `clob_orderbook_snapshots`

The first cut stores raw integer amounts from chain events. Do not convert those
to trading PnL in reports until the side, token, and maker/taker amount semantics
are validated against live fills.

## Run the Envio Indexer

Run this outside the Ploy daemon process. The indexer needs Node 22+, pnpm,
Docker or Podman, and an Envio HyperSync token.

```bash
git clone https://github.com/enviodev/polymarket-v2-indexer /opt/polymarket-v2-indexer
cd /opt/polymarket-v2-indexer
cp .env.example .env
# set ENVIO_API_TOKEN in .env
pnpm install
pnpm codegen
TUI_OFF=true pnpm dev
```

On `tango-1-1`, the prepared service path is:

```bash
install -m 0644 deployment/env.polymarket-v2-indexer.example /opt/polymarket-v2-indexer/.env
install -m 0644 deployment/systemd/polymarket-v2-indexer.service /etc/systemd/system/polymarket-v2-indexer.service
systemctl daemon-reload
```

Do not start the service until `/opt/polymarket-v2-indexer/.env` contains a real
`ENVIO_API_TOKEN`. The service binds the GraphQL surface to `127.0.0.1:8080` and
has a 2 GiB memory cap so it stays isolated from trading services.

The default local GraphQL playground is usually exposed on `http://127.0.0.1:8080`.
Keep it private or bind it behind an internal-only network path. Do not expose it
publicly without authentication.

## Import Into Ploy

Apply the migration first on the target Ploy database.

```bash
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f migrations/040_polymarket_v2_indexer_events.sql
```

Then import from the sidecar GraphQL endpoint:

```bash
PLOY_PM_V2_INDEXER_URL=http://127.0.0.1:8080/v1/graphql \
PLOY_DATABASE__URL=postgresql://postgres:postgres@localhost:5432/ploy \
python3 scripts/import_polymarket_v2_indexer.py --min-block 84902320
```

For periodic imports, set the private endpoint in the host env file:

```bash
install -d -m 0755 /opt/ploy/env
printf 'PLOY_PM_V2_INDEXER_URL=http://127.0.0.1:8080/v1/graphql\n' \
  > /opt/ploy/env/polymarket-v2-indexer.env
systemctl enable --now ploy-polymarket-v2-indexer-import.timer
```

The timer uses `--from-sync-state`, so subsequent runs continue from
`polymarket_v2_indexer_sync_state` instead of rescanning from the deployment
start block. If `PLOY_PM_V2_INDEXER_URL` is not configured, the service exits
successfully without importing.

For a dry-run parse without writing:

```bash
python3 scripts/import_polymarket_v2_indexer.py \
  --endpoint http://127.0.0.1:8080/v1/graphql \
  --min-block 84902320 \
  --dry-run
```

If GraphQL schema naming changes in Envio, export JSON/JSONL from the indexer
and import it without relying on the query builder:

```bash
python3 scripts/import_polymarket_v2_indexer.py --input /tmp/pm-v2-events.jsonl --dry-run
python3 scripts/import_polymarket_v2_indexer.py --input /tmp/pm-v2-events.jsonl
```

JSONL rows must include an `entity` field such as `OrderFill`, `OrderMatch`,
`FeeEvent`, `PolyUSDTransfer`, or `PolyUSDWrap`.

## Reconciliation Queries

Latest indexer cursor:

```sql
SELECT * FROM polymarket_v2_indexer_health;
```

Recent chain fills for a token:

```sql
SELECT block_timestamp, transaction_hash, side, token_id, maker_amount_raw,
       taker_amount_raw, fee_raw, builder
FROM polymarket_v2_order_fills
WHERE token_id = '<token_id>'
ORDER BY block_timestamp DESC
LIMIT 50;
```

Candidate runtime-to-chain comparison by token and time window:

```sql
SELECT
  f.fill_timestamp,
  f.strategy_id,
  f.deployment_id,
  f.token_id,
  f.quantity,
  f.price,
  c.block_timestamp AS chain_time,
  c.transaction_hash,
  c.fee_raw,
  c.builder
FROM strategy_runtime_fills f
LEFT JOIN polymarket_v2_order_fills c
  ON c.token_id = f.token_id
 AND c.block_timestamp BETWEEN f.fill_timestamp - INTERVAL '2 minutes'
                           AND f.fill_timestamp + INTERVAL '2 minutes'
WHERE f.runtime_mode NOT IN ('dry_run', 'dryrun', 'paper')
ORDER BY f.fill_timestamp DESC
LIMIT 100;
```

This join is only a first-pass audit. Use order hash or venue order IDs when the
runtime records expose them consistently.

## Deployment Notes

- Do not run the Envio indexer inside `ployd`.
- Do not make PM5D entry logic depend on indexer lag.
- Keep importer scheduling independent from dry-run/live services.
- On `tango-1-1`, deploy the importer as `ploy-polymarket-v2-indexer-import.timer`
  after confirming the indexer endpoint is local/private.
- Rewards entities are intentionally not imported in this first cut. Add them
  only when a concrete research or accounting report consumes them.
