use crate::{
    cli::{print_json, PredictionSnapshotArgs},
    data_mission,
    mission_runner::{configured_sibling_binary, create_bundle, publish_result, sha256_file},
};
use anyhow::{bail, Context};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
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
    let snapshot_dir = args.work_dir.join("snapshot");
    std::fs::create_dir_all(&artifact_dir)?;
    std::fs::create_dir_all(&snapshot_dir)?;

    let stdout = File::create(artifact_dir.join("snapshot-compiler.stdout"))?;
    let stderr = File::create(artifact_dir.join("snapshot-compiler.stderr"))?;
    let status = Command::new(compiler)
        .arg("--output-dir")
        .arg(&snapshot_dir)
        .args(&args.compiler_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .with_context(|| format!("start prediction snapshot compiler {}", compiler.display()))?;
    if !status.success() {
        bail!(
            "prediction snapshot compiler exited unsuccessfully with {:?}; see {}",
            status.code(),
            artifact_dir.display()
        );
    }

    let manifest_path = snapshot_dir.join("manifest.json");
    let manifest: SnapshotManifestIdentity =
        serde_json::from_slice(&std::fs::read(&manifest_path).with_context(|| {
            format!(
                "snapshot compiler did not create {}",
                manifest_path.display()
            )
        })?)
        .context("prediction snapshot manifest identity is invalid JSON")?;
    if manifest.snapshot_hash.is_none() || manifest.snapshot_contract_hash.is_none() {
        bail!("prediction snapshot manifest is not content addressed");
    }
    let evidence = SnapshotExecutionEvidence {
        lane: "prediction_market_snapshot",
        schema_version: &manifest.schema_version,
        snapshot_hash: manifest.snapshot_hash.as_deref(),
        snapshot_contract_hash: manifest.snapshot_contract_hash.as_deref(),
        compiler_exit_code: status.code(),
    };
    data_mission::write_json_atomic(
        &snapshot_dir.join("monday-snapshot-evidence.json"),
        &evidence,
    )?;

    let bundle = args.work_dir.join("snapshot.zip");
    // Consumers expect manifest.json at the archive root. Compiler logs remain
    // local and the evidence record carries no evaluator or execution authority.
    create_bundle(&snapshot_dir, &bundle, [&snapshot_dir])?;
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
            "#!/bin/sh\ntest \"$1\" = '--output-dir' || exit 2\nmkdir -p \"$2\"\nprintf '{\"schema_version\":\"research_snapshot_v2\",\"snapshot_hash\":\"sha256:outer\",\"snapshot_contract_hash\":\"sha256:contract\"}\\n' > \"$2/manifest.json\"\n",
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
