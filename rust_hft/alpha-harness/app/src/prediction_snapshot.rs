use crate::{
    cli::{print_json, PredictionSnapshotArgs},
    data_mission,
    mission_runner::{configured_sibling_binary, create_bundle, publish_result, sha256_file},
};
use anyhow::{bail, Context};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Debug, Deserialize)]
struct SnapshotManifestIdentity {
    schema_version: String,
    snapshot_hash: Option<String>,
    snapshot_contract_hash: Option<String>,
}

#[derive(Debug, Serialize)]
struct SnapshotExecutionEvidence<'a> {
    lane: &'static str,
    schema_version: &'a str,
    snapshot_hash: Option<&'a str>,
    snapshot_contract_hash: Option<&'a str>,
    compiler_exit_code: Option<i32>,
}

#[derive(Debug, Serialize)]
struct SnapshotExecutionReport<'a> {
    #[serde(flatten)]
    evidence: SnapshotExecutionEvidence<'a>,
    bundle_bytes: u64,
    bundle_sha256: String,
}

pub fn snapshot(args: PredictionSnapshotArgs) -> anyhow::Result<()> {
    let compiler = configured_sibling_binary(
        "MONDAY_PREDICTION_SNAPSHOT_BIN",
        "monday-prediction-snapshot",
    )?;
    snapshot_with_compiler(args, &compiler)
}

fn snapshot_with_compiler(args: PredictionSnapshotArgs, compiler: &Path) -> anyhow::Result<()> {
    validate_snapshot_args(&args)?;
    let artifact_dir = args.work_dir.join("artifacts");
    data_mission::ensure_real_directory(&args.work_dir, "prediction snapshot work")?;
    data_mission::ensure_real_directory(&artifact_dir, "prediction snapshot artifact")?;
    let stdout_path = artifact_dir.join("snapshot-compiler.stdout");
    let stderr_path = artifact_dir.join("snapshot-compiler.stderr");
    data_mission::ensure_output_path_is_not_symlink(&stdout_path, "snapshot compiler stdout")?;
    data_mission::ensure_output_path_is_not_symlink(&stderr_path, "snapshot compiler stderr")?;
    // A failed immutable upload cannot make a writable previous output
    // trustworthy. Compile each retry into a new private directory.
    let snapshot_dir = tempfile::Builder::new()
        .prefix("prediction-snapshot-output-")
        .tempdir_in(&args.work_dir)
        .with_context(|| {
            format!(
                "create isolated prediction snapshot directory in {}",
                args.work_dir.display()
            )
        })?;

    let stdout = data_mission::temporary_output_file(&stdout_path, ".monday-artifact-log-")?;
    let stderr = data_mission::temporary_output_file(&stderr_path, ".monday-artifact-log-")?;
    let status = Command::new(compiler)
        .arg("--output-dir")
        .arg(snapshot_dir.path())
        .args(&args.compiler_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.reopen()?))
        .stderr(Stdio::from(stderr.reopen()?))
        .status()
        .with_context(|| format!("start prediction snapshot compiler {}", compiler.display()))?;
    stdout.as_file().sync_all()?;
    stderr.as_file().sync_all()?;
    data_mission::persist_output_file(stdout, &stdout_path, "snapshot compiler stdout")?;
    data_mission::persist_output_file(stderr, &stderr_path, "snapshot compiler stderr")?;
    if !status.success() {
        bail!(
            "prediction snapshot compiler exited unsuccessfully with {:?}; see {}",
            status.code(),
            artifact_dir.display()
        );
    }

    let manifest_path = snapshot_dir.path().join("manifest.json");
    let manifest: SnapshotManifestIdentity =
        serde_json::from_slice(&std::fs::read(&manifest_path).with_context(|| {
            format!(
                "snapshot compiler did not create {}",
                manifest_path.display()
            )
        })?)
        .context("prediction snapshot manifest identity is invalid JSON")?;
    validate_snapshot_manifest_identity(&manifest)?;
    let evidence = SnapshotExecutionEvidence {
        lane: "prediction_market_snapshot",
        schema_version: &manifest.schema_version,
        snapshot_hash: manifest.snapshot_hash.as_deref(),
        snapshot_contract_hash: manifest.snapshot_contract_hash.as_deref(),
        compiler_exit_code: status.code(),
    };
    data_mission::write_json_atomic(
        &snapshot_dir.path().join("monday-snapshot-evidence.json"),
        &evidence,
    )?;

    let bundle = args.work_dir.join("snapshot.zip");
    // Consumers expect manifest.json at the archive root. Compiler logs remain
    // local and the evidence record carries no evaluator or execution authority.
    let snapshot_root = snapshot_dir.path().to_path_buf();
    create_bundle(&snapshot_root, &bundle, [&snapshot_root])?;
    let bundle_bytes = bundle.metadata()?.len();
    let bundle_sha256 = sha256_file(&bundle)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    publish_result(&client, &args.result_put_url, &bundle)?;
    print_json(&SnapshotExecutionReport {
        evidence,
        bundle_bytes,
        bundle_sha256,
    })
}

