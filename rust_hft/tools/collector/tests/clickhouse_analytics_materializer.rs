use parquet::basic::Compression;
use parquet::data_type::{ByteArray, ByteArrayType, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::parser::parse_message_type;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

const BINARY: &str = env!("CARGO_BIN_EXE_clickhouse-analytics-materializer");
const SCHEMA: &str = "timestamp_us:int64,sequence:int64,event:utf8,payload_json:utf8";
const MESSAGE: &str = "
message binance_replay {
  REQUIRED INT64 timestamp_us;
  REQUIRED INT64 sequence;
  REQUIRED BINARY event (UTF8);
  REQUIRED BINARY payload_json (UTF8);
}
";

struct Fixture {
    directory: tempfile::TempDir,
    manifest: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let artifact_bytes = parquet_bytes();
        let artifact_sha = sha256(&artifact_bytes);
        let artifact = directory.path().join(format!("{artifact_sha}.parquet"));
        fs::write(&artifact, artifact_bytes).unwrap();
        let source_sha = "1".repeat(64);
        let source_revision = data::binance_lob_replay::source_revision([source_sha.as_str()]);
        let success_marker_sha = sha256(format!("{source_sha}\n").as_bytes());
        let manifest_value = json!({
            "dataset_kind": "backtest_canonical_replay_parquet",
            "schema_version": "binance-replay-parquet-v1",
            "format": "parquet",
            "parquet_schema": SCHEMA,
            "mission_id": "analytics-fixture",
            "market": "usdm",
            "symbol": "BTCUSDT",
            "dataset": "binance_usdm_lob",
            "modalities": ["lob"],
            "source_revision": source_revision,
            "source_segments": [{
                "file": "part-1.jsonl.zst",
                "sha256": source_sha,
                "collector_manifest_sha256": "2".repeat(64),
                "success_marker_sha256": success_marker_sha,
                "start_received_at_ns": 1,
                "end_received_at_ns": 2_000_000,
                "events": 2
            }],
            "rows": 2,
            "first_event_time_us": 1_000,
            "last_event_time_us": 2_000,
            "sequence_start": 1,
            "sequence_end": 2,
            "artifact_path": artifact.file_name().unwrap().to_str().unwrap(),
            "artifact_sha256": artifact_sha,
            "point_in_time": true
        });
        let manifest_bytes = serde_json::to_vec_pretty(&manifest_value).unwrap();
        let manifest = directory.path().join(format!(
            "{}.canonical-manifest.json",
            sha256(&manifest_bytes)
        ));
        fs::write(&manifest, manifest_bytes).unwrap();
        Self {
            directory,
            manifest,
        }
    }

    fn run(&self, output: &Path) -> Output {
        self.run_manifest(&self.manifest, output)
    }

    fn run_manifest(&self, manifest: &Path, output: &Path) -> Output {
        Command::new(BINARY)
            .args(["--manifest", manifest.to_str().unwrap()])
            .args(["--output", output.to_str().unwrap()])
            .output()
            .unwrap()
    }
}

fn parquet_bytes() -> Vec<u8> {
    let schema = Arc::new(parse_message_type(MESSAGE).unwrap());
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .build(),
    );
    let mut bytes = Vec::new();
    {
        let writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
        write_row_group(writer, &[1_000, 2_000], &[1, 2], &["snapshot", "l2_update"]);
    }
    bytes
}

