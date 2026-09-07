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
}

#[derive(Default)]
pub(super) struct State {
    pub family: Option<CampaignFamilyPolicyV1>,
    pub roots: BTreeMap<String, Root>,
    pub attempts: BTreeMap<String, Attempt>,
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
                    },
                );
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
            CampaignLedgerEventV1::ApprovalRevoked { revocation } => {
                let root = self
                    .roots
                    .values_mut()
                    .find(|r| r.approval.approval_id == revocation.approval_id)
                    .ok_or_else(|| err("revocation has no registered root"))?;
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
