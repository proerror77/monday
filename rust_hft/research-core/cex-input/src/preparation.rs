//! Read-only verification of immutable CEX preparation evidence. No acquisition or process launch.
use crate::inventory::{
    freeze_inventory_from_selection, selection_fingerprint_from_inputs, FreshWindowMode,
    FreshWindowSelection, FRESH_WINDOW_SELECTION_SCHEMA,
};
use anyhow::{bail, Context};
use hft_research_artifacts::sha256_file;
use hft_research_manifest::binance::Market;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone)]
#[cfg_attr(feature = "preparation-cli", derive(clap::Args))]
pub struct PreparationArgs {
    /// Exact source compiled into the caller; not user supplied.
    #[cfg_attr(feature = "preparation-cli", arg(skip))]
    pub source_revision: String,
    /// Read-only root of sealed raw collector triplets.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub raw_root: PathBuf,
    /// Read-only root of published Binance Spot or USD-M reference triplets.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub reference_root: PathBuf,
    /// Binance market whose raw and reference identities are frozen together.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub market: String,
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub start_received_at_ns: Option<u64>,
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub end_received_at_ns: Option<u64>,
    /// Latest-window duration in nanoseconds. Mutually exclusive with an explicit window.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub duration_ns: Option<u64>,
    /// Latest-window receive-time cutoff. When omitted, the first request resolves and persists now.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub cutoff_received_at_ns: Option<u64>,
    /// Maximum number of latest-window candidates to inspect.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub max_candidates: Option<usize>,
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub symbol: String,
    /// Producer image digest recorded in the frozen inventory.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub image_ref: String,
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub mission_id: String,
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub output_prefix: String,
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub bucket_ms: u64,
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub label_horizon_buckets: u64,
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub top_depth: usize,
    #[cfg_attr(feature = "preparation-cli", arg(long, default_value_t = 100_000))]
    pub max_scan_entries: usize,
    /// Maximum number of frozen raw and reference inputs.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub max_inputs: usize,
    /// Maximum total source bytes verified by the collector freezer.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub max_input_bytes: u64,
    /// Create-once frozen.env output retained as the preparation identity.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub inventory_out: PathBuf,
    /// Mandatory create-once preparation request binding the fresh window and limits.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub request_out: PathBuf,
    /// Create-once campaign-inputs.json output produced by the existing materializer.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub campaign_inputs_out: PathBuf,
    /// Parent of the materializer run-specific output prefix.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub output_root: PathBuf,
    /// Existing cex-materialization-entrypoint.sh.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub materializer: PathBuf,
    /// Private work directory for the existing materializer.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub materializer_work_dir: PathBuf,
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub binary_dir: Option<PathBuf>,
    /// Export verified SOL 1s/top5/30s sequence inputs along with PIT artifacts.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub sequence_output: bool,
    /// Export separate label-free market features and 30-second targets.
    #[cfg_attr(feature = "preparation-cli", arg(long))]
    pub market_encoder_output: bool,
    /// Feature start after at most 60 seconds of admitted raw/PIT warmup.
    #[cfg_attr(
        feature = "preparation-cli",
        arg(long, requires = "market_encoder_output")
    )]
    pub market_feature_start_received_at_ns: Option<u64>,
    /// Exclusive feature-partition end inside the explicit admitted label window.
    #[cfg_attr(
        feature = "preparation-cli",
        arg(long, requires = "market_encoder_output")
    )]
    pub market_feature_end_received_at_ns: Option<u64>,
    /// Upper bound for the materializer process lifetime.
    #[cfg_attr(feature = "preparation-cli", arg(long, default_value_t = 7_200))]
    pub materializer_timeout_seconds: u64,
    /// Upper bound for captured materializer stdout and stderr combined.
    #[cfg_attr(feature = "preparation-cli", arg(long, default_value_t = 16 * 1024 * 1024))]
    pub max_materializer_output_bytes: u64,
    /// Completed create-once report required for controller handoff.
    #[cfg_attr(feature = "preparation-cli", arg(long, required = true))]
    pub report_out: Option<PathBuf>,
}

pub const CAMPAIGN_INPUTS_SCHEMA: &str = "monday.cex_campaign_inputs.v1";
pub const MATERIALIZATION_RECEIPT_SCHEMA: &str = "monday.cex_materialization_receipt.v1";
pub const PREPARATION_SCHEMA: &str = "monday.cex_fresh_inputs_preparation.v1";
pub const PREPARATION_REQUEST_SCHEMA: &str = "monday.cex_fresh_inputs_request.v1";
pub const MAX_PREPARATION_BYTES: u64 = 1024 * 1024;
pub const MAX_MARKET_METADATA_BYTES: u64 = 16 * 1024 * 1024;

