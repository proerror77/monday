//! Cloud evidence and lightweight receipts from already verified ACK artifacts.
//! No model fitting, ledger reconstruction, raw-data download or log parsing lives here.

use super::{existing_output_matches, persist_immutable_bytes, MAX_METRICS_BYTES, METRICS_JSON};
use alpha_domain::{canonical_json_hash, CexBaselineArtifactV1};
use alpha_engine::model_metrics::CexModelMetricsReportV1;
use anyhow::{bail, Context};
use hft_research_manifest::{
    mlp_training::{MlpLearningDiagnosticsV1, MlpPredictionDiagnosticsV1},
    model::CexBaselineModelV1,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};
use zip::ZipArchive;

const SCHEMA: &str = "cex-campaign-evidence-report-v1";
pub(crate) const MAX_WORKSTATION_REPORT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MlpFoldReport {
    pub fold_index: usize,
    pub model_seed: u64,
    pub learning: MlpLearningDiagnosticsV1,
    pub validation_prediction: MlpPredictionDiagnosticsV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RoundEvidenceReport {
    pub round_id: String,
    pub seed: u64,
    pub mission_id: String,
    pub result_bundle_sha256: String,
    pub model_metrics: Option<CexModelMetricsReportV1>,
    pub mlp_folds: Vec<MlpFoldReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CampaignEvidenceReport {
    pub schema_version: String,
    pub report_id: String,
    pub campaign_id: String,
    pub request_sha256: String,
    pub campaign_result_sha256: String,
    pub execution_source_revision: String,
    pub reader_source_revision: String,
    pub training_performed: bool,
    pub metrics_recomputed: bool,
    pub raw_data_required_by_report_consumer: bool,
    pub termination_reason: String,
    pub consumed_trials: usize,
    pub rounds: Vec<RoundEvidenceReport>,
}

impl CampaignEvidenceReport {
    pub(crate) fn new(
        campaign_id: String,
        request_sha256: String,
        campaign_result_sha256: String,
        execution_source_revision: String,
        termination_reason: String,
        consumed_trials: usize,
        rounds: Vec<RoundEvidenceReport>,
    ) -> anyhow::Result<Self> {
        let mut report = Self {
            schema_version: SCHEMA.into(),
            report_id: String::new(),
            campaign_id,
            request_sha256,
            campaign_result_sha256,
            execution_source_revision,
            reader_source_revision: crate::cli::BUILD_SOURCE_REVISION.into(),
            training_performed: false,
            metrics_recomputed: false,
            raw_data_required_by_report_consumer: false,
            termination_reason,
            consumed_trials,
            rounds,
        };
        let mut value = serde_json::to_value(&report)?;
        value
            .as_object_mut()
            .context("report object")?
            .remove("report_id");
        report.report_id = format!("cex-campaign-evidence-{}", canonical_json_hash(&value)?);
        Ok(report)
    }

    pub(crate) fn persist(&self, output: &Path) -> anyhow::Result<serde_json::Value> {
        let bytes = serde_json::to_vec_pretty(self)?;
        // This aggregate stays in ACK/OSS and may legitimately exceed one
        // round's input bound. Only its small receipt is returned by default;
        // consumers must enforce the separate workstation transfer ceiling.
        if !existing_output_matches(output, &bytes)? {
            persist_immutable_bytes(output, &bytes)?;
        }
        Ok(serde_json::json!({
            "report_id": self.report_id, "path": output,
            "sha256": format!("{:x}", Sha256::digest(&bytes)), "bytes": bytes.len(),
            "campaign_result_sha256": self.campaign_result_sha256,
            "training_performed": false, "metrics_recomputed": false,
            "workstation_byte_limit": MAX_WORKSTATION_REPORT_BYTES,
            "fits_workstation_byte_limit": bytes.len() <= MAX_WORKSTATION_REPORT_BYTES,
        }))
    }
}

/// The caller has authenticated the bundle and reconstructed its round evidence.
/// Copy the small native summaries, without rerunning their evaluator or fitter.
pub(crate) fn collect_verified_archive(
    bundle: &Path,
    round_id: &str,
    seed: u64,
    mission_id: &str,
    bundle_sha256: &str,
) -> anyhow::Result<RoundEvidenceReport> {
    let mut archive = ZipArchive::new(File::open(bundle)?)?;
    let model_metrics = if let Some(bytes) = optional_entry(
        &mut archive,
        &format!("results/{METRICS_JSON}"),
        MAX_METRICS_BYTES,
    )? {
        let report: CexModelMetricsReportV1 = serde_json::from_slice(&bytes)?;
        if report.groups.len() != 1 || report.groups[0].cohort.mission_id != mission_id {
            bail!("verified round metric cohort differs from its Mission");
        }
        Some(report)
    } else {
        None
    };
    let mut mlp_folds = Vec::new();
    if let Some(bytes) = optional_entry(
        &mut archive,
        "results/burn-mlp-baseline.json",
        crate::mission_runner::MAX_MLP_BASELINE_BYTES,
    )? {
        let baseline: CexBaselineArtifactV1 = serde_json::from_slice(&bytes)?;
        if baseline.mission_id != mission_id {
            bail!("verified MLP artifact differs from its Mission");
        }
        for fold in baseline.folds {
            if let CexBaselineModelV1::BurnMlpPortableV2 { seed, learning, .. } = fold.model {
                let observation = fold
                    .mlp_observation
                    .context("verified MLP fold has no validation diagnostics")?;
                mlp_folds.push(MlpFoldReport {
                    fold_index: fold.fold_index,
                    model_seed: seed,
                    learning: *learning,
                    validation_prediction: observation.validation_prediction,
                });
            }
        }
    }
    Ok(RoundEvidenceReport {
        round_id: round_id.into(),
        seed,
        mission_id: mission_id.into(),
        result_bundle_sha256: bundle_sha256.into(),
        model_metrics,
        mlp_folds,
    })
}

fn optional_entry(
    archive: &mut ZipArchive<File>,
    name: &str,
    limit: u64,
) -> anyhow::Result<Option<Vec<u8>>> {
    let entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if entry.is_dir() || entry.size() > limit {
        bail!("Campaign report input {name} exceeds its byte bound");
    }
    let mut bytes = Vec::new();
    entry.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("Campaign report input {name} exceeded its byte bound while reading");
    }
    Ok(Some(bytes))
}
