//! Deterministic matching of declarations to existing tools. No fitting or dispatch occurs here.
use alpha_domain::{
    representation::*, CexResearchContentRefV1, CexResearchFalsificationTestV1,
    CexResearchHypothesisTargetV1, CexResearchHypothesisV1,
};
use hft_research_manifest::sequence::SequenceInputSpecV1;
use sha2::{Digest, Sha256};

const STATIC_FIELDS: [&str; 9] = [
    "aggregate_trade_flow_imbalance",
    "ask_depth_top5",
    "bid_depth_top5",
    "book_imbalance",
    "book_imbalance_top5",
    "near_depth_concentration_skew_top5",
    "spread_bps",
    "vwap_center_deviation_top5_bps",
    "weighted_book_imbalance_top5",
];
const HISTORY_FIELDS: [&str; 9] = [
    "ask_depth_top5",
    "bid_depth_top5",
    "book_imbalance",
    "book_imbalance_top5",
    "cont_ofi_lag60s",
    "near_depth_concentration_skew_top5",
    "spread_bps",
    "vwap_center_deviation_top5_bps",
    "weighted_book_imbalance_top5",
];

fn reference(id: &str, bytes: &[u8]) -> CexResearchContentRefV1 {
    CexResearchContentRefV1 {
        id: id.into(),
        content_sha256: format!("{:x}", Sha256::digest(bytes)),
    }
}

fn implementation(tool: RepresentationToolV1) -> CexResearchContentRefV1 {
    match tool {
        RepresentationToolV1::CapturedBookReplay
        | RepresentationToolV1::StaticTop5
        | RepresentationToolV1::LaggedContinuousOfi
        | RepresentationToolV1::AggregateTradeFlow => reference(
            "lob-pit-materializer:captured-book-v1",
            include_bytes!("../../../tools/collector/src/bin/lob-pit-materializer.rs"),
        ),
        RepresentationToolV1::SolSequence => {
            reference("sol-sequence-study:v1", include_bytes!("sequence_study.rs"))
        }
        RepresentationToolV1::SolMarketEncoder => reference(
            "sol-market-encoder-study:v1",
            concat!(
                include_str!("market_encoder_study.rs"),
                include_str!("../../../tools/collector/src/bin/lob-pit-materializer.rs"),
                include_str!(
                    "../../../tools/collector/src/bin/lob-pit-materializer/market_encoder.rs"
                ),
            )
            .as_bytes(),
        ),
    }
}

