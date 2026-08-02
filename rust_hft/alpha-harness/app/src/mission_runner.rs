use crate::{
    cli::{
        print_json, DatasetArgs, EngineChoice, ExecuteMissionArgs, RunMissionArgs, ValidationArgs,
    },
    data_mission, governance, mission,
};
use alpha_domain::{
    canonical_json_hash, CexResearchMissionArtifactV1, EvaluationCostsV1, MissionCompletionPolicy,
    MissionStatus, ResearchMission, ValidatorMode,
};
use alpha_store::{AlphaStore, RegistryRevision, StoreError};
use anyhow::{bail, Context};
use chrono::Utc;
use hft_research_manifest::{
    CexReplayDatasetManifestV1, CexReplaySnapshotV1, ManifestId,
    BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2,
};
use reqwest::{
    blocking::Client,
    header::{HeaderValue, CONTENT_TYPE},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

const MATERIALIZATION_KIND: &str = "lob_point_in_time_materialization";
const MAX_MISSION_BYTES: u64 = 4 * 1024 * 1024;
// ponytail: one Mission is capped at 1 GiB; raise this only when staged partitions exceed it.
const MAX_FEATURE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_MATERIALIZATION_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Materialization {
    dataset_kind: String,
    schema_version: String,
    mission_id: String,
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
    rebate_bps: f64,
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
            schema_version: "execution_cost_model_v2",
            fee_bps: costs.fee_bps,
            rebate_bps: costs.rebate_bps,
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
    let bundle = args.work_dir.join("results.zip");
    for path in [&input_dir, &artifact_dir, &results_dir, &bundle] {
        if path.try_exists()? {
            bail!(
                "Mission execution requires a fresh work directory; existing path: {}",
                path.display()
            );
        }
    }
    std::fs::create_dir_all(&input_dir)?;
    std::fs::create_dir_all(&artifact_dir)?;
    std::fs::create_dir_all(&results_dir)?;

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let mission_path = input_dir.join("mission.json");
    let feature_path = input_dir.join("features.jsonl");
    let materialization_path = input_dir.join("materialization.json");
    let (_, mission_sha256) =
        fetch_to_file(&client, &args.mission_url, &mission_path, MAX_MISSION_BYTES)?;
    if mission_sha256 != normalized_sha256("Mission", &args.mission_sha256)? {
        bail!("Mission artifact SHA256 mismatch");
    }
    let control_mission: CexResearchMissionArtifactV1 =
        serde_json::from_slice(&std::fs::read(&mission_path)?)
            .context("CEX Research Mission artifact is invalid JSON or schema")?;
    control_mission.validate()?;
    mission::validate_live_feature_fields(&control_mission.spec.feature_fields)?;
    let mission_id = control_mission.semantic_id()?;
    let validation = ValidationArgs::from_protocol(&control_mission.spec.evaluation_protocol);
    let engine = EngineChoice::Mcts;

    let (_, feature_sha256) =
        fetch_to_file(&client, &args.feature_url, &feature_path, MAX_FEATURE_BYTES)?;
    let (_, materialization_sha256) = fetch_to_file(
        &client,
        &args.materialization_url,
        &materialization_path,
        MAX_MATERIALIZATION_BYTES,
    )?;
    let materialization: Materialization =
        serde_json::from_slice(&std::fs::read(&materialization_path)?)
            .context("materialization manifest is invalid JSON")?;
    validate_materialization(&materialization, &feature_sha256, &validation)?;
    validate_mission_materialization_binding(
        &control_mission,
        &materialization,
        &materialization_sha256,
        &feature_sha256,
    )?;
    let evaluation_protocol = control_mission.spec.evaluation_protocol.clone();

    let db = results_dir.join("alpha.duckdb");
    let feature_manifest_path = results_dir.join("feature-manifest.json");
    let dataset_manifest_path = results_dir.join("cex-replay-dataset-manifest.json");
    let mut store = AlphaStore::open(&db)?;
    let feature_manifest = data_mission::import_and_register_features(
        &mut store,
        &control_mission.spec.data_mission_id,
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
    validate_mission_dataset_binding(&control_mission, &feature_manifest, &dataset_manifest)?;
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
    let semantic_mission = serde_json::json!({
        "schema_version": &control_mission.schema_version,
        "spec": &control_mission.spec,
    });
    store.put_registry_revision(&RegistryRevision {
        revision_id: mission_id.clone(),
        registry_kind: "cex_research_mission".to_string(),
        asset_id: control_mission.spec.instrument.symbol.clone(),
        parent_revision_id: Some(dataset_manifest.manifest_id.clone()),
        payload: semantic_mission,
        created_at: now,
    })?;
    data_mission::write_json_atomic(
        &results_dir.join("control-plane-mission.json"),
        &control_mission,
    )?;
    data_mission::write_json_atomic(
        &results_dir.join("mission-admission.json"),
        &serde_json::json!({
            "schema_version": &control_mission.schema_version,
            "mission_id": &mission_id,
            "mission_artifact_sha256": &mission_sha256,
            "dataset_manifest_id": &dataset_manifest.manifest_id,
        }),
    )?;
    let research_mission = ResearchMission {
        mission_id: mission_id.clone(),
        objective: control_mission.spec.objective.clone(),
        hypothesis_scope: control_mission
            .spec
            .hypotheses
            .iter()
            .map(|hypothesis| hypothesis.hypothesis_id.as_str())
            .collect::<Vec<_>>()
            .join(","),
        mutable_scope: vec!["factor_ast".to_string()],
        dataset_manifest_id: ManifestId::new(dataset_manifest.manifest_id.clone())?,
        baseline_artifact_id: None,
        validation_mode: ValidatorMode::MissionValidator,
        validator_spec: serde_json::json!({
            "multiple_testing_trials": control_mission.spec.search.budget.max_candidates
        }),
        search_budget: control_mission.spec.search.budget.clone(),
        completion_policy: MissionCompletionPolicy::default(),
        prompt_snapshot_id: None,
        search_policy_snapshot_id: control_mission.spec.policies.subset_search.id.clone(),
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
        validation,
    };
    let run_args = RunMissionArgs {
        db: db.clone(),
        mission_id: mission_id.clone(),
        engine,
        seed: control_mission.spec.search.seed,
        feature_fields: control_mission.spec.feature_fields.clone(),
        offline_trace: None,
        max_new_iterations: Some(control_mission.spec.search.max_new_iterations),
        dataset: dataset.clone(),
    };
    let mut run_report = mission::execute_mission(&run_args, false)?;
    while run_report.status == MissionStatus::Paused {
        let previous_iterations = run_report.total_iterations;
        run_report = mission::execute_mission(&run_args, true)?;
        if run_report.total_iterations <= previous_iterations
            && run_report.status == MissionStatus::Paused
        {
            bail!("resumed MCTS mission made no progress");
        }
    }
    data_mission::write_json_atomic(&results_dir.join("mission-run.json"), &run_report)?;

    let store = AlphaStore::open(&db)?;
    let lineage = store.mission_lineage(&mission_id)?;
    let checkpoint = match store.get_checkpoint(&mission_id) {
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
            "mission_id": &mission_id,
            "candidates": lineage.candidates,
            "evaluations": lineage.evaluations,
        }),
    )?;
    let selected = governance::selected_walk_forward_candidate(&store, &mission_id)?;
    drop(store);
    std::fs::write(
        results_dir.join("kept-candidates.txt"),
        selected
            .iter()
            .map(|candidate| format!("{candidate}\n"))
            .collect::<String>(),
    )?;
    create_bundle(&args.work_dir, &bundle, [&results_dir, &artifact_dir])?;
    let bundle_bytes = bundle.metadata()?.len();
    let bundle_sha256 = sha256_file(&bundle)?;
    publish_result(&client, &args.result_put_url, &bundle)?;
    print_json(&ExecutionReport {
        mission_id: &mission_id,
        engine: engine_name(engine),
        bundle_bytes,
        bundle_sha256,
    })
}