pub fn metadata_limit(args: &PreparationArgs) -> u64 {
    if args.market_encoder_output {
        MAX_MARKET_METADATA_BYTES
    } else {
        MAX_PREPARATION_BYTES
    }
}
pub const MAX_TIMEOUT_SECONDS: u64 = 24 * 60 * 60;
pub const MAX_INPUTS: usize = 8192;
pub const MAX_SCAN_ENTRIES: usize = 100_000;
pub const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const MAX_MATERIALIZER_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreparationRequest {
    pub schema_version: String,
    pub raw_root: PathBuf,
    pub reference_root: PathBuf,
    pub selection: FreshWindowMode,
    pub market: String,
    pub symbol: String,
    pub image_ref: String,
    pub mission_id: String,
    pub output_root: PathBuf,
    pub output_prefix: String,
    pub bucket_ms: u64,
    pub label_horizon_buckets: u64,
    pub top_depth: usize,
    pub max_scan_entries: usize,
    pub max_inputs: usize,
    pub max_input_bytes: u64,
    pub materializer: PathBuf,
    pub binary_dir: Option<PathBuf>,
    pub materializer_work_dir: PathBuf,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sequence_output: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub market_encoder_output: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_feature_start_received_at_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_feature_end_received_at_ns: Option<u64>,
    pub materializer_timeout_seconds: u64,
    pub max_materializer_output_bytes: u64,
    pub inventory_path: PathBuf,
    pub request_path: PathBuf,
    pub selection_path: PathBuf,
    pub campaign_inputs_path: PathBuf,
    pub report_path: Option<PathBuf>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignInputsReceipt {
    pub schema_version: String,
    pub run_id: String,
    pub source_revision: String,
    pub image_ref: String,
    pub mission_id: String,
    pub market: String,
    pub symbol: String,
    pub output_prefix: String,
    pub output_object_base_url: String,
    pub readback_scope: String,
    pub feature: CampaignInputItem,
    pub materialization: CampaignInputItem,
    pub replay_artifact: CampaignInputItem,
    pub replay_manifest: CampaignInputItem,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignInputItem {
    pub relative_path: PathBuf,
    pub object_url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializationReceipt {
    pub schema_version: String,
    pub run_id: String,
    pub source_revision: String,
    pub image_ref: String,
    pub inventory_sha256: String,
    pub campaign_inputs_sha256: String,
    pub feature_sha256: String,
    pub materialization_sha256: String,
    pub replay_artifact_sha256: String,
    pub replay_manifest_sha256: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreparationReport {
    pub schema_version: String,
    pub status: String,
    pub source_revision: String,
    pub image_ref: String,
    pub market: String,
    pub symbol: String,
    pub mission_id: String,
    pub selection: FreshWindowMode,
    pub selected_start_received_at_ns: u64,
    pub selected_end_received_at_ns: u64,
    pub max_inputs: usize,
    pub max_input_bytes: u64,
    pub input_root: PathBuf,
    pub run_root: PathBuf,
    pub output_prefix: String,
    pub inventory_path: PathBuf,
    pub request_path: PathBuf,
    pub selection_path: PathBuf,
    pub selection_sha256: String,
    pub inventory_eligible: bool,
    pub materialized_pit_admitted: bool,
    pub campaign_inputs_path: PathBuf,
    pub request_sha256: String,
    pub inventory_sha256: String,
    pub input_fingerprint_sha256: String,
    pub campaign_inputs_sha256: String,
    pub materialization_receipt_sha256: String,
    pub feature_sha256: String,
    pub materialization_sha256: String,
    pub replay_artifact_sha256: String,
    pub replay_manifest_sha256: String,
}

#[derive(Debug, serde::Serialize)]
pub struct PreparationOutput {
    #[serde(flatten)]
    pub report: PreparationReport,
    pub reused_existing: bool,
}

#[derive(Debug)]
pub struct VerifiedOutputs {
    pub receipt_sha256: String,
    pub materialization_receipt_sha256: String,
    pub inventory_sha256: String,
    pub feature_sha256: String,
    pub materialization_sha256: String,
    pub replay_artifact_sha256: String,
    pub replay_manifest_sha256: String,
}

pub fn validate_market_window(args: &PreparationArgs) -> anyhow::Result<()> {
    if args.market_encoder_output {
        let (start, end) = args
            .start_received_at_ns
            .zip(args.end_received_at_ns)
            .context("market encoder preparation requires an explicit admitted time window")?;
        let feature_start = args.market_feature_start_received_at_ns.unwrap_or(start);
        if feature_start < start
            || feature_start - start > 60_000_000_000
            || !feature_start.is_multiple_of(1_000_000_000)
            || feature_start >= end
            || !start.is_multiple_of(1_000_000_000)
            || !end.is_multiple_of(1_000_000_000)
            || args
                .market_feature_end_received_at_ns
                .is_some_and(|feature_end| {
                    feature_end <= feature_start
                        || feature_end > end
                        || !feature_end.is_multiple_of(1_000_000_000)
                })
        {
            bail!("market feature partition escapes whole-second admitted clocks");
        }
    } else if args.market_feature_end_received_at_ns.is_some()
        || args.market_feature_start_received_at_ns.is_some()
    {
        bail!("market feature partition requires market encoder output");
    }
    Ok(())
}

pub fn validate_args(args: &PreparationArgs) -> anyhow::Result<()> {
    if args.source_revision.len() != 40
        || !args
            .source_revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("preparation requires an exact compiled source revision");
    }
    parse_market(&args.market)?;
    if (args.sequence_output || args.market_encoder_output)
        && (args.symbol != "SOLUSDT"
            || args.market != "usdm"
            || args.bucket_ms != 1000
            || args.top_depth != 5
            || args.label_horizon_buckets != 30)
    {
        bail!("sequence preparation requires SOLUSDT USD-M 1s/top5/30s");
    }
    validate_market_window(args)?;
    let has_explicit = args.start_received_at_ns.is_some() || args.end_received_at_ns.is_some();
    let has_latest = args.duration_ns.is_some()
        || args.cutoff_received_at_ns.is_some()
        || args.max_candidates.is_some();
    if has_explicit == has_latest {
        bail!("fresh input requires exactly one explicit or latest window mode");
    }
    if has_explicit && (args.start_received_at_ns.is_none() || args.end_received_at_ns.is_none()) {
        bail!("explicit fresh input mode requires both start and end");
    }
    if !has_explicit && (args.duration_ns.is_none() || args.max_candidates.is_none()) {
        bail!("latest fresh input mode requires duration and max candidates");
    }
    if let (Some(start), Some(end)) = (args.start_received_at_ns, args.end_received_at_ns) {
        if start >= end {
            bail!("explicit fresh input mode requires start < end");
        }
    }
    if args.max_inputs == 0 || args.max_inputs > MAX_INPUTS {
        bail!("max-inputs must be between 1 and {MAX_INPUTS}");
    }
    if args.max_scan_entries == 0 || args.max_scan_entries > MAX_SCAN_ENTRIES {
        bail!("max-scan-entries must be between 1 and {MAX_SCAN_ENTRIES}");
    }
    if args.max_input_bytes == 0 || args.max_input_bytes > MAX_INPUT_BYTES {
        bail!("max-input-bytes must be between 1 and {MAX_INPUT_BYTES}");
    }
    if args.materializer_timeout_seconds == 0
        || args.materializer_timeout_seconds > MAX_TIMEOUT_SECONDS
    {
        bail!("materializer-timeout-seconds exceeds the bounded preparation limit");
    }
    if args.max_materializer_output_bytes == 0
        || args.max_materializer_output_bytes > MAX_MATERIALIZER_OUTPUT_BYTES
    {
        bail!("max-materializer-output-bytes exceeds the bounded preparation limit");
    }
    if args.symbol.is_empty()
        || !args
            .symbol
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        bail!("fresh input symbol must be canonical uppercase ASCII");
    }
    if args.materializer.as_os_str().is_empty()
        || args.inventory_out.as_os_str().is_empty()
        || args.campaign_inputs_out.as_os_str().is_empty()
    {
        bail!("fresh input preparation paths are required");
    }
    validate_relative_prefix(&args.output_prefix)?;
    hft_research_manifest::canonical_image_digest(&args.image_ref).map_err(anyhow::Error::msg)?;
    Ok(())
}

pub fn parse_market(value: &str) -> anyhow::Result<Market> {
    let market = value.parse::<Market>().map_err(anyhow::Error::msg)?;
    if value != market.as_str() {
        bail!("fresh input market must be canonical lowercase: {value}");
    }
    Ok(market)
}

pub fn validate_relative_prefix(value: &str) -> anyhow::Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        bail!("fresh output-prefix must be a non-empty safe relative path");
    }
    Ok(())
}

