use crate::{
    cli::{
        print_json, DatasetArgs, EngineChoice, EvaluateArgs, ExecuteMissionArgs, RunMissionArgs,
        ValidationArgs,
    },
    data_mission, governance, mission,
};
use alpha_domain::{
    EvaluationCostsV1, EvaluationLabelSpecV1, MissionCompletionPolicy, MissionStatus,
    ResearchMission, SearchBudget, ValidatorMode,
};
use alpha_store::{AlphaStore, StoreError};
use anyhow::{bail, Context};
use chrono::Utc;
use hft_research_manifest::{
    CexReplaySnapshotV1, ManifestId, BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2,
};
use reqwest::{
    blocking::Client,
    header::{HeaderValue, CONTENT_TYPE},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

const MATERIALIZATION_KIND: &str = "lob_point_in_time_materialization";
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
    top_depth: usize,
    first_event_time: chrono::DateTime<Utc>,
    last_event_time: chrono::DateTime<Utc>,
    snapshot: CexReplaySnapshotV1,
    snapshot_sha256: String,
}

#[derive(Debug, Deserialize)]
struct SourceSegment {
    sha256: String,
    collector_manifest_sha256: String,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    events: u64,
}

#[derive(Debug, Serialize)]
struct ExecutionReport<'a> {
    mission_id: &'a str,
    engine: &'static str,
    bundle_bytes: u64,
    bundle_sha256: String,
}

#[derive(Debug, Serialize)]
struct ExecutionModelEvidence {
    schema_version: &'static str,
    fee_bps: f64,
    funding_bps: f64,
    latency_bps: f64,
    additional_slippage_bps: f64,
    cross_spread: bool,
    turnover_definition: &'static str,
    queue_position_modeled: bool,
    partial_fills_modeled: bool,
    market_impact_modeled: bool,
    capacity_modeled: bool,
    capacity_gate_enabled: bool,
    capacity_gate_model: &'static str,
    position_notional_usd: f64,
    capacity_depth_levels: usize,
    max_book_depth_fraction: f64,
}

impl From<&EvaluationCostsV1> for ExecutionModelEvidence {
    fn from(costs: &EvaluationCostsV1) -> Self {
        let capacity_gate_enabled = costs.capacity_enabled();
        Self {
            schema_version: "execution_cost_model_v1",
            fee_bps: costs.fee_bps,
            funding_bps: costs.funding_bps,
            latency_bps: costs.latency_bps,
            additional_slippage_bps: costs.slippage_bps,
            cross_spread: costs.cross_spread,
            turnover_definition: "absolute_position_change; a full side flip has turnover 2",
            queue_position_modeled: false,
            partial_fills_modeled: false,
            market_impact_modeled: false,
            capacity_modeled: false,
            capacity_gate_enabled,
            capacity_gate_model: if capacity_gate_enabled {
                "same_side_top_n_depth_fraction"
            } else {
                "disabled"
            },
            position_notional_usd: costs.position_notional_usd,
            capacity_depth_levels: costs.capacity_depth_levels,
            max_book_depth_fraction: costs.max_book_depth_fraction,
        }
    }
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
    let evaluation_protocol = args
        .validation
        .evaluation_protocol(&EvaluationLabelSpecV1 {
            horizon_buckets: materialization.label_horizon_buckets,
            observation_frequency_millis: materialization.bucket_ms,
        })?;

