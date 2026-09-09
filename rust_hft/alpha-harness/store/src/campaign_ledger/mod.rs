//! Transactional, authenticated Campaign family receipts and a replayable budget.
//!
//! One family head is updated in the same transaction as every receipt, so
//! concurrent writers conflict rather than spending the same remaining budget.
//! This is local ledger serialization, not Kubernetes writer fencing.

mod dispatch;
mod final_dispatch;
mod study;
pub use final_dispatch::{
    CampaignFinalDispatchClaimV1, CampaignFinalDispatchRecord, CampaignFinalDispatchSettlementV1,
    CampaignFinalOutcomeV1,
};
mod state;

pub use dispatch::{
    CampaignDispatchClaimV1, CampaignDispatchRecord, CampaignDispatchSettlementV1,
    CampaignDispatchTargetV1,
};
pub use study::{
    AuthenticatedCampaignStudyReceiptV1, CampaignStudyLedgerEventV1, CampaignStudyLedgerReceiptV1,
    CampaignStudyMemberHeadV1, CampaignStudySnapshotV1,
};

use super::approval_revocations::{
    insert_revocation_evidence, read_effective_approval, read_revocation_evidence,
    serialize_approval_mutation, ApprovalRevocationV1,
};
use super::{
    append_journal, authentication_tag, database_error, decode_authenticated, encoded,
    read_json_row_with_hash, verify_authentication_tag, AlphaStore, ApprovalRecord, StoreError,
};
use alpha_domain::campaign_control::{
    CampaignAttemptOutcomeV1, CampaignAttemptReservationV1, CampaignAttemptSettlementV1,
    SignedCampaignRootGrantV1, VerifiedCampaignRootGrant,
};
use alpha_domain::campaign_study::CampaignStudyMemberV1;
use chrono::{DateTime, Utc};
use duckdb::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};
use state::State;

const SCHEMA: &str = "monday.campaign_ledger_receipt.v1";
const AUTH_DOMAIN: &str = "campaign-ledger-receipt";
use alpha_domain::campaign_finalization::{
    verify_campaign_final_evaluation_grant, SignedCampaignFinalEvaluationGrantV1,
    VerifiedCampaignFinalEvaluationGrant,
};

const HEAD_DOMAIN: &str = "campaign-family-head";
const PUBLICATION_DOMAIN: &str = "campaign-receipt-publication";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CampaignLedgerEventV1 {
    RootRegistered {
        signed: Box<SignedCampaignRootGrantV1>,
        verifying_key_hex: String,
        approval: ApprovalRecord,
        approval_content_sha256: String,
    },
    StudyMemberBound {
        study_id: String,
        study_grant_sha256: String,
        member: CampaignStudyMemberV1,
    },
    AttemptReserved {
        reservation: CampaignAttemptReservationV1,
    },
    DispatchClaimed {
        operation_id: String,
        target: CampaignDispatchTargetV1,
    },
    DispatchJobBound {
        operation_id: String,
        job_uid: String,
    },
    AttemptSettled {
        settlement: CampaignAttemptSettlementV1,
    },
    DispatchSettled {
        evidence: CampaignDispatchSettlementV1,
    },
    ApprovalRevoked {
        revocation: ApprovalRevocationV1,
    },
    FinalDispatchClaimed {
        request_sha256: String,
        target: CampaignDispatchTargetV1,
    },
    FinalDispatchJobBound {
        job_uid: String,
    },
    FinalDispatchSettled {
        evidence: CampaignFinalDispatchSettlementV1,
    },
    FamilyClosedForFinalEvaluation {
        signed: Box<SignedCampaignFinalEvaluationGrantV1>,
        verifying_key_hex: String,
        approval: ApprovalRecord,
        approval_content_sha256: String,
    },
}

