//! A source-published terminal audit witness is evidence for mechanical
//! retirement only. It grants no execution, refund, budget or admission.
//! A receiver must separately verify the fixed PG terminal/native/source/Run,
//! attempt, fence, UIDs and actual receipt/archive readback before retirement.
use crate::{admission::NativeAdmissionTrust, identity, valid_digest};
use anyhow::{ensure, Result};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

pub const NATIVE_TERMINAL_AUDIT_SCHEMA: &str = "monday.native_terminal_audit_witness.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeTerminalAuditWitness {
    pub schema: String,
    pub tenant: String,
    pub operation_sha256: String,
    /// Platform TaskSpec identity, not the scientific finalized request SHA.
    pub request_sha256: String,
    pub run_sha256: String,
    pub native_evidence_sha256: String,
    pub audit_receipt_sha256: String,
    pub retained_manifest_sha256: String,
    pub task_id: String,
    pub attempt: u32,
    pub fence: i64,
    pub job_uid: String,
    pub pod_uid: String,
    pub terminal_revision: i64,
    /// UTC epoch milliseconds from the authenticated Source PlatformSettled
    /// receipt.recorded_at. Publication retries retain this original value.
    pub issued_ms: i64,
}

fn uid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}
impl NativeTerminalAuditWitness {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == NATIVE_TERMINAL_AUDIT_SCHEMA
                && !self.tenant.is_empty()
                && self.tenant.len() <= 128
                && self.tenant.trim() == self.tenant
                && !self.tenant.chars().any(char::is_control)
                && self.attempt > 0
                && self.fence > 0
                && self.terminal_revision > 0
                && self.issued_ms > 0
                && uid(&self.job_uid)
                && uid(&self.pod_uid),
            "invalid native terminal audit scope or units"
        );
        ensure!(
            [
                &self.operation_sha256,
                &self.request_sha256,
                &self.run_sha256,
                &self.native_evidence_sha256,
                &self.audit_receipt_sha256,
                &self.retained_manifest_sha256,
                &self.task_id,
            ]
            .into_iter()
            .all(|value| valid_digest(value))
                && self.request_sha256 == self.task_id,
            "native terminal audit changed the platform request or evidence identity"
        );
        Ok(())
    }
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        identity(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedNativeTerminalAuditWitness {
    pub evidence: NativeTerminalAuditWitness,
    pub key_id: String,
    pub evidence_sha256: String,
    pub signature_hex: String,
}
impl SignedNativeTerminalAuditWitness {
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        ensure!(
            self.evidence.id()? == self.evidence_sha256,
            "native terminal audit evidence changed"
        );
        crate::admission::native_signing_bytes(
            NATIVE_TERMINAL_AUDIT_SCHEMA,
            &self.key_id,
            &self.evidence_sha256,
        )
    }
}

/// Only the Source terminal producer signs after authenticating its opaque
/// observer and actually publishing/readback-verifying the retained audit.
/// This pure helper neither proves those facts nor issues a retirement permit.
pub fn sign_terminal_audit(
    evidence: NativeTerminalAuditWitness,
    key_id: String,
    key: &SigningKey,
) -> Result<SignedNativeTerminalAuditWitness> {
    let mut signed = SignedNativeTerminalAuditWitness {
        evidence_sha256: evidence.id()?,
        evidence,
        key_id,
        signature_hex: String::new(),
    };
    signed.signature_hex = key
        .sign(&signed.signing_bytes()?)
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(signed)
}

