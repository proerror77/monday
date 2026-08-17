use crate::{
    cli::{
        print_json, CandidateShowArgs, EnvelopeArgs, EvaluateArgs, FeedbackLogArgs,
        FeedbackRecordArgs, JsonRecordArgs, MissionStatusArgs, PromoteArgs, RegisterOnnxArgs,
        RevokeApprovalArgs, SignDeploymentArgs,
    },
    data_mission,
};
use alpha_domain::{
    canonical_json_hash, deployment_scope_hash, sign_envelope, verify_runtime_attribution_event,
    ApprovalClass, CandidateArtifact, DeploymentEnvelope, EngineKind, IterationVerdict,
    MissionStatus, MissionTerminalReason, OnnxModelCandidate, PromotionRecord, ResearchIteration,
    SearchBudgetLimit, SearchBudgetUsage, SearchPolicyRevision, SignedRuntimeAttributionEvent,
    StrategyBundle, VerifiedRuntimeAttributionEvent, ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION,
    ONNX_WALK_FORWARD_EVALUATOR_VERSION, SEALED_HOLDOUT_EVALUATOR_VERSION,
};
use alpha_engine::{
    evaluation::prepare_dataset,
    formula_evaluator::{FormulaEvaluator, WALK_FORWARD_EVALUATOR_VERSION},
    CandidateEvaluation, EngineProposal,
};
use alpha_onnx_evaluator::OnnxEvaluator;
use alpha_store::{
    AlphaStore, ApprovalRecord, EvaluationRecord, MissionLineage, RegistryRevision, StoreError,
};
use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use ed25519_dalek::{SigningKey, VerifyingKey};
use hft_factor_dsl::validate_live_formula;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_BUNDLE_STAGE_ID: AtomicU64 = AtomicU64::new(0);

pub fn candidate_list(args: MissionStatusArgs) -> anyhow::Result<()> {
    let store = AlphaStore::open(args.db)?;
    let lineage = store.mission_lineage(&args.mission_id)?;
    print_json(&serde_json::json!({
        "mission_id": args.mission_id,
        "candidates": lineage.candidates,
        "evaluations": lineage.evaluations,
    }))
}

pub fn candidate_show(args: CandidateShowArgs) -> anyhow::Result<()> {
    print_json(&candidate_show_report(&args)?)
}

pub(crate) fn candidate_show_report(args: &CandidateShowArgs) -> anyhow::Result<serde_json::Value> {
    let store = AlphaStore::open(&args.db)?;
    let lineage = store.mission_lineage(&args.mission_id)?;
    let candidate = lineage
        .candidates
        .iter()
        .find(|candidate| candidate.candidate_id == args.candidate_id)
        .with_context(|| {
            format!(
                "candidate {} does not belong to mission {}",
                args.candidate_id, args.mission_id
            )
        })?;
    let mut evaluations = Vec::new();
    for stored in lineage
        .evaluations
        .iter()
        .filter(|stored| stored.record.candidate_id == args.candidate_id)
    {
        let evaluation: CandidateEvaluation =
            serde_json::from_value(stored.record.payload.clone()).with_context(|| {
                format!(
                    "evaluation {} payload is malformed",
                    stored.record.evaluation_id
                )
            })?;
        evaluations.push(serde_json::json!({
            "evaluation_id": stored.record.evaluation_id,
            "content_hash": stored.content_hash,
            "dataset_manifest_id": stored.record.dataset_manifest_id,
            "evaluation_protocol_hash": stored.record.evaluation_protocol_hash,
            "created_at": stored.record.created_at,
            "evaluation": evaluation,
        }));
    }
    Ok(serde_json::json!({
        "mission_id": args.mission_id,
        "candidate": candidate,
        "evaluations": evaluations,
    }))
}

pub(crate) fn validated_walk_forward_candidates(
    store: &AlphaStore,
    mission_id: &str,
) -> anyhow::Result<Vec<String>> {
    let lineage = store.mission_lineage(mission_id)?;
    validated_walk_forward_candidates_in_lineage(&lineage)
}

