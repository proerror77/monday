use data::binance_market_tape::{
    AggregateTrade, AggregateTradeSummaryBuilder, LobContinuitySummaryBuilder,
    AGGREGATE_TRADE_SUMMARY_CONTRACT,
};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::RowAccessor;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const BINARY: &str = env!("CARGO_BIN_EXE_binance-replay-parquet-materializer");
const START_NS: u64 = 1_783_987_200_000_000_000;
static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

fn event_ns(milliseconds: u64) -> u64 {
    START_NS + milliseconds * 1_000_000
}

#[rustfmt::skip]
fn diff(
    milliseconds: u64,
    first: u64,
    final_id: u64,
    previous: u64,
    bids: Value,
    asks: Value,
) -> Value {
    let received_at_ns = event_ns(milliseconds);
    json!({
        "schema": "binance.market_tape.v1",
        "received_at_ns": received_at_ns,
        "type": "diff",
        "session_id": "session-1",
        "frame": {"stream": "btcusdt@depth@100ms", "data": {
            "e": "depthUpdate", "E": received_at_ns / 1_000_000,
            "T": received_at_ns / 1_000_000, "s": "BTCUSDT",
            "U": first, "u": final_id, "pu": previous, "b": bids, "a": asks
        }}
    })
}

#[rustfmt::skip]
fn trade(milliseconds: u64) -> Value {
    let received_at_ns = event_ns(milliseconds);
    json!({
        "schema": "binance.market_tape.v1",
        "received_at_ns": received_at_ns,
        "type": "agg_trade",
        "session_id": "session-1",
        "frame": {"stream": "btcusdt@aggTrade", "data": {
            "e": "aggTrade", "E": received_at_ns / 1_000_000,
            "s": "BTCUSDT", "a": 10, "p": "100.5", "q": "2",
            "f": 10, "l": 10, "T": received_at_ns / 1_000_000, "m": false
        }}
    })
}

#[rustfmt::skip]
fn valid_rows() -> Vec<Value> {
    vec![
        json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(0),"type":"session_start","session_id":"session-1","market":"usdm","symbols":1,"websocket_shards":2,"websocket_streams":2}),
        json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(1),"type":"stream_coverage","session_id":"session-1","shards":[["btcusdt@aggTrade"],["btcusdt@depth@100ms"]]}),
        json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(100),"type":"snapshot","session_id":"session-1","symbol":"BTCUSDT","request_started_at_ns":event_ns(50),"snapshot":{"lastUpdateId":100,"bids":[["100.00000000","10.01000000"],["99","5"]],"asks":[["102","4"],["103","6"]]}}),
        diff(600, 101, 101, 100, json!([["100.00000000", "11.01000000"]]), json!([])),
        trade(700),
        diff(1_400, 102, 102, 101, json!([]), json!([["102", "3"]])),
        json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(1_500),"type":"checkpoint","session_id":"session-1","symbol":"BTCUSDT","last_update_id":102,"synced":true,"bridged":true,"continuity_complete":true,"stream_coverage_verified":true,"bids":[["100.00000000","11.01000000"],["99","5"]],"asks":[["102","3"],["103","6"]],"reason":"test","replay_safe":true}),
    ]
}

struct Fixture {
    directory: PathBuf,
    data: PathBuf,
    content_sha256: String,
    manifest_sha256: String,
}

impl Fixture {
    fn new(rows: &[Value]) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "binance-replay-parquet-cli-{}-{}",
            std::process::id(),
            NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let directory = fs::canonicalize(directory).unwrap();
        let data = directory.join("part-1.jsonl.zst");
        let mut encoder =
            zstd::stream::write::Encoder::new(File::create(&data).unwrap(), 3).unwrap();
        for row in rows {
            serde_json::to_writer(&mut encoder, row).unwrap();
            encoder.write_all(b"\n").unwrap();
        }
        encoder.finish().unwrap().sync_all().unwrap();

        let content_sha256 = sha256_file(&data);
        let event_types = rows.iter().fold(BTreeMap::new(), |mut counts, row| {
            *counts
                .entry(row["type"].as_str().unwrap().to_string())
                .or_insert(0_u64) += 1;
            counts
        });
        let mut trade_summaries = AggregateTradeSummaryBuilder::default();
        let mut lob_continuity = LobContinuitySummaryBuilder::new(["BTCUSDT".to_string()]).unwrap();
        for row in rows {
            let raw = row.as_object().unwrap();
            lob_continuity.observe(raw).unwrap();
            if row["type"] == "agg_trade" {
                trade_summaries
                    .observe(
                        &AggregateTrade::from_archived_event(
                            raw,
                            row["received_at_ns"].as_u64().unwrap(),
                        )
                        .unwrap(),
                    )
                    .unwrap();
            }
        }
        let checkpointed_symbols = rows
            .iter()
            .filter(|row| row["type"] == "checkpoint")
            .filter_map(|row| row["symbol"].as_str())
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let manifest_value = json!({
            "schema": "binance.market_tape.v1", "venue": "binance", "market": "usdm",
            "dataset": "usdm_all", "shard_id": "all", "mode": "diff", "symbols": ["BTCUSDT"],
            "security_token_symbols": [], "excluded_symbols": [], "snapshot_limit": 1000,
            "replay_scope": "captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs",
            "venue_depth_complete": false, "events": rows.len(), "event_types": event_types,
            "has_replay_safe_checkpoint": true, "snapshot_ready_count": checkpointed_symbols.len(),
            "bridged_count": checkpointed_symbols.len(),
            "stream_coverage_verified_count": checkpointed_symbols.len(), "snapshot_only_symbols": [],
            "all_symbols_bridged": checkpointed_symbols == BTreeSet::from(["BTCUSDT".to_string()]),
            "all_stream_coverage_verified": checkpointed_symbols == BTreeSet::from(["BTCUSDT".to_string()]),
            "start_received_at_ns": rows.first().unwrap()["received_at_ns"],
            "end_received_at_ns": rows.last().unwrap()["received_at_ns"], "date": "2026-08-03", "hour": "00",
            "file": "part-1.jsonl.zst", "bytes": data.metadata().unwrap().len(), "sha256": content_sha256,
            "trade_representation": "aggregate_trade_only", "price_surface_derivation": "latest aggregate trade price",
            "trade_summary_contract": AGGREGATE_TRADE_SUMMARY_CONTRACT,
            "trade_summaries": trade_summaries.finish().unwrap(), "lob_continuity": lob_continuity.finish().unwrap()
        });
        let mut manifest_bytes = serde_json::to_vec(&manifest_value).unwrap();
        manifest_bytes.push(b'\n');
        fs::write(sibling(&data, ".manifest.json"), &manifest_bytes).unwrap();
        fs::write(sibling(&data, "._SUCCESS"), format!("{content_sha256}\n")).unwrap();

