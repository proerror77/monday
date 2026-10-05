#![cfg(feature = "control")]
use hft_cex_research_input::data::{BlockRef, DataViewSpec, Exit, PublishedView, Split, Window};
use hft_research_platform::{
    execution::{Acceptance, Backend, Profile},
    identity,
    orchestrator::{State, TaskKind, TaskSpec},
    postgres::{Ledger, BUILD_RELEASE_MIGRATION, MIGRATION, SESSION_DELIVERY_MIGRATION},
};
mod common;

fn hash(c: char) -> String {
    c.to_string().repeat(64)
}

#[test]
fn researchctl_rejects_direct_view_publication() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_researchctl"))
        .args(["publish-view", "unverified.json"])
        .env_remove("MONDAY_RESEARCH_DATABASE_URL")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("usage: researchctl"));
    assert!(!error.contains("publish-view MANIFEST"));
}

#[test]
fn researchctl_rejects_unsigned_build_registration_before_connecting_to_pg() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_researchctl"))
        .args(["register-build", "unverified.json"])
        .env_remove("MONDAY_RESEARCH_DATABASE_URL")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("SIGNED_RELEASE"));
}

#[test]
fn session_host_rejects_resume_without_registered_checkpoint_before_starting_child() {
    for arguments in [
        vec!["resume", "missing-config.json", "native.json"],
        vec![
            "resume",
            "missing-config.json",
            "invalid-checkpoint",
            "native.json",
        ],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_research-session"))
            .args(arguments)
            .env_remove("MONDAY_RESEARCH_DATABASE_URL")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains("CHECKPOINT_SHA256"));
    }
}

