# ClickHouse analytics materialization contract

`clickhouse-analytics-materializer` consumes a fully verified manifest and
emits JSONEachRow payloads for the optional analytics plane:

```text
cargo run --manifest-path rust_hft/Cargo.toml -p hft-collector --bin \
  clickhouse-analytics-materializer -- \
  --manifest /path/to/canonical-manifest.json \
  --output /path/to/analytics-plan.jsonl
```

The replay-event input is the `backtest_canonical_replay_parquet` manifest
(`binance-replay-parquet-v1`). The writer verifies the manifest bytes, source
revision, content-addressed Parquet SHA, Parquet schema, ordered sequence and
time coverage before emitting `cex_replay_events`. A partition row is emitted
to `cex_analytics_partitions` first. Its immutable identity is the schema,
venue, market, symbol and time range; a retry with the same identity and
manifest SHA is a no-op, while a different manifest SHA is a conflict.
Remote writes claim the identity as `pending` before data rows and publish a
`complete` registry row only after all three-writer inserts succeed; a retry
seeing a pending claim fails closed instead of hiding a partial materialization.

The replay manifest filename is not part of the trust boundary. A verified
cache entry produced by `binance-replay-parquet-cache-warmer` may therefore be
passed as `--manifest /path/to/cache/<manifest-sha>/canonical-manifest.json`;
the writer authenticates the manifest bytes and referenced Parquet SHA instead
of requiring a content-addressed manifest filename.

The three analytics tables are intentionally separate:

| input kind | schema | table | role |
| --- | --- | --- | --- |
| `replay-event` | `binance-replay-parquet-v1` | `cex_replay_events` | event rows for analysis only |
| `pit-feature` | `pit-feature-matrix-v2` | `cex_pit_features` | point-in-time feature rows |
| `backtest-result` | `backtest-result-metadata-v1` | `cex_backtest_results` | result metadata |

Every row carries `partition_identity`, `manifest_sha256`,
`artifact_sha256`, `source_revision`, `venue`, `market`, `symbol`,
`start_time_us`, `end_time_us`, and `schema_version` so lineage remains
queryable after the source cache is evicted.

The registry additionally stores `dataset_kind`, `row_count`,
`materialization_state` (`pending` or `complete`), and
`materialization_version` (`1` for pending, `2` for complete). The registry
is expected to be a `ReplacingMergeTree(materialization_version)` keyed by
`partition_identity`; the writer reads it with `FINAL` so the complete state
replaces the pending claim. Replay rows add
`event_time_us`, `sequence`, `event`, and `payload_json`; PIT rows add the
four availability timestamps, `features_json`, and `label`; result rows add
`result_json`. These are schema contracts for an existing ClickHouse database,
not a provisioning script.

ClickHouse is a query/materialization plane, not evidence storage and not
sequential LOB replay truth. `hft-backtest` must continue to validate and read
the canonical local Parquet partition; ClickHouse rows must never be used to
reconstruct replay order, replace raw collector evidence, or authorize live
runtime behavior. The `--clickhouse-url` option only sends already-built rows;
it does not create or provision tables.

For multimodal PIT manifests, pass `--venue` and `--market` explicitly. A
backtest-result manifest uses `backtest-result-metadata-v1` and must include a
SHA-256 `source_revision`; the result JSON is stored as metadata, never as a
replay event tape.