pub fn read_only_root(path: &Path, label: &str) -> anyhow::Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("{label} does not exist: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{label} must be a real directory: {}", path.display());
    }
    let canonical = path.canonicalize()?;
    if fs::symlink_metadata(&canonical)?.file_type().is_symlink() {
        bail!(
            "{label} resolves through a symbolic link: {}",
            path.display()
        );
    }
    Ok(canonical)
}

pub fn normalize_for_compare(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut suffix = Vec::new();
    let mut existing = absolute.as_path();
    while !existing.exists() {
        suffix.push(
            existing
                .file_name()
                .context("fresh input path has no file name")?
                .to_owned(),
        );
        existing = existing
            .parent()
            .context("fresh input path has no existing parent")?;
    }
    let mut normalized = existing.canonicalize()?;
    for component in suffix.iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

pub fn expected_request_path(output_root: &Path, output_prefix: &str) -> PathBuf {
    output_root
        .join(".fresh-inputs")
        .join(output_prefix)
        .join("request.json")
}

pub fn expected_selection_path(output_root: &Path, output_prefix: &str) -> PathBuf {
    output_root
        .join(".fresh-inputs")
        .join(output_prefix)
        .join("selection.json")
}

pub fn current_time_ns() -> anyhow::Result<u64> {
    u64::try_from(
        chrono::Utc::now()
            .timestamp_nanos_opt()
            .context("fresh window wall clock is out of range")?,
    )
    .context("fresh window wall clock is out of range")
}

pub fn resolve_selection_mode(
    args: &PreparationArgs,
    existing: Option<&PreparationRequest>,
) -> anyhow::Result<FreshWindowMode> {
    validate_market_window(args)?;
    let has_explicit = args.start_received_at_ns.is_some() || args.end_received_at_ns.is_some();
    let has_latest = args.duration_ns.is_some()
        || args.cutoff_received_at_ns.is_some()
        || args.max_candidates.is_some();
    if has_explicit && has_latest {
        bail!("explicit and latest fresh window modes are mutually exclusive");
    }
    if has_explicit {
        return Ok(FreshWindowMode::Explicit {
            start_received_at_ns: args
                .start_received_at_ns
                .context("explicit fresh window start is required")?,
            end_received_at_ns: args
                .end_received_at_ns
                .context("explicit fresh window end is required")?,
        });
    }
    let duration_ns = args
        .duration_ns
        .context("latest fresh window duration is required")?;
    let max_candidates = args
        .max_candidates
        .context("latest fresh window max candidates is required")?;
    let cutoff_received_at_ns = if let Some(cutoff) = args.cutoff_received_at_ns {
        cutoff
    } else if let Some(request) = existing {
        match &request.selection {
            FreshWindowMode::Latest {
                cutoff_received_at_ns,
                ..
            } => *cutoff_received_at_ns,
            FreshWindowMode::Explicit { .. } => current_time_ns()?,
        }
    } else {
        current_time_ns()?
    };
    Ok(FreshWindowMode::Latest {
        duration_ns,
        cutoff_received_at_ns,
        max_candidates,
    })
}

pub fn build_preparation_request(
    args: &PreparationArgs,
    raw_root: &Path,
    reference_root: &Path,
    output_root: &Path,
    selection: FreshWindowMode,
    selection_path: &Path,
) -> anyhow::Result<PreparationRequest> {
    let binary_dir = args
        .binary_dir
        .as_deref()
        .map(|path| normalize_for_compare(path))
        .transpose()?;
    Ok(PreparationRequest {
        schema_version: PREPARATION_REQUEST_SCHEMA.to_string(),
        raw_root: raw_root.to_path_buf(),
        reference_root: reference_root.to_path_buf(),
        selection,
        market: args.market.clone(),
        symbol: args.symbol.clone(),
        image_ref: args.image_ref.clone(),
        mission_id: args.mission_id.clone(),
        output_root: output_root.to_path_buf(),
        output_prefix: args.output_prefix.clone(),
        bucket_ms: args.bucket_ms,
        label_horizon_buckets: args.label_horizon_buckets,
        top_depth: args.top_depth,
        max_scan_entries: args.max_scan_entries,
        max_inputs: args.max_inputs,
        max_input_bytes: args.max_input_bytes,
        materializer: normalize_for_compare(&args.materializer)?,
        binary_dir,
        materializer_work_dir: normalize_for_compare(&args.materializer_work_dir)?,
        sequence_output: args.sequence_output,
        market_encoder_output: args.market_encoder_output,
        market_feature_start_received_at_ns: args.market_feature_start_received_at_ns,
        market_feature_end_received_at_ns: args.market_feature_end_received_at_ns,
        materializer_timeout_seconds: args.materializer_timeout_seconds,
        max_materializer_output_bytes: args.max_materializer_output_bytes,
        inventory_path: normalize_for_compare(&args.inventory_out)?,
        request_path: normalize_for_compare(&args.request_out)?,
        selection_path: normalize_for_compare(selection_path)?,
        campaign_inputs_path: normalize_for_compare(&args.campaign_inputs_out)?,
        report_path: args
            .report_out
            .as_deref()
            .map(normalize_for_compare)
            .transpose()?,
    })
}

pub fn inventory_from_selection(
    args: &PreparationArgs,
    selection: &FreshWindowSelection,
) -> anyhow::Result<crate::inventory::FrozenInventory> {
    let request = crate::inventory::InventoryRequest {
        raw_root: args.raw_root.clone(),
        reference_root: args.reference_root.clone(),
        start_received_at_ns: selection.selected_start_received_at_ns,
        end_received_at_ns: selection.selected_end_received_at_ns,
        market: parse_market(&args.market)?,
        symbol: args.symbol.clone(),
        source_revision: args.source_revision.clone(),
        image_ref: args.image_ref.clone(),
        mission_id: args.mission_id.clone(),
        output_prefix: args.output_prefix.clone(),
        bucket_ms: args.bucket_ms,
        label_horizon_buckets: args.label_horizon_buckets,
        top_depth: args.top_depth,
        max_scan_entries: args.max_scan_entries,
        max_inputs: args.max_inputs,
        max_input_bytes: args.max_input_bytes,
    };
    freeze_inventory_from_selection(&request, selection)
}

pub fn read_selection(
    path: &Path,
    request: &PreparationRequest,
    args: &PreparationArgs,
) -> anyhow::Result<(FreshWindowSelection, String)> {
    let bytes = read_file_bounded(path, metadata_limit(args), "fresh window selection")?;
    let selection: FreshWindowSelection = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse fresh window selection {}", path.display()))?;
    validate_selection(&selection, request, args)?;
    Ok((selection, hex::encode(Sha256::digest(&bytes))))
}

pub fn validate_selection(
    selection: &FreshWindowSelection,
    request: &PreparationRequest,
    args: &PreparationArgs,
) -> anyhow::Result<()> {
    let market = parse_market(&args.market)?;
    if selection.schema_version != FRESH_WINDOW_SELECTION_SCHEMA
        || selection.mode != request.selection
        || selection.market != market
        || request.market != args.market
        || !selection.inventory_eligible
        || selection.materialized_pit_admitted
        || selection.selected_start_received_at_ns >= selection.selected_end_received_at_ns
    {
        bail!("fresh window selection identity is invalid");
    }
    match &selection.mode {
        FreshWindowMode::Explicit {
            start_received_at_ns,
            end_received_at_ns,
        } if selection.selected_start_received_at_ns == *start_received_at_ns
            && selection.selected_end_received_at_ns == *end_received_at_ns => {}
        FreshWindowMode::Latest {
            duration_ns,
            cutoff_received_at_ns,
            ..
        } if selection.selected_end_received_at_ns <= *cutoff_received_at_ns
            && selection
                .selected_end_received_at_ns
                .saturating_sub(selection.selected_start_received_at_ns)
                >= *duration_ns => {}
        _ => bail!("fresh window selection bounds do not match its request"),
    }
    let total = selection
        .raw
        .len()
        .checked_add(selection.references.len())
        .context("fresh window selection input count overflowed")?;
    if total == 0 || total > args.max_inputs || total > MAX_INPUTS {
        bail!("fresh window selection exceeds the input count budget");
    }
    let verified_bytes = selection
        .raw
        .iter()
        .chain(&selection.references)
        .try_fold(0_u64, |total, input| total.checked_add(input.bytes))
        .context("fresh window selection byte count overflowed")?;
    if verified_bytes != selection.verified_bytes
        || verified_bytes > args.max_input_bytes
        || selection.verification_bytes < selection.verified_bytes
        || selection.verification_bytes > args.max_input_bytes
    {
        bail!("fresh window selection exceeds the input byte budget");
    }
    for (label, input) in selection.raw.iter().map(|input| ("raw", input)).chain(
        selection
            .references
            .iter()
            .map(|input| ("reference", input)),
    ) {
        if input.relative_path.is_empty()
            || Path::new(&input.relative_path).is_absolute()
            || Path::new(&input.relative_path)
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || input.bytes == 0
            || input.start_received_at_ns > input.end_received_at_ns
            || !is_digest(&input.content_sha256)
            || !is_digest(&input.manifest_sha256)
        {
            bail!("fresh {label} selection input identity is invalid");
        }
    }
    if selection_fingerprint_from_inputs(&selection.raw, &selection.references)?
        != selection.input_fingerprint_sha256
    {
        bail!("fresh window selection fingerprint differs from its inputs");
    }
    Ok(())
}

pub fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn verify_inventory_matches(
    path: &Path,
    expected: &crate::inventory::FrozenInventory,
    max_bytes: u64,
) -> anyhow::Result<()> {
    let bytes = read_file_bounded(path, max_bytes, "frozen inventory")?;
    if bytes != expected.inventory_env.as_bytes() {
        bail!("frozen inventory differs from the create-once window selection");
    }
    Ok(())
}

pub fn validate_existing_inventory(args: &PreparationArgs, path: &Path) -> anyhow::Result<()> {
    let bytes = read_file_bounded(path, metadata_limit(args), "frozen inventory")?;
    let env = String::from_utf8(bytes)?;
    validate_inventory_env(args, &env)
}

pub fn validate_inventory_env(args: &PreparationArgs, env: &str) -> anyhow::Result<()> {
    let values = parse_inventory_values(env)?;
    parse_market(&args.market)?;
    for (key, expected) in [
        ("SOURCE_REVISION", args.source_revision.clone()),
        ("IMAGE_REF", args.image_ref.clone()),
        ("MISSION_ID", args.mission_id.clone()),
        ("MARKET", args.market.clone()),
        ("SYMBOL", args.symbol.clone()),
        ("BUCKET_MS", args.bucket_ms.to_string()),
        (
            "LABEL_HORIZON_BUCKETS",
            args.label_horizon_buckets.to_string(),
        ),
        ("TOP_DEPTH", args.top_depth.to_string()),
        ("OUTPUT_PREFIX", args.output_prefix.clone()),
    ] {
        if values.get(key).copied() != Some(expected.as_str()) {
            bail!("frozen inventory {key} does not match the requested fresh window");
        }
    }
    inventory_counts(&values, args.max_inputs)?;
    let window_start = values
        .get("WINDOW_START_RECEIVED_AT_NS")
        .context("frozen inventory is missing WINDOW_START_RECEIVED_AT_NS")?
        .parse::<u64>()
        .context("frozen inventory window start is invalid")?;
    let window_end = values
        .get("WINDOW_END_RECEIVED_AT_NS")
        .context("frozen inventory is missing WINDOW_END_RECEIVED_AT_NS")?
        .parse::<u64>()
        .context("frozen inventory window end is invalid")?;
    if window_start >= window_end {
        bail!("frozen inventory window bounds are invalid");
    }
    Ok(())
}

pub fn input_fingerprint(
    args: &PreparationArgs,
    request_sha256: &str,
    inventory_sha256: &str,
) -> anyhow::Result<String> {
    if let Some(report_path) = &args.report_out {
        if report_path.is_file() {
            let report: PreparationReport = read_json_bounded(report_path)?;
            if report.request_sha256 != request_sha256
                || report.inventory_sha256 != inventory_sha256
            {
                bail!("fresh preparation report is not bound to the frozen request");
            }
        }
    }
    let env = String::from_utf8(read_file_bounded(
        &args.inventory_out,
        metadata_limit(args),
        "frozen inventory",
    )?)?;
    let values = parse_inventory_values(&env)?;
    let (raw_count, reference_count) = inventory_counts(&values, args.max_inputs)?;
    let total = raw_count
        .checked_add(reference_count)
        .context("frozen inventory input count overflowed")?;
    let mut identities = Vec::with_capacity(total);
    for (prefix, count) in [("RAW_SEGMENT", raw_count), ("REFERENCE", reference_count)] {
        for ordinal in 1..=count {
            let path_key = format!("{prefix}_{ordinal}");
            let content_key = format!("{prefix}_{ordinal}_SHA256");
            let manifest_key = format!("{prefix}_{ordinal}_MANIFEST_SHA256");
            let path = values
                .get(path_key.as_str())
                .with_context(|| format!("frozen inventory is missing {path_key}"))?;
            let content = values
                .get(content_key.as_str())
                .with_context(|| format!("frozen inventory is missing {content_key}"))?;
            let manifest = values
                .get(manifest_key.as_str())
                .with_context(|| format!("frozen inventory is missing {manifest_key}"))?;
            identities.push((path, content, manifest));
        }
    }
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(
        &identities,
    )?)))
}

