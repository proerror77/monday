#![cfg(feature = "control")]
use hft_research_platform::{
    data::{BlockRef, DataViewSpec, Exit, PublishedView, Split, Window},
    execution::{Acceptance, Backend, Profile},
    identity,
    orchestrator::{State, TaskKind, TaskSpec},
    postgres::{Ledger, MIGRATION},
};

fn hash(c: char) -> String {
    c.to_string().repeat(64)
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
    assert!(ledger.publish_view(&view).await.is_err());
    // Test-only activation fixture. No application path performs this UPDATE.
    sqlx_core::query::query("UPDATE research.authority SET mode='postgres',legacy_quiescence_sha256=$1,migration_receipt_sha256=$2").bind(hash('a')).bind(hash('b')).execute(&pool).await?;
    let view_sha = ledger.publish_view(&view).await?;
    assert_eq!(view_sha, ledger.publish_view(&view).await?);
    let mut conflicting = view.clone();
    conflicting.blocks[0].sha256 = hash('e');
    assert!(ledger.publish_view(&conflicting).await.is_err());
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
    let artifact_id = ledger.register_build(&artifact).await?;
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
    let session = hft_research_platform::research::Session {
        schema: 1,
        experiment_sha256: run.experiment_sha256.clone(),
        provider: hft_research_platform::research::CodingAgent::CodexAppServer,
        provider_version: "0.159.2".into(),
        provider_thread_id: "fixture-thread".into(),
        provider_binary_sha256: hash('a'),
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
    assert_eq!(build_count, 1);
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