/// Only the explicitly named disposable test database is permitted. This test
/// never targets production, imports business data, or connects to Kubernetes.
#[tokio::test]
#[ignore = "requires disposable MONDAY_TEST_DATABASE_URL ending /monday_foundation_test"]
async fn postgres_single_authority_claims_idempotency_and_append_only_evidence(
) -> anyhow::Result<()> {
    let url = std::env::var("MONDAY_TEST_DATABASE_URL")?;
    anyhow::ensure!(
        url.ends_with("/monday_foundation_test"),
        "test database identity mismatch"
    );
    let pool = sqlx_postgres::PgPool::connect(&url).await?;
    sqlx_core::raw_sql::raw_sql(MIGRATION)
        .execute(&pool)
        .await?;
    sqlx_core::raw_sql::raw_sql(BUILD_RELEASE_MIGRATION)
        .execute(&pool)
        .await?;
    sqlx_core::raw_sql::raw_sql(SESSION_DELIVERY_MIGRATION)
        .execute(&pool)
        .await?;
    #[cfg(feature = "gateway")]
    sqlx_core::raw_sql::raw_sql(hft_research_platform::postgres::ARTIFACT_GATEWAY_MIGRATION)
        .execute(&pool)
        .await?;
    sqlx_core::raw_sql::raw_sql(hft_research_platform::postgres::NATIVE_ADMISSION_MIGRATION)
        .execute(&pool)
        .await?;
    sqlx_core::raw_sql::raw_sql(hft_research_platform::postgres::NATIVE_CAMPAIGN_INPUTS_MIGRATION)
        .execute(&pool)
        .await?;
    sqlx_core::raw_sql::raw_sql(
        hft_research_platform::postgres::NATIVE_REQUEST_REVOCATION_MIGRATION,
    )
    .execute(&pool)
    .await?;
    let ledger = Ledger::connect(&url).await?;
    let view = PublishedView {
        prepared_id: hash('a'),
        spec: DataViewSpec {
            schema: 1,
            venue: "fixture".into(),
            instrument: "fixture".into(),
            market: "usdm".into(),
            depth: 2,
            sources: vec![hash('a')],
            normalizer_sha256: hash('b'),
            feature_sql_sha256: hash('c'),
            feature_names: vec!["x".into()],
            window: Window {
                start_ns: 100,
                end_ns: 1000,
            },
            lookback_ns: 50,
            horizons_ns: vec![100],
            label_tolerance_ns: 0,
            fit_cutoff_ns: 1000,
            split: Split::Train,
        },
        blocks: vec![BlockRef {
            sha256: hash('d'),
            bytes: 16,
            rows: 1,
            decoded_bytes: 128,
            exit: Exit::Training,
        }],
        producer_image: format!("fixture@sha256:{}", hash('e')),
        source_receipt_sha256: hash('f'),
    };
    let mut plan = hft_research_platform::preparation::PreparationPlan {
        spec: view.spec.clone(),
        source_receipt_sha256: view.source_receipt_sha256.clone(),
        producer_image: view.producer_image.clone(),
        recipe_sql: None,
    };
    plan.spec.feature_sql_sha256 =
        hft_research_platform::sha256(hft_research_platform::preparation::PREPARE_SQL.as_bytes());
    assert!(ledger.register_plan(&plan).await.is_err());
    // Test-only activation fixture. No application path performs this UPDATE.
    sqlx_core::query::query("UPDATE research.authority SET mode='postgres',legacy_quiescence_sha256=$1,migration_receipt_sha256=$2").bind(hash('a')).bind(hash('b')).execute(&pool).await?;
    // Ledger fixtures do not claim verified object bytes. Production publication
    // enters through reconciler receipt readback and successful stop only.
    let view_sha = identity(&view)?;
    sqlx_core::query::query(
        "INSERT INTO research.inputs(manifest_sha256,kind,document) VALUES($1,'prepared',$2)",
    )
    .bind(&view_sha)
    .bind(serde_json::to_value(&view)?)
    .execute(&pool)
    .await?;
    sqlx_core::query::query("INSERT INTO research.views(view_id,manifest_sha256,manifest,source_receipt_sha256) VALUES($1,$2,$3,$4)")
        .bind(view.spec.id()?).bind(&view_sha).bind(serde_json::to_value(&view)?).bind(&view.source_receipt_sha256).execute(&pool).await?;
    let profile = Profile {
        backend: Backend::KubernetesJob,
        cluster: "fixture".into(),
        namespace: "research".into(),
        service_account: "worker".into(),
        architecture: "amd64".into(),
        cpu_millis: 1000,
        memory_mib: 128,
        scratch_mib: 64,
        gpu: 0,
        acceptance_sha256: hash('e'),
        prepared_pvc: None,
        worker_secret: None,
    };
    let acceptance = Acceptance {
        profile: profile.clone(),
        ready: true,
        process_tree_stop: true,
        immutable_prepared_mount: false,
        command_reattach: false,
        artifact_readback: true,
    };
    sqlx_core::query::query(
        "INSERT INTO research.backends(acceptance_sha256,acceptance,enabled) VALUES($1,$2,true)",
    )
    .bind(&profile.acceptance_sha256)
    .bind(serde_json::to_value(&acceptance)?)
    .execute(&pool)
    .await?;
    let mut spec = TaskSpec {
        schema: 1,
        kind: TaskKind::Train,
        run_manifest_sha256: hash('a'),
        view_manifest_sha256: view_sha,
        source_sha256: hash('a'),
        image: format!("fixture@sha256:{}", hash('b')),
        command: vec!["/usr/local/bin/fixture".into()],
        profile,
        timeout_ms: 10000,
        max_attempts: 2,
        output_prefix: "research/fixture".into(),
        fit_identity_sha256: None,
        worker_configuration: None,
    };
    let experiment = hft_research_platform::research::Experiment {
        schema: 1,
        hypothesis: "fixture hypothesis".into(),
        parent_experiment_sha256: None,
        variant: std::collections::BTreeMap::new(),
    };
    let experiment_sha = ledger.register_experiment("fixture", &experiment).await?;
    let build = hft_research_platform::build::BuildSpec {
        schema: 2,
        workspace_manifest: "research-core/Cargo.toml".into(),
        code_commit: "a".repeat(40),
        source_manifest_sha256: spec.source_sha256.clone(),
        cargo_lock_sha256: hash('a'),
        toolchain_manifest_sha256: hash('b'),
        target: "x86_64-unknown-linux-gnu".into(),
        packages: vec!["fixture".into()],
        binaries: vec!["fixture".into()],
        features: vec![],
        default_features: false,
        profile: "research".into(),
        profile_manifest_sha256: hash('c'),
        rustflags_sha256: hash('d'),
        native_environment_sha256: hash('e'),
        builder_image: format!("builder@sha256:{}", hash('f')),
    };
    let build_id = build.id()?;
    let artifact = hft_research_platform::build::BuildArtifact {
        schema: 1,
        build,
        image: spec.image.clone(),
        release_receipt_sha256: hash('a'),
        executables: vec![hft_research_platform::build::BuiltExecutable {
            name: "fixture".into(),
            blob: hft_research_platform::orchestrator::Artifact {
                key: format!("research/builds/{build_id}/fixture"),
                sha256: hash('b'),
                bytes: 16,
            },
        }],
    };
    // Historical unsigned rows remain readable by auditors, but cannot become
    // the Build of a new scientific Run.
    let unsigned_id = artifact.id()?;
    sqlx_core::query::query("INSERT INTO research.build_artifacts VALUES($1,$2,$3)")
        .bind(&unsigned_id)
        .bind(artifact.build.id()?)
        .bind(serde_json::to_value(&artifact)?)
        .execute(&pool)
        .await?;
    assert!(ledger.build_artifact(&unsigned_id).await.is_err());
    let (artifact, signed, trust) = common::attest(artifact);
    spec.source_sha256 = artifact.build.source_manifest_sha256.clone();
    let verified = trust.verify(&artifact, &signed)?;
    sqlx_core::query::query("UPDATE research.authority SET mode='paused'")
        .execute(&pool)
        .await?;
    let artifact_id = ledger.register_build(&verified).await?;
    assert_eq!(ledger.register_build(&verified).await?, artifact_id);
    assert_eq!(ledger.build_artifact(&artifact_id).await?, artifact);
    let mode: String = sqlx_core::query_scalar::query_scalar("SELECT mode FROM research.authority")
        .fetch_one(&pool)
        .await?;
    assert_eq!(mode, "paused");
    assert!(
        sqlx_core::query::query("UPDATE research.build_releases SET trust_sha256=$1")
            .bind(hash('f'))
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx_core::query::query("DELETE FROM research.build_releases")
            .execute(&pool)
            .await
            .is_err()
    );
    let release: serde_json::Value = sqlx_core::query_scalar::query_scalar(
        "SELECT document FROM research.build_releases WHERE artifact_sha256=$1",
    )
    .bind(&artifact_id)
    .fetch_one(&pool)
    .await?;
    let readback: hft_research_platform::release::SignedBuildRelease =
        serde_json::from_value(release)?;
    trust.verify(&artifact, &readback)?;
    sqlx_core::query::query("UPDATE research.authority SET mode='postgres'")
        .execute(&pool)
        .await?;
    let mut run = hft_research_platform::research::Run {
        schema: 1,
        experiment_sha256: experiment_sha,
        kind: spec.kind,
        build_artifact_sha256: artifact_id.clone(),
        configuration_sha256: hash('a'),
        command: spec.command.clone(),
        code_commit: "a".repeat(40),
        source_manifest_sha256: spec.source_sha256.clone(),
        image: spec.image.clone(),
        data_manifest_sha256: spec.view_manifest_sha256.clone(),
        seed: 7,
        evaluator_sha256: hash('c'),
        evaluation_protocol_sha256: hash('d'),
        fit_identity_sha256: spec.fit_identity_sha256.clone(),
    };
    spec.run_manifest_sha256 = ledger.register_run("fixture", &run).await?;
    assert!(ledger.submit("fixture", "one", spec.clone()).await.is_err());
    // An old row containing plausible receipt hashes remains audit history,
    // but cannot become current native authority without the signed import.
    let unsigned_admission = hft_research_platform::orchestrator::Admission {
        schema: 1,
        request_sha256: spec.id()?,
        task_spec: spec.clone(),
        resource_reservation_receipt_sha256: hash('a'),
        scientific_grant_receipt_sha256: hash('b'),
        release_admission_receipt_sha256: artifact.release_receipt_sha256.clone(),
        max_attempts: spec.max_attempts,
    };
    sqlx_core::query::query(
        "INSERT INTO research.admissions(request_sha256,document) VALUES($1,$2)",
    )
    .bind(&unsigned_admission.request_sha256)
    .bind(serde_json::to_value(&unsigned_admission)?)
    .execute(&pool)
    .await?;
    assert!(ledger
        .submit("fixture", "unsigned-native", spec.clone())
        .await
        .is_err());
    let admit = |spec: hft_research_platform::orchestrator::TaskSpec| {
        let ledger = &ledger;
        async move {
            use hft_research_platform::admission::{NativeAdmission, NativeAdmissionTrust};
            let run = ledger
                .run_for_tenant("fixture", &spec.run_manifest_sha256)
                .await?;
            let build = ledger.build_artifact(&run.build_artifact_sha256).await?;
            let a = hft_research_platform::orchestrator::Admission {
                schema: 1,
                request_sha256: spec.id()?,
                task_spec: spec.clone(),
                resource_reservation_receipt_sha256: hash('a'),
                scientific_grant_receipt_sha256: hash('b'),
                release_admission_receipt_sha256: build.release_receipt_sha256,
                max_attempts: spec.max_attempts,
            };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as i64;
            let evidence = NativeAdmission {
                schema: "monday.native_scientific_admission.v1".into(),
                tenant: "fixture".into(),
                native_request_sha256: run.configuration_sha256.clone(),
                run,
                operation_sha256: a.request_sha256.clone(),
                family_id: "synthetic-native-budget".into(),
                root_grant_sha256: hash('b'),
                approval_sha256: hash('c'),
                transfer_receipt_sha256: hash('d'),
                declared_trials: 1,
                reserved_job_seconds: (((spec.timeout_ms + 999) / 1000) as u64)
                    * u64::from(spec.max_attempts),
                reserved_llm_tokens: 0,
                issued_ms: now,
                expires_ms: now + 3_600_000,
                admission: a,
            };
            let key = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
            let signed = hft_research_platform::admission::sign(
                evidence,
                "fixture-native-issuer".into(),
                &key,
            )?;
            let public = key
                .verifying_key()
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let trust = NativeAdmissionTrust {
                schema: "monday.native_reservation_trust.v1".into(),
                native_reservation_keys: std::collections::BTreeMap::from([(
                    "fixture-native-issuer".into(),
                    public,
                )]),
            };
            ledger
                .register_native_admission(&trust.verify(&signed)?)
                .await?;
            Ok::<_, anyhow::Error>(())
        }
    };
    // A valid native witness cannot route the fixed Campaign through the
    // generic Training view or reuse a different input role.
    let mut campaign_spec = spec.clone();
    campaign_spec.kind = TaskKind::CexCampaign;
    campaign_spec.max_attempts = 1;
    campaign_spec.profile.acceptance_sha256 = hash('8');
    campaign_spec.profile.worker_secret = Some("static-configuration".into());
    campaign_spec.worker_configuration = Some(
        hft_research_platform::orchestrator::WorkerConfigurationRef {
            schema: "monday.worker_configuration.v1".into(),
            secret_name: "static-configuration".into(),
            secret_uid: "synthetic-source-uid".into(),
            configuration_sha256: hash('e'),
        },
    );
    campaign_spec.command = vec![
        "/usr/local/bin/fixture".into(),
        "mission".into(),
        "campaign-execute".into(),
        "--request-sha256".into(),
        run.configuration_sha256.clone(),
        "--pre-holdout".into(),
    ];
    let campaign_acceptance = Acceptance {
        profile: campaign_spec.profile.clone(),
        ..acceptance.clone()
    };
    sqlx_core::query::query(
        "INSERT INTO research.backends(acceptance_sha256,acceptance,enabled) VALUES($1,$2,true)",
    )
    .bind(&campaign_spec.profile.acceptance_sha256)
    .bind(serde_json::to_value(campaign_acceptance)?)
    .execute(&pool)
    .await?;
    let mut campaign_run = run.clone();
    campaign_run.kind = campaign_spec.kind;
    campaign_run.command = campaign_spec.command.clone();
    campaign_spec.run_manifest_sha256 = ledger.register_run("fixture", &campaign_run).await?;
    admit(campaign_spec.clone()).await?;
    let rejected = ledger
        .submit(
            "fixture",
            "campaign-cannot-use-generic-train",
            campaign_spec,
        )
        .await
        .unwrap_err();
    assert!(
        rejected.to_string().contains("typed collection"),
        "unexpected Campaign rejection: {rejected:#}"
    );
    plan.spec.split = Split::Validation;
    plan.producer_image = spec.image.clone();
    let plan_id = plan.id()?;
    assert!(ledger
        .register_plan(&plan)
        .await
        .unwrap_err()
        .to_string()
        .contains("only the training split"));
    // An older registered plan must also fail before consuming an attempt.
    sqlx_core::query::query(
        "INSERT INTO research.inputs(manifest_sha256,kind,document) VALUES($1,'plan',$2)",
    )
    .bind(&plan_id)
    .bind(serde_json::to_value(&plan)?)
    .execute(&pool)
    .await?;
    let mut preparation = spec.clone();
    preparation.kind = TaskKind::Prepare;
    preparation.view_manifest_sha256 = plan_id;
    let mut preparation_run = run.clone();
    preparation_run.kind = preparation.kind;
    preparation_run.data_manifest_sha256 = preparation.view_manifest_sha256.clone();
    preparation.run_manifest_sha256 = ledger.register_run("fixture", &preparation_run).await?;
    admit(preparation.clone()).await?;
    assert!(ledger
        .submit("fixture", "validation", preparation)
        .await
        .unwrap_err()
        .to_string()
        .contains("only the training split"));
    let count: i64 = sqlx_core::query_scalar::query_scalar("SELECT count(*) FROM research.tasks")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0);
    admit(spec.clone()).await?;
    let native_row: serde_json::Value = sqlx_core::query_scalar::query_scalar(
        "SELECT document FROM research.native_admission_imports WHERE request_sha256=$1",
    )
    .bind(spec.id()?)
    .fetch_one(&pool)
    .await?;
    let mut duplicate: hft_research_platform::admission::SignedNativeAdmission =
        serde_json::from_value(native_row)?;
    let original_operation = duplicate.evidence.operation_sha256.clone();
    let key = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
    let public = key
        .verifying_key()
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let native_trust = hft_research_platform::admission::NativeAdmissionTrust {
        schema: "monday.native_reservation_trust.v1".into(),
        native_reservation_keys: std::collections::BTreeMap::from([(
            "fixture-native-issuer".into(),
            public,
        )]),
    };
    duplicate.evidence.tenant = "another".into();
    let foreign = hft_research_platform::admission::sign(
        duplicate.evidence,
        "fixture-native-issuer".into(),
        &key,
    )?;
    assert!(ledger
        .register_native_admission(&native_trust.verify(&foreign)?)
        .await
        .is_err());
    let id = ledger.submit("fixture", "one", spec.clone()).await?;
    assert_eq!(id, ledger.submit("fixture", "one", spec.clone()).await?);
    let status = hft_research_platform::agent_api::execute(
        &ledger,
        "fixture",
        hft_research_platform::research::ResearchTool::Status {
            run_sha256: spec.run_manifest_sha256.clone(),
        },
    )
    .await?;
    assert_eq!(status["state"], "queued");
    assert!(hft_research_platform::agent_api::execute(
        &ledger,
        "another",
        hft_research_platform::research::ResearchTool::Status {
            run_sha256: spec.run_manifest_sha256.clone()
        }
    )
    .await
    .is_err());
    let mut conflicting = spec.clone();
    conflicting.command.push("changed".into());
    assert!(ledger.submit("fixture", "one", conflicting).await.is_err());
    let mut second = spec;
    second.command.push("second".into());
    run.seed += 1;
    run.command = second.command.clone();
    second.run_manifest_sha256 = ledger.register_run("fixture", &run).await?;
    admit(second.clone()).await?;
    let second_row: serde_json::Value = sqlx_core::query_scalar::query_scalar(
        "SELECT document FROM research.native_admission_imports WHERE request_sha256=$1",
    )
    .bind(second.id()?)
    .fetch_one(&pool)
    .await?;
    let mut reused: hft_research_platform::admission::SignedNativeAdmission =
        serde_json::from_value(second_row)?;
    reused.evidence.operation_sha256 = original_operation;
    let reused = hft_research_platform::admission::sign(
        reused.evidence,
        "fixture-native-issuer".into(),
        &key,
    )?;
    assert!(ledger
        .register_native_admission(&native_trust.verify(&reused)?)
        .await
        .is_err());
    ledger.submit("fixture", "two", second).await?;
    let session_temp = tempfile::tempdir()?;
    let session_root = session_temp.path().canonicalize()?;
    let fixture_source = session_root.join("fixture.rs");
    std::fs::write(&fixture_source, include_str!("fixtures/app_server.rs"))?;
    let fixture_binary = session_root.join("fixture-codex");
    anyhow::ensure!(
        std::process::Command::new("rustc")
            .arg("--edition=2021")
            .arg(&fixture_source)
            .arg("-o")
            .arg(&fixture_binary)
            .status()?
            .success(),
        "native protocol fixture compile failed"
    );
    let workspace = session_root.join("workspace");
    std::fs::create_dir(&workspace)?;
    anyhow::ensure!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&workspace)
            .status()?
            .success(),
        "fixture Git init failed"
    );
    anyhow::ensure!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "fixture"
            ])
            .status()?
            .success(),
        "fixture Git commit failed"
    );
    let session_config = hft_research_platform::session::SessionConfig {
        executable_sha256: hft_research_platform::sha256(&std::fs::read(&fixture_binary)?),
        executable: fixture_binary,
        workspace,
        native_home: session_root.join("native"),
        delivery_directory: session_root.join("delivery"),
    };
    let mut native_session =
        hft_research_platform::session::AppServer::start(session_config.clone()).await?;
    native_session.open_thread(None).await?;
    let session = hft_research_platform::research::Session {
        schema: 1,
        experiment_sha256: run.experiment_sha256.clone(),
        provider: hft_research_platform::research::CodingAgent::CodexAppServer,
        provider_version: hft_research_platform::coding_agent::SCHEMA_VERSION.into(),
        provider_thread_id: native_session.thread_id().unwrap().into(),
        provider_binary_sha256: session_config.executable_sha256.clone(),
        capability_policy_receipt_sha256: hash('b'),
    };
    let session_id = ledger.register_session("fixture", &session).await?;
    ledger.subscribe("fixture", &session_id, &run.id()?).await?;
    ledger
        .subscribe(
            "fixture",
            &session_id,
            &ledger.read(&id).await?.spec.run_manifest_sha256,
        )
        .await?;
    assert!(ledger
        .subscribe("another", &session_id, &run.id()?)
        .await
        .is_err());
    let build_count: i64 =
        sqlx_core::query_scalar::query_scalar("SELECT count(*) FROM research.build_artifacts")
            .fetch_one(&pool)
            .await?;
    assert_eq!(build_count, 2);
    let claim = |owner: &'static str| async {
        match ledger.lock_next(owner, 30000).await? {
            Some(locked) => {
                let id = locked.task.id.clone();
                locked.commit("fixture_claim").await?;
                Ok::<_, anyhow::Error>(Some(id))
            }
            None => Ok(None),
        }
    };
    let (a, b) = tokio::join!(claim("owner-a"), claim("owner-b"));
    assert_eq!(usize::from(a?.is_some()) + usize::from(b?.is_some()), 1);
    assert_eq!(ledger.read(&id).await?.state, State::Launching);
    ledger.cancel(&id).await?;
    assert_eq!(ledger.read(&id).await?.state, State::Stopping);
    sqlx_core::query::query("UPDATE research.authority SET mode='paused'")
        .execute(&pool)
        .await?;
    let mut drain = ledger
        .lock_next("drainer", 30000)
        .await?
        .expect("paused authority must drain existing stop");
    assert_eq!(drain.task.state, State::Stopping);
    drain.task.stopped(drain.task.attempt, drain.task.fence)?;
    drain.commit("fixture_stopped_while_paused").await?;
    assert_eq!(ledger.read(&id).await?.state, State::Cancelled);
    assert!(ledger.lock_next("drainer", 30000).await?.is_none());
    let intents: i64 = sqlx_core::query_scalar::query_scalar(
        "SELECT count(*) FROM research.completion_intents WHERE session_sha256=$1",
    )
    .bind(&session_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(intents, 1);
    let pending = ledger.pending_completions("fixture", &session_id).await?;
    assert_eq!(pending.len(), 1);
    assert!(
        hft_research_platform::postgres::completion_message(&pending[0].0, &pending[0].1)?
            .contains("cancelled")
    );
    assert!(ledger
        .pending_completions("another", &session_id)
        .await
        .is_err());
    let message =
        hft_research_platform::postgres::completion_message(&pending[0].0, &pending[0].1)?;
    native_session.send_message(&pending[0].0, &message).await?;
    let delivery = native_session.verified_delivery(&pending[0].0).await?;
    assert!(ledger
        .record_completion_delivery("another", &session_id, &delivery)
        .await
        .is_err());
    ledger
        .record_completion_delivery("fixture", &session_id, &delivery)
        .await?;
    ledger
        .record_completion_delivery("fixture", &session_id, &delivery)
        .await?;
    assert!(ledger
        .pending_completions("fixture", &session_id)
        .await?
        .is_empty());
    assert!(sqlx_core::query::query(
        "UPDATE research.completion_deliveries SET native_readback_sha256=$1"
    )
    .bind(hash('e'))
    .execute(&pool)
    .await
    .is_err());
    let native = native_session.checkpoint().await?;
    let native_bytes = serde_json::to_vec(&native)?;

    sqlx_core::query::query("UPDATE research.authority SET mode='postgres'")
        .execute(&pool)
        .await?;
    let mut checkpoint = hft_research_platform::research::SessionSnapshot {
        session_sha256: session_id.clone(),
        parent_snapshot_sha256: None,
        code_commit: native.host.as_ref().unwrap().code_commit.clone(),
        workspace_manifest_sha256: native
            .host
            .as_ref()
            .unwrap()
            .workspace_manifest_sha256
            .clone(),
        transcript_manifest_sha256: native
            .host
            .as_ref()
            .unwrap()
            .transcript_manifest_sha256
            .clone(),
        native_state_manifest_sha256: hft_research_platform::sha256(&native_bytes),
    };
    assert!(ledger
        .session_checkpoint_for_resume("fixture", &checkpoint.id()?)
        .await
        .is_err());
    let checkpoint_id = ledger.snapshot_session("fixture", &checkpoint).await?;
    assert!(ledger
        .session_checkpoint_for_resume("another", &checkpoint_id)
        .await
        .is_err());
    assert!(ledger
        .session_checkpoint_for_resume("fixture", &hash('0'))
        .await
        .is_err());
    assert_eq!(
        ledger.snapshot_session("fixture", &checkpoint).await?,
        checkpoint_id
    );
    let registered = ledger
        .session_checkpoint_for_resume("fixture", &checkpoint_id)
        .await?;
    let resumed = registered
        .resume(session_config.clone(), &native_bytes)
        .await?;
    resumed.close().await?;
    let native_path = session_root.join("native-manifest.json");
    std::fs::write(&native_path, &native_bytes)?;
    let token_path = session_root.join("research.token");
    std::fs::write(&token_path, "x".repeat(32))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&session_root, std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600))?;
    }
    let config_path = session_root.join("host.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&serde_json::json!({
            "session":session_config,
            "tenant":"fixture",
            "experiment_sha256":session.experiment_sha256,
            "capability_policy_receipt_sha256":session.capability_policy_receipt_sha256,
            "research_endpoint":"http://127.0.0.1:9/research",
            "research_token_file":token_path,
        }))?,
    )?;
    // Exercise the production host command against PG and the offline native
    // protocol peer. No broker request, model turn or external send occurs.
    let mut host = std::process::Command::new(env!("CARGO_BIN_EXE_research-session"))
        .arg("resume")
        .arg(&config_path)
        .arg(&checkpoint_id)
        .arg(&native_path)
        .env("MONDAY_RESEARCH_DATABASE_URL", &url)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    {
        use std::io::Write;
        host.stdin
            .take()
            .unwrap()
            .write_all(b"{\"operation\":\"close\"}\n")?;
    }
    let output = host.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "registered host resume failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout)?;
    let opened: serde_json::Value = serde_json::from_str(output.lines().next().unwrap())?;
    assert_eq!(opened["session_sha256"], session_id);
    assert!(output.contains("\"child_stopped\":true"));
    checkpoint.parent_snapshot_sha256 = Some(checkpoint_id.clone());
    let registered = ledger
        .session_checkpoint_for_resume("fixture", &checkpoint_id)
        .await?;
    let writer = Ledger::connect(&url).await?;
    let next = checkpoint.clone();
    let mut append = tokio::spawn(async move { writer.snapshot_session("fixture", &next).await });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let blocked: bool = sqlx_core::query_scalar::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname='monday_foundation_test' AND wait_event_type='Lock' AND query LIKE '%research.sessions%FOR UPDATE%')").fetch_one(&pool).await?;
        if blocked {
            break;
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline && !append.is_finished(),
            "checkpoint append did not wait for native admission"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut append)
            .await
            .is_err()
    );
    let resumed = registered
        .resume(session_config.clone(), &native_bytes)
        .await?;
    resumed.close().await?;
    let next_checkpoint = append.await??;
    assert!(ledger
        .session_checkpoint_for_resume("fixture", &checkpoint_id)
        .await
        .is_err());
    assert_eq!(
        ledger
            .session_checkpoint_for_resume("fixture", &next_checkpoint)
            .await?
            .snapshot()
            .id()?,
        next_checkpoint
    );
    // A privileged fixture injects a fork that normal locked registration
    // rejects. Neither branch is a canonical checkpoint for resume.
    let mut fork = checkpoint.clone();
    fork.code_commit = "e".repeat(40);
    let fork_id = fork.id()?;
    sqlx_core::query::query("INSERT INTO research.session_snapshots(snapshot_sha256,session_sha256,parent_snapshot_sha256,document) VALUES($1,$2,$3,$4)")
        .bind(&fork_id).bind(&session_id).bind(&checkpoint_id).bind(serde_json::to_value(&fork)?).execute(&pool).await?;
    assert!(ledger
        .session_checkpoint_for_resume("fixture", &next_checkpoint)
        .await
        .is_err());
    assert!(ledger
        .session_checkpoint_for_resume("fixture", &fork_id)
        .await
        .is_err());
    assert!(ledger
        .snapshot_session("fixture", &checkpoint)
        .await
        .is_err());
    let mut result = ledger.lock_next("result-owner", 30000).await?.unwrap();
    let mut lease = result.task.lease.clone().unwrap();
    let original_launch_lease = lease.clone();
    let handle = hft_research_platform::execution::ExecutionHandle {
        backend: result.task.spec.profile.backend,
        cluster: result.task.spec.profile.cluster.clone(),
        namespace: result.task.spec.profile.namespace.clone(),
        name: hft_research_platform::execution::resource_name(&lease),
        uid: "fixture-result".into(),
        attempt: lease.attempt,
        fence: lease.fence,
        task_id: result.task.id.clone(),
        request_sha256: result.task.id.clone(),
    };
    result.task.launched(&lease, result.now_ms, handle)?;
    result.commit("fixture_initial_launch").await?;
    result = ledger.lock_next("result-owner", 30000).await?.unwrap();
    #[cfg(feature = "gateway")]
    {
        let task = result.task.id.clone();
        let prefix = format!(
            "{}/{}/{}/",
            result.task.spec.output_prefix, task, lease.attempt
        );
        result.commit("fixture_running").await?;
        assert_eq!(
            ledger
                .artifact_writer("fixture", &task, lease.attempt, lease.fence)
                .await?,
            prefix
        );
        assert!(ledger
            .artifact_writer("foreign", &task, lease.attempt, lease.fence)
            .await
            .is_err());
        assert!(ledger
            .artifact_writer("fixture", &task, lease.attempt + 1, lease.fence)
            .await
            .is_err());
        assert!(ledger
            .artifact_writer("fixture", &task, lease.attempt, lease.fence + 1)
            .await
            .is_err());
        // The gateway role can order publication with cancellation using one
        // reviewed function, without UPDATE/INSERT rights on the ledger.
        sqlx_core::raw_sql::raw_sql("CREATE ROLE monday_gateway_fixture; GRANT USAGE ON SCHEMA research TO monday_gateway_fixture; GRANT EXECUTE ON FUNCTION research.artifact_write_permit(text,text,integer,bigint) TO monday_gateway_fixture;")
            .execute(&pool).await?;
        let mut permit = pool.begin().await?;
        sqlx_core::query::query("SET LOCAL ROLE monday_gateway_fixture")
            .execute(&mut *permit)
            .await?;
        let admitted: String = sqlx_core::query_scalar::query_scalar(
            "SELECT research.artifact_write_permit($1,$2,$3,$4)",
        )
        .bind("fixture")
        .bind(&task)
        .bind(lease.attempt as i32)
        .bind(lease.fence)
        .fetch_one(&mut *permit)
        .await?;
        assert_eq!(admitted, prefix);
        let claim_task = task.clone();
        let takeover = sqlx_core::query::query(
            "SELECT task_id FROM research.tasks WHERE task_id=$1 FOR UPDATE",
        )
        .bind(&claim_task)
        .fetch_one(&pool);
        tokio::pin!(takeover);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut takeover)
                .await
                .is_err()
        );
        assert!(sqlx_core::query::query(
            "UPDATE research.admissions SET document=document WHERE request_sha256=$1"
        )
        .bind(&task)
        .execute(&mut *permit)
        .await
        .is_err());
        permit.rollback().await?;
        takeover.await?;
        result = ledger.lock_next("result-owner", 30000).await?.unwrap();
    }
    lease = result.task.heartbeat(&lease, result.now_ms, 90_000)?;
    result.commit("fixture_heartbeat").await?;
    result = ledger.lock_next("result-owner", 30000).await?.unwrap();
    let receipt = hft_research_platform::orchestrator::ResultReceipt {
        task_id: result.task.id.clone(),
        attempt: lease.attempt,
        fence: lease.fence,
        view_manifest_sha256: result.task.spec.view_manifest_sha256.clone(),
        source_sha256: result.task.spec.source_sha256.clone(),
        image: result.task.spec.image.clone(),
        fit_identity_sha256: result.task.spec.fit_identity_sha256.clone(),
        artifacts: vec![hft_research_platform::orchestrator::Artifact {
            key: format!(
                "{}/{}/{}/fixture.bin",
                result.task.spec.output_prefix, result.task.id, lease.attempt
            ),
            sha256: hash('f'),
            bytes: 1,
        }],
        checkpoint: None,
        prepared_view: None,
    };
    result.task.stage_result(&lease, result.now_ms, receipt)?;
    let result_id = result.task.id.clone();
    result.commit("fixture_result_staged").await?;
    assert!(ledger
        .native_terminal_snapshot("fixture", &result_id)
        .await
        .is_err());
    sqlx_core::query::query(
        "INSERT INTO research.revocations(request_sha256,reason_receipt_sha256) VALUES($1,$2)",
    )
    .bind(&result_id)
    .bind(hash('f'))
    .execute(&pool)
    .await?;
    #[cfg(feature = "gateway")]
    assert!(ledger
        .artifact_writer("fixture", &result_id, lease.attempt, lease.fence)
        .await
        .is_err());
    let mut stopped = ledger.lock_next("result-owner", 30000).await?.unwrap();
    stopped.task.stopped(lease.attempt, lease.fence)?;
    assert!(stopped
        .commit("fixture_revoked_success")
        .await
        .unwrap_err()
        .to_string()
        .contains("admission was revoked"));
    assert_eq!(ledger.read(&result_id).await?.state, State::Stopping);
    let count: i64 = sqlx_core::query_scalar::query_scalar("SELECT count(*) FROM research.results")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0);
    let mut cancelled = ledger.lock_next("result-owner", 30000).await?.unwrap();
    cancelled.task.stop(State::Cancelled, false)?;
    cancelled.task.stopped(lease.attempt, lease.fence)?;
    cancelled.commit("stop_reconciled").await?;
    assert_eq!(ledger.read(&result_id).await?.state, State::Cancelled);
    assert!(ledger.read(&result_id).await?.receipt.is_none());
    // Historical readback is not new execution authority. Revocation remains
    // readable, while foreign scopes and missing reconciled events are rejected.
    let snapshot = ledger
        .native_terminal_snapshot("fixture", &result_id)
        .await?;
    assert_eq!(snapshot.task.state, State::Cancelled);
    assert_eq!(snapshot.native_admission.evidence.tenant, "fixture");
    assert!(snapshot.result.is_none());
    assert_eq!(
        snapshot
            .execution_event
            .as_ref()
            .unwrap()
            .document
            .lease
            .as_ref(),
        Some(&original_launch_lease)
    );
    assert_ne!(original_launch_lease.expires_ms, lease.expires_ms);
    assert_eq!(
        snapshot
            .execution_event
            .as_ref()
            .unwrap()
            .document
            .execution
            .as_ref()
            .unwrap()
            .uid,
        "fixture-result"
    );
    assert!(ledger
        .native_terminal_snapshot("foreign", &result_id)
        .await
        .is_err());
    assert!(ledger
        .native_terminal_snapshot("fixture", &hash('0'))
        .await
        .is_err());
    // A readback cannot append an event or charge/refund a native budget.
    let before: i64 = sqlx_core::query_scalar::query_scalar("SELECT count(*) FROM research.events")
        .fetch_one(&pool)
        .await?;
    ledger
        .native_terminal_snapshot("fixture", &result_id)
        .await?;
    let after: i64 = sqlx_core::query_scalar::query_scalar("SELECT count(*) FROM research.events")
        .fetch_one(&pool)
        .await?;
    assert_eq!(before, after);

    // A separate synthetic Prepare request exercises scheduled source revocation.
    // These signatures prove receiver semantics, never real source publication.
    plan.spec.split = Split::Train;
    let mut revoke_spec = ledger.read(&result_id).await?.spec;
    revoke_spec.kind = TaskKind::Prepare;
    revoke_spec.timeout_ms = 60_000;
    revoke_spec.view_manifest_sha256 = ledger.register_plan(&plan).await?;
    run.kind = revoke_spec.kind;
    run.data_manifest_sha256 = revoke_spec.view_manifest_sha256.clone();
    run.seed += 1;
    revoke_spec.run_manifest_sha256 = ledger.register_run("fixture", &run).await?;
    admit(revoke_spec.clone()).await?;
    let revoke_task = ledger
        .submit("fixture", "source-revoke", revoke_spec.clone())
        .await?;
    let native: hft_research_platform::admission::SignedNativeAdmission = serde_json::from_value(
        sqlx_core::query_scalar::query_scalar(
            "SELECT document FROM research.native_admission_imports WHERE request_sha256=$1",
        )
        .bind(&revoke_task)
        .fetch_one(&pool)
        .await?,
    )?;
    use hft_research_platform::revocation::{
        sign_revocation, NativeRequestRevocation, NATIVE_REQUEST_REVOCATION_SCHEMA,
    };
    let now: i64 = sqlx_core::query_scalar::query_scalar(
        "SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint",
    )
    .fetch_one(&pool)
    .await?;
    let mut evidence = NativeRequestRevocation {
        schema: NATIVE_REQUEST_REVOCATION_SCHEMA.into(),
        tenant: native.evidence.tenant.clone(),
        request_sha256: revoke_task.clone(),
        operation_sha256: native.evidence.operation_sha256.clone(),
        family_id: native.evidence.family_id.clone(),
        root_grant_sha256: native.evidence.root_grant_sha256.clone(),
        reason_receipt_sha256: hash('1'),
        effective_ms: now + 40_000,
        issued_ms: now,
    };
    let witness = |e| {
        native_trust.verify_revocation(&sign_revocation(e, "fixture-native-issuer".into(), &key)?)
    };
    // A trusted signature still cannot redirect the original admitted identity.
    for field in 0..5 {
        let mut changed = evidence.clone();
        match field {
            0 => changed.tenant = "foreign".into(),
            1 => changed.request_sha256 = hash('9'),
            2 => changed.operation_sha256 = hash('9'),
            3 => changed.family_id = "foreign".into(),
            _ => changed.root_grant_sha256 = hash('9'),
        }
        assert!(ledger
            .register_native_request_revocation(&witness(changed)?)
            .await
            .is_err());
    }
    let count: i64 = sqlx_core::query_scalar::query_scalar(
        "SELECT count(*) FROM research.native_request_revocations",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 0);
    // Import uses the original admission row lock, including when it waited.
    let mut admission_lock = pool.begin().await?;
    sqlx_core::query::query(
        "SELECT request_sha256 FROM research.admissions WHERE request_sha256=$1 FOR UPDATE",
    )
    .bind(&revoke_task)
    .fetch_one(&mut *admission_lock)
    .await?;
    let verified = witness(evidence.clone())?;
    let import = ledger.register_native_request_revocation(&verified);
    tokio::pin!(import);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut import)
            .await
            .is_err()
    );
    admission_lock.rollback().await?;
    let revocation_id = import.await?;
    assert_eq!(
        ledger.register_native_request_revocation(&verified).await?,
        revocation_id
    );
    let mut conflicting = evidence.clone();
    conflicting.effective_ms += 1;
    assert!(ledger
        .register_native_request_revocation(&witness(conflicting)?)
        .await
        .is_err());
    let mut later = evidence.clone();
    later.reason_receipt_sha256 = hash('2');
    later.effective_ms += 1_000;
    ledger
        .register_native_request_revocation(&witness(later)?)
        .await?;
    // A later recorded reason can take effect earlier; history is retained.
    evidence.reason_receipt_sha256 = hash('3');
    evidence.effective_ms -= 1_000;
    sqlx_core::raw_sql::raw_sql("CREATE ROLE monday_revocation_importer; GRANT USAGE ON SCHEMA research TO monday_revocation_importer; GRANT SELECT ON research.admissions,research.native_admission_imports TO monday_revocation_importer; GRANT UPDATE(request_sha256) ON research.admissions TO monday_revocation_importer; GRANT SELECT,INSERT ON research.native_request_revocations TO monday_revocation_importer;").execute(&pool).await?;
    let importer_url = format!("{url}?options=-c%20role%3Dmonday_revocation_importer");
    let importer = Ledger::connect(&importer_url).await?;
    importer
        .register_native_request_revocation(&witness(evidence.clone())?)
        .await?;
    let importer_pool = sqlx_postgres::PgPool::connect(&importer_url).await?;
    let error = sqlx_core::query::query(
        "UPDATE research.admissions SET request_sha256=request_sha256 WHERE request_sha256=$1",
    )
    .bind(&revoke_task)
    .execute(&importer_pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("immutable research record"));
    let error = sqlx_core::query::query(
        "INSERT INTO research.revocations(request_sha256,reason_receipt_sha256) VALUES($1,$2)",
    )
    .bind(&revoke_task)
    .bind(hash('5'))
    .execute(&importer_pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("permission denied"));
    let error = sqlx_core::query::query(
        "UPDATE research.native_request_revocations SET effective_ms=effective_ms",
    )
    .execute(&importer_pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("permission denied"));
    importer_pool.close().await;
    assert_eq!(
        ledger.native_request_deadline_ms(&revoke_task).await?,
        Some(evidence.effective_ms)
    );
    assert!(ledger.admission(&revoke_spec).await?.is_some());
    assert!(ledger.admits_launch(&revoke_spec, 1_000).await?);
    assert!(
        !ledger
            .admits_launch(&revoke_spec, revoke_spec.timeout_ms)
            .await?
    );
    let count: i64 = sqlx_core::query_scalar::query_scalar(
        "SELECT count(*) FROM research.native_request_revocations",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 3);
    assert!(sqlx_core::query::query(
        "UPDATE research.native_request_revocations SET effective_ms=effective_ms"
    )
    .execute(&pool)
    .await
    .is_err());
    assert!(
        sqlx_core::query::query("DELETE FROM research.native_request_revocations")
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx_core::raw_sql::raw_sql("CREATE ROLE monday_revocation_reader; GRANT USAGE ON SCHEMA research TO monday_revocation_reader; GRANT EXECUTE ON FUNCTION research.native_request_deadline_ms(text) TO monday_revocation_reader;").execute(&pool).await?;
    let mut reader = pool.begin().await?;
    sqlx_core::query::query("SET LOCAL ROLE monday_revocation_reader")
        .execute(&mut *reader)
        .await?;
    let cap: Option<i64> =
        sqlx_core::query_scalar::query_scalar("SELECT research.native_request_deadline_ms($1)")
            .bind(&revoke_task)
            .fetch_one(&mut *reader)
            .await?;
    assert_eq!(cap, Some(evidence.effective_ms));
    assert!(sqlx_core::query::query("INSERT INTO research.native_request_revocations SELECT * FROM research.native_request_revocations").execute(&mut *reader).await.is_err());
    reader.rollback().await?;
    let mut running = ledger.lock_next("revoke-owner", 30_000).await?.unwrap();
    assert_eq!(running.task.id, revoke_task);
    let cap = running.native_request_deadline_ms().await?.unwrap();
    running.task.deadline_ms = running.task.deadline_ms.map(|deadline| deadline.min(cap));
    let revoke_lease = running.task.lease.clone().unwrap();
    let handle = hft_research_platform::execution::ExecutionHandle {
        backend: running.task.spec.profile.backend,
        cluster: running.task.spec.profile.cluster.clone(),
        namespace: running.task.spec.profile.namespace.clone(),
        name: hft_research_platform::execution::resource_name(&revoke_lease),
        uid: "fixture-revoke".into(),
        attempt: revoke_lease.attempt,
        fence: revoke_lease.fence,
        task_id: revoke_task.clone(),
        request_sha256: revoke_task.clone(),
    };
    running
        .task
        .launched(&revoke_lease, running.now_ms, handle)?;
    running.commit("fixture_revoke_running").await?;
    let mut preparation = ledger
        .preparation(&revoke_task, revoke_lease.attempt, revoke_lease.fence)
        .await?;
    assert_eq!(preparation.task().deadline_ms, Some(evidence.effective_ms));
    evidence.reason_receipt_sha256 = hash('4');
    evidence.effective_ms = now - 1;
    let due = witness(evidence.clone())?;
    #[cfg(feature = "gateway")]
    {
        // Upload holds the same admission lock until publication completes.
        let mut permit = pool.begin().await?;
        sqlx_core::query::query("SET LOCAL ROLE monday_gateway_fixture")
            .execute(&mut *permit)
            .await?;
        let _: String = sqlx_core::query_scalar::query_scalar(
            "SELECT research.artifact_write_permit($1,$2,$3,$4)",
        )
        .bind("fixture")
        .bind(&revoke_task)
        .bind(revoke_lease.attempt as i32)
        .bind(revoke_lease.fence)
        .fetch_one(&mut *permit)
        .await?;
        let mut during_upload = evidence.clone();
        during_upload.reason_receipt_sha256 = hash('5');
        during_upload.effective_ms = cap + 1_000;
        let during_upload = witness(during_upload)?;
        let import = ledger.register_native_request_revocation(&during_upload);
        tokio::pin!(import);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut import)
                .await
                .is_err()
        );
        permit.rollback().await?;
        import.await?;
        // Reverse ordering: an upload waiting on import must see its new cap.
        // Pause the validated import transaction at its pre-commit boundary.
        due.evidence().matches_admission(&native.evidence)?;
        let mut import_tx = pool.begin().await?;
        sqlx_core::query::query("SET LOCAL ROLE monday_revocation_importer")
            .execute(&mut *import_tx)
            .await?;
        sqlx_core::query::query(
            "SELECT request_sha256 FROM research.admissions WHERE request_sha256=$1 FOR UPDATE",
        )
        .bind(&revoke_task)
        .fetch_one(&mut *import_tx)
        .await?;
        sqlx_core::query::query("INSERT INTO research.native_request_revocations(evidence_sha256,request_sha256,tenant,operation_sha256,family_id,root_grant_sha256,reason_receipt_sha256,effective_ms,issued_ms,trust_sha256,document,trust_document) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
            .bind(due.evidence().id()?).bind(&revoke_task).bind(&due.evidence().tenant).bind(&due.evidence().operation_sha256)
            .bind(&due.evidence().family_id).bind(&due.evidence().root_grant_sha256).bind(&due.evidence().reason_receipt_sha256)
            .bind(due.evidence().effective_ms).bind(due.evidence().issued_ms).bind(due.trust_sha256())
            .bind(serde_json::to_value(due.signed())?).bind(serde_json::to_value(&native_trust)?)
            .execute(&mut *import_tx).await?;
        let upload = sqlx_core::query_scalar::query_scalar::<_, String>(
            "SELECT research.artifact_write_permit($1,$2,$3,$4)",
        )
        .bind("fixture")
        .bind(&revoke_task)
        .bind(revoke_lease.attempt as i32)
        .bind(revoke_lease.fence)
        .fetch_one(&pool);
        tokio::pin!(upload);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut upload)
                .await
                .is_err()
        );
        import_tx.commit().await?;
        assert!(upload
            .await
            .unwrap_err()
            .to_string()
            .contains("native artifact admission absent, revoked or expired"));
        assert_eq!(
            ledger.register_native_request_revocation(&due).await?,
            due.evidence().id()?
        );
    }
    #[cfg(not(feature = "gateway"))]
    ledger.register_native_request_revocation(&due).await?;
    assert!(ledger.admission(&revoke_spec).await?.is_none());
    assert!(!ledger.admits_launch(&revoke_spec, 1_000).await?);
    assert!(preparation.check().await.is_err());
    assert!(ledger
        .artifact_writer(
            "fixture",
            &revoke_task,
            revoke_lease.attempt,
            revoke_lease.fence
        )
        .await
        .is_err());
    drop(preparation);
    let mut completed = ledger.lock_next("revoke-owner", 30_000).await?.unwrap();
    let mut revoked_view = view.clone();
    revoked_view.spec = plan.spec.clone();
    revoked_view.prepared_id = identity(&(&revoke_task, revoke_lease.attempt, revoke_lease.fence))?;
    revoked_view.producer_image = completed.task.spec.image.clone();
    let receipt = hft_research_platform::orchestrator::ResultReceipt {
        task_id: revoke_task.clone(),
        attempt: revoke_lease.attempt,
        fence: revoke_lease.fence,
        view_manifest_sha256: completed.task.spec.view_manifest_sha256.clone(),
        source_sha256: completed.task.spec.source_sha256.clone(),
        image: completed.task.spec.image.clone(),
        fit_identity_sha256: None,
        artifacts: vec![hft_research_platform::orchestrator::Artifact {
            key: format!(
                "{}/{}/{}/{}.mondaybin",
                completed.task.spec.output_prefix,
                revoke_task,
                revoke_lease.attempt,
                hash('d')
            ),
            sha256: hash('d'),
            bytes: 16,
        }],
        checkpoint: None,
        prepared_view: Some(revoked_view),
    };
    completed
        .task
        .stage_result(&revoke_lease, completed.now_ms, receipt)?;
    completed.commit("fixture_revoke_result_staged").await?;
    let mut completed = ledger.lock_next("revoke-owner", 30_000).await?.unwrap();
    completed
        .task
        .stopped(revoke_lease.attempt, revoke_lease.fence)?;
    assert!(completed
        .commit("fixture_revoke_terminal_rejected")
        .await
        .is_err());
    let count: i64 = sqlx_core::query_scalar::query_scalar(
        "SELECT count(*) FROM research.results WHERE task_id=$1",
    )
    .bind(&revoke_task)
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 0);
    // Source history does not alter existing immediate manual PG revocation.
    let manual: String = sqlx_core::query_scalar::query_scalar(
        "SELECT reason_receipt_sha256 FROM research.revocations WHERE request_sha256=$1",
    )
    .bind(&result_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(manual, hash('f'));
    assert!(sqlx_core::query::query("DELETE FROM research.revocations")
        .execute(&pool)
        .await
        .is_err());

    let count: i64 = sqlx_core::query_scalar::query_scalar("SELECT count(*) FROM research.events")
        .fetch_one(&pool)
        .await?;
    assert!(count >= 2);
    assert!(sqlx_core::query::query("DELETE FROM research.events")
        .execute(&pool)
        .await
        .is_err());
    assert!(
        sqlx_core::query::query("UPDATE research.views SET source_receipt_sha256=$1")
            .bind(hash('a'))
            .execute(&pool)
            .await
            .is_err()
    );
    assert_eq!(
        identity(&view)?,
        ledger
            .view(&identity(&view)?)
            .await?
            .verify(&identity(&view)?)
            .map(|_| identity(&view).unwrap())?
    );
    Ok(())
}
