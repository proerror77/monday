# Aliyun Binance data host

The Tokyo ECS runs two public-market-data services. Neither service submits orders.

```bash
systemctl status binance-lob-archiver@spot.service
systemctl status binance-lob-archiver@usdm.service
journalctl -u 'binance-lob-archiver@*' -f
```

Each service opens bounded WebSocket shards, records every diff, fetches a REST
Top-100 snapshot, validates sequence continuity, writes replay checkpoints, compresses
hourly segments, and uploads `.jsonl.zst`, `manifest.json`, and `_SUCCESS` to OSS.
`health.json` under `/data/monday/spool/binance-lob/<market>/` reports freshness,
symbol coverage, bridge state, and gap count. A silent WebSocket shard fails after
`STALL_TIMEOUT_SECONDS`; systemd then restarts the service.
The shared pending-diff budget and 20GiB free-space watermark fail closed before
an initialization burst or OSS outage can exhaust the 2C8G host. Services restart
every six hours to refresh the active-symbol catalog.

Current coverage:

- `spot`: every active Binance spot symbol; `TRD_GRP_261` symbols are tagged as
  tokenized securities in manifests.
- `usdm`: every active Binance USD-M perpetual contract.

The July 13 full-market probe measured about 1,450 events/s and 0.46MiB/s on the
wire. With both production services synchronized, the host used about 1.2 CPU
cores and less than 1GiB of service memory. The 80% CPU quota and 3.2GiB memory
limit on each service preserve host headroom during bursts.

## Backtesting storage boundary

Use OSS raw segments as the immutable source of truth, not as the repeated query
format. Validate `_SUCCESS`, SHA-256, snapshots, and sequence-gap status, then
materialize selected partitions as Parquet. A research host caches those Parquet
files locally and uses DuckDB for filtering, joins, feature datasets, run lineage,
and result metadata. The Rust `apps/replay` / `apps/backtest` path owns order-book
replay, fees, latency, slippage, fills, and capacity simulation.

This is a top-100 LOB replay contract, not a complete full-depth book. It is
appropriate for Top-20/Top-50 imbalance and flow research. A strategy requiring
deeper market-impact modeling must collect deeper snapshots in a separately
benchmarked dataset.

ClickHouse is optional for always-on shared analytics, dashboards, and derived
realtime features. It is not required for the first backtest pipeline and should
not duplicate the complete raw OSS tape.
