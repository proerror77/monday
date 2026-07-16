use crate::{
    cli::{
        print_json, DatasetArgs, EngineChoice, EvaluateArgs, ExecuteMissionArgs, RunMissionArgs,
        ValidationArgs,
    },
    data_mission, governance, mission,
};
use alpha_domain::{
    MissionCompletionPolicy, MissionStatus, ResearchMission, SearchBudget, ValidatorMode,
};
use alpha_store::{AlphaStore, StoreError};
use anyhow::{bail, Context};
use chrono::Utc;
use hft_research_manifest::ManifestId;
use reqwest::{
    blocking::Client,
    header::{HeaderValue, CONTENT_TYPE},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

const MATERIALIZATION_KIND: &str = "lob_point_in_time_materialization";
const MATERIALIZATION_SCHEMA: &str = "binance-lob-pit-v1";
// ponytail: one Mission is capped at 1 GiB; raise this only when staged partitions exceed it.
const MAX_FEATURE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_MATERIALIZATION_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Materialization {
    dataset_kind: String,
    schema_version: String,
    symbol: String,
    market: String,
    source_revision: String,
    source_segments: Vec<SourceSegment>,
    artifact_sha256: String,
    bucket_ms: u64,
    label_horizon_buckets: usize,
}

#[derive(Debug, Deserialize)]
struct SourceSegment {
    sha256: String,
}

#[derive(Debug, Serialize)]
struct ExecutionReport<'a> {
    mission_id: &'a str,
    engine: &'static str,
    bundle_bytes: u64,
    bundle_sha256: String,
}

