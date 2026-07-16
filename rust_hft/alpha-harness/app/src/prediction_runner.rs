use crate::{
    cli::{print_json, PredictionExecuteArgs, PredictionSnapshotArgs},
    mission_runner::{
        create_bundle, fetch_to_file, normalized_sha256, publish_result, sha256_file,
    },
};
use anyhow::{bail, Context};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use zip::ZipArchive;

const MAX_MISSION_BYTES: u64 = 1024 * 1024;
const MAX_SNAPSHOT_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_SNAPSHOT_EXTRACTED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_SNAPSHOT_ENTRIES: usize = 10_000;

#[derive(Debug, Deserialize)]
struct PredictionMissionIdentity {
    mission_id: String,
    lane: String,
    data_snapshot_id: String,
    search_policy_snapshot_id: String,
}

#[derive(Debug, Serialize)]
struct PredictionExecutionEvidence<'a> {
    lane: &'static str,
    mission_id: &'a str,
    data_snapshot_id: &'a str,
    evaluator_version: &'a str,
    mission_sha256: &'a str,
    snapshot_archive_sha256: &'a str,
    runner_exit_code: Option<i32>,
}

#[derive(Debug, Serialize)]
struct PredictionExecutionReport<'a> {
    #[serde(flatten)]
    evidence: PredictionExecutionEvidence<'a>,
    bundle_bytes: u64,
    bundle_sha256: String,
}

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

pub fn execute(args: PredictionExecuteArgs) -> anyhow::Result<()> {
    let runner = configured_binary(
        "MONDAY_PREDICTION_RESEARCH_BIN",
        "monday-prediction-research",
    )?;
    execute_with_runner(args, &runner)
}

