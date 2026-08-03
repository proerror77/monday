use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use data::binance_lob_replay::source_revision;
use hft_collector::{lob_archiver::sha256_file, read_feature_rows, FeatureDatasetManifest};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::RowAccessor;
use parquet::schema::parser::parse_message_type;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

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
    /// Input schema family; each family has a separate table and writer.
    #[arg(long, value_enum, default_value_t = InputKind::ReplayEvent)]
    kind: InputKind,
    /// Immutable local SQL/payload plan. Existing identical bytes are a retry; conflicts fail.
    #[arg(long)]
    output: PathBuf,
    /// Optional ClickHouse HTTP endpoint. No endpoint is contacted by offline tests.
    #[arg(long)]
    clickhouse_url: Option<String>,
    #[arg(long, default_value = "default")]
    database: String,
    #[arg(long, default_value = "default")]
    user: String,
    #[arg(long, default_value = "")]
    password: String,
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
}

#[derive(Debug, Clone)]
struct Plan {
    lineage: Lineage,
    rows: Vec<(String, Value)>,
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
    let plan = match args.kind {
        InputKind::ReplayEvent => replay_plan(&args.manifest)?,
        InputKind::PitFeature => pit_plan(
            &args.manifest,
            args.venue.as_deref(),
            args.market.as_deref(),
        )?,
        InputKind::BacktestResult => result_plan(&args.manifest)?,
    };
    let bytes = encode_plan(&plan)?;
    publish_immutable(&args.output, &bytes)?;
    if let Some(url) = args.clickhouse_url.as_deref() {
        let sent = tokio::runtime::Runtime::new()?.block_on(send_to_clickhouse(
            url,
            &args.database,
            &args.user,
            &args.password,
            &plan,
        ))?;
        eprintln!("clickhouse materialization: {sent}");
    }
    std::io::stdout().write_all(&bytes)?;
    Ok(())
}

fn replay_plan(path: &Path) -> Result<Plan> {
    let (manifest_path, manifest_bytes, manifest_sha256) = read_manifest(path)?;
    let manifest: CanonicalManifest = serde_json::from_slice(&manifest_bytes)
        .context("cannot parse canonical Parquet manifest")?;
    validate_replay_manifest(&manifest)?;
    let artifact = manifest_path
        .parent()
        .context("canonical manifest has no parent")?
        .join(&manifest.artifact_path);
    let artifact_sha256 = sha256_file(&artifact)?;
    if artifact_sha256 != manifest.artifact_sha256 {
        bail!(
            "canonical Parquet SHA-256 mismatch: expected {}, actual {artifact_sha256}",
            manifest.artifact_sha256
        );
    }
    let rows = read_replay_rows(&artifact, &manifest)?;
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
    });
    let mut plan = Plan {
        lineage,
        rows: Vec::with_capacity(rows.len() + 1),
    };
    plan.rows
        .push(registry_row(&plan.lineage, REPLAY_KIND, rows.len()));
    for (timestamp_us, sequence, event, payload_json) in rows {
        plan.rows.push((
            "cex_replay_events".to_string(),
            row_with_lineage(
                &plan.lineage,
                json!({
                    "event_time_us": timestamp_us,
                    "sequence": sequence,
                    "event": event,
                    "payload_json": payload_json,
                }),
            ),
        ));
    }
    Ok(plan)
}

fn pit_plan(
    path: &Path,
    venue_override: Option<&str>,
    market_override: Option<&str>,
) -> Result<Plan> {
    let (manifest_path, manifest_bytes, manifest_sha256) = read_manifest(path)?;
    let mut manifest: FeatureDatasetManifest =
        serde_json::from_slice(&manifest_bytes).context("cannot parse PIT feature manifest")?;
    if manifest.dataset_kind != PIT_KIND || manifest.schema_version != PIT_SCHEMA {
        bail!("PIT feature manifest schema is unsupported");
    }
    if manifest.artifact_path.is_relative()
        && manifest
            .artifact_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("PIT feature artifact path escapes its manifest directory");
    }
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

