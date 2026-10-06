//! A validated proposal joins the existing Campaign request. It grants no execution authority.
use super::{
    declared_total_trials_for_rounds, declared_total_trials_for_validated_plan, CampaignRequest,
};
use crate::{
    mission_render::{CexCampaignResearchPlanV1, PreparedCexInputs, RenderedCexMission},
    mission_runner::Materialization,
    representation_plan::VerifiedTapeRepresentationProposal,
};
use alpha_domain::{
    campaign_control::VerifiedCampaignRootGrant, canonical_json_hash, representation::*,
    CexResearchContentRefV1, CexResearchMissionArtifactV1,
};
use anyhow::{bail, ensure, Context, Result};
use std::{collections::BTreeSet, path::Path};

fn reference(id: &str, value: &impl serde::Serialize) -> Result<CexResearchContentRefV1> {
    Ok(CexResearchContentRefV1 {
        id: id.into(),
        content_sha256: canonical_json_hash(value)?,
    })
}

/// Read the existing renderer contracts before freezing a representation goal.
/// Paths remain subject to the existing source loader and its integrity checks.
pub fn preview_representation_campaign_contracts(
    research_template: serde_json::Value,
    selected_arm: &str,
    feature: &Path,
    materialization: &Path,
    seeds: &[u64],
    runner_source_revision: &str,
) -> Result<RepresentationCampaignContractRefsV1> {
    let plan = registered_template(serde_json::from_value(research_template)?, selected_arm)?;
    ensure!(
        plan.representation_binding.is_none(),
        "preview requires an unbound research template"
    );
    let inputs = PreparedCexInputs::load(feature, materialization, plan.calendar.is_some())?;
    let rendered = render_template(&inputs, &plan, seeds)?;
    contract_refs(
        &plan,
        &rendered.mission,
        inputs.materialization(),
        seeds,
        runner_source_revision,
    )
}

/// Only the first slice's opaque raw-verification handle can call this mapper.
/// Returned JSON is a declaration consumed by the existing `--research-plan` freeze path.
pub fn bind_verified_representation_campaign(
    verified: &VerifiedTapeRepresentationProposal,
    selected_arm: &str,
    research_template: serde_json::Value,
    feature: &Path,
    materialization: &Path,
    seeds: &[u64],
    runner_source_revision: &str,
) -> Result<serde_json::Value> {
    let template = registered_template(serde_json::from_value(research_template)?, selected_arm)?;
    let inputs = PreparedCexInputs::load(feature, materialization, template.calendar.is_some())?;
    let plan = bind_proposal(
        verified.capability(),
        verified.proposal(),
        selected_arm,
        template,
        &inputs,
        seeds,
        runner_source_revision,
    )?;
    Ok(serde_json::to_value(plan)?)
}

fn registered_template(
    mut template: CexCampaignResearchPlanV1,
    selected_arm: &str,
) -> Result<CexCampaignResearchPlanV1> {
    template.validate()?;
    ensure!(
        template.representation_binding.is_none()
            && (template.feature_fields == CexCampaignResearchPlanV1::canonical().feature_fields
                || template.feature_fields == CexCampaignResearchPlanV1::h2().feature_fields),
        "representation mapper requires an unbound registered family template"
    );
    let registered = match selected_arm {
        "registered_h1_snapshot_family" => CexCampaignResearchPlanV1::canonical(),
        "registered_h2_lagged_ofi_family" => CexCampaignResearchPlanV1::h2(),
        _ => bail!("representation mapper rejects an unregistered arm"),
    };
    template.feature_fields = registered.feature_fields;
    template.objective = registered.objective;
    template.hypothesis = registered.hypothesis;
    template.validate()?;
    Ok(template)
}

fn render_template(
    inputs: &PreparedCexInputs,
    plan: &CexCampaignResearchPlanV1,
    seeds: &[u64],
) -> Result<RenderedCexMission> {
    let seed = *seeds
        .first()
        .context("representation Campaign requires its actual seed schedule")?;
    crate::mission_render::render_prepared_cex_bundle(
        inputs,
        plan,
        seed,
        declared_total_trials_for_rounds(plan, seeds.len())?,
    )
}

