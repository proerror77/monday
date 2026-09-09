//! Signed, finite Campaign study scopes shared by multiple root grants.
//!
//! A study grant is a separately signed authority.  It does not change the
//! historical root-grant wire format; instead it binds a finite set of already
//! signed roots to one immutable semantic scope and cumulative budget.

use crate::campaign_control::{
    CampaignControlError, CampaignExecutionBindingV1, CampaignExecutionScope, CampaignRootGrantV1,
};
use crate::canonical_json_hash;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const STUDY_GRANT_SCHEMA: &str = "monday.campaign_study_grant.v1";

type Result<T> = std::result::Result<T, CampaignControlError>;

/// A predeclared study member.  The root hash is the authority identity; all
/// other fields make the semantic scope explicit so a future label-horizon or
/// input/protocol change cannot be smuggled into an existing study.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignStudyMemberV1 {
    pub family_id: String,
    pub root_grant_sha256: String,
    pub family_definition_sha256: String,
    pub family_max_trials: u64,
    pub execution_scope: CampaignExecutionScope,
    pub execution: CampaignExecutionBindingV1,
    /// Canonical hash of the typed label/horizon semantics.  Root V1 predates
    /// this field, so the study sidecar owns the explicit versioned binding.
    pub label_horizon_sha256: String,
}

impl CampaignStudyMemberV1 {
    pub fn validate(&self) -> Result<()> {
        identifier(&self.family_id, "family ID")?;
        digest(&self.root_grant_sha256, "root grant")?;
        digest(&self.family_definition_sha256, "family definition")?;
        if self.family_max_trials == 0 {
            return Err(CampaignControlError::Invalid("family trial ceiling"));
        }
        self.execution.validate()?;
        digest(&self.label_horizon_sha256, "label horizon")?;
        Ok(())
    }

    pub fn content_hash(&self) -> Result<String> {
        self.validate()?;
        Ok(canonical_json_hash(self)?)
    }

    /// Match a member against the exact historical root grant bytes selected
    /// by the caller.  The caller supplies the root content hash read from the
    /// verified root authority, rather than trusting a duplicated field.
    pub fn matches_root(&self, root: &CampaignRootGrantV1, root_hash: &str) -> bool {
        self.root_grant_sha256 == root_hash
            && self.family_id == root.family.family_id
            && self.family_definition_sha256 == root.family.definition_sha256
            && self.family_max_trials == root.family.max_trials
            && self.execution_scope == root.execution_scope
            && self.execution == root.execution
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignStudyBudgetV1 {
    pub max_trials: u64,
    pub max_job_attempts: u64,
    pub max_job_seconds: u64,
    pub max_llm_tokens: u64,
}

impl CampaignStudyBudgetV1 {
    fn validate(&self) -> Result<()> {
        if self.max_trials == 0
            || self.max_job_attempts == 0
            || self.max_job_seconds == 0
            || self.max_llm_tokens == 0
        {
            return Err(CampaignControlError::Invalid("study budget"));
        }
        Ok(())
    }
}

/// One immutable, finite set of root grants sharing cumulative usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignStudyGrantV1 {
    pub schema_version: String,
    pub study_id: String,
    pub members: Vec<CampaignStudyMemberV1>,
    pub budget: CampaignStudyBudgetV1,
    pub valid_from: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl CampaignStudyGrantV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != STUDY_GRANT_SCHEMA {
            return Err(CampaignControlError::Invalid("study schema"));
        }
        identifier(&self.study_id, "study ID")?;
        if self.members.is_empty() || self.members.len() > 64 {
            return Err(CampaignControlError::Invalid("study members"));
        }
        self.budget.validate()?;
        if self.valid_from >= self.expires_at {
            return Err(CampaignControlError::Invalid("study validity window"));
        }
        let mut families = BTreeSet::new();
        let mut roots = BTreeSet::new();
        for member in &self.members {
            member.validate()?;
            if !families.insert(&member.family_id) || !roots.insert(&member.root_grant_sha256) {
                return Err(CampaignControlError::Invalid(
                    "study members must have unique families and roots",
                ));
            }
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
pub struct SignedCampaignStudyGrantV1 {
    pub grant: CampaignStudyGrantV1,
    /// The signed key ID is the issuer identity, matching the existing root
    /// grant convention and the approval signer binding in the store.
    pub key_id: String,
    pub content_sha256: String,
    pub signature_hex: String,
}

/// Deliberately not deserializable: untrusted study JSON becomes authority
/// only after signature, content, key, and active-window verification.
#[derive(Debug, Clone)]
pub struct VerifiedCampaignStudyGrant {
    grant: CampaignStudyGrantV1,
    content_sha256: String,
    signed: SignedCampaignStudyGrantV1,
    verifying_key: VerifyingKey,
}

impl VerifiedCampaignStudyGrant {
    pub fn grant(&self) -> &CampaignStudyGrantV1 {
        &self.grant
    }

    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }

    pub fn signed_grant(&self) -> &SignedCampaignStudyGrantV1 {
        &self.signed
    }

    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    pub fn validate_active_at(&self, now: DateTime<Utc>) -> Result<()> {
        if now < self.grant.valid_from || now >= self.grant.expires_at {
            return Err(CampaignControlError::InactiveGrant);
        }
        Ok(())
    }

    pub fn member(&self, family_id: &str) -> Option<&CampaignStudyMemberV1> {
        self.grant
            .members
            .iter()
            .find(|member| member.family_id == family_id)
    }
}

pub fn sign_campaign_study_grant(
    grant: CampaignStudyGrantV1,
    key_id: String,
    key: &SigningKey,
) -> Result<SignedCampaignStudyGrantV1> {
    identifier(&key_id, "study signer")?;
    let content_sha256 = grant.content_hash()?;
    let signature = key.sign(signing_message(&content_sha256).as_bytes());
    Ok(SignedCampaignStudyGrantV1 {
        grant,
        key_id,
        content_sha256,
        signature_hex: hex::encode(signature.to_bytes()),
    })
}

pub fn verify_campaign_study_grant(
    signed: &SignedCampaignStudyGrantV1,
    trusted_keys: &BTreeMap<String, VerifyingKey>,
    now: DateTime<Utc>,
) -> Result<VerifiedCampaignStudyGrant> {
    identifier(&signed.key_id, "study signer")?;
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
    let verified = VerifiedCampaignStudyGrant {
        grant: signed.grant.clone(),
        content_sha256: signed.content_sha256.clone(),
        signed: signed.clone(),
        verifying_key: *key,
    };
    verified.validate_active_at(now)?;
    Ok(verified)
}

fn signing_message(hash: &str) -> String {
    format!("{STUDY_GRANT_SCHEMA}:{hash}")
}

fn digest(value: &str, what: &'static str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(CampaignControlError::Invalid(what));
    }
    Ok(())
}

fn identifier(value: &str, _what: &'static str) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::campaign_control::{
        CampaignEvaluationViewsV1, CampaignExecutionScope, CampaignFamilyPolicyV1,
        CampaignRootBudgetV1, CampaignRootGrantV1, CampaignSelectionFeedbackV1, ROOT_GRANT_SCHEMA,
    };
    use chrono::TimeDelta;
    use ed25519_dalek::SigningKey;

