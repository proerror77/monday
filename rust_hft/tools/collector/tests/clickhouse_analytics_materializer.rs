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
    manifest_sha256: String,
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
        let manifest_sha256 = sha256(&manifest_bytes);
        let manifest = directory
            .path()
            .join(format!("{manifest_sha256}.canonical-manifest.json"));
        fs::write(&manifest, manifest_bytes).unwrap();
        Self {
            directory,
            manifest,
            manifest_sha256,
        }
    }

    fn run(&self, output: &Path) -> Output {
        self.run_manifest(&self.manifest, output)
    }

    fn run_manifest(&self, manifest: &Path, output: &Path) -> Output {
        self.run_manifest_with_digest(manifest, &self.manifest_sha256, output)
    }

    fn run_manifest_with_digest(&self, manifest: &Path, digest: &str, output: &Path) -> Output {
        Command::new(BINARY)
            .args(["--manifest", manifest.to_str().unwrap()])
            .args(["--manifest-sha256", digest])
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

fn run_typed_manifest(manifest: &Path, manifest_sha256: &str, kind: &str, output: &Path) -> Output {
    Command::new(BINARY)
        .args(["--manifest", manifest.to_str().unwrap()])
        .args(["--manifest-sha256", manifest_sha256])
        .args(["--kind", kind])
        .args(["--output", output.to_str().unwrap()])
        .output()
        .unwrap()
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
    for row in rows.iter().skip(1) {
        assert_eq!(row["materialization_version"], 2);
        assert!(!row["row_identity"].as_str().unwrap().is_empty());
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
    let manifest_sha256 = sha256(&manifest_bytes);
    fs::write(&fixture.manifest, manifest_bytes).unwrap();
    let conflict = fixture.run_manifest_with_digest(&fixture.manifest, &manifest_sha256, &output);
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

#[test]
fn rejects_a_manifest_without_the_independently_expected_digest() {
    let fixture = Fixture::new();
    let output = fixture.directory.path().join("plan.jsonl");
    let result = Command::new(BINARY)
        .args(["--manifest", fixture.manifest.to_str().unwrap()])
        .args(["--manifest-sha256", &"0".repeat(64)])
        .args(["--output", output.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("manifest SHA-256 mismatch"));
    assert!(!output.exists());
}

#[test]
fn does_not_expose_a_plaintext_password_argument() {
    let help = Command::new(BINARY).arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(!String::from_utf8_lossy(&help.stdout).contains("--password"));
}

#[test]
fn remote_write_requires_an_external_partition_claim_directory() {
    let fixture = Fixture::new();
    let output = fixture.directory.path().join("plan.jsonl");
    let result = Command::new(BINARY)
        .args(["--manifest", fixture.manifest.to_str().unwrap()])
        .args(["--manifest-sha256", &fixture.manifest_sha256])
        .args(["--output", output.to_str().unwrap()])
        .args(["--clickhouse-url", "http://127.0.0.1:1"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--claim-dir"));
    assert!(!output.exists());
}

#[test]
fn materializes_pit_features_with_a_distinct_typed_writer() {
    let directory = tempfile::tempdir().unwrap();
    let rows = (0..3)
        .map(|index| {
            let second = format!("0{index}");
            json!({
                "event_time": format!("2026-01-01T00:00:{second}Z"),
                "feature_available_time": format!("2026-01-01T00:00:{second}.100Z"),
                "label_available_time": format!("2026-01-01T00:00:0{}Z", index + 1),
                "ingestion_time": "2026-01-01T00:00:03Z",
                "symbol": "BTCUSDT",
                "source_revisions": {"binance-usdm-lob": "revision-1"},
                "modalities": ["lob"],
                "features": {"imbalance": index as f64},
                "label": index as f64,
            })
        })
        .collect::<Vec<_>>();
    let mut artifact_bytes = Vec::new();
    for row in &rows {
        serde_json::to_writer(&mut artifact_bytes, row).unwrap();
        artifact_bytes.push(b'\n');
    }
    let artifact_sha256 = sha256(&artifact_bytes);
    let artifact_name = format!("{artifact_sha256}.jsonl");
    fs::write(directory.path().join(&artifact_name), artifact_bytes).unwrap();
    let manifest_value = json!({
        "dataset_kind": "point_in_time_feature_matrix",
        "manifest_id": format!("dataset-{artifact_sha256}"),
        "mission_id": "pit-fixture",
        "symbol": "BTCUSDT",
        "schema_version": "pit-feature-matrix-v2",
        "source_revisions": {"binance-usdm-lob": "revision-1"},
        "modalities": ["lob"],
        "feature_names": ["imbalance"],
        "label_spec": {"horizon_buckets": 1, "observation_frequency_millis": 1000},
        "rows": 3,
        "time_bounds": {
            "first_event_time": "2026-01-01T00:00:00Z",
            "last_event_time": "2026-01-01T00:00:02Z",
            "first_feature_available_time": "2026-01-01T00:00:00.100Z",
            "last_feature_available_time": "2026-01-01T00:00:02.100Z",
            "first_label_available_time": "2026-01-01T00:00:01Z",
            "last_label_available_time": "2026-01-01T00:00:03Z",
            "first_ingestion_time": "2026-01-01T00:00:03Z",
            "last_ingestion_time": "2026-01-01T00:00:03Z"
        },
        "artifact_path": artifact_name,
        "artifact_sha256": artifact_sha256,
        "created_at": "2026-01-01T00:00:04Z"
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest_value).unwrap();
    let manifest_sha256 = sha256(&manifest_bytes);
    let manifest = directory.path().join("pit-manifest.json");
    fs::write(&manifest, manifest_bytes).unwrap();
    let output = directory.path().join("pit-plan.jsonl");
    let result = run_typed_manifest(&manifest, &manifest_sha256, "pit-feature", &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let plan = fs::read_to_string(output).unwrap();
    assert_eq!(plan.lines().count(), 4);
    assert!(plan.contains("cex_pit_features"));
    assert!(!plan.contains("cex_replay_events"));
}

#[test]
fn materializes_backtest_results_with_a_distinct_typed_writer() {
    let directory = tempfile::tempdir().unwrap();
    let artifact_bytes = br#"{"pnl":"1.25"}"#;
    let artifact_sha256 = sha256(artifact_bytes);
    let artifact_name = format!("{artifact_sha256}.json");
    fs::write(directory.path().join(&artifact_name), artifact_bytes).unwrap();
    let manifest_value = json!({
        "dataset_kind": "backtest_result_metadata",
        "schema_version": "backtest-result-metadata-v1",
        "format": "json",
        "venue": "binance",
        "market": "usdm",
        "symbol": "BTCUSDT",
        "source_revision": "c".repeat(64),
        "rows": 1,
        "first_event_time_us": 1,
        "last_event_time_us": 2,
        "artifact_path": artifact_name,
        "artifact_sha256": artifact_sha256
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest_value).unwrap();
    let manifest_sha256 = sha256(&manifest_bytes);
    let manifest = directory
        .path()
        .join(format!("{manifest_sha256}.result-manifest.json"));
    fs::write(&manifest, manifest_bytes).unwrap();
    let output = directory.path().join("result-plan.jsonl");
    let result = run_typed_manifest(&manifest, &manifest_sha256, "backtest-result", &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let plan = fs::read_to_string(output).unwrap();
    assert_eq!(plan.lines().count(), 2);
    assert!(plan.contains("cex_backtest_results"));
    assert!(!plan.contains("cex_replay_events"));
}
