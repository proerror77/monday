use crate::{
    cli::{print_json, PredictionExecuteArgs},
    data_mission,
    mission_runner::{
        configured_sibling_binary, create_bundle, fetch_to_file, normalized_sha256, publish_result,
        sha256_file,
    },
};
use anyhow::{bail, Context};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use zip::ZipArchive;

const MAX_MISSION_BYTES: u64 = 1024 * 1024;
const MAX_SNAPSHOT_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_RESUME_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
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
    resume_bundle_sha256: Option<&'a str>,
    runner_exit_code: Option<i32>,
}

#[derive(Debug, Serialize)]
struct PredictionExecutionReport<'a> {
    #[serde(flatten)]
    evidence: PredictionExecutionEvidence<'a>,
    bundle_bytes: u64,
    bundle_sha256: String,
}

pub fn execute(args: PredictionExecuteArgs) -> anyhow::Result<()> {
    let runner = configured_sibling_binary(
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
    data_mission::ensure_real_directory(&args.work_dir, "prediction work")?;
    data_mission::ensure_real_directory(&input_dir, "prediction input")?;
    data_mission::ensure_real_directory(&artifact_dir, "prediction artifact")?;
    ensure_empty_results_directory(&results_dir)?;
    let stdout_path = artifact_dir.join("runner.stdout");
    let stderr_path = artifact_dir.join("runner.stderr");
    data_mission::ensure_output_path_is_not_symlink(&stdout_path, "prediction runner stdout")?;
    data_mission::ensure_output_path_is_not_symlink(&stderr_path, "prediction runner stderr")?;

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
    // The declared archive SHA-256 is the trust anchor. A prior work-dir
    // extraction is writable state, so a retry always extracts into a fresh
    // private directory instead of attempting to reuse it.
    let snapshot_dir = tempfile::Builder::new()
        .prefix("prediction-snapshot-")
        .tempdir_in(&input_dir)
        .with_context(|| {
            format!(
                "create isolated prediction snapshot directory in {}",
                input_dir.display()
            )
        })?;
    extract_archive(&snapshot_archive, snapshot_dir.path(), None)?;

    let resume_bundle_sha256 = if let Some((resume_url, resume_sha256)) = resume_source(&args)? {
        let resume_archive = input_dir.join("resume.zip");
        let (_, actual_sha256) = fetch_to_file(
            &client,
            resume_url,
            &resume_archive,
            MAX_RESUME_ARCHIVE_BYTES,
        )?;
        if actual_sha256 != normalized_sha256("resume bundle", resume_sha256)? {
            bail!("prediction resume bundle SHA256 mismatch");
        }
        extract_archive(&resume_archive, &args.work_dir, Some(Path::new("results")))?;
        Some(actual_sha256)
    } else {
        None
    };

    let stdout = data_mission::temporary_output_file(&stdout_path, ".monday-artifact-log-")?;
    let stderr = data_mission::temporary_output_file(&stderr_path, ".monday-artifact-log-")?;
    let status = Command::new(runner)
        .arg(&mission_path)
        .arg(snapshot_dir.path())
        .arg(&results_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.reopen()?))
        .stderr(Stdio::from(stderr.reopen()?))
        .status()
        .with_context(|| format!("start prediction research runner {}", runner.display()))?;
    stdout.as_file().sync_all()?;
    stderr.as_file().sync_all()?;
    data_mission::persist_output_file(stdout, &stdout_path, "prediction runner stdout")?;
    data_mission::persist_output_file(stderr, &stderr_path, "prediction runner stderr")?;
    let evidence = PredictionExecutionEvidence {
        lane: "prediction_market",
        mission_id: &mission.mission_id,
        data_snapshot_id: &mission.data_snapshot_id,
        evaluator_version: &mission.search_policy_snapshot_id,
        mission_sha256: &mission_sha256,
        snapshot_archive_sha256: &snapshot_sha256,
        resume_bundle_sha256: resume_bundle_sha256.as_deref(),
        runner_exit_code: status.code(),
    };
    data_mission::write_json_atomic(&artifact_dir.join("execution-evidence.json"), &evidence)?;

    let bundle = args.work_dir.join("results.zip");
    create_bundle(&args.work_dir, &bundle, [&results_dir, &artifact_dir])?;
    let bundle_bytes = bundle.metadata()?.len();
    let bundle_sha256 = sha256_file(&bundle)?;
    publish_result(&client, &args.result_put_url, &bundle)?;
    print_json(&PredictionExecutionReport {
        evidence,
        bundle_bytes,
        bundle_sha256,
    })?;
    if !status.success() {
        bail!(
            "prediction research runner exited unsuccessfully with {:?}; immutable evidence was published",
            status.code()
        );
    }
    Ok(())
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
    resume_source(args)?;
    Ok(())
}

