//! Select existing materialized columns under the original current signed planning view.
use super::{
    declared_total_trials_for_rounds, declared_total_trials_for_validated_plan, CampaignRequest,
};
use crate::{
    mission_dispatch::admission::planning_view::{
        with_authorized_prepared_view, NormalizedPlanningColumns, VerifiedPlanningView,
    },
    mission_render::{CexCampaignResearchPlanV1, PreparedCexInputs, RenderedCexMission},
    mission_runner::Materialization,
};
use alpha_domain::{
    campaign_control::VerifiedCampaignRootGrant, canonical_json_hash, representation::*,
    CexResearchContentRefV1, CexResearchMissionArtifactV1,
};
use anyhow::{bail, ensure, Context, Result};
use hft_cex_research_input::campaign::VerifiedCampaignPreparedInputsV1;
use std::path::Path;

fn reference(id: &str, value: &impl serde::Serialize) -> Result<CexResearchContentRefV1> {
    Ok(CexResearchContentRefV1 {
        id: id.into(),
        content_sha256: canonical_json_hash(value)?,
    })
}

/// Preview only already materialized registered columns. This neither opens raw data nor reserves a Run.
pub fn preview_representation_campaign_contracts(
    control_path: &Path,
    request_path: &Path,
    selected_arm: &str,
) -> Result<RepresentationCampaignContractRefsV1> {
    with_authorized_prepared_view(control_path, request_path, |scope| {
        preview_for_scope(scope, selected_arm)
    })
}

/// Emit the existing research-plan JSON under the shared current Root/Study read permission.
/// Actual dispatch still requires the original signed native admission and cumulative budget.
pub fn bind_signed_prepared_representation_campaign(
    control_path: &Path,
    request_path: &Path,
    selected_arm: &str,
    goal: &RepresentationGoalV1,
) -> Result<serde_json::Value> {
    with_authorized_prepared_view(control_path, request_path, |scope| {
        Ok(serde_json::to_value(bind_for_scope(
            scope,
            selected_arm,
            goal,
        )?)?)
    })
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
        "prepared-column selection requires an unbound registered template"
    );
    let registered = match selected_arm {
        "registered_h1_snapshot_family" => CexCampaignResearchPlanV1::canonical(),
        "registered_h2_lagged_ofi_family" => CexCampaignResearchPlanV1::h2(),
        _ => bail!("unregistered prepared-column family"),
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
        .context("missing actual native seed schedule")?;
    crate::mission_render::render_prepared_cex_bundle(
        inputs,
        plan,
        seed,
        declared_total_trials_for_rounds(plan, seeds.len())?,
    )
}

fn checked_columns(
    data: &VerifiedCampaignPreparedInputsV1,
    plan: &CexCampaignResearchPlanV1,
) -> Result<Vec<String>> {
    let spec = &data.manifest().features.manifest.spec;
    ensure!(
        spec.window == data.original_metadata().development_window
            && data.rows().len() == data.original_metadata().visible_rows.len(),
        "prepared column facts differ from the original development projection"
    );
    for field in &plan.feature_fields {
        ensure!(spec.feature_names.contains(field) && data.rows().iter().all(|row| row.features.get(field).is_some_and(|value| value.is_finite())),
            "registered column {field} is not already materialized; its original producer must prepare it before planning reuse");
    }
    // Inventory contains real column names and values, never inferred raw snapshots, diffs or OFI generation ability.
    Ok(spec.feature_names.clone())
}

fn scope_inputs(scope: &VerifiedPlanningView<'_>) -> Result<PreparedCexInputs> {
    let columns = scope.columns();
    let source = columns.source();
    PreparedCexInputs::restore_metadata(
        scope.render_metadata()?.clone(),
        &source.feature_sha256,
        &source.materialization_sha256,
    )
}

