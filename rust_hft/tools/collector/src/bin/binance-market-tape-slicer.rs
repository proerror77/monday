//! Slice an immutable Binance market-tape segment into symbol-subset segments.
//!
//! Production all-market segments can decompress well past the 2 GiB resource
//! bound enforced by `seal_binance_market_tape_triplet`, which keeps them out
//! of the canonical replay materialization path. This tool rewrites one
//! digest-verified source segment into disjoint symbol-subset segments
//! (session rows rewritten to the subset scope, manifests and digests
//! recomputed) so every slice seals and verifies under the unchanged
//! fail-closed market-tape gates. Each emitted slice is re-sealed and
//! re-verified with the strict gate before it is reported.

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use data::binance_market_tape::{
    market_tape_schema, AggregateTrade, AggregateTradeSummaryBuilder,
    LobContinuitySummaryBuilder, AGGREGATE_TRADE_SUMMARY_CONTRACT, MARKET_TAPE_SCHEMA_V2,
};
use data::binance_market_tape_artifact::{
    seal_binance_market_tape_triplet, verify_binance_market_tape_for_strict_gate,
    BinanceMarketTapeTriplet, BinanceMarketTapeTrustAnchor,
};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

const DEFAULT_MAX_SLICE_BYTES: u64 = 1_500_000_000;
// Keep every slice strictly below the 2 GiB seal bound in
// binance_market_tape_artifact.rs; that bound is a deliberate resource limit
// and must not be raised to accommodate large segments.
const SEAL_DECOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ROW_BYTES: usize = 8 * 1024 * 1024;
const MAX_SLICE_ROWS: u64 = 10_000_000;
const MAX_SOURCE_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const SESSION_ROW_SLACK_BYTES: u64 = 4 * 1024;
const COMPRESSION_LEVEL: i32 = 3;

#[derive(Debug, Parser)]
#[command(
    name = "binance-market-tape-slicer",
    about = "Slice an oversized Binance market-tape segment into verified symbol-subset segments"
)]
struct Args {
    /// Source segment data file (<name>.jsonl.zst); its .manifest.json and
    /// ._SUCCESS siblings must sit beside it.
    #[arg(long)]
    segment: PathBuf,
    /// Directory that receives the slice triplets.
    #[arg(long)]
    output_dir: PathBuf,
    /// Optional comma-separated symbol subset to extract; defaults to every
    /// declared symbol of the source segment.
    #[arg(long)]
    symbols: Option<String>,
    /// Maximum decompressed bytes per slice.
    #[arg(long, default_value_t = DEFAULT_MAX_SLICE_BYTES)]
    max_slice_bytes: u64,
}

#[derive(Debug, Serialize)]
struct SourceEvidence {
    file: String,
    sha256: String,
    manifest_sha256: String,
    events: u64,
    decompressed_bytes: u64,
    declared_symbols: usize,
    selected_symbols: usize,
}

#[derive(Debug, Serialize)]
struct SliceEvidence {
    file: String,
    sha256: String,
    manifest_sha256: String,
    symbols: Vec<String>,
    events: u64,
    decompressed_bytes: u64,
    compressed_bytes: u64,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
}

#[derive(Debug, Serialize)]
struct SliceReport {
    source: SourceEvidence,
    slices: Vec<SliceEvidence>,
}

enum RowAttribution {
    Session,
    Symbol(String),
}

struct SourceScan {
    content_sha256: String,
    manifest_sha256: String,
    decompressed_bytes: u64,
    events: u64,
    symbol_bytes: BTreeMap<String, u64>,
    observed_symbols: BTreeSet<String>,
    session_starts: Vec<Map<String, Value>>,
    coverage: Map<String, Value>,
    session_bytes: u64,
}

struct SliceBuild {
    symbols: Vec<String>,
    writer: BufWriter<File>,
    temporary: PathBuf,
    bytes: u64,
    events: u64,
    event_types: BTreeMap<String, u64>,
    start_received_at_ns: Option<u64>,
    end_received_at_ns: Option<u64>,
    checkpoint_symbols: BTreeSet<String>,
    lob_continuity: LobContinuitySummaryBuilder,
    trade_summaries: AggregateTradeSummaryBuilder,
}