pub fn parse_inventory_values(env: &str) -> anyhow::Result<BTreeMap<&str, &str>> {
    let mut values = BTreeMap::new();
    for line in env.lines() {
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .with_context(|| format!("frozen inventory line is not key=value: {line}"))?;
        if values.insert(key, value).is_some() {
            bail!("frozen inventory repeats key {key}");
        }
    }
    Ok(values)
}

pub fn inventory_counts(
    values: &BTreeMap<&str, &str>,
    max_inputs: usize,
) -> anyhow::Result<(usize, usize)> {
    let raw_count = values
        .get("RAW_SEGMENT_COUNT")
        .context("frozen inventory is missing RAW_SEGMENT_COUNT")?
        .parse::<usize>()
        .context("frozen inventory RAW_SEGMENT_COUNT is invalid")?;
    if raw_count == 0 {
        bail!("frozen inventory RAW_SEGMENT_COUNT is invalid");
    }
    let reference_count = values
        .get("REFERENCE_COUNT")
        .context("frozen inventory is missing REFERENCE_COUNT")?
        .parse::<usize>()
        .context("frozen inventory REFERENCE_COUNT is invalid")?;
    let total = raw_count
        .checked_add(reference_count)
        .context("frozen inventory input count overflowed")?;
    if total > max_inputs || total > MAX_INPUTS {
        bail!("frozen inventory input count exceeds the bounded limit");
    }
    Ok((raw_count, reference_count))
}