fn bind_proposal(
    data: &DataCapabilityV1,
    proposal: &RepresentationPlanV1,
    selected_arm: &str,
    mut template: CexCampaignResearchPlanV1,
    inputs: &PreparedCexInputs,
    seeds: &[u64],
    runner_source_revision: &str,
) -> Result<CexCampaignResearchPlanV1> {
    ensure!(
        template.representation_binding.is_none(),
        "research template is already bound"
    );
    ensure!(
        crate::mission_runner::valid_git_revision(runner_source_revision),
        "invalid representation runner source revision"
    );
    alpha_engine::representation_plan::validate_representation_plan(proposal, data, &proposal.goal)
        .map_err(anyhow::Error::msg)?;
    let declared = declared_total_trials_for_rounds(&template, seeds.len())?;
    template.representation_binding = Some(RepresentationCampaignBindingV1 {
        schema: "monday.representation_campaign_binding.v1".into(),
        capability: data.clone(),
        proposal: proposal.clone(),
        selected_arm: selected_arm.into(),
        runner_source_revision: runner_source_revision.into(),
        seeds: seeds.to_vec(),
        declared_total_trials: declared,
    });
    template.validate()?;
    let rendered = render_template(inputs, &template, seeds)?;
    let expected = contract_refs(
        &template,
        &rendered.mission,
        inputs.materialization(),
        seeds,
        runner_source_revision,
    )?;
    validate_goal_contracts(template.representation_binding.as_ref().unwrap(), &expected)?;
    Ok(template)
}

pub(crate) fn validate_research_plan_binding(
    base: &crate::mission_render::ValidatedCexResearchPlanBase<'_>,
) -> Result<()> {
    let plan = base.plan();
    let Some(binding) = &plan.representation_binding else {
        return Ok(());
    };
    binding.validate().map_err(anyhow::Error::msg)?;
    alpha_engine::representation_plan::validate_representation_plan(
        &binding.proposal,
        &binding.capability,
        &binding.proposal.goal,
    )
    .map_err(anyhow::Error::msg)?;
    ensure!(plan.generation == 0 && plan.parent.is_none() && plan.learning_directive.is_none()
        && plan.llm.is_none() && plan.mlp_training.is_none() && plan.search_policy_revision.research_delta.is_none(),
        "representation binding selects registered root families only; it cannot widen a policy revision");
    let expected = match binding.selected_arm.as_str() {
        "registered_h1_snapshot_family" => CexCampaignResearchPlanV1::canonical().feature_fields,
        "registered_h2_lagged_ofi_family" => CexCampaignResearchPlanV1::h2().feature_fields,
        _ => bail!("unregistered representation family"),
    };
    let arm = binding
        .proposal
        .arms
        .iter()
        .find(|arm| arm.name == binding.selected_arm)
        .context("selected arm missing")?;
    ensure!(
        plan.feature_fields == expected && arm.fields == expected,
        "Campaign fields differ from the exact registered representation arm"
    );
    let trials = declared_total_trials_for_validated_plan(base, binding.seeds.len())?;
    ensure!(
        binding.declared_total_trials == trials
            && trials <= binding.proposal.goal.resource_limit.trials as usize,
        "planning trials cannot replace actual Campaign trial charging or widen its limit"
    );
    Ok(())
}

pub(super) fn validate_campaign_seeds(
    plan: &CexCampaignResearchPlanV1,
    seeds: &[u64],
) -> Result<()> {
    if let Some(binding) = &plan.representation_binding {
        ensure!(
            binding.seeds == seeds,
            "representation Campaign seed schedule drifted"
        );
    }
    Ok(())
}

pub(super) fn validate_campaign_request(request: &CampaignRequest) -> Result<()> {
    let Some(binding) = &request.research_plan.representation_binding else {
        return Ok(());
    };
    ensure!(
        binding.runner_source_revision == request.build_source_revision,
        "representation runner source differs from the actual finalized request Build"
    );
    ensure!(
        request.prepared_inputs.is_some(),
        "representation Campaign requires the existing verified native prepared path"
    );
    validate_campaign_seeds(
        &request.research_plan,
        &request
            .rounds
            .iter()
            .map(|round| round.seed)
            .collect::<Vec<_>>(),
    )?;
    ensure!(
        request.declared_total_trials == binding.declared_total_trials,
        "representation Campaign declared charge changed"
    );
    let prepared = request.prepared_inputs.as_ref().unwrap();
    let inputs = PreparedCexInputs::restore_metadata(
        prepared.render_metadata.clone(),
        &request.feature_sha256,
        &request.materialization_sha256,
    )?;
    let rendered = render_template(&inputs, &request.research_plan, &binding.seeds)?;
    let expected = contract_refs(
        &request.research_plan,
        &rendered.mission,
        inputs.materialization(),
        &binding.seeds,
        &request.build_source_revision,
    )?;
    validate_goal_contracts(binding, &expected)
}

