use super::*;
use alpha_domain::campaign_control::{verify_campaign_root_grant, CampaignFamilyPolicyV1};
use ed25519_dalek::VerifyingKey;
use std::collections::BTreeMap;

pub(super) struct Root {
    pub grant: VerifiedCampaignRootGrant,
    pub approval: ApprovalRecord,
    pub approval_hash: String,
    pub revoked_at: Option<DateTime<Utc>>,
}

pub(super) struct Attempt {
    pub reservation: CampaignAttemptReservationV1,
    pub sequence: u64,
    pub settlement: Option<CampaignAttemptSettlementV1>,
    pub dispatch: Option<CampaignDispatchClaimV1>,
    pub terminal_pod_uid: Option<String>,
}

pub(super) struct FinalClosure {
    pub grant: VerifiedCampaignFinalEvaluationGrant,
    pub approval: ApprovalRecord,
    pub approval_hash: String,
    pub revoked_at: Option<DateTime<Utc>>,
    pub dispatch: Option<CampaignFinalDispatchClaimV1>,
    pub settlement: Option<CampaignFinalDispatchSettlementV1>,
}

#[derive(Default)]
pub(super) struct State {
    pub family: Option<CampaignFamilyPolicyV1>,
    pub roots: BTreeMap<String, Root>,
    pub attempts: BTreeMap<String, Attempt>,
    pub final_closure: Option<FinalClosure>,
}

impl State {
    pub fn usage(&self, root: Option<&str>) -> Result<CampaignBudgetUsageV1, StoreError> {
        let mut usage = CampaignBudgetUsageV1::default();
        for a in self
            .attempts
            .values()
            .filter(|a| root.is_none_or(|id| a.reservation.root_grant_sha256 == id))
        {
            match &a.settlement {
                None => {
                    usage.pending_trials = add(usage.pending_trials, a.reservation.declared_trials)?
                }
                Some(s) => match s.consumed_trials {
                    Some(count) => usage.consumed_trials = add(usage.consumed_trials, count)?,
                    None => {
                        usage.uncertain_trials =
                            add(usage.uncertain_trials, a.reservation.declared_trials)?
                    }
                },
            }
            usage.job_attempts = add(usage.job_attempts, 1)?;
            usage.reserved_job_seconds = add(
                usage.reserved_job_seconds,
                a.reservation.reserved_job_seconds,
            )?;
            usage.reserved_llm_tokens =
                add(usage.reserved_llm_tokens, a.reservation.reserved_llm_tokens)?;
        }
        Ok(usage)
    }

