use data::binance_lob_replay::source_revision;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BINARY: &str = env!("CARGO_BIN_EXE_binance-replay-parquet-cache-warmer");

struct Fixture {
    directory: tempfile::TempDir,
    manifest: PathBuf,
    artifact: PathBuf,
}

impl Fixture {
    fn new(artifact_bytes: &[u8]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let artifact_sha = sha256(artifact_bytes);
        let artifact = directory.path().join(format!("{artifact_sha}.parquet"));
        fs::write(&artifact, artifact_bytes).unwrap();
        let source_sha = "1".repeat(64);
        let manifest_value = json!({
            "dataset_kind": "backtest_canonical_replay_parquet",
            "schema_version": "binance-replay-parquet-v1",
            "format": "parquet",
            "parquet_schema": "timestamp_us:int64,sequence:int64,event:utf8,payload_json:utf8",
            "mission_id": "cache-warmer-fixture",
            "market": "spot",
            "symbol": "BTCUSDT",
            "dataset": "binance_spot_lob",
            "modalities": ["lob"],
            "source_revision": source_revision([source_sha.as_str()]),
            "source_segments": [{
                "file": "part-1.jsonl.zst",
                "sha256": source_sha,
                "collector_manifest_sha256": "2".repeat(64),
                "success_marker_sha256": "3".repeat(64),
                "start_received_at_ns": 1,
                "end_received_at_ns": 2,
                "events": 1
            }],
            "rows": 1,
            "first_event_time_us": 1,
            "last_event_time_us": 1,
            "sequence_start": 1,
            "sequence_end": 1,
            "artifact_path": artifact.file_name().unwrap().to_str().unwrap(),
            "artifact_sha256": artifact_sha,
            "point_in_time": true
        });
        let manifest = directory.path().join("canonical-manifest.json");
        fs::write(
            &manifest,
            serde_json::to_vec_pretty(&manifest_value).unwrap(),
        )
        .unwrap();
        Self {
            directory,
            manifest,
            artifact,
        }
    }

    fn run(&self, cache_dir: &Path) -> Output {
        Command::new(BINARY)
            .args(["--manifest", self.manifest.to_str().unwrap()])
            .args(["--cache-dir", cache_dir.to_str().unwrap()])
            .output()
            .unwrap()
    }
}

#[test]
fn cache_hit_is_atomic_and_does_not_retransfer_the_source() {
    let fixture = Fixture::new(b"canonical parquet bytes");
    let cache_dir = fixture.directory.path().join("cache");

    let first = fixture.run(&cache_dir);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_json: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_json["cache_hit"], false);
    assert!(Path::new(first_json["ready_marker"].as_str().unwrap()).is_file());
    assert_eq!(
        fs::read(first_json["artifact_path"].as_str().unwrap()).unwrap(),
        b"canonical parquet bytes"
    );

    fs::remove_file(&fixture.artifact).unwrap();
    let second = fixture.run(&cache_dir);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_json: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second_json["cache_hit"], true);
    assert_eq!(
        second_json["manifest_sha256"],
        first_json["manifest_sha256"]
    );

    fs::write(second_json["artifact_path"].as_str().unwrap(), b"corrupt").unwrap();
    assert!(!fixture.run(&cache_dir).status.success());
}

#[test]
fn artifact_sha_mismatch_fails_without_a_consumable_entry() {
    let fixture = Fixture::new(b"canonical parquet bytes");
    let manifest_bytes = fs::read(&fixture.manifest).unwrap();
    let mut manifest: Value = serde_json::from_slice(&manifest_bytes).unwrap();
    manifest["artifact_sha256"] = "f".repeat(64).into();
    manifest["artifact_path"] = format!("{}.parquet", "f".repeat(64)).into();
    fs::write(&fixture.artifact, b"canonical parquet bytes").unwrap();
    fs::rename(
        &fixture.artifact,
        fixture
            .directory
            .path()
            .join(format!("{}.parquet", "f".repeat(64))),
    )
    .unwrap();
    fs::write(&fixture.manifest, serde_json::to_vec(&manifest).unwrap()).unwrap();

    let cache_dir = fixture.directory.path().join("cache");
    let output = fixture.run(&cache_dir);
    assert!(!output.status.success());
    assert!(!cache_dir.join("f").exists());
    assert_eq!(
        fs::read_dir(&cache_dir)
            .map(|entries| entries.count())
            .unwrap_or(0),
        0
    );
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