pub(crate) fn validate_rendered_binding(
    plan: &CexCampaignResearchPlanV1,
    mission: &CexResearchMissionArtifactV1,
    materialization: &Materialization,
    declared: usize,
) -> Result<()> {
    let Some(binding) = &plan.representation_binding else {
        return Ok(());
    };
    ensure!(
        binding.declared_total_trials == declared
            || plan.comparison_family_trials == Some(declared),
        "rendered statistical trial scope differs from its representation Campaign"
    );
    let goal = &binding.proposal.goal;
    ensure!(
        goal.venue == mission.spec.instrument.venue.as_str()
            && goal.market == materialization.market
            && goal.symbol == materialization.symbol
            && goal.target_name == "forward_mid_return"
            && goal.labels == mission.spec.instrument.horizon,
        "representation target or instrument differs from the actual renderer"
    );
    let expected_sources = materialization
        .snapshot
        .source_segments
        .iter()
        .flat_map(|source| [&source.content_sha256, &source.manifest_sha256])
        .collect::<BTreeSet<_>>();
    let observed_sources = binding
        .capability
        .sources
        .iter()
        .map(|source| &source.content_sha256)
        .collect::<BTreeSet<_>>();
    ensure!(
        observed_sources == expected_sources,
        "representation source digests differ from actual native materialization"
    );
    let refs = contract_refs(
        plan,
        mission,
        materialization,
        &binding.seeds,
        &binding.runner_source_revision,
    )?;
    validate_goal_contracts(binding, &refs)
}

fn contract_refs(
    plan: &CexCampaignResearchPlanV1,
    mission: &CexResearchMissionArtifactV1,
    materialization: &Materialization,
    seeds: &[u64],
    runner_source_revision: &str,
) -> Result<RepresentationCampaignContractRefsV1> {
    ensure!(
        crate::mission_runner::valid_git_revision(runner_source_revision),
        "invalid representation runner source revision"
    );
    let protocol = &mission.spec.evaluation_protocol;
    let partitions = protocol.row_partitions(materialization.rows)?;
    let visible_end = protocol
        .calendar
        .as_ref()
        .map_or(partitions.search.end, |calendar| calendar.develop_end_row);
    ensure!(
        materialization.snapshot.series.len() == 1 && visible_end > 0,
        "representation native path requires one continuous source series"
    );
    let first = u64::try_from(
        materialization
            .snapshot
            .first_event_time
            .timestamp_nanos_opt()
            .context("representation first clock overflow")?,
    )?;
    let last = u64::try_from(
        materialization
            .snapshot
            .last_event_time
            .timestamp_nanos_opt()
            .context("representation last clock overflow")?,
    )?;
    let cadence = materialization
        .bucket_ms
        .checked_mul(1_000_000)
        .context("representation cadence overflow")?;
    let original_end = first
        .checked_add(
            u64::try_from(materialization.rows - 1)?
                .checked_mul(cadence)
                .context("representation row clock overflow")?,
        )
        .context("representation final clock overflow")?;
    ensure!(
        last == original_end,
        "representation source clocks are not the actual regular native sequence"
    );
    let window_end = first
        .checked_add(
            u64::try_from(visible_end)?
                .checked_mul(cadence)
                .context("representation window overflow")?,
        )
        .context("representation window overflow")?;
    let baseline = crate::mission_runner::bound_baseline_policy(mission)?;
    let model = reference("campaign-native-model-policy-v1", &(&baseline, seeds))?;
    let scaling = reference(
        "campaign-native-training-scaling-v1",
        &(
            mission.spec.supervised_model_scope,
            include_str!("../../../engine/src/baselines/classic.rs"),
        ),
    )?;
    let costs = reference(
        "campaign-native-cost-contract-v1",
        &(
            &protocol.costs,
            plan.decision_policy_for_market(mission.spec.instrument.market.clone())?,
            &mission.spec.policies.replay,
        ),
    )?;
    let partition = reference(
        "campaign-native-partitions-v1",
        &(
            materialization.snapshot.sha256(),
            protocol,
            &partitions,
            seeds,
        ),
    )?;
    let view = canonical_json_hash(&serde_json::json!({
        "schema_version": "monday.campaign_evaluation_view.v2", "purpose": "search_and_learning",
        "materialization_sha256": mission.spec.inputs.materialization.content_sha256,
        "feature_sha256": mission.spec.inputs.feature.content_sha256,
        "snapshot_sha256": materialization.snapshot.sha256(), "evaluation_protocol_sha256": protocol.content_hash()?,
        "source_revision": runner_source_revision, "rows": partitions.search,
    }))?;
    Ok(RepresentationCampaignContractRefsV1 {
        model,
        scaling,
        costs,
        partition,
        planning_view: CexResearchContentRefV1 {
            id: "campaign-search-and-learning-v2".into(),
            content_sha256: view,
        },
        window_start_ns: first,
        window_end_ns: window_end,
        declared_total_trials: declared_total_trials_for_rounds(plan, seeds.len())?,
    })
}

