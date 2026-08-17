use data::binance_market_tape::{
    AggregateTrade, AggregateTradeSummaryBuilder, LobContinuitySummaryBuilder,
    AGGREGATE_TRADE_SUMMARY_CONTRACT,
};
use parquet::file::reader::{FileReader, SerializedFileReader};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const SLICER: &str = env!("CARGO_BIN_EXE_binance-market-tape-slicer");
const MATERIALIZER: &str = env!("CARGO_BIN_EXE_binance-replay-parquet-materializer");
const START_NS: u64 = 1_783_987_200_000_000_000;
const SYMBOLS: [&str; 3] = ["BTCUSDT", "ETHUSDT", "SOLUSDT"];
static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

fn event_ns(milliseconds: u64) -> u64 {
    START_NS + milliseconds * 1_000_000
}

fn lowercase(symbol: &str) -> String {
    symbol.to_ascii_lowercase()
}

#[rustfmt::skip]
fn snapshot(symbol: &str, milliseconds: u64) -> Value {
    json!({
        "schema": "binance.market_tape.v2", "received_at_ns": event_ns(milliseconds),
        "type": "snapshot", "session_id": "session-1", "symbol": symbol,
        "request_started_at_ns": event_ns(milliseconds) - 1_000_000,
        "snapshot": {"lastUpdateId": 100, "bids": [["100.00000000","10.01000000"],["99","5"]], "asks": [["102","4"],["103","6"]]}
    })
}

#[rustfmt::skip]
fn diff(symbol: &str, milliseconds: u64, update_id: u64) -> Value {
    let received_at_ns = event_ns(milliseconds);
    json!({
        "schema": "binance.market_tape.v2", "received_at_ns": received_at_ns,
        "type": "diff", "session_id": "session-1",
        "frame": {"stream": format!("{}@depth@100ms", lowercase(symbol)), "data": {
            "e": "depthUpdate", "E": received_at_ns / 1_000_000,
            "T": received_at_ns / 1_000_000, "s": symbol,
            "U": update_id, "u": update_id, "pu": update_id - 1,
            "b": [["100.00000000", "11.01000000"]], "a": []
        }}
    })
}

#[rustfmt::skip]
fn trade(symbol: &str, milliseconds: u64) -> Value {
    let received_at_ns = event_ns(milliseconds);
    json!({
        "schema": "binance.market_tape.v2", "received_at_ns": received_at_ns,
        "type": "agg_trade", "session_id": "session-1",
        "frame": {"stream": format!("{}@aggTrade", lowercase(symbol)), "data": {
            "e": "aggTrade", "E": received_at_ns / 1_000_000,
            "s": symbol, "a": 10, "p": "100.5", "q": "2",
            "f": 10, "l": 10, "T": received_at_ns / 1_000_000, "m": false
        }}
    })
}

#[rustfmt::skip]
fn checkpoint(symbol: &str, milliseconds: u64, last_update_id: u64) -> Value {
    json!({
        "schema": "binance.market_tape.v2", "received_at_ns": event_ns(milliseconds),
        "type": "checkpoint", "session_id": "session-1", "symbol": symbol,
        "last_update_id": last_update_id, "synced": true, "bridged": true,
        "continuity_complete": true, "stream_coverage_verified": true,
        "bids": [["100.00000000","11.01000000"],["99","5"]], "asks": [["102","4"],["103","6"]],
        "reason": "test", "replay_safe": true
    })
}

/// One v2 all-market segment: a fat BTCUSDT series, a fat ETHUSDT series, and
/// a thin SOLUSDT series, so byte budgets control the slice partitioning.
fn segment_rows() -> Vec<Value> {
    let mut rows = vec![
        json!({"schema":"binance.market_tape.v2","received_at_ns":event_ns(0),"type":"session_start","session_id":"session-1","market":"spot","symbols":3,"websocket_shards":2,"websocket_streams":6,"stream_types":["depth@100ms","aggTrade"]}),
        json!({"schema":"binance.market_tape.v2","received_at_ns":event_ns(1),"type":"stream_coverage","session_id":"session-1","shards":[["btcusdt@depth@100ms","btcusdt@aggTrade","ethusdt@depth@100ms"],["ethusdt@aggTrade","solusdt@depth@100ms","solusdt@aggTrade"]]}),
    ];
    for (index, symbol) in SYMBOLS.iter().enumerate() {
        let base = 100_u64 + index as u64 * 10_000;
        let diff_count = if *symbol == "SOLUSDT" { 2 } else { 300 };
        rows.push(snapshot(symbol, base));
        for update in 1..=diff_count {
            rows.push(diff(symbol, base + update, 100 + update));
        }
        rows.push(trade(symbol, base + 9000));
        rows.push(checkpoint(symbol, base + 9500, 100 + diff_count));
    }
    rows
}