pub fn execute(args: ExecuteMissionArgs) -> anyhow::Result<()> {
    validate_args(&args)?;
    let input_dir = args.work_dir.join("input");
    let artifact_dir = args.work_dir.join("artifacts");
    let results_dir = args.work_dir.join("results");
    std::fs::create_dir_all(&input_dir)?;
    std::fs::create_dir_all(&artifact_dir)?;
    std::fs::create_dir_all(&results_dir)?;

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let feature_path = input_dir.join("features.jsonl");
    let materialization_path = input_dir.join("materialization.json");
    let (_, feature_sha256) =
        fetch_to_file(&client, &args.feature_url, &feature_path, MAX_FEATURE_BYTES)?;
    let (_, materialization_sha256) = fetch_to_file(
        &client,
        &args.materialization_url,
        &materialization_path,
        MAX_MATERIALIZATION_BYTES,
    )?;
    if materialization_sha256 != normalized_sha256("materialization", &args.materialization_sha256)?
    {
        bail!("materialization manifest SHA256 mismatch");
    }
    let materialization: Materialization =
        serde_json::from_slice(&std::fs::read(&materialization_path)?)
            .context("materialization manifest is invalid JSON")?;
    validate_materialization(&materialization, &feature_sha256, &args.validation)?;

    let db = results_dir.join("alpha.duckdb");
    let feature_manifest_path = results_dir.join("feature-manifest.json");
    let mut store = AlphaStore::open(&db)?;
    let feature_manifest = data_mission::import_and_register_features(
        &mut store,
        &args.data_mission_id,
        &feature_path,
        &artifact_dir,
    )?;
    let source_key = format!("binance-{}-lob", materialization.market);
    if feature_manifest.symbol != materialization.symbol
        || feature_manifest.source_revisions.get(&source_key)
            != Some(&materialization.source_revision)
        || feature_manifest.artifact_sha256 != feature_sha256
        || feature_manifest.label_spec.horizon_buckets != materialization.label_horizon_buckets
        || feature_manifest.label_spec.observation_frequency_millis != materialization.bucket_ms
    {
        bail!("registered feature lineage or label facts do not match the materialization");
    }
    data_mission::write_json_atomic(&feature_manifest_path, &feature_manifest)?;
    data_mission::write_json_atomic(
        &results_dir.join("data-import.json"),
        &serde_json::json!({
            "manifest": &feature_manifest,
            "manifest_path": &feature_manifest_path,
        }),
    )?;
    std::fs::copy(
        &materialization_path,
        results_dir.join("materialization.json"),
    )?;

    let now = Utc::now();
    let research_mission = ResearchMission {
        mission_id: args.mission_id.clone(),
        objective: args.objective.clone(),
        hypothesis_scope: args.hypothesis_scope.clone(),
        mutable_scope: vec!["factor_ast".to_string()],
        dataset_manifest_id: ManifestId::new(feature_manifest.manifest_id.clone())?,
        baseline_artifact_id: None,
        validation_mode: ValidatorMode::MissionValidator,
        validator_spec: serde_json::json!({"multiple_testing_trials": args.max_candidates}),
        search_budget: SearchBudget {
            max_candidates: args.max_candidates,
            max_expansions: args.max_expansions,
            max_tokens: 0,
            max_seconds: args.max_seconds,
        },
        completion_policy: MissionCompletionPolicy::default(),
        prompt_snapshot_id: None,
        search_policy_snapshot_id: format!("{}-lob-pit-v1", engine_name(args.engine)),
        status: MissionStatus::Pending,
        terminal_reason: None,
        created_at: now,
        updated_at: now,
    };
    data_mission::write_json_atomic(&results_dir.join("mission.json"), &research_mission)?;
    store.create_mission(&research_mission)?;
    data_mission::write_json_atomic(&results_dir.join("mission-create.json"), &research_mission)?;
    drop(store);

    let dataset = DatasetArgs {
        dataset_manifest: feature_manifest_path,
        validation: args.validation.clone(),
    };
    let run_report = mission::execute_mission(
        &RunMissionArgs {
            db: db.clone(),
            mission_id: args.mission_id.clone(),
            engine: args.engine,
            seed: args.seed,
            feature_fields: args.feature_fields.clone(),
            offline_trace: None,
            max_new_iterations: Some(args.max_new_iterations),
            dataset: dataset.clone(),
        },
        false,
    )?;
    data_mission::write_json_atomic(&results_dir.join("mission-run.json"), &run_report)?;

    let store = AlphaStore::open(&db)?;
    let lineage = store.mission_lineage(&args.mission_id)?;
    let checkpoint = match store.get_checkpoint(&args.mission_id) {
        Ok(checkpoint) => Some(checkpoint),
        Err(StoreError::NotFound) => None,
        Err(error) => return Err(error.into()),
    };
    data_mission::write_json_atomic(
        &results_dir.join("mission-status.json"),
        &serde_json::json!({
            "mission": lineage.mission,
            "iteration_count": lineage.iterations.len(),
            "candidate_count": lineage.candidates.len(),
            "evaluation_count": lineage.evaluations.len(),
            "checkpoint": checkpoint,
        }),
    )?;
    data_mission::write_json_atomic(
        &results_dir.join("candidates.json"),
        &serde_json::json!({
            "mission_id": &args.mission_id,
            "candidates": lineage.candidates,
            "evaluations": lineage.evaluations,
        }),
    )?;
    let kept = governance::validated_walk_forward_candidates(&store, &args.mission_id)?;
    drop(store);
    std::fs::write(
        results_dir.join("kept-candidates.txt"),
        kept.iter()
            .map(|candidate| format!("{candidate}\n"))
            .collect::<String>(),
    )?;
    let mut sealed = BufWriter::new(File::create(results_dir.join("sealed-evaluations.jsonl"))?);
    for candidate_id in kept {
        let revision = governance::execute_evaluate(EvaluateArgs {
            db: db.clone(),
            mission_id: args.mission_id.clone(),
            candidate_id,
            model_root: None,
            dataset: dataset.clone(),
        })?;
        serde_json::to_writer(&mut sealed, &revision)?;
        sealed.write_all(b"\n")?;
    }
    sealed.flush()?;

    let bundle = args.work_dir.join("results.zip");
    create_bundle(&args.work_dir, &bundle, [&results_dir, &artifact_dir])?;
    let bundle_bytes = bundle.metadata()?.len();
    let bundle_sha256 = sha256_file(&bundle)?;
    publish_result(&client, &args.result_put_url, &bundle)?;
    print_json(&ExecutionReport {
        mission_id: &args.mission_id,
        engine: engine_name(args.engine),
        bundle_bytes,
        bundle_sha256,
    })
}