fn validate_goal_core(
    binding: &RepresentationCampaignBindingV1,
    refs: &RepresentationCampaignContractRefsV1,
) -> Result<()> {
    let goal = &binding.proposal.goal;
    ensure!(
        goal.model == refs.model
            && goal.scaling == refs.scaling
            && goal.costs == refs.costs
            && goal.partition == refs.partition,
        "representation model, scaling, costs or partition differ from actual native contracts"
    );
    ensure!(
        goal.window_start_ns == refs.window_start_ns && goal.window_end_ns == refs.window_end_ns,
        "representation goal window differs from actual development inputs"
    );
    ensure!(
        binding.declared_total_trials == refs.declared_total_trials,
        "representation charge differs from actual model/seed schedule"
    );
    Ok(())
}
fn validate_goal_contracts(
    binding: &RepresentationCampaignBindingV1,
    refs: &RepresentationCampaignContractRefsV1,
) -> Result<()> {
    validate_goal_core(binding, refs)?;
    ensure!(
        binding.capability.view.view == refs.planning_view,
        "representation planning view differs from the actual search partition"
    );
    Ok(())
}

pub(crate) fn validate_manifest_authority(
    manifest: &serde_json::Value,
    request_sha256: &str,
    root: &VerifiedCampaignRootGrant,
) -> Result<()> {
    let Some(json) = manifest["items"][0]["stringData"]["campaign.json"].as_str() else {
        return Ok(());
    };
    let value: serde_json::Value = serde_json::from_str(json)?;
    if value["research_plan"]["representation_binding"].is_null() {
        return Ok(());
    }
    ensure!(
        hft_cex_research_input::sha256(json.as_bytes()) == request_sha256,
        "representation authority checked a different request"
    );
    let request: CampaignRequest = serde_json::from_value(value)?;
    validate_campaign_request(&request)?;
    let binding = request
        .research_plan
        .representation_binding
        .as_ref()
        .unwrap();
    ensure!(
        binding.proposal.goal.family_id == root.grant().family.family_id
            && binding.capability.view.family_id == root.grant().family.family_id
            && binding.capability.view.permission.content_sha256 == root.content_sha256(),
        "representation permission or family differs from the authenticated root"
    );
    Ok(())
}

