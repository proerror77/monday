//! Compile a point-in-time research snapshot from Tango PostgreSQL raw tables.
//!
//! This binary is the boundary between collector storage and repeated research
//! scoring. Factor review, walk-forward, and optimizer jobs should consume the
//! resulting immutable snapshot artifacts instead of rebuilding raw joins.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_research::{
    build_research_snapshot_from_database, write_research_snapshot, ResearchSnapshotBuildOptions,
};
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let db_url = flag_value(&args, "--db-url").expect("--db-url required");
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
    let data_audit_status = flag_value(&args, "--data-audit-status");
    let data_audit_report = flag_value(&args, "--data-audit-report");
    let include_deribit = !flag_present(&args, "--skip-deribit");
    let pm_book_archive_dir = flag_value(&args, "--pm-book-archive-dir")
        .or_else(|| std::env::var("PLOY_CLOB_BOOK_ARCHIVE_DIR").ok())
        .or_else(|| Some("/opt/ploy/data/lake/orderbook_snapshots".to_string()))
        .filter(|raw| !raw.trim().is_empty())
        .map(PathBuf::from);

    eprintln!(
        "research_snapshot_compile: {} -> {} for {:?}, stake_usd={:.2}, output={}, data_requirements={}, include_deribit={}, pm_book_sample_secs={}, pm_book_archive_dir={}",
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

    let snapshot = build_research_snapshot_from_database(
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
            data_audit_report,
            include_deribit,
            pm_book_archive_dir,
        },
    )
    .await?;
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
