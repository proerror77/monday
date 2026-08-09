use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use data::binance_lob_replay::source_revision;
use hft_collector::{read_feature_rows, FeatureDatasetManifest};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::RowAccessor;
use parquet::schema::parser::parse_message_type;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REPLAY_KIND: &str = "backtest_canonical_replay_parquet";
const REPLAY_SCHEMA: &str = "binance-replay-parquet-v1";
const REPLAY_FORMAT: &str = "parquet";
const REPLAY_PARQUET_SCHEMA: &str =
    "timestamp_us:int64,sequence:int64,event:utf8,payload_json:utf8";
const REPLAY_PARQUET_MESSAGE: &str = "
message binance_replay {
  REQUIRED INT64 timestamp_us;
  REQUIRED INT64 sequence;
  REQUIRED BINARY event (UTF8);
  REQUIRED BINARY payload_json (UTF8);
}
";
const PIT_KIND: &str = "point_in_time_feature_matrix";
const PIT_SCHEMA: &str = "pit-feature-matrix-v2";
const RESULT_KIND: &str = "backtest_result_metadata";
const RESULT_SCHEMA: &str = "backtest-result-metadata-v1";

#[derive(Debug, Clone, Copy, ValueEnum)]
enum InputKind {
    ReplayEvent,
    PitFeature,
    BacktestResult,
}

