//! Authenticated study ledger and the cross-family admission guard.
//!
//! A study is a finite, signed set of existing Campaign root grants.  Its
//! receipt head is locked before the member family head, after the existing
//! approval guard, and both ledgers are updated in one DuckDB transaction for
//! every budget-changing member operation.

use super::*;
use alpha_domain::campaign_control::{
    CampaignAttemptReservationV1, CampaignAttemptSettlementV1, VerifiedCampaignRootGrant,
};
use alpha_domain::campaign_study::{
    verify_campaign_study_grant, CampaignStudyGrantV1, CampaignStudyMemberV1,
    SignedCampaignStudyGrantV1, VerifiedCampaignStudyGrant,
};
use chrono::{DateTime, TimeDelta, Utc};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const STUDY_RECEIPT_SCHEMA: &str = "monday.campaign_study_receipt.v1";
const STUDY_AUTH_DOMAIN: &str = "campaign-study-receipt";
const STUDY_HEAD_DOMAIN: &str = "campaign-study-head";
const STUDY_PUBLICATION_DOMAIN: &str = "campaign-study-receipt-publication";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignStudyMemberHeadV1 {
    pub sequence: u64,
    pub last_receipt_sha256: String,
    pub auth_tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CampaignStudyLedgerEventV1 {
    StudyRegistered {
        signed: Box<SignedCampaignStudyGrantV1>,
        verifying_key_hex: String,
        approval: ApprovalRecord,
        approval_content_sha256: String,
        initial_usage: BTreeMap<String, CampaignBudgetUsageV1>,
        member_heads: BTreeMap<String, CampaignStudyMemberHeadV1>,
    },
    AttemptReserved {
        family_id: String,
        reservation: CampaignAttemptReservationV1,
        family_receipt_sha256: String,
    },
    AttemptSettled {
        family_id: String,
        settlement: CampaignAttemptSettlementV1,
        family_receipt_sha256: String,
    },
    ApprovalRevoked {
        revocation: ApprovalRevocationV1,
    },
}

impl CampaignStudyLedgerEventV1 {
    fn semantic_id(&self) -> Result<String, StoreError> {
        Ok(match self {
            Self::StudyRegistered { signed, .. } => {
                format!("campaign-study:{}", signed.grant.study_id)
            }
            Self::AttemptReserved {
                family_id,
                reservation,
                ..
            } => format!(
                "campaign-study-attempt-reserved:{family_id}:{}",
                reservation.operation_id().map_err(err)?
            ),
            Self::AttemptSettled {
                family_id,
                settlement,
                ..
            } => format!(
                "campaign-study-attempt-settled:{family_id}:{}",
                settlement.operation_id
            ),
            Self::ApprovalRevoked { revocation } => {
                format!("campaign-study-revocation:{}", revocation.approval_id)
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignStudyLedgerReceiptV1 {
    pub schema_version: String,
    pub study_id: String,
    pub sequence: u64,
    pub previous_receipt_sha256: Option<String>,
    pub recorded_at: DateTime<Utc>,
    pub event: CampaignStudyLedgerEventV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedCampaignStudyReceiptV1 {
    pub receipt: CampaignStudyLedgerReceiptV1,
    pub content_sha256: String,
    pub auth_tag: String,
}

impl AuthenticatedCampaignStudyReceiptV1 {
    pub fn object_key(&self) -> String {
        format!(
            "research/campaign-ledger/study-id={}/sequence={:020}/receipt.json",
            self.receipt.study_id, self.receipt.sequence
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
pub struct CampaignStudySnapshotV1 {
    pub study_id: String,
    pub sequence: u64,
    pub last_receipt_sha256: String,
    pub head_auth_tag: String,
    pub receipts: Vec<AuthenticatedCampaignStudyReceiptV1>,
    /// Every member family snapshot is required.  A study-only restore cannot
    /// silently forget a family head or its root-local evidence.
    pub member_heads: BTreeMap<String, CampaignStudyMemberHeadV1>,
    pub member_snapshots: Vec<CampaignFamilySnapshotV1>,
}

struct StudyMember {
    binding: CampaignStudyMemberV1,
    initial_usage: CampaignBudgetUsageV1,
}

struct StudyAttempt {
    reservation: CampaignAttemptReservationV1,
    settlement: Option<CampaignAttemptSettlementV1>,
}

struct FamilyHistoryCache {
    history: Vec<AuthenticatedCampaignReceiptV1>,
    hashes: BTreeSet<String>,
}

#[derive(Default)]
struct StudyState {
    grant: Option<VerifiedCampaignStudyGrant>,
    approval: Option<ApprovalRecord>,
    approval_hash: Option<String>,
    revoked_at: Option<DateTime<Utc>>,
    members: BTreeMap<String, StudyMember>,
    attempts: BTreeMap<String, StudyAttempt>,
    usage: CampaignBudgetUsageV1,
}

impl StudyState {
    fn grant(&self) -> Result<&VerifiedCampaignStudyGrant, StoreError> {
        self.grant
            .as_ref()
            .ok_or_else(|| err("study is not registered"))
    }

    fn member(&self, family_id: &str) -> Result<&StudyMember, StoreError> {
        self.members
            .get(family_id)
            .ok_or_else(|| err("family is not a member of the study"))
    }

    fn can_reserve(
        &self,
        verified: &VerifiedCampaignRootGrant,
        reservation: &CampaignAttemptReservationV1,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let member = self.member(&reservation.family_id)?;
        if !member
            .binding
            .matches_root(verified.grant(), verified.content_sha256())
            || reservation.root_grant_sha256 != member.binding.root_grant_sha256
            || reservation.execution != member.binding.execution
        {
            return Err(err("reservation is outside its signed study member"));
        }
        self.validate_attempt_window(reservation, at)?;
        let operation_id = reservation.operation_id().map_err(err)?;
        if let Some(existing) = self.attempts.get(&operation_id) {
            if existing.reservation == *reservation {
                return Ok(true);
            }
            return Err(err("study attempt identity changed"));
        }
        let budget = &self.grant()?.grant().budget;
        if add(self.usage.accounted_trials()?, reservation.declared_trials)? > budget.max_trials
            || add(self.usage.job_attempts, 1)? > budget.max_job_attempts
            || add(
                self.usage.reserved_job_seconds,
                reservation.reserved_job_seconds,
            )? > budget.max_job_seconds
            || add(
                self.usage.reserved_llm_tokens,
                reservation.reserved_llm_tokens,
            )? > budget.max_llm_tokens
        {
            return Err(err("cumulative study budget exhausted"));
        }
        Ok(false)
    }

    fn validate_attempt_window(
        &self,
        reservation: &CampaignAttemptReservationV1,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let verified = self.grant()?;
        verified.validate_active_at(at).map_err(err)?;
        let grant = verified.grant();
        if self.revoked_at.is_some_and(|when| at >= when) {
            return Err(err("study grant is inactive"));
        }
        let seconds = i64::try_from(reservation.reserved_job_seconds)
            .map_err(|_| err("study Job deadline overflow"))?;
        let duration =
            TimeDelta::try_seconds(seconds).ok_or_else(|| err("study Job deadline overflow"))?;
        let deadline = at
            .checked_add_signed(duration)
            .ok_or_else(|| err("study Job deadline overflow"))?;
        if deadline > grant.expires_at {
            return Err(err("Job exceeds study expiry"));
        }
        if self.revoked_at.is_some_and(|when| deadline > when) {
            return Err(err("Job exceeds scheduled study revocation"));
        }
        Ok(())
    }

    fn apply(&mut self, receipt: &CampaignStudyLedgerReceiptV1) -> Result<(), StoreError> {
        match &receipt.event {
            CampaignStudyLedgerEventV1::StudyRegistered {
                signed,
                verifying_key_hex,
                approval,
                approval_content_sha256,
                initial_usage,
                member_heads,
            } => {
                if self.grant.is_some() {
                    return Err(err("study identity was already registered"));
                }
                let bytes: [u8; 32] = hex::decode(verifying_key_hex)
                    .map_err(err)?
                    .try_into()
                    .map_err(|_| err("invalid historical study verification key"))?;
                let key = VerifyingKey::from_bytes(&bytes).map_err(err)?;
                let verified = verify_campaign_study_grant(
                    signed,
                    &BTreeMap::from([(signed.key_id.clone(), key)]),
                    receipt.recorded_at,
                )
                .map_err(err)?;
                validate_study_approval(
                    approval,
                    approval_content_sha256,
                    &verified,
                    receipt.recorded_at,
                )?;
                if receipt.study_id != verified.grant().study_id {
                    return Err(err("study receipt identity mismatch"));
                }
                let expected_families: BTreeSet<_> = verified
                    .grant()
                    .members
                    .iter()
                    .map(|member| member.family_id.clone())
                    .collect();
                if initial_usage.len() != expected_families.len()
                    || member_heads.len() != expected_families.len()
                    || initial_usage.keys().collect::<BTreeSet<_>>()
                        != expected_families.iter().collect::<BTreeSet<_>>()
                    || member_heads
                        .keys()
                        .collect::<BTreeSet<_>>()
                        .iter()
                        .copied()
                        .collect::<BTreeSet<_>>()
                        != expected_families.iter().collect::<BTreeSet<_>>()
                {
                    return Err(err("study member snapshot set changed"));
                }
                let mut usage = CampaignBudgetUsageV1::default();
                let mut members = BTreeMap::new();
                for binding in &verified.grant().members {
                    let initial = initial_usage
                        .get(&binding.family_id)
                        .ok_or_else(|| err("study member usage is missing"))?;
                    let head = member_heads
                        .get(&binding.family_id)
                        .ok_or_else(|| err("study member head is missing"))?;
                    if initial.pending_trials != 0 || initial.uncertain_trials != 0 {
                        return Err(err(
                            "study registration rejects pre-existing pending or uncertain usage",
                        ));
                    }
                    validate_member_head(head)?;
                    usage = add_usage(&usage, initial)?;
                    members.insert(
                        binding.family_id.clone(),
                        StudyMember {
                            binding: binding.clone(),
                            initial_usage: initial.clone(),
                        },
                    );
                }
                if usage.accounted_trials()? > verified.grant().budget.max_trials
                    || usage.job_attempts > verified.grant().budget.max_job_attempts
                    || usage.reserved_job_seconds > verified.grant().budget.max_job_seconds
                    || usage.reserved_llm_tokens > verified.grant().budget.max_llm_tokens
                {
                    return Err(err("pre-existing usage exceeds study budget"));
                }
                self.grant = Some(verified);
                self.approval = Some(approval.clone());
                self.approval_hash = Some(approval_content_sha256.clone());
                self.members = members;
                self.usage = usage;
            }
            CampaignStudyLedgerEventV1::AttemptReserved {
                family_id,
                reservation,
                family_receipt_sha256,
            } => {
                if receipt.study_id != self.grant()?.grant().study_id
                    || family_id != &reservation.family_id
                {
                    return Err(err("study reservation family mismatch"));
                }
                validate_digest(family_receipt_sha256)?;
                let member = self.member(family_id)?;
                if reservation.root_grant_sha256 != member.binding.root_grant_sha256
                    || reservation.execution != member.binding.execution
                {
                    return Err(err("study reservation root or execution mismatch"));
                }
                reservation.validate().map_err(err)?;
                self.validate_attempt_window(reservation, receipt.recorded_at)?;
                let operation_id = reservation.operation_id().map_err(err)?;
                if self.attempts.contains_key(&operation_id) {
                    return Err(err("study attempt identity was already recorded"));
                }
                let budget = &self.grant()?.grant().budget;
                let next_trials = add(self.usage.accounted_trials()?, reservation.declared_trials)?;
                let next_attempts = add(self.usage.job_attempts, 1)?;
                let next_seconds = add(
                    self.usage.reserved_job_seconds,
                    reservation.reserved_job_seconds,
                )?;
                let next_tokens = add(
                    self.usage.reserved_llm_tokens,
                    reservation.reserved_llm_tokens,
                )?;
                if next_trials > budget.max_trials
                    || next_attempts > budget.max_job_attempts
                    || next_seconds > budget.max_job_seconds
                    || next_tokens > budget.max_llm_tokens
                {
                    return Err(err("cumulative study budget exhausted"));
                }
                self.usage.pending_trials =
                    add(self.usage.pending_trials, reservation.declared_trials)?;
                self.usage.job_attempts = next_attempts;
                self.usage.reserved_job_seconds = next_seconds;
                self.usage.reserved_llm_tokens = next_tokens;
                self.attempts.insert(
                    operation_id,
                    StudyAttempt {
                        reservation: reservation.clone(),
                        settlement: None,
                    },
                );
            }
            CampaignStudyLedgerEventV1::AttemptSettled {
                family_id,
                settlement,
                family_receipt_sha256,
            } => {
                if receipt.study_id != self.grant()?.grant().study_id {
                    return Err(err("study settlement identity mismatch"));
                }
                self.member(family_id)?;
                validate_digest(family_receipt_sha256)?;
                let attempt = self
                    .attempts
                    .get_mut(&settlement.operation_id)
                    .ok_or_else(|| err("study settlement has no reservation"))?;
                if attempt.reservation.family_id != *family_id {
                    return Err(err("study settlement family mismatch"));
                }
                settlement
                    .validate_against(&attempt.reservation)
                    .map_err(err)?;
                if attempt.settlement.is_some() {
                    return Err(err("study attempt already settled"));
                }
                self.usage.pending_trials = self
                    .usage
                    .pending_trials
                    .checked_sub(attempt.reservation.declared_trials)
                    .ok_or_else(|| err("study pending usage underflow"))?;
                match settlement.consumed_trials {
                    Some(count) => {
                        self.usage.consumed_trials = add(self.usage.consumed_trials, count)?
                    }
                    None => {
                        self.usage.uncertain_trials = add(
                            self.usage.uncertain_trials,
                            attempt.reservation.declared_trials,
                        )?
                    }
                }
                attempt.settlement = Some(settlement.clone());
            }
            CampaignStudyLedgerEventV1::ApprovalRevoked { revocation } => {
                let approval = self
                    .approval
                    .as_ref()
                    .ok_or_else(|| err("study revocation has no registered approval"))?;
                let hash = self
                    .approval_hash
                    .as_ref()
                    .ok_or_else(|| err("study approval hash is missing"))?;
                revocation.apply_to(approval.clone(), hash)?;
                if self.revoked_at.is_some() {
                    return Err(err("study approval already revoked"));
                }
                self.revoked_at = Some(revocation.revoked_at);
            }
        }
        Ok(())
    }
}

fn add_usage(
    current: &CampaignBudgetUsageV1,
    delta: &CampaignBudgetUsageV1,
) -> Result<CampaignBudgetUsageV1, StoreError> {
    Ok(CampaignBudgetUsageV1 {
        pending_trials: add(current.pending_trials, delta.pending_trials)?,
        consumed_trials: add(current.consumed_trials, delta.consumed_trials)?,
        uncertain_trials: add(current.uncertain_trials, delta.uncertain_trials)?,
        job_attempts: add(current.job_attempts, delta.job_attempts)?,
        reserved_job_seconds: add(current.reserved_job_seconds, delta.reserved_job_seconds)?,
        reserved_llm_tokens: add(current.reserved_llm_tokens, delta.reserved_llm_tokens)?,
    })
}

fn validate_member_head(head: &CampaignStudyMemberHeadV1) -> Result<(), StoreError> {
    if head.sequence == 0 {
        return Err(err("study member head is empty"));
    }
    validate_digest(&head.last_receipt_sha256)?;
    validate_digest(&head.auth_tag)
}

fn validate_digest(value: &str) -> Result<(), StoreError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(err("invalid study digest"));
    }
    Ok(())
}

fn validate_study_approval(
    approval: &ApprovalRecord,
    hash: &str,
    verified: &VerifiedCampaignStudyGrant,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    approval.validate()?;
    let grant = verified.grant();
    if encoded(approval)?.1 != hash
        || approval.approval_class != "campaign_study"
        || approval.subject_id != grant.study_id
        || !approval.is_active_at(at)
        || approval.revoked_at.is_some()
        || approval.signer_id.as_deref() != Some(verified.signed_grant().key_id.as_str())
        || approval
            .payload
            .get("grant_sha256")
            .and_then(|v| v.as_str())
            != Some(verified.content_sha256())
        || approval.payload.get("study_id").and_then(|v| v.as_str())
            != Some(grant.study_id.as_str())
        || approval
            .valid_from
            .is_none_or(|from| from > grant.valid_from)
        || approval
            .expires_at
            .is_none_or(|until| until < grant.expires_at)
    {
        return Err(err("approval does not authorize this study grant"));
    }
    Ok(())
}

fn study_head_json(study: &str, sequence: u64, hash: &str) -> Result<String, StoreError> {
    serde_json::to_string(&(STUDY_HEAD_DOMAIN, study, sequence, hash)).map_err(err)
}

fn study_publication_json(
    receipt: &AuthenticatedCampaignStudyReceiptV1,
) -> Result<String, StoreError> {
    serde_json::to_string(&(
        STUDY_PUBLICATION_DOMAIN,
        receipt.object_key(),
        receipt.object_sha256()?,
    ))
    .map_err(err)
}

fn study_load(
    conn: &Connection,
    key: &[u8; 32],
    study_id: &str,
) -> Result<(Option<StudyState>, Vec<AuthenticatedCampaignStudyReceiptV1>), StoreError> {
    let head = conn.query_row(
        "SELECT sequence, last_receipt_sha256, auth_tag FROM campaign_study_heads WHERE study_id = ?",
        params![study_id],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
    );
    let head = match head {
        Ok(h) => h,
        Err(duckdb::Error::QueryReturnedNoRows) => {
            let orphan: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM campaign_study_receipts WHERE study_id = ?)",
                    params![study_id],
                    |r| r.get(0),
                )
                .map_err(database_error)?;
            if orphan {
                return Err(err("study receipt history has no study head"));
            }
            return Ok((None, Vec::new()));
        }
        Err(error) => return Err(database_error(error)),
    };
    verify_authentication_tag(
        key,
        STUDY_HEAD_DOMAIN,
        study_id,
        &study_head_json(study_id, u64::try_from(head.0).map_err(err)?, &head.1)?,
        &head.2,
    )?;
    let mut stmt = conn
        .prepare("SELECT sequence, semantic_id, payload_json, content_hash, auth_tag FROM campaign_study_receipts WHERE study_id = ? ORDER BY sequence")
        .map_err(database_error)?;
    let rows = stmt
        .query_map(params![study_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(database_error)?;
    let mut state = StudyState::default();
    let mut receipts = Vec::new();
    let mut family_histories = BTreeMap::new();
    for row in rows {
        let (sequence, semantic, json, hash, auth) = row.map_err(database_error)?;
        let receipt: CampaignStudyLedgerReceiptV1 =
            decode_authenticated(key, STUDY_AUTH_DOMAIN, &semantic, &json, &hash, &auth)?;
        let expected = u64::try_from(receipts.len()).map_err(err)? + 1;
        if receipt.schema_version != STUDY_RECEIPT_SCHEMA
            || receipt.study_id != study_id
            || receipt.sequence != expected
            || sql_sequence(expected)? != sequence
            || receipt.event.semantic_id()? != semantic
            || receipt.previous_receipt_sha256.as_ref()
                != receipts
                    .last()
                    .map(|r: &AuthenticatedCampaignStudyReceiptV1| &r.content_sha256)
            || receipts
                .last()
                .is_some_and(|r: &AuthenticatedCampaignStudyReceiptV1| {
                    r.receipt.recorded_at > receipt.recorded_at
                })
        {
            return Err(err("study receipt identity, sequence or chain mismatch"));
        }
        state.apply(&receipt)?;
        validate_family_receipt_link(conn, key, &receipt.event, &mut family_histories)?;
        receipts.push(AuthenticatedCampaignStudyReceiptV1 {
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
        return Err(err("study head does not match receipt history"));
    }
    if state.grant.is_some() {
        validate_study_family_bindings(conn, key, &state, &mut family_histories)?;
        Ok((Some(state), receipts))
    } else {
        Ok((None, receipts))
    }
}

fn validate_family_receipt_link(
    conn: &Connection,
    key: &[u8; 32],
    event: &CampaignStudyLedgerEventV1,
    family_histories: &mut BTreeMap<String, FamilyHistoryCache>,
) -> Result<(), StoreError> {
    let (family_id, family_receipt_sha256) = match event {
        CampaignStudyLedgerEventV1::AttemptReserved {
            family_id,
            family_receipt_sha256,
            ..
        }
        | CampaignStudyLedgerEventV1::AttemptSettled {
            family_id,
            family_receipt_sha256,
            ..
        } => (family_id, family_receipt_sha256),
        _ => return Ok(()),
    };
    if !family_histories.contains_key(family_id) {
        let (_, history) = super::load(conn, key, family_id)?;
        let hashes = history
            .iter()
            .map(|receipt| receipt.content_sha256.clone())
            .collect();
        family_histories.insert(family_id.clone(), FamilyHistoryCache { history, hashes });
    }
    if !family_histories
        .get(family_id)
        .expect("family history cache entry was inserted")
        .hashes
        .contains(family_receipt_sha256)
    {
        return Err(err("study receipt is not linked to a family receipt"));
    }
    Ok(())
}

fn insert_study_receipt(
    conn: &Connection,
    key: &[u8; 32],
    receipt: &AuthenticatedCampaignStudyReceiptV1,
) -> Result<(), StoreError> {
    let r = &receipt.receipt;
    let semantic = r.event.semantic_id()?;
    let (json, hash) = encoded(r)?;
    if hash != receipt.content_sha256 {
        return Err(StoreError::ContentHashMismatch);
    }
    let _: CampaignStudyLedgerReceiptV1 = decode_authenticated(
        key,
        STUDY_AUTH_DOMAIN,
        &semantic,
        &json,
        &hash,
        &receipt.auth_tag,
    )?;
    conn.execute(
        "INSERT INTO campaign_study_receipts VALUES (?, ?, ?, ?, ?, ?)",
        params![
            r.study_id,
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
        STUDY_HEAD_DOMAIN,
        &r.study_id,
        &study_head_json(&r.study_id, r.sequence, &hash)?,
    )?;
    let changed = conn
        .execute(
            "UPDATE campaign_study_heads SET sequence = ?, last_receipt_sha256 = ?, auth_tag = ? WHERE study_id = ? AND sequence = ? AND last_receipt_sha256 = ?",
            params![
                sql_sequence(r.sequence)?,
                hash,
                head_auth,
                r.study_id,
                sql_sequence(r.sequence - 1)?,
                r.previous_receipt_sha256.as_deref().unwrap_or("")
            ],
        )
        .map_err(database_error)?;
    if changed != 1 {
        return Err(err("study writer changed"));
    }
    Ok(())
}

fn study_append(
    conn: &Connection,
    key: &[u8; 32],
    study_id: &str,
    event: CampaignStudyLedgerEventV1,
    at: DateTime<Utc>,
) -> Result<AuthenticatedCampaignStudyReceiptV1, StoreError> {
    let (mut state, history) = study_load(conn, key, study_id)?;
    let semantic = event.semantic_id()?;
    for old in &history {
        if old.receipt.event.semantic_id()? == semantic {
            return if old.receipt.event == event {
                Ok(old.clone())
            } else {
                Err(err("same study operation has conflicting receipt bytes"))
            };
        }
    }
    if history.last().is_some_and(|r| r.receipt.recorded_at > at) {
        return Err(err("study receipt time moved backwards"));
    }
    let receipt = CampaignStudyLedgerReceiptV1 {
        schema_version: STUDY_RECEIPT_SCHEMA.into(),
        study_id: study_id.into(),
        sequence: u64::try_from(history.len()).map_err(err)? + 1,
        previous_receipt_sha256: history.last().map(|r| r.content_sha256.clone()),
        recorded_at: at,
        event,
    };
    if let Some(ref mut state) = state {
        state.apply(&receipt)?;
    } else {
        let mut fresh = StudyState::default();
        fresh.apply(&receipt)?;
    }
    if history.is_empty() {
        conn.execute(
            "INSERT INTO campaign_study_heads VALUES (?, 0, '', ?) ON CONFLICT DO NOTHING",
            params![
                study_id,
                authentication_tag(
                    key,
                    STUDY_HEAD_DOMAIN,
                    study_id,
                    &study_head_json(study_id, 0, "")?
                )?
            ],
        )
        .map_err(database_error)?;
    }
    let (json, hash) = encoded(&receipt)?;
    let auth_tag = authentication_tag(key, STUDY_AUTH_DOMAIN, &semantic, &json)?;
    let authenticated = AuthenticatedCampaignStudyReceiptV1 {
        receipt,
        content_sha256: hash,
        auth_tag,
    };
    insert_study_receipt(conn, key, &authenticated)?;
    Ok(authenticated)
}

fn require_published_study_receipts(
    conn: &Connection,
    key: &[u8; 32],
    study_id: &str,
    history: &[AuthenticatedCampaignStudyReceiptV1],
    through: u64,
) -> Result<(), StoreError> {
    for receipt in history.iter().take(usize::try_from(through).map_err(err)?) {
        let (hash, auth): (String, String) = conn
            .query_row(
                "SELECT object_sha256, auth_tag FROM campaign_study_receipt_publications WHERE study_id = ? AND sequence = ?",
                params![study_id, sql_sequence(receipt.receipt.sequence)?],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(database_error)?;
        if hash != receipt.object_sha256()? {
            return Err(StoreError::ContentHashMismatch);
        }
        verify_authentication_tag(
            key,
            STUDY_PUBLICATION_DOMAIN,
            &receipt.object_key(),
            &study_publication_json(receipt)?,
            &auth,
        )?;
    }
    Ok(())
}

fn ensure_study_head(conn: &Connection, key: &[u8; 32], study_id: &str) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO campaign_study_heads SELECT ?, 0, '', ? WHERE NOT EXISTS (SELECT 1 FROM campaign_study_heads WHERE study_id = ?)",
        params![
            study_id,
            authentication_tag(key, STUDY_HEAD_DOMAIN, study_id, &study_head_json(study_id, 0, "")?)?,
            study_id
        ],
    )
    .map_err(database_error)?;
    if conn
        .execute(
            "UPDATE campaign_study_heads SET sequence = sequence WHERE study_id = ?",
            params![study_id],
        )
        .map_err(database_error)?
        != 1
    {
        return Err(err("missing study guard"));
    }
    Ok(())
}

fn read_member_projection(
    conn: &Connection,
    key: &[u8; 32],
    family_id: &str,
) -> Result<Option<(String, String, CampaignStudyMemberV1)>, StoreError> {
    let row = conn.query_row(
        "SELECT study_id, root_grant_sha256, binding_json, content_hash FROM campaign_study_members WHERE family_id = ?",
        params![family_id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        },
    );
    let (study_id, root_hash, json, hash) = match row {
        Ok(row) => row,
        Err(duckdb::Error::QueryReturnedNoRows) => {
            // The authenticated family chain is the membership source of
            // truth.  A missing projection must not downgrade a bound family
            // to legacy V1, even when its Study tables were lost entirely.
            let (_, family_history) = super::load(conn, key, family_id)?;
            if family_study_binding_from_receipts(family_id, &family_history)?.is_some() {
                return Err(err("study member projection is missing"));
            }
            return Ok(None);
        }
        Err(error) => return Err(database_error(error)),
    };
    let binding: CampaignStudyMemberV1 = serde_json::from_str(&json).map_err(err)?;
    binding.validate().map_err(err)?;
    if encoded(&binding)?.1 != hash
        || binding.family_id != family_id
        || binding.root_grant_sha256 != root_hash
    {
        return Err(err("study member projection is corrupted"));
    }
    let (state, _) = study_load(conn, key, &study_id)?;
    let state = state.ok_or_else(|| err("study member has no authenticated study head"))?;
    let member = state.member(family_id)?;
    if member.binding != binding {
        return Err(err("study member projection differs from its receipt"));
    }
    let (_, family_history) = super::load(conn, key, family_id)?;
    let Some((bound_study, bound_grant, bound_member)) =
        family_study_binding_from_receipts(family_id, &family_history)?
    else {
        return Err(err("study member family binding is missing"));
    };
    if bound_study != study_id
        || bound_grant != state.grant()?.content_sha256()
        || bound_member != binding
    {
        return Err(err(
            "study member projection differs from its family binding",
        ));
    }
    Ok(Some((study_id, root_hash, binding)))
}

/// Lock the optional study head and the member family head in the same order
/// used by every direct Campaign V1 admission path.  The approval guard is
/// intentionally acquired by the caller first, matching revocation ordering.
pub(super) fn lock_campaign_guards(
    conn: &Connection,
    key: &[u8; 32],
    verified: &VerifiedCampaignRootGrant,
    family_id: &str,
    at: DateTime<Utc>,
    require_active: bool,
) -> Result<Option<String>, StoreError> {
    let Some((study_id, root_hash, _binding)) = read_member_projection(conn, key, family_id)?
    else {
        if conn
            .execute(
                "UPDATE campaign_family_heads SET sequence = sequence WHERE family_id = ?",
                params![family_id],
            )
            .map_err(database_error)?
            != 1
        {
            return Err(err("missing family guard"));
        }
        return Ok(None);
    };
    if root_hash != verified.content_sha256() {
        return Err(err("root grant is not the declared study member"));
    }
    let (state, _) = study_load(conn, key, &study_id)?;
    let state = state.ok_or_else(|| err("study registration is incomplete"))?;
    let grant = state.grant()?;
    if require_active {
        grant.validate_active_at(at).map_err(err)?;
        let approval = state
            .approval
            .as_ref()
            .ok_or_else(|| err("study approval is missing"))?;
        if state.revoked_at.is_some_and(|when| at >= when)
            || !read_effective_approval(conn, key, &approval.approval_id)?.is_active_at(at)
        {
            return Err(err("study grant is inactive"));
        }
    }
    ensure_study_head(conn, key, &study_id)?;
    if conn
        .execute(
            "UPDATE campaign_family_heads SET sequence = sequence WHERE family_id = ?",
            params![family_id],
        )
        .map_err(database_error)?
        != 1
    {
        return Err(err("missing family guard"));
    }
    Ok(Some(study_id))
}

/// Read-only membership and study authority check used by reservation
/// inspection and by dispatch admission after its guards have been locked.
pub(super) fn check_member_reservation(
    conn: &Connection,
    key: &[u8; 32],
    verified: &VerifiedCampaignRootGrant,
    reservation: &CampaignAttemptReservationV1,
    at: DateTime<Utc>,
    require_publication: bool,
) -> Result<(), StoreError> {
    let Some((study_id, root_hash, binding)) =
        read_member_projection(conn, key, &reservation.family_id)?
    else {
        return Ok(());
    };
    if root_hash != verified.content_sha256()
        || !binding.matches_root(verified.grant(), verified.content_sha256())
        || reservation.root_grant_sha256 != root_hash
        || reservation.execution != binding.execution
    {
        return Err(err("reservation is outside its signed study member"));
    }
    let (state, _) = study_load(conn, key, &study_id)?;
    let state = state.ok_or_else(|| err("study registration is incomplete"))?;
    state.grant()?.validate_active_at(at).map_err(err)?;
    let approval = state
        .approval
        .as_ref()
        .ok_or_else(|| err("study approval is missing"))?;
    if state.revoked_at.is_some_and(|when| at >= when)
        || !read_effective_approval(conn, key, &approval.approval_id)?.is_active_at(at)
    {
        return Err(err("study grant is inactive"));
    }
    state.validate_attempt_window(reservation, at)?;
    if require_publication {
        let (_, history) = study_load(conn, key, &study_id)?;
        let operation_id = reservation.operation_id().map_err(err)?;
        let mut through = None;
        for receipt in &history {
            if let CampaignStudyLedgerEventV1::AttemptReserved {
                reservation: observed,
                ..
            } = &receipt.receipt.event
            {
                if observed.operation_id().map_err(err)? == operation_id {
                    through = Some(receipt.receipt.sequence);
                    break;
                }
            }
        }
        let through = through.ok_or_else(|| err("study reservation receipt is missing"))?;
        require_published_study_receipts(conn, key, &study_id, &history, through)?;
    }
    Ok(())
}

fn study_prepare_reservation(
    conn: &Connection,
    key: &[u8; 32],
    study_id: &str,
    verified: &VerifiedCampaignRootGrant,
    reservation: &CampaignAttemptReservationV1,
    at: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let (state, _) = study_load(conn, key, study_id)?;
    let state = state.ok_or_else(|| err("study registration is incomplete"))?;
    state.can_reserve(verified, reservation, at)
}

pub(super) fn prepare_member_reservation(
    conn: &Connection,
    key: &[u8; 32],
    verified: &VerifiedCampaignRootGrant,
    reservation: &CampaignAttemptReservationV1,
    at: DateTime<Utc>,
) -> Result<(Option<String>, bool), StoreError> {
    let study_id = lock_campaign_guards(conn, key, verified, &reservation.family_id, at, true)?;
    if let Some(study_id) = &study_id {
        let duplicate = study_prepare_reservation(conn, key, study_id, verified, reservation, at)?;
        return Ok((Some(study_id.clone()), duplicate));
    }
    Ok((None, false))
}

pub(super) fn append_member_reservation(
    conn: &Connection,
    key: &[u8; 32],
    study_id: Option<&str>,
    reservation: &CampaignAttemptReservationV1,
    family_receipt: &AuthenticatedCampaignReceiptV1,
    at: DateTime<Utc>,
) -> Result<Option<AuthenticatedCampaignStudyReceiptV1>, StoreError> {
    let Some(study_id) = study_id else {
        return Ok(None);
    };
    Ok(Some(study_append(
        conn,
        key,
        study_id,
        CampaignStudyLedgerEventV1::AttemptReserved {
            family_id: reservation.family_id.clone(),
            reservation: reservation.clone(),
            family_receipt_sha256: family_receipt.content_sha256.clone(),
        },
        at,
    )?))
}

pub(super) fn prepare_member_settlement(
    conn: &Connection,
    key: &[u8; 32],
    family_id: &str,
    settlement: &CampaignAttemptSettlementV1,
    _at: DateTime<Utc>,
) -> Result<Option<String>, StoreError> {
    let Some((study_id, _, _)) = read_member_projection(conn, key, family_id)? else {
        return Ok(None);
    };
    let (state, _) = study_load(conn, key, &study_id)?;
    let state = state.ok_or_else(|| err("study registration is incomplete"))?;
    let attempt = state
        .attempts
        .get(&settlement.operation_id)
        .ok_or_else(|| err("study settlement has no reservation"))?;
    settlement
        .validate_against(&attempt.reservation)
        .map_err(err)?;
    if attempt.reservation.family_id != family_id {
        return Err(err("study settlement family mismatch"));
    }
    // Settlement may happen after expiry or revocation, but still acquires the
    // same study/family serialization guards and checks membership.
    Ok(Some(study_id))
}

pub(super) fn lock_member_settlement_guards(
    conn: &Connection,
    key: &[u8; 32],
    family_id: &str,
    at: DateTime<Utc>,
) -> Result<Option<String>, StoreError> {
    let Some((study_id, root_hash, binding)) = read_member_projection(conn, key, family_id)? else {
        if conn
            .execute(
                "UPDATE campaign_family_heads SET sequence = sequence WHERE family_id = ?",
                params![family_id],
            )
            .map_err(database_error)?
            != 1
        {
            return Err(err("missing family guard"));
        }
        return Ok(None);
    };
    let (state, _) = study_load(conn, key, &study_id)?;
    let state = state.ok_or_else(|| err("study registration is incomplete"))?;
    state.member(family_id)?;
    let _ = (&root_hash, &binding, at);
    ensure_study_head(conn, key, &study_id)?;
    if conn
        .execute(
            "UPDATE campaign_family_heads SET sequence = sequence WHERE family_id = ?",
            params![family_id],
        )
        .map_err(database_error)?
        != 1
    {
        return Err(err("missing family guard"));
    }
    Ok(Some(study_id))
}

pub(super) fn append_member_settlement(
    conn: &Connection,
    key: &[u8; 32],
    study_id: Option<&str>,
    family_id: &str,
    settlement: &CampaignAttemptSettlementV1,
    family_receipt: &AuthenticatedCampaignReceiptV1,
    at: DateTime<Utc>,
) -> Result<Option<AuthenticatedCampaignStudyReceiptV1>, StoreError> {
    let Some(study_id) = study_id else {
        return Ok(None);
    };
    Ok(Some(study_append(
        conn,
        key,
        study_id,
        CampaignStudyLedgerEventV1::AttemptSettled {
            family_id: family_id.into(),
            settlement: settlement.clone(),
            family_receipt_sha256: family_receipt.content_sha256.clone(),
        },
        at,
    )?))
}

pub(super) fn append_registered_study_revocation(
    conn: &Connection,
    key: &[u8; 32],
    approval: &ApprovalRecord,
    event: &ApprovalRevocationV1,
) -> Result<(), StoreError> {
    let Some(study_id) = approval.payload.get("study_id").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    let (state, history) = study_load(conn, key, study_id)?;
    let Some(state) = state else {
        return Ok(());
    };
    if state
        .approval
        .as_ref()
        .is_none_or(|registered| registered.approval_id != approval.approval_id)
    {
        return Err(err("study revocation approval does not match registration"));
    }
    let at = history.last().map_or(event.revoked_at, |receipt| {
        receipt.receipt.recorded_at.max(event.revoked_at)
    });
    study_append(
        conn,
        key,
        study_id,
        CampaignStudyLedgerEventV1::ApprovalRevoked {
            revocation: event.clone(),
        },
        at,
    )?;
    Ok(())
}

fn restore_study_evidence_projection(
    tx: &Transaction<'_>,
    key: &[u8; 32],
    event: &CampaignStudyLedgerEventV1,
) -> Result<(), StoreError> {
    let CampaignStudyLedgerEventV1::StudyRegistered {
        approval,
        approval_content_sha256,
        ..
    } = event
    else {
        if let CampaignStudyLedgerEventV1::ApprovalRevoked { revocation } = event {
            insert_revocation_evidence(tx, key, revocation)?;
        }
        return Ok(());
    };
    let existing: Result<(ApprovalRecord, String), StoreError> = read_json_row_with_hash(
        tx,
        "SELECT payload_json, content_hash FROM approvals WHERE approval_id = ?",
        &approval.approval_id,
    );
    match existing {
        Ok((record, hash)) if record == *approval && hash == *approval_content_sha256 => {
            return Ok(())
        }
        Ok(_) => return Err(err("existing study approval conflicts with source receipt")),
        Err(StoreError::NotFound) => (),
        Err(error) => return Err(error),
    }
    let (json, hash) = encoded(approval)?;
    tx.execute(
        "INSERT INTO approvals (approval_id, approval_class, subject_id, payload_json, content_hash, created_at, signer_id, valid_from, expires_at, revoked_at, revoked_by, revocation_reason) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL)",
        params![
            approval.approval_id,
            approval.approval_class,
            approval.subject_id,
            json,
            hash,
            approval.created_at.to_rfc3339(),
            approval.signer_id,
            approval.valid_from.map(|t| t.to_rfc3339()),
            approval.expires_at.map(|t| t.to_rfc3339())
        ],
    )
    .map_err(database_error)?;
    append_journal(
        tx,
        None,
        "approval_recorded",
        &approval.approval_id,
        &hash,
        approval.created_at,
    )?;
    Ok(())
}

fn validate_study_snapshot(
    snapshot: &CampaignStudySnapshotV1,
    key: &[u8; 32],
) -> Result<StudyState, StoreError> {
    if snapshot.sequence == 0
        || snapshot.sequence != u64::try_from(snapshot.receipts.len()).map_err(err)?
    {
        return Err(err("study snapshot is incomplete"));
    }
    verify_authentication_tag(
        key,
        STUDY_HEAD_DOMAIN,
        &snapshot.study_id,
        &study_head_json(
            &snapshot.study_id,
            snapshot.sequence,
            &snapshot.last_receipt_sha256,
        )?,
        &snapshot.head_auth_tag,
    )?;
    let mut state = StudyState::default();
    let mut previous: Option<&AuthenticatedCampaignStudyReceiptV1> = None;
    for (index, authenticated) in snapshot.receipts.iter().enumerate() {
        let (json, hash) = encoded(&authenticated.receipt)?;
        if hash != authenticated.content_sha256 {
            return Err(StoreError::ContentHashMismatch);
        }
        let semantic = authenticated.receipt.event.semantic_id()?;
        let _: CampaignStudyLedgerReceiptV1 = decode_authenticated(
            key,
            STUDY_AUTH_DOMAIN,
            &semantic,
            &json,
            &hash,
            &authenticated.auth_tag,
        )?;
        if authenticated.receipt.schema_version != STUDY_RECEIPT_SCHEMA
            || authenticated.receipt.study_id != snapshot.study_id
            || authenticated.receipt.sequence != u64::try_from(index).map_err(err)? + 1
            || authenticated.receipt.previous_receipt_sha256.as_ref()
                != previous.map(|r| &r.content_sha256)
        {
            return Err(err("study snapshot receipt chain mismatch"));
        }
        state.apply(&authenticated.receipt)?;
        previous = Some(authenticated);
    }
    if previous.map(|r| r.content_sha256.as_str()) != Some(snapshot.last_receipt_sha256.as_str()) {
        return Err(err("study snapshot head mismatch"));
    }
    let expected: BTreeSet<_> = state.members.keys().cloned().collect();
    let heads: BTreeSet<_> = snapshot.member_heads.keys().cloned().collect();
    if expected != heads {
        return Err(err("study snapshot member head set mismatch"));
    }
    let mut seen = BTreeSet::new();
    for family in &snapshot.member_snapshots {
        if !seen.insert(family.family_id.clone()) {
            return Err(err("duplicate study member family snapshot"));
        }
        validate_snapshot(family, key)?;
        let head = snapshot
            .member_heads
            .get(&family.family_id)
            .ok_or_else(|| err("study member head is missing"))?;
        let observed = CampaignStudyMemberHeadV1 {
            sequence: family.sequence,
            last_receipt_sha256: family.last_receipt_sha256.clone(),
            auth_tag: family.head_auth_tag.clone(),
        };
        if head != &observed {
            return Err(err("study member head differs from family snapshot"));
        }
        let member = state.member(&family.family_id)?;
        let Some((bound_study, bound_grant, bound_member)) =
            family_study_binding_from_receipts(&family.family_id, &family.receipts)?
        else {
            return Err(err("study member family binding is missing"));
        };
        if bound_study != snapshot.study_id
            || bound_grant != state.grant()?.content_sha256()
            || bound_member != member.binding.clone()
        {
            return Err(err("study member family binding differs from authority"));
        }
        let usage = family_usage_for_member(family, &member.binding, key)?;
        // The study ledger and member family ledgers must agree at recovery;
        // this catches a family-only restore or a dropped cross-family receipt.
        if usage != state_member_usage(&state, &family.family_id)? {
            return Err(err("study/member usage diverged"));
        }
    }
    if seen != expected {
        return Err(err("study snapshot omits a member family"));
    }
    for receipt in &snapshot.receipts {
        let (family_id, family_receipt_sha256) = match &receipt.receipt.event {
            CampaignStudyLedgerEventV1::AttemptReserved {
                family_id,
                family_receipt_sha256,
                ..
            }
            | CampaignStudyLedgerEventV1::AttemptSettled {
                family_id,
                family_receipt_sha256,
                ..
            } => (family_id, family_receipt_sha256),
            _ => continue,
        };
        let family = snapshot
            .member_snapshots
            .iter()
            .find(|family_snapshot| &family_snapshot.family_id == family_id)
            .ok_or_else(|| err("study receipt family snapshot is missing"))?;
        let linked = family
            .receipts
            .iter()
            .find(|family_receipt| family_receipt.content_sha256 == *family_receipt_sha256)
            .ok_or_else(|| err("study receipt is not linked to a family snapshot"))?;
        let matches_event = match &receipt.receipt.event {
            CampaignStudyLedgerEventV1::AttemptReserved { reservation, .. } => {
                matches!(
                    &linked.receipt.event,
                    CampaignLedgerEventV1::AttemptReserved { reservation: observed }
                        if observed == reservation
                )
            }
            CampaignStudyLedgerEventV1::AttemptSettled { settlement, .. } => {
                matches!(
                    &linked.receipt.event,
                    CampaignLedgerEventV1::AttemptSettled { settlement: observed }
                        if observed == settlement
                ) || matches!(
                    &linked.receipt.event,
                    CampaignLedgerEventV1::DispatchSettled { evidence }
                        if evidence.settlement == *settlement
                )
            }
            _ => true,
        };
        if !matches_event {
            return Err(err("study receipt differs from linked family event"));
        }
    }
    Ok(state)
}

fn state_member_usage(
    state: &StudyState,
    family_id: &str,
) -> Result<CampaignBudgetUsageV1, StoreError> {
    let member = state.member(family_id)?;
    let mut usage = member.initial_usage.clone();
    for attempt in state
        .attempts
        .values()
        .filter(|a| a.reservation.family_id == family_id)
    {
        usage.pending_trials = add(usage.pending_trials, attempt.reservation.declared_trials)?;
        if let Some(settlement) = &attempt.settlement {
            usage.pending_trials = usage
                .pending_trials
                .checked_sub(attempt.reservation.declared_trials)
                .ok_or_else(|| err("study member pending usage underflow"))?;
            match settlement.consumed_trials {
                Some(count) => usage.consumed_trials = add(usage.consumed_trials, count)?,
                None => {
                    usage.uncertain_trials =
                        add(usage.uncertain_trials, attempt.reservation.declared_trials)?
                }
            }
        }
        usage.job_attempts = add(usage.job_attempts, 1)?;
        usage.reserved_job_seconds = add(
            usage.reserved_job_seconds,
            attempt.reservation.reserved_job_seconds,
        )?;
        usage.reserved_llm_tokens = add(
            usage.reserved_llm_tokens,
            attempt.reservation.reserved_llm_tokens,
        )?;
    }
    Ok(usage)
}

fn family_usage_for_member(
    snapshot: &CampaignFamilySnapshotV1,
    binding: &CampaignStudyMemberV1,
    key: &[u8; 32],
) -> Result<CampaignBudgetUsageV1, StoreError> {
    super::validate_snapshot(snapshot, key)?;
    let mut state = super::state::State::default();
    for receipt in &snapshot.receipts {
        state.apply(&receipt.receipt)?;
    }
    let root = state
        .roots
        .get(&binding.root_grant_sha256)
        .ok_or_else(|| err("study member root is absent from family snapshot"))?;
    if !binding.matches_root(root.grant.grant(), root.grant.content_sha256()) {
        return Err(err("study member root differs from family snapshot"));
    }
    state.usage(None)
}

fn family_study_binding_from_receipts(
    family_id: &str,
    receipts: &[AuthenticatedCampaignReceiptV1],
) -> Result<Option<(String, String, CampaignStudyMemberV1)>, StoreError> {
    let mut binding = None;
    for receipt in receipts {
        let CampaignLedgerEventV1::StudyMemberBound {
            study_id,
            study_grant_sha256,
            member,
        } = &receipt.receipt.event
        else {
            continue;
        };
        member.validate().map_err(err)?;
        validate_digest(study_grant_sha256)?;
        if !valid_study_id(study_id) || receipt.receipt.family_id != family_id {
            return Err(err("family study binding identity is invalid"));
        }
        let candidate = (study_id.clone(), study_grant_sha256.clone(), member.clone());
        if binding
            .as_ref()
            .is_some_and(|existing| existing != &candidate)
        {
            return Err(err("family has conflicting study bindings"));
        }
        binding = Some(candidate);
    }
    Ok(binding)
}

fn valid_study_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn validate_study_family_bindings(
    conn: &Connection,
    key: &[u8; 32],
    state: &StudyState,
    family_histories: &mut BTreeMap<String, FamilyHistoryCache>,
) -> Result<(), StoreError> {
    let grant = state.grant()?.grant();
    for member in &grant.members {
        if !family_histories.contains_key(&member.family_id) {
            let (_, history) = super::load(conn, key, &member.family_id)?;
            let hashes = history
                .iter()
                .map(|receipt| receipt.content_sha256.clone())
                .collect();
            family_histories.insert(
                member.family_id.clone(),
                FamilyHistoryCache { history, hashes },
            );
        }
        let family_history = &family_histories
            .get(&member.family_id)
            .expect("family history cache entry was inserted")
            .history;
        let Some((study_id, study_grant_sha256, bound_member)) =
            family_study_binding_from_receipts(&member.family_id, family_history)?
        else {
            return Err(err("study member family binding is missing"));
        };
        if study_id != grant.study_id
            || study_grant_sha256 != state.grant()?.content_sha256()
            || bound_member != member.clone()
        {
            return Err(err("study member family binding differs from authority"));
        }
    }
    Ok(())
}

impl AlphaStore {
    pub fn campaign_study_id_for_family(
        &self,
        family_id: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(
            read_member_projection(&self.connection, &self.integrity_key, family_id)?
                .map(|(study_id, _, _)| study_id),
        )
    }

    /// Register one signed finite study against already registered root grants.
    /// The study head and every member family head are serialized in this same
    /// transaction as the registration receipt and immutable projections.
    pub fn register_campaign_study(
        &mut self,
        verified: &VerifiedCampaignStudyGrant,
        approval_id: &str,
        at: DateTime<Utc>,
    ) -> Result<AuthenticatedCampaignStudyReceiptV1, StoreError> {
        verified.validate_active_at(at).map_err(err)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        serialize_approval_mutation(&tx, approval_id)?;
        let (approval, hash) = read_json_row_with_hash(
            &tx,
            "SELECT payload_json, content_hash FROM approvals WHERE approval_id = ?",
            approval_id,
        )?;
        if !read_effective_approval(&tx, &self.integrity_key, approval_id)?.is_active_at(at) {
            return Err(err("study approval is not active"));
        }
        validate_study_approval(&approval, &hash, verified, at)?;
        let revocation = read_revocation_evidence(&tx, &self.integrity_key, approval_id)?;
        ensure_study_head(&tx, &self.integrity_key, &verified.grant().study_id)?;
        let (existing_state, existing_receipts) =
            study_load(&tx, &self.integrity_key, &verified.grant().study_id)?;
        if let Some(existing) = existing_state {
            if existing.grant()?.content_sha256() != verified.content_sha256()
                || existing
                    .approval
                    .as_ref()
                    .is_none_or(|registered| registered.approval_id != approval_id)
            {
                return Err(err("study registration conflicts with existing authority"));
            }
            for member in &verified.grant().members {
                let Some((existing_study, existing_root, existing_binding)) =
                    read_member_projection(&tx, &self.integrity_key, &member.family_id)?
                else {
                    return Err(err("study member projection is missing"));
                };
                if existing_study != verified.grant().study_id
                    || existing_root != member.root_grant_sha256
                    || existing_binding != *member
                {
                    return Err(err("study member projection conflicts with authority"));
                }
                let (_, family_history) = super::load(&tx, &self.integrity_key, &member.family_id)?;
                let Some((bound_study, bound_grant, bound_member)) =
                    family_study_binding_from_receipts(&member.family_id, &family_history)?
                else {
                    return Err(err("study member family binding is missing"));
                };
                if bound_study != verified.grant().study_id
                    || bound_grant != verified.content_sha256()
                    || bound_member != member.clone()
                {
                    return Err(err("study member family binding conflicts with authority"));
                }
            }
            let registration = existing_receipts
                .first()
                .cloned()
                .ok_or_else(|| err("study registration receipt is missing"))?;
            tx.commit().map_err(database_error)?;
            return Ok(registration);
        }

        let mut initial_usage = BTreeMap::new();
        let mut member_heads = BTreeMap::new();
        // Lock member family heads in canonical order after the study guard.
        // This prevents two concurrent registrations/reservations from
        // observing different pre-existing usage totals.
        for member in &verified.grant().members {
            if read_member_projection(&tx, &self.integrity_key, &member.family_id)?.is_some() {
                return Err(err("family is already assigned to a study"));
            }
            if tx
                .execute(
                    "UPDATE campaign_family_heads SET sequence = sequence WHERE family_id = ?",
                    params![member.family_id],
                )
                .map_err(database_error)?
                != 1
            {
                return Err(err("study member family is not registered"));
            }
            let (state, family_history) = super::load(&tx, &self.integrity_key, &member.family_id)?;
            if family_history
                .last()
                .is_some_and(|receipt| receipt.receipt.recorded_at > at)
            {
                return Err(err(
                    "study registration time precedes a member family receipt",
                ));
            }
            let root = state
                .roots
                .get(&member.root_grant_sha256)
                .ok_or_else(|| err("study member root is not registered"))?;
            if !member.matches_root(root.grant.grant(), root.grant.content_sha256()) {
                return Err(err("study member root semantic binding differs"));
            }
            // The family ledger already aggregates every root in the immutable
            // family. Carry the full pre-study family charge forward so an
            // older sibling root cannot hide consumption during binding.
            let usage = state.usage(None)?;
            if usage.pending_trials != 0 || usage.uncertain_trials != 0 {
                return Err(err(
                    "study registration rejects pre-existing pending or uncertain usage",
                ));
            }
            initial_usage.insert(member.family_id.clone(), usage);
        }
        // Persist the study membership in each authenticated family chain
        // before recording the Study head.  A family snapshot then carries
        // enough identity to reject a family-only restore in an empty DB.
        for member in &verified.grant().members {
            super::append(
                &tx,
                &self.integrity_key,
                &member.family_id,
                CampaignLedgerEventV1::StudyMemberBound {
                    study_id: verified.grant().study_id.clone(),
                    study_grant_sha256: verified.content_sha256().into(),
                    member: member.clone(),
                },
                at,
            )?;
            let head = tx
                .query_row(
                    "SELECT sequence, last_receipt_sha256, auth_tag FROM campaign_family_heads WHERE family_id = ?",
                    params![member.family_id],
                    |r| {
                        Ok(CampaignStudyMemberHeadV1 {
                            sequence: u64::try_from(r.get::<_, i64>(0)?).unwrap_or(0),
                            last_receipt_sha256: r.get(1)?,
                            auth_tag: r.get(2)?,
                        })
                    },
                )
                .map_err(database_error)?;
            validate_member_head(&head)?;
            member_heads.insert(member.family_id.clone(), head);
        }
        let event = CampaignStudyLedgerEventV1::StudyRegistered {
            signed: Box::new(verified.signed_grant().clone()),
            verifying_key_hex: hex::encode(verified.verifying_key().as_bytes()),
            approval,
            approval_content_sha256: hash,
            initial_usage,
            member_heads,
        };
        let receipt = study_append(
            &tx,
            &self.integrity_key,
            &verified.grant().study_id,
            event.clone(),
            at,
        )?;
        insert_study_member_projections(&tx, verified.grant(), &event)?;
        if let Some(revocation) = revocation {
            study_append(
                &tx,
                &self.integrity_key,
                &verified.grant().study_id,
                CampaignStudyLedgerEventV1::ApprovalRevoked { revocation },
                at,
            )?;
        }
        tx.commit().map_err(database_error)?;
        Ok(receipt)
    }

    pub fn campaign_study_receipts(
        &self,
        study_id: &str,
    ) -> Result<Vec<AuthenticatedCampaignStudyReceiptV1>, StoreError> {
        Ok(study_load(&self.connection, &self.integrity_key, study_id)?.1)
    }

    /// Historical signed authority readback; this never grants a new
    /// admission after expiry or revocation.
    pub fn campaign_study_grant(
        &self,
        study_id: &str,
    ) -> Result<Option<SignedCampaignStudyGrantV1>, StoreError> {
        let (state, _) = study_load(&self.connection, &self.integrity_key, study_id)?;
        let Some(state) = state else {
            return Ok(None);
        };
        Ok(Some(state.grant()?.signed_grant().clone()))
    }

    pub fn campaign_study_usage(
        &self,
        study_id: &str,
    ) -> Result<CampaignBudgetUsageV1, StoreError> {
        let (state, _) = study_load(&self.connection, &self.integrity_key, study_id)?;
        Ok(state.ok_or_else(|| err("study is not registered"))?.usage)
    }

    pub fn campaign_study_snapshot(
        &self,
        study_id: &str,
    ) -> Result<CampaignStudySnapshotV1, StoreError> {
        let receipts = self.campaign_study_receipts(study_id)?;
        let (state, _) = study_load(&self.connection, &self.integrity_key, study_id)?;
        let state = state.ok_or_else(|| err("study is not registered"))?;
        let (sequence, last, auth) = self
            .connection
            .query_row(
                "SELECT sequence, last_receipt_sha256, auth_tag FROM campaign_study_heads WHERE study_id = ?",
                params![study_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
            )
            .map_err(database_error)?;
        let mut member_heads = BTreeMap::new();
        let mut member_snapshots = Vec::new();
        for family_id in state.members.keys() {
            let snapshot = self.campaign_family_snapshot(family_id)?;
            member_heads.insert(
                family_id.clone(),
                CampaignStudyMemberHeadV1 {
                    sequence: snapshot.sequence,
                    last_receipt_sha256: snapshot.last_receipt_sha256.clone(),
                    auth_tag: snapshot.head_auth_tag.clone(),
                },
            );
            member_snapshots.push(snapshot);
        }
        let snapshot = CampaignStudySnapshotV1 {
            study_id: study_id.into(),
            sequence: u64::try_from(sequence).map_err(err)?,
            last_receipt_sha256: last,
            head_auth_tag: auth,
            receipts,
            member_heads,
            member_snapshots,
        };
        validate_study_snapshot(&snapshot, &self.integrity_key)?;
        Ok(snapshot)
    }

    pub fn import_campaign_study_snapshot(
        &mut self,
        snapshot: &CampaignStudySnapshotV1,
    ) -> Result<(), StoreError> {
        validate_study_snapshot(snapshot, &self.integrity_key)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        let (old_state, old_receipts) = study_load(&tx, &self.integrity_key, &snapshot.study_id)?;
        if old_receipts.len() > snapshot.receipts.len() {
            return Err(err("stale study snapshot would omit existing history"));
        }
        for (index, receipt) in snapshot.receipts.iter().enumerate() {
            if let Some(old) = old_receipts.get(index) {
                if old != receipt {
                    return Err(err("study snapshot conflicts with existing history"));
                }
            }
        }
        for family in &snapshot.member_snapshots {
            import_family_snapshot_tx(&tx, &self.integrity_key, family, true)?;
        }
        if old_receipts.is_empty() {
            tx.execute(
                "INSERT INTO campaign_study_heads VALUES (?, 0, '', ?) ON CONFLICT DO NOTHING",
                params![
                    snapshot.study_id,
                    authentication_tag(
                        &self.integrity_key,
                        STUDY_HEAD_DOMAIN,
                        &snapshot.study_id,
                        &study_head_json(&snapshot.study_id, 0, "")?
                    )?
                ],
            )
            .map_err(database_error)?;
        }
        for (index, receipt) in snapshot.receipts.iter().enumerate() {
            if old_receipts.get(index).is_none() {
                insert_study_receipt(&tx, &self.integrity_key, receipt)?;
            }
            restore_study_evidence_projection(&tx, &self.integrity_key, &receipt.receipt.event)?;
        }
        let (state, _) = study_load(&tx, &self.integrity_key, &snapshot.study_id)?;
        if state.is_none()
            || old_state.is_some_and(|old| {
                old.grant.map(|g| g.content_sha256().to_string())
                    != state
                        .as_ref()
                        .and_then(|s| s.grant.as_ref())
                        .map(|g| g.content_sha256().to_string())
            })
        {
            return Err(err("study snapshot did not restore authenticated state"));
        }
        let restored_grant = state
            .as_ref()
            .and_then(|restored| restored.grant.as_ref())
            .ok_or_else(|| err("study is not registered"))?
            .grant();
        insert_study_member_projections(&tx, restored_grant, &snapshot.receipts[0].receipt.event)?;
        tx.commit().map_err(database_error)
    }

    pub fn pending_campaign_study_receipts(
        &self,
        study_id: &str,
    ) -> Result<Vec<AuthenticatedCampaignStudyReceiptV1>, StoreError> {
        let receipts = self.campaign_study_receipts(study_id)?;
        let mut pending = Vec::new();
        for receipt in receipts {
            let ack = self.connection.query_row(
                "SELECT object_sha256, auth_tag FROM campaign_study_receipt_publications WHERE study_id = ? AND sequence = ?",
                params![study_id, sql_sequence(receipt.receipt.sequence)?],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            );
            match ack {
                Err(duckdb::Error::QueryReturnedNoRows) => pending.push(receipt),
                Err(error) => return Err(database_error(error)),
                Ok((hash, auth)) => {
                    if hash != receipt.object_sha256()? {
                        return Err(StoreError::ContentHashMismatch);
                    }
                    verify_authentication_tag(
                        &self.integrity_key,
                        STUDY_PUBLICATION_DOMAIN,
                        &receipt.object_key(),
                        &study_publication_json(&receipt)?,
                        &auth,
                    )?;
                }
            }
        }
        Ok(pending)
    }

    pub fn acknowledge_campaign_study_receipt_readback(
        &mut self,
        study_id: &str,
        sequence: u64,
        observed_object_key: &str,
        observed_sha256: &str,
    ) -> Result<(), StoreError> {
        let history = self.campaign_study_receipts(study_id)?;
        let receipt = sequence
            .checked_sub(1)
            .and_then(|n| usize::try_from(n).ok())
            .and_then(|n| history.get(n))
            .ok_or_else(|| err("unknown study receipt sequence"))?;
        if receipt.object_key() != observed_object_key
            || receipt.object_sha256()? != observed_sha256
        {
            return Err(StoreError::ContentHashMismatch);
        }
        self.connection
            .execute(
                "INSERT INTO campaign_study_receipt_publications VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
                params![
                    study_id,
                    sql_sequence(sequence)?,
                    observed_sha256,
                    authentication_tag(
                        &self.integrity_key,
                        STUDY_PUBLICATION_DOMAIN,
                        &receipt.object_key(),
                        &study_publication_json(receipt)?
                    )?
                ],
            )
            .map_err(database_error)?;
        let (hash, auth): (String, String) = self
            .connection
            .query_row(
                "SELECT object_sha256, auth_tag FROM campaign_study_receipt_publications WHERE study_id = ? AND sequence = ?",
                params![study_id, sql_sequence(sequence)?],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(database_error)?;
        if hash != observed_sha256 {
            return Err(StoreError::ContentHashMismatch);
        }
        verify_authentication_tag(
            &self.integrity_key,
            STUDY_PUBLICATION_DOMAIN,
            &receipt.object_key(),
            &study_publication_json(receipt)?,
            &auth,
        )
    }
}

fn insert_study_member_projections(
    tx: &Transaction<'_>,
    grant: &CampaignStudyGrantV1,
    event: &CampaignStudyLedgerEventV1,
) -> Result<(), StoreError> {
    let CampaignStudyLedgerEventV1::StudyRegistered { .. } = event else {
        return Err(err("study registration receipt is missing"));
    };
    for member in &grant.members {
        let (json, hash) = encoded(member)?;
        tx.execute(
            "INSERT INTO campaign_study_members VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
            params![
                grant.study_id,
                member.family_id,
                member.root_grant_sha256,
                json,
                hash
            ],
        )
        .map_err(database_error)?;
        let (observed_study, observed_root, observed_json, observed_hash): (
            String,
            String,
            String,
            String,
        ) = tx
            .query_row(
                "SELECT study_id, root_grant_sha256, binding_json, content_hash FROM campaign_study_members WHERE family_id = ?",
                params![member.family_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(database_error)?;
        if observed_study != grant.study_id
            || observed_root != member.root_grant_sha256
            || observed_json != json
            || observed_hash != hash
        {
            return Err(err("study member projection conflicts with registration"));
        }
    }
    Ok(())
}

fn import_family_snapshot_tx(
    tx: &Transaction<'_>,
    key: &[u8; 32],
    snapshot: &CampaignFamilySnapshotV1,
    allow_study: bool,
) -> Result<(), StoreError> {
    validate_snapshot(snapshot, key)?;
    if !allow_study && read_member_projection(tx, key, &snapshot.family_id)?.is_some() {
        return Err(err("study member requires a complete study snapshot"));
    }
    let (mut state, old) = super::load(tx, key, &snapshot.family_id)?;
    if old.len() > snapshot.receipts.len() {
        return Err(err("stale family snapshot would omit existing history"));
    }
    for (index, receipt) in snapshot.receipts.iter().enumerate() {
        if let Some(existing) = old.get(index) {
            if existing != receipt {
                return Err(err("snapshot conflicts with existing history"));
            }
        } else {
            state.apply(&receipt.receipt)?;
            if index == 0 {
                let auth = authentication_tag(
                    key,
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
            insert_receipt(tx, key, receipt)?;
        }
        restore_evidence_projection(tx, key, &receipt.receipt.event)?;
    }
    Ok(())
}

/// Used by family snapshot restore to reject silently dropping a registered
/// study's shared ledger.  Study restore calls the internal helper with the
/// explicit complete-member flag.
pub(super) fn reject_family_only_restore(
    conn: &Connection,
    key: &[u8; 32],
    snapshot: &CampaignFamilySnapshotV1,
) -> Result<(), StoreError> {
    if read_member_projection(conn, key, &snapshot.family_id)?.is_some() {
        return Err(err("study member requires a complete study snapshot"));
    }
    if family_study_binding_from_receipts(&snapshot.family_id, &snapshot.receipts)?.is_some() {
        return Err(err("study member requires a complete study snapshot"));
    }
    Ok(())
}

pub(super) fn reject_new_root_registration(
    conn: &Connection,
    key: &[u8; 32],
    family_id: &str,
) -> Result<(), StoreError> {
    if read_member_projection(conn, key, family_id)?.is_some() {
        return Err(err("study member family cannot register another root"));
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
    use alpha_domain::campaign_study::{
        sign_campaign_study_grant, verify_campaign_study_grant, CampaignStudyBudgetV1,
        CampaignStudyGrantV1, CampaignStudyMemberV1, STUDY_GRANT_SCHEMA,
    };
    use chrono::TimeDelta;
    use ed25519_dalek::SigningKey;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-05T00:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn at(minutes: i64) -> DateTime<Utc> {
        t0() + TimeDelta::minutes(minutes)
    }

    fn repeat_hex(nibble: char) -> String {
        nibble.to_string().repeat(64)
    }

    fn root(root_id: &str, family_id: &str, nibble: char) -> CampaignRootGrantV1 {
        CampaignRootGrantV1 {
            schema_version: ROOT_GRANT_SCHEMA.into(),
            root_id: root_id.into(),
            family: CampaignFamilyPolicyV1 {
                family_id: family_id.into(),
                definition_sha256: repeat_hex(nibble),
                max_trials: 200,
            },
            execution_scope: CampaignExecutionScope::PreHoldout,
            execution: CampaignExecutionBindingV1 {
                campaign_inputs_sha256: repeat_hex('b'),
                evaluation_protocol_sha256: repeat_hex('c'),
                evaluation_views: CampaignEvaluationViewsV1 {
                    search_view_sha256: repeat_hex('d'),
                    selection_view_sha256: repeat_hex('d'),
                    selection_feedback:
                        CampaignSelectionFeedbackV1::SearchAndLearningVisibleWalkForward,
                },
                source_revision: "a".repeat(40),
                runner_image: format!("registry/runner@sha256:{}", repeat_hex('e')),
                controller_image: format!("registry/controller@sha256:{}", repeat_hex('f')),
                job_cpu_millis: 1,
                job_memory_mib: 1,
            },
            allowed_policy_revision_ids: BTreeSet::from([format!(
                "cex-search-policy-{}",
                repeat_hex('1')
            )]),
            max_follow_ups: 1,
            budget: CampaignRootBudgetV1 {
                max_trials: 100,
                max_job_attempts: 4,
                max_job_seconds: 1_000,
                max_llm_tokens: 10_000,
            },
            valid_from: t0(),
            expires_at: at(600),
        }
    }

    fn verify_root(grant: CampaignRootGrantV1) -> VerifiedCampaignRootGrant {
        let key = SigningKey::from_bytes(&[7; 32]);
        let signed = sign_campaign_root_grant(grant, "operator".into(), &key).unwrap();
        verify_campaign_root_grant(
            &signed,
            &BTreeMap::from([("operator".into(), key.verifying_key())]),
            t0(),
        )
        .unwrap()
    }

    fn root_approval(root: &VerifiedCampaignRootGrant, approval_id: &str) -> ApprovalRecord {
        ApprovalRecord {
            approval_id: approval_id.into(),
            approval_class: "campaign_root".into(),
            subject_id: root.grant().root_id.clone(),
            payload: serde_json::json!({
                "grant_sha256": root.content_sha256(),
                "family_id": root.grant().family.family_id,
            }),
            signer_id: Some("operator".into()),
            valid_from: Some(t0()),
            expires_at: Some(at(600)),
            revoked_at: None,
            revoked_by: None,
            revocation_reason: None,
            created_at: t0(),
        }
    }

    fn member(root: &VerifiedCampaignRootGrant, label: char) -> CampaignStudyMemberV1 {
        CampaignStudyMemberV1 {
            family_id: root.grant().family.family_id.clone(),
            root_grant_sha256: root.content_sha256().into(),
            family_definition_sha256: root.grant().family.definition_sha256.clone(),
            family_max_trials: root.grant().family.max_trials,
            execution_scope: root.grant().execution_scope.clone(),
            execution: root.grant().execution.clone(),
            label_horizon_sha256: repeat_hex(label),
        }
    }

    fn study_grant(roots: &[&VerifiedCampaignRootGrant], max_trials: u64) -> CampaignStudyGrantV1 {
        CampaignStudyGrantV1 {
            schema_version: STUDY_GRANT_SCHEMA.into(),
            study_id: "study-1".into(),
            members: roots
                .iter()
                .enumerate()
                .map(|(index, root)| member(root, char::from(b'8' + index as u8)))
                .collect(),
            budget: CampaignStudyBudgetV1 {
                max_trials,
                max_job_attempts: 4,
                max_job_seconds: 1_000,
                max_llm_tokens: 10_000,
            },
            valid_from: t0(),
            expires_at: at(600),
        }
    }

    fn study_approval(grant: &VerifiedCampaignStudyGrant, approval_id: &str) -> ApprovalRecord {
        ApprovalRecord {
            approval_id: approval_id.into(),
            approval_class: "campaign_study".into(),
            subject_id: grant.grant().study_id.clone(),
            payload: serde_json::json!({
                "grant_sha256": grant.content_sha256(),
                "study_id": grant.grant().study_id,
            }),
            signer_id: Some("operator".into()),
            valid_from: Some(t0()),
            expires_at: Some(at(600)),
            revoked_at: None,
            revoked_by: None,
            revocation_reason: None,
            created_at: t0(),
        }
    }

    fn verify_study(grant: CampaignStudyGrantV1) -> VerifiedCampaignStudyGrant {
        let key = SigningKey::from_bytes(&[9; 32]);
        let signed = sign_campaign_study_grant(grant, "operator".into(), &key).unwrap();
        verify_campaign_study_grant(
            &signed,
            &BTreeMap::from([("operator".into(), key.verifying_key())]),
            t0(),
        )
        .unwrap()
    }

    fn reservation(
        root: &VerifiedCampaignRootGrant,
        ordinal: u32,
        trials: u64,
    ) -> CampaignAttemptReservationV1 {
        CampaignAttemptReservationV1 {
            schema_version: ATTEMPT_SCHEMA.into(),
            root_grant_sha256: root.content_sha256().into(),
            family_id: root.grant().family.family_id.clone(),
            campaign_id: format!("campaign-{}", root.grant().root_id),
            execution: root.grant().execution.clone(),
            generation: 0,
            parent_result_sha256: None,
            policy_revision_id: root
                .grant()
                .allowed_policy_revision_ids
                .first()
                .unwrap()
                .clone(),
            request_sha256: repeat_hex('2'),
            attempt_ordinal: ordinal,
            declared_trials: trials,
            reserved_job_seconds: 100,
            reserved_llm_tokens: 100,
        }
    }

    fn settlement(
        reservation: &CampaignAttemptReservationV1,
        outcome: CampaignAttemptOutcomeV1,
        consumed_trials: Option<u64>,
        nibble: char,
    ) -> CampaignAttemptSettlementV1 {
        CampaignAttemptSettlementV1 {
            operation_id: reservation.operation_id().unwrap(),
            reservation_sha256: reservation.content_hash().unwrap(),
            evidence_sha256: repeat_hex(nibble),
            outcome,
            consumed_trials,
        }
    }

    fn register_root(store: &mut AlphaStore, root: &VerifiedCampaignRootGrant, approval_id: &str) {
        store
            .record_approval(&root_approval(root, approval_id))
            .unwrap();
        store
            .register_campaign_root(root, approval_id, t0())
            .unwrap();
    }

    fn register_study(
        store: &mut AlphaStore,
        roots: &[&VerifiedCampaignRootGrant],
        max_trials: u64,
    ) -> VerifiedCampaignStudyGrant {
        register_study_at(store, roots, max_trials, t0())
    }

    fn register_study_at(
        store: &mut AlphaStore,
        roots: &[&VerifiedCampaignRootGrant],
        max_trials: u64,
        at: DateTime<Utc>,
    ) -> VerifiedCampaignStudyGrant {
        let verified = verify_study(study_grant(roots, max_trials));
        store
            .record_approval(&study_approval(&verified, "study-approval"))
            .unwrap();
        store
            .register_campaign_study(&verified, "study-approval", at)
            .unwrap();
        verified
    }

    #[test]
    fn shared_budget_counts_pending_consumed_and_uncertain_usage_across_families() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let first = verify_root(root("root-1", "family-1", '1'));
        let second = verify_root(root("root-2", "family-2", '2'));
        register_root(&mut store, &first, "root-approval-1");
        register_root(&mut store, &second, "root-approval-2");
        let study = register_study(&mut store, &[&first, &second], 60);

        let first_attempt = reservation(&first, 0, 40);
        store
            .reserve_campaign_attempt(&first, &first_attempt, at(1))
            .unwrap();
        let second_attempt = reservation(&second, 0, 21);
        assert!(store
            .reserve_campaign_attempt(&second, &second_attempt, at(2))
            .is_err());
        let second_attempt = reservation(&second, 0, 20);
        store
            .reserve_campaign_attempt(&second, &second_attempt, at(2))
            .unwrap();
        assert_eq!(
            store
                .campaign_study_usage(study.grant().study_id.clone().as_str())
                .unwrap()
                .pending_trials,
            60
        );

        let failed = settlement(&first_attempt, CampaignAttemptOutcomeV1::Failed, None, '3');
        store
            .settle_campaign_attempt(&first_attempt.family_id, &failed, at(3))
            .unwrap();
        let usage = store.campaign_study_usage(&study.grant().study_id).unwrap();
        assert_eq!((usage.pending_trials, usage.uncertain_trials), (20, 40));

        let consumed = settlement(
            &second_attempt,
            CampaignAttemptOutcomeV1::NoCandidate,
            Some(20),
            '4',
        );
        store
            .settle_campaign_attempt(&second_attempt.family_id, &consumed, at(4))
            .unwrap();
        let usage = store.campaign_study_usage(&study.grant().study_id).unwrap();
        assert_eq!(
            (
                usage.pending_trials,
                usage.consumed_trials,
                usage.uncertain_trials
            ),
            (0, 20, 40)
        );
        // The unknown first attempt remains charged, so a retry cannot spend
        // even one additional trial from the 60-trial study ceiling.
        let retry = reservation(&first, 1, 1);
        assert!(store
            .reserve_campaign_attempt(&first, &retry, at(5))
            .is_err());
        // Retransmitting a settlement is idempotent and cannot double-charge.
        let before = store
            .campaign_study_receipts(&study.grant().study_id)
            .unwrap();
        store
            .settle_campaign_attempt(&second_attempt.family_id, &consumed, at(5))
            .unwrap();
        assert_eq!(
            store
                .campaign_study_receipts(&study.grant().study_id)
                .unwrap(),
            before
        );
    }

    #[test]
    fn study_zero_llm_budget_allows_zero_token_reservation_only() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let root = verify_root(root("root-zero-llm", "family-zero-llm", '3'));
        register_root(&mut store, &root, "zero-llm-root-approval");
        let mut grant = study_grant(&[&root], 10);
        grant.budget.max_llm_tokens = 0;
        let study = verify_study(grant);
        store
            .record_approval(&study_approval(&study, "zero-llm-study-approval"))
            .unwrap();
        store
            .register_campaign_study(&study, "zero-llm-study-approval", t0())
            .unwrap();

        let mut zero_tokens = reservation(&root, 0, 1);
        zero_tokens.reserved_llm_tokens = 0;
        store
            .reserve_campaign_attempt(&root, &zero_tokens, at(1))
            .unwrap();
        let positive_tokens = reservation(&root, 1, 1);
        assert!(store
            .reserve_campaign_attempt(&root, &positive_tokens, at(2))
            .is_err());
    }

    #[test]
    fn study_membership_blocks_unlisted_root_direct_v1_and_reparenting() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let listed = verify_root(root("root-listed", "family-1", '1'));
        let unlisted = verify_root(root("root-unlisted", "family-1", '1'));
        register_root(&mut store, &listed, "root-approval-1");
        register_root(&mut store, &unlisted, "root-approval-2");
        let _study = register_study(&mut store, &[&listed], 60);

        // Replaying the exact already-bound root registration remains
        // idempotent; a different root in the family is not admitted.
        store
            .register_campaign_root(&listed, "root-approval-1", at(1))
            .unwrap();

        assert!(store
            .reserve_campaign_attempt(&unlisted, &reservation(&unlisted, 0, 1), at(1))
            .is_err());
        // Removing the query projection cannot downgrade the family back to
        // legacy V1: the authenticated study receipt still names the member.
        store
            .connection
            .execute(
                "DELETE FROM campaign_study_members WHERE family_id = ?",
                params![listed.grant().family.family_id],
            )
            .unwrap();
        assert!(store
            .reserve_campaign_attempt(&listed, &reservation(&listed, 0, 1), at(1))
            .is_err());
        let replacement = verify_study(study_grant(&[&unlisted], 60));
        store
            .record_approval(&study_approval(&replacement, "study-approval-2"))
            .unwrap();
        assert!(store
            .register_campaign_study(&replacement, "study-approval-2", at(1))
            .is_err());

        let new_root = verify_root(root("root-after-study", "family-1", '1'));
        store
            .record_approval(&root_approval(&new_root, "root-approval-3"))
            .unwrap();
        assert!(store
            .register_campaign_root(&new_root, "root-approval-3", at(2))
            .is_err());
    }

    #[test]
    fn family_binding_blocks_direct_reserve_when_study_derived_state_is_lost() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let root = verify_root(root(
            "root-study-state-lost",
            "family-study-state-lost",
            '7',
        ));
        register_root(&mut store, &root, "study-state-lost-root-approval");
        register_study(&mut store, &[&root], 100);
        store
            .connection
            .execute_batch(
                "DELETE FROM campaign_study_receipt_publications;
                 DELETE FROM campaign_study_receipts;
                 DELETE FROM campaign_study_members;
                 DELETE FROM campaign_study_heads;",
            )
            .unwrap();

        assert!(store
            .reserve_campaign_attempt(&root, &reservation(&root, 0, 1), at(1))
            .is_err());
    }

    #[test]
    fn registration_includes_preexisting_consumed_usage_and_rejects_pending_usage() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let consumed_root = verify_root(root("root-consumed", "family-consumed", '1'));
        register_root(&mut store, &consumed_root, "root-approval-consumed");
        let consumed_attempt = reservation(&consumed_root, 0, 7);
        store
            .reserve_campaign_attempt(&consumed_root, &consumed_attempt, at(1))
            .unwrap();
        store
            .settle_campaign_attempt(
                &consumed_attempt.family_id,
                &settlement(
                    &consumed_attempt,
                    CampaignAttemptOutcomeV1::Failed,
                    Some(5),
                    '2',
                ),
                at(2),
            )
            .unwrap();
        let study = verify_study(study_grant(&[&consumed_root], 10));
        store
            .record_approval(&study_approval(&study, "study-approval"))
            .unwrap();
        let before_registration = store
            .campaign_family_snapshot(&consumed_root.grant().family.family_id)
            .unwrap();
        assert!(store
            .register_campaign_study(&study, "study-approval", t0())
            .is_err());
        assert_eq!(
            store
                .campaign_family_snapshot(&consumed_root.grant().family.family_id)
                .unwrap(),
            before_registration
        );
        store
            .register_campaign_study(&study, "study-approval", at(2))
            .unwrap();
        assert_eq!(
            store
                .campaign_study_usage(&study.grant().study_id)
                .unwrap()
                .consumed_trials,
            5
        );
        assert!(store
            .reserve_campaign_attempt(&consumed_root, &reservation(&consumed_root, 1, 6), at(3))
            .is_err());
        store
            .reserve_campaign_attempt(&consumed_root, &reservation(&consumed_root, 1, 5), at(3))
            .unwrap();

        let mut pending_store = AlphaStore::open_in_memory().unwrap();
        let pending_root = verify_root(root("root-pending", "family-pending", '4'));
        register_root(&mut pending_store, &pending_root, "root-approval-pending");
        let pending_attempt = reservation(&pending_root, 0, 1);
        pending_store
            .reserve_campaign_attempt(&pending_root, &pending_attempt, at(1))
            .unwrap();
        let pending_study = verify_study(study_grant(&[&pending_root], 10));
        pending_store
            .record_approval(&study_approval(&pending_study, "pending-study-approval"))
            .unwrap();
        assert!(pending_store
            .register_campaign_study(&pending_study, "pending-study-approval", at(2))
            .is_err());
    }

    #[test]
    fn study_snapshot_requires_authenticated_study_and_member_heads() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let first = verify_root(root("root-1", "family-1", '1'));
        let second = verify_root(root("root-2", "family-2", '2'));
        register_root(&mut store, &first, "root-approval-1");
        register_root(&mut store, &second, "root-approval-2");
        let study = register_study(&mut store, &[&first, &second], 100);
        let attempt = reservation(&first, 0, 12);
        store
            .reserve_campaign_attempt(&first, &attempt, at(1))
            .unwrap();
        let snapshot = store
            .campaign_study_snapshot(&study.grant().study_id)
            .unwrap();
        assert_eq!(snapshot.member_snapshots.len(), 2);

        let mut family_only = AlphaStore::open_in_memory().unwrap();
        family_only.integrity_key = store.integrity_key;
        assert!(family_only
            .import_campaign_family_snapshot(&snapshot.member_snapshots[0])
            .is_err());
        assert!(family_only
            .reserve_campaign_attempt(&first, &attempt, at(2))
            .is_err());

        let mut restored = AlphaStore::open_in_memory().unwrap();
        restored.integrity_key = store.integrity_key;
        restored.import_campaign_study_snapshot(&snapshot).unwrap();
        assert_eq!(
            restored
                .campaign_study_usage(&study.grant().study_id)
                .unwrap(),
            store.campaign_study_usage(&study.grant().study_id).unwrap()
        );
        assert!(restored
            .import_campaign_family_snapshot(&snapshot.member_snapshots[0])
            .is_err());

        let mut corrupted = snapshot.clone();
        corrupted.head_auth_tag = repeat_hex('0');
        let mut rejected = AlphaStore::open_in_memory().unwrap();
        rejected.integrity_key = store.integrity_key;
        assert!(rejected.import_campaign_study_snapshot(&corrupted).is_err());

        let mut independent = AlphaStore::open_in_memory().unwrap();
        independent.integrity_key = store.integrity_key;
        let independent_root = verify_root(root("root-independent", "family-independent", '3'));
        register_root(
            &mut independent,
            &independent_root,
            "independent-root-approval",
        );
        let independent_snapshot = independent
            .campaign_family_snapshot(&independent_root.grant().family.family_id)
            .unwrap();
        let mut independent_restored = AlphaStore::open_in_memory().unwrap();
        independent_restored.integrity_key = store.integrity_key;
        independent_restored
            .import_campaign_family_snapshot(&independent_snapshot)
            .unwrap();
    }

    #[test]
    fn shared_study_guard_serializes_cross_family_reservations() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let first = verify_root(root("root-1", "family-1", '1'));
        let second = verify_root(root("root-2", "family-2", '2'));
        register_root(&mut store, &first, "root-approval-1");
        register_root(&mut store, &second, "root-approval-2");
        let study = register_study(&mut store, &[&first, &second], 100);
        let mut other = store.connection.try_clone().unwrap();
        let tx = other.transaction().unwrap();
        tx.execute(
            "UPDATE campaign_study_heads SET sequence = sequence WHERE study_id = ?",
            params![study.grant().study_id],
        )
        .unwrap();
        let attempt = reservation(&first, 0, 10);
        assert!(store
            .reserve_campaign_attempt(&first, &attempt, at(1))
            .is_err());
        tx.rollback().unwrap();
        store
            .reserve_campaign_attempt(&first, &attempt, at(1))
            .unwrap();
        assert_eq!(
            store
                .campaign_study_usage(&study.grant().study_id)
                .unwrap()
                .pending_trials,
            10
        );
    }

    fn acknowledge_family_receipts(store: &mut AlphaStore, family_id: &str) {
        for receipt in store.campaign_family_receipts(family_id).unwrap() {
            store
                .acknowledge_campaign_receipt_readback(
                    family_id,
                    receipt.receipt.sequence,
                    &receipt.object_key(),
                    &receipt.object_sha256().unwrap(),
                )
                .unwrap();
        }
    }

    fn acknowledge_study_receipts(store: &mut AlphaStore, study_id: &str) {
        for receipt in store.campaign_study_receipts(study_id).unwrap() {
            store
                .acknowledge_campaign_study_receipt_readback(
                    study_id,
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
            namespace: "research".into(),
            job_name: "study-job".into(),
            manifest_sha256: repeat_hex('a'),
        }
    }

    #[test]
    fn direct_dispatch_adoption_and_settlement_cannot_bypass_study_receipts() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let initial_root = verify_root(root("root-1", "family-1", '1'));
        register_root(&mut store, &initial_root, "root-approval-1");
        let study = register_study(&mut store, &[&initial_root], 100);
        let attempt = reservation(&initial_root, 0, 10);
        store
            .reserve_campaign_attempt(&initial_root, &attempt, at(1))
            .unwrap();
        let target = dispatch_target();

        // The direct V1 adoption path requires both family and study receipt
        // publication before any external Job operation is allowed.
        assert!(store
            .claim_campaign_dispatch(&initial_root, &attempt, &target, at(2))
            .is_err());
        acknowledge_family_receipts(&mut store, &attempt.family_id);
        assert!(store
            .claim_campaign_dispatch(&initial_root, &attempt, &target, at(2))
            .is_err());
        acknowledge_study_receipts(&mut store, &study.grant().study_id);
        let (claim, first) = store
            .claim_campaign_dispatch(&initial_root, &attempt, &target, at(2))
            .unwrap();
        assert!(first);
        assert!(claim.job_uid.is_none());
        acknowledge_family_receipts(&mut store, &attempt.family_id);
        store
            .bind_campaign_dispatch_job(&initial_root, &attempt, &target, "study-job-uid", at(3))
            .unwrap();
        acknowledge_family_receipts(&mut store, &attempt.family_id);

        let evidence = CampaignDispatchSettlementV1 {
            job_uid: "study-job-uid".into(),
            pod_uid: "study-pod-uid".into(),
            settlement: settlement(
                &attempt,
                CampaignAttemptOutcomeV1::NoCandidate,
                Some(10),
                'b',
            ),
        };
        let receipt = store
            .settle_campaign_dispatch(&attempt, &evidence, at(4))
            .unwrap();
        let duplicate = store
            .settle_campaign_dispatch(&attempt, &evidence, at(5))
            .unwrap();
        assert_eq!(receipt, duplicate);
        assert_eq!(
            store
                .campaign_study_usage(&study.grant().study_id)
                .unwrap()
                .consumed_trials,
            10
        );
    }

    #[test]
    fn study_approval_revocation_and_expiry_stop_new_member_admission() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let initial_root = verify_root(root("root-1", "family-1", '1'));
        register_root(&mut store, &initial_root, "root-approval-1");
        let study = register_study(&mut store, &[&initial_root], 100);
        store
            .revoke_approval("study-approval", "operator", "study stopped", at(2))
            .unwrap();
        assert!(store
            .reserve_campaign_attempt(&initial_root, &reservation(&initial_root, 0, 1), at(3),)
            .is_err());
        let receipts = store
            .campaign_study_receipts(&study.grant().study_id)
            .unwrap();
        assert!(matches!(
            receipts.last().map(|receipt| &receipt.receipt.event),
            Some(CampaignStudyLedgerEventV1::ApprovalRevoked { .. })
        ));

        let mut expired_store = AlphaStore::open_in_memory().unwrap();
        let expired_root = verify_root(root("root-expired", "family-expired", '2'));
        register_root(&mut expired_store, &expired_root, "expired-root-approval");
        let expired_study = register_study(&mut expired_store, &[&expired_root], 100);
        assert!(expired_store
            .reserve_campaign_attempt(
                &expired_root,
                &reservation(&expired_root, 0, 1),
                expired_study.grant().expires_at,
            )
            .is_err());
    }

    #[test]
    fn study_job_deadline_cannot_cross_expiry_or_scheduled_revocation() {
        let mut expired_store = AlphaStore::open_in_memory().unwrap();
        let expired_root = verify_root(root("root-short-study", "family-short-study", '3'));
        register_root(
            &mut expired_store,
            &expired_root,
            "short-study-root-approval",
        );
        let mut short_grant = study_grant(&[&expired_root], 100);
        short_grant.expires_at = at(5);
        let short_study = verify_study(short_grant);
        expired_store
            .record_approval(&study_approval(&short_study, "short-study-approval"))
            .unwrap();
        expired_store
            .register_campaign_study(&short_study, "short-study-approval", t0())
            .unwrap();

        let mut crossing_expiry = reservation(&expired_root, 0, 1);
        crossing_expiry.reserved_job_seconds = 120;
        assert!(expired_store
            .reserve_campaign_attempt(&expired_root, &crossing_expiry, at(4))
            .is_err());

        let valid = reservation(&expired_root, 0, 1);
        expired_store
            .reserve_campaign_attempt(&expired_root, &valid, at(1))
            .unwrap();
        assert!(expired_store
            .inspect_campaign_reservation(&expired_root, &valid, at(5))
            .is_err());
        expired_store
            .settle_campaign_attempt(
                &valid.family_id,
                &settlement(&valid, CampaignAttemptOutcomeV1::Failed, Some(1), '4'),
                at(6),
            )
            .unwrap();

        let mut revoked_store = AlphaStore::open_in_memory().unwrap();
        let revoked_root = verify_root(root("root-scheduled-study", "family-scheduled-study", '5'));
        register_root(
            &mut revoked_store,
            &revoked_root,
            "scheduled-study-root-approval",
        );
        let scheduled_study = register_study(&mut revoked_store, &[&revoked_root], 100);
        let mut crossing_revocation = reservation(&revoked_root, 0, 1);
        crossing_revocation.reserved_job_seconds = 300;
        revoked_store
            .reserve_campaign_attempt(&revoked_root, &crossing_revocation, at(1))
            .unwrap();
        assert!(revoked_store
            .inspect_campaign_reservation(&revoked_root, &crossing_revocation, at(4))
            .is_ok());
        revoked_store
            .revoke_approval("study-approval", "operator", "scheduled stop", at(5))
            .unwrap();
        let mut reserve_after_schedule = reservation(&revoked_root, 1, 1);
        reserve_after_schedule.reserved_job_seconds = 120;
        let reserve_error = revoked_store
            .reserve_campaign_attempt(&revoked_root, &reserve_after_schedule, at(4))
            .expect_err("a reservation crossing scheduled study revocation must be rejected");
        assert!(reserve_error
            .to_string()
            .contains("scheduled study revocation"));
        assert!(revoked_store
            .inspect_campaign_reservation(&revoked_root, &crossing_revocation, at(4))
            .is_err());
        assert!(revoked_store
            .inspect_campaign_reservation(&revoked_root, &crossing_revocation, at(6))
            .is_err());
        revoked_store
            .settle_campaign_attempt(
                &crossing_revocation.family_id,
                &settlement(
                    &crossing_revocation,
                    CampaignAttemptOutcomeV1::Failed,
                    Some(1),
                    '6',
                ),
                at(6),
            )
            .unwrap();
        assert_eq!(
            revoked_store
                .campaign_study_usage(&scheduled_study.grant().study_id)
                .unwrap()
                .consumed_trials,
            1
        );
    }
}