fn result_plan(path: &Path) -> Result<Plan> {
    let (manifest_path, manifest_bytes, manifest_sha256) = read_manifest(path)?;
    let canonical_name = format!("{manifest_sha256}.result-manifest.json");
    if manifest_path.file_name().and_then(|name| name.to_str()) != Some(canonical_name.as_str()) {
        bail!("backtest result manifest path is not content addressed");
    }
    let manifest: ResultManifest =
        serde_json::from_slice(&manifest_bytes).context("cannot parse backtest result manifest")?;
    if manifest.dataset_kind != RESULT_KIND
        || manifest.schema_version != RESULT_SCHEMA
        || manifest.format != "json"
        || manifest.rows != 1
        || manifest.venue.trim().is_empty()
        || !matches!(manifest.market.as_str(), "spot" | "usdm")
        || manifest.symbol.trim().is_empty()
        || manifest.source_revision.trim().is_empty()
        || manifest.first_event_time_us > manifest.last_event_time_us
        || manifest.artifact_path.is_absolute()
        || manifest
            .artifact_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("backtest result manifest is incomplete or unsupported");
    }
    valid_sha256(&manifest.source_revision, "result source revision")?;
    valid_sha256(&manifest.artifact_sha256, "result artifact sha256")?;
    let artifact = manifest_path
        .parent()
        .context("backtest result manifest has no parent")?
        .join(&manifest.artifact_path);
    let artifact_sha256 = sha256_file(&artifact)?;
    if artifact_sha256 != manifest.artifact_sha256 {
        bail!(
            "backtest result artifact SHA-256 mismatch: expected {}, actual {artifact_sha256}",
            manifest.artifact_sha256
        );
    }
    let artifact_json: Value = serde_json::from_slice(&fs::read(&artifact)?)
        .context("backtest result artifact is not JSON")?;
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
            json!({"result_json": serde_json::to_string(&artifact_json)?}),
        ),
    ));
    Ok(plan)
}

fn validate_replay_manifest(manifest: &CanonicalManifest) -> Result<()> {
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
        || manifest.artifact_path.is_absolute()
        || manifest
            .artifact_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
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

fn read_replay_rows(
    artifact: &Path,
    manifest: &CanonicalManifest,
) -> Result<Vec<(i64, u64, String, String)>> {
    let reader = SerializedFileReader::new(File::open(artifact)?)?;
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
    let mut rows = Vec::with_capacity(manifest.rows);
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
            || (rows.is_empty() && event != "snapshot")
        {
            bail!("canonical Parquet rows are not valid replay events");
        }
        validate_replay_payload(&payload)?;
        expected_sequence = expected_sequence
            .checked_add(1)
            .context("canonical sequence overflow")?;
        previous_timestamp = Some(timestamp_us);
        rows.push((timestamp_us, sequence, event, payload_json));
    }
    if rows.len() != manifest.rows
        || rows.first().map(|row| row.0) != Some(manifest.first_event_time_us)
        || rows.last().map(|row| row.0) != Some(manifest.last_event_time_us)
        || expected_sequence
            != manifest
                .sequence_end
                .checked_add(1)
                .context("canonical sequence overflow")?
    {
        bail!("canonical Parquet coverage does not match its manifest");
    }
    Ok(rows)
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
    } = input;
    let partition_identity =
        format!("{schema_version}:{venue}:{market}:{symbol}:{start_time_us}:{end_time_us}");
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

fn encode_plan(plan: &Plan) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    for (table, payload) in &plan.rows {
        let mut row = payload.clone();
        let object = row
            .as_object_mut()
            .context("analytics row is not an object")?;
        object.insert("table".to_string(), table.clone().into());
        object.insert(
            "sql".to_string(),
            format!("INSERT INTO {table} FORMAT JSONEachRow").into(),
        );
        serde_json::to_writer(&mut output, &row)?;
        output.push(b'\n');
    }
    Ok(output)
}