fn checked_normalized_columns(
    data: &NormalizedPlanningColumns<'_>,
    plan: &CexCampaignResearchPlanV1,
) -> Result<Vec<String>> {
    let metadata = data.original_metadata();
    ensure!(
        data.rows().len() == metadata.visible_rows.len()
            && data.rows().iter().all(|row| {
                row.available_time
                    .timestamp_nanos_opt()
                    .is_some_and(|time| {
                        time >= metadata.development_window.start_ns
                            && time < metadata.development_window.end_ns
                    })
            }),
        "normalized column clocks exceed the approved development window"
    );
    for field in &plan.feature_fields {
        ensure!(data.feature_names().contains(field) && data.rows().iter().all(|row| row.features.get(field).is_some_and(|value| value.is_finite())),
            "registered column {field} is not already materialized; original producer preparation is required");
    }
    Ok(data.feature_names().to_vec())
}

fn validate_normalized_binding(
    plan: &CexCampaignResearchPlanV1,
    data: &NormalizedPlanningColumns<'_>,
) -> Result<()> {
    let binding = plan
        .representation_binding
        .as_ref()
        .context("missing prepared-column binding")?;
    ensure!(
        binding.collection_sha256 == data.collection_id()
            && binding.development_rows_sha256 == data.development_rows_sha256()
            && binding.protocol_sha256 == data.protocol_sha256()
            && binding.materialized_columns == checked_normalized_columns(data, plan)?,
        "bound columns differ from the approved normalized projection"
    );
    Ok(())
}

fn preview_for_scope(
    scope: &VerifiedPlanningView<'_>,
    selected_arm: &str,
) -> Result<RepresentationCampaignContractRefsV1> {
    scope.recheck()?;
    let columns = scope.columns();
    let plan = registered_template(scope.research_plan().clone(), selected_arm)?;
    checked_normalized_columns(&columns, &plan)?;
    let inputs = scope_inputs(scope)?;
    let seeds = scope.seeds();
    let rendered = render_template(&inputs, &plan, &seeds)?;
    let refs = contract_refs(
        &plan,
        &rendered.mission,
        inputs.materialization(),
        &seeds,
        scope.runner_source_revision(),
    )?;
    ensure!(
        refs.planning_view == scope.view().view,
        "selected template differs from original signed search view"
    );
    let window = &columns.original_metadata().development_window;
    ensure!(
        i64::try_from(refs.window_start_ns)? == window.start_ns
            && i64::try_from(refs.window_end_ns)? == window.end_ns,
        "goal window differs from actual exclusive-end normalized projection"
    );
    scope.recheck()?;
    Ok(refs)
}

fn bind_for_scope(
    scope: &VerifiedPlanningView<'_>,
    selected_arm: &str,
    goal: &RepresentationGoalV1,
) -> Result<CexCampaignResearchPlanV1> {
    scope.recheck()?;
    goal.validate().map_err(anyhow::Error::msg)?;
    let columns = scope.columns();
    let mut plan = registered_template(scope.research_plan().clone(), selected_arm)?;
    let names = checked_normalized_columns(&columns, &plan)?;
    let inputs = scope_inputs(scope)?;
    let seeds = scope.seeds();
    let declared = declared_total_trials_for_rounds(&plan, seeds.len())?;
    let source = columns.source();
    ensure!(
        goal.family_id == scope.view().family_id,
        "goal belongs to a different signed family"
    );
    plan.representation_binding = Some(RepresentationCampaignBindingV1 {
        schema: "monday.representation_campaign_binding.v1".into(),
        goal: goal.clone(),
        planning_view: scope.view().clone(),
        selected_arm: selected_arm.into(),
        runner_source_revision: scope.runner_source_revision().into(),
        collection_sha256: columns.collection_id().into(),
        development_rows_sha256: columns.development_rows_sha256().into(),
        producer_source_revision: source.build.source_revision.clone(),
        producer_image_identity: source.build.image_identity.clone(),
        preparation_run_id: source.preparation_run_id.clone(),
        preparation_receipt_sha256: source.preparation_receipt_sha256.clone(),
        feature_sha256: source.feature_sha256.clone(),
        materialization_sha256: source.materialization_sha256.clone(),
        replay_artifact_sha256: source.replay_artifact_sha256.clone(),
        replay_manifest_sha256: source.replay_manifest_sha256.clone(),
        protocol_sha256: columns.protocol_sha256().into(),
        materialized_columns: names,
        seeds: seeds.clone(),
        declared_total_trials: declared,
    });
    plan.validate()?;
    let rendered = render_template(&inputs, &plan, &seeds)?;
    let refs = contract_refs(
        &plan,
        &rendered.mission,
        inputs.materialization(),
        &seeds,
        scope.runner_source_revision(),
    )?;
    let window = &columns.original_metadata().development_window;
    ensure!(
        i64::try_from(refs.window_start_ns)? == window.start_ns
            && i64::try_from(refs.window_end_ns)? == window.end_ns,
        "goal window differs from actual exclusive-end normalized projection"
    );
    validate_goal_contracts(plan.representation_binding.as_ref().unwrap(), &refs)?;
    validate_normalized_binding(&plan, &columns)?;
    scope.recheck()?;
    Ok(plan)
}

