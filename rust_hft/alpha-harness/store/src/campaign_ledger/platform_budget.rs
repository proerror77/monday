//! Opaque evidence from the charged native ledger. Reads grant no new budget.
//! Export callbacks retain approval, family and Study guards through the action.
use super::*;

#[derive(Debug, Clone)]
pub struct VerifiedCampaignPlatformBudget {
    root: VerifiedCampaignRootGrant,
    reservation: CampaignAttemptReservationV1,
    root_receipt: AuthenticatedCampaignReceiptV1,
    reservation_receipt: AuthenticatedCampaignReceiptV1,
    approval_sha256: String,
    authority_expires_at: DateTime<Utc>,
    existing_transfer: Option<CampaignPlatformTransferV1>,
}
impl VerifiedCampaignPlatformBudget {
    pub fn root(&self) -> &VerifiedCampaignRootGrant {
        &self.root
    }
    pub fn reservation(&self) -> &CampaignAttemptReservationV1 {
        &self.reservation
    }
    pub fn root_receipt(&self) -> &AuthenticatedCampaignReceiptV1 {
        &self.root_receipt
    }
    pub fn reservation_receipt(&self) -> &AuthenticatedCampaignReceiptV1 {
        &self.reservation_receipt
    }
    pub fn approval_sha256(&self) -> &str {
        &self.approval_sha256
    }
    pub fn authority_expires_at(&self) -> DateTime<Utc> {
        self.authority_expires_at
    }
    pub fn existing_transfer(&self) -> Option<&CampaignPlatformTransferV1> {
        self.existing_transfer.as_ref()
    }
    pub fn operation_sha256(&self) -> Result<String, StoreError> {
        alpha_domain::canonical_json_hash(&self.reservation.operation_id().map_err(err)?)
            .map_err(err)
    }
}

#[derive(Debug, Clone)]
pub struct VerifiedCampaignPlatformExport {
    budget: VerifiedCampaignPlatformBudget,
    transfer: CampaignPlatformTransferV1,
    transfer_receipt: AuthenticatedCampaignReceiptV1,
}
impl VerifiedCampaignPlatformExport {
    pub fn budget(&self) -> &VerifiedCampaignPlatformBudget {
        &self.budget
    }
    pub fn transfer(&self) -> &CampaignPlatformTransferV1 {
        &self.transfer
    }
    pub fn transfer_receipt(&self) -> &AuthenticatedCampaignReceiptV1 {
        &self.transfer_receipt
    }
}

/// A source constraint for an existing platform-owned operation. Private fields
/// prevent caller JSON from selecting a reason or another tenant's request.
#[derive(Debug, Clone)]
pub struct VerifiedCampaignPlatformRevocation {
    transfer: CampaignPlatformTransferV1,
    root_grant_sha256: String,
    family_id: String,
    reason_receipt_sha256: String,
    effective_at: DateTime<Utc>,
}
impl VerifiedCampaignPlatformRevocation {
    pub fn transfer(&self) -> &CampaignPlatformTransferV1 {
        &self.transfer
    }
    pub fn root_grant_sha256(&self) -> &str {
        &self.root_grant_sha256
    }
    pub fn family_id(&self) -> &str {
        &self.family_id
    }
    pub fn reason_receipt_sha256(&self) -> &str {
        &self.reason_receipt_sha256
    }
    pub fn effective_at(&self) -> DateTime<Utc> {
        self.effective_at
    }
    pub fn operation_sha256(&self) -> Result<String, StoreError> {
        alpha_domain::canonical_json_hash(&self.transfer.operation_id).map_err(err)
    }
}