struct Fixture {
    directory: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(rows: &[Value]) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "binance-market-tape-slicer-{}-{}",
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
        let mut lob_continuity = LobContinuitySummaryBuilder::new(SYMBOLS.map(str::to_string))
            .unwrap();
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
        let manifest_value = json!({
            "schema": "binance.market_tape.v2", "venue": "binance", "market": "spot",
            "dataset": "spot_all", "shard_id": "all", "mode": "diff",
            "symbols": SYMBOLS, "security_token_symbols": [], "excluded_symbols": [],
            "snapshot_limit": 1000,
            "replay_scope": "captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs",
            "venue_depth_complete": false, "events": rows.len(), "event_types": event_types,
            "has_replay_safe_checkpoint": true, "snapshot_ready_count": 3,
            "bridged_count": 3, "stream_coverage_verified_count": 3,
            "snapshot_only_symbols": [], "all_symbols_bridged": true,
            "all_stream_coverage_verified": true,
            "start_received_at_ns": rows.first().unwrap()["received_at_ns"],
            "end_received_at_ns": rows.last().unwrap()["received_at_ns"],
            "date": "2026-08-16", "hour": "12",
            "file": "part-1.jsonl.zst", "bytes": data.metadata().unwrap().len(),
            "sha256": content_sha256,
            "trade_representation": "aggregate_trade_only",
            "price_surface_derivation": "latest aggregate trade price",
            "trade_summary_contract": AGGREGATE_TRADE_SUMMARY_CONTRACT,
            "trade_summaries": trade_summaries.finish().unwrap(),
            "lob_continuity": lob_continuity.finish().unwrap(),
            "stream_types": ["depth@100ms", "aggTrade"]
        });
        let mut manifest_bytes = serde_json::to_vec(&manifest_value).unwrap();
        manifest_bytes.push(b'\n');
        fs::write(sibling(&data, ".manifest.json"), &manifest_bytes).unwrap();
        fs::write(sibling(&data, "._SUCCESS"), format!("{content_sha256}\n")).unwrap();

        Self { directory, data }
    }

    fn slice(&self, output_dir: &Path, extra: &[String]) -> Output {
        let mut command = Command::new(SLICER);
        command
            .arg("--segment")
            .arg(&self.data)
            .arg("--output-dir")
            .arg(output_dir);
        for argument in extra {
            command.arg(argument);
        }
        command.output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn slice_report(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "slicer stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn slice_for_symbol<'a>(report: &'a Value, symbol: &str) -> &'a Value {
    report["slices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|slice| {
            slice["symbols"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry.as_str() == Some(symbol))
        })
        .unwrap_or_else(|| panic!("no slice carries {symbol}"))
}

fn materialize(slice_dir: &Path, slice: &Value, symbol: &str, artifact_dir: &Path) -> Output {
    Command::new(MATERIALIZER)
        .arg("--mission-id")
        .arg(format!("slicer-test-{}", symbol.to_ascii_lowercase()))
        .arg("--symbol")
        .arg(symbol)
        .arg("--market")
        .arg("spot")
        .arg("--segment")
        .arg(slice_dir.join(slice["file"].as_str().unwrap()))
        .arg("--segment-content-sha256")
        .arg(slice["sha256"].as_str().unwrap())
        .arg("--segment-manifest-sha256")
        .arg(slice["manifest_sha256"].as_str().unwrap())
        .arg("--artifact-dir")
        .arg(artifact_dir)
        .output()
        .unwrap()
}