impl CampaignLedgerEventV1 {
    fn semantic_id(&self) -> Result<String, StoreError> {
        Ok(match self {
            Self::RootRegistered { signed, .. } => {
                format!("campaign-root:{}", signed.grant.root_id)
            }
            Self::StudyMemberBound {
                study_id, member, ..
            } => {
                format!("campaign-study-member:{study_id}:{}", member.family_id)
            }
            Self::AttemptReserved { reservation } => reservation.operation_id().map_err(err)?,
            Self::DispatchClaimed { operation_id, .. } => {
                format!("campaign-dispatch:{operation_id}")
            }
            Self::DispatchJobBound { operation_id, .. } => format!("campaign-job:{operation_id}"),
            Self::AttemptSettled { settlement } => {
                format!("campaign-settlement:{}", settlement.operation_id)
            }
            Self::DispatchSettled { evidence } => {
                format!("campaign-settlement:{}", evidence.settlement.operation_id)
            }
            Self::ApprovalRevoked { revocation } => {
                format!("campaign-revocation:{}", revocation.approval_id)
            }
            Self::FinalDispatchClaimed { .. } => "campaign-final-dispatch".into(),
            Self::FinalDispatchJobBound { .. } => "campaign-final-job".into(),
            Self::FinalDispatchSettled { .. } => "campaign-final-settlement".into(),
            Self::FamilyClosedForFinalEvaluation { signed, .. } => {
                format!("campaign-family-closed:{}", signed.grant.family_id)
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignLedgerReceiptV1 {
    pub schema_version: String,
    pub family_id: String,
    pub sequence: u64,
    pub previous_receipt_sha256: Option<String>,
    pub recorded_at: DateTime<Utc>,
    pub event: CampaignLedgerEventV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedCampaignReceiptV1 {
    pub receipt: CampaignLedgerReceiptV1,
    pub content_sha256: String,
    pub auth_tag: String,
}

impl AuthenticatedCampaignReceiptV1 {
    /// A create-once sequence key prevents competing or rolled-back writers from
    /// publishing different histories under independent content-addressed keys.
    pub fn object_key(&self) -> String {
        format!(
            "research/campaign-ledger/family-id={}/sequence={:020}/receipt.json",
            self.receipt.family_id, self.receipt.sequence
        )
    }

    pub fn publication_bytes(&self) -> Result<Vec<u8>, StoreError> {
        Ok(encoded(self)?.0.into_bytes())
    }

    pub fn object_sha256(&self) -> Result<String, StoreError> {
        Ok(encoded(self)?.1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignFamilySnapshotV1 {
    pub family_id: String,
    pub sequence: u64,
    pub last_receipt_sha256: String,
    pub head_auth_tag: String,
    pub receipts: Vec<AuthenticatedCampaignReceiptV1>,
}

fn head_json(family: &str, sequence: u64, hash: &str) -> Result<String, StoreError> {
    serde_json::to_string(&(HEAD_DOMAIN, family, sequence, hash)).map_err(err)
}

fn publication_json(receipt: &AuthenticatedCampaignReceiptV1) -> Result<String, StoreError> {
    serde_json::to_string(&(
        PUBLICATION_DOMAIN,
        receipt.object_key(),
        receipt.object_sha256()?,
    ))
    .map_err(err)
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignBudgetUsageV1 {
    pub pending_trials: u64,
    pub consumed_trials: u64,
    pub uncertain_trials: u64,
    pub job_attempts: u64,
    pub reserved_job_seconds: u64,
    pub reserved_llm_tokens: u64,
}

impl CampaignBudgetUsageV1 {
    pub fn accounted_trials(&self) -> Result<u64, StoreError> {
        add(
            add(self.pending_trials, self.consumed_trials)?,
            self.uncertain_trials,
        )
    }
}

fn err(error: impl std::fmt::Display) -> StoreError {
    StoreError::Domain(format!("Campaign ledger: {error}"))
}
fn add(a: u64, b: u64) -> Result<u64, StoreError> {
    a.checked_add(b).ok_or_else(|| err("budget overflow"))
}
fn sql_sequence(sequence: u64) -> Result<i64, StoreError> {
    i64::try_from(sequence).map_err(err)
}

fn validate_approval(
    approval: &ApprovalRecord,
    hash: &str,
    verified: &VerifiedCampaignRootGrant,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    approval.validate()?;
    let grant = verified.grant();
    if encoded(approval)?.1 != hash
        || approval.approval_class != "campaign_root"
        || approval.subject_id != grant.root_id
        || !approval.is_active_at(at)
        || approval.revoked_at.is_some()
        || approval.signer_id.as_deref() != Some(verified.signed_grant().key_id.as_str())
        || approval
            .payload
            .get("grant_sha256")
            .and_then(|v| v.as_str())
            != Some(verified.content_sha256())
        || approval.payload.get("family_id").and_then(|v| v.as_str())
            != Some(grant.family.family_id.as_str())
        || approval
            .valid_from
            .is_none_or(|from| from > grant.valid_from)
        || approval
            .expires_at
            .is_none_or(|until| until < grant.expires_at)
    {
        return Err(err("approval does not authorize this root grant"));
    }
    Ok(())
}

fn validate_final_approval(
    approval: &ApprovalRecord,
    hash: &str,
    verified: &VerifiedCampaignFinalEvaluationGrant,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    approval.validate()?;
    let grant = verified.grant();
    if encoded(approval)?.1 != hash
        || approval.approval_class != "campaign_final_evaluation"
        || approval.subject_id != grant.grant_id
        || !approval.is_active_at(at)
        || approval.revoked_at.is_some()
        || approval.signer_id.as_deref() != Some(verified.signed_grant().key_id.as_str())
        || approval
            .payload
            .get("grant_sha256")
            .and_then(|v| v.as_str())
            != Some(verified.content_sha256())
        || approval.payload.get("family_id").and_then(|v| v.as_str())
            != Some(grant.family_id.as_str())
        || approval
            .valid_from
            .is_none_or(|from| from > grant.valid_from)
        || approval
            .expires_at
            .is_none_or(|until| until < grant.expires_at)
    {
        return Err(err("approval does not authorize final evaluation"));
    }
    Ok(())
}

fn load(
    conn: &Connection,
    key: &[u8; 32],
    family: &str,
) -> Result<(State, Vec<AuthenticatedCampaignReceiptV1>), StoreError> {
    let head = conn.query_row(
        "SELECT sequence, last_receipt_sha256, auth_tag FROM campaign_family_heads WHERE family_id = ?",
        params![family],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
    );
    let head = match head {
        Ok(h) => h,
        Err(duckdb::Error::QueryReturnedNoRows) => {
            let orphan: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM campaign_ledger_receipts WHERE family_id = ?)",
                    params![family],
                    |r| r.get(0),
                )
                .map_err(database_error)?;
            if orphan {
                return Err(err("receipt history has no family head"));
            }
            return Ok((State::default(), Vec::new()));
        }
        Err(e) => return Err(database_error(e)),
    };
    verify_authentication_tag(
        key,
        HEAD_DOMAIN,
        family,
        &head_json(family, u64::try_from(head.0).map_err(err)?, &head.1)?,
        &head.2,
    )?;
    let mut stmt = conn.prepare("SELECT sequence, semantic_id, payload_json, content_hash, auth_tag FROM campaign_ledger_receipts WHERE family_id = ? ORDER BY sequence").map_err(database_error)?;
    let rows = stmt
        .query_map(params![family], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(database_error)?;
    let mut state = State::default();
    let mut receipts: Vec<AuthenticatedCampaignReceiptV1> = Vec::new();
    for row in rows {
        let (sequence, semantic, json, hash, auth) = row.map_err(database_error)?;
        let receipt: CampaignLedgerReceiptV1 =
            decode_authenticated(key, AUTH_DOMAIN, &semantic, &json, &hash, &auth)?;
        let expected = u64::try_from(receipts.len()).map_err(err)? + 1;
        if receipt.schema_version != SCHEMA
            || receipt.family_id != family
            || receipt.sequence != expected
            || sql_sequence(expected)? != sequence
            || receipt.event.semantic_id()? != semantic
            || receipt.previous_receipt_sha256.as_ref()
                != receipts.last().map(|r| &r.content_sha256)
            || receipts
                .last()
                .is_some_and(|r| r.receipt.recorded_at > receipt.recorded_at)
        {
            return Err(err("receipt identity, sequence or chain mismatch"));
        }
        state.apply(&receipt)?;
        receipts.push(AuthenticatedCampaignReceiptV1 {
            receipt,
            content_sha256: hash,
            auth_tag: auth,
        });
    }
    if head.0 != i64::try_from(receipts.len()).map_err(err)?
        || head.1
            != receipts
                .last()
                .map(|r| r.content_sha256.as_str())
                .unwrap_or("")
    {
        return Err(err("family head does not match receipt history"));
    }
    Ok((state, receipts))
}

fn insert_receipt(
    conn: &Connection,
    key: &[u8; 32],
    receipt: &AuthenticatedCampaignReceiptV1,
) -> Result<(), StoreError> {
    let r = &receipt.receipt;
    let semantic = r.event.semantic_id()?;
    let (json, hash) = encoded(r)?;
    if hash != receipt.content_sha256 {
        return Err(StoreError::ContentHashMismatch);
    }
    let _: CampaignLedgerReceiptV1 =
        decode_authenticated(key, AUTH_DOMAIN, &semantic, &json, &hash, &receipt.auth_tag)?;
    conn.execute(
        "INSERT INTO campaign_ledger_receipts VALUES (?, ?, ?, ?, ?, ?)",
        params![
            r.family_id,
            sql_sequence(r.sequence)?,
            semantic,
            json,
            hash,
            receipt.auth_tag
        ],
    )
    .map_err(database_error)?;
    let head_auth = authentication_tag(
        key,
        HEAD_DOMAIN,
        &r.family_id,
        &head_json(&r.family_id, r.sequence, &hash)?,
    )?;
    let changed = conn.execute("UPDATE campaign_family_heads SET sequence = ?, last_receipt_sha256 = ?, auth_tag = ? WHERE family_id = ? AND sequence = ? AND last_receipt_sha256 = ?", params![sql_sequence(r.sequence)?, hash, head_auth, r.family_id, sql_sequence(r.sequence - 1)?, r.previous_receipt_sha256.as_deref().unwrap_or("")]).map_err(database_error)?;
    if changed != 1 {
        return Err(err("family writer changed"));
    }
    Ok(())
}

fn append(
    conn: &Connection,
    key: &[u8; 32],
    family: &str,
    event: CampaignLedgerEventV1,
    at: DateTime<Utc>,
) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
    let (mut state, history) = load(conn, key, family)?;
    let semantic = event.semantic_id()?;
    for old in &history {
        if old.receipt.event.semantic_id()? == semantic {
            return if old.receipt.event == event {
                Ok(old.clone())
            } else {
                Err(err("same operation has conflicting receipt bytes"))
            };
        }
    }
    if history.last().is_some_and(|r| r.receipt.recorded_at > at) {
        return Err(err("receipt time moved backwards"));
    }
    let receipt = CampaignLedgerReceiptV1 {
        schema_version: SCHEMA.into(),
        family_id: family.into(),
        sequence: u64::try_from(history.len()).map_err(err)? + 1,
        previous_receipt_sha256: history.last().map(|r| r.content_sha256.clone()),
        recorded_at: at,
        event,
    };
    state.apply(&receipt)?;
    if history.is_empty() {
        conn.execute(
            "INSERT INTO campaign_family_heads VALUES (?, 0, '', ?)",
            params![
                family,
                authentication_tag(key, HEAD_DOMAIN, family, &head_json(family, 0, "")?)?
            ],
        )
        .map_err(database_error)?;
    }
    let (json, hash) = encoded(&receipt)?;
    let auth_tag = authentication_tag(key, AUTH_DOMAIN, &semantic, &json)?;
    let authenticated = AuthenticatedCampaignReceiptV1 {
        receipt,
        content_sha256: hash,
        auth_tag,
    };
    insert_receipt(conn, key, &authenticated)?;
    Ok(authenticated)
}

fn validate_snapshot(
    snapshot: &CampaignFamilySnapshotV1,
    key: &[u8; 32],
) -> Result<(), StoreError> {
    if snapshot.sequence == 0
        || snapshot.sequence != u64::try_from(snapshot.receipts.len()).map_err(err)?
    {
        return Err(err("snapshot is incomplete"));
    }
    verify_authentication_tag(
        key,
        HEAD_DOMAIN,
        &snapshot.family_id,
        &head_json(
            &snapshot.family_id,
            snapshot.sequence,
            &snapshot.last_receipt_sha256,
        )?,
        &snapshot.head_auth_tag,
    )?;
    let mut state = State::default();
    let mut previous: Option<&AuthenticatedCampaignReceiptV1> = None;
    for (index, r) in snapshot.receipts.iter().enumerate() {
        let (json, hash) = encoded(&r.receipt)?;
        if hash != r.content_sha256 {
            return Err(StoreError::ContentHashMismatch);
        }
        let _: CampaignLedgerReceiptV1 = decode_authenticated(
            key,
            AUTH_DOMAIN,
            &r.receipt.event.semantic_id()?,
            &json,
            &hash,
            &r.auth_tag,
        )?;
        if r.receipt.schema_version != SCHEMA
            || r.receipt.family_id != snapshot.family_id
            || r.receipt.sequence != u64::try_from(index).map_err(err)? + 1
            || r.receipt.previous_receipt_sha256.as_ref() != previous.map(|r| &r.content_sha256)
            || previous.is_some_and(|p| p.receipt.recorded_at > r.receipt.recorded_at)
        {
            return Err(err("snapshot receipt chain mismatch"));
        }
        state.apply(&r.receipt)?;
        previous = Some(r);
    }
    if previous.map(|r| r.content_sha256.as_str()) != Some(snapshot.last_receipt_sha256.as_str()) {
        return Err(err("snapshot head mismatch"));
    }
    Ok(())
}

fn restore_evidence_projection(
    tx: &Transaction<'_>,
    key: &[u8; 32],
    event: &CampaignLedgerEventV1,
) -> Result<(), StoreError> {
    match event {
        CampaignLedgerEventV1::RootRegistered {
            approval,
            approval_content_sha256,
            ..
        }
        | CampaignLedgerEventV1::FamilyClosedForFinalEvaluation {
            approval,
            approval_content_sha256,
            ..
        } => {
            let existing: Result<(ApprovalRecord, String), StoreError> = read_json_row_with_hash(
                tx,
                "SELECT payload_json, content_hash FROM approvals WHERE approval_id = ?",
                &approval.approval_id,
            );
            match existing {
                Ok((record, hash)) if record == *approval && hash == *approval_content_sha256 => {
                    return Ok(())
                }
                Ok(_) => return Err(err("existing approval conflicts with source receipt")),
                Err(StoreError::NotFound) => (),
                Err(e) => return Err(e),
            }
            let (json, hash) = encoded(approval)?;
            tx.execute("INSERT INTO approvals (approval_id, approval_class, subject_id, payload_json, content_hash, created_at, signer_id, valid_from, expires_at, revoked_at, revoked_by, revocation_reason) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL)", params![approval.approval_id, approval.approval_class, approval.subject_id, json, hash, approval.created_at.to_rfc3339(), approval.signer_id, approval.valid_from.map(|t| t.to_rfc3339()), approval.expires_at.map(|t| t.to_rfc3339())]).map_err(database_error)?;
            append_journal(
                tx,
                None,
                "approval_recorded",
                &approval.approval_id,
                &hash,
                approval.created_at,
            )?;
        }
        CampaignLedgerEventV1::ApprovalRevoked { revocation } => {
            insert_revocation_evidence(tx, key, revocation)?
        }
        _ => (),
    }
    Ok(())
}

impl AlphaStore {
    /// Authenticated historical grant readback, not active execution admission.
    pub fn campaign_final_evaluation_grant(
        &self,
        family: &str,
    ) -> Result<Option<SignedCampaignFinalEvaluationGrantV1>, StoreError> {
        let (state, _) = load(&self.connection, &self.integrity_key, family)?;
        Ok(state
            .final_closure
            .map(|closure| closure.grant.signed_grant().clone()))
    }

    /// Closes search against the exact settled family tail. This only appends a
    /// receipt; it neither opens evaluation data nor submits a final Job.
    pub fn close_campaign_family_for_final_evaluation(
        &mut self,
        verified: &VerifiedCampaignFinalEvaluationGrant,
        approval_id: &str,
        at: DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        verified.validate_job_deadline_at(at).map_err(err)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        serialize_approval_mutation(&tx, approval_id)?;
        let (approval, hash) = read_json_row_with_hash(
            &tx,
            "SELECT payload_json, content_hash FROM approvals WHERE approval_id = ?",
            approval_id,
        )?;
        let effective = read_effective_approval(&tx, &self.integrity_key, approval_id)?;
        if !effective.is_active_at(at) {
            return Err(err("final evaluation approval is not active"));
        }
        if let Some(when) = effective.revoked_at {
            let duration = chrono::TimeDelta::try_seconds(
                i64::try_from(verified.grant().max_job_seconds).map_err(err)?,
            )
            .ok_or_else(|| err("final deadline overflow"))?;
            if at.checked_add_signed(duration).is_none_or(|end| end > when) {
                return Err(err("final Job exceeds scheduled revocation"));
            }
        }
        validate_final_approval(&approval, &hash, verified, at)?;
        let revocation = read_revocation_evidence(&tx, &self.integrity_key, approval_id)?;
        let receipt = append(
            &tx,
            &self.integrity_key,
            &verified.grant().family_id,
            CampaignLedgerEventV1::FamilyClosedForFinalEvaluation {
                signed: Box::new(verified.signed_grant().clone()),
                verifying_key_hex: hex::encode(verified.verifying_key().as_bytes()),
                approval,
                approval_content_sha256: hash,
            },
            at,
        )?;
        if let Some(revocation) = revocation {
            append(
                &tx,
                &self.integrity_key,
                &verified.grant().family_id,
                CampaignLedgerEventV1::ApprovalRevoked { revocation },
                at,
            )?;
        }
        tx.commit().map_err(database_error)?;
        Ok(receipt)
    }

    pub fn register_campaign_root(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        approval_id: &str,
        at: DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        verified.validate_active_at(at).map_err(err)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        // Share the revocation writer's guard before reading the approval snapshot.
        // A losing writer retries the entire operation with a fresh transaction.
        serialize_approval_mutation(&tx, approval_id)?;
        let (approval, hash) = read_json_row_with_hash(
            &tx,
            "SELECT payload_json, content_hash FROM approvals WHERE approval_id = ?",
            approval_id,
        )?;
        if !read_effective_approval(&tx, &self.integrity_key, approval_id)?.is_active_at(at) {
            return Err(err("root approval is not active"));
        }
        validate_approval(&approval, &hash, verified, at)?;
        let revocation = read_revocation_evidence(&tx, &self.integrity_key, approval_id)?;
        // A family already bound to a finite study cannot acquire an
        // unlisted root later; that would create a second budget authority.
        let (family_state, _) = load(&tx, &self.integrity_key, &verified.grant().family.family_id)?;
        if !family_state.roots.contains_key(verified.content_sha256()) {
            study::reject_new_root_registration(
                &tx,
                &self.integrity_key,
                &verified.grant().family.family_id,
            )?;
        }
        let event = CampaignLedgerEventV1::RootRegistered {
            signed: Box::new(verified.signed_grant().clone()),
            verifying_key_hex: hex::encode(verified.verifying_key().as_bytes()),
            approval,
            approval_content_sha256: hash,
        };
        let receipt = append(
            &tx,
            &self.integrity_key,
            &verified.grant().family.family_id,
            event,
            at,
        )?;
        if let Some(revocation) = revocation {
            append(
                &tx,
                &self.integrity_key,
                &verified.grant().family.family_id,
                CampaignLedgerEventV1::ApprovalRevoked { revocation },
                at,
            )?;
        }
        tx.commit().map_err(database_error)?;
        Ok(receipt)
    }

    pub fn reserve_campaign_attempt(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        at: DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        verified
            .validate_attempt_scope(reservation, at)
            .map_err(err)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        let (state, _) = load(&tx, &self.integrity_key, &reservation.family_id)?;
        let root = state
            .roots
            .get(verified.content_sha256())
            .ok_or_else(|| err("root is not registered"))?;
        let approval_id = root.approval.approval_id.clone();
        serialize_approval_mutation(&tx, &approval_id)?;
        if !read_effective_approval(&tx, &self.integrity_key, &root.approval.approval_id)?
            .is_active_at(at)
        {
            return Err(err("root approval is not active"));
        }
        let (study_id, _duplicate) =
            study::prepare_member_reservation(&tx, &self.integrity_key, verified, reservation, at)?;
        let receipt = append(
            &tx,
            &self.integrity_key,
            &reservation.family_id,
            CampaignLedgerEventV1::AttemptReserved {
                reservation: reservation.clone(),
            },
            at,
        )?;
        study::append_member_reservation(
            &tx,
            &self.integrity_key,
            study_id.as_deref(),
            reservation,
            &receipt,
            at,
        )?;
        tx.commit().map_err(database_error)?;
        Ok(receipt)
    }

    /// Called only after the controller has independently verified terminal
    /// provenance and result bytes. Settlement can record evidence after expiry
    /// or revocation; it never authorizes a new attempt by itself.
    pub fn settle_campaign_attempt(
        &mut self,
        family: &str,
        settlement: &CampaignAttemptSettlementV1,
        at: DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        let tx = self.connection.transaction().map_err(database_error)?;
        let study_id = study::lock_member_settlement_guards(&tx, &self.integrity_key, family, at)?;
        let prepared_study_id =
            study::prepare_member_settlement(&tx, &self.integrity_key, family, settlement, at)?;
        if study_id != prepared_study_id {
            return Err(err("study settlement membership changed"));
        }
        let receipt = append(
            &tx,
            &self.integrity_key,
            family,
            CampaignLedgerEventV1::AttemptSettled {
                settlement: settlement.clone(),
            },
            at,
        )?;
        study::append_member_settlement(
            &tx,
            &self.integrity_key,
            study_id.as_deref(),
            family,
            settlement,
            &receipt,
            at,
        )?;
        tx.commit().map_err(database_error)?;
        Ok(receipt)
    }

    pub fn campaign_family_usage(&self, family: &str) -> Result<CampaignBudgetUsageV1, StoreError> {
        let (state, _) = load(&self.connection, &self.integrity_key, family)?;
        state.usage(None)
    }

    pub fn campaign_family_receipts(
        &self,
        family: &str,
    ) -> Result<Vec<AuthenticatedCampaignReceiptV1>, StoreError> {
        Ok(load(&self.connection, &self.integrity_key, family)?.1)
    }

    pub fn campaign_root_usage(
        &self,
        family: &str,
        grant_sha256: &str,
    ) -> Result<CampaignBudgetUsageV1, StoreError> {
        let (state, _) = load(&self.connection, &self.integrity_key, family)?;
        if !state.roots.contains_key(grant_sha256) {
            return Err(StoreError::NotFound);
        }
        state.usage(Some(grant_sha256))
    }

    pub fn campaign_family_snapshot(
        &self,
        family: &str,
    ) -> Result<CampaignFamilySnapshotV1, StoreError> {
        let receipts = self.campaign_family_receipts(family)?;
        let (sequence, last, auth) = self.connection.query_row("SELECT sequence, last_receipt_sha256, auth_tag FROM campaign_family_heads WHERE family_id = ?", params![family], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))).map_err(database_error)?;
        let snapshot = CampaignFamilySnapshotV1 {
            family_id: family.into(),
            sequence: u64::try_from(sequence).map_err(err)?,
            last_receipt_sha256: last,
            head_auth_tag: auth,
            receipts,
        };
        validate_snapshot(&snapshot, &self.integrity_key)?;
        Ok(snapshot)
    }

    /// Rebuild from the complete authenticated receipt set, including approvals
    /// and revocations. Restore the original integrity key separately first.
    /// Freshness against OSS is the controller's responsibility. Publication
    /// acknowledgements are deliberately not imported or inferred from a backup.
    pub fn import_campaign_family_snapshot(
        &mut self,
        snapshot: &CampaignFamilySnapshotV1,
    ) -> Result<(), StoreError> {
        validate_snapshot(snapshot, &self.integrity_key)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        study::reject_family_only_restore(&tx, &self.integrity_key, snapshot)?;
        let (mut state, old) = load(&tx, &self.integrity_key, &snapshot.family_id)?;
        if old.len() > snapshot.receipts.len() {
            return Err(err("stale snapshot would omit existing history"));
        }
        for (index, r) in snapshot.receipts.iter().enumerate() {
            if let Some(existing) = old.get(index) {
                if existing != r {
                    return Err(err("snapshot conflicts with existing history"));
                }
            } else {
                state.apply(&r.receipt)?;
                if index == 0 {
                    let auth = authentication_tag(
                        &self.integrity_key,
                        HEAD_DOMAIN,
                        &snapshot.family_id,
                        &head_json(&snapshot.family_id, 0, "")?,
                    )?;
                    tx.execute(
                        "INSERT INTO campaign_family_heads VALUES (?, 0, '', ?)",
                        params![snapshot.family_id, auth],
                    )
                    .map_err(database_error)?;
                }
                insert_receipt(&tx, &self.integrity_key, r)?;
            }
            restore_evidence_projection(&tx, &self.integrity_key, &r.receipt.event)?;
        }
        tx.commit().map_err(database_error)?;
        Ok(())
    }

    pub fn pending_campaign_receipts(
        &self,
        family: &str,
    ) -> Result<Vec<AuthenticatedCampaignReceiptV1>, StoreError> {
        let receipts = self.campaign_family_receipts(family)?;
        let mut pending = Vec::new();
        for r in receipts {
            let ack = self.connection.query_row("SELECT object_sha256, auth_tag FROM campaign_receipt_publications WHERE family_id = ? AND sequence = ?", params![family, sql_sequence(r.receipt.sequence)?], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)));
            match ack {
                Err(duckdb::Error::QueryReturnedNoRows) => pending.push(r),
                Err(e) => return Err(database_error(e)),
                Ok((hash, auth)) => {
                    if hash != r.object_sha256()? {
                        return Err(StoreError::ContentHashMismatch);
                    }
                    verify_authentication_tag(
                        &self.integrity_key,
                        PUBLICATION_DOMAIN,
                        &r.object_key(),
                        &publication_json(&r)?,
                        &auth,
                    )?;
                }
            }
        }
        Ok(pending)
    }

    /// The caller supplies a SHA observed by an authenticated independent OSS
    /// GET. This method verifies equality, not remote reachability or IAM policy.
    pub fn acknowledge_campaign_receipt_readback(
        &mut self,
        family: &str,
        sequence: u64,
        observed_object_key: &str,
        observed_sha256: &str,
    ) -> Result<(), StoreError> {
        let history = self.campaign_family_receipts(family)?;
        let receipt = sequence
            .checked_sub(1)
            .and_then(|n| usize::try_from(n).ok())
            .and_then(|n| history.get(n))
            .ok_or_else(|| err("unknown receipt sequence"))?;
        if receipt.object_sha256()? != observed_sha256
            || receipt.object_key() != observed_object_key
        {
            return Err(StoreError::ContentHashMismatch);
        }
        self.connection
            .execute(
                "INSERT INTO campaign_receipt_publications VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
                params![family, sql_sequence(sequence)?, observed_sha256, authentication_tag(&self.integrity_key, PUBLICATION_DOMAIN, &receipt.object_key(), &publication_json(receipt)?)?],
            )
            .map_err(database_error)?;
        let (stored, auth): (String, String) = self.connection.query_row("SELECT object_sha256, auth_tag FROM campaign_receipt_publications WHERE family_id = ? AND sequence = ?", params![family, sql_sequence(sequence)?], |r| Ok((r.get(0)?, r.get(1)?))).map_err(database_error)?;
        if stored != observed_sha256 {
            return Err(StoreError::ContentHashMismatch);
        }
        verify_authentication_tag(
            &self.integrity_key,
            PUBLICATION_DOMAIN,
            &receipt.object_key(),
            &publication_json(receipt)?,
            &auth,
        )?;
        Ok(())
    }

    pub fn check_campaign_dispatch_admission(
        &self,
        verified: &VerifiedCampaignRootGrant,
        family: &str,
        operation_id: &str,
        at: DateTime<Utc>,
    ) -> Result<CampaignAttemptReservationV1, StoreError> {
        dispatch::checked_reservation(
            &self.connection,
            &self.integrity_key,
            verified,
            family,
            operation_id,
            at,
            true,
        )
    }
}

pub(crate) fn append_registered_campaign_revocation(
    conn: &Connection,
    key: &[u8; 32],
    approval: &ApprovalRecord,
    event: &ApprovalRevocationV1,
) -> Result<(), StoreError> {
    if approval.approval_class == "campaign_study" {
        study::append_registered_study_revocation(conn, key, approval, event)?;
        return Ok(());
    }
    if !matches!(
        approval.approval_class.as_str(),
        "campaign_root" | "campaign_final_evaluation"
    ) {
        return Ok(());
    }
    let Some(family) = approval.payload.get("family_id").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    let (state, history) = load(conn, key, family)?;
    if state
        .roots
        .values()
        .any(|root| root.approval.approval_id == approval.approval_id)
        || state
            .final_closure
            .as_ref()
            .is_some_and(|closure| closure.approval.approval_id == approval.approval_id)
    {
        // The receipt is recorded no earlier than the chain tail so a revocation
        // is never rejected for ordering; the event itself keeps `revoked_at`.
        let at = history.last().map_or(event.revoked_at, |r| {
            r.receipt.recorded_at.max(event.revoked_at)
        });
        append(
            conn,
            key,
            family,
            CampaignLedgerEventV1::ApprovalRevoked {
                revocation: event.clone(),
            },
            at,
        )?;
    }
    Ok(())
}

fn require_published_receipts(
    conn: &Connection,
    key: &[u8; 32],
    family: &str,
    history: &[AuthenticatedCampaignReceiptV1],
    through: u64,
) -> Result<(), StoreError> {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::campaign_control::{
        sign_campaign_root_grant, verify_campaign_root_grant, CampaignEvaluationViewsV1,
        CampaignExecutionBindingV1, CampaignExecutionScope, CampaignFamilyPolicyV1,
        CampaignRootBudgetV1, CampaignRootGrantV1, CampaignSelectionFeedbackV1, ATTEMPT_SCHEMA,
        ROOT_GRANT_SCHEMA,
    };
    use chrono::TimeDelta;
    use ed25519_dalek::SigningKey;
    use std::collections::{BTreeMap, BTreeSet};

    const FAMILY: &str = "study-1";
    const APPROVAL: &str = "approval-1";

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-05T00:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn minutes(count: i64) -> DateTime<Utc> {
        t0() + TimeDelta::minutes(count)
    }

    fn grant(root_id: &str) -> CampaignRootGrantV1 {
        CampaignRootGrantV1 {
            schema_version: ROOT_GRANT_SCHEMA.into(),
            root_id: root_id.into(),
            family: CampaignFamilyPolicyV1 {
                family_id: FAMILY.into(),
                definition_sha256: "1".repeat(64),
                max_trials: 200,
            },
            execution_scope: CampaignExecutionScope::PreHoldout,
            execution: CampaignExecutionBindingV1 {
                campaign_inputs_sha256: "2".repeat(64),
                evaluation_protocol_sha256: "3".repeat(64),
                evaluation_views: CampaignEvaluationViewsV1 {
                    search_view_sha256: "b".repeat(64),
                    selection_view_sha256: "b".repeat(64),
                    selection_feedback:
                        CampaignSelectionFeedbackV1::SearchAndLearningVisibleWalkForward,
                },
                source_revision: "a".repeat(40),
                runner_image: format!("registry/research@sha256:{}", "4".repeat(64)),
                controller_image: format!("registry/controller@sha256:{}", "5".repeat(64)),
                job_cpu_millis: 3500,
                job_memory_mib: 12288,
            },
            allowed_policy_revision_ids: BTreeSet::from([format!(
                "cex-search-policy-{}",
                "6".repeat(64)
            )]),
            max_follow_ups: 1,
            budget: CampaignRootBudgetV1 {
                max_trials: 100,
                max_job_attempts: 2,
                max_job_seconds: 14400,
                max_llm_tokens: 3000,
            },
            valid_from: t0(),
            expires_at: t0() + TimeDelta::hours(12),
        }
    }

    fn verify(grant: CampaignRootGrantV1) -> VerifiedCampaignRootGrant {
        let key = SigningKey::from_bytes(&[7; 32]);
        let signed = sign_campaign_root_grant(grant, "operator".into(), &key).unwrap();
        verify_campaign_root_grant(
            &signed,
            &BTreeMap::from([("operator".into(), key.verifying_key())]),
            t0(),
        )
        .unwrap()
    }

    fn approval(verified: &VerifiedCampaignRootGrant, approval_id: &str) -> ApprovalRecord {
        let grant = verified.grant();
        ApprovalRecord {
            approval_id: approval_id.into(),
            approval_class: "campaign_root".into(),
            subject_id: grant.root_id.clone(),
            payload: serde_json::json!({
                "grant_sha256": verified.content_sha256(),
                "family_id": grant.family.family_id,
            }),
            signer_id: Some("operator".into()),
            valid_from: Some(grant.valid_from),
            expires_at: Some(grant.expires_at),
            revoked_at: None,
            revoked_by: None,
            revocation_reason: None,
            created_at: grant.valid_from,
        }
    }

    fn reservation(
        verified: &VerifiedCampaignRootGrant,
        ordinal: u32,
        declared_trials: u64,
    ) -> CampaignAttemptReservationV1 {
        CampaignAttemptReservationV1 {
            schema_version: ATTEMPT_SCHEMA.into(),
            root_grant_sha256: verified.content_sha256().into(),
            family_id: FAMILY.into(),
            campaign_id: "campaign-0".into(),
            execution: verified.grant().execution.clone(),
            generation: 0,
            parent_result_sha256: None,
            policy_revision_id: verified
                .grant()
                .allowed_policy_revision_ids
                .first()
                .unwrap()
                .clone(),
            request_sha256: "8".repeat(64),
            attempt_ordinal: ordinal,
            declared_trials,
            reserved_job_seconds: 7200,
            reserved_llm_tokens: 1500,
        }
    }

    fn settlement(
        reservation: &CampaignAttemptReservationV1,
        outcome: CampaignAttemptOutcomeV1,
        consumed_trials: Option<u64>,
    ) -> CampaignAttemptSettlementV1 {
        CampaignAttemptSettlementV1 {
            operation_id: reservation.operation_id().unwrap(),
            reservation_sha256: reservation.content_hash().unwrap(),
            evidence_sha256: "d".repeat(64),
            outcome,
            consumed_trials,
        }
    }

    fn registered() -> (AlphaStore, VerifiedCampaignRootGrant) {
        registered_grant(grant("root-1"))
    }

    fn registered_grant(
        definition: CampaignRootGrantV1,
    ) -> (AlphaStore, VerifiedCampaignRootGrant) {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let verified = verify(definition);
        store
            .record_approval(&approval(&verified, APPROVAL))
            .unwrap();
        store
            .register_campaign_root(&verified, APPROVAL, t0())
            .unwrap();
        (store, verified)
    }

    #[test]
    fn root_registration_conflicts_with_inflight_approval_revocation() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let verified = verify(grant("root-1"));
        store
            .record_approval(&approval(&verified, APPROVAL))
            .unwrap();
        let mut other = store.connection.try_clone().unwrap();
        // Establish the shared guard before either writer starts its transaction.
        let tx = other.transaction().unwrap();
        super::super::approval_revocations::serialize_approval_mutation(&tx, APPROVAL).unwrap();
        tx.commit().unwrap();
        let tx = other.transaction().unwrap();
        super::super::approval_revocations::serialize_approval_mutation(&tx, APPROVAL).unwrap();
        assert!(
            store
                .register_campaign_root(&verified, APPROVAL, t0())
                .is_err(),
            "registration must not commit while revocation owns the approval guard"
        );
        tx.rollback().unwrap();
        assert!(store.campaign_family_receipts(FAMILY).unwrap().is_empty());
        store
            .register_campaign_root(&verified, APPROVAL, t0())
            .unwrap();
    }

    #[test]
    fn reservation_conflicts_with_inflight_approval_revocation() {
        let (mut store, verified) = registered();
        let reservation = reservation(&verified, 0, 40);
        let mut other = store.connection.try_clone().unwrap();
        let tx = other.transaction().unwrap();
        serialize_approval_mutation(&tx, APPROVAL).unwrap();
        assert!(
            store
                .reserve_campaign_attempt(&verified, &reservation, t0())
                .is_err(),
            "reservation must not commit while revocation owns the approval guard"
        );
        tx.rollback().unwrap();
        assert_eq!(store.campaign_family_usage(FAMILY).unwrap().job_attempts, 0);
        store
            .reserve_campaign_attempt(&verified, &reservation, t0())
            .unwrap();
    }

    fn acknowledge_all(store: &mut AlphaStore) {
        for receipt in store.campaign_family_receipts(FAMILY).unwrap() {
            store
                .acknowledge_campaign_receipt_readback(
                    FAMILY,
                    receipt.receipt.sequence,
                    &receipt.object_key(),
                    &receipt.object_sha256().unwrap(),
                )
                .unwrap();
        }
    }

    fn dispatch_target() -> CampaignDispatchTargetV1 {
        CampaignDispatchTargetV1 {
            context: "research-context".into(),
            namespace: "monday-research".into(),
            job_name: "campaign-job".into(),
            manifest_sha256: "a".repeat(64),
        }
    }

    fn claimed() -> (
        AlphaStore,
        VerifiedCampaignRootGrant,
        CampaignAttemptReservationV1,
    ) {
        let (mut store, verified) = registered();
        let attempt = reservation(&verified, 0, 40);
        store
            .reserve_campaign_attempt(&verified, &attempt, t0())
            .unwrap();
        acknowledge_all(&mut store);
        let (_, first) = store
            .claim_campaign_dispatch(&verified, &attempt, &dispatch_target(), t0())
            .unwrap();
        assert!(first);
        acknowledge_all(&mut store);
        (store, verified, attempt)
    }

    #[test]
    fn dispatch_claim_and_job_uid_survive_replay_without_a_second_create_or_charge() {
        let (mut store, verified, attempt) = claimed();
        store
            .bind_campaign_dispatch_job(&verified, &attempt, &dispatch_target(), "job-uid-1", t0())
            .unwrap();
        acknowledge_all(&mut store);
        let expected = store.campaign_family_snapshot(FAMILY).unwrap();
        let mut restored = AlphaStore::open_in_memory().unwrap();
        // Same integrity key represents restoring this owner's durable backup.
        restored.integrity_key = store.integrity_key;
        restored.import_campaign_family_snapshot(&expected).unwrap();
        acknowledge_all(&mut restored);
        let (claim, first) = restored
            .claim_campaign_dispatch(&verified, &attempt, &dispatch_target(), minutes(1))
            .unwrap();
        assert!(
            !first,
            "a restored claim must only adopt, never create again"
        );
        assert_eq!(claim.job_uid.as_deref(), Some("job-uid-1"));
        assert_eq!(
            restored.campaign_family_usage(FAMILY).unwrap().job_attempts,
            1
        );
        assert_eq!(restored.campaign_family_snapshot(FAMILY).unwrap(), expected);
        let mut changed = dispatch_target();
        for field in ["context", "namespace", "job_name", "manifest"] {
            match field {
                "context" => changed.context = "other".into(),
                "namespace" => changed.namespace = "other".into(),
                "job_name" => changed.job_name = "other".into(),
                _ => changed.manifest_sha256 = "b".repeat(64),
            }
            assert!(restored
                .claim_campaign_dispatch(&verified, &attempt, &changed, minutes(1))
                .is_err());
            changed = dispatch_target();
        }
        assert!(restored
            .bind_campaign_dispatch_job(
                &verified,
                &attempt,
                &dispatch_target(),
                "replacement",
                minutes(1)
            )
            .is_err());
    }

    #[test]
    fn dispatch_guard_requires_claim_and_binding_receipts_and_exact_reserved_request() {
        let (mut store, verified, attempt) = claimed();
        store
            .bind_campaign_dispatch_job(&verified, &attempt, &dispatch_target(), "job-uid-1", t0())
            .unwrap();
        let mut called = false;
        let result: Result<(), StoreError> = store.with_campaign_dispatch_admission(
            &verified,
            &attempt,
            &dispatch_target(),
            Some("job-uid-1"),
            t0,
            || {
                called = true;
                Ok(())
            },
        );
        assert!(
            result.is_err(),
            "unpublished Job binding must block release"
        );
        assert!(!called);
        acknowledge_all(&mut store);
        let mut changed = attempt.clone();
        changed.request_sha256 = "f".repeat(64);
        let result: Result<(), StoreError> = store.with_campaign_dispatch_admission(
            &verified,
            &changed,
            &dispatch_target(),
            Some("job-uid-1"),
            t0,
            || {
                called = true;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!called);
        let result: Result<(), StoreError> = store.with_campaign_dispatch_admission(
            &verified,
            &attempt,
            &dispatch_target(),
            Some("replacement"),
            t0,
            || {
                called = true;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!called);
        let result: Result<(), StoreError> = store.with_campaign_dispatch_admission(
            &verified,
            &attempt,
            &dispatch_target(),
            Some("job-uid-1"),
            t0,
            || {
                called = true;
                Ok(())
            },
        );
        result.unwrap();
        assert!(called);
    }

    #[test]
    fn dispatch_guard_serializes_revocation_and_settlement_before_the_external_action() {
        let (mut store, verified, attempt) = claimed();
        let mut other = store.connection.try_clone().unwrap();
        for guard in ["approval", "family"] {
            let tx = other.transaction().unwrap();
            if guard == "approval" {
                serialize_approval_mutation(&tx, APPROVAL).unwrap();
            } else {
                tx.execute(
                    "UPDATE campaign_family_heads SET sequence = sequence WHERE family_id = ?",
                    params![FAMILY],
                )
                .unwrap();
            }
            let mut called = false;
            let result: Result<(), StoreError> = store.with_campaign_dispatch_admission(
                &verified,
                &attempt,
                &dispatch_target(),
                None,
                t0,
                || {
                    called = true;
                    Ok(())
                },
            );
            assert!(result.is_err(), "must conflict with the {guard} writer");
            assert!(!called);
            tx.rollback().unwrap();
        }
        let result: Result<(), StoreError> = store.with_campaign_dispatch_admission(
            &verified,
            &attempt,
            &dispatch_target(),
            None,
            t0,
            || {
                let tx = other.transaction().unwrap();
                assert!(serialize_approval_mutation(&tx, APPROVAL).is_err());
                tx.rollback().unwrap();
                let tx = other.transaction().unwrap();
                assert!(tx
                    .execute(
                        "UPDATE campaign_family_heads SET sequence = sequence WHERE family_id = ?",
                        params![FAMILY]
                    )
                    .is_err());
                tx.rollback().unwrap();
                Ok(())
            },
        );
        result.unwrap();
    }

    #[test]
    fn dispatch_guard_rechecks_deadline_revocation_and_settlement_and_preserves_uncertain_budget() {
        let (mut store, verified, attempt) = claimed();
        let mut called = false;
        let result: Result<(), StoreError> = store.with_campaign_dispatch_admission(
            &verified,
            &attempt,
            &dispatch_target(),
            None,
            || minutes(601),
            || {
                called = true;
                Ok(())
            },
        );
        assert!(
            result.is_err(),
            "full Job deadline must still fit in the grant"
        );
        assert!(!called);
        let result: Result<(), StoreError> = store.with_campaign_dispatch_admission(
            &verified,
            &attempt,
            &dispatch_target(),
            None,
            t0,
            || Err(err("network response was lost")),
        );
        assert!(result.is_err());
        assert_eq!(
            store.campaign_family_usage(FAMILY).unwrap().pending_trials,
            40
        );
        assert!(
            !store
                .claim_campaign_dispatch(&verified, &attempt, &dispatch_target(), minutes(1))
                .unwrap()
                .1
        );
        store
            .revoke_approval(APPROVAL, "operator", "cancel", minutes(1))
            .unwrap();
        let result: Result<(), StoreError> = store.with_campaign_dispatch_admission(
            &verified,
            &attempt,
            &dispatch_target(),
            None,
            || minutes(1),
            || {
                called = true;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!called);
        let (mut store, verified, attempt) = claimed();
        store
            .settle_campaign_attempt(
                FAMILY,
                &settlement(&attempt, CampaignAttemptOutcomeV1::Failed, None),
                minutes(1),
            )
            .unwrap();
        let result: Result<(), StoreError> = store.with_campaign_dispatch_admission(
            &verified,
            &attempt,
            &dispatch_target(),
            None,
            || minutes(1),
            || {
                called = true;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!called);
        assert_eq!(
            store
                .campaign_family_usage(FAMILY)
                .unwrap()
                .uncertain_trials,
            40
        );
    }

    #[test]
    fn dispatch_terminal_receipt_survives_revocation_replay_and_retransmission() {
        let (mut store, verified, attempt) = claimed();
        store
            .bind_campaign_dispatch_job(&verified, &attempt, &dispatch_target(), "job-uid-1", t0())
            .unwrap();
        acknowledge_all(&mut store);
        let evidence = CampaignDispatchSettlementV1 {
            job_uid: "job-uid-1".into(),
            pod_uid: "pod-uid-1".into(),
            settlement: settlement(&attempt, CampaignAttemptOutcomeV1::NoCandidate, Some(31)),
        };
        let mut changed = evidence.clone();
        changed.job_uid = "replacement".into();
        assert!(store
            .settle_campaign_dispatch(&attempt, &changed, minutes(1))
            .is_err());
        store
            .revoke_approval(APPROVAL, "operator", "cancel", minutes(1))
            .unwrap();
        // Expired/revoked authority cannot dispatch, but cannot prevent honest
        // accounting of an already executed Job after its terminal readback.
        let receipt = store
            .settle_campaign_dispatch(&attempt, &evidence, minutes(800))
            .unwrap();
        let record = store
            .campaign_dispatch_record(FAMILY, &attempt.operation_id().unwrap())
            .unwrap();
        assert_eq!(record.terminal_pod_uid.as_deref(), Some("pod-uid-1"));
        assert_eq!(
            record.settlement.as_ref().unwrap().evidence_sha256,
            evidence.settlement.evidence_sha256
        );
        assert_eq!(
            store.campaign_family_usage(FAMILY).unwrap().consumed_trials,
            31
        );
        assert_eq!(
            store
                .settle_campaign_dispatch(&attempt, &evidence, minutes(801))
                .unwrap(),
            receipt
        );
        changed = evidence.clone();
        changed.settlement.consumed_trials = Some(30);
        assert!(store
            .settle_campaign_dispatch(&attempt, &changed, minutes(801))
            .is_err());
        let snapshot = store.campaign_family_snapshot(FAMILY).unwrap();
        let mut restored = AlphaStore::open_in_memory().unwrap();
        restored.integrity_key = store.integrity_key;
        restored.import_campaign_family_snapshot(&snapshot).unwrap();
        assert_eq!(
            restored
                .campaign_dispatch_record(FAMILY, &attempt.operation_id().unwrap())
                .unwrap()
                .terminal_pod_uid,
            Some("pod-uid-1".into())
        );
    }

    #[test]
    fn dispatch_settlement_binds_child_admission_to_the_actual_parent_and_published_chain() {
        let mut approved = grant("root-1");
        let child_policy = format!("cex-search-policy-{}", "7".repeat(64));
        approved
            .allowed_policy_revision_ids
            .insert(child_policy.clone());
        let verified = verify(approved);
        let mut store = AlphaStore::open_in_memory().unwrap();
        store
            .record_approval(&approval(&verified, APPROVAL))
            .unwrap();
        store
            .register_campaign_root(&verified, APPROVAL, t0())
            .unwrap();
        let parent = reservation(&verified, 0, 40);
        store
            .reserve_campaign_attempt(&verified, &parent, t0())
            .unwrap();
        acknowledge_all(&mut store);
        store
            .claim_campaign_dispatch(&verified, &parent, &dispatch_target(), t0())
            .unwrap();
        acknowledge_all(&mut store);
        store
            .bind_campaign_dispatch_job(&verified, &parent, &dispatch_target(), "job-uid-1", t0())
            .unwrap();
        acknowledge_all(&mut store);
        let terminal = CampaignDispatchSettlementV1 {
            job_uid: "job-uid-1".into(),
            pod_uid: "pod-uid-1".into(),
            settlement: settlement(&parent, CampaignAttemptOutcomeV1::NoCandidate, Some(31)),
        };
        store
            .settle_campaign_dispatch(&parent, &terminal, minutes(1))
            .unwrap();
        let mut child = parent.clone();
        child.generation = 1;
        child.campaign_id = "child".into();
        child.policy_revision_id = child_policy;
        child.request_sha256 = "9".repeat(64);
        child.parent_result_sha256 = Some("a".repeat(64));
        assert!(store
            .reserve_campaign_attempt(&verified, &child, minutes(2))
            .is_err());
        child.parent_result_sha256 = Some(terminal.settlement.evidence_sha256.clone());
        store
            .reserve_campaign_attempt(&verified, &child, minutes(2))
            .unwrap();
        assert!(store.check_campaign_dispatch_admission(&verified, FAMILY, &child.operation_id().unwrap(), minutes(2)).is_err(),
            "parent terminal and child reservation receipts must be read back before child execution");
        acknowledge_all(&mut store);
        assert_eq!(
            store
                .check_campaign_dispatch_admission(
                    &verified,
                    FAMILY,
                    &child.operation_id().unwrap(),
                    minutes(2)
                )
                .unwrap(),
            child
        );
        let usage = store.campaign_family_usage(FAMILY).unwrap();
        assert_eq!(
            (
                usage.consumed_trials,
                usage.pending_trials,
                usage.job_attempts
            ),
            (31, 40, 2)
        );
    }

    fn final_family_base() -> (
        AlphaStore,
        VerifiedCampaignRootGrant,
        CampaignAttemptReservationV1,
    ) {
        let mut definition = grant("root-1");
        definition.execution.evaluation_views.selection_feedback =
            CampaignSelectionFeedbackV1::IndependentSelectionWithheld;
        definition.execution.evaluation_views.selection_view_sha256 = "c".repeat(64);
        let (mut store, root) = registered_grant(definition);
        let attempt = reservation(&root, 0, 40);
        store
            .reserve_campaign_attempt(&root, &attempt, t0())
            .unwrap();
        acknowledge_all(&mut store);
        store
            .claim_campaign_dispatch(&root, &attempt, &dispatch_target(), t0())
            .unwrap();
        acknowledge_all(&mut store);
        store
            .bind_campaign_dispatch_job(
                &root,
                &attempt,
                &dispatch_target(),
                "final-source-job",
                t0(),
            )
            .unwrap();
        (store, root, attempt)
    }

    fn settle_final_source(store: &mut AlphaStore, attempt: &CampaignAttemptReservationV1) {
        store
            .settle_campaign_dispatch(
                attempt,
                &CampaignDispatchSettlementV1 {
                    job_uid: "final-source-job".into(),
                    pod_uid: "final-source-pod".into(),
                    settlement: settlement(
                        attempt,
                        CampaignAttemptOutcomeV1::SelectedPreHoldout,
                        Some(40),
                    ),
                },
                minutes(1),
            )
            .unwrap();
        acknowledge_all(store);
    }

    fn final_definition(
        store: &AlphaStore,
        root: &VerifiedCampaignRootGrant,
        attempt: &CampaignAttemptReservationV1,
    ) -> alpha_domain::campaign_finalization::CampaignFinalEvaluationGrantV1 {
        alpha_domain::campaign_finalization::CampaignFinalEvaluationGrantV1 {
            schema_version: alpha_domain::campaign_finalization::FINAL_EVALUATION_GRANT_SCHEMA
                .into(),
            grant_id: "final-grant-1".into(),
            family_id: FAMILY.into(),
            family_definition_sha256: root.grant().family.definition_sha256.clone(),
            family_head_sha256: store
                .campaign_family_snapshot(FAMILY)
                .unwrap()
                .last_receipt_sha256,
            execution: root.grant().execution.clone(),
            selected_results: BTreeMap::from([(attempt.operation_id().unwrap(), "d".repeat(64))]),
            max_candidates: 3,
            max_job_seconds: 3600,
            valid_from: t0(),
            expires_at: minutes(720),
        }
    }

    fn approve_final(
        store: &mut AlphaStore,
        definition: alpha_domain::campaign_finalization::CampaignFinalEvaluationGrantV1,
    ) -> VerifiedCampaignFinalEvaluationGrant {
        use alpha_domain::campaign_finalization::sign_campaign_final_evaluation_grant;
        let key = SigningKey::from_bytes(&[7; 32]);
        let signed =
            sign_campaign_final_evaluation_grant(definition, "operator".into(), &key).unwrap();
        let verified = verify_campaign_final_evaluation_grant(
            &signed,
            &BTreeMap::from([("operator".into(), key.verifying_key())]),
            t0(),
        )
        .unwrap();
        store.record_approval(&ApprovalRecord {
            approval_id: "final-approval".into(), approval_class: "campaign_final_evaluation".into(),
            subject_id: verified.grant().grant_id.clone(),
            payload: serde_json::json!({"grant_sha256": verified.content_sha256(), "family_id": FAMILY}),
            signer_id: Some("operator".into()), valid_from: Some(t0()), expires_at: Some(minutes(720)),
            revoked_at: None, revoked_by: None, revocation_reason: None, created_at: t0(),
        }).unwrap();
        verified
    }

    fn closed_final_family() -> (AlphaStore, VerifiedCampaignFinalEvaluationGrant) {
        let (mut store, root, attempt) = final_family_base();
        settle_final_source(&mut store, &attempt);
        let definition = final_definition(&store, &root, &attempt);
        let grant = approve_final(&mut store, definition);
        store
            .close_campaign_family_for_final_evaluation(&grant, "final-approval", minutes(2))
            .unwrap();
        (store, grant)
    }

    #[test]
    fn final_dispatch_is_single_use_published_and_restore_safe() {
        let (mut store, grant) = closed_final_family();
        let request = "a".repeat(64);
        let target = dispatch_target();
        assert!(store
            .claim_campaign_final_dispatch(&grant, &request, &target, minutes(3))
            .is_err());
        acknowledge_all(&mut store);
        let (claim, first) = store
            .claim_campaign_final_dispatch(&grant, &request, &target, minutes(3))
            .unwrap();
        assert!(first);
        assert_eq!(claim.claimed_at, minutes(3));
        assert!(store
            .bind_campaign_final_dispatch_job(&grant, &request, &target, "final-job", minutes(4))
            .is_err());
        acknowledge_all(&mut store);
        let (same, first) = store
            .claim_campaign_final_dispatch(&grant, &request, &target, minutes(4))
            .unwrap();
        assert!(!first);
        assert_eq!(same, claim);
        assert!(store
            .claim_campaign_final_dispatch(&grant, &"b".repeat(64), &target, minutes(4))
            .is_err());
        store
            .bind_campaign_final_dispatch_job(&grant, &request, &target, "final-job", minutes(4))
            .unwrap();
        let mut called = false;
        let result: Result<(), StoreError> = store.with_campaign_final_dispatch_admission(
            &grant,
            &request,
            &target,
            Some("final-job"),
            || minutes(5),
            || {
                called = true;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!called);
        acknowledge_all(&mut store);
        let snapshot = store.campaign_family_snapshot(FAMILY).unwrap();
        let mut restored = AlphaStore::open_in_memory().unwrap();
        restored.integrity_key = store.integrity_key;
        restored.import_campaign_family_snapshot(&snapshot).unwrap();
        acknowledge_all(&mut restored);
        assert!(
            !restored
                .claim_campaign_final_dispatch(&grant, &request, &target, minutes(5))
                .unwrap()
                .1
        );
        assert!(restored
            .bind_campaign_final_dispatch_job(&grant, &request, &target, "another-job", minutes(5))
            .is_err());
        let result: Result<(), StoreError> = restored.with_campaign_final_dispatch_admission(
            &grant,
            &request,
            &target,
            Some("final-job"),
            || minutes(5),
            || Ok(()),
        );
        result.unwrap();
        assert!(
            restored
                .claim_campaign_final_dispatch(&grant, &request, &target, minutes(63))
                .is_err(),
            "claim deadline cannot be extended by adoption"
        );
    }

    #[test]
    fn final_dispatch_revocation_and_terminal_evidence_preserve_one_attempt() {
        let (mut store, grant) = closed_final_family();
        let request = "a".repeat(64);
        let target = dispatch_target();
        acknowledge_all(&mut store);
        store
            .claim_campaign_final_dispatch(&grant, &request, &target, minutes(3))
            .unwrap();
        acknowledge_all(&mut store);
        store
            .bind_campaign_final_dispatch_job(&grant, &request, &target, "final-job", minutes(4))
            .unwrap();
        acknowledge_all(&mut store);
        store
            .revoke_approval("final-approval", "operator", "stop", minutes(5))
            .unwrap();
        acknowledge_all(&mut store);
        let mut called = false;
        let result: Result<(), StoreError> = store.with_campaign_final_dispatch_admission(
            &grant,
            &request,
            &target,
            Some("final-job"),
            || minutes(6),
            || {
                called = true;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!called);
        let mut evidence = CampaignFinalDispatchSettlementV1 {
            request_sha256: request.clone(),
            job_uid: "wrong-job".into(),
            pod_uid: "final-pod".into(),
            result_sha256: "d".repeat(64),
            outcome: CampaignFinalOutcomeV1::PromotionReady,
            candidates_evaluated: None,
            consumed_job_seconds: None,
        };
        assert!(store
            .settle_campaign_final_dispatch(FAMILY, &evidence, minutes(7))
            .is_err());
        evidence.job_uid = "final-job".into();
        assert!(
            store
                .settle_campaign_final_dispatch(FAMILY, &evidence, minutes(7))
                .is_err(),
            "successful final result cannot omit accounting"
        );
        evidence.outcome = CampaignFinalOutcomeV1::Failed;
        let receipt = store
            .settle_campaign_final_dispatch(FAMILY, &evidence, minutes(7))
            .unwrap();
        assert_eq!(
            store
                .settle_campaign_final_dispatch(FAMILY, &evidence, minutes(8))
                .unwrap(),
            receipt
        );
        assert!(store
            .claim_campaign_final_dispatch(&grant, &request, &target, minutes(8))
            .is_err());
        let record = store.campaign_final_dispatch_record(FAMILY).unwrap();
        assert_eq!(record.settlement, Some(evidence));
        assert_eq!(
            record.grant.grant().max_job_seconds,
            3600,
            "uncertain failure preserves full reservation"
        );
    }

    #[test]
    fn final_family_closure_is_permanent_idempotent_and_survives_restore() {
        let (mut store, root, attempt) = final_family_base();
        settle_final_source(&mut store, &attempt);
        let definition = final_definition(&store, &root, &attempt);
        let final_grant = approve_final(&mut store, definition);
        let closure = store
            .close_campaign_family_for_final_evaluation(&final_grant, "final-approval", minutes(2))
            .unwrap();
        assert_eq!(
            store
                .close_campaign_family_for_final_evaluation(
                    &final_grant,
                    "final-approval",
                    minutes(3)
                )
                .unwrap(),
            closure
        );
        assert_eq!(
            store.pending_campaign_receipts(FAMILY).unwrap(),
            vec![closure]
        );
        assert_eq!(
            store
                .campaign_final_evaluation_grant(FAMILY)
                .unwrap()
                .as_ref(),
            Some(final_grant.signed_grant())
        );
        acknowledge_all(&mut store);
        let snapshot = store.campaign_family_snapshot(FAMILY).unwrap();
        let mut restored = AlphaStore::open_in_memory().unwrap();
        restored.integrity_key = store.integrity_key;
        restored.import_campaign_family_snapshot(&snapshot).unwrap();
        assert_eq!(restored.campaign_family_snapshot(FAMILY).unwrap(), snapshot);
        assert_eq!(
            restored.campaign_final_evaluation_grant(FAMILY).unwrap(),
            store.campaign_final_evaluation_grant(FAMILY).unwrap()
        );
        let mut another = root.grant().clone();
        another.root_id = "root-after-selection".into();
        let another = verify(another);
        restored
            .record_approval(&approval(&another, "later-root-approval"))
            .unwrap();
        let error = restored
            .register_campaign_root(&another, "later-root-approval", minutes(4))
            .unwrap_err();
        assert!(error.to_string().contains("permanently closed"));
        let mut later = attempt.clone();
        later.attempt_ordinal = 1;
        let error = restored
            .reserve_campaign_attempt(&root, &later, minutes(4))
            .unwrap_err();
        assert!(error.to_string().contains("permanently closed"));
        assert!(restored
            .check_campaign_dispatch_admission(
                &root,
                FAMILY,
                &attempt.operation_id().unwrap(),
                minutes(4)
            )
            .is_err());
    }

    #[test]
    fn final_family_closure_rejects_unsettled_uncertain_and_unbound_sources() {
        for kind in ["pending", "uncertain", "diagnostic"] {
            let (mut store, root, attempt) = final_family_base();
            match kind {
                "uncertain" => {
                    store
                        .settle_campaign_attempt(
                            FAMILY,
                            &settlement(&attempt, CampaignAttemptOutcomeV1::Failed, None),
                            minutes(1),
                        )
                        .unwrap();
                }
                "diagnostic" => {
                    store
                        .settle_campaign_attempt(
                            FAMILY,
                            &settlement(
                                &attempt,
                                CampaignAttemptOutcomeV1::SelectedPreHoldout,
                                Some(40),
                            ),
                            minutes(1),
                        )
                        .unwrap();
                }
                _ => (),
            }
            let definition = final_definition(&store, &root, &attempt);
            let final_grant = approve_final(&mut store, definition);
            let before = store.campaign_family_snapshot(FAMILY).unwrap();
            assert!(
                store
                    .close_campaign_family_for_final_evaluation(
                        &final_grant,
                        "final-approval",
                        minutes(2)
                    )
                    .is_err(),
                "{kind}"
            );
            assert_eq!(before, store.campaign_family_snapshot(FAMILY).unwrap());
        }
    }

    #[test]
    fn final_family_closure_binds_complete_results_view_and_exact_head() {
        for field in [
            "result",
            "extra_result",
            "head",
            "definition",
            "view",
            "source",
        ] {
            let (mut store, root, attempt) = final_family_base();
            settle_final_source(&mut store, &attempt);
            let mut definition = final_definition(&store, &root, &attempt);
            match field {
                "result" => {
                    *definition.selected_results.values_mut().next().unwrap() = "e".repeat(64)
                }
                "extra_result" => {
                    definition.selected_results.insert(
                        format!("campaign-attempt-{}", "e".repeat(64)),
                        "f".repeat(64),
                    );
                }
                "head" => definition.family_head_sha256 = "e".repeat(64),
                "definition" => definition.family_definition_sha256 = "e".repeat(64),
                "view" => {
                    definition.execution.evaluation_views.selection_view_sha256 = "e".repeat(64)
                }
                _ => definition.execution.source_revision = "e".repeat(40),
            }
            let final_grant = approve_final(&mut store, definition);
            let before = store.campaign_family_snapshot(FAMILY).unwrap();
            assert!(
                store
                    .close_campaign_family_for_final_evaluation(
                        &final_grant,
                        "final-approval",
                        minutes(2)
                    )
                    .is_err(),
                "{field}"
            );
            assert_eq!(before, store.campaign_family_snapshot(FAMILY).unwrap());
        }
    }

    #[test]
    fn final_family_closure_replays_revocation_without_reopening_search() {
        let (mut store, root, attempt) = final_family_base();
        settle_final_source(&mut store, &attempt);
        let definition = final_definition(&store, &root, &attempt);
        let final_grant = approve_final(&mut store, definition);
        store
            .close_campaign_family_for_final_evaluation(&final_grant, "final-approval", minutes(2))
            .unwrap();
        store
            .revoke_approval(
                "final-approval",
                "operator",
                "stop final evaluation",
                minutes(3),
            )
            .unwrap();
        let snapshot = store.campaign_family_snapshot(FAMILY).unwrap();
        assert!(matches!(
            snapshot.receipts.last().unwrap().receipt.event,
            CampaignLedgerEventV1::ApprovalRevoked { .. }
        ));
        let mut restored = AlphaStore::open_in_memory().unwrap();
        restored.integrity_key = store.integrity_key;
        restored.import_campaign_family_snapshot(&snapshot).unwrap();
        assert_eq!(restored.campaign_family_snapshot(FAMILY).unwrap(), snapshot);
        assert!(restored
            .close_campaign_family_for_final_evaluation(&final_grant, "final-approval", minutes(4))
            .is_err());
        assert!(restored
            .campaign_final_evaluation_grant(FAMILY)
            .unwrap()
            .is_some());
        let mut later = attempt;
        later.attempt_ordinal = 1;
        assert!(restored
            .reserve_campaign_attempt(&root, &later, minutes(4))
            .unwrap_err()
            .to_string()
            .contains("permanently closed"));
    }

    #[test]
    fn final_authority_signature_scope_and_scheduled_revocation_are_enforced() {
        use alpha_domain::campaign_finalization::sign_campaign_final_evaluation_grant;
        use ed25519_dalek::Signer;
        let (mut store, root, attempt) = final_family_base();
        settle_final_source(&mut store, &attempt);
        let definition = final_definition(&store, &root, &attempt);
        let key = SigningKey::from_bytes(&[7; 32]);
        let keys = BTreeMap::from([("operator".into(), key.verifying_key())]);
        let signed =
            sign_campaign_final_evaluation_grant(definition.clone(), "operator".into(), &key)
                .unwrap();
        assert!(verify_campaign_final_evaluation_grant(&signed, &BTreeMap::new(), t0()).is_err());
        assert!(verify_campaign_final_evaluation_grant(&signed, &keys, minutes(720)).is_err());
        let mut changed = signed.clone();
        changed.grant.max_candidates += 1;
        changed.content_sha256 = changed.grant.content_hash().unwrap();
        assert!(verify_campaign_final_evaluation_grant(&changed, &keys, t0()).is_err());
        changed = signed;
        changed.signature_hex = hex::encode(
            key.sign(format!("{ROOT_GRANT_SCHEMA}:{}", changed.content_sha256).as_bytes())
                .to_bytes(),
        );
        assert!(verify_campaign_final_evaluation_grant(&changed, &keys, t0()).is_err());
        for field in ["shared", "empty", "budget", "deadline"] {
            let mut changed = definition.clone();
            match field {
                "shared" => {
                    changed.execution.evaluation_views.selection_feedback =
                        CampaignSelectionFeedbackV1::SearchAndLearningVisibleWalkForward
                }
                "empty" => changed.selected_results.clear(),
                "budget" => changed.max_candidates = 129,
                _ => changed.max_job_seconds = u64::MAX,
            }
            assert!(changed.validate().is_err(), "{field}");
        }
        let final_grant = approve_final(&mut store, definition);
        store
            .revoke_approval("final-approval", "operator", "scheduled stop", minutes(30))
            .unwrap();
        assert!(store
            .close_campaign_family_for_final_evaluation(&final_grant, "final-approval", minutes(2))
            .unwrap_err()
            .to_string()
            .contains("scheduled revocation"));
        assert!(store
            .campaign_final_evaluation_grant(FAMILY)
            .unwrap()
            .is_none());
    }

    fn kinds(receipts: &[AuthenticatedCampaignReceiptV1]) -> Vec<&'static str> {
        receipts
            .iter()
            .map(|r| match r.receipt.event {
                CampaignLedgerEventV1::RootRegistered { .. } => "root_registered",
                CampaignLedgerEventV1::StudyMemberBound { .. } => "study_member_bound",
                CampaignLedgerEventV1::AttemptReserved { .. } => "attempt_reserved",
                CampaignLedgerEventV1::DispatchClaimed { .. } => "dispatch_claimed",
                CampaignLedgerEventV1::DispatchJobBound { .. } => "dispatch_job_bound",
                CampaignLedgerEventV1::AttemptSettled { .. } => "attempt_settled",
                CampaignLedgerEventV1::DispatchSettled { .. } => "dispatch_settled",
                CampaignLedgerEventV1::ApprovalRevoked { .. } => "approval_revoked",
                CampaignLedgerEventV1::FinalDispatchClaimed { .. } => "final_dispatch_claimed",
                CampaignLedgerEventV1::FinalDispatchJobBound { .. } => "final_dispatch_job_bound",
                CampaignLedgerEventV1::FinalDispatchSettled { .. } => "final_dispatch_settled",
                CampaignLedgerEventV1::FamilyClosedForFinalEvaluation { .. } => {
                    "family_closed_for_final_evaluation"
                }
            })
            .collect()
    }

    fn revoked_journal_entries(store: &AlphaStore) -> i64 {
        store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM run_journal WHERE event_kind = 'approval_revoked'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn family_chain_links_authorization_reservation_settlement_and_revocation() {
        let (mut store, verified) = registered();
        let attempt = reservation(&verified, 0, 44);
        store
            .reserve_campaign_attempt(&verified, &attempt, minutes(10))
            .unwrap();
        assert_eq!(
            store.campaign_family_usage(FAMILY).unwrap().pending_trials,
            44
        );
        store
            .settle_campaign_attempt(
                FAMILY,
                &settlement(&attempt, CampaignAttemptOutcomeV1::NoCandidate, Some(10)),
                minutes(20),
            )
            .unwrap();
        let usage = store.campaign_family_usage(FAMILY).unwrap();
        assert_eq!(
            (
                usage.pending_trials,
                usage.consumed_trials,
                usage.job_attempts
            ),
            (0, 10, 1)
        );

        let revoked_at = minutes(60);
        store
            .revoke_approval(APPROVAL, "operator-b", "stop", revoked_at)
            .unwrap();
        let receipts = store.campaign_family_receipts(FAMILY).unwrap();
        assert_eq!(
            kinds(&receipts),
            [
                "root_registered",
                "attempt_reserved",
                "attempt_settled",
                "approval_revoked"
            ]
        );
        assert!(receipts[0].receipt.previous_receipt_sha256.is_none());
        for (index, pair) in receipts.windows(2).enumerate() {
            assert_eq!(pair[1].receipt.sequence, index as u64 + 2);
            assert_eq!(
                pair[1].receipt.previous_receipt_sha256.as_deref(),
                Some(pair[0].content_sha256.as_str())
            );
            assert!(pair[0].receipt.recorded_at <= pair[1].receipt.recorded_at);
        }
        assert_eq!(receipts[3].receipt.recorded_at, revoked_at);
        assert!(matches!(
            &receipts[3].receipt.event,
            CampaignLedgerEventV1::ApprovalRevoked { revocation }
                if revocation.revoked_at == revoked_at && revocation.approval_id == APPROVAL
        ));
        assert!(!store
            .get_approval(APPROVAL)
            .unwrap()
            .is_active_at(revoked_at));
        assert!(store
            .reserve_campaign_attempt(&verified, &reservation(&verified, 1, 10), revoked_at)
            .is_err());
        assert_eq!(store.campaign_family_receipts(FAMILY).unwrap().len(), 4);

        let snapshot = store.campaign_family_snapshot(FAMILY).unwrap();
        assert_eq!(snapshot.sequence, 4);
        assert_eq!(snapshot.last_receipt_sha256, receipts[3].content_sha256);
        assert_eq!(snapshot.receipts, receipts);
    }

    #[test]
    fn unknown_consumption_after_failure_keeps_the_full_reservation_charged() {
        let (mut store, verified) = registered();
        let first = reservation(&verified, 0, 60);
        store
            .reserve_campaign_attempt(&verified, &first, minutes(10))
            .unwrap();
        // A retry cannot start while the previous attempt is unresolved.
        assert!(store
            .reserve_campaign_attempt(&verified, &reservation(&verified, 1, 60), minutes(11))
            .is_err());

        let failed = settlement(&first, CampaignAttemptOutcomeV1::Failed, None);
        let settled = store
            .settle_campaign_attempt(FAMILY, &failed, minutes(20))
            .unwrap();
        let usage = store.campaign_family_usage(FAMILY).unwrap();
        assert_eq!(usage.pending_trials, 0);
        assert_eq!(usage.consumed_trials, 0);
        assert_eq!(usage.uncertain_trials, 60);
        assert_eq!(usage.accounted_trials().unwrap(), 60);
        assert_eq!(
            store
                .campaign_root_usage(FAMILY, verified.content_sha256())
                .unwrap(),
            usage
        );

        // The root ceiling is 100 trials: the unknown 60 still count against it.
        assert!(store
            .reserve_campaign_attempt(&verified, &reservation(&verified, 1, 41), minutes(30))
            .is_err());
        store
            .reserve_campaign_attempt(&verified, &reservation(&verified, 1, 40), minutes(30))
            .unwrap();
        let usage = store.campaign_family_usage(FAMILY).unwrap();
        assert_eq!((usage.pending_trials, usage.uncertain_trials), (40, 60));
        assert_eq!(usage.accounted_trials().unwrap(), 100);
        assert_eq!(usage.job_attempts, 2);

        // Identical settlement retransmission is idempotent; different bytes for
        // the same operation are rejected instead of rewriting consumption.
        assert_eq!(
            store
                .settle_campaign_attempt(FAMILY, &failed, minutes(31))
                .unwrap(),
            settled
        );
        let mut conflicting = failed.clone();
        conflicting.consumed_trials = Some(5);
        assert!(store
            .settle_campaign_attempt(FAMILY, &conflicting, minutes(31))
            .is_err());
        assert_eq!(store.campaign_family_receipts(FAMILY).unwrap().len(), 4);
    }

    #[test]
    fn revocation_evidence_and_family_receipt_commit_in_one_transaction() {
        let (mut store, _) = registered();
        let original_tag: String = store
            .connection
            .query_row(
                "SELECT auth_tag FROM campaign_family_heads WHERE family_id = ?",
                params![FAMILY],
                |row| row.get(0),
            )
            .unwrap();
        // Corruption fixture: the family head no longer authenticates, so the
        // ledger receipt cannot be appended and the revocation must roll back.
        store
            .connection
            .execute(
                "UPDATE campaign_family_heads SET auth_tag = ? WHERE family_id = ?",
                params!["00", FAMILY],
            )
            .unwrap();
        let at = minutes(60);
        assert!(matches!(
            store.revoke_approval(APPROVAL, "operator-b", "stop", at),
            Err(StoreError::AuthenticityMismatch)
        ));
        assert!(store.get_approval_revocation(APPROVAL).unwrap().is_none());
        assert!(store.get_approval(APPROVAL).unwrap().is_active_at(at));
        assert_eq!(revoked_journal_entries(&store), 0);

        store
            .connection
            .execute(
                "UPDATE campaign_family_heads SET auth_tag = ? WHERE family_id = ?",
                params![original_tag, FAMILY],
            )
            .unwrap();
        store
            .revoke_approval(APPROVAL, "operator-b", "stop", at)
            .unwrap();
        let receipts = store.campaign_family_receipts(FAMILY).unwrap();
        assert_eq!(kinds(&receipts), ["root_registered", "approval_revoked"]);
        assert_eq!(revoked_journal_entries(&store), 1);
        let event = store.get_approval_revocation(APPROVAL).unwrap().unwrap();
        assert!(matches!(
            &receipts[1].receipt.event,
            CampaignLedgerEventV1::ApprovalRevoked { revocation } if *revocation == event
        ));

        // Replaying the identical revocation appends nothing new anywhere.
        store.append_approval_revocation(&event).unwrap();
        assert_eq!(store.campaign_family_receipts(FAMILY).unwrap(), receipts);
        assert_eq!(revoked_journal_entries(&store), 1);
    }

    #[test]
    fn scheduled_revocation_joins_at_registration_and_bounds_job_deadlines() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let verified = verify(grant("root-1"));
        store
            .record_approval(&approval(&verified, APPROVAL))
            .unwrap();
        let when = minutes(180);
        store
            .revoke_approval(APPROVAL, "operator-b", "scheduled stop", when)
            .unwrap();
        // Nothing is registered yet, so the revocation has no family receipt.
        assert!(store.campaign_family_receipts(FAMILY).unwrap().is_empty());

        store
            .register_campaign_root(&verified, APPROVAL, t0())
            .unwrap();
        let receipts = store.campaign_family_receipts(FAMILY).unwrap();
        assert_eq!(kinds(&receipts), ["root_registered", "approval_revoked"]);

        // A Job that would still be running at the revocation is not admitted;
        // one that ends exactly at the revocation is.
        let mut too_long = reservation(&verified, 0, 44);
        too_long.reserved_job_seconds = 7201;
        assert!(store
            .reserve_campaign_attempt(&verified, &too_long, minutes(60))
            .is_err());
        let fits = reservation(&verified, 0, 44);
        store
            .reserve_campaign_attempt(&verified, &fits, minutes(60))
            .unwrap();
        assert!(store
            .reserve_campaign_attempt(&verified, &reservation(&verified, 1, 10), when)
            .is_err());
        assert_eq!(store.campaign_family_receipts(FAMILY).unwrap().len(), 3);
    }

    #[test]
    fn snapshot_import_replays_receipts_and_evidence_into_a_fresh_store() {
        let (mut store, verified) = registered();
        let attempt = reservation(&verified, 0, 44);
        store
            .reserve_campaign_attempt(&verified, &attempt, minutes(10))
            .unwrap();
        store
            .settle_campaign_attempt(
                FAMILY,
                &settlement(&attempt, CampaignAttemptOutcomeV1::Failed, None),
                minutes(20),
            )
            .unwrap();
        store
            .revoke_approval(APPROVAL, "operator-b", "stop", minutes(60))
            .unwrap();
        let snapshot = store.campaign_family_snapshot(FAMILY).unwrap();

        let mut rebuilt = AlphaStore::open_in_memory().unwrap();
        // Without the original integrity key the snapshot does not authenticate.
        assert!(matches!(
            rebuilt.import_campaign_family_snapshot(&snapshot),
            Err(StoreError::AuthenticityMismatch)
        ));
        assert!(rebuilt.campaign_family_receipts(FAMILY).unwrap().is_empty());
        rebuilt.integrity_key = store.integrity_key;
        rebuilt.import_campaign_family_snapshot(&snapshot).unwrap();
        assert_eq!(
            rebuilt.campaign_family_receipts(FAMILY).unwrap(),
            snapshot.receipts
        );
        assert_eq!(
            rebuilt.campaign_family_usage(FAMILY).unwrap(),
            store.campaign_family_usage(FAMILY).unwrap()
        );
        assert_eq!(
            rebuilt
                .campaign_family_usage(FAMILY)
                .unwrap()
                .uncertain_trials,
            44
        );
        assert_eq!(
            rebuilt.get_approval_evidence(APPROVAL).unwrap(),
            store.get_approval_evidence(APPROVAL).unwrap()
        );
        assert_eq!(
            rebuilt.get_approval(APPROVAL).unwrap(),
            store.get_approval(APPROVAL).unwrap()
        );
        assert!(!rebuilt
            .get_approval(APPROVAL)
            .unwrap()
            .is_active_at(minutes(60)));
        // Publication acknowledgements are never inferred from a backup.
        assert_eq!(rebuilt.pending_campaign_receipts(FAMILY).unwrap().len(), 4);

        // Import is idempotent, and a shorter history cannot replace a longer one.
        rebuilt.import_campaign_family_snapshot(&snapshot).unwrap();
        assert_eq!(rebuilt.campaign_family_receipts(FAMILY).unwrap().len(), 4);
        let mut truncated = snapshot.clone();
        truncated.receipts.pop();
        assert!(rebuilt.import_campaign_family_snapshot(&truncated).is_err());
        assert_eq!(rebuilt.campaign_family_snapshot(FAMILY).unwrap(), snapshot);
    }

    #[test]
    fn cumulative_budget_survives_root_change_and_reopen() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "monday-campaign-ledger-reopen-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let db = dir.join("control.duckdb");
        let mut first = grant("root-1");
        first.family.max_trials = 150;
        let first = verify(first);
        let mut second = grant("root-2");
        second.family.max_trials = 150;
        let second = verify(second);
        let attempt = reservation(&first, 0, 100);
        {
            let mut store = AlphaStore::open(&db).unwrap();
            store.record_approval(&approval(&first, APPROVAL)).unwrap();
            store
                .register_campaign_root(&first, APPROVAL, t0())
                .unwrap();
            store
                .reserve_campaign_attempt(&first, &attempt, minutes(10))
                .unwrap();
            store
                .settle_campaign_attempt(
                    FAMILY,
                    &settlement(&attempt, CampaignAttemptOutcomeV1::Failed, None),
                    minutes(20),
                )
                .unwrap();
        }

        // Restart: the unknown consumption is still charged after reopening.
        let mut store = AlphaStore::open(&db).unwrap();
        let usage = store.campaign_family_usage(FAMILY).unwrap();
        assert_eq!(
            (
                usage.pending_trials,
                usage.consumed_trials,
                usage.uncertain_trials
            ),
            (0, 0, 100)
        );
        assert!(store
            .reserve_campaign_attempt(&first, &reservation(&first, 1, 1), minutes(30))
            .is_err());

        // Root change: a new root under the same family cannot reset the
        // family's cumulative charge; only the remaining 50 trials are grantable.
        store
            .record_approval(&approval(&second, "approval-2"))
            .unwrap();
        store
            .register_campaign_root(&second, "approval-2", minutes(30))
            .unwrap();
        assert!(store
            .reserve_campaign_attempt(&second, &reservation(&second, 0, 51), minutes(31))
            .is_err());
        store
            .reserve_campaign_attempt(&second, &reservation(&second, 0, 50), minutes(31))
            .unwrap();
        let usage = store.campaign_family_usage(FAMILY).unwrap();
        assert_eq!((usage.pending_trials, usage.uncertain_trials), (50, 100));
        assert_eq!(usage.accounted_trials().unwrap(), 150);
        assert_eq!(
            store
                .campaign_root_usage(FAMILY, second.content_sha256())
                .unwrap()
                .pending_trials,
            50
        );
        drop(store);

        let store = AlphaStore::open(&db).unwrap();
        assert_eq!(
            kinds(&store.campaign_family_receipts(FAMILY).unwrap()),
            [
                "root_registered",
                "attempt_reserved",
                "attempt_settled",
                "root_registered",
                "attempt_reserved"
            ]
        );
        assert_eq!(store.campaign_family_usage(FAMILY).unwrap(), usage);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dispatch_admission_requires_readback_of_every_prior_receipt() {
        let (mut store, verified) = registered();
        let attempt = reservation(&verified, 0, 44);
        store
            .reserve_campaign_attempt(&verified, &attempt, minutes(10))
            .unwrap();
        let operation_id = attempt.operation_id().unwrap();
        let at = minutes(15);
        assert!(store
            .check_campaign_dispatch_admission(&verified, FAMILY, &operation_id, at)
            .is_err());

        let pending = store.pending_campaign_receipts(FAMILY).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending[0].object_key(),
            format!(
                "research/campaign-ledger/family-id={FAMILY}/sequence={:020}/receipt.json",
                1
            )
        );
        assert!(matches!(
            store.acknowledge_campaign_receipt_readback(
                FAMILY,
                1,
                &pending[0].object_key(),
                &"0".repeat(64)
            ),
            Err(StoreError::ContentHashMismatch)
        ));
        assert!(store
            .check_campaign_dispatch_admission(&verified, FAMILY, &operation_id, at)
            .is_err());
        for receipt in &pending {
            store
                .acknowledge_campaign_receipt_readback(
                    FAMILY,
                    receipt.receipt.sequence,
                    &receipt.object_key(),
                    &receipt.object_sha256().unwrap(),
                )
                .unwrap();
        }
        assert!(store.pending_campaign_receipts(FAMILY).unwrap().is_empty());
        assert_eq!(
            store
                .check_campaign_dispatch_admission(&verified, FAMILY, &operation_id, at)
                .unwrap(),
            attempt
        );

        store
            .settle_campaign_attempt(
                FAMILY,
                &settlement(&attempt, CampaignAttemptOutcomeV1::Failed, None),
                minutes(20),
            )
            .unwrap();
        assert!(store
            .check_campaign_dispatch_admission(&verified, FAMILY, &operation_id, minutes(21))
            .is_err());
    }

    #[test]
    fn retransmission_is_idempotent_and_conflicts_or_family_changes_are_rejected() {
        let (mut store, verified) = registered();
        let attempt = reservation(&verified, 0, 44);
        let first = store
            .reserve_campaign_attempt(&verified, &attempt, minutes(10))
            .unwrap();
        assert_eq!(
            store
                .reserve_campaign_attempt(&verified, &attempt, minutes(11))
                .unwrap(),
            first
        );
        let mut changed = attempt.clone();
        changed.declared_trials = 45;
        assert!(store
            .reserve_campaign_attempt(&verified, &changed, minutes(12))
            .is_err());
        assert_eq!(
            store
                .register_campaign_root(&verified, APPROVAL, t0())
                .unwrap()
                .receipt
                .sequence,
            1
        );
        assert_eq!(store.campaign_family_receipts(FAMILY).unwrap().len(), 2);

        // A second root cannot change the family definition or ceiling.
        let mut escalated = grant("root-2");
        escalated.family.max_trials = 300;
        let escalated = verify(escalated);
        store
            .record_approval(&approval(&escalated, "approval-2"))
            .unwrap();
        assert!(store
            .register_campaign_root(&escalated, "approval-2", minutes(13))
            .is_err());
        assert_eq!(store.campaign_family_receipts(FAMILY).unwrap().len(), 2);

        // A second root under the unchanged family joins the same chain.
        let sibling = verify(grant("root-2"));
        store
            .record_approval(&approval(&sibling, "approval-3"))
            .unwrap();
        store
            .register_campaign_root(&sibling, "approval-3", minutes(14))
            .unwrap();
        assert_eq!(
            kinds(&store.campaign_family_receipts(FAMILY).unwrap()),
            ["root_registered", "attempt_reserved", "root_registered"]
        );
        assert_eq!(
            store.campaign_family_usage(FAMILY).unwrap().pending_trials,
            44
        );
        assert_eq!(
            store
                .campaign_root_usage(FAMILY, sibling.content_sha256())
                .unwrap(),
            CampaignBudgetUsageV1::default()
        );
    }
}
