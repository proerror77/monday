//! Signed source revocation witnesses never create admission or refund budget.
use crate::{admission::NativeAdmissionTrust, identity, valid_digest};
use anyhow::{ensure, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

pub const NATIVE_REQUEST_REVOCATION_SCHEMA: &str = "monday.native_request_revocation.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeRequestRevocation {
    pub schema: String,
    pub tenant: String,
    pub request_sha256: String,
    pub operation_sha256: String,
    pub family_id: String,
    pub root_grant_sha256: String,
    pub reason_receipt_sha256: String,
    pub effective_ms: i64,
    pub issued_ms: i64,
}
impl NativeRequestRevocation {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == NATIVE_REQUEST_REVOCATION_SCHEMA
                && !self.tenant.is_empty()
                && self.tenant.len() <= 128
                && self.tenant.trim() == self.tenant
                && !self.family_id.is_empty()
                && self.family_id.len() <= 128
                && self.family_id.trim() == self.family_id
                && self.effective_ms > 0
                && self.issued_ms > 0,
            "invalid native revocation scope or time"
        );
        ensure!(
            [
                &self.request_sha256,
                &self.operation_sha256,
                &self.root_grant_sha256,
                &self.reason_receipt_sha256,
            ]
            .into_iter()
            .all(|value| valid_digest(value)),
            "incomplete native revocation identities"
        );
        // Source revoked_at can be scheduled after the host export timestamp.
        Ok(())
    }
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        identity(self)
    }
    pub fn matches_admission(&self, admission: &crate::admission::NativeAdmission) -> Result<()> {
        self.validate()?;
        admission.validate()?;
        ensure!(
            self.tenant == admission.tenant
                && self.request_sha256 == admission.admission.request_sha256
                && self.operation_sha256 == admission.operation_sha256
                && self.family_id == admission.family_id
                && self.root_grant_sha256 == admission.root_grant_sha256,
            "revocation changed the admitted native request"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedNativeRequestRevocation {
    pub evidence: NativeRequestRevocation,
    pub key_id: String,
    pub evidence_sha256: String,
    pub signature_hex: String,
}
impl SignedNativeRequestRevocation {
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        ensure!(
            !self.key_id.is_empty()
                && self.key_id.len() <= 128
                && self.evidence.id()? == self.evidence_sha256,
            "invalid native revocation signer or evidence"
        );
        crate::admission::native_signing_bytes(
            NATIVE_REQUEST_REVOCATION_SCHEMA,
            &self.key_id,
            &self.evidence_sha256,
        )
    }
}

/// Host witness only. The caller must first authenticate, publish and read back
/// the source receipt. A signature cannot replace those source-ledger checks.
pub fn sign_revocation(
    evidence: NativeRequestRevocation,
    key_id: String,
    key: &SigningKey,
) -> Result<SignedNativeRequestRevocation> {
    let evidence_sha256 = evidence.id()?;
    let mut signed = SignedNativeRequestRevocation {
        evidence,
        key_id,
        evidence_sha256,
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

pub struct VerifiedNativeRequestRevocation {
    signed: SignedNativeRequestRevocation,
    trust_sha256: String,
    #[cfg(feature = "control")]
    pub(crate) trust_document: serde_json::Value,
}
impl VerifiedNativeRequestRevocation {
    pub fn evidence(&self) -> &NativeRequestRevocation {
        &self.signed.evidence
    }
    pub fn signed(&self) -> &SignedNativeRequestRevocation {
        &self.signed
    }
    pub fn trust_sha256(&self) -> &str {
        &self.trust_sha256
    }
}

impl NativeAdmissionTrust {
    pub fn verify_revocation(
        &self,
        signed: &SignedNativeRequestRevocation,
    ) -> Result<VerifiedNativeRequestRevocation> {
        ensure!(
            self.schema == "monday.native_reservation_trust.v1"
                && (1..=32).contains(&self.native_reservation_keys.len()),
            "native reservation trust required"
        );
        let public = self
            .native_reservation_keys
            .get(&signed.key_id)
            .context("untrusted native revocation issuer")?;
        let decode = |value: &str, bytes: usize| -> Result<Vec<u8>> {
            ensure!(
                value.len() == bytes * 2
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "invalid native revocation signature encoding"
            );
            (0..value.len())
                .step_by(2)
                .map(|index| u8::from_str_radix(&value[index..index + 2], 16).map_err(Into::into))
                .collect()
        };
        let public: [u8; 32] = decode(public, 32)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid native public key"))?;
        let signature = Signature::from_slice(&decode(&signed.signature_hex, 64)?)?;
        VerifyingKey::from_bytes(&public)?.verify_strict(&signed.signing_bytes()?, &signature)?;
        Ok(VerifiedNativeRequestRevocation {
            signed: signed.clone(),
            trust_sha256: identity(self)?,
            #[cfg(feature = "control")]
            trust_document: serde_json::to_value(self)?,
        })
    }
}

#[cfg(feature = "control")]
impl crate::postgres::Ledger {
    /// Append the exact host witness while holding the original admission lock.
    /// Future effective times remain scheduled; neither budget nor authority moves.
    pub async fn register_native_request_revocation(
        &self,
        verified: &VerifiedNativeRequestRevocation,
    ) -> Result<String> {
        use sqlx_core::query::query;
        let evidence = verified.evidence();
        let id = evidence.id()?;
        let mut tx = self.pool.begin().await?;
        let row = query("SELECT a.document AS admission,n.document AS native_document,n.trust_document,n.trust_sha256,n.evidence_sha256 FROM research.admissions a JOIN research.native_admission_imports n USING(request_sha256) WHERE a.request_sha256=$1 AND n.tenant=$2 FOR UPDATE OF a")
            .bind(&evidence.request_sha256).bind(&evidence.tenant).fetch_one(&mut *tx).await?;
        use sqlx_core::row::Row;
        let native: crate::admission::SignedNativeAdmission =
            serde_json::from_value(row.get("native_document"))?;
        let native_trust: NativeAdmissionTrust = serde_json::from_value(row.get("trust_document"))?;
        let original = native_trust.verify(&native)?;
        ensure!(
            original.trust_sha256() == row.get::<String, _>("trust_sha256")
                && native.evidence_sha256 == row.get::<String, _>("evidence_sha256")
                && native.evidence.admission
                    == serde_json::from_value::<crate::orchestrator::Admission>(
                        row.get("admission")
                    )?,
            "original native admission import changed"
        );
        evidence.matches_admission(original.evidence())?;
        query("INSERT INTO research.native_request_revocations(evidence_sha256,request_sha256,tenant,operation_sha256,family_id,root_grant_sha256,reason_receipt_sha256,effective_ms,issued_ms,trust_sha256,document,trust_document) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) ON CONFLICT DO NOTHING")
            .bind(&id).bind(&evidence.request_sha256).bind(&evidence.tenant).bind(&evidence.operation_sha256)
            .bind(&evidence.family_id).bind(&evidence.root_grant_sha256).bind(&evidence.reason_receipt_sha256)
            .bind(evidence.effective_ms).bind(evidence.issued_ms).bind(verified.trust_sha256())
            .bind(serde_json::to_value(verified.signed())?).bind(verified.trust_document.clone())
            .execute(&mut *tx).await?;
        let stored = query("SELECT evidence_sha256,document,trust_sha256,trust_document FROM research.native_request_revocations WHERE request_sha256=$1 AND reason_receipt_sha256=$2")
            .bind(&evidence.request_sha256).bind(&evidence.reason_receipt_sha256).fetch_one(&mut *tx).await?;
        ensure!(
            stored.get::<String, _>("evidence_sha256") == id
                && serde_json::from_value::<SignedNativeRequestRevocation>(stored.get("document"))?
                    == *verified.signed()
                && stored.get::<String, _>("trust_sha256") == verified.trust_sha256()
                && stored.get::<serde_json::Value, _>("trust_document") == verified.trust_document,
            "source revocation receipt already has conflicting evidence"
        );
        tx.commit().await?;
        Ok(id)
    }

    /// This is a fixed execution cap, not an admission or source-ledger claim.
    /// Callers still validate admission and current time under their own locks.
    pub async fn native_request_deadline_ms(&self, request: &str) -> Result<Option<i64>> {
        use sqlx_core::query_scalar::query_scalar;
        ensure!(valid_digest(request), "invalid native request identity");
        Ok(
            query_scalar("SELECT research.native_request_deadline_ms($1)")
                .bind(request)
                .fetch_one(&self.pool)
                .await?,
        )
    }
}