#[derive(Debug, Parser)]
#[command(
    name = "clickhouse-analytics-materializer",
    about = "Verify canonical research artifacts and emit ClickHouse analytics rows"
)]
struct Args {
    /// Canonical input manifest. The artifact is resolved beside this file.
    #[arg(long)]
    manifest: PathBuf,
    /// Independently supplied SHA-256 identity for the manifest bytes.
    #[arg(long)]
    manifest_sha256: String,
    /// Input schema family; each family has a separate table and writer.
    #[arg(long, value_enum, default_value_t = InputKind::ReplayEvent)]
    kind: InputKind,
    /// Immutable local SQL/payload plan. Existing identical bytes are a retry; conflicts fail.
    #[arg(long)]
    output: PathBuf,
    /// Optional ClickHouse HTTP endpoint. No endpoint is contacted by offline tests.
    #[arg(long, requires = "claim_dir")]
    clickhouse_url: Option<String>,
    /// Shared filesystem directory used to serialize one writer per partition identity.
    #[arg(long, requires = "clickhouse_url")]
    claim_dir: Option<PathBuf>,
    #[arg(long, default_value = "default")]
    database: String,
    #[arg(long, default_value = "default")]
    user: String,
    /// PIT feature lineage overrides when source revisions contain multiple modalities.
    #[arg(long)]
    venue: Option<String>,
    #[arg(long)]
    market: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Lineage {
    partition_identity: String,
    manifest_sha256: String,
    artifact_sha256: String,
    source_revision: String,
    venue: String,
    market: String,
    symbol: String,
    start_time_us: i64,
    end_time_us: i64,
    schema_version: String,
}

struct LineageInput {
    manifest_sha256: String,
    artifact_sha256: String,
    source_revision: String,
    venue: String,
    market: String,
    symbol: String,
    start_time_us: i64,
    end_time_us: i64,
    schema_version: String,
    dataset_identity: Option<String>,
}

#[derive(Debug, Clone)]
struct Plan {
    lineage: Lineage,
    rows: Vec<(String, Value)>,
}

impl Plan {
    fn write_to(self, output: &mut impl Write) -> Result<Lineage> {
        for row in &self.rows {
            write_plan_row(output, row)?;
        }
        Ok(self.lineage)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalManifest {
    dataset_kind: String,
    schema_version: String,
    format: String,
    parquet_schema: String,
    mission_id: String,
    market: String,
    symbol: String,
    dataset: String,
    modalities: Vec<String>,
    source_revision: String,
    source_segments: Vec<SourceSegment>,
    rows: usize,
    first_event_time_us: i64,
    last_event_time_us: i64,
    sequence_start: u64,
    sequence_end: u64,
    artifact_path: PathBuf,
    artifact_sha256: String,
    point_in_time: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceSegment {
    file: String,
    sha256: String,
    collector_manifest_sha256: String,
    success_marker_sha256: String,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    events: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultManifest {
    dataset_kind: String,
    schema_version: String,
    format: String,
    venue: String,
    market: String,
    symbol: String,
    source_revision: String,
    rows: usize,
    first_event_time_us: i64,
    last_event_time_us: i64,
    artifact_path: PathBuf,
    artifact_sha256: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let output_parent = args
        .output
        .parent()
        .context("analytics plan has no parent directory")?;
    fs::create_dir_all(output_parent)?;
    let mut staged = tempfile::Builder::new()
        .prefix(&format!(".{}.", file_name(&args.output)?))
        .tempfile_in(output_parent)?;
    let lineage = match args.kind {
        InputKind::ReplayEvent => {
            replay_plan(&args.manifest, &args.manifest_sha256, staged.as_file_mut())?
        }
        InputKind::PitFeature => pit_plan(
            &args.manifest,
            &args.manifest_sha256,
            args.venue.as_deref(),
            args.market.as_deref(),
        )?
        .write_to(staged.as_file_mut())?,
        InputKind::BacktestResult => {
            result_plan(&args.manifest, &args.manifest_sha256)?.write_to(staged.as_file_mut())?
        }
    };
    staged.as_file().sync_all()?;
    publish_immutable_file(staged.path(), &args.output)?;
    if let Some(url) = args.clickhouse_url.as_deref() {
        let claim_dir = args
            .claim_dir
            .as_deref()
            .context("--claim-dir is required with --clickhouse-url")?;
        let password = std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_default();
        let sent = tokio::runtime::Runtime::new()?.block_on(send_to_clickhouse(
            url,
            &args.database,
            &args.user,
            &password,
            claim_dir,
            &lineage,
            staged.path(),
        ))?;
        eprintln!("clickhouse materialization: {sent}");
    }
    let mut input = File::open(staged.path())?;
    std::io::copy(&mut input, &mut std::io::stdout().lock())?;
    Ok(())
}

fn replay_plan(
    path: &Path,
    expected_manifest_sha256: &str,
    output: &mut impl Write,
) -> Result<Lineage> {
    let (manifest_path, manifest_bytes, manifest_sha256) =
        read_manifest(path, expected_manifest_sha256)?;
    let manifest: CanonicalManifest = serde_json::from_slice(&manifest_bytes)
        .context("cannot parse canonical Parquet manifest")?;
    validate_replay_manifest(&manifest)?;
    let artifact = manifest_path
        .parent()
        .context("canonical manifest has no parent")?
        .join(&manifest.artifact_path);
    let artifact_file =
        open_verified_file(&artifact, &manifest.artifact_sha256, "canonical Parquet")?;
    let lineage = lineage(LineageInput {
        manifest_sha256,
        artifact_sha256: manifest.artifact_sha256.clone(),
        source_revision: manifest.source_revision.clone(),
        venue: "binance".to_string(),
        market: manifest.market.clone(),
        symbol: manifest.symbol.clone(),
        start_time_us: manifest.first_event_time_us,
        end_time_us: manifest.last_event_time_us,
        schema_version: REPLAY_SCHEMA.to_string(),
        dataset_identity: None,
    });
    write_plan_row(output, &registry_row(&lineage, REPLAY_KIND, manifest.rows))?;
    read_replay_rows(
        artifact_file,
        &manifest,
        |timestamp_us, sequence, event, payload_json| {
            write_plan_row(
                output,
                &(
                    "cex_replay_events".to_string(),
                    row_with_lineage(
                        &lineage,
                        json!({
                            "row_identity": sequence.to_string(),
                            "materialization_version": 2,
                            "event_time_us": timestamp_us,
                            "sequence": sequence,
                            "event": event,
                            "payload_json": payload_json,
                        }),
                    ),
                ),
            )
        },
    )?;
    Ok(lineage)
}

fn pit_plan(
    path: &Path,
    expected_manifest_sha256: &str,
    venue_override: Option<&str>,
    market_override: Option<&str>,
) -> Result<Plan> {
    let (manifest_path, manifest_bytes, manifest_sha256) =
        read_manifest(path, expected_manifest_sha256)?;
    let mut manifest: FeatureDatasetManifest =
        serde_json::from_slice(&manifest_bytes).context("cannot parse PIT feature manifest")?;
    if manifest.dataset_kind != PIT_KIND || manifest.schema_version != PIT_SCHEMA {
        bail!("PIT feature manifest schema is unsupported");
    }
    validate_relative_artifact_path(&manifest.artifact_path)?;
    let artifact = manifest_path
        .parent()
        .context("PIT feature manifest has no parent")?
        .join(&manifest.artifact_path);
    manifest.artifact_path = artifact;
    let rows = read_feature_rows(&manifest).map_err(anyhow::Error::msg)?;
    if rows.len() != manifest.rows {
        bail!("PIT feature row count does not match its manifest");
    }
    let source_revision = feature_source_revision(&manifest.source_revisions)?;
    let (venue, market) =
        feature_venue_market(&manifest.source_revisions, venue_override, market_override)?;
    let first = rows.first().context("PIT feature matrix is empty")?;
    let last = rows.last().context("PIT feature matrix is empty")?;
    let lineage = lineage(LineageInput {
        manifest_sha256,
        artifact_sha256: manifest.artifact_sha256.clone(),
        source_revision,
        venue,
        market,
        symbol: manifest.symbol.clone(),
        start_time_us: first.event_time.timestamp_micros(),
        end_time_us: last.event_time.timestamp_micros(),
        schema_version: PIT_SCHEMA.to_string(),
        dataset_identity: Some(manifest.manifest_id.clone()),
    });
    let mut plan = Plan {
        lineage,
        rows: Vec::with_capacity(rows.len() + 1),
    };
    plan.rows
        .push(registry_row(&plan.lineage, PIT_KIND, rows.len()));
    for row in rows {
        plan.rows.push((
            "cex_pit_features".to_string(),
            row_with_lineage(
                &plan.lineage,
                json!({
                    "row_identity": row.event_time.timestamp_micros().to_string(),
                    "materialization_version": 2,
                    "event_time_us": row.event_time.timestamp_micros(),
                    "feature_available_time_us": row.feature_available_time.timestamp_micros(),
                    "label_available_time_us": row.label_available_time.timestamp_micros(),
                    "ingestion_time_us": row.ingestion_time.timestamp_micros(),
                    "features_json": serde_json::to_string(&row.features)?,
                    "label": row.label,
                }),
            ),
        ));
    }
    Ok(plan)
}

fn result_plan(path: &Path, expected_manifest_sha256: &str) -> Result<Plan> {
    let (manifest_path, manifest_bytes, manifest_sha256) =
        read_manifest(path, expected_manifest_sha256)?;
    let canonical_name = format!("{manifest_sha256}.result-manifest.json");
    if manifest_path.file_name().and_then(|name| name.to_str()) != Some(canonical_name.as_str()) {
        bail!("backtest result manifest path is not content addressed");
    }
    let manifest: ResultManifest =
        serde_json::from_slice(&manifest_bytes).context("cannot parse backtest result manifest")?;
    validate_relative_artifact_path(&manifest.artifact_path)?;
    if manifest.dataset_kind != RESULT_KIND
        || manifest.schema_version != RESULT_SCHEMA
        || manifest.format != "json"
        || manifest.rows != 1
        || manifest.venue.trim().is_empty()
        || !matches!(manifest.market.as_str(), "spot" | "usdm")
        || manifest.symbol.trim().is_empty()
        || manifest.source_revision.trim().is_empty()
        || manifest.first_event_time_us > manifest.last_event_time_us
    {
        bail!("backtest result manifest is incomplete or unsupported");
    }
    valid_sha256(&manifest.source_revision, "result source revision")?;
    valid_sha256(&manifest.artifact_sha256, "result artifact sha256")?;
    let artifact = manifest_path
        .parent()
        .context("backtest result manifest has no parent")?
        .join(&manifest.artifact_path);
    let artifact_bytes = fs::read(&artifact)?;
    let artifact_sha256 = sha256_bytes(&artifact_bytes);
    if artifact_sha256 != manifest.artifact_sha256 {
        bail!(
            "backtest result artifact SHA-256 mismatch: expected {}, actual {artifact_sha256}",
            manifest.artifact_sha256
        );
    }
    let artifact_json: Value =
        serde_json::from_slice(&artifact_bytes).context("backtest result artifact is not JSON")?;
    let lineage = lineage(LineageInput {
        manifest_sha256,
        artifact_sha256: manifest.artifact_sha256.clone(),
        source_revision: manifest.source_revision,
        venue: manifest.venue,
        market: manifest.market,
        symbol: manifest.symbol,
        start_time_us: manifest.first_event_time_us,
        end_time_us: manifest.last_event_time_us,
        schema_version: RESULT_SCHEMA.to_string(),
        dataset_identity: Some(manifest.artifact_sha256.clone()),
    });
    let mut plan = Plan {
        lineage,
        rows: Vec::with_capacity(2),
    };
    plan.rows
        .push(registry_row(&plan.lineage, RESULT_KIND, manifest.rows));
    plan.rows.push((
        "cex_backtest_results".to_string(),
        row_with_lineage(
            &plan.lineage,
            json!({
                "row_identity": plan.lineage.artifact_sha256,
                "materialization_version": 2,
                "result_json": serde_json::to_string(&artifact_json)?
            }),
        ),
    ));
    Ok(plan)
}

fn validate_replay_manifest(manifest: &CanonicalManifest) -> Result<()> {
    validate_relative_artifact_path(&manifest.artifact_path)?;
    if manifest.dataset_kind != REPLAY_KIND
        || manifest.schema_version != REPLAY_SCHEMA
        || manifest.format != REPLAY_FORMAT
        || manifest.parquet_schema != REPLAY_PARQUET_SCHEMA
        || manifest.mission_id.trim().is_empty()
        || !matches!(manifest.market.as_str(), "spot" | "usdm")
        || manifest.symbol.trim().is_empty()
        || manifest.symbol != manifest.symbol.to_ascii_uppercase()
        || manifest.dataset
            != match manifest.market.as_str() {
                "spot" => "binance_spot_lob",
                "usdm" => "binance_usdm_lob",
                _ => "",
            }
        || manifest.modalities != ["lob"]
        || !manifest.point_in_time
        || manifest.rows == 0
        || manifest.source_segments.is_empty()
        || manifest.first_event_time_us > manifest.last_event_time_us
        || manifest.sequence_start != 1
        || manifest.sequence_end < manifest.sequence_start
        || manifest.sequence_end - manifest.sequence_start + 1 != manifest.rows as u64
    {
        bail!("canonical Parquet manifest is incomplete or unsupported");
    }
    valid_sha256(&manifest.artifact_sha256, "manifest.artifact_sha256")?;
    valid_sha256(&manifest.source_revision, "manifest.source_revision")?;
    if manifest.artifact_path != Path::new(&format!("{}.parquet", manifest.artifact_sha256)) {
        bail!("canonical Parquet artifact path is not content addressed");
    }
    let mut hashes = Vec::with_capacity(manifest.source_segments.len());
    let mut unique = HashSet::new();
    for segment in &manifest.source_segments {
        if segment.file.trim().is_empty()
            || segment.events == 0
            || segment.start_received_at_ns > segment.end_received_at_ns
            || !unique.insert(&segment.sha256)
        {
            bail!("canonical source segment evidence is incomplete or duplicated");
        }
        valid_sha256(&segment.sha256, "source segment sha256")?;
        valid_sha256(
            &segment.collector_manifest_sha256,
            "source collector manifest sha256",
        )?;
        valid_sha256(
            &segment.success_marker_sha256,
            "source success marker sha256",
        )?;
        if segment.success_marker_sha256 != sha256_bytes(format!("{}\n", segment.sha256).as_bytes())
        {
            bail!("canonical source success marker identity is inconsistent");
        }
        hashes.push(segment.sha256.as_str());
    }
    if source_revision(hashes) != manifest.source_revision {
        bail!("canonical source revision does not match source segments");
    }
    Ok(())
}

fn validate_relative_artifact_path(path: &Path) -> Result<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("artifact path escapes its manifest directory");
    }
    Ok(())
}

fn read_replay_rows<F>(artifact: File, manifest: &CanonicalManifest, mut consume: F) -> Result<()>
where
    F: FnMut(i64, u64, String, String) -> Result<()>,
{
    let reader = SerializedFileReader::new(artifact)?;
    let schema = parse_message_type(REPLAY_PARQUET_MESSAGE)?;
    if reader
        .metadata()
        .file_metadata()
        .schema_descr_ptr()
        .root_schema()
        != &schema
    {
        bail!("canonical Parquet schema does not match its manifest");
    }
    let mut row_count = 0_usize;
    let mut first_timestamp = None;
    let mut last_timestamp = None;
    let mut expected_sequence = manifest.sequence_start;
    let mut previous_timestamp = None;
    for row in reader.get_row_iter(Some(schema))? {
        let row = row?;
        let timestamp_us = row.get_long(0)?;
        let sequence = u64::try_from(row.get_long(1)?).context("canonical sequence is negative")?;
        let event = row.get_string(2)?.to_string();
        let payload_json = row.get_string(3)?.to_string();
        let payload: Value = serde_json::from_str(&payload_json)?;
        if !matches!(event.as_str(), "snapshot" | "l2_update")
            || sequence != expected_sequence
            || previous_timestamp.is_some_and(|previous| timestamp_us < previous)
            || (row_count == 0 && event != "snapshot")
        {
            bail!("canonical Parquet rows are not valid replay events");
        }
        validate_replay_payload(&payload)?;
        expected_sequence = expected_sequence
            .checked_add(1)
            .context("canonical sequence overflow")?;
        previous_timestamp = Some(timestamp_us);
        first_timestamp.get_or_insert(timestamp_us);
        last_timestamp = Some(timestamp_us);
        row_count += 1;
        consume(timestamp_us, sequence, event, payload_json)?;
    }
    if row_count != manifest.rows
        || first_timestamp != Some(manifest.first_event_time_us)
        || last_timestamp != Some(manifest.last_event_time_us)
        || expected_sequence
            != manifest
                .sequence_end
                .checked_add(1)
                .context("canonical sequence overflow")?
    {
        bail!("canonical Parquet coverage does not match its manifest");
    }
    Ok(())
}

fn feature_source_revision(revisions: &BTreeMap<String, String>) -> Result<String> {
    if revisions.is_empty()
        || revisions
            .values()
            .any(|revision| revision.trim().is_empty())
    {
        bail!("PIT source revisions are incomplete");
    }
    if revisions.len() == 1 {
        return Ok(revisions
            .values()
            .next()
            .expect("one revision exists")
            .clone());
    }
    Ok(sha256_bytes(&serde_json::to_vec(revisions)?))
}

fn feature_venue_market(
    revisions: &BTreeMap<String, String>,
    venue_override: Option<&str>,
    market_override: Option<&str>,
) -> Result<(String, String)> {
    if venue_override.is_some() != market_override.is_some() {
        bail!("PIT lineage requires both --venue and --market overrides");
    }
    if let (Some(venue), Some(market)) = (venue_override, market_override) {
        if venue.trim().is_empty() || !matches!(market, "spot" | "usdm") {
            bail!("PIT lineage overrides must be non-empty");
        }
        return Ok((venue.to_string(), market.to_string()));
    }
    let key = revisions
        .keys()
        .next()
        .context("PIT source revisions are empty")?;
    let mut parts = key.split('-');
    let venue = parts.next().unwrap_or_default();
    let market = parts.next().unwrap_or_default();
    if venue.trim().is_empty()
        || !matches!(market, "spot" | "usdm")
        || revisions.keys().any(|key| {
            let mut parts = key.split('-');
            parts.next() != Some(venue) || parts.next() != Some(market)
        })
    {
        bail!("PIT source revision key must include venue and market");
    }
    Ok((venue.to_string(), market.to_string()))
}

fn validate_replay_payload(payload: &Value) -> Result<()> {
    let object = payload
        .as_object()
        .context("canonical replay payload must be an object")?;
    for side in ["bids", "asks"] {
        let levels = object
            .get(side)
            .and_then(Value::as_array)
            .with_context(|| format!("canonical replay payload is missing {side}"))?;
        for level in levels {
            let pair = level
                .as_array()
                .filter(|pair| pair.len() == 2)
                .with_context(|| {
                    format!("canonical replay {side} level is not a price/quantity pair")
                })?;
            let price = pair[0]
                .as_str()
                .and_then(|value| value.parse::<Decimal>().ok())
                .context("canonical replay price is not numeric")?;
            let quantity = pair[1]
                .as_str()
                .and_then(|value| value.parse::<Decimal>().ok())
                .context("canonical replay quantity is not numeric")?;
            if price <= Decimal::ZERO || quantity < Decimal::ZERO {
                bail!("canonical replay level has invalid price or quantity");
            }
        }
    }
    Ok(())
}

fn lineage(input: LineageInput) -> Lineage {
    let LineageInput {
        manifest_sha256,
        artifact_sha256,
        source_revision,
        venue,
        market,
        symbol,
        start_time_us,
        end_time_us,
        schema_version,
        dataset_identity,
    } = input;
    let partition_identity = sha256_bytes(
        &serde_json::to_vec(&(
            &schema_version,
            &venue,
            &market,
            &symbol,
            start_time_us,
            end_time_us,
            dataset_identity,
        ))
        .expect("partition identity serializes"),
    );
    Lineage {
        partition_identity,
        manifest_sha256,
        artifact_sha256,
        source_revision,
        venue,
        market,
        symbol,
        start_time_us,
        end_time_us,
        schema_version,
    }
}

fn registry_row(lineage: &Lineage, dataset_kind: &str, row_count: usize) -> (String, Value) {
    (
        "cex_analytics_partitions".to_string(),
        row_with_lineage(
            lineage,
            json!({
                "dataset_kind": dataset_kind,
                "artifact_sha256": lineage.artifact_sha256,
                "row_count": row_count,
                "materialization_state": "complete",
                "materialization_version": 2,
            }),
        ),
    )
}

fn row_with_lineage(lineage: &Lineage, extra: Value) -> Value {
    let mut row = serde_json::to_value(lineage).expect("lineage serializes");
    let object = row.as_object_mut().expect("lineage is an object");
    object.extend(extra.as_object().expect("row payload is an object").clone());
    Value::Object(object.clone())
}

#[cfg(test)]
fn encode_plan(plan: &Plan) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    for row in &plan.rows {
        write_plan_row(&mut output, row)?;
    }
    Ok(output)
}

fn write_plan_row(output: &mut impl Write, (table, payload): &(String, Value)) -> Result<()> {
    let mut row = payload.clone();
    let object = row
        .as_object_mut()
        .context("analytics row is not an object")?;
    object.insert("table".to_string(), table.clone().into());
    object.insert(
        "sql".to_string(),
        format!("INSERT INTO {table} FORMAT JSONEachRow").into(),
    );
    serde_json::to_writer(&mut *output, &row)?;
    output.write_all(b"\n")?;
    Ok(())
}

fn read_manifest(
    path: &Path,
    expected_manifest_sha256: &str,
) -> Result<(PathBuf, Vec<u8>, String)> {
    valid_sha256(expected_manifest_sha256, "expected manifest sha256")?;
    let path = fs::canonicalize(path)
        .with_context(|| format!("cannot resolve manifest {}", path.display()))?;
    let bytes =
        fs::read(&path).with_context(|| format!("cannot read manifest {}", path.display()))?;
    let hash = sha256_bytes(&bytes);
    if hash != expected_manifest_sha256 {
        bail!("manifest SHA-256 mismatch: expected {expected_manifest_sha256}, actual {hash}");
    }
    Ok((path, bytes, hash))
}

fn open_verified_file(path: &Path, expected_sha256: &str, label: &str) -> Result<File> {
    valid_sha256(expected_sha256, &format!("{label} sha256"))?;
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual = hex::encode(digest.finalize());
    if actual != expected_sha256 {
        bail!("{label} SHA-256 mismatch: expected {expected_sha256}, actual {actual}");
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

#[cfg(test)]
fn publish_immutable(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("analytics plan has no parent directory")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(&format!(".{}.", file_name(path)?))
        .tempfile_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    publish_immutable_file(temporary.path(), path)
}

fn publish_immutable_file(temporary: &Path, path: &Path) -> Result<()> {
    match fs::hard_link(temporary, path) {
        Ok(()) => {
            File::open(
                path.parent()
                    .context("analytics plan has no parent directory")?,
            )?
            .sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("immutable analytics plan path is not a regular file");
            }
            if sha256_path(path)? != sha256_path(temporary)? {
                bail!("immutable analytics plan already exists with different content");
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

async fn send_to_clickhouse(
    url: &str,
    database: &str,
    user: &str,
    password: &str,
    claim_dir: &Path,
    lineage: &Lineage,
    plan_path: &Path,
) -> Result<String> {
    let database = sql_identifier(database)?;
    let _claim = acquire_partition_claim(claim_dir, &lineage.partition_identity)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()?;
    let endpoint = format!("{}/", url.trim_end_matches('/'));
    verify_table_contracts(&client, &endpoint, database, user, password, plan_path).await?;
    let existing = fetch_partition(
        &client,
        &endpoint,
        database,
        user,
        password,
        &lineage.partition_identity,
    )
    .await?;
    if let Some(row) = existing.as_ref() {
        let existing_hash = row
            .get("manifest_sha256")
            .and_then(Value::as_str)
            .context("ClickHouse partition registry is missing manifest_sha256")?;
        if existing_hash != lineage.manifest_sha256 {
            bail!("ClickHouse partition identity conflicts with existing manifest");
        }
        match row.get("materialization_state").and_then(Value::as_str) {
            Some("complete") => return Ok("idempotent-hit".to_string()),
            Some("pending") => {}
            _ => bail!("ClickHouse partition registry state is unsupported"),
        }
    }

    let complete = read_registry_row(plan_path)?;
    if existing.is_none() {
        let mut pending = complete.clone();
        pending["materialization_state"] = "pending".into();
        pending["materialization_version"] = 1.into();
        insert_rows(
            &client,
            &endpoint,
            database,
            user,
            password,
            "cex_analytics_partitions",
            serde_json::to_vec(&pending)?,
        )
        .await?;
        let winner = fetch_partition(
            &client,
            &endpoint,
            database,
            user,
            password,
            &lineage.partition_identity,
        )
        .await?
        .context("ClickHouse pending claim was not readable after insert")?;
        if winner.get("manifest_sha256").and_then(Value::as_str)
            != Some(lineage.manifest_sha256.as_str())
            || winner.get("materialization_state").and_then(Value::as_str) != Some("pending")
        {
            bail!("ClickHouse pending claim lost its identity readback");
        }
    }
    send_plan_rows(&client, &endpoint, database, user, password, plan_path).await?;
    insert_rows(
        &client,
        &endpoint,
        database,
        user,
        password,
        "cex_analytics_partitions",
        serde_json::to_vec(&complete)?,
    )
    .await?;
    Ok("inserted".to_string())
}

async fn verify_table_contracts(
    client: &reqwest::Client,
    endpoint: &str,
    database: &str,
    user: &str,
    password: &str,
    plan_path: &Path,
) -> Result<()> {
    let mut tables = BTreeSet::from(["cex_analytics_partitions".to_string()]);
    for line in BufReader::new(File::open(plan_path)?).lines().skip(1) {
        let row: Value = serde_json::from_str(&line?)?;
        let table = row
            .get("table")
            .and_then(Value::as_str)
            .context("analytics plan row is missing table")?;
        if !matches!(
            table,
            "cex_replay_events" | "cex_pit_features" | "cex_backtest_results"
        ) {
            bail!("analytics plan contains an unsupported table");
        }
        tables.insert(table.to_string());
    }
    for table in tables {
        let query = "SELECT engine, engine_full, sorting_key FROM system.tables WHERE database = {database:String} AND name = {table:String} FORMAT JSONEachRow";
        let response = client
            .post(endpoint)
            .query(&[
                ("query", query),
                ("param_database", database),
                ("param_table", table.as_str()),
            ])
            .basic_auth(user, (!password.is_empty()).then_some(password))
            .send()
            .await?;
        if !response.status().is_success() {
            bail!(
                "ClickHouse table contract query failed with {}",
                response.status()
            );
        }
        let body = response.text().await?;
        let mut rows = body.lines().map(serde_json::from_str::<Value>);
        let row = rows
            .next()
            .transpose()?
            .with_context(|| format!("ClickHouse table {database}.{table} does not exist"))?;
        if rows.next().is_some() {
            bail!("ClickHouse table contract query returned duplicate rows");
        }
        validate_table_contract(&table, &row)?;
    }
    Ok(())
}

fn validate_table_contract(table: &str, row: &Value) -> Result<()> {
    let engine = row
        .get("engine")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let engine_full = row
        .get("engine_full")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != '`')
        .collect::<String>();
    let sorting_key = normalize_sorting_key(
        row.get("sorting_key")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let expected_key = if table == "cex_analytics_partitions" {
        "partition_identity"
    } else {
        "partition_identity,row_identity"
    };
    if engine != "ReplacingMergeTree"
        || !engine_full.contains("ReplacingMergeTree(materialization_version)")
        || sorting_key != expected_key
    {
        bail!("ClickHouse table {table} does not satisfy the materialization engine/key contract");
    }
    Ok(())
}

fn normalize_sorting_key(value: &str) -> String {
    let compact = value
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != '`')
        .collect::<String>();
    let compact = compact
        .strip_prefix("tuple(")
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(&compact)
        .to_string();
    compact
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(&compact)
        .to_string()
}

fn acquire_partition_claim(claim_dir: &Path, partition_identity: &str) -> Result<File> {
    fs::create_dir_all(claim_dir)?;
    let claim_dir = fs::canonicalize(claim_dir)?;
    let claim_path = claim_dir.join(format!("{partition_identity}.lock"));
    let claim = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&claim_path)?;
    claim.try_lock().map_err(|error| {
        anyhow::anyhow!(
            "another ClickHouse materializer owns partition claim {}: {error}",
            claim_path.display()
        )
    })?;
    Ok(claim)
}

async fn fetch_partition(
    client: &reqwest::Client,
    endpoint: &str,
    database: &str,
    user: &str,
    password: &str,
    partition_identity: &str,
) -> Result<Option<Value>> {
    let query = identity_query(database);
    let response = client
        .post(endpoint)
        .query(&[
            ("query", query.as_str()),
            ("param_partition_identity", partition_identity),
        ])
        .basic_auth(user, (!password.is_empty()).then_some(password))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "ClickHouse identity query failed with {}",
            response.status()
        );
    }
    let body = response.text().await?;
    let mut rows = body.lines().map(serde_json::from_str::<Value>);
    let first = rows.next().transpose()?;
    if rows.next().is_some() {
        bail!("ClickHouse partition registry returned multiple identities");
    }
    Ok(first)
}

fn read_registry_row(plan_path: &Path) -> Result<Value> {
    let mut line = String::new();
    BufReader::new(File::open(plan_path)?).read_line(&mut line)?;
    let mut row: Value =
        serde_json::from_str(&line).context("analytics plan registry row is invalid")?;
    let object = row
        .as_object_mut()
        .context("analytics plan registry row is not an object")?;
    if object
        .remove("table")
        .and_then(|value| value.as_str().map(str::to_string))
        != Some("cex_analytics_partitions".to_string())
    {
        bail!("analytics plan does not start with its partition registry");
    }
    object.remove("sql");
    Ok(row)
}

async fn send_plan_rows(
    client: &reqwest::Client,
    endpoint: &str,
    database: &str,
    user: &str,
    password: &str,
    plan_path: &Path,
) -> Result<()> {
    const MAX_BATCH_BYTES: usize = 1024 * 1024;
    let mut lines = BufReader::new(File::open(plan_path)?).lines();
    lines
        .next()
        .transpose()?
        .context("analytics plan is empty")?;
    let mut table = String::new();
    let mut batch = Vec::with_capacity(MAX_BATCH_BYTES);
    for line in lines {
        let mut row: Value = serde_json::from_str(&line?)?;
        let object = row
            .as_object_mut()
            .context("analytics plan row is not an object")?;
        let row_table = object
            .remove("table")
            .and_then(|value| value.as_str().map(str::to_string))
            .context("analytics plan row is missing table")?;
        object.remove("sql");
        let mut encoded = serde_json::to_vec(&row)?;
        encoded.push(b'\n');
        if !batch.is_empty()
            && (row_table != table || batch.len() + encoded.len() > MAX_BATCH_BYTES)
        {
            insert_rows(client, endpoint, database, user, password, &table, batch).await?;
            batch = Vec::with_capacity(MAX_BATCH_BYTES);
        }
        table = row_table;
        batch.extend_from_slice(&encoded);
    }
    if !batch.is_empty() {
        insert_rows(client, endpoint, database, user, password, &table, batch).await?;
    }
    Ok(())
}

async fn insert_rows(
    client: &reqwest::Client,
    endpoint: &str,
    database: &str,
    user: &str,
    password: &str,
    table: &str,
    mut bytes: Vec<u8>,
) -> Result<()> {
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    let query = format!("INSERT INTO {database}.{table} FORMAT JSONEachRow");
    let response = client
        .post(endpoint)
        .query(&[("query", query.as_str())])
        .basic_auth(user, (!password.is_empty()).then_some(password))
        .body(bytes)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "ClickHouse insert into {table} failed with {}",
            response.status()
        );
    }
    Ok(())
}

fn identity_query(database: &str) -> String {
    format!(
        "SELECT manifest_sha256, materialization_state, materialization_version FROM {database}.cex_analytics_partitions FINAL WHERE partition_identity = {{partition_identity:String}} LIMIT 1 FORMAT JSONEachRow"
    )
}

fn sql_identifier(value: &str) -> Result<&str> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("ClickHouse database name must be an ASCII identifier");
    }
    Ok(value)
}