        Self {
            directory,
            data,
            content_sha256,
            manifest_sha256: hex::encode(Sha256::digest(&manifest_bytes)),
        }
    }

    fn artifact_dir(&self) -> PathBuf {
        self.directory.join("artifacts")
    }

    fn materialize(&self, artifact_dir: &Path) -> Output {
        Command::new(BINARY)
            .arg("--mission-id")
            .arg("canonical-btc-usdm-1")
            .arg("--symbol")
            .arg("BTCUSDT")
            .arg("--market")
            .arg("usdm")
            .arg("--segment")
            .arg(&self.data)
            .arg("--segment-content-sha256")
            .arg(&self.content_sha256)
            .arg("--segment-manifest-sha256")
            .arg(&self.manifest_sha256)
            .arg("--artifact-dir")
            .arg(artifact_dir)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn cli_materializes_a_verified_triplet_into_a_content_addressed_parquet_partition() {
    let fixture = Fixture::new(&valid_rows());
    let artifact_dir = fixture.artifact_dir();
    let output = fixture.materialize(&artifact_dir);
    assert!(
        output.status.success(),
        "materializer stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let published: Value = serde_json::from_slice(&output.stdout).unwrap();
    let manifest_path = PathBuf::from(published["manifest_path"].as_str().unwrap());
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let artifact_name = manifest["artifact_path"].as_str().unwrap();
    let artifact_path = artifact_dir.join(artifact_name);
    let reader = SerializedFileReader::new(File::open(&artifact_path).unwrap()).unwrap();

    assert_eq!(manifest["format"], "parquet");
    assert_eq!(manifest["source_segments"][0]["file"], "part-1.jsonl.zst");
    assert!(manifest["source_segments"][0].get("path").is_none());
    assert!(!Path::new(artifact_name).is_absolute());
    assert_eq!(
        artifact_name,
        format!("{}.parquet", manifest["artifact_sha256"].as_str().unwrap())
    );
    assert_eq!(reader.metadata().file_metadata().num_rows(), 3);
    assert!(fs::read(&artifact_path).unwrap().starts_with(b"PAR1"));
    let rows = reader
        .get_row_iter(None)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows[0].get_long(0).unwrap(),
        START_NS as i64 / 1_000 + 100_000
    );
    assert_eq!(rows[0].get_long(1).unwrap(), 1);
    assert_eq!(rows[0].get_string(2).unwrap(), "snapshot");
    assert_eq!(rows[1].get_string(2).unwrap(), "l2_update");
    let payload: Value = serde_json::from_str(rows[0].get_string(3).unwrap()).unwrap();
    assert_eq!(payload["bids"][0], json!(["100.00000000", "10.01000000"]));

    let retry = fixture.materialize(&artifact_dir);
    assert!(retry.status.success());
    let retried: Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(retried["manifest_sha256"], published["manifest_sha256"]);

    let other_artifact_dir = fixture.directory.join("other-artifacts");
    let relocated = fixture.materialize(&other_artifact_dir);
    assert!(relocated.status.success());
    let relocated: Value = serde_json::from_slice(&relocated.stdout).unwrap();
    assert_eq!(relocated["manifest_sha256"], published["manifest_sha256"]);

    fs::write(&artifact_path, b"conflict").unwrap();
    assert!(!fixture.materialize(&artifact_dir).status.success());
}

#[test]
fn cli_rejects_a_corrupted_raw_triplet_without_publishing_a_partition() {
    let fixture = Fixture::new(&valid_rows());
    let artifact_dir = fixture.artifact_dir();
    fs::write(&fixture.data, b"corrupt").unwrap();

    let output = fixture.materialize(&artifact_dir);

    assert!(!output.status.success());
    assert!(!artifact_dir.exists());
}

fn sha256_file(path: &Path) -> String {
    let mut source = File::open(path).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = source.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    hex::encode(digest.finalize())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    path.with_file_name(format!(
        "{}{}",
        path.file_name().unwrap().to_str().unwrap(),
        suffix
    ))
}
