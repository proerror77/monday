//! Signed scope and stable attempt identities for bounded Campaign research.
//!
//! Scope validation is not cumulative budget admission. The persistent ledger
//! must reserve the declared charges before any external operation is submitted.

use crate::canonical_json_hash;
use chrono::{DateTime, TimeDelta, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const ROOT_GRANT_SCHEMA: &str = "monday.campaign_root_grant.v1";
pub const ATTEMPT_SCHEMA: &str = "monday.campaign_attempt_reservation.v1";

#[derive(Debug, Error)]
pub enum CampaignControlError {
    #[error("invalid Campaign control contract: {0}")]
    Invalid(&'static str),
    #[error("Campaign grant is not active at admission time")]
    InactiveGrant,
    #[error("Campaign grant signing key is not trusted")]
    UntrustedSigner,
    #[error("Campaign grant signature or content hash is invalid")]
    InvalidSignature,
    #[error(transparent)]
    Domain(#[from] crate::DomainError),
}

type Result<T> = std::result::Result<T, CampaignControlError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignExecutionScope {
    PreHoldout,
}

/// Family membership is approved independently of mutable seed/policy choices.
/// The ledger must reject a different definition or ceiling for an existing ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignFamilyPolicyV1 {
    pub family_id: String,
    pub definition_sha256: String,
    pub max_trials: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignRootBudgetV1 {
    pub max_trials: u64,
    pub max_job_attempts: u64,
    /// Sum of reserved wall-clock Job deadlines, with fixed per-Job resources.
    pub max_job_seconds: u64,
    /// Total provider input plus output tokens, not just completion max_tokens.
    pub max_llm_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignSelectionFeedbackV1 {
    /// Current walk-forward feedback is consumed by both search and learning.
    /// Independent selection is deliberately not representable until its
    /// evaluator/reader isolation is implemented and verified.
    SearchAndLearningVisibleWalkForward,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignEvaluationViewsV1 {
    pub search_view_sha256: String,
    pub selection_view_sha256: String,
    pub selection_feedback: CampaignSelectionFeedbackV1,
}

impl CampaignEvaluationViewsV1 {
    pub fn validate(&self) -> Result<()> {
        digest(&self.search_view_sha256, 64)?;
        digest(&self.selection_view_sha256, 64)?;
        if self.search_view_sha256 != self.selection_view_sha256 {
            return Err(CampaignControlError::Invalid(
                "v1 selection must declare the shared walk-forward view",
            ));
        }
        Ok(())
    }

    pub fn evidence_label(&self) -> &'static str {
        "search_visible_validation"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignExecutionBindingV1 {
    pub campaign_inputs_sha256: String,
    pub evaluation_protocol_sha256: String,
    pub evaluation_views: CampaignEvaluationViewsV1,
    pub source_revision: String,
    pub runner_image: String,
    pub controller_image: String,
    pub job_cpu_millis: u32,
    pub job_memory_mib: u32,
}

impl CampaignExecutionBindingV1 {
    pub fn validate(&self) -> Result<()> {
        digest(&self.campaign_inputs_sha256, 64)?;
        digest(&self.evaluation_protocol_sha256, 64)?;
        self.evaluation_views.validate()?;
        digest(&self.source_revision, 40)?;
        image(&self.runner_image)?;
        image(&self.controller_image)?;
        if self.job_cpu_millis == 0 || self.job_memory_mib == 0 {
            return Err(CampaignControlError::Invalid("Job resources"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignRootGrantV1 {
    pub schema_version: String,
    pub root_id: String,
    pub family: CampaignFamilyPolicyV1,
    pub execution_scope: CampaignExecutionScope,
    pub execution: CampaignExecutionBindingV1,
    pub allowed_policy_revision_ids: BTreeSet<String>,
    pub max_follow_ups: u8,
    pub budget: CampaignRootBudgetV1,
    pub valid_from: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl CampaignRootGrantV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != ROOT_GRANT_SCHEMA {
            return Err(CampaignControlError::Invalid("root schema"));
        }
        identifier(&self.root_id)?;
        identifier(&self.family.family_id)?;
        digest(&self.family.definition_sha256, 64)?;
        self.execution.validate()?;
        if self.allowed_policy_revision_ids.is_empty()
            || self.allowed_policy_revision_ids.len() > 16
        {
            return Err(CampaignControlError::Invalid("policy allowlist"));
        }
        for policy in &self.allowed_policy_revision_ids {
            let sha = policy
                .strip_prefix("cex-search-policy-")
                .ok_or(CampaignControlError::Invalid("policy revision ID"))?;
            digest(sha, 64)?;
        }
        if self.valid_from >= self.expires_at
            || self.family.max_trials == 0
            || self.budget.max_trials == 0
            || self.budget.max_trials > self.family.max_trials
            || self.budget.max_job_attempts == 0
            || self.budget.max_job_seconds == 0
        {
            return Err(CampaignControlError::Invalid("budget or validity window"));
        }
        Ok(())
    }

    pub fn content_hash(&self) -> Result<String> {
        self.validate()?;
        Ok(canonical_json_hash(self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedCampaignRootGrantV1 {
    pub grant: CampaignRootGrantV1,
    pub key_id: String,
    pub content_sha256: String,
    pub signature_hex: String,
}

/// Deliberately not deserializable: callers cannot turn untrusted JSON into
/// verified authority without checking a trusted signing key and the payload.
#[derive(Debug, Clone)]
pub struct VerifiedCampaignRootGrant {
    grant: CampaignRootGrantV1,
    content_sha256: String,
    signed: SignedCampaignRootGrantV1,
    verifying_key: VerifyingKey,
}

impl VerifiedCampaignRootGrant {
    pub fn grant(&self) -> &CampaignRootGrantV1 {
        &self.grant
    }

    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }

    pub fn signed_grant(&self) -> &SignedCampaignRootGrantV1 {
        &self.signed
    }

    /// Public verification material for authenticated historical receipts. A
    /// new admission must still use the operator's current trusted-key set.
    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    pub fn validate_active_at(&self, now: DateTime<Utc>) -> Result<()> {
        if now < self.grant.valid_from || now >= self.grant.expires_at {
            return Err(CampaignControlError::InactiveGrant);
        }
        Ok(())
    }

    /// Validates one attempt's scope. Revocation, parent-result provenance,
    /// single-generation ownership and cumulative charges are ledger duties.
    pub fn validate_attempt_scope(
        &self,
        attempt: &CampaignAttemptReservationV1,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.validate_active_at(now)?;
        attempt.validate()?;
        let job_seconds = i64::try_from(attempt.reserved_job_seconds)
            .map_err(|_| CampaignControlError::Invalid("Job deadline overflow"))?;
        let duration = TimeDelta::try_seconds(job_seconds)
            .ok_or(CampaignControlError::Invalid("Job deadline overflow"))?;
        if now
            .checked_add_signed(duration)
            .is_none_or(|deadline| deadline > self.grant.expires_at)
        {
            return Err(CampaignControlError::Invalid("Job exceeds grant expiry"));
        }
        if attempt.root_grant_sha256 != self.content_sha256
            || attempt.family_id != self.grant.family.family_id
            || attempt.execution != self.grant.execution
            || attempt.generation > self.grant.max_follow_ups
            || !self
                .grant
                .allowed_policy_revision_ids
                .contains(&attempt.policy_revision_id)
            || attempt.declared_trials > self.grant.budget.max_trials
            || attempt.reserved_job_seconds > self.grant.budget.max_job_seconds
            || attempt.reserved_llm_tokens > self.grant.budget.max_llm_tokens
            || u64::from(attempt.attempt_ordinal) >= self.grant.budget.max_job_attempts
        {
            return Err(CampaignControlError::Invalid("attempt outside root grant"));
        }
        Ok(())
    }
}

pub fn sign_campaign_root_grant(
    grant: CampaignRootGrantV1,
    key_id: String,
    key: &SigningKey,
) -> Result<SignedCampaignRootGrantV1> {
    identifier(&key_id)?;
    let content_sha256 = grant.content_hash()?;
    let signature = key.sign(signing_message(&content_sha256).as_bytes());
    Ok(SignedCampaignRootGrantV1 {
        grant,
        key_id,
        content_sha256,
        signature_hex: hex::encode(signature.to_bytes()),
    })
}

pub fn verify_campaign_root_grant(
    signed: &SignedCampaignRootGrantV1,
    trusted_keys: &BTreeMap<String, VerifyingKey>,
    now: DateTime<Utc>,
) -> Result<VerifiedCampaignRootGrant> {
    identifier(&signed.key_id)?;
    if signed.grant.content_hash()? != signed.content_sha256 {
        return Err(CampaignControlError::InvalidSignature);
    }
    let key = trusted_keys
        .get(&signed.key_id)
        .ok_or(CampaignControlError::UntrustedSigner)?;
    let bytes =
        hex::decode(&signed.signature_hex).map_err(|_| CampaignControlError::InvalidSignature)?;
    let signature =
        Signature::from_slice(&bytes).map_err(|_| CampaignControlError::InvalidSignature)?;
    key.verify(
        signing_message(&signed.content_sha256).as_bytes(),
        &signature,
    )
    .map_err(|_| CampaignControlError::InvalidSignature)?;
    let verified = VerifiedCampaignRootGrant {
        grant: signed.grant.clone(),
        content_sha256: signed.content_sha256.clone(),
        signed: signed.clone(),
        verifying_key: *key,
    };
    verified.validate_active_at(now)?;
    Ok(verified)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignAttemptReservationV1 {
    pub schema_version: String,
    pub root_grant_sha256: String,
    pub family_id: String,
    pub campaign_id: String,
    /// The dispatcher must derive this binding from its verified request and
    /// rendered execution template, not from a caller's claimed metadata.
    pub execution: CampaignExecutionBindingV1,
    pub generation: u8,
    pub parent_result_sha256: Option<String>,
    pub policy_revision_id: String,
    pub request_sha256: String,
    /// Retransmitting one ordinal is idempotent; actual re-execution uses a new
    /// ordinal and requires a new reservation, even after an infrastructure loss.
    pub attempt_ordinal: u32,
    pub declared_trials: u64,
    pub reserved_job_seconds: u64,
    pub reserved_llm_tokens: u64,
}

impl CampaignAttemptReservationV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != ATTEMPT_SCHEMA
            || self.declared_trials == 0
            || self.reserved_job_seconds == 0
            || (self.generation == 0) != self.parent_result_sha256.is_none()
        {
            return Err(CampaignControlError::Invalid(
                "attempt schema, charges or parent",
            ));
        }
        identifier(&self.family_id)?;
        identifier(&self.campaign_id)?;
        self.execution.validate()?;
        digest(&self.root_grant_sha256, 64)?;
        digest(&self.request_sha256, 64)?;
        if let Some(parent) = &self.parent_result_sha256 {
            digest(parent, 64)?;
        }
        let policy = self
            .policy_revision_id
            .strip_prefix("cex-search-policy-")
            .ok_or(CampaignControlError::Invalid("policy revision ID"))?;
        digest(policy, 64)
    }

    /// A stable external-operation key, independent of receipt retransmission
    /// metadata or charged amounts. The ledger rejects different bytes at this ID.
    pub fn operation_id(&self) -> Result<String> {
        self.validate()?;
        Ok(format!(
            "campaign-attempt-{}",
            canonical_json_hash(&(
                ATTEMPT_SCHEMA,
                &self.root_grant_sha256,
                &self.campaign_id,
                self.generation,
                self.attempt_ordinal,
            ))?
        ))
    }

    pub fn content_hash(&self) -> Result<String> {
        self.validate()?;
        Ok(canonical_json_hash(self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignAttemptOutcomeV1 {
    NoCandidate,
    SelectedPreHoldout,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignAttemptSettlementV1 {
    pub operation_id: String,
    pub reservation_sha256: String,
    /// The controller must verify the terminal Job and these immutable evidence
    /// bytes before settlement; a hash by itself is not execution evidence.
    pub evidence_sha256: String,
    pub outcome: CampaignAttemptOutcomeV1,
    /// None preserves the full reservation when terminal infrastructure failure
    /// leaves actual trial consumption unknown. It must never mean zero.
    pub consumed_trials: Option<u64>,
}

impl CampaignAttemptSettlementV1 {
    pub fn validate_against(&self, attempt: &CampaignAttemptReservationV1) -> Result<()> {
        if self.operation_id != attempt.operation_id()?
            || self.reservation_sha256 != attempt.content_hash()?
            || self
                .consumed_trials
                .is_some_and(|count| count > attempt.declared_trials)
            || (matches!(
                self.outcome,
                CampaignAttemptOutcomeV1::NoCandidate
                    | CampaignAttemptOutcomeV1::SelectedPreHoldout
            ) && self.consumed_trials.is_none())
        {
            return Err(CampaignControlError::Invalid(
                "attempt settlement binding or consumption",
            ));
        }
        digest(&self.evidence_sha256, 64)
    }
}

fn signing_message(hash: &str) -> String {
    format!("{ROOT_GRANT_SCHEMA}:{hash}")
}

fn digest(value: &str, length: usize) -> Result<()> {
    if value.len() != length
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(CampaignControlError::Invalid(
            "lowercase hexadecimal identity",
        ));
    }
    Ok(())
}

fn identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
    {
        return Err(CampaignControlError::Invalid("identifier"));
    }
    Ok(())
}

fn image(value: &str) -> Result<()> {
    let (repository, sha) = value
        .split_once("@sha256:")
        .ok_or(CampaignControlError::Invalid("digest-pinned image"))?;
    if repository.is_empty()
        || repository.contains('@')
        || repository.bytes().any(|b| b.is_ascii_whitespace())
    {
        return Err(CampaignControlError::Invalid("image repository"));
    }
    digest(sha, 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (CampaignRootGrantV1, SigningKey, DateTime<Utc>) {
        let now = DateTime::parse_from_rfc3339("2026-09-05T00:00:00Z")
            .unwrap()
            .to_utc();
        let grant = CampaignRootGrantV1 {
            schema_version: ROOT_GRANT_SCHEMA.into(),
            root_id: "root-1".into(),
            family: CampaignFamilyPolicyV1 {
                family_id: "study-1".into(),
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
            valid_from: now,
            expires_at: now + TimeDelta::hours(12),
        };
        (grant, SigningKey::from_bytes(&[7; 32]), now)
    }

    fn verified() -> (VerifiedCampaignRootGrant, DateTime<Utc>) {
        let (grant, key, now) = fixture();
        let signed = sign_campaign_root_grant(grant, "operator".into(), &key).unwrap();
        (
            verify_campaign_root_grant(
                &signed,
                &BTreeMap::from([("operator".into(), key.verifying_key())]),
                now,
            )
            .unwrap(),
            now,
        )
    }

    fn attempt(grant: &VerifiedCampaignRootGrant) -> CampaignAttemptReservationV1 {
        CampaignAttemptReservationV1 {
            schema_version: ATTEMPT_SCHEMA.into(),
            root_grant_sha256: grant.content_sha256().into(),
            family_id: "study-1".into(),
            campaign_id: "campaign-0".into(),
            execution: grant.grant().execution.clone(),
            generation: 0,
            parent_result_sha256: None,
            policy_revision_id: grant
                .grant()
                .allowed_policy_revision_ids
                .first()
                .unwrap()
                .clone(),
            request_sha256: "8".repeat(64),
            attempt_ordinal: 0,
            declared_trials: 44,
            reserved_job_seconds: 7200,
            reserved_llm_tokens: 1500,
        }
    }

    #[test]
    fn signature_binds_scope_and_rejects_untrusted_signer() {
        let (grant, key, now) = fixture();
        let mut signed = sign_campaign_root_grant(grant, "operator".into(), &key).unwrap();
        assert!(matches!(
            verify_campaign_root_grant(&signed, &BTreeMap::new(), now),
            Err(CampaignControlError::UntrustedSigner)
        ));
        let trusted = BTreeMap::from([("operator".into(), key.verifying_key())]);
        assert!(verify_campaign_root_grant(&signed, &trusted, now).is_ok());
        signed.grant.budget.max_trials += 1;
        signed.content_sha256 = signed.grant.content_hash().unwrap();
        assert!(matches!(
            verify_campaign_root_grant(&signed, &trusted, now),
            Err(CampaignControlError::InvalidSignature)
        ));
    }

    #[test]
    fn cached_verification_does_not_extend_expiry() {
        let (grant, now) = verified();
        let a = attempt(&grant);
        assert!(grant.validate_attempt_scope(&a, now).is_ok());
        assert!(matches!(
            grant.validate_attempt_scope(&a, grant.grant().expires_at),
            Err(CampaignControlError::InactiveGrant)
        ));
        assert!(matches!(
            grant.validate_attempt_scope(&a, now - TimeDelta::seconds(1)),
            Err(CampaignControlError::InactiveGrant)
        ));
        assert!(grant
            .validate_attempt_scope(&a, grant.grant().expires_at - TimeDelta::hours(2))
            .is_ok());
        assert!(grant
            .validate_attempt_scope(&a, grant.grant().expires_at - TimeDelta::seconds(1))
            .is_err());
    }

    #[test]
    fn attempts_cannot_change_family_policy_generation_or_grant() {
        let (grant, now) = verified();
        for change in 0..12 {
            let mut a = attempt(&grant);
            match change {
                0 => a.family_id = "other-family".into(),
                1 => a.policy_revision_id = format!("cex-search-policy-{}", "9".repeat(64)),
                2 => {
                    a.generation = 2;
                    a.parent_result_sha256 = Some("b".repeat(64));
                }
                3 => a.root_grant_sha256 = "c".repeat(64),
                4 => a.parent_result_sha256 = Some("b".repeat(64)),
                5 => a.execution.campaign_inputs_sha256 = "c".repeat(64),
                6 => a.execution.evaluation_protocol_sha256 = "d".repeat(64),
                7 => a.execution.source_revision = "e".repeat(40),
                8 => a.execution.runner_image = format!("registry/other@sha256:{}", "4".repeat(64)),
                9 => {
                    a.execution.controller_image =
                        format!("registry/other@sha256:{}", "5".repeat(64))
                }
                10 => a.execution.job_cpu_millis += 1,
                _ => a.execution.job_memory_mib += 1,
            }
            assert!(
                grant.validate_attempt_scope(&a, now).is_err(),
                "case {change}"
            );
        }
    }

    #[test]
    fn operation_identity_detects_conflicting_retransmission() {
        let (grant, _) = verified();
        let a = attempt(&grant);
        let mut changed = a.clone();
        changed.declared_trials += 1;
        assert_eq!(a.operation_id().unwrap(), changed.operation_id().unwrap());
        assert_ne!(a.content_hash().unwrap(), changed.content_hash().unwrap());
        changed.attempt_ordinal += 1;
        assert_ne!(a.operation_id().unwrap(), changed.operation_id().unwrap());
        assert_eq!(a.family_id, changed.family_id);
    }

    #[test]
    fn rejects_live_scope_unknown_fields_and_unpinned_images() {
        let (mut grant, _, _) = fixture();
        let mut value = serde_json::to_value(&grant).unwrap();
        value["execution_scope"] = "live".into();
        assert!(serde_json::from_value::<CampaignRootGrantV1>(value).is_err());
        let mut value = serde_json::to_value(&grant).unwrap();
        value["deployment_authority"] = true.into();
        assert!(serde_json::from_value::<CampaignRootGrantV1>(value).is_err());
        grant.execution.runner_image = "registry/research:latest".into();
        assert!(grant.validate().is_err());
    }

    #[test]
    fn rejects_per_attempt_excess_and_family_ceiling_escalation() {
        let (grant, now) = verified();
        for charge in 0..4 {
            let mut a = attempt(&grant);
            match charge {
                0 => a.declared_trials = 101,
                1 => a.reserved_job_seconds = 14401,
                2 => a.reserved_llm_tokens = 3001,
                _ => a.attempt_ordinal = 2,
            }
            assert!(grant.validate_attempt_scope(&a, now).is_err());
        }
        let mut changed = grant.grant().clone();
        changed.budget.max_trials = changed.family.max_trials + 1;
        assert!(changed.validate().is_err());
    }

    #[test]
    fn view_exposure_is_signed_and_cannot_claim_independent_selection() {
        let (grant, _) = verified();
        assert_eq!(
            grant.grant().execution.evaluation_views.evidence_label(),
            "search_visible_validation"
        );
        let mut changed = grant.grant().clone();
        changed.execution.evaluation_views.selection_view_sha256 = "c".repeat(64);
        assert!(changed.validate().is_err());
        let mut value = serde_json::to_value(grant.grant()).unwrap();
        value["execution"]["evaluation_views"]["selection_feedback"] =
            "independent_selection".into();
        assert!(serde_json::from_value::<CampaignRootGrantV1>(value).is_err());
        let (mut grant, key, now) = fixture();
        let signed = sign_campaign_root_grant(grant.clone(), "operator".into(), &key).unwrap();
        grant.execution.evaluation_views.search_view_sha256 = "c".repeat(64);
        grant.execution.evaluation_views.selection_view_sha256 = "c".repeat(64);
        assert_ne!(signed.content_sha256, grant.content_hash().unwrap());
        let mut changed_signed = signed;
        changed_signed.grant = grant;
        changed_signed.content_sha256 = changed_signed.grant.content_hash().unwrap();
        assert!(verify_campaign_root_grant(
            &changed_signed,
            &BTreeMap::from([("operator".into(), key.verifying_key())]),
            now
        )
        .is_err());
    }

    #[test]
    fn generation_ceiling_comes_from_the_signed_grant() {
        let (mut grant, key, now) = fixture();
        grant.max_follow_ups = 5;
        let signed = sign_campaign_root_grant(grant, "operator".into(), &key).unwrap();
        let verified = verify_campaign_root_grant(
            &signed,
            &BTreeMap::from([("operator".into(), key.verifying_key())]),
            now,
        )
        .unwrap();
        let mut a = attempt(&verified);
        a.generation = 5;
        a.parent_result_sha256 = Some("b".repeat(64));
        assert!(verified.validate_attempt_scope(&a, now).is_ok());
        a.generation = 6;
        assert!(verified.validate_attempt_scope(&a, now).is_err());
    }

    #[test]
    fn settlement_keeps_unknown_consumption_distinct_from_zero() {
        let (grant, _) = verified();
        let reservation = attempt(&grant);
        let mut settlement = CampaignAttemptSettlementV1 {
            operation_id: reservation.operation_id().unwrap(),
            reservation_sha256: reservation.content_hash().unwrap(),
            evidence_sha256: "d".repeat(64),
            outcome: CampaignAttemptOutcomeV1::Failed,
            consumed_trials: None,
        };
        assert!(settlement.validate_against(&reservation).is_ok());
        settlement.outcome = CampaignAttemptOutcomeV1::NoCandidate;
        assert!(settlement.validate_against(&reservation).is_err());
        settlement.consumed_trials = Some(0);
        assert!(settlement.validate_against(&reservation).is_ok());
        settlement.consumed_trials = Some(reservation.declared_trials + 1);
        assert!(settlement.validate_against(&reservation).is_err());
    }

    #[test]
    fn settlement_is_bound_to_the_exact_reservation() {
        let (grant, _) = verified();
        let reservation = attempt(&grant);
        let settlement = CampaignAttemptSettlementV1 {
            operation_id: reservation.operation_id().unwrap(),
            reservation_sha256: reservation.content_hash().unwrap(),
            evidence_sha256: "d".repeat(64),
            outcome: CampaignAttemptOutcomeV1::NoCandidate,
            consumed_trials: Some(10),
        };
        assert!(settlement.validate_against(&reservation).is_ok());
        let mut changed = reservation.clone();
        changed.declared_trials += 1;
        assert!(settlement.validate_against(&changed).is_err());
        changed = reservation;
        changed.attempt_ordinal += 1;
        assert!(settlement.validate_against(&changed).is_err());
    }
}