fn validate_args(args: &ExecuteMissionArgs) -> anyhow::Result<()> {
    if args.work_dir.as_os_str().is_empty()
        || [
            args.mission_url.as_str(),
            args.feature_url.as_str(),
            args.materialization_url.as_str(),
            args.result_put_url.as_str(),
        ]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        bail!("Mission execution paths and URLs are required");
    }
    normalized_sha256("Mission", &args.mission_sha256)?;
    Ok(())
}

fn validate_mission_materialization_binding(
    mission: &CexResearchMissionArtifactV1,
    materialization: &Materialization,
    materialization_sha256: &str,
    feature_sha256: &str,
) -> anyhow::Result<()> {
    let spec = &mission.spec;
    let snapshot_sha256 = materialization.snapshot.sha256();
    let partition_sha256 = canonical_json_hash(&materialization.snapshot.source_segments)?;
    if spec.instrument.venue.as_str() != materialization.snapshot.venue
        || spec.instrument.market.as_str() != materialization.market
        || spec.instrument.market.as_str() != materialization.snapshot.instrument_type
        || spec.instrument.symbol != materialization.symbol
        || spec.instrument.symbol != materialization.snapshot.symbol
        || spec.instrument.horizon.horizon_buckets != materialization.label_horizon_buckets
        || spec.instrument.horizon.observation_frequency_millis != materialization.bucket_ms
    {
        bail!("CEX Research Mission instrument or horizon does not match materialization");
    }
    if spec.data_mission_id != materialization.mission_id
        || spec.inputs.materialization.id != materialization.mission_id
        || spec.inputs.materialization.content_sha256 != materialization_sha256
        || spec.inputs.snapshot.id != format!("cex-replay-snapshot-{snapshot_sha256}")
        || spec.inputs.snapshot.content_sha256 != snapshot_sha256
        || spec.inputs.partition.id != format!("cex-replay-partition-{partition_sha256}")
        || spec.inputs.partition.content_sha256 != partition_sha256
        || spec.inputs.source.id != materialization.source_revision
        || spec.inputs.source.content_sha256 != materialization.source_revision
        || spec.inputs.feature.id != format!("dataset-{feature_sha256}")
        || spec.inputs.feature.content_sha256 != feature_sha256
    {
        bail!("CEX Research Mission input identities do not match materialization");
    }
    Ok(())
}

