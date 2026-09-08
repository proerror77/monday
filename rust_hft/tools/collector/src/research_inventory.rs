//! Deterministic preparation of frozen input inventories from existing collector
//! triplets. This is discovery and byte integrity, not PIT/replay admission.
use crate::{
    binance_usdm_reference_artifact::{
        verify_reference_artifact_read_only_current_batch, PublishedReferenceArtifact,
    },
    lob_archiver::files_with_suffix_bounded,
};
use anyhow::{bail, Context, Result};
use data::binance_market_tape::{
    market_tape_schema, AGGREGATE_TRADE_SUMMARY_CONTRACT, MARKET_TAPE_SCHEMA_V2,
};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

const USDM_LOB_DATASET: &str = "usdm_perpetual_top100_lob";
const USDM_LOB_DEPTH_ONLY_STREAM_TYPES: [&str; 1] = ["depth@100ms"];
const USDM_LOB_HISTORICAL_STREAM_TYPES: [&str; 2] = ["depth@100ms", "bookTicker"];

pub fn declared_symbols(manifest: &Map<String, Value>) -> Result<Vec<String>> {
    let symbols = manifest
        .get("symbols")
        .and_then(Value::as_array)
        .context("source manifest is missing symbols")?;
    let declared = symbols
        .iter()
        .map(|symbol| {
            symbol
                .as_str()
                .map(str::to_string)
                .context("source manifest declares a non-string symbol")
        })
        .collect::<Result<Vec<_>>>()?;
    if declared.is_empty()
        || declared.iter().collect::<BTreeSet<_>>().len() != declared.len()
        || declared
            .iter()
            .any(|symbol| symbol.is_empty() || symbol != &symbol.to_ascii_uppercase())
    {
        bail!("source manifest symbols must be non-empty, unique, and uppercase");
    }
    Ok(declared)
}

pub fn validate_source_manifest(manifest: &Map<String, Value>) -> Result<()> {
    let schema = required_string(manifest, "schema", "source manifest")?;
    if !market_tape_schema(schema) {
        bail!("source manifest schema is not a binance market tape: {schema}");
    }
    if schema == MARKET_TAPE_SCHEMA_V2 {
        let stream_types = manifest
            .get("stream_types")
            .and_then(Value::as_array)
            .context("v2 source manifest is missing stream_types")?;
        let valid = !stream_types.is_empty()
            && stream_types
                .iter()
                .all(|value| value.as_str().is_some_and(|kind| !kind.is_empty()))
            && stream_types
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
                .len()
                == stream_types.len();
        if !valid {
            bail!("v2 source manifest stream types are malformed");
        }
    }
    let requires_trade_contract = !manifest_is_usdm_lob_only(manifest);
    if requires_trade_contract
        && manifest
            .get("trade_summary_contract")
            .and_then(Value::as_str)
            != Some(AGGREGATE_TRADE_SUMMARY_CONTRACT)
    {
        bail!("source segment is missing the aggregate-trade summary contract");
    }
    if !requires_trade_contract
        && [
            "trade_representation",
            "price_surface_derivation",
            "trade_summary_contract",
            "trade_summaries",
        ]
        .iter()
        .any(|field| manifest.contains_key(*field))
    {
        bail!("USD-M LOB-only source carries trade-summary metadata");
    }
    let flags_ok = manifest
        .get("has_replay_safe_checkpoint")
        .and_then(Value::as_bool)
        == Some(true)
        && manifest.get("all_symbols_bridged").and_then(Value::as_bool) == Some(true)
        && manifest
            .get("all_stream_coverage_verified")
            .and_then(Value::as_bool)
            == Some(true)
        && manifest
            .get("venue_depth_complete")
            .and_then(Value::as_bool)
            == Some(false);
    if !flags_ok {
        bail!("source segment is not a fully replayable market-tape segment");
    }
    for field in ["snapshot_only_symbols", "raw_trade_incomplete_symbols"] {
        // Both fields default to an empty scope when the collector omits them.
        let empty = manifest
            .get(field)
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty);
        if !empty {
            bail!("source manifest field {field} must be an empty array");
        }
    }
    for field in ["dataset", "shard_id", "date", "hour"] {
        required_string(manifest, field, "source manifest")?;
    }
    if manifest.get("snapshot_limit").and_then(Value::as_u64) == Some(0) {
        bail!("source manifest snapshot limit must be nonzero");
    }
    Ok(())
}

