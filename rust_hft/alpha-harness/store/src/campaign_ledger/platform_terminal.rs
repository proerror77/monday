//! Historical source binding for a terminal observer. Reading this value grants
//! no execution, reservation, legacy claim, settlement or budget release.
use super::*;

#[derive(Debug, Clone)]
pub struct VerifiedCampaignPlatformTerminalSource {
    root: VerifiedCampaignRootGrant,
    reservation: CampaignAttemptReservationV1,
    transfer: CampaignPlatformTransferV1,
    transfer_receipt: AuthenticatedCampaignReceiptV1,
}
impl VerifiedCampaignPlatformTerminalSource {
    pub fn root(&self) -> &VerifiedCampaignRootGrant {
        &self.root
    }
    pub fn reservation(&self) -> &CampaignAttemptReservationV1 {
        &self.reservation
    }
    pub fn transfer(&self) -> &CampaignPlatformTransferV1 {
        &self.transfer
    }
    pub fn transfer_receipt(&self) -> &AuthenticatedCampaignReceiptV1 {
        &self.transfer_receipt
    }
    pub fn operation_sha256(&self) -> Result<String, StoreError> {
        alpha_domain::canonical_json_hash(&self.transfer.operation_id).map_err(err)
    }
}
impl AlphaStore {
    /// Resolve only an existing authenticated, independently published transfer.
    /// Expiration or revocation does not erase the obligation to audit it.
    pub fn campaign_platform_terminal_source(
        &self,
        family: &str,
        operation_id: &str,
    ) -> Result<VerifiedCampaignPlatformTerminalSource, StoreError> {
        let (state, history) = load(&self.connection, &self.integrity_key, family)?;
        let attempt = state
            .attempts
            .get(operation_id)
            .ok_or_else(|| err("terminal observer has no original native operation"))?;
        if attempt.dispatch.is_some() {
            return Err(err(
                "terminal observer cannot replace a legacy execution owner",
            ));
        }
        let transfer = attempt
            .platform_transfer
            .as_ref()
            .ok_or_else(|| err("terminal observer requires exclusive platform ownership"))?;
        if transfer.operation_id != attempt.reservation.operation_id().map_err(err)? {
            return Err(err(
                "terminal transfer changed its original native operation",
            ));
        }
        let receipt = history.iter().find(|entry| matches!(
            &entry.receipt.event, CampaignLedgerEventV1::PlatformTransferred { transfer: observed } if observed == transfer
        )).ok_or_else(|| err("terminal observer has no authenticated transfer receipt"))?;
        require_published_receipts(
            &self.connection,
            &self.integrity_key,
            family,
            &history,
            receipt.receipt.sequence,
        )?;
        let root = state
            .roots
            .get(&attempt.reservation.root_grant_sha256)
            .ok_or_else(|| err("terminal observer has no original Root"))?;
        Ok(VerifiedCampaignPlatformTerminalSource {
            root: root.grant.clone(),
            reservation: attempt.reservation.clone(),
            transfer: transfer.clone(),
            transfer_receipt: receipt.clone(),
        })
    }
}