pub(crate) fn validate_research_plan_binding(
    base: &crate::mission_render::ValidatedCexResearchPlanBase<'_>,
) -> Result<()> {
    let plan = base.plan();
    let Some(binding) = &plan.representation_binding else {
        return Ok(());
    };
    binding.validate().map_err(anyhow::Error::msg)?;
    ensure!(
        plan.generation == 0
            && plan.parent.is_none()
            && plan.learning_directive.is_none()
            && plan.llm.is_none()
            && plan.mlp_training.is_none()
            && plan.search_policy_revision.research_delta.is_none(),
        "prepared-column selection cannot widen the original registered policy scope"
    );
    let expected = match binding.selected_arm.as_str() {
        "registered_h1_snapshot_family" => CexCampaignResearchPlanV1::canonical().feature_fields,
        "registered_h2_lagged_ofi_family" => CexCampaignResearchPlanV1::h2().feature_fields,
        _ => bail!("unregistered prepared-column family"),
    };
    ensure!(
        plan.feature_fields == expected
            && expected
                .iter()
                .all(|field| binding.materialized_columns.contains(field)),
        "Campaign columns differ from the selected materialized family"
    );
    ensure!(
        binding.declared_total_trials
            == declared_total_trials_for_validated_plan(base, binding.seeds.len())?,
        "prepared-column binding changed actual native trial accounting"
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
            "prepared-column seed schedule drifted"
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
        "prepared-column runner differs from actual request Build"
    );
    let reference = request
        .prepared_inputs
        .as_ref()
        .context("prepared-column request requires its original native data")?;
    validate_campaign_seeds(
        &request.research_plan,
        &request
            .rounds
            .iter()
            .map(|round| round.seed)
            .collect::<Vec<_>>(),
    )?;
    let source = &reference.expected_native.source;
    ensure!(
        request.declared_total_trials == binding.declared_total_trials
            && binding.collection_sha256 == reference.collection_sha256
            && binding.development_rows_sha256 == reference.expected_native.development_rows_sha256
            && binding.protocol_sha256 == reference.expected_native.native_protocol_sha256
            && binding.producer_source_revision == source.build.source_revision
            && binding.producer_image_identity == source.build.image_identity
            && binding.preparation_run_id == source.preparation_run_id
            && binding.preparation_receipt_sha256 == source.preparation_receipt_sha256
            && binding.feature_sha256 == source.feature_sha256
            && binding.materialization_sha256 == source.materialization_sha256
            && binding.replay_artifact_sha256 == source.replay_artifact_sha256
            && binding.replay_manifest_sha256 == source.replay_manifest_sha256,
        "prepared-column binding changed its original producer, receipt or data identities"
    );
    let inputs = PreparedCexInputs::restore_metadata(
        reference.render_metadata.clone(),
        &request.feature_sha256,
        &request.materialization_sha256,
    )?;
    ensure!(
        binding.materialized_columns == inputs.feature_manifest().feature_names,
        "column declaration differs from signed prepared metadata"
    );
    let rendered = render_template(&inputs, &request.research_plan, &binding.seeds)?;
    let refs = contract_refs(
        &request.research_plan,
        &rendered.mission,
        inputs.materialization(),
        &binding.seeds,
        &request.build_source_revision,
    )?;
    validate_goal_contracts(binding, &refs)
}