pub fn manifest_is_usdm_lob_only(manifest: &Map<String, Value>) -> bool {
    manifest.get("market").and_then(Value::as_str) == Some("usdm")
        && manifest.get("dataset").and_then(Value::as_str) == Some(USDM_LOB_DATASET)
        && manifest.get("schema").and_then(Value::as_str) == Some(MARKET_TAPE_SCHEMA_V2)
        && manifest
            .get("stream_types")
            .and_then(Value::as_array)
            .is_some_and(|stream_types| {
                let declared = stream_types
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<BTreeSet<_>>();
                declared
                    == USDM_LOB_DEPTH_ONLY_STREAM_TYPES
                        .iter()
                        .copied()
                        .collect::<BTreeSet<_>>()
                    || declared
                        == USDM_LOB_HISTORICAL_STREAM_TYPES
                            .iter()
                            .copied()
                            .collect::<BTreeSet<_>>()
            })
}

fn required_string<'a>(raw: &'a Map<String, Value>, field: &str, what: &str) -> Result<&'a str> {
    raw.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{what} is missing {field}"))
}

const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct InventoryRequest {
    pub raw_root: PathBuf,
    pub reference_root: PathBuf,
    pub start_received_at_ns: u64,
    pub end_received_at_ns: u64,
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

#[derive(Debug, Clone, Serialize)]
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
    pub raw: Vec<FrozenInput>,
    pub references: Vec<FrozenInput>,
    pub verified_bytes: u64,
    pub requires_pit_admission: bool,
    #[serde(skip)]
    pub inventory_env: String,
}

fn safe_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./:@=+-".contains(&byte))
}

