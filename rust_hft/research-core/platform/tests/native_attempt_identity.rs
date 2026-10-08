#![cfg(all(feature = "control", feature = "publisher"))]
use anyhow::Result;
use hft_research_platform::{
    admission::{sign, NativeAdmission, NativeAdmissionTrust},
    artifact_identity::{AttemptIdentityConfig, AttemptIdentityIssuer},
    build::{BuildArtifact, BuildSpec, BuiltExecutable},
    execution::{Backend, Profile},
    identity,
    orchestrator::{Admission, Artifact, State, Task, TaskKind, TaskSpec},
    postgres::{
        Ledger, BUILD_IMPORT_ADMISSION_MIGRATION, BUILD_RELEASE_MIGRATION, MIGRATION,
        NATIVE_ADMISSION_MIGRATION, NATIVE_REQUEST_REVOCATION_MIGRATION,
    },
    research::{Experiment, Run},
    revocation::{sign_revocation, NativeRequestRevocation, NATIVE_REQUEST_REVOCATION_SCHEMA},
};
use sqlx_core::{query::query, query_scalar::query_scalar};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt};
mod common;
fn hash(c: char) -> String {
    c.to_string().repeat(64)
}

#[cfg(feature = "publisher")]
#[tokio::test]
#[ignore = "requires disposable loopback MONDAY_TEST_DATABASE_URL ending /monday_foundation_test"]
async fn admitted_attempt_issuer_binds_current_pg_lease_and_native_cap() -> Result<()> {
    let url = std::env::var("MONDAY_TEST_DATABASE_URL")?;
    anyhow::ensure!(
        url.ends_with("/monday_foundation_test")
            && (url.contains("@127.0.0.1:") || url.contains("@localhost:")),
        "disposable loopback database required"
    );
    let pool = sqlx_postgres::PgPool::connect(&url).await?;
    let exists: bool =
        query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='research')")
            .fetch_one(&pool)
            .await?;
    if !exists {
        for migration in [
            MIGRATION,
            BUILD_RELEASE_MIGRATION,
            BUILD_IMPORT_ADMISSION_MIGRATION,
            NATIVE_ADMISSION_MIGRATION,
            NATIVE_REQUEST_REVOCATION_MIGRATION,
        ] {
            sqlx_core::raw_sql::raw_sql(migration)
                .execute(&pool)
                .await?;
        }
    }
    // This named disposable fixture never changes production authority.
    query("UPDATE research.authority SET mode='postgres',legacy_quiescence_sha256=$1,migration_receipt_sha256=$2").bind(hash('1')).bind(hash('2')).execute(&pool).await?;
    let ledger = Ledger::connect(&url).await?;
    let experiment = Experiment {
        schema: 1,
        hypothesis: "private Attempt issuer fixture".into(),
        parent_experiment_sha256: None,
        variant: BTreeMap::new(),
    };
    let experiment_id = ledger
        .register_experiment("identity-fixture", &experiment)
        .await?;
    let build = BuildSpec {
        schema: 2,
        workspace_manifest: "research-core/Cargo.toml".into(),
        code_commit: "b".repeat(40),
        source_manifest_sha256: hash('b'),
        cargo_lock_sha256: hash('c'),
        toolchain_manifest_sha256: hash('d'),
        target: "x86_64-unknown-linux-gnu".into(),
        packages: vec!["fixture".into()],
        binaries: vec!["fixture".into()],
        features: vec![],
        default_features: false,
        profile: "research".into(),
        profile_manifest_sha256: hash('e'),
        rustflags_sha256: hash('f'),
        native_environment_sha256: hash('1'),
        builder_image: format!("builder@sha256:{}", hash('2')),
    };
    let build_id = build.id()?;
    let artifact = BuildArtifact {
        schema: 1,
        build,
        image: format!("fixture@sha256:{}", hash('3')),
        release_receipt_sha256: hash('4'),
        executables: vec![BuiltExecutable {
            name: "fixture".into(),
            blob: Artifact {
                key: format!("research/builds/{build_id}/fixture"),
                sha256: hash('5'),
                bytes: 16,
            },
        }],
    };
    let package = common::published::Package::new(artifact).await?;
    let artifact = package.artifact.clone();
    ledger
        .set_build_import_admission(&package.admission)
        .await?;
    let artifact_id = package.import(&ledger).await?;
    let run = Run {
        schema: 1,
        experiment_sha256: experiment_id,
        kind: TaskKind::Train,
        build_artifact_sha256: artifact_id,
        configuration_sha256: hash('6'),
        command: vec!["/usr/local/bin/fixture".into()],
        code_commit: artifact.build.code_commit.clone(),
        source_manifest_sha256: artifact.build.source_manifest_sha256.clone(),
        image: artifact.image.clone(),
        data_manifest_sha256: hash('7'),
        seed: 42,
        evaluator_sha256: hash('8'),
        evaluation_protocol_sha256: hash('9'),
        fit_identity_sha256: None,
    };
    let run_id = ledger.register_run("identity-fixture", &run).await?;
    let spec = TaskSpec {
        schema: 1,
        kind: run.kind,
        run_manifest_sha256: run_id.clone(),
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
            acceptance_sha256: hash('a'),
            prepared_pvc: None,
            worker_secret: None,
        },
        timeout_ms: 120_000,
        max_attempts: 1,
        output_prefix: "research/identity-fixture".into(),
        fit_identity_sha256: None,
        worker_configuration: None,
    };
    let task_id = spec.id()?;
    let now: i64 = query_scalar("SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint")
        .fetch_one(&pool)
        .await?;
    let native = NativeAdmission {
        schema: "monday.native_scientific_admission.v1".into(),
        tenant: "identity-fixture".into(),
        native_request_sha256: run.configuration_sha256.clone(),
        run,
        admission: Admission {
            schema: 1,
            request_sha256: task_id.clone(),
            task_spec: spec.clone(),
            resource_reservation_receipt_sha256: hash('b'),
            scientific_grant_receipt_sha256: hash('c'),
            release_admission_receipt_sha256: artifact.release_receipt_sha256.clone(),
            max_attempts: 1,
        },
        operation_sha256: identity(&"identity-fixture-operation")?,
        family_id: "identity-fixture-family".into(),
        root_grant_sha256: hash('d'),
        approval_sha256: hash('e'),
        transfer_receipt_sha256: hash('f'),
        declared_trials: 1,
        reserved_job_seconds: 120,
        reserved_llm_tokens: 0,
        issued_ms: now,
        expires_ms: now + 3_600_000,
    };
    let key = ed25519_dalek::SigningKey::from_bytes(&[23; 32]);
    let trust = NativeAdmissionTrust {
        schema: "monday.native_reservation_trust.v1".into(),
        native_reservation_keys: BTreeMap::from([(
            "identity-fixture-host".into(),
            key.verifying_key()
                .to_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )]),
    };
    let signed = sign(native.clone(), "identity-fixture-host".into(), &key)?;
    ledger
        .register_native_admission(&trust.verify(&signed)?)
        .await?;
    // The fixture records a claimed lease without starting any worker or model.
    query("INSERT INTO research.inputs(manifest_sha256,kind,document) VALUES($1,'prepared','{}'::jsonb) ON CONFLICT DO NOTHING").bind(&spec.view_manifest_sha256).execute(&pool).await?;
    let mut task = Task::new(spec)?;
    task.claim("identity-fixture-controller", now, 60_000)?;
    query("INSERT INTO research.tasks(task_id,tenant,idempotency_key,request_sha256,run_manifest_sha256,view_manifest_sha256,state,document) VALUES($1,$2,'identity-fixture',$1,$3,$4,'launching',$5)")
        .bind(&task_id).bind("identity-fixture").bind(&run_id).bind(&task.spec.view_manifest_sha256).bind(serde_json::to_value(&task)?).execute(&pool).await?;
    let mut revoke = NativeRequestRevocation {
        schema: NATIVE_REQUEST_REVOCATION_SCHEMA.into(),
        tenant: native.tenant.clone(),
        request_sha256: task_id.clone(),
        operation_sha256: native.operation_sha256.clone(),
        family_id: native.family_id.clone(),
        root_grant_sha256: native.root_grant_sha256.clone(),
        reason_receipt_sha256: hash('1'),
        effective_ms: now + 90_000,
        issued_ms: now,
    };
    ledger
        .register_native_request_revocation(&trust.verify_revocation(&sign_revocation(
            revoke.clone(),
            "identity-fixture-host".into(),
            &key,
        )?)?)
        .await?;
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let state = root.join("state");
    std::fs::create_dir(&state)?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    let projection = root.join("capabilities.json");
    let original = serde_json::json!([{"token_sha256":hash('a'),"expires_ms":now+90_000,"access":{"role":"reader","prefixes":["research/identity-fixture/"]}}]);
    std::fs::write(&projection, serde_json::to_vec(&original)?)?;
    std::fs::set_permissions(&projection, std::fs::Permissions::from_mode(0o600))?;
    let config = AttemptIdentityConfig {
        capabilities_file: projection.clone(),
        state_root: state.clone(),
        namespace_prefix: "research/identity-fixture/".into(),
        tls_identity_file: None,
    };
    let issuer = AttemptIdentityIssuer::new(config.clone())?;
    let mut foreign_config = config.clone();
    foreign_config.namespace_prefix = "research/foreign/".into();
    let foreign_issuer = AttemptIdentityIssuer::new(foreign_config)?;
    let mut rejected = pool.begin().await?;
    assert!(foreign_issuer
        .issue_for_task(&mut rejected, &task)
        .await
        .is_err());
    rejected.rollback().await?;
    assert_eq!(std::fs::read_dir(&state)?.count(), 0);
    let mut tx = pool.begin().await?;
    // The caller already owns the task lock. A second connection would deadlock.
    query("SELECT task_id FROM research.tasks WHERE task_id=$1 FOR UPDATE")
        .bind(&task_id)
        .fetch_one(&mut *tx)
        .await?;
    let issued = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        issuer.issue_for_task(&mut tx, &task),
    )
    .await??;
    assert_eq!(issued.task_id(), task_id);
    assert_eq!(issued.tenant(), native.tenant);
    assert_eq!(issued.deadline_ms(), revoke.effective_ms);
    assert!(issued.deadline_ms() > task.lease.as_ref().unwrap().expires_ms);
    assert_eq!(issued.native_evidence_sha256(), signed.evidence_sha256);
    let mounted: hft_research_platform::admission::SignedNativeAdmission =
        serde_json::from_slice(&issued.late_files()["native-admission.json"])?;
    trust.verify(&mounted)?;
    assert!(mounted == signed);
    let token = issued.late_files()["artifact.token"].clone();
    assert!(token.len() == 64);
    let projected: serde_json::Value = serde_json::from_slice(&std::fs::read(&projection)?)?;
    assert!(projected
        .as_array()
        .unwrap()
        .iter()
        .any(
            |cap| cap["token_sha256"] == hft_research_platform::sha256(&token)
                && cap["expires_ms"] == revoke.effective_ms
        ));
    tx.rollback().await?;
    let restored_issuer = AttemptIdentityIssuer::new(config)?;
    let mut tx = pool.begin().await?;
    let recovered = restored_issuer.issue_for_task(&mut tx, &task).await?;
    assert!(recovered.late_files()["artifact.token"] == token);
    assert_eq!(recovered.scope_id(), issued.scope_id());
    tx.rollback().await?;
    let mut stale = task.clone();
    stale.fence += 1;
    let mut tx = pool.begin().await?;
    assert!(issuer.issue_for_task(&mut tx, &stale).await.is_err());
    tx.rollback().await?;
    let original_lease = task.lease.clone();
    task.lease.as_mut().unwrap().expires_ms = now - 1;
    query("UPDATE research.tasks SET document=$1 WHERE task_id=$2")
        .bind(serde_json::to_value(&task)?)
        .bind(&task_id)
        .execute(&pool)
        .await?;
    let mut expired = pool.begin().await?;
    assert!(issuer.issue_for_task(&mut expired, &task).await.is_err());
    expired.rollback().await?;
    task.lease = original_lease;
    task.state = State::Stopping;
    query("UPDATE research.tasks SET state='stopping',document=$1 WHERE task_id=$2")
        .bind(serde_json::to_value(&task)?)
        .bind(&task_id)
        .execute(&pool)
        .await?;
    let mut tx = pool.begin().await?;
    assert!(issuer.issue_for_task(&mut tx, &task).await.is_err());
    tx.rollback().await?;
    task.state = State::Launching;
    query("UPDATE research.tasks SET state='launching',document=$1 WHERE task_id=$2")
        .bind(serde_json::to_value(&task)?)
        .bind(&task_id)
        .execute(&pool)
        .await?;
    query("UPDATE research.authority SET mode='paused'")
        .execute(&pool)
        .await?;
    let mut tx = pool.begin().await?;
    assert!(issuer.issue_for_task(&mut tx, &task).await.is_err());
    tx.rollback().await?;
    query("UPDATE research.authority SET mode='postgres'")
        .execute(&pool)
        .await?;
    revoke.reason_receipt_sha256 = hash('2');
    revoke.effective_ms = now - 1;
    ledger
        .register_native_request_revocation(&trust.verify_revocation(&sign_revocation(
            revoke,
            "identity-fixture-host".into(),
            &key,
        )?)?)
        .await?;
    let mut tx = pool.begin().await?;
    assert!(issuer.issue_for_task(&mut tx, &task).await.is_err());
    tx.rollback().await?;
    assert!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&projection)?)? == projected
    );
    task.state = State::Stopping;
    query("UPDATE research.tasks SET state='stopping',document=$1 WHERE task_id=$2")
        .bind(serde_json::to_value(&task)?)
        .bind(&task_id)
        .execute(&pool)
        .await?;
    let mut cleanup_tx = pool.begin().await?;
    let owned = restored_issuer
        .recover_for_cleanup(&mut cleanup_tx, &task)
        .await?
        .unwrap();
    assert_eq!(owned.scope_id(), recovered.scope_id());
    assert!(owned.late_files()["artifact.token"] == token);
    assert!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&projection)?)? == projected
    );
    let mut foreign = task.clone();
    foreign.fence += 1;
    assert!(restored_issuer
        .recover_for_cleanup(&mut cleanup_tx, &foreign)
        .await
        .is_err());
    cleanup_tx.rollback().await?;
    restored_issuer.cleanup(owned)?;
    assert!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(&projection)?)? == original);
    assert_eq!(std::fs::read_dir(state)?.count(), 0);
    let mut cleanup_tx = pool.begin().await?;
    assert!(restored_issuer
        .recover_for_cleanup(&mut cleanup_tx, &task)
        .await?
        .is_none());
    cleanup_tx.rollback().await?;
    pool.close().await;
    Ok(())
}
