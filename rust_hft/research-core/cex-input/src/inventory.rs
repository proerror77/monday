//! Frozen inventory identities and deterministic rendering. No archive discovery or acquisition.
use anyhow::{bail, Context, Result};
use hft_research_manifest::binance::Market;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
pub const FRESH_WINDOW_SELECTION_SCHEMA: &str = "monday.cex_fresh_window_selection.v1";
fn digest(value: &str) -> bool {
    crate::valid_digest(value)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum FreshWindowMode {
    Explicit {
        start_received_at_ns: u64,
        end_received_at_ns: u64,
    },
    Latest {
        duration_ns: u64,
        cutoff_received_at_ns: u64,
        max_candidates: usize,
    },
}

#[derive(Debug, Clone)]
pub struct FreshWindowRequest {
    pub raw_root: PathBuf,
    pub reference_root: PathBuf,
    pub mode: FreshWindowMode,
    pub market: Market,
    pub symbol: String,
    pub source_revision: String,
    pub image_ref: String,
    pub mission_id: String,
    pub output_prefix: String,
    pub bucket_ms: u64,
    pub label_horizon_buckets: u64,
    pub top_depth: usize,
    pub max_scan_entries: usize,
    pub max_inputs: usize,
    pub max_input_bytes: u64,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FreshWindowSelection {
    pub schema_version: String,
    pub mode: FreshWindowMode,
    pub market: Market,
    pub selected_start_received_at_ns: u64,
    pub selected_end_received_at_ns: u64,
    pub raw: Vec<FrozenInput>,
    pub references: Vec<FrozenInput>,
    /// Bytes belonging to the final selected raw/reference inputs.
    pub verified_bytes: u64,
    /// Cumulative bytes read while evaluating this bounded selection, including
    /// candidates rejected before the final window. This never exceeds the
    /// request budget and is distinct from the selected input bytes above.
    pub verification_bytes: u64,
    pub input_fingerprint_sha256: String,
    pub inventory_eligible: bool,
    pub materialized_pit_admitted: bool,
}

#[derive(Debug, Clone)]
pub struct InventoryRequest {
    pub raw_root: PathBuf,
    pub reference_root: PathBuf,
    pub start_received_at_ns: u64,
    pub end_received_at_ns: u64,
    pub market: Market,
    pub symbol: String,
    pub source_revision: String,
    pub image_ref: String,
    pub mission_id: String,
    pub output_prefix: String,
    pub bucket_ms: u64,
    pub label_horizon_buckets: u64,
    pub top_depth: usize,
    pub max_scan_entries: usize,
    pub max_inputs: usize,
    pub max_input_bytes: u64,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct FrozenInput {
    pub relative_path: String,
    pub content_sha256: String,
    pub manifest_sha256: String,
    pub bytes: u64,
    pub start_received_at_ns: u64,
    pub end_received_at_ns: u64,
}

#[derive(Debug, Serialize)]
pub struct FrozenInventory {
    pub schema_version: &'static str,
    pub inventory_sha256: String,
    pub input_fingerprint_sha256: String,
    pub market: Market,
    pub raw: Vec<FrozenInput>,
    pub references: Vec<FrozenInput>,
    pub verified_bytes: u64,
    pub requires_pit_admission: bool,
    #[serde(skip)]
    pub inventory_env: String,
}
pub fn freeze_inventory_from_selection(
    request: &InventoryRequest,
    selection: &FreshWindowSelection,
) -> Result<FrozenInventory> {
    if selection.schema_version != FRESH_WINDOW_SELECTION_SCHEMA
        || !selection.inventory_eligible
        || selection.materialized_pit_admitted
        || selection.market != request.market
        || selection.raw.is_empty()
        || selection.references.is_empty()
        || request.start_received_at_ns != selection.selected_start_received_at_ns
        || request.end_received_at_ns != selection.selected_end_received_at_ns
    {
        bail!("fresh window selection is invalid for inventory materialization");
    }
    let total = selection
        .raw
        .len()
        .checked_add(selection.references.len())
        .context("fresh window selection input count overflowed")?;
    if total > request.max_inputs {
        bail!("fresh window selection input count budget exceeded");
    }
    let verified_bytes = selection
        .raw
        .iter()
        .chain(&selection.references)
        .try_fold(0_u64, |total, input| total.checked_add(input.bytes))
        .context("fresh window selection byte count overflowed")?;
    if verified_bytes != selection.verified_bytes
        || verified_bytes > request.max_input_bytes
        || selection.verification_bytes < selection.verified_bytes
        || selection.verification_bytes > request.max_input_bytes
    {
        bail!("fresh window selection input byte budget exceeded");
    }
    if selection_fingerprint_from_inputs(&selection.raw, &selection.references)?
        != selection.input_fingerprint_sha256
    {
        bail!("fresh window selection fingerprint differs from its inputs");
    }
    for input in selection.raw.iter().chain(&selection.references) {
        if input.relative_path.is_empty()
            || Path::new(&input.relative_path).is_absolute()
            || Path::new(&input.relative_path)
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || input.bytes == 0
            || input.start_received_at_ns > input.end_received_at_ns
            || !digest(&input.content_sha256)
            || !digest(&input.manifest_sha256)
        {
            bail!("fresh window selection contains an invalid input identity");
        }
    }
    if selection.raw.iter().any(|input| {
        input.start_received_at_ns >= request.end_received_at_ns
            || input.end_received_at_ns < request.start_received_at_ns
    }) {
        bail!("fresh window selection raw input does not overlap its selected window");
    }
    build_frozen_inventory(
        request,
        selection.raw.clone(),
        selection.references.clone(),
        selection.verified_bytes,
        selection.input_fingerprint_sha256.clone(),
    )
}

pub fn selection_fingerprint_from_inputs(
    raw: &[FrozenInput],
    references: &[FrozenInput],
) -> Result<String> {
    let identities: Vec<_> = raw
        .iter()
        .chain(references)
        .map(|input| {
            (
                &input.relative_path,
                &input.content_sha256,
                &input.manifest_sha256,
            )
        })
        .collect();
    if identities
        .iter()
        .map(|(_, content, _)| *content)
        .collect::<BTreeSet<_>>()
        .len()
        != identities.len()
    {
        bail!("duplicate source content in frozen inventory");
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&identities)?)
    ))
}