fn validate_args(args: &ExecuteMissionArgs) -> anyhow::Result<()> {
    if !matches!(args.engine, EngineChoice::Mcts | EngineChoice::Bayesian) {
        bail!(
            "unsupported durable Mission engine: {}",
            engine_name(args.engine)
        );
    }
    if args.work_dir.as_os_str().is_empty()
        || [
            args.feature_url.as_str(),
            args.materialization_url.as_str(),
            args.result_put_url.as_str(),
            args.data_mission_id.as_str(),
            args.mission_id.as_str(),
        ]
        .iter()
        .any(|value| value.trim().is_empty())
        || args.feature_fields.is_empty()
        || args
            .feature_fields
            .iter()
            .any(|field| field.trim().is_empty())
    {
        bail!("Mission execution paths, ids, URLs, and feature fields are required");
    }
    Ok(())
}

fn validate_materialization(
    materialization: &Materialization,
    feature_sha256: &str,
    validation: &ValidationArgs,
) -> anyhow::Result<()> {
    if materialization.dataset_kind != MATERIALIZATION_KIND
        || materialization.schema_version != MATERIALIZATION_SCHEMA
    {
        bail!("materialization kind or schema is unsupported");
    }
    if materialization.symbol.trim().is_empty()
        || !matches!(materialization.market.as_str(), "spot" | "usdm")
        || materialization.source_revision.trim().is_empty()
        || materialization.source_segments.is_empty()
        || materialization.bucket_ms == 0
        || materialization.label_horizon_buckets == 0
    {
        bail!("materialization lineage is incomplete");
    }
    if materialization.bucket_ms != validation.observation_frequency_millis
        || materialization.label_horizon_buckets != validation.label_horizon_buckets
    {
        bail!("evaluation label horizon or frequency does not match the materialization");
    }
    let artifact_sha256 = normalized_sha256("feature artifact", &materialization.artifact_sha256)?;
    for segment in &materialization.source_segments {
        normalized_sha256("source segment", &segment.sha256)?;
    }
    if artifact_sha256 != feature_sha256 {
        bail!("PIT feature artifact does not match materialization");
    }
    Ok(())
}

fn normalized_sha256(label: &str, value: &str) -> anyhow::Result<String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} SHA256 is invalid");
    }
    Ok(value)
}

fn fetch_to_file(
    client: &Client,
    source: &str,
    destination: &Path,
    max_bytes: u64,
) -> anyhow::Result<(u64, String)> {
    let mut reader: Box<dyn Read> =
        if source.starts_with("http://") || source.starts_with("https://") {
            let response = client.get(source).send()?.error_for_status()?;
            if response
                .content_length()
                .is_some_and(|length| length > max_bytes)
            {
                bail!("source exceeds the allowed size");
            }
            Box::new(response)
        } else {
            let path = Path::new(source.strip_prefix("file://").unwrap_or(source));
            let file = File::open(path)
                .with_context(|| format!("failed to open local source {}", path.display()))?;
            if file.metadata()?.len() > max_bytes {
                bail!("source exceeds the allowed size");
            }
            Box::new(file)
        };
    let temporary = destination.with_extension("tmp");
    let mut output = File::create(&temporary)?;
    let bytes = std::io::copy(&mut reader.by_ref().take(max_bytes + 1), &mut output)?;
    output.sync_all()?;
    if bytes > max_bytes {
        let _ = std::fs::remove_file(&temporary);
        bail!("source exceeds the allowed size");
    }
    std::fs::rename(&temporary, destination)?;
    Ok((bytes, sha256_file(destination)?))
}