impl AlphaStore {
    /// Historical Root/Study revocations remain projectable after source expiry.
    /// Every returned item names one already transferred request. It gives no
    /// right to submit, mint an approval, refund or settle that operation.
    pub fn campaign_platform_revocations(
        &self,
        family_id: &str,
    ) -> Result<Vec<VerifiedCampaignPlatformRevocation>, StoreError> {
        let (state, history) = load(&self.connection, &self.integrity_key, family_id)?;
        let mut output = Vec::new();
        for attempt in state.attempts.values() {
            let Some(transfer) = &attempt.platform_transfer else {
                continue;
            };
            let transfer_entry = history.iter().find(|entry| matches!(
                &entry.receipt.event, CampaignLedgerEventV1::PlatformTransferred { transfer: observed } if observed == transfer
            )).ok_or_else(|| err("revocation transfer receipt is missing"))?;
            require_published_receipts(
                &self.connection,
                &self.integrity_key,
                family_id,
                &history,
                transfer_entry.receipt.sequence,
            )?;
            let root = &state.roots[&attempt.reservation.root_grant_sha256];
            let mut reasons = Vec::new();
            for entry in &history {
                if let CampaignLedgerEventV1::ApprovalRevoked { revocation } = &entry.receipt.event
                {
                    if revocation.approval_id == root.approval.approval_id {
                        require_published_receipts(
                            &self.connection,
                            &self.integrity_key,
                            family_id,
                            &history,
                            entry.receipt.sequence,
                        )?;
                        reasons.push((revocation.clone(), entry.object_sha256()?));
                    }
                }
            }
            reasons.extend(study::published_member_revocations(
                &self.connection,
                &self.integrity_key,
                family_id,
                &attempt.reservation.root_grant_sha256,
            )?);
            for (reason, reason_receipt_sha256) in reasons {
                output.push(VerifiedCampaignPlatformRevocation {
                    transfer: transfer.clone(),
                    root_grant_sha256: attempt.reservation.root_grant_sha256.clone(),
                    family_id: family_id.into(),
                    reason_receipt_sha256,
                    effective_at: reason.revoked_at,
                });
            }
        }
        output.sort_by(|a, b| {
            (
                &a.transfer.operation_id,
                a.effective_at,
                &a.reason_receipt_sha256,
            )
                .cmp(&(
                    &b.transfer.operation_id,
                    b.effective_at,
                    &b.reason_receipt_sha256,
                ))
        });
        Ok(output)
    }

    /// Only an existing reserved and independently published operation can
    /// produce this witness. The callback cannot reserve or refund another run.
    pub fn with_campaign_platform_budget<T, E: From<StoreError>>(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        action: impl FnOnce(&VerifiedCampaignPlatformBudget) -> Result<T, E>,
    ) -> Result<T, E> {
        self.with_campaign_platform_budget_impl(verified, reservation, Utc::now, action)
    }

    // The public API fixes real time; only source-ledger tests inject a clock.
    #[cfg(test)]
    pub(super) fn with_campaign_platform_budget_with_clock<T, E: From<StoreError>>(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        now: impl FnOnce() -> DateTime<Utc>,
        action: impl FnOnce(&VerifiedCampaignPlatformBudget) -> Result<T, E>,
    ) -> Result<T, E> {
        self.with_campaign_platform_budget_impl(verified, reservation, now, action)
    }

    fn with_campaign_platform_budget_impl<T, E: From<StoreError>>(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        now: impl FnOnce() -> DateTime<Utc>,
        action: impl FnOnce(&VerifiedCampaignPlatformBudget) -> Result<T, E>,
    ) -> Result<T, E> {
        let tx = self.connection.transaction().map_err(database_error)?;
        let at = lock_budget(&tx, &self.integrity_key, verified, reservation, now)?;
        let (state, history) = load(&tx, &self.integrity_key, &reservation.family_id)?;
        let budget = budget_evidence(
            &tx,
            &self.integrity_key,
            &state,
            &history,
            verified,
            reservation,
            at,
        )?;
        let result = action(&budget);
        tx.commit().map_err(database_error)?;
        result
    }

    /// Recheck source authority and exact exclusive ownership immediately before
    /// signing/publishing. Unknown export or PG import retains the full charge.
    pub fn with_campaign_platform_export<T, E: From<StoreError>>(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        transfer: &CampaignPlatformTransferV1,
        action: impl FnOnce(&VerifiedCampaignPlatformExport) -> Result<T, E>,
    ) -> Result<T, E> {
        self.with_campaign_platform_export_impl(verified, reservation, transfer, Utc::now, action)
    }

    #[cfg(test)]
    pub(super) fn with_campaign_platform_export_with_clock<T, E: From<StoreError>>(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        transfer: &CampaignPlatformTransferV1,
        now: impl FnOnce() -> DateTime<Utc>,
        action: impl FnOnce(&VerifiedCampaignPlatformExport) -> Result<T, E>,
    ) -> Result<T, E> {
        self.with_campaign_platform_export_impl(verified, reservation, transfer, now, action)
    }