fn uint(manifest: &Map<String, Value>, field: &str) -> Result<u64> {
    manifest
        .get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("manifest requires {field}"))
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn open_contained(root: &Path, path: &Path) -> Result<File> {
    let resolved = path.canonicalize()?;
    if !resolved.starts_with(root) || resolved != path {
        bail!("inventory input is not a physical path beneath its declared root");
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let opened = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if !opened.is_file()
        || current.file_type().is_symlink()
        || current.dev() != opened.dev()
        || current.ino() != opened.ino()
    {
        bail!("inventory input file identity changed");
    }
    Ok(file)
}

fn bounded_bytes(root: &Path, path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_contained(root, path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("inventory metadata exceeds byte budget");
    }
    Ok(bytes)
}

fn read_manifest(root: &Path, path: &Path) -> Result<(Map<String, Value>, String)> {
    let bytes = bounded_bytes(root, path, MAX_MANIFEST_BYTES)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    Ok((
        value
            .as_object()
            .context("inventory manifest must be an object")?
            .clone(),
        hex::encode(Sha256::digest(&bytes)),
    ))
}

fn verify_input(
    root: &Path,
    manifest_path: &Path,
    manifest: &Map<String, Value>,
    manifest_sha256: String,
    start: u64,
    end: u64,
    remaining_bytes: &mut u64,
) -> Result<FrozenInput> {
    let manifest_name = manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("manifest name must be UTF-8")?;
    let data_name = manifest_name
        .strip_suffix(".manifest.json")
        .context("manifest suffix is missing")?;
    if required_string(manifest, "file", "inventory")? != data_name {
        bail!("inventory manifest file binding differs");
    }
    let expected_sha = required_string(manifest, "sha256", "inventory")?;
    if !digest(expected_sha) {
        bail!("inventory content digest is invalid");
    }
    let expected_bytes = uint(manifest, "bytes")?;
    if expected_bytes == 0 || expected_bytes > *remaining_bytes {
        bail!("inventory input byte budget exceeded");
    }
    let data_path = manifest_path.with_file_name(data_name);
    let success = manifest_path.with_file_name(format!("{data_name}._SUCCESS"));
    if bounded_bytes(root, &success, 65)? != format!("{expected_sha}\n").as_bytes() {
        bail!("inventory success marker differs from content digest");
    }
    let file = open_contained(root, &data_path)?;
    if file.metadata()?.len() != expected_bytes {
        bail!("inventory source byte size differs from manifest");
    }
    let mut reader = file.take(expected_bytes + 1);
    let mut hasher = Sha256::new();
    let actual_bytes = std::io::copy(&mut reader, &mut hasher)?;
    if actual_bytes != expected_bytes || hex::encode(hasher.finalize()) != expected_sha {
        bail!("inventory source content digest differs");
    }
    // Re-read the manifest and marker after the streaming read to reject a
    // changed trust anchor. Later materialization verifies these frozen digests again.
    if read_manifest(root, manifest_path)?.1 != manifest_sha256
        || bounded_bytes(root, &success, 65)? != format!("{expected_sha}\n").as_bytes()
    {
        bail!("inventory trust anchor changed during freeze");
    }
    *remaining_bytes -= actual_bytes;
    let relative = data_path
        .strip_prefix(root)?
        .to_str()
        .context("inventory path must be UTF-8")?
        .to_string();
    if !safe_value(&relative) {
        bail!("inventory path cannot be represented safely in frozen.env");
    }
    Ok(FrozenInput {
        relative_path: relative,
        content_sha256: expected_sha.to_string(),
        manifest_sha256,
        bytes: actual_bytes,
        start_received_at_ns: start,
        end_received_at_ns: end,
    })
}

pub fn freeze_inventory(request: &InventoryRequest) -> Result<FrozenInventory> {
    let now = u64::try_from(
        chrono::Utc::now()
            .timestamp_nanos_opt()
            .context("inventory wall clock is out of range")?,
    )?;
    if request.end_received_at_ns > now
        || Path::new(&request.output_prefix)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || !request
            .mission_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        || request.start_received_at_ns >= request.end_received_at_ns
        || request.max_inputs == 0
        || request.max_scan_entries == 0
        || request.max_input_bytes == 0
        || request.bucket_ms == 0
        || request.label_horizon_buckets == 0
        || request.top_depth == 0
        || request.symbol.is_empty()
        || !request
            .symbol
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        || request.source_revision.len() != 40
        || !request
            .source_revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || [
            &request.image_ref,
            &request.mission_id,
            &request.output_prefix,
        ]
        .into_iter()
        .any(|value| !safe_value(value))
        || !request
            .image_ref
            .rsplit_once("@sha256:")
            .is_some_and(|(name, sha)| !name.is_empty() && digest(sha))
    {
        bail!("invalid frozen inventory request or budget");
    }
    let raw_root = request.raw_root.canonicalize()?;
    let reference_root = request.reference_root.canonicalize()?;
    // The same conservative scan rejects partial symlink views and bounds tree
    // traversal independently of the number of selected inputs.
    let raw_manifests =
        files_with_suffix_bounded(&raw_root, ".manifest.json", request.max_scan_entries)?;
    let reference_manifests =
        files_with_suffix_bounded(&reference_root, ".manifest.json", request.max_scan_entries)?;
    let mut remaining_bytes = request.max_input_bytes;
    let mut raw = Vec::new();
    for path in raw_manifests {
        let (manifest, sha) = read_manifest(&raw_root, &path)?;
        if manifest.get("market").and_then(Value::as_str) != Some("usdm") {
            continue;
        }
        if !declared_symbols(&manifest)?.contains(&request.symbol) {
            continue;
        }
        let start = uint(&manifest, "start_received_at_ns")?;
        let end = uint(&manifest, "end_received_at_ns")?;
        if start < request.start_received_at_ns || end > request.end_received_at_ns {
            continue;
        }
        if start > end {
            bail!("inventory raw interval is reversed");
        }
        validate_source_manifest(&manifest)?;
        if manifest.get("venue").and_then(Value::as_str) != Some("binance") {
            bail!("inventory source venue differs");
        }
        if raw.len() >= request.max_inputs {
            bail!("inventory input count budget exceeded");
        }
        raw.push(verify_input(
            &raw_root,
            &path,
            &manifest,
            sha,
            start,
            end,
            &mut remaining_bytes,
        )?);
    }
    raw.sort_by(|left, right| {
        (left.start_received_at_ns, &left.relative_path)
            .cmp(&(right.start_received_at_ns, &right.relative_path))
    });
    if raw.is_empty() {
        bail!("no eligible sealed USD-M segments in the requested window");
    }
    let first = raw
        .iter()
        .map(|input| input.start_received_at_ns)
        .min()
        .unwrap();
    let last = raw
        .iter()
        .map(|input| input.end_received_at_ns)
        .max()
        .unwrap();
    let mut references = Vec::new();
    for path in reference_manifests {
        let (manifest, sha) = read_manifest(&reference_root, &path)?;
        if manifest.get("venue").and_then(Value::as_str) != Some("binance_usdm")
            || manifest.get("dataset").and_then(Value::as_str) != Some("reference")
        {
            continue;
        }
        let observed = uint(&manifest, "observed_at_ns")?;
        if observed < first.saturating_sub(hft_research_manifest::CEX_DERIVATIVES_MAX_GAP_NS)
            || observed > last
        {
            continue;
        }
        if raw.len() + references.len() >= request.max_inputs {
            bail!("inventory input count budget exceeded");
        }
        let input = verify_input(
            &reference_root,
            &path,
            &manifest,
            sha,
            observed,
            observed,
            &mut remaining_bytes,
        )?;
        let data_path = reference_root.join(&input.relative_path);
        let artifact = PublishedReferenceArtifact {
            data_path: data_path.clone(),
            manifest_path: path,
            success_path: data_path.with_file_name(format!(
                "{}._SUCCESS",
                data_path.file_name().unwrap().to_str().unwrap()
            )),
            data_sha256: input.content_sha256.clone(),
            manifest_sha256: input.manifest_sha256.clone(),
        };
        let batch = verify_reference_artifact_read_only_current_batch(
            &artifact,
            &input.content_sha256,
            &input.manifest_sha256,
        )?;
        if !batch
            .contracts()
            .iter()
            .any(|contract| contract.symbol == request.symbol)
        {
            bail!("reference batch does not cover the selected symbol");
        }
        references.push(input);
    }
    references.sort_by(|left, right| {
        (left.start_received_at_ns, &left.relative_path)
            .cmp(&(right.start_received_at_ns, &right.relative_path))
    });
    if references.is_empty() {
        bail!("no eligible reference seed for the selected USD-M input");
    }
    let identities: Vec<_> = raw
        .iter()
        .chain(&references)
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
    let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(&identities)?));
    let mut env = format!("SOURCE_REVISION={}\nIMAGE_REF={}\nMISSION_ID={}\nMARKET=usdm\nSYMBOL={}\nBUCKET_MS={}\nLABEL_HORIZON_BUCKETS={}\nTOP_DEPTH={}\nOUTPUT_PREFIX={}\nRAW_SEGMENT_COUNT={}\n", request.source_revision, request.image_ref, request.mission_id, request.symbol, request.bucket_ms, request.label_horizon_buckets, request.top_depth, request.output_prefix, raw.len());
    for (prefix, inputs) in [("RAW_SEGMENT", &raw), ("REFERENCE", &references)] {
        if prefix == "REFERENCE" {
            env.push_str(&format!("REFERENCE_COUNT={}\n", inputs.len()));
        }
        for (index, input) in inputs.iter().enumerate() {
            let ordinal = index + 1;
            env.push_str(&format!("{prefix}_{ordinal}={}\n{prefix}_{ordinal}_SHA256={}\n{prefix}_{ordinal}_MANIFEST_SHA256={}\n", input.relative_path, input.content_sha256, input.manifest_sha256));
        }
    }
    let run_id = hex::encode(Sha256::digest(env.as_bytes()));
    env.insert_str(0, &format!("RUN_ID=inventory-{}\n", &run_id[..16]));
    Ok(FrozenInventory {
        schema_version: "monday.research_frozen_inventory.v1",
        inventory_sha256: hex::encode(Sha256::digest(env.as_bytes())),
        input_fingerprint_sha256: fingerprint,
        raw,
        references,
        verified_bytes: request.max_input_bytes - remaining_bytes,
        requires_pit_admission: true,
        inventory_env: env,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binance_usdm_reference_artifact::{
        publish_reference_batch, ReferenceArtifactConfig,
    };
    use crate::binance_usdm_reference_collector::OFFICIAL_USDM_SOURCE_ORIGIN;
    use data::binance_usdm_reference::{
        ActivePerpetualContract, CompleteReferenceBatch, MarkIndexFundingObservation,
        OpenInterestObservation, EXCHANGE_INFO_ENDPOINT, OPEN_INTEREST_ENDPOINT,
        PREMIUM_INDEX_ENDPOINT, REFERENCE_SCHEMA, SERVER_TIME_ENDPOINT,
    };
    use rust_decimal::Decimal;
    use serde_json::json;

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const RECEIVED_NS: u64 = 1_700_000_000_500_000_000;

    fn fixture() -> (tempfile::TempDir, InventoryRequest) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let raw_root = root.join("raw");
        let reference_root = root.join("reference");
        fs::create_dir_all(&raw_root).unwrap();
        fs::create_dir_all(&reference_root).unwrap();
        let data = b"unit test raw bytes: semantic admission belongs to the slicer";
        let sha = hex::encode(Sha256::digest(data));
        fs::write(raw_root.join("part-1.jsonl.zst"), data).unwrap();
        fs::write(
            raw_root.join("part-1.jsonl.zst._SUCCESS"),
            format!("{sha}\n"),
        )
        .unwrap();
        let manifest = json!({"schema":MARKET_TAPE_SCHEMA_V2,"venue":"binance","market":"usdm","dataset":USDM_LOB_DATASET,
            "shard_id":"test","date":"2023-11-14","hour":"22","symbols":["BTCUSDT"],"stream_types":["depth@100ms"],
            "has_replay_safe_checkpoint":true,"all_symbols_bridged":true,"all_stream_coverage_verified":true,"venue_depth_complete":false,
            "snapshot_limit":100,"start_received_at_ns":RECEIVED_NS+200,"end_received_at_ns":RECEIVED_NS+1000,
            "file":"part-1.jsonl.zst","bytes":data.len(),"sha256":sha});
        fs::write(
            raw_root.join("part-1.jsonl.zst.manifest.json"),
            format!("{manifest}\n"),
        )
        .unwrap();
        let batch = CompleteReferenceBatch::new(
            vec![ActivePerpetualContract {
                schema: REFERENCE_SCHEMA.into(),
                symbol: "BTCUSDT".into(),
                pair: "BTCUSDT".into(),
                base_asset: "BTC".into(),
                quote_asset: "USDT".into(),
                margin_asset: "USDT".into(),
                tick_size: Decimal::new(1, 1),
                step_size: Decimal::new(1, 3),
                min_notional: Decimal::from(5),
                contract_type: "PERPETUAL".into(),
                status: "TRADING".into(),
                onboard_date_ms: 1,
                delivery_date_ms: 4_133_404_800_000,
                source_time_ms: SOURCE_MS,
                source_clock_received_at_ns: RECEIVED_NS - 100,
                received_at_ns: RECEIVED_NS - 50,
                source_endpoint: EXCHANGE_INFO_ENDPOINT.into(),
                source_clock_endpoint: SERVER_TIME_ENDPOINT.into(),
            }],
            vec![MarkIndexFundingObservation {
                schema: REFERENCE_SCHEMA.into(),
                symbol: "BTCUSDT".into(),
                mark_price: Decimal::from(101),
                index_price: Decimal::from(100),
                basis: Decimal::ONE,
                basis_rate: Decimal::new(1, 2),
                last_funding_rate: Decimal::new(1, 4),
                interest_rate: Decimal::new(1, 4),
                next_funding_time_ms: SOURCE_MS + 28_800_000,
                source_time_ms: SOURCE_MS,
                received_at_ns: RECEIVED_NS,
                source_endpoint: PREMIUM_INDEX_ENDPOINT.into(),
            }],
            vec![OpenInterestObservation {
                schema: REFERENCE_SCHEMA.into(),
                symbol: "BTCUSDT".into(),
                open_interest: Decimal::new(12345, 3),
                source_time_ms: SOURCE_MS,
                received_at_ns: RECEIVED_NS + 50,
                source_endpoint: OPEN_INTEREST_ENDPOINT.into(),
            }],
        )
        .unwrap();
        publish_reference_batch(
            &ReferenceArtifactConfig {
                output_root: reference_root.clone(),
                observed_at_ns: RECEIVED_NS + 100,
                max_staleness_ms: 1000,
            },
            OFFICIAL_USDM_SOURCE_ORIGIN,
            &batch,
        )
        .unwrap();
        (
            directory,
            InventoryRequest {
                raw_root,
                reference_root,
                start_received_at_ns: RECEIVED_NS,
                end_received_at_ns: RECEIVED_NS + 2000,
                symbol: "BTCUSDT".into(),
                source_revision: "a".repeat(40),
                image_ref: format!("registry/runner@sha256:{}", "b".repeat(64)),
                mission_id: "data-test".into(),
                output_prefix: "runs/test".into(),
                bucket_ms: 1000,
                label_horizon_buckets: 5,
                top_depth: 5,
                max_scan_entries: 100,
                max_inputs: 10,
                max_input_bytes: 1_000_000,
            },
        )
    }

    #[test]
    fn freeze_selects_sealed_inputs_deterministically_and_keeps_pit_admission_separate() {
        let (_directory, request) = fixture();
        let first = freeze_inventory(&request).unwrap();
        let second = freeze_inventory(&request).unwrap();
        assert_eq!(first.inventory_env, second.inventory_env);
        assert_eq!(first.raw.len(), 1);
        assert_eq!(first.references.len(), 1);
        assert!(first.requires_pit_admission);
        assert!(first
            .inventory_env
            .contains("RAW_SEGMENT_1=part-1.jsonl.zst\n"));
        assert!(first.inventory_env.contains("REFERENCE_COUNT=1\n"));
        assert_eq!(
            first.inventory_sha256,
            hex::encode(Sha256::digest(first.inventory_env.as_bytes()))
        );
        assert!(!first
            .inventory_env
            .contains(request.raw_root.to_str().unwrap()));
        let mut changed_policy = request.clone();
        changed_policy.label_horizon_buckets = 10;
        let changed = freeze_inventory(&changed_policy).unwrap();
        assert_eq!(
            changed.input_fingerprint_sha256,
            first.input_fingerprint_sha256
        );
        assert_ne!(changed.inventory_sha256, first.inventory_sha256);
    }

    #[test]
    fn freeze_rejects_missing_markers_tampered_data_and_unreplayable_input() {
        let (_directory, request) = fixture();
        let marker = request.raw_root.join("part-1.jsonl.zst._SUCCESS");
        let saved = fs::read(&marker).unwrap();
        fs::remove_file(&marker).unwrap();
        assert!(freeze_inventory(&request).is_err());
        fs::write(&marker, saved).unwrap();
        let data = request.raw_root.join("part-1.jsonl.zst");
        let saved_data = fs::read(&data).unwrap();
        let mut changed = saved_data.clone();
        changed[0] ^= 1;
        fs::write(&data, changed).unwrap();
        assert!(freeze_inventory(&request)
            .unwrap_err()
            .to_string()
            .contains("content digest"));
        fs::write(&data, saved_data).unwrap();
        let manifest = request.raw_root.join("part-1.jsonl.zst.manifest.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        value["all_stream_coverage_verified"] = json!(false);
        fs::write(manifest, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(freeze_inventory(&request)
            .unwrap_err()
            .to_string()
            .contains("fully replayable"));
    }

    #[test]
    fn freeze_bounds_discovery_bytes_and_paths_without_fabricating_empty_inputs() {
        let (_directory, request) = fixture();
        let mut limited = request.clone();
        limited.max_scan_entries = 1;
        assert!(freeze_inventory(&limited)
            .unwrap_err()
            .to_string()
            .contains("scan entry budget"));
        limited = request.clone();
        limited.max_input_bytes = 1;
        assert!(freeze_inventory(&limited)
            .unwrap_err()
            .to_string()
            .contains("byte budget"));
        limited = request.clone();
        limited.max_inputs = 1;
        assert!(freeze_inventory(&limited)
            .unwrap_err()
            .to_string()
            .contains("count budget"));
        limited = request.clone();
        limited.start_received_at_ns = RECEIVED_NS + 1500;
        assert!(freeze_inventory(&limited)
            .unwrap_err()
            .to_string()
            .contains("no eligible sealed"));
        limited = request.clone();
        limited.output_prefix = "../escape".into();
        assert!(freeze_inventory(&limited).is_err());
        std::os::unix::fs::symlink(
            request.raw_root.join("part-1.jsonl.zst"),
            request.raw_root.join("alias.jsonl.zst"),
        )
        .unwrap();
        assert!(freeze_inventory(&request)
            .unwrap_err()
            .to_string()
            .contains("symlink"));
    }
}
