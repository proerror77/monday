//! One durable final-evaluation job per closed family, using the same receipt
//! publication and approval serialization as search dispatch.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignFinalDispatchClaimV1 {
    pub request_sha256: String,
    pub dispatch: CampaignDispatchClaimV1,
    pub claimed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignFinalOutcomeV1 {
    NoSelectionCandidate,
    HoldoutRejected,
    ReplayRejected,
    PromotionReady,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignFinalDispatchSettlementV1 {
    pub request_sha256: String,
    pub job_uid: String,
    pub pod_uid: String,
    pub result_sha256: String,
    pub outcome: CampaignFinalOutcomeV1,
    pub candidates_evaluated: Option<u32>,
    pub consumed_job_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct CampaignFinalDispatchRecord {
    pub grant: VerifiedCampaignFinalEvaluationGrant,
    pub claim: Option<CampaignFinalDispatchClaimV1>,
    pub settlement: Option<CampaignFinalDispatchSettlementV1>,
}

pub(super) fn validate_digest(value: &str) -> Result<(), StoreError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(err("invalid final dispatch digest"));
    }
    Ok(())
}

impl AlphaStore {
    pub fn inspect_campaign_final_authority(
        &self,
        grant: &VerifiedCampaignFinalEvaluationGrant,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        checked_final(&self.connection, &self.integrity_key, grant, at, false).map(|_| ())
    }
    /// Historical readback, including after expiry or revocation; never authority.
    pub fn campaign_final_dispatch_record(
        &self,
        family: &str,
    ) -> Result<CampaignFinalDispatchRecord, StoreError> {
        let (state, _) = load(&self.connection, &self.integrity_key, family)?;
        let closure = state
            .final_closure
            .ok_or_else(|| err("family is not closed"))?;
        Ok(CampaignFinalDispatchRecord {
            grant: closure.grant,
            claim: closure.dispatch,
            settlement: closure.settlement,
        })
    }

    /// Only the first successful claim may create a Job. A crash before creation
    /// is uncertain and cannot mint another create permission.
    pub fn claim_campaign_final_dispatch(
        &mut self,
        verified: &VerifiedCampaignFinalEvaluationGrant,
        request_sha256: &str,
        target: &CampaignDispatchTargetV1,
        at: DateTime<Utc>,
    ) -> Result<(CampaignFinalDispatchClaimV1, bool), StoreError> {
        validate_digest(request_sha256)?;
        target.validate()?;
        let tx = self.connection.transaction().map_err(database_error)?;
        let closure = checked_final(&tx, &self.integrity_key, verified, at, true)?;
        if let Some(claim) = closure.dispatch {
            if claim.request_sha256 != request_sha256 || claim.dispatch.target != *target {
                return Err(err("final dispatch request or target changed"));
            }
            return Ok((claim, false));
        }
        let receipt = append(
            &tx,
            &self.integrity_key,
            &verified.grant().family_id,
            CampaignLedgerEventV1::FinalDispatchClaimed {
                request_sha256: request_sha256.into(),
                target: target.clone(),
            },
            at,
        )?;
        tx.commit().map_err(database_error)?;
        Ok((
            CampaignFinalDispatchClaimV1 {
                request_sha256: request_sha256.into(),
                dispatch: CampaignDispatchClaimV1 {
                    target: target.clone(),
                    job_uid: None,
                    sequence: receipt.receipt.sequence,
                },
                claimed_at: at,
            },
            true,
        ))
    }

    pub fn bind_campaign_final_dispatch_job(
        &mut self,
        verified: &VerifiedCampaignFinalEvaluationGrant,
        request_sha256: &str,
        target: &CampaignDispatchTargetV1,
        job_uid: &str,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        dispatch::validate_job_uid(job_uid)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        let closure = checked_final(&tx, &self.integrity_key, verified, at, true)?;
        let claim = closure
            .dispatch
            .ok_or_else(|| err("missing final dispatch claim"))?;
        if claim.request_sha256 != request_sha256
            || claim.dispatch.target != *target
            || claim
                .dispatch
                .job_uid
                .as_ref()
                .is_some_and(|uid| uid != job_uid)
        {
            return Err(err("final dispatch request, target or Job UID changed"));
        }
        append(
            &tx,
            &self.integrity_key,
            &verified.grant().family_id,
            CampaignLedgerEventV1::FinalDispatchJobBound {
                job_uid: job_uid.into(),
            },
            at,
        )?;
        tx.commit().map_err(database_error)?;
        Ok(())
    }

