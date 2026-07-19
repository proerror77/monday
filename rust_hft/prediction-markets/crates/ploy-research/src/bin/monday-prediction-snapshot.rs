//! Compile a governed point-in-time snapshot from Monday research PostgreSQL tables.
//!
//! This binary is the boundary between collector storage and repeated research
//! scoring. Factor review, walk-forward, and optimizer jobs should consume the
//! resulting immutable snapshot artifacts instead of rebuilding raw joins.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::Value;
use ploy_market_data::diagnostics::PredictionMarketDataAuditReport;
use ploy_research::research_snapshot::ResearchSnapshotInputArtifact;
use ploy_research::{
    build_research_snapshot_from_database, write_research_snapshot, ResearchSnapshotBuildOptions,
};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn flag_present(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn parse_csv(raw: Option<String>, default: &str) -> Vec<String> {
    raw.unwrap_or_else(|| default.to_string())
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    let next_day = date
        .succ_opt()
        .unwrap_or_else(|| panic!("invalid end date: {raw}"));
    Utc.from_utc_datetime(&next_day.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    raw.parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| panic!("invalid timestamp: {raw}"))
}

fn validated_data_audit(
    report_path: Option<String>,
    supplied_status: Option<String>,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> anyhow::Result<(Option<String>, Option<String>, Option<String>)> {
    let Some(report_path) = report_path else {
        anyhow::bail!(
            "--data-audit-report is required; prediction snapshots cannot be compiled without typed audit evidence"
        );
    };
    let bytes = std::fs::read(&report_path)
        .map_err(|error| anyhow::anyhow!("read data audit report {report_path}: {error}"))?;
    let report: PredictionMarketDataAuditReport = serde_json::from_slice(&bytes)
        .map_err(|error| anyhow::anyhow!("parse typed data audit report {report_path}: {error}"))?;
    report
        .validate_for_prediction_snapshot(symbols, start, end)
        .map_err(|error| anyhow::anyhow!("validate data audit report {report_path}: {error}"))?;
    let report_status = report.data_audit_status.as_str().to_string();
    if supplied_status
        .as_deref()
        .is_some_and(|status| status != report_status)
    {
        anyhow::bail!("--data-audit-status does not match typed report status {report_status}");
    }
    let report_hash = format!("sha256:{:x}", Sha256::digest(&bytes));
    Ok((Some(report_status), Some(report_path), Some(report_hash)))
}

fn parse_polymarket_evidence_manifest(
    evidence_manifest: Option<String>,
) -> anyhow::Result<(bool, bool, Option<String>)> {
    let Some(path) = evidence_manifest else {
        return Ok((false, false, None));
    };
    let bytes = std::fs::read(&path)
        .map_err(|error| anyhow::anyhow!("read polymarket evidence manifest {path}: {error}"))?;
    let manifest: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| anyhow::anyhow!("parse polymarket evidence manifest {path}: {error}"))?;
    let trade_tape_sequence_contiguous = manifest
        .get("trade_tape_sequence_contiguous")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            anyhow::anyhow!("polymarket evidence manifest {path} missing trade_tape_sequence_contiguous")
        })?;
    let trade_tape_event_local_complete = manifest
        .get("trade_tape_event_local_complete")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "polymarket evidence manifest {path} missing trade_tape_event_local_complete"
            )
        })?;
    Ok((
        trade_tape_sequence_contiguous,
        trade_tape_event_local_complete,
        Some(path),
    ))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args
        .iter()
        .any(|arg| arg == "--db-url" || arg.starts_with("--db-url="))
    {
        anyhow::bail!(
            "--db-url is forbidden because it exposes credentials; use MONDAY_RESEARCH_DATABASE_URL or DATABASE_URL"
        );
    }
    let db_url = std::env::var("MONDAY_RESEARCH_DATABASE_URL")
        .ok()
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "database URL is required via MONDAY_RESEARCH_DATABASE_URL or DATABASE_URL"
            )
        })?;
    let output_dir =
        PathBuf::from(flag_value(&args, "--output-dir").expect("--output-dir required"));
    let start = flag_value(&args, "--start-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_start(&flag_value(&args, "--start-date").expect("--start-date required"))
        });
    let end = flag_value(&args, "--end-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_end(&flag_value(&args, "--end-date").expect("--end-date required"))
        });
    let symbols: Vec<String> = parse_csv(flag_value(&args, "--symbols"), "BTCUSDT");
    let lob_sample_secs: i32 = flag_value(&args, "--lob-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let pm_book_sample_secs: i32 = flag_value(&args, "--pm-book-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(lob_sample_secs);
    let max_quote_age_secs: i64 = flag_value(&args, "--max-quote-age-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let observation_sample_secs: i64 = flag_value(&args, "--observation-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let stake_usd = flag_value(&args, "--stake-usd")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(15.0);
    let require_official_settlement = !flag_present(&args, "--allow-missing-official-settlement");
    let optimizer_data_dir =
        flag_value(&args, "--optimizer-data-dir").expect("--optimizer-data-dir required");
    let data_requirements = parse_csv(flag_value(&args, "--data-requirements"), "all");
    let (data_audit_status, data_audit_report, data_audit_report_hash) = validated_data_audit(
        flag_value(&args, "--data-audit-report"),
        flag_value(&args, "--data-audit-status"),
        &symbols,
        start,
        end,
    )?;
    let include_deribit = !flag_present(&args, "--skip-deribit");
    let pm_book_archive_dir = flag_value(&args, "--pm-book-archive-dir")
        .or_else(|| std::env::var("MONDAY_PREDICTION_BOOK_ARCHIVE_DIR").ok())
        .filter(|raw| !raw.trim().is_empty())
        .map(PathBuf::from);
    let (
        polymarket_trade_tape_sequence_contiguous,
        polymarket_trade_tape_event_local_complete,
        polymarket_evidence_manifest,
    ) = parse_polymarket_evidence_manifest(flag_value(&args, "--polymarket-evidence-manifest"))?;

    eprintln!(
        "monday-prediction-snapshot: {} -> {} for {:?}, stake_usd={:.2}, output={}, data_requirements={}, include_deribit={}, pm_book_sample_secs={}, pm_book_archive_dir={}",
        start,
        end,
        symbols,
        stake_usd,
        output_dir.display(),
        data_requirements.join(","),
        include_deribit,
        pm_book_sample_secs,
        pm_book_archive_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<not-configured>".to_string())
    );

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(120))
        .connect(&db_url)
        .await?;

    let mut snapshot = build_research_snapshot_from_database(
        &pool,
        ResearchSnapshotBuildOptions {
            symbols,
            start,
            end,
            lob_sample_secs,
            pm_book_sample_secs,
            observation_sample_secs,
            max_quote_age_secs,
            stake_usd,
            require_official_settlement,
            optimizer_data_dir: Some(optimizer_data_dir),
            git_sha: std::env::var("GITHUB_SHA").ok(),
            data_requirements,
            data_audit_status,
            data_audit_report: data_audit_report.clone(),
            polymarket_trade_tape_sequence_contiguous: Some(
                polymarket_trade_tape_sequence_contiguous,
            ),
            polymarket_trade_tape_event_local_complete: Some(
                polymarket_trade_tape_event_local_complete,
            ),
            polymarket_evidence_manifest,
            include_deribit,
            pm_book_archive_dir,
        },
    )
    .await?;
    if let (Some(path), Some(content_hash)) =
        (data_audit_report.as_ref(), data_audit_report_hash.as_ref())
    {
        snapshot
            .manifest
            .input_artifacts
            .push(ResearchSnapshotInputArtifact {
                name: "prediction_market_data_audit".to_string(),
                path: path.clone(),
                content_hash: Some(content_hash.clone()),
                row_count: None,
            });
    }
    let manifest = write_research_snapshot(&output_dir, snapshot)?;

    eprintln!("research snapshot written: {}", output_dir.display());
    eprintln!(
        "rows: observations={} deribit={} pm_books={}",
        manifest.row_counts.observations,
        manifest.row_counts.deribit_snapshots,
        manifest.row_counts.pm_book_snapshots
    );
    for timing in &manifest.phase_timings {
        eprintln!(
            "phase {:<24} {:>8} ms rows={}",
            timing.phase,
            timing.elapsed_ms,
            timing
                .rows
                .map(|rows| rows.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validated_data_audit;
    use chrono::{TimeZone, Utc};

    #[test]
    fn prediction_snapshot_requires_typed_audit_evidence() {
        let start = Utc
            .with_ymd_and_hms(2026, 7, 1, 0, 0, 0)
            .single()
            .expect("valid start");
        let end = Utc
            .with_ymd_and_hms(2026, 7, 2, 0, 0, 0)
            .single()
            .expect("valid end");

        let error = validated_data_audit(None, None, &["BTCUSDT".to_string()], start, end)
            .expect_err("missing typed audit must fail closed");

        assert!(error
            .to_string()
            .contains("--data-audit-report is required"));
    }
}