fn main() -> Result<()> {
    let report = slice_segment(&Args::parse())?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &report)?;
    println!();
    Ok(())
}

fn slice_segment(args: &Args) -> Result<SliceReport> {
    if args.max_slice_bytes == 0 || args.max_slice_bytes >= SEAL_DECOMPRESSED_BYTES {
        bail!(
            "--max-slice-bytes must be within 1..{} so slices stay below the seal bound",
            SEAL_DECOMPRESSED_BYTES
        );
    }
    let segment = fs::canonicalize(&args.segment)
        .with_context(|| format!("cannot resolve segment path {}", args.segment.display()))?;
    let data_name = file_name(&segment)?;
    if !data_name.ends_with(".jsonl.zst") {
        bail!("segment data file must end in .jsonl.zst: {data_name}");
    }
    let stem = data_name
        .strip_suffix(".jsonl.zst")
        .expect("suffix checked above")
        .to_string();
    let manifest_path = sibling(&segment, ".manifest.json")?;
    let success_path = sibling(&segment, "._SUCCESS")?;
    let output_dir = canonical_output_dir(&args.output_dir)?;

    let (source_manifest, manifest_bytes) = read_source_manifest(&manifest_path, &data_name)?;
    validate_source_manifest(&source_manifest)?;
    let declared_order = declared_symbols(&source_manifest)?;
    let declared = declared_order.iter().cloned().collect::<BTreeSet<_>>();
    let selected = selected_symbols(args.symbols.as_deref(), &declared)?;

    let expected_content_sha256 = required_string(&source_manifest, "sha256", "source manifest")?
        .to_string();
    let success_bytes = fs::read(&success_path)
        .with_context(|| format!("cannot read success marker {}", success_path.display()))?;
    if success_bytes != format!("{expected_content_sha256}\n").as_bytes() {
        bail!("source success marker does not match the manifest content digest");
    }

    let scan = scan_segment(&segment, &manifest_bytes)?;
    if scan.content_sha256 != expected_content_sha256 {
        bail!("source segment bytes do not match the manifest content digest");
    }
    if !scan.observed_symbols.is_subset(&declared) {
        bail!("source rows reference symbols outside the declared manifest scope");
    }

    let plans = plan_slices(
        &scan.symbol_bytes,
        &declared_order,
        &selected,
        scan.session_bytes,
        args.max_slice_bytes,
    )?;
    let mut slice_of = BTreeMap::new();
    for (index, symbols) in plans.iter().enumerate() {
        for symbol in symbols {
            slice_of.insert(symbol.clone(), index);
        }
    }

    let mut builds = Vec::with_capacity(plans.len());
    for (index, symbols) in plans.iter().enumerate() {
        let (temporary, file) =
            temporary_file(&output_dir.join(format!("{stem}.slice-{:03}.jsonl", index + 1)))?;
        builds.push(SliceBuild {
            symbols: symbols.clone(),
            writer: BufWriter::new(file),
            temporary,
            bytes: 0,
            events: 0,
            event_types: BTreeMap::new(),
            start_received_at_ns: None,
            end_received_at_ns: None,
            checkpoint_symbols: BTreeSet::new(),
            lob_continuity: LobContinuitySummaryBuilder::new(symbols.iter().cloned())?,
            trade_summaries: AggregateTradeSummaryBuilder::default(),
        });
    }

    // Shared session rows lead every slice, rewritten to the slice scope, so a
    // slice stays a self-contained segment under the unchanged verifier.
    for build in &mut builds {
        let slice_symbols = build.symbols.iter().cloned().collect::<BTreeSet<_>>();
        let coverage =
            rewrite_stream_coverage(&scan.coverage, &source_manifest, &slice_symbols)?;
        let coverage_shards = coverage
            .get("shards")
            .and_then(Value::as_array)
            .expect("rewritten coverage keeps its shard array")
            .len() as u64;
        for row in &scan.session_starts {
            let rewritten =
                rewrite_session_start(row, &source_manifest, &slice_symbols, coverage_shards)?;
            emit_row(build, &rewritten, None)?;
        }
        emit_row(build, &coverage, None)?;
    }

    let routed_digest = route_rows(&segment, &slice_of, &mut builds)?;
    if routed_digest != scan.content_sha256 {
        bail!("source segment changed between slicing passes");
    }

    let mut slices = Vec::with_capacity(builds.len());
    for (index, build) in builds.into_iter().enumerate() {
        let name = format!("{stem}.slice-{:03}.jsonl.zst", index + 1);
        slices.push(publish_slice(
            build,
            &source_manifest,
            &output_dir,
            &name,
        )?);
    }

    Ok(SliceReport {
        source: SourceEvidence {
            file: data_name,
            sha256: scan.content_sha256,
            manifest_sha256: scan.manifest_sha256,
            events: scan.events,
            decompressed_bytes: scan.decompressed_bytes,
            declared_symbols: declared.len(),
            selected_symbols: selected.len(),
        },
        slices,
    })
}