pub fn verify_materialized_outputs(
    args: &PreparationArgs,
    run_root: &Path,
    inventory_path: &Path,
) -> anyhow::Result<VerifiedOutputs> {
    ensure_regular_file(inventory_path, "frozen inventory")?;
    let run_metadata = fs::symlink_metadata(run_root).with_context(|| {
        format!(
            "fresh materializer run root is missing: {}",
            run_root.display()
        )
    })?;
    if run_metadata.file_type().is_symlink() || !run_metadata.is_dir() {
        bail!(
            "fresh materializer run root must be a real directory: {}",
            run_root.display()
        );
    }
    let inventory_sha256 = sha256_file(inventory_path)?;
    let campaign_inputs_path = run_root.join("receipts/campaign-inputs.json");
    let materialization_receipt_path = run_root.join("receipts/materialization-receipt.json");
    let receipt: CampaignInputsReceipt = read_json_bounded(&campaign_inputs_path)?;
    let receipt_sha256 = sha256_file(&campaign_inputs_path)?;
    validate_campaign_receipt(args, run_root, &receipt)?;
    let materialization: MaterializationReceipt = read_json_bounded(&materialization_receipt_path)?;
    let materialization_receipt_sha256 = sha256_file(&materialization_receipt_path)?;
    validate_materialization_receipt(
        args,
        &receipt,
        &materialization,
        &inventory_sha256,
        &receipt_sha256,
    )?;
    let feature_sha256 = verify_item(run_root, &receipt.feature, "feature", MAX_INPUT_BYTES)?;
    let materialization_sha256 = verify_item(
        run_root,
        &receipt.materialization,
        "materialization",
        MAX_INPUT_BYTES,
    )?;
    let replay_artifact_sha256 = verify_item(
        run_root,
        &receipt.replay_artifact,
        "replay artifact",
        MAX_INPUT_BYTES,
    )?;
    let replay_manifest_sha256 = verify_item(
        run_root,
        &receipt.replay_manifest,
        "replay manifest",
        MAX_INPUT_BYTES,
    )?;
    verify_sequence_output(args, run_root, &receipt.materialization)?;
    verify_market_encoder_output(args, run_root, &receipt.materialization)?;
    Ok(VerifiedOutputs {
        receipt_sha256,
        materialization_receipt_sha256,
        inventory_sha256,
        feature_sha256,
        materialization_sha256,
        replay_artifact_sha256,
        replay_manifest_sha256,
    })
}

pub fn file_contains_pattern(path: &Path, needle: &[u8]) -> anyhow::Result<bool> {
    if needle.is_empty() {
        bail!("empty sequence marker");
    }
    let mut file = File::open(path)?;
    let mut carry = Vec::with_capacity(needle.len() + 65_536);
    let mut buffer = vec![0_u8; 65_536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(false);
        }
        carry.extend_from_slice(&buffer[..read]);
        if carry.windows(needle.len()).any(|window| window == needle) {
            return Ok(true);
        }
        if carry.len() >= needle.len() {
            carry.drain(..carry.len() + 1 - needle.len());
        }
    }
}