fn validated_walk_forward_candidates_in_lineage(
    lineage: &MissionLineage,
) -> anyhow::Result<Vec<String>> {
    Ok(validated_walk_forward_evidence_in_lineage(lineage)?
        .into_iter()
        .map(|(candidate_id, _)| candidate_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn selected_walk_forward_candidate_in_lineage(
    lineage: &MissionLineage,
) -> anyhow::Result<Option<(String, String)>> {
    let evidence = validated_walk_forward_evidence_in_lineage(lineage)?;
    match evidence.as_slice() {
        [] => Ok(None),
        [selected] => Ok(Some(selected.clone())),
        _ => bail!("holdout evaluation requires exactly one canonical walk-forward candidate"),
    }
}

fn validated_walk_forward_evidence_in_lineage(
    lineage: &MissionLineage,
) -> anyhow::Result<Vec<(String, String)>> {
    let expected_config = FormulaEvaluator::for_mission(&lineage.mission)
        .map_err(anyhow::Error::msg)?
        .config_evidence()
        .map_err(anyhow::Error::msg)?;
    let mut candidates = Vec::new();
    for iteration in &lineage.iterations {
        if iteration.verdict != IterationVerdict::Keep {
            continue;
        }
        let (Some(candidate_id), Some(evaluation_id)) = (
            iteration.candidate_artifact_id.as_deref(),
            iteration.evaluation_artifact_id.as_deref(),
        ) else {
            continue;
        };
        let Some(stored) = lineage.evaluations.iter().find(|stored| {
            stored.record.evaluation_id == evaluation_id
                && stored.record.candidate_id == candidate_id
        }) else {
            continue;
        };
        let evaluator_version = stored
            .record
            .payload
            .get("evaluator_version")
            .and_then(serde_json::Value::as_str);
        if !matches!(
            evaluator_version,
            Some(WALK_FORWARD_EVALUATOR_VERSION | ONNX_WALK_FORWARD_EVALUATOR_VERSION)
        ) {
            continue;
        }
        let Some(candidate) = lineage
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_id == candidate_id)
        else {
            continue;
        };
        if !matches!(
            (&candidate.artifact, evaluator_version),
            (
                CandidateArtifact::Formula(_),
                Some(WALK_FORWARD_EVALUATOR_VERSION)
            ) | (
                CandidateArtifact::OnnxModel(_),
                Some(ONNX_WALK_FORWARD_EVALUATOR_VERSION)
            )
        ) {
            continue;
        }
        if let CandidateArtifact::Formula(ast) = &candidate.artifact {
            if validate_live_formula(ast).is_err() {
                continue;
            }
        }
        let evaluation: CandidateEvaluation = serde_json::from_value(stored.record.payload.clone())
            .with_context(|| format!("walk-forward evaluation {evaluation_id} is malformed"))?;
        evaluation
            .validate()
            .map_err(anyhow::Error::new)
            .with_context(|| format!("walk-forward evaluation {evaluation_id} is invalid"))?;
        if evaluation.passed && evaluation.evaluator_config == expected_config {
            let (_, protocol_hash) = evaluation
                .protocol_binding()
                .map_err(anyhow::Error::new)
                .with_context(|| {
                    format!("walk-forward evaluation {evaluation_id} has no bound protocol")
                })?;
            if stored.record.dataset_manifest_id != lineage.mission.dataset_manifest_id.as_str()
                || stored.record.evaluation_protocol_hash != protocol_hash
            {
                continue;
            }
            candidates.push((candidate_id.to_string(), protocol_hash.to_string()));
        }
    }
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

pub fn register_onnx_candidate(args: RegisterOnnxArgs) -> anyhow::Result<()> {
    let mut store = AlphaStore::open(&args.db)?;
    let mission = store.get_mission(&args.mission_id)?;
    let lineage = store.mission_lineage(&args.mission_id)?;
    if mission.status != MissionStatus::Pending
        || mission.completion_policy.min_kept_candidates != 1
        || !lineage.iterations.is_empty()
    {
        bail!("ONNX registration requires an empty pending one-candidate mission");
    }
    let model: OnnxModelCandidate = serde_json::from_slice(
        &std::fs::read(&args.model)
            .with_context(|| format!("failed to read ONNX candidate {}", args.model.display()))?,
    )?;
    model.validate().map_err(anyhow::Error::new)?;
    let model_path = verified_model_path(&model, &args.model_root)?;
    let manifest =
        data_mission::read_registered_research_dataset(&store, &args.dataset.dataset_manifest)?;
    if mission.dataset_manifest_id.as_str() != manifest.manifest_id() {
        bail!("mission dataset id does not match the supplied manifest");
    }
    let labels = manifest.evaluation_label_spec()?;
    let protocol = args.dataset.validation.evaluation_protocol(&labels)?;
    let dataset = prepare_dataset(manifest.load_rows(&protocol.costs)?, &protocol)?;
    let evaluation = OnnxEvaluator::for_mission(&mission)
        .map_err(anyhow::Error::msg)?
        .evaluate(&model, &model_path, &dataset.engine_context())
        .map_err(anyhow::Error::msg)?;
    let now = Utc::now();
    store.bind_mission_evaluation_protocol(&mission.mission_id, false, &protocol, now)?;
    let evaluation_id = format!("{}-onnx-evaluation-1", args.mission_id);
    let iteration = ResearchIteration {
        iteration_id: format!("{}-onnx-iteration-1", args.mission_id),
        mission_id: args.mission_id.clone(),
        parent_candidate_ids: vec![],
        engine: EngineKind::ManualSeed,
        hypothesis: args.hypothesis,
        candidate_artifact_id: Some(args.candidate_id.clone()),
        evaluation_artifact_id: Some(evaluation_id.clone()),
        budget_usage: SearchBudgetUsage {
            candidates: 1,
            ..SearchBudgetUsage::default()
        },
        verdict: if evaluation.passed {
            IterationVerdict::Keep
        } else {
            IterationVerdict::Discard
        },
        failure_class: None,
        failure_explanation: (!evaluation.passed).then(|| evaluation.failure_reasons.join("; ")),
        created_at: now,
    };
    let candidate = CandidateArtifact::OnnxModel(model);
    let record = EvaluationRecord {
        evaluation_id,
        mission_id: args.mission_id.clone(),
        candidate_id: args.candidate_id.clone(),
        dataset_manifest_id: mission.dataset_manifest_id.as_str().to_string(),
        evaluation_protocol_hash: protocol.content_hash()?.to_string(),
        payload: serde_json::to_value(&evaluation)?,
        created_at: now,
    };
    store.transition_mission(&args.mission_id, MissionStatus::Running, now)?;
    store.append_iteration(
        &iteration,
        Some((&args.candidate_id, &candidate)),
        Some(&record),
    )?;
    let reason = if evaluation.passed {
        MissionTerminalReason::CompletionPolicySatisfied { kept_candidates: 1 }
    } else {
        MissionTerminalReason::SearchBudgetExhausted {
            exhausted_limits: vec![SearchBudgetLimit::Candidates],
        }
    };
    store.finish_mission(&args.mission_id, reason, Utc::now())?;
    print_json(&serde_json::json!({
        "candidate_id": args.candidate_id,
        "evaluation": evaluation,
        "model_path": model_path,
    }))
}

pub fn evaluate(args: EvaluateArgs) -> anyhow::Result<()> {
    print_json(&execute_evaluate(args)?)
}

pub(crate) fn execute_evaluate(args: EvaluateArgs) -> anyhow::Result<RegistryRevision> {
    let mut store = AlphaStore::open(&args.db)?;
    let lineage = store.mission_lineage(&args.mission_id)?;
    let manifest =
        data_mission::read_registered_research_dataset(&store, &args.dataset.dataset_manifest)?;
    if lineage.mission.dataset_manifest_id.as_str() != manifest.manifest_id() {
        bail!("mission dataset id does not match the supplied manifest");
    }
    let labels = manifest.evaluation_label_spec()?;
    let requested_protocol = args.dataset.validation.evaluation_protocol(&labels)?;
    let requested_protocol_hash = requested_protocol.content_hash()?;
    let Some((selected_candidate_id, selected_protocol_hash)) =
        selected_walk_forward_candidate_in_lineage(&lineage)?
    else {
        bail!("candidate lacks canonical walk-forward evidence for this evaluation protocol");
    };
    if selected_candidate_id != args.candidate_id
        || selected_protocol_hash != requested_protocol_hash
    {
        bail!("only the selected canonical walk-forward candidate can access holdout");
    }
    store.bind_mission_evaluation_protocol(
        &lineage.mission.mission_id,
        !lineage.iterations.is_empty(),
        &requested_protocol,
        Utc::now(),
    )?;
    let candidate = lineage
        .candidates
        .iter()
        .find(|candidate| candidate.candidate_id == args.candidate_id)
        .context("candidate does not belong to mission")?;
    let iteration = lineage
        .iterations
        .iter()
        .find(|iteration| {
            iteration.candidate_artifact_id.as_deref() == Some(args.candidate_id.as_str())
        })
        .context("candidate iteration is missing")?;
    if iteration.verdict != IterationVerdict::Keep {
        bail!("only a candidate that passed walk-forward evaluation can access holdout");
    }
    if iteration.engine == alpha_domain::EngineKind::OfflineReinforcementLearning {
        bail!("offline RL candidates are lab search-policy output and cannot access holdout");
    }
    let expected_sealed_version = sealed_evaluator_version(&candidate.artifact)?;
    let revision_id = sealed_evaluation_revision_id(&args.candidate_id, expected_sealed_version);
    let existing = match store.get_registry_revision(&revision_id) {
        Ok(existing) => Some(existing),
        Err(StoreError::NotFound) => None,
        Err(error) => return Err(error.into()),
    };

    if let Some(existing) = existing {
        let existing_evaluation: CandidateEvaluation = serde_json::from_value(
            existing
                .payload
                .get("evaluation")
                .cloned()
                .context("existing sealed evaluation payload is incomplete")?,
        )
        .context("existing sealed evaluation is legacy or malformed")?;
        existing_evaluation
            .validate()
            .map_err(anyhow::Error::new)
            .context("existing sealed evaluation is invalid")?;
        let expected_config = FormulaEvaluator::for_mission(&lineage.mission)
            .map_err(anyhow::Error::msg)?
            .config_evidence()
            .map_err(anyhow::Error::msg)?;
        if existing.registry_kind != "sealed_evaluation"
            || existing.asset_id != candidate.candidate_id
            || existing
                .payload
                .get("mission_id")
                .and_then(serde_json::Value::as_str)
                != Some(args.mission_id.as_str())
            || existing
                .payload
                .get("candidate_content_hash")
                .and_then(serde_json::Value::as_str)
                != Some(candidate.content_hash.as_str())
            || existing
                .payload
                .get("dataset_manifest_id")
                .and_then(serde_json::Value::as_str)
                != Some(lineage.mission.dataset_manifest_id.as_str())
            || existing_evaluation.evaluator_version != expected_sealed_version
            || existing_evaluation.evaluator_config != expected_config
            || existing
                .payload
                .get("evaluation_protocol_hash")
                .and_then(serde_json::Value::as_str)
                != Some(requested_protocol_hash.as_str())
            || existing_evaluation
                .protocol_binding()
                .map(|(_, hash)| hash)
                .ok()
                != Some(requested_protocol_hash.as_str())
        {
            bail!("existing sealed evaluation is not canonical evidence for this protocol");
        }
        return Ok(existing);
    }

    let rows = manifest.load_rows(&requested_protocol.costs)?;
    let dataset = prepare_dataset(rows, &requested_protocol)?;
    let proposal = EngineProposal {
        candidate_id: candidate.candidate_id.clone(),
        hypothesis: iteration.hypothesis.clone(),
        artifact: candidate.artifact.clone(),
        expansions: 0,
        tokens: 0,
        elapsed_ms: 0,
    };
    let evaluation = match &candidate.artifact {
        CandidateArtifact::Formula(_) => FormulaEvaluator::for_mission(&lineage.mission)
            .map_err(anyhow::Error::msg)?
            .evaluate_sealed(&proposal, &dataset)
            .map_err(anyhow::Error::msg)?,
        CandidateArtifact::OnnxModel(model) => {
            let root = args
                .model_root
                .as_deref()
                .context("--model-root is required for ONNX sealed evaluation")?;
            let model_path = verified_model_path(model, root)?;
            OnnxEvaluator::for_mission(&lineage.mission)
                .map_err(anyhow::Error::msg)?
                .evaluate_sealed(model, &model_path, &dataset)
                .map_err(anyhow::Error::msg)?
        }
        _ => bail!("candidate artifact has no governed sealed evaluator"),
    };
    let revision = RegistryRevision {
        revision_id,
        registry_kind: "sealed_evaluation".to_string(),
        asset_id: args.candidate_id,
        parent_revision_id: None,
        payload: serde_json::json!({
            "mission_id": args.mission_id,
            "candidate_content_hash": candidate.content_hash,
            "dataset_manifest_id": manifest.manifest_id(),
            "evaluation_protocol_hash": requested_protocol_hash,
            "evaluation": evaluation,
        }),
        created_at: Utc::now(),
    };
    store.put_registry_revision(&revision)?;
    Ok(revision)
}

fn sealed_evaluator_version(artifact: &CandidateArtifact) -> anyhow::Result<&'static str> {
    match artifact {
        CandidateArtifact::Formula(ast) => {
            validate_live_formula(ast).map_err(anyhow::Error::new)?;
            Ok(SEALED_HOLDOUT_EVALUATOR_VERSION)
        }
        CandidateArtifact::OnnxModel(_) => Ok(ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION),
        _ => bail!("candidate artifact has no governed sealed evaluator"),
    }
}

pub(crate) fn sealed_evaluation_revision_id(candidate_id: &str, evaluator_version: &str) -> String {
    format!("sealed-evaluation:{evaluator_version}:{candidate_id}")
}

fn verified_model_path(model: &OnnxModelCandidate, root: &Path) -> anyhow::Result<PathBuf> {
    let uri = model.artifact.uri.as_str();
    let relative = Path::new(uri);
    if uri.contains("://")
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        bail!("ONNX artifact path must remain inside the model root");
    }
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve model root {}", root.display()))?;
    let path = canonical_root
        .join(relative)
        .canonicalize()
        .with_context(|| format!("failed to resolve ONNX artifact {}", relative.display()))?;
    if !path.starts_with(&canonical_root) || !path.is_file() {
        bail!("ONNX artifact escapes the model root or is not a regular file");
    }
    let bytes = std::fs::read(&path)?;
    let checksum = hex::encode(Sha256::digest(&bytes));
    if bytes.len() as u64 != model.byte_len
        || model.artifact.checksum.as_deref() != Some(checksum.as_str())
    {
        bail!("ONNX artifact size or checksum does not match candidate metadata");
    }
    Ok(path)
}

pub fn promote(args: PromoteArgs) -> anyhow::Result<()> {
    let bundle_out = args.bundle_out.clone();
    let model_root = args.model_root.clone();
    let mut store = AlphaStore::open(&args.db)?;
    let lineage = store.mission_lineage(&args.mission_id)?;
    data_mission::require_promotable_research_dataset(
        &store,
        lineage.mission.dataset_manifest_id.as_str(),
    )?;
    let candidate = lineage
        .candidates
        .iter()
        .find(|candidate| candidate.candidate_id == args.candidate_id)
        .context("candidate does not belong to mission")?;
    let walk_forward_protocol_hashes = validated_walk_forward_evidence_in_lineage(&lineage)?
        .into_iter()
        .filter_map(|(candidate_id, protocol_hash)| {
            (candidate_id == args.candidate_id).then_some(protocol_hash)
        })
        .collect::<BTreeSet<_>>();
    let Some(walk_forward_protocol_hash) = walk_forward_protocol_hashes
        .iter()
        .next()
        .filter(|_| walk_forward_protocol_hashes.len() == 1)
    else {
        bail!("candidate lacks a unique canonical walk-forward protocol binding");
    };
    let expected_sealed_version = sealed_evaluator_version(&candidate.artifact)?;
    let sealed_id = sealed_evaluation_revision_id(&args.candidate_id, expected_sealed_version);
    let sealed = store.get_registry_revision(&sealed_id)?;
    let sealed_evaluation = sealed
        .payload
        .get("evaluation")
        .cloned()
        .context("sealed evaluation payload is incomplete")?;
    let evaluation: CandidateEvaluation = serde_json::from_value(sealed_evaluation.clone())?;
    evaluation.validate().map_err(anyhow::Error::new)?;
    if evaluation.evaluator_version != expected_sealed_version {
        bail!("candidate sealed evaluation uses the wrong evaluator version");
    }
    if !evaluation.passed {
        bail!("candidate failed sealed holdout and cannot be promoted");
    }
    let (sealed_protocol, sealed_protocol_hash) = evaluation
        .protocol_binding()
        .map_err(anyhow::Error::new)
        .context("sealed evaluation has no valid protocol binding")?;
    let evaluation_protocol_hash = sealed_protocol_hash.to_string();
    store.require_mission_evaluation_protocol(&lineage.mission.mission_id, sealed_protocol)?;
    if sealed.asset_id != candidate.candidate_id
        || sealed
            .payload
            .get("mission_id")
            .and_then(serde_json::Value::as_str)
            != Some(args.mission_id.as_str())
        || sealed
            .payload
            .get("candidate_content_hash")
            .and_then(serde_json::Value::as_str)
            != Some(candidate.content_hash.as_str())
        || sealed
            .payload
            .get("dataset_manifest_id")
            .and_then(serde_json::Value::as_str)
            != Some(lineage.mission.dataset_manifest_id.as_str())
        || sealed
            .payload
            .get("evaluation_protocol_hash")
            .and_then(serde_json::Value::as_str)
            != Some(sealed_protocol_hash)
        || sealed_protocol_hash != walk_forward_protocol_hash.as_str()
    {
        bail!("sealed evaluation binding does not match candidate and mission");
    }
    let promotion_id = args
        .promotion_id
        .unwrap_or_else(|| format!("promotion:{}", args.candidate_id));
    let existing = match store.get_promotion(&promotion_id) {
        Ok(existing) => Some(existing),
        Err(StoreError::NotFound) => None,
        Err(error) => return Err(error.into()),
    };
    let now = existing
        .as_ref()
        .map(|existing| existing.record.created_at)
        .unwrap_or_else(Utc::now);
    let evaluator_config_hash = canonical_json_hash(
        sealed_evaluation
            .get("evaluator_config")
            .context("sealed evaluator config is missing")?,
    )?;
    let evaluation_metrics_hash = canonical_json_hash(
        sealed_evaluation
            .get("metrics")
            .context("sealed evaluation metrics are missing")?,
    )?;
    let sealed_evaluation_hash = canonical_json_hash(&sealed_evaluation)?;
    let bundle = StrategyBundle::new(
        format!("bundle:{}", candidate.candidate_id),
        candidate.candidate_id.clone(),
        candidate.content_hash.clone(),
        lineage.mission.dataset_manifest_id.clone(),
        evaluation.evaluator_version.clone(),
        evaluation_protocol_hash.clone(),
        evaluator_config_hash.clone(),
        evaluation_metrics_hash.clone(),
        sealed_evaluation_hash.clone(),
        candidate.artifact.to_governed_strategy_bundle_artifact()?,
        now,
    )?;
    let promotion = PromotionRecord {
        promotion_id,
        mission_id: args.mission_id,
        candidate_id: candidate.candidate_id.clone(),
        candidate_content_hash: candidate.content_hash.clone(),
        dataset_manifest_id: lineage.mission.dataset_manifest_id.clone(),
        evaluator_version: evaluation.evaluator_version,
        evaluation_protocol_hash,
        evaluator_config_hash,
        evaluation_metrics_hash,
        sealed_evaluation_id: sealed_id,
        sealed_evaluation_hash,
        bundle_id: bundle.bundle_id.clone(),
        bundle_hash: bundle.bundle_hash.clone(),
        created_at: now,
    };
    if let Some(existing) = existing {
        let stored_bundle = store.get_strategy_bundle(&existing.record.bundle_id)?;
        ensure_exact_promotion_replay(&existing.record, &stored_bundle, &promotion, &bundle)?;
        materialize_bundle(&stored_bundle, bundle_out.as_deref(), model_root.as_deref())?;
        return print_json(&serde_json::json!({
            "promotion": existing,
            "bundle": stored_bundle,
        }));
    }
    let staged = stage_bundle(&bundle, bundle_out.as_deref(), model_root.as_deref())?;
    let stored = store.promote_candidate(&bundle, &promotion)?;
    staged.publish()?;
    print_json(&serde_json::json!({
        "promotion": stored,
        "bundle": bundle,
    }))
}

fn materialize_bundle(
    bundle: &StrategyBundle,
    bundle_out: Option<&Path>,
    model_root: Option<&Path>,
) -> anyhow::Result<()> {
    stage_bundle(bundle, bundle_out, model_root)?.publish()
}

struct StagedBundle {
    staging_dir: Option<PathBuf>,
    created_dirs: Vec<PathBuf>,
    bundle_out: Option<PathBuf>,
    staged_bundle: Option<PathBuf>,
    model_target: Option<PathBuf>,
    staged_model: Option<PathBuf>,
    model: Option<OnnxModelCandidate>,
}

impl StagedBundle {
    fn publish(mut self) -> anyhow::Result<()> {
        let mut published_model = None;
        let result = (|| {
            if let Some(staged_model) = self.staged_model.take() {
                let model_target = self
                    .model_target
                    .clone()
                    .context("staged ONNX artifact has no target path")?;
                let model = self
                    .model
                    .clone()
                    .context("staged ONNX artifact has no model metadata")?;
                let target_parent = model_target
                    .parent()
                    .context("ONNX bundle target has no parent directory")?;
                let bundle_dir = self
                    .bundle_out
                    .as_ref()
                    .and_then(|path| path.parent())
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."))
                    .canonicalize()?;
                if !target_parent.canonicalize()?.starts_with(&bundle_dir) {
                    bail!("ONNX bundle target escapes the bundle directory");
                }
                if model_target.exists() {
                    verify_onnx_file(&model_target, &model, "existing ONNX bundle artifact")?;
                    std::fs::remove_file(staged_model)?;
                } else {
                    match std::fs::hard_link(&staged_model, &model_target) {
                        Ok(()) => published_model = Some((model_target, model.clone())),
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            verify_onnx_file(
                                &model_target,
                                &model,
                                "concurrently published ONNX bundle artifact",
                            )?;
                            std::fs::remove_file(staged_model)?;
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            if let Some(staged_bundle) = self.staged_bundle.take() {
                let bundle_out = self
                    .bundle_out
                    .clone()
                    .context("staged strategy bundle has no output path")?;
                if bundle_out.exists() {
                    let existing: StrategyBundle =
                        serde_json::from_slice(&std::fs::read(&bundle_out)?)?;
                    let requested: StrategyBundle =
                        serde_json::from_slice(&std::fs::read(&staged_bundle)?)?;
                    if existing != requested {
                        bail!("existing strategy bundle has different content");
                    }
                    std::fs::remove_file(staged_bundle)?;
                } else {
                    match std::fs::hard_link(&staged_bundle, &bundle_out) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            let existing: StrategyBundle =
                                serde_json::from_slice(&std::fs::read(&bundle_out)?)?;
                            let requested: StrategyBundle =
                                serde_json::from_slice(&std::fs::read(&staged_bundle)?)?;
                            if existing != requested {
                                bail!(
                                    "concurrently published strategy bundle has different content"
                                );
                            }
                            std::fs::remove_file(staged_bundle)?;
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Some((path, model)) = published_model {
                let published_bundle_uses_model = self
                    .bundle_out
                    .as_deref()
                    .and_then(|bundle_out| std::fs::read(bundle_out).ok())
                    .and_then(|bytes| serde_json::from_slice::<StrategyBundle>(&bytes).ok())
                    .is_some_and(|bundle| {
                        matches!(
                            bundle.artifact,
                            alpha_domain::StrategyBundleArtifact::Onnx {
                                model: published_model
                            } if published_model == model
                        )
                    });
                if !published_bundle_uses_model {
                    let _ = std::fs::remove_file(path);
                }
            }
            let _ = self.cleanup();
            return Err(error);
        }
        self.cleanup_staging()?;
        self.created_dirs.clear();
        Ok(())
    }

    fn cleanup_staging(&mut self) -> anyhow::Result<()> {
        if let Some(staging_dir) = self.staging_dir.take() {
            if staging_dir.exists() {
                std::fs::remove_dir_all(staging_dir)?;
            }
        }
        Ok(())
    }

    fn cleanup(&mut self) -> anyhow::Result<()> {
        self.cleanup_staging()?;
        for path in self.created_dirs.iter().rev() {
            if path.exists() && std::fs::read_dir(path)?.next().is_none() {
                match std::fs::remove_dir(path) {
                    Ok(()) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                        ) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        self.created_dirs.clear();
        Ok(())
    }
}

impl Drop for StagedBundle {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn stage_bundle(
    bundle: &StrategyBundle,
    bundle_out: Option<&Path>,
    model_root: Option<&Path>,
) -> anyhow::Result<StagedBundle> {
    let Some(bundle_out) = bundle_out else {
        if matches!(
            &bundle.artifact,
            alpha_domain::StrategyBundleArtifact::Onnx { .. }
        ) {
            bail!("ONNX promotion requires --bundle-out and --model-root");
        }
        return Ok(StagedBundle {
            staging_dir: None,
            created_dirs: Vec::new(),
            bundle_out: None,
            staged_bundle: None,
            model_target: None,
            staged_model: None,
            model: None,
        });
    };
    let bundle_dir = bundle_out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let created_dirs = create_dir_all_tracked(bundle_dir)?;
    let mut staged = StagedBundle {
        staging_dir: None,
        created_dirs,
        bundle_out: Some(bundle_out.to_path_buf()),
        staged_bundle: None,
        model_target: None,
        staged_model: None,
        model: None,
    };
    let canonical_bundle_dir = bundle_dir.canonicalize()?;
    let canonical_bundle_out = canonical_bundle_dir.join(
        bundle_out
            .file_name()
            .context("strategy bundle output has no file name")?,
    );
    staged.bundle_out = Some(canonical_bundle_out.clone());
    let staging_dir = canonical_bundle_dir.join(format!(
        ".alpha-bundle-{}-{}-{}-staging",
        std::process::id(),
        &bundle.bundle_hash[..16],
        NEXT_BUNDLE_STAGE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&staging_dir)?;
    staged.staging_dir = Some(staging_dir.clone());
    let staged_bundle = staging_dir.join("bundle.json");
    std::fs::write(&staged_bundle, serde_json::to_vec_pretty(bundle)?)?;
    std::fs::File::open(&staged_bundle)?.sync_all()?;
    staged.staged_bundle = Some(staged_bundle);
    if canonical_bundle_out.exists() {
        let existing: StrategyBundle =
            serde_json::from_slice(&std::fs::read(&canonical_bundle_out)?)?;
        if &existing != bundle {
            bail!("existing strategy bundle has different content");
        }
    }
    if let alpha_domain::StrategyBundleArtifact::Onnx { model } = &bundle.artifact {
        let source = verified_model_path(
            model,
            model_root.context("ONNX promotion requires --model-root")?,
        )?;
        let relative = Path::new(&model.artifact.uri);
        let target = canonical_bundle_dir.join(relative);
        if target == canonical_bundle_out {
            bail!("strategy bundle JSON and ONNX artifact cannot share a path");
        }
        let target_parent = target
            .parent()
            .context("ONNX bundle target has no parent directory")?;
        staged
            .created_dirs
            .extend(create_dir_all_tracked(target_parent)?);
        let canonical_target_parent = target_parent.canonicalize()?;
        if !canonical_target_parent.starts_with(&canonical_bundle_dir) {
            bail!("ONNX bundle target escapes the bundle directory");
        }
        if target.exists() {
            verify_onnx_file(&target, model, "existing ONNX bundle artifact")?;
        } else {
            let staged_model = staging_dir.join("model.onnx");
            std::fs::copy(&source, &staged_model)?;
            std::fs::File::open(&staged_model)?.sync_all()?;
            verify_onnx_file(&staged_model, model, "staged ONNX bundle artifact")?;
            staged.model_target = Some(target);
            staged.staged_model = Some(staged_model);
            staged.model = Some(model.clone());
        }
    }
    Ok(staged)
}

fn verify_onnx_file(
    path: &Path,
    model: &OnnxModelCandidate,
    description: &str,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(path)?;
    let checksum = hex::encode(Sha256::digest(&bytes));
    if bytes.len() as u64 != model.byte_len
        || model.artifact.checksum.as_deref() != Some(checksum.as_str())
    {
        bail!("{description} has different content");
    }
    Ok(())
}

fn create_dir_all_tracked(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut missing = Vec::new();
    let mut current = path;
    while !current.exists() {
        missing.push(current.to_path_buf());
        current = current
            .parent()
            .context("directory path has no existing ancestor")?;
    }
    missing.reverse();
    let mut created = Vec::new();
    for directory in missing {
        match std::fs::create_dir(&directory) {
            Ok(()) => created.push(directory),
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists && directory.is_dir() => {}
            Err(error) => {
                for created_directory in created.iter().rev() {
                    let _ = std::fs::remove_dir(created_directory);
                }
                return Err(error.into());
            }
        }
    }
    Ok(created)
}

fn ensure_exact_promotion_replay(
    existing: &PromotionRecord,
    existing_bundle: &StrategyBundle,
    requested: &PromotionRecord,
    requested_bundle: &StrategyBundle,
) -> anyhow::Result<()> {
    if existing != requested || existing_bundle != requested_bundle {
        bail!(
            "promotion_id {} is already bound to a different mission, candidate, evaluation, or bundle",
            requested.promotion_id
        );
    }
    Ok(())
}

pub fn sign_deployment(args: SignDeploymentArgs) -> anyhow::Result<()> {
    let envelope: DeploymentEnvelope = serde_json::from_slice(
        &std::fs::read(&args.envelope)
            .with_context(|| format!("failed to read envelope {}", args.envelope.display()))?,
    )?;
    let mut store = AlphaStore::open(args.db)?;
    let (_, bundle) = store.validate_deployment_binding(&envelope)?;
    data_mission::require_promotable_research_dataset(&store, bundle.dataset_manifest_id.as_str())?;
    enforce_deployment_approvals(&store, &envelope, Utc::now())?;
    let key_hex = std::fs::read_to_string(&args.signing_key)
        .with_context(|| format!("failed to read signing key {}", args.signing_key.display()))?;
    let key_bytes = hex::decode(key_hex.trim()).context("signing key must be hex encoded")?;
    let key_array: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("signing key must contain exactly 32 bytes"))?;
    let signing_key = SigningKey::from_bytes(&key_array);
    let verifying_key_hex = hex::encode(signing_key.verifying_key().to_bytes());
    let signed = sign_envelope(envelope, args.key_id, &signing_key)?;
    store.store_deployment(&signed, Utc::now())?;
    data_mission::write_json_atomic(&args.output, &signed)?;
    print_json(&serde_json::json!({
        "deployment_id": signed.envelope.deployment_id,
        "signed_envelope_path": args.output,
        "key_id": signed.key_id,
        "verifying_key_hex": verifying_key_hex,
    }))
}

pub fn print_deployment_scope(args: EnvelopeArgs) -> anyhow::Result<()> {
    let envelope: DeploymentEnvelope = read_record(&args.envelope)?;
    print_json(&serde_json::json!({
        "scope_hash": deployment_scope_hash(&envelope)?,
        "asset_revision_id": envelope.asset_revision_id,
    }))
}

pub fn ingest_feedback(args: FeedbackRecordArgs) -> anyhow::Result<()> {
    let signed: SignedRuntimeAttributionEvent = read_record(&args.record)?;
    let trusted_keys = read_trusted_attribution_keys(&args.trusted_keys)?;
    let event = verify_runtime_attribution_event(&signed, &trusted_keys)
        .context("runtime feedback signature verification failed")?;
    let mut store = AlphaStore::open(args.db)?;
    let inserted = store.ingest_runtime_attribution(event.clone())?;
    let stored = store.get_runtime_attribution(&event.event_id)?;
    print_json(&serde_json::json!({
        "inserted": inserted,
        "event": stored,
    }))
}

pub fn ingest_feedback_log(args: FeedbackLogArgs) -> anyhow::Result<()> {
    let contents = std::fs::read_to_string(&args.log)
        .with_context(|| format!("failed to read feedback log {}", args.log.display()))?;
    let trusted_keys = read_trusted_attribution_keys(&args.trusted_keys)?;
    let events = parse_runtime_attribution_log(&contents, &trusted_keys)?;
    let records = events.len();
    let mut store = AlphaStore::open(args.db)?;
    let inserted = store.ingest_runtime_attributions(events)?;
    print_json(&serde_json::json!({
        "records": records,
        "inserted": inserted,
        "duplicates": records - inserted,
    }))
}

fn parse_runtime_attribution_log(
    contents: &str,
    trusted_keys: &BTreeMap<String, VerifyingKey>,
) -> anyhow::Result<Vec<VerifiedRuntimeAttributionEvent>> {
    let mut events = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let signed: SignedRuntimeAttributionEvent = serde_json::from_str(line)
            .with_context(|| format!("feedback log line {} is invalid JSON", index + 1))?;
        let event = verify_runtime_attribution_event(&signed, trusted_keys)
            .with_context(|| format!("feedback log line {} failed verification", index + 1))?;
        events.push(event);
    }
    if events.is_empty() {
        bail!("feedback log contains no attribution events");
    }
    Ok(events)
}

pub fn propose_policy(args: JsonRecordArgs) -> anyhow::Result<()> {
    let revision: SearchPolicyRevision = read_record(&args.record)?;
    let mut store = AlphaStore::open(args.db)?;
    print_json(&store.put_search_policy_revision(revision)?)
}

pub fn record_approval(args: JsonRecordArgs) -> anyhow::Result<()> {
    let approval: ApprovalRecord = read_record(&args.record)?;
    let mut store = AlphaStore::open(args.db)?;
    store.record_approval(&approval)?;
    print_json(&approval)
}

pub fn revoke_approval(args: RevokeApprovalArgs) -> anyhow::Result<()> {
    let mut store = AlphaStore::open(args.db)?;
    let approval = store.revoke_approval(
        &args.approval_id,
        &args.revoked_by,
        &args.reason,
        Utc::now(),
    )?;
    print_json(&approval)
}

fn enforce_deployment_approvals(
    store: &AlphaStore,
    envelope: &DeploymentEnvelope,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    if envelope.approval_class == ApprovalClass::SameClassAutoLiveSmall {
        bail!("same-class automatic live-small approval is disabled");
    }
    let scope_hash = deployment_scope_hash(envelope)?;
    let mut approved = false;
    for approval_id in &envelope.approval_signatures {
        let approval = store
            .get_approval(approval_id)
            .with_context(|| format!("referenced approval {approval_id} is not persisted"))?;
        if !approval.is_active_at(now) {
            bail!("referenced approval {approval_id} is expired, revoked, or not yet valid");
        }
        let scope_matches = approval
            .payload
            .get("scope_hash")
            .and_then(serde_json::Value::as_str)
            == Some(scope_hash.as_str());
        let class_matches = match envelope.approval_class {
            ApprovalClass::Paper => approval.approval_class == "paper",
            ApprovalClass::Shadow => approval.approval_class == "shadow",
            ApprovalClass::HumanApprovedLiveSmall => approval.approval_class == "human_live_small",
            ApprovalClass::SameClassAutoLiveSmall => false,
        };
        let subject_matches = approval.subject_id == envelope.promotion_id;
        approved |= scope_matches && class_matches && subject_matches;
    }
    if !approved {
        bail!("deployment has no active approval matching class, subject, and scope");
    }
    Ok(())
}

fn read_record<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> anyhow::Result<T> {
    serde_json::from_slice(
        &std::fs::read(path)
            .with_context(|| format!("failed to read record {}", path.display()))?,
    )
    .with_context(|| format!("record {} is invalid JSON", path.display()))
}

fn read_trusted_attribution_keys(
    path: &std::path::Path,
) -> anyhow::Result<BTreeMap<String, VerifyingKey>> {
    let encoded: BTreeMap<String, String> = read_record(path)?;
    encoded
        .into_iter()
        .map(|(key_id, value)| {
            let bytes = hex::decode(value)
                .with_context(|| format!("runtime feedback key {key_id} is not hex"))?;
            let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
                anyhow::anyhow!("runtime feedback key {key_id} must contain exactly 32 bytes")
            })?;
            let key = VerifyingKey::from_bytes(&bytes)
                .with_context(|| format!("runtime feedback key {key_id} is invalid"))?;
            Ok((key_id, key))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::{
        sign_runtime_attribution_event, AllowedIntentType, EvaluationCostsV1,
        EvaluationLabelSpecV1, EvaluationProtocolV1, EvaluationWalkForwardV1,
        MissionCompletionPolicy, ResearchMission, RuntimeAttributionEvent, SearchBudget,
        TensorElementType, TensorSpec, ValidatorMode, LOB_ONNX_PREPROCESSING_VERSION,
    };
    use alpha_engine::evaluation::ResearchRow;
    use alpha_store::{StoredCandidate, StoredEvaluation};
    use chrono::Duration;
    use std::sync::{Arc, Barrier};

    fn evaluation_protocol() -> EvaluationProtocolV1 {
        EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 1,
                validation_rows: 32,
                fold_count: 2,
                purge_rows: 1,
                embargo_rows: 0,
                sealed_holdout_rows: 1,
            },
            EvaluationCostsV1 {
                fee_bps: 0.0,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.0,
                slippage_bps: 0.0,
                cross_spread: false,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            },
            EvaluationLabelSpecV1 {
                horizon_buckets: 1,
                observation_frequency_millis: 1_000,
            },
        )
        .unwrap()
    }

    fn live_small_envelope() -> DeploymentEnvelope {
        let now = Utc::now();
        DeploymentEnvelope {
            deployment_id: "deployment-live-small".to_string(),
            asset_revision_id: "factor-1@2".to_string(),
            promotion_id: "promotion-1".to_string(),
            promotion_manifest_hash: "a".repeat(64),
            bundle_id: "bundle-1".to_string(),
            bundle_hash: "b".repeat(64),
            runtime_config_hash: "c".repeat(64),
            risk_policy_hash: "d".repeat(64),
            account_id: "account-1".to_string(),
            venue: "binance".to_string(),
            instruments: vec!["BTCUSDT".to_string()],
            allowed_intent_types: vec![AllowedIntentType::StartLiveSmall],
            max_notional: 100.0,
            max_symbol_exposure: 50.0,
            max_order_size: 10.0,
            max_slippage_bps: 2.0,
            valid_from: now - Duration::minutes(1),
            expires_at: now + Duration::minutes(5),
            nonce: "nonce-live-small".to_string(),
            approval_class: ApprovalClass::HumanApprovedLiveSmall,
            approval_signatures: vec!["approval-1".to_string()],
            payload_hash: String::new(),
        }
    }

    fn promotion_fixture() -> (PromotionRecord, StrategyBundle) {
        let now = Utc::now();
        let mut bundle: StrategyBundle = serde_json::from_value(serde_json::json!({
            "bundle_id": "bundle-1",
            "candidate_id": "candidate-1",
            "candidate_content_hash": "1".repeat(64),
            "dataset_manifest_id": "dataset-1",
            "evaluator_version": SEALED_HOLDOUT_EVALUATOR_VERSION,
            "evaluation_protocol_hash": "5".repeat(64),
            "evaluator_config_hash": "2".repeat(64),
            "evaluation_metrics_hash": "3".repeat(64),
            "sealed_evaluation_hash": "4".repeat(64),
            "artifact": {
                "Formula": {"ast": {"Terminal": {"Field": "mid_price"}}}
            },
            "bundle_hash": "",
            "created_at": now,
        }))
        .unwrap();
        bundle.bundle_hash = bundle.calculated_hash().unwrap();
        let promotion = PromotionRecord {
            promotion_id: "promotion-1".to_string(),
            mission_id: "mission-1".to_string(),
            candidate_id: bundle.candidate_id.clone(),
            candidate_content_hash: bundle.candidate_content_hash.clone(),
            dataset_manifest_id: bundle.dataset_manifest_id.clone(),
            evaluator_version: bundle.evaluator_version.clone(),
            evaluation_protocol_hash: bundle.evaluation_protocol_hash.clone(),
            evaluator_config_hash: bundle.evaluator_config_hash.clone(),
            evaluation_metrics_hash: bundle.evaluation_metrics_hash.clone(),
            sealed_evaluation_id: sealed_evaluation_revision_id(
                "candidate-1",
                SEALED_HOLDOUT_EVALUATOR_VERSION,
            ),
            sealed_evaluation_hash: bundle.sealed_evaluation_hash.clone(),
            bundle_id: bundle.bundle_id.clone(),
            bundle_hash: bundle.bundle_hash.clone(),
            created_at: now,
        };
        (promotion, bundle)
    }

    fn research_mission() -> ResearchMission {
        let now = Utc::now();
        ResearchMission {
            mission_id: "mission-1".to_string(),
            objective: "validate ONNX governance".to_string(),
            hypothesis_scope: "LOB flow".to_string(),
            mutable_scope: vec!["model".to_string()],
            dataset_manifest_id: serde_json::from_value(serde_json::json!("dataset-1")).unwrap(),
            baseline_artifact_id: None,
            validation_mode: ValidatorMode::MissionValidator,
            validator_spec: serde_json::json!({}),
            search_budget: SearchBudget {
                max_candidates: 1,
                max_expansions: 1,
                max_tokens: 0,
                max_seconds: 30,
            },
            completion_policy: MissionCompletionPolicy::default(),
            prompt_snapshot_id: None,
            search_policy_snapshot_id: "policy-1".to_string(),
            status: MissionStatus::Pending,
            terminal_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn onnx_candidate() -> CandidateArtifact {
        CandidateArtifact::OnnxModel(OnnxModelCandidate {
            artifact: serde_json::from_value(serde_json::json!({
                "uri": "model.onnx",
                "content_type": "application/onnx",
                "checksum": "a".repeat(64),
            }))
            .unwrap(),
            byte_len: 1,
            opset: 17,
            preprocessing_version: LOB_ONNX_PREPROCESSING_VERSION.to_string(),
            inputs: vec![TensorSpec {
                name: "lob".to_string(),
                element_type: TensorElementType::Float32,
                dimensions: vec![Some(1), Some(4), Some(1), Some(1)],
            }],
            output: TensorSpec {
                name: "signal".to_string(),
                element_type: TensorElementType::Float32,
                dimensions: vec![Some(1), Some(1)],
            },
        })
    }

    fn conflicting_onnx_bundles_for_shared_model(
        model_bytes: &[u8],
    ) -> (StrategyBundle, StrategyBundle) {
        let mut model = match onnx_candidate() {
            CandidateArtifact::OnnxModel(model) => model,
            _ => unreachable!("ONNX fixture must contain an ONNX model"),
        };
        model.byte_len = model_bytes.len() as u64;
        model.artifact.checksum = Some(hex::encode(Sha256::digest(model_bytes)));

        let (_, mut first) = promotion_fixture();
        first.artifact = CandidateArtifact::OnnxModel(model)
            .to_governed_strategy_bundle_artifact()
            .unwrap();
        first.bundle_hash = first.calculated_hash().unwrap();
        let mut second = first.clone();
        second.candidate_content_hash = "f".repeat(64);
        second.bundle_hash = second.calculated_hash().unwrap();
        (first, second)
    }

    #[test]
    fn promotion_idempotency_requires_an_exact_replay_binding() {
        let (promotion, bundle) = promotion_fixture();
        ensure_exact_promotion_replay(&promotion, &bundle, &promotion, &bundle).unwrap();

        let mut different_mission = promotion.clone();
        different_mission.mission_id = "mission-2".to_string();
        assert!(
            ensure_exact_promotion_replay(&promotion, &bundle, &different_mission, &bundle)
                .is_err()
        );

        let mut different_candidate = promotion.clone();
        different_candidate.candidate_id = "candidate-2".to_string();
        assert!(
            ensure_exact_promotion_replay(&promotion, &bundle, &different_candidate, &bundle)
                .is_err()
        );

        let mut different_bundle = bundle.clone();
        different_bundle.bundle_hash = "f".repeat(64);
        assert!(
            ensure_exact_promotion_replay(&promotion, &bundle, &promotion, &different_bundle)
                .is_err()
        );
    }

    #[test]
    fn bundle_staging_cleans_up_on_persistence_failure_without_partial_output() {
        let (promotion, bundle) = promotion_fixture();
        let directory = std::env::temp_dir().join(format!(
            "alpha-bundle-stage-failure-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let bundle_out = directory.join("bundle.json");
        let staged = stage_bundle(&bundle, Some(&bundle_out), None).unwrap();
        assert!(!bundle_out.exists());
        let mut store = AlphaStore::open_in_memory().unwrap();
        assert!(store.promote_candidate(&bundle, &promotion).is_err());

        drop(staged);

        assert!(!bundle_out.exists());
        assert!(!directory.exists());
    }

    #[test]
    fn conflicting_bundle_output_is_rejected_before_any_overwrite() {
        let (_, bundle) = promotion_fixture();
        let directory = std::env::temp_dir().join(format!(
            "alpha-bundle-conflict-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let bundle_out = directory.join("bundle.json");
        let sentinel = b"pre-existing different content";
        std::fs::write(&bundle_out, sentinel).unwrap();

        assert!(stage_bundle(&bundle, Some(&bundle_out), None).is_err());
        assert_eq!(std::fs::read(&bundle_out).unwrap(), sentinel);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn concurrent_bundle_publish_is_atomic_and_never_clobbers() {
        let (_, first) = promotion_fixture();
        let mut second = first.clone();
        second.candidate_content_hash = "f".repeat(64);
        second.bundle_hash = second.calculated_hash().unwrap();
        let directory = std::env::temp_dir().join(format!(
            "alpha-bundle-concurrent-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let bundle_out = directory.join("bundle.json");
        let first_staged = stage_bundle(&first, Some(&bundle_out), None).unwrap();
        let second_staged = stage_bundle(&second, Some(&bundle_out), None).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let first_barrier = barrier.clone();
        let first_publish = std::thread::spawn(move || {
            first_barrier.wait();
            first_staged.publish()
        });
        let second_barrier = barrier.clone();
        let second_publish = std::thread::spawn(move || {
            second_barrier.wait();
            second_staged.publish()
        });
        barrier.wait();
        let results = [
            first_publish.join().unwrap(),
            second_publish.join().unwrap(),
        ];

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let published: StrategyBundle =
            serde_json::from_slice(&std::fs::read(&bundle_out).unwrap()).unwrap();
        assert!(published == first || published == second);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn concurrent_onnx_bundle_publish_preserves_the_winners_shared_model() {
        let model_bytes = b"shared-onnx-model";
        let (first, second) = conflicting_onnx_bundles_for_shared_model(model_bytes);

        let directory = std::env::temp_dir().join(format!(
            "alpha-onnx-bundle-concurrent-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let model_root = directory.join("source");
        let output = directory.join("output");
        std::fs::create_dir_all(&model_root).unwrap();
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(model_root.join("model.onnx"), model_bytes).unwrap();
        let bundle_out = output.join("bundle.json");
        let first_staged = stage_bundle(&first, Some(&bundle_out), Some(&model_root)).unwrap();
        let second_staged = stage_bundle(&second, Some(&bundle_out), Some(&model_root)).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let first_barrier = barrier.clone();
        let first_publish = std::thread::spawn(move || {
            first_barrier.wait();
            first_staged.publish()
        });
        let second_barrier = barrier.clone();
        let second_publish = std::thread::spawn(move || {
            second_barrier.wait();
            second_staged.publish()
        });
        barrier.wait();
        let results = [
            first_publish.join().unwrap(),
            second_publish.join().unwrap(),
        ];

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let published: StrategyBundle =
            serde_json::from_slice(&std::fs::read(&bundle_out).unwrap()).unwrap();
        assert!(published == first || published == second);
        assert_eq!(
            std::fs::read(output.join("model.onnx")).unwrap(),
            model_bytes.to_vec()
        );
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 2);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bundle_conflict_keeps_a_shared_model_referenced_by_the_winner() {
        let model_bytes = b"shared-onnx-model";
        let (first, second) = conflicting_onnx_bundles_for_shared_model(model_bytes);
        let directory = std::env::temp_dir().join(format!(
            "alpha-onnx-bundle-shared-model-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let model_root = directory.join("source");
        let output = directory.join("output");
        std::fs::create_dir_all(&model_root).unwrap();
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(model_root.join("model.onnx"), model_bytes).unwrap();
        let bundle_out = output.join("bundle.json");
        let first_staged = stage_bundle(&first, Some(&bundle_out), Some(&model_root)).unwrap();

        std::fs::write(&bundle_out, serde_json::to_vec_pretty(&second).unwrap()).unwrap();
        assert!(first_staged.publish().is_err());

        let published: StrategyBundle =
            serde_json::from_slice(&std::fs::read(&bundle_out).unwrap()).unwrap();
        assert_eq!(published, second);
        assert_eq!(
            std::fs::read(output.join("model.onnx")).unwrap(),
            model_bytes.to_vec()
        );
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 2);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn sealed_and_promotion_entrypoint_rejects_non_live_formula() {
        let ast = hft_factor_dsl::FactorAst::call(
            hft_factor_dsl::FactorOperator::Mean,
            vec![
                hft_factor_dsl::FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field(
                    "mid_price".to_string(),
                )),
                hft_factor_dsl::FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Constant(
                    "20".to_string(),
                )),
            ],
        )
        .unwrap();

        assert_eq!(
            sealed_evaluator_version(&CandidateArtifact::Formula(ast))
                .unwrap_err()
                .to_string(),
            "unsupported live operator: mean"
        );
    }

    #[test]
    fn sealed_revision_id_is_bound_to_evaluator_version() {
        assert_eq!(
            sealed_evaluation_revision_id("candidate-1", SEALED_HOLDOUT_EVALUATOR_VERSION),
            "sealed-evaluation:sealed-holdout-v4:candidate-1"
        );
    }

    #[test]
    fn canonical_onnx_walk_forward_candidate_reaches_governance() {
        let mission = research_mission();
        let start = Utc::now();
        let rows = (0..64)
            .map(|index| ResearchRow {
                available_time: start + Duration::seconds(index),
                signal: if index % 2 == 0 { 1.0 } else { -1.0 },
                features: BTreeMap::new(),
                label: if index % 2 == 0 { 0.01 } else { -0.01 },
                fee_bps: 0.0,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.0,
            })
            .collect::<Vec<_>>();
        let signals = rows.iter().map(|row| row.signal).collect::<Vec<_>>();
        let evaluation = FormulaEvaluator::for_mission(&mission)
            .unwrap()
            .evaluate_onnx_signals(
                &rows,
                &signals,
                [0..32, 32..64],
                false,
                &evaluation_protocol(),
            )
            .unwrap();
        assert!(evaluation.passed);
        let created_at = Utc::now();
        let dataset_manifest_id = mission.dataset_manifest_id.as_str().to_string();
        let lineage = MissionLineage {
            mission,
            iterations: vec![ResearchIteration {
                iteration_id: "iteration-1".to_string(),
                mission_id: "mission-1".to_string(),
                parent_candidate_ids: vec![],
                engine: EngineKind::ManualSeed,
                hypothesis: "ONNX model predicts next return".to_string(),
                candidate_artifact_id: Some("candidate-1".to_string()),
                evaluation_artifact_id: Some("evaluation-1".to_string()),
                budget_usage: SearchBudgetUsage {
                    candidates: 1,
                    expansions: 1,
                    tokens: 0,
                    elapsed_ms: 1,
                },
                verdict: IterationVerdict::Keep,
                failure_class: None,
                failure_explanation: None,
                created_at,
            }],
            candidates: vec![StoredCandidate {
                candidate_id: "candidate-1".to_string(),
                mission_id: "mission-1".to_string(),
                iteration_id: "iteration-1".to_string(),
                artifact: onnx_candidate(),
                content_hash: "a".repeat(64),
                created_at,
            }],
            evaluations: vec![StoredEvaluation {
                record: EvaluationRecord {
                    evaluation_id: "evaluation-1".to_string(),
                    mission_id: "mission-1".to_string(),
                    candidate_id: "candidate-1".to_string(),
                    dataset_manifest_id,
                    evaluation_protocol_hash: evaluation_protocol().content_hash().unwrap(),
                    payload: serde_json::to_value(evaluation).unwrap(),
                    created_at,
                },
                content_hash: "b".repeat(64),
            }],
        };

        assert_eq!(
            validated_walk_forward_candidates_in_lineage(&lineage).unwrap(),
            vec!["candidate-1".to_string()]
        );
        assert_eq!(
            selected_walk_forward_candidate_in_lineage(&lineage)
                .unwrap()
                .unwrap()
                .0,
            "candidate-1"
        );

        let mut ambiguous = lineage.clone();
        let mut second_iteration = ambiguous.iterations[0].clone();
        second_iteration.iteration_id = "iteration-2".to_string();
        second_iteration.candidate_artifact_id = Some("candidate-2".to_string());
        second_iteration.evaluation_artifact_id = Some("evaluation-2".to_string());
        ambiguous.iterations.push(second_iteration);
        let mut second_candidate = ambiguous.candidates[0].clone();
        second_candidate.candidate_id = "candidate-2".to_string();
        second_candidate.iteration_id = "iteration-2".to_string();
        ambiguous.candidates.push(second_candidate);
        let mut second_evaluation = ambiguous.evaluations[0].clone();
        second_evaluation.record.evaluation_id = "evaluation-2".to_string();
        second_evaluation.record.candidate_id = "candidate-2".to_string();
        ambiguous.evaluations.push(second_evaluation);

        assert!(selected_walk_forward_candidate_in_lineage(&ambiguous)
            .unwrap_err()
            .to_string()
            .contains("exactly one canonical walk-forward candidate"));

        let mut wrong_dataset = lineage.clone();
        wrong_dataset.evaluations[0].record.dataset_manifest_id = "other-dataset".to_string();
        assert!(validated_walk_forward_candidates_in_lineage(&wrong_dataset)
            .unwrap()
            .is_empty());
        let mut wrong_protocol = lineage.clone();
        wrong_protocol.evaluations[0]
            .record
            .evaluation_protocol_hash = "f".repeat(64);
        assert!(
            validated_walk_forward_candidates_in_lineage(&wrong_protocol)
                .unwrap()
                .is_empty()
        );

        let mut legacy_formula_lineage = lineage.clone();
        legacy_formula_lineage.candidates[0].artifact = CandidateArtifact::Formula(
            hft_factor_dsl::FactorAst::call(
                hft_factor_dsl::FactorOperator::Rank,
                vec![hft_factor_dsl::FactorAst::Terminal(
                    hft_factor_dsl::FactorTerminal::Field("mid_price".to_string()),
                )],
            )
            .unwrap(),
        );
        let mut legacy_evaluation: CandidateEvaluation =
            serde_json::from_value(legacy_formula_lineage.evaluations[0].record.payload.clone())
                .unwrap();
        legacy_evaluation.evaluator_version = WALK_FORWARD_EVALUATOR_VERSION.to_string();
        assert!(legacy_evaluation.validate().is_ok());
        legacy_formula_lineage.evaluations[0].record.payload =
            serde_json::to_value(legacy_evaluation).unwrap();

        assert!(
            validated_walk_forward_candidates_in_lineage(&legacy_formula_lineage)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn live_small_requires_a_persisted_human_approval_for_the_same_scope() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let mut envelope = live_small_envelope();
        let now = Utc::now();
        let mut automatic = envelope.clone();
        automatic.approval_class = ApprovalClass::SameClassAutoLiveSmall;
        assert!(enforce_deployment_approvals(&store, &automatic, now).is_err());
        assert!(
            enforce_deployment_approvals(&store, &envelope, now + Duration::seconds(1)).is_err()
        );
        store
            .record_approval(&ApprovalRecord {
                approval_id: "approval-1".to_string(),
                approval_class: "human_live_small".to_string(),
                subject_id: "earlier-factor@1".to_string(),
                payload: serde_json::json!({
                    "scope_hash": deployment_scope_hash(&envelope).unwrap(),
                }),
                signer_id: Some("risk-officer-1".to_string()),
                valid_from: Some(now),
                expires_at: Some(now + Duration::minutes(10)),
                revoked_at: None,
                revoked_by: None,
                revocation_reason: None,
                created_at: now,
            })
            .unwrap();
        assert!(enforce_deployment_approvals(&store, &envelope, now).is_err());
        store
            .record_approval(&ApprovalRecord {
                approval_id: "approval-2".to_string(),
                approval_class: "human_live_small".to_string(),
                subject_id: envelope.promotion_id.clone(),
                payload: serde_json::json!({
                    "scope_hash": deployment_scope_hash(&envelope).unwrap(),
                }),
                signer_id: Some("risk-officer-2".to_string()),
                valid_from: Some(now),
                expires_at: Some(now + Duration::minutes(10)),
                revoked_at: None,
                revoked_by: None,
                revocation_reason: None,
                created_at: now,
            })
            .unwrap();
        envelope.approval_signatures = vec!["approval-2".to_string()];
        assert!(enforce_deployment_approvals(&store, &envelope, now).is_ok());

        store
            .revoke_approval(
                "approval-2",
                "risk-officer-2",
                "risk posture changed",
                now + Duration::seconds(1),
            )
            .unwrap();
        assert!(
            enforce_deployment_approvals(&store, &envelope, now + Duration::seconds(1)).is_err()
        );
    }

    #[test]
    fn deployment_scope_hash_normalizes_instrument_and_intent_sets() {
        let mut first = live_small_envelope();
        first.instruments = vec!["ETHUSDT".to_string(), "BTCUSDT".to_string()];
        first.allowed_intent_types = vec![
            AllowedIntentType::StartLiveSmall,
            AllowedIntentType::LoadFactor,
        ];
        let mut second = first.clone();
        second.instruments = vec![
            "BTCUSDT".to_string(),
            "ETHUSDT".to_string(),
            "BTCUSDT".to_string(),
        ];
        second.allowed_intent_types = vec![
            AllowedIntentType::LoadFactor,
            AllowedIntentType::StartLiveSmall,
            AllowedIntentType::LoadFactor,
        ];

        assert_eq!(
            deployment_scope_hash(&first).unwrap(),
            deployment_scope_hash(&second).unwrap()
        );
    }

    #[test]
    fn feedback_log_parser_validates_every_line_before_ingestion() {
        let key = SigningKey::from_bytes(&[9_u8; 32]);
        let trusted = BTreeMap::from([("feedback-1".to_string(), key.verifying_key())]);
        let event = RuntimeAttributionEvent {
            event_id: "activation-1".to_string(),
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode: alpha_domain::AttributionMode::Paper,
            outcome: alpha_domain::AttributionOutcome::Activated,
            kind: alpha_domain::AttributionKind::Activation,
            strategy_id: None,
            order_id: None,
            account_id: None,
            venue: None,
            symbol: None,
            metrics: std::collections::BTreeMap::new(),
            reason: None,
            observed_at: Utc::now(),
        };
        let signed = sign_runtime_attribution_event(event.clone(), "feedback-1", &key).unwrap();
        let valid = serde_json::to_string(&signed).unwrap();
        assert_eq!(
            parse_runtime_attribution_log(&valid, &trusted)
                .unwrap()
                .into_iter()
                .map(VerifiedRuntimeAttributionEvent::into_event)
                .collect::<Vec<_>>(),
            vec![event.clone()]
        );
        assert!(
            parse_runtime_attribution_log(&format!("{valid}\n{{bad"), &trusted)
                .unwrap_err()
                .to_string()
                .contains("line 2")
        );
        assert!(
            parse_runtime_attribution_log(&serde_json::to_string(&event).unwrap(), &trusted)
                .unwrap_err()
                .to_string()
                .contains("line 1")
        );

        let mut tampered = signed;
        tampered.event.asset_revision_id = "candidate-forged".to_string();
        assert!(parse_runtime_attribution_log(
            &serde_json::to_string(&tampered).unwrap(),
            &trusted
        )
        .unwrap_err()
        .to_string()
        .contains("failed verification"));
    }

    #[test]
    fn candidate_show_returns_typed_metrics_with_folds_and_failure_reasons() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("alpha.duckdb");
        let now = Utc::now();
        let candidate = onnx_candidate();
        let evaluation = EvaluationRecord {
            evaluation_id: "evaluation-1".to_string(),
            mission_id: "mission-1".to_string(),
            candidate_id: "candidate-1".to_string(),
            dataset_manifest_id: "dataset-1".to_string(),
            evaluation_protocol_hash: evaluation_protocol().content_hash().unwrap(),
            payload: serde_json::json!({
                "passed": false,
                "score": 0.0,
                "failure_reasons": ["time_series_ic 0.010 below floor 0.020"],
                "evaluator_version": WALK_FORWARD_EVALUATOR_VERSION,
                "evaluator_config": {},
                "metrics": {
                    "predictive": {
                        "row_count": 2,
                        "time_series_ic": 0.01,
                        "time_series_rank_ic": 0.02,
                        "time_series_icir": 0.5,
                        "time_series_rank_icir": 0.6,
                        "positive_ic_ratio": 0.5,
                        "folds": [
                            {"fold_index": 0, "row_count": 1, "time_series_ic": 0.01, "time_series_rank_ic": 0.02},
                            {"fold_index": 1, "row_count": 1, "time_series_ic": 0.01, "time_series_rank_ic": 0.02}
                        ]
                    },
                    "row_count": 2,
                    "trade_count": 2,
                    "total_turnover": 4.0,
                    "mean_net_return": -0.001,
                    "cumulative_net_return": -0.002,
                    "max_drawdown": 0.003,
                    "net_sharpe": -1.5,
                    "raw_score": -0.5,
                    "adjusted_score": -0.25,
                    "folds": [
                        {"fold_index": 0, "row_count": 1, "trade_count": 1, "total_turnover": 2.0, "mean_net_return": -0.001, "cumulative_net_return": -0.001, "max_drawdown": 0.001, "net_sharpe": -1.0, "raw_score": -0.5},
                        {"fold_index": 1, "row_count": 1, "trade_count": 1, "total_turnover": 2.0, "mean_net_return": -0.001, "cumulative_net_return": -0.001, "max_drawdown": 0.003, "net_sharpe": -2.0, "raw_score": -0.5}
                    ]
                }
            }),
            created_at: now,
        };
        let iteration = ResearchIteration {
            iteration_id: "iteration-1".to_string(),
            mission_id: "mission-1".to_string(),
            parent_candidate_ids: vec![],
            engine: EngineKind::ManualSeed,
            hypothesis: "candidate show fixture".to_string(),
            candidate_artifact_id: Some("candidate-1".to_string()),
            evaluation_artifact_id: Some("evaluation-1".to_string()),
            budget_usage: SearchBudgetUsage {
                candidates: 1,
                ..SearchBudgetUsage::default()
            },
            verdict: IterationVerdict::Discard,
            failure_class: None,
            failure_explanation: None,
            created_at: now,
        };
        {
            let mut store = AlphaStore::open(&db).unwrap();
            store.create_mission(&research_mission()).unwrap();
            store
                .append_iteration(
                    &iteration,
                    Some(("candidate-1", &candidate)),
                    Some(&evaluation),
                )
                .unwrap();
        }

        let report = candidate_show_report(&CandidateShowArgs {
            db: db.clone(),
            mission_id: "mission-1".to_string(),
            candidate_id: "candidate-1".to_string(),
        })
        .unwrap();

        assert_eq!(report["mission_id"], "mission-1");
        assert_eq!(report["candidate"]["candidate_id"], "candidate-1");
        let evaluations = report["evaluations"].as_array().unwrap();
        assert_eq!(evaluations.len(), 1);
        let shown = &evaluations[0];
        assert_eq!(shown["evaluation_id"], "evaluation-1");
        assert_eq!(shown["content_hash"].as_str().unwrap().len(), 64);
        let shown_evaluation = &shown["evaluation"];
        assert_eq!(shown_evaluation["passed"], false);
        assert_eq!(
            shown_evaluation["failure_reasons"],
            serde_json::json!(["time_series_ic 0.010 below floor 0.020"])
        );
        assert_eq!(shown_evaluation["metrics"]["net_sharpe"], -1.5);
        assert_eq!(
            shown_evaluation["metrics"]["predictive"]["positive_ic_ratio"],
            0.5
        );
        assert_eq!(
            shown_evaluation["metrics"]["folds"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            shown_evaluation["metrics"]["folds"][1]["net_sharpe"],
            -2.0
        );

        assert!(candidate_show_report(&CandidateShowArgs {
            db: db.clone(),
            mission_id: "mission-1".to_string(),
            candidate_id: "candidate-unknown".to_string(),
        })
        .unwrap_err()
        .to_string()
        .contains("does not belong to mission"));
        assert!(candidate_show_report(&CandidateShowArgs {
            db,
            mission_id: "mission-unknown".to_string(),
            candidate_id: "candidate-1".to_string(),
        })
        .is_err());
    }
}
