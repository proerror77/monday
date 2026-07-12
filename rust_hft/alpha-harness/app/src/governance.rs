use crate::{
    cli::{
        print_json, EnvelopeArgs, EvaluateArgs, FeedbackLogArgs, FeedbackRecordArgs,
        JsonRecordArgs, MissionStatusArgs, PromoteArgs, RevokeApprovalArgs, SignDeploymentArgs,
    },
    data_mission,
};
use alpha_domain::{
    canonical_json_hash, deployment_scope_hash, sign_envelope, verify_runtime_attribution_event,
    ApprovalClass, CandidateArtifact, DeploymentEnvelope, IterationVerdict, PromotionRecord,
    RuntimeAttributionEvent, SearchPolicyRevision, SignedRuntimeAttributionEvent, StrategyBundle,
    SEALED_HOLDOUT_EVALUATOR_VERSION,
};
use alpha_engine::{
    evaluation::{prepare_dataset, WalkForwardConfig},
    formula_evaluator::{FormulaEvaluator, WALK_FORWARD_EVALUATOR_VERSION},
    CandidateEvaluation, EngineProposal,
};
use alpha_store::{AlphaStore, ApprovalRecord, MissionLineage, RegistryRevision, StoreError};
use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use ed25519_dalek::{SigningKey, VerifyingKey};
use std::collections::BTreeMap;

