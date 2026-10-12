use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn executable() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_monday-offline-baseline")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_monday-offline-baseline")))
}

fn cli(args: &[&str]) -> Output {
    Command::new(executable())
        .args(args)
        .env_remove("MONDAY_EXECUTION_HOST")
        .output()
        .unwrap()
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn run(input: &Path, spec: &Path, output: &Path, sha: &str, kind: &str) -> Output {
    run_command("run", input, spec, output, sha, kind)
}
fn run_command(
    command: &str,
    input: &Path,
    spec: &Path,
    output: &Path,
    sha: &str,
    kind: &str,
) -> Output {
    cli(&[
        command,
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
        "--executable-sha256",
        &digest(&fs::read(executable()).unwrap()),
    ])
}

#[test]
fn pit_cli_requires_bound_materialization_before_any_fit() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("synthetic");
    assert!(
        cli(&["synthetic-fixture", "--output", fixture.to_str().unwrap()])
            .status
            .success()
    );
    let input = fixture.join("synthetic-rows.json");
    let output = dir.path().join("must-not-exist");
    let result = cli(&[
        "run",
        "--input",
        input.to_str().unwrap(),
        "--input-sha256",
        &digest(&fs::read(&input).unwrap()),
        "--input-format",
        "pit-jsonl",
        "--spec",
        fixture.join("synthetic-spec.json").to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--input-kind",
        "synthetic",
        "--executable-sha256",
        &digest(&fs::read(executable()).unwrap()),
    ]);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("PIT input requires --materialization")
    );
    assert!(!output.exists());
}

#[test]
fn predeclared_short_segment_cycle_runs_six_fits_without_opening_the_tail() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("synthetic-short-segment");
    assert!(
        cli(&["synthetic-fixture", "--output", fixture.to_str().unwrap()])
            .status
            .success()
    );
    let input = fixture.join("synthetic-rows.json");
    let spec = fixture.join("synthetic-spec.json");
    let mut rows: Value = serde_json::from_slice(&fs::read(&input).unwrap()).unwrap();
    rows.as_array_mut().unwrap().truncate(235);
    for row in rows.as_array_mut().unwrap() {
        row["features"]["book_imbalance_top5"] = row["features"]["book_imbalance"].clone();
        row["fee_bps"] = 5.0.into();
    }
    fs::write(&input, serde_json::to_vec(&rows).unwrap()).unwrap();
    let mut configuration: Value = serde_json::from_slice(&fs::read(&spec).unwrap()).unwrap();
    configuration["protocol"]["walk_forward"]["initial_train_rows"] = 50.into();
    configuration["protocol"]["walk_forward"]["validation_rows"] = 30.into();
    configuration["protocol"]["walk_forward"]["fold_count"] = 2.into();
    configuration["protocol"]["walk_forward"]["sealed_holdout_rows"] = 20.into();
    configuration["protocol"]["costs"]["fee_bps"] = 5.0.into();
    configuration["protocol"]["costs"]["slippage_bps"] = 2.0.into();
    configuration["evaluator"]["multiple_testing_trials"] = 2.into();
    fs::write(&spec, serde_json::to_vec(&configuration).unwrap()).unwrap();
    let output = dir.path().join("short-cycle");
    let result = run_command(
        "cycle",
        &input,
        &spec,
        &output,
        &digest(&fs::read(&input).unwrap()),
        "synthetic",
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["fit_count"], 6);
    assert_eq!(report["automatic_iterations"], 1);
    assert_eq!(
        report["baseline"]["folds"][0]["partition"]["validation"],
        serde_json::json!({"start":55,"end":85})
    );
    assert_eq!(
        report["successor"]["report"]["folds"][0]["partition"]["validation"],
        serde_json::json!({"start":130,"end":160})
    );
    assert_eq!(
        report["baseline"]["protocol"]["walk_forward"]["sealed_holdout_rows"],
        20
    );
    assert_eq!(report["statistical_strategy_acceptance"], false);
    let original_report = fs::read(output.join("report.json")).unwrap();
    for row in rows.as_array_mut().unwrap().iter_mut().skip(215) {
        row["label"] = (-99.0).into();
        row["features"]["book_imbalance"] = 500.0.into();
        row["features"]["book_imbalance_top5"] = (-500.0).into();
    }
    fs::write(&input, serde_json::to_vec(&rows).unwrap()).unwrap();
    let poisoned_output = dir.path().join("poisoned-sealed-tail");
    let poisoned = run_command(
        "cycle",
        &input,
        &spec,
        &poisoned_output,
        &digest(&fs::read(&input).unwrap()),
        "synthetic",
    );
    assert!(
        poisoned.status.success(),
        "{}",
        String::from_utf8_lossy(&poisoned.stderr)
    );
    assert_eq!(
        fs::read(poisoned_output.join("report.json")).unwrap(),
        original_report
    );
}

