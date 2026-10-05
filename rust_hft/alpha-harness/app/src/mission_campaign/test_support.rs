//! Pure request fixtures shared by operator admission and scientific tests.
use super::*;
pub(super) fn valid_request() -> CampaignRequest {
    const TEST_ROOT: &str =
        "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research";
    let research_plan = CexCampaignResearchPlanV1::canonical();
    let round_identity = CampaignRoundIdentityV1 {
        schema_version: CAMPAIGN_ROUND_IDENTITY_SCHEMA_V1.to_string(),
        data_window_hours: CAMPAIGN_DATA_WINDOW_HOURS,
        data_fingerprint_sha256: campaign_data_fingerprint_sha256(
            &"f".repeat(64),
            &"b".repeat(40),
            &"1".repeat(64),
            &"2".repeat(64),
            &"3".repeat(64),
            &"4".repeat(64),
        )
        .unwrap(),
        image_identity: "1".repeat(64),
        build_source_revision: "a".repeat(40),
    };
    let mut request = CampaignRequest {
        schema_version: CAMPAIGN_REQUEST_SCHEMA_V5.to_string(),
        campaign_id: String::new(),
        build_source_revision: "a".repeat(40),
        image_identity: "1".repeat(64),
        campaign_inputs_sha256: "f".repeat(64),
        producer_source_revision: "b".repeat(40),
        producer_image_identity: "e".repeat(64),
        research_plan: research_plan.clone(),
        study_proposal: None,
        feature_url: format!("{TEST_ROOT}/features.jsonl"),
        feature_sha256: "1".repeat(64),
        materialization_url: format!("{TEST_ROOT}/materialization.json"),
        materialization_sha256: "2".repeat(64),
        replay_artifact_url: format!("{TEST_ROOT}/replay.parquet"),
        replay_artifact_sha256: "3".repeat(64),
        replay_manifest_url: format!("{TEST_ROOT}/replay-manifest.json"),
        replay_manifest_sha256: "4".repeat(64),
        holdout_id: "cex-holdout-test".to_string(),
        declared_total_trials: declared_total_trials_for_rounds(&research_plan, 2).unwrap(),
        rounds: vec![
            CampaignRoundRequest {
                round_id: "r1".to_string(),
                seed: 11,
                identity: round_identity.clone(),
                mission_put_url: format!(
                    "{TEST_ROOT}/campaign-id=placeholder/round=r1/mission.json"
                ),
                mission_readback_url: format!(
                    "{TEST_ROOT}/campaign-id=placeholder/round=r1/mission.json?readback=1"
                ),
                result_put_url: format!("{TEST_ROOT}/campaign-id=placeholder/round=r1/results.zip"),
                result_readback_url: format!(
                    "{TEST_ROOT}/campaign-id=placeholder/round=r1/results.zip?readback=1"
                ),
            },
            CampaignRoundRequest {
                round_id: "r2".to_string(),
                seed: 17,
                identity: round_identity,
                mission_put_url: format!(
                    "{TEST_ROOT}/campaign-id=placeholder/round=r2/mission.json"
                ),
                mission_readback_url: format!(
                    "{TEST_ROOT}/campaign-id=placeholder/round=r2/mission.json?readback=1"
                ),
                result_put_url: format!("{TEST_ROOT}/campaign-id=placeholder/round=r2/results.zip"),
                result_readback_url: format!(
                    "{TEST_ROOT}/campaign-id=placeholder/round=r2/results.zip?readback=1"
                ),
            },
        ],
        holdout_claim_put_url: String::new(),
        holdout_claim_readback_url: String::new(),
        campaign_result_put_url: format!(
            "{TEST_ROOT}/campaign-id=placeholder/campaign-result.json"
        ),
        campaign_result_readback_url: format!(
            "{TEST_ROOT}/campaign-id=placeholder/campaign-result.json?readback=1"
        ),
    };
    request.campaign_id = expected_campaign_id(&request).unwrap();
    for round in &mut request.rounds {
        round.mission_put_url = format!(
            "{TEST_ROOT}/campaign-id={}/round={}/mission.json",
            request.campaign_id, round.round_id
        );
        round.mission_readback_url = format!(
            "{TEST_ROOT}/campaign-id={}/round={}/mission.json?readback=1",
            request.campaign_id, round.round_id
        );
        round.result_put_url = format!(
            "{TEST_ROOT}/campaign-id={}/round={}/results.zip",
            request.campaign_id, round.round_id
        );
        round.result_readback_url = format!(
            "{TEST_ROOT}/campaign-id={}/round={}/results.zip?readback=1",
            request.campaign_id, round.round_id
        );
    }
    request.holdout_claim_put_url = cex_global_holdout_claim_object(&request.holdout_id).unwrap();
    request.holdout_claim_readback_url = request.holdout_claim_put_url.clone();
    request.campaign_result_put_url = format!(
        "{TEST_ROOT}/campaign-id={}/campaign-result.json",
        request.campaign_id
    );
    request.campaign_result_readback_url = format!(
        "{TEST_ROOT}/campaign-id={}/campaign-result.json?readback=1",
        request.campaign_id
    );
    request
}

pub(super) fn paired_mlp_plan_for_tests() -> alpha_domain::CexMlpTrainingPlanV1 {
    use alpha_domain::mlp_training::CexMlpInitializationV1;
    alpha_domain::CexMlpTrainingPlanV1 {
        schema_version: "cex-mlp-training-plan-v1".into(),
        updates: 64,
        target_scale: hft_research_manifest::mlp_training::MlpTargetScaleV1::TrainStandardized,
        optimization: None,
        initializations: [(7, vec![71, 72, 73]), (11, vec![111, 112, 113])]
            .into_iter()
            .map(|(seed, fold_seeds)| {
                (
                    seed,
                    CexMlpInitializationV1 {
                        fold_seeds,
                        expected_factor_ids: vec!["cex-factor-1".into()],
                        expected_factor_columns_sha256: "a".repeat(64),
                    },
                )
            })
            .collect(),
    }
}