fn validate_snapshot_manifest_identity(manifest: &SnapshotManifestIdentity) -> anyhow::Result<()> {
    if manifest.schema_version != "research_snapshot_v2" {
        bail!("prediction snapshot manifest schema_version must be research_snapshot_v2");
    }

    let snapshot_hash = manifest
        .snapshot_hash
        .as_deref()
        .context("prediction snapshot manifest is missing snapshot_hash")?;
    if snapshot_hash.len() != 16
        || !snapshot_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("prediction snapshot manifest snapshot_hash must be exactly 16 ASCII hex characters");
    }

    let snapshot_contract_hash = manifest
        .snapshot_contract_hash
        .as_deref()
        .context("prediction snapshot manifest is missing snapshot_contract_hash")?;
    let contract_hex = snapshot_contract_hash.strip_prefix("sha256:").unwrap_or("");
    if contract_hex.len() != 64
        || !contract_hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("prediction snapshot manifest snapshot_contract_hash must use sha256:<64 ASCII hex>");
    }
    Ok(())
}

fn validate_snapshot_args(args: &PredictionSnapshotArgs) -> anyhow::Result<()> {
    if args.work_dir.as_os_str().is_empty()
        || args.result_put_url.trim().is_empty()
        || args.compiler_args.is_empty()
    {
        bail!(
            "prediction snapshot work directory, result URL, and compiler arguments are required"
        );
    }
    const VALUE_FLAGS: [&str; 15] = [
        "--start-ts",
        "--end-ts",
        "--start-date",
        "--end-date",
        "--symbols",
        "--lob-sample-secs",
        "--pm-book-sample-secs",
        "--max-quote-age-secs",
        "--observation-sample-secs",
        "--stake-usd",
        "--optimizer-data-dir",
        "--data-requirements",
        "--data-audit-report",
        "--data-audit-status",
        "--pm-book-archive-dir",
    ];
    const BOOLEAN_FLAGS: [&str; 2] = ["--allow-missing-official-settlement", "--skip-deribit"];
    let mut index = 0;
    while index < args.compiler_args.len() {
        let argument = args.compiler_args[index]
            .to_str()
            .context("prediction snapshot arguments must be UTF-8")?;
        if BOOLEAN_FLAGS.contains(&argument) {
            index += 1;
            continue;
        }
        if VALUE_FLAGS.contains(&argument) {
            let value = args.compiler_args.get(index + 1).ok_or_else(|| {
                anyhow::anyhow!("prediction snapshot argument requires a value: {argument}")
            })?;
            if value.is_empty() || value.to_string_lossy().starts_with("--") {
                bail!("prediction snapshot argument requires a value: {argument}");
            }
            index += 2;
            continue;
        }
        bail!("unsupported prediction snapshot argument: {argument}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::OsString,
        fs::File,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    use zip::ZipArchive;

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    #[cfg(unix)]
    #[test]
    fn snapshot_bundle_has_the_archive_root_expected_by_execute() {
        use std::os::unix::fs::PermissionsExt;

        let root = temporary_root("snapshot-layout");
        let compiler = root.join("monday-prediction-snapshot");
        std::fs::write(
            &compiler,
            "#!/bin/sh\ntest \"$1\" = '--output-dir' || exit 2\nmkdir -p \"$2\"\nprintf '{\"schema_version\":\"research_snapshot_v2\",\"snapshot_hash\":\"0123456789abcdef\",\"snapshot_contract_hash\":\"sha256:1111111111111111111111111111111111111111111111111111111111111111\"}\\n' > \"$2/manifest.json\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o700)).unwrap();
        let published = root.join("published-snapshot.zip");

        snapshot_with_compiler(
            PredictionSnapshotArgs {
                work_dir: root.join("work"),
                result_put_url: published.to_string_lossy().into_owned(),
                compiler_args: vec![OsString::from("--start-date"), OsString::from("2026-07-01")],
            },
            &compiler,
        )
        .unwrap();

        let mut archive = ZipArchive::new(File::open(&published).unwrap()).unwrap();
        assert!(archive.by_name("manifest.json").is_ok());
        assert!(archive.by_name("snapshot/manifest.json").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_retry_uses_a_fresh_output_directory_despite_stale_work_tree_state() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = temporary_root("snapshot-retry");
        let compiler = root.join("monday-prediction-snapshot");
        std::fs::write(
            &compiler,
            "#!/bin/sh\ntest \"$1\" = '--output-dir' || exit 2\nprintf 'attempt\\n' >> \"$2/../compiler-attempts\"\nmkdir -p \"$2\"\nprintf '{\"schema_version\":\"research_snapshot_v2\",\"snapshot_hash\":\"0123456789abcdef\",\"snapshot_contract_hash\":\"sha256:1111111111111111111111111111111111111111111111111111111111111111\"}\\n' > \"$2/manifest.json\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o700)).unwrap();
        let published = root.join("published-snapshot.zip");
        std::fs::write(&published, "occupied").unwrap();
        let args = PredictionSnapshotArgs {
            work_dir: root.join("work"),
            result_put_url: published.to_string_lossy().into_owned(),
            compiler_args: vec![OsString::from("--start-date"), OsString::from("2026-07-01")],
        };

        snapshot_with_compiler(args.clone(), &compiler)
            .expect_err("occupied destination must fail the first publication");
        let protected_target = root.join("protected-manifest");
        std::fs::write(&protected_target, "preserve\n").unwrap();
        let stale_manifest = args.work_dir.join("snapshot/manifest.json");
        if stale_manifest.exists() {
            std::fs::remove_file(&stale_manifest).unwrap();
        } else {
            std::fs::create_dir_all(stale_manifest.parent().unwrap()).unwrap();
        }
        symlink(&protected_target, &stale_manifest).unwrap();
        std::fs::remove_file(&published).unwrap();

        snapshot_with_compiler(args, &compiler)
            .expect("retry must rebuild from a fresh private output directory");

        assert_eq!(
            std::fs::read_to_string(protected_target).unwrap(),
            "preserve\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("work/compiler-attempts")).unwrap(),
            "attempt\nattempt\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_a_symlinked_artifact_leaf_before_starting_compiler() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = temporary_root("symlinked-artifact-leaf");
        let compiler = root.join("must-not-start");
        std::fs::write(
            &compiler,
            "#!/bin/sh\nprintf started > \"$2/../compiler-started\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o700)).unwrap();
        let work_dir = root.join("work");
        let artifact_dir = work_dir.join("artifacts");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let protected_target = root.join("protected-compiler-stdout");
        std::fs::write(&protected_target, "preserve\n").unwrap();
        symlink(
            &protected_target,
            artifact_dir.join("snapshot-compiler.stdout"),
        )
        .unwrap();

        let error = snapshot_with_compiler(
            PredictionSnapshotArgs {
                work_dir: work_dir.clone(),
                result_put_url: root.join("published.zip").to_string_lossy().into_owned(),
                compiler_args: vec![OsString::from("--start-date"), OsString::from("2026-07-01")],
            },
            &compiler,
        )
        .expect_err("a symlinked artifact path must be rejected before the compiler starts");

        assert!(error.to_string().contains("symbolic link"));
        assert_eq!(
            std::fs::read_to_string(&protected_target).unwrap(),
            "preserve\n"
        );
        assert!(!work_dir.join("compiler-started").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn snapshot_wrapper_rejects_unknown_or_execution_authority_flags() {
        let args = PredictionSnapshotArgs {
            work_dir: PathBuf::from("work"),
            result_put_url: "snapshot.zip".to_string(),
            compiler_args: vec![OsString::from("--submit")],
        };
        assert!(validate_snapshot_args(&args)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
        let mut typo = args;
        typo.compiler_args = vec![OsString::from("--symblos"), OsString::from("BTCUSDT")];
        assert!(validate_snapshot_args(&typo)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
    }

    #[test]
    fn snapshot_manifest_identity_rejects_empty_or_malformed_hashes() {
        let valid_snapshot_hash = "0123456789abcdef";
        let valid_contract_hash = format!("sha256:{}", "a".repeat(64));
        let uppercase_contract_hash = format!("sha256:{}", "A".repeat(64));
        let legacy_manifest = SnapshotManifestIdentity {
            schema_version: "research_snapshot_v1".to_string(),
            snapshot_hash: Some(valid_snapshot_hash.to_string()),
            snapshot_contract_hash: Some(valid_contract_hash.clone()),
        };
        assert!(validate_snapshot_manifest_identity(&legacy_manifest)
            .unwrap_err()
            .to_string()
            .contains("schema_version"));

        for (snapshot_hash, snapshot_contract_hash, expected_error) in [
            ("", valid_contract_hash.as_str(), "snapshot_hash"),
            (
                "0123456789abcdeg",
                valid_contract_hash.as_str(),
                "snapshot_hash",
            ),
            (
                "0123456789ABCDEf",
                valid_contract_hash.as_str(),
                "snapshot_hash",
            ),
            (valid_snapshot_hash, "", "snapshot_contract_hash"),
            (valid_snapshot_hash, "sha256:abc", "snapshot_contract_hash"),
            (
                valid_snapshot_hash,
                uppercase_contract_hash.as_str(),
                "snapshot_contract_hash",
            ),
        ] {
            let manifest = SnapshotManifestIdentity {
                schema_version: "research_snapshot_v2".to_string(),
                snapshot_hash: Some(snapshot_hash.to_string()),
                snapshot_contract_hash: Some(snapshot_contract_hash.to_string()),
            };
            assert!(validate_snapshot_manifest_identity(&manifest)
                .unwrap_err()
                .to_string()
                .contains(expected_error));
        }
    }

    fn temporary_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "alpha-prediction-snapshot-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}
