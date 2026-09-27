//! Durable external-operation identity and local dispatch serialization.
//!
//! A claim is committed before the first create. Retransmission may adopt that
//! Job, never recreate it. Holding the approval and family guards orders local
//! revocation/settlement against external operations; this is not a distributed
//! Kubernetes Lease or protection against an operator copying a live database.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignDispatchTargetV1 {
    pub context: String,
    pub namespace: String,
    pub job_name: String,
    pub manifest_sha256: String,
    /// Bound by native admission for stage-authorized Market Jobs. Omission
    /// preserves previously recorded claims and their original authority model.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_completion_authority: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl CampaignDispatchTargetV1 {
    pub fn validate(&self) -> Result<(), StoreError> {
        for value in [&self.context, &self.namespace, &self.job_name] {
            if value.is_empty()
                || value.len() > 253
                || value.trim() != value
                || value.chars().any(char::is_control)
            {
                return Err(err("invalid dispatch target"));
            }
        }
        if self.manifest_sha256.len() != 64
            || !self
                .manifest_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(err("invalid dispatch manifest SHA256"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignDispatchClaimV1 {
    pub target: CampaignDispatchTargetV1,
    pub job_uid: Option<String>,
    /// Last claim/binding receipt required before another external operation.
    pub sequence: u64,
}

/// This receipt records the dispatcher's independent Job/Pod and immutable
/// result readback. Failed or indeterminate attempts retain their full charge
/// until a separate, evidence-backed infrastructure reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignDispatchSettlementV1 {
    pub job_uid: String,
    pub pod_uid: String,
    pub settlement: CampaignAttemptSettlementV1,
}

/// Times from the independently verified bound Kubernetes Job and unique Pod,
/// never from a worker result. `completed_at` is the authority evaluation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignDispatchCompletionV1 {
    pub job_uid: String,
    pub pod_uid: String,
    pub job_started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
}

/// Authenticated evidence that the native controller shortened this exact Job's
/// deadline. It grants only failed-terminal readback, never a successful result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignDispatchCancellationV1 {
    pub operation_id: String,
    pub job_uid: String,
    pub pod_uid: String,
    pub original_resource_version: String,
    pub patched_resource_version: String,
    pub reason: String,
    pub requested_at: DateTime<Utc>,
    pub job_started_at: DateTime<Utc>,
    pub original_deadline_at: DateTime<Utc>,
    pub patch_sha256: String,
    pub patch_result_sha256: String,
}
impl CampaignDispatchCancellationV1 {
    pub(super) fn validate(
        &self,
        reservation: &CampaignAttemptReservationV1,
    ) -> Result<(), StoreError> {
        validate_job_uid(&self.job_uid)?;
        validate_job_uid(&self.pod_uid)?;
        let duration = chrono::TimeDelta::try_seconds(
            i64::try_from(reservation.reserved_job_seconds).map_err(err)?,
        )
        .ok_or_else(|| err("cancellation deadline overflow"))?;
        if self.operation_id != reservation.operation_id().map_err(err)?
            || self.original_resource_version.is_empty()
            || self.patched_resource_version.is_empty()
            || self.original_resource_version == self.patched_resource_version
            || self.reason.is_empty()
            || self.reason.len() > 8192
            || self.requested_at < self.job_started_at
            || self.job_started_at.checked_add_signed(duration) != Some(self.original_deadline_at)
        {
            return Err(err("invalid bound Job cancellation"));
        }
        super::final_dispatch::validate_digest(&self.patch_sha256)?;
        super::final_dispatch::validate_digest(&self.patch_result_sha256)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct CampaignDispatchRecord {
    pub root: VerifiedCampaignRootGrant,
    pub reservation: CampaignAttemptReservationV1,
    pub claim: CampaignDispatchClaimV1,
    pub settlement: Option<CampaignAttemptSettlementV1>,
    pub terminal_pod_uid: Option<String>,
    pub cancellation: Option<CampaignDispatchCancellationV1>,
}

pub(super) fn validate_job_uid(uid: &str) -> Result<(), StoreError> {
    if uid.is_empty() || uid.len() > 253 || uid.trim() != uid || uid.chars().any(char::is_control) {
        return Err(err("invalid dispatch Job UID"));
    }
    Ok(())
}

impl AlphaStore {
    /// Authenticated historical state, not permission for a new dispatch. It
    /// remains readable after expiry, revocation and signing-key rotation.
    pub fn campaign_dispatch_record(
        &self,
        family: &str,
        operation_id: &str,
    ) -> Result<CampaignDispatchRecord, StoreError> {
        let (state, _) = load(&self.connection, &self.integrity_key, family)?;
        let attempt = state
            .attempts
            .get(operation_id)
            .ok_or_else(|| err("missing attempt"))?;
        let root = state
            .roots
            .get(&attempt.reservation.root_grant_sha256)
            .ok_or_else(|| err("missing root"))?;
        Ok(CampaignDispatchRecord {
            root: root.grant.clone(),
            reservation: attempt.reservation.clone(),
            claim: attempt
                .dispatch
                .clone()
                .ok_or_else(|| err("missing dispatch claim"))?,
            settlement: attempt.settlement.clone(),
            terminal_pod_uid: attempt.terminal_pod_uid.clone(),
            cancellation: attempt.cancellation.clone(),
        })
    }

    pub fn record_campaign_dispatch_cancellation(
        &mut self,
        reservation: &CampaignAttemptReservationV1,
        cancellation: &CampaignDispatchCancellationV1,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        cancellation.validate(reservation)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        study::lock_member_settlement_guards(
            &tx,
            &self.integrity_key,
            &reservation.family_id,
            cancellation.requested_at,
        )?;
        let (state, history) = load(&tx, &self.integrity_key, &reservation.family_id)?;
        let a = state
            .attempts
            .get(&cancellation.operation_id)
            .ok_or_else(|| err("cancellation has no attempt"))?;
        if a.reservation != *reservation {
            return Err(err("cancellation reservation changed"));
        }
        if let Some(existing) = &a.cancellation {
            if existing != cancellation {
                return Err(err("cancellation evidence changed"));
            }
            return history.into_iter().find(|r|matches!(&r.receipt.event,CampaignLedgerEventV1::DispatchCancelled{evidence} if evidence==cancellation)).ok_or_else(||err("cancellation receipt missing"));
        }
        let receipt = append(
            &tx,
            &self.integrity_key,
            &reservation.family_id,
            CampaignLedgerEventV1::DispatchCancelled {
                evidence: cancellation.clone(),
            },
            Utc::now(),
        )?;
        tx.commit().map_err(database_error)?;
        Ok(receipt)
    }

    pub fn settle_campaign_dispatch(
        &mut self,
        expected: &CampaignAttemptReservationV1,
        evidence: &CampaignDispatchSettlementV1,
        at: DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        self.settle_dispatch(expected, evidence, None, at)
    }

    /// Historical authority at actual completion, including later-recorded
    /// revocations whose effective time precedes completion. This read does not
    /// authorize settlement: the guarded write below repeats it atomically.
    pub fn campaign_dispatch_completion_active(
        &self,
        expected: &CampaignAttemptReservationV1,
        completion: &CampaignDispatchCompletionV1,
        observed_at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        completion_active(
            &self.connection,
            &self.integrity_key,
            expected,
            completion,
            observed_at,
        )
    }

    pub fn settle_campaign_dispatch_at_completion(
        &mut self,
        expected: &CampaignAttemptReservationV1,
        evidence: &CampaignDispatchSettlementV1,
        completion: &CampaignDispatchCompletionV1,
        at: DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        self.settle_dispatch(expected, evidence, Some(completion), at)
    }

    fn settle_dispatch(
        &mut self,
        expected: &CampaignAttemptReservationV1,
        evidence: &CampaignDispatchSettlementV1,
        completion: Option<&CampaignDispatchCompletionV1>,
        at: DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        let tx = self.connection.transaction().map_err(database_error)?;
        let (state, _) = load(&tx, &self.integrity_key, &expected.family_id)?;
        let operation_id = expected.operation_id().map_err(err)?;
        if state
            .attempts
            .get(&operation_id)
            .is_none_or(|a| a.reservation != *expected)
            || evidence.settlement.operation_id != operation_id
        {
            return Err(err("terminal evidence differs from the reserved request"));
        }
        let attempt = &state.attempts[&operation_id];
        if completion.is_none()
            && attempt
                .dispatch
                .as_ref()
                .is_some_and(|claim| claim.target.require_completion_authority)
            && attempt.cancellation.is_none()
        {
            return Err(err(
                "this native dispatch requires completion authority evidence",
            ));
        }
        let study_id = study::lock_member_settlement_guards(
            &tx,
            &self.integrity_key,
            &expected.family_id,
            at,
        )?;
        if let Some(completion) = completion {
            let root = state
                .roots
                .get(&expected.root_grant_sha256)
                .ok_or_else(|| err("missing completion Root"))?;
            // The same approval guards are updated by revoke_approval. The
            // Study/family heads above also serialize registered revocations.
            serialize_approval_mutation(&tx, &root.approval.approval_id)?;
            study::lock_completion_approval(&tx, &self.integrity_key, &expected.family_id)?;
            if completion.job_uid != evidence.job_uid || completion.pod_uid != evidence.pod_uid {
                return Err(err("completion provenance differs from settlement"));
            }
            if !completion_active(&tx, &self.integrity_key, expected, completion, at)?
                && (evidence.settlement.outcome != CampaignAttemptOutcomeV1::Failed
                    || evidence.settlement.consumed_trials != Some(expected.declared_trials))
            {
                return Err(err(
                    "authority at completion requires Failed settlement with full charge",
                ));
            }
        }
        let prepared_study_id = study::prepare_member_settlement(
            &tx,
            &self.integrity_key,
            &expected.family_id,
            &evidence.settlement,
            at,
        )?;
        if study_id != prepared_study_id {
            return Err(err("study settlement membership changed"));
        }
        let receipt = append(
            &tx,
            &self.integrity_key,
            &expected.family_id,
            CampaignLedgerEventV1::DispatchSettled {
                evidence: evidence.clone(),
            },
            at,
        )?;
        study::append_member_settlement(
            &tx,
            &self.integrity_key,
            study_id.as_deref(),
            &expected.family_id,
            &evidence.settlement,
            &receipt,
            at,
        )?;
        tx.commit().map_err(database_error)?;
        Ok(receipt)
    }

    /// Checks a prepared reservation before publishing receipts. This does not
    /// authorize any Kubernetes operation: publication and the guarded check
    /// remain mandatory at dispatch.
    pub fn inspect_campaign_reservation(
        &self,
        verified: &VerifiedCampaignRootGrant,
        expected: &CampaignAttemptReservationV1,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let observed = checked_reservation(
            &self.connection,
            &self.integrity_key,
            verified,
            &expected.family_id,
            &expected.operation_id().map_err(err)?,
            at,
            false,
        )?;
        if observed != *expected {
            return Err(err("request differs from its reserved attempt"));
        }
        Ok(())
    }

    /// Returns `true` only to the invocation that durably creates the claim.
    /// After a crash in the claim/create gap, absence of the Job is uncertain;
    /// it must not be converted to permission to create again.
    pub fn claim_campaign_dispatch(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        target: &CampaignDispatchTargetV1,
        at: DateTime<Utc>,
    ) -> Result<(CampaignDispatchClaimV1, bool), StoreError> {
        target.validate()?;
        let operation_id = reservation.operation_id().map_err(err)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        lock_dispatch_guards(&tx, &self.integrity_key, verified, reservation, at)?;
        let observed = checked_reservation(
            &tx,
            &self.integrity_key,
            verified,
            &reservation.family_id,
            &operation_id,
            at,
            true,
        )?;
        if observed != *reservation {
            return Err(err("request differs from its reserved attempt"));
        }
        let (state, _) = load(&tx, &self.integrity_key, &reservation.family_id)?;
        if let Some(dispatch) = &state.attempts[&operation_id].dispatch {
            if dispatch.target != *target {
                return Err(err("dispatch target changed"));
            }
            return Ok((dispatch.clone(), false));
        }
        let receipt = append(
            &tx,
            &self.integrity_key,
            &reservation.family_id,
            CampaignLedgerEventV1::DispatchClaimed {
                operation_id,
                target: target.clone(),
            },
            at,
        )?;
        tx.commit().map_err(database_error)?;
        Ok((
            CampaignDispatchClaimV1 {
                target: target.clone(),
                job_uid: None,
                sequence: receipt.receipt.sequence,
            },
            true,
        ))
    }

    pub fn bind_campaign_dispatch_job(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        target: &CampaignDispatchTargetV1,
        job_uid: &str,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        validate_job_uid(job_uid)?;
        let operation_id = reservation.operation_id().map_err(err)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        lock_dispatch_guards(&tx, &self.integrity_key, verified, reservation, at)?;
        let observed = checked_reservation(
            &tx,
            &self.integrity_key,
            verified,
            &reservation.family_id,
            &operation_id,
            at,
            true,
        )?;
        if observed != *reservation {
            return Err(err("request differs from its reserved attempt"));
        }
        let (state, _) = load(&tx, &self.integrity_key, &reservation.family_id)?;
        let claim = state.attempts[&operation_id]
            .dispatch
            .as_ref()
            .ok_or_else(|| err("missing dispatch claim"))?;
        if claim.target != *target || claim.job_uid.as_ref().is_some_and(|uid| uid != job_uid) {
            return Err(err("dispatch target or Job UID changed"));
        }
        append(
            &tx,
            &self.integrity_key,
            &reservation.family_id,
            CampaignLedgerEventV1::DispatchJobBound {
                operation_id,
                job_uid: job_uid.into(),
            },
            at,
        )?;
        tx.commit().map_err(database_error)?;
        Ok(())
    }

    /// Issue a bounded stage permission inside the existing approval/family/Study
    /// serialization boundary. The attempt is already charged; this never reserves
    /// more time or trials and never interprets the original duration as new time.
    #[allow(clippy::too_many_arguments)]
    pub fn with_running_campaign_admission<T, E: From<StoreError>>(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        target: &CampaignDispatchTargetV1,
        job_uid: &str,
        job_started_at: DateTime<Utc>,
        now: impl FnOnce() -> DateTime<Utc>,
        action: impl FnOnce(DateTime<Utc>, DateTime<Utc>) -> Result<T, E>,
    ) -> Result<T, E> {
        let operation_id = reservation.operation_id().map_err(err)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        let (state, history) = load(&tx, &self.integrity_key, &reservation.family_id)?;
        let root = state
            .roots
            .get(verified.content_sha256())
            .ok_or_else(|| err("missing root"))?;
        serialize_approval_mutation(&tx, &root.approval.approval_id)?;
        let at = now();
        let study_id = study::lock_campaign_guards(
            &tx,
            &self.integrity_key,
            verified,
            &reservation.family_id,
            at,
            true,
        )?;
        if study_id.is_none() {
            return Err(err("running stages require a cumulative Study").into());
        }
        verified.validate_active_at(at).map_err(err)?;
        let attempt = state
            .attempts
            .get(&operation_id)
            .ok_or_else(|| err("missing reserved attempt"))?;
        let claim = attempt
            .dispatch
            .as_ref()
            .ok_or_else(|| err("missing dispatch claim"))?;
        let approval =
            read_effective_approval(&tx, &self.integrity_key, &root.approval.approval_id)?;
        let duration = chrono::TimeDelta::try_seconds(
            i64::try_from(reservation.reserved_job_seconds).map_err(err)?,
        )
        .ok_or_else(|| err("running Job duration overflow"))?;
        let deadline = job_started_at
            .checked_add_signed(duration)
            .ok_or_else(|| err("running Job deadline overflow"))?;
        if state.final_closure.is_some()
            || attempt.cancellation.is_some()
            || attempt.settlement.is_some()
            || attempt.reservation != *reservation
            || claim.target != *target
            || claim.job_uid.as_deref() != Some(job_uid)
            || !approval.is_active_at(at)
            || root.revoked_at.is_some_and(|when| at >= when)
            || job_started_at > at
            || at >= deadline
            || deadline > verified.grant().expires_at
        {
            return Err(err(
                "running attempt is revoked, expired, settled or has changed identity",
            )
            .into());
        }
        require_published_receipts(
            &tx,
            &self.integrity_key,
            &reservation.family_id,
            &history,
            claim.sequence,
        )?;
        let study_deadline =
            study::check_running_member(&tx, &self.integrity_key, verified, reservation, at)?;
        let mut authority_deadline = deadline
            .min(verified.grant().expires_at)
            .min(study_deadline);
        for when in [root.revoked_at, approval.revoked_at, approval.expires_at]
            .into_iter()
            .flatten()
        {
            authority_deadline = authority_deadline.min(when);
        }
        let result = action(at, authority_deadline);
        tx.commit().map_err(database_error)?;
        result
    }

    /// The caller's clock is sampled after acquiring both serialization guards.
    /// A concurrent revocation or settlement must conflict before the callback
    /// can start a Kubernetes write. Uncertain network outcomes keep the claim
    /// and full reservation; this method never refunds or creates a new attempt.
    pub fn with_campaign_dispatch_admission<T, E: From<StoreError>>(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        target: &CampaignDispatchTargetV1,
        expected_job_uid: Option<&str>,
        now: impl FnOnce() -> DateTime<Utc>,
        action: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let operation_id = reservation.operation_id().map_err(err)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        let (state, _) = load(&tx, &self.integrity_key, &reservation.family_id)?;
        let root = state
            .roots
            .get(verified.content_sha256())
            .ok_or_else(|| err("missing root"))?;
        let approval_id = root.approval.approval_id.clone();
        serialize_approval_mutation(&tx, &approval_id)?;
        let admission_at = now();
        study::lock_campaign_guards(
            &tx,
            &self.integrity_key,
            verified,
            &reservation.family_id,
            admission_at,
            true,
        )?;
        let observed = checked_reservation(
            &tx,
            &self.integrity_key,
            verified,
            &reservation.family_id,
            &operation_id,
            admission_at,
            true,
        )?;
        if observed != *reservation {
            return Err(err("request differs from its reserved attempt").into());
        }
        let claim = state
            .attempts
            .get(&operation_id)
            .and_then(|a| a.dispatch.as_ref())
            .ok_or_else(|| err("missing dispatch claim"))?;
        if claim.target != *target || claim.job_uid.as_deref() != expected_job_uid {
            return Err(err("dispatch target or Job UID changed").into());
        }
        let result = action();
        tx.commit().map_err(database_error)?;
        result
    }
}

pub(super) fn checked_reservation(
    conn: &Connection,
    key: &[u8; 32],
    verified: &VerifiedCampaignRootGrant,
    family: &str,
    operation_id: &str,
    at: DateTime<Utc>,
    require_publication: bool,
) -> Result<CampaignAttemptReservationV1, StoreError> {
    let (state, history) = load(conn, key, family)?;
    if state.final_closure.is_some() {
        return Err(err(
            "family search is permanently closed for final evaluation",
        ));
    }
    let a = state
        .attempts
        .get(operation_id)
        .ok_or_else(|| err("missing reservation"))?;
    verified
        .validate_attempt_scope(&a.reservation, at)
        .map_err(err)?;
    let root = state
        .roots
        .get(verified.content_sha256())
        .ok_or_else(|| err("missing root"))?;
    if root.revoked_at.is_some_and(|when| at >= when)
        || a.settlement.is_some()
        || a.cancellation.is_some()
        || !read_effective_approval(conn, key, &root.approval.approval_id)?.is_active_at(at)
    {
        return Err(err("attempt is settled or root is inactive"));
    }
    if let Some(when) = root.revoked_at {
        let duration = chrono::TimeDelta::try_seconds(
            i64::try_from(a.reservation.reserved_job_seconds).map_err(err)?,
        )
        .ok_or_else(|| err("deadline overflow"))?;
        if at.checked_add_signed(duration).is_none_or(|end| end > when) {
            return Err(err("Job exceeds scheduled revocation"));
        }
    }
    study::check_member_reservation(conn, key, verified, &a.reservation, at, require_publication)?;
    if !require_publication {
        return Ok(a.reservation.clone());
    }
    let through = a
        .dispatch
        .as_ref()
        .map_or(a.sequence, |dispatch| dispatch.sequence);
    require_published_receipts(conn, key, family, &history, through)?;
    Ok(a.reservation.clone())
}

fn completion_active(
    conn: &Connection,
    key: &[u8; 32],
    expected: &CampaignAttemptReservationV1,
    completion: &CampaignDispatchCompletionV1,
    observed_at: DateTime<Utc>,
) -> Result<bool, StoreError> {
    validate_job_uid(&completion.job_uid)?;
    validate_job_uid(&completion.pod_uid)?;
    if completion.completed_at < completion.job_started_at || completion.completed_at > observed_at
    {
        return Err(err(
            "completion time precedes Job start or is in the future",
        ));
    }
    let (state, history) = load(conn, key, &expected.family_id)?;
    let attempt = state
        .attempts
        .get(&expected.operation_id().map_err(err)?)
        .ok_or_else(|| err("missing completion attempt"))?;
    let claim = attempt
        .dispatch
        .as_ref()
        .ok_or_else(|| err("missing completion claim"))?;
    if attempt.reservation != *expected
        || claim.job_uid.as_deref() != Some(&completion.job_uid)
        || attempt
            .terminal_pod_uid
            .as_ref()
            .is_some_and(|uid| uid != &completion.pod_uid)
    {
        return Err(err("completion differs from the reserved Job identity"));
    }
    let root = state
        .roots
        .get(&expected.root_grant_sha256)
        .ok_or_else(|| err("missing completion Root"))?;
    require_published_receipts(conn, key, &expected.family_id, &history, claim.sequence)?;
    let approval = read_effective_approval(conn, key, &root.approval.approval_id)?;
    let duration =
        chrono::TimeDelta::try_seconds(i64::try_from(expected.reserved_job_seconds).map_err(err)?)
            .ok_or_else(|| err("completion deadline overflow"))?;
    let deadline = completion
        .job_started_at
        .checked_add_signed(duration)
        .ok_or_else(|| err("completion deadline overflow"))?;
    let at = completion.completed_at;
    // Always verify Study membership/integrity, even when Root authority failed.
    let study_active = study::completion_member_active(conn, key, &root.grant, expected, at)?;
    Ok(study_active
        && attempt.cancellation.is_none()
        && root.grant.validate_active_at(at).is_ok()
        && approval.is_active_at(at)
        && !root.revoked_at.is_some_and(|when| at >= when)
        && completion.job_started_at >= root.grant.grant().valid_from
        && at < deadline
        && deadline <= root.grant.grant().expires_at)
}

fn lock_dispatch_guards(
    tx: &Transaction<'_>,
    key: &[u8; 32],
    verified: &VerifiedCampaignRootGrant,
    reservation: &CampaignAttemptReservationV1,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let (state, _) = load(tx, key, &reservation.family_id)?;
    let root = state
        .roots
        .get(verified.content_sha256())
        .ok_or_else(|| err("missing root"))?;
    let approval_id = root.approval.approval_id.clone();
    serialize_approval_mutation(tx, &approval_id)?;
    study::lock_campaign_guards(tx, key, verified, &reservation.family_id, at, true)?;
    Ok(())
}