pub fn verify_sequence_output(
    args: &PreparationArgs,
    root: &Path,
    materialization: &CampaignInputItem,
) -> anyhow::Result<()> {
    use hft_research_manifest::sequence::{SequenceDatasetV1, SequenceInputSpecV1, SequenceViewV1};
    let path = root.join(&materialization.relative_path);
    ensure_regular_file(&path, "materialization report")?;
    // Legacy reports are not sequence JSON. Scan the requested marker without
    // requiring a parse, then leave the historical receipt contract unchanged.
    let declares_sequence = file_contains_pattern(&path, br#""sequence_manifest_sha256""#)?;
    if declares_sequence != args.sequence_output {
        bail!("sequence output differs from frozen preparation mode");
    }
    if !args.sequence_output {
        return Ok(());
    }
    let report: serde_json::Value = read_json_bounded(&path)?;
    let hash = report["sequence_manifest_sha256"].as_str();
    let Some(hash) = hash else {
        bail!("sequence output differs from frozen preparation mode");
    };
    if !hft_research_manifest::sequence::valid_sha256(hash) {
        bail!("invalid sequence manifest identity");
    }
    let parent = path.parent().context("sequence output has no parent")?;
    let manifest = parent.join(format!("{hash}.sequence.json"));
    if sha256_file(&manifest)? != hash {
        bail!("published sequence manifest differs");
    }
    let dataset: SequenceDatasetV1 = read_json_bounded(&manifest)?;
    dataset.validate().map_err(anyhow::Error::msg)?;
    let materialized = crate::materialization::decode_materialization(&read_file_bounded(
        &path,
        MAX_PREPARATION_BYTES,
        "sequence PIT report",
    )?)?;
    if dataset.input != SequenceInputSpecV1::sol_lob()
        || dataset.source_manifest_sha256 != materialized.snapshot.sha256()
        || dataset.shards.iter().map(|shard| shard.rows).sum::<u64>() != materialized.rows as u64
    {
        bail!("published sequence input channels differ");
    }
    let first = dataset
        .shards
        .first()
        .context("empty sequence")?
        .first_observed_at_ms;
    let last = dataset
        .shards
        .last()
        .context("empty sequence")?
        .last_observed_at_ms;
    let view = SequenceViewV1 {
        history_start_ms: first,
        decision_start_ms: first,
        end_ms: last
            .checked_add(31_000)
            .context("sequence clock overflow")?,
        decision_stride_ms: 1000,
    };
    let mut reader = crate::sequence::SequenceReader::open(parent, dataset, hash, view)
        .map_err(anyhow::Error::msg)?;
    reader.finish_pass().map_err(anyhow::Error::msg)
}

pub fn verify_market_encoder_output(
    args: &PreparationArgs,
    root: &Path,
    materialization: &CampaignInputItem,
) -> anyhow::Result<()> {
    use crate::market_encoder::{MarketFeatureReader, MarketTaskReader};
    use hft_research_manifest::{
        market_encoder::{MarketDataReadRequestV1, MarketFeatureDatasetV1, MarketTargetDatasetV1},
        sequence::{valid_sha256, SequenceInputSpecV1, SequenceViewV1},
    };
    let path = root.join(&materialization.relative_path);
    ensure_regular_file(&path, "market materialization report")?;
    if file_contains_pattern(&path, br#""market_encoder""#)? != args.market_encoder_output {
        bail!("market encoder output differs from frozen preparation mode");
    }
    if !args.market_encoder_output {
        return Ok(());
    }
    let report_bytes = read_file_bounded(&path, MAX_MARKET_METADATA_BYTES, "market PIT report")?;
    let report: serde_json::Value = serde_json::from_slice(&report_bytes)?;
    let exported = &report["market_encoder"];
    if exported["schema_version"] != "monday.market_encoder_export.v1"
        || exported["feature_start_received_at_ns"]
            != serde_json::to_value(args.market_feature_start_received_at_ns)?
        || exported["feature_end_received_at_ns"]
            != serde_json::to_value(args.market_feature_end_received_at_ns)?
    {
        bail!("market encoder export changed its partition contract");
    }
    let parent = path.parent().context("market report parent")?;
    let artifact = |name: &str, suffix: &str| -> anyhow::Result<(PathBuf, String)> {
        let hash = exported[name]["sha256"]
            .as_str()
            .context("missing market manifest digest")?;
        if !valid_sha256(hash) {
            bail!("invalid market manifest digest");
        }
        let filename = format!("{hash}.{suffix}.json");
        if exported[name]["file"] != filename {
            bail!("market manifest path changed");
        }
        let path = parent.join(filename);
        ensure_regular_file(&path, "market manifest")?;
        if sha256_file(&path)? != hash {
            bail!("market manifest hash differs");
        }
        Ok((path, hash.into()))
    };
    let (feature_path, feature_hash) = artifact("features", "market-features")?;
    let (target_path, target_hash) = artifact("targets", "market-targets")?;
    let (source_path, source_hash) = artifact("feature_sources", "market-feature-sources")?;
    let features: MarketFeatureDatasetV1 = read_json_bounded(&feature_path)?;
    let targets: MarketTargetDatasetV1 = read_json_bounded(&target_path)?;
    features.validate().map_err(anyhow::Error::msg)?;
    targets.validate().map_err(anyhow::Error::msg)?;
    crate::materialization::decode_materialization(&report_bytes)?;
    crate::market_sources::verify_feature_sources(
        &report,
        &read_file_bounded(
            &source_path,
            MAX_MARKET_METADATA_BYTES,
            "market feature sources",
        )?,
        &source_hash,
        &features,
    )?;
    if features.input != SequenceInputSpecV1::sol_lob()
        || features.digest().map_err(anyhow::Error::msg)? != feature_hash
        || features.source_manifest_sha256 != source_hash
        || targets.feature_dataset_sha256 != feature_hash
    {
        bail!("market features or targets differ from verified PIT provenance");
    }
    let (start, end) = args
        .start_received_at_ns
        .zip(args.end_received_at_ns)
        .context("market export has no admitted window")?;
    let feature_start = args.market_feature_start_received_at_ns.unwrap_or(start);
    let feature_end = args.market_feature_end_received_at_ns.unwrap_or(end);
    for shard in &features.shards {
        if shard.first_observed_at_ms as u128 * 1_000_000 < feature_start as u128
            || shard.last_observed_at_ms as u128 * 1_000_000 >= feature_end as u128
        {
            bail!("market feature shard escaped its partition");
        }
    }
    let first = features.shards[0].first_observed_at_ms;
    let end_ms = i64::try_from(end / 1_000_000).context("market view clock overflow")?;
    let request = MarketDataReadRequestV1 {
        feature_dataset_sha256: feature_hash,
        qualified_anchors_sha256: None,
        input: features.input.clone(),
        view: SequenceViewV1 {
            history_start_ms: first,
            decision_start_ms: first
                .checked_add(59000)
                .context("market context clock overflow")?,
            end_ms,
            decision_stride_ms: 1000,
        },
        anchor_end_ms: i64::try_from(feature_end / 1_000_000)
            .context("market partition overflow")?
            .min(end_ms - 30000),
    };
    let reader =
        MarketFeatureReader::open(parent, features, &request).map_err(anyhow::Error::msg)?;
    let mut reader = MarketTaskReader::open(reader, parent, targets, &target_hash)
        .map_err(anyhow::Error::msg)?;
    reader.finish_pass().map_err(anyhow::Error::msg)
}

/// Validate the owner/hash evidence that makes a partial materializer output
/// safe to resume. Missing known files are allowed because the entrypoint may
/// have been interrupted between two create-once publishes; existing files,
/// receipts, and inventory are never overwritten or silently accepted when
/// their identity differs.
pub fn verify_partial_materialized_outputs(
    args: &PreparationArgs,
    run_root: &Path,
    owner_receipt_path: &Path,
) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(run_root).with_context(|| {
        format!(
            "fresh materializer run root is missing: {}",
            run_root.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "fresh materializer run root must be a real directory: {}",
            run_root.display()
        );
    }
    let owner: CampaignInputsReceipt = read_json_bounded(owner_receipt_path)?;
    validate_campaign_receipt(args, run_root, &owner)?;
    let owner_sha256 = sha256_file(owner_receipt_path)?;
    for item in [
        &owner.feature,
        &owner.materialization,
        &owner.replay_artifact,
        &owner.replay_manifest,
    ] {
        let path = run_root.join(&item.relative_path);
        if path.try_exists()? {
            verify_item(run_root, item, "partial Campaign output", MAX_INPUT_BYTES)?;
        }
    }
    let inventory = run_root.join("receipts/frozen-inventory.env");
    if inventory.try_exists()? {
        ensure_regular_file(&inventory, "partial frozen inventory")?;
        let expected = sha256_file(&args.inventory_out)?;
        if sha256_file(&inventory)? != expected {
            bail!("partial frozen inventory SHA256 differs from the requested inventory");
        }
    }
    let materialization_path = run_root.join("receipts/materialization-receipt.json");
    if materialization_path.try_exists()? {
        let materialization: MaterializationReceipt = read_json_bounded(&materialization_path)?;
        if materialization.schema_version != MATERIALIZATION_RECEIPT_SCHEMA
            || materialization.run_id != owner.run_id
            || materialization.source_revision != args.source_revision
            || materialization.image_ref != args.image_ref
            || materialization.campaign_inputs_sha256 != owner_sha256
        {
            bail!("partial materialization receipt identity differs from its owner receipt");
        }
    }
    Ok(())
}

pub fn validate_campaign_receipt(
    args: &PreparationArgs,
    run_root: &Path,
    receipt: &CampaignInputsReceipt,
) -> anyhow::Result<()> {
    if receipt.schema_version != CAMPAIGN_INPUTS_SCHEMA {
        bail!("campaign inputs receipt schema drifted");
    }
    for (label, value) in [
        ("run_id", receipt.run_id.as_str()),
        ("mission_id", receipt.mission_id.as_str()),
        ("source_revision", receipt.source_revision.as_str()),
    ] {
        if value.is_empty() || value.chars().any(char::is_control) {
            bail!("campaign inputs receipt {label} is invalid");
        }
    }
    if receipt.source_revision != args.source_revision
        || receipt.image_ref != args.image_ref
        || receipt.mission_id != args.mission_id
        || receipt.market != args.market
        || receipt.symbol != args.symbol
        || receipt.output_prefix != args.output_prefix
        || receipt.readback_scope != "same-mounted-ossfs-prefix"
    {
        bail!("campaign inputs receipt does not match the fresh preparation request");
    }
    let base = hft_research_artifacts::identity::canonical_tokyo_oss_internal_object(
        "campaign inputs output root",
        &receipt.output_object_base_url,
    )?;
    let output_root = format!("{base}/{}", receipt.output_prefix);
    for (label, item) in [
        ("feature", &receipt.feature),
        ("materialization", &receipt.materialization),
        ("replay artifact", &receipt.replay_artifact),
        ("replay manifest", &receipt.replay_manifest),
    ] {
        validate_item_shape(label, item, &output_root)?;
        let local = run_root.join(&item.relative_path);
        if !local.starts_with(run_root) {
            bail!("{label} path escapes the frozen materializer run root");
        }
    }
    Ok(())
}

pub fn validate_materialization_receipt(
    args: &PreparationArgs,
    receipt: &CampaignInputsReceipt,
    materialization: &MaterializationReceipt,
    inventory_sha256: &str,
    campaign_inputs_sha256: &str,
) -> anyhow::Result<()> {
    if materialization.schema_version != MATERIALIZATION_RECEIPT_SCHEMA
        || materialization.run_id != receipt.run_id
        || materialization.source_revision != args.source_revision
        || materialization.image_ref != args.image_ref
        || materialization.inventory_sha256 != inventory_sha256
        || materialization.campaign_inputs_sha256 != campaign_inputs_sha256
        || materialization.feature_sha256 != receipt.feature.sha256
        || materialization.materialization_sha256 != receipt.materialization.sha256
        || materialization.replay_artifact_sha256 != receipt.replay_artifact.sha256
        || materialization.replay_manifest_sha256 != receipt.replay_manifest.sha256
    {
        bail!("materialization receipt does not match frozen Campaign inputs");
    }
    Ok(())
}

pub fn validate_item_shape(
    label: &str,
    item: &CampaignInputItem,
    output_root: &str,
) -> anyhow::Result<()> {
    if item.relative_path.as_os_str().is_empty()
        || item.relative_path.is_absolute()
        || item
            .relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("{label} relative path is unsafe");
    }
    let object = hft_research_artifacts::identity::canonical_tokyo_oss_internal_object(
        label,
        &item.object_url,
    )?;
    if item.sha256.len() != 64
        || !item
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !object.starts_with(&format!("{output_root}/"))
    {
        bail!("{label} receipt identity is invalid");
    }
    Ok(())
}

