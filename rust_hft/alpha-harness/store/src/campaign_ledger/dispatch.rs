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

#[derive(Debug, Clone)]
pub struct CampaignDispatchRecord {
    pub root: VerifiedCampaignRootGrant,
    pub reservation: CampaignAttemptReservationV1,
    pub claim: CampaignDispatchClaimV1,
    pub settlement: Option<CampaignAttemptSettlementV1>,
    pub terminal_pod_uid: Option<String>,
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
        })
    }

    pub fn settle_campaign_dispatch(
        &mut self,
        expected: &CampaignAttemptReservationV1,
        evidence: &CampaignDispatchSettlementV1,
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
        let receipt = append(
            &tx,
            &self.integrity_key,
            &expected.family_id,
            CampaignLedgerEventV1::DispatchSettled {
                evidence: evidence.clone(),
            },
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
        serialize_approval_mutation(&tx, &root.approval.approval_id)?;
        let changed = tx
            .execute(
                "UPDATE campaign_family_heads SET sequence = sequence WHERE family_id = ?",
                params![reservation.family_id],
            )
            .map_err(database_error)?;
        if changed != 1 {
            return Err(err("missing family guard").into());
        }
        let observed = checked_reservation(
            &tx,
            &self.integrity_key,
            verified,
            &reservation.family_id,
            &operation_id,
            now(),
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
    if !require_publication {
        return Ok(a.reservation.clone());
    }
    let through = a
        .dispatch
        .as_ref()
        .map_or(a.sequence, |dispatch| dispatch.sequence);
    for receipt in history.iter().take(usize::try_from(through).map_err(err)?) {
        let (hash, auth): (String, String) = conn.query_row("SELECT object_sha256, auth_tag FROM campaign_receipt_publications WHERE family_id = ? AND sequence = ?", params![family, sql_sequence(receipt.receipt.sequence)?], |r| Ok((r.get(0)?, r.get(1)?))).map_err(database_error)?;
        if hash != receipt.object_sha256()? {
            return Err(StoreError::ContentHashMismatch);
        }
        verify_authentication_tag(
            key,
            PUBLICATION_DOMAIN,
            &receipt.object_key(),
            &publication_json(receipt)?,
            &auth,
        )?;
    }
    Ok(a.reservation.clone())
}