#[test]
fn cli_partitions_a_segment_into_budget_compliant_slices_that_materialize() {
    let fixture = Fixture::new(&segment_rows());
    let slice_dir = fixture.directory.join("slices");
    let output = fixture.slice(
        &slice_dir,
        &["--max-slice-bytes".to_string(), "130000".to_string()],
    );
    let report = slice_report(&output);

    assert_eq!(report["source"]["declared_symbols"], json!(3));
    assert_eq!(report["slices"].as_array().unwrap().len(), 2);
    let eth = slice_for_symbol(&report, "ETHUSDT");
    assert_eq!(eth["symbols"], json!(["ETHUSDT"]));
    let btc = slice_for_symbol(&report, "BTCUSDT");
    assert_eq!(btc["symbols"], json!(["BTCUSDT", "SOLUSDT"]));
    for slice in report["slices"].as_array().unwrap() {
        assert!(slice["decompressed_bytes"].as_u64().unwrap() < 2 * 1024 * 1024 * 1024);
    }

    // A slice is a self-contained market-tape segment: the unchanged
    // materializer verifies and publishes it into the backtest path.
    let artifact_dir = fixture.directory.join("artifacts");
    let materialized = materialize(&slice_dir, eth, "ETHUSDT", &artifact_dir);
    assert!(
        materialized.status.success(),
        "materializer stderr: {}",
        String::from_utf8_lossy(&materialized.stderr)
    );
    let published: Value = serde_json::from_slice(&materialized.stdout).unwrap();
    let manifest = &published["manifest"];
    assert_eq!(manifest["symbol"], json!("ETHUSDT"));
    assert_eq!(manifest["rows"], json!(301));
    assert_eq!(
        manifest["source_segments"][0]["file"].as_str().unwrap(),
        eth["file"].as_str().unwrap()
    );
    let artifact =
        artifact_dir.join(manifest["artifact_path"].as_str().unwrap());
    let reader = SerializedFileReader::new(File::open(&artifact).unwrap()).unwrap();
    assert_eq!(reader.metadata().file_metadata().num_rows(), 301);
}

#[test]
fn cli_extracts_a_requested_symbol_subset_and_is_idempotent() {
    let fixture = Fixture::new(&segment_rows());
    let slice_dir = fixture.directory.join("slices");
    let arguments = vec![
        "--symbols".to_string(),
        "solusdt".to_string(),
    ];
    let first = slice_report(&fixture.slice(&slice_dir, &arguments));
    assert_eq!(first["slices"].as_array().unwrap().len(), 1);
    assert_eq!(first["slices"][0]["symbols"], json!(["SOLUSDT"]));
    assert_eq!(first["source"]["selected_symbols"], json!(1));

    let second = slice_report(&fixture.slice(&slice_dir, &arguments));
    assert_eq!(first, second);

    let artifact_dir = fixture.directory.join("artifacts");
    let slice = &first["slices"][0];
    let materialized = materialize(&slice_dir, slice, "SOLUSDT", &artifact_dir);
    assert!(
        materialized.status.success(),
        "materializer stderr: {}",
        String::from_utf8_lossy(&materialized.stderr)
    );
    let missing = materialize(&slice_dir, slice, "BTCUSDT", &artifact_dir);
    assert!(!missing.status.success());
}

#[test]
fn cli_rejects_symbols_outside_the_declared_scope() {
    let fixture = Fixture::new(&segment_rows());
    let output = fixture.slice(
        &fixture.directory.join("slices"),
        &["--symbols".to_string(), "BTCUSDT,FOOUSDT".to_string()],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("FOOUSDT"));
}

#[test]
fn cli_rejects_a_budget_that_cannot_fit_one_symbol() {
    let fixture = Fixture::new(&segment_rows());
    let output = fixture.slice(
        &fixture.directory.join("slices"),
        &["--max-slice-bytes".to_string(), "1000".to_string()],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot fit one slice"));
}

#[test]
fn cli_rejects_a_tampered_segment() {
    let fixture = Fixture::new(&segment_rows());
    let mut bytes = fs::read(&fixture.data).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x5a;
    fs::write(&fixture.data, bytes).unwrap();

    let output = fixture.slice(&fixture.directory.join("slices"), &[]);
    assert!(!output.status.success());
}

#[test]
fn cli_rejects_a_manifest_digest_mismatch() {
    let fixture = Fixture::new(&segment_rows());
    let success = sibling(&fixture.data, "._SUCCESS");
    fs::write(&success, format!("{}\n", "0".repeat(64))).unwrap();

    let output = fixture.slice(&fixture.directory.join("slices"), &[]);
    assert!(!output.status.success());
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