pub(crate) fn create_bundle<'a>(
    work_dir: &Path,
    bundle: &Path,
    roots: impl IntoIterator<Item = &'a PathBuf>,
) -> anyhow::Result<()> {
    let mut files = Vec::new();
    for root in roots {
        collect_files(root, &mut files)?;
    }
    files.sort();
    let mut archive = ZipWriter::new(File::create(bundle)?);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for path in files {
        let name = path
            .strip_prefix(work_dir)
            .with_context(|| format!("bundle path escapes work directory: {}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        archive.start_file(name, options)?;
        std::io::copy(&mut File::open(path)?, &mut archive)?;
    }
    archive.finish()?.sync_all()?;
    Ok(())
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = entry.path().symlink_metadata()?;
        if metadata.file_type().is_symlink() {
            bail!("bundle input cannot contain symbolic links");
        }
        if metadata.is_dir() {
            collect_files(&entry.path(), files)?;
        } else if metadata.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

pub(crate) fn publish_result(
    client: &Client,
    destination: &str,
    bundle: &Path,
) -> anyhow::Result<()> {
    if destination.starts_with("http://") || destination.starts_with("https://") {
        client
            .put(destination)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/zip"))
            .header("x-oss-forbid-overwrite", "true")
            .body(File::open(bundle)?)
            .send()?
            .error_for_status()?;
        return Ok(());
    }
    let path = Path::new(destination.strip_prefix("file://").unwrap_or(destination));
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut output = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!("result destination already exists: {}", path.display())
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to create result {}", path.display()))
        }
    };
    let publication =
        std::io::copy(&mut File::open(bundle)?, &mut output).and_then(|_| output.sync_all());
    if let Err(error) = publication {
        let _ = std::fs::remove_file(path);
        return Err(error.into());
    }
    Ok(())
}

pub(crate) fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest)?;
    Ok(hex::encode(digest.finalize()))
}

pub(crate) fn configured_sibling_binary(environment: &str, name: &str) -> anyhow::Result<PathBuf> {
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
            "configured sibling binary does not exist: {}",
            path.display()
        );
    }
    Ok(path)
}

