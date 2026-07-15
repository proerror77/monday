//! Produce a typed, read-only market-data audit for a prediction snapshot.
//!
//! Example:
//! cargo run -p ploy-market-data --features audit --example market_data_gap_audit -- \
//!   --start-ts 2026-07-01T00:00:00Z --end-ts 2026-07-02T00:00:00Z \
//!   --symbols BTCUSDT,SOLUSDT --output /tmp/prediction-data-audit.json

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use ploy_market_data::diagnostics::{
    assemble_prediction_market_data_audit, audit_prediction_market_data_url, AuditFinding,
    AuditStatus, PredictionMarketDataAuditRequest,
};

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn parse_timestamp(args: &[String], flag: &str) -> Result<DateTime<Utc>> {
    let raw = flag_value(args, flag).ok_or_else(|| anyhow!("{flag} is required"))?;
    raw.parse::<DateTime<Utc>>()
        .with_context(|| format!("invalid {flag} timestamp {raw:?}"))
}

fn parse_i64(args: &[String], flag: &str, default: i64) -> Result<i64> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<i64>()
                .with_context(|| format!("invalid {flag} value {raw:?}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_u16(args: &[String], flag: &str, default: u16) -> Result<u16> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<u16>()
                .with_context(|| format!("invalid {flag} value {raw:?}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_symbols(args: &[String]) -> Vec<String> {
    flag_value(args, "--symbols")
        .unwrap_or_else(|| "BTCUSDT,SOLUSDT".to_string())
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(str::to_ascii_uppercase)
        .collect()
}

fn database_url() -> Result<String> {
    std::env::var("PLOY_DATABASE__URL")
        .ok()
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("database URL is required via PLOY_DATABASE__URL or DATABASE_URL"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create audit output directory {}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("market-data-audit.json");
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("create temporary audit file {}", temporary.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("write temporary audit file {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temporary audit file {}", temporary.display()))?;
        fs::hard_link(&temporary, path).with_context(|| {
            format!(
                "publish immutable audit file {} -> {}; destination must not already exist",
                temporary.display(),
                path.display()
            )
        })?;
        fs::remove_file(&temporary)
            .with_context(|| format!("remove audit staging file {}", temporary.display()))?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::File::open(parent)
                .with_context(|| format!("open audit output directory {}", parent.display()))?
                .sync_all()
                .with_context(|| format!("sync audit output directory {}", parent.display()))?;
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args
        .iter()
        .any(|arg| arg == "--db-url" || arg.starts_with("--db-url="))
    {
        return Err(anyhow!(
            "--db-url is forbidden because it exposes credentials; use PLOY_DATABASE__URL or DATABASE_URL"
        ));
    }
    let snapshot_start = parse_timestamp(&args, "--start-ts")?;
    let snapshot_end = parse_timestamp(&args, "--end-ts")?;
    let mut request =
        PredictionMarketDataAuditRequest::btc_sol_five_minute(snapshot_start, snapshot_end);
    request.symbols = parse_symbols(&args);
    request.horizon_secs = parse_i64(&args, "--horizon-secs", request.horizon_secs)?;
    request.warmup_secs = parse_i64(&args, "--warmup-secs", request.warmup_secs)?;
    request.bucket_secs = parse_i64(&args, "--bucket-secs", request.bucket_secs)?;
    request.minimum_coverage_bps = parse_u16(
        &args,
        "--minimum-coverage-bps",
        request.minimum_coverage_bps,
    )?;
    request.maximum_gap_secs = parse_i64(&args, "--maximum-gap-secs", request.maximum_gap_secs)?;
    request.maximum_source_delay_secs = parse_i64(
        &args,
        "--maximum-source-delay-secs",
        request.maximum_source_delay_secs,
    )?;

    let mut report = match database_url() {
        Ok(db_url) => audit_prediction_market_data_url(&db_url, request).await,
        Err(_) => assemble_prediction_market_data_audit(
            request,
            Utc::now(),
            Vec::new(),
            vec![AuditFinding::producer_failure(
                "database_connection",
                "database URL was not configured",
            )],
        ),
    };
    if report.data_audit_status == AuditStatus::Ok {
        if let Err(error) = report.validate_for_prediction_snapshot(
            &report.request.symbols,
            report.request.snapshot_start,
            report.request.snapshot_end,
        ) {
            report = assemble_prediction_market_data_audit(
                report.request.clone(),
                report.generated_at,
                report.surface_results.clone(),
                vec![AuditFinding::producer_failure(
                    "semantic_validation",
                    format!("typed audit failed canonical self-validation: {error}"),
                )],
            );
        }
    }
    let mut json = serde_json::to_vec_pretty(&report).context("serialize typed audit report")?;
    json.push(b'\n');

    if let Some(output) = flag_value(&args, "--output").map(PathBuf::from) {
        atomic_write(&output, &json)?;
        eprintln!("data_audit_report={}", output.display());
    } else {
        std::io::stdout()
            .write_all(&json)
            .context("write audit report to stdout")?;
    }
    eprintln!("data_audit_status={}", report.data_audit_status.as_str());
    if report.data_audit_status == AuditStatus::Critical {
        return Err(anyhow!(
            "prediction market-data audit is critical; blockers={}",
            report.blockers.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::atomic_write;

    #[test]
    fn immutable_audit_write_refuses_to_replace_existing_evidence() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("audit.json");
        atomic_write(&path, b"first\n").expect("first publication");

        let error = atomic_write(&path, b"second\n").expect_err("overwrite must fail closed");

        assert!(error.to_string().contains("must not already exist"));
        assert_eq!(std::fs::read(path).expect("published evidence"), b"first\n");
    }
}