fn read_source_manifest(path: &Path, data_name: &str) -> Result<(Map<String, Value>, Vec<u8>)> {
    let bytes = fs::read(path)
        .with_context(|| format!("cannot read source manifest {}", path.display()))?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_SOURCE_MANIFEST_BYTES {
        bail!(
            "source manifest is not a bounded file: {}",
            path.display()
        );
    }
    if bytes.last() != Some(&b'\n')
        || bytes[..bytes.len() - 1].contains(&b'\n')
        || bytes.contains(&b'\r')
    {
        bail!("source manifest must be one JSON line ending in one newline");
    }
    let value: Value = serde_json::from_slice(&bytes).context("parse source manifest")?;
    let manifest = value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("source manifest must be a JSON object"))?;
    if required_string(&manifest, "file", "source manifest")? != data_name {
        bail!("source manifest file field does not match the segment name");
    }
    Ok((manifest, bytes))
}

fn declared_symbols(manifest: &Map<String, Value>) -> Result<Vec<String>> {
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

fn validate_source_manifest(manifest: &Map<String, Value>) -> Result<()> {
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
    if manifest
        .get("trade_summary_contract")
        .and_then(Value::as_str)
        != Some(AGGREGATE_TRADE_SUMMARY_CONTRACT)
    {
        bail!("source segment is missing the aggregate-trade summary contract");
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
        && manifest.get("venue_depth_complete").and_then(Value::as_bool) == Some(false);
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

fn selected_symbols(raw: Option<&str>, declared: &BTreeSet<String>) -> Result<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(declared.iter().cloned().collect());
    };
    let selected = raw
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(str::to_ascii_uppercase)
        .collect::<BTreeSet<_>>();
    if selected.is_empty() {
        bail!("--symbols must name at least one symbol");
    }
    if !selected.is_subset(declared) {
        let missing = selected
            .difference(declared)
            .cloned()
            .collect::<Vec<_>>()
            .join(",");
        bail!("requested symbols are outside the source segment scope: {missing}");
    }
    Ok(selected.into_iter().collect())
}

fn required_string<'a>(raw: &'a Map<String, Value>, field: &str, what: &str) -> Result<&'a str> {
    raw.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{what} is missing {field}"))
}

