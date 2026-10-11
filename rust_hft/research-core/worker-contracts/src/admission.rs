//! Native Campaign budget authority projects an exact, already reserved request.
//! The generic task API cannot manufacture or widen this projection.
use crate::{
    identity,
    research::Run,
    task::{Admission, TaskKind},
    valid_digest,
};
use anyhow::{ensure, Context, Result};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const NATIVE_ADMISSION_DOMAIN: &str = "monday.native_scientific_admission.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeAdmission {
    pub schema: String,
    pub tenant: String,
    pub run: Run,
    pub admission: Admission,
    pub operation_sha256: String,
    pub native_request_sha256: String,
    pub family_id: String,
    pub root_grant_sha256: String,
    pub approval_sha256: String,
    pub transfer_receipt_sha256: String,
    pub declared_trials: u64,
    pub reserved_job_seconds: u64,
    pub reserved_llm_tokens: u64,
    pub issued_ms: i64,
    pub expires_ms: i64,
}

impl NativeAdmission {
    pub fn validate(&self) -> Result<()> {
        let spec = &self.admission.task_spec;
        self.admission.validate(spec)?;
        self.run.admit(spec)?;
        ensure!(
            self.schema == NATIVE_ADMISSION_DOMAIN
                && !self.tenant.is_empty()
                && self.tenant.len() <= 128
                && !self.family_id.is_empty()
                && self.family_id.len() <= 128,
            "invalid native admission scope"
        );
        ensure!(
            [
                &self.operation_sha256,
                &self.native_request_sha256,
                &self.root_grant_sha256,
                &self.approval_sha256,
                &self.transfer_receipt_sha256
            ]
            .into_iter()
            .all(|s| valid_digest(s)),
            "incomplete native budget evidence"
        );
        ensure!(
            self.admission.max_attempts == spec.max_attempts
                && self.run.configuration_sha256 == self.native_request_sha256,
            "native admission changed the fixed request or reserved attempt count"
        );
        // The configuration identity is the finalized canonical Campaign request,
        // not a caller's arbitrary argv or a diagnostic execution surface.
        if spec.kind == TaskKind::CexCampaign {
            ensure!(
                spec.max_attempts == 1,
                "a native Campaign transfer covers one source reservation"
            );
            ensure!(
                spec.command.get(1).map(String::as_str) == Some("mission")
                    && spec.command.get(2).map(String::as_str) == Some("campaign-execute")
                    && spec
                        .command
                        .iter()
                        .filter(|s| s.as_str() == "--pre-holdout")
                        .count()
                        == 1,
                "native admission requires the canonical Campaign worker command"
            );
            let identities: Vec<_> = spec
                .command
                .windows(2)
                .filter(|pair| pair[0] == "--request-sha256")
                .map(|pair| pair[1].as_str())
                .collect();
            ensure!(
                identities == [self.native_request_sha256.as_str()],
                "worker configuration differs from native request"
            );
        }
        ensure!(
            self.declared_trials > 0
                && self.reserved_job_seconds > 0
                && u64::try_from((spec.timeout_ms + 999) / 1000)?
                    .checked_mul(u64::from(spec.max_attempts))
                    == Some(self.reserved_job_seconds),
            "task changed native reserved worst-case Job duration"
        );
        ensure!(
            self.issued_ms > 0
                && self.expires_ms > self.issued_ms
                && self
                    .issued_ms
                    .checked_add(spec.timeout_ms)
                    .is_some_and(|end| end <= self.expires_ms),
            "native approval cannot cover the requested Job duration"
        );
        Ok(())
    }
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        identity(self)
    }
    pub fn active_at(&self, now_ms: i64) -> Result<()> {
        self.validate()?;
        ensure!(
            now_ms >= self.issued_ms && now_ms < self.expires_ms,
            "native admission expired or not yet active"
        );
        Ok(())
    }
    pub fn admits_launch_at(&self, now_ms: i64) -> Result<()> {
        self.active_at(now_ms)?;
        ensure!(
            now_ms
                .checked_add(self.admission.task_spec.timeout_ms)
                .is_some_and(|end| end <= self.expires_ms),
            "Job would exceed native approval expiry"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedNativeAdmission {
    pub evidence: NativeAdmission,
    pub key_id: String,
    pub evidence_sha256: String,
    pub signature_hex: String,
}

pub fn native_signing_bytes(domain: &str, key_id: &str, evidence_sha256: &str) -> Result<Vec<u8>> {
    ensure!(
        !key_id.is_empty() && key_id.len() <= 128 && valid_digest(evidence_sha256),
        "invalid native signer or evidence identity"
    );
    Ok(serde_json::to_vec(&(domain, key_id, evidence_sha256))?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAdmissionTrust {
    pub schema: String,
    /// Distinct from Build/release signing trust and never chosen by an Agent.
    pub native_reservation_keys: BTreeMap<String, String>,
}

pub struct VerifiedNativeAdmission {
    signed: SignedNativeAdmission,
    trust_sha256: String,
    trust_document: serde_json::Value,
}
impl VerifiedNativeAdmission {
    pub fn evidence(&self) -> &NativeAdmission {
        &self.signed.evidence
    }
    pub fn signed(&self) -> &SignedNativeAdmission {
        &self.signed
    }
    pub fn trust_sha256(&self) -> &str {
        &self.trust_sha256
    }
    /// Public verifier trust bytes, retained for immutable control-ledger import.
    pub fn trust_document(&self) -> &serde_json::Value {
        &self.trust_document
    }
}
impl NativeAdmissionTrust {
    /// Shared strict native witness verification. A valid signature proves
    /// only this purpose and body, never a new grant or source publication.
    pub fn verify_native_signature(
        &self,
        domain: &str,
        key_id: &str,
        evidence_sha256: &str,
        signature_hex: &str,
    ) -> Result<String> {
        ensure!(
            self.schema == "monday.native_reservation_trust.v1"
                && !self.native_reservation_keys.is_empty()
                && self.native_reservation_keys.len() <= 32,
            "native reservation trust required"
        );
        let public = self
            .native_reservation_keys
            .get(key_id)
            .context("untrusted native reservation issuer")?;
        let decode = |s: &str, bytes: usize| -> Result<Vec<u8>> {
            ensure!(
                s.len() == bytes * 2
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid native signature encoding"
            );
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
                .collect()
        };
        let bytes: [u8; 32] = decode(public, 32)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid native public key"))?;
        let signature = Signature::from_slice(&decode(signature_hex, 64)?)?;
        VerifyingKey::from_bytes(&bytes)?.verify_strict(
            &native_signing_bytes(domain, key_id, evidence_sha256)?,
            &signature,
        )?;
        identity(self)
    }

    pub fn verify(&self, signed: &SignedNativeAdmission) -> Result<VerifiedNativeAdmission> {
        ensure!(
            signed.evidence.id()? == signed.evidence_sha256,
            "native evidence changed"
        );
        let trust_sha256 = self.verify_native_signature(
            NATIVE_ADMISSION_DOMAIN,
            &signed.key_id,
            &signed.evidence_sha256,
            &signed.signature_hex,
        )?;
        Ok(VerifiedNativeAdmission {
            signed: signed.clone(),
            trust_sha256,
            trust_document: serde_json::to_value(self)?,
        })
    }
}