pub(crate) fn validate_execution_limits(
    plan: &CexCampaignResearchPlanV1,
    cpu_millis: u32,
    memory_mib: u32,
    seconds: u64,
    trials: usize,
    search_view_sha256: &str,
) -> Result<()> {
    let Some(binding) = &plan.representation_binding else {
        return Ok(());
    };
    let limit = &binding.proposal.goal.resource_limit;
    ensure!(
        cpu_millis <= limit.cpu_millis
            && memory_mib <= limit.memory_mib
            && seconds <= u64::from(limit.wall_seconds)
            && trials == binding.declared_total_trials
            && trials <= limit.trials as usize,
        "actual Campaign resources or scientific charge exceed the frozen representation goal"
    );
    ensure!(
        binding.capability.view.view.content_sha256 == search_view_sha256,
        "representation planning view differs from the actual admitted search view"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::EvaluationLabelSpecV1;

    const TEST_RUNNER_SOURCE: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Synthetic source declarations test binding behavior, not raw continuity or scientific results.
    fn prepared_case() -> (
        crate::mission_render::tests::Fixture,
        PreparedCexInputs,
        CexCampaignResearchPlanV1,
        DataCapabilityV1,
        RepresentationPlanV1,
    ) {
        let fixture = crate::mission_render::tests::Fixture::canonical();
        let inputs =
            PreparedCexInputs::load(&fixture.feature_path, &fixture.materialization_path, false)
                .unwrap();
        let plan = CexCampaignResearchPlanV1::canonical();
        let seeds = [7, 11];
        let rendered = render_template(&inputs, &plan, &seeds).unwrap();
        let materialization = inputs.materialization();
        assert_eq!(materialization.source_revision.len(), 64);
        let refs = contract_refs(
            &plan,
            &rendered.mission,
            materialization,
            &seeds,
            TEST_RUNNER_SOURCE,
        )
        .unwrap();
        let content = |id: &str| reference(id, &id).unwrap();
        let goal = RepresentationGoalV1 {
            goal: content("goal"),
            family_id: "represented-family".into(),
            venue: "binance".into(),
            market: "usdm".into(),
            symbol: "BTCUSDT".into(),
            target_name: "forward_mid_return".into(),
            labels: EvaluationLabelSpecV1 {
                horizon_buckets: 5,
                observation_frequency_millis: 1000,
            },
            window_start_ns: refs.window_start_ns,
            window_end_ns: refs.window_end_ns,
            model: refs.model,
            scaling: refs.scaling,
            costs: refs.costs,
            partition: refs.partition,
            resource_limit: PlanningResourcesV1 {
                cpu_millis: 16000,
                memory_mib: 131072,
                wall_seconds: 86400,
                trials: 1000,
            },
        };
        let sources = materialization
            .snapshot
            .source_segments
            .iter()
            .flat_map(|segment| {
                [
                    CexResearchContentRefV1 {
                        id: format!("market-tape-{}", segment.content_sha256),
                        content_sha256: segment.content_sha256.clone(),
                    },
                    CexResearchContentRefV1 {
                        id: format!("market-manifest-{}", segment.manifest_sha256),
                        content_sha256: segment.manifest_sha256.clone(),
                    },
                ]
            })
            .collect();
        let capability = DataCapabilityV1 {
            schema: CAPABILITY_SCHEMA.into(),
            venue: goal.venue.clone(),
            market: goal.market.clone(),
            symbol: goal.symbol.clone(),
            sources,
            normalizer: content("normalizer-declaration"),
            series: vec![BookSeriesCapabilityV1 {
                session_id: "synthetic-planning-series".into(),
                start_available_ns: goal.window_start_ns - 60_000_000_000,
                end_available_ns: goal.window_end_ns,
                snapshots: 1,
                diffs: 10,
                captured_seed_depth: 5,
                continuity: BookContinuityV1::SequenceChecked,
            }],
            fields: vec![FieldClockV1 {
                field: "captured_book".into(),
                unit: "price_and_quantity".into(),
                event_ns: None,
                received_ns: goal.window_start_ns,
                available_ns: goal.window_start_ns,
                decision_ns: goal.window_start_ns,
            }],
            aggregate_trade_direction: true,
            view: PlanningViewV1 {
                view: refs.planning_view,
                family_id: goal.family_id.clone(),
                visibility: PlanningVisibilityV1::Development,
                permission: content("root-permission"),
            },
        };
        let proposal = alpha_engine::representation_plan::propose_representation_comparison(
            &capability,
            &goal,
        )
        .unwrap();
        (fixture, inputs, plan, capability, proposal)
    }
    fn bind_case(
        plan: CexCampaignResearchPlanV1,
        inputs: &PreparedCexInputs,
        data: &DataCapabilityV1,
        proposal: &RepresentationPlanV1,
    ) -> Result<CexCampaignResearchPlanV1> {
        bind_proposal(
            data,
            proposal,
            "registered_h1_snapshot_family",
            plan,
            inputs,
            &[7, 11],
            TEST_RUNNER_SOURCE,
        )
    }
    #[test]
    fn representation_runner_revision_is_exact_and_never_unknown() {
        let (_fixture, inputs, plan, data, proposal) = prepared_case();
        let baseline = bind_case(plan.clone(), &inputs, &data, &proposal).unwrap();
        baseline.validate().unwrap();
        for source in ["unknown", "not-a-git-revision", "0123456789abcdef"] {
            let error = bind_proposal(
                &data,
                &proposal,
                "registered_h1_snapshot_family",
                plan.clone(),
                &inputs,
                &[7, 11],
                source,
            )
            .unwrap_err();
            assert!(error
                .to_string()
                .contains("invalid representation runner source revision"));
        }
    }
    #[test]
    fn imported_runner_identity_cannot_replace_the_frozen_software_source() {
        let (_fixture, inputs, plan, data, proposal) = prepared_case();
        let baseline = bind_case(plan, &inputs, &data, &proposal).unwrap();
        baseline.validate().unwrap();
        render_template(&inputs, &baseline, &[7, 11]).unwrap();
        let mut changed = baseline.clone();
        changed
            .representation_binding
            .as_mut()
            .unwrap()
            .runner_source_revision = "abcdef0123456789abcdef0123456789abcdef01".into();
        // A valid Git string still changes the actual view contract.
        changed.validate().unwrap();
        assert!(render_template(&inputs, &changed, &[7, 11]).is_err());
        let mut unknown = baseline;
        unknown
            .representation_binding
            .as_mut()
            .unwrap()
            .runner_source_revision = "unknown".into();
        assert!(unknown.validate().is_err());
    }
    #[test]
    fn representation_mapper_selects_exact_registered_arms_without_a_feature_mix() {
        let template = CexCampaignResearchPlanV1::canonical();
        let h2 = registered_template(template.clone(), "registered_h2_lagged_ofi_family").unwrap();
        assert_eq!(
            h2.feature_fields,
            CexCampaignResearchPlanV1::h2().feature_fields
        );
        assert_eq!(
            h2.allowed_search_policy_revisions,
            template.allowed_search_policy_revisions
        );
        assert_eq!(h2.search_policy_revision, template.search_policy_revision);
        let h1 = registered_template(h2, "registered_h1_snapshot_family").unwrap();
        assert_eq!(h1.feature_fields, template.feature_fields);
        assert!(registered_template(template.clone(), "custom_mixed_arm").is_err());
        let mut mixed = template;
        mixed.feature_fields.remove(0);
        assert!(registered_template(mixed, "registered_h2_lagged_ofi_family").is_err());
    }
    #[test]
    fn ordinary_and_bound_validation_keep_checked_accounting_without_recursion() {
        let (_fixture, inputs, plain, data, proposal) = prepared_case();
        plain.validate().unwrap();
        let candidates = plain.max_candidates().unwrap();
        let trials = declared_total_trials_for_rounds(&plain, 2).unwrap();
        let bound = bind_case(plain.clone(), &inputs, &data, &proposal).unwrap();
        bound.validate().unwrap();
        assert_eq!(bound.max_candidates().unwrap(), candidates);
        assert_eq!(declared_total_trials_for_rounds(&bound, 2).unwrap(), trials);
        let mut invalid_plain = plain;
        invalid_plain.schema_version = "untrusted-schema".into();
        assert!(invalid_plain.max_candidates().is_err());
        assert!(declared_total_trials_for_rounds(&invalid_plain, 2).is_err());
        let mut invalid_bound = bound;
        invalid_bound
            .representation_binding
            .as_mut()
            .unwrap()
            .declared_total_trials = 2;
        assert!(invalid_bound.validate().is_err());
        assert!(invalid_bound.max_candidates().is_err());
        assert!(declared_total_trials_for_rounds(&invalid_bound, 2).is_err());
    }
    #[test]
    fn representation_campaign_binds_actual_native_contracts_and_real_trial_charge() {
        let (_fixture, inputs, plan, data, proposal) = prepared_case();
        let expected = declared_total_trials_for_rounds(&plan, 2).unwrap();
        let original = plan.content_hash().unwrap();
        let bound = bind_case(plan, &inputs, &data, &proposal).unwrap();
        assert_ne!(bound.content_hash().unwrap(), original);
        assert_eq!(
            bound
                .representation_binding
                .as_ref()
                .unwrap()
                .declared_total_trials,
            expected
        );
        assert!(expected > proposal.requested_resources.trials as usize);
        let frozen = serde_json::to_vec(&bound).unwrap();
        let imported: CexCampaignResearchPlanV1 = serde_json::from_slice(&frozen).unwrap();
        imported.validate().unwrap();
        render_template(&inputs, &imported, &[7, 11]).unwrap();
        assert!(validate_campaign_seeds(&imported, &[11, 7]).is_err());
    }
    #[test]
    fn renderer_reimport_does_not_replace_the_frozen_prepared_audit_metadata() {
        let (fixture, inputs, plan, data, proposal) = prepared_case();
        let frozen = inputs.native_metadata().unwrap();
        let frozen_bytes = serde_json::to_vec(&frozen).unwrap();
        let rebound = bind_case(plan.clone(), &inputs, &data, &proposal).unwrap();
        rebound.validate().unwrap();
        let reimport =
            PreparedCexInputs::load(&fixture.feature_path, &fixture.materialization_path, false)
                .unwrap();
        assert_ne!(
            inputs.feature_manifest().created_at,
            reimport.feature_manifest().created_at
        );
        let old = render_template(&inputs, &plan, &[7, 11]).unwrap();
        let new = render_template(&reimport, &plan, &[7, 11]).unwrap();
        let old_refs = contract_refs(
            &plan,
            &old.mission,
            inputs.materialization(),
            &[7, 11],
            TEST_RUNNER_SOURCE,
        )
        .unwrap();
        let new_refs = contract_refs(
            &plan,
            &new.mission,
            reimport.materialization(),
            &[7, 11],
            TEST_RUNNER_SOURCE,
        )
        .unwrap();
        assert_eq!(old_refs, new_refs);
        let restored = PreparedCexInputs::restore_metadata(
            frozen,
            inputs.feature_sha256(),
            inputs.materialization_sha256(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_vec(&restored.native_metadata().unwrap()).unwrap(),
            frozen_bytes
        );
        render_template(&restored, &rebound, &[7, 11]).unwrap();
    }
    #[test]
    fn representation_campaign_rejects_goal_contract_and_source_drift() {
        let (_fixture, inputs, plan, data, proposal) = prepared_case();
        let baseline = bind_case(plan.clone(), &inputs, &data, &proposal).unwrap();
        baseline.validate().unwrap();
        for field in ["model", "scaling", "costs", "partition", "target", "window"] {
            let mut goal = proposal.goal.clone();
            match field {
                "model" => goal.model = reference("different-model", &field).unwrap(),
                "scaling" => goal.scaling = reference("different-scaling", &field).unwrap(),
                "costs" => goal.costs = reference("different-costs", &field).unwrap(),
                "partition" => goal.partition = reference("different-partition", &field).unwrap(),
                "target" => goal.target_name = "invented_return".into(),
                "window" => goal.window_end_ns -= 1,
                _ => unreachable!(),
            }
            let changed =
                alpha_engine::representation_plan::propose_representation_comparison(&data, &goal)
                    .unwrap();
            assert!(
                bind_case(plan.clone(), &inputs, &data, &changed).is_err(),
                "{field}"
            );
        }
        let mut changed = data.clone();
        changed.sources[0] = reference("other-raw", &"other-raw").unwrap();
        let recomputed = alpha_engine::representation_plan::propose_representation_comparison(
            &changed,
            &proposal.goal,
        )
        .unwrap();
        assert!(bind_case(plan, &inputs, &changed, &recomputed).is_err());
    }
    #[test]
    fn representation_campaign_cannot_mix_fields_or_admit_two_planning_trials() {
        let (_fixture, inputs, plan, data, proposal) = prepared_case();
        let baseline = bind_case(plan.clone(), &inputs, &data, &proposal).unwrap();
        baseline.validate().unwrap();
        let mut changed = plan.clone();
        changed.feature_fields.remove(0);
        assert!(bind_case(changed, &inputs, &data, &proposal).is_err());
        let mut goal = proposal.goal.clone();
        goal.resource_limit.trials = 2;
        let recomputed =
            alpha_engine::representation_plan::propose_representation_comparison(&data, &goal)
                .unwrap();
        assert!(bind_case(plan.clone(), &inputs, &data, &recomputed).is_err());
        let mut changed = proposal.clone();
        changed.registry_sha256 = "0".repeat(64);
        assert!(bind_case(plan, &inputs, &data, &changed).is_err());
    }
    #[test]
    fn representation_execution_rechecks_actual_resources_and_search_view() {
        let (_fixture, inputs, plan, data, proposal) = prepared_case();
        let bound = bind_case(plan, &inputs, &data, &proposal).unwrap();
        let binding = bound.representation_binding.as_ref().unwrap();
        let limit = &binding.proposal.goal.resource_limit;
        let trials = binding.declared_total_trials;
        let view = &binding.capability.view.view.content_sha256;
        validate_execution_limits(
            &bound,
            limit.cpu_millis,
            limit.memory_mib,
            u64::from(limit.wall_seconds),
            trials,
            view,
        )
        .unwrap();
        assert!(validate_execution_limits(
            &bound,
            limit.cpu_millis + 1,
            limit.memory_mib,
            u64::from(limit.wall_seconds),
            trials,
            view
        )
        .is_err());
        assert!(validate_execution_limits(
            &bound,
            limit.cpu_millis,
            limit.memory_mib + 1,
            u64::from(limit.wall_seconds),
            trials,
            view
        )
        .is_err());
        assert!(validate_execution_limits(
            &bound,
            limit.cpu_millis,
            limit.memory_mib,
            u64::from(limit.wall_seconds) + 1,
            trials,
            view
        )
        .is_err());
        assert!(validate_execution_limits(
            &bound,
            limit.cpu_millis,
            limit.memory_mib,
            u64::from(limit.wall_seconds),
            2,
            view
        )
        .is_err());
        assert!(validate_execution_limits(
            &bound,
            limit.cpu_millis,
            limit.memory_mib,
            u64::from(limit.wall_seconds),
            trials,
            &"0".repeat(64)
        )
        .is_err());
    }
}

/// Test-only metadata assembly. This does not create a raw-verification handle.
#[cfg(all(test, feature = "scientific"))]
pub(crate) fn bind_native_request_for_test(
    request: &mut CampaignRequest,
    root: &VerifiedCampaignRootGrant,
) -> Result<()> {
    request.research_plan = research_plan_for_native_test(request, root)?;
    // Keep the original local transport ID; only the full request SHA changes.
    super::validate_request_for_execute(request)
}

#[cfg(all(test, feature = "scientific"))]
pub(crate) fn research_plan_for_native_test(
    request: &CampaignRequest,
    root: &VerifiedCampaignRootGrant,
) -> Result<CexCampaignResearchPlanV1> {
    let prepared = request
        .prepared_inputs
        .as_ref()
        .context("test request has no native input")?;
    let inputs = PreparedCexInputs::restore_metadata(
        prepared.render_metadata.clone(),
        &request.feature_sha256,
        &request.materialization_sha256,
    )?;
    let template = request.research_plan.clone();
    let seeds = request
        .rounds
        .iter()
        .map(|round| round.seed)
        .collect::<Vec<_>>();
    let rendered = render_template(&inputs, &template, &seeds)?;
    let materialization = inputs.materialization();
    let refs = contract_refs(
        &template,
        &rendered.mission,
        materialization,
        &seeds,
        &request.build_source_revision,
    )?;
    let goal = RepresentationGoalV1 {
        goal: reference("synthetic-goal", &"synthetic-goal")?,
        family_id: root.grant().family.family_id.clone(),
        venue: "binance".into(),
        market: materialization.market.clone(),
        symbol: materialization.symbol.clone(),
        target_name: "forward_mid_return".into(),
        labels: rendered.mission.spec.instrument.horizon.clone(),
        window_start_ns: refs.window_start_ns,
        window_end_ns: refs.window_end_ns,
        model: refs.model,
        scaling: refs.scaling,
        costs: refs.costs,
        partition: refs.partition,
        resource_limit: PlanningResourcesV1 {
            cpu_millis: root.grant().execution.job_cpu_millis,
            memory_mib: root.grant().execution.job_memory_mib,
            wall_seconds: u32::try_from(root.grant().budget.max_job_seconds)?,
            trials: u32::try_from(root.grant().budget.max_trials)?,
        },
    };
    let capability = DataCapabilityV1 {
        schema: CAPABILITY_SCHEMA.into(),
        venue: "binance".into(),
        market: goal.market.clone(),
        symbol: goal.symbol.clone(),
        sources: materialization
            .snapshot
            .source_segments
            .iter()
            .flat_map(|segment| {
                [
                    CexResearchContentRefV1 {
                        id: format!("raw-{}", segment.content_sha256),
                        content_sha256: segment.content_sha256.clone(),
                    },
                    CexResearchContentRefV1 {
                        id: format!("manifest-{}", segment.manifest_sha256),
                        content_sha256: segment.manifest_sha256.clone(),
                    },
                ]
            })
            .collect(),
        normalizer: reference("synthetic-normalizer", &"synthetic-normalizer")?,
        series: vec![BookSeriesCapabilityV1 {
            session_id: "synthetic-planning-series".into(),
            start_available_ns: goal.window_start_ns - 60_000_000_000,
            end_available_ns: goal.window_end_ns,
            snapshots: 1,
            diffs: 10,
            captured_seed_depth: 5,
            continuity: BookContinuityV1::SequenceChecked,
        }],
        fields: vec![FieldClockV1 {
            field: "book".into(),
            unit: "price_and_quantity".into(),
            event_ns: None,
            received_ns: goal.window_start_ns,
            available_ns: goal.window_start_ns,
            decision_ns: goal.window_start_ns,
        }],
        aggregate_trade_direction: true,
        view: PlanningViewV1 {
            view: refs.planning_view,
            family_id: goal.family_id.clone(),
            visibility: PlanningVisibilityV1::Development,
            permission: CexResearchContentRefV1 {
                id: root.grant().root_id.clone(),
                content_sha256: root.content_sha256().into(),
            },
        },
    };
    let proposal =
        alpha_engine::representation_plan::propose_representation_comparison(&capability, &goal)
            .map_err(anyhow::Error::msg)?;
    bind_proposal(
        &capability,
        &proposal,
        "registered_h1_snapshot_family",
        template,
        &inputs,
        &seeds,
        &request.build_source_revision,
    )
}