/// All inputs remain declarations. Only the IO owner may derive them from verified raw handles.
pub fn propose_representation_comparison(
    data: &DataCapabilityV1,
    goal: &RepresentationGoalV1,
) -> Result<RepresentationPlanV1, String> {
    data.validate()?;
    goal.validate()?;
    if (
        data.venue.as_str(),
        data.market.as_str(),
        data.symbol.as_str(),
        data.view.family_id.as_str(),
    ) != (
        goal.venue.as_str(),
        goal.market.as_str(),
        goal.symbol.as_str(),
        goal.family_id.as_str(),
    ) {
        return Err("data instrument or planning family differs from frozen goal".into());
    }
    if !matches!(
        data.view.visibility,
        PlanningVisibilityV1::Training | PlanningVisibilityV1::Development
    ) {
        return Err("independent, sealed, meta certification and exposed terminal views cannot drive this family search".into());
    }
    if data.fields.iter().any(|field| {
        field.decision_ns < goal.window_start_ns
            || field.decision_ns > goal.window_end_ns
            || field.available_ns > goal.window_end_ns
    }) {
        return Err("field clock falls outside the frozen goal decision window".into());
    }
    let mut matches = Vec::new();
    for tool in [
        RepresentationToolV1::CapturedBookReplay,
        RepresentationToolV1::StaticTop5,
        RepresentationToolV1::LaggedContinuousOfi,
        RepresentationToolV1::AggregateTradeFlow,
        RepresentationToolV1::SolSequence,
        RepresentationToolV1::SolMarketEncoder,
    ] {
        let history_ms = match tool {
            RepresentationToolV1::LaggedContinuousOfi
            | RepresentationToolV1::SolSequence
            | RepresentationToolV1::SolMarketEncoder => 60_000,
            _ => 0,
        };
        let mut reasons = Vec::new();
        // Recovery series stay separate. A lookback must fit wholly within one verified series.
        let covered = data.series.iter().any(|s| {
            goal.window_start_ns
                .checked_sub(history_ms * 1_000_000)
                .is_some_and(|start| {
                    s.start_available_ns <= start && s.end_available_ns >= goal.window_end_ns
                })
                && s.snapshots > 0
                && s.captured_seed_depth >= 5
                && match tool {
                    RepresentationToolV1::StaticTop5 | RepresentationToolV1::AggregateTradeFlow => {
                        matches!(
                            s.continuity,
                            BookContinuityV1::SnapshotOnly | BookContinuityV1::SequenceChecked
                        )
                    }
                    _ => s.continuity == BookContinuityV1::SequenceChecked && s.diffs > 0,
                }
        });
        if !covered {
            reasons.push("requires captured Top5 seed and coverage inside one suitable series; gaps and unseeded diffs are unusable".into());
        }
        if matches!(
            tool,
            RepresentationToolV1::AggregateTradeFlow
                | RepresentationToolV1::SolSequence
                | RepresentationToolV1::SolMarketEncoder
        ) && !data.aggregate_trade_direction
        {
            reasons.push("source lacks verified aggregate-trade aggressor direction".into());
        }
        if matches!(
            tool,
            RepresentationToolV1::SolSequence | RepresentationToolV1::SolMarketEncoder
        ) && (goal.symbol != "SOLUSDT"
            || goal.market != "usdm"
            || goal.target_name != "forward_mid_return"
            || goal.labels.observation_frequency_millis != 1000
            || goal.labels.horizon_buckets != 30)
        {
            reasons.push("existing SOL Study supports Binance USD-M SOLUSDT, 24 channels, 60 x 1s context and a fixed 30s primary target; 5/10s are diagnostic labels only".into());
        }
        matches.push(ToolMatchV1 {
            tool,
            implementation: implementation(tool),
            history_ms,
            supported: reasons.is_empty(),
            reasons,
        });
    }
    let registry_sha256 = alpha_domain::canonical_json_hash(&(
        "representation-registry-v1",
        matches
            .iter()
            .map(|m| (&m.tool, &m.implementation, m.history_ms))
            .collect::<Vec<_>>(),
        STATIC_FIELDS,
        HISTORY_FIELDS,
        SequenceInputSpecV1::sol_lob(),
        include_str!("representation_plan.rs"),
    ))
    .map_err(|e| e.to_string())?;
    let has = |tool| matches.iter().any(|m| m.tool == tool && m.supported);
    // Two bounded materializations are proposed. No trial or compute has been reserved.
    let requested_resources = PlanningResourcesV1 {
        cpu_millis: 2000,
        memory_mib: 4096,
        wall_seconds: 900,
        trials: 2,
    };
    let renderer_supported = goal.target_name == "forward_mid_return"
        && goal.labels.observation_frequency_millis == 1000
        && [5, 10, 30].contains(&goal.labels.horizon_buckets)
        && matches!(
            (goal.market.as_str(), goal.symbol.as_str()),
            ("usdm", "BTCUSDT" | "SOLUSDT" | "BNBUSDT") | ("spot", "BTCUSDT")
        );
    let feasible = has(RepresentationToolV1::StaticTop5)
        && has(RepresentationToolV1::LaggedContinuousOfi)
        && has(RepresentationToolV1::AggregateTradeFlow)
        && renderer_supported
        && requested_resources.fits(&goal.resource_limit);
    let mut limitations = vec![
        "Raw declarations and hashes are not verification receipts or read permissions.".into(),
        "Captured L2 depth does not establish full venue depth, L3 queue position, or exact cancellations.".into(),
        "Diff quantities replace price-level quantities. The existing replay implements this rule.".into(),
        "Current H2 substitutes lagged OFI for aggregate trade imbalance; it does not add arbitrary fields.".into(),
        "Materialization must still verify per-row depth, availability, warmup and label maturity.".into(),
        "Plan output never authorizes dispatch, changes a finite grant allowlist, or debits a budget.".into(),
    ];
    if !renderer_supported {
        limitations.push(
            "Current H1/H2 renderer has no registered instrument, target, cadence or horizon for this goal.".into(),
        );
    }
    if !requested_resources.fits(&goal.resource_limit) {
        limitations.push("The two-arm requirement exceeds the declared resource limit; no executable plan is emitted.".into());
    }
    let arms = if feasible {
        vec![
            RepresentationArmV1 {
                name: "registered_h1_snapshot_family".into(),
                fields: STATIC_FIELDS.map(str::to_string).to_vec(),
                history_ms: 0,
                tools: vec![
                    RepresentationToolV1::StaticTop5,
                    RepresentationToolV1::AggregateTradeFlow,
                ],
            },
            RepresentationArmV1 {
                name: "registered_h2_lagged_ofi_family".into(),
                fields: HISTORY_FIELDS.map(str::to_string).to_vec(),
                history_ms: 60_000,
                tools: vec![
                    RepresentationToolV1::CapturedBookReplay,
                    RepresentationToolV1::StaticTop5,
                    RepresentationToolV1::LaggedContinuousOfi,
                ],
            },
        ]
    } else {
        Vec::new()
    };
    let hypothesis = feasible.then(|| CexResearchHypothesisV1 {
        hypothesis_id: format!("representation-{}", &goal.goal.content_sha256[..16]),
        statement: "Compare the registered snapshot family with lagged continuous OFI under the same frozen target, model, scaling, costs and partitions.".into(),
        target: CexResearchHypothesisTargetV1 { name: goal.target_name.clone(), horizon: goal.labels.clone() },
        required_feature_families: vec!["h1_snapshot_top5".into(), "h2_cont_ofi_lag60s".into()],
        required_template_families: Vec::new(),
        falsification_tests: vec![CexResearchFalsificationTestV1 {
            test_id: "fixed_predictive_evaluator_and_post_cost_replay".into(),
            reject_when: "Reject when the precommitted evaluator finds no incremental prediction or the fixed cost replay is economically unusable; thresholds require native admission.".into(),
        }],
        source_evidence_ids: data.sources.iter().map(|v| v.id.clone()).collect(),
    });
    Ok(RepresentationPlanV1 {
        schema: REPRESENTATION_PLAN_SCHEMA.into(),
        capability_sha256: data.digest()?,
        goal_sha256: goal.digest()?,
        registry_sha256,
        status: if feasible {
            RepresentationPlanStatusV1::RequiresDataVerificationAndNativeAdmission
        } else {
            RepresentationPlanStatusV1::NoFeasibleComparison
        },
        goal: goal.clone(),
        matches,
        arms,
        hypothesis,
        requested_resources,
        limitations,
    })
}

