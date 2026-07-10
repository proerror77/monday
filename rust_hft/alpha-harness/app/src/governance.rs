use crate::{
    cli::{
        print_json, EnvelopeArgs, EvaluateArgs, JsonRecordArgs, MissionStatusArgs, PromoteArgs,
        SignDeploymentArgs,
    },
    data_mission,
};
use alpha_domain::{
    sign_envelope, AllowedIntentType, ApprovalClass, DeploymentEnvelope, IterationVerdict,
    RuntimeAttributionEvent, SearchPolicyRevision,
};
use alpha_engine::{
    evaluation::{prepare_dataset, WalkForwardConfig},
    formula_evaluator::{FormulaEvaluator, FormulaEvaluatorConfig},
    CandidateEvaluation, EngineProposal,
};
use alpha_store::{AlphaStore, ApprovalRecord, RegistryRevision, StoreError};
use anyhow::{bail, Context};
use chrono::Utc;
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

pub fn candidate_list(args: MissionStatusArgs) -> anyhow::Result<()> {
    let store = AlphaStore::open(args.db)?;
    let lineage = store.mission_lineage(&args.mission_id)?;
    print_json(&serde_json::json!({
        "mission_id": args.mission_id,
        "candidates": lineage.candidates,
        "evaluations": lineage.evaluations,
    }))
}

pub fn evaluate(args: EvaluateArgs) -> anyhow::Result<()> {
    let mut store = AlphaStore::open(&args.db)?;
    let revision_id = format!("sealed-evaluation:{}", args.candidate_id);
    match store.get_registry_revision(&revision_id) {
        Ok(existing) => return print_json(&existing),
        Err(StoreError::NotFound) => {}
        Err(error) => return Err(error.into()),
    }

    let lineage = store.mission_lineage(&args.mission_id)?;
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

    let manifest = data_mission::read_manifest(&args.dataset.dataset_manifest)?;
    if lineage.mission.dataset_manifest_id.as_str() != manifest.manifest_id {
        bail!("mission dataset id does not match the supplied manifest");
    }
    let rows = data_mission::load_research_rows(
        &manifest,
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
        format!("sealed:{}", manifest.manifest_id),
    )?;
    let proposal = EngineProposal {
        candidate_id: candidate.candidate_id.clone(),
        hypothesis: iteration.hypothesis.clone(),
        artifact: candidate.artifact.clone(),
        expansions: 0,
        tokens: 0,
        elapsed_ms: 0,
    };
    let evaluation = FormulaEvaluator::new(FormulaEvaluatorConfig::default())
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
            "dataset_manifest_id": manifest.manifest_id,
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
    let evaluation: CandidateEvaluation = serde_json::from_value(
        sealed
            .payload
            .get("evaluation")
            .cloned()
            .context("sealed evaluation payload is incomplete")?,
    )?;
    if !evaluation.passed {
        bail!("candidate failed sealed holdout and cannot be promoted");
    }
    let lineage = store.mission_lineage(&args.mission_id)?;
    let candidate = lineage
        .candidates
        .iter()
        .find(|candidate| candidate.candidate_id == args.candidate_id)
        .context("candidate does not belong to mission")?;
    let promotion_id = args
        .promotion_id
        .unwrap_or_else(|| format!("promotion:{}", args.candidate_id));
    match store.get_registry_revision(&promotion_id) {
        Ok(existing) => return print_json(&existing),
        Err(StoreError::NotFound) => {}
        Err(error) => return Err(error.into()),
    }
    let revision = RegistryRevision {
        revision_id: promotion_id,
        registry_kind: "promotion".to_string(),
        asset_id: args.candidate_id,
        parent_revision_id: Some(sealed_id),
        payload: serde_json::json!({
            "mission_id": args.mission_id,
            "candidate_content_hash": candidate.content_hash,
            "sealed_evaluation": evaluation,
            "capability": "research-only",
        }),
        created_at: Utc::now(),
    };
    store.put_registry_revision(&revision)?;
    print_json(&revision)
}

