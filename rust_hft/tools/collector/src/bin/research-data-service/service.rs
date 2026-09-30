use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use hft_research_manifest::market_encoder::{
    bytes_digest, digest, MarketFeatureDatasetV1, MarketFeatureFrameV1, MarketTargetDatasetV1,
    MarketTargetFrameV1, MarketTrainingAnchorSetV1, MarketTrainingAnchorV1, FEATURE_PARQUET_SCHEMA,
    FEATURE_SCHEMA, TARGET_PARQUET_SCHEMA, TARGET_SCHEMA,
};
use hft_research_manifest::prepared_market::{
    validate_prepared_producer, write_feature_parquet_shard, write_target_parquet_shard,
    FeatureParquetReader, PreparedMarketGapV1, PreparedMarketReadyReceiptV2,
    PreparedMarketSeriesV1, PreparedMarketSourceV1, PreparedMarketViewV1, TargetParquetReader,
    PREPARED_MARKET_VIEW_SCHEMA,
};
use hft_research_manifest::sequence::{
    valid_sha256, SequenceInputSpecV1, SequenceShardV1, SequenceViewV1,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PAGE_ROWS: usize = 4096;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 32 * 1024;
const MAX_VIEW_ROWS: u64 = 14 * 86_400 + 512 + 30;
const MAX_RECEIPT_BYTES: usize = 256 * 1024;
const SOURCE_REVISION: &str = match option_env!("MONDAY_SOURCE_REVISION") {
    Some(value) => value,
    None => "source-unbound",
};

#[derive(Parser)]
#[command(name = "research-data-service", version = SOURCE_REVISION, about = "Verified incremental ClickHouse ingestion and reusable ACK dataset views")]
struct Args {
    #[arg(long)]
    state_root: PathBuf,
    #[arg(long, default_value = "http://monday-clickhouse:8123")]
    clickhouse_url: String,
    #[arg(long, default_value = "monday_analytics")]
    database: String,
    #[arg(long, default_value = "monday_reader")]
    user: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Consume one externally pinned canonical-export receipt. Never discovers raw OSS.
    Ingest {
        #[arg(long)]
        receipt: PathBuf,
        #[arg(long)]
        receipt_sha256: String,
        #[arg(long)]
        artifact_root: PathBuf,
    },
    /// Publish a seed-independent data receipt from the native verified materialization seam.
    Enqueue {
        #[arg(long)]
        campaign_inputs: PathBuf,
        #[arg(long)]
        campaign_inputs_sha256: String,
        #[arg(long)]
        materialization_receipt: PathBuf,
        #[arg(long)]
        materialization_receipt_sha256: String,
        #[arg(long)]
        run_root: PathBuf,
        #[arg(long)]
        artifact_root: PathBuf,
        #[arg(long)]
        admission: PathBuf,
        #[arg(long)]
        admission_sha256: String,
        #[arg(long)]
        expected_feature_start_received_at_ns: Option<u64>,
        #[arg(long)]
        expected_feature_end_received_at_ns: Option<u64>,
    },
    /// Process a bounded append-only receipt queue; advance the cursor only after verified commit.
    Drain {
        #[arg(long)]
        queue: PathBuf,
        #[arg(long)]
        queue_sha256: String,
        #[arg(long)]
        artifact_root: PathBuf,
        #[arg(long, default_value_t = 16)]
        max_partitions: usize,
    },
    /// Return ready/preparing/blocked; prepare once when all pinned source partitions are ready.
    Request {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        request_sha256: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRef {
    /// Relative to the controller's admitted artifact root; no URLs or traversal.
    file: String,
    sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowedRange {
    start_ms: i64,
    end_ms: i64,
}

/// Produced by the canonical input coordinator, which owns source sealing and grant admission.
/// A receipt is not a replacement for raw collector _SUCCESS verification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IngestReceipt {
    schema_version: String,
    sequence: u64,
    previous_receipt_sha256: Option<String>,
    features: ArtifactRef,
    targets: Option<ArtifactRef>,
    transform_sha256: String,
    /// Original canonical source/reference manifest identity (the export already pins it).
    source_manifest_sha256: String,
    source_manifest: ArtifactRef,
    allowed_ranges: Vec<AllowedRange>,
    /// Must be granted by the canonical coordinator; ordinary feature consumers cannot set it.
    pre_holdout_supervised: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DataAdmission {
    schema_version: String,
    transform_sha256: String,
    allowed_ranges: Vec<AllowedRange>,
    pre_holdout_supervised: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeInputRef {
    relative_path: String,
    object_url: String,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCampaignInputs {
    schema_version: String,
    run_id: String,
    source_revision: String,
    image_ref: String,
    mission_id: String,
    market: String,
    symbol: String,
    output_prefix: String,
    output_object_base_url: String,
    readback_scope: String,
    feature: NativeInputRef,
    materialization: NativeInputRef,
    replay_artifact: NativeInputRef,
    replay_manifest: NativeInputRef,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeMaterializationReceipt {
    schema_version: String,
    run_id: String,
    source_revision: String,
    image_ref: String,
    inventory_sha256: String,
    campaign_inputs_sha256: String,
    feature_sha256: String,
    materialization_sha256: String,
    replay_artifact_sha256: String,
    replay_manifest_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeMarketArtifact {
    file: String,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeMarketExport {
    schema_version: String,
    feature_start_received_at_ns: Option<u64>,
    feature_end_received_at_ns: Option<u64>,
    feature_sources: NativeMarketArtifact,
    features: NativeMarketArtifact,
    targets: NativeMarketArtifact,
}

impl IngestReceipt {
    fn validate(&self) -> Result<()> {
        if self.schema_version != "monday.market_ingest_receipt.v1"
            || self.sequence == 0
            || !valid_sha256(&self.transform_sha256)
            || !valid_sha256(&self.source_manifest_sha256)
            || self
                .previous_receipt_sha256
                .as_deref()
                .is_some_and(|h| !valid_sha256(h))
            || self.allowed_ranges.is_empty()
            || self.allowed_ranges.len() > 64
            || self.allowed_ranges.iter().any(|r| {
                r.start_ms < 0
                    || r.start_ms % 1000 != 0
                    || r.end_ms % 1000 != 0
                    || r.end_ms <= r.start_ms
            })
            || self
                .allowed_ranges
                .windows(2)
                .any(|r| r[0].end_ms > r[1].start_ms)
            || self.targets.is_some() != self.pre_holdout_supervised
        {
            bail!("invalid admitted ingest receipt, ranges or supervised binding");
        }
        validate_ref(&self.features)?;
        validate_ref(&self.source_manifest)?;
        if self.source_manifest.sha256 != self.source_manifest_sha256 {
            bail!("receipt source file and source identity differ");
        }
        if let Some(targets) = &self.targets {
            validate_ref(targets)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DataSource {
    feature_dataset_sha256: String,
    target_dataset_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DataRequest {
    schema_version: String,
    /// Ordered disjoint incremental partitions. The order itself is immutable identity.
    sources: Vec<DataSource>,
    transform_sha256: String,
    input: SequenceInputSpecV1,
    view: SequenceViewV1,
    anchor_end_ms: i64,
    /// False means no target query, target file, or target payload is accessible.
    purpose: String,
    /// Training anchor grid. Evaluation may read all frames without building an anchor index.
    qualified_anchors: bool,
}

impl DataRequest {
    fn validate(&self) -> Result<()> {
        self.input.validate().map_err(anyhow::Error::msg)?;
        self.view.validate().map_err(anyhow::Error::msg)?;
        let mut unique = BTreeSet::new();
        if self.schema_version != "monday.market_data_request.v1"
            || self.sources.is_empty()
            || self.sources.len() > 512
            || !valid_sha256(&self.transform_sha256)
            || self.sources.iter().any(|s| {
                !valid_sha256(&s.feature_dataset_sha256)
                    || !unique.insert(&s.feature_dataset_sha256)
                    || s.target_dataset_sha256
                        .as_deref()
                        .is_some_and(|h| !valid_sha256(h))
                    || s.target_dataset_sha256.is_some() != self.supervised()
            })
            || !matches!(
                self.purpose.as_str(),
                "label_free" | "pre_holdout_supervised"
            )
            || self.view.decision_start_ms - self.view.history_start_ms
                < (self.input.context_rows as i64 - 1) * 1000
            || self.anchor_end_ms <= self.view.decision_start_ms
            || self.anchor_end_ms > self.view.end_ms
            || self.anchor_end_ms % 1000 != 0
            || self.view.end_ms - self.view.history_start_ms > (14 * 86_400 + 512 + 30) * 1_000
            || self.anchor_end_ms - self.view.decision_start_ms > 14 * 86_400_000
        {
            bail!("invalid fixed DataRequest identity, causal bounds or source partition union");
        }
        Ok(())
    }
    fn identity(&self) -> Result<String> {
        self.validate()?;
        digest(self).map_err(anyhow::Error::msg)
    }
    fn supervised(&self) -> bool {
        self.purpose == "pre_holdout_supervised"
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    sequence: u64,
    receipt_sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueEntry {
    receipt: IngestReceipt,
    feature_dataset: MarketFeatureDatasetV1,
    target_dataset: Option<MarketTargetDatasetV1>,
    feature_content_sha256: String,
    target_content_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryRow {
    dataset_identity: String,
    dataset_kind: String,
    source_manifest_sha256: String,
    manifest_json: String,
    row_count: u64,
    content_sha256: String,
    first_observed_at_ms: i64,
    last_observed_at_ms: i64,
    materialization_state: String,
    materialization_version: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceSegment {
    path: PathBuf,
    sha256: String,
    collector_manifest_path: PathBuf,
    collector_manifest_sha256: String,
    success_marker_path: PathBuf,
    success_marker_sha256: String,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    events: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeatureSources {
    schema_version: String,
    market: String,
    symbol: String,
    replay_clock: String,
    source_revision: String,
    source_segments: Vec<SourceSegment>,
    feature_start_received_at_ns: Option<u64>,
    feature_end_received_at_ns: Option<u64>,
    first_feature_observed_at_ms: i64,
    last_feature_observed_at_ms: i64,
    first_dependency_received_at_ns: u64,
    last_dependency_received_at_ns: u64,
}
impl FeatureSources {
    fn validate(&self, features: &MarketFeatureDatasetV1) -> Result<()> {
        let mut segment_hashes = BTreeSet::new();
        let mut segment_paths = BTreeSet::new();
        let first = features
            .shards
            .first()
            .context("missing canonical feature coverage")?
            .first_observed_at_ms;
        let last = features
            .shards
            .last()
            .context("missing canonical feature coverage")?
            .last_observed_at_ms;
        if self.schema_version != "monday.market_feature_sources.v1"
            || self.market != "usdm"
            || self.symbol != features.symbol
            || self.replay_clock != "received_at_ns"
            || self.source_segments.is_empty()
            || self.source_segments.len() > 32_768
            || self.first_feature_observed_at_ms != first
            || self.last_feature_observed_at_ms != last
            || self.first_dependency_received_at_ns
                > u64::try_from(first)?
                    .checked_mul(1_000_000)
                    .context("source clock overflow")?
            || self.last_dependency_received_at_ns
                != u64::try_from(last)?
                    .checked_mul(1_000_000)
                    .context("source clock overflow")?
            || self.feature_start_received_at_ns.is_some_and(|clock| {
                u64::try_from(first)
                    .ok()
                    .and_then(|v| v.checked_mul(1_000_000))
                    .is_none_or(|v| v < clock)
            })
            || self
                .feature_end_received_at_ns
                .is_some_and(|clock| self.last_dependency_received_at_ns >= clock)
            || self.source_segments.iter().any(|s| {
                !valid_sha256(&s.sha256)
                    || !valid_sha256(&s.collector_manifest_sha256)
                    || !valid_sha256(&s.success_marker_sha256)
                    || s.success_marker_sha256 != bytes_digest(format!("{}\n", s.sha256).as_bytes())
                    || !segment_hashes.insert(&s.sha256)
                    || !segment_paths.insert(&s.path)
                    || s.events == 0
                    || s.end_received_at_ns < s.start_received_at_ns
                    || s.path.as_os_str().is_empty()
                    || s.collector_manifest_path.as_os_str().is_empty()
                    || s.success_marker_path.as_os_str().is_empty()
            })
            || self
                .source_segments
                .iter()
                .map(|s| s.start_received_at_ns)
                .min()
                .is_none_or(|t| t > self.first_dependency_received_at_ns)
            || self
                .source_segments
                .iter()
                .map(|s| s.end_received_at_ns)
                .max()
                .is_none_or(|t| t < self.last_dependency_received_at_ns)
            || self.source_revision
                != data::binance_lob_replay::source_revision(
                    self.source_segments.iter().map(|s| s.sha256.as_str()),
                )
        {
            bail!("canonical feature source/collector sealing metadata does not bind the export coverage");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadyReceipt {
    schema_version: String,
    producer_source_revision: String,
    producer_image: String,
    request_sha256: String,
    request: DataRequest,
    prepared_view_sha256: String,
    prepared_view: PreparedMarketViewV1,
    feature_manifest: ArtifactRef,
    target_manifest: Option<ArtifactRef>,
    qualified_anchors: Option<ArtifactRef>,
}

/// Physical cache observations are mutable certificates, never dataset identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheBlock {
    file: String,
    sha256: String,
    device: u64,
    inode: u64,
    bytes: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheCertification {
    schema_version: String,
    prepared_view_sha256: String,
    blocks: Vec<CacheBlock>,
}

pub fn run() -> Result<()> {
    let args = Args::parse();
    if std::env::var("MONDAY_ACK_DATA_PLANE").as_deref() != Ok("1") {
        bail!("data-plane execution requires the admitted ACK workload environment");
    }
    validate_identifier(&args.database)?;
    fs::create_dir_all(&args.state_root)?;
    let state_root = fs::canonicalize(&args.state_root)?;
    let client = ClickHouse::new(&args)?;
    tokio::runtime::Runtime::new()?.block_on(async {
        match &args.command {
            Command::Enqueue {campaign_inputs,campaign_inputs_sha256,materialization_receipt,materialization_receipt_sha256,run_root,artifact_root,admission,admission_sha256,expected_feature_start_received_at_ns,expected_feature_end_received_at_ns} => {
                let _owner=claim(&state_root.join("controller.lock"))?.context("another controller owns receipt queue")?;
                let admission:DataAdmission=read_pinned(admission,admission_sha256)?;
                let receipt=native_receipt(campaign_inputs,campaign_inputs_sha256,materialization_receipt,materialization_receipt_sha256,run_root,artifact_root,admission,*expected_feature_start_received_at_ns,*expected_feature_end_received_at_ns)?;
                let result=enqueue_receipt(&state_root,receipt)?;
                print_response(&result)?;
            }
            Command::Ingest { receipt, receipt_sha256, artifact_root } => {
                let _owner = claim(&state_root.join("controller.lock"))?.context("another controller owns ingestion")?;
                let receipt: IngestReceipt = read_pinned(receipt, receipt_sha256)?;
                let result = ingest(&client, &state_root, artifact_root, receipt).await?;
                print_response(&result)?;
            }
            Command::Drain { queue, queue_sha256, artifact_root, max_partitions } => {
                if !(1..=128).contains(max_partitions) { bail!("max-partitions must be in 1..=128"); }
                let _owner = claim(&state_root.join("controller.lock"))?.context("another controller owns ingestion")?;
                let receipts: Vec<IngestReceipt> = read_pinned(queue, queue_sha256)?;
                let path = state_root.join("queue-cursor.json");
                let mut cursor: Cursor = if path.exists() { read_bounded_json(&path)? } else { Cursor { sequence: 0, receipt_sha256: None } };
                validate_queue(&receipts, &cursor)?;
                let queued_sequence=receipts.last().map_or(cursor.sequence,|receipt|receipt.sequence);
                let mut committed = 0;
                let committed_sequence = cursor.sequence;
                for receipt in receipts.into_iter().filter(|r| r.sequence > committed_sequence).take(*max_partitions) {
                    let receipt_sha256 = digest(&receipt).map_err(anyhow::Error::msg)?;
                    ingest(&client, &state_root, artifact_root, receipt.clone()).await?;
                    cursor = Cursor { sequence: receipt.sequence, receipt_sha256: Some(receipt_sha256) };
                    replace_json(&path, &cursor)?;
                    committed += 1;
                }
                let pending_partitions=queued_sequence-cursor.sequence;
                print_response(&serde_json::json!({"state":if pending_partitions==0{"ready"}else{"preparing"},"committed_partitions":committed,"pending_partitions":pending_partitions,"cursor":cursor}))?;
            }
            Command::Request { request, request_sha256 } => {
                let request: DataRequest = read_pinned(request, request_sha256)?;
                request.validate()?;
                let response = request_view(&client, &state_root, request).await?;
                print_response(&response)?;
            }
        }
        Ok(())
    })
}

fn validate_queue(receipts: &[IngestReceipt], cursor: &Cursor) -> Result<()> {
    if receipts.len() > 1024
        || cursor
            .receipt_sha256
            .as_deref()
            .is_some_and(|h| !valid_sha256(h))
    {
        bail!("invalid bounded queue or cursor");
    }
    let mut previous = Cursor {
        sequence: 0,
        receipt_sha256: None,
    };
    let mut cursor_found = cursor.sequence == 0 && cursor.receipt_sha256.is_none();
    for receipt in receipts {
        receipt.validate()?;
        if receipt.sequence != previous.sequence + 1
            || receipt.previous_receipt_sha256 != previous.receipt_sha256
        {
            bail!("receipt queue is not an append-only contiguous hash chain");
        }
        previous = Cursor {
            sequence: receipt.sequence,
            receipt_sha256: Some(digest(receipt).map_err(anyhow::Error::msg)?),
        };
        if previous == *cursor {
            cursor_found = true;
        }
    }
    if !cursor_found {
        bail!(
            "queue no longer contains the committed cursor identity; use a reviewed recovery queue"
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn native_receipt(
    campaign_path: &Path,
    campaign_sha: &str,
    native_receipt_path: &Path,
    native_receipt_sha: &str,
    run_root: &Path,
    artifact_root: &Path,
    admission: DataAdmission,
    expected_feature_start_received_at_ns: Option<u64>,
    expected_feature_end_received_at_ns: Option<u64>,
) -> Result<IngestReceipt> {
    let campaign: NativeCampaignInputs = read_pinned(campaign_path, campaign_sha)?;
    let native: NativeMaterializationReceipt =
        read_pinned(native_receipt_path, native_receipt_sha)?;
    let run_root = fs::canonicalize(run_root)?;
    let artifact_root = fs::canonicalize(artifact_root)?;
    if admission.schema_version != "monday.market_data_admission.v1"
        || campaign.schema_version != "monday.cex_campaign_inputs.v1"
        || native.schema_version != "monday.cex_materialization_receipt.v1"
        || campaign.market != "usdm"
        || campaign.symbol != "SOLUSDT"
        || campaign.readback_scope != "same-mounted-ossfs-prefix"
        || campaign.run_id != native.run_id
        || campaign.source_revision != native.source_revision
        || campaign.image_ref != native.image_ref
        || campaign.run_id.is_empty()
        || campaign.mission_id.is_empty()
        || campaign.output_object_base_url.is_empty()
        || !campaign
            .image_ref
            .rsplit_once("@sha256:")
            .is_some_and(|(_, h)| valid_sha256(h))
        || native.campaign_inputs_sha256 != campaign_sha
        || native.feature_sha256 != campaign.feature.sha256
        || native.materialization_sha256 != campaign.materialization.sha256
        || native.replay_artifact_sha256 != campaign.replay_artifact.sha256
        || native.replay_manifest_sha256 != campaign.replay_manifest.sha256
        || !run_root.starts_with(&artifact_root)
    {
        bail!(
            "native materialization/Campaign receipts are not a pinned published SOL export pair"
        );
    }
    if run_root.strip_prefix(&artifact_root)?.to_string_lossy() != campaign.output_prefix {
        bail!("native publication prefix differs from the admitted artifact root");
    }
    for reference in [
        &campaign.feature,
        &campaign.materialization,
        &campaign.replay_artifact,
        &campaign.replay_manifest,
    ] {
        validate_ref(&ArtifactRef {
            file: reference.relative_path.clone(),
            sha256: reference.sha256.clone(),
        })?;
        if reference.object_url.is_empty() {
            bail!("native published artifact lacks its evidence URL");
        }
        resolve_ref(
            &run_root,
            &ArtifactRef {
                file: reference.relative_path.clone(),
                sha256: reference.sha256.clone(),
            },
        )?;
    }
    let inventory = run_root.join("receipts/frozen-inventory.env");
    if hash_file(&inventory)? != native.inventory_sha256 {
        bail!("native frozen inventory checksum differs from its published receipt");
    }
    let materialization_path = resolve_ref(
        &run_root,
        &ArtifactRef {
            file: campaign.materialization.relative_path.clone(),
            sha256: campaign.materialization.sha256.clone(),
        },
    )?;
    let report: serde_json::Value =
        read_pinned(&materialization_path, &campaign.materialization.sha256)?;
    if report
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        != Some(hft_research_manifest::BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7)
        || report
            .get("dataset_kind")
            .and_then(serde_json::Value::as_str)
            != Some("lob_point_in_time_materialization")
        || report.get("market").and_then(serde_json::Value::as_str) != Some("usdm")
        || report.get("symbol").and_then(serde_json::Value::as_str) != Some("SOLUSDT")
        || report.get("mission_id").and_then(serde_json::Value::as_str)
            != Some(campaign.mission_id.as_str())
        || report
            .get("artifact_sha256")
            .and_then(serde_json::Value::as_str)
            != Some(campaign.feature.sha256.as_str())
    {
        bail!("native canonical materialization report differs from its published receipt");
    }
    let snapshot: hft_research_manifest::CexReplaySnapshotV5 = serde_json::from_value(
        report
            .get("snapshot")
            .context("native report lacks its PIT snapshot")?
            .clone(),
    )?;
    snapshot.validate().map_err(anyhow::Error::new)?;
    if report
        .get("snapshot_sha256")
        .and_then(serde_json::Value::as_str)
        != Some(snapshot.sha256().as_str())
    {
        bail!("native PIT snapshot identity mismatch");
    }
    let export: NativeMarketExport = serde_json::from_value(
        report
            .get("market_encoder")
            .context("native report has no market encoder output")?
            .clone(),
    )?;
    validate_export_window(
        &export,
        expected_feature_start_received_at_ns,
        expected_feature_end_received_at_ns,
    )?;
    let export_root = materialization_path
        .parent()
        .context("native materialization parent missing")?;
    let to_reference = |item: &NativeMarketArtifact| -> Result<ArtifactRef> {
        let reference = ArtifactRef {
            file: item.file.clone(),
            sha256: item.sha256.clone(),
        };
        validate_ref(&reference)?;
        if Path::new(&item.file).components().count() != 1 {
            bail!("native market export filename is not a basename");
        }
        let path = resolve_ref(export_root, &reference)?;
        Ok(ArtifactRef {
            file: path
                .strip_prefix(&artifact_root)?
                .to_string_lossy()
                .into_owned(),
            sha256: item.sha256.clone(),
        })
    };
    let features = to_reference(&export.features)?;
    let sources = to_reference(&export.feature_sources)?;
    let feature: MarketFeatureDatasetV1 =
        read_pinned(&resolve_ref(&artifact_root, &features)?, &features.sha256)?;
    feature.validate().map_err(anyhow::Error::msg)?;
    let source: FeatureSources =
        read_pinned(&resolve_ref(&artifact_root, &sources)?, &sources.sha256)?;
    source.validate(&feature)?;
    if feature.digest().map_err(anyhow::Error::msg)? != features.sha256
        || feature.source_manifest_sha256 != sources.sha256
    {
        bail!("native numerical feature export is not content bound to its source manifest");
    }
    let targets = if admission.pre_holdout_supervised {
        Some(to_reference(&export.targets)?)
    } else {
        None
    };
    if let Some(reference) = &targets {
        let target: MarketTargetDatasetV1 =
            read_pinned(&resolve_ref(&artifact_root, reference)?, &reference.sha256)?;
        target.validate().map_err(anyhow::Error::msg)?;
        if target.feature_dataset_sha256 != features.sha256
            || target.digest().map_err(anyhow::Error::msg)? != reference.sha256
        {
            bail!("native target export differs from the admitted feature identity");
        }
    }
    let receipt = IngestReceipt {
        schema_version: "monday.market_ingest_receipt.v1".into(),
        sequence: 1,
        previous_receipt_sha256: None,
        features,
        targets,
        transform_sha256: admission.transform_sha256,
        source_manifest_sha256: sources.sha256.clone(),
        source_manifest: sources,
        allowed_ranges: admission.allowed_ranges,
        pre_holdout_supervised: admission.pre_holdout_supervised,
    };
    receipt.validate()?;
    Ok(receipt)
}

fn validate_export_window(
    export: &NativeMarketExport,
    start: Option<u64>,
    end: Option<u64>,
) -> Result<()> {
    if export.schema_version != "monday.market_encoder_export.v1"
        || export
            .feature_start_received_at_ns
            .zip(export.feature_end_received_at_ns)
            .is_some_and(|(start, end)| start >= end)
        || export.feature_start_received_at_ns != start
        || export.feature_end_received_at_ns != end
    {
        bail!("native market encoder optional feature window differs from the fixed publication recipe");
    }
    Ok(())
}
fn enqueue_receipt(state_root: &Path, mut receipt: IngestReceipt) -> Result<serde_json::Value> {
    receipt.validate()?;
    let path = state_root.join("receipt-queue.json");
    let mut queue: Vec<IngestReceipt> = if path.exists() {
        read_bounded_json(&path)?
    } else {
        Vec::new()
    };
    validate_queue(
        &queue,
        &Cursor {
            sequence: 0,
            receipt_sha256: None,
        },
    )?;
    if let Some(existing) = queue
        .iter()
        .find(|entry| entry.features.sha256 == receipt.features.sha256)
    {
        if !equivalent_receipt(existing, &receipt) {
            bail!("source feature identity already has a conflicting admission/target/transform receipt");
        }
        return Ok(
            serde_json::json!({"state":"queued","reused":true,"sequence":existing.sequence,"receipt_sha256":digest(existing).map_err(anyhow::Error::msg)?,"queue_sha256":hash_file(&path)?}),
        );
    }
    if queue.len() >= 1024 {
        bail!("bounded receipt queue is full; perform a reviewed rollover");
    }
    receipt.sequence = queue.last().map_or(1, |last| last.sequence + 1);
    receipt.previous_receipt_sha256 = queue
        .last()
        .map(digest)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let receipt_sha = digest(&receipt).map_err(anyhow::Error::msg)?;
    let sequence = receipt.sequence;
    queue.push(receipt);
    replace_json(&path, &queue)?;
    let readback: Vec<IngestReceipt> = read_bounded_json(&path)?;
    validate_queue(
        &readback,
        &Cursor {
            sequence,
            receipt_sha256: Some(receipt_sha.clone()),
        },
    )?;
    Ok(
        serde_json::json!({"state":"queued","reused":false,"sequence":sequence,"receipt_sha256":receipt_sha,"queue_sha256":hash_file(&path)?}),
    )
}
fn equivalent_receipt(first: &IngestReceipt, second: &IngestReceipt) -> bool {
    let normalize = |receipt: &IngestReceipt| {
        let mut receipt = receipt.clone();
        receipt.sequence = 1;
        receipt.previous_receipt_sha256 = None;
        receipt.features.file.clear();
        receipt.source_manifest.file.clear();
        if let Some(target) = &mut receipt.targets {
            target.file.clear();
        }
        receipt
    };
    normalize(first) == normalize(second)
}
fn hash_file(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("checksum input is not a regular immutable file");
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        bail!("metadata checksum input exceeds byte budget");
    }
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        let read = file.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        hash.update(&bytes[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

struct ClickHouse {
    client: reqwest::Client,
    endpoint: String,
    database: String,
    user: String,
    password: String,
}
impl ClickHouse {
    fn new(args: &Args) -> Result<Self> {
        let url = reqwest::Url::parse(&args.clickhouse_url)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("invalid credential-free ClickHouse endpoint");
        }
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(120))
                .build()?,
            endpoint: format!("{}/", args.clickhouse_url.trim_end_matches('/')),
            database: args.database.clone(),
            user: args.user.clone(),
            password: std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_default(),
        })
    }
    async fn query(
        &self,
        query: &str,
        params: &[(&str, String)],
        body: Vec<u8>,
    ) -> Result<Vec<u8>> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .basic_auth(&self.user, Some(&self.password))
            .query(&[
                ("query", query),
                ("wait_end_of_query", "1"),
                ("output_format_json_quote_64bit_integers", "0"),
                ("max_result_bytes", "8388608"),
                ("result_overflow_mode", "throw"),
                ("max_memory_usage", "268435456"),
            ]);
        for (name, value) in params {
            request = request.query(&[(*name, value)]);
        }
        let mut response = request
            .header(reqwest::header::CONTENT_LENGTH, body.len().to_string())
            .body(body)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("ClickHouse operation failed with {}", response.status());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|n| n > MAX_RESPONSE_BYTES)
            {
                bail!("ClickHouse result exceeds bounded response budget");
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
    async fn registry(&self, identity: &str) -> Result<Option<RegistryRow>> {
        let query = format!("SELECT dataset_identity,dataset_kind,source_manifest_sha256,manifest_json,row_count,content_sha256,first_observed_at_ms,last_observed_at_ms,materialization_state,materialization_version FROM {}.cex_market_datasets FINAL WHERE dataset_identity={{identity:String}} LIMIT 2 FORMAT JSONEachRow", self.database);
        let bytes = self
            .query(&query, &[("param_identity", identity.into())], vec![])
            .await?;
        let rows = decode_json_rows::<RegistryRow>(&bytes)?;
        if rows.len() > 1 {
            bail!("duplicate dataset registry identity");
        }
        Ok(rows.into_iter().next())
    }
    async fn insert<T: Serialize>(&self, table: &str, rows: &[T]) -> Result<()> {
        if rows.is_empty() || rows.len() > PAGE_ROWS {
            bail!("invalid bounded insert batch");
        }
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, row)?;
            bytes.push(b'\n');
        }
        if bytes.len() > MAX_RESPONSE_BYTES {
            bail!("insert batch exceeds byte budget");
        }
        let response = self
            .query(
                &format!("INSERT INTO {}.{table} FORMAT JSONEachRow", self.database),
                &[],
                bytes,
            )
            .await?;
        if response.iter().any(|byte| !byte.is_ascii_whitespace()) {
            bail!("ClickHouse insert returned an error payload after success headers");
        }
        Ok(())
    }
    async fn feature_page(
        &self,
        identity: &str,
        start_ms: i64,
        end_ms: i64,
        after_ms: i64,
    ) -> Result<Vec<MarketFeatureFrameV1>> {
        let query = format!("SELECT series_id,observed_at_ms,feature_max_available_at_ms,channels,row_identity FROM {}.cex_market_feature_frames FINAL WHERE dataset_identity={{identity:String}} AND observed_at_ms>={{start:Int64}} AND observed_at_ms<{{end:Int64}} AND observed_at_ms>{{after:Int64}} ORDER BY observed_at_ms,series_id LIMIT {PAGE_ROWS} FORMAT RowBinary", self.database);
        let bytes = self
            .query(
                &query,
                &page_params(identity, start_ms, end_ms, after_ms),
                vec![],
            )
            .await?;
        decode_features(&bytes)
    }
    async fn target_page(
        &self,
        identity: &str,
        start_ms: i64,
        end_ms: i64,
        after_ms: i64,
    ) -> Result<Vec<MarketTargetFrameV1>> {
        let query = format!("SELECT series_id,observed_at_ms,available_at_ms,simple_return,spread_bps,row_identity FROM {}.cex_market_target_frames FINAL WHERE dataset_identity={{identity:String}} AND observed_at_ms>={{start:Int64}} AND observed_at_ms<{{end:Int64}} AND observed_at_ms>{{after:Int64}} ORDER BY observed_at_ms,series_id LIMIT {PAGE_ROWS} FORMAT RowBinary", self.database);
        let bytes = self
            .query(
                &query,
                &page_params(identity, start_ms, end_ms, after_ms),
                vec![],
            )
            .await?;
        decode_targets(&bytes)
    }
}

fn page_params(identity: &str, start: i64, end: i64, after: i64) -> Vec<(&'static str, String)> {
    vec![
        ("param_identity", identity.into()),
        ("param_start", start.to_string()),
        ("param_end", end.to_string()),
        ("param_after", after.to_string()),
    ]
}

fn feature_bytes(row: &MarketFeatureFrameV1) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(32 + row.channels.len() * 4);
    bytes.extend(row.series_id.to_le_bytes());
    bytes.extend(row.observed_at_ms.to_le_bytes());
    bytes.extend(row.feature_max_available_at_ms.to_le_bytes());
    bytes.extend((row.channels.len() as u64).to_le_bytes());
    for value in &row.channels {
        bytes.extend(value.to_bits().to_le_bytes());
    }
    bytes
}
fn target_bytes(row: &MarketTargetFrameV1) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(36);
    bytes.extend(row.series_id.to_le_bytes());
    bytes.extend(row.observed_at_ms.to_le_bytes());
    bytes.extend(row.available_at_ms.to_le_bytes());
    bytes.extend(row.simple_return.to_bits().to_le_bytes());
    bytes.extend(row.spread_bps.to_bits().to_le_bytes());
    bytes
}

#[derive(Serialize)]
struct FeatureInsert<'a> {
    dataset_identity: &'a str,
    row_identity: String,
    series_id: u64,
    observed_at_ms: i64,
    feature_max_available_at_ms: i64,
    channels: &'a [f32],
    materialization_version: u64,
}
#[derive(Serialize)]
struct TargetInsert<'a> {
    dataset_identity: &'a str,
    row_identity: String,
    series_id: u64,
    observed_at_ms: i64,
    available_at_ms: i64,
    simple_return: f32,
    spread_bps: f64,
    materialization_version: u64,
}

async fn ingest(
    client: &ClickHouse,
    state_root: &Path,
    artifact_root: &Path,
    receipt: IngestReceipt,
) -> Result<serde_json::Value> {
    receipt.validate()?;
    let root = fs::canonicalize(artifact_root)?;
    let feature_path = resolve_ref(&root, &receipt.features)?;
    let features: MarketFeatureDatasetV1 = read_pinned(&feature_path, &receipt.features.sha256)?;
    features.validate().map_err(anyhow::Error::msg)?;
    if features.schema_version != FEATURE_SCHEMA
        || features.digest().map_err(anyhow::Error::msg)? != receipt.features.sha256
        || features.source_manifest_sha256 != receipt.source_manifest_sha256
    {
        bail!("ingest receipt does not bind the canonical feature export/source identity");
    }
    let sources: FeatureSources = read_pinned(
        &resolve_ref(&root, &receipt.source_manifest)?,
        &receipt.source_manifest_sha256,
    )?;
    sources.validate(&features)?;
    let target = receipt
        .targets
        .as_ref()
        .map(|reference| -> Result<MarketTargetDatasetV1> {
            let target: MarketTargetDatasetV1 =
                read_pinned(&resolve_ref(&root, reference)?, &reference.sha256)?;
            target.validate().map_err(anyhow::Error::msg)?;
            if target.schema_version != TARGET_SCHEMA
                || target.feature_dataset_sha256 != receipt.features.sha256
                || target.digest().map_err(anyhow::Error::msg)? != reference.sha256
            {
                bail!("target export is not bound to the feature version");
            }
            Ok(target)
        })
        .transpose()?;
    let catalogue_path = state_root
        .join("catalogue")
        .join(format!("{}.json", receipt.features.sha256));
    if catalogue_path.exists() {
        let existing: CatalogueEntry = read_bounded_json(&catalogue_path)?;
        if existing.receipt != receipt
            || existing.feature_dataset != features
            || existing.target_dataset != target
        {
            bail!("immutable catalogue identity already exists with conflicting receipt");
        }
        verify_registry(
            client,
            &expected_registry(
                &features,
                &receipt.features.sha256,
                &existing.feature_content_sha256,
                "features",
            )?,
            &features.input,
        )
        .await?;
        if let (Some(target), Some(reference), Some(content)) =
            (&target, &receipt.targets, &existing.target_content_sha256)
        {
            verify_target_registry(
                client,
                &expected_target_registry(
                    target,
                    &reference.sha256,
                    content,
                    &receipt.source_manifest_sha256,
                )?,
            )
            .await?;
        }
        return Ok(
            serde_json::json!({"state":"ready","feature_dataset_sha256":receipt.features.sha256,"reused":true}),
        );
    }
    let feature_content = ingest_features(
        client,
        feature_path
            .parent()
            .context("feature manifest parent missing")?,
        &features,
        &receipt.features.sha256,
        &sources,
        &receipt.allowed_ranges,
    )
    .await?;
    let target_content = if let (Some(target), Some(reference)) = (&target, &receipt.targets) {
        let target_path = resolve_ref(&root, reference)?;
        Some(
            ingest_targets(
                client,
                target_path
                    .parent()
                    .context("target manifest parent missing")?,
                target,
                &reference.sha256,
                &receipt.source_manifest_sha256,
                &receipt.allowed_ranges,
            )
            .await?,
        )
    } else {
        None
    };
    let entry = CatalogueEntry {
        receipt: receipt.clone(),
        feature_dataset: features,
        target_dataset: target,
        feature_content_sha256: feature_content,
        target_content_sha256: target_content,
    };
    publish_json(&catalogue_path, &entry)?;
    Ok(
        serde_json::json!({"state":"ready","feature_dataset_sha256":receipt.features.sha256,"reused":false}),
    )
}

async fn ingest_features(
    client: &ClickHouse,
    root: &Path,
    dataset: &MarketFeatureDatasetV1,
    identity: &str,
    sources: &FeatureSources,
    allowed_ranges: &[AllowedRange],
) -> Result<String> {
    let mut digest = Sha256::new();
    let mut last_clock = None;
    let mut total = 0;
    for shard in &dataset.shards {
        let mut reader = verified_lines(root, shard)?;
        let mut source_hash = Sha256::new();
        let mut rows = Vec::with_capacity(PAGE_ROWS);
        let mut shard_rows = 0;
        let mut first = None;
        while let Some(bytes) = read_line(&mut reader)? {
            source_hash.update(&bytes);
            source_hash.update(b"\n");
            let row: MarketFeatureFrameV1 = serde_json::from_slice(&bytes)?;
            row.validate(&dataset.input).map_err(anyhow::Error::msg)?;
            if !range_contains(allowed_ranges, row.observed_at_ms, row.observed_at_ms) {
                bail!("feature observation crosses the canonical coordinator's admitted boundary");
            }
            if row.series_id < sources.first_dependency_received_at_ns
                || row.series_id
                    > u64::try_from(row.observed_at_ms)?
                        .checked_mul(1_000_000)
                        .context("series clock overflow")?
            {
                bail!("feature series is not a canonical recovery timestamp identity");
            }
            check_clock(&mut last_clock, row.observed_at_ms)?;
            first.get_or_insert(row.observed_at_ms);
            shard_rows += 1;
            total += 1;
            digest.update(feature_bytes(&row));
            rows.push(row);
            if rows.len() == PAGE_ROWS {
                insert_features(client, identity, &rows).await?;
                rows.clear();
            }
        }
        if !rows.is_empty() {
            insert_features(client, identity, &rows).await?;
        }
        check_shard(shard, shard_rows, first, last_clock)?;
        if format!("{:x}", source_hash.finalize()) != shard.sha256 {
            bail!("canonical feature bytes changed while ingesting");
        }
    }
    let content = format!("{:x}", digest.finalize());
    let registry = expected_registry(dataset, identity, &content, "features")?;
    if total != registry.row_count {
        bail!("source feature count mismatch");
    }
    verify_feature_content(client, &registry, &dataset.input).await?;
    commit_registry(client, &registry).await?;
    Ok(content)
}

async fn insert_features(
    client: &ClickHouse,
    identity: &str,
    rows: &[MarketFeatureFrameV1],
) -> Result<()> {
    let insert: Vec<_> = rows
        .iter()
        .map(|r| FeatureInsert {
            dataset_identity: identity,
            row_identity: bytes_digest(&feature_bytes(r)),
            series_id: r.series_id,
            observed_at_ms: r.observed_at_ms,
            feature_max_available_at_ms: r.feature_max_available_at_ms,
            channels: &r.channels,
            materialization_version: 1,
        })
        .collect();
    client.insert("cex_market_feature_frames", &insert).await
}
async fn insert_targets(
    client: &ClickHouse,
    identity: &str,
    rows: &[MarketTargetFrameV1],
) -> Result<()> {
    let insert: Vec<_> = rows
        .iter()
        .map(|r| TargetInsert {
            dataset_identity: identity,
            row_identity: bytes_digest(&target_bytes(r)),
            series_id: r.series_id,
            observed_at_ms: r.observed_at_ms,
            available_at_ms: r.available_at_ms,
            simple_return: r.simple_return,
            spread_bps: r.spread_bps,
            materialization_version: 1,
        })
        .collect();
    client.insert("cex_market_target_frames", &insert).await
}
async fn ingest_targets(
    client: &ClickHouse,
    root: &Path,
    dataset: &MarketTargetDatasetV1,
    identity: &str,
    source: &str,
    allowed_ranges: &[AllowedRange],
) -> Result<String> {
    let mut digest = Sha256::new();
    let mut last_clock = None;
    for shard in &dataset.shards {
        let mut reader = verified_lines(root, shard)?;
        let mut rows = Vec::with_capacity(PAGE_ROWS);
        let mut shard_rows = 0;
        let mut first = None;
        let mut source_hash = Sha256::new();
        while let Some(bytes) = read_line(&mut reader)? {
            source_hash.update(&bytes);
            source_hash.update(b"\n");
            let row: MarketTargetFrameV1 = serde_json::from_slice(&bytes)?;
            row.validate().map_err(anyhow::Error::msg)?;
            if !range_contains(allowed_ranges, row.observed_at_ms, row.available_at_ms) {
                bail!("target observation or maturity crosses the admitted pre-holdout boundary");
            }
            check_clock(&mut last_clock, row.observed_at_ms)?;
            first.get_or_insert(row.observed_at_ms);
            shard_rows += 1;
            digest.update(target_bytes(&row));
            rows.push(row);
            if rows.len() == PAGE_ROWS {
                insert_targets(client, identity, &rows).await?;
                rows.clear();
            }
        }
        if !rows.is_empty() {
            insert_targets(client, identity, &rows).await?;
        }
        check_shard(shard, shard_rows, first, last_clock)?;
        if format!("{:x}", source_hash.finalize()) != shard.sha256 {
            bail!("canonical target bytes changed while ingesting");
        }
    }
    let content = format!("{:x}", digest.finalize());
    let registry = expected_target_registry(dataset, identity, &content, source)?;
    verify_target_content(client, &registry).await?;
    commit_registry(client, &registry).await?;
    Ok(content)
}

fn expected_registry(
    dataset: &MarketFeatureDatasetV1,
    identity: &str,
    content: &str,
    kind: &str,
) -> Result<RegistryRow> {
    expected_metadata(
        identity,
        kind,
        &dataset.source_manifest_sha256,
        serde_json::to_string(dataset)?,
        &dataset.shards,
        content,
    )
}
fn expected_target_registry(
    dataset: &MarketTargetDatasetV1,
    identity: &str,
    content: &str,
    source: &str,
) -> Result<RegistryRow> {
    expected_metadata(
        identity,
        "targets",
        source,
        serde_json::to_string(dataset)?,
        &dataset.shards,
        content,
    )
}
fn expected_metadata(
    identity: &str,
    kind: &str,
    source: &str,
    manifest: String,
    shards: &[SequenceShardV1],
    content: &str,
) -> Result<RegistryRow> {
    Ok(RegistryRow {
        dataset_identity: identity.into(),
        dataset_kind: kind.into(),
        source_manifest_sha256: source.into(),
        manifest_json: manifest,
        row_count: shards.iter().map(|s| s.rows).sum(),
        content_sha256: content.into(),
        first_observed_at_ms: shards
            .first()
            .context("missing first shard")?
            .first_observed_at_ms,
        last_observed_at_ms: shards
            .last()
            .context("missing last shard")?
            .last_observed_at_ms,
        materialization_state: "complete".into(),
        materialization_version: 2,
    })
}
async fn commit_registry(client: &ClickHouse, row: &RegistryRow) -> Result<()> {
    if let Some(existing) = client.registry(&row.dataset_identity).await? {
        if serde_json::to_value(&existing)? != serde_json::to_value(row)? {
            bail!("dataset registry immutable identity conflict");
        }
    } else {
        client
            .insert("cex_market_datasets", std::slice::from_ref(row))
            .await?;
    }
    let actual = client
        .registry(&row.dataset_identity)
        .await?
        .context("complete registry insert not independently visible")?;
    if serde_json::to_value(actual)? != serde_json::to_value(row)? {
        bail!("complete registry readback does not match verified identity");
    }
    Ok(())
}
async fn verify_registry(
    client: &ClickHouse,
    row: &RegistryRow,
    input: &SequenceInputSpecV1,
) -> Result<()> {
    require_registry(client, row).await?;
    verify_feature_content(client, row, input).await
}
async fn verify_target_registry(client: &ClickHouse, row: &RegistryRow) -> Result<()> {
    require_registry(client, row).await?;
    verify_target_content(client, row).await
}
async fn require_registry(client: &ClickHouse, row: &RegistryRow) -> Result<()> {
    let actual = client
        .registry(&row.dataset_identity)
        .await?
        .context("dataset registry is missing")?;
    if serde_json::to_value(actual)? != serde_json::to_value(row)? {
        bail!("dataset registry differs from pinned catalogue");
    }
    Ok(())
}
async fn verify_feature_content(
    client: &ClickHouse,
    registry: &RegistryRow,
    input: &SequenceInputSpecV1,
) -> Result<()> {
    let mut hash = Sha256::new();
    let mut count = 0;
    let mut after = -1;
    loop {
        let rows = client
            .feature_page(&registry.dataset_identity, 0, i64::MAX, after)
            .await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            row.validate(input).map_err(anyhow::Error::msg)?;
            if row.observed_at_ms <= after {
                bail!("duplicate/unordered ClickHouse feature clock");
            }
            after = row.observed_at_ms;
            count += 1;
            hash.update(feature_bytes(&row));
            if count > registry.row_count {
                bail!("unexpected extra ClickHouse feature rows");
            }
        }
    }
    if count != registry.row_count
        || after != registry.last_observed_at_ms
        || format!("{:x}", hash.finalize()) != registry.content_sha256
    {
        bail!("actual ClickHouse feature count/clocks/bit-content do not match canonical export");
    }
    Ok(())
}
async fn verify_target_content(client: &ClickHouse, registry: &RegistryRow) -> Result<()> {
    let mut hash = Sha256::new();
    let mut count = 0;
    let mut after = -1;
    loop {
        let rows = client
            .target_page(&registry.dataset_identity, 0, i64::MAX, after)
            .await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            row.validate().map_err(anyhow::Error::msg)?;
            if row.observed_at_ms <= after {
                bail!("duplicate/unordered ClickHouse target clock");
            }
            after = row.observed_at_ms;
            count += 1;
            hash.update(target_bytes(&row));
            if count > registry.row_count {
                bail!("unexpected extra ClickHouse target rows");
            }
        }
    }
    if count != registry.row_count
        || after != registry.last_observed_at_ms
        || format!("{:x}", hash.finalize()) != registry.content_sha256
    {
        bail!("actual ClickHouse target count/clocks/bit-content do not match canonical export");
    }
    Ok(())
}

async fn request_view(
    client: &ClickHouse,
    state_root: &Path,
    request: DataRequest,
) -> Result<serde_json::Value> {
    let identity = request.identity()?;
    let views = state_root.join("views");
    fs::create_dir_all(&views)?;
    let ready_root = views.join(&identity);
    if ready_root.exists() {
        return ready_response(state_root, &ready_root, &request);
    }
    let lock_dir = state_root.join("claims");
    fs::create_dir_all(&lock_dir)?;
    let Some(_claim) = claim(&lock_dir.join(format!("{identity}.lock")))? else {
        return Ok(serde_json::json!({"state":"preparing","request_sha256":identity}));
    };
    // Close the race between the first cache read and acquiring the sole preparation claim.
    if ready_root.exists() {
        return ready_response(state_root, &ready_root, &request);
    }
    let mut entries = Vec::new();
    let mut previous_end = None;
    for source in &request.sources {
        let path = state_root
            .join("catalogue")
            .join(format!("{}.json", source.feature_dataset_sha256));
        if !path.exists() {
            return Ok(
                serde_json::json!({"state":"preparing","request_sha256":identity,"missing_partition":source.feature_dataset_sha256}),
            );
        }
        let entry: CatalogueEntry = read_bounded_json(&path)?;
        if let Err(error) = admit_entry(&request, source, &entry, &mut previous_end) {
            return Ok(
                serde_json::json!({"state":"blocked","request_sha256":identity,"reason":error.to_string()}),
            );
        }
        entries.push(entry);
    }
    let staged = tempfile::Builder::new()
        .prefix(&format!(".{identity}.prepare-"))
        .tempdir_in(&views)?;
    match prepare_view(client, staged.path(), &request, &entries).await {
        Ok(receipt) => {
            publish_json(&staged.path().join("_READY.json"), &receipt)?;
            fs::rename(staged.path(), &ready_root)?;
            File::open(&views)?.sync_all()?;
            ready_response(state_root, &ready_root, &request)
        }
        Err(error) => Ok(
            serde_json::json!({"state":"blocked","request_sha256":identity,"reason":error.to_string()}),
        ),
    }
}

fn admit_entry(
    request: &DataRequest,
    source: &DataSource,
    entry: &CatalogueEntry,
    previous_end: &mut Option<i64>,
) -> Result<()> {
    entry.receipt.validate()?;
    entry
        .feature_dataset
        .validate()
        .map_err(anyhow::Error::msg)?;
    let first = entry
        .feature_dataset
        .shards
        .first()
        .context("empty admitted partition")?
        .first_observed_at_ms;
    let last = entry
        .feature_dataset
        .shards
        .last()
        .context("empty admitted partition")?
        .last_observed_at_ms;
    if entry.feature_dataset.digest().map_err(anyhow::Error::msg)? != source.feature_dataset_sha256
        || entry.feature_dataset.input.context_rows != request.input.context_rows
        || entry.feature_dataset.input.bucket_ms != request.input.bucket_ms
        || request
            .input
            .ordered_channels
            .iter()
            .any(|name| !entry.feature_dataset.input.ordered_channels.contains(name))
        || entry.receipt.features.sha256 != source.feature_dataset_sha256
        || entry.receipt.source_manifest_sha256 != entry.feature_dataset.source_manifest_sha256
        || entry.receipt.transform_sha256 != request.transform_sha256
        || (previous_end.is_none() && first > request.view.history_start_ms)
        || previous_end.is_some_and(|end| first <= end)
        || last < request.view.history_start_ms
        || first >= request.view.end_ms
    {
        bail!("source union is not ordered, disjoint, relevant or bound to the admitted input/transform");
    }
    let intersection = AllowedRange {
        start_ms: first.max(request.view.history_start_ms),
        end_ms: last
            .checked_add(1000)
            .context("partition end overflow")?
            .min(request.view.end_ms),
    };
    if !entry
        .receipt
        .allowed_ranges
        .iter()
        .any(|r| r.start_ms <= intersection.start_ms && r.end_ms >= intersection.end_ms)
    {
        bail!("requested range crosses the canonical coordinator's admitted pre-holdout boundary");
    }
    if request.supervised() {
        let target = entry
            .target_dataset
            .as_ref()
            .context("source lacks supervised target grant")?;
        target.validate().map_err(anyhow::Error::msg)?;
        if !entry.receipt.pre_holdout_supervised
            || target.feature_dataset_sha256 != source.feature_dataset_sha256
            || Some(target.digest().map_err(anyhow::Error::msg)?) != source.target_dataset_sha256
            || entry.receipt.targets.as_ref().map(|r| &r.sha256)
                != source.target_dataset_sha256.as_ref()
        {
            bail!("supervised source has no matching admitted target version");
        }
    }
    *previous_end = Some(last);
    Ok(())
}

async fn prepare_view(
    client: &ClickHouse,
    root: &Path,
    request: &DataRequest,
    entries: &[CatalogueEntry],
) -> Result<ReadyReceipt> {
    let producer_image = std::env::var("MONDAY_DATA_PLATFORM_IMAGE")
        .context("prepared converter immutable image identity is missing")?;
    validate_prepared_producer(SOURCE_REVISION, &producer_image).map_err(anyhow::Error::msg)?;
    let feature_root = root.join("features");
    let target_root = root.join("targets");
    fs::create_dir_all(&feature_root)?;
    if request.supervised() {
        fs::create_dir_all(&target_root)?;
    }
    let mut feature_shards = Vec::new();
    let mut target_shards = Vec::new();
    let mut series: Vec<PreparedMarketSeriesV1> = Vec::new();
    let mut gaps = Vec::new();
    let mut context = VecDeque::with_capacity(request.input.context_rows);
    let mut anchors = Vec::new();
    let mut last_clock = None;
    let mut total_rows = 0_u64;
    for (source, entry) in request.sources.iter().zip(entries) {
        let expected = expected_registry(
            &entry.feature_dataset,
            &source.feature_dataset_sha256,
            &entry.feature_content_sha256,
            "features",
        )?;
        // Full actual source verification precedes publication. It is done once per new view,
        // never on a ready hit; training subsequently verifies the immutable prepared shards.
        verify_registry(client, &expected, &entry.feature_dataset.input).await?;
        let channel_indices = request
            .input
            .ordered_channels
            .iter()
            .map(|name| {
                entry
                    .feature_dataset
                    .input
                    .ordered_channels
                    .iter()
                    .position(|source| source == name)
                    .context("requested column disappeared from admitted source")
            })
            .collect::<Result<Vec<_>>>()?;
        if request.supervised() {
            let target = entry
                .target_dataset
                .as_ref()
                .context("target metadata missing")?;
            let identity = source
                .target_dataset_sha256
                .as_deref()
                .context("target identity missing")?;
            let expected = expected_target_registry(
                target,
                identity,
                entry
                    .target_content_sha256
                    .as_deref()
                    .context("target content proof missing")?,
                &entry.receipt.source_manifest_sha256,
            )?;
            verify_target_registry(client, &expected).await?;
        }
        let mut after = request
            .view
            .history_start_ms
            .checked_sub(1)
            .context("history cursor overflow")?;
        loop {
            let mut rows = client
                .feature_page(
                    &source.feature_dataset_sha256,
                    request.view.history_start_ms,
                    request.view.end_ms,
                    after,
                )
                .await?;
            if rows.is_empty() {
                break;
            }
            if total_rows == 0
                && rows.first().map(|row| row.observed_at_ms) != Some(request.view.history_start_ms)
            {
                bail!("requested leading history coverage is missing; no implicit partial view admission");
            }
            for row in &mut rows {
                row.validate(&entry.feature_dataset.input)
                    .map_err(anyhow::Error::msg)?;
                row.channels = channel_indices
                    .iter()
                    .map(|index| row.channels[*index])
                    .collect();
            }
            let start = rows.first().context("empty feature page")?.observed_at_ms;
            let end = rows
                .last()
                .context("empty feature page")?
                .observed_at_ms
                .checked_add(1)
                .context("page end overflow")?;
            let mut target_rows = Vec::new();
            let feature_keys: BTreeSet<_> = rows
                .iter()
                .map(|r| (r.series_id, r.observed_at_ms))
                .collect();
            if request.supervised() {
                let identity = source
                    .target_dataset_sha256
                    .as_deref()
                    .context("target identity missing")?;
                let mut target_after = start.checked_sub(1).context("target cursor overflow")?;
                loop {
                    let page = client
                        .target_page(identity, start, end, target_after)
                        .await?;
                    if page.is_empty() {
                        break;
                    }
                    for row in page {
                        row.validate().map_err(anyhow::Error::msg)?;
                        if row.observed_at_ms <= target_after {
                            bail!("duplicate target clock in prepared page");
                        }
                        target_after = row.observed_at_ms;
                        if !feature_keys.contains(&(row.series_id, row.observed_at_ms)) {
                            bail!("target is not structurally aligned to its canonical feature observation");
                        }
                        if row.observed_at_ms >= request.view.history_start_ms
                            && row.observed_at_ms < request.anchor_end_ms
                            && row.available_at_ms < request.view.end_ms
                        {
                            target_rows.push(row);
                        }
                        if target_rows.len() > PAGE_ROWS {
                            bail!("target page exceeds bounded source feature count");
                        }
                    }
                }
                if !target_rows.is_empty() {
                    let name = format!("part-{:06}.parquet", target_shards.len());
                    let shard = write_target_parquet_shard(&target_root, &name, &target_rows)
                        .map_err(anyhow::Error::msg)?;
                    readback_target_shard(&target_root, &shard, &target_rows)?;
                    target_shards.push(shard);
                }
            }
            let structural: BTreeSet<_> = target_rows
                .iter()
                .map(|r| (r.series_id, r.observed_at_ms))
                .collect();
            for row in &rows {
                row.validate(&request.input).map_err(anyhow::Error::msg)?;
                if let Some(previous) = last_clock {
                    if row.observed_at_ms <= previous {
                        bail!("source union produced duplicate or unordered frame clocks");
                    }
                    if row.observed_at_ms != previous + 1000 {
                        gaps.push(PreparedMarketGapV1 {
                            last_before_ms: previous,
                            first_after_ms: row.observed_at_ms,
                        });
                        if gaps.len() > 65_536 {
                            bail!("prepared gap metadata budget exceeded");
                        }
                    }
                }
                last_clock = Some(row.observed_at_ms);
                total_rows += 1;
                if total_rows > MAX_VIEW_ROWS {
                    bail!("prepared view exceeds 14-day row budget");
                }
                if series.last().is_none_or(|s| s.series_id != row.series_id) {
                    series.push(PreparedMarketSeriesV1 {
                        series_id: row.series_id,
                        first_observed_at_ms: row.observed_at_ms,
                        last_observed_at_ms: row.observed_at_ms,
                        rows: 0,
                    });
                }
                let coverage = series.last_mut().context("missing prepared series")?;
                coverage.last_observed_at_ms = row.observed_at_ms;
                coverage.rows += 1;
                if series.len() > 65_536 {
                    bail!("prepared series metadata budget exceeded");
                }
                if context.back().is_some_and(|(id, clock): &(u64, i64)| {
                    *id != row.series_id || *clock + 1000 != row.observed_at_ms
                }) {
                    context.clear();
                }
                context.push_back((row.series_id, row.observed_at_ms));
                if context.len() > request.input.context_rows {
                    context.pop_front();
                }
                if request.qualified_anchors
                    && context.len() == request.input.context_rows
                    && row.observed_at_ms >= request.view.decision_start_ms
                    && row.observed_at_ms < request.anchor_end_ms
                    && (row.observed_at_ms - request.view.decision_start_ms)
                        % request.view.decision_stride_ms
                        == 0
                    && (!request.supervised()
                        || structural.contains(&(row.series_id, row.observed_at_ms)))
                {
                    anchors.push(MarketTrainingAnchorV1 {
                        series_id: row.series_id,
                        observed_at_ms: row.observed_at_ms,
                    });
                    if anchors.len() > 32_768 {
                        bail!(
                            "qualified anchor budget exceeded; choose the admitted training stride"
                        );
                    }
                }
            }
            let name = format!("part-{:06}.parquet", feature_shards.len());
            let shard = write_feature_parquet_shard(&feature_root, &name, &rows, &request.input)
                .map_err(anyhow::Error::msg)?;
            readback_feature_shard(&feature_root, &shard, &rows, &request.input)?;
            feature_shards.push(shard);
            after = rows
                .last()
                .context("feature cursor missing")?
                .observed_at_ms;
        }
    }
    if feature_shards.is_empty() {
        bail!("requested view has no verified real feature rows");
    }
    if feature_shards
        .first()
        .map(|shard| shard.first_observed_at_ms)
        != Some(request.view.history_start_ms)
    {
        bail!("requested leading history coverage is missing; no implicit partial view admission");
    }
    let source_feature = union_digest(
        &request
            .sources
            .iter()
            .map(|s| s.feature_dataset_sha256.clone())
            .collect::<Vec<_>>(),
    )?;
    let source_manifest = union_digest(
        &entries
            .iter()
            .map(|e| e.receipt.source_manifest_sha256.clone())
            .collect::<Vec<_>>(),
    )?;
    let feature_dataset = MarketFeatureDatasetV1 {
        schema_version: FEATURE_PARQUET_SCHEMA.into(),
        venue: "binance-usdm".into(),
        symbol: "SOLUSDT".into(),
        source_manifest_sha256: source_manifest.clone(),
        input: request.input.clone(),
        shards: feature_shards,
    };
    let feature_hash = feature_dataset.digest().map_err(anyhow::Error::msg)?;
    let feature_ref = ArtifactRef {
        file: "features/manifest.json".into(),
        sha256: feature_hash.clone(),
    };
    publish_json(&root.join(&feature_ref.file), &feature_dataset)?;
    let target = if request.supervised() {
        if target_shards.is_empty() {
            bail!("supervised view has no admitted mature targets");
        }
        let dataset = MarketTargetDatasetV1 {
            schema_version: TARGET_PARQUET_SCHEMA.into(),
            feature_dataset_sha256: feature_hash.clone(),
            horizon_ms: 30_000,
            shards: target_shards,
        };
        let hash = dataset.digest().map_err(anyhow::Error::msg)?;
        let reference = ArtifactRef {
            file: "targets/manifest.json".into(),
            sha256: hash,
        };
        publish_json(&root.join(&reference.file), &dataset)?;
        Some(reference)
    } else {
        None
    };
    let anchors_ref = if request.qualified_anchors {
        let set = MarketTrainingAnchorSetV1 {
            schema_version: "monday.market_training_anchors.v1".into(),
            feature_dataset_sha256: feature_hash.clone(),
            view: request.view,
            anchor_end_ms: request.anchor_end_ms,
            anchors,
        };
        let anchor_hash = set.digest().map_err(anyhow::Error::msg)?;
        let reference = ArtifactRef {
            file: format!("features/{anchor_hash}.market-anchors.json"),
            sha256: anchor_hash,
        };
        publish_json(&root.join(&reference.file), &set)?;
        Some(reference)
    } else {
        None
    };
    let prepared = PreparedMarketViewV1 {
        schema_version: PREPARED_MARKET_VIEW_SCHEMA.into(),
        sources: request
            .sources
            .iter()
            .zip(entries)
            .map(|(s, e)| PreparedMarketSourceV1 {
                feature_dataset_sha256: s.feature_dataset_sha256.clone(),
                target_dataset_sha256: s.target_dataset_sha256.clone(),
                source_manifest_sha256: e.receipt.source_manifest_sha256.clone(),
                transform_sha256: e.receipt.transform_sha256.clone(),
            })
            .collect(),
        source_feature_dataset_sha256: source_feature,
        source_target_dataset_sha256: if request.supervised() {
            Some(union_digest(
                &request
                    .sources
                    .iter()
                    .map(|s| {
                        s.target_dataset_sha256
                            .clone()
                            .expect("validated target source")
                    })
                    .collect::<Vec<_>>(),
            )?)
        } else {
            None
        },
        source_manifest_sha256: source_manifest,
        transform_sha256: request.transform_sha256.clone(),
        data_watermark_ms: last_clock
            .context("missing actual watermark")?
            .checked_add(1000)
            .context("watermark overflow")?,
        view: request.view,
        feature_dataset_sha256: feature_hash,
        target_dataset_sha256: target.as_ref().map(|r| r.sha256.clone()),
        qualified_anchors_sha256: anchors_ref.as_ref().map(|r| r.sha256.clone()),
        series,
        gaps,
    };
    let prepared_hash = prepared.digest().map_err(anyhow::Error::msg)?;
    let target_dataset = target
        .as_ref()
        .map(|r| read_pinned::<MarketTargetDatasetV1>(&root.join(&r.file), &r.sha256))
        .transpose()?;
    prepared
        .validate_datasets(&feature_dataset, target_dataset.as_ref())
        .map_err(anyhow::Error::msg)?;
    publish_json(&root.join("prepared-view.json"), &prepared)?;
    Ok(ReadyReceipt {
        schema_version: "monday.market_ready_receipt.v2".into(),
        producer_source_revision: SOURCE_REVISION.into(),
        producer_image,
        request_sha256: request.identity()?,
        request: request.clone(),
        prepared_view_sha256: prepared_hash,
        prepared_view: prepared,
        feature_manifest: feature_ref,
        target_manifest: target,
        qualified_anchors: anchors_ref,
    })
}

fn ready_response(
    state_root: &Path,
    root: &Path,
    request: &DataRequest,
) -> Result<serde_json::Value> {
    match read_ready(state_root, root, request) {
        Ok(result) => Ok(result),
        Err(error) => Ok(
            serde_json::json!({"state":"blocked","request_sha256":request.identity()?,"reason":error.to_string()}),
        ),
    }
}
fn response_bytes<T: Serialize>(response: &T) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(response)?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        bail!("response exceeds the bounded control-plane receipt budget");
    }
    Ok(bytes)
}
fn print_response<T: Serialize>(response: &T) -> Result<()> {
    let bytes = response_bytes(response)?;
    let mut output = std::io::stdout().lock();
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    Ok(())
}
fn read_ready(state_root: &Path, root: &Path, request: &DataRequest) -> Result<serde_json::Value> {
    let ready: ReadyReceipt = read_bounded_json(&root.join("_READY.json"))?;
    let shared_ready: PreparedMarketReadyReceiptV2 =
        serde_json::from_value(serde_json::to_value(&ready)?)?;
    shared_ready.validate().map_err(anyhow::Error::msg)?;
    if ready.schema_version != "monday.market_ready_receipt.v2"
        || ready.request != *request
        || ready.request_sha256 != request.identity()?
        || ready.prepared_view.digest().map_err(anyhow::Error::msg)? != ready.prepared_view_sha256
        || ready.target_manifest.is_some() != request.supervised()
        || ready.qualified_anchors.is_some() != request.qualified_anchors
    {
        bail!("immutable ready receipt identity is corrupt");
    }
    let prepared_file: PreparedMarketViewV1 = read_pinned(
        &root.join("prepared-view.json"),
        &ready.prepared_view_sha256,
    )?;
    if prepared_file != ready.prepared_view {
        bail!("published prepared-view file differs from its ready receipt");
    }
    let feature_path = resolve_ref(root, &ready.feature_manifest)?;
    let features: MarketFeatureDatasetV1 =
        read_pinned(&feature_path, &ready.feature_manifest.sha256)?;
    features.validate().map_err(anyhow::Error::msg)?;
    if features.digest().map_err(anyhow::Error::msg)? != ready.prepared_view.feature_dataset_sha256
    {
        bail!("ready feature manifest binding mismatch");
    }
    verify_shard_sizes(
        feature_path.parent().context("missing feature parent")?,
        &features.shards,
    )?;
    let mut targets = None;
    if let Some(reference) = &ready.target_manifest {
        if !request.supervised() {
            bail!("label-free ready receipt contains target artifacts");
        }
        let path = resolve_ref(root, reference)?;
        let target: MarketTargetDatasetV1 = read_pinned(&path, &reference.sha256)?;
        target.validate().map_err(anyhow::Error::msg)?;
        if Some(target.digest().map_err(anyhow::Error::msg)?)
            != ready.prepared_view.target_dataset_sha256
            || target.feature_dataset_sha256 != features.digest().map_err(anyhow::Error::msg)?
        {
            bail!("ready target binding mismatch");
        }
        verify_shard_sizes(
            path.parent().context("missing target parent")?,
            &target.shards,
        )?;
        targets = Some(target);
    }
    if let Some(reference) = &ready.qualified_anchors {
        let anchors: MarketTrainingAnchorSetV1 =
            read_pinned(&resolve_ref(root, reference)?, &reference.sha256)?;
        if Some(anchors.digest().map_err(anyhow::Error::msg)?)
            != ready.prepared_view.qualified_anchors_sha256
            || anchors.feature_dataset_sha256 != features.digest().map_err(anyhow::Error::msg)?
        {
            bail!("ready anchor binding mismatch");
        }
    }
    ready
        .prepared_view
        .validate_datasets(&features, targets.as_ref())
        .map_err(anyhow::Error::msg)?;
    if !ensure_certification(state_root, root, &ready, &features, targets.as_ref())? {
        return Ok(
            serde_json::json!({"state":"preparing","request_sha256":ready.request_sha256,"stage":"cache_recertification"}),
        );
    }
    Ok(ready_summary(root, &ready))
}
fn ready_summary(root: &Path, ready: &ReadyReceipt) -> serde_json::Value {
    serde_json::json!({"state":"ready","request_sha256":ready.request_sha256,"prepared_view_sha256":ready.prepared_view_sha256,
        "producer_source_revision":ready.producer_source_revision,"producer_image":ready.producer_image,
        "prepared_view_manifest":{"file":"prepared-view.json","sha256":ready.prepared_view_sha256},
        "root":root,"feature_manifest":ready.feature_manifest,"target_manifest":ready.target_manifest,"qualified_anchors":ready.qualified_anchors,
        "watermark_ms":ready.prepared_view.data_watermark_ms,"series_count":ready.prepared_view.series.len(),"gap_count":ready.prepared_view.gaps.len(),
        "total_rows":ready.prepared_view.series.iter().map(|s|s.rows).sum::<u64>(),
        "first_observed_at_ms":ready.prepared_view.series.first().map(|s|s.first_observed_at_ms),"last_observed_at_ms":ready.prepared_view.series.last().map(|s|s.last_observed_at_ms),
        "gaps_preview":ready.prepared_view.gaps.iter().take(32).collect::<Vec<_>>()})
}

fn ensure_certification(
    state_root: &Path,
    root: &Path,
    ready: &ReadyReceipt,
    features: &MarketFeatureDatasetV1,
    targets: Option<&MarketTargetDatasetV1>,
) -> Result<bool> {
    let directory = state_root.join("certifications");
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.json", ready.request_sha256));
    let current = cache_blocks(root, features, targets)?;
    if path.exists() {
        let previous: CacheCertification = read_bounded_json(&path)?;
        if previous.schema_version == "monday.market_cache_certification.v1"
            && previous.prepared_view_sha256 == ready.prepared_view_sha256
            && previous.blocks == current
        {
            return Ok(true);
        }
    }
    let Some(_owner) = claim(&directory.join(format!("{}.lock", ready.request_sha256)))? else {
        return Ok(false);
    };
    let mut feature_reader = FeatureParquetReader::open(
        &root.join("features"),
        features.shards.clone(),
        features.input.clone(),
    )
    .map_err(anyhow::Error::msg)?;
    feature_reader.finish_pass().map_err(anyhow::Error::msg)?;
    if let Some(targets) = targets {
        let mut reader = TargetParquetReader::open(&root.join("targets"), targets.shards.clone())
            .map_err(anyhow::Error::msg)?;
        reader.finish_pass().map_err(anyhow::Error::msg)?;
    }
    let verified = cache_blocks(root, features, targets)?;
    if verified != current {
        bail!("prepared cache changed during checksum/full typed-pass recertification");
    }
    replace_json(
        &path,
        &CacheCertification {
            schema_version: "monday.market_cache_certification.v1".into(),
            prepared_view_sha256: ready.prepared_view_sha256.clone(),
            blocks: verified,
        },
    )?;
    Ok(true)
}
fn cache_blocks(
    root: &Path,
    features: &MarketFeatureDatasetV1,
    targets: Option<&MarketTargetDatasetV1>,
) -> Result<Vec<CacheBlock>> {
    use std::os::unix::fs::MetadataExt;
    let mut blocks = Vec::new();
    for (prefix, shards) in std::iter::once(("features", features.shards.as_slice()))
        .chain(targets.map(|t| ("targets", t.shards.as_slice())))
    {
        for shard in shards {
            let relative = format!("{prefix}/{}", shard.file);
            let metadata = fs::symlink_metadata(root.join(&relative))?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != shard.bytes
            {
                bail!("prepared physical cache block is missing, redirected or truncated");
            }
            blocks.push(CacheBlock {
                file: relative,
                sha256: shard.sha256.clone(),
                device: metadata.dev(),
                inode: metadata.ino(),
                bytes: metadata.len(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
            });
        }
    }
    Ok(blocks)
}

fn union_digest(hashes: &[String]) -> Result<String> {
    if hashes.len() == 1 {
        Ok(hashes[0].clone())
    } else {
        digest(&hashes).map_err(anyhow::Error::msg)
    }
}
fn readback_feature_shard(
    root: &Path,
    shard: &SequenceShardV1,
    rows: &[MarketFeatureFrameV1],
    input: &SequenceInputSpecV1,
) -> Result<()> {
    let mut reader = FeatureParquetReader::open(root, vec![shard.clone()], input.clone())
        .map_err(anyhow::Error::msg)?;
    for expected in rows {
        let actual = reader
            .next_frame()
            .map_err(anyhow::Error::msg)?
            .context("prepared feature shard lost rows")?;
        if feature_bytes(&actual) != feature_bytes(expected) {
            bail!("prepared feature bits differ from verified ClickHouse values");
        }
    }
    if reader.next_frame().map_err(anyhow::Error::msg)?.is_some() {
        bail!("prepared feature shard contains extra rows");
    }
    reader.finish_pass().map_err(anyhow::Error::msg)
}
fn readback_target_shard(
    root: &Path,
    shard: &SequenceShardV1,
    rows: &[MarketTargetFrameV1],
) -> Result<()> {
    let mut reader =
        TargetParquetReader::open(root, vec![shard.clone()]).map_err(anyhow::Error::msg)?;
    for expected in rows {
        let actual = reader
            .next_frame()
            .map_err(anyhow::Error::msg)?
            .context("prepared target shard lost rows")?;
        if target_bytes(&actual) != target_bytes(expected) {
            bail!("prepared target bits differ from verified ClickHouse values");
        }
    }
    if reader.next_frame().map_err(anyhow::Error::msg)?.is_some() {
        bail!("prepared target shard contains extra rows");
    }
    reader.finish_pass().map_err(anyhow::Error::msg)
}
fn verify_shard_sizes(root: &Path, shards: &[SequenceShardV1]) -> Result<()> {
    for shard in shards {
        let metadata = fs::symlink_metadata(root.join(&shard.file))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != shard.bytes
        {
            bail!("prepared shard missing, redirected or truncated");
        }
    }
    Ok(())
}

fn validate_ref(reference: &ArtifactRef) -> Result<()> {
    if !valid_sha256(&reference.sha256)
        || reference.file.is_empty()
        || reference.file.len() > 4096
        || Path::new(&reference.file)
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        bail!("invalid relative artifact reference or checksum");
    }
    Ok(())
}
fn resolve_ref(root: &Path, reference: &ArtifactRef) -> Result<PathBuf> {
    validate_ref(reference)?;
    let root = fs::canonicalize(root)?;
    let path = fs::canonicalize(root.join(&reference.file))?;
    if !path.starts_with(&root) || !fs::metadata(&path)?.is_file() {
        bail!("artifact escapes the admitted root");
    }
    Ok(path)
}
fn read_bounded_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        bail!("manifest is not a bounded regular file");
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("manifest grew beyond byte budget");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn read_pinned<T: serde::de::DeserializeOwned>(path: &Path, expected: &str) -> Result<T> {
    if !valid_sha256(expected) {
        bail!("invalid expected manifest checksum");
    }
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        bail!("pinned manifest exceeds byte budget");
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES || bytes_digest(&bytes) != expected {
        bail!("pinned manifest bytes do not match expected checksum");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn publish_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let parent = path.parent().context("publish parent missing")?;
    fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    match fs::hard_link(staged.path(), path) {
        Ok(()) => {
            File::open(parent)?.sync_all()?;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::read(path)? != bytes {
                bail!("immutable publication conflicts with existing bytes");
            }
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
fn replace_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("cursor parent missing")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut staged, value)?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn claim(path: &Path) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match fs4::FileExt::try_lock(&file) {
        Ok(()) => Ok(Some(file)),
        Err(fs4::TryLockError::WouldBlock) => Ok(None),
        Err(fs4::TryLockError::Error(error)) => Err(error.into()),
    }
}
fn validate_identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        bail!("invalid SQL identifier");
    }
    Ok(())
}

fn range_contains(ranges: &[AllowedRange], observed: i64, available: i64) -> bool {
    ranges.iter().any(|range| {
        observed >= range.start_ms
            && observed < range.end_ms
            && available >= observed
            && available < range.end_ms
    })
}
fn decode_json_rows<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<Vec<T>> {
    std::str::from_utf8(bytes)?
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn verified_lines(root: &Path, shard: &SequenceShardV1) -> Result<BufReader<File>> {
    let reference = ArtifactRef {
        file: shard.file.clone(),
        sha256: shard.sha256.clone(),
    };
    let path = resolve_ref(root, &reference)?;
    let mut file = File::open(path)?;
    if file.metadata()?.len() != shard.bytes {
        bail!("canonical shard byte count mismatch");
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    if format!("{:x}", hash.finalize()) != shard.sha256 {
        bail!("canonical shard checksum mismatch before insertion");
    }
    use std::io::Seek;
    file.rewind()?;
    Ok(BufReader::with_capacity(64 * 1024, file))
}
fn read_line(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let read = reader
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() > MAX_FRAME_BYTES || line.last() != Some(&b'\n') {
        bail!("canonical frame is oversized or truncated");
    }
    line.pop();
    if line.is_empty() {
        bail!("empty canonical frame");
    }
    Ok(Some(line))
}
fn check_clock(previous: &mut Option<i64>, clock: i64) -> Result<()> {
    if previous.is_some_and(|p| clock <= p) {
        bail!("duplicate/unordered canonical frame clock");
    }
    *previous = Some(clock);
    Ok(())
}
fn check_shard(
    shard: &SequenceShardV1,
    rows: u64,
    first: Option<i64>,
    last: Option<i64>,
) -> Result<()> {
    if rows != shard.rows
        || first != Some(shard.first_observed_at_ms)
        || last != Some(shard.last_observed_at_ms)
    {
        bail!("canonical shard row count or bounds mismatch");
    }
    Ok(())
}

struct Binary<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Binary<'a> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self
            .offset
            .checked_add(N)
            .context("RowBinary offset overflow")?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .context("truncated RowBinary row")?;
        self.offset = end;
        Ok(bytes.try_into().expect("fixed slice size"))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take()?))
    }
    fn varint(&mut self) -> Result<usize> {
        let mut value = 0_u64;
        for shift in (0..=63).step_by(7) {
            let byte = self.take::<1>()?[0];
            if shift == 63 && byte > 1 {
                bail!("RowBinary varint overflow");
            }
            value |= u64::from(byte & 127) << shift;
            if byte & 128 == 0 {
                return usize::try_from(value).context("RowBinary length overflow");
            }
        }
        bail!("RowBinary invalid varint")
    }
    fn row_identity(&mut self, expected: &str) -> Result<()> {
        let len = self.varint()?;
        if len != 64 {
            bail!("invalid RowBinary row identity length");
        }
        let bytes = self.take::<64>()?;
        if bytes.as_slice() != expected.as_bytes() {
            bail!("actual ClickHouse projected numeric columns disagree with row identity");
        }
        Ok(())
    }
}
fn decode_features(bytes: &[u8]) -> Result<Vec<MarketFeatureFrameV1>> {
    let mut reader = Binary { bytes, offset: 0 };
    let mut rows = Vec::new();
    while reader.offset < bytes.len() {
        let series_id = reader.u64()?;
        let observed_at_ms = reader.i64()?;
        let feature_max_available_at_ms = reader.i64()?;
        let len = reader.varint()?;
        if len == 0 || len > 64 {
            bail!("RowBinary feature channel budget exceeded");
        }
        let mut channels = Vec::with_capacity(len);
        for _ in 0..len {
            channels.push(f32::from_bits(u32::from_le_bytes(reader.take()?)));
        }
        let row = MarketFeatureFrameV1 {
            series_id,
            observed_at_ms,
            feature_max_available_at_ms,
            channels,
        };
        reader.row_identity(&bytes_digest(&feature_bytes(&row)))?;
        rows.push(row);
        if rows.len() > PAGE_ROWS {
            bail!("RowBinary feature page row budget exceeded");
        }
    }
    Ok(rows)
}
fn decode_targets(bytes: &[u8]) -> Result<Vec<MarketTargetFrameV1>> {
    let mut reader = Binary { bytes, offset: 0 };
    let mut rows = Vec::new();
    while reader.offset < bytes.len() {
        let row = MarketTargetFrameV1 {
            series_id: reader.u64()?,
            observed_at_ms: reader.i64()?,
            available_at_ms: reader.i64()?,
            simple_return: f32::from_bits(u32::from_le_bytes(reader.take()?)),
            spread_bps: f64::from_bits(u64::from_le_bytes(reader.take()?)),
        };
        reader.row_identity(&bytes_digest(&target_bytes(&row)))?;
        rows.push(row);
        if rows.len() > PAGE_ROWS {
            bail!("RowBinary target page row budget exceeded");
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn data_service_clickhouse_posts_bind_empty_and_nonempty_body_lengths() {
        for body in [Vec::new(), vec![1, 2, 3]] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let expected_length = body.len();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut headers = Vec::new();
                while !headers.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let mut chunk = [0; 1024];
                    let read = stream.read(&mut chunk).unwrap();
                    assert!(read > 0 && headers.len() + read <= 8192);
                    headers.extend_from_slice(&chunk[..read]);
                }
                let headers = String::from_utf8_lossy(&headers).to_ascii_lowercase();
                let valid = headers
                    .lines()
                    .any(|line| line.trim() == format!("content-length: {expected_length}"));
                let status = if valid {
                    "200 OK"
                } else {
                    "411 Length Required"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                valid
            });
            let client = ClickHouse {
                client: reqwest::Client::builder().no_proxy().build().unwrap(),
                endpoint: format!("http://{address}/"),
                database: "test".into(),
                user: "test".into(),
                password: String::new(),
            };
            assert!(client.query("SELECT 1", &[], body).await.is_ok());
            assert!(server.join().unwrap());
        }
    }

    fn hash(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }
    fn request() -> DataRequest {
        DataRequest {
            schema_version: "monday.market_data_request.v1".into(),
            sources: vec![DataSource {
                feature_dataset_sha256: hash('a'),
                target_dataset_sha256: None,
            }],
            transform_sha256: hash('b'),
            input: SequenceInputSpecV1::sol_lob(),
            view: SequenceViewV1 {
                history_start_ms: 0,
                decision_start_ms: 59_000,
                end_ms: 120_000,
                decision_stride_ms: 1_000,
            },
            anchor_end_ms: 120_000,
            purpose: "label_free".into(),
            qualified_anchors: true,
        }
    }
    fn receipt() -> IngestReceipt {
        IngestReceipt {
            schema_version: "monday.market_ingest_receipt.v1".into(),
            sequence: 1,
            previous_receipt_sha256: None,
            features: ArtifactRef {
                file: "feature-manifest.json".into(),
                sha256: hash('a'),
            },
            targets: None,
            transform_sha256: hash('b'),
            source_manifest_sha256: hash('c'),
            source_manifest: ArtifactRef {
                file: "sources.json".into(),
                sha256: hash('c'),
            },
            allowed_ranges: vec![AllowedRange {
                start_ms: 0,
                end_ms: 120_000,
            }],
            pre_holdout_supervised: false,
        }
    }
    #[test]
    fn data_service_claim_rejects_busy_owner_and_recovers_after_release() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("partition.lock");
        let owner = claim(&path).unwrap().unwrap();
        assert!(claim(&path).unwrap().is_none());
        drop(owner);
        assert!(claim(&path).unwrap().is_some());
    }
    #[test]
    fn data_service_retry_rejects_changed_or_omitted_optional_feature_window() {
        let artifact = || NativeMarketArtifact {
            file: "artifact.json".into(),
            sha256: hash('a'),
        };
        let mut export = NativeMarketExport {
            schema_version: "monday.market_encoder_export.v1".into(),
            feature_start_received_at_ns: Some(30_000_000_000),
            feature_end_received_at_ns: Some(90_000_000_000),
            feature_sources: artifact(),
            features: artifact(),
            targets: artifact(),
        };
        validate_export_window(&export, Some(30_000_000_000), Some(90_000_000_000)).unwrap();
        assert!(
            validate_export_window(&export, Some(31_000_000_000), Some(90_000_000_000)).is_err()
        );
        assert!(validate_export_window(&export, None, Some(90_000_000_000)).is_err());
        assert!(validate_export_window(&export, Some(30_000_000_000), None).is_err());
        export.feature_start_received_at_ns = None;
        export.feature_end_received_at_ns = None;
        validate_export_window(&export, None, None).unwrap();
    }
    #[test]
    fn data_service_sources_reject_forged_success_marker_and_duplicate_segment() {
        let segment_hash = hash('b');
        let segment = SourceSegment {
            path: "slice.jsonl.zst".into(),
            sha256: segment_hash.clone(),
            collector_manifest_path: "slice.jsonl.zst.manifest.json".into(),
            collector_manifest_sha256: hash('c'),
            success_marker_path: "slice.jsonl.zst._SUCCESS".into(),
            success_marker_sha256: bytes_digest(format!("{segment_hash}\n").as_bytes()),
            start_received_at_ns: 0,
            end_received_at_ns: 1_000_000_000,
            events: 2,
        };
        let features = MarketFeatureDatasetV1 {
            schema_version: FEATURE_SCHEMA.into(),
            venue: "binance-usdm".into(),
            symbol: "SOLUSDT".into(),
            source_manifest_sha256: hash('d'),
            input: SequenceInputSpecV1::sol_lob(),
            shards: vec![SequenceShardV1 {
                file: "source.jsonl".into(),
                sha256: hash('a'),
                bytes: 1,
                rows: 2,
                first_observed_at_ms: 0,
                last_observed_at_ms: 1000,
            }],
        };
        let mut source = FeatureSources {
            schema_version: "monday.market_feature_sources.v1".into(),
            market: "usdm".into(),
            symbol: "SOLUSDT".into(),
            replay_clock: "received_at_ns".into(),
            source_revision: data::binance_lob_replay::source_revision([segment_hash.as_str()]),
            source_segments: vec![segment],
            feature_start_received_at_ns: None,
            feature_end_received_at_ns: None,
            first_feature_observed_at_ms: 0,
            last_feature_observed_at_ms: 1000,
            first_dependency_received_at_ns: 0,
            last_dependency_received_at_ns: 1_000_000_000,
        };
        source.validate(&features).unwrap();
        let success = source.source_segments[0].success_marker_sha256.clone();
        source.source_segments[0].success_marker_sha256 = hash('e');
        assert!(source.validate(&features).is_err());
        source.source_segments[0].success_marker_sha256 = success;
        source
            .source_segments
            .push(source.source_segments[0].clone());
        source.source_revision = data::binance_lob_replay::source_revision(
            source.source_segments.iter().map(|s| s.sha256.as_str()),
        );
        assert!(source.validate(&features).is_err());
    }
    fn row_binary(row: &MarketFeatureFrameV1) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(row.series_id.to_le_bytes());
        bytes.extend(row.observed_at_ms.to_le_bytes());
        bytes.extend(row.feature_max_available_at_ms.to_le_bytes());
        bytes.push(row.channels.len() as u8);
        for value in &row.channels {
            bytes.extend(value.to_bits().to_le_bytes());
        }
        bytes.push(64);
        bytes.extend(bytes_digest(&feature_bytes(row)).as_bytes());
        bytes
    }
    #[test]
    fn data_service_view_identity_has_no_optimizer_or_consumer_parameters() {
        let request = request();
        let identity = request.identity().unwrap();
        let mut changed = request.clone();
        changed.view.decision_stride_ms = 2_000;
        assert_ne!(changed.identity().unwrap(), identity);
        changed = request.clone();
        changed.sources.push(DataSource {
            feature_dataset_sha256: hash('d'),
            target_dataset_sha256: None,
        });
        assert_ne!(changed.identity().unwrap(), identity);
        let mut unknown = serde_json::to_value(request).unwrap();
        unknown["seed"] = 7.into();
        assert!(serde_json::from_value::<DataRequest>(unknown).is_err());
    }
    #[test]
    fn data_service_queue_cursor_rejects_rewrite_reorder_and_gap() {
        let first = receipt();
        let first_hash = digest(&first).unwrap();
        let mut second = first.clone();
        second.sequence = 2;
        second.previous_receipt_sha256 = Some(first_hash.clone());
        second.features.sha256 = hash('d');
        let committed = Cursor {
            sequence: 1,
            receipt_sha256: Some(first_hash),
        };
        validate_queue(&[first.clone(), second.clone()], &committed).unwrap();
        let mut rewritten = first.clone();
        rewritten.transform_sha256 = hash('e');
        assert!(validate_queue(&[rewritten, second.clone()], &committed).is_err());
        assert!(validate_queue(&[second.clone(), first.clone()], &committed).is_err());
        second.sequence = 3;
        assert!(validate_queue(&[first, second], &committed).is_err());
    }
    #[test]
    fn data_service_enqueue_deduplicates_content_and_rejects_admission_drift_transactionally() {
        let root = tempfile::tempdir().unwrap();
        let first = receipt();
        let result = enqueue_receipt(root.path(), first.clone()).unwrap();
        assert_eq!(result["sequence"], 1);
        assert_eq!(result["reused"], false);
        let queue = root.path().join("receipt-queue.json");
        let previous = fs::read(&queue).unwrap();
        let mut alias = first.clone();
        alias.features.file = "different-run/feature.json".into();
        alias.source_manifest.file = "different-run/sources.json".into();
        let reused = enqueue_receipt(root.path(), alias).unwrap();
        assert_eq!(reused["reused"], true);
        assert_eq!(fs::read(&queue).unwrap(), previous);
        let mut drift = first.clone();
        drift.transform_sha256 = hash('d');
        assert!(enqueue_receipt(root.path(), drift).is_err());
        assert_eq!(fs::read(&queue).unwrap(), previous);
        let mut next = first;
        next.features.sha256 = hash('e');
        enqueue_receipt(root.path(), next).unwrap();
        let queue: Vec<IngestReceipt> = read_bounded_json(&queue).unwrap();
        assert_eq!(queue.len(), 2);
        validate_queue(
            &queue,
            &Cursor {
                sequence: 0,
                receipt_sha256: None,
            },
        )
        .unwrap();
    }
    #[test]
    fn data_service_rowbinary_preserves_float_bits_and_rejects_actual_value_tampering() {
        let row = MarketFeatureFrameV1 {
            series_id: u64::MAX,
            observed_at_ms: 1_000,
            feature_max_available_at_ms: 999,
            channels: vec![-0.0, f32::from_bits(0x3e000001)],
        };
        let bytes = row_binary(&row);
        let actual = decode_features(&bytes).unwrap();
        assert_eq!(feature_bytes(&actual[0]), feature_bytes(&row));
        let mut corrupt = bytes.clone();
        corrupt[25] ^= 1;
        assert!(decode_features(&corrupt).is_err());
        assert!(decode_features(&bytes[..bytes.len() - 1]).is_err());
        let mut oversized = bytes;
        oversized[24] = 65;
        assert!(decode_features(&oversized).is_err());
    }
    #[test]
    fn data_service_target_maturity_and_traversal_never_enter_an_admitted_view() {
        let allowed = [AllowedRange {
            start_ms: 100_000,
            end_ms: 200_000,
        }];
        assert!(range_contains(&allowed, 150_000, 180_000));
        assert!(!range_contains(&allowed, 170_000, 200_000));
        assert!(!range_contains(&allowed, 99_000, 130_000));
        assert!(!range_contains(&allowed, 150_000, 149_000));
        let mut source = receipt();
        source.source_manifest.file = "../source.json".into();
        assert!(source.validate().is_err());
        source = receipt();
        source.targets = Some(ArtifactRef {
            file: "targets.json".into(),
            sha256: hash('d'),
        });
        assert!(source.validate().is_err());
        let mut view = request();
        view.purpose = "sealed_holdout".into();
        assert!(view.validate().is_err());
    }
    #[test]
    fn data_service_canonical_line_reader_rejects_unterminated_and_oversized_frames() {
        assert!(read_line(&mut std::io::Cursor::new(b"{\"value\":1}".to_vec())).is_err());
        let mut bytes = vec![b'x'; MAX_FRAME_BYTES];
        bytes.push(b'\n');
        assert!(read_line(&mut std::io::Cursor::new(bytes)).is_err());
        let bytes = b"{\"value\":1}\n";
        assert_eq!(
            read_line(&mut std::io::Cursor::new(bytes))
                .unwrap()
                .unwrap(),
            b"{\"value\":1}"
        );
    }
    #[test]
    fn data_service_ready_reuse_and_missing_partition_need_no_clickhouse_connection() {
        let root = tempfile::tempdir().unwrap();
        let args = Args {
            state_root: root.path().into(),
            clickhouse_url: "http://127.0.0.1:1".into(),
            database: "monday_analytics".into(),
            user: "monday_reader".into(),
            command: Command::Request {
                request: PathBuf::new(),
                request_sha256: hash('a'),
            },
        };
        let client = ClickHouse::new(&args).unwrap();
        let mut request = request();
        request.qualified_anchors = false;
        let missing = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(request_view(&client, root.path(), request.clone()))
            .unwrap();
        assert_eq!(missing["state"], "preparing");
        let destination = root.path().join("views").join(request.identity().unwrap());
        fs::create_dir_all(destination.join("features")).unwrap();
        let rows = (0..120)
            .map(|i| MarketFeatureFrameV1 {
                series_id: 0,
                observed_at_ms: i * 1000,
                feature_max_available_at_ms: i * 1000,
                channels: vec![i as f32; 24],
            })
            .collect::<Vec<_>>();
        let shard = write_feature_parquet_shard(
            &destination.join("features"),
            "part-000000.parquet",
            &rows,
            &request.input,
        )
        .unwrap();
        let feature = MarketFeatureDatasetV1 {
            schema_version: FEATURE_PARQUET_SCHEMA.into(),
            venue: "binance-usdm".into(),
            symbol: "SOLUSDT".into(),
            source_manifest_sha256: hash('c'),
            input: request.input.clone(),
            shards: vec![shard],
        };
        let feature_hash = feature.digest().unwrap();
        publish_json(&destination.join("features/manifest.json"), &feature).unwrap();
        let prepared = PreparedMarketViewV1 {
            schema_version: PREPARED_MARKET_VIEW_SCHEMA.into(),
            sources: vec![PreparedMarketSourceV1 {
                feature_dataset_sha256: hash('a'),
                target_dataset_sha256: None,
                source_manifest_sha256: hash('c'),
                transform_sha256: hash('b'),
            }],
            source_feature_dataset_sha256: hash('a'),
            source_target_dataset_sha256: None,
            source_manifest_sha256: hash('c'),
            transform_sha256: hash('b'),
            data_watermark_ms: 120_000,
            view: request.view,
            feature_dataset_sha256: feature_hash.clone(),
            target_dataset_sha256: None,
            qualified_anchors_sha256: None,
            series: vec![PreparedMarketSeriesV1 {
                series_id: 0,
                first_observed_at_ms: 0,
                last_observed_at_ms: 119_000,
                rows: 120,
            }],
            gaps: vec![],
        };
        let ready = ReadyReceipt {
            schema_version: "monday.market_ready_receipt.v2".into(),
            producer_source_revision: "a".repeat(40),
            producer_image: format!("registry.example/monday/research-data@sha256:{}", hash('b')),
            request_sha256: request.identity().unwrap(),
            request: request.clone(),
            prepared_view_sha256: prepared.digest().unwrap(),
            prepared_view: prepared,
            feature_manifest: ArtifactRef {
                file: "features/manifest.json".into(),
                sha256: feature_hash,
            },
            target_manifest: None,
            qualified_anchors: None,
        };
        publish_json(
            &destination.join("prepared-view.json"),
            &ready.prepared_view,
        )
        .unwrap();
        publish_json(&destination.join("_READY.json"), &ready).unwrap();
        let mut large = ready.clone();
        large.prepared_view.gaps = (0..65_536)
            .map(|i| PreparedMarketGapV1 {
                last_before_ms: i * 3000,
                first_after_ms: i * 3000 + 2000,
            })
            .collect();
        let summary = ready_summary(&destination, &large);
        assert_eq!(summary["gap_count"], 65_536);
        assert_eq!(summary["gaps_preview"].as_array().unwrap().len(), 32);
        assert!(summary.get("gaps").is_none());
        assert!(summary.get("series").is_none());
        assert!(response_bytes(&summary).unwrap().len() < 8 * 1024);
        assert!(response_bytes(&"x".repeat(MAX_RECEIPT_BYTES + 1)).is_err());
        let reused = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(request_view(&client, root.path(), request.clone()))
            .unwrap();
        assert_eq!(reused["state"], "ready");
        let file = destination.join("features/part-000000.parquet");
        let original = fs::read(&file).unwrap();
        let ready_bytes = fs::read(destination.join("_READY.json")).unwrap();
        let mut corrupt = original.clone();
        corrupt[0] ^= 1;
        fs::write(&file, &corrupt).unwrap();
        assert!(read_ready(root.path(), &destination, &request).is_err());
        let replacement = destination.join("features/.restored.parquet");
        fs::write(&replacement, &original).unwrap();
        fs::rename(replacement, &file).unwrap();
        let restored = read_ready(root.path(), &destination, &request).unwrap();
        assert_eq!(restored["state"], "ready");
        assert_eq!(
            fs::read(destination.join("_READY.json")).unwrap(),
            ready_bytes
        );
        OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_len(1)
            .unwrap();
        assert!(read_ready(root.path(), &destination, &request).is_err());
    }
}