fn valid_sha256(value: &str, field: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{field} must be a 64-character hex SHA-256");
    }
    Ok(())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn sha256_path(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .context("path has no UTF-8 file name")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_fixture() -> (tempfile::NamedTempFile, Lineage) {
        let lineage = lineage(LineageInput {
            manifest_sha256: "a".repeat(64),
            artifact_sha256: "b".repeat(64),
            source_revision: "c".repeat(64),
            venue: "binance".to_string(),
            market: "spot".to_string(),
            symbol: "BTCUSDT".to_string(),
            start_time_us: 1,
            end_time_us: 2,
            schema_version: RESULT_SCHEMA.to_string(),
            dataset_identity: Some("experiment-1".to_string()),
        });
        let plan = Plan {
            lineage: lineage.clone(),
            rows: vec![
                registry_row(&lineage, RESULT_KIND, 1),
                (
                    "cex_backtest_results".to_string(),
                    row_with_lineage(
                        &lineage,
                        json!({
                            "row_identity": lineage.artifact_sha256,
                            "materialization_version": 2,
                            "result_json": "{}",
                        }),
                    ),
                ),
            ],
        };
        let mut file = tempfile::NamedTempFile::new().unwrap();
        plan.write_to(file.as_file_mut()).unwrap();
        file.as_file().sync_all().unwrap();
        (file, lineage)
    }

    fn mock_http(
        responses: Vec<String>,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = requests.clone();
        let server = std::thread::spawn(move || {
            for response_body in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    let Some(header_end) =
                        request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .or_else(|| line.strip_prefix("Content-Length: "))
                        })
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&request).into_owned());
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                )
                .unwrap();
            }
        });
        (format!("http://{address}"), requests, server)
    }

    #[test]
    fn partition_identity_conflict_is_not_an_overwrite() {
        let a = lineage(LineageInput {
            manifest_sha256: "a".repeat(64),
            artifact_sha256: "b".repeat(64),
            source_revision: "c".repeat(64),
            venue: "binance".to_string(),
            market: "spot".to_string(),
            symbol: "BTCUSDT".to_string(),
            start_time_us: 1,
            end_time_us: 2,
            schema_version: REPLAY_SCHEMA.to_string(),
            dataset_identity: None,
        });
        let mut b = a.clone();
        b.manifest_sha256 = "d".repeat(64);
        assert_eq!(a.partition_identity, b.partition_identity);
        assert_ne!(a.manifest_sha256, b.manifest_sha256);
    }

    #[test]
    fn partition_identity_distinguishes_experiments() {
        let make = |dataset_identity: &str| {
            lineage(LineageInput {
                manifest_sha256: "a".repeat(64),
                artifact_sha256: "b".repeat(64),
                source_revision: "c".repeat(64),
                venue: "binance".to_string(),
                market: "spot".to_string(),
                symbol: "BTCUSDT".to_string(),
                start_time_us: 1,
                end_time_us: 2,
                schema_version: RESULT_SCHEMA.to_string(),
                dataset_identity: Some(dataset_identity.to_string()),
            })
        };
        assert_ne!(
            make("experiment-1").partition_identity,
            make("experiment-2").partition_identity
        );
    }

    #[test]
    fn artifact_paths_cannot_escape_the_manifest_directory() {
        assert!(validate_relative_artifact_path(Path::new("artifact.parquet")).is_ok());
        assert!(validate_relative_artifact_path(Path::new("../artifact.parquet")).is_err());
        assert!(validate_relative_artifact_path(Path::new("/tmp/artifact.parquet")).is_err());
    }

    #[test]
    fn immutable_publication_has_one_race_winner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("plan.jsonl");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles = [b"first\n".as_slice(), b"second\n".as_slice()].map(|bytes| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                publish_immutable(&path, bytes)
            })
        });
        let results = handles.map(|handle| handle.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let winner = fs::read(&path).unwrap();
        assert!(winner == b"first\n" || winner == b"second\n");
        assert!(publish_immutable(&path, &winner).is_ok());
        assert!(publish_immutable(&path, b"conflict\n").is_err());
    }

    #[test]
    fn partition_claim_allows_only_one_writer() {
        let directory = tempfile::tempdir().unwrap();
        let first = acquire_partition_claim(directory.path(), &"a".repeat(64)).unwrap();
        assert!(acquire_partition_claim(directory.path(), &"a".repeat(64)).is_err());
        drop(first);
        assert!(acquire_partition_claim(directory.path(), &"a".repeat(64)).is_ok());
    }

    #[test]
    fn remote_writer_claims_reads_back_resumes_and_rejects_conflicts() {
        let (plan, lineage) = remote_fixture();
        let claim_dir = tempfile::tempdir().unwrap();
        let pending = json!({
            "manifest_sha256": lineage.manifest_sha256,
            "materialization_state": "pending",
            "materialization_version": 1,
        });
        let registry_contract = json!({
            "engine": "ReplacingMergeTree",
            "engine_full": "ReplacingMergeTree(materialization_version)",
            "sorting_key": "partition_identity",
        });
        let data_contract = json!({
            "engine": "ReplacingMergeTree",
            "engine_full": "ReplacingMergeTree(materialization_version)",
            "sorting_key": "tuple(partition_identity, row_identity)",
        });
        let contracts = || {
            vec![
                format!("{registry_contract}\n"),
                format!("{data_contract}\n"),
            ]
        };
        let (url, requests, server) = mock_http(
            [
                contracts(),
                vec![
                    String::new(),
                    String::new(),
                    format!("{pending}\n"),
                    String::new(),
                    String::new(),
                ],
            ]
            .concat(),
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(
            runtime
                .block_on(send_to_clickhouse(
                    &url,
                    "default",
                    "default",
                    "",
                    claim_dir.path(),
                    &lineage,
                    plan.path(),
                ))
                .unwrap(),
            "inserted"
        );
        server.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 7);
        assert!(requests[2].contains("param_partition_identity="));
        assert!(requests[2].contains("partition_identity%3AString"));
        assert!(requests[3].contains("\"materialization_state\":\"pending\""));
        assert!(requests[5].contains("\"row_identity\""));
        assert!(requests[6].contains("\"materialization_state\":\"complete\""));
        drop(requests);

        let complete = json!({
            "manifest_sha256": lineage.manifest_sha256,
            "materialization_state": "complete",
            "materialization_version": 2,
        });
        let (url, _, server) = mock_http([contracts(), vec![format!("{complete}\n")]].concat());
        assert_eq!(
            runtime
                .block_on(send_to_clickhouse(
                    &url,
                    "default",
                    "default",
                    "",
                    claim_dir.path(),
                    &lineage,
                    plan.path(),
                ))
                .unwrap(),
            "idempotent-hit"
        );
        server.join().unwrap();

        let conflict = json!({
            "manifest_sha256": "d".repeat(64),
            "materialization_state": "complete",
            "materialization_version": 2,
        });
        let (url, _, server) = mock_http([contracts(), vec![format!("{conflict}\n")]].concat());
        let error = runtime
            .block_on(send_to_clickhouse(
                &url,
                "default",
                "default",
                "",
                claim_dir.path(),
                &lineage,
                plan.path(),
            ))
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("conflicts with existing manifest"));
        server.join().unwrap();
    }

    #[test]
    fn table_contracts_fail_closed_on_wrong_engine_or_key() {
        let wrong_engine = json!({
            "engine": "MergeTree",
            "engine_full": "MergeTree()",
            "sorting_key": "partition_identity",
        });
        assert!(validate_table_contract("cex_analytics_partitions", &wrong_engine).is_err());
        let wrong_key = json!({
            "engine": "ReplacingMergeTree",
            "engine_full": "ReplacingMergeTree(materialization_version)",
            "sorting_key": "partition_identity",
        });
        assert!(validate_table_contract("cex_backtest_results", &wrong_key).is_err());
    }

    #[test]
    fn sql_literals_escape_identity_delimiters() {
        assert!(sql_identifier("hft_db").is_ok());
        assert!(sql_identifier("hft-db").is_err());
        let query = identity_query("hft_db");
        assert!(query.contains("partition_identity = {partition_identity:String}"));
        assert!(query.contains("FINAL"));
    }

    #[test]
    fn feature_lineage_rejects_mixed_modalities_without_overrides() {
        let revisions = BTreeMap::from([
            ("binance-usdm-lob".to_string(), "a".to_string()),
            ("ethereum".to_string(), "b".to_string()),
        ]);
        assert!(feature_venue_market(&revisions, None, None).is_err());
        assert_eq!(
            feature_venue_market(&revisions, Some("binance"), Some("usdm")).unwrap(),
            ("binance".to_string(), "usdm".to_string())
        );
    }

    #[test]
    fn typed_writers_have_disjoint_tables_and_sql_contracts() {
        let lineage = lineage(LineageInput {
            manifest_sha256: "a".repeat(64),
            artifact_sha256: "b".repeat(64),
            source_revision: "c".repeat(64),
            venue: "binance".to_string(),
            market: "spot".to_string(),
            symbol: "BTCUSDT".to_string(),
            start_time_us: 1,
            end_time_us: 2,
            schema_version: RESULT_SCHEMA.to_string(),
            dataset_identity: Some("experiment-1".to_string()),
        });
        let plan = Plan {
            lineage: lineage.clone(),
            rows: vec![
                registry_row(&lineage, REPLAY_KIND, 1),
                (
                    "cex_replay_events".to_string(),
                    row_with_lineage(&lineage, json!({})),
                ),
                (
                    "cex_pit_features".to_string(),
                    row_with_lineage(&lineage, json!({})),
                ),
                (
                    "cex_backtest_results".to_string(),
                    row_with_lineage(&lineage, json!({})),
                ),
            ],
        };
        let encoded = String::from_utf8(encode_plan(&plan).unwrap()).unwrap();
        assert!(encoded.contains("INSERT INTO cex_replay_events FORMAT JSONEachRow"));
        assert!(encoded.contains("INSERT INTO cex_pit_features FORMAT JSONEachRow"));
        assert!(encoded.contains("INSERT INTO cex_backtest_results FORMAT JSONEachRow"));
    }
}