/// Constructed only by native trust verification, never deserialized or
/// caller-asserted. This witness remains distinct from a GC/retirement permit.
pub struct VerifiedNativeTerminalAuditWitness {
    signed: SignedNativeTerminalAuditWitness,
    trust_sha256: String,
}
impl VerifiedNativeTerminalAuditWitness {
    pub fn evidence(&self) -> &NativeTerminalAuditWitness {
        &self.signed.evidence
    }
    pub fn signed(&self) -> &SignedNativeTerminalAuditWitness {
        &self.signed
    }
    pub fn trust_sha256(&self) -> &str {
        &self.trust_sha256
    }
}
impl NativeAdmissionTrust {
    pub fn verify_terminal_audit(
        &self,
        signed: &SignedNativeTerminalAuditWitness,
    ) -> Result<VerifiedNativeTerminalAuditWitness> {
        ensure!(
            signed.evidence.id()? == signed.evidence_sha256,
            "native terminal audit evidence changed"
        );
        let trust_sha256 = self.verify_native_signature(
            NATIVE_TERMINAL_AUDIT_SCHEMA,
            &signed.key_id,
            &signed.evidence_sha256,
            &signed.signature_hex,
        )?;
        Ok(VerifiedNativeTerminalAuditWitness {
            signed: signed.clone(),
            trust_sha256,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn witness() -> NativeTerminalAuditWitness {
        let hash = |byte: char| byte.to_string().repeat(64);
        NativeTerminalAuditWitness {
            schema: NATIVE_TERMINAL_AUDIT_SCHEMA.into(),
            tenant: "fixture".into(),
            operation_sha256: hash('a'),
            request_sha256: hash('b'),
            run_sha256: hash('c'),
            native_evidence_sha256: hash('d'),
            audit_receipt_sha256: hash('e'),
            retained_manifest_sha256: hash('f'),
            task_id: hash('b'),
            attempt: 1,
            fence: 9,
            job_uid: "job-fixture-1".into(),
            pod_uid: "pod-fixture-1".into(),
            terminal_revision: 12,
            issued_ms: 1000,
        }
    }
    fn trust(key: &SigningKey) -> NativeAdmissionTrust {
        let public: String = key
            .verifying_key()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        NativeAdmissionTrust {
            schema: "monday.native_reservation_trust.v1".into(),
            native_reservation_keys: BTreeMap::from([
                ("host|audit".into(), public.clone()),
                ("alias".into(), public),
            ]),
        }
    }
    #[test]
    fn valid_wire_has_exact_opaque_scope_and_same_receipt_retry_bytes() {
        let key = SigningKey::from_bytes(&[38; 32]);
        let trust = trust(&key);
        let signed = sign_terminal_audit(witness(), "host|audit".into(), &key).unwrap();
        let bytes = serde_json::to_vec(&signed).unwrap();
        let read: SignedNativeTerminalAuditWitness = serde_json::from_slice(&bytes).unwrap();
        let verified = trust.verify_terminal_audit(&read).unwrap();
        assert_eq!(verified.evidence(), &witness());
        assert_eq!(verified.signed(), &signed);
        assert_eq!(verified.trust_sha256(), identity(&trust).unwrap());
        assert_eq!(
            bytes,
            serde_json::to_vec(&sign_terminal_audit(witness(), "host|audit".into(), &key).unwrap())
                .unwrap()
        );
    }
    #[test]
    fn changed_body_and_trusted_alias_substitution_are_rejected() {
        let key = SigningKey::from_bytes(&[38; 32]);
        let trust = trust(&key);
        let signed = sign_terminal_audit(witness(), "host|audit".into(), &key).unwrap();
        let mut changed = signed.clone();
        changed.evidence.fence += 1;
        assert!(trust.verify_terminal_audit(&changed).is_err());
        changed.evidence_sha256 = changed.evidence.id().unwrap();
        assert!(trust.verify_terminal_audit(&changed).is_err());
        let mut alias = signed;
        alias.key_id = "alias".into();
        assert!(trust.verify_terminal_audit(&alias).is_err());
    }
    #[test]
    fn other_native_purpose_signature_cannot_authorize_terminal_audit() {
        let key = SigningKey::from_bytes(&[38; 32]);
        let trust = trust(&key);
        let mut signed = sign_terminal_audit(witness(), "host|audit".into(), &key).unwrap();
        signed.signature_hex = key
            .sign(
                &crate::admission::native_signing_bytes(
                    crate::revocation::NATIVE_REQUEST_REVOCATION_SCHEMA,
                    &signed.key_id,
                    &signed.evidence_sha256,
                )
                .unwrap(),
            )
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert!(trust.verify_terminal_audit(&signed).is_err());
    }
    #[test]
    fn invalid_scope_identity_and_units_cannot_be_signed() {
        let key = SigningKey::from_bytes(&[38; 32]);
        let cases: [fn(&mut NativeTerminalAuditWitness); 15] = [
            |e| e.schema = "monday.native_request_revocation.v1".into(),
            |e| e.tenant = " fixture".into(),
            |e| e.tenant = "fixture\npeer".into(),
            |e| e.request_sha256 = "0".repeat(64),
            |e| e.run_sha256 = "C".repeat(64),
            |e| e.native_evidence_sha256.clear(),
            |e| e.audit_receipt_sha256 = "missing".into(),
            |e| e.retained_manifest_sha256 = "f".repeat(63),
            |e| e.task_id = "z".repeat(64),
            |e| e.attempt = 0,
            |e| e.fence = 0,
            |e| e.terminal_revision = 0,
            |e| e.issued_ms = 0,
            |e| e.job_uid.clear(),
            |e| e.pod_uid = "../pod".into(),
        ];
        for change in cases {
            let mut e = witness();
            change(&mut e);
            assert!(sign_terminal_audit(e, "host|audit".into(), &key).is_err());
        }
    }
    #[test]
    fn weak_key_and_malformed_signature_are_rejected_by_shared_native_trust() {
        let key = SigningKey::from_bytes(&[38; 32]);
        let trust = trust(&key);
        let signed = sign_terminal_audit(witness(), "host|audit".into(), &key).unwrap();
        let mut malformed = signed.clone();
        malformed.signature_hex = "A".repeat(128);
        assert!(trust.verify_terminal_audit(&malformed).is_err());
        let mut weak = trust;
        weak.native_reservation_keys
            .insert("host|audit".into(), format!("01{}", "00".repeat(31)));
        malformed = signed;
        malformed.signature_hex = format!("01{}", "00".repeat(63));
        assert!(weak.verify_terminal_audit(&malformed).is_err());
    }
}
