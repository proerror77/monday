Structure V2 (collector focus)

Goals
- Single‑exchange per ECS (Bitget first), lean runtime footprint
- Minimal repo surface; logs/scripts go under scripts/ and logs/
- Clear env knobs for Bitget v2 WS (books + books1)

Layout (key dirs)
- apps/collector/src         # source code
- apps/collector/systemd     # unit templates (if any)
- scripts/                    # ops helpers (local/ECS/cleanup)
- logs/                       # local test logs
- VERSION                     # structure version marker

New scripts
- scripts/local_bitget_futures_test.sh
  Local smoke test for Bitget USDT‑Futures ingestion (books,books1,publicTrade),
  with logs and ClickHouse post checks.

- scripts/cleanup_collector_workspace.sh
  Removes large/binary artifacts (tar/chunk parts) to keep tree slim.
  DRY_RUN=1 preview, RUN=1 to delete.

Bitget runtime env (suggested)
- BITGET_MARKET=USDT-FUTURES
- BITGET_CHANNELS=books,books1,publicTrade
- SYMBOLS=BTCUSDT:USDT-FUTURES,ETHUSDT:USDT-FUTURES (or SYMBOLS_FILE)
- batch-size=500, flush-ms=3000, depth-mode=limited, depth-levels=20, lob-mode=snapshot

Pending (on confirmation)
- Move legacy deployment scripts into apps/collector/legacy/ or remove permanently
- Optional cargo clean of apps/collector/target to reduce ~400MB

