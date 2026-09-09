//! One-shot preparation of a fresh bounded CEX Campaign input window.
//!
//! This module owns only the boundary between the immutable collector inventory
//! and the already-reviewed materializer entrypoint. It does not freeze a
//! Campaign, sign a request, reserve a trial, or dispatch a Job.

use crate::{
    cli::{print_json, PrepareFreshInputsArgs, BUILD_SOURCE_REVISION},
    data_mission,
    mission_runner::sha256_file,
};
use anyhow::{bail, Context};
use hft_collector::research_inventory::{
    freeze_inventory_from_selection, select_fresh_window, FreshWindowMode, FreshWindowRequest,
    FreshWindowSelection, FRESH_WINDOW_SELECTION_SCHEMA,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

const CAMPAIGN_INPUTS_SCHEMA: &str = "monday.cex_campaign_inputs.v1";
const MATERIALIZATION_RECEIPT_SCHEMA: &str = "monday.cex_materialization_receipt.v1";
const PREPARATION_SCHEMA: &str = "monday.cex_fresh_inputs_preparation.v1";
const PREPARATION_REQUEST_SCHEMA: &str = "monday.cex_fresh_inputs_request.v1";
const MAX_PREPARATION_BYTES: u64 = 1024 * 1024;
const MAX_TIMEOUT_SECONDS: u64 = 24 * 60 * 60;
const MAX_INPUTS: usize = 8192;
const MAX_SCAN_ENTRIES: usize = 100_000;
const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_MATERIALIZER_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PreparationRequest {
    schema_version: String,
    raw_root: PathBuf,
    reference_root: PathBuf,
    selection: FreshWindowMode,
    symbol: String,
    image_ref: String,
    mission_id: String,
    output_root: PathBuf,
    output_prefix: String,
    bucket_ms: u64,
    label_horizon_buckets: u64,
    top_depth: usize,
    max_scan_entries: usize,
    max_inputs: usize,
    max_input_bytes: u64,
    materializer: PathBuf,
    binary_dir: Option<PathBuf>,
    materializer_work_dir: PathBuf,
    materializer_timeout_seconds: u64,
    max_materializer_output_bytes: u64,
    inventory_path: PathBuf,
    request_path: PathBuf,
    selection_path: PathBuf,
    campaign_inputs_path: PathBuf,
    report_path: Option<PathBuf>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignInputsReceipt {
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
    feature: CampaignInputItem,
    materialization: CampaignInputItem,
    replay_artifact: CampaignInputItem,
    replay_manifest: CampaignInputItem,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignInputItem {
    relative_path: PathBuf,
    object_url: String,
    sha256: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterializationReceipt {
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PreparationReport {
    schema_version: String,
    status: String,
    source_revision: String,
    image_ref: String,
    symbol: String,
    mission_id: String,
    selection: FreshWindowMode,
    selected_start_received_at_ns: u64,
    selected_end_received_at_ns: u64,
    max_inputs: usize,
    max_input_bytes: u64,
    input_root: PathBuf,
    run_root: PathBuf,
    output_prefix: String,
    inventory_path: PathBuf,
    request_path: PathBuf,
    selection_path: PathBuf,
    selection_sha256: String,
    inventory_eligible: bool,
    materialized_pit_admitted: bool,
    campaign_inputs_path: PathBuf,
    request_sha256: String,
    inventory_sha256: String,
    input_fingerprint_sha256: String,
    campaign_inputs_sha256: String,
    materialization_receipt_sha256: String,
    feature_sha256: String,
    materialization_sha256: String,
    replay_artifact_sha256: String,
    replay_manifest_sha256: String,
}

#[derive(Debug, serde::Serialize)]
struct PreparationOutput {
    #[serde(flatten)]
    report: PreparationReport,
    reused_existing: bool,
}

#[derive(Debug)]
struct VerifiedOutputs {
    receipt_sha256: String,
    materialization_receipt_sha256: String,
    inventory_sha256: String,
    feature_sha256: String,
    materialization_sha256: String,
    replay_artifact_sha256: String,
    replay_manifest_sha256: String,
}

/// Freeze the selected archive window once, run the existing materializer, and
/// verify the resulting immutable local Campaign input set. A completed run is
/// read back and reused; it is never rematerialized on restart.
pub fn prepare(args: PrepareFreshInputsArgs) -> anyhow::Result<()> {
    validate_args(&args)?;
    let raw_root = read_only_root(&args.raw_root, "raw archive")?;
    let reference_root = read_only_root(&args.reference_root, "reference archive")?;
    data_mission::ensure_real_directory(&args.output_root, "fresh input output")?;
    data_mission::ensure_real_directory(&args.materializer_work_dir, "materializer work")?;
    ensure_output_parent(&args.inventory_out, "frozen inventory")?;
    ensure_output_parent(&args.request_out, "fresh preparation request")?;
    if let Some(report) = &args.report_out {
        ensure_output_parent(report, "fresh input preparation report")?;
    }
    ensure_regular_executable(&args.materializer, "materializer")?;

    let output_root = args.output_root.canonicalize()?;
    let run_root = output_root.join(&args.output_prefix);
    let expected_campaign_inputs = run_root.join("receipts/campaign-inputs.json");
    if normalize_for_compare(&args.campaign_inputs_out)?
        != normalize_for_compare(&expected_campaign_inputs)?
    {
        bail!(
            "campaign inputs output must be the materializer receipt under the selected output prefix: {}",
            expected_campaign_inputs.display()
        );
    }
    let expected_request = expected_request_path(&output_root, &args.output_prefix);
    if normalize_for_compare(&args.request_out)? != normalize_for_compare(&expected_request)? {
        bail!(
            "fresh preparation request output must be the fixed identity path: {}",
            expected_request.display()
        );
    }
    let expected_selection = expected_selection_path(&output_root, &args.output_prefix);
    let existing_request = if args.request_out.is_file() {
        Some(read_json_bounded::<PreparationRequest>(&args.request_out)?)
    } else {
        None
    };
    if args.inventory_out.try_exists()? && existing_request.is_none() {
        bail!("fresh preparation request identity is required before an existing inventory can be reused");
    }
    if args.inventory_out.try_exists()? && !expected_selection.is_file() {
        bail!("fresh window selection identity is required before an existing inventory can be reused");
    }
    let mode = resolve_selection_mode(&args, existing_request.as_ref())?;
    let request = build_preparation_request(
        &args,
        &raw_root,
        &reference_root,
        &output_root,
        mode,
        &expected_selection,
    )?;

    let run_exists = run_root.try_exists()?;
    if run_exists && !args.request_out.is_file() {
        bail!("fresh preparation request identity is missing for an existing output prefix");
    }
    if run_exists && !expected_selection.is_file() {
        bail!("fresh window selection identity is missing for an existing output prefix");
    }
    let request_sha256 = ensure_preparation_request(&args.request_out, &request, !run_exists)?;
    let mut partial_owner_verified = false;
    if args.campaign_inputs_out.is_file() {
        let complete_result = (|| -> anyhow::Result<PreparationOutput> {
            let (selection, selection_sha256) =
                read_selection(&expected_selection, &request, &args)?;
            let inventory = inventory_from_selection(&args, &selection)?;
            verify_inventory_matches(&args.inventory_out, &inventory)?;
            let verified = verify_materialized_outputs(&args, &run_root, &args.inventory_out)?;
            let fingerprint =
                input_fingerprint(&args, &request_sha256, &verified.inventory_sha256)?;
            if fingerprint != selection.input_fingerprint_sha256 {
                bail!("selected fresh window fingerprint differs from frozen inventory");
            }
            let report = preparation_report(
                &args,
                &run_root,
                &verified,
                &fingerprint,
                &request_sha256,
                &selection,
                &selection_sha256,
            )?;
            write_report_create_once(args.report_out.as_deref(), &report)?;
            Ok(PreparationOutput {
                report,
                reused_existing: true,
            })
        })();
        match complete_result {
            Ok(output) => return print_json(&output),
            Err(full_error) => {
                verify_partial_materialized_outputs(
                    &args,
                    &run_root,
                    &args.campaign_inputs_out,
                )
                .with_context(|| {
                    format!(
                        "complete fresh output failed verification and cannot be safely resumed: {full_error}"
                    )
                })?;
                partial_owner_verified = true;
            }
        }
    }
    if run_exists && !partial_owner_verified {
        let staged_owner = args
            .materializer_work_dir
            .join("staged-output/receipts/campaign-inputs.json");
        if args.campaign_inputs_out.is_file() {
            verify_partial_materialized_outputs(&args, &run_root, &args.campaign_inputs_out)?;
        } else if staged_owner.is_file() {
            verify_partial_materialized_outputs(
                &args,
                &args.materializer_work_dir.join("staged-output"),
                &staged_owner,
            )?;
            verify_partial_materialized_outputs(&args, &run_root, &staged_owner)?;
        } else {
            bail!(
                "existing fresh materializer output has no verifiable preparation owner: {}",
                run_root.display()
            );
        }
    }

    let (selection, selection_sha256) = if expected_selection.is_file() {
        read_selection(&expected_selection, &request, &args)?
    } else {
        let window_request =
            fresh_window_request(&args, &raw_root, &reference_root, &request.selection);
        let selection = select_fresh_window(&window_request)?;
        write_selection_create_once(&expected_selection, &selection)?;
        read_selection(&expected_selection, &request, &args)?
    };
    let inventory = inventory_from_selection(&args, &selection)?;
    if args.inventory_out.try_exists()? {
        validate_existing_inventory(&args, &args.inventory_out)?;
        verify_inventory_matches(&args.inventory_out, &inventory)?;
    } else {
        write_bytes_create_once(
            &args.inventory_out,
            inventory.inventory_env.as_bytes(),
            "frozen inventory",
        )?;
    }
    let inventory_sha256 = sha256_file(&args.inventory_out)?;
    if inventory_sha256 != inventory.inventory_sha256 {
        bail!("frozen inventory SHA256 readback differs from collector freezer");
    }
    validate_inventory_env(&args, &inventory.inventory_env)?;

    run_materializer(&args, &args.inventory_out)?;
    let verified = verify_materialized_outputs(&args, &run_root, &args.inventory_out)?;
    let derived_fingerprint =
        input_fingerprint(&args, &request_sha256, &verified.inventory_sha256)?;
    let fingerprint = if inventory.input_fingerprint_sha256.is_empty() {
        derived_fingerprint
    } else {
        if inventory.input_fingerprint_sha256 != derived_fingerprint {
            bail!("collector input fingerprint differs from frozen inventory contents");
        }
        inventory.input_fingerprint_sha256.clone()
    };
    if fingerprint != selection.input_fingerprint_sha256 {
        bail!("selected fresh window fingerprint differs from frozen inventory");
    }
    let report = preparation_report(
        &args,
        &run_root,
        &verified,
        &fingerprint,
        &request_sha256,
        &selection,
        &selection_sha256,
    )?;
    write_report_create_once(args.report_out.as_deref(), &report)?;
    print_json(&PreparationOutput {
        report,
        reused_existing: false,
    })
}

fn validate_args(args: &PrepareFreshInputsArgs) -> anyhow::Result<()> {
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
    crate::mission_dispatch::image_digest(&args.image_ref)?;
    Ok(())
}

fn validate_relative_prefix(value: &str) -> anyhow::Result<()> {
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

fn read_only_root(path: &Path, label: &str) -> anyhow::Result<PathBuf> {
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

fn ensure_output_parent(path: &Path, label: &str) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    data_mission::ensure_real_directory(parent, label)
}

fn normalize_for_compare(path: &Path) -> anyhow::Result<PathBuf> {
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

fn expected_request_path(output_root: &Path, output_prefix: &str) -> PathBuf {
    output_root
        .join(".fresh-inputs")
        .join(output_prefix)
        .join("request.json")
}

fn expected_selection_path(output_root: &Path, output_prefix: &str) -> PathBuf {
    output_root
        .join(".fresh-inputs")
        .join(output_prefix)
        .join("selection.json")
}

fn current_time_ns() -> anyhow::Result<u64> {
    u64::try_from(
        chrono::Utc::now()
            .timestamp_nanos_opt()
            .context("fresh window wall clock is out of range")?,
    )
    .context("fresh window wall clock is out of range")
}

fn resolve_selection_mode(
    args: &PrepareFreshInputsArgs,
    existing: Option<&PreparationRequest>,
) -> anyhow::Result<FreshWindowMode> {
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

fn build_preparation_request(
    args: &PrepareFreshInputsArgs,
    raw_root: &Path,
    reference_root: &Path,
    output_root: &Path,
    selection: FreshWindowMode,
    selection_path: &Path,
) -> anyhow::Result<PreparationRequest> {
    let binary_dir = args
        .binary_dir
        .as_deref()
        .map(|path| read_only_root(path, "materializer binary directory"))
        .transpose()?;
    Ok(PreparationRequest {
        schema_version: PREPARATION_REQUEST_SCHEMA.to_string(),
        raw_root: raw_root.to_path_buf(),
        reference_root: reference_root.to_path_buf(),
        selection,
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
        materializer: args.materializer.canonicalize()?,
        binary_dir,
        materializer_work_dir: args.materializer_work_dir.canonicalize()?,
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

fn fresh_window_request(
    args: &PrepareFreshInputsArgs,
    raw_root: &Path,
    reference_root: &Path,
    mode: &FreshWindowMode,
) -> FreshWindowRequest {
    FreshWindowRequest {
        raw_root: raw_root.to_path_buf(),
        reference_root: reference_root.to_path_buf(),
        mode: mode.clone(),
        symbol: args.symbol.clone(),
        source_revision: BUILD_SOURCE_REVISION.to_string(),
        image_ref: args.image_ref.clone(),
        mission_id: args.mission_id.clone(),
        output_prefix: args.output_prefix.clone(),
        bucket_ms: args.bucket_ms,
        label_horizon_buckets: args.label_horizon_buckets,
        top_depth: args.top_depth,
        max_scan_entries: args.max_scan_entries,
        max_inputs: args.max_inputs,
        max_input_bytes: args.max_input_bytes,
    }
}

fn inventory_from_selection(
    args: &PrepareFreshInputsArgs,
    selection: &FreshWindowSelection,
) -> anyhow::Result<hft_collector::research_inventory::FrozenInventory> {
    let request = hft_collector::research_inventory::InventoryRequest {
        raw_root: args.raw_root.clone(),
        reference_root: args.reference_root.clone(),
        start_received_at_ns: selection.selected_start_received_at_ns,
        end_received_at_ns: selection.selected_end_received_at_ns,
        symbol: args.symbol.clone(),
        source_revision: BUILD_SOURCE_REVISION.to_string(),
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

fn read_selection(
    path: &Path,
    request: &PreparationRequest,
    args: &PrepareFreshInputsArgs,
) -> anyhow::Result<(FreshWindowSelection, String)> {
    let bytes = read_file_bounded(path, MAX_PREPARATION_BYTES, "fresh window selection")?;
    let selection: FreshWindowSelection = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse fresh window selection {}", path.display()))?;
    validate_selection(&selection, request, args)?;
    Ok((selection, hex::encode(Sha256::digest(&bytes))))
}

fn validate_selection(
    selection: &FreshWindowSelection,
    request: &PreparationRequest,
    args: &PrepareFreshInputsArgs,
) -> anyhow::Result<()> {
    if selection.schema_version != FRESH_WINDOW_SELECTION_SCHEMA
        || selection.mode != request.selection
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
    if selection_fingerprint(&selection.raw, &selection.references)?
        != selection.input_fingerprint_sha256
    {
        bail!("fresh window selection fingerprint differs from its inputs");
    }
    Ok(())
}

fn selection_fingerprint(
    raw: &[hft_collector::research_inventory::FrozenInput],
    references: &[hft_collector::research_inventory::FrozenInput],
) -> anyhow::Result<String> {
    let identities = raw
        .iter()
        .chain(references)
        .map(|input| {
            (
                &input.relative_path,
                &input.content_sha256,
                &input.manifest_sha256,
            )
        })
        .collect::<Vec<_>>();
    if identities
        .iter()
        .map(|(_, content, _)| *content)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != identities.len()
    {
        bail!("fresh window selection contains duplicate source content");
    }
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(
        &identities,
    )?)))
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn write_selection_create_once(
    path: &Path,
    selection: &FreshWindowSelection,
) -> anyhow::Result<()> {
    write_bytes_create_once(
        path,
        &serde_json::to_vec_pretty(selection)?,
        "fresh window selection",
    )
}

fn verify_inventory_matches(
    path: &Path,
    expected: &hft_collector::research_inventory::FrozenInventory,
) -> anyhow::Result<()> {
    let bytes = read_file_bounded(path, MAX_PREPARATION_BYTES, "frozen inventory")?;
    if bytes != expected.inventory_env.as_bytes() {
        bail!("frozen inventory differs from the create-once window selection");
    }
    Ok(())
}

fn ensure_preparation_request(
    path: &Path,
    request: &PreparationRequest,
    create_if_missing: bool,
) -> anyhow::Result<String> {
    let bytes = serde_json::to_vec_pretty(request)?;
    if path.try_exists()? {
        let existing: PreparationRequest = read_json_bounded(path)?;
        if existing != *request {
            bail!("fresh preparation request identity differs from the existing request");
        }
    } else if create_if_missing {
        write_bytes_create_once(path, &bytes, "fresh preparation request")?;
    } else {
        bail!("fresh preparation request identity is missing for existing outputs");
    }
    let actual: PreparationRequest = read_json_bounded(path)?;
    if actual != *request {
        bail!("fresh preparation request changed during readback");
    }
    sha256_file(path)
}

fn validate_existing_inventory(args: &PrepareFreshInputsArgs, path: &Path) -> anyhow::Result<()> {
    let bytes = read_file_bounded(path, MAX_PREPARATION_BYTES, "frozen inventory")?;
    let env = String::from_utf8(bytes)?;
    validate_inventory_env(args, &env)
}

fn validate_inventory_env(args: &PrepareFreshInputsArgs, env: &str) -> anyhow::Result<()> {
    let values = parse_inventory_values(env)?;
    for (key, expected) in [
        ("SOURCE_REVISION", BUILD_SOURCE_REVISION.to_string()),
        ("IMAGE_REF", args.image_ref.clone()),
        ("MISSION_ID", args.mission_id.clone()),
        ("MARKET", "usdm".to_string()),
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

fn input_fingerprint(
    args: &PrepareFreshInputsArgs,
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
        MAX_PREPARATION_BYTES,
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

fn parse_inventory_values(env: &str) -> anyhow::Result<BTreeMap<&str, &str>> {
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

fn inventory_counts(
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

struct MaterializerProcess {
    child: Option<Child>,
}

impl MaterializerProcess {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("materializer process guard still owns its child")
    }

    fn finish(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate_materializer(&mut child);
        }
    }
}

impl Drop for MaterializerProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate_materializer(&mut child);
        }
    }
}

fn run_materializer(args: &PrepareFreshInputsArgs, inventory: &Path) -> anyhow::Result<()> {
    let output_root = args.output_root.canonicalize()?;
    let mut command = Command::new(&args.materializer);
    command
        .arg("--inventory")
        .arg(inventory)
        .arg("--raw-root")
        .arg(&args.raw_root)
        .arg("--reference-root")
        .arg(&args.reference_root)
        .arg("--output-root")
        .arg(output_root)
        .arg("--work-dir")
        .arg(&args.materializer_work_dir)
        .arg("--role")
        .arg("all");
    if let Some(binary_dir) = &args.binary_dir {
        command.arg("--binary-dir").arg(binary_dir);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start fresh materializer {}", args.materializer.display()))?;
    let mut process = MaterializerProcess::new(child);
    let stdout = process
        .child_mut()
        .stdout
        .take()
        .context("materializer stdout unavailable")?;
    let stderr = process
        .child_mut()
        .stderr
        .take()
        .context("materializer stderr unavailable")?;
    let max = args.max_materializer_output_bytes;
    let total_bytes = Arc::new(AtomicU64::new(0));
    let output_exceeded = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let stdout_total = Arc::clone(&total_bytes);
    let stdout_exceeded = Arc::clone(&output_exceeded);
    let stdout_sender = sender.clone();
    thread::spawn(move || {
        let result = read_stream_bounded(stdout, max, &stdout_total, &stdout_exceeded);
        let _ = stdout_sender.send((true, result));
    });
    let stderr_total = Arc::clone(&total_bytes);
    let stderr_exceeded = Arc::clone(&output_exceeded);
    thread::spawn(move || {
        let result = read_stream_bounded(stderr, max, &stderr_total, &stderr_exceeded);
        let _ = sender.send((false, result));
    });
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(args.materializer_timeout_seconds))
        .context("materializer timeout overflow")?;
    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;
    let mut failure = None;
    loop {
        while let Ok((is_stdout, result)) = receiver.try_recv() {
            if is_stdout {
                stdout = Some(result);
            } else {
                stderr = Some(result);
            }
        }
        if output_exceeded.load(Ordering::SeqCst) {
            terminate_materializer(process.child_mut());
            failure = Some("materializer stdout and stderr exceed the bounded output budget");
            break;
        }
        if status.is_none() {
            status = process
                .child_mut()
                .try_wait()
                .context("poll fresh materializer")?;
        }
        if status.is_some() && stdout.is_some() && stderr.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            terminate_materializer(process.child_mut());
            failure = Some("fresh materializer exceeded its bounded timeout");
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let cleanup_deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .context("materializer cleanup timeout overflow")?;
    while stdout.is_none() || stderr.is_none() {
        if Instant::now() >= cleanup_deadline {
            bail!("fresh materializer pipe cleanup exceeded its bounded timeout");
        }
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok((is_stdout, result)) => {
                if is_stdout {
                    stdout = Some(result);
                } else {
                    stderr = Some(result);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("fresh materializer pipe reader exited without a result");
            }
        }
    }
    if let Some(reason) = failure {
        bail!("{reason}");
    }
    let status = status
        .or_else(|| process.child_mut().try_wait().ok().flatten())
        .context("fresh materializer exit status is unavailable")?;
    let _stdout = stdout
        .expect("bounded materializer stdout result was collected")
        .context("read materializer stdout")?;
    let stderr = stderr
        .expect("bounded materializer stderr result was collected")
        .context("read materializer stderr")?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        bail!(
            "fresh materializer exited unsuccessfully with {:?}: {}",
            status.code(),
            stderr.trim()
        );
    }
    process.finish();
    Ok(())
}

fn read_stream_bounded(
    mut reader: impl Read,
    max_bytes: u64,
    total_bytes: &AtomicU64,
    output_exceeded: &AtomicBool,
) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return if total > max_bytes {
                bail!("materializer output exceeds the bounded output budget")
            } else {
                Ok(output)
            };
        }
        total = total.saturating_add(read as u64);
        let global_total = total_bytes.fetch_add(read as u64, Ordering::SeqCst) + read as u64;
        if global_total > max_bytes {
            output_exceeded.store(true, Ordering::SeqCst);
            return Ok(output);
        }
        if total <= max_bytes {
            output.extend_from_slice(&buffer[..read]);
        }
    }
}

fn terminate_materializer(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &process_group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn verify_materialized_outputs(
    args: &PrepareFreshInputsArgs,
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

/// Validate the owner/hash evidence that makes a partial materializer output
/// safe to resume. Missing known files are allowed because the entrypoint may
/// have been interrupted between two create-once publishes; existing files,
/// receipts, and inventory are never overwritten or silently accepted when
/// their identity differs.
fn verify_partial_materialized_outputs(
    args: &PrepareFreshInputsArgs,
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
            || materialization.source_revision != BUILD_SOURCE_REVISION
            || materialization.image_ref != args.image_ref
            || materialization.campaign_inputs_sha256 != owner_sha256
        {
            bail!("partial materialization receipt identity differs from its owner receipt");
        }
    }
    Ok(())
}

fn validate_campaign_receipt(
    args: &PrepareFreshInputsArgs,
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
    if receipt.source_revision != BUILD_SOURCE_REVISION
        || receipt.image_ref != args.image_ref
        || receipt.mission_id != args.mission_id
        || receipt.market != "usdm"
        || receipt.symbol != args.symbol
        || receipt.output_prefix != args.output_prefix
        || receipt.readback_scope != "same-mounted-ossfs-prefix"
    {
        bail!("campaign inputs receipt does not match the fresh preparation request");
    }
    let base = crate::prediction_dispatch::canonical_tokyo_oss_internal_object(
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

fn validate_materialization_receipt(
    args: &PrepareFreshInputsArgs,
    receipt: &CampaignInputsReceipt,
    materialization: &MaterializationReceipt,
    inventory_sha256: &str,
    campaign_inputs_sha256: &str,
) -> anyhow::Result<()> {
    if materialization.schema_version != MATERIALIZATION_RECEIPT_SCHEMA
        || materialization.run_id != receipt.run_id
        || materialization.source_revision != BUILD_SOURCE_REVISION
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

fn validate_item_shape(
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
    let object =
        crate::prediction_dispatch::canonical_tokyo_oss_internal_object(label, &item.object_url)?;
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

fn verify_item(
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

fn ensure_regular_file(path: &Path, label: &str) -> anyhow::Result<()> {
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

fn ensure_regular_executable(path: &Path, label: &str) -> anyhow::Result<()> {
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

fn read_json_bounded<T: for<'de> serde::Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    let bytes = read_file_bounded(path, MAX_PREPARATION_BYTES, "JSON evidence")?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse JSON evidence {}", path.display()))
}

fn read_file_bounded(path: &Path, max_bytes: u64, label: &str) -> anyhow::Result<Vec<u8>> {
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

fn preparation_report(
    args: &PrepareFreshInputsArgs,
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
        source_revision: BUILD_SOURCE_REVISION.to_string(),
        image_ref: args.image_ref.clone(),
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

fn write_report_create_once(path: Option<&Path>, report: &PreparationReport) -> anyhow::Result<()> {
    let Some(path) = path else { return Ok(()) };
    let bytes = serde_json::to_vec_pretty(report)?;
    if path.try_exists()? {
        ensure_regular_file(path, "fresh preparation report")?;
        if read_file_bounded(path, bytes.len() as u64, "fresh preparation report")? != bytes {
            bail!("fresh preparation report identity differs from the existing report");
        }
        return Ok(());
    }
    write_bytes_create_once(path, &bytes, "fresh preparation report")
}

fn write_bytes_create_once(path: &Path, bytes: &[u8], label: &str) -> anyhow::Result<()> {
    ensure_output_parent(path, label)?;
    data_mission::ensure_output_path_is_not_symlink(path, label)?;
    let mut temporary = data_mission::temporary_output_file(path, ".fresh-inputs-")?;
    std::io::Write::write_all(temporary.as_file_mut(), bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            ensure_regular_file(path, label)?;
            if read_file_bounded(path, bytes.len() as u64, label)? == bytes {
                Ok(())
            } else {
                bail!("{label} identity differs from the existing create-once file")
            }
        }
        Err(error) => {
            Err(error.error).with_context(|| format!("publish {label} {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> PrepareFreshInputsArgs {
        PrepareFreshInputsArgs {
            raw_root: PathBuf::from("/raw"),
            reference_root: PathBuf::from("/reference"),
            start_received_at_ns: Some(10),
            end_received_at_ns: Some(20),
            duration_ns: None,
            cutoff_received_at_ns: None,
            max_candidates: None,
            symbol: "BTCUSDT".into(),
            image_ref: format!("registry/research@sha256:{}", "a".repeat(64)),
            mission_id: "fresh-window".into(),
            output_prefix: "campaigns/fresh".into(),
            bucket_ms: 1_000,
            label_horizon_buckets: 5,
            top_depth: 5,
            max_scan_entries: 100,
            max_inputs: 10,
            max_input_bytes: 1_000_000,
            inventory_out: PathBuf::from("inventory.env"),
            request_out: PathBuf::from("fresh-request.json"),
            campaign_inputs_out: PathBuf::from("out/campaigns/fresh/receipts/campaign-inputs.json"),
            output_root: PathBuf::from("out"),
            materializer: PathBuf::from("materializer.sh"),
            materializer_work_dir: PathBuf::from("materializer-work"),
            binary_dir: None,
            materializer_timeout_seconds: 10,
            max_materializer_output_bytes: 1024,
            report_out: None,
        }
    }

    #[test]
    fn rejects_unbounded_preparation_limits() {
        let mut request = args();
        request.max_inputs = 0;
        assert!(validate_args(&request).is_err());
        request = args();
        request.max_input_bytes = MAX_INPUT_BYTES + 1;
        assert!(validate_args(&request).is_err());
        request = args();
        request.materializer_timeout_seconds = MAX_TIMEOUT_SECONDS + 1;
        assert!(validate_args(&request).is_err());

        let root = tempfile::tempdir().unwrap();
        let oversized = root.path().join("oversized");
        fs::write(
            &oversized,
            vec![b'x'; usize::try_from(MAX_PREPARATION_BYTES + 1).unwrap()],
        )
        .unwrap();
        assert!(read_file_bounded(&oversized, MAX_PREPARATION_BYTES, "test").is_err());

        let raw_count = usize::MAX.to_string();
        let counts = BTreeMap::from([
            ("RAW_SEGMENT_COUNT", raw_count.as_str()),
            ("REFERENCE_COUNT", "1"),
        ]);
        assert!(inventory_counts(&counts, MAX_INPUTS).is_err());
        let counts = BTreeMap::from([("RAW_SEGMENT_COUNT", "8192"), ("REFERENCE_COUNT", "1")]);
        assert!(inventory_counts(&counts, MAX_INPUTS).is_err());
    }

    #[test]
    fn rejects_archive_window_reversal_and_unsafe_prefix() {
        let mut request = args();
        request.end_received_at_ns = request.start_received_at_ns;
        assert!(validate_args(&request).is_err());
        request = args();
        request.output_prefix = "../fresh".into();
        assert!(validate_args(&request).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn prepare_runs_materializer_once_and_reuses_verified_receipt() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let raw_root = root.path().join("raw");
        let reference_root = root.path().join("reference");
        let output_root = root.path().join("output");
        let work_dir = root.path().join("materializer-work");
        fs::create_dir_all(&raw_root).unwrap();
        fs::create_dir_all(&reference_root).unwrap();
        fs::create_dir_all(&output_root).unwrap();

        let inventory_out = root.path().join("frozen.env");
        let image_ref = format!("registry/research@sha256:{}", "a".repeat(64));

        let materializer = root.path().join("materializer.sh");
        fs::write(
            &materializer,
            r#"#!/bin/sh
set -eu
inventory=
output_root=
work_dir=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --inventory) inventory=$2; shift 2 ;;
    --output-root) output_root=$2; shift 2 ;;
    --work-dir) work_dir=$2; shift 2 ;;
    --raw-root|--reference-root|--role) shift 2 ;;
    *) shift ;;
  esac
done
count=0
[ ! -e "$work_dir/count" ] || count=$(cat "$work_dir/count")
printf '%s\n' "$((count + 1))" >"$work_dir/count"
if [ -e "$work_dir/fail-next" ]; then
  rm -f "$work_dir/fail-next"
  exit 75
fi
run_root="$output_root/run"
mkdir -p "$run_root/artifacts" "$run_root/receipts"
printf 'feature\n' >"$run_root/artifacts/feature.jsonl"
printf 'materialization\n' >"$run_root/artifacts/materialization.json"
printf 'replay\n' >"$run_root/artifacts/replay.parquet"
printf 'manifest\n' >"$run_root/artifacts/replay-manifest.json"
sha() { shasum -a 256 "$1" | awk '{print $1}'; }
inventory_sha=$(sha "$inventory")
feature_sha=$(sha "$run_root/artifacts/feature.jsonl")
materialization_sha=$(sha "$run_root/artifacts/materialization.json")
replay_sha=$(sha "$run_root/artifacts/replay.parquet")
manifest_sha=$(sha "$run_root/artifacts/replay-manifest.json")
cat >"$run_root/receipts/campaign-inputs.json" <<EOF
{
  "schema_version":"monday.cex_campaign_inputs.v1",
  "run_id":"inventory-fresh",
  "source_revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "image_ref":"registry/research@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "mission_id":"fresh-window","market":"usdm","symbol":"BTCUSDT","output_prefix":"run",
  "output_object_base_url":"https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization",
  "readback_scope":"same-mounted-ossfs-prefix",
  "feature":{"relative_path":"artifacts/feature.jsonl","object_url":"https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization/run/artifacts/feature.jsonl","sha256":"$feature_sha"},
  "materialization":{"relative_path":"artifacts/materialization.json","object_url":"https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization/run/artifacts/materialization.json","sha256":"$materialization_sha"},
  "replay_artifact":{"relative_path":"artifacts/replay.parquet","object_url":"https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization/run/artifacts/replay.parquet","sha256":"$replay_sha"},
  "replay_manifest":{"relative_path":"artifacts/replay-manifest.json","object_url":"https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/cex-materialization/run/artifacts/replay-manifest.json","sha256":"$manifest_sha"}
}
EOF
campaign_sha=$(sha "$run_root/receipts/campaign-inputs.json")
cat >"$run_root/receipts/materialization-receipt.json" <<EOF
{
  "schema_version":"monday.cex_materialization_receipt.v1",
  "run_id":"inventory-fresh",
  "source_revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "image_ref":"registry/research@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "inventory_sha256":"$inventory_sha",
  "campaign_inputs_sha256":"$campaign_sha",
  "feature_sha256":"$feature_sha",
  "materialization_sha256":"$materialization_sha",
  "replay_artifact_sha256":"$replay_sha",
  "replay_manifest_sha256":"$manifest_sha"
}
EOF
"#,
        )
        .unwrap();
        fs::set_permissions(&materializer, fs::Permissions::from_mode(0o700)).unwrap();

        let args = PrepareFreshInputsArgs {
            raw_root,
            reference_root,
            start_received_at_ns: Some(1),
            end_received_at_ns: Some(2),
            duration_ns: None,
            cutoff_received_at_ns: None,
            max_candidates: None,
            symbol: "BTCUSDT".into(),
            image_ref,
            mission_id: "fresh-window".into(),
            output_prefix: "run".into(),
            bucket_ms: 1_000,
            label_horizon_buckets: 5,
            top_depth: 5,
            max_scan_entries: 100,
            max_inputs: 4,
            max_input_bytes: 1_000_000,
            inventory_out,
            request_out: root.path().join("output/.fresh-inputs/run/request.json"),
            campaign_inputs_out: output_root.join("run/receipts/campaign-inputs.json"),
            output_root,
            materializer,
            materializer_work_dir: work_dir.clone(),
            binary_dir: None,
            materializer_timeout_seconds: 10,
            max_materializer_output_bytes: 16 * 1024,
            report_out: None,
        };

        let raw_input = hft_collector::research_inventory::FrozenInput {
            relative_path: "raw.jsonl.zst".into(),
            content_sha256: "1".repeat(64),
            manifest_sha256: "2".repeat(64),
            bytes: 1,
            start_received_at_ns: 1,
            end_received_at_ns: 2,
        };
        let reference_input = hft_collector::research_inventory::FrozenInput {
            relative_path: "reference.json".into(),
            content_sha256: "3".repeat(64),
            manifest_sha256: "4".repeat(64),
            bytes: 1,
            start_received_at_ns: 1,
            end_received_at_ns: 1,
        };
        let selection = FreshWindowSelection {
            schema_version: FRESH_WINDOW_SELECTION_SCHEMA.to_string(),
            mode: FreshWindowMode::Explicit {
                start_received_at_ns: 1,
                end_received_at_ns: 2,
            },
            selected_start_received_at_ns: 1,
            selected_end_received_at_ns: 2,
            raw: vec![raw_input],
            references: vec![reference_input],
            verified_bytes: 2,
            // The latest candidate may have consumed one additional reference
            // before falling back; prepare must preserve that cumulative
            // budget evidence while accepting the selected input bytes.
            verification_bytes: 3,
            input_fingerprint_sha256: selection_fingerprint(
                &[hft_collector::research_inventory::FrozenInput {
                    relative_path: "raw.jsonl.zst".into(),
                    content_sha256: "1".repeat(64),
                    manifest_sha256: "2".repeat(64),
                    bytes: 1,
                    start_received_at_ns: 1,
                    end_received_at_ns: 2,
                }],
                &[hft_collector::research_inventory::FrozenInput {
                    relative_path: "reference.json".into(),
                    content_sha256: "3".repeat(64),
                    manifest_sha256: "4".repeat(64),
                    bytes: 1,
                    start_received_at_ns: 1,
                    end_received_at_ns: 1,
                }],
            )
            .unwrap(),
            inventory_eligible: true,
            materialized_pit_admitted: false,
        };
        let selection_path = expected_selection_path(
            &args.output_root.canonicalize().unwrap(),
            &args.output_prefix,
        );
        fs::create_dir_all(&args.materializer_work_dir).unwrap();
        let request = build_preparation_request(
            &args,
            &args.raw_root.canonicalize().unwrap(),
            &args.reference_root.canonicalize().unwrap(),
            &args.output_root.canonicalize().unwrap(),
            selection.mode.clone(),
            &selection_path,
        )
        .unwrap();
        write_bytes_create_once(
            &args.request_out,
            &serde_json::to_vec_pretty(&request).unwrap(),
            "fresh preparation request",
        )
        .unwrap();
        write_selection_create_once(&selection_path, &selection).unwrap();
        let expected_inventory = inventory_from_selection(&args, &selection).unwrap();
        fs::write(&args.inventory_out, expected_inventory.inventory_env).unwrap();
        prepare(args.clone()).unwrap();
        assert_eq!(
            fs::read_to_string(work_dir.join("count")).unwrap().trim(),
            "1"
        );
        fs::write(args.raw_root.join("new-after-freeze"), b"ignored").unwrap();
        prepare(args.clone()).unwrap();
        assert_eq!(
            fs::read_to_string(work_dir.join("count")).unwrap().trim(),
            "1"
        );
        fs::remove_dir_all(args.output_root.join("run")).unwrap();
        fs::write(work_dir.join("fail-next"), b"").unwrap();
        assert!(prepare(args.clone()).is_err());
        assert_eq!(
            fs::read_to_string(work_dir.join("count")).unwrap().trim(),
            "2"
        );
        prepare(args.clone()).unwrap();
        assert_eq!(
            fs::read_to_string(work_dir.join("count")).unwrap().trim(),
            "3"
        );
        let mut changed_request = args.clone();
        changed_request.request_out = root.path().join("another-request.json");
        assert!(prepare(changed_request).is_err());
        let mut changed_window = args.clone();
        changed_window.start_received_at_ns = Some(3);
        assert!(prepare(changed_window).is_err());
        let mut changed_limits = args.clone();
        changed_limits.max_inputs = 3;
        assert!(prepare(changed_limits).is_err());
        let changed_root = root.path().join("other-raw");
        fs::create_dir_all(&changed_root).unwrap();
        let mut changed_archive = args;
        changed_archive.raw_root = changed_root;
        assert!(prepare(changed_archive).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn materializer_timeout_and_output_bomb_kill_the_owned_process_group() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let output_root = root.path().join("output");
        let work_dir = root.path().join("work");
        fs::create_dir_all(&output_root).unwrap();
        fs::create_dir_all(&work_dir).unwrap();

        let mut request = args();
        request.output_root = output_root;
        request.materializer_work_dir = work_dir.clone();
        request.materializer_timeout_seconds = 1;
        request.max_materializer_output_bytes = 1024;

        let timeout_script = root.path().join("timeout.sh");
        fs::write(
            &timeout_script,
            r#"#!/bin/sh
set -eu
work_dir=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --work-dir) work_dir=$2; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s\n' "$$" >"$work_dir/timeout-pid"
(sleep 3; printf late >"$work_dir/late-marker") &
exit 0
"#,
        )
        .unwrap();
        fs::set_permissions(&timeout_script, fs::Permissions::from_mode(0o700)).unwrap();
        request.materializer = timeout_script;
        let timeout_error = run_materializer(&request, Path::new("/tmp/frozen.env"))
            .expect_err("a descendant-held pipe must hit the preparation timeout");
        assert!(timeout_error.to_string().contains("bounded timeout"));
        thread::sleep(Duration::from_millis(100));
        assert!(!work_dir.join("late-marker").exists());
        let timeout_pid = fs::read_to_string(work_dir.join("timeout-pid")).unwrap();
        assert!(!std::process::Command::new("kill")
            .args(["-0", timeout_pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());

        let bomb_script = root.path().join("bomb.sh");
        fs::write(
            &bomb_script,
            r#"#!/bin/sh
set -eu
work_dir=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --work-dir) work_dir=$2; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s\n' "$$" >"$work_dir/bomb-pid"
(sleep 3; printf late >"$work_dir/bomb-late-marker") &
dd if=/dev/zero bs=4096 count=8 2>/dev/null
"#,
        )
        .unwrap();
        fs::set_permissions(&bomb_script, fs::Permissions::from_mode(0o700)).unwrap();
        request.materializer_timeout_seconds = 10;
        request.materializer = bomb_script;
        let bomb_error = run_materializer(&request, Path::new("/tmp/frozen.env"))
            .expect_err("output overrun must terminate the preparation process group");
        assert!(bomb_error.to_string().contains("output budget"));
        thread::sleep(Duration::from_millis(100));
        assert!(!work_dir.join("bomb-late-marker").exists());
        let bomb_pid = fs::read_to_string(work_dir.join("bomb-pid")).unwrap();
        assert!(!std::process::Command::new("kill")
            .args(["-0", bomb_pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());

        let success_script = root.path().join("success.sh");
        fs::write(
            &success_script,
            r#"#!/bin/sh
set -eu
work_dir=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --work-dir) work_dir=$2; shift 2 ;;
    *) shift ;;
  esac
done
(sleep 3; printf late >"$work_dir/success-late-marker") >/dev/null 2>&1 &
printf '%s\n' "$!" >"$work_dir/success-pid"
exit 0
"#,
        )
        .unwrap();
        fs::set_permissions(&success_script, fs::Permissions::from_mode(0o700)).unwrap();
        request.materializer = success_script;
        run_materializer(&request, Path::new("/tmp/frozen.env"))
            .expect("successful materializer must still clean descendants");
        thread::sleep(Duration::from_millis(100));
        assert!(!work_dir.join("success-late-marker").exists());
        let success_pid = fs::read_to_string(work_dir.join("success-pid")).unwrap();
        assert!(!std::process::Command::new("kill")
            .args(["-0", success_pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());
    }
}
