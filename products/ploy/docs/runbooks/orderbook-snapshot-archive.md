# CLOB Orderbook Snapshot Archive

## Final Storage Policy

Keep full-fidelity Polymarket CLOB orderbook snapshots, but split storage by
temperature:

- PostgreSQL `clob_orderbook_snapshots`: hot, short retention, used by live
  operations and recent diagnostics.
- `/opt/ploy/data/lake/orderbook_snapshots`: cold, complete Parquet/ZSTD
  archive, used for research/backtests and long-term retention.

The archive must not sample, coalesce, or drop fields. Each Parquet export keeps:

- `id`
- `domain`
- `token_id`
- `market`
- `bids`
- `asks`
- `book_timestamp`
- `hash`
- `source`
- `context`
- `received_at`

## Paths

Hourly archive files:

```text
/opt/ploy/data/lake/orderbook_snapshots/date=YYYY-MM-DD/hour=HH/
  snapshots.parquet
  manifest.json
  _SUCCESS
```

When all 24 hourly `_SUCCESS` markers exist, the day marker is created:

```text
/opt/ploy/data/lake/orderbook_snapshots/date=YYYY-MM-DD/_SUCCESS
```

## Services

Archive timer:

```bash
systemctl status ploy-orderbook-snapshot-archive.timer
systemctl status ploy-orderbook-snapshot-archive.service
```

Retention timer:

```bash
systemctl status ploy-orderbook-snapshot-retention.timer
systemctl status ploy-orderbook-snapshot-retention.service
```

The archive service runs the gap filler:

```bash
/opt/ploy/scripts/archive_clob_orderbook_snapshots_backfill.sh
```

The gap filler sequentially exports missing completed hours. Defaults are
bounded for `tango-1-1`:

- look back 72 hours
- export at most 6 missing hours per run
- leave a 1-hour lag so the active hour is not archived

## Safety Rules

Retention is archive-gated by default:

```bash
PLOY_CLOB_BOOK_REQUIRE_ARCHIVE=true
```

Old hot rows or partitions may be deleted only when the corresponding archive
date has a completed day marker. Do not disable this except for emergency
operator action with separate evidence that the archive is already safe.

DuckDB export is intentionally constrained:

```sql
SET threads=1;
SET pg_connection_limit=1;
SET pg_use_ctid_scan=false;
```

This avoids the PostgreSQL ctid-parallel scan path that can saturate CPU on the
trading host.

## Manual Gap Fill

Use a bounded pass first:

```bash
nice -n 10 ionice -c2 -n7 \
  env PLOY_CLOB_BOOK_ARCHIVE_LOOKBACK_HOURS=80 \
      PLOY_CLOB_BOOK_ARCHIVE_MAX_HOURS_PER_RUN=8 \
  /opt/ploy/scripts/archive_clob_orderbook_snapshots_backfill.sh
```

Repeat bounded passes until the target day has 24 hour markers and a day marker.

## Verification

Check archive completeness:

```bash
base=/opt/ploy/data/lake/orderbook_snapshots/date=YYYY-MM-DD
find "$base" -mindepth 2 -maxdepth 2 -name _SUCCESS | wc -l
test -f "$base/_SUCCESS"
duckdb -noheader -csv -c "SELECT count(*) FROM read_parquet('$base/hour=*/snapshots.parquet');"
```

Compare against the hot partition before deletion:

```bash
PGPASSWORD=postgres psql -U postgres -d ploy -tAc \
  "SELECT count(*) FROM clob_orderbook_snapshots_v2_YYYYMMDD;"
```

Dry-run retention:

```bash
PLOY_ORDERBOOK_SNAPSHOT_DRY_RUN=true \
  /opt/ploy/scripts/ploy_orderbook_snapshot_retention.sh
```

Only proceed when the archive row count matches the PostgreSQL row count and
dry-run shows `archive_complete = t` for the old partition.

Health checks after retention:

```bash
systemctl --failed --no-pager
systemctl is-active ploy-quote-collector.service ployd.service
curl -fsS http://127.0.0.1:8081/health
df -h /
sar -u 1 3
```
