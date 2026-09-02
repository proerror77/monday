use crate::{
    cli::ValidationArgs,
    data_mission,
    mission_runner::{
        decode_materialization, normalized_sha256, sha256_file, validate_materialization,
        MAX_FEATURE_BYTES, MAX_MATERIALIZATION_BYTES,
    },
};
use alpha_domain::{
    canonical_json_hash, CexBaselinePolicyV1, CexEqualAbsoluteWeightPolicyV1,
    CexEventReplayPolicyV1, CexGpPolicyV1, CexResearchContentRefV1, CexResearchEvidenceKindV1,
    CexResearchEvidenceRefV1, CexResearchFalsificationTestV1, CexResearchHoldoutStateV1,
    CexResearchHoldoutV1, CexResearchHypothesisTargetV1, CexResearchHypothesisV1,
    CexResearchInputBindingsV1, CexResearchInstrumentV1, CexResearchMarketV1,
    CexResearchMissionArtifactV1, CexResearchMissionSpecV1, CexResearchOperationalMetadataV1,
    CexResearchPolicyBindingsV1, CexResearchSearchPlanV1, CexResearchVenueV1, EvaluationCostsV1,
    EvaluationLabelSpecV1, EvaluationProtocolV1, EvaluationWalkForwardV1, SearchBudget,
    CEX_RESEARCH_MISSION_SCHEMA_V1,
};
use anyhow::{bail, Context};
use hft_collector::{import_feature_dataset, FeatureDatasetManifest};
use hft_research_manifest::CexReplayDatasetManifestV5;
use serde::{Deserialize, Serialize};
use std::path::Path;

const STABLE_VERSION: &str = "binance-btcusdt-usdm-1s-h5-top5-factor-plan-v5";
const STABLE_HYPOTHESIS_ID: &str = "l2-microstructure-factor-plan-v5";
const RESEARCH_PLAN_SCHEMA_V1: &str = "cex-campaign-research-plan-v1";
pub(crate) const MAX_RESEARCH_PLAN_GENERATION: u8 = 3;
const INITIAL_TRAIN_ROWS: usize = 7_200;
const VALIDATION_ROWS: usize = 3_600;
const FOLD_COUNT: usize = 3;
const PURGE_ROWS: usize = 5;
const EMBARGO_ROWS: usize = 1;
const HOLDOUT_ROWS: usize = 3_600;
const MIN_ROWS: usize =
    INITIAL_TRAIN_ROWS + FOLD_COUNT * (VALIDATION_ROWS + EMBARGO_ROWS) + PURGE_ROWS + HOLDOUT_ROWS;