pub(crate) fn validate_verified_prepared_binding(
    plan: &CexCampaignResearchPlanV1,
    prepared: &VerifiedCampaignPreparedInputsV1,
) -> Result<()> {
    let Some(binding) = &plan.representation_binding else {
        return Ok(());
    };
    ensure!(
        binding.collection_sha256 == prepared.id()
            && binding.development_rows_sha256 == prepared.development_rows_sha256()
            && binding.protocol_sha256 == prepared.expected_native().native_protocol_sha256
            && binding.materialized_columns == checked_columns(prepared, plan)?,
        "bound materialized columns differ from actual decoded native inputs"
    );
    Ok(())
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
        "statistical comparison scope differs from actual Campaign accounting"
    );
    let goal = &binding.goal;
    ensure!(
        goal.venue == mission.spec.instrument.venue.as_str()
            && goal.market == materialization.market
            && goal.symbol == materialization.symbol
            && goal.target_name == "forward_mid_return"
            && goal.labels == mission.spec.instrument.horizon,
        "prepared-column goal differs from actual instrument/target"
    );
    ensure!(
        binding.feature_sha256 == mission.spec.inputs.feature.content_sha256
            && binding.materialization_sha256 == mission.spec.inputs.materialization.content_sha256,
        "prepared-column source changed during rendering"
    );
    let refs = contract_refs(
        plan,
        mission,
        materialization,
        &binding.seeds,
        &binding.runner_source_revision,
    )?;
    ensure!(
        binding.protocol_sha256 == mission.spec.evaluation_protocol.content_hash()?,
        "prepared-column native protocol changed"
    );
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
    let visible_end = partitions.search.end;
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
    let goal = &binding.goal;
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
        binding.planning_view.view == refs.planning_view,
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
        "authority checked a different prepared-column request"
    );
    let request: CampaignRequest = serde_json::from_value(value)?;
    validate_campaign_request(&request)?;
    let binding = request
        .research_plan
        .representation_binding
        .as_ref()
        .unwrap();
    ensure!(
        binding.goal.family_id == root.grant().family.family_id
            && binding.planning_view.family_id == root.grant().family.family_id
            && binding.planning_view.permission.content_sha256 == root.content_sha256(),
        "prepared-column permission differs from authenticated original root"
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
    let limit = &binding.goal.resource_limit;
    ensure!(
        cpu_millis <= limit.cpu_millis
            && memory_mib <= limit.memory_mib
            && seconds <= u64::from(limit.wall_seconds)
            && trials == binding.declared_total_trials
            && trials <= limit.trials as usize,
        "actual native resources exceed the frozen column-selection goal"
    );
    ensure!(
        binding.planning_view.view.content_sha256 == search_view_sha256,
        "prepared-column view differs from actual admitted search view"
    );
    Ok(())
}