    pub fn with_campaign_final_dispatch_admission<T, E: From<StoreError>>(
        &mut self,
        verified: &VerifiedCampaignFinalEvaluationGrant,
        request_sha256: &str,
        target: &CampaignDispatchTargetV1,
        expected_job_uid: Option<&str>,
        now: impl FnOnce() -> DateTime<Utc>,
        action: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let family = &verified.grant().family_id;
        let tx = self.connection.transaction().map_err(database_error)?;
        let (state, _) = load(&tx, &self.integrity_key, family)?;
        let closure = state
            .final_closure
            .ok_or_else(|| err("family is not closed"))?;
        serialize_approval_mutation(&tx, &closure.approval.approval_id)?;
        if tx
            .execute(
                "UPDATE campaign_family_heads SET sequence = sequence WHERE family_id = ?",
                params![family],
            )
            .map_err(database_error)?
            != 1
        {
            return Err(err("missing final family guard").into());
        }
        let closure = checked_final(&tx, &self.integrity_key, verified, now(), true)?;
        let claim = closure
            .dispatch
            .ok_or_else(|| err("missing final dispatch claim"))?;
        if claim.request_sha256 != request_sha256
            || claim.dispatch.target != *target
            || claim.dispatch.job_uid.as_deref() != expected_job_uid
        {
            return Err(err("final dispatch request, target or Job UID changed").into());
        }
        let result = action();
        tx.commit().map_err(database_error)?;
        result
    }

    /// Independent terminal readback may be recorded after authority expires.
    /// A failed/uncertain job consumes the only final attempt; no retry is minted.
    pub fn settle_campaign_final_dispatch(
        &mut self,
        family: &str,
        evidence: &CampaignFinalDispatchSettlementV1,
        at: DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        let tx = self.connection.transaction().map_err(database_error)?;
        let receipt = append(
            &tx,
            &self.integrity_key,
            family,
            CampaignLedgerEventV1::FinalDispatchSettled {
                evidence: evidence.clone(),
            },
            at,
        )?;
        tx.commit().map_err(database_error)?;
        Ok(receipt)
    }
}

fn checked_final(
    conn: &Connection,
    key: &[u8; 32],
    verified: &VerifiedCampaignFinalEvaluationGrant,
    at: DateTime<Utc>,
    require_publication: bool,
) -> Result<state::FinalClosure, StoreError> {
    let (state, history) = load(conn, key, &verified.grant().family_id)?;
    let closure = state
        .final_closure
        .ok_or_else(|| err("family is not closed"))?;
    if closure.grant.content_sha256() != verified.content_sha256() || closure.settlement.is_some() {
        return Err(err("final authority differs or evaluation is settled"));
    }
    verified.validate_job_deadline_at(at).map_err(err)?;
    let approval = read_effective_approval(conn, key, &closure.approval.approval_id)?;
    if !approval.is_active_at(at) || closure.revoked_at.is_some_and(|when| at >= when) {
        return Err(err("final approval is inactive"));
    }
    let duration = chrono::TimeDelta::try_seconds(
        i64::try_from(verified.grant().max_job_seconds).map_err(err)?,
    )
    .ok_or_else(|| err("final deadline overflow"))?;
    if let Some(claim) = &closure.dispatch {
        if at
            >= claim
                .claimed_at
                .checked_add_signed(duration)
                .ok_or_else(|| err("final deadline overflow"))?
        {
            return Err(err("final dispatch reservation expired"));
        }
    }
    if approval
        .revoked_at
        .or(closure.revoked_at)
        .is_some_and(|when| at.checked_add_signed(duration).is_none_or(|end| end > when))
    {
        return Err(err("final Job exceeds scheduled revocation"));
    }
    if require_publication {
        require_published_receipts(
            conn,
            key,
            &verified.grant().family_id,
            &history,
            history.len() as u64,
        )?;
    }
    Ok(closure)
}
