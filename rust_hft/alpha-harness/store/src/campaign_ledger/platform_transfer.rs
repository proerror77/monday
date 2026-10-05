//! A native reservation keeps its full charge when its execution owner moves.
//! Transfer alone cannot issue a scientific grant, launch a Job or refund budget.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignPlatformTransferV1 {
    pub operation_id: String,
    pub tenant: String,
    pub run_sha256: String,
    pub request_sha256: String,
}
impl CampaignPlatformTransferV1 {
    pub(crate) fn validate(&self) -> Result<(), StoreError> {
        for hash in [&self.run_sha256, &self.request_sha256] {
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(err("invalid platform transfer identity"));
            }
        }
        if self.operation_id.is_empty()
            || self.operation_id.len() > 512
            || self.operation_id.chars().any(char::is_control)
        {
            return Err(err("invalid native operation identity"));
        }
        if self.tenant.is_empty() || self.tenant.len() > 128 || self.tenant.trim() != self.tenant {
            return Err(err("invalid platform transfer tenant"));
        }
        Ok(())
    }
}
impl AlphaStore {
    /// Atomically bars the legacy dispatcher from claiming this exact native
    /// attempt. Full native approval, cumulative debit and family/study guards
    /// remain the source of authority. A failed PG import keeps the reservation;
    /// retrying this exact transfer is idempotent and cannot allocate it again.
    pub fn transfer_campaign_execution_to_platform(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        transfer: &CampaignPlatformTransferV1,
        at: DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        transfer.validate()?;
        if transfer.operation_id != reservation.operation_id().map_err(err)? {
            return Err(err("platform transfer differs from reserved operation"));
        }
        let tx = self.connection.transaction().map_err(database_error)?;
        let (state, _) = load(&tx, &self.integrity_key, &reservation.family_id)?;
        let root = state
            .roots
            .get(verified.content_sha256())
            .ok_or_else(|| err("missing native transfer root"))?;
        serialize_approval_mutation(&tx, &root.approval.approval_id)?;
        study::lock_campaign_guards(
            &tx,
            &self.integrity_key,
            verified,
            &reservation.family_id,
            at,
            true,
        )?;
        let observed = dispatch::checked_reservation(
            &tx,
            &self.integrity_key,
            verified,
            &reservation.family_id,
            &transfer.operation_id,
            at,
            true,
        )?;
        if observed != *reservation {
            return Err(err("platform transfer changed native reservation"));
        }
        let attempt = state
            .attempts
            .get(&transfer.operation_id)
            .ok_or_else(|| err("missing native transfer attempt"))?;
        if attempt.dispatch.is_some()
            || attempt
                .platform_transfer
                .as_ref()
                .is_some_and(|old| old != transfer)
        {
            return Err(err("native execution already has a different owner"));
        }
        let receipt = append(
            &tx,
            &self.integrity_key,
            &reservation.family_id,
            CampaignLedgerEventV1::PlatformTransferred {
                transfer: transfer.clone(),
            },
            at,
        )?;
        tx.commit().map_err(database_error)?;
        Ok(receipt)
    }
}
