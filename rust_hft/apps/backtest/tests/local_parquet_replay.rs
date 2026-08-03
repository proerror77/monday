use data::binance_lob_replay::source_revision;
use parquet::basic::Compression;
use parquet::data_type::{ByteArray, ByteArrayType, Int64Type};
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use parquet::file::writer::{SerializedFileWriter, SerializedRowGroupWriter};
use parquet::schema::parser::parse_message_type;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

const BINARY: &str = env!("CARGO_BIN_EXE_hft-backtest");
const MESSAGE: &str = "
message binance_replay {
  REQUIRED INT64 timestamp_us;
  REQUIRED INT64 sequence;
  REQUIRED BINARY event (UTF8);
  REQUIRED BINARY payload_json (UTF8);
}
";

#[test]
fn cli_replays_local_canonical_parquet_without_raw_sources_and_records_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("artifact.parquet");
    write_parquet(
        &artifact,
        &[
            (
                100_i64,
                1_i64,
                "snapshot",
                json!({"bids":[[100.0,1.0]],"asks":[[101.0,1.0]]}),
            ),
            (
                200_i64,
                2_i64,
                "l2_update",
                json!({"bids":[[100.0,2.0]],"asks":[[101.0,1.0]]}),
            ),
        ],
    );
    let artifact_sha = sha256_file(&artifact);
    let canonical_artifact = directory.path().join(format!("{artifact_sha}.parquet"));
    fs::rename(&artifact, &canonical_artifact).unwrap();
    let source_sha = "1".repeat(64);
    let manifest_value = json!({
        "dataset_kind": "backtest_canonical_replay_parquet",
        "schema_version": "binance-replay-parquet-v1",
        "format": "parquet",
        "parquet_schema": "timestamp_us:int64,sequence:int64,event:utf8,payload_json:utf8",
        "mission_id": "backtest-fixture",
        "market": "spot",
        "symbol": "BTCUSDT",
        "dataset": "binance_spot_lob",
        "modalities": ["lob"],
        "source_revision": source_revision([source_sha.as_str()]),
        "source_segments": [{
            "file": "missing.raw.ndjson.zst",
            "sha256": source_sha,
            "collector_manifest_sha256": "2".repeat(64),
            "success_marker_sha256": "3".repeat(64),
            "start_received_at_ns": 1,
            "end_received_at_ns": 2,
            "events": 1
        }],
        "rows": 2,
        "first_event_time_us": 100,
        "last_event_time_us": 200,
        "sequence_start": 1,
        "sequence_end": 2,
        "artifact_path": format!("{artifact_sha}.parquet"),
        "artifact_sha256": artifact_sha,
        "point_in_time": true
    });
    let manifest = directory.path().join("canonical-manifest.json");
    let manifest_bytes = serde_json::to_vec_pretty(&manifest_value).unwrap();
    fs::write(&manifest, &manifest_bytes).unwrap();
    let manifest_sha = sha256(&manifest_bytes);
    let output_dir = directory.path().join("output");
    let config = directory.path().join("backtest.yaml");
    let config_yaml = format!(
        "data:\n  path: {}\n  format: parquet\n  manifest_path: {}\n  manifest_sha256: {}\n  require_sequence: true\n  start_ts: 200\nstrategy:\n  volume_factor: 0\n  cvd_threshold: 0\nexecution: {{}}\nrisk: {{}}\noutput:\n  metrics_json: backtest_metrics.json\n",
        canonical_artifact.display(),
        manifest.display(),
        manifest_sha
    );
    fs::write(&config, &config_yaml).unwrap();

    let status = Command::new(BINARY)
        .args([
            "--config",
            config.to_str().unwrap(),
            "--output-dir",
            output_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let metrics: Value =
        serde_json::from_slice(&fs::read(output_dir.join("backtest_metrics.json")).unwrap())
            .unwrap();
    assert_eq!(metrics["input"]["manifest_sha256"], manifest_sha);
    assert_eq!(
        metrics["input"]["source_revision"],
        manifest_value["source_revision"]
    );
    assert_eq!(metrics["input"]["replay_rows"], 1);
    assert_eq!(
        metrics["input"]["config_sha256"].as_str().unwrap().len(),
        64
    );

    let full_config = directory.path().join("full-backtest.yaml");
    fs::write(&full_config, config_yaml.replace("  start_ts: 200\n", "")).unwrap();
    let full_output_dir = directory.path().join("full-output");
    let full_status = Command::new(BINARY)
        .args([
            "--config",
            full_config.to_str().unwrap(),
            "--output-dir",
            full_output_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        full_status.status.success(),
        "{}",
        String::from_utf8_lossy(&full_status.stderr)
    );
    let full_metrics: Value =
        serde_json::from_slice(&fs::read(full_output_dir.join("backtest_metrics.json")).unwrap())
            .unwrap();
    assert_eq!(full_metrics["input"]["replay_rows"], 2);
    assert_ne!(
        metrics["input"]["config_sha256"],
        full_metrics["input"]["config_sha256"]
    );
}

fn write_parquet(path: &Path, rows: &[(i64, i64, &str, Value)]) {
    let schema = Arc::new(parse_message_type(MESSAGE).unwrap());
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(false)
            .set_statistics_enabled(EnabledStatistics::Chunk)
            .build(),
    );
    let mut writer =
        SerializedFileWriter::new(File::create(path).unwrap(), schema, properties).unwrap();
    let mut group = writer.next_row_group().unwrap();
    let timestamps = rows.iter().map(|row| row.0).collect::<Vec<_>>();
    let sequences = rows.iter().map(|row| row.1).collect::<Vec<_>>();
    let events = rows.iter().map(|row| row.2.to_string()).collect::<Vec<_>>();
    let payloads = rows
        .iter()
        .map(|row| serde_json::to_string(&row.3).unwrap())
        .collect::<Vec<_>>();
    write_i64(&mut group, &timestamps);
    write_i64(&mut group, &sequences);
    write_strings(&mut group, &events);
    write_strings(&mut group, &payloads);
    group.close().unwrap();
    writer.close().unwrap();
}

fn write_i64(group: &mut SerializedRowGroupWriter<'_, File>, values: &[i64]) {
    let mut column = group.next_column().unwrap().unwrap();
    column
        .typed::<Int64Type>()
        .write_batch(values, None, None)
        .unwrap();
    column.close().unwrap();
}

fn write_strings(group: &mut SerializedRowGroupWriter<'_, File>, values: &[String]) {
    let values = values
        .iter()
        .map(|value| ByteArray::from(value.as_str()))
        .collect::<Vec<_>>();
    let mut column = group.next_column().unwrap().unwrap();
    column
        .typed::<ByteArrayType>()
        .write_batch(&values, None, None)
        .unwrap();
    column.close().unwrap();
}

fn sha256_file(path: &Path) -> String {
    sha256(&fs::read(path).unwrap())
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