fn scan_segment(segment: &Path, manifest_bytes: &[u8]) -> Result<SourceScan> {
    let mut coverage = Vec::new();
    let mut session_starts = Vec::new();
    let mut session_bytes = 0_u64;
    let mut scan = SourceScan {
        content_sha256: String::new(),
        manifest_sha256: hex::encode(Sha256::digest(manifest_bytes)),
        decompressed_bytes: 0,
        events: 0,
        symbol_bytes: BTreeMap::new(),
        observed_symbols: BTreeSet::new(),
        session_starts: Vec::new(),
        coverage: Map::new(),
        session_bytes: 0,
    };
    let digest = stream_segment(segment, |line, raw| {
        scan.decompressed_bytes = scan
            .decompressed_bytes
            .checked_add(line.len() as u64 + 1)
            .context("segment decompressed byte count overflow")?;
        scan.events = scan
            .events
            .checked_add(1)
            .context("segment event count overflow")?;
        match attribute_row(raw)? {
            RowAttribution::Session => {
                let bytes = line.len() as u64 + 1 + SESSION_ROW_SLACK_BYTES;
                session_bytes = session_bytes
                    .checked_add(bytes)
                    .context("session row byte count overflow")?;
                if required_string(raw, "type", "market-tape row")? == "stream_coverage" {
                    coverage.push(raw.clone());
                } else {
                    session_starts.push(raw.clone());
                }
            }
            RowAttribution::Symbol(symbol) => {
                *scan.symbol_bytes.entry(symbol.clone()).or_insert(0) +=
                    line.len() as u64 + 1;
                scan.observed_symbols.insert(symbol);
            }
        }
        Ok(())
    })?;
    if coverage.len() != 1 {
        bail!(
            "source segment carries {} stream coverage rows; slices need exactly one to satisfy the LOB continuity gate",
            coverage.len()
        );
    }
    scan.content_sha256 = digest;
    scan.session_starts = session_starts;
    scan.coverage = coverage.into_iter().next().expect("length checked above");
    scan.session_bytes = session_bytes;
    Ok(scan)
}

fn route_rows(
    segment: &Path,
    slice_of: &BTreeMap<String, usize>,
    builds: &mut [SliceBuild],
) -> Result<String> {
    stream_segment(segment, |line, raw| {
        if let RowAttribution::Symbol(symbol) = attribute_row(raw)? {
            if let Some(&index) = slice_of.get(&symbol) {
                emit_row(&mut builds[index], raw, Some(line))?;
            }
        }
        Ok(())
    })
}

fn stream_segment(
    segment: &Path,
    mut visit: impl FnMut(&[u8], &Map<String, Value>) -> Result<()>,
) -> Result<String> {
    let file = File::open(segment)
        .with_context(|| format!("cannot open segment {}", segment.display()))?;
    let decoder = zstd::stream::read::Decoder::new(BufReader::new(file))
        .context("cannot decode market-tape zstd stream")?;
    let mut reader = BufReader::new(decoder);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .context("read market-tape row")?;
        if read == 0 {
            break;
        }
        if line.last() != Some(&b'\n') || read == 1 || read - 1 > MAX_ROW_BYTES {
            bail!("market-tape row violates its resource bound");
        }
        line.pop();
        let raw = parse_row(&line)?;
        visit(&line, &raw)?;
    }
    // Hash the whole compressed object separately so the digest always covers
    // exactly the bytes the collector pinned, independent of decoder framing.
    sha256_file(segment)
}

fn parse_row(line: &[u8]) -> Result<Map<String, Value>> {
    let value: Value = serde_json::from_slice(line).context("parse market-tape row")?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("market-tape row must be an object"))
}

fn attribute_row(raw: &Map<String, Value>) -> Result<RowAttribution> {
    let event_type = required_string(raw, "type", "market-tape row")?;
    match event_type {
        "session_start" | "stream_coverage" => Ok(RowAttribution::Session),
        "snapshot" | "checkpoint" | "stale_raw_trade" | "stale_book_ticker" => {
            Ok(RowAttribution::Symbol(row_symbol(raw)?))
        }
        "diff" | "agg_trade" | "raw_trade" | "raw_trade_zero_price" | "book_ticker"
        | "force_order" => Ok(RowAttribution::Symbol(frame_symbol(raw)?)),
        _ => bail!("market-tape event {event_type} cannot be sliced"),
    }
}

fn row_symbol(raw: &Map<String, Value>) -> Result<String> {
    Ok(required_string(raw, "symbol", "market-tape row")?.to_ascii_uppercase())
}