pub fn verify_item(
    run_root: &Path,
    item: &CampaignInputItem,
    label: &str,
    max_bytes: u64,
) -> anyhow::Result<String> {
    let path = run_root.join(&item.relative_path);
    ensure_regular_file(&path, label)?;
    let bytes = path.metadata()?.len();
    if bytes > max_bytes {
        bail!("{label} exceeds the bounded output budget");
    }
    let actual = sha256_file(&path)?;
    if actual != item.sha256 {
        bail!("{label} local SHA256 does not match campaign inputs receipt");
    }
    Ok(actual)
}

pub fn ensure_regular_file(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("{label} is missing: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "{label} must be a regular non-symlink file: {}",
            path.display()
        );
    }
    Ok(())
}

pub fn ensure_regular_executable(path: &Path, label: &str) -> anyhow::Result<()> {
    ensure_regular_file(path, label)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.metadata()?.permissions().mode() & 0o111 == 0 {
            bail!("{label} is not executable: {}", path.display());
        }
    }
    Ok(())
}

pub fn read_json_bounded<T: for<'de> serde::Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    let bytes = read_file_bounded(path, MAX_PREPARATION_BYTES, "JSON evidence")?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse JSON evidence {}", path.display()))
}

pub fn read_file_bounded(path: &Path, max_bytes: u64, label: &str) -> anyhow::Result<Vec<u8>> {
    ensure_regular_file(path, label)?;
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(
        max_bytes
            .checked_add(1)
            .context("bounded file size overflow")?,
    )
    .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        bail!("{label} exceeds {max_bytes} bytes: {}", path.display());
    }
    Ok(bytes)
}

