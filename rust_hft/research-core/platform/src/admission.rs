//! Native reservation signing and immutable PG import remain control-owned.
use anyhow::{ensure, Result};
use ed25519_dalek::{Signer, SigningKey};
use hft_research_worker_contracts::admission::{
    native_signing_bytes, NativeAdmission, SignedNativeAdmission, NATIVE_ADMISSION_DOMAIN as DOMAIN,
};
#[cfg(feature = "control")]
use hft_research_worker_contracts::{admission::VerifiedNativeAdmission, task::Admission};

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
        crate::research::admit_run_build(&run, &build)?;
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
            .bind(verified.trust_document().clone())
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
    use hft_research_worker_contracts::{
        admission::NativeAdmissionTrust,
        research::Run,
        task::{Admission, TaskKind},
    };
    use std::collections::BTreeMap;
    use {
        hft_research_worker_contracts::execution::Backend,
        hft_research_worker_contracts::execution::Profile,
        hft_research_worker_contracts::task::TaskSpec,
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
