//! Transactional, authenticated Campaign family receipts and a replayable budget.
//!
//! One family head is updated in the same transaction as every receipt, so
//! concurrent writers conflict rather than spending the same remaining budget.
//! This is local ledger serialization, not Kubernetes writer fencing.

mod state;

use super::approval_revocations::{insert_revocation_evidence, ApprovalRevocationV1};
use super::{
    append_journal, authentication_tag, database_error, decode_authenticated, encoded,
    read_json_row_with_hash, verify_authentication_tag, AlphaStore, ApprovalRecord, StoreError,
};
use alpha_domain::campaign_control::{
    CampaignAttemptOutcomeV1, CampaignAttemptReservationV1, CampaignAttemptSettlementV1,
    SignedCampaignRootGrantV1, VerifiedCampaignRootGrant,
};
use chrono::{DateTime, Utc};
use duckdb::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};
use state::State;

const SCHEMA: &str = "monday.campaign_ledger_receipt.v1";
const AUTH_DOMAIN: &str = "campaign-ledger-receipt";
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
    AttemptReserved {
        reservation: CampaignAttemptReservationV1,
    },
    AttemptSettled {
        settlement: CampaignAttemptSettlementV1,
    },
    ApprovalRevoked {
        revocation: ApprovalRevocationV1,
    },
}

impl CampaignLedgerEventV1 {
    fn semantic_id(&self) -> Result<String, StoreError> {
        Ok(match self {
            Self::RootRegistered { signed, .. } => {
                format!("campaign-root:{}", signed.grant.root_id)
            }
            Self::AttemptReserved { reservation } => reservation.operation_id().map_err(err)?,
            Self::AttemptSettled { settlement } => {
                format!("campaign-settlement:{}", settlement.operation_id)
            }
            Self::ApprovalRevoked { revocation } => {
                format!("campaign-revocation:{}", revocation.approval_id)
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
    pub fn register_campaign_root(
        &mut self,
        verified: &VerifiedCampaignRootGrant,
        approval_id: &str,
        at: DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignReceiptV1, StoreError> {
        verified.validate_active_at(at).map_err(err)?;
        let (approval, hash) = self.get_approval_evidence(approval_id)?;
        if !self.get_approval(approval_id)?.is_active_at(at) {
            return Err(err("root approval is not active"));
        }
        validate_approval(&approval, &hash, verified, at)?;
        let revocation = self.get_approval_revocation(approval_id)?;
        let event = CampaignLedgerEventV1::RootRegistered {
            signed: Box::new(verified.signed_grant().clone()),
            verifying_key_hex: hex::encode(verified.verifying_key().as_bytes()),
            approval,
            approval_content_sha256: hash,
        };
        let tx = self.connection.transaction().map_err(database_error)?;
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
        let (state, _) = load(
            &self.connection,
            &self.integrity_key,
            &reservation.family_id,
        )?;
        let root = state
            .roots
            .get(verified.content_sha256())
            .ok_or_else(|| err("root is not registered"))?;
        if !self
            .get_approval(&root.approval.approval_id)?
            .is_active_at(at)
        {
            return Err(err("root approval is not active"));
        }
        let tx = self.connection.transaction().map_err(database_error)?;
        let receipt = append(
            &tx,
            &self.integrity_key,
            &reservation.family_id,
            CampaignLedgerEventV1::AttemptReserved {
                reservation: reservation.clone(),
            },
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
        let receipt = append(
            &tx,
            &self.integrity_key,
            family,
            CampaignLedgerEventV1::AttemptSettled {
                settlement: settlement.clone(),
            },
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
        let (state, history) = load(&self.connection, &self.integrity_key, family)?;
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
            || !self
                .get_approval(&root.approval.approval_id)?
                .is_active_at(at)
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
        for receipt in history
            .iter()
            .take(usize::try_from(a.sequence).map_err(err)?)
        {
            let (hash, auth): (String, String) = self.connection.query_row("SELECT object_sha256, auth_tag FROM campaign_receipt_publications WHERE family_id = ? AND sequence = ?", params![family, sql_sequence(receipt.receipt.sequence)?], |r| Ok((r.get(0)?, r.get(1)?))).map_err(database_error)?;
            if hash != receipt.object_sha256()? {
                return Err(StoreError::ContentHashMismatch);
            }
            verify_authentication_tag(
                &self.integrity_key,
                PUBLICATION_DOMAIN,
                &receipt.object_key(),
                &publication_json(receipt)?,
                &auth,
            )?;
        }
        Ok(a.reservation.clone())
    }
}

pub(crate) fn append_registered_root_revocation(
    conn: &Connection,
    key: &[u8; 32],
    approval: &ApprovalRecord,
    event: &ApprovalRevocationV1,
) -> Result<(), StoreError> {
    if approval.approval_class != "campaign_root" {
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
        let mut store = AlphaStore::open_in_memory().unwrap();
        let verified = verify(grant("root-1"));
        store
            .record_approval(&approval(&verified, APPROVAL))
            .unwrap();
        store
            .register_campaign_root(&verified, APPROVAL, t0())
            .unwrap();
        (store, verified)
    }

    fn kinds(receipts: &[AuthenticatedCampaignReceiptV1]) -> Vec<&'static str> {
        receipts
            .iter()
            .map(|r| match r.receipt.event {
                CampaignLedgerEventV1::RootRegistered { .. } => "root_registered",
                CampaignLedgerEventV1::AttemptReserved { .. } => "attempt_reserved",
                CampaignLedgerEventV1::AttemptSettled { .. } => "attempt_settled",
                CampaignLedgerEventV1::ApprovalRevoked { .. } => "approval_revoked",
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
