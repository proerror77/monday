//! Real canonical data/body proof plus synthetic budget/software/configuration
//! peers. This checks source assembly, not a cloud Run or scientific result.

use alpha_domain::campaign_control::*;
use alpha_store::{AlphaStore, ApprovalRecord};
use chrono::{TimeDelta, Utc};
use ed25519_dalek::{Signer, SigningKey};
use hft_research_platform::{
    build::*,
    execution::{Backend, Profile},
    orchestrator::Artifact,
    release::*,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn encode(bytes: &[u8]) -> String {
    use std::io::Write;
    let mut input = tempfile::NamedTempFile::new().unwrap();
    input.write_all(bytes).unwrap();
    let output = std::process::Command::new("openssl")
        .args(["base64", "-A", "-in"])
        .arg(input.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

fn software(source: &str, image: &str) -> super::released_build::ReadbackBuildRelease {
    let h = |c: char| c.to_string().repeat(64);
    let key = SigningKey::from_bytes(&[20; 32]);
    let blob = |key: String, bytes: &[u8]| Artifact {
        key,
        sha256: hft_research_platform::sha256(bytes),
        bytes: bytes.len() as u64,
    };
    let source_archive = SourceArchive {
        schema: 1,
        code_commit: source.into(),
        archive: blob(
            format!("research/sources/{source}/source.tar"),
            b"synthetic source readback",
        ),
    };
    let build = BuildSpec {
        schema: 2,
        code_commit: source.into(),
        workspace_manifest: "research-core/Cargo.toml".into(),
        source_manifest_sha256: hft_research_platform::identity(&source_archive).unwrap(),
        cargo_lock_sha256: h('a'),
        toolchain_manifest_sha256: h('b'),
        target: "x86_64-unknown-linux-gnu".into(),
        packages: vec!["monday-cex-worker".into()],
        binaries: vec!["monday-cex-worker".into()],
        features: vec!["scientific".into()],
        default_features: false,
        profile: "research".into(),
        profile_manifest_sha256: h('c'),
        rustflags_sha256: h('d'),
        native_environment_sha256: h('e'),
        builder_image: format!("builder@sha256:{}", h('f')),
    };
    let executable = BuiltExecutable {
        name: "monday-cex-worker".into(),
        blob: blob(
            format!("research/builds/{}/monday-cex-worker", build.id().unwrap()),
            b"synthetic worker readback",
        ),
    };
    let mut signed = SignedBuildRelease {
        schema: 1,
        key_id: "software".into(),
        signature_hex: String::new(),
        receipt: BuildReleaseReceipt {
            schema: 1,
            build_sha256: build.id().unwrap(),
            source: source_archive.clone(),
            image: image.into(),
            target: build.target.clone(),
            executables: vec![executable.clone()],
            producer: ReleaseProducer {
                repository: "proerror77/monday".into(),
                workflow_path: ".github/workflows/acr-publish.yml".into(),
                source_sha: source.into(),
                run_id: 1,
                run_attempt: 1,
                job_id: 1,
            },
            publication_readback_sha256: h('1'),
        },
    };
    signed.signature_hex = hex::encode(key.sign(&signed.signing_bytes().unwrap()).to_bytes());
    let artifact = BuildArtifact {
        schema: 1,
        build,
        image: image.into(),
        executables: vec![executable.clone()],
        release_receipt_sha256: hft_research_platform::identity(&signed).unwrap(),
    };
    let trust = BuildReleaseTrust {
        schema: 1,
        repository: "proerror77/monday".into(),
        producer_workflow_path: ".github/workflows/acr-publish.yml".into(),
        keys: [(
            "software".into(),
            hex::encode(key.verifying_key().as_bytes()),
        )]
        .into(),
    };
    let mut actual = BTreeMap::from([
        (
            source_archive.archive.key,
            b"synthetic source readback".to_vec(),
        ),
        (
            executable.blob.key.clone(),
            b"synthetic worker readback".to_vec(),
        ),
    ]);
    let readback =
        super::released_build::from_test_readback_peer(&trust, &artifact, &signed, &actual)
            .unwrap();
    actual.insert(executable.blob.key, b"changed worker".to_vec());
    assert!(
        super::released_build::from_test_readback_peer(&trust, &artifact, &signed, &actual)
            .is_err()
    );
    readback
}

#[test]
fn genuine_finalized_budget_data_and_actual_config_construct_one_exact_task() {
    let fixture = crate::mission_campaign::tests::native_prepared_fixture_for_tests();
    let image = format!("registry/worker@sha256:{}", fixture.request.image_identity);
    let submission = super::super::MissionDispatchSubmission {
        attempt_id: "native-export-fixture".into(),
        image: image.clone(),
        request: fixture.request.clone(),
    };
    // This genuine local-data fixture is not a cloud transport acceptance.
    // Production continues to reject its file-backed collection URLs.
    assert!(super::super::validate_submission(submission.clone()).is_err());
    let validated = super::super::validate_submission_with_request_check(
        submission,
        crate::mission_campaign::validate_request_for_execute,
    )
    .unwrap();
    let manifest = super::super::render_manifest(&validated, "monday-research").unwrap();
    let inspection = super::super::admission::reconstruct_binding(
        &validated,
        &manifest,
        fixture.materialization_path(),
        &format!("controller@sha256:{}", "e".repeat(64)),
        0,
    )
    .unwrap();
    let now = Utc::now();
    let root = CampaignRootGrantV1 {
        schema_version: ROOT_GRANT_SCHEMA.into(),
        root_id: "native-export-fixture".into(),
        family: CampaignFamilyPolicyV1 {
            family_id: "native-export-family".into(),
            definition_sha256: "a".repeat(64),
            max_trials: 1000,
        },
        execution_scope: CampaignExecutionScope::PreHoldout,
        execution: inspection.execution.clone(),
        allowed_policy_revision_ids: BTreeSet::from([inspection.policy_revision_id.clone()]),
        max_follow_ups: 1,
        budget: CampaignRootBudgetV1 {
            max_trials: 1000,
            max_job_attempts: 2,
            max_job_seconds: 100_000,
            max_llm_tokens: 0,
        },
        valid_from: now - TimeDelta::minutes(1),
        expires_at: now + TimeDelta::hours(24),
    };
    let key = SigningKey::from_bytes(&[19; 32]);
    let signed = sign_campaign_root_grant(root, "authority".into(), &key).unwrap();
    let root = verify_campaign_root_grant(
        &signed,
        &[("authority".into(), key.verifying_key())].into(),
        now,
    )
    .unwrap();
    let mut store = AlphaStore::open_in_memory().unwrap();
    store.record_approval(&ApprovalRecord {
        approval_id: "native-export-approval".into(), approval_class: "campaign_root".into(), subject_id: root.grant().root_id.clone(),
        payload: json!({"grant_sha256":root.content_sha256(),"family_id":root.grant().family.family_id}), signer_id: Some("authority".into()),
        valid_from: Some(root.grant().valid_from), expires_at: Some(root.grant().expires_at), revoked_at: None,
        revoked_by: None, revocation_reason: None, created_at: root.grant().valid_from,
    }).unwrap();
    store
        .register_campaign_root(&root, "native-export-approval", now)
        .unwrap();
    let reservation = inspection.reservation(&root);
    store
        .reserve_campaign_attempt(&root, &reservation, now)
        .unwrap();
    for entry in store
        .campaign_family_receipts(&reservation.family_id)
        .unwrap()
    {
        let bytes = entry.publication_bytes().unwrap();
        store
            .acknowledge_campaign_receipt_readback(
                &reservation.family_id,
                entry.receipt.sequence,
                &entry.object_key(),
                &hft_research_platform::sha256(&bytes),
            )
            .unwrap();
    }
    let budget = store
        .with_campaign_platform_budget(&root, &reservation, |b| {
            Ok::<_, alpha_store::StoreError>(b.clone())
        })
        .unwrap();
    assert_eq!(
        budget.authority_public_keys(),
        &[key.verifying_key().to_bytes()]
    );
    let build = software(fixture.inputs.source_revision(), &image);
    let directory = tempfile::tempdir().unwrap();
    let certificate = directory.path().join("ca.pem");
    let result = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "ed25519",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=Native Fixture CA",
            "-keyout",
        ])
        .arg(directory.path().join("tls-key.pem"))
        .arg("-out")
        .arg(&certificate)
        .output()
        .unwrap();
    assert!(result.status.success());
    let config = serde_json::to_vec(&json!({"schema_version":"monday.cex_campaign_artifact_io.v1","artifact_gateway":"https://gateway.invalid/","artifact_token_file":"/identity/artifact.token","artifact_tls":{"ca_file":"/config/ca.pem","identity_file":"/identity/tls.pem"}})).unwrap();
    let native_trust = hft_research_platform::admission::NativeAdmissionTrust {
        schema: "monday.native_reservation_trust.v1".into(),
        native_reservation_keys: [(
            "host-witness".into(),
            hex::encode(SigningKey::from_bytes(&[42; 32]).verifying_key().as_bytes()),
        )]
        .into(),
    };
    let observed = json!({"apiVersion":"v1","kind":"Secret","type":"Opaque","immutable":true,"metadata":{"name":"native-config","namespace":"monday-research","uid":"fixture-uid"},"data":{"campaign.json":encode(validated.request_json.as_bytes()),"artifact-io.json":encode(&config),"ca.pem":encode(&std::fs::read(certificate).unwrap()),"native-trust.json":encode(&serde_json::to_vec(&native_trust).unwrap())}});
    let configuration = super::worker_configuration::from_test_readback_peer(
        &observed,
        "monday-research",
        "native-config",
        validated.request_json.as_bytes(),
        &reservation.request_sha256,
        &native_trust,
    )
    .unwrap();
    let profile = Profile {
        backend: Backend::KubernetesJob,
        cluster: "fixture-cluster".into(),
        namespace: "monday-research".into(),
        service_account: "research-worker".into(),
        architecture: "amd64".into(),
        cpu_millis: reservation.execution.job_cpu_millis,
        memory_mib: reservation.execution.job_memory_mib,
        scratch_mib: 20 * 1024,
        gpu: 0,
        acceptance_sha256: "a".repeat(64),
        prepared_pvc: None,
        worker_secret: Some("native-config".into()),
    };
    let construct = |profile| {
        super::fixed_campaign::construct(super::fixed_campaign::Inputs {
            budget: &budget,
            data: &fixture.inputs,
            build: &build,
            configuration: &configuration,
            profile,
            validated: &validated,
            manifest: &manifest,
            context: "fixture-cluster",
            namespace: "monday-research",
        })
    };
    let fixed = construct(profile.clone()).unwrap();
    assert_eq!(
        fixed.run.data_manifest_sha256,
        fixture.inputs.collection_id()
    );
    assert_eq!(fixed.run.configuration_sha256, reservation.request_sha256);
    assert_eq!(fixed.spec.max_attempts, 1);
    assert_eq!(
        fixed.spec.timeout_ms as u64,
        reservation.reserved_job_seconds * 1000
    );
    assert!(fixed
        .spec
        .command
        .windows(2)
        .any(|pair| pair == ["--request", "/config/campaign.json"]));
    let mut changed = profile.clone();
    changed.cpu_millis += 1;
    assert!(construct(changed).is_err());
    let mut changed = profile;
    changed.worker_secret = Some("foreign-config".into());
    assert!(construct(changed).is_err());
    let mut late = observed.clone();
    late["data"]["artifact.token"] = json!(encode(b"must-not-enter-static-science-identity"));
    assert!(super::worker_configuration::from_test_readback_peer(
        &late,
        "monday-research",
        "native-config",
        validated.request_json.as_bytes(),
        &reservation.request_sha256,
        &native_trust,
    )
    .is_err());
    let mut tampered = observed;
    tampered["data"]["campaign.json"] = json!(encode(b"{}"));
    assert!(super::worker_configuration::from_test_readback_peer(
        &tampered,
        "monday-research",
        "native-config",
        validated.request_json.as_bytes(),
        &reservation.request_sha256,
        &native_trust,
    )
    .is_err());
    let transfer = alpha_store::campaign_ledger::CampaignPlatformTransferV1 {
        operation_id: reservation.operation_id().unwrap(),
        tenant: "native-fixture".into(),
        run_sha256: fixed.run.id().unwrap(),
        request_sha256: fixed.spec.id().unwrap(),
    };
    store
        .transfer_campaign_execution_to_platform(&root, &reservation, &transfer)
        .unwrap();
    let effective = Utc::now() + TimeDelta::hours(12);
    store
        .revoke_approval(
            "native-export-approval",
            "authority",
            "scheduled stop",
            effective,
        )
        .unwrap();
    for entry in store
        .campaign_family_receipts(&reservation.family_id)
        .unwrap()
    {
        store
            .acknowledge_campaign_receipt_readback(
                &reservation.family_id,
                entry.receipt.sequence,
                &entry.object_key(),
                &entry.object_sha256().unwrap(),
            )
            .unwrap();
    }
    let reasons = store
        .campaign_platform_revocations(&reservation.family_id)
        .unwrap();
    assert_eq!(reasons.len(), 1);
    let evidence =
        super::signed_revocations::statement(&reasons[0], Utc::now().timestamp_millis()).unwrap();
    assert_eq!(evidence.request_sha256, fixed.spec.id().unwrap());
    assert_eq!(
        evidence.operation_sha256,
        budget.operation_sha256().unwrap()
    );
    assert_eq!(evidence.effective_ms, effective.timestamp_millis());
    assert!(evidence.effective_ms > evidence.issued_ms);
    assert_eq!(
        reasons[0].authority_public_keys(),
        &[key.verifying_key().to_bytes()]
    );
    let witness = SigningKey::from_bytes(&[42; 32]);
    let signed = hft_research_platform::revocation::sign_revocation(
        evidence,
        "host-witness".into(),
        &witness,
    )
    .unwrap();
    native_trust.verify_revocation(&signed).unwrap();
}