/// Recompute the registry and all matching outcomes. A deserialized plan cannot certify itself.
pub fn validate_representation_plan(
    plan: &RepresentationPlanV1,
    data: &DataCapabilityV1,
    goal: &RepresentationGoalV1,
) -> Result<(), String> {
    plan.validate_binding(data, goal)?;
    if &propose_representation_comparison(data, goal)? != plan {
        return Err("representation plan, fields, tools or resource requirements drifted".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::EvaluationLabelSpecV1;
    fn content(id: &str) -> CexResearchContentRefV1 {
        reference(id, id.as_bytes())
    }
    fn input() -> (DataCapabilityV1, RepresentationGoalV1) {
        let data = DataCapabilityV1 {
            schema: CAPABILITY_SCHEMA.into(),
            venue: "binance".into(),
            market: "usdm".into(),
            symbol: "BTCUSDT".into(),
            sources: vec![content("raw"), content("raw-manifest")],
            normalizer: content("normalizer"),
            series: vec![BookSeriesCapabilityV1 {
                session_id: "one".into(),
                start_available_ns: 1_000_000_000,
                end_available_ns: 200_000_000_000,
                snapshots: 1,
                diffs: 10,
                captured_seed_depth: 100,
                continuity: BookContinuityV1::SequenceChecked,
            }],
            fields: vec![FieldClockV1 {
                field: "book".into(),
                unit: "price_and_base_quantity".into(),
                event_ns: None,
                received_ns: 1_000_000_000,
                available_ns: 1_000_000_000,
                decision_ns: 61_000_000_000,
            }],
            aggregate_trade_direction: true,
            view: PlanningViewV1 {
                view: content("development"),
                family_id: "family".into(),
                visibility: PlanningVisibilityV1::Development,
                permission: content("read-permission"),
            },
        };
        let goal = RepresentationGoalV1 {
            goal: content("goal"),
            family_id: "family".into(),
            venue: "binance".into(),
            market: "usdm".into(),
            symbol: "BTCUSDT".into(),
            target_name: "forward_mid_return".into(),
            labels: EvaluationLabelSpecV1 {
                horizon_buckets: 30,
                observation_frequency_millis: 1000,
            },
            window_start_ns: 61_000_000_000,
            window_end_ns: 180_000_000_000,
            model: content("ridge"),
            scaling: content("train-scaler"),
            costs: content("costs"),
            partition: content("partition"),
            resource_limit: PlanningResourcesV1 {
                cpu_millis: 2000,
                memory_mib: 4096,
                wall_seconds: 900,
                trials: 2,
            },
        };
        (data, goal)
    }
    fn supported(plan: &RepresentationPlanV1, tool: RepresentationToolV1) -> bool {
        plan.matches.iter().any(|v| v.tool == tool && v.supported)
    }
    #[test]
    fn continuous_inputs_propose_registered_comparison_without_feature_hints() {
        let (data, goal) = input();
        let plan = propose_representation_comparison(&data, &goal).unwrap();
        assert!(supported(&plan, RepresentationToolV1::CapturedBookReplay));
        assert!(supported(&plan, RepresentationToolV1::LaggedContinuousOfi));
        assert_eq!(plan.arms.len(), 2);
        assert_eq!(plan.arms[0].fields, STATIC_FIELDS);
        assert_eq!(plan.arms[1].fields, HISTORY_FIELDS);
        assert_eq!(plan.goal, goal);
        assert_eq!(
            plan.status,
            RepresentationPlanStatusV1::RequiresDataVerificationAndNativeAdmission
        );
        validate_representation_plan(&plan, &data, &goal).unwrap();
    }
    #[test]
    fn snapshot_only_gaps_and_unseeded_diffs_never_match_history() {
        let (data, goal) = input();
        for continuity in [
            BookContinuityV1::SnapshotOnly,
            BookContinuityV1::Gap,
            BookContinuityV1::Unseeded,
        ] {
            let mut changed = data.clone();
            changed.series[0].continuity = continuity;
            if continuity == BookContinuityV1::SnapshotOnly {
                changed.series[0].diffs = 0;
            }
            let plan = propose_representation_comparison(&changed, &goal).unwrap();
            assert!(!supported(&plan, RepresentationToolV1::CapturedBookReplay));
            assert!(!supported(&plan, RepresentationToolV1::LaggedContinuousOfi));
            assert!(plan.arms.is_empty());
            if continuity == BookContinuityV1::SnapshotOnly {
                assert!(supported(&plan, RepresentationToolV1::StaticTop5));
            }
        }
    }
    #[test]
    fn no_direction_cannot_propose_real_aggressor_or_sequence_family() {
        let (mut data, goal) = input();
        data.aggregate_trade_direction = false;
        let plan = propose_representation_comparison(&data, &goal).unwrap();
        assert!(!supported(&plan, RepresentationToolV1::AggregateTradeFlow));
        assert!(!supported(&plan, RepresentationToolV1::SolSequence));
        assert!(supported(&plan, RepresentationToolV1::LaggedContinuousOfi));
        assert!(plan.arms.is_empty());
    }
    #[test]
    fn future_availability_and_protected_views_are_rejected() {
        let (data, goal) = input();
        let mut future = data.clone();
        future.fields[0].available_ns = future.fields[0].decision_ns + 1;
        assert!(propose_representation_comparison(&future, &goal).is_err());
        for visibility in [
            PlanningVisibilityV1::IndependentValidation,
            PlanningVisibilityV1::StrategySealed,
            PlanningVisibilityV1::MetaCertification,
            PlanningVisibilityV1::ExposedTerminal,
        ] {
            let mut changed = data.clone();
            changed.view.visibility = visibility;
            assert!(propose_representation_comparison(&changed, &goal).is_err());
        }
    }
    #[test]
    fn recovery_boundaries_and_missing_lookback_do_not_get_stitched() {
        let (mut data, goal) = input();
        data.series[0].end_available_ns = goal.window_start_ns;
        let mut second = data.series[0].clone();
        second.session_id = "recovery".into();
        second.start_available_ns = goal.window_start_ns;
        second.end_available_ns = goal.window_end_ns;
        data.series.push(second);
        let plan = propose_representation_comparison(&data, &goal).unwrap();
        assert!(!supported(&plan, RepresentationToolV1::LaggedContinuousOfi));
        assert!(plan.arms.is_empty());
    }
    #[test]
    fn hashes_and_recomputation_reject_changed_data_goal_tool_and_columns() {
        let (data, goal) = input();
        let plan = propose_representation_comparison(&data, &goal).unwrap();
        let mut changed_data = data.clone();
        changed_data.sources[0] = content("other-raw");
        assert!(validate_representation_plan(&plan, &changed_data, &goal).is_err());
        let mut changed_goal = goal.clone();
        changed_goal.labels.horizon_buckets = 10;
        assert_ne!(
            plan.digest().unwrap(),
            propose_representation_comparison(&data, &changed_goal)
                .unwrap()
                .digest()
                .unwrap()
        );
        assert!(validate_representation_plan(&plan, &data, &changed_goal).is_err());
        let mut changed_plan = plan.clone();
        changed_plan.matches[0].implementation = content("different-tool");
        assert!(validate_representation_plan(&changed_plan, &data, &goal).is_err());
        let mut changed_plan = plan.clone();
        changed_plan.arms[1]
            .fields
            .push("unauthorized_column".into());
        assert!(validate_representation_plan(&changed_plan, &data, &goal).is_err());
    }
    #[test]
    fn resource_horizon_and_sol_scope_are_bounded() {
        let (mut data, mut goal) = input();
        goal.resource_limit.trials = 1;
        assert!(propose_representation_comparison(&data, &goal)
            .unwrap()
            .arms
            .is_empty());
        goal.resource_limit.trials = 2;
        goal.labels.horizon_buckets = 60;
        assert!(propose_representation_comparison(&data, &goal)
            .unwrap()
            .arms
            .is_empty());
        assert!(!supported(
            &propose_representation_comparison(&data, &goal).unwrap(),
            RepresentationToolV1::SolSequence
        ));
        data.symbol = "SOLUSDT".into();
        goal.symbol = "SOLUSDT".into();
        goal.labels.horizon_buckets = 30;
        assert!(supported(
            &propose_representation_comparison(&data, &goal).unwrap(),
            RepresentationToolV1::SolSequence
        ));
        assert_eq!(SequenceInputSpecV1::sol_lob().ordered_channels.len(), 24);
    }
    #[test]
    fn field_decision_and_availability_cannot_escape_the_frozen_goal_window() {
        let (data, goal) = input();
        let mut later = data.clone();
        later.fields[0].decision_ns = goal.window_end_ns + 1;
        later.fields[0].available_ns = goal.window_end_ns + 1;
        // The declaration is internally causal, but belongs to a future window.
        later.validate().unwrap();
        assert!(propose_representation_comparison(&later, &goal).is_err());
        later.fields[0].available_ns = data.fields[0].available_ns;
        assert!(propose_representation_comparison(&later, &goal).is_err());
        let mut earlier = data.clone();
        earlier.fields[0].decision_ns = goal.window_start_ns - 1;
        assert!(propose_representation_comparison(&earlier, &goal).is_err());
        let mut edge = data;
        edge.fields[0].available_ns = goal.window_end_ns;
        edge.fields[0].decision_ns = goal.window_end_ns;
        assert!(propose_representation_comparison(&edge, &goal).is_ok());
    }
    #[test]
    fn comparison_uses_the_registered_renderer_instrument_allowlist() {
        for (market, symbol, supported_instrument) in [
            ("usdm", "BTCUSDT", true),
            ("usdm", "SOLUSDT", true),
            ("usdm", "BNBUSDT", true),
            ("spot", "BTCUSDT", true),
            ("spot", "SOLUSDT", false),
            ("usdm", "ETHUSDT", false),
        ] {
            let (mut data, mut goal) = input();
            data.market = market.into();
            goal.market = market.into();
            data.symbol = symbol.into();
            goal.symbol = symbol.into();
            let plan = propose_representation_comparison(&data, &goal).unwrap();
            assert_eq!(
                !plan.arms.is_empty(),
                supported_instrument,
                "{market}/{symbol}"
            );
            assert_eq!(plan.hypothesis.is_some(), supported_instrument);
        }
    }
    #[test]
    fn sol_diagnostic_horizons_are_not_registered_study_primary_targets() {
        let (mut data, mut goal) = input();
        data.symbol = "SOLUSDT".into();
        goal.symbol = "SOLUSDT".into();
        for horizon in [5, 10, 30] {
            goal.labels.horizon_buckets = horizon;
            let plan = propose_representation_comparison(&data, &goal).unwrap();
            for tool in [
                RepresentationToolV1::SolSequence,
                RepresentationToolV1::SolMarketEncoder,
            ] {
                assert_eq!(supported(&plan, tool), horizon == 30, "{tool:?}/{horizon}");
            }
        }
    }

    #[test]
    fn review_encoder_identity_covers_feature_and_target_materialization() {
        let (data, goal) = input();
        let plan = propose_representation_comparison(&data, &goal).unwrap();
        let encoder = plan
            .matches
            .iter()
            .find(|tool| tool.tool == RepresentationToolV1::SolMarketEncoder)
            .unwrap();
        let complete = concat!(
            include_str!("market_encoder_study.rs"),
            include_str!("../../../tools/collector/src/bin/lob-pit-materializer.rs"),
            include_str!("../../../tools/collector/src/bin/lob-pit-materializer/market_encoder.rs"),
        );
        assert_eq!(
            encoder.implementation.content_sha256,
            format!("{:x}", Sha256::digest(complete.as_bytes()))
        );
        let mut obsolete = plan;
        obsolete
            .matches
            .iter_mut()
            .find(|tool| tool.tool == RepresentationToolV1::SolMarketEncoder)
            .unwrap()
            .implementation = reference(
            "sol-market-encoder-study:v1",
            include_bytes!("market_encoder_study.rs"),
        );
        assert!(validate_representation_plan(&obsolete, &data, &goal).is_err());
    }

    #[test]
    fn review_only_canonical_forward_mid_return_can_emit_comparison_arms() {
        for symbol in ["BTCUSDT", "SOLUSDT"] {
            let (mut data, mut goal) = input();
            data.symbol = symbol.into();
            goal.symbol = symbol.into();
            for target in ["mid_return", "unregistered_target"] {
                goal.target_name = target.into();
                let plan = propose_representation_comparison(&data, &goal).unwrap();
                assert!(plan.arms.is_empty(), "{symbol}/{target}");
                assert!(plan.hypothesis.is_none());
                if symbol == "SOLUSDT" {
                    for tool in [
                        RepresentationToolV1::SolSequence,
                        RepresentationToolV1::SolMarketEncoder,
                    ] {
                        assert!(!supported(&plan, tool), "{tool:?}/{target}");
                    }
                }
            }
        }
    }
    #[test]
    fn json_claimed_verified_does_not_create_a_verified_capability() {
        let (data, goal) = input();
        let mut claimed = serde_json::to_value(&data).unwrap();
        claimed["verified"] = serde_json::json!(true);
        assert!(serde_json::from_value::<DataCapabilityV1>(claimed).is_err());
        assert_eq!(
            propose_representation_comparison(&data, &goal)
                .unwrap()
                .status,
            RepresentationPlanStatusV1::RequiresDataVerificationAndNativeAdmission
        );
    }
}