#[test]
fn offline_cycle_preserves_lineage_reproducibility_budget_and_history_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("explicit-synthetic-cycle");
    assert!(
        cli(&["synthetic-fixture", "--output", fixture.to_str().unwrap()])
            .status
            .success()
    );
    let input = fixture.join("synthetic-rows.json");
    let spec = fixture.join("synthetic-spec.json");
    let mut rows: Value = serde_json::from_slice(&fs::read(&input).unwrap()).unwrap();
    for (index, row) in rows.as_array_mut().unwrap().iter_mut().enumerate() {
        row["features"]["book_imbalance_top5"] = ((index as f64 / 11.0).sin()).into();
    }
    fs::write(&input, serde_json::to_vec(&rows).unwrap()).unwrap();
    let mut configuration: Value = serde_json::from_slice(&fs::read(&spec).unwrap()).unwrap();
    configuration["protocol"]["walk_forward"]["fold_count"] = 2.into();
    configuration["evaluator"]["multiple_testing_trials"] = 2.into();
    fs::write(&spec, serde_json::to_vec(&configuration).unwrap()).unwrap();
    let sha = digest(&fs::read(&input).unwrap());
    let first = dir.path().join("first-cycle");
    let second = dir.path().join("second-cycle");
    for output in [&first, &second] {
        let result = run_command("cycle", &input, &spec, output, &sha, "synthetic");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let bytes = fs::read(output.join("report.json")).unwrap();
        let report: Value = serde_json::from_slice(&bytes).unwrap();
        let manifest: Value =
            serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(report["fit_limit"], 6);
        assert_eq!(report["fit_count"], 6);
        assert_eq!(report["automatic_iterations"], 1);
        assert_eq!(report["reproducibility"]["byte_identical"], true);
        assert_eq!(
            report["successor"]["spec"]["features"],
            serde_json::json!(["book_imbalance_top5"])
        );
        assert_eq!(
            report["successor"]["parent_score"],
            report["baseline"]["out_of_sample_accounting"]["evaluation"]["score"]
        );
        assert_eq!(manifest["report_sha256"], digest(&bytes));
        assert_eq!(manifest["automatic_iterations"], 1);
        assert_eq!(manifest["input_kind"], "synthetic");
        assert_eq!(manifest["campaign_acceptance"], false);
        assert_eq!(manifest["compute_authority_verified_by_diagnostic"], false);
        let feedback_at = chrono::DateTime::parse_from_rfc3339(
            report["successor"]["feedback_available_until"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let prediction_at = chrono::DateTime::parse_from_rfc3339(
            report["successor"]["first_prediction_at"].as_str().unwrap(),
        )
        .unwrap();
        assert!(prediction_at > feedback_at + chrono::TimeDelta::seconds(5));
    }
    assert_eq!(
        fs::read(first.join("report.json")).unwrap(),
        fs::read(second.join("report.json")).unwrap()
    );
    assert!(
        !run_command("cycle", &input, &spec, &first, &sha, "synthetic")
            .status
            .success()
    );
    let historical = dir.path().join("rejected-local-history");
    assert!(
        !run_command("cycle", &input, &spec, &historical, &sha, "historical")
            .status
            .success()
    );
    assert!(!historical.exists());
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
            digest(&fs::read(executable()).unwrap())
        );
        assert_eq!(manifest["campaign_acceptance"], false);
        assert_eq!(manifest["executable_sha256_verified_by_diagnostic"], false);
        assert_eq!(
            manifest["executable_digest_source"],
            "external_artifact_declaration"
        );
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