pub fn sign_deployment(args: SignDeploymentArgs) -> anyhow::Result<()> {
    let envelope: DeploymentEnvelope = serde_json::from_slice(
        &std::fs::read(&args.envelope)
            .with_context(|| format!("failed to read envelope {}", args.envelope.display()))?,
    )?;
    let mut store = AlphaStore::open(args.db)?;
    enforce_live_small_approval(&store, &envelope)?;
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

pub fn ingest_feedback(args: JsonRecordArgs) -> anyhow::Result<()> {
    let event: RuntimeAttributionEvent = read_record(&args.record)?;
    let mut store = AlphaStore::open(args.db)?;
    let inserted = store.ingest_runtime_attribution(event.clone())?;
    let stored = store.get_runtime_attribution(&event.event_id)?;
    print_json(&serde_json::json!({
        "inserted": inserted,
        "event": stored,
    }))
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

fn enforce_live_small_approval(
    store: &AlphaStore,
    envelope: &DeploymentEnvelope,
) -> anyhow::Result<()> {
    if !envelope
        .allowed_intent_types
        .contains(&AllowedIntentType::StartLiveSmall)
    {
        return Ok(());
    }
    if !matches!(
        envelope.approval_class,
        ApprovalClass::HumanApprovedLiveSmall | ApprovalClass::SameClassAutoLiveSmall
    ) {
        bail!("live-small deployment requires a live-small approval class");
    }
    let scope_hash = deployment_scope_hash(envelope)?;
    let approved = envelope.approval_signatures.iter().any(|approval_id| {
        store.get_approval(approval_id).is_ok_and(|approval| {
            approval.approval_class == "human_live_small"
                && approval
                    .payload
                    .get("scope_hash")
                    .and_then(serde_json::Value::as_str)
                    == Some(scope_hash.as_str())
                && (envelope.approval_class == ApprovalClass::SameClassAutoLiveSmall
                    || approval.subject_id == envelope.asset_revision_id)
        })
    });
    if !approved {
        bail!("live-small deployment has no matching persisted human approval");
    }
    Ok(())
}

fn deployment_scope_hash(envelope: &DeploymentEnvelope) -> anyhow::Result<String> {
    let mut instruments = envelope.instruments.clone();
    instruments.sort();
    let scope = serde_json::json!({
        "venue": envelope.venue,
        "instruments": instruments,
        "intent": "live_small",
    });
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&scope)?)))
}

fn read_record<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> anyhow::Result<T> {
    serde_json::from_slice(
        &std::fs::read(path)
            .with_context(|| format!("failed to read record {}", path.display()))?,
    )
    .with_context(|| format!("record {} is invalid JSON", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn live_small_envelope() -> DeploymentEnvelope {
        let now = Utc::now();
        DeploymentEnvelope {
            deployment_id: "deployment-live-small".to_string(),
            asset_revision_id: "factor-1@2".to_string(),
            promotion_manifest_hash: "promotion-hash".to_string(),
            runtime_config_hash: "runtime-hash".to_string(),
            risk_policy_hash: "risk-hash".to_string(),
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
            approval_class: ApprovalClass::SameClassAutoLiveSmall,
            approval_signatures: vec!["approval-1".to_string()],
            payload_hash: String::new(),
        }
    }

    #[test]
    fn live_small_requires_a_persisted_human_approval_for_the_same_scope() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let envelope = live_small_envelope();
        assert!(enforce_live_small_approval(&store, &envelope).is_err());
        store
            .record_approval(&ApprovalRecord {
                approval_id: "approval-1".to_string(),
                approval_class: "human_live_small".to_string(),
                subject_id: "earlier-factor@1".to_string(),
                payload: serde_json::json!({
                    "scope_hash": deployment_scope_hash(&envelope).unwrap(),
                }),
                created_at: Utc::now(),
            })
            .unwrap();
        assert!(enforce_live_small_approval(&store, &envelope).is_ok());
    }
}
