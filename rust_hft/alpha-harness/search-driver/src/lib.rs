//! Native governed GP execution over verified development data.
//! This driver owns round mission/trial execution, not native budget authority.
use alpha_domain::{
    canonical_json_hash, CexGpPolicyV1, CexResearchContentRefV1, EvaluationProtocolV1,
    MissionStatus,
};
use alpha_engine::{
    engines::GeneticProgrammingEngine, evaluation::PreparedDataset,
    formula_evaluator::FormulaEvaluator, AutoResearchKernel, ProposalEngine, RunControl,
    RunOutcome,
};
use alpha_store::AlphaStore;
use anyhow::{bail, Result};
use std::collections::BTreeSet;

/// All fields come from the finalized Mission and its actual prepared data.
/// A protocol or policy cannot be widened by the caller's execution arguments.
pub struct NativeGpRequest<'a> {
    pub mission_id: &'a str,
    pub resume: bool,
    pub dataset: &'a PreparedDataset,
    pub original_manifest_id: &'a str,
    pub expected_protocol: &'a EvaluationProtocolV1,
    pub feature_fields: &'a [String],
    pub seed: u64,
    pub policy: &'a CexGpPolicyV1,
    pub candidate_namespace: &'a str,
    pub max_new_iterations: Option<usize>,
}

#[derive(Debug)]
pub struct NativeGpRun {
    pub outcome: RunOutcome,
    pub dataset_manifest_id: String,
    pub research_dataset: CexResearchContentRefV1,
    pub walk_forward_partition: CexResearchContentRefV1,
}

pub fn run(store: &mut AlphaStore, request: NativeGpRequest<'_>) -> Result<NativeGpRun> {
    let dataset = request.dataset;
    if dataset.withheld_metadata().is_none() {
        bail!("native governed search requires verified metadata-only withheld inputs");
    }
    if request.expected_protocol != dataset.protocol() {
        bail!("native search protocol differs from the frozen validation arguments");
    }
    if request.feature_fields.is_empty()
        || request
            .feature_fields
            .iter()
            .any(|field| field.trim().is_empty())
    {
        bail!("mission feature fields are required");
    }
    let mission = store.get_mission(request.mission_id)?;
    match (request.resume, &mission.status) {
        (false, MissionStatus::Pending | MissionStatus::Running)
        | (true, MissionStatus::Paused | MissionStatus::Running) => {}
        (false, _) => bail!("mission run requires a pending or running mission"),
        (true, _) => bail!("mission resume requires a paused or running mission"),
    }
    if mission.dataset_manifest_id.as_str() != request.original_manifest_id {
        bail!("mission dataset id does not match the supplied manifest");
    }
    let fields = request
        .feature_fields
        .iter()
        .map(|field| field.trim().to_owned())
        .collect::<BTreeSet<_>>();
    if fields.is_empty()
        || fields.contains("")
        || fields
            .iter()
            .any(|field| !dataset.feature_names().contains(field))
    {
        bail!(
            "feature fields must be non-empty and registered by the prepared dataset: {:?}",
            dataset.feature_names()
        );
    }
    if fields.into_iter().collect::<Vec<_>>() != request.policy.admitted_fields
        || request.seed != request.policy.seed
        || mission.search_budget != request.policy.budget
    {
        bail!("mission GP execution drifted from its frozen policy");
    }
    let context = dataset.engine_context();
    let rows_sha256 = canonical_json_hash(&context.rows())?;
    let research_dataset = CexResearchContentRefV1 {
        id: format!("cex-research-dataset-{rows_sha256}"),
        content_sha256: rows_sha256,
    };
    let folds_sha256 = canonical_json_hash(&serde_json::json!({
        "research_dataset": &research_dataset, "folds": context.folds(),
    }))?;
    let walk_forward_partition = CexResearchContentRefV1 {
        id: format!("cex-walk-forward-partition-{folds_sha256}"),
        content_sha256: folds_sha256,
    };
    let evaluator = FormulaEvaluator::for_governed_mission(&mission, request.policy)
        .map_err(anyhow::Error::msg)?;
    let engine: Box<dyn ProposalEngine> = Box::new(
        GeneticProgrammingEngine::new_governed(
            request.policy.clone(),
            request.candidate_namespace.to_owned(),
        )
        .map_err(anyhow::Error::msg)?,
    );
    let mut kernel = AutoResearchKernel::new(store, engine, evaluator);
    let outcome = kernel.run(
        request.mission_id,
        dataset,
        RunControl {
            max_new_iterations: request.max_new_iterations,
        },
    )?;
    Ok(NativeGpRun {
        outcome,
        dataset_manifest_id: request.original_manifest_id.to_owned(),
        research_dataset,
        walk_forward_partition,
    })
}