    let db = results_dir.join("alpha.duckdb");
    let feature_manifest_path = results_dir.join("feature-manifest.json");
    let dataset_manifest_path = results_dir.join("cex-replay-dataset-manifest.json");
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
    let dataset_manifest = data_mission::admit_cex_replay_dataset(
        &mut store,
        &feature_manifest,
        &materialization.snapshot,
    )?;
    data_mission::write_json_atomic(&feature_manifest_path, &feature_manifest)?;
    data_mission::write_json_atomic(&dataset_manifest_path, &dataset_manifest)?;
    data_mission::write_json_atomic(
        &results_dir.join("data-import.json"),
        &serde_json::json!({
            "manifest": &dataset_manifest,
            "manifest_path": &dataset_manifest_path,
            "feature_manifest": &feature_manifest,
            "feature_manifest_path": &feature_manifest_path,
        }),
    )?;
    std::fs::copy(
        &materialization_path,
        results_dir.join("materialization.json"),
    )?;
    data_mission::write_json_atomic(
        &results_dir.join("execution-model.json"),
        &ExecutionModelEvidence::from(&evaluation_protocol.costs),
    )?;

    let now = Utc::now();
    let research_mission = ResearchMission {
        mission_id: args.mission_id.clone(),
        objective: args.objective.clone(),
        hypothesis_scope: args.hypothesis_scope.clone(),
        mutable_scope: vec!["factor_ast".to_string()],
        dataset_manifest_id: ManifestId::new(dataset_manifest.manifest_id.clone())?,
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
        dataset_manifest: dataset_manifest_path,
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
    mission::validate_live_formula_engine(args.engine)?;
    if !matches!(args.engine, EngineChoice::Mcts) {
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
    mission::validate_live_feature_fields(&args.feature_fields)?;
    Ok(())
}

fn validate_materialization(
    materialization: &Materialization,
    feature_sha256: &str,
    validation: &ValidationArgs,
) -> anyhow::Result<()> {
    materialization
        .snapshot
        .validate()
        .map_err(anyhow::Error::new)?;
    let snapshot_sha256 =
        normalized_sha256("CEX replay snapshot", &materialization.snapshot_sha256)?;
    if snapshot_sha256 != materialization.snapshot.sha256() {
        bail!("CEX replay snapshot SHA256 mismatch");
    }
    if materialization.dataset_kind != MATERIALIZATION_KIND
        || materialization.schema_version != BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2
    {
        bail!("materialization kind or schema is unsupported");
    }
    if materialization.symbol.trim().is_empty()
        || !matches!(materialization.market.as_str(), "spot" | "usdm")
        || materialization.source_revision.trim().is_empty()
        || materialization.source_segments.is_empty()
        || materialization.bucket_ms == 0
        || materialization.label_horizon_buckets == 0
        || materialization.top_depth == 0
    {
        bail!("materialization lineage is incomplete");
    }
    if materialization.snapshot.symbol != materialization.symbol
        || materialization.snapshot.instrument_type != materialization.market
        || materialization.snapshot.bucket_ms != materialization.bucket_ms
        || materialization.snapshot.label_horizon_buckets != materialization.label_horizon_buckets
        || materialization.snapshot.top_depth != materialization.top_depth
        || materialization.snapshot.first_event_time != materialization.first_event_time
        || materialization.snapshot.last_event_time != materialization.last_event_time
        || materialization.snapshot.feature_artifact_sha256 != materialization.artifact_sha256
        || materialization.snapshot.source_segments.len() != materialization.source_segments.len()
        || materialization
            .snapshot
            .source_segments
            .iter()
            .zip(&materialization.source_segments)
            .any(|(snapshot, report)| {
                snapshot.content_sha256 != report.sha256
                    || snapshot.manifest_sha256 != report.collector_manifest_sha256
                    || snapshot.start_received_at_ns != report.start_received_at_ns
                    || snapshot.end_received_at_ns != report.end_received_at_ns
                    || snapshot.events != report.events
            })
    {
        bail!("CEX replay snapshot does not match the materialization");
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

pub(crate) fn normalized_sha256(label: &str, value: &str) -> anyhow::Result<String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} SHA256 is invalid");
    }
    Ok(value)
}

pub(crate) fn fetch_to_file(
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
    let mut temporary = data_mission::temporary_output_file(destination, ".monday-fetch-")?;
    let bytes = std::io::copy(
        &mut reader.by_ref().take(max_bytes + 1),
        temporary.as_file_mut(),
    )?;
    temporary.as_file().sync_all()?;
    if bytes > max_bytes {
        bail!("source exceeds the allowed size");
    }
    data_mission::persist_output_file(temporary, destination, "fetched source")?;
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
    let temporary = data_mission::temporary_output_file(bundle, ".monday-bundle-")?;
    let mut archive = ZipWriter::new(temporary.reopen()?);
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
    let file = archive.finish()?;
    file.sync_all()?;
    drop(file);
    data_mission::persist_output_file(temporary, bundle, "bundle")?;
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
    let mut output = data_mission::temporary_output_file(path, ".monday-result-")?;
    std::io::copy(&mut File::open(bundle)?, output.as_file_mut())?;
    output.as_file().sync_all()?;
    match output.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!("result destination already exists: {}", path.display())
        }
        Err(error) => Err(error.error)
            .with_context(|| format!("atomically publish result to {}", path.display())),
    }
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
        io::BufRead,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn execute_rejects_features_that_have_no_live_formula_semantics() {
        let mut fixture = fixture("unsupported-live-fields");
        fixture.args.feature_fields = vec!["book_imbalance_top5".to_string()];

        let error = validate_args(&fixture.args).unwrap_err();

        assert_eq!(
            error.to_string(),
            "feature field book_imbalance_top5 is not live executable: unsupported live field: book_imbalance_top5"
        );
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_bayesian_window_search_before_materialization() {
        let mut fixture = fixture("bayesian-window-search");
        fixture.args.engine = EngineChoice::Bayesian;

        let error = validate_args(&fixture.args).unwrap_err();

        assert_eq!(
            error.to_string(),
            "Bayesian window search is research-only and cannot produce live-executable formulas"
        );
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_feature_hash_mismatch() {
        let fixture = fixture("hash-mismatch");
        let mut materialization = fixture.materialization;
        materialization["artifact_sha256"] = serde_json::json!("0".repeat(64));
        materialization["snapshot"]["feature_artifact_sha256"] = serde_json::json!("0".repeat(64));
        let snapshot: CexReplaySnapshotV1 =
            serde_json::from_value(materialization["snapshot"].clone()).unwrap();
        materialization["snapshot_sha256"] = serde_json::json!(snapshot.sha256());
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
    fn execute_rejects_snapshot_missing_aggregate_trade_modality() {
        let fixture = fixture("missing-aggregate-trade-modality");
        let mut materialization = fixture.materialization;
        materialization["snapshot"] = serde_json::json!({
            "schema_version": "cex-replay-snapshot-v1",
            "venue": "binance",
            "instrument_type": "usdm",
            "symbol": "BTCUSDT",
            "replay_clock": "received_at_ns",
            "required_modalities": ["lob"],
            "source_segments": [{
                "content_sha256": "1".repeat(64),
                "manifest_sha256": "2".repeat(64),
                "start_received_at_ns": 1,
                "end_received_at_ns": 2,
                "events": 1
            }],
            "first_event_time": materialization["first_event_time"].clone(),
            "last_event_time": materialization["last_event_time"].clone(),
            "feature_artifact_sha256": materialization["artifact_sha256"].clone(),
            "feature_availability_policy": "feature_available_time_equals_event_time",
            "bucket_ms": 1_000,
            "label_horizon_buckets": 5,
            "top_depth": 5
        });
        materialization["snapshot_sha256"] = serde_json::json!("0".repeat(64));
        std::fs::write(
            &fixture.materialization_path,
            serde_json::to_vec_pretty(&materialization).unwrap(),
        )
        .unwrap();
        let mut args = fixture.args;
        args.materialization_sha256 = sha256_file(&fixture.materialization_path).unwrap();

        let error = execute(args).unwrap_err();

        assert!(error.to_string().contains("required modalities"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_binds_snapshot_to_mission_dataset_identity() {
        let fixture = fixture("snapshot-dataset-identity");
        let expected = format!(
            "dataset-cex-replay-{}",
            fixture.materialization["snapshot_sha256"].as_str().unwrap()
        );

        execute(fixture.args.clone()).unwrap();

        let mission: ResearchMission = serde_json::from_slice(
            &std::fs::read(fixture.args.work_dir.join("results/mission.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(mission.dataset_manifest_id.as_str(), expected);
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_feature_available_after_decision_clock() {
        let mut fixture = fixture("feature-after-decision-clock");
        rewrite_features(&mut fixture, |row| {
            row.feature_available_time += ChronoDuration::milliseconds(1);
        });

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("feature availability does not match the CEX replay decision clock"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_snapshot_range_that_disagrees_with_feature_rows() {
        let mut fixture = fixture("snapshot-feature-range-mismatch");
        let first_event_time = serde_json::from_value::<chrono::DateTime<Utc>>(
            fixture.materialization["first_event_time"].clone(),
        )
        .unwrap()
            + ChronoDuration::seconds(1);
        fixture.materialization["first_event_time"] = serde_json::json!(first_event_time);
        fixture.materialization["snapshot"]["first_event_time"] =
            serde_json::json!(first_event_time);
        resign_materialization(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("feature time bounds do not match the CEX replay snapshot"));
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

    #[test]
    fn execute_records_explicit_taker_costs_in_evidence() {
        let mut fixture = fixture("explicit-taker-costs");
        fixture.args.validation.slippage_bps = 0.75;
        fixture.args.validation.cross_spread = true;
        fixture.args.validation.position_notional_usd = 10_000.0;
        fixture.args.validation.capacity_depth_levels = 5;
        fixture.args.validation.max_book_depth_fraction = 0.1;

        execute(fixture.args.clone()).unwrap();

        let evidence: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.args.work_dir.join("results/candidates.json")).unwrap(),
        )
        .unwrap();
        let costs =
            &evidence["evaluations"][0]["record"]["payload"]["evaluation_protocol"]["costs"];
        assert_eq!(costs["fee_bps"], 2.0);
        assert_eq!(costs["latency_bps"], 0.5);
        assert_eq!(costs["slippage_bps"], 0.75);
        assert_eq!(costs["cross_spread"], true);
        assert_eq!(costs["position_notional_usd"], 10_000.0);
        assert_eq!(costs["capacity_depth_levels"], 5);
        assert_eq!(costs["max_book_depth_fraction"], 0.1);
        assert!(
            evidence["evaluations"][0]["record"]["payload"]["metrics"]["folds"]
                .as_array()
                .unwrap()
                .iter()
                .all(|fold| fold["max_book_depth_fraction"].as_f64().unwrap() <= 0.1)
        );
        let execution_model: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.args.work_dir.join("results/execution-model.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(execution_model["additional_slippage_bps"], 0.75);
        assert_eq!(execution_model["queue_position_modeled"], false);
        assert_eq!(execution_model["partial_fills_modeled"], false);
        assert_eq!(execution_model["market_impact_modeled"], false);
        assert_eq!(execution_model["capacity_modeled"], false);
        assert_eq!(execution_model["capacity_gate_enabled"], true);
        assert_eq!(
            execution_model["capacity_gate_model"],
            "same_side_top_n_depth_fraction"
        );
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn baseline_execution_model_discloses_zero_slippage_and_unmodeled_microstructure() {
        let fixture = fixture("baseline-execution-model");
        let protocol = fixture
            .args
            .validation
            .evaluation_protocol(&EvaluationLabelSpecV1 {
                horizon_buckets: fixture.args.validation.label_horizon_buckets,
                observation_frequency_millis: fixture.args.validation.observation_frequency_millis,
            })
            .unwrap();
        let evidence = serde_json::to_value(ExecutionModelEvidence::from(&protocol.costs)).unwrap();

        assert_eq!(evidence["fee_bps"], 2.0);
        assert_eq!(evidence["latency_bps"], 0.5);
        assert_eq!(evidence["additional_slippage_bps"], 0.0);
        assert_eq!(evidence["cross_spread"], false);
        assert_eq!(evidence["queue_position_modeled"], false);
        assert_eq!(evidence["partial_fills_modeled"], false);
        assert_eq!(evidence["market_impact_modeled"], false);
        assert_eq!(evidence["capacity_modeled"], false);
        assert_eq!(evidence["capacity_gate_enabled"], false);
        assert_eq!(evidence["capacity_gate_model"], "disabled");
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn incomplete_capacity_inputs_fail_before_execution_evidence_is_written() {
        let mut fixture = fixture("incomplete-capacity-evidence");
        fixture.args.validation.position_notional_usd = 10_000.0;

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error.to_string().contains("evaluation protocol is invalid"));
        assert!(!fixture
            .args
            .work_dir
            .join("results/execution-model.json")
            .exists());
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn create_bundle_rejects_a_stale_symlink() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "alpha-bundle-symlink-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let results_dir = root.join("results");
        std::fs::create_dir_all(&results_dir).unwrap();
        std::fs::write(results_dir.join("summary.json"), "{}\n").unwrap();
        let protected_target = root.join("protected-target");
        std::fs::write(&protected_target, "preserve\n").unwrap();
        let bundle = root.join("checkpoint.zip");
        symlink(&protected_target, &bundle).unwrap();

        let error = create_bundle(&root, &bundle, [&results_dir])
            .expect_err("a symlinked bundle path must be rejected");

        assert!(error.to_string().contains("symbolic link"));
        assert_eq!(
            std::fs::read_to_string(protected_target).unwrap(),
            "preserve\n"
        );
        let metadata = std::fs::symlink_metadata(&bundle).unwrap();
        assert!(metadata.file_type().is_symlink());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn publish_result_does_not_leave_a_destination_when_the_bundle_is_missing() {
        let root = tempfile::tempdir().expect("create result publication test root");
        let destination = root.path().join("result.zip");
        let client = Client::builder().build().unwrap();

        publish_result(
            &client,
            &destination.to_string_lossy(),
            &root.path().join("missing-bundle.zip"),
        )
        .expect_err("a missing bundle must fail before publication");

        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn publish_result_rejects_a_symlinked_parent_directory() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "alpha-result-parent-symlink-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let bundle = root.join("bundle.zip");
        std::fs::write(&bundle, "bundle").unwrap();
        let protected_directory = root.join("protected-directory");
        std::fs::create_dir(&protected_directory).unwrap();
        let linked_parent = root.join("linked-parent");
        symlink(&protected_directory, &linked_parent).unwrap();
        let client = Client::builder().build().unwrap();

        let error = publish_result(
            &client,
            &linked_parent.join("result.zip").to_string_lossy(),
            &bundle,
        )
        .expect_err("a symlinked result parent must be rejected");

        assert!(error.to_string().contains("symbolic link"));
        assert!(std::fs::read_dir(protected_directory)
            .unwrap()
            .next()
            .is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    struct Fixture {
        root: PathBuf,
        feature_path: PathBuf,
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
                        ("ask_depth_top5".to_string(), 10.0),
                        ("bid_depth_top5".to_string(), 10.0),
                        ("book_imbalance".to_string(), index as f64 / 100.0),
                        ("mid_price".to_string(), 60_000.0),
                        ("spread_bps".to_string(), (index as f64 / 10.0).sin().abs()),
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
        let first_event_time = ingestion_time - ChronoDuration::seconds(300);
        let last_event_time = ingestion_time - ChronoDuration::seconds(141);
        let source_start_ns = u64::try_from(
            (first_event_time - ChronoDuration::seconds(1))
                .timestamp_nanos_opt()
                .unwrap(),
        )
        .unwrap();
        let source_end_ns = u64::try_from(ingestion_time.timestamp_nanos_opt().unwrap()).unwrap();
        let snapshot = CexReplaySnapshotV1 {
            schema_version: hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V1.to_string(),
            venue: "binance".to_string(),
            instrument_type: "usdm".to_string(),
            symbol: "BTCUSDT".to_string(),
            replay_clock: hft_research_manifest::CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
            required_modalities: BTreeSet::from([
                hft_research_manifest::CEX_MODALITY_LOB.to_string(),
                hft_research_manifest::CEX_MODALITY_AGGREGATE_TRADE.to_string(),
            ]),
            source_segments: vec![hft_research_manifest::CexReplaySegmentIdentity {
                content_sha256: "1".repeat(64),
                manifest_sha256: "2".repeat(64),
                start_received_at_ns: source_start_ns,
                end_received_at_ns: source_end_ns,
                events: 160,
            }],
            first_event_time,
            last_event_time,
            feature_artifact_sha256: feature_sha256.clone(),
            feature_availability_policy: hft_research_manifest::CEX_FEATURE_AVAILABILITY_POLICY
                .to_string(),
            bucket_ms: 1_000,
            label_horizon_buckets: 5,
            top_depth: 5,
        };
        let snapshot_sha256 = snapshot.sha256();
        let materialization = serde_json::json!({
            "dataset_kind": MATERIALIZATION_KIND,
            "schema_version": BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2,
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
                "collector_manifest_path": "segment.jsonl.zst.manifest.json",
                "collector_manifest_sha256": "2".repeat(64),
                "success_marker_path": "segment.jsonl.zst._SUCCESS",
                "success_marker_sha256": "3".repeat(64),
                "start_received_at_ns": source_start_ns,
                "end_received_at_ns": source_end_ns,
                "events": 160
            }],
            "rows": 160,
            "first_event_time": first_event_time,
            "last_event_time": last_event_time,
            "artifact_path": "features.jsonl",
            "artifact_sha256": feature_sha256,
            "snapshot": snapshot,
            "snapshot_sha256": snapshot_sha256,
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
            feature_fields: vec!["book_imbalance".to_string(), "spread_bps".to_string()],
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
                fee_bps: 2.0,
                funding_bps: 0.0,
                latency_bps: 0.5,
                slippage_bps: 0.0,
                cross_spread: false,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
                label_horizon_buckets: 5,
                observation_frequency_millis: 1_000,
            },
        };
        Fixture {
            root,
            feature_path,
            materialization_path,
            result_path,
            materialization,
            args,
        }
    }

    fn rewrite_features(fixture: &mut Fixture, mutate: impl Fn(&mut PointInTimeFeatureRow)) {
        let bytes = std::fs::read(&fixture.feature_path).unwrap();
        let mut rows = std::io::BufReader::new(bytes.as_slice())
            .lines()
            .map(|line| serde_json::from_str::<PointInTimeFeatureRow>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();
        rows.iter_mut().for_each(mutate);
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, &row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(&fixture.feature_path, &bytes).unwrap();
        let sha256 = hex::encode(Sha256::digest(&bytes));
        fixture.materialization["artifact_sha256"] = serde_json::json!(&sha256);
        fixture.materialization["snapshot"]["feature_artifact_sha256"] = serde_json::json!(&sha256);
        resign_materialization(fixture);
    }

    fn resign_materialization(fixture: &mut Fixture) {
        let snapshot: CexReplaySnapshotV1 =
            serde_json::from_value(fixture.materialization["snapshot"].clone()).unwrap();
        fixture.materialization["snapshot_sha256"] = serde_json::json!(snapshot.sha256());
        std::fs::write(
            &fixture.materialization_path,
            serde_json::to_vec_pretty(&fixture.materialization).unwrap(),
        )
        .unwrap();
        fixture.args.materialization_sha256 = sha256_file(&fixture.materialization_path).unwrap();
    }
}
