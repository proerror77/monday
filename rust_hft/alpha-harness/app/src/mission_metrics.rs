//! Read-only reporting of completed model evaluations.

use crate::{
    cli::{ModelMetricsArgs, ModelMetricsBenchmark},
    data_mission,
    mission_runner::CexSupervisedModelSelectionV1,
};
use alpha_domain::{CexBaselineModelKindV1, CexResearchContentRefV1};
use alpha_engine::{
    baselines::CexSupervisedModelEvaluationV2,
    model_metrics::{summarize_model_evaluation, CexModelMetricsReportV1, ModelMetricInput},
};
use anyhow::{bail, Context};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
};

pub(crate) const METRICS_JSON: &str = "supervised-model-metrics.json";
pub(crate) const METRICS_CSV: &str = "supervised-model-metrics.csv";
pub(crate) const MAX_BACKTEST_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const MAX_METRICS_BYTES: u64 = 4 * 1024 * 1024;
const MAX_MODEL_FILES: usize = 256;

pub(crate) fn summarize_bytes(bytes: &[u8]) -> anyhow::Result<ModelMetricInput> {
    if bytes.len() as u64 > MAX_BACKTEST_BYTES {
        bail!("model backtest exceeds the reporting byte limit");
    }
    let evaluation: CexSupervisedModelEvaluationV2 =
        serde_json::from_slice(bytes).context("decode completed supervised model evaluation")?;
    summarize_model_evaluation(&evaluation, &format!("{:x}", Sha256::digest(bytes)))
        .map_err(anyhow::Error::msg)
}

pub(crate) fn build_report(
    inputs: Vec<ModelMetricInput>,
    selections: &[CexSupervisedModelSelectionV1],
    benchmark: Option<CexBaselineModelKindV1>,
) -> anyhow::Result<CexModelMetricsReportV1> {
    let mut selected = BTreeMap::<String, CexResearchContentRefV1>::new();
    for selection in selections {
        selection.validate()?;
        if selected
            .insert(
                selection.mission_id.clone(),
                selection.selected_candidate.clone(),
            )
            .is_some()
        {
            bail!("model metrics received duplicate selection evidence for one Mission");
        }
    }
    CexModelMetricsReportV1::new(inputs, &selected, benchmark).map_err(anyhow::Error::msg)
}

pub(crate) fn persist_report(
    report: &CexModelMetricsReportV1,
    json_path: &Path,
    csv_path: &Path,
) -> anyhow::Result<()> {
    if json_path == csv_path {
        bail!("model metric JSON and CSV outputs must have different paths");
    }
    let json = serde_json::to_vec_pretty(report)?;
    let csv = report.to_csv();
    if json.len() as u64 > MAX_METRICS_BYTES || csv.len() as u64 > MAX_METRICS_BYTES {
        bail!("model metrics exceed the report byte limit");
    }
    // Validate both existing destinations before committing either file.
    let json_exists = existing_output_matches(json_path, &json)?;
    let csv_exists = existing_output_matches(csv_path, csv.as_bytes())?;
    if !json_exists {
        persist_immutable_bytes(json_path, &json)?;
    }
    if !csv_exists {
        persist_immutable_bytes(csv_path, csv.as_bytes())?;
    }
    if !existing_output_matches(json_path, &json)?
        || !existing_output_matches(csv_path, csv.as_bytes())?
    {
        bail!("model metric output readback failed");
    }
    Ok(())
}

pub(crate) fn run(args: ModelMetricsArgs) -> anyhow::Result<()> {
    if args.backtest.is_empty()
        || args.backtest.len() > MAX_MODEL_FILES
        || args.selection.len() > MAX_MODEL_FILES
    {
        bail!("model metrics require 1..={MAX_MODEL_FILES} backtests and bounded selection inputs");
    }
    let mut inputs = Vec::with_capacity(args.backtest.len());
    for path in &args.backtest {
        // Each large evaluation is dropped after its small summary is built.
        // Comparing many Missions therefore does not retain all model ledgers.
        let bytes = read_regular_file(path, MAX_BACKTEST_BYTES)?;
        inputs.push(
            summarize_bytes(&bytes)
                .with_context(|| format!("summarize model backtest {}", path.display()))?,
        );
    }
    let selections: Vec<CexSupervisedModelSelectionV1> = args
        .selection
        .iter()
        .map(|path| {
            serde_json::from_slice(&read_regular_file(path, 64 * 1024)?)
                .with_context(|| format!("decode model selection {}", path.display()))
        })
        .collect::<anyhow::Result<_>>()?;
    let benchmark = match args.benchmark {
        ModelMetricsBenchmark::Ridge => Some(CexBaselineModelKindV1::Ridge),
        ModelMetricsBenchmark::Cart => Some(CexBaselineModelKindV1::ShallowCart),
        ModelMetricsBenchmark::BurnMlp => Some(CexBaselineModelKindV1::BurnMlp),
        ModelMetricsBenchmark::None => None,
    };
    let report = build_report(inputs, &selections, benchmark)?;
    let csv_path = args
        .csv_output
        .unwrap_or_else(|| args.output.with_extension("csv"));
    persist_report(&report, &args.output, &csv_path)?;
    println!(
        "{}",
        serde_json::json!({
            "report_id": report.report_id,
            "groups": report.groups.len(),
            "model_count": report.model_count(),
            "output": args.output,
            "csv_output": csv_path,
            "training_performed": false,
            "settlement_verified": false,
            "deployment_authority": false,
            "order_submission_authority": false,
        })
    );
    Ok(())
}

fn read_regular_file(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("open metric source {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        bail!(
            "metric source is not a bounded regular file: {}",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("metric source exceeded its byte limit while reading");
    }
    Ok(bytes)
}

fn existing_output_matches(path: &Path, bytes: &[u8]) -> anyhow::Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("inspect existing metric output"),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() != bytes.len() as u64
        || read_regular_file(path, bytes.len() as u64)? != bytes
    {
        bail!(
            "existing metric output differs; refusing to overwrite {}",
            path.display()
        );
    }
    Ok(true)
}

fn persist_immutable_bytes(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = data_mission::temporary_output_file(path, ".monday-model-metrics-")?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    match file.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error)
            if error.error.kind() == std::io::ErrorKind::AlreadyExists
                && existing_output_matches(path, bytes)? =>
        {
            Ok(())
        }
        Err(error) => {
            Err(error.error).context("persist model metrics without overwriting evidence")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_outputs_reuse_only_identical_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("report.json");
        assert!(!existing_output_matches(&path, b"one").unwrap());
        persist_immutable_bytes(&path, b"one").unwrap();
        assert!(existing_output_matches(&path, b"one").unwrap());
        persist_immutable_bytes(&path, b"one").unwrap();
        assert!(persist_immutable_bytes(&path, b"two").is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"one");
    }

    #[test]
    fn metric_sources_enforce_the_byte_bound() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("backtest.json");
        std::fs::write(&path, b"12345").unwrap();
        assert!(read_regular_file(&path, 4).is_err());
        assert_eq!(read_regular_file(&path, 5).unwrap(), b"12345");
    }

    #[cfg(unix)]
    #[test]
    fn metric_outputs_reject_symlinks_even_with_identical_bytes() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target.json");
        let output = root.path().join("report.json");
        std::fs::write(&target, b"one").unwrap();
        std::os::unix::fs::symlink(&target, &output).unwrap();
        assert!(existing_output_matches(&output, b"one").is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"one");
    }
}