fn resume_source(args: &PredictionExecuteArgs) -> anyhow::Result<Option<(&str, &str)>> {
    let url = args
        .resume_url
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let sha256 = args
        .resume_sha256
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    match (url, sha256) {
        (None, None) => Ok(None),
        (Some(url), Some(sha256)) => Ok(Some((url, sha256))),
        _ => bail!("prediction resume URL and SHA256 must be supplied together"),
    }
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

fn ensure_empty_results_directory(results_dir: &Path) -> anyhow::Result<()> {
    data_mission::ensure_real_directory(results_dir, "prediction results")?;
    if std::fs::read_dir(results_dir)?
        .next()
        .transpose()?
        .is_some()
    {
        bail!(
            "prediction results directory is not empty; start a new work directory and restore from a pinned resume bundle"
        );
    }
    Ok(())
}

fn extract_archive(
    archive_path: &Path,
    destination: &Path,
    required_prefix: Option<&Path>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)?;
    let mut archive = ZipArchive::new(File::open(archive_path)?)
        .context("prediction snapshot is not a valid ZIP archive")?;
    if archive.len() > MAX_SNAPSHOT_ENTRIES {
        bail!("prediction snapshot archive has too many entries");
    }
    let mut extracted_bytes = 0_u64;
    let mut extracted_files = 0_usize;
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
        let is_directory = entry.is_dir();
        let is_file = entry.is_file();
        if is_directory {
            if mode_type != 0 && mode_type != 0o040000 {
                bail!("prediction snapshot directory has an unsupported file type");
            }
        } else if !is_file || (mode_type != 0 && mode_type != 0o100000) {
            bail!("prediction snapshot archive contains a non-regular file");
        }
        if required_prefix.is_some_and(|prefix| !relative.starts_with(prefix)) {
            continue;
        }
        if is_directory {
            std::fs::create_dir_all(destination.join(relative))?;
            continue;
        }
        let entry_size = entry.size();
        extracted_bytes = extracted_bytes
            .checked_add(entry_size)
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
        let bytes = std::io::copy(&mut entry.by_ref().take(entry_size + 1), &mut output)?;
        if bytes != entry_size {
            bail!("prediction snapshot entry size does not match ZIP metadata");
        }
        output.sync_all()?;
        extracted_files += 1;
    }
    if required_prefix.is_some() && extracted_files == 0 {
        bail!("prediction resume bundle contains no results state");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        io::Write,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
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

        let error = extract_archive(&archive_path, &root.join("snapshot"), None)
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

        let error = extract_archive(&archive_path, &root.join("snapshot"), None)
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
    fn failed_runner_still_publishes_immutable_resume_evidence() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = execute_fixture("failed-runner-evidence");
        let runner = fixture.root.join("monday-prediction-research");
        std::fs::write(
            &runner,
            "#!/bin/sh\nmkdir -p \"$3\"\nprintf '{\"status\":\"paused\"}\\n' > \"$3/checkpoint.json\"\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error = execute_with_runner(fixture.args, &runner)
            .expect_err("paused runner must keep a failing transport status");

        assert!(error
            .to_string()
            .contains("immutable evidence was published"));
        assert!(fixture.result_path.is_file());
        let archive = ZipArchive::new(File::open(&fixture.result_path).unwrap()).unwrap();
        assert!(archive
            .file_names()
            .any(|name| name == "results/checkpoint.json"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn same_work_dir_retry_uses_a_fresh_verified_snapshot_extraction() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = execute_fixture("same-work-dir-retry");
        let paused_runner = fixture.root.join("paused-runner");
        std::fs::write(&paused_runner, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&paused_runner, std::fs::Permissions::from_mode(0o700)).unwrap();
        execute_with_runner(fixture.args.clone(), &paused_runner)
            .expect_err("first attempt must publish its pause checkpoint");
        std::fs::create_dir_all(fixture.args.work_dir.join("input/snapshot")).unwrap();
        std::fs::write(
            fixture.args.work_dir.join("input/snapshot/manifest.json"),
            "tampered\n",
        )
        .unwrap();
        let resumed_runner = fixture.root.join("resumed-runner");
        std::fs::write(
            &resumed_runner,
            "#!/bin/sh\ntest \"$(cat \"$2/manifest.json\")\" = '{}' || exit 4\nprintf '{\"status\":\"budget_exhausted\"}\\n' > \"$3/summary.json\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&resumed_runner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut retry_args = fixture.args.clone();
        retry_args.result_put_url = fixture
            .root
            .join("resumed-results.zip")
            .to_string_lossy()
            .into_owned();

        execute_with_runner(retry_args, &resumed_runner)
            .expect("retry with no local results must ignore stale extraction");

        assert!(fixture.root.join("resumed-results.zip").is_file());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_a_symlinked_artifact_leaf_before_starting_runner() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let fixture = execute_fixture("symlinked-artifact-leaf");
        let artifact_dir = fixture.args.work_dir.join("artifacts");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let protected_target = fixture.root.join("protected-runner-stdout");
        std::fs::write(&protected_target, "preserve\n").unwrap();
        symlink(&protected_target, artifact_dir.join("runner.stdout")).unwrap();
        let runner = fixture.root.join("must-not-start");
        std::fs::write(
            &runner,
            "#!/bin/sh\nprintf started > \"$3/runner-started\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error = execute_with_runner(fixture.args.clone(), &runner)
            .expect_err("a symlinked artifact path must be rejected before the runner starts");

        assert!(error.to_string().contains("symbolic link"));
        assert_eq!(
            std::fs::read_to_string(&protected_target).unwrap(),
            "preserve\n"
        );
        assert!(!fixture
            .args
            .work_dir
            .join("results/runner-started")
            .exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_untrusted_local_results_state() {
        let fixture = execute_fixture("untrusted-local-results");
        let results_dir = fixture.args.work_dir.join("results");
        std::fs::create_dir_all(&results_dir).unwrap();
        std::fs::write(results_dir.join("checkpoint.json"), "tampered\n").unwrap();

        let error = execute_with_runner(fixture.args, Path::new("/runner-must-not-start"))
            .expect_err("local writable results must not become resume input");

        assert!(error
            .to_string()
            .contains("prediction results directory is not empty"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_a_work_directory_with_a_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let fixture = execute_fixture("symlinked-artifacts");
        let protected_directory = fixture.root.join("protected-artifacts");
        let protected_work_directory = protected_directory.join("work");
        std::fs::create_dir_all(&protected_work_directory).unwrap();
        let linked_parent = fixture.root.join("linked-parent");
        symlink(&protected_directory, &linked_parent).unwrap();
        let mut args = fixture.args;
        args.work_dir = linked_parent.join("work");

        let error = execute_with_runner(args, Path::new("/runner-must-not-start"))
            .expect_err("a symlinked work directory ancestor must be rejected");

        assert!(error.to_string().contains("symbolic link"));
        assert!(std::fs::read_dir(protected_work_directory)
            .unwrap()
            .next()
            .is_none());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn next_attempt_restores_results_from_a_pinned_resume_bundle() {
        use std::os::unix::fs::PermissionsExt;

        let mut fixture = execute_fixture("resume-results");
        let resume_path = fixture.root.join("resume.zip");
        let mut archive = ZipWriter::new(File::create(&resume_path).unwrap());
        archive
            .start_file("results/checkpoint.json", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"{\"status\":\"paused\"}\n").unwrap();
        archive.finish().unwrap();
        fixture.args.resume_url = Some(resume_path.to_string_lossy().into_owned());
        fixture.args.resume_sha256 = Some(sha256_file(&resume_path).unwrap());
        let runner = fixture.root.join("monday-prediction-research");
        std::fs::write(
            &runner,
            "#!/bin/sh\ntest -f \"$3/checkpoint.json\" || exit 2\nprintf '{\"status\":\"budget_exhausted\"}\\n' > \"$3/summary.json\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();

        execute_with_runner(fixture.args, &runner).unwrap();

        assert!(fixture.result_path.is_file());
        std::fs::remove_dir_all(fixture.root).unwrap();
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
            resume_url: None,
            resume_sha256: None,
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
