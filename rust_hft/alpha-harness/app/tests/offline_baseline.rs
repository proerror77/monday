use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_monday-offline-baseline"))
        .args(args)
        .env_remove("MONDAY_EXECUTION_HOST")
        .output()
        .unwrap()
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn run(input: &Path, spec: &Path, output: &Path, sha: &str, kind: &str) -> Output {
    cli(&[
        "run",
        "--input",
        input.to_str().unwrap(),
        "--input-sha256",
        sha,
        "--spec",
        spec.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--input-kind",
        kind,
    ])
}

#[test]
fn offline_baseline_reproduces_and_rejects_tamper_overwrite_and_local_history() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("fixture");
    let generated = cli(&["synthetic-fixture", "--output", fixture.to_str().unwrap()]);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let input = fixture.join("synthetic-rows.json");
    let spec = fixture.join("synthetic-spec.json");
    let hash = digest(&fs::read(&input).unwrap());
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    for output in [&first, &second] {
        let result = run(&input, &spec, output, &hash, "synthetic");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let bytes = fs::read(output.join("report.json")).unwrap();
        let report: Value = serde_json::from_slice(&bytes).unwrap();
        let manifest: Value =
            serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(report["fit_count"], 3);
        assert_eq!(report["input_rows"], 300);
        assert_eq!(manifest["input_kind"], "synthetic");
        assert_eq!(manifest["input_sha256"], hash);
        assert_eq!(manifest["report_sha256"], digest(&bytes));
        assert_eq!(
            manifest["executable_sha256"],
            digest(&fs::read(env!("CARGO_BIN_EXE_monday-offline-baseline")).unwrap())
        );
        assert_eq!(manifest["campaign_acceptance"], false);
        assert_eq!(report["statistical_strategy_acceptance"], false);
        let ledger = report["out_of_sample_accounting"]["ledger"]
            .as_array()
            .unwrap();
        assert_eq!(ledger.len(), 90);
        for fold in 1..=3 {
            let terminal = ledger.iter().rfind(|p| p["fold_index"] == fold).unwrap();
            assert_eq!(terminal["target_position"], 0.0);
        }
    }
    assert_eq!(
        fs::read(first.join("report.json")).unwrap(),
        fs::read(second.join("report.json")).unwrap()
    );
    let original = fs::read(first.join("manifest.json")).unwrap();
    assert!(!run(&input, &spec, &first, &hash, "synthetic")
        .status
        .success());
    assert_eq!(original, fs::read(first.join("manifest.json")).unwrap());
    let rejected = dir.path().join("rejected");
    fs::write(&input, b"[]").unwrap();
    let tamper = run(&input, &spec, &rejected, &hash, "synthetic");
    assert!(!tamper.status.success());
    assert!(String::from_utf8_lossy(&tamper.stderr).contains("SHA256 mismatch"));
    assert!(!rejected.exists());
    let historical = run(&input, &spec, &rejected, &hash, "historical");
    assert!(!historical.status.success());
    assert!(String::from_utf8_lossy(&historical.stderr).contains("ACK placement"));
    assert!(!rejected.exists());
}