pub fn candidate_list(args: MissionStatusArgs) -> anyhow::Result<()> {
    let store = AlphaStore::open(args.db)?;
    let lineage = store.mission_lineage(&args.mission_id)?;
    print_json(&serde_json::json!({
        "mission_id": args.mission_id,
        "candidates": lineage.candidates,
        "evaluations": lineage.evaluations,
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
        let Some(candidate) = lineage
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_id == candidate_id)
        else {
            continue;
        };
        if !matches!(candidate.artifact, CandidateArtifact::Formula(_)) {
            continue;
        }
        let Some(stored) = lineage.evaluations.iter().find(|stored| {
            stored.record.evaluation_id == evaluation_id
                && stored.record.candidate_id == candidate_id
        }) else {
            continue;
        };
        if stored
            .record
            .payload
            .get("evaluator_version")
            .and_then(serde_json::Value::as_str)
            != Some(WALK_FORWARD_EVALUATOR_VERSION)
        {
            continue;
        }
        let evaluation: CandidateEvaluation = serde_json::from_value(stored.record.payload.clone())
            .with_context(|| format!("walk-forward evaluation {evaluation_id} is malformed"))?;
        evaluation
            .validate()
            .map_err(anyhow::Error::new)
            .with_context(|| format!("walk-forward evaluation {evaluation_id} is invalid"))?;
        if evaluation.passed && evaluation.evaluator_config == expected_config {
            candidates.push(candidate_id.to_string());
        }
    }
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

pub fn evaluate(args: EvaluateArgs) -> anyhow::Result<()> {
    let mut store = AlphaStore::open(&args.db)?;
    let revision_id = format!("sealed-evaluation:{}", args.candidate_id);
    let existing = match store.get_registry_revision(&revision_id) {
        Ok(existing) => Some(existing),
        Err(StoreError::NotFound) => None,
        Err(error) => return Err(error.into()),
    };

    let lineage = store.mission_lineage(&args.mission_id)?;
    if !validated_walk_forward_candidates_in_lineage(&lineage)?
        .iter()
        .any(|candidate_id| candidate_id == &args.candidate_id)
    {
        bail!("candidate lacks canonical v2 walk-forward evidence");
    }
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
            || existing_evaluation.evaluator_version != SEALED_HOLDOUT_EVALUATOR_VERSION
            || existing_evaluation.evaluator_config != expected_config
        {
            bail!("existing sealed evaluation is not canonical v2 evidence for this candidate");
        }
        return print_json(&existing);
    }

    let manifest =
        data_mission::read_registered_research_dataset(&store, &args.dataset.dataset_manifest)?;
    if lineage.mission.dataset_manifest_id.as_str() != manifest.manifest_id() {
        bail!("mission dataset id does not match the supplied manifest");
    }
    let rows = manifest.load_rows(
        args.dataset.fee_bps,
        args.dataset.funding_bps,
        args.dataset.latency_bps,
    )?;
    let dataset = prepare_dataset(
        rows,
        &WalkForwardConfig {
            initial_train_rows: args.dataset.initial_train_rows,
            validation_rows: args.dataset.validation_rows,
            fold_count: args.dataset.fold_count,
            purge_rows: args.dataset.purge_rows,
            embargo_rows: args.dataset.embargo_rows,
            sealed_holdout_rows: args.dataset.sealed_holdout_rows,
        },
        format!("sealed:{}", manifest.manifest_id()),
    )?;
    let proposal = EngineProposal {
        candidate_id: candidate.candidate_id.clone(),
        hypothesis: iteration.hypothesis.clone(),
        artifact: candidate.artifact.clone(),
        expansions: 0,
        tokens: 0,
        elapsed_ms: 0,
    };
    let evaluation = FormulaEvaluator::for_mission(&lineage.mission)
        .map_err(anyhow::Error::msg)?
        .evaluate_sealed(&proposal, &dataset)
        .map_err(anyhow::Error::msg)?;
    let revision = RegistryRevision {
        revision_id,
        registry_kind: "sealed_evaluation".to_string(),
        asset_id: args.candidate_id,
        parent_revision_id: None,
        payload: serde_json::json!({
            "mission_id": args.mission_id,
            "candidate_content_hash": candidate.content_hash,
            "dataset_manifest_id": manifest.manifest_id(),
            "evaluation": evaluation,
        }),
        created_at: Utc::now(),
    };
    store.put_registry_revision(&revision)?;
    print_json(&revision)
}

pub fn promote(args: PromoteArgs) -> anyhow::Result<()> {
    let mut store = AlphaStore::open(&args.db)?;
    let sealed_id = format!("sealed-evaluation:{}", args.candidate_id);
    let sealed = store.get_registry_revision(&sealed_id)?;
    let sealed_evaluation = sealed
        .payload
        .get("evaluation")
        .cloned()
        .context("sealed evaluation payload is incomplete")?;
    let evaluation: CandidateEvaluation = serde_json::from_value(sealed_evaluation.clone())?;
    evaluation.validate().map_err(anyhow::Error::new)?;
    if !evaluation.passed {
        bail!("candidate failed sealed holdout and cannot be promoted");
    }
    let lineage = store.mission_lineage(&args.mission_id)?;
    let candidate = lineage
        .candidates
        .iter()
        .find(|candidate| candidate.candidate_id == args.candidate_id)
        .context("candidate does not belong to mission")?;
    if sealed.asset_id != candidate.candidate_id
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
        return print_json(&serde_json::json!({
            "promotion": existing,
            "bundle": stored_bundle,
        }));
    }
    let stored = store.promote_candidate(&bundle, &promotion)?;
    print_json(&serde_json::json!({
        "promotion": stored,
        "bundle": bundle,
    }))
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
    store.validate_deployment_binding(&envelope)?;
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
) -> anyhow::Result<Vec<RuntimeAttributionEvent>> {
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
    use alpha_domain::{sign_runtime_attribution_event, AllowedIntentType};
    use chrono::Duration;

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
            "evaluator_version": "sealed-holdout-v2",
            "evaluator_config_hash": "2".repeat(64),
            "evaluation_metrics_hash": "3".repeat(64),
            "sealed_evaluation_hash": "4".repeat(64),
            "artifact": {
                "Formula": {"ast": {"Terminal": {"Field": "signal"}}}
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
            evaluator_config_hash: bundle.evaluator_config_hash.clone(),
            evaluation_metrics_hash: bundle.evaluation_metrics_hash.clone(),
            sealed_evaluation_id: "sealed-evaluation:candidate-1".to_string(),
            sealed_evaluation_hash: bundle.sealed_evaluation_hash.clone(),
            bundle_id: bundle.bundle_id.clone(),
            bundle_hash: bundle.bundle_hash.clone(),
            created_at: now,
        };
        (promotion, bundle)
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
            parse_runtime_attribution_log(&valid, &trusted).unwrap(),
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
}