    pub fn apply(&mut self, receipt: &CampaignLedgerReceiptV1) -> Result<(), StoreError> {
        if self.final_closure.is_some()
            && matches!(
                &receipt.event,
                CampaignLedgerEventV1::RootRegistered { .. }
                    | CampaignLedgerEventV1::AttemptReserved { .. }
                    | CampaignLedgerEventV1::DispatchClaimed { .. }
                    | CampaignLedgerEventV1::DispatchJobBound { .. }
            )
        {
            return Err(err(
                "family search is permanently closed for final evaluation",
            ));
        }
        match &receipt.event {
            CampaignLedgerEventV1::RootRegistered {
                signed,
                verifying_key_hex,
                approval,
                approval_content_sha256,
            } => {
                let bytes: [u8; 32] = hex::decode(verifying_key_hex)
                    .map_err(err)?
                    .try_into()
                    .map_err(|_| err("invalid historical verification key"))?;
                let key = VerifyingKey::from_bytes(&bytes).map_err(err)?;
                // Receipt authenticity was checked before replay. This records
                // the key trusted at registration, not a new trust decision.
                let verified = verify_campaign_root_grant(
                    signed,
                    &BTreeMap::from([(signed.key_id.clone(), key)]),
                    receipt.recorded_at,
                )
                .map_err(err)?;
                let grant = verified.grant();
                validate_approval(
                    approval,
                    approval_content_sha256,
                    &verified,
                    receipt.recorded_at,
                )?;
                if receipt.family_id != grant.family.family_id
                    || self.family.as_ref().is_some_and(|p| p != &grant.family)
                {
                    return Err(err("family definition or ceiling changed"));
                }
                if self
                    .roots
                    .values()
                    .any(|root| root.grant.grant().root_id == grant.root_id)
                    || self.roots.contains_key(verified.content_sha256())
                {
                    return Err(err("root identity was already registered"));
                }
                self.family = Some(grant.family.clone());
                self.roots.insert(
                    verified.content_sha256().into(),
                    Root {
                        grant: verified,
                        approval: approval.clone(),
                        approval_hash: approval_content_sha256.clone(),
                        revoked_at: None,
                    },
                );
            }
            CampaignLedgerEventV1::AttemptReserved { reservation } => {
                let root = self
                    .roots
                    .get(&reservation.root_grant_sha256)
                    .ok_or_else(|| err("root is not registered"))?;
                root.grant
                    .validate_attempt_scope(reservation, receipt.recorded_at)
                    .map_err(err)?;
                if root
                    .revoked_at
                    .is_some_and(|when| receipt.recorded_at >= when)
                {
                    return Err(err("root approval is revoked"));
                }
                if let Some(when) = root.revoked_at {
                    let duration = chrono::TimeDelta::try_seconds(
                        i64::try_from(reservation.reserved_job_seconds).map_err(err)?,
                    )
                    .ok_or_else(|| err("deadline overflow"))?;
                    if receipt
                        .recorded_at
                        .checked_add_signed(duration)
                        .is_none_or(|end| end > when)
                    {
                        return Err(err("Job exceeds scheduled revocation"));
                    }
                }
                if receipt.family_id != reservation.family_id {
                    return Err(err("reservation family mismatch"));
                }
                let same_generation: Vec<_> = self
                    .attempts
                    .values()
                    .filter(|a| {
                        a.reservation.root_grant_sha256 == reservation.root_grant_sha256
                            && a.reservation.generation == reservation.generation
                    })
                    .collect();
                if same_generation.iter().any(|a| {
                    a.reservation.campaign_id != reservation.campaign_id
                        || a.reservation.request_sha256 != reservation.request_sha256
                        || a.reservation.policy_revision_id != reservation.policy_revision_id
                        || a.reservation.parent_result_sha256 != reservation.parent_result_sha256
                }) {
                    return Err(err("generation identity changed"));
                }
                if let Some(last) = same_generation
                    .iter()
                    .max_by_key(|a| a.reservation.attempt_ordinal)
                {
                    let Some(settlement) = &last.settlement else {
                        return Err(err("previous attempt is still unresolved"));
                    };
                    if matches!(
                        settlement.outcome,
                        CampaignAttemptOutcomeV1::NoCandidate
                            | CampaignAttemptOutcomeV1::SelectedPreHoldout
                    ) || last.reservation.attempt_ordinal.checked_add(1)
                        != Some(reservation.attempt_ordinal)
                    {
                        return Err(err("completed generation or nonsequential retry"));
                    }
                } else if reservation.attempt_ordinal != 0 {
                    return Err(err("first attempt ordinal must be zero"));
                }
                if reservation.generation > 0 {
                    let parent = self
                        .attempts
                        .values()
                        .find(|a| {
                            a.reservation.root_grant_sha256 == reservation.root_grant_sha256
                                && a.reservation.generation == reservation.generation - 1
                                && a.settlement.as_ref().is_some_and(|s| {
                                    s.outcome == CampaignAttemptOutcomeV1::NoCandidate
                                        && Some(&s.evidence_sha256)
                                            == reservation.parent_result_sha256.as_ref()
                                })
                        })
                        .ok_or_else(|| err("child has no completed negative parent"))?;
                    if parent.reservation.policy_revision_id == reservation.policy_revision_id
                        || self.attempts.values().any(|a| {
                            a.reservation.root_grant_sha256 == reservation.root_grant_sha256
                                && a.reservation.generation < reservation.generation
                                && a.reservation.policy_revision_id
                                    == reservation.policy_revision_id
                        })
                    {
                        return Err(err("child repeats an ancestor policy"));
                    }
                }
                let root_usage = self.usage(Some(&reservation.root_grant_sha256))?;
                let family_usage = self.usage(None)?;
                let budget = &root.grant.grant().budget;
                if add(root_usage.accounted_trials()?, reservation.declared_trials)?
                    > budget.max_trials
                    || add(
                        family_usage.accounted_trials()?,
                        reservation.declared_trials,
                    )? > root.grant.grant().family.max_trials
                    || add(root_usage.job_attempts, 1)? > budget.max_job_attempts
                    || add(
                        root_usage.reserved_job_seconds,
                        reservation.reserved_job_seconds,
                    )? > budget.max_job_seconds
                    || add(
                        root_usage.reserved_llm_tokens,
                        reservation.reserved_llm_tokens,
                    )? > budget.max_llm_tokens
                {
                    return Err(err("cumulative Campaign budget exhausted"));
                }
                self.attempts.insert(
                    reservation.operation_id().map_err(err)?,
                    Attempt {
                        reservation: reservation.clone(),
                        sequence: receipt.sequence,
                        settlement: None,
                        dispatch: None,
                        terminal_pod_uid: None,
                    },
                );
            }
            CampaignLedgerEventV1::DispatchClaimed {
                operation_id,
                target,
            } => {
                target.validate()?;
                let attempt = self
                    .attempts
                    .get_mut(operation_id)
                    .ok_or_else(|| err("dispatch has no reservation"))?;
                if attempt.dispatch.is_some() || attempt.settlement.is_some() {
                    return Err(err("attempt already dispatched or settled"));
                }
                let root = self
                    .roots
                    .get(&attempt.reservation.root_grant_sha256)
                    .ok_or_else(|| err("dispatch has no root"))?;
                root.grant
                    .validate_attempt_scope(&attempt.reservation, receipt.recorded_at)
                    .map_err(err)?;
                if root
                    .revoked_at
                    .is_some_and(|when| receipt.recorded_at >= when)
                {
                    return Err(err("dispatch root is revoked"));
                }
                attempt.dispatch = Some(CampaignDispatchClaimV1 {
                    target: target.clone(),
                    job_uid: None,
                    sequence: receipt.sequence,
                });
            }
            CampaignLedgerEventV1::DispatchJobBound {
                operation_id,
                job_uid,
            } => {
                super::dispatch::validate_job_uid(job_uid)?;
                let attempt = self
                    .attempts
                    .get_mut(operation_id)
                    .ok_or_else(|| err("Job binding has no reservation"))?;
                if attempt.settlement.is_some() {
                    return Err(err("Job binding attempt is settled"));
                }
                let dispatch = attempt
                    .dispatch
                    .as_mut()
                    .ok_or_else(|| err("Job binding has no dispatch claim"))?;
                if dispatch.job_uid.is_some() {
                    return Err(err("dispatch Job is already bound"));
                }
                dispatch.job_uid = Some(job_uid.clone());
                dispatch.sequence = receipt.sequence;
            }
            CampaignLedgerEventV1::DispatchSettled { evidence } => {
                let attempt = self
                    .attempts
                    .get_mut(&evidence.settlement.operation_id)
                    .ok_or_else(|| err("dispatch settlement has no reservation"))?;
                evidence
                    .settlement
                    .validate_against(&attempt.reservation)
                    .map_err(err)?;
                super::dispatch::validate_job_uid(&evidence.job_uid)?;
                super::dispatch::validate_job_uid(&evidence.pod_uid)?;
                let dispatch = attempt
                    .dispatch
                    .as_ref()
                    .ok_or_else(|| err("dispatch settlement has no claim"))?;
                if dispatch.job_uid.as_deref() != Some(evidence.job_uid.as_str())
                    || attempt.settlement.is_some()
                    || !matches!(
                        evidence.settlement.outcome,
                        CampaignAttemptOutcomeV1::NoCandidate
                            | CampaignAttemptOutcomeV1::SelectedPreHoldout
                    )
                {
                    return Err(err("dispatch terminal identity or outcome is invalid"));
                }
                attempt.settlement = Some(evidence.settlement.clone());
                attempt.terminal_pod_uid = Some(evidence.pod_uid.clone());
            }
            CampaignLedgerEventV1::AttemptSettled { settlement } => {
                let attempt = self
                    .attempts
                    .get_mut(&settlement.operation_id)
                    .ok_or_else(|| err("settlement has no reservation"))?;
                settlement
                    .validate_against(&attempt.reservation)
                    .map_err(err)?;
                if attempt.settlement.is_some() {
                    return Err(err("attempt already settled"));
                }
                attempt.settlement = Some(settlement.clone());
            }
            CampaignLedgerEventV1::FinalDispatchClaimed {
                request_sha256,
                target,
            } => {
                final_dispatch::validate_digest(request_sha256)?;
                target.validate()?;
                let closure = self
                    .final_closure
                    .as_mut()
                    .ok_or_else(|| err("family is not closed"))?;
                closure
                    .grant
                    .validate_job_deadline_at(receipt.recorded_at)
                    .map_err(err)?;
                if closure.dispatch.is_some()
                    || closure.settlement.is_some()
                    || closure
                        .revoked_at
                        .is_some_and(|when| receipt.recorded_at >= when)
                {
                    return Err(err("final evaluation already claimed or revoked"));
                }
                closure.dispatch = Some(CampaignFinalDispatchClaimV1 {
                    request_sha256: request_sha256.clone(),
                    dispatch: CampaignDispatchClaimV1 {
                        target: target.clone(),
                        job_uid: None,
                        sequence: receipt.sequence,
                    },
                    claimed_at: receipt.recorded_at,
                });
            }
            CampaignLedgerEventV1::FinalDispatchJobBound { job_uid } => {
                dispatch::validate_job_uid(job_uid)?;
                let closure = self
                    .final_closure
                    .as_mut()
                    .ok_or_else(|| err("family is not closed"))?;
                closure
                    .grant
                    .validate_job_deadline_at(receipt.recorded_at)
                    .map_err(err)?;
                if closure.settlement.is_some()
                    || closure
                        .revoked_at
                        .is_some_and(|when| receipt.recorded_at >= when)
                {
                    return Err(err("final evaluation settled or revoked"));
                }
                let claim = closure
                    .dispatch
                    .as_mut()
                    .ok_or_else(|| err("missing final dispatch claim"))?;
                if claim.dispatch.job_uid.is_some() {
                    return Err(err("final Job already bound"));
                }
                claim.dispatch.job_uid = Some(job_uid.clone());
                claim.dispatch.sequence = receipt.sequence;
            }
            CampaignLedgerEventV1::FinalDispatchSettled { evidence } => {
                final_dispatch::validate_digest(&evidence.request_sha256)?;
                final_dispatch::validate_digest(&evidence.result_sha256)?;
                dispatch::validate_job_uid(&evidence.job_uid)?;
                dispatch::validate_job_uid(&evidence.pod_uid)?;
                let closure = self
                    .final_closure
                    .as_mut()
                    .ok_or_else(|| err("family is not closed"))?;
                let claim = closure
                    .dispatch
                    .as_ref()
                    .ok_or_else(|| err("missing final dispatch claim"))?;
                let grant = closure.grant.grant();
                if closure.settlement.is_some()
                    || claim.request_sha256 != evidence.request_sha256
                    || claim.dispatch.job_uid.as_deref() != Some(&evidence.job_uid)
                    || receipt.recorded_at < claim.claimed_at
                    || evidence
                        .candidates_evaluated
                        .is_some_and(|count| count > grant.max_candidates)
                    || (evidence.outcome != CampaignFinalOutcomeV1::Failed
                        && (evidence.candidates_evaluated.is_none_or(|count| {
                            count == 0
                                && evidence.outcome != CampaignFinalOutcomeV1::NoSelectionCandidate
                        }) || evidence
                            .consumed_job_seconds
                            .is_none_or(|seconds| seconds > grant.max_job_seconds)))
                {
                    return Err(err("invalid final terminal identity, budget or outcome"));
                }
                closure.settlement = Some(evidence.clone());
            }
            CampaignLedgerEventV1::FamilyClosedForFinalEvaluation {
                signed,
                verifying_key_hex,
                approval,
                approval_content_sha256,
            } => {
                let bytes: [u8; 32] = hex::decode(verifying_key_hex)
                    .map_err(err)?
                    .try_into()
                    .map_err(|_| err("invalid historical final verification key"))?;
                let key = VerifyingKey::from_bytes(&bytes).map_err(err)?;
                let verified = verify_campaign_final_evaluation_grant(
                    signed,
                    &BTreeMap::from([(signed.key_id.clone(), key)]),
                    receipt.recorded_at,
                )
                .map_err(err)?;
                verified
                    .validate_job_deadline_at(receipt.recorded_at)
                    .map_err(err)?;
                validate_final_approval(
                    approval,
                    approval_content_sha256,
                    &verified,
                    receipt.recorded_at,
                )?;
                let grant = verified.grant();
                if self.final_closure.is_some()
                    || receipt.family_id != grant.family_id
                    || receipt.previous_receipt_sha256.as_deref() != Some(&grant.family_head_sha256)
                    || self.family.as_ref().is_none_or(|family| {
                        family.definition_sha256 != grant.family_definition_sha256
                    })
                    || self.attempts.is_empty()
                    || self
                        .roots
                        .values()
                        .any(|root| root.grant.grant().execution != grant.execution)
                {
                    return Err(err("final evaluation family, view or ledger head mismatch"));
                }
                let mut selected = BTreeMap::new();
                for (operation, attempt) in &self.attempts {
                    let settlement = attempt
                        .settlement
                        .as_ref()
                        .ok_or_else(|| err("family has unresolved attempts"))?;
                    if settlement.consumed_trials.is_none() || attempt.terminal_pod_uid.is_none() {
                        return Err(err("family lacks canonical terminal consumption evidence"));
                    }
                    if settlement.outcome == CampaignAttemptOutcomeV1::SelectedPreHoldout {
                        selected.insert(operation.clone(), settlement.evidence_sha256.clone());
                    }
                }
                if selected != grant.selected_results {
                    return Err(err(
                        "final result set differs from the complete settled family",
                    ));
                }
                self.final_closure = Some(FinalClosure {
                    grant: verified,
                    approval: approval.clone(),
                    approval_hash: approval_content_sha256.clone(),
                    revoked_at: None,
                    dispatch: None,
                    settlement: None,
                });
            }
            CampaignLedgerEventV1::ApprovalRevoked { revocation } => {
                if let Some(closure) = self
                    .final_closure
                    .as_mut()
                    .filter(|closure| closure.approval.approval_id == revocation.approval_id)
                {
                    revocation.apply_to(closure.approval.clone(), &closure.approval_hash)?;
                    if closure.revoked_at.is_some() {
                        return Err(err("final approval already revoked"));
                    }
                    closure.revoked_at = Some(revocation.revoked_at);
                    return Ok(());
                }
                let root = self
                    .roots
                    .values_mut()
                    .find(|r| r.approval.approval_id == revocation.approval_id)
                    .ok_or_else(|| err("revocation has no registered Campaign approval"))?;
                revocation.apply_to(root.approval.clone(), &root.approval_hash)?;
                if root.revoked_at.is_some() {
                    return Err(err("root already revoked"));
                }
                root.revoked_at = Some(revocation.revoked_at);
            }
        }
        Ok(())
    }
}