    fn with_campaign_platform_export_impl<T, E: From<StoreError>>(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        transfer: &CampaignPlatformTransferV1,
        now: impl FnOnce() -> DateTime<Utc>,
        action: impl FnOnce(&VerifiedCampaignPlatformExport) -> Result<T, E>,
    ) -> Result<T, E> {
        transfer.validate()?;
        let tx = self.connection.transaction().map_err(database_error)?;
        let at = lock_budget(&tx, &self.integrity_key, verified, reservation, now)?;
        let (state, history) = load(&tx, &self.integrity_key, &reservation.family_id)?;
        let budget = budget_evidence(
            &tx,
            &self.integrity_key,
            &state,
            &history,
            verified,
            reservation,
            at,
        )?;
        let attempt = &state.attempts[&reservation.operation_id().map_err(err)?];
        if attempt.platform_transfer.as_ref() != Some(transfer) {
            return Err(err("export differs from exclusive platform transfer").into());
        }
        let receipt = history.iter().find(|entry| matches!(
            &entry.receipt.event, CampaignLedgerEventV1::PlatformTransferred { transfer: observed } if observed == transfer
        )).ok_or_else(|| err("missing platform transfer receipt"))?;
        require_published_receipts(
            &tx,
            &self.integrity_key,
            &reservation.family_id,
            &history,
            receipt.receipt.sequence,
        )?;
        let result = action(&VerifiedCampaignPlatformExport {
            budget,
            transfer: transfer.clone(),
            transfer_receipt: receipt.clone(),
        });
        tx.commit().map_err(database_error)?;
        result
    }
}

fn lock_budget(
    tx: &Transaction<'_>,
    key: &[u8; 32],
    verified: &VerifiedCampaignRootGrant,
    reservation: &CampaignAttemptReservationV1,
    now: impl FnOnce() -> DateTime<Utc>,
) -> Result<DateTime<Utc>, StoreError> {
    let (state, _) = load(tx, key, &reservation.family_id)?;
    let root = state
        .roots
        .get(verified.content_sha256())
        .ok_or_else(|| err("missing native budget root"))?;
    serialize_approval_mutation(tx, &root.approval.approval_id)?;
    study::lock_campaign_guards(
        tx,
        key,
        verified,
        &reservation.family_id,
        verified.grant().valid_from,
        false,
    )?;
    // Sample after approval, Study and family guards. Waiting for another
    // writer cannot turn a caller's stale timestamp into current authority.
    let at = now();
    let observed = dispatch::checked_reservation(
        tx,
        key,
        verified,
        &reservation.family_id,
        &reservation.operation_id().map_err(err)?,
        at,
        true,
    )?;
    if observed != *reservation {
        return Err(err("native platform budget differs from charged operation"));
    }
    Ok(at)
}

fn budget_evidence(
    conn: &Connection,
    key: &[u8; 32],
    state: &State,
    history: &[AuthenticatedCampaignReceiptV1],
    verified: &VerifiedCampaignRootGrant,
    reservation: &CampaignAttemptReservationV1,
    at: DateTime<Utc>,
) -> Result<VerifiedCampaignPlatformBudget, StoreError> {
    let attempt = state
        .attempts
        .get(&reservation.operation_id().map_err(err)?)
        .ok_or_else(|| err("missing native budget attempt"))?;
    if attempt.dispatch.is_some() {
        return Err(err("legacy dispatch already owns native operation"));
    }
    let root = state
        .roots
        .get(verified.content_sha256())
        .ok_or_else(|| err("missing native budget root"))?;
    if root.grant.signed_grant() != verified.signed_grant() {
        return Err(err("native root signature changed"));
    }
    let approval = read_effective_approval(conn, key, &root.approval.approval_id)?;
    let mut expires = verified.grant().expires_at;
    for when in [root.revoked_at, approval.revoked_at, approval.expires_at]
        .into_iter()
        .flatten()
    {
        expires = expires.min(when);
    }
    if state.study_member_binding.is_some() {
        expires = expires.min(study::check_running_member(
            conn,
            key,
            verified,
            reservation,
            at,
        )?);
    }
    let duration = chrono::TimeDelta::try_seconds(
        i64::try_from(reservation.reserved_job_seconds).map_err(err)?,
    )
    .ok_or_else(|| err("native duration overflow"))?;
    if at
        .checked_add_signed(duration)
        .is_none_or(|end| end > expires)
    {
        return Err(err(
            "reserved Job exceeds original native authority deadline",
        ));
    }
    let root_receipt = history.iter().find(|entry| matches!(
        &entry.receipt.event, CampaignLedgerEventV1::RootRegistered { signed, .. } if signed.as_ref() == verified.signed_grant()
    )).ok_or_else(|| err("missing native root receipt"))?;
    let reservation_receipt = history.iter().find(|entry| matches!(
        &entry.receipt.event, CampaignLedgerEventV1::AttemptReserved { reservation: observed } if observed == reservation
    )).ok_or_else(|| err("missing charged native reservation receipt"))?;
    Ok(VerifiedCampaignPlatformBudget {
        root: verified.clone(),
        reservation: reservation.clone(),
        root_receipt: root_receipt.clone(),
        reservation_receipt: reservation_receipt.clone(),
        approval_sha256: root.approval_hash.clone(),
        authority_expires_at: expires,
        existing_transfer: attempt.platform_transfer.clone(),
    })
}