pub fn preparation_report(
    args: &PreparationArgs,
    run_root: &Path,
    outputs: &VerifiedOutputs,
    input_fingerprint_sha256: &str,
    request_sha256: &str,
    selection: &FreshWindowSelection,
    selection_sha256: &str,
) -> anyhow::Result<PreparationReport> {
    if input_fingerprint_sha256.len() != 64
        || !input_fingerprint_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("fresh preparation is missing the collector input fingerprint");
    }
    Ok(PreparationReport {
        schema_version: PREPARATION_SCHEMA.to_string(),
        status: "ready".to_string(),
        source_revision: args.source_revision.clone(),
        image_ref: args.image_ref.clone(),
        market: args.market.clone(),
        symbol: args.symbol.clone(),
        mission_id: args.mission_id.clone(),
        selection: selection.mode.clone(),
        selected_start_received_at_ns: selection.selected_start_received_at_ns,
        selected_end_received_at_ns: selection.selected_end_received_at_ns,
        max_inputs: args.max_inputs,
        max_input_bytes: args.max_input_bytes,
        input_root: run_root.to_path_buf(),
        run_root: run_root.to_path_buf(),
        output_prefix: args.output_prefix.clone(),
        inventory_path: args.inventory_out.clone(),
        request_path: args.request_out.clone(),
        selection_path: expected_selection_path(
            &args.output_root.canonicalize()?,
            &args.output_prefix,
        ),
        selection_sha256: selection_sha256.to_string(),
        inventory_eligible: true,
        materialized_pit_admitted: true,
        campaign_inputs_path: args.campaign_inputs_out.clone(),
        request_sha256: request_sha256.to_string(),
        inventory_sha256: outputs.inventory_sha256.clone(),
        input_fingerprint_sha256: input_fingerprint_sha256.to_string(),
        campaign_inputs_sha256: outputs.receipt_sha256.clone(),
        materialization_receipt_sha256: outputs.materialization_receipt_sha256.clone(),
        feature_sha256: outputs.feature_sha256.clone(),
        materialization_sha256: outputs.materialization_sha256.clone(),
        replay_artifact_sha256: outputs.replay_artifact_sha256.clone(),
        replay_manifest_sha256: outputs.replay_manifest_sha256.clone(),
    })
}

/// Verify completed preparation without reading raw archives, launching a process, or writing evidence.
/// All original request, selection, inventory and output bindings remain mandatory.
pub fn verify_prepared_inputs(args: &PreparationArgs) -> anyhow::Result<PreparationOutput> {
    validate_args(args)?;
    let output_root = read_only_root(&args.output_root, "prepared output root")?;
    let run_root = output_root.join(&args.output_prefix);
    let request_path = expected_request_path(&output_root, &args.output_prefix);
    let selection_path = expected_selection_path(&output_root, &args.output_prefix);
    if normalize_for_compare(&args.request_out)? != request_path
        || normalize_for_compare(&args.campaign_inputs_out)?
            != run_root.join("receipts/campaign-inputs.json")
    {
        bail!("prepared input paths differ from the fixed request identity");
    }
    let report_path = args
        .report_out
        .as_deref()
        .context("completed preparation report is required")?;
    let request: PreparationRequest = read_json_bounded(&request_path)?;
    let mode = resolve_selection_mode(args, Some(&request))?;
    let expected = build_preparation_request(
        args,
        &normalize_for_compare(&args.raw_root)?,
        &normalize_for_compare(&args.reference_root)?,
        &output_root,
        mode,
        &selection_path,
    )?;
    if request != expected {
        bail!("prepared request differs from the controller's expected inputs");
    }
    let request_sha256 = sha256_file(&request_path)?;
    let (selection, selection_sha256) = read_selection(&selection_path, &request, args)?;
    let inventory = inventory_from_selection(args, &selection)?;
    verify_inventory_matches(&args.inventory_out, &inventory, metadata_limit(args))?;
    validate_existing_inventory(args, &args.inventory_out)?;
    let outputs = verify_materialized_outputs(args, &run_root, &args.inventory_out)?;
    // The producer publishes this exact frozen inventory beside the receipts.
    verify_inventory_matches(
        &run_root.join("receipts/frozen-inventory.env"),
        &inventory,
        metadata_limit(args),
    )?;
    let fingerprint = input_fingerprint(args, &request_sha256, &outputs.inventory_sha256)?;
    if fingerprint != selection.input_fingerprint_sha256 {
        bail!("prepared selection differs from the frozen source inventory");
    }
    let expected_report = preparation_report(
        args,
        &run_root,
        &outputs,
        &fingerprint,
        &request_sha256,
        &selection,
        &selection_sha256,
    )?;
    let report: PreparationReport = read_json_bounded(report_path)?;
    if report != expected_report {
        bail!("completed preparation report differs from independently verified bytes");
    }
    // Re-read identities after the bounded output pass. A changed receipt is not a completed readback.
    if sha256_file(&request_path)? != request_sha256
        || sha256_file(&selection_path)? != selection_sha256
        || sha256_file(&args.inventory_out)? != outputs.inventory_sha256
        || read_json_bounded::<PreparationReport>(report_path)? != report
    {
        bail!("preparation identity changed during independent verification");
    }
    Ok(PreparationOutput {
        report,
        reused_existing: true,
    })
}
