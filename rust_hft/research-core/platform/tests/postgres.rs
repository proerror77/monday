#![cfg(feature = "control")]
use hft_research_platform::{
    data::{BlockRef, DataViewSpec, Exit, PublishedView, Split, Window},
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
    let mut plan = hft_research_platform::data::PreparationPlan {
        spec: view.spec.clone(),
        source_receipt_sha256: view.source_receipt_sha256.clone(),
        producer_image: view.producer_image.clone(),
        recipe_sql: None,
    };
    plan.spec.feature_sql_sha256 =
        hft_research_platform::sha256(hft_research_platform::data::PREPARE_SQL.as_bytes());
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
    .bind(serde_json::to_value(acceptance)?)
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
    let admit = |spec: hft_research_platform::orchestrator::TaskSpec| {
        let pool = &pool;
        async move {
            let a = hft_research_platform::orchestrator::Admission {
                schema: 1,
                request_sha256: spec.id()?,
                task_spec: spec.clone(),
                resource_reservation_receipt_sha256: hash('a'),
                scientific_grant_receipt_sha256: hash('b'),
                release_admission_receipt_sha256: hash('c'),
                max_attempts: spec.max_attempts,
            };
            sqlx_core::query::query(
                "INSERT INTO research.admissions(request_sha256,document) VALUES($1,$2)",
            )
            .bind(&a.request_sha256)
            .bind(serde_json::to_value(&a)?)
            .execute(pool)
            .await?;
            Ok::<_, anyhow::Error>(())
        }
    };
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
        provider_version: "0.159.2".into(),
        provider_thread_id: native_session.thread_id().unwrap().into(),
        provider_binary_sha256: session_config.executable_sha256,
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
        hft_research_platform::postgres::completion_message(&pending[0].1)?.contains("cancelled")
    );
    assert!(ledger
        .pending_completions("another", &session_id)
        .await
        .is_err());
    let message = hft_research_platform::postgres::completion_message(&pending[0].1)?;
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
    native_session.close().await?;

    sqlx_core::query::query("UPDATE research.authority SET mode='postgres'")
        .execute(&pool)
        .await?;
    let mut result = ledger.lock_next("result-owner", 30000).await?.unwrap();
    let lease = result.task.lease.clone().unwrap();
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
    sqlx_core::query::query(
        "INSERT INTO research.revocations(request_sha256,reason_receipt_sha256) VALUES($1,$2)",
    )
    .bind(&result_id)
    .bind(hash('f'))
    .execute(&pool)
    .await?;
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
    cancelled.commit("fixture_revoked_cancelled").await?;
    assert_eq!(ledger.read(&result_id).await?.state, State::Cancelled);
    assert!(ledger.read(&result_id).await?.receipt.is_none());

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
