//! Native Campaign budget authority projects an exact, already reserved request.
//! The generic task API cannot manufacture or widen this projection.
use crate::{
    identity,
    orchestrator::{Admission, TaskKind},
    research::Run,
    valid_digest,
};
use anyhow::{ensure, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const DOMAIN: &str = "monday.native_scientific_admission.v1";

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
            self.schema == DOMAIN
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

pub(crate) fn native_signing_bytes(
    domain: &str,
    key_id: &str,
    evidence_sha256: &str,
) -> Result<Vec<u8>> {
    ensure!(
        !key_id.is_empty() && key_id.len() <= 128 && valid_digest(evidence_sha256),
        "invalid native signer or evidence identity"
    );
    Ok(serde_json::to_vec(&(domain, key_id, evidence_sha256))?)
}

/// Only the separately controlled native reservation producer receives this key.
/// Passing a key never replaces its source ledger's signature, approval and
/// cumulative budget checks. The producer must construct evidence under those
/// guards; this receiver does not issue the underlying grant or charge a budget.
pub fn sign(
    evidence: NativeAdmission,
    key_id: String,
    key: &SigningKey,
) -> Result<SignedNativeAdmission> {
    ensure!(
        !key_id.is_empty() && key_id.len() <= 128,
        "invalid native issuer key identity"
    );
    let evidence_sha256 = evidence.id()?;
    let signature_hex = key
        .sign(&native_signing_bytes(DOMAIN, &key_id, &evidence_sha256)?)
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(SignedNativeAdmission {
        evidence,
        key_id,
        evidence_sha256,
        signature_hex,
    })
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
    #[cfg(feature = "control")]
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
}
impl NativeAdmissionTrust {
    pub fn verify(&self, signed: &SignedNativeAdmission) -> Result<VerifiedNativeAdmission> {
        ensure!(
            self.schema == "monday.native_reservation_trust.v1"
                && !self.native_reservation_keys.is_empty()
                && self.native_reservation_keys.len() <= 32,
            "native reservation trust required"
        );
        let public = self
            .native_reservation_keys
            .get(&signed.key_id)
            .context("untrusted native reservation issuer")?;
        let decode = |s: &str| -> Result<Vec<u8>> {
            ensure!(
                s.len().is_multiple_of(2)
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid native signature encoding"
            );
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
                .collect()
        };
        let bytes: [u8; 32] = decode(public)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid native public key"))?;
        ensure!(
            signed.evidence.id()? == signed.evidence_sha256,
            "native evidence changed"
        );
        let signature = Signature::from_slice(&decode(&signed.signature_hex)?)?;
        VerifyingKey::from_bytes(&bytes)?.verify_strict(
            &native_signing_bytes(DOMAIN, &signed.key_id, &signed.evidence_sha256)?,
            &signature,
        )?;
        Ok(VerifiedNativeAdmission {
            signed: signed.clone(),
            trust_sha256: identity(self)?,
            #[cfg(feature = "control")]
            trust_document: serde_json::to_value(self)?,
        })
    }
}

#[cfg(feature = "control")]
impl crate::postgres::Ledger {
    /// Immutable import. It leaves global authority paused and grants no backend
    /// activation. One native reservation can cover only one PG request/tenant.
    pub async fn register_native_admission(
        &self,
        verified: &VerifiedNativeAdmission,
    ) -> Result<String> {
        use sqlx_core::{query::query, query_scalar::query_scalar};
        let evidence = verified.evidence();
        let id = evidence.id()?;
        let run = self
            .run_for_tenant(&evidence.tenant, &evidence.run.id()?)
            .await?;
        ensure!(
            run == evidence.run,
            "native projection changed the registered Run"
        );
        let build = self.build_artifact(&run.build_artifact_sha256).await?;
        run.admit_build(&build)?;
        ensure!(
            evidence.admission.release_admission_receipt_sha256 == build.release_receipt_sha256,
            "native projection changed admitted release evidence"
        );
        let mut tx = self.pool.begin().await?;
        let now: i64 =
            query_scalar("SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint")
                .fetch_one(&mut *tx)
                .await?;
        evidence.admits_launch_at(now)?;
        let request = &evidence.admission.request_sha256;
        query("INSERT INTO research.native_admission_imports(request_sha256,tenant,operation_sha256,evidence_sha256,trust_sha256,expires_ms,document,trust_document) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING")
            .bind(request).bind(&evidence.tenant).bind(&evidence.operation_sha256).bind(&id)
            .bind(verified.trust_sha256()).bind(evidence.expires_ms).bind(serde_json::to_value(verified.signed())?)
            .bind(verified.trust_document.clone())
            .execute(&mut *tx).await?;
        let stored: serde_json::Value = query_scalar("SELECT document FROM research.native_admission_imports WHERE request_sha256=$1 AND tenant=$2")
            .bind(request).bind(&evidence.tenant).fetch_one(&mut *tx).await?;
        ensure!(
            serde_json::from_value::<SignedNativeAdmission>(stored)? == *verified.signed(),
            "native reservation already covers another request or evidence"
        );
        query("INSERT INTO research.admissions(request_sha256,document) VALUES($1,$2) ON CONFLICT DO NOTHING")
            .bind(request).bind(serde_json::to_value(&evidence.admission)?).execute(&mut *tx).await?;
        let admitted: serde_json::Value =
            query_scalar("SELECT document FROM research.admissions WHERE request_sha256=$1")
                .bind(request)
                .fetch_one(&mut *tx)
                .await?;
        ensure!(
            serde_json::from_value::<Admission>(admitted)? == evidence.admission,
            "admission conflicts with native reservation"
        );
        tx.commit().await?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        execution::{Backend, Profile},
        orchestrator::TaskSpec,
    };
    fn fixture() -> NativeAdmission {
        let h = |c: char| c.to_string().repeat(64);
        let run = Run {
            schema: 1,
            experiment_sha256: h('a'),
            kind: TaskKind::Train,
            build_artifact_sha256: h('b'),
            configuration_sha256: h('c'),
            command: vec!["/usr/local/bin/fixture".into()],
            code_commit: "a".repeat(40),
            source_manifest_sha256: h('d'),
            image: format!("fixture@sha256:{}", h('e')),
            data_manifest_sha256: h('f'),
            seed: 7,
            evaluator_sha256: h('a'),
            evaluation_protocol_sha256: h('b'),
            fit_identity_sha256: None,
        };
        let task = TaskSpec {
            schema: 1,
            kind: run.kind,
            run_manifest_sha256: run.id().unwrap(),
            view_manifest_sha256: run.data_manifest_sha256.clone(),
            source_sha256: run.source_manifest_sha256.clone(),
            image: run.image.clone(),
            command: run.command.clone(),
            profile: Profile {
                backend: Backend::KubernetesJob,
                cluster: "fixture".into(),
                namespace: "research".into(),
                service_account: "worker".into(),
                architecture: "amd64".into(),
                cpu_millis: 1000,
                memory_mib: 128,
                scratch_mib: 64,
                gpu: 0,
                acceptance_sha256: h('a'),
                prepared_pvc: None,
                worker_secret: None,
            },
            timeout_ms: 10_000,
            max_attempts: 2,
            output_prefix: "research/fixture".into(),
            fit_identity_sha256: None,
            worker_configuration: None,
        };
        let admission = Admission {
            schema: 1,
            request_sha256: task.id().unwrap(),
            task_spec: task,
            resource_reservation_receipt_sha256: h('a'),
            scientific_grant_receipt_sha256: h('b'),
            release_admission_receipt_sha256: h('c'),
            max_attempts: 2,
        };
        NativeAdmission {
            schema: DOMAIN.into(),
            tenant: "fixture".into(),
            run,
            admission,
            operation_sha256: h('d'),
            native_request_sha256: h('c'),
            family_id: "fixture-family".into(),
            root_grant_sha256: h('e'),
            approval_sha256: h('f'),
            transfer_receipt_sha256: h('a'),
            declared_trials: 2,
            reserved_job_seconds: 20,
            reserved_llm_tokens: 0,
            issued_ms: 1000,
            expires_ms: 60000,
        }
    }
    #[test]
    fn native_reservation_signature_binds_exact_tenant_run_budget_and_request() {
        let key = SigningKey::from_bytes(&[21; 32]);
        let public = key
            .verifying_key()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut trust = NativeAdmissionTrust {
            schema: "monday.native_reservation_trust.v1".into(),
            native_reservation_keys: BTreeMap::from([("native".into(), public)]),
        };
        trust.native_reservation_keys.insert(
            "alias".into(),
            trust.native_reservation_keys["native"].clone(),
        );
        let signed = sign(fixture(), "native".into(), &key).unwrap();
        let mut relabeled = signed.clone();
        relabeled.key_id = "alias".into();
        assert!(trust.verify(&relabeled).is_err());
        let mut legacy = signed.clone();
        legacy.signature_hex = key
            .sign(format!("{DOMAIN}:{}", legacy.evidence_sha256).as_bytes())
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(trust.verify(&legacy).is_err());
        trust.verify(&signed).unwrap();
        for field in 0..5 {
            let mut changed = signed.clone();
            match field {
                0 => changed.evidence.tenant = "another".into(),
                1 => changed.evidence.run.seed += 1,
                2 => changed.evidence.declared_trials += 1,
                3 => changed.evidence.admission.task_spec.max_attempts += 1,
                _ => changed.key_id = "publisher".into(),
            }
            assert!(trust.verify(&changed).is_err());
        }
        let mut changed = signed;
        changed.evidence.tenant = "another".into();
        changed.evidence_sha256 = changed.evidence.id().unwrap();
        assert!(trust.verify(&changed).is_err());
    }
    #[test]
    fn expired_or_under_reserved_permits_cannot_launch_or_widen_retries() {
        let mut evidence = fixture();
        evidence.validate().unwrap();
        evidence.admits_launch_at(50_000).unwrap();
        assert!(evidence.admits_launch_at(50_001).is_err());
        assert!(evidence.active_at(60_000).is_err());
        evidence.reserved_job_seconds = 10;
        assert!(evidence.validate().is_err());
        evidence.reserved_job_seconds = 20;
        evidence.admission.max_attempts = 3;
        assert!(evidence.validate().is_err());
    }

    #[test]
    fn weak_public_key_cannot_forge_a_native_reservation_without_a_signer() {
        let evidence = fixture();
        let identity = format!("01{}", "00".repeat(31));
        let signed = SignedNativeAdmission {
            evidence_sha256: evidence.id().unwrap(),
            evidence,
            key_id: "weak".into(),
            signature_hex: format!("{identity}{}", "00".repeat(32)),
        };
        let trust = NativeAdmissionTrust {
            schema: "monday.native_reservation_trust.v1".into(),
            native_reservation_keys: BTreeMap::from([("weak".into(), identity)]),
        };
        assert!(trust.verify(&signed).is_err());
    }
}