fn read_manifest(path: &Path) -> Result<(PathBuf, Vec<u8>, String)> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("cannot resolve manifest {}", path.display()))?;
    let bytes =
        fs::read(&path).with_context(|| format!("cannot read manifest {}", path.display()))?;
    let hash = sha256_bytes(&bytes);
    valid_sha256(&hash, "manifest sha256")?;
    Ok((path, bytes, hash))
}

fn publish_immutable(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("immutable analytics plan path is not a regular file");
        }
        if fs::read(path)? != bytes {
            bail!("immutable analytics plan already exists with different content");
        }
        return Ok(());
    }
    let temporary =
        path.with_file_name(format!(".{}.tmp-{}", file_name(path)?, std::process::id()));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    drop(output);
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(_error) if path.exists() && fs::read(path)? == bytes => {
            let _ = fs::remove_file(&temporary);
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error.into())
        }
    }
}

async fn send_to_clickhouse(
    url: &str,
    database: &str,
    user: &str,
    password: &str,
    plan: &Plan,
) -> Result<String> {
    let database = sql_identifier(database)?;
    let client = reqwest::Client::new();
    let query_url = format!("{}/?query=", url.trim_end_matches('/'));
    let query = identity_query(database, &plan.lineage.partition_identity);
    let response = client
        .post(format!("{query_url}{}", urlencoding::encode(&query)))
        .basic_auth(user, (!password.is_empty()).then_some(password))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "ClickHouse identity query failed with {}",
            response.status()
        );
    }
    let existing = response.text().await?;
    for row in existing
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        let Some(existing_hash) = row
            .get("manifest_sha256")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        if existing_hash != plan.lineage.manifest_sha256 {
            bail!("ClickHouse partition identity conflicts with existing manifest");
        }
        match row.get("materialization_state").and_then(Value::as_str) {
            Some("complete") => return Ok("idempotent-hit".to_string()),
            Some("pending") => {
                bail!("ClickHouse partition identity has an incomplete materialization")
            }
            _ => bail!("ClickHouse partition registry state is unsupported"),
        }
    }

    let mut pending = plan.rows[0].1.clone();
    pending["materialization_state"] = "pending".into();
    pending["materialization_version"] = 1.into();
    insert_rows(
        &client,
        &query_url,
        database,
        user,
        password,
        "cex_analytics_partitions",
        serde_json::to_vec(&pending)?,
    )
    .await?;

    let mut grouped: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    for (table, payload) in plan.rows.iter().skip(1) {
        let bytes = grouped.entry(table.as_str()).or_default();
        serde_json::to_writer(&mut *bytes, payload)?;
        bytes.push(b'\n');
    }
    for (table, bytes) in grouped {
        insert_rows(&client, &query_url, database, user, password, table, bytes).await?;
    }
    insert_rows(
        &client,
        &query_url,
        database,
        user,
        password,
        "cex_analytics_partitions",
        serde_json::to_vec(&plan.rows[0].1)?,
    )
    .await?;
    Ok("inserted".to_string())
}

async fn insert_rows(
    client: &reqwest::Client,
    query_url: &str,
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
        .post(format!("{query_url}{}", urlencoding::encode(&query)))
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

fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn identity_query(database: &str, partition_identity: &str) -> String {
    format!(
        "SELECT manifest_sha256, materialization_state, materialization_version FROM {database}.cex_analytics_partitions FINAL WHERE partition_identity = {} LIMIT 1 FORMAT JSONEachRow",
        sql_literal(partition_identity)
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

fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .context("path has no UTF-8 file name")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        });
        let mut b = a.clone();
        b.manifest_sha256 = "d".repeat(64);
        assert_eq!(a.partition_identity, b.partition_identity);
        assert_ne!(a.manifest_sha256, b.manifest_sha256);
    }

    #[test]
    fn sql_literals_escape_identity_delimiters() {
        assert_eq!(sql_literal("a'b"), "'a''b'");
        assert!(sql_identifier("hft_db").is_ok());
        assert!(sql_identifier("hft-db").is_err());
        let query = identity_query("hft_db", "binance:spot:BTCUSDT:1:2");
        assert!(query.contains("partition_identity = 'binance:spot:BTCUSDT:1:2'"));
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
