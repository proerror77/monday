use crate::{
    cli::{print_json, RenderCexMissionArgs, ValidationArgs},
    data_mission,
    mission_runner::{
        decode_materialization, normalized_sha256, sha256_file, validate_materialization,
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
use hft_research_manifest::CexReplayDatasetManifestV4;
use serde::Serialize;
use std::path::{Path, PathBuf};

const STABLE_VERSION: &str = "binance-btcusdt-usdm-1s-h5-top5-dynamic-v2";
const STABLE_HYPOTHESIS_ID: &str = "signed-rolling-imbalance-dynamic-v2";
const MIN_ROWS: usize = 1232;
const INITIAL_TRAIN_ROWS: usize = 200;
const VALIDATION_ROWS: usize = 256;
const FOLD_COUNT: usize = 3;
const PURGE_ROWS: usize = 5;
const EMBARGO_ROWS: usize = 1;
const HOLDOUT_ROWS: usize = 256;
const MAX_CANDIDATES: usize = 8;
const MAX_EXPANSIONS: u64 = 256;
const GP_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-dynamic-v2-gp-policy";
const BASELINE_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-baseline-policy";
const WEIGHT_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-weight-policy";
const REPLAY_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-replay-policy";
const SCREENING_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-screening-policy";
const SUBSET_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-subset-policy";
const EVALUATION_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-evaluation-policy";
const HOLDOUT_POLICY_ID: &str = "binance-btcusdt-usdm-1s-h5-top5-holdout-policy";
const FEATURE_FIELDS: [&str; 2] = ["book_imbalance", "spread_bps"];

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RenderedHoldoutPolicyV1 {
    schema_version: &'static str,
    holdout_id: String,
    max_opens: u8,
    state: CexResearchHoldoutStateV1,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RenderReportV1 {
    schema_version: &'static str,
    stable_version: &'static str,
    frozen_input_sha256: String,
    mission_id: String,
    mission_sha256: String,
    gp_policy_content_sha256: String,
    gp_policy_file_sha256: String,
    holdout_policy_content_sha256: String,
    holdout_policy_file_sha256: String,
    feature_manifest_id: String,
    feature_sha256: String,
    materialization_sha256: String,
    row_count: usize,
    source_revision: String,
}

pub fn render_cex(args: RenderCexMissionArgs) -> anyhow::Result<()> {
    let feature_sha256 = sha256_file(&args.feature)?;
    let materialization_sha256 = sha256_file(&args.materialization)?;
    let materialization = decode_materialization(
        &std::fs::read(&args.materialization)
            .with_context(|| format!("read {}", args.materialization.display()))?,
    )?;
    let feature_artifacts = tempfile::tempdir().context("create feature validation directory")?;
    let feature_manifest = import_feature_manifest(
        &materialization.mission_id,
        &args.feature,
        feature_artifacts.path(),
    )?;
    ensure_materialization_scope(&materialization, &feature_manifest, &feature_sha256)?;
    data_mission::validate_cex_replay_features(&materialization.snapshot, &feature_manifest)?;
    let validation = approved_validation(&materialization)?;
    validate_materialization(&materialization, &feature_sha256, &validation)?;
    let dataset = CexReplayDatasetManifestV4::new(
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
    let search_lineage_id = format!("{STABLE_VERSION}-lineage-{}", &frozen_input_sha256[..16]);
    let input_lineage_id = format!("{STABLE_VERSION}-input-{}", &materialization_sha256[..16]);
    let holdout_id = format!("{STABLE_VERSION}-holdout-{}", &frozen_input_sha256[..16]);
    let evaluation_protocol = approved_evaluation_protocol(&materialization)?;
    let search = CexResearchSearchPlanV1 {
        seed: 7,
        budget: SearchBudget {
            max_candidates: MAX_CANDIDATES,
            max_expansions: MAX_EXPANSIONS,
            max_tokens: 0,
            max_seconds: 0,
        },
        max_new_iterations: MAX_CANDIDATES,
    };
    let gp_policy = CexGpPolicyV1::controlled_dynamic_v2(
        GP_POLICY_ID,
        FEATURE_FIELDS.into_iter().map(str::to_string).collect(),
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
            objective: "Test signed rolling imbalance on Binance USD-M BTCUSDT 1s/h5/top5 under governed dynamic-v2 GP"
                .to_string(),
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
                hypothesis_id: STABLE_HYPOTHESIS_ID.to_string(),
                statement: "Signed rolling imbalance predicts the next five one-second buckets on Binance USD-M BTCUSDT top5 depth".to_string(),
                target: CexResearchHypothesisTargetV1 {
                    name: "forward_mid_return".to_string(),
                    horizon: EvaluationLabelSpecV1 {
                        horizon_buckets: materialization.label_horizon_buckets,
                        observation_frequency_millis: materialization.bucket_ms,
                    },
                },
                required_feature_families: FEATURE_FIELDS.into_iter().map(str::to_string).collect(),
                required_template_families: vec!["signed_rolling_imbalance".to_string()],
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
            feature_fields: FEATURE_FIELDS.into_iter().map(str::to_string).collect(),
            search,
            evaluation_protocol,
            holdout: CexResearchHoldoutV1 {
                holdout_id: holdout_id.clone(),
                state: CexResearchHoldoutStateV1::Unopened,
            },
        },
        operational: CexResearchOperationalMetadataV1::default(),
    };
    mission.validate()?;
    let output_dir = create_output_dir(&args.output_dir)?;
    let render_result = (|| -> anyhow::Result<serde_json::Value> {
        let mission_id = mission.semantic_id()?;
        let mission_path = output_dir.join("mission.json");
        let holdout_policy_path = output_dir.join("holdout-policy.json");
        let gp_policy_path = output_dir.join("gp-policy.json");
        let report_path = output_dir.join("render-report.json");
        data_mission::write_json_atomic(&mission_path, &mission)?;
        data_mission::write_json_atomic(&holdout_policy_path, &holdout_policy)?;
        data_mission::write_json_atomic(&gp_policy_path, &gp_policy)?;
        let report = RenderReportV1 {
            schema_version: "cex-mission-render-report-v1",
            stable_version: STABLE_VERSION,
            frozen_input_sha256,
            mission_id,
            mission_sha256: sha256_file(&mission_path)?,
            gp_policy_content_sha256,
            gp_policy_file_sha256: sha256_file(&gp_policy_path)?,
            holdout_policy_content_sha256,
            holdout_policy_file_sha256: sha256_file(&holdout_policy_path)?,
            feature_manifest_id: feature_manifest.manifest_id,
            feature_sha256,
            materialization_sha256,
            row_count: materialization.rows,
            source_revision: materialization.source_revision,
        };
        data_mission::write_json_atomic(&report_path, &report)?;
        Ok(serde_json::json!({
        "mission": mission_path,
        "holdout_policy": holdout_policy_path,
        "gp_policy": gp_policy_path,
        "report": report_path,
        }))
    })();
    let rendered = match render_result {
        Ok(rendered) => rendered,
        Err(error) => {
            if let Err(cleanup_error) = std::fs::remove_dir_all(&output_dir) {
                return Err(error.context(format!(
                    "remove partial render output {}: {cleanup_error}",
                    output_dir.display()
                )));
            }
            return Err(error);
        }
    };
    print_json(&rendered)
}

fn create_output_dir(path: &Path) -> anyhow::Result<PathBuf> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(path.to_path_buf()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!("render output directory already exists: {}", path.display())
        }
        Err(error) => Err(error).with_context(|| format!("create {}", path.display())),
    }
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
        funding_bps: data_mission::cex_snapshot_funding_bps(&materialization.snapshot)?,
        latency_bps: 0.5,
        slippage_bps: 0.0,
        cross_spread: false,
        position_notional_usd: 10_000.0,
        capacity_depth_levels: 5,
        max_book_depth_fraction: 0.1,
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
            funding_bps: data_mission::cex_snapshot_funding_bps(&materialization.snapshot)?,
            latency_bps: 0.5,
            slippage_bps: 0.0,
            cross_spread: false,
            position_notional_usd: 10_000.0,
            capacity_depth_levels: 5,
            max_book_depth_fraction: 0.1,
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
    for field in FEATURE_FIELDS
        .into_iter()
        .chain(["mid_price", "bid_depth_top5", "ask_depth_top5"])
    {
        if !manifest.feature_names.iter().any(|name| name == field) {
            bail!("approved Mission render requires feature field {field}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::CEX_GP_POLICY_SCHEMA_V2;
    use chrono::{Duration as ChronoDuration, Utc};
    use hft_collector::{DataModality, PointInTimeFeatureRow};
    use sha2::{Digest, Sha256};
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn render_cex_writes_a_dynamic_v2_mission_bundle() {
        let fixture = Fixture::new(1232);
        let feature_sha256 = sha256_file(&fixture.feature_path).unwrap();
        let materialization_sha256 = sha256_file(&fixture.materialization_path).unwrap();
        render_cex(fixture.args()).unwrap();

        let mission: CexResearchMissionArtifactV1 = serde_json::from_slice(
            &std::fs::read(fixture.output_dir.join("mission.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(mission.spec.search.budget.max_candidates, MAX_CANDIDATES);
        assert_eq!(mission.spec.search.budget.max_expansions, MAX_EXPANSIONS);
        assert_eq!(
            mission
                .spec
                .evaluation_protocol
                .walk_forward
                .validation_rows,
            VALIDATION_ROWS
        );
        assert_eq!(
            mission
                .spec
                .evaluation_protocol
                .walk_forward
                .sealed_holdout_rows,
            HOLDOUT_ROWS
        );
        assert_eq!(mission.spec.evaluation_protocol.costs.fee_bps, 2.0);
        assert_eq!(
            mission.spec.evaluation_protocol.costs.position_notional_usd,
            10_000.0
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
            0.1
        );
        assert_eq!(
            mission.spec.hypotheses[0].required_template_families,
            vec!["signed_rolling_imbalance".to_string()]
        );
        let gp_policy: CexGpPolicyV1 = serde_json::from_slice(
            &std::fs::read(fixture.output_dir.join("gp-policy.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(gp_policy.schema_version, CEX_GP_POLICY_SCHEMA_V2);
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.output_dir.join("render-report.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(report["feature_sha256"], feature_sha256);
        assert_eq!(report["materialization_sha256"], materialization_sha256);
        assert_ne!(report["feature_sha256"], report["materialization_sha256"]);
        let mut output_names = std::fs::read_dir(&fixture.output_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        output_names.sort();
        assert_eq!(
            output_names,
            [
                "gp-policy.json",
                "holdout-policy.json",
                "mission.json",
                "render-report.json",
            ]
        );
    }

    #[test]
    fn render_cex_rejects_short_materializations() {
        let fixture = Fixture::new(MIN_ROWS - 1);
        let error = render_cex(fixture.args()).unwrap_err();
        assert!(format!("{error:#}").contains("at least 1232 point-in-time rows"));
        assert!(!fixture.output_dir.exists());
    }

    #[test]
    fn render_cex_rejects_feature_source_drift_without_leaving_output() {
        let fixture = Fixture::with_feature_source(MIN_ROWS, "d".repeat(64));
        let error = render_cex(fixture.args()).unwrap_err();
        assert!(format!("{error:#}").contains("feature source revision"));
        assert!(!fixture.output_dir.exists());
    }

    struct Fixture {
        _root: tempfile::TempDir,
        feature_path: PathBuf,
        materialization_path: PathBuf,
        output_dir: PathBuf,
    }

    impl Fixture {
        fn new(rows: usize) -> Self {
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
            let output_dir = root.path().join("rendered");
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
            let reference_evidence = "0123456789abcdef"
                .chars()
                .map(cex_triplet)
                .collect::<Vec<_>>();
            let snapshot = hft_research_manifest::CexReplaySnapshotV4 {
                schema_version: hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V4.to_string(),
                venue: "binance".to_string(),
                instrument_type: "usdm".to_string(),
                symbol: "BTCUSDT".to_string(),
                replay_clock: hft_research_manifest::CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
                required_modalities: BTreeSet::from([
                    hft_research_manifest::CEX_MODALITY_LOB.to_string(),
                    hft_research_manifest::CEX_MODALITY_AGGREGATE_TRADE.to_string(),
                    hft_research_manifest::CEX_MODALITY_FUNDING.to_string(),
                    hft_research_manifest::CEX_MODALITY_OPEN_INTEREST.to_string(),
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
                    evidence: vec![cex_triplet('e')],
                },
                derivatives_reference: Some(hft_research_manifest::CexDerivativesReferenceV2 {
                    funding: hft_research_manifest::CexPitSeriesEvidenceV2 {
                        evidence: reference_evidence.clone(),
                        first_available_at: first_event_time - ChronoDuration::seconds(1),
                        last_available_at: last_event_time + ChronoDuration::seconds(5),
                        observations: reference_evidence.len() as u64,
                        max_gap_ns: 90_000_000_000,
                    },
                    open_interest: hft_research_manifest::CexPitSeriesEvidenceV2 {
                        evidence: reference_evidence.clone(),
                        first_available_at: first_event_time - ChronoDuration::seconds(1),
                        last_available_at: last_event_time + ChronoDuration::seconds(5),
                        observations: reference_evidence.len() as u64,
                        max_gap_ns: 90_000_000_000,
                    },
                    evaluation_funding_bps_per_bucket: "0".to_string(),
                }),
            };
            let snapshot_sha256 = snapshot.sha256();
            let report = serde_json::json!({
                "dataset_kind": "lob_point_in_time_materialization",
                "schema_version": hft_research_manifest::BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V5,
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
                output_dir,
            }
        }

        fn args(&self) -> RenderCexMissionArgs {
            RenderCexMissionArgs {
                feature: self.feature_path.clone(),
                materialization: self.materialization_path.clone(),
                output_dir: self.output_dir.clone(),
            }
        }
    }

    fn feature_rows(count: usize, source_revision: &str) -> Vec<PointInTimeFeatureRow> {
        let ingestion_time = Utc::now();
        let start = ingestion_time - ChronoDuration::seconds(count as i64 + 10);
        (0..count)
            .map(|index| PointInTimeFeatureRow {
                event_time: start + ChronoDuration::seconds(index as i64),
                feature_available_time: start + ChronoDuration::seconds(index as i64),
                label_available_time: start + ChronoDuration::seconds(index as i64 + 5),
                ingestion_time,
                symbol: "BTCUSDT".to_string(),
                source_revisions: BTreeMap::from([(
                    "binance-usdm-lob".to_string(),
                    source_revision.to_string(),
                )]),
                modalities: BTreeSet::from([
                    DataModality::Lob,
                    DataModality::TradeTick,
                    DataModality::Funding,
                    DataModality::OpenInterest,
                ]),
                features: BTreeMap::from([
                    ("ask_depth_top5".to_string(), 10.0),
                    ("bid_depth_top5".to_string(), 10.0),
                    ("book_imbalance".to_string(), 0.1),
                    ("mid_price".to_string(), 60_000.0),
                    ("funding_rate".to_string(), 0.0001),
                    ("open_interest".to_string(), 12_345.0),
                    ("spread_bps".to_string(), 1.2),
                ]),
                label: 0.0001,
            })
            .collect()
    }

    fn cex_triplet(byte: char) -> hft_research_manifest::CexArtifactTripletV2 {
        hft_research_manifest::CexArtifactTripletV2 {
            data_sha256: byte.to_string().repeat(64),
            manifest_sha256: byte.to_string().repeat(64),
            success_sha256: byte.to_string().repeat(64),
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
}