pub fn build_frozen_inventory(
    request: &InventoryRequest,
    raw: Vec<FrozenInput>,
    references: Vec<FrozenInput>,
    verified_bytes: u64,
    fingerprint: String,
) -> Result<FrozenInventory> {
    if raw.is_empty() || references.is_empty() {
        bail!("frozen inventory requires raw and reference inputs");
    }
    let mut env = format!("SOURCE_REVISION={}\nIMAGE_REF={}\nMISSION_ID={}\nMARKET={}\nSYMBOL={}\nBUCKET_MS={}\nLABEL_HORIZON_BUCKETS={}\nTOP_DEPTH={}\nOUTPUT_PREFIX={}\nWINDOW_START_RECEIVED_AT_NS={}\nWINDOW_END_RECEIVED_AT_NS={}\nRAW_SEGMENT_COUNT={}\n", request.source_revision, request.image_ref, request.mission_id, request.market.as_str(), request.symbol, request.bucket_ms, request.label_horizon_buckets, request.top_depth, request.output_prefix, request.start_received_at_ns, request.end_received_at_ns, raw.len());
    for (prefix, inputs) in [("RAW_SEGMENT", &raw), ("REFERENCE", &references)] {
        if prefix == "REFERENCE" {
            env.push_str(&format!("REFERENCE_COUNT={}\n", inputs.len()));
        }
        for (index, input) in inputs.iter().enumerate() {
            let ordinal = index + 1;
            env.push_str(&format!("{prefix}_{ordinal}={}\n{prefix}_{ordinal}_SHA256={}\n{prefix}_{ordinal}_MANIFEST_SHA256={}\n", input.relative_path, input.content_sha256, input.manifest_sha256));
        }
    }
    let run_id = format!("{:x}", Sha256::digest(env.as_bytes()));
    env.insert_str(0, &format!("RUN_ID=inventory-{}\n", &run_id[..16]));
    Ok(FrozenInventory {
        schema_version: "monday.research_frozen_inventory.v1",
        inventory_sha256: format!("{:x}", Sha256::digest(env.as_bytes())),
        input_fingerprint_sha256: fingerprint,
        market: request.market,
        raw,
        references,
        verified_bytes,
        requires_pit_admission: true,
        inventory_env: env,
    })
}