    fn root() -> CampaignRootGrantV1 {
        CampaignRootGrantV1 {
            schema_version: ROOT_GRANT_SCHEMA.into(),
            root_id: "root-1".into(),
            family: CampaignFamilyPolicyV1 {
                family_id: "family-1".into(),
                definition_sha256: "1".repeat(64),
                max_trials: 100,
            },
            execution_scope: CampaignExecutionScope::PreHoldout,
            execution: crate::campaign_control::CampaignExecutionBindingV1 {
                campaign_inputs_sha256: "2".repeat(64),
                evaluation_protocol_sha256: "3".repeat(64),
                evaluation_views: CampaignEvaluationViewsV1 {
                    search_view_sha256: "4".repeat(64),
                    selection_view_sha256: "4".repeat(64),
                    selection_feedback:
                        CampaignSelectionFeedbackV1::SearchAndLearningVisibleWalkForward,
                },
                source_revision: "a".repeat(40),
                runner_image: format!("registry/runner@sha256:{}", "5".repeat(64)),
                controller_image: format!("registry/controller@sha256:{}", "6".repeat(64)),
                job_cpu_millis: 1,
                job_memory_mib: 1,
            },
            allowed_policy_revision_ids: BTreeSet::from([format!(
                "cex-search-policy-{}",
                "7".repeat(64)
            )]),
            max_follow_ups: 0,
            budget: CampaignRootBudgetV1 {
                max_trials: 100,
                max_job_attempts: 2,
                max_job_seconds: 10,
                max_llm_tokens: 10,
            },
            valid_from: DateTime::parse_from_rfc3339("2026-09-05T00:00:00Z")
                .unwrap()
                .to_utc(),
            expires_at: DateTime::parse_from_rfc3339("2026-09-05T01:00:00Z")
                .unwrap()
                .to_utc(),
        }
    }

    fn grant() -> CampaignStudyGrantV1 {
        let root = root();
        CampaignStudyGrantV1 {
            schema_version: STUDY_GRANT_SCHEMA.into(),
            study_id: "study-1".into(),
            members: vec![CampaignStudyMemberV1 {
                family_id: root.family.family_id.clone(),
                root_grant_sha256: "8".repeat(64),
                family_definition_sha256: root.family.definition_sha256,
                family_max_trials: root.family.max_trials,
                execution_scope: root.execution_scope,
                execution: root.execution,
                label_horizon_sha256: "9".repeat(64),
            }],
            budget: CampaignStudyBudgetV1 {
                max_trials: 10,
                max_job_attempts: 2,
                max_job_seconds: 10,
                max_llm_tokens: 10,
            },
            valid_from: DateTime::parse_from_rfc3339("2026-09-05T00:00:00Z")
                .unwrap()
                .to_utc(),
            expires_at: DateTime::parse_from_rfc3339("2026-09-05T01:00:00Z")
                .unwrap()
                .to_utc(),
        }
    }

    #[test]
    fn study_grant_signs_and_verifies_with_a_distinct_domain() {
        let grant = grant();
        let key = SigningKey::from_bytes(&[7; 32]);
        let signed = sign_campaign_study_grant(grant, "operator".into(), &key).unwrap();
        let now = signed.grant.valid_from + TimeDelta::seconds(1);
        let verified = verify_campaign_study_grant(
            &signed,
            &BTreeMap::from([("operator".into(), key.verifying_key())]),
            now,
        )
        .unwrap();
        assert_eq!(verified.content_sha256(), signed.content_sha256);
        let mut root_signed = signed.clone();
        root_signed.signature_hex = hex::encode(
            key.sign(
                format!(
                    "monday.campaign_root_grant.v1:{}",
                    root_signed.content_sha256
                )
                .as_bytes(),
            )
            .to_bytes(),
        );
        assert!(verify_campaign_study_grant(
            &root_signed,
            &BTreeMap::from([("operator".into(), key.verifying_key())]),
            now,
        )
        .is_err());
    }

    #[test]
    fn duplicate_family_or_root_member_is_rejected() {
        let mut grant = grant();
        grant.members.push(grant.members[0].clone());
        assert!(grant.validate().is_err());
    }
}
