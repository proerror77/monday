//! Short-lived, nonce-bound permission to start one stage of an already charged Job.
//! The controller owns the signing key. A permit never reserves another attempt.
use crate::{canonical_json_hash, market_encoder_study::MarketTrainingStageKeyV1};
use chrono::{DateTime, TimeDelta, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
pub const AUTHORITY_SCHEMA: &str = "monday.campaign_stage_authority.v1";
pub const REQUEST_SCHEMA: &str = "monday.campaign_stage_request.v1";
pub const PERMIT_SCHEMA: &str = "monday.campaign_stage_permit.v1";
pub const PERMIT_SECONDS: i64 = 30;
pub const REQUEST_SECONDS: i64 = 120;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignStageAuthorityV1 {
    pub schema_version: String,
    pub public_key_hex: String,
    pub work_pvc_name: String,
    pub work_pvc_uid: String,
}
impl CampaignStageAuthorityV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != AUTHORITY_SCHEMA
            || !digest(&self.public_key_hex)
            || self.work_pvc_name.is_empty()
            || self.work_pvc_name.len() > 63
            || !self
                .work_pvc_name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || self.work_pvc_name.starts_with('-')
            || self.work_pvc_name.ends_with('-')
            || !identifier(&self.work_pvc_uid)
        {
            return Err("invalid Campaign stage authority or work PVC".into());
        }
        self.key()?;
        Ok(())
    }
    pub fn key(&self) -> Result<VerifyingKey, String> {
        let key: [u8; 32] = hex::decode(&self.public_key_hex)
            .map_err(|e| e.to_string())?
            .try_into()
            .map_err(|_| "invalid stage public key length")?;
        let key = VerifyingKey::from_bytes(&key).map_err(|e| e.to_string())?;
        if key.is_weak() {
            return Err("weak stage public key".into());
        }
        Ok(key)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignStageRequestV1 {
    pub schema_version: String,
    pub request_sha256: String,
    pub attempt_sha256: String,
    pub root_grant_sha256: String,
    pub job_name: String,
    pub pod_uid: String,
    pub stage: MarketTrainingStageKeyV1,
    pub nonce: String,
    pub requested_at: DateTime<Utc>,
}
impl CampaignStageRequestV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != REQUEST_SCHEMA
            || !identifier(&self.job_name)
            || !identifier(&self.pod_uid)
            || [
                &self.request_sha256,
                &self.attempt_sha256,
                &self.root_grant_sha256,
                &self.nonce,
            ]
            .iter()
            .any(|s| !digest(s))
        {
            return Err("invalid Campaign stage request identity".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedCampaignStagePermitV1 {
    pub schema_version: String,
    pub request: CampaignStageRequestV1,
    pub job_uid: String,
    pub job_deadline_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub signature_hex: String,
}
fn message(permit: &SignedCampaignStagePermitV1) -> Result<Vec<u8>, String> {
    let mut unsigned = permit.clone();
    unsigned.signature_hex.clear();
    Ok(format!(
        "monday.campaign-stage-permit.v1:{}",
        canonical_json_hash(&unsigned).map_err(|e| e.to_string())?
    )
    .into_bytes())
}
pub fn sign_stage_permit(
    authority: &CampaignStageAuthorityV1,
    request: CampaignStageRequestV1,
    job_uid: String,
    job_deadline_at: DateTime<Utc>,
    now: DateTime<Utc>,
    key: &SigningKey,
) -> Result<SignedCampaignStagePermitV1, String> {
    sign_stage_permit_bounded(
        authority,
        request,
        job_uid,
        job_deadline_at,
        job_deadline_at,
        now,
        key,
    )
}
/// A scheduled Root/Study revocation may shorten permission without changing
/// the original Job deadline recorded by every completed stage.
pub fn sign_stage_permit_bounded(
    authority: &CampaignStageAuthorityV1,
    request: CampaignStageRequestV1,
    job_uid: String,
    job_deadline_at: DateTime<Utc>,
    authority_deadline_at: DateTime<Utc>,
    now: DateTime<Utc>,
    key: &SigningKey,
) -> Result<SignedCampaignStagePermitV1, String> {
    authority.validate()?;
    if key.verifying_key() != authority.key()? {
        return Err("stage signing key differs from frozen authority".into());
    }
    let mut permit = SignedCampaignStagePermitV1 {
        schema_version: PERMIT_SCHEMA.into(),
        request,
        job_uid,
        job_deadline_at,
        issued_at: now,
        expires_at: (now + TimeDelta::seconds(PERMIT_SECONDS))
            .min(job_deadline_at)
            .min(authority_deadline_at),
        signature_hex: String::new(),
    };
    permit.signature_hex = hex::encode(key.sign(&message(&permit)?).to_bytes());
    verify_stage_permit(authority, &permit.request, &permit, now)?;
    Ok(permit)
}
pub fn verify_stage_permit(
    authority: &CampaignStageAuthorityV1,
    expected: &CampaignStageRequestV1,
    permit: &SignedCampaignStagePermitV1,
    now: DateTime<Utc>,
) -> Result<(), String> {
    authority.validate()?;
    expected.validate()?;
    if permit.schema_version != PERMIT_SCHEMA
        || permit.request != *expected
        || !identifier(&permit.job_uid)
        || permit.issued_at < expected.requested_at
        || permit.issued_at > expected.requested_at + TimeDelta::seconds(REQUEST_SECONDS)
        || permit.expires_at <= permit.issued_at
        || permit.expires_at > permit.issued_at + TimeDelta::seconds(PERMIT_SECONDS)
        || permit.expires_at > permit.job_deadline_at
        || now < permit.issued_at
        || now >= permit.expires_at
    {
        return Err("stage permit identity, freshness or original deadline differs".into());
    }
    let signature =
        Signature::from_slice(&hex::decode(&permit.signature_hex).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    authority
        .key()?
        .verify_strict(&message(permit)?, &signature)
        .map_err(|e| e.to_string())
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_encoder_study::{MarketTrainingStageKindV1, MarketTrainingStagePurposeV1};
    #[test]
    fn campaign_stage_permit_binds_nonce_writer_stage_and_original_deadline() {
        let key = SigningKey::from_bytes(&[73; 32]);
        let authority = CampaignStageAuthorityV1 {
            schema_version: AUTHORITY_SCHEMA.into(),
            public_key_hex: hex::encode(key.verifying_key().as_bytes()),
            work_pvc_name: "market-work".into(),
            work_pvc_uid: "work-uid".into(),
        };
        let now = Utc::now();
        let request = CampaignStageRequestV1 {
            schema_version: REQUEST_SCHEMA.into(),
            request_sha256: "a".repeat(64),
            attempt_sha256: "b".repeat(64),
            root_grant_sha256: "c".repeat(64),
            job_name: "market-job".into(),
            pod_uid: "pod-uid".into(),
            stage: MarketTrainingStageKeyV1 {
                fold_id: 1,
                kind: MarketTrainingStageKindV1::Pretrain,
                seed: 7,
                purpose: MarketTrainingStagePurposeV1::Primary,
            },
            nonce: "d".repeat(64),
            requested_at: now,
        };
        let deadline = now + TimeDelta::seconds(10);
        let permit = sign_stage_permit(
            &authority,
            request.clone(),
            "job-uid".into(),
            deadline,
            now,
            &key,
        )
        .unwrap();
        assert_eq!(permit.expires_at, deadline);
        let bounded = sign_stage_permit_bounded(
            &authority,
            request.clone(),
            "job-uid".into(),
            now + TimeDelta::hours(1),
            deadline,
            now,
            &key,
        )
        .unwrap();
        assert_eq!(bounded.job_deadline_at, now + TimeDelta::hours(1));
        assert_eq!(bounded.expires_at, deadline);
        verify_stage_permit(&authority, &request, &permit, now + TimeDelta::seconds(9)).unwrap();
        assert!(verify_stage_permit(&authority, &request, &permit, deadline).is_err());
        for field in ["nonce", "attempt_sha256", "pod_uid", "job_name"] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[field] = serde_json::json!("e".repeat(64));
            let other = serde_json::from_value(value).unwrap();
            assert!(verify_stage_permit(&authority, &other, &permit, now).is_err());
        }
        let mut altered = permit.clone();
        altered.job_uid = "replacement-job".into();
        assert!(verify_stage_permit(&authority, &request, &altered, now).is_err());
        let mut altered = permit;
        altered.job_deadline_at += TimeDelta::hours(1);
        assert!(verify_stage_permit(&authority, &request, &altered, now).is_err());
        assert!(sign_stage_permit(
            &authority,
            request,
            "job-uid".into(),
            now + TimeDelta::hours(1),
            now + TimeDelta::seconds(REQUEST_SECONDS + 1),
            &key
        )
        .is_err());
    }
}