#[cfg(all(test, feature = "scientific"))]
pub(crate) fn prepared_plan_for_fixture(
    scope: &VerifiedPlanningView<'_>,
    root: &VerifiedCampaignRootGrant,
) -> Result<CexCampaignResearchPlanV1> {
    let refs = preview_for_scope(scope, "registered_h1_snapshot_family")?;
    let inputs = scope_inputs(scope)?;
    let materialization = inputs.materialization();
    let goal = RepresentationGoalV1 {
        goal: reference("prepared-fixture-goal", &scope.columns().collection_id())?,
        family_id: scope.view().family_id.clone(),
        venue: "binance".into(),
        market: materialization.market.clone(),
        symbol: materialization.symbol.clone(),
        target_name: "forward_mid_return".into(),
        labels: alpha_domain::EvaluationLabelSpecV1 {
            horizon_buckets: materialization.label_horizon_buckets,
            observation_frequency_millis: materialization.bucket_ms,
        },
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
    ensure!(
        scope.runner_source_revision() == root.grant().execution.source_revision,
        "fixture runner source differs from signed authority"
    );
    let bound = bind_for_scope(scope, "registered_h1_snapshot_family", &goal)?;
    assert_scope_regressions_for_fixture(scope, &goal, &bound)?;
    Ok(bound)
}

#[cfg(all(test, feature = "scientific"))]
fn assert_scope_regressions_for_fixture(
    scope: &VerifiedPlanningView<'_>,
    goal: &RepresentationGoalV1,
    bound: &CexCampaignResearchPlanV1,
) -> Result<()> {
    // The baseline is actual decoded materialized data under current signed authority.
    bound.validate()?;
    validate_normalized_binding(bound, &scope.columns())?;
    let declared = declared_total_trials_for_rounds(
        bound,
        bound.representation_binding.as_ref().unwrap().seeds.len(),
    )?;
    ensure!(
        declared
            == bound
                .representation_binding
                .as_ref()
                .unwrap()
                .declared_total_trials
            && declared > 2,
        "scope fixture did not retain original scientific accounting"
    );
    let inputs = scope_inputs(scope)?;
    let seeds = bound.representation_binding.as_ref().unwrap().seeds.clone();
    render_template(&inputs, bound, &seeds)?;
    for change in [
        "model",
        "scaling",
        "cost",
        "partition",
        "target",
        "window",
        "horizon",
        "family",
        "budget",
    ] {
        let mut goal = goal.clone();
        match change {
            "model" => goal.model.content_sha256 = "0".repeat(64),
            "scaling" => goal.scaling.content_sha256 = "0".repeat(64),
            "cost" => goal.costs.content_sha256 = "0".repeat(64),
            "partition" => goal.partition.content_sha256 = "0".repeat(64),
            "target" => goal.target_name = "unregistered_target".into(),
            "window" => goal.window_end_ns -= 1,
            "horizon" => goal.labels.horizon_buckets += 1,
            "family" => goal.family_id = "different-family".into(),
            "budget" => goal.resource_limit.trials = 2,
            _ => unreachable!(),
        }
        ensure!(
            bind_for_scope(scope, "registered_h1_snapshot_family", &goal).is_err(),
            "prepared fixture accepted {change} drift"
        );
    }
    ensure!(
        preview_for_scope(scope, "invented_family").is_err(),
        "prepared fixture accepted an unregistered column family"
    );
    if !scope
        .columns()
        .feature_names()
        .contains(&"cont_ofi_lag60s".into())
    {
        ensure!(
            preview_for_scope(scope, "registered_h2_lagged_ofi_family").is_err(),
            "missing OFI column was treated as rematerializable"
        );
    }
    let mut changed = bound.clone();
    changed
        .representation_binding
        .as_mut()
        .unwrap()
        .collection_sha256 = "0".repeat(64);
    ensure!(
        validate_normalized_binding(&changed, &scope.columns()).is_err(),
        "actual decoded collection identity was ignored"
    );
    let mut changed = bound.clone();
    changed
        .representation_binding
        .as_mut()
        .unwrap()
        .runner_source_revision = "abcdef0123456789abcdef0123456789abcdef01".into();
    ensure!(
        render_template(&inputs, &changed, &seeds).is_err(),
        "bound runner drift was ignored"
    );
    let mut changed = bound.clone();
    changed
        .representation_binding
        .as_mut()
        .unwrap()
        .materialized_columns
        .retain(|name| name != "spread_bps");
    ensure!(
        changed.validate().is_err(),
        "selected registered column was removed"
    );
    scope.recheck()?;
    Ok(())
}

#[cfg(test)]
mod base_tests {
    use super::*;
    #[test]
    fn ordinary_accounting_still_requires_complete_plan_validation() {
        let plan = CexCampaignResearchPlanV1::canonical();
        assert_eq!(plan.max_candidates().unwrap(), 22);
        assert_eq!(declared_total_trials_for_rounds(&plan, 2).unwrap(), 88);
        let mut invalid = plan;
        invalid.schema_version = "untrusted-schema".into();
        assert!(invalid.max_candidates().is_err());
        assert!(declared_total_trials_for_rounds(&invalid, 2).is_err());
    }
}