fn execute_with_runner(args: PredictionExecuteArgs, runner: &Path) -> anyhow::Result<()> {
    validate_execute_args(&args)?;
    let input_dir = args.work_dir.join("input");
    let artifact_dir = args.work_dir.join("artifacts");
    let results_dir = args.work_dir.join("results");
    std::fs::create_dir_all(&input_dir)?;
    std::fs::create_dir_all(&artifact_dir)?;
    std::fs::create_dir_all(&results_dir)?;

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let mission_path = input_dir.join("mission.json");
    let snapshot_archive = input_dir.join("snapshot.zip");
    let (_, mission_sha256) =
        fetch_to_file(&client, &args.mission_url, &mission_path, MAX_MISSION_BYTES)?;
    if mission_sha256 != normalized_sha256("mission", &args.mission_sha256)? {
        bail!("prediction mission SHA256 mismatch");
    }
    let mission: PredictionMissionIdentity = serde_json::from_slice(&std::fs::read(&mission_path)?)
        .context("prediction mission identity is invalid JSON")?;
    validate_mission_identity(&mission)?;

    let (_, snapshot_sha256) = fetch_to_file(
        &client,
        &args.snapshot_url,
        &snapshot_archive,
        MAX_SNAPSHOT_ARCHIVE_BYTES,
    )?;
    if snapshot_sha256 != normalized_sha256("snapshot archive", &args.snapshot_sha256)? {
        bail!("prediction snapshot archive SHA256 mismatch");
    }
    let snapshot_dir = input_dir.join("snapshot");
    extract_snapshot_archive(&snapshot_archive, &snapshot_dir)?;

    let stdout = File::create(artifact_dir.join("runner.stdout"))?;
    let stderr = File::create(artifact_dir.join("runner.stderr"))?;
    let status = Command::new(runner)
        .arg(&mission_path)
        .arg(&snapshot_dir)
        .arg(&results_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .with_context(|| format!("start prediction research runner {}", runner.display()))?;
    let evidence = PredictionExecutionEvidence {
        lane: "prediction_market",
        mission_id: &mission.mission_id,
        data_snapshot_id: &mission.data_snapshot_id,
        evaluator_version: &mission.search_policy_snapshot_id,
        mission_sha256: &mission_sha256,
        snapshot_archive_sha256: &snapshot_sha256,
        runner_exit_code: status.code(),
    };
    write_json_atomic(&artifact_dir.join("execution-evidence.json"), &evidence)?;
    if !status.success() {
        bail!(
            "prediction research runner exited unsuccessfully with {:?}; see {}",
            status.code(),
            artifact_dir.display()
        );
    }

    let bundle = args.work_dir.join("results.zip");
    create_bundle(&args.work_dir, &bundle, [&results_dir, &artifact_dir])?;
    let bundle_bytes = bundle.metadata()?.len();
    let bundle_sha256 = sha256_file(&bundle)?;
    publish_result(&client, &args.result_put_url, &bundle)?;
    print_json(&PredictionExecutionReport {
        evidence,
        bundle_bytes,
        bundle_sha256,
    })
}

pub fn snapshot(args: PredictionSnapshotArgs) -> anyhow::Result<()> {
    let compiler = configured_binary(
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
    write_json_atomic(
        &snapshot_dir.join("monday-snapshot-evidence.json"),
        &evidence,
    )?;

    let bundle = args.work_dir.join("snapshot.zip");
    // Snapshot consumers expect manifest.json at the archive root. Compiler
    // logs remain local; the content-addressed evidence record travels with the
    // snapshot without becoming evaluator authority.
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

fn validate_execute_args(args: &PredictionExecuteArgs) -> anyhow::Result<()> {
    if args.work_dir.as_os_str().is_empty()
        || [
            args.mission_url.as_str(),
            args.mission_sha256.as_str(),
            args.snapshot_url.as_str(),
            args.snapshot_sha256.as_str(),
            args.result_put_url.as_str(),
        ]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        bail!("prediction execution paths, URLs, and hashes are required");
    }
    Ok(())
}

fn validate_mission_identity(mission: &PredictionMissionIdentity) -> anyhow::Result<()> {
    if mission.lane != "prediction_market"
        || mission.mission_id.trim().is_empty()
        || mission.data_snapshot_id.trim().is_empty()
        || mission.search_policy_snapshot_id.trim().is_empty()
    {
        bail!("prediction mission identity or lane is invalid");
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
    const FORBIDDEN: [&str; 7] = [
        "--output-dir",
        "--db-url",
        "--live",
        "--deploy",
        "--submit",
        "--cancel",
        "--replace",
    ];
    for argument in &args.compiler_args {
        let argument = argument.to_string_lossy();
        if FORBIDDEN
            .iter()
            .any(|flag| argument == *flag || argument.starts_with(&format!("{flag}=")))
        {
            bail!("prediction snapshot argument is forbidden: {argument}");
        }
    }
    Ok(())
}

fn configured_binary(environment: &str, name: &str) -> anyhow::Result<PathBuf> {
    let path = std::env::var_os(environment)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| {
            let current = std::env::current_exe().context("resolve alpha-harness executable")?;
            let parent = current
                .parent()
                .context("alpha-harness executable has no parent directory")?;
            Ok::<_, anyhow::Error>(parent.join(name))
        })?;
    if !path.is_file() {
        bail!(
            "configured prediction binary does not exist: {}",
            path.display()
        );
    }
    Ok(path)
}

fn extract_snapshot_archive(archive_path: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)?;
    let mut archive = ZipArchive::new(File::open(archive_path)?)
        .context("prediction snapshot is not a valid ZIP archive")?;
    if archive.len() > MAX_SNAPSHOT_ENTRIES {
        bail!("prediction snapshot archive has too many entries");
    }
    let mut extracted_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| anyhow::anyhow!("prediction snapshot entry escapes destination"))?
            .to_path_buf();
        if relative.as_os_str().is_empty() {
            bail!("prediction snapshot entry has an empty path");
        }
        let mode_type = entry.unix_mode().unwrap_or_default() & 0o170000;
        if mode_type == 0o120000 {
            bail!("prediction snapshot archive cannot contain symbolic links");
        }
        if entry.is_dir() {
            if mode_type != 0 && mode_type != 0o040000 {
                bail!("prediction snapshot directory has an unsupported file type");
            }
            std::fs::create_dir_all(destination.join(relative))?;
            continue;
        }
        if !entry.is_file() || (mode_type != 0 && mode_type != 0o100000) {
            bail!("prediction snapshot archive contains a non-regular file");
        }
        extracted_bytes = extracted_bytes
            .checked_add(entry.size())
            .filter(|bytes| *bytes <= MAX_SNAPSHOT_EXTRACTED_BYTES)
            .ok_or_else(|| anyhow::anyhow!("prediction snapshot expands beyond the size limit"))?;
        let output_path = destination.join(relative);
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)
            .with_context(|| format!("create extracted snapshot file {}", output_path.display()))?;
        let bytes = std::io::copy(
            &mut entry.by_ref().take(MAX_SNAPSHOT_EXTRACTED_BYTES + 1),
            &mut output,
        )?;
        if bytes != entry.size() {
            bail!("prediction snapshot entry size does not match ZIP metadata");
        }
        output.sync_all()?;
    }
    Ok(())
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn execute_rejects_snapshot_hash_mismatch_before_starting_runner() {
        let fixture = execute_fixture("hash-mismatch");
        let mut args = fixture.args;
        args.snapshot_sha256 = "0".repeat(64);

        let error = execute_with_runner(args, Path::new("/runner-must-not-start"))
            .expect_err("mismatched archive must fail");

        assert!(error
            .to_string()
            .contains("prediction snapshot archive SHA256 mismatch"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn safe_extraction_rejects_path_traversal() {
        let root = temporary_root("zip-traversal");
        let archive_path = root.join("snapshot.zip");
        let mut archive = ZipWriter::new(File::create(&archive_path).unwrap());
        archive
            .start_file(
                "../escaped.json",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        archive.write_all(b"{}\n").unwrap();
        archive.finish().unwrap();

        let error = extract_snapshot_archive(&archive_path, &root.join("snapshot"))
            .expect_err("traversal must fail");

        assert!(error.to_string().contains("escapes destination"));
        assert!(!root.join("escaped.json").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn safe_extraction_rejects_symbolic_links() {
        let root = temporary_root("zip-symlink");
        let archive_path = root.join("snapshot.zip");
        let mut archive = ZipWriter::new(File::create(&archive_path).unwrap());
        archive
            .add_symlink("manifest.json", "../outside", SimpleFileOptions::default())
            .unwrap();
        archive.finish().unwrap();

        let error = extract_snapshot_archive(&archive_path, &root.join("snapshot"))
            .expect_err("symbolic link must fail");

        assert!(error.to_string().contains("symbolic links"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_publishes_one_bundle_through_a_precompiled_runner() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = execute_fixture("precompiled-runner");
        let runner = fixture.root.join("monday-prediction-research");
        std::fs::write(
            &runner,
            "#!/bin/sh\nmkdir -p \"$3\"\nprintf '{\"status\":\"budget_exhausted\"}\\n' > \"$3/summary.json\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();

        execute_with_runner(fixture.args, &runner).unwrap();

        assert!(fixture.result_path.is_file());
        let archive = ZipArchive::new(File::open(&fixture.result_path).unwrap()).unwrap();
        let names = archive.file_names().collect::<Vec<_>>();
        assert!(names.contains(&"results/summary.json"));
        assert!(names.contains(&"artifacts/execution-evidence.json"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

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

        let archive = ZipArchive::new(File::open(&published).unwrap()).unwrap();
        let names = archive.file_names().collect::<Vec<_>>();
        assert!(names.contains(&"manifest.json"));
        assert!(!names.contains(&"snapshot/manifest.json"));
        let extracted = root.join("extracted");
        extract_snapshot_archive(&published, &extracted).unwrap();
        assert!(extracted.join("manifest.json").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn snapshot_wrapper_rejects_execution_authority_flags() {
        let args = PredictionSnapshotArgs {
            work_dir: PathBuf::from("work"),
            result_put_url: "snapshot.zip".to_string(),
            compiler_args: vec![OsString::from("--submit")],
        };
        assert!(validate_snapshot_args(&args)
            .unwrap_err()
            .to_string()
            .contains("forbidden"));
    }

    struct ExecuteFixture {
        root: PathBuf,
        result_path: PathBuf,
        args: PredictionExecuteArgs,
    }

    fn execute_fixture(name: &str) -> ExecuteFixture {
        let root = temporary_root(name);
        let mission_path = root.join("mission.json");
        let mission = serde_json::json!({
            "mission_id": "prediction-test",
            "lane": "prediction_market",
            "data_snapshot_id": "sha256:snapshot-contract",
            "search_policy_snapshot_id": "sha256:evaluator-version"
        });
        let mission_bytes = serde_json::to_vec(&mission).unwrap();
        std::fs::write(&mission_path, &mission_bytes).unwrap();
        let snapshot_path = root.join("input-snapshot.zip");
        let mut archive = ZipWriter::new(File::create(&snapshot_path).unwrap());
        archive
            .start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"{}\n").unwrap();
        archive.finish().unwrap();
        let result_path = root.join("published.zip");
        let args = PredictionExecuteArgs {
            work_dir: root.join("work"),
            mission_url: mission_path.to_string_lossy().into_owned(),
            mission_sha256: format!("{:x}", Sha256::digest(&mission_bytes)),
            snapshot_url: snapshot_path.to_string_lossy().into_owned(),
            snapshot_sha256: sha256_file(&snapshot_path).unwrap(),
            result_put_url: result_path.to_string_lossy().into_owned(),
        };
        ExecuteFixture {
            root,
            result_path,
            args,
        }
    }

    fn temporary_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "alpha-prediction-runner-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}