const MAX_EXPANSIONS: u64 = 256;
const GP_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-factor-plan-v5-gp-policy";
const BASELINE_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-baseline-policy";
const WEIGHT_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-weight-policy";
const REPLAY_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-replay-policy";
const SCREENING_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-screening-policy";
const SUBSET_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-subset-policy";
const EVALUATION_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-evaluation-policy";
const HOLDOUT_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-holdout-policy";
const FEATURE_FIELDS: [&str; 8] = [
    "ask_depth_top5",
    "bid_depth_top5",
    "book_imbalance",
    "book_imbalance_top5",
    "near_depth_concentration_skew_top5",
    "spread_bps",
    "vwap_center_deviation_top5_bps",
    "weighted_book_imbalance_top5",
];
const REQUIRED_NAMED_TEMPLATE_FIELDS: [&str; 3] = [
    "book_imbalance",
    "spread_bps",
    "weighted_book_imbalance_top5",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CexCampaignResearchPlanV1 {
    pub(crate) schema_version: String,
    pub(crate) generation: u8,
    pub(crate) objective: String,
    pub(crate) hypothesis: String,
    pub(crate) focus_field: String,
    pub(crate) feature_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent: Option<CexCampaignResearchParentV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) llm: Option<CexCampaignLlmProvenanceV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CexCampaignResearchParentV1 {
    pub(crate) campaign_id: String,
    pub(crate) request_sha256: String,
    pub(crate) campaign_result_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CexCampaignLlmProvenanceV1 {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) prompt_sha256: String,
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
    pub(crate) total_tokens: u64,
}

impl CexCampaignResearchPlanV1 {
    pub(crate) fn canonical() -> Self {
        Self {
            schema_version: RESEARCH_PLAN_SCHEMA_V1.to_string(),
            generation: 0,
            objective: "Generate and screen continuous L2 microstructure factors, including inverse spread, cross-depth pressure consensus, top-five depth concentration, and VWAP-center displacement, then evaluate Ridge and shallow CART with purged walk-forward OOS predictions on Binance USD-M BTCUSDT 1s/h5/top5 under governed dynamic-v4 GP"
                .to_string(),
            hypothesis: "L1 pressure, top-five depth balance, inverse spread, cross-depth pressure consensus, near-touch depth concentration, linearly weighted top-five pressure, and top-five VWAP-center displacement predict the next five one-second BTCUSDT mid-price returns"
                .to_string(),
            focus_field: "book_imbalance_top5".to_string(),
            feature_fields: FEATURE_FIELDS.into_iter().map(str::to_string).collect(),
            parent: None,
            llm: None,
        }
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != RESEARCH_PLAN_SCHEMA_V1 {
            bail!("CEX Campaign research plan schema_version must be {RESEARCH_PLAN_SCHEMA_V1}");
        }
        for (label, value) in [
            ("objective", self.objective.as_str()),
            ("hypothesis", self.hypothesis.as_str()),
            ("focus_field", self.focus_field.as_str()),
        ] {
            if value.trim().is_empty() || value.len() > 4_096 || value.chars().any(char::is_control)
            {
                bail!("CEX Campaign research plan {label} is invalid");
            }
        }
        let allowed = FEATURE_FIELDS
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if self.feature_fields.is_empty()
            || self
                .feature_fields
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self
                .feature_fields
                .iter()
                .any(|field| !allowed.contains(field.as_str()))
            || !self
                .feature_fields
                .iter()
                .any(|field| field == &self.focus_field)
            || REQUIRED_NAMED_TEMPLATE_FIELDS
                .iter()
                .any(|required| !self.feature_fields.iter().any(|field| field == required))
        {
            bail!("CEX Campaign research plan feature fields are invalid");
        }
        match (&self.parent, &self.llm) {
            (None, None) if self.generation == 0 => {}
            (Some(parent), Some(llm)) => {
                if self.generation == 0 || self.generation > MAX_RESEARCH_PLAN_GENERATION {
                    bail!("CEX Campaign follow-up generation is outside the bounded loop");
                }
                if !parent.campaign_id.starts_with("cex-campaign-")
                    || parent.campaign_id.len() > 63
                    || parent.campaign_id.bytes().any(|byte| {
                        !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                    })
                    || normalized_sha256("parent Campaign request", &parent.request_sha256)?
                        != parent.request_sha256
                    || normalized_sha256("parent Campaign result", &parent.campaign_result_sha256)?
                        != parent.campaign_result_sha256
                {
                    bail!("CEX Campaign research plan parent binding is invalid");
                }
                for (label, value) in [
                    ("LLM provider", llm.provider.as_str()),
                    ("LLM model", llm.model.as_str()),
                ] {
                    if value.trim().is_empty()
                        || value.len() > 256
                        || value.chars().any(char::is_control)
                    {
                        bail!("CEX Campaign research plan {label} is invalid");
                    }
                }
                if normalized_sha256("LLM prompt", &llm.prompt_sha256)? != llm.prompt_sha256
                    || llm.prompt_tokens == 0
                    || llm.completion_tokens == 0
                    || llm.total_tokens != llm.prompt_tokens.saturating_add(llm.completion_tokens)
                {
                    bail!("CEX Campaign research plan LLM provenance is invalid");
                }
            }
            _ => bail!("CEX Campaign follow-up requires both parent and LLM provenance"),
        }
        Ok(())
    }

    pub(crate) fn content_hash(&self) -> anyhow::Result<String> {
        self.validate()?;
        canonical_json_hash(self).map_err(anyhow::Error::new)
    }

    pub(crate) fn max_candidates(&self) -> anyhow::Result<usize> {
        self.validate()?;
        self.feature_fields
            .len()
            .checked_mul(2)
            .and_then(|count| count.checked_add(4))
            .context("CEX Campaign research plan candidate budget overflowed")
    }
}

pub(crate) fn allowed_research_feature_fields() -> Vec<String> {
    FEATURE_FIELDS.into_iter().map(str::to_string).collect()
}

pub(crate) fn required_named_template_fields() -> Vec<String> {
    REQUIRED_NAMED_TEMPLATE_FIELDS
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[derive(Debug)]
pub(crate) struct RenderedCexMission {
    pub(crate) mission: CexResearchMissionArtifactV1,
    pub(crate) mission_id: String,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RenderedHoldoutPolicyV1 {
    schema_version: &'static str,
    holdout_id: String,
    max_opens: u8,
    state: CexResearchHoldoutStateV1,
}

fn sealed_holdout_cohort_sha256(
    materialization: &crate::mission_runner::Materialization,
) -> anyhow::Result<String> {
    Ok(canonical_json_hash(&serde_json::json!({
        "venue": materialization.snapshot.venue,
        "market": materialization.market,
        "symbol": materialization.symbol,
        "source_revision": materialization.source_revision,
        "source_segments": materialization.snapshot.source_segments,
        "bucket_ms": materialization.bucket_ms,
        "label_horizon_buckets": materialization.label_horizon_buckets,
        "rows": materialization.rows,
        "first_event_time": materialization.snapshot.first_event_time,
        "last_event_time": materialization.snapshot.last_event_time,
        "sealed_holdout_rows": HOLDOUT_ROWS,
    }))?)
}

#[allow(dead_code)]
pub(crate) fn render_cex_bundle(
    feature: &Path,
    materialization_path: &Path,
    research_plan: &CexCampaignResearchPlanV1,
    seed: u64,
    multiple_testing_trials: usize,
) -> anyhow::Result<RenderedCexMission> {
    research_plan.validate()?;
    validate_input_sizes(feature, materialization_path)?;
    let feature_sha256 = sha256_file(feature)?;
    let materialization_sha256 = sha256_file(materialization_path)?;
    let materialization = decode_materialization(
        &std::fs::read(materialization_path)
            .with_context(|| format!("read {}", materialization_path.display()))?,
    )?;
    let feature_artifacts = tempfile::tempdir().context("create feature validation directory")?;
    let feature_manifest = import_feature_manifest(
        &materialization.mission_id,
        feature,
        feature_artifacts.path(),
    )?;
    ensure_materialization_scope(&materialization, &feature_manifest, &feature_sha256)?;
    data_mission::validate_cex_replay_features(&materialization.snapshot, &feature_manifest)?;
    let validation = approved_validation(&materialization)?;
    validate_materialization(&materialization, &feature_sha256, &validation)?;
    let dataset = CexReplayDatasetManifestV5::new(
        feature_manifest.manifest_id.clone(),
        materialization.snapshot.clone(),
    )
    .context("construct CEX replay dataset manifest")?;
    let dataset_sha256 = canonical_json_hash(&dataset)?;
    let feature_sha256 = normalized_sha256("feature", &feature_sha256)?;
    let materialization_sha256 = normalized_sha256("materialization", &materialization_sha256)?;
    let snapshot_sha256 = materialization.snapshot.sha256();
    let frozen_input_sha256 = canonical_json_hash(&serde_json::json!({
        "stable_version": STABLE_VERSION,
        "feature_sha256": feature_sha256,
        "materialization_sha256": materialization_sha256,
        "snapshot_sha256": snapshot_sha256,
        "source_revision": materialization.source_revision,
    }))?;
    let sealed_holdout_cohort_sha256 = sealed_holdout_cohort_sha256(&materialization)?;
    let research_plan_sha256 = research_plan.content_hash()?;
    let stable_version = if research_plan == &CexCampaignResearchPlanV1::canonical() {
        STABLE_VERSION.to_string()
    } else {
        format!("{STABLE_VERSION}-plan-{}", &research_plan_sha256[..16])
    };
    let gp_policy_id = if research_plan == &CexCampaignResearchPlanV1::canonical() {
        GP_POLICY_ID.to_string()
    } else {
        format!("{GP_POLICY_ID}-{}", &research_plan_sha256[..16])
    };
    let hypothesis_id = if research_plan == &CexCampaignResearchPlanV1::canonical() {
        STABLE_HYPOTHESIS_ID.to_string()
    } else {
        format!("l2-microstructure-followup-{}", &research_plan_sha256[..16])
    };
    let search_lineage_id = format!(
        "{stable_version}-lineage-seed-{seed}-{}",
        &frozen_input_sha256[..16]
    );
    let input_lineage_id = format!("{stable_version}-input-{}", &materialization_sha256[..16]);
    let holdout_id = format!("cex-holdout-{}", &sealed_holdout_cohort_sha256[..48]);
    let evaluation_protocol = approved_evaluation_protocol(&materialization)?;
    let search = CexResearchSearchPlanV1 {
        seed,
        budget: SearchBudget {
            max_candidates: research_plan.max_candidates()?,
            max_expansions: MAX_EXPANSIONS,
            max_tokens: 0,
            max_seconds: 0,
        },
        max_new_iterations: research_plan.max_candidates()?,
        multiple_testing_trials,
    };
    let gp_policy = CexGpPolicyV1::controlled_dynamic_v4(
        gp_policy_id,
        research_plan.feature_fields.clone(),
        search.seed,
        &search.budget,
    )?;
    let gp_policy_content_sha256 = gp_policy.content_hash()?;
    let baseline_policy = CexBaselinePolicyV1::controlled_v1(BASELINE_POLICY_ID)?;
    let weight_policy = CexEqualAbsoluteWeightPolicyV1::controlled_v1(WEIGHT_POLICY_ID)?;
    let replay_policy = CexEventReplayPolicyV1::controlled_v1(
        REPLAY_POLICY_ID,
        materialization.top_depth,
        materialization.bucket_ms,
    )?;
    let holdout_policy = RenderedHoldoutPolicyV1 {
        schema_version: "cex-holdout-policy-v1",
        holdout_id: holdout_id.clone(),
        max_opens: 1,
        state: CexResearchHoldoutStateV1::Unopened,
    };
    let holdout_policy_content_sha256 = canonical_json_hash(&holdout_policy)?;
    let partition_sha256 = canonical_json_hash(&materialization.snapshot.source_segments)?;
    let mission = CexResearchMissionArtifactV1 {
        schema_version: CEX_RESEARCH_MISSION_SCHEMA_V1.to_string(),
        spec: CexResearchMissionSpecV1 {
            objective: research_plan.objective.clone(),
            search_lineage_id,
            data_mission_id: materialization.mission_id.clone(),
            instrument: CexResearchInstrumentV1 {
                venue: CexResearchVenueV1::Binance,
                market: CexResearchMarketV1::Usdm,
                symbol: "BTCUSDT".to_string(),
                horizon: EvaluationLabelSpecV1 {
                    horizon_buckets: materialization.label_horizon_buckets,
                    observation_frequency_millis: materialization.bucket_ms,
                },
            },
            hypotheses: vec![CexResearchHypothesisV1 {
                hypothesis_id,
                statement: research_plan.hypothesis.clone(),
                target: CexResearchHypothesisTargetV1 {
                    name: "forward_mid_return".to_string(),
                    horizon: EvaluationLabelSpecV1 {
                        horizon_buckets: materialization.label_horizon_buckets,
                        observation_frequency_millis: materialization.bucket_ms,
                    },
                },
                required_feature_families: research_plan.feature_fields.clone(),
                required_template_families: vec![
                    "atomic_l2_microstructure".to_string(),
                    "named_composite_l2_microstructure".to_string(),
                ],
                falsification_tests: vec![
                    CexResearchFalsificationTestV1 {
                        test_id: "purged-predictive-gate".to_string(),
                        reject_when: "the governed purged walk-forward IC, RankIC, ICIR, or positive-fold thresholds fail".to_string(),
                    },
                    CexResearchFalsificationTestV1 {
                        test_id: "post-cost-capacity-gate".to_string(),
                        reject_when: "declared post-cost return, drawdown, trade-count, or top-five-depth capacity evidence fails".to_string(),
                    },
                ],
                source_evidence_ids: vec!["input-materialization-evidence".to_string()],
            }],
            inputs: CexResearchInputBindingsV1 {
                dataset: CexResearchContentRefV1 {
                    id: dataset.manifest_id.clone(),
                    content_sha256: dataset_sha256,
                },
                snapshot: CexResearchContentRefV1 {
                    id: format!("cex-replay-snapshot-{snapshot_sha256}"),
                    content_sha256: snapshot_sha256,
                },
                partition: CexResearchContentRefV1 {
                    id: format!("cex-replay-partition-{partition_sha256}"),
                    content_sha256: partition_sha256,
                },
                source: CexResearchContentRefV1 {
                    id: materialization.source_revision.clone(),
                    content_sha256: materialization.source_revision.clone(),
                },
                feature: CexResearchContentRefV1 {
                    id: feature_manifest.manifest_id.clone(),
                    content_sha256: feature_sha256.clone(),
                },
                materialization: CexResearchContentRefV1 {
                    id: materialization.mission_id.clone(),
                    content_sha256: materialization_sha256.clone(),
                },
            },
            policies: CexResearchPolicyBindingsV1 {
                gp: CexResearchContentRefV1 {
                    id: gp_policy.policy_id.clone(),
                    content_sha256: gp_policy_content_sha256.clone(),
                },
                screening: CexResearchContentRefV1 {
                    id: SCREENING_POLICY_ID.to_string(),
                    content_sha256: canonical_json_hash(
                        &alpha_domain::FormulaEvaluatorConfig::for_trials(
                            search.planned_gp_and_subset_trials()?,
                        )?,
                    )?,
                },
                baseline: CexResearchContentRefV1 {
                    id: baseline_policy.policy_id.clone(),
                    content_sha256: baseline_policy.content_hash()?,
                },
                subset_search: CexResearchContentRefV1 {
                    id: SUBSET_POLICY_ID.to_string(),
                    content_sha256: canonical_json_hash(&search)?,
                },
                weight: CexResearchContentRefV1 {
                    id: weight_policy.policy_id.clone(),
                    content_sha256: weight_policy.content_hash()?,
                },
                evaluation: CexResearchContentRefV1 {
                    id: EVALUATION_POLICY_ID.to_string(),
                    content_sha256: evaluation_protocol.content_hash()?,
                },
                replay: CexResearchContentRefV1 {
                    id: replay_policy.policy_id.clone(),
                    content_sha256: replay_policy.content_hash()?,
                },
                holdout: CexResearchContentRefV1 {
                    id: HOLDOUT_POLICY_ID.to_string(),
                    content_sha256: holdout_policy_content_sha256.clone(),
                },
            },
            evidence: vec![CexResearchEvidenceRefV1 {
                evidence_id: "input-materialization-evidence".to_string(),
                kind: CexResearchEvidenceKindV1::TrainingValidation,
                source_mission_id: materialization.mission_id.clone(),
                source_search_lineage_id: input_lineage_id,
                artifact_sha256: materialization_sha256.clone(),
                signature: None,
                holdout_id: None,
            }],
            feature_fields: research_plan.feature_fields.clone(),
            search,
            evaluation_protocol,
            holdout: CexResearchHoldoutV1 {
                holdout_id,
                state: CexResearchHoldoutStateV1::Unopened,
            },
        },
        operational: CexResearchOperationalMetadataV1::default(),
    };
    mission.validate()?;
    let mission_id = mission.semantic_id()?;
    Ok(RenderedCexMission {
        mission,
        mission_id,
    })
}

fn validate_input_sizes(feature: &Path, materialization: &Path) -> anyhow::Result<()> {
    if feature.metadata()?.len() > MAX_FEATURE_BYTES
        || materialization.metadata()?.len() > MAX_MATERIALIZATION_BYTES
    {
        bail!("source exceeds the allowed size");
    }
    Ok(())
}

fn import_feature_manifest(
    mission_id: &str,
    input: &Path,
    artifact_dir: &Path,
) -> anyhow::Result<FeatureDatasetManifest> {
    import_feature_dataset(mission_id.to_string(), input, artifact_dir)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("import feature dataset from {}", input.display()))
}

fn approved_validation(
    materialization: &crate::mission_runner::Materialization,
) -> anyhow::Result<ValidationArgs> {
    Ok(ValidationArgs {
        initial_train_rows: INITIAL_TRAIN_ROWS,
        validation_rows: VALIDATION_ROWS,
        fold_count: FOLD_COUNT,
        purge_rows: PURGE_ROWS,
        embargo_rows: EMBARGO_ROWS,
        sealed_holdout_rows: HOLDOUT_ROWS,
        fee_bps: 2.0,
        rebate_bps: 0.0,
        funding_bps: 0.0,
        latency_bps: 0.5,
        slippage_bps: 0.0,
        cross_spread: true,
        position_notional_usd: 1_000.0,
        capacity_depth_levels: 5,
        max_book_depth_fraction: 0.05,
        label_horizon_buckets: materialization.label_horizon_buckets,
        observation_frequency_millis: materialization.bucket_ms,
    })
}

fn approved_evaluation_protocol(
    materialization: &crate::mission_runner::Materialization,
) -> anyhow::Result<EvaluationProtocolV1> {
    EvaluationProtocolV1::new(
        EvaluationWalkForwardV1 {
            initial_train_rows: INITIAL_TRAIN_ROWS,
            validation_rows: VALIDATION_ROWS,
            fold_count: FOLD_COUNT,
            purge_rows: PURGE_ROWS,
            embargo_rows: EMBARGO_ROWS,
            sealed_holdout_rows: HOLDOUT_ROWS,
        },
        EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            slippage_bps: 0.0,
            cross_spread: true,
            position_notional_usd: 1_000.0,
            capacity_depth_levels: 5,
            max_book_depth_fraction: 0.05,
        },
        EvaluationLabelSpecV1 {
            horizon_buckets: materialization.label_horizon_buckets,
            observation_frequency_millis: materialization.bucket_ms,
        },
    )
    .map_err(anyhow::Error::new)
}

fn ensure_materialization_scope(
    materialization: &crate::mission_runner::Materialization,
    manifest: &FeatureDatasetManifest,
    feature_sha256: &str,
) -> anyhow::Result<()> {
    if materialization.market != "usdm"
        || materialization.symbol != "BTCUSDT"
        || materialization.bucket_ms != 1_000
        || materialization.label_horizon_buckets != 5
        || materialization.top_depth != 5
    {
        bail!("only the approved Binance USD-M BTCUSDT 1s/h5/top5 materialization can render this Mission");
    }
    if materialization.rows < MIN_ROWS || manifest.rows < MIN_ROWS {
        bail!("approved Mission render requires at least {MIN_ROWS} point-in-time rows");
    }
    if materialization.rows != manifest.rows {
        bail!("materialization evidence row count does not match the feature artifact");
    }
    if materialization.series_count != manifest.series_count {
        bail!("materialization evidence series count does not match the feature artifact");
    }
    if materialization.artifact_sha256 != feature_sha256
        || materialization.snapshot.feature_artifact_sha256 != feature_sha256
    {
        bail!("materialization evidence does not bind the supplied feature artifact");
    }
    if manifest.manifest_id != format!("dataset-{feature_sha256}")
        || manifest.artifact_sha256 != feature_sha256
    {
        bail!("feature manifest does not bind the supplied feature artifact");
    }
    for field in FEATURE_FIELDS.into_iter().chain(["mid_price"]) {
        if !manifest.feature_names.iter().any(|name| name == field) {
            bail!("approved Mission render requires feature field {field}");
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use hft_collector::{DataModality, PointInTimeFeatureRow};
    use sha2::{Digest, Sha256};
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
    };

    fn default_trials() -> usize {
        CexCampaignResearchPlanV1::canonical()
            .max_candidates()
            .unwrap()
            * 2
    }

    #[test]
    fn cloud_renderer_builds_an_l2_factor_plan_v5_mission() {
        let fixture = Fixture::new(MIN_ROWS);
        let rendered = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap();
        let mission = rendered.mission;
        assert_eq!(MIN_ROWS, 21_608);
        assert_eq!(mission.spec.search.budget.max_candidates, 20);
        assert_eq!(
            mission.spec.search.planned_gp_and_subset_trials().unwrap(),
            40
        );
        assert_eq!(mission.spec.search.budget.max_expansions, MAX_EXPANSIONS);
        assert_eq!(
            mission.spec.evaluation_protocol.walk_forward,
            EvaluationWalkForwardV1 {
                initial_train_rows: INITIAL_TRAIN_ROWS,
                validation_rows: VALIDATION_ROWS,
                fold_count: FOLD_COUNT,
                purge_rows: PURGE_ROWS,
                embargo_rows: EMBARGO_ROWS,
                sealed_holdout_rows: HOLDOUT_ROWS,
            }
        );
        assert_eq!(mission.spec.evaluation_protocol.costs.fee_bps, 2.0);
        assert_eq!(mission.spec.evaluation_protocol.costs.funding_bps, 0.0);
        assert_eq!(
            mission.spec.evaluation_protocol.costs.position_notional_usd,
            1_000.0
        );
        assert_eq!(
            mission.spec.evaluation_protocol.costs.capacity_depth_levels,
            5
        );
        assert_eq!(
            mission
                .spec
                .evaluation_protocol
                .costs
                .max_book_depth_fraction,
            0.05
        );
        assert!(mission.spec.evaluation_protocol.costs.cross_spread);
        assert_eq!(
            mission.spec.feature_fields,
            FEATURE_FIELDS.map(str::to_string)
        );
        assert_eq!(
            mission.spec.hypotheses[0].hypothesis_id,
            STABLE_HYPOTHESIS_ID
        );
        assert_eq!(
            mission.spec.hypotheses[0].required_template_families,
            vec![
                "atomic_l2_microstructure".to_string(),
                "named_composite_l2_microstructure".to_string(),
            ]
        );
        assert_eq!(mission.spec.policies.gp.id, GP_POLICY_ID);
        let expected_gp = CexGpPolicyV1::controlled_dynamic_v4(
            mission.spec.policies.gp.id.clone(),
            mission.spec.feature_fields.clone(),
            mission.spec.search.seed,
            &mission.spec.search.budget,
        )
        .unwrap();
        expected_gp
            .validate_binding(&mission.spec.policies.gp)
            .unwrap();
        assert!(mission.spec.search_lineage_id.starts_with(STABLE_VERSION));
    }

    #[test]
    fn follow_up_plan_narrows_the_governed_gp_search() {
        let fixture = Fixture::new(MIN_ROWS);
        let plan = CexCampaignResearchPlanV1 {
            schema_version: RESEARCH_PLAN_SCHEMA_V1.to_string(),
            generation: 1,
            objective: "Test one bounded follow-up".to_string(),
            hypothesis: "Near-depth concentration changes predict forward return".to_string(),
            focus_field: "near_depth_concentration_skew_top5".to_string(),
            feature_fields: vec![
                "book_imbalance".to_string(),
                "near_depth_concentration_skew_top5".to_string(),
                "spread_bps".to_string(),
                "weighted_book_imbalance_top5".to_string(),
            ],
            parent: Some(CexCampaignResearchParentV1 {
                campaign_id: format!("cex-campaign-{}", "1".repeat(32)),
                request_sha256: "2".repeat(64),
                campaign_result_sha256: "3".repeat(64),
            }),
            llm: Some(CexCampaignLlmProvenanceV1 {
                provider: "test".to_string(),
                model: "test".to_string(),
                prompt_sha256: "4".repeat(64),
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        };
        let rendered = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &plan,
            7,
            plan.max_candidates().unwrap() * 2,
        )
        .unwrap();

        assert_eq!(rendered.mission.spec.objective, plan.objective);
        assert_eq!(rendered.mission.spec.feature_fields, plan.feature_fields);
        assert_eq!(rendered.mission.spec.search.budget.max_candidates, 12);
        assert_ne!(rendered.mission.spec.policies.gp.id, GP_POLICY_ID);

        let mut invalid = plan.clone();
        invalid.feature_fields.push("open_interest".to_string());
        assert!(invalid.validate().is_err());

        let mut exhausted = plan;
        exhausted.generation = MAX_RESEARCH_PLAN_GENERATION + 1;
        assert!(exhausted.validate().is_err());
    }

    #[test]
    fn render_cex_rejects_short_materializations() {
        let fixture = Fixture::new(MIN_ROWS - 1);
        let error = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains(&format!("at least {MIN_ROWS} point-in-time rows")));
    }

    #[test]
    fn render_cex_rejects_feature_source_drift_without_leaving_output() {
        let fixture = Fixture::with_feature_source(MIN_ROWS, "d".repeat(64));
        let error = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("feature source revision"));
    }

    #[test]
    fn render_cex_rejects_forbidden_current_cex_feature_fields() {
        for field in ["funding_cost_bps", "funding_rate", "open_interest"] {
            let fixture = Fixture::new(MIN_ROWS);
            let mut rows = read_feature_rows(&fixture.feature_path);
            for row in &mut rows {
                row.features.insert(field.to_string(), 0.0001);
            }
            rewrite_feature_rows(&fixture.feature_path, &rows);
            let feature_sha256 = hex::encode(Sha256::digest(
                std::fs::read(&fixture.feature_path).unwrap(),
            ));
            let mut materialization: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&fixture.materialization_path).unwrap())
                    .unwrap();
            materialization["artifact_sha256"] = serde_json::json!(feature_sha256.clone());
            materialization["snapshot"]["feature_artifact_sha256"] =
                serde_json::json!(feature_sha256);
            let snapshot: hft_research_manifest::CexReplaySnapshotV5 =
                serde_json::from_value(materialization["snapshot"].clone()).unwrap();
            materialization["snapshot_sha256"] = serde_json::json!(snapshot.sha256());
            std::fs::write(
                &fixture.materialization_path,
                serde_json::to_vec_pretty(&materialization).unwrap(),
            )
            .unwrap();
            let feature_artifacts = tempfile::tempdir().unwrap();
            let feature_manifest = import_feature_manifest(
                "data-mission-1",
                &fixture.feature_path,
                feature_artifacts.path(),
            )
            .unwrap();
            let materialization = load_materialization(&fixture.materialization_path);
            let error = data_mission::validate_cex_replay_features(
                &materialization.snapshot,
                &feature_manifest,
            )
            .unwrap_err();

            assert!(format!("{error:#}").contains(&format!(
                "current L2-only CEX replay cannot include {field}"
            )));

            assert!(render_cex_bundle(
                &fixture.feature_path,
                &fixture.materialization_path,
                &CexCampaignResearchPlanV1::canonical(),
                7,
                default_trials(),
            )
            .is_err());
        }
    }

    #[test]
    fn render_cex_rejects_execution_oversized_inputs() {
        let fixture = Fixture::new(MIN_ROWS);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.feature_path)
            .unwrap()
            .set_len(MAX_FEATURE_BYTES + 1)
            .unwrap();
        let error = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("source exceeds the allowed size"));

        let fixture = Fixture::new(MIN_ROWS);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.materialization_path)
            .unwrap()
            .set_len(MAX_MATERIALIZATION_BYTES + 1)
            .unwrap();
        let error = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("source exceeds the allowed size"));
    }

    #[test]
    fn render_cex_rejects_materialization_source_revision_drift() {
        let fixture = Fixture::new(MIN_ROWS);
        let mut materialization: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&fixture.materialization_path).unwrap()).unwrap();
        materialization["source_revision"] = serde_json::json!("9".repeat(64));
        std::fs::write(
            &fixture.materialization_path,
            serde_json::to_vec_pretty(&materialization).unwrap(),
        )
        .unwrap();

        let error = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("source revision does not bind"));
    }

    #[test]
    fn render_cex_bundle_changes_search_lineage_per_seed_but_keeps_holdout() {
        let fixture = Fixture::new(MIN_ROWS);
        let first = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap();
        let second = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            11,
            default_trials(),
        )
        .unwrap();

        assert_ne!(first.mission_id, second.mission_id);
        assert_ne!(
            first.mission.spec.search_lineage_id,
            second.mission.spec.search_lineage_id
        );
        assert_eq!(
            first.mission.spec.holdout.holdout_id,
            second.mission.spec.holdout.holdout_id
        );
        assert_holdout_id_format(&first.mission.spec.holdout.holdout_id);
    }

    #[test]
    fn render_cex_bundle_keeps_holdout_for_rematerialized_same_cohort() {
        let fixture = Fixture::new(MIN_ROWS);
        let first = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap();

        let shifted_ingestion_time = read_feature_rows(&fixture.feature_path)[0].ingestion_time
            + ChronoDuration::seconds(60);
        let feature_bytes = std::fs::read_to_string(&fixture.feature_path)
            .unwrap()
            .lines()
            .map(|line| format!("{line} "))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&fixture.feature_path, feature_bytes).unwrap();
        let feature_sha256 = hex::encode(Sha256::digest(
            std::fs::read(&fixture.feature_path).unwrap(),
        ));

        let mut materialization: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&fixture.materialization_path).unwrap()).unwrap();
        materialization["mission_id"] = serde_json::json!("data-mission-rematerialized");
        materialization["artifact_sha256"] = serde_json::json!(feature_sha256);
        materialization["snapshot"]["feature_artifact_sha256"] = serde_json::json!(feature_sha256);
        let snapshot: hft_research_manifest::CexReplaySnapshotV5 =
            serde_json::from_value(materialization["snapshot"].clone()).unwrap();
        materialization["snapshot_sha256"] = serde_json::json!(snapshot.sha256());
        materialization["created_at"] = serde_json::json!(shifted_ingestion_time);
        std::fs::write(
            &fixture.materialization_path,
            serde_json::to_vec_pretty(&materialization).unwrap(),
        )
        .unwrap();

        let second = render_cex_bundle(
            &fixture.feature_path,
            &fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            default_trials(),
        )
        .unwrap();

        assert_eq!(
            first.mission.spec.holdout.holdout_id,
            second.mission.spec.holdout.holdout_id
        );
        assert_holdout_id_format(&first.mission.spec.holdout.holdout_id);
    }

    #[test]
    fn sealed_holdout_cohort_hash_changes_with_window_source_or_label_slice() {
        let fixture = Fixture::new(MIN_ROWS);
        let base = load_materialization(&fixture.materialization_path);
        let base_hash = sealed_holdout_cohort_sha256(&base).unwrap();

        let shifted_window = mutate_materialization(&fixture.materialization_path, |value| {
            let last_event_time = serde_json::from_value::<chrono::DateTime<Utc>>(
                value["snapshot"]["last_event_time"].clone(),
            )
            .unwrap()
                + ChronoDuration::seconds(1);
            value["rows"] = serde_json::json!(MIN_ROWS + 1);
            value["snapshot"]["last_event_time"] = serde_json::json!(last_event_time);
            value["last_event_time"] = serde_json::json!(last_event_time);
        });
        let shifted_window_hash = sealed_holdout_cohort_sha256(&shifted_window).unwrap();
        assert_ne!(base_hash, shifted_window_hash);

        let shifted_source = mutate_materialization(&fixture.materialization_path, |value| {
            value["source_revision"] = serde_json::json!("d".repeat(64));
            value["snapshot"]["source_segments"][0]["content_sha256"] =
                serde_json::json!("e".repeat(64));
        });
        let shifted_source_hash = sealed_holdout_cohort_sha256(&shifted_source).unwrap();
        assert_ne!(base_hash, shifted_source_hash);

        let shifted_label = mutate_materialization(&fixture.materialization_path, |value| {
            value["label_horizon_buckets"] = serde_json::json!(6);
            value["snapshot"]["label_horizon_buckets"] = serde_json::json!(6);
        });
        let shifted_label_hash = sealed_holdout_cohort_sha256(&shifted_label).unwrap();
        assert_ne!(base_hash, shifted_label_hash);
    }

    pub(crate) struct Fixture {
        pub(crate) _root: tempfile::TempDir,
        pub(crate) feature_path: PathBuf,
        pub(crate) materialization_path: PathBuf,
    }

    impl Fixture {
        pub(crate) fn new(rows: usize) -> Self {
            Self::with_optional_feature_source(rows, None)
        }

        fn with_feature_source(rows: usize, feature_source_revision: String) -> Self {
            Self::with_optional_feature_source(rows, Some(feature_source_revision))
        }

        fn with_optional_feature_source(
            rows: usize,
            feature_source_revision: Option<String>,
        ) -> Self {
            let root = tempfile::tempdir().unwrap();
            let feature_path = root.path().join("features.jsonl");
            let materialization_path = root.path().join("materialization.json");
            let source_content_sha256 = "b".repeat(64);
            let source_revision =
                hft_collector::lob_archiver::source_revision([source_content_sha256.as_str()]);
            let feature_source_revision = feature_source_revision
                .as_deref()
                .unwrap_or(&source_revision);
            let rows = feature_rows(rows, feature_source_revision);
            write_feature_rows(&feature_path, &rows);
            let bytes = std::fs::read(&feature_path).unwrap();
            let feature_sha256 = hex::encode(Sha256::digest(&bytes));
            let first_event_time = rows.first().unwrap().event_time;
            let last_event_time = rows.last().unwrap().event_time;
            let ingestion_time = rows.first().unwrap().ingestion_time;
            let source_manifest_sha256 = "c".repeat(64);
            let source_start_ns = u64::try_from(
                (first_event_time - ChronoDuration::seconds(1))
                    .timestamp_nanos_opt()
                    .unwrap(),
            )
            .unwrap();
            let source_end_ns =
                u64::try_from(ingestion_time.timestamp_nanos_opt().unwrap()).unwrap();
            let instrument_rules_evidence = (0..256).map(indexed_cex_triplet).collect::<Vec<_>>();
            let snapshot = hft_research_manifest::CexReplaySnapshotV5 {
                schema_version: hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V5.to_string(),
                venue: "binance".to_string(),
                instrument_type: "usdm".to_string(),
                symbol: "BTCUSDT".to_string(),
                replay_clock: hft_research_manifest::CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
                required_modalities: BTreeSet::from([
                    hft_research_manifest::CEX_MODALITY_LOB.to_string(),
                    hft_research_manifest::CEX_MODALITY_AGGREGATE_TRADE.to_string(),
                ]),
                source_segments: vec![hft_research_manifest::CexReplaySegmentIdentity {
                    content_sha256: source_content_sha256.clone(),
                    manifest_sha256: source_manifest_sha256.clone(),
                    start_received_at_ns: source_start_ns,
                    end_received_at_ns: source_end_ns,
                    events: rows.len() as u64,
                }],
                first_event_time,
                last_event_time,
                feature_artifact_sha256: feature_sha256.clone(),
                feature_availability_policy: hft_research_manifest::CEX_FEATURE_AVAILABILITY_POLICY
                    .to_string(),
                bucket_ms: 1_000,
                label_horizon_buckets: 5,
                top_depth: 5,
                instrument_rules: hft_research_manifest::CexInstrumentRulesV2 {
                    tick_size: "0.1".to_string(),
                    step_size: "0.001".to_string(),
                    min_notional: "5".to_string(),
                    available_at: first_event_time - ChronoDuration::seconds(1),
                    valid_through: last_event_time + ChronoDuration::seconds(5),
                    evidence: instrument_rules_evidence.clone(),
                },
                series: vec![hft_research_manifest::CexReplaySeriesV1 {
                    series_id: 1,
                    first_event_time,
                    last_event_time,
                    instrument_rules_coverage: hft_research_manifest::CexPitSeriesEvidenceV2 {
                        evidence: instrument_rules_evidence,
                        first_available_at: first_event_time - ChronoDuration::seconds(1),
                        last_available_at: last_event_time + ChronoDuration::seconds(5),
                        observations: 256,
                        max_gap_ns: hft_research_manifest::CEX_DERIVATIVES_MAX_GAP_NS,
                    },
                }],
            };
            let snapshot_sha256 = snapshot.sha256();
            let report = serde_json::json!({
                "dataset_kind": "lob_point_in_time_materialization",
                "schema_version": hft_research_manifest::BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7,
                "mission_id": "data-mission-1",
                "symbol": "BTCUSDT",
                "market": "usdm",
                "bucket_ms": 1000,
                "label_horizon_buckets": 5,
                "top_depth": 5,
                "source_revision": source_revision,
                "source_segments": [{
                    "path": "raw/segment.jsonl.zst",
                    "sha256": source_content_sha256,
                    "collector_manifest_sha256": source_manifest_sha256,
                    "success_marker_sha256": hex::encode(Sha256::digest(format!("{source_content_sha256}\n"))),
                    "start_received_at_ns": source_start_ns,
                    "end_received_at_ns": source_end_ns,
                    "events": rows.len()
                }],
                "series_count": 1,
                "rows": rows.len(),
                "first_event_time": first_event_time,
                "last_event_time": last_event_time,
                "artifact_path": feature_path,
                "artifact_sha256": feature_sha256,
                "snapshot": snapshot,
                "snapshot_sha256": snapshot_sha256,
                "created_at": ingestion_time,
            });
            std::fs::write(
                &materialization_path,
                serde_json::to_vec_pretty(&report).unwrap(),
            )
            .unwrap();
            Self {
                _root: root,
                feature_path,
                materialization_path,
            }
        }
    }

    fn feature_rows(count: usize, source_revision: &str) -> Vec<PointInTimeFeatureRow> {
        let ingestion_time = Utc::now();
        let start = ingestion_time - ChronoDuration::seconds(count as i64 + 10);
        (0..count)
            .map(|index| PointInTimeFeatureRow {
                series_id: 1,
                event_time: start + ChronoDuration::seconds(index as i64),
                feature_available_time: start + ChronoDuration::seconds(index as i64),
                label_available_time: start + ChronoDuration::seconds(index as i64 + 5),
                ingestion_time,
                symbol: "BTCUSDT".to_string(),
                source_revisions: BTreeMap::from([(
                    "binance-usdm-lob".to_string(),
                    source_revision.to_string(),
                )]),
                modalities: BTreeSet::from([DataModality::Lob, DataModality::TradeTick]),
                features: BTreeMap::from([
                    ("ask_depth_top5".to_string(), 10.0),
                    ("bid_depth_top5".to_string(), 10.0),
                    ("book_imbalance".to_string(), 0.1),
                    ("book_imbalance_top5".to_string(), 0.08),
                    ("mid_price".to_string(), 60_000.0),
                    ("near_depth_concentration_skew_top5".to_string(), 0.04),
                    ("spread_bps".to_string(), 1.2),
                    ("vwap_center_deviation_top5_bps".to_string(), 0.7),
                    ("weighted_book_imbalance_top5".to_string(), 0.09),
                ]),
                label: 0.0001,
            })
            .collect()
    }

    fn indexed_cex_triplet(index: usize) -> hft_research_manifest::CexArtifactTripletV2 {
        let data_sha256 = hex::encode(Sha256::digest(format!("render-reference-data-{index}")));
        hft_research_manifest::CexArtifactTripletV2 {
            manifest_sha256: hex::encode(Sha256::digest(format!(
                "render-reference-manifest-{index}"
            ))),
            success_sha256: data_sha256.clone(),
            data_sha256,
        }
    }

    fn write_feature_rows(path: &Path, rows: &[PointInTimeFeatureRow]) {
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(path, bytes).unwrap();
    }

    pub(crate) fn rewrite_feature_rows(path: &Path, rows: &[PointInTimeFeatureRow]) {
        write_feature_rows(path, rows);
    }

    pub(crate) fn read_feature_rows(path: &Path) -> Vec<PointInTimeFeatureRow> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn load_materialization(path: &Path) -> crate::mission_runner::Materialization {
        decode_materialization(&std::fs::read(path).unwrap()).unwrap()
    }

    fn mutate_materialization(
        path: &Path,
        mutate: impl FnOnce(&mut serde_json::Value),
    ) -> crate::mission_runner::Materialization {
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        mutate(&mut value);
        decode_materialization(&serde_json::to_vec_pretty(&value).unwrap()).unwrap()
    }

    fn assert_holdout_id_format(holdout_id: &str) {
        assert!(holdout_id.starts_with("cex-holdout-"));
        assert_eq!(holdout_id.len(), 60);
        assert!(holdout_id
            .strip_prefix("cex-holdout-")
            .unwrap()
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()));
    }
}