fn validate_mission_dataset_binding(
    mission: &CexResearchMissionArtifactV1,
    features: &hft_collector::FeatureDatasetManifest,
    dataset: &CexReplayDatasetManifestV1,
) -> anyhow::Result<()> {
    if mission.spec.inputs.feature.id != features.manifest_id
        || mission.spec.inputs.feature.content_sha256 != features.artifact_sha256
        || mission.spec.inputs.dataset.id != dataset.manifest_id
        || mission.spec.inputs.dataset.content_sha256 != canonical_json_hash(dataset)?
    {
        bail!("CEX Research Mission dataset identities do not match admitted manifests");
    }
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
    if materialization.mission_id.trim().is_empty()
        || materialization.symbol.trim().is_empty()
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
    use alpha_domain::{
        CexResearchContentRefV1, CexResearchEvidenceKindV1, CexResearchEvidenceRefV1,
        CexResearchFalsificationTestV1, CexResearchHoldoutStateV1, CexResearchHoldoutV1,
        CexResearchHypothesisTargetV1, CexResearchHypothesisV1, CexResearchInputBindingsV1,
        CexResearchInstrumentV1, CexResearchMarketV1, CexResearchMissionSpecV1,
        CexResearchOperationalMetadataV1, CexResearchPolicyBindingsV1, CexResearchSearchPlanV1,
        CexResearchVenueV1, EvaluationLabelSpecV1, SearchBudget, CEX_RESEARCH_MISSION_SCHEMA_V1,
    };
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
        fixture.mission.spec.feature_fields = vec!["book_imbalance_top5".to_string()];
        resign_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert_eq!(
            error.to_string(),
            "feature field book_imbalance_top5 is not live executable: unsupported live field: book_imbalance_top5"
        );
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_unknown_mission_schema_before_materialization() {
        let mut fixture = fixture("unknown-mission-schema");
        fixture.mission.schema_version = "cex-research-mission-v999".to_string();
        write_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error.to_string().contains("schema version is unsupported"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_tampered_mission_artifact_before_admission() {
        let fixture = fixture("tampered-mission-artifact");
        let mut bytes = std::fs::read(&fixture.mission_path).unwrap();
        bytes.push(b'\n');
        std::fs::write(&fixture.mission_path, bytes).unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("Mission artifact SHA256 mismatch"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_cross_instrument_mission_before_admission() {
        let mut fixture = fixture("cross-instrument-mission");
        fixture.mission.spec.instrument.symbol = "ETHUSDT".to_string();
        write_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("instrument or horizon does not match materialization"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_prediction_market_fields_before_admission() {
        let mut fixture = fixture("cross-lane-mission");
        let mut value = serde_json::to_value(&fixture.mission).unwrap();
        value["task_capability"] = serde_json::json!("btc_5m_backtest");
        value["cohort_manifest_id"] = serde_json::json!("prediction-cohort-1");
        value["settlement"] = serde_json::json!({"token_id": "yes-token"});
        std::fs::write(
            &fixture.mission_path,
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        fixture.args.mission_sha256 = sha256_file(&fixture.mission_path).unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(format!("{error:#}").contains("unknown field"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_agent_action_fields_before_admission() {
        let mut fixture = fixture("agent-action-mission");
        let mut value = serde_json::to_value(&fixture.mission).unwrap();
        value["spec"]["actions"] = serde_json::json!([
            "evaluate",
            "open_holdout",
            "create_deployment_envelope",
            "submit_order",
            "start_live_small"
        ]);
        std::fs::write(
            &fixture.mission_path,
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        fixture.args.mission_sha256 = sha256_file(&fixture.mission_path).unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(format!("{error:#}").contains("unknown field"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_exposed_holdout_from_resumed_search() {
        let mut fixture = fixture("resumed-search-holdout-evidence");
        fixture.mission.spec.evidence[0].kind = CexResearchEvidenceKindV1::ExposedHoldout;
        fixture.mission.spec.evidence[0].source_search_lineage_id =
            fixture.mission.spec.search_lineage_id.clone();
        fixture.mission.spec.evidence[0].holdout_id = Some("earlier-holdout-1".to_string());
        write_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("exposed holdout evidence cannot feed the same search"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_feature_hash_mismatch() {
        let mut fixture = fixture("hash-mismatch");
        fixture.materialization["artifact_sha256"] = serde_json::json!("0".repeat(64));
        fixture.materialization["snapshot"]["feature_artifact_sha256"] =
            serde_json::json!("0".repeat(64));
        let snapshot: CexReplaySnapshotV1 =
            serde_json::from_value(fixture.materialization["snapshot"].clone()).unwrap();
        fixture.materialization["snapshot_sha256"] = serde_json::json!(snapshot.sha256());
        resign_materialization_outer(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("PIT feature artifact does not match materialization"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_snapshot_missing_aggregate_trade_modality() {
        let mut fixture = fixture("missing-aggregate-trade-modality");
        fixture.materialization["snapshot"] = serde_json::json!({
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
            "first_event_time": fixture.materialization["first_event_time"].clone(),
            "last_event_time": fixture.materialization["last_event_time"].clone(),
            "feature_artifact_sha256": fixture.materialization["artifact_sha256"].clone(),
            "feature_availability_policy": "feature_available_time_equals_event_time",
            "bucket_ms": 1_000,
            "label_horizon_buckets": 5,
            "top_depth": 5
        });
        fixture.materialization["snapshot_sha256"] = serde_json::json!("0".repeat(64));
        resign_materialization_outer(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

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
        assert_eq!(mission.mission_id, fixture.mission.semantic_id().unwrap());
        let admission: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.args.work_dir.join("results/mission-admission.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(admission["mission_id"], mission.mission_id);
        assert_eq!(
            admission["mission_artifact_sha256"],
            fixture.args.mission_sha256
        );
        assert_eq!(admission["dataset_manifest_id"], expected);
        let store = AlphaStore::open(fixture.args.work_dir.join("results/alpha.duckdb")).unwrap();
        let revision = store.get_registry_revision(&mission.mission_id).unwrap();
        assert_eq!(revision.revision_id, mission.mission_id);
        assert_eq!(revision.registry_kind, "cex_research_mission");
        assert_eq!(revision.asset_id, "BTCUSDT");
        assert_eq!(
            revision.parent_revision_id.as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            revision.payload,
            serde_json::json!({
                "schema_version": &fixture.mission.schema_version,
                "spec": &fixture.mission.spec,
            })
        );
        drop(store);
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
    fn execute_rejects_feature_source_revision_not_bound_to_snapshot_segments() {
        let mut fixture = fixture("forged-feature-source-revision");
        let forged_revision = "9".repeat(64);
        rewrite_features(&mut fixture, |row| {
            row.source_revisions
                .insert("binance-usdm-lob".to_string(), forged_revision.clone());
        });
        fixture.materialization["source_revision"] = serde_json::json!(forged_revision);
        resign_materialization(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("feature source revision does not match the CEX replay snapshot"));
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
    fn execute_rejects_label_availability_past_snapshot_segment_end() {
        let mut fixture = fixture("label-past-snapshot-segment-end");
        let last_event_time = serde_json::from_value::<chrono::DateTime<Utc>>(
            fixture.materialization["last_event_time"].clone(),
        )
        .unwrap();
        let source_end_ns = u64::try_from(
            (last_event_time + ChronoDuration::seconds(5))
                .timestamp_nanos_opt()
                .unwrap(),
        )
        .unwrap();
        fixture.materialization["source_segments"][0]["end_received_at_ns"] =
            serde_json::json!(source_end_ns);
        fixture.materialization["snapshot"]["source_segments"][0]["end_received_at_ns"] =
            serde_json::json!(source_end_ns);
        rewrite_features(&mut fixture, |row| {
            if row.event_time == last_event_time {
                row.label_available_time += ChronoDuration::nanoseconds(1);
            }
        });

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("feature label availability is outside the CEX replay snapshot"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_publishes_once_and_refuses_overwrite() {
        let fixture = fixture("immutable-result");
        execute(fixture.args.clone()).unwrap();
        assert!(fixture.result_path.is_file());

        let mut second_args = fixture.args;
        second_args.work_dir = fixture.root.join("work-2");
        let error = execute(second_args).unwrap_err();

        assert!(error
            .to_string()
            .contains("result destination already exists"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_records_explicit_taker_costs_in_evidence() {
        let mut fixture = fixture("explicit-taker-costs");
        fixture.mission.spec.evaluation_protocol.costs.rebate_bps = 0.25;
        fixture.mission.spec.evaluation_protocol.costs.slippage_bps = 0.75;
        fixture.mission.spec.evaluation_protocol.costs.cross_spread = true;
        fixture
            .mission
            .spec
            .evaluation_protocol
            .costs
            .position_notional_usd = 10_000.0;
        fixture
            .mission
            .spec
            .evaluation_protocol
            .costs
            .capacity_depth_levels = 5;
        fixture
            .mission
            .spec
            .evaluation_protocol
            .costs
            .max_book_depth_fraction = 0.1;
        resign_mission(&mut fixture);
        execute(fixture.args.clone()).unwrap();

        let evidence: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.args.work_dir.join("results/candidates.json")).unwrap(),
        )
        .unwrap();
        let costs =
            &evidence["evaluations"][0]["record"]["payload"]["evaluation_protocol"]["costs"];
        assert_eq!(costs["fee_bps"], 2.0);
        assert_eq!(costs["rebate_bps"], 0.25);
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
        assert!(
            evidence["evaluations"][0]["record"]["payload"]["metrics"]["total_turnover"]
                .as_f64()
                .unwrap()
                > 0.0
        );
        let execution_model: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.args.work_dir.join("results/execution-model.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(execution_model["schema_version"], "execution_cost_model_v2");
        assert_eq!(execution_model["rebate_bps"], 0.25);
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
        assert!(!fixture
            .args
            .work_dir
            .join("results/sealed-evaluations.jsonl")
            .exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_stops_before_sealed_holdout() {
        let fixture = fixture("sealed-holdout-remains-closed");

        execute(fixture.args.clone()).unwrap();

        let results = fixture.args.work_dir.join("results");
        assert!(results.join("mission-admission.json").exists());
        assert!(!results.join("sealed-evaluations.jsonl").exists());
        assert!(!results.join("mcts-research-receipt.json").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_preserves_prior_holdout_evidence_in_reused_work_dir() {
        let fixture = fixture("preserve-prior-holdout");
        let results = fixture.args.work_dir.join("results");
        let sealed = results.join("sealed-evaluations.jsonl");
        let receipt = results.join("mcts-research-receipt.json");
        std::fs::create_dir_all(&results).unwrap();
        std::fs::write(&sealed, b"sealed-evidence\n").unwrap();
        std::fs::write(&receipt, b"sealed-receipt\n").unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error.to_string().contains("fresh work directory"));
        assert_eq!(std::fs::read(&sealed).unwrap(), b"sealed-evidence\n");
        assert_eq!(std::fs::read(&receipt).unwrap(), b"sealed-receipt\n");
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn baseline_execution_model_discloses_zero_slippage_and_unmodeled_microstructure() {
        let fixture = fixture("baseline-execution-model");
        let protocol = &fixture.mission.spec.evaluation_protocol;
        let evidence = serde_json::to_value(ExecutionModelEvidence::from(&protocol.costs)).unwrap();

        assert_eq!(evidence["fee_bps"], 2.0);
        assert_eq!(evidence["rebate_bps"], 0.0);
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
        fixture
            .mission
            .spec
            .evaluation_protocol
            .costs
            .position_notional_usd = 10_000.0;
        write_mission(&mut fixture);

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
        mission_path: PathBuf,
        materialization_path: PathBuf,
        result_path: PathBuf,
        mission: CexResearchMissionArtifactV1,
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
        let source_content_sha256 = "1".repeat(64);
        let source_revision =
            hft_collector::lob_archiver::source_revision([source_content_sha256.as_str()]);
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
                        source_revision.clone(),
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
                content_sha256: source_content_sha256.clone(),
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
            "mission_id": "data-1",
            "symbol": "BTCUSDT",
            "market": "usdm",
            "bucket_ms": 1000,
            "label_horizon_buckets": 5,
            "top_depth": 5,
            "source_revision": source_revision,
            "source_segments": [{
                "path": "segment.jsonl.zst",
                "sha256": source_content_sha256,
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
        let validation = ValidationArgs {
            initial_train_rows: 40,
            validation_rows: 30,
            fold_count: 2,
            purge_rows: 5,
            embargo_rows: 1,
            sealed_holdout_rows: 30,
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            slippage_bps: 0.0,
            cross_spread: false,
            position_notional_usd: 0.0,
            capacity_depth_levels: 0,
            max_book_depth_fraction: 0.0,
            label_horizon_buckets: 5,
            observation_frequency_millis: 1_000,
        };
        let evaluation_protocol = validation
            .evaluation_protocol(&EvaluationLabelSpecV1 {
                horizon_buckets: 5,
                observation_frequency_millis: 1_000,
            })
            .unwrap();
        let search = CexResearchSearchPlanV1 {
            seed: 7,
            budget: SearchBudget {
                max_candidates: 1,
                max_expansions: 1,
                max_tokens: 0,
                max_seconds: 5,
            },
            max_new_iterations: 1,
        };
        let reference = |id: &str, byte: char| CexResearchContentRefV1 {
            id: id.to_string(),
            content_sha256: byte.to_string().repeat(64),
        };
        let materialization_sha256 = sha256_file(&materialization_path).unwrap();
        let partition_sha256 = canonical_json_hash(&snapshot.source_segments).unwrap();
        let dataset =
            CexReplayDatasetManifestV1::new(format!("dataset-{feature_sha256}"), snapshot.clone())
                .unwrap();
        let mission = CexResearchMissionArtifactV1 {
            schema_version: CEX_RESEARCH_MISSION_SCHEMA_V1.to_string(),
            spec: CexResearchMissionSpecV1 {
                objective: "test objective".to_string(),
                search_lineage_id: "search-lineage-1".to_string(),
                data_mission_id: "data-1".to_string(),
                instrument: CexResearchInstrumentV1 {
                    venue: CexResearchVenueV1::Binance,
                    market: CexResearchMarketV1::Usdm,
                    symbol: "BTCUSDT".to_string(),
                    horizon: evaluation_protocol.labels.clone(),
                },
                hypotheses: vec![CexResearchHypothesisV1 {
                    hypothesis_id: "hypothesis-1".to_string(),
                    statement: "LOB imbalance predicts the next five buckets".to_string(),
                    target: CexResearchHypothesisTargetV1 {
                        name: "forward_mid_return".to_string(),
                        horizon: evaluation_protocol.labels.clone(),
                    },
                    required_feature_families: vec!["book_imbalance".to_string()],
                    required_template_families: vec!["signed_rolling_imbalance".to_string()],
                    falsification_tests: vec![CexResearchFalsificationTestV1 {
                        test_id: "rank-ic-positive".to_string(),
                        reject_when: "purged validation rank IC is non-positive".to_string(),
                    }],
                    source_evidence_ids: vec!["evidence-training-1".to_string()],
                }],
                inputs: CexResearchInputBindingsV1 {
                    dataset: CexResearchContentRefV1 {
                        id: dataset.manifest_id.clone(),
                        content_sha256: canonical_json_hash(&dataset).unwrap(),
                    },
                    snapshot: CexResearchContentRefV1 {
                        id: format!("cex-replay-snapshot-{snapshot_sha256}"),
                        content_sha256: snapshot_sha256.clone(),
                    },
                    partition: CexResearchContentRefV1 {
                        id: format!("cex-replay-partition-{partition_sha256}"),
                        content_sha256: partition_sha256,
                    },
                    source: CexResearchContentRefV1 {
                        id: source_revision.clone(),
                        content_sha256: source_revision.clone(),
                    },
                    feature: CexResearchContentRefV1 {
                        id: format!("dataset-{feature_sha256}"),
                        content_sha256: feature_sha256.clone(),
                    },
                    materialization: CexResearchContentRefV1 {
                        id: "data-1".to_string(),
                        content_sha256: materialization_sha256,
                    },
                },
                policies: CexResearchPolicyBindingsV1 {
                    gp: reference("gp-policy-1", '1'),
                    screening: reference("screening-policy-1", '2'),
                    baseline: reference("baseline-policy-1", '3'),
                    subset_search: CexResearchContentRefV1 {
                        id: "subset-search-policy-1".to_string(),
                        content_sha256: canonical_json_hash(&search).unwrap(),
                    },
                    weight: reference("weight-policy-1", '4'),
                    evaluation: CexResearchContentRefV1 {
                        id: "evaluation-policy-1".to_string(),
                        content_sha256: evaluation_protocol.content_hash().unwrap(),
                    },
                    replay: reference("replay-policy-1", '5'),
                    holdout: reference("holdout-policy-1", '6'),
                },
                evidence: vec![CexResearchEvidenceRefV1 {
                    evidence_id: "evidence-training-1".to_string(),
                    kind: CexResearchEvidenceKindV1::TrainingValidation,
                    source_mission_id: "earlier-mission-1".to_string(),
                    source_search_lineage_id: "earlier-search-lineage-1".to_string(),
                    artifact_sha256: "7".repeat(64),
                    signature: None,
                    holdout_id: None,
                }],
                feature_fields: vec!["book_imbalance".to_string(), "spread_bps".to_string()],
                search,
                evaluation_protocol,
                holdout: CexResearchHoldoutV1 {
                    holdout_id: "holdout-fresh-1".to_string(),
                    state: CexResearchHoldoutStateV1::Unopened,
                },
            },
            operational: CexResearchOperationalMetadataV1 {
                submitted_at: Some(Utc::now()),
            },
        };
        mission.validate().unwrap();
        let mission_path = root.join("mission.json");
        std::fs::write(&mission_path, serde_json::to_vec_pretty(&mission).unwrap()).unwrap();
        let result_path = root.join("result.zip");
        let args = ExecuteMissionArgs {
            work_dir: root.join("work-1"),
            mission_url: mission_path.to_string_lossy().into_owned(),
            mission_sha256: sha256_file(&mission_path).unwrap(),
            feature_url: feature_path.to_string_lossy().into_owned(),
            materialization_url: materialization_path.to_string_lossy().into_owned(),
            result_put_url: result_path.to_string_lossy().into_owned(),
        };
        Fixture {
            root,
            feature_path,
            mission_path,
            materialization_path,
            result_path,
            mission,
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

    fn write_mission(fixture: &mut Fixture) {
        std::fs::write(
            &fixture.mission_path,
            serde_json::to_vec_pretty(&fixture.mission).unwrap(),
        )
        .unwrap();
        fixture.args.mission_sha256 = sha256_file(&fixture.mission_path).unwrap();
    }

    fn resign_mission(fixture: &mut Fixture) {
        fixture.mission.spec.policies.subset_search.content_sha256 =
            canonical_json_hash(&fixture.mission.spec.search).unwrap();
        fixture.mission.spec.policies.evaluation.content_sha256 = fixture
            .mission
            .spec
            .evaluation_protocol
            .content_hash()
            .unwrap();
        write_mission(fixture);
    }

    fn rebind_mission_inputs(fixture: &mut Fixture) {
        let snapshot: CexReplaySnapshotV1 =
            serde_json::from_value(fixture.materialization["snapshot"].clone()).unwrap();
        let snapshot_sha256 = snapshot.sha256();
        let partition_sha256 = canonical_json_hash(&snapshot.source_segments).unwrap();
        let feature_sha256 = sha256_file(&fixture.feature_path).unwrap();
        let source_revision = fixture.materialization["source_revision"]
            .as_str()
            .unwrap()
            .to_string();
        let data_mission_id = fixture.materialization["mission_id"]
            .as_str()
            .unwrap()
            .to_string();
        let dataset =
            CexReplayDatasetManifestV1::new(format!("dataset-{feature_sha256}"), snapshot).unwrap();
        fixture.mission.spec.data_mission_id = data_mission_id.clone();
        fixture.mission.spec.inputs = CexResearchInputBindingsV1 {
            dataset: CexResearchContentRefV1 {
                id: dataset.manifest_id.clone(),
                content_sha256: canonical_json_hash(&dataset).unwrap(),
            },
            snapshot: CexResearchContentRefV1 {
                id: format!("cex-replay-snapshot-{snapshot_sha256}"),
                content_sha256: snapshot_sha256,
            },
            partition: CexResearchContentRefV1 {
                id: format!("cex-replay-partition-{partition_sha256}"),
                content_sha256: partition_sha256,
            },
            source: CexResearchContentRefV1 {
                id: source_revision.clone(),
                content_sha256: source_revision,
            },
            feature: CexResearchContentRefV1 {
                id: format!("dataset-{feature_sha256}"),
                content_sha256: feature_sha256,
            },
            materialization: CexResearchContentRefV1 {
                id: data_mission_id,
                content_sha256: sha256_file(&fixture.materialization_path).unwrap(),
            },
        };
        resign_mission(fixture);
    }

    fn resign_materialization_outer(fixture: &mut Fixture) {
        std::fs::write(
            &fixture.materialization_path,
            serde_json::to_vec_pretty(&fixture.materialization).unwrap(),
        )
        .unwrap();
        fixture.mission.spec.inputs.materialization.content_sha256 =
            sha256_file(&fixture.materialization_path).unwrap();
        write_mission(fixture);
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
        rebind_mission_inputs(fixture);
    }
}
