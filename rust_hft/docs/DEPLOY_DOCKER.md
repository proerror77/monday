Docker Deployment (Multi‑Exchange Collector)

1) Build image (only hft-collector)
- docker buildx build --platform linux/amd64 -f apps/collector/Dockerfile -t yourrepo/hft-collector:latest apps/collector
- Push to registry (optional): docker push yourrepo/hft-collector:latest

2) Prepare target ECS hosts
- apt-get update && apt-get install -y docker.io docker-compose-plugin
- systemctl enable --now docker
- Copy ops/docker/.env.sample → /etc/hft-collector.env and fill values:
  - CH_URL, CH_DB=HTf_db_collect, CLICKHOUSE_USER, CLICKHOUSE_PASSWORD
  - Optional: SYMBOLS=BTCUSDT,ETHUSDT

3) Compose up (multi services)
- Copy ops/docker/compose.yml to /root/hft/compose.yml
- docker compose -f /root/hft/compose.yml up -d

4) Verify
- docker compose -f /root/hft/compose.yml ps
- docker compose -f /root/hft/compose.yml logs -f hft-collector-binance
- ClickHouse checks (examples):
  - SELECT now(), max(ts), dateDiff('second', max(ts), now()) AS lag_s FROM HTf_db_collect.binance_orderbook;
  - SELECT now(), max(ts), dateDiff('second', max(ts), now()) AS lag_s FROM HTf_db_collect.binance_futures_orderbook;
  - SELECT count() FROM HTf_db_collect.snapshot_books WHERE venue='binance_futures' AND ts > (toUnixTimestamp64Micro(now64(6)) - 300000000);

Notes
- Each service is one exchange; add more services by copying a block and changing EXCHANGE.
- entrypoint reads EXCHANGE, CH_URL, CH_DB, TOP_LIMIT, BATCH_SIZE, FLUSH_MS and optional SYMBOLS.
- Use host time sync on ECS to keep timestamps accurate.