fn engine_name(engine: EngineChoice) -> &'static str {
    match engine {
        EngineChoice::Gp => "gp",
        EngineChoice::Mcts => "mcts",
        EngineChoice::Bayesian => "bayesian",
        EngineChoice::OfflineRl => "offline-rl",
        EngineChoice::Llm => "llm",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ValidationArgs;
    use chrono::{Duration as ChronoDuration, Utc};
    use hft_collector::{DataModality, PointInTimeFeatureRow};
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn execute_rejects_feature_hash_mismatch() {
        let fixture = fixture("hash-mismatch");
        let mut materialization = fixture.materialization;
        materialization["artifact_sha256"] = serde_json::json!("0".repeat(64));
        std::fs::write(
            &fixture.materialization_path,
            serde_json::to_vec_pretty(&materialization).unwrap(),
        )
        .unwrap();
        let mut args = fixture.args;
        args.materialization_sha256 = sha256_file(&fixture.materialization_path).unwrap();

        let error = execute(args).unwrap_err();

        assert!(error
            .to_string()
            .contains("PIT feature artifact does not match materialization"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_publishes_once_and_refuses_overwrite() {
        let fixture = fixture("immutable-result");
        execute(fixture.args.clone()).unwrap();
        assert!(fixture.result_path.is_file());

        let mut second_args = fixture.args;
        second_args.work_dir = fixture.root.join("work-2");
        second_args.data_mission_id = "data-2".to_string();
        second_args.mission_id = "mission-2".to_string();
        let error = execute(second_args).unwrap_err();

        assert!(error
            .to_string()
            .contains("result destination already exists"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    struct Fixture {
        root: PathBuf,
        materialization_path: PathBuf,
        result_path: PathBuf,
        materialization: serde_json::Value,
        args: ExecuteMissionArgs,
    }

    fn fixture(name: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "alpha-mission-runner-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let feature_path = root.join("features.jsonl");
        let ingestion_time = Utc::now() - ChronoDuration::seconds(1);
        let rows = (0..160)
            .map(|index| {
                let event_time = ingestion_time - ChronoDuration::seconds(300 - index);
                PointInTimeFeatureRow {
                    event_time,
                    feature_available_time: event_time,
                    label_available_time: event_time + ChronoDuration::seconds(5),
                    ingestion_time,
                    symbol: "BTCUSDT".to_string(),
                    source_revisions: BTreeMap::from([(
                        "binance-usdm-lob".to_string(),
                        "revision-1".to_string(),
                    )]),
                    modalities: BTreeSet::from([DataModality::Lob]),
                    features: BTreeMap::from([
                        ("book_imbalance_top5".to_string(), index as f64 / 100.0),
                        ("ofi_top5".to_string(), (index as f64 / 10.0).sin()),
                    ]),
                    label: if index % 2 == 0 { 0.001 } else { -0.0005 },
                }
            })
            .collect::<Vec<_>>();
        let mut feature_bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut feature_bytes, &row).unwrap();
            feature_bytes.push(b'\n');
        }
        std::fs::write(&feature_path, &feature_bytes).unwrap();
        let feature_sha256 = hex::encode(Sha256::digest(&feature_bytes));
        let materialization = serde_json::json!({
            "dataset_kind": MATERIALIZATION_KIND,
            "schema_version": MATERIALIZATION_SCHEMA,
            "mission_id": "materialize-1",
            "symbol": "BTCUSDT",
            "market": "usdm",
            "bucket_ms": 1000,
            "label_horizon_buckets": 5,
            "top_depth": 5,
            "source_revision": "revision-1",
            "source_segments": [{
                "path": "segment.jsonl.zst",
                "sha256": "1".repeat(64),
                "start_received_at_ns": 1,
                "end_received_at_ns": 2,
                "events": 1
            }],
            "rows": 160,
            "first_event_time": ingestion_time - ChronoDuration::seconds(300),
            "last_event_time": ingestion_time - ChronoDuration::seconds(141),
            "artifact_path": "features.jsonl",
            "artifact_sha256": feature_sha256,
            "created_at": ingestion_time
        });
        let materialization_path = root.join("materialization.json");
        std::fs::write(
            &materialization_path,
            serde_json::to_vec_pretty(&materialization).unwrap(),
        )
        .unwrap();
        let result_path = root.join("result.zip");
        let args = ExecuteMissionArgs {
            work_dir: root.join("work-1"),
            feature_url: feature_path.to_string_lossy().into_owned(),
            materialization_url: materialization_path.to_string_lossy().into_owned(),
            materialization_sha256: sha256_file(&materialization_path).unwrap(),
            result_put_url: result_path.to_string_lossy().into_owned(),
            data_mission_id: "data-1".to_string(),
            mission_id: "mission-1".to_string(),
            engine: EngineChoice::Mcts,
            feature_fields: vec!["book_imbalance_top5".to_string(), "ofi_top5".to_string()],
            seed: 7,
            max_candidates: 1,
            max_expansions: 1,
            max_seconds: 5,
            max_new_iterations: 1,
            objective: "test objective".to_string(),
            hypothesis_scope: "test scope".to_string(),
            validation: ValidationArgs {
                initial_train_rows: 40,
                validation_rows: 30,
                fold_count: 2,
                purge_rows: 5,
                embargo_rows: 1,
                sealed_holdout_rows: 30,
                fee_bps: 1.0,
                funding_bps: 0.0,
                latency_bps: 0.5,
                label_horizon_buckets: 5,
                observation_frequency_millis: 1_000,
            },
        };
        Fixture {
            root,
            materialization_path,
            result_path,
            materialization,
            args,
        }
    }
}
