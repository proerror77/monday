//! Diagnostic entry point. It neither authenticates a lease nor completes a Campaign.
use alpha_engine::{
    baselines::diagnostic::{run_diagnostic_baseline, DiagnosticBaselineSpec},
    evaluation::ResearchRow,
};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
#[derive(Parser)]
#[command(about = "Bounded offline Ridge diagnostic; does not grant research authority")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Write an explicitly synthetic native input and fixed spec for a local smoke run.
    SyntheticFixture {
        #[arg(long)]
        output: PathBuf,
    },
    /// Fit train-only Ridge folds and account for complete walk-forward OOS positions.
    Run(Args),
}
#[derive(clap::Args)]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    input_sha256: String,
    #[arg(long)]
    spec: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long,value_parser=["synthetic","historical"])]
    input_kind: String,
    /// External admitted authority reference; this diagnostic cannot grant compute.
    #[arg(long)]
    lease_reference: Option<String>,
}
fn hash(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}
#[derive(Serialize)]
struct Manifest<'a> {
    schema: &'static str,
    source_revision: &'static str,
    input_kind: &'a str,
    input_sha256: String,
    spec_sha256: String,
    report_sha256: String,
    executable_sha256: String,
    external_lease_reference: &'a Option<String>,
    compute_authority_verified_by_diagnostic: bool,
    campaign_acceptance: bool,
}
fn main() -> Result<()> {
    match Cli::parse().command {
        Command::SyntheticFixture { output } => synthetic_fixture(output),
        Command::Run(args) => run(args),
    }
}
fn run(a: Args) -> Result<()> {
    if a.input_kind == "historical"
        && (std::env::var("MONDAY_EXECUTION_HOST").as_deref() != Ok("ack")
            || a.lease_reference
                .as_deref()
                .is_none_or(|s| s.trim().is_empty()))
    {
        bail!("historical inputs require ACK placement and an externally admitted lease reference");
    }
    if a.output.exists() {
        bail!("result directory must be new");
    }
    let bytes = read_bounded(&a.input, 32 * 1024 * 1024)?;
    if hash(&bytes) != a.input_sha256 {
        bail!("prepared input SHA256 mismatch");
    }
    let rows: Vec<ResearchRow> =
        serde_json::from_slice(&bytes).context("expected native prepared-row JSON array")?;
    let spec_bytes = read_bounded(&a.spec, 256 * 1024)?;
    let spec: DiagnosticBaselineSpec = serde_json::from_slice(&spec_bytes)?;
    let report = run_diagnostic_baseline(rows, &spec).map_err(anyhow::Error::msg)?;
    let output = serde_json::to_vec_pretty(&report)?;
    fs::create_dir(&a.output)?;
    let write = |name: &str, data: &[u8]| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(a.output.join(name))?;
        file.write_all(data)?;
        file.sync_all()?;
        Ok(())
    };
    write("report.json", &output)?;
    let m = Manifest {
        schema: "monday.offline_ridge_diagnostic_manifest.v1",
        source_revision: option_env!("MONDAY_SOURCE_REVISION").unwrap_or("unbound-local-code"),
        input_kind: &a.input_kind,
        input_sha256: hash(&bytes),
        spec_sha256: hash(&spec_bytes),
        report_sha256: hash(&output),
        executable_sha256: hash(&fs::read(std::env::current_exe()?)?),
        external_lease_reference: &a.lease_reference,
        compute_authority_verified_by_diagnostic: false,
        campaign_acceptance: false,
    };
    write("manifest.json", &serde_json::to_vec_pretty(&m)?)?;
    println!(
        "{}",
        serde_json::json!({"diagnostic_only":true,"fit_count":report.fit_count,
 "report_sha256":m.report_sha256,"campaign_acceptance":false})
    );
    Ok(())
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > maximum {
        bail!("input must be nonempty and at most {maximum} bytes");
    }
    Ok(bytes)
}

fn synthetic_fixture(output: PathBuf) -> Result<()> {
    use alpha_domain::{
        EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationProtocolV1, EvaluationWalkForwardV1,
        FormulaEvaluatorConfig,
    };
    use chrono::{TimeDelta, TimeZone, Utc};
    use std::collections::BTreeMap;
    let start = Utc
        .with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
        .single()
        .context("fixture time")?;
    let mids = (0..305)
        .map(|i| 100.0 + 0.01 * i as f64 + 0.03 * (i as f64 / 7.0).sin())
        .collect::<Vec<_>>();
    let rows = (0..300)
        .map(|i| ResearchRow {
            series_id: 1,
            available_time: start + TimeDelta::seconds(i as i64),
            label_available_time: start + TimeDelta::seconds(i as i64 + 5),
            signal: 0.0,
            features: BTreeMap::from([
                ("book_imbalance".into(), (i as f64 / 7.0).cos()),
                ("mid_price".into(), mids[i]),
                ("spread_bps".into(), 1.0),
            ]),
            label: mids[i + 5] / mids[i] - 1.0,
            fee_bps: 2.0,
            funding_bps: 0.0,
            pit_funding: false,
            latency_bps: 0.5,
        })
        .collect::<Vec<_>>();
    let protocol = EvaluationProtocolV1::new(
        EvaluationWalkForwardV1 {
            initial_train_rows: 120,
            validation_rows: 30,
            fold_count: 3,
            purge_rows: 5,
            embargo_rows: 5,
            sealed_holdout_rows: 20,
        },
        EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            slippage_bps: 1.0,
            cross_spread: true,
            position_notional_usd: 0.0,
            capacity_depth_levels: 0,
            max_book_depth_fraction: 0.0,
        },
        EvaluationLabelSpecV1 {
            horizon_buckets: 5,
            observation_frequency_millis: 1000,
        },
    )?;
    let spec = DiagnosticBaselineSpec {
        protocol,
        features: vec!["book_imbalance".into()],
        ridge_l2: 1.0,
        evaluator: FormulaEvaluatorConfig::default(),
    };
    fs::create_dir(&output)?;
    let rows_bytes = serde_json::to_vec_pretty(&rows)?;
    fs::write(output.join("synthetic-rows.json"), &rows_bytes)?;
    fs::write(
        output.join("synthetic-spec.json"),
        serde_json::to_vec_pretty(&spec)?,
    )?;
    fs::write(output.join("SYNTHETIC_ONLY.txt"),
        "Generated deterministic fixture. Contains no market observations. Zero fixture funding is not evidence for a historical funding interval.\n")?;
    println!(
        "{}",
        serde_json::json!({"input_kind":"synthetic", "rows":rows.len(),
        "input_sha256":hash(&rows_bytes)})
    );
    Ok(())
}
