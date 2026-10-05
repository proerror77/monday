//! Historical source binding for a terminal observer. Reading this value grants
//! no execution, reservation, legacy claim, settlement or budget release.
use super::*;

/// Historical audit data, not a scientific result, stop authority or refund.
/// The controlled app observer assembles this only from its opaque independent
/// platform/provider/native readback. No user-facing import accepts this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignPlatformTerminalAuditV1 {
    pub schema_version: String,
    pub transfer: CampaignPlatformTransferV1,
    pub platform_state: CampaignPlatformTerminalStateV1,
    pub scientific_status: CampaignPlatformScientificStatusV1,
    /// Policy charge retained regardless of independently known consumption.
    pub charging_trials: u64,
    /// Actual original native validator output, never a platform metric.
    pub known_scientific_consumption: Option<u64>,
    pub platform_snapshot_sha256: String,
    pub observer_release_sha256: String,
    pub native_admission_sha256: String,
    pub native_trust_sha256: String,
    pub collection_sha256: String,
    pub terminal_revision: i64,
    pub terminal_event_sha256: String,
    pub execution_event_sha256: String,
    pub job_uid: String,
    pub pod_uid: String,
    pub job_sha256: String,
    pub pod_sha256: String,
    pub native_result_sha256: Option<String>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignPlatformTerminalStateV1 {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignPlatformScientificStatusV1 {
    Unknown,
    InsufficientEvidence,
    ValidatedNoCandidate,
    ValidatedSelectedPreHoldout,
}

impl CampaignPlatformTerminalAuditV1 {
    pub(super) fn validate_against(
        &self,
        reservation: &CampaignAttemptReservationV1,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.transfer.validate()?;
        if self.schema_version != "monday.campaign_platform_terminal_audit.v1"
            || self.transfer.operation_id != reservation.operation_id().map_err(err)?
            || self.charging_trials != reservation.declared_trials
            || self.terminal_revision < 1
            || self.observed_at > at
            || self
                .known_scientific_consumption
                .is_some_and(|n| n > reservation.declared_trials)
        {
            return Err(err(
                "platform audit changed original operation, full charge or observed facts",
            ));
        }
        for digest in [
            &self.platform_snapshot_sha256,
            &self.observer_release_sha256,
            &self.native_admission_sha256,
            &self.native_trust_sha256,
            &self.collection_sha256,
            &self.terminal_event_sha256,
            &self.execution_event_sha256,
            &self.job_sha256,
            &self.pod_sha256,
        ] {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(err("platform audit content identity is invalid"));
            }
        }
        if let Some(digest) = &self.native_result_sha256 {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(err("native scientific result identity is invalid"));
            }
        }
        dispatch::validate_job_uid(&self.job_uid)?;
        dispatch::validate_job_uid(&self.pod_uid)?;
        let known = matches!(
            self.scientific_status,
            CampaignPlatformScientificStatusV1::ValidatedNoCandidate
                | CampaignPlatformScientificStatusV1::ValidatedSelectedPreHoldout
        );
        if known != self.known_scientific_consumption.is_some()
            || (known
                && (self.native_result_sha256.is_none()
                    || self.platform_state != CampaignPlatformTerminalStateV1::Succeeded))
            || (self.platform_state == CampaignPlatformTerminalStateV1::Succeeded
                && self.scientific_status == CampaignPlatformScientificStatusV1::Unknown)
        {
            return Err(err(
                "platform compute state and native scientific evidence are distinct",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct VerifiedCampaignPlatformTerminalSource {
    root: VerifiedCampaignRootGrant,
    reservation: CampaignAttemptReservationV1,
    transfer: CampaignPlatformTransferV1,
    transfer_receipt: AuthenticatedCampaignReceiptV1,
    reservation_receipt: AuthenticatedCampaignReceiptV1,
    root_receipt: AuthenticatedCampaignReceiptV1,
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
    pub fn reservation_receipt(&self) -> &AuthenticatedCampaignReceiptV1 {
        &self.reservation_receipt
    }
    pub fn root_receipt(&self) -> &AuthenticatedCampaignReceiptV1 {
        &self.root_receipt
    }
    pub fn operation_sha256(&self) -> Result<String, StoreError> {
        alpha_domain::canonical_json_hash(&self.transfer.operation_id).map_err(err)
    }
}
impl AlphaStore {
    /// Append an exact full-charge mechanical audit. This cannot release budget,
    /// settle legacy science, admit retries or transfer ownership back. Actual
    /// stop/scientific authority belongs to the controlled app observer.
    pub fn record_campaign_platform_terminal_audit(
        &mut self,
        source: &VerifiedCampaignPlatformTerminalSource,
        audit: &CampaignPlatformTerminalAuditV1,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        self.record_campaign_platform_terminal_audit_with_clock(source, audit, Utc::now)
    }

    pub(super) fn record_campaign_platform_terminal_audit_with_clock(
        &mut self,
        source: &VerifiedCampaignPlatformTerminalSource,
        audit: &CampaignPlatformTerminalAuditV1,
        now: impl FnOnce() -> DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        let family = &source.reservation.family_id;
        let tx = self.connection.transaction().map_err(database_error)?;
        let (before, _) = load(&tx, &self.integrity_key, family)?;
        let root = before
            .roots
            .get(source.root.content_sha256())
            .ok_or_else(|| err("terminal audit has no original Root"))?;
        serialize_approval_mutation(&tx, &root.approval.approval_id)?;
        let study_id = study::lock_campaign_guards(
            &tx,
            &self.integrity_key,
            &source.root,
            family,
            source.root.grant().valid_from,
            false,
        )?;
        // All authority/head guards are now locked. Auditing does not test or
        // restore active scientific authority, including after expiry/revoke.
        let at = now();
        audit.validate_against(&source.reservation, at)?;
        let (state, history) = load(&tx, &self.integrity_key, family)?;
        let attempt = state
            .attempts
            .get(&source.transfer.operation_id)
            .ok_or_else(|| err("terminal audit has no existing operation"))?;
        if attempt.reservation != source.reservation
            || attempt.platform_transfer.as_ref() != Some(&source.transfer)
            || audit.transfer != source.transfer
            || attempt.dispatch.is_some()
            || attempt.settlement.is_some()
            || !history
                .iter()
                .any(|entry| entry == &source.transfer_receipt)
        {
            return Err(err(
                "terminal audit changed exclusive original execution ownership",
            ));
        }
        require_published_receipts(
            &tx,
            &self.integrity_key,
            family,
            &history,
            source.transfer_receipt.receipt.sequence,
        )?;
        let receipt = append(
            &tx,
            &self.integrity_key,
            family,
            CampaignLedgerEventV1::PlatformSettled {
                audit: Box::new(audit.clone()),
            },
            at,
        )?;
        study::append_member_platform_audit(
            &tx,
            &self.integrity_key,
            study_id.as_deref(),
            family,
            audit,
            &receipt,
            at,
        )?;
        tx.commit().map_err(database_error)?;
        Ok(receipt)
    }

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
        let reservation_receipt = history.iter().find(|entry| matches!(&entry.receipt.event,
            CampaignLedgerEventV1::AttemptReserved { reservation } if reservation == &attempt.reservation
        )).ok_or_else(|| err("terminal observer has no authenticated native reservation receipt"))?;
        let root_receipt = history.iter().find(|entry| matches!(&entry.receipt.event,
            CampaignLedgerEventV1::RootRegistered { signed, .. } if signed.as_ref() == root.grant.signed_grant()
        )).ok_or_else(|| err("terminal observer has no authenticated original Root receipt"))?;
        Ok(VerifiedCampaignPlatformTerminalSource {
            root: root.grant.clone(),
            reservation: attempt.reservation.clone(),
            transfer: transfer.clone(),
            transfer_receipt: receipt.clone(),
            reservation_receipt: reservation_receipt.clone(),
            root_receipt: root_receipt.clone(),
        })
    }
}