fn frame_symbol(raw: &Map<String, Value>) -> Result<String> {
    let stream = raw
        .get("frame")
        .and_then(|frame| frame.get("stream"))
        .and_then(Value::as_str)
        .context("market-tape frame row is missing its stream identity")?;
    let (symbol, _) = stream
        .split_once('@')
        .filter(|(symbol, _)| !symbol.is_empty())
        .context("market-tape stream identity is malformed")?;
    Ok(symbol.to_ascii_uppercase())
}

fn manifest_stream_types(manifest: &Map<String, Value>) -> Result<Vec<String>> {
    let schema = required_string(manifest, "schema", "source manifest")?;
    if schema == MARKET_TAPE_SCHEMA_V2 {
        manifest
            .get("stream_types")
            .and_then(Value::as_array)
            .expect("v2 stream types validated above")
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .context("v2 stream type is not a string")
            })
            .collect()
    } else {
        Ok(vec!["depth@100ms".to_string(), "aggTrade".to_string()])
    }
}

fn rewrite_stream_coverage(
    row: &Map<String, Value>,
    manifest: &Map<String, Value>,
    symbols: &BTreeSet<String>,
) -> Result<Map<String, Value>> {
    let expected = manifest_stream_types(manifest)?
        .iter()
        .flat_map(|kind| {
            symbols
                .iter()
                .map(|symbol| format!("{}@{kind}", symbol.to_ascii_lowercase()))
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    let shards = row
        .get("shards")
        .and_then(Value::as_array)
        .context("stream coverage row has no shard array")?;
    let mut kept = Vec::new();
    let mut actual = BTreeSet::new();
    for shard in shards {
        let streams = shard
            .as_array()
            .context("stream coverage shard is not an array")?;
        let mut filtered = Vec::new();
        for stream in streams {
            let stream = stream
                .as_str()
                .context("stream coverage contains a non-string stream")?;
            let (symbol, _) = stream
                .split_once('@')
                .filter(|(symbol, _)| !symbol.is_empty())
                .context("stream coverage stream identity is malformed")?;
            if symbols.contains(&symbol.to_ascii_uppercase()) {
                actual.insert(stream.to_string());
                filtered.push(Value::String(stream.to_string()));
            }
        }
        if !filtered.is_empty() {
            kept.push(Value::Array(filtered));
        }
    }
    if actual != expected {
        bail!("source stream coverage does not cover every slice symbol");
    }
    let mut rewritten = row.clone();
    rewritten.insert("shards".to_string(), Value::Array(kept));
    Ok(rewritten)
}

fn rewrite_session_start(
    row: &Map<String, Value>,
    manifest: &Map<String, Value>,
    symbols: &BTreeSet<String>,
    coverage_shards: u64,
) -> Result<Map<String, Value>> {
    let count = u64::try_from(symbols.len()).context("slice symbol count exceeds u64")?;
    let stream_types = u64::try_from(manifest_stream_types(manifest)?.len())
        .context("stream type count exceeds u64")?;
    let mut rewritten = row.clone();
    rewritten.insert("symbols".to_string(), Value::from(count));
    rewritten.insert(
        "websocket_streams".to_string(),
        Value::from(
            count
                .checked_mul(stream_types)
                .context("session stream count overflow")?,
        ),
    );
    // Keep the declared shard count aligned with the rewritten coverage
    // evidence: the verifier cross-checks the two when both are present.
    rewritten.insert(
        "websocket_shards".to_string(),
        Value::from(coverage_shards),
    );
    Ok(rewritten)
}

fn emit_row(build: &mut SliceBuild, raw: &Map<String, Value>, line: Option<&[u8]>) -> Result<()> {
    let event_type = required_string(raw, "type", "market-tape row")?
        .to_string();
    let received_at_ns = raw
        .get("received_at_ns")
        .and_then(Value::as_u64)
        .context("market-tape row is missing received_at_ns")?;
    let bytes = match line {
        Some(line) => line.to_vec(),
        None => serde_json::to_vec(&Value::Object(raw.clone()))
            .context("serialize rewritten session row")?,
    };
    build.lob_continuity.observe(raw)?;
    if event_type == "agg_trade" {
        let trade = AggregateTrade::from_archived_event(raw, received_at_ns)?;
        build.trade_summaries.observe(&trade)?;
    }
    if event_type == "checkpoint" {
        build.checkpoint_symbols.insert(row_symbol(raw)?);
    }
    *build.event_types.entry(event_type).or_insert(0) += 1;
    build.events = build
        .events
        .checked_add(1)
        .context("slice event count overflow")?;
    build.bytes = build
        .bytes
        .checked_add(bytes.len() as u64 + 1)
        .context("slice byte count overflow")?;
    build.start_received_at_ns = Some(
        build
            .start_received_at_ns
            .map_or(received_at_ns, |start| start.min(received_at_ns)),
    );
    build.end_received_at_ns = Some(
        build
            .end_received_at_ns
            .map_or(received_at_ns, |end| end.max(received_at_ns)),
    );
    build.writer.write_all(&bytes)?;
    build.writer.write_all(b"\n")?;
    Ok(())
}

fn plan_slices(
    symbol_bytes: &BTreeMap<String, u64>,
    declared_order: &[String],
    selected: &[String],
    session_estimate: u64,
    budget: u64,
) -> Result<Vec<Vec<String>>> {
    let rank = declared_order
        .iter()
        .enumerate()
        .map(|(index, symbol)| (symbol.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let mut ordered = selected
        .iter()
        .map(|symbol| {
            (
                symbol_bytes.get(symbol).copied().unwrap_or(0),
                symbol.clone(),
            )
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let mut plans: Vec<(Vec<String>, u64)> = Vec::new();
    for (bytes, symbol) in ordered {
        if bytes + session_estimate > budget {
            bail!(
                "symbol {symbol} alone carries {bytes} decompressed bytes and cannot fit one slice below the seal bound"
            );
        }
        let target = plans
            .iter_mut()
            .find(|plan| plan.1 + bytes <= budget);
        match target {
            Some(plan) => {
                plan.0.push(symbol);
                plan.1 += bytes;
            }
            None => plans.push((vec![symbol], session_estimate + bytes)),
        }
    }
    if plans.is_empty() {
        bail!("no symbols selected for slicing");
    }
    for (symbols, _) in &mut plans {
        symbols.sort_by_key(|symbol| rank.get(symbol).copied().unwrap_or(usize::MAX));
    }
    Ok(plans.into_iter().map(|(symbols, _)| symbols).collect())
}

fn publish_slice(
    build: SliceBuild,
    source_manifest: &Map<String, Value>,
    output_dir: &Path,
    data_name: &str,
) -> Result<SliceEvidence> {
    let SliceBuild {
        symbols,
        mut writer,
        temporary,
        bytes,
        events,
        event_types,
        start_received_at_ns,
        end_received_at_ns,
        checkpoint_symbols,
        lob_continuity,
        trade_summaries,
    } = build;
    writer.flush()?;
    sync_file(&temporary)?;
    // The verifier requires one replay-safe checkpoint per declared symbol; a
    // literal snapshot row is optional because the v2 continuity gate seeds
    // replay state from a verified checkpoint (replay_checkpoint_seed).
    for symbol in &symbols {
        if !checkpoint_symbols.contains(symbol) {
            let _ = fs::remove_file(&temporary);
            bail!("slice would not carry a replay-safe checkpoint for {symbol}");
        }
    }
    if event_types.get("agg_trade").copied().unwrap_or(0) == 0 {
        let _ = fs::remove_file(&temporary);
        bail!("slice carries no aggregate trades and cannot pass the LOB continuity gate");
    }
    if events > MAX_SLICE_ROWS || bytes >= SEAL_DECOMPRESSED_BYTES {
        let _ = fs::remove_file(&temporary);
        bail!("slice exceeds the market-tape resource bounds");
    }
    let lob_continuity = lob_continuity.finish()?;
    let trade_summaries = trade_summaries.finish()?;
    let summary = SliceSummary {
        symbols,
        event_types,
        events,
        start_received_at_ns: start_received_at_ns.context("slice carries no rows")?,
        end_received_at_ns: end_received_at_ns.context("slice carries no rows")?,
    };

    let data_path = output_dir.join(data_name);
    let (temporary_zst, compressed) = temporary_file(&data_path)?;
    drop(compressed);
    compress_file(&temporary, &temporary_zst)?;
    fs::remove_file(&temporary)?;
    let compressed_bytes = fs::metadata(&temporary_zst)?.len();
    let content_sha256 = sha256_file(&temporary_zst)?;
    publish_temp_immutable(&temporary_zst, &data_path, &content_sha256)?;

    let success_path = sibling(&data_path, "._SUCCESS")?;
    publish_immutable_bytes(
        &success_path,
        format!("{content_sha256}\n").as_bytes(),
    )?;

    let manifest = slice_manifest(
        source_manifest,
        &summary,
        data_name,
        compressed_bytes,
        &content_sha256,
        trade_summaries,
        lob_continuity,
    )?;
    let mut manifest_bytes = serde_json::to_vec(&Value::Object(manifest))?;
    manifest_bytes.push(b'\n');
    let manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
    let manifest_path = sibling(&data_path, ".manifest.json")?;
    publish_immutable_bytes(&manifest_path, &manifest_bytes)?;

    // Proof before report: every slice must seal and verify under the
    // unchanged strict market-tape gate.
    let triplet = BinanceMarketTapeTriplet {
        data: data_path.clone(),
        manifest: manifest_path,
        success: success_path,
    };
    let trust =
        BinanceMarketTapeTrustAnchor::from_lower_hex(&content_sha256, &manifest_sha256)?;
    let sealed = seal_binance_market_tape_triplet(&triplet, &trust)?;
    verify_binance_market_tape_for_strict_gate(vec![sealed]).with_context(|| {
        format!("slice {data_name} failed the strict market-tape gate")
    })?;

    Ok(SliceEvidence {
        file: data_name.to_string(),
        sha256: content_sha256,
        manifest_sha256,
        symbols: summary.symbols,
        events: summary.events,
        decompressed_bytes: bytes,
        compressed_bytes,
        start_received_at_ns: summary.start_received_at_ns,
        end_received_at_ns: summary.end_received_at_ns,
    })
}

struct SliceSummary {
    symbols: Vec<String>,
    event_types: BTreeMap<String, u64>,
    events: u64,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
}

fn slice_manifest(
    source: &Map<String, Value>,
    summary: &SliceSummary,
    data_name: &str,
    compressed_bytes: u64,
    content_sha256: &str,
    trade_summaries: BTreeMap<String, data::binance_market_tape::AggregateTradeSummary>,
    lob_continuity: data::binance_market_tape::LobContinuitySummary,
) -> Result<Map<String, Value>> {
    let mut manifest = source.clone();
    let symbol_values = summary
        .symbols
        .iter()
        .cloned()
        .map(Value::String)
        .collect::<Vec<_>>();
    let slice_symbols = summary.symbols.iter().collect::<BTreeSet<_>>();
    let security_token_symbols = source
        .get("security_token_symbols")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|symbol| {
            symbol
                .as_str()
                .is_some_and(|symbol| slice_symbols.contains(&symbol.to_string()))
        })
        .collect::<Vec<_>>();
    let count = summary.symbols.len() as u64;
    manifest.insert("symbols".to_string(), Value::Array(symbol_values));
    manifest.insert(
        "security_token_symbols".to_string(),
        Value::Array(security_token_symbols),
    );
    manifest.insert("events".to_string(), Value::from(summary.events));
    manifest.insert(
        "event_types".to_string(),
        serde_json::to_value(&summary.event_types)?,
    );
    manifest.insert("snapshot_ready_count".to_string(), Value::from(count));
    manifest.insert("bridged_count".to_string(), Value::from(count));
    manifest.insert(
        "stream_coverage_verified_count".to_string(),
        Value::from(count),
    );
    manifest.insert("snapshot_only_symbols".to_string(), Value::Array(vec![]));
    manifest.insert(
        "raw_trade_incomplete_symbols".to_string(),
        Value::Array(vec![]),
    );
    manifest.insert("all_symbols_bridged".to_string(), Value::from(true));
    manifest.insert("all_stream_coverage_verified".to_string(), Value::from(true));
    manifest.insert("has_replay_safe_checkpoint".to_string(), Value::from(true));
    manifest.insert(
        "start_received_at_ns".to_string(),
        Value::from(summary.start_received_at_ns),
    );
    manifest.insert(
        "end_received_at_ns".to_string(),
        Value::from(summary.end_received_at_ns),
    );
    manifest.insert("file".to_string(), Value::from(data_name));
    manifest.insert("bytes".to_string(), Value::from(compressed_bytes));
    manifest.insert("sha256".to_string(), Value::from(content_sha256));
    manifest.insert(
        "trade_summaries".to_string(),
        serde_json::to_value(&trade_summaries)?,
    );
    manifest.insert(
        "lob_continuity".to_string(),
        serde_json::to_value(&lob_continuity)?,
    );
    Ok(manifest)
}

fn compress_file(source: &Path, target: &Path) -> Result<()> {
    let input = File::open(source)
        .with_context(|| format!("cannot reopen slice {}", source.display()))?;
    let output = File::create(target)
        .with_context(|| format!("cannot create compressed slice {}", target.display()))?;
    let mut encoder = zstd::stream::write::Encoder::new(output, COMPRESSION_LEVEL)
        .context("cannot create slice zstd encoder")?;
    std::io::copy(&mut BufReader::new(input), &mut encoder).context("compress slice rows")?;
    let output = encoder.finish().context("finish slice zstd stream")?;
    output.sync_all()?;
    Ok(())
}

fn canonical_output_dir(path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(path).with_context(|| {
        format!(
            "cannot create slice output directory {}",
            path.display()
        )
    })?;
    fs::canonicalize(path).with_context(|| {
        format!(
            "cannot resolve slice output directory {}",
            path.display()
        )
    })
}

fn publish_immutable_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let (temporary, mut output) = temporary_file(path)?;
    let write_result = output.write_all(bytes).and_then(|_| output.sync_all());
    drop(output);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    let expected = hex::encode(Sha256::digest(bytes));
    publish_temp_immutable(&temporary, path, &expected)
}

fn publish_temp_immutable(temporary: &Path, path: &Path, expected_sha256: &str) -> Result<()> {
    match fs::hard_link(temporary, path) {
        Ok(()) => {
            if let Err(error) = sync_parent_directory(path) {
                let _ = fs::remove_file(temporary);
                return Err(error);
            }
            fs::remove_file(temporary)?;
            sync_parent_directory(path)?;
        }
        Err(_error) if path.exists() => {
            let existing = sha256_file(path);
            let _ = fs::remove_file(temporary);
            let existing = existing?;
            if existing != expected_sha256 {
                bail!(
                    "immutable slice already exists with different content: {}",
                    path.display()
                );
            }
            return Ok(());
        }
        Err(error) => {
            let _ = fs::remove_file(temporary);
            return Err(error.into());
        }
    }
    Ok(())
}

fn temporary_file(path: &Path) -> Result<(PathBuf, File)> {
    let parent = path.parent().context("slice path has no parent directory")?;
    let file_name = file_name(path)?;
    let temporary = tempfile::Builder::new()
        .prefix(&format!(".{file_name}."))
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("cannot create temporary slice beside {}", path.display()))?;
    let (file, temporary_path) = temporary
        .keep()
        .with_context(|| format!("cannot retain temporary slice beside {}", path.display()))?;
    Ok((temporary_path, file))
}

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("cannot reopen slice for sync {}", path.display()))?
        .sync_all()
        .with_context(|| format!("cannot sync slice {}", path.display()))
}

fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path.parent().context("slice path has no parent directory")?;
    File::open(parent)
        .with_context(|| format!("cannot open slice directory {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("cannot sync slice directory {}", parent.display()))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut source = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn sibling(path: &Path, suffix: &str) -> Result<PathBuf> {
    Ok(path.with_file_name(format!("{}{suffix}", file_name(path)?)))
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .context("path has no UTF-8 file name")
}
