use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
#[cfg(feature = "db")]
use std::process::Command;
#[cfg(feature = "db")]
use std::time::Instant;

use anyhow::{Context, Result};
#[cfg(feature = "db")]
use chrono::Timelike;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{DeribitFeatureSnapshot, FactorObservation, ResearchPmBookSnapshot};

pub const RESEARCH_SNAPSHOT_SCHEMA_VERSION: &str = "research_snapshot_v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotArtifacts {
    pub observations_json: String,
    pub deribit_snapshots_json: String,
    pub pm_book_snapshots_json: String,
    pub quality_markdown: String,
    pub query_timings_json: String,
    pub observations_parquet: Option<String>,
}

impl Default for ResearchSnapshotArtifacts {
    fn default() -> Self {
        Self {
            observations_json: "observations.json".to_string(),
            deribit_snapshots_json: "deribit_snapshots.json".to_string(),
            pm_book_snapshots_json: "pm_book_snapshots.json".to_string(),
            quality_markdown: "quality.md".to_string(),
            query_timings_json: "query_timings.json".to_string(),
            observations_parquet: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResearchSnapshotRowCounts {
    pub observations: usize,
    pub deribit_snapshots: usize,
    pub pm_book_snapshots: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotSourceSurface {
    pub name: String,
    pub role: String,
    #[serde(default = "default_source_surface_gate_category")]
    pub gate_category: String,
    pub raw_full_fidelity: bool,
    pub snapshot_sampled: bool,
    pub sample_secs: Option<i64>,
    pub row_count: Option<usize>,
    pub notes: String,
}

fn default_source_surface_gate_category() -> String {
    "optional_context".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotInputArtifact {
    pub name: String,
    pub path: String,
    pub content_hash: Option<String>,
    pub row_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotPhaseTiming {
    pub phase: String,
    pub elapsed_ms: u128,
    pub rows: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResearchSnapshotPmBookSource {
    pub hot_postgres_sampled_rows: usize,
    pub archive_sampled_rows: usize,
    pub archive_manifest_rows: usize,
    pub archive_files: usize,
    #[serde(default)]
    pub archive_token_windows: usize,
    pub merged_sampled_rows: usize,
    pub archive_dir: Option<String>,
    pub archive_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotManifest {
    pub schema_version: String,
    pub snapshot_hash: Option<String>,
    #[serde(default)]
    pub snapshot_contract_hash: Option<String>,
    pub generated_at: DateTime<Utc>,
    pub git_sha: Option<String>,
    pub symbols: Vec<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub history_start: DateTime<Utc>,
    pub lob_sample_secs: i32,
    #[serde(default)]
    pub pm_book_sample_secs: Option<i32>,
    pub observation_sample_secs: i64,
    pub max_quote_age_secs: i64,
    pub stake_usd: f64,
    pub require_official_settlement: bool,
    pub immutable_input: bool,
    pub source_kind: String,
    pub optimizer_data_dir: Option<String>,
    #[serde(default)]
    pub source_surfaces: Vec<ResearchSnapshotSourceSurface>,
    #[serde(default)]
    pub input_artifacts: Vec<ResearchSnapshotInputArtifact>,
    #[serde(default)]
    pub data_requirements: Vec<String>,
    #[serde(default)]
    pub data_audit_status: Option<String>,
    #[serde(default)]
    pub data_audit_report: Option<String>,
    #[serde(default = "default_include_deribit")]
    pub include_deribit: bool,
    pub artifacts: ResearchSnapshotArtifacts,
    pub row_counts: ResearchSnapshotRowCounts,
    pub phase_timings: Vec<ResearchSnapshotPhaseTiming>,
    pub quality_flags: Vec<String>,
    #[serde(default)]
    pub pm_book_source: ResearchSnapshotPmBookSource,
}

fn default_include_deribit() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct ResearchSnapshot {
    pub manifest: ResearchSnapshotManifest,
    pub observations: Vec<FactorObservation>,
    pub deribit_snapshots: Vec<DeribitFeatureSnapshot>,
    pub pm_book_snapshots: Vec<ResearchPmBookSnapshot>,
}

#[derive(Debug, Clone, Copy)]
pub struct ResearchSnapshotRequest<'a> {
    pub symbols: &'a [String],
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub lob_sample_secs: i32,
    pub pm_book_sample_secs: i32,
    pub observation_sample_secs: i64,
    pub max_quote_age_secs: i64,
    pub stake_usd: f64,
    pub require_official_settlement: bool,
}

pub fn load_research_snapshot(snapshot_dir: impl AsRef<Path>) -> Result<ResearchSnapshot> {
    let snapshot_dir = snapshot_dir.as_ref();
    let manifest: ResearchSnapshotManifest =
        read_json(snapshot_dir.join("manifest.json")).context("read research snapshot manifest")?;

    if manifest.schema_version != RESEARCH_SNAPSHOT_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported research snapshot schema {}; expected {}",
            manifest.schema_version,
            RESEARCH_SNAPSHOT_SCHEMA_VERSION
        );
    }
    if let Some(recorded_contract_hash) = manifest.snapshot_contract_hash.as_deref() {
        let contract_hex = recorded_contract_hash
            .strip_prefix("sha256:")
            .filter(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .context("research snapshot contract hash must use sha256:<64hex>")?;
        debug_assert_eq!(contract_hex.len(), 64);
        let computed_contract_hash = compute_snapshot_contract_hash(snapshot_dir, &manifest)
            .context("verify research snapshot evaluator contract hash")?;
        if recorded_contract_hash != computed_contract_hash {
            anyhow::bail!(
                "research snapshot evaluator contract hash mismatch: manifest={} computed={}",
                recorded_contract_hash,
                computed_contract_hash
            );
        }
    }
    let recorded_hash = manifest
        .snapshot_hash
        .as_deref()
        .filter(|hash| !hash.is_empty())
        .context("research snapshot manifest is missing snapshot_hash")?;
    let computed_hash = compute_snapshot_hash(snapshot_dir, &manifest)
        .context("verify research snapshot content hash")?;
    if recorded_hash != computed_hash {
        anyhow::bail!(
            "research snapshot content hash mismatch: manifest={} computed={}",
            recorded_hash,
            computed_hash
        );
    }

    let observations = read_json(snapshot_dir.join(&manifest.artifacts.observations_json))
        .context("read snapshot observations")?;
    let deribit_snapshots =
        read_json(snapshot_dir.join(&manifest.artifacts.deribit_snapshots_json))
            .context("read snapshot Deribit rows")?;
    let pm_book_snapshots =
        read_json(snapshot_dir.join(&manifest.artifacts.pm_book_snapshots_json))
            .context("read snapshot PM book rows")?;

    Ok(ResearchSnapshot {
        manifest,
        observations,
        deribit_snapshots,
        pm_book_snapshots,
    })
}

pub fn write_research_snapshot(
    snapshot_dir: impl AsRef<Path>,
    mut snapshot: ResearchSnapshot,
) -> Result<ResearchSnapshotManifest> {
    if snapshot.manifest.optimizer_data_dir.is_none() {
        anyhow::bail!("research snapshot manifest requires optimizer_data_dir");
    }

    let snapshot_dir = snapshot_dir.as_ref();
    fs::create_dir_all(snapshot_dir)
        .with_context(|| format!("create snapshot dir {}", snapshot_dir.display()))?;

    snapshot.manifest.row_counts = ResearchSnapshotRowCounts {
        observations: snapshot.observations.len(),
        deribit_snapshots: snapshot.deribit_snapshots.len(),
        pm_book_snapshots: snapshot.pm_book_snapshots.len(),
    };

    write_json(
        snapshot_dir.join(&snapshot.manifest.artifacts.observations_json),
        &snapshot.observations,
    )?;
    write_json(
        snapshot_dir.join(&snapshot.manifest.artifacts.deribit_snapshots_json),
        &snapshot.deribit_snapshots,
    )?;
    write_json(
        snapshot_dir.join(&snapshot.manifest.artifacts.pm_book_snapshots_json),
        &snapshot.pm_book_snapshots,
    )?;

    #[cfg(feature = "polars-export")]
    {
        let parquet_name = "observations.parquet";
        crate::export_observations_parquet(
            &snapshot.observations,
            &snapshot_dir.join(parquet_name),
        )
        .context("write snapshot observations parquet")?;
        snapshot.manifest.artifacts.observations_parquet = Some(parquet_name.to_string());
    }

    snapshot.manifest.snapshot_hash =
        Some(compute_snapshot_hash(snapshot_dir, &snapshot.manifest)?);
    snapshot.manifest.snapshot_contract_hash = Some(compute_snapshot_contract_hash(
        snapshot_dir,
        &snapshot.manifest,
    )?);

    write_json(
        snapshot_dir.join(&snapshot.manifest.artifacts.query_timings_json),
        &snapshot.manifest.phase_timings,
    )?;
    write_quality_markdown(
        &snapshot_dir.join(&snapshot.manifest.artifacts.quality_markdown),
        &snapshot.manifest,
    )?;
    write_json(snapshot_dir.join("manifest.json"), &snapshot.manifest)?;

    Ok(snapshot.manifest)
}

pub fn validate_snapshot_request(
    manifest: &ResearchSnapshotManifest,
    request: ResearchSnapshotRequest<'_>,
) -> Result<()> {
    validate_snapshot_request_with_window_mode(manifest, request, true)
}

pub fn validate_snapshot_request_coverage(
    manifest: &ResearchSnapshotManifest,
    request: ResearchSnapshotRequest<'_>,
) -> Result<()> {
    validate_snapshot_request_with_window_mode(manifest, request, false)
}

fn validate_snapshot_request_with_window_mode(
    manifest: &ResearchSnapshotManifest,
    request: ResearchSnapshotRequest<'_>,
    require_exact_window: bool,
) -> Result<()> {
    let mut requested_symbols = request.symbols.to_vec();
    requested_symbols.sort();
    let mut snapshot_symbols = manifest.symbols.clone();
    snapshot_symbols.sort();
    if requested_symbols != snapshot_symbols {
        anyhow::bail!(
            "snapshot symbols {:?} do not match requested symbols {:?}",
            snapshot_symbols,
            requested_symbols
        );
    }
    if require_exact_window && (manifest.start != request.start || manifest.end != request.end) {
        anyhow::bail!(
            "snapshot window {} -> {} does not match requested window {} -> {}",
            manifest.start,
            manifest.end,
            request.start,
            request.end
        );
    }
    if !require_exact_window && (manifest.start > request.start || manifest.end < request.end) {
        anyhow::bail!(
            "snapshot window {} -> {} does not cover requested window {} -> {}",
            manifest.start,
            manifest.end,
            request.start,
            request.end
        );
    }
    if manifest.lob_sample_secs != request.lob_sample_secs {
        anyhow::bail!(
            "snapshot lob_sample_secs {} does not match requested {}",
            manifest.lob_sample_secs,
            request.lob_sample_secs
        );
    }
    let manifest_pm_book_sample_secs = manifest
        .pm_book_sample_secs
        .unwrap_or(manifest.lob_sample_secs)
        .max(1);
    let requested_pm_book_sample_secs = request.pm_book_sample_secs.max(1);
    if manifest_pm_book_sample_secs != requested_pm_book_sample_secs {
        anyhow::bail!(
            "snapshot pm_book_sample_secs {} does not match requested {}",
            manifest_pm_book_sample_secs,
            requested_pm_book_sample_secs
        );
    }
    if i64::from(manifest_pm_book_sample_secs) > request.max_quote_age_secs.max(1) {
        anyhow::bail!(
            "snapshot pm_book_sample_secs {} is coarser than requested max_quote_age_secs {}; full-depth execution claims require PM book cadence no coarser than quote-age gate",
            manifest_pm_book_sample_secs,
            request.max_quote_age_secs
        );
    }
    if manifest.observation_sample_secs != request.observation_sample_secs {
        anyhow::bail!(
            "snapshot observation_sample_secs {} does not match requested {}",
            manifest.observation_sample_secs,
            request.observation_sample_secs
        );
    }
    if manifest.max_quote_age_secs != request.max_quote_age_secs {
        anyhow::bail!(
            "snapshot max_quote_age_secs {} does not match requested {}",
            manifest.max_quote_age_secs,
            request.max_quote_age_secs
        );
    }
    if (manifest.stake_usd - request.stake_usd).abs() > 1e-9 {
        anyhow::bail!(
            "snapshot stake_usd {} does not match requested {}",
            manifest.stake_usd,
            request.stake_usd
        );
    }
    if manifest.require_official_settlement != request.require_official_settlement {
        anyhow::bail!(
            "snapshot require_official_settlement {} does not match requested {}",
            manifest.require_official_settlement,
            request.require_official_settlement
        );
    }
    if !manifest.immutable_input {
        anyhow::bail!("snapshot manifest is not marked immutable_input=true");
    }
    if manifest
        .snapshot_hash
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        anyhow::bail!("snapshot manifest is missing snapshot_hash");
    }
    Ok(())
}

#[cfg(feature = "db")]
pub struct ResearchSnapshotBuildOptions {
    pub symbols: Vec<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub lob_sample_secs: i32,
    pub pm_book_sample_secs: i32,
    pub observation_sample_secs: i64,
    pub max_quote_age_secs: i64,
    pub stake_usd: f64,
    pub require_official_settlement: bool,
    pub optimizer_data_dir: Option<String>,
    pub git_sha: Option<String>,
    pub data_requirements: Vec<String>,
    pub data_audit_status: Option<String>,
    pub data_audit_report: Option<String>,
    pub include_deribit: bool,
    pub pm_book_archive_dir: Option<PathBuf>,
}

fn research_snapshot_quality_flags(
    observation_count: usize,
    deribit_snapshot_count: usize,
    pm_book_snapshot_count: usize,
    include_deribit: bool,
    pm_book_sample_secs: i32,
    max_quote_age_secs: i64,
    pm_book_source: &ResearchSnapshotPmBookSource,
) -> Vec<String> {
    let mut quality_flags = Vec::new();
    if observation_count == 0 {
        quality_flags.push("no_factor_observations".to_string());
    }
    if include_deribit && deribit_snapshot_count == 0 {
        quality_flags.push("no_deribit_snapshots".to_string());
    }
    if pm_book_snapshot_count == 0 {
        quality_flags.push("no_pm_book_snapshots".to_string());
    }
    if i64::from(pm_book_sample_secs.max(1)) > max_quote_age_secs.max(1) {
        quality_flags.push(format!(
            "pm_book_sample_secs_gt_max_quote_age:{pm_book_sample_secs}>{max_quote_age_secs}"
        ));
    }
    if pm_book_source.archive_status == "archive_configured_no_candidate_files" {
        quality_flags.push("pm_book_archive_configured_no_candidate_files".to_string());
    }
    if pm_book_source.archive_status == "archive_configured_no_token_windows" {
        quality_flags.push("pm_book_archive_configured_no_token_windows".to_string());
    }
    if pm_book_source.archive_manifest_rows > 0 && pm_book_source.archive_sampled_rows == 0 {
        quality_flags.push("pm_book_archive_manifest_rows_but_no_sampled_rows".to_string());
    }
    quality_flags
}

#[cfg(feature = "db")]
#[derive(Debug)]
struct ArchivedPmBookLoad {
    snapshots: Vec<ResearchPmBookSnapshot>,
    manifest_rows: usize,
    files: usize,
    token_windows: usize,
    status: String,
}

#[cfg(feature = "db")]
const MAX_ARCHIVED_PM_BOOK_SAMPLED_ROWS: usize = 250_000;

#[cfg(feature = "db")]
#[derive(Debug, Deserialize)]
struct ArchiveManifest {
    #[serde(default)]
    row_count: usize,
}

#[cfg(feature = "db")]
#[derive(Debug, Deserialize)]
struct ArchivePmBookRow {
    event_id: Option<String>,
    token_id: String,
    side: Option<String>,
    received_at: String,
    bids: String,
    asks: String,
}

#[cfg(feature = "db")]
#[derive(Debug, Clone)]
struct PmBookTokenWindow {
    market_slug: String,
    token_id: String,
    side: String,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
}

#[cfg(feature = "db")]
fn parse_duckdb_timestamptz(raw: &str) -> Result<DateTime<Utc>> {
    let compact_hour_offset = raw
        .len()
        .checked_sub(3)
        .and_then(|idx| raw.get(idx..))
        .filter(|suffix| {
            let bytes = suffix.as_bytes();
            matches!(bytes.first(), Some(b'+') | Some(b'-'))
                && bytes.get(1).is_some_and(u8::is_ascii_digit)
                && bytes.get(2).is_some_and(u8::is_ascii_digit)
        })
        .map(|_| format!("{raw}:00"));
    DateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f%:z")
        .or_else(|_| DateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f%z"))
        .or_else(|_| {
            compact_hour_offset
                .as_deref()
                .map(|normalized| DateTime::parse_from_str(normalized, "%Y-%m-%d %H:%M:%S%.f%:z"))
                .unwrap_or_else(|| DateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f%:z"))
        })
        .or_else(|_| DateTime::parse_from_rfc3339(raw))
        .map(|ts| ts.with_timezone(&Utc))
        .with_context(|| format!("parse duckdb timestamp {raw:?}"))
}

#[cfg(feature = "db")]
fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(feature = "db")]
fn archive_hour_dirs(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    if end <= start {
        return out;
    }
    let local_start = start + chrono::Duration::hours(8);
    let local_end = end + chrono::Duration::hours(8) - chrono::Duration::microseconds(1);
    let mut hour = local_start
        .date_naive()
        .and_hms_opt(local_start.hour(), 0, 0)
        .expect("valid local archive start hour");
    let end_hour = local_end
        .date_naive()
        .and_hms_opt(local_end.hour(), 0, 0)
        .expect("valid local archive end hour");
    while hour <= end_hour {
        out.push((hour.date().format("%Y-%m-%d").to_string(), hour.hour()));
        hour += chrono::Duration::hours(1);
    }
    out
}

#[cfg(feature = "db")]
fn candidate_archive_files(
    archive_dir: &Path,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<(Vec<PathBuf>, usize)> {
    let mut files = Vec::new();
    let mut manifest_rows = 0usize;
    for (date, hour) in archive_hour_dirs(start, end) {
        let hour_dir = archive_dir.join(format!("date={date}/hour={hour:02}"));
        let parquet = hour_dir.join("snapshots.parquet");
        if !parquet.exists() {
            continue;
        }
        let manifest = hour_dir.join("manifest.json");
        if manifest.exists() {
            let parsed: ArchiveManifest = read_json(manifest)?;
            manifest_rows = manifest_rows.saturating_add(parsed.row_count);
        }
        files.push(parquet);
    }
    Ok((files, manifest_rows))
}

#[cfg(feature = "db")]
async fn load_pm_book_token_windows(
    pool: &sqlx::PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_every_secs: i32,
) -> Result<Vec<PmBookTokenWindow>, sqlx::Error> {
    sqlx::query_as::<_, (String, String, String, DateTime<Utc>, DateTime<Utc>)>(
        r#"
        SELECT DISTINCT
            m.market_slug,
            trim(both '"' from token.value::text) AS token_id,
            CASE token.ordinality WHEN 1 THEN 'UP' ELSE 'DOWN' END AS side,
            GREATEST(
                $2::timestamptz,
                COALESCE(m.start_time, $2::timestamptz)
                    - make_interval(secs => $4::int)
            ) AS window_start,
            LEAST(
                $3::timestamptz,
                COALESCE(m.end_time, $3::timestamptz)
                    + make_interval(secs => $4::int)
            ) AS window_end
        FROM pm_market_metadata m
        CROSS JOIN LATERAL jsonb_array_elements(
            (m.raw_market->'markets'->0->>'clobTokenIds')::jsonb
        ) WITH ORDINALITY AS token(value, ordinality)
        WHERE m.symbol = ANY($1)
          AND m.end_time >= $2
          AND m.start_time <= $3
          AND m.raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        ORDER BY market_slug, side
        "#,
    )
    .bind(symbols)
    .bind(start)
    .bind(end)
    .bind(sample_every_secs.max(1))
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(
                |(market_slug, token_id, side, window_start, window_end)| PmBookTokenWindow {
                    market_slug,
                    token_id,
                    side,
                    window_start,
                    window_end,
                },
            )
            .collect()
    })
}

#[cfg(feature = "db")]
fn archive_token_window_values_sql(windows: &[PmBookTokenWindow]) -> String {
    windows
        .iter()
        .map(|window| {
            format!(
                "({}, {}, {}, TIMESTAMPTZ {}, TIMESTAMPTZ {})",
                sql_string_literal(&window.market_slug),
                sql_string_literal(&window.token_id),
                sql_string_literal(&window.side),
                sql_string_literal(&window.window_start.to_rfc3339()),
                sql_string_literal(&window.window_end.to_rfc3339())
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(feature = "db")]
fn load_archived_pm_book_snapshots_sampled(
    archive_dir: &Path,
    token_windows: &[PmBookTokenWindow],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_every_secs: i32,
) -> Result<ArchivedPmBookLoad> {
    let (files, manifest_rows) = candidate_archive_files(archive_dir, start, end)?;
    if files.is_empty() {
        return Ok(ArchivedPmBookLoad {
            snapshots: Vec::new(),
            manifest_rows,
            files: 0,
            token_windows: token_windows.len(),
            status: "archive_configured_no_candidate_files".to_string(),
        });
    }
    if token_windows.is_empty() {
        return Ok(ArchivedPmBookLoad {
            snapshots: Vec::new(),
            manifest_rows,
            files: files.len(),
            token_windows: 0,
            status: "archive_configured_no_token_windows".to_string(),
        });
    }

    let token_window_values = archive_token_window_values_sql(token_windows);
    let sample_every_secs = sample_every_secs.max(1);
    let start_literal = sql_string_literal(&start.to_rfc3339());
    let end_literal = sql_string_literal(&end.to_rfc3339());
    let duckdb_temp_dir_literal = sql_string_literal(
        &std::env::temp_dir()
            .join("ploy-duckdb")
            .display()
            .to_string(),
    );
    let mut rows = Vec::new();

    for file in &files {
        let row_limit = MAX_ARCHIVED_PM_BOOK_SAMPLED_ROWS
            .saturating_sub(rows.len())
            .saturating_add(1);
        let file_literal = sql_string_literal(&file.display().to_string());
        let sql = format!(
            r#"
SET threads = 1;
SET memory_limit = '1024MB';
SET temp_directory = {duckdb_temp_dir_literal};
WITH token_map(event_id, token_id, side, window_start, window_end) AS (
  VALUES {token_window_values}
),
raw_keys AS (
  SELECT
    t.event_id,
    t.token_id,
    t.side,
    o.received_at,
    floor(epoch(o.received_at) / {sample_every_secs}) AS bucket
  FROM read_parquet({file_literal}) o
  JOIN token_map t
    ON o.token_id = t.token_id
   AND o.received_at >= t.window_start
   AND o.received_at < t.window_end
  WHERE o.received_at >= TIMESTAMPTZ {start_literal}
    AND o.received_at < TIMESTAMPTZ {end_literal}
),
ranked AS (
  SELECT
    event_id,
    token_id,
    side,
    received_at,
    row_number() OVER (
      PARTITION BY token_id, bucket
      ORDER BY received_at DESC
    ) AS rn
  FROM raw_keys
)
SELECT
  r.event_id,
  r.token_id,
  r.side,
  r.received_at,
  o.bids,
  o.asks
FROM ranked r
JOIN read_parquet({file_literal}) o
  ON o.token_id = r.token_id
 AND o.received_at = r.received_at
WHERE r.rn = 1
ORDER BY r.received_at
LIMIT {row_limit}
"#
        );
        rows.extend(run_duckdb_archive_pm_book_query(sql)?);
        if rows.len() > MAX_ARCHIVED_PM_BOOK_SAMPLED_ROWS {
            anyhow::bail!(
                "archived PM book sampled rows exceeded safety limit {} for {} token windows; narrow the window or raise pm_book_sample_secs",
                MAX_ARCHIVED_PM_BOOK_SAMPLED_ROWS,
                token_windows.len()
            );
        }
    }

    let snapshots = rows
        .into_iter()
        .map(|row| {
            let ts = parse_duckdb_timestamptz(&row.received_at)?;
            let bids: serde_json::Value = serde_json::from_str(&row.bids)
                .with_context(|| format!("parse archived bids JSON for {}", row.token_id))?;
            let asks: serde_json::Value = serde_json::from_str(&row.asks)
                .with_context(|| format!("parse archived asks JSON for {}", row.token_id))?;
            Ok(ResearchPmBookSnapshot {
                event_id: row.event_id.unwrap_or_default(),
                token_id: row.token_id,
                side: row.side.unwrap_or_default(),
                ts,
                bids: crate::factors::research_pm_book_levels_from_json(&bids, true),
                asks: crate::factors::research_pm_book_levels_from_json(&asks, false),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ArchivedPmBookLoad {
        snapshots,
        manifest_rows,
        files: files.len(),
        token_windows: token_windows.len(),
        status: "archive_loaded".to_string(),
    })
}

#[cfg(feature = "db")]
fn run_duckdb_archive_pm_book_query(sql: String) -> Result<Vec<ArchivePmBookRow>> {
    let duckdb_temp_dir = std::env::temp_dir().join("ploy-duckdb");
    fs::create_dir_all(&duckdb_temp_dir)
        .with_context(|| format!("create DuckDB temp dir {}", duckdb_temp_dir.display()))?;
    let sql_path = duckdb_temp_dir.join(format!(
        "archive-pm-books-{}-{}.sql",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    fs::write(&sql_path, sql)
        .with_context(|| format!("write DuckDB archive query {}", sql_path.display()))?;
    let sql_file = File::open(&sql_path)
        .with_context(|| format!("open DuckDB archive query {}", sql_path.display()))?;
    let output = Command::new("duckdb")
        .arg("-json")
        .stdin(sql_file)
        .output()
        .context("run duckdb for archived PM book snapshots")?;
    let _ = fs::remove_file(&sql_path);
    if !output.status.success() {
        anyhow::bail!(
            "duckdb archived PM book load failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("parse duckdb archived PM book JSON rows")
}

#[cfg(feature = "db")]
fn merge_pm_book_snapshots(
    mut hot_rows: Vec<ResearchPmBookSnapshot>,
    archive_rows: Vec<ResearchPmBookSnapshot>,
) -> Vec<ResearchPmBookSnapshot> {
    hot_rows.extend(archive_rows);
    hot_rows.sort_by(|a, b| {
        (a.ts, &a.event_id, &a.token_id, &a.side).cmp(&(b.ts, &b.event_id, &b.token_id, &b.side))
    });
    hot_rows.dedup_by(|a, b| {
        a.ts == b.ts && a.event_id == b.event_id && a.token_id == b.token_id && a.side == b.side
    });
    hot_rows
}

#[cfg(feature = "db")]
pub async fn build_research_snapshot_from_database(
    pool: &sqlx::PgPool,
    options: ResearchSnapshotBuildOptions,
) -> Result<ResearchSnapshot> {
    use ploy_feed_loaders::{load_from_database_with_options, HistoricalLoadOptions};
    use ploy_market_contracts::MarketUpdate;

    use crate::{
        build_factor_observations_with_lob_sampled, load_deribit_feature_snapshots_with_timings,
        load_research_lob_snapshots_sampled, load_research_pm_book_snapshots_sampled,
    };

    let mut phase_timings = Vec::new();
    let history_start = options.start - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
    let historical_sample_secs = u32::try_from(options.lob_sample_secs.max(1)).unwrap_or(1);

    let started = Instant::now();
    let all_updates = load_from_database_with_options(
        pool,
        &options.symbols,
        history_start,
        options.end,
        &HistoricalLoadOptions {
            require_official_settlement: options.require_official_settlement,
            include_l2: false,
            spot_sample_secs: historical_sample_secs,
            lob_sample_secs: historical_sample_secs,
            ..Default::default()
        },
    )
    .await
    .context("load historical market updates")?;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "historical_updates".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(all_updates.len()),
    });

    let started = Instant::now();
    let all_lob_snapshots = load_research_lob_snapshots_sampled(
        pool,
        &options.symbols,
        history_start,
        options.end,
        options.lob_sample_secs,
    )
    .await
    .context("load CEX LOB snapshots")?;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "cex_lob_snapshots".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(all_lob_snapshots.len()),
    });

    let started = Instant::now();
    let pm_book_sample_secs = options.pm_book_sample_secs.max(1);
    let hot_pm_book_snapshots = load_research_pm_book_snapshots_sampled(
        pool,
        &options.symbols,
        history_start,
        options.end,
        pm_book_sample_secs,
    )
    .await
    .context("load PM book snapshots")?;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "pm_book_snapshots_hot_postgres".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(hot_pm_book_snapshots.len()),
    });

    let mut pm_book_source = ResearchSnapshotPmBookSource {
        hot_postgres_sampled_rows: hot_pm_book_snapshots.len(),
        archive_dir: options
            .pm_book_archive_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        archive_status: "archive_not_configured".to_string(),
        ..Default::default()
    };
    let archived_pm_book_snapshots =
        if let Some(archive_dir) = options.pm_book_archive_dir.as_deref() {
            let started = Instant::now();
            let token_windows = load_pm_book_token_windows(
                pool,
                &options.symbols,
                history_start,
                options.end,
                pm_book_sample_secs,
            )
            .await
            .context("load PM book token windows for archive snapshot")?;
            pm_book_source.archive_token_windows = token_windows.len();
            phase_timings.push(ResearchSnapshotPhaseTiming {
                phase: "pm_book_archive_token_windows".to_string(),
                elapsed_ms: started.elapsed().as_millis(),
                rows: Some(token_windows.len()),
            });

            let started = Instant::now();
            let archived = load_archived_pm_book_snapshots_sampled(
                archive_dir,
                &token_windows,
                history_start,
                options.end,
                pm_book_sample_secs,
            )
            .with_context(|| {
                format!(
                    "load archived PM book snapshots from {}",
                    archive_dir.display()
                )
            })?;
            pm_book_source.archive_sampled_rows = archived.snapshots.len();
            pm_book_source.archive_manifest_rows = archived.manifest_rows;
            pm_book_source.archive_files = archived.files;
            pm_book_source.archive_token_windows = archived.token_windows;
            pm_book_source.archive_status = archived.status;
            phase_timings.push(ResearchSnapshotPhaseTiming {
                phase: "pm_book_snapshots_archive".to_string(),
                elapsed_ms: started.elapsed().as_millis(),
                rows: Some(archived.snapshots.len()),
            });
            archived.snapshots
        } else {
            Vec::new()
        };
    let all_pm_book_snapshots =
        merge_pm_book_snapshots(hot_pm_book_snapshots, archived_pm_book_snapshots);
    pm_book_source.merged_sampled_rows = all_pm_book_snapshots.len();
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "pm_book_snapshots_merged".to_string(),
        elapsed_ms: 0,
        rows: Some(all_pm_book_snapshots.len()),
    });

    let deribit_snapshots = if options.include_deribit {
        let started = Instant::now();
        let deribit_result = load_deribit_feature_snapshots_with_timings(
            pool,
            &options.symbols,
            options.start,
            options.end,
            options.observation_sample_secs,
        )
        .await;
        let deribit_snapshots = deribit_result.snapshots;
        phase_timings.extend(deribit_result.phase_timings);
        phase_timings.push(ResearchSnapshotPhaseTiming {
            phase: "deribit_snapshots".to_string(),
            elapsed_ms: started.elapsed().as_millis(),
            rows: Some(deribit_snapshots.len()),
        });
        deribit_snapshots
    } else {
        phase_timings.push(ResearchSnapshotPhaseTiming {
            phase: "deribit_snapshots_skipped".to_string(),
            elapsed_ms: 0,
            rows: Some(0),
        });
        Vec::new()
    };

    let started = Instant::now();
    let updates_slice = slice_by_time(
        &all_updates,
        history_start,
        options.end,
        MarketUpdate::sort_ts,
    );
    let lob_slice = slice_by_time(&all_lob_snapshots, history_start, options.end, |snapshot| {
        snapshot.ts
    });
    let observations = build_factor_observations_with_lob_sampled(
        updates_slice,
        lob_slice,
        options.max_quote_age_secs,
        options.observation_sample_secs,
    );
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "factor_observations".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(observations.len()),
    });

    let quality_flags = research_snapshot_quality_flags(
        observations.len(),
        deribit_snapshots.len(),
        all_pm_book_snapshots.len(),
        options.include_deribit,
        pm_book_sample_secs,
        options.max_quote_age_secs,
        &pm_book_source,
    );
    let symbols_csv = options.symbols.join(",");

    Ok(ResearchSnapshot {
        manifest: ResearchSnapshotManifest {
            schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
            snapshot_hash: None,
            snapshot_contract_hash: None,
            generated_at: Utc::now(),
            git_sha: options.git_sha,
            symbols: options.symbols,
            start: options.start,
            end: options.end,
            history_start,
            lob_sample_secs: options.lob_sample_secs,
            pm_book_sample_secs: Some(pm_book_sample_secs),
            observation_sample_secs: options.observation_sample_secs,
            max_quote_age_secs: options.max_quote_age_secs,
            stake_usd: options.stake_usd,
            require_official_settlement: options.require_official_settlement,
            immutable_input: true,
            source_kind: "tango_postgres_compiled_snapshot".to_string(),
            optimizer_data_dir: options.optimizer_data_dir,
            source_surfaces: vec![
                ResearchSnapshotSourceSurface {
                    name: "historical_market_updates".to_string(),
                    role: "prediction_context".to_string(),
                    gate_category: "required_for_prediction".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: true,
                    sample_secs: Some(i64::from(historical_sample_secs)),
                    row_count: Some(all_updates.len()),
                    notes: "DB MarketUpdate tape loaded with sampler settings; suitable for factor search, not tick-complete replay.".to_string(),
                },
                ResearchSnapshotSourceSurface {
                    name: "binance_lob_ticks".to_string(),
                    role: "prediction_lob_context".to_string(),
                    gate_category: "required_for_prediction".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: true,
                    sample_secs: Some(i64::from(options.lob_sample_secs.max(1))),
                    row_count: Some(all_lob_snapshots.len()),
                    notes: "Partial-depth CEX LOB snapshots; not a sequence-correct local book for queue-position evidence.".to_string(),
                },
                ResearchSnapshotSourceSurface {
                    name: "clob_orderbook_snapshots".to_string(),
                    role: "execution_depth_context".to_string(),
                    gate_category: "required_for_execution".to_string(),
                    raw_full_fidelity: true,
                    snapshot_sampled: true,
                    sample_secs: Some(i64::from(pm_book_sample_secs)),
                    row_count: Some(all_pm_book_snapshots.len()),
                    notes: "Raw Polymarket full-depth CLOB surface exists, but this research snapshot stores sampled book states.".to_string(),
                },
                ResearchSnapshotSourceSurface {
                    name: "pm_token_settlements".to_string(),
                    role: "settlement_labels".to_string(),
                    gate_category: "required_for_execution".to_string(),
                    raw_full_fidelity: true,
                    snapshot_sampled: false,
                    sample_secs: None,
                    row_count: None,
                    notes: "Official settlement labels are required when require_official_settlement=true.".to_string(),
                },
                ResearchSnapshotSourceSurface {
                    name: "deribit_feature_snapshots".to_string(),
                    role: "optional_vol_context".to_string(),
                    gate_category: "optional_context".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: options.include_deribit,
                    sample_secs: if options.include_deribit {
                        Some(options.observation_sample_secs)
                    } else {
                        None
                    },
                    row_count: Some(deribit_snapshots.len()),
                    notes: if options.include_deribit {
                        "Deribit context materialized at observation cadence.".to_string()
                    } else {
                        "Deribit context intentionally excluded for this profile.".to_string()
                    },
                },
            ],
            input_artifacts: vec![ResearchSnapshotInputArtifact {
                name: "tango_postgres_research_window".to_string(),
                path: format!(
                    "tango_postgres://research_snapshot?start={}&end={}&symbols={}",
                    options.start,
                    options.end,
                    symbols_csv
                ),
                content_hash: None,
                row_count: Some(
                    all_updates.len()
                        + all_lob_snapshots.len()
                        + all_pm_book_snapshots.len()
                        + deribit_snapshots.len(),
                ),
            }],
            data_requirements: options.data_requirements,
            data_audit_status: options.data_audit_status,
            data_audit_report: options.data_audit_report,
            include_deribit: options.include_deribit,
            artifacts: ResearchSnapshotArtifacts::default(),
            row_counts: ResearchSnapshotRowCounts {
                observations: observations.len(),
                deribit_snapshots: deribit_snapshots.len(),
                pm_book_snapshots: all_pm_book_snapshots.len(),
            },
            phase_timings,
            quality_flags,
            pm_book_source,
        },
        observations,
        deribit_snapshots,
        pm_book_snapshots: all_pm_book_snapshots,
    })
}

#[cfg(feature = "db")]
fn slice_by_time<T, F>(items: &[T], start: DateTime<Utc>, end: DateTime<Utc>, ts_fn: F) -> &[T]
where
    F: Fn(&T) -> DateTime<Utc>,
{
    let lo = items.partition_point(|item| ts_fn(item) < start);
    let hi = items.partition_point(|item| ts_fn(item) < end);
    &items[lo..hi]
}

fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Result<T> {
    let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("parse {}", path.display()))
}

fn write_json<T: Serialize>(path: PathBuf, value: &T) -> Result<()> {
    let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
    serde_json::to_writer_pretty(file, value).with_context(|| format!("write {}", path.display()))
}

fn compute_snapshot_hash(
    snapshot_dir: &Path,
    manifest: &ResearchSnapshotManifest,
) -> Result<String> {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn update(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(FNV_PRIME);
        }
    }

    let mut hash = FNV_OFFSET;
    update(&mut hash, RESEARCH_SNAPSHOT_SCHEMA_VERSION.as_bytes());
    update(&mut hash, manifest.start.to_rfc3339().as_bytes());
    update(&mut hash, manifest.end.to_rfc3339().as_bytes());
    update(&mut hash, manifest.symbols.join(",").as_bytes());
    update(&mut hash, manifest.stake_usd.to_string().as_bytes());
    update(
        &mut hash,
        manifest
            .pm_book_sample_secs
            .map(|value| value.to_string())
            .unwrap_or_default()
            .as_bytes(),
    );
    update(&mut hash, manifest.data_requirements.join(",").as_bytes());
    update(
        &mut hash,
        serde_json::to_string(&manifest.source_surfaces)
            .context("serialize source_surfaces for snapshot hash")?
            .as_bytes(),
    );
    update(
        &mut hash,
        serde_json::to_string(&manifest.input_artifacts)
            .context("serialize input_artifacts for snapshot hash")?
            .as_bytes(),
    );
    update(&mut hash, manifest.include_deribit.to_string().as_bytes());
    update(&mut hash, manifest.pm_book_source.archive_status.as_bytes());
    update(
        &mut hash,
        manifest
            .pm_book_source
            .archive_dir
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    update(
        &mut hash,
        manifest
            .pm_book_source
            .archive_manifest_rows
            .to_string()
            .as_bytes(),
    );
    update(
        &mut hash,
        manifest
            .pm_book_source
            .archive_token_windows
            .to_string()
            .as_bytes(),
    );
    update(
        &mut hash,
        manifest
            .data_audit_status
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    update(
        &mut hash,
        manifest
            .data_audit_report
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    for artifact in [
        &manifest.artifacts.observations_json,
        &manifest.artifacts.deribit_snapshots_json,
        &manifest.artifacts.pm_book_snapshots_json,
    ] {
        update(&mut hash, artifact.as_bytes());
        let bytes = fs::read(snapshot_dir.join(artifact))
            .with_context(|| format!("read snapshot artifact {artifact} for hashing"))?;
        update(&mut hash, &bytes);
    }
    Ok(format!("{hash:016x}"))
}

fn compute_snapshot_contract_hash(
    snapshot_dir: &Path,
    manifest: &ResearchSnapshotManifest,
) -> Result<String> {
    fn update_framed(hasher: &mut Sha256, bytes: &[u8]) {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }

    let mut contract_manifest = manifest.clone();
    contract_manifest.snapshot_hash = None;
    contract_manifest.snapshot_contract_hash = None;

    let manifest_bytes = serde_json::to_vec(&contract_manifest)
        .context("serialize manifest for research snapshot evaluator contract hash")?;
    let mut hasher = Sha256::new();
    update_framed(&mut hasher, b"ploy.research_snapshot.evaluator_contract.v1");
    update_framed(&mut hasher, &manifest_bytes);

    for artifact in [
        Some(manifest.artifacts.observations_json.as_str()),
        Some(manifest.artifacts.deribit_snapshots_json.as_str()),
        Some(manifest.artifacts.pm_book_snapshots_json.as_str()),
        manifest.artifacts.observations_parquet.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        update_framed(&mut hasher, artifact.as_bytes());
        let bytes = fs::read(snapshot_dir.join(artifact)).with_context(|| {
            format!("read evaluator artifact {artifact} for snapshot contract hashing")
        })?;
        update_framed(&mut hasher, &bytes);
    }

    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn write_quality_markdown(path: &Path, manifest: &ResearchSnapshotManifest) -> Result<()> {
    let mut body = String::new();
    body.push_str("# Research Snapshot Quality\n\n");
    body.push_str(&format!("- Schema: `{}`\n", manifest.schema_version));
    body.push_str(&format!(
        "- Snapshot hash: `{}`\n",
        manifest.snapshot_hash.as_deref().unwrap_or("<missing>")
    ));
    body.push_str(&format!(
        "- Snapshot contract hash: `{}`\n",
        manifest
            .snapshot_contract_hash
            .as_deref()
            .unwrap_or("<legacy-unavailable>")
    ));
    body.push_str(&format!("- Generated at: `{}`\n", manifest.generated_at));
    body.push_str(&format!(
        "- Window: `{}` -> `{}`\n",
        manifest.start, manifest.end
    ));
    body.push_str(&format!("- Symbols: `{}`\n", manifest.symbols.join(",")));
    body.push_str(&format!(
        "- LOB sample secs: `{}`\n",
        manifest.lob_sample_secs
    ));
    body.push_str(&format!(
        "- PM book sample secs: `{}`\n",
        manifest
            .pm_book_sample_secs
            .map(|value| value.to_string())
            .unwrap_or_else(|| "<same-as-lob>".to_string())
    ));
    body.push_str(&format!(
        "- Observation sample secs: `{}`\n",
        manifest.observation_sample_secs
    ));
    body.push_str(&format!(
        "- Immutable input: `{}`\n",
        manifest.immutable_input
    ));
    body.push_str(&format!("- Source kind: `{}`\n", manifest.source_kind));
    body.push_str(&format!(
        "- Optimizer data dir: `{}`\n",
        manifest
            .optimizer_data_dir
            .as_deref()
            .unwrap_or("<missing>")
    ));
    body.push_str("\n## Source Surfaces\n\n");
    if manifest.source_surfaces.is_empty() {
        body.push_str("- `<not-recorded>`\n");
    } else {
        for surface in &manifest.source_surfaces {
            body.push_str(&format!(
                "- `{}` role=`{}` gate_category=`{}` raw_full_fidelity=`{}` snapshot_sampled=`{}` sample_secs=`{}` rows=`{}` notes=`{}`\n",
                surface.name,
                surface.role,
                surface.gate_category,
                surface.raw_full_fidelity,
                surface.snapshot_sampled,
                surface
                    .sample_secs
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                surface
                    .row_count
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                surface.notes
            ));
        }
    }
    body.push_str("\n## Input Artifacts\n\n");
    if manifest.input_artifacts.is_empty() {
        body.push_str("- `<database-or-remote-source>`\n");
    } else {
        for artifact in &manifest.input_artifacts {
            body.push_str(&format!(
                "- `{}` path=`{}` hash=`{}` rows=`{}`\n",
                artifact.name,
                artifact.path,
                artifact.content_hash.as_deref().unwrap_or("<missing>"),
                artifact
                    .row_count
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string())
            ));
        }
    }
    body.push_str(&format!(
        "- Data requirements: `{}`\n",
        if manifest.data_requirements.is_empty() {
            "<unspecified>".to_string()
        } else {
            manifest.data_requirements.join(",")
        }
    ));
    body.push_str(&format!(
        "- Data audit status: `{}`\n",
        manifest
            .data_audit_status
            .as_deref()
            .unwrap_or("<not-recorded>")
    ));
    body.push_str(&format!(
        "- Data audit report: `{}`\n",
        manifest
            .data_audit_report
            .as_deref()
            .unwrap_or("<not-recorded>")
    ));
    body.push_str(&format!(
        "- Deribit included: `{}`\n",
        manifest.include_deribit
    ));
    body.push_str(&format!(
        "- Rows: observations={}, deribit={}, pm_books={}\n",
        manifest.row_counts.observations,
        manifest.row_counts.deribit_snapshots,
        manifest.row_counts.pm_book_snapshots
    ));
    body.push_str(&format!(
        "- PM book source: hot_postgres_sampled_rows={}, archive_sampled_rows={}, archive_manifest_rows={}, archive_files={}, archive_token_windows={}, merged_sampled_rows={}, archive_status=`{}` archive_dir=`{}`\n",
        manifest.pm_book_source.hot_postgres_sampled_rows,
        manifest.pm_book_source.archive_sampled_rows,
        manifest.pm_book_source.archive_manifest_rows,
        manifest.pm_book_source.archive_files,
        manifest.pm_book_source.archive_token_windows,
        manifest.pm_book_source.merged_sampled_rows,
        manifest.pm_book_source.archive_status,
        manifest
            .pm_book_source
            .archive_dir
            .as_deref()
            .unwrap_or("<not-configured>")
    ));
    body.push_str(&format!(
        "- Official settlement required: `{}`\n",
        manifest.require_official_settlement
    ));
    body.push_str("\n## Phase Timings\n\n");
    for timing in &manifest.phase_timings {
        body.push_str(&format!(
            "- `{}`: {} ms, rows={}\n",
            timing.phase,
            timing.elapsed_ms,
            timing
                .rows
                .map(|rows| rows.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        ));
    }
    if !manifest.quality_flags.is_empty() {
        body.push_str("\n## Quality Flags\n\n");
        for flag in &manifest.quality_flags {
            body.push_str(&format!("- `{flag}`\n"));
        }
    }
    fs::write(path, body).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_load_empty_snapshot_roundtrips_manifest() {
        let root = std::env::temp_dir().join(format!(
            "ploy-research-snapshot-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let start = "2026-04-24T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let subset_start = "2026-04-25T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let subset_end = "2026-04-27T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let snapshot = ResearchSnapshot {
            manifest: ResearchSnapshotManifest {
                schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
                snapshot_hash: None,
                snapshot_contract_hash: None,
                generated_at: Utc::now(),
                git_sha: Some("test-sha".to_string()),
                symbols: vec!["BTCUSDT".to_string()],
                start,
                end,
                history_start: start,
                lob_sample_secs: 30,
                pm_book_sample_secs: Some(30),
                observation_sample_secs: 30,
                max_quote_age_secs: 30,
                stake_usd: 15.0,
                require_official_settlement: true,
                immutable_input: true,
                source_kind: "unit_test".to_string(),
                optimizer_data_dir: Some("/tmp/immutable-parquet".to_string()),
                source_surfaces: vec![ResearchSnapshotSourceSurface {
                    name: "unit_surface".to_string(),
                    role: "test".to_string(),
                    gate_category: "required_for_prediction".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: true,
                    sample_secs: Some(30),
                    row_count: Some(0),
                    notes: "unit test sampled surface".to_string(),
                }],
                input_artifacts: vec![ResearchSnapshotInputArtifact {
                    name: "unit_input".to_string(),
                    path: "/tmp/unit-input.parquet".to_string(),
                    content_hash: Some("abc123".to_string()),
                    row_count: Some(0),
                }],
                data_requirements: vec!["polymarket_quotes".to_string()],
                data_audit_status: Some("ok".to_string()),
                data_audit_report: Some("data-gap-audit.json".to_string()),
                include_deribit: false,
                artifacts: ResearchSnapshotArtifacts::default(),
                row_counts: ResearchSnapshotRowCounts::default(),
                phase_timings: vec![ResearchSnapshotPhaseTiming {
                    phase: "unit".to_string(),
                    elapsed_ms: 1,
                    rows: Some(0),
                }],
                quality_flags: vec![],
                pm_book_source: ResearchSnapshotPmBookSource::default(),
            },
            observations: vec![],
            deribit_snapshots: vec![],
            pm_book_snapshots: vec![],
        };

        let written = write_research_snapshot(&root, snapshot).expect("write snapshot");
        let loaded = load_research_snapshot(&root).expect("load snapshot");
        assert_eq!(written.schema_version, RESEARCH_SNAPSHOT_SCHEMA_VERSION);
        assert!(written.snapshot_hash.is_some());
        let contract_hex = written
            .snapshot_contract_hash
            .as_deref()
            .and_then(|hash| hash.strip_prefix("sha256:"))
            .expect("SHA-256 contract hash");
        assert_eq!(contract_hex.len(), 64);
        assert!(contract_hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(loaded.manifest.git_sha.as_deref(), Some("test-sha"));
        assert_eq!(loaded.manifest.source_surfaces.len(), 1);
        assert!(loaded.manifest.source_surfaces[0].snapshot_sampled);
        assert_eq!(
            loaded.manifest.source_surfaces[0].gate_category,
            "required_for_prediction"
        );
        assert_eq!(loaded.manifest.input_artifacts.len(), 1);
        validate_snapshot_request(
            &loaded.manifest,
            ResearchSnapshotRequest {
                symbols: &["BTCUSDT".to_string()],
                start: loaded.manifest.start,
                end: loaded.manifest.end,
                lob_sample_secs: loaded.manifest.lob_sample_secs,
                pm_book_sample_secs: loaded.manifest.pm_book_sample_secs.unwrap_or(30),
                observation_sample_secs: loaded.manifest.observation_sample_secs,
                max_quote_age_secs: loaded.manifest.max_quote_age_secs,
                stake_usd: loaded.manifest.stake_usd,
                require_official_settlement: loaded.manifest.require_official_settlement,
            },
        )
        .expect("snapshot request validation");
        validate_snapshot_request_coverage(
            &loaded.manifest,
            ResearchSnapshotRequest {
                symbols: &["BTCUSDT".to_string()],
                start: subset_start,
                end: subset_end,
                lob_sample_secs: loaded.manifest.lob_sample_secs,
                pm_book_sample_secs: loaded.manifest.pm_book_sample_secs.unwrap_or(30),
                observation_sample_secs: loaded.manifest.observation_sample_secs,
                max_quote_age_secs: loaded.manifest.max_quote_age_secs,
                stake_usd: loaded.manifest.stake_usd,
                require_official_settlement: loaded.manifest.require_official_settlement,
            },
        )
        .expect("snapshot coverage validation");
        let exact_subset_result = validate_snapshot_request(
            &loaded.manifest,
            ResearchSnapshotRequest {
                symbols: &["BTCUSDT".to_string()],
                start: subset_start,
                end: subset_end,
                lob_sample_secs: loaded.manifest.lob_sample_secs,
                pm_book_sample_secs: loaded.manifest.pm_book_sample_secs.unwrap_or(30),
                observation_sample_secs: loaded.manifest.observation_sample_secs,
                max_quote_age_secs: loaded.manifest.max_quote_age_secs,
                stake_usd: loaded.manifest.stake_usd,
                require_official_settlement: loaded.manifest.require_official_settlement,
            },
        );
        assert!(exact_subset_result.is_err());
        assert_eq!(loaded.manifest.row_counts.observations, 0);
        let quality = std::fs::read_to_string(root.join("quality.md")).expect("read quality");
        assert!(quality.contains("Snapshot contract hash: `sha256:"));
        assert!(quality.contains("## Source Surfaces"));
        assert!(quality.contains("gate_category=`required_for_prediction`"));
        assert!(quality.contains("snapshot_sampled=`true`"));
        assert!(quality.contains("## Input Artifacts"));

        let mut legacy_manifest = serde_json::to_value(&loaded.manifest).expect("legacy manifest");
        legacy_manifest
            .as_object_mut()
            .expect("manifest object")
            .remove("snapshot_contract_hash");
        write_json(root.join("manifest.json"), &legacy_manifest).expect("write legacy manifest");
        load_research_snapshot(&root).expect("legacy snapshot without contract hash must load");

        let mut semantic_tamper = loaded.manifest.clone();
        semantic_tamper.max_quote_age_secs += 1;
        write_json(root.join("manifest.json"), &semantic_tamper)
            .expect("tamper snapshot manifest semantics");
        let tampered_manifest =
            load_research_snapshot(&root).expect_err("semantic tamper must fail");
        assert!(tampered_manifest
            .to_string()
            .contains("evaluator contract hash mismatch"));

        write_json(root.join("manifest.json"), &loaded.manifest)
            .expect("restore snapshot manifest");
        std::fs::write(
            root.join(&loaded.manifest.artifacts.observations_json),
            "[]\n",
        )
        .expect("tamper snapshot observations");
        let tampered = load_research_snapshot(&root).expect_err("tampered snapshot must fail");
        assert!(tampered
            .to_string()
            .contains("evaluator contract hash mismatch"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn snapshot_request_rejects_pm_book_cadence_mismatch_or_coarse_execution_gate() {
        let start = "2026-04-24T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2026-04-25T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let manifest = ResearchSnapshotManifest {
            schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
            snapshot_hash: Some("hash".to_string()),
            snapshot_contract_hash: None,
            generated_at: Utc::now(),
            git_sha: Some("test-sha".to_string()),
            symbols: vec!["BTCUSDT".to_string()],
            start,
            end,
            history_start: start,
            lob_sample_secs: 30,
            pm_book_sample_secs: Some(120),
            observation_sample_secs: 30,
            max_quote_age_secs: 120,
            stake_usd: 15.0,
            require_official_settlement: true,
            immutable_input: true,
            source_kind: "unit_test".to_string(),
            optimizer_data_dir: Some("/tmp/immutable-parquet".to_string()),
            source_surfaces: vec![],
            input_artifacts: vec![],
            data_requirements: vec![],
            data_audit_status: Some("ok".to_string()),
            data_audit_report: None,
            include_deribit: false,
            artifacts: ResearchSnapshotArtifacts::default(),
            row_counts: ResearchSnapshotRowCounts::default(),
            phase_timings: vec![],
            quality_flags: vec![],
            pm_book_source: ResearchSnapshotPmBookSource::default(),
        };
        let symbols = vec!["BTCUSDT".to_string()];

        let mismatch = validate_snapshot_request_coverage(
            &manifest,
            ResearchSnapshotRequest {
                symbols: &symbols,
                start,
                end,
                lob_sample_secs: 30,
                pm_book_sample_secs: 30,
                observation_sample_secs: 30,
                max_quote_age_secs: 120,
                stake_usd: 15.0,
                require_official_settlement: true,
            },
        )
        .expect_err("PM book cadence mismatch should fail closed");
        assert!(mismatch
            .to_string()
            .contains("snapshot pm_book_sample_secs 120 does not match requested 30"));

        let coarse = validate_snapshot_request_coverage(
            &manifest,
            ResearchSnapshotRequest {
                symbols: &symbols,
                start,
                end,
                lob_sample_secs: 30,
                pm_book_sample_secs: 120,
                observation_sample_secs: 30,
                max_quote_age_secs: 30,
                stake_usd: 15.0,
                require_official_settlement: true,
            },
        )
        .expect_err("PM book cadence coarser than quote-age gate should fail closed");
        assert!(coarse
            .to_string()
            .contains("full-depth execution claims require PM book cadence"));
    }

    #[test]
    fn quality_flags_sparse_pm_book_sampling_against_quote_age() {
        let flags = research_snapshot_quality_flags(
            100,
            0,
            10,
            false,
            300,
            30,
            &ResearchSnapshotPmBookSource::default(),
        );

        assert!(flags.contains(&"pm_book_sample_secs_gt_max_quote_age:300>30".to_string()));
        assert!(!flags.contains(&"no_factor_observations".to_string()));
        assert!(!flags.contains(&"no_pm_book_snapshots".to_string()));
    }

    #[test]
    fn quality_flags_accepts_pm_book_sampling_within_quote_age() {
        let flags = research_snapshot_quality_flags(
            100,
            0,
            10,
            false,
            30,
            30,
            &ResearchSnapshotPmBookSource::default(),
        );

        assert!(flags.is_empty());
    }

    #[test]
    fn quality_flags_archive_manifest_without_sampled_rows() {
        let flags = research_snapshot_quality_flags(
            100,
            0,
            0,
            false,
            30,
            30,
            &ResearchSnapshotPmBookSource {
                archive_manifest_rows: 1000,
                archive_status: "archive_loaded".to_string(),
                ..Default::default()
            },
        );

        assert!(flags.contains(&"no_pm_book_snapshots".to_string()));
        assert!(flags.contains(&"pm_book_archive_manifest_rows_but_no_sampled_rows".to_string()));
    }

    #[cfg(feature = "db")]
    #[test]
    fn archive_hour_dirs_only_includes_overlapping_local_hours() {
        let start = "2026-05-18T16:30:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2026-05-18T18:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let hours = archive_hour_dirs(start, end);

        assert_eq!(
            hours,
            vec![("2026-05-19".to_string(), 0), ("2026-05-19".to_string(), 1)]
        );
    }

    #[cfg(feature = "db")]
    #[test]
    fn parse_duckdb_timestamptz_accepts_compact_hour_offset() {
        let parsed = parse_duckdb_timestamptz("2026-05-19 06:55:13.870644+08")
            .expect("compact offset timestamp");

        assert_eq!(parsed.to_rfc3339(), "2026-05-18T22:55:13.870644+00:00");
    }

    #[cfg(feature = "db")]
    #[test]
    fn archive_token_window_values_sql_quotes_contract_fields() {
        let windows = vec![PmBookTokenWindow {
            market_slug: "btc-up's-test".to_string(),
            token_id: "123".to_string(),
            side: "UP".to_string(),
            window_start: "2026-05-18T00:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            window_end: "2026-05-18T00:05:00Z".parse::<DateTime<Utc>>().unwrap(),
        }];

        let sql = archive_token_window_values_sql(&windows);

        assert!(sql.contains("'btc-up''s-test'"));
        assert!(sql.contains("TIMESTAMPTZ '2026-05-18T00:00:00+00:00'"));
        assert!(sql.contains("TIMESTAMPTZ '2026-05-18T00:05:00+00:00'"));
    }
}
