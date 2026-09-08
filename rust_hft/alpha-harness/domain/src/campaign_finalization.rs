//! Separate authority to close search and evaluate its frozen results once.
//! A root grant cannot be converted into this authority. No order permission is
//! representable here; actual admission also requires an authenticated closed
//! family ledger, current approval, and publication readback.
use crate::{
    campaign_control::{
        digest, identifier, CampaignControlError, CampaignExecutionBindingV1,
        CampaignSelectionFeedbackV1,
    },
    canonical_json_hash,
};
use chrono::{DateTime, TimeDelta, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

type Result<T> = std::result::Result<T, CampaignControlError>;
pub const FINAL_EVALUATION_GRANT_SCHEMA: &str = "monday.campaign_final_evaluation_grant.v1";
pub const MAX_FINAL_CANDIDATES: u32 = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignFinalEvaluationGrantV1 {
    pub schema_version: String,
    pub grant_id: String,
    pub family_id: String,
    pub family_definition_sha256: String,
    /// Compare-and-close against the exact settled ledger tail.
    pub family_head_sha256: String,
    pub execution: CampaignExecutionBindingV1,
    /// Every selected terminal result in the family, keyed by attempt identity.
    /// The ledger checks equality, not just membership of the requested subset.
    pub selected_results: BTreeMap<String, String>,
    /// Includes every candidate that the final evaluator scores; no refit/search.
    pub max_candidates: u32,
    /// Exactly one final evaluation Job is permitted by this contract.
    pub max_job_seconds: u64,
    pub valid_from: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl CampaignFinalEvaluationGrantV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != FINAL_EVALUATION_GRANT_SCHEMA {
            return Err(CampaignControlError::Invalid("final evaluation schema"));
        }
        identifier(&self.grant_id)?;
        identifier(&self.family_id)?;
        digest(&self.family_definition_sha256, 64)?;
        digest(&self.family_head_sha256, 64)?;
        self.execution.validate()?;
        if self.execution.evaluation_views.selection_feedback
            != CampaignSelectionFeedbackV1::IndependentSelectionWithheld
            || self.selected_results.is_empty()
            || self.selected_results.len() > MAX_FINAL_CANDIDATES as usize
            || (self.max_candidates as usize) < self.selected_results.len()
            || self.max_candidates == 0
            || self.max_candidates > MAX_FINAL_CANDIDATES
            || self.max_job_seconds == 0
            || self.valid_from >= self.expires_at
        {
            return Err(CampaignControlError::Invalid(
                "final evaluation scope or budget",
            ));
        }
        for (operation, result) in &self.selected_results {
            digest(
                operation.strip_prefix("campaign-attempt-").ok_or(
                    CampaignControlError::Invalid("final result attempt identity"),
                )?,
                64,
            )?;
            digest(result, 64)?;
        }
        let duration = i64::try_from(self.max_job_seconds)
            .ok()
            .and_then(TimeDelta::try_seconds)
            .ok_or(CampaignControlError::Invalid("final evaluation deadline"))?;
        if self
            .valid_from
            .checked_add_signed(duration)
            .is_none_or(|end| end > self.expires_at)
        {
            return Err(CampaignControlError::Invalid(
                "final evaluation deadline exceeds expiry",
            ));
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
pub struct SignedCampaignFinalEvaluationGrantV1 {
    pub grant: CampaignFinalEvaluationGrantV1,
    pub key_id: String,
    pub content_sha256: String,
    pub signature_hex: String,
}

/// No public constructor or Deserialize implementation: JSON is not authority.
#[derive(Debug, Clone)]
pub struct VerifiedCampaignFinalEvaluationGrant {
    signed: SignedCampaignFinalEvaluationGrantV1,
    verifying_key: VerifyingKey,
}

impl VerifiedCampaignFinalEvaluationGrant {
    pub fn grant(&self) -> &CampaignFinalEvaluationGrantV1 {
        &self.signed.grant
    }
    pub fn signed_grant(&self) -> &SignedCampaignFinalEvaluationGrantV1 {
        &self.signed
    }
    pub fn content_sha256(&self) -> &str {
        &self.signed.content_sha256
    }
    pub fn verifying_key(&self) -> VerifyingKey {
        self.verifying_key
    }
    pub fn validate_active_at(&self, at: DateTime<Utc>) -> Result<()> {
        if at < self.grant().valid_from || at >= self.grant().expires_at {
            return Err(CampaignControlError::InactiveGrant);
        }
        Ok(())
    }
    pub fn validate_job_deadline_at(&self, at: DateTime<Utc>) -> Result<()> {
        self.validate_active_at(at)?;
        let duration = i64::try_from(self.grant().max_job_seconds)
            .ok()
            .and_then(TimeDelta::try_seconds)
            .ok_or(CampaignControlError::Invalid("final evaluation deadline"))?;
        if at
            .checked_add_signed(duration)
            .is_none_or(|end| end > self.grant().expires_at)
        {
            return Err(CampaignControlError::Invalid(
                "final Job exceeds grant expiry",
            ));
        }
        Ok(())
    }
}

pub fn sign_campaign_final_evaluation_grant(
    grant: CampaignFinalEvaluationGrantV1,
    key_id: String,
    key: &SigningKey,
) -> Result<SignedCampaignFinalEvaluationGrantV1> {
    identifier(&key_id)?;
    let content_sha256 = grant.content_hash()?;
    let signature = key.sign(signing_message(&content_sha256).as_bytes());
    Ok(SignedCampaignFinalEvaluationGrantV1 {
        grant,
        key_id,
        content_sha256,
        signature_hex: hex::encode(signature.to_bytes()),
    })
}

pub fn verify_campaign_final_evaluation_grant(
    signed: &SignedCampaignFinalEvaluationGrantV1,
    trusted_keys: &BTreeMap<String, VerifyingKey>,
    at: DateTime<Utc>,
) -> Result<VerifiedCampaignFinalEvaluationGrant> {
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
    let verified = VerifiedCampaignFinalEvaluationGrant {
        signed: signed.clone(),
        verifying_key: *key,
    };
    verified.validate_active_at(at)?;
    Ok(verified)
}

fn signing_message(hash: &str) -> String {
    format!("{FINAL_EVALUATION_GRANT_SCHEMA}:{hash}")
}