fn write_row_group(
    mut writer: SerializedFileWriter<&mut Vec<u8>>,
    timestamps: &[i64],
    sequences: &[i64],
    events: &[&str],
) {
    let mut group = writer.next_row_group().unwrap();
    let mut column = group.next_column().unwrap().unwrap();
    column
        .typed::<Int64Type>()
        .write_batch(timestamps, None, None)
        .unwrap();
    column.close().unwrap();
    let mut column = group.next_column().unwrap().unwrap();
    column
        .typed::<Int64Type>()
        .write_batch(sequences, None, None)
        .unwrap();
    column.close().unwrap();
    let event_values = events
        .iter()
        .map(|event| ByteArray::from(*event))
        .collect::<Vec<_>>();
    let mut column = group.next_column().unwrap().unwrap();
    column
        .typed::<ByteArrayType>()
        .write_batch(&event_values, None, None)
        .unwrap();
    column.close().unwrap();
    let payload_values = [
        ByteArray::from(r#"{"bids":[],"asks":[]}"#),
        ByteArray::from(r#"{"bids":[],"asks":[]}"#),
    ];
    let mut column = group.next_column().unwrap().unwrap();
    column
        .typed::<ByteArrayType>()
        .write_batch(&payload_values, None, None)
        .unwrap();
    column.close().unwrap();
    group.close().unwrap();
    writer.close().unwrap();
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[test]
fn materializes_replay_rows_with_queryable_lineage_and_idempotent_output() {
    let fixture = Fixture::new();
    let output = fixture.directory.path().join("plan.jsonl");
    let first = fixture.run(&output);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_bytes = fs::read(&output).unwrap();
    let rows = String::from_utf8(first_bytes.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["table"], "cex_analytics_partitions");
    assert_eq!(rows[1]["table"], "cex_replay_events");
    assert_eq!(rows[2]["table"], "cex_replay_events");
    assert_eq!(
        rows[1]["sql"],
        "INSERT INTO cex_replay_events FORMAT JSONEachRow"
    );
    for row in &rows {
        assert_eq!(row["manifest_sha256"], rows[0]["manifest_sha256"]);
        assert_eq!(row["source_revision"], rows[0]["source_revision"]);
        assert_eq!(row["venue"], "binance");
        assert_eq!(row["market"], "usdm");
        assert_eq!(row["symbol"], "BTCUSDT");
        assert_eq!(row["schema_version"], "binance-replay-parquet-v1");
        assert_eq!(row["start_time_us"], 1_000);
        assert_eq!(row["end_time_us"], 2_000);
        assert!(!row["partition_identity"].as_str().unwrap().is_empty());
    }
    let retry = fixture.run(&output);
    assert!(retry.status.success());
    assert_eq!(fs::read(&output).unwrap(), first_bytes);

    let cache = fixture.directory.path().join("cache");
    fs::create_dir(&cache).unwrap();
    let manifest_value: Value =
        serde_json::from_slice(&fs::read(&fixture.manifest).unwrap()).unwrap();
    let artifact_name = manifest_value["artifact_path"].as_str().unwrap();
    fs::copy(&fixture.manifest, cache.join("canonical-manifest.json")).unwrap();
    fs::copy(
        fixture.directory.path().join(artifact_name),
        cache.join(artifact_name),
    )
    .unwrap();
    let cached_output = cache.join("plan.jsonl");
    let cached = fixture.run_manifest(&cache.join("canonical-manifest.json"), &cached_output);
    assert!(
        cached.status.success(),
        "{}",
        String::from_utf8_lossy(&cached.stderr)
    );
    assert_eq!(fs::read(&cached_output).unwrap(), first_bytes);

    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&fixture.manifest).unwrap()).unwrap();
    manifest["mission_id"] = "different-content-same-partition".into();
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    fs::write(&fixture.manifest, manifest_bytes).unwrap();
    let conflict = fixture.run(&output);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("different content"));
}

#[test]
fn rejects_wrong_parquet_bytes_without_publishing_a_plan() {
    let fixture = Fixture::new();
    let manifest_bytes = fs::read(&fixture.manifest).unwrap();
    let manifest: Value = serde_json::from_slice(&manifest_bytes).unwrap();
    let artifact_name = manifest["artifact_path"].as_str().unwrap().to_string();
    fs::write(fixture.directory.path().join(artifact_name), b"not parquet").unwrap();
    let output = fixture.directory.path().join("plan.jsonl");
    let result = fixture.run(&output);
    assert!(!result.status.success());
    assert!(!output.exists());
}
