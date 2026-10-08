#![cfg(feature = "publisher")]
mod common;
use anyhow::{ensure, Result};
use hft_research_platform::{
    build::{BuildArtifact, BuildSpec, BuiltExecutable},
    orchestrator::Artifact,
    postgres::{Ledger, BUILD_IMPORT_ADMISSION_MIGRATION, BUILD_RELEASE_MIGRATION, MIGRATION},
};
use std::process::Command;
fn h(c: char) -> String {
    c.to_string().repeat(64)
}
fn artifact_fixture() -> Result<BuildArtifact> {
    let build = BuildSpec {
        schema: 2,
        workspace_manifest: "research-core/Cargo.toml".into(),
        code_commit: "a".repeat(40),
        source_manifest_sha256: h('a'),
        cargo_lock_sha256: h('b'),
        toolchain_manifest_sha256: h('c'),
        target: "x86_64-unknown-linux-gnu".into(),
        packages: vec!["fixture".into()],
        binaries: vec!["fixture".into()],
        features: vec![],
        default_features: false,
        profile: "research".into(),
        profile_manifest_sha256: h('d'),
        rustflags_sha256: h('e'),
        native_environment_sha256: h('f'),
        builder_image: format!("fixture@sha256:{}", h('1')),
    };
    let build_id = build.id()?;
    Ok(BuildArtifact {
        schema: 1,
        build,
        image: format!("fixture@sha256:{}", h('2')),
        release_receipt_sha256: h('3'),
        executables: vec![BuiltExecutable {
            name: "fixture".into(),
            blob: Artifact {
                key: format!("research/builds/{build_id}/fixture"),
                sha256: h('a'),
                bytes: 16,
            },
        }],
    })
}
#[tokio::test]
#[ignore = "requires disposable loopback MONDAY_TEST_DATABASE_URL ending /monday_foundation_build_import_test"]
async fn signed_release_requires_actual_blob_and_only_strong_import_reaches_pg() -> Result<()> {
    let url = std::env::var("MONDAY_TEST_DATABASE_URL")?;
    ensure!(
        url.ends_with("/monday_foundation_build_import_test") && url.contains("@127.0.0.1:"),
        "disposable loopback database required"
    );
    let pool = sqlx_postgres::PgPool::connect(&url).await?;
    for migration in [
        MIGRATION,
        BUILD_RELEASE_MIGRATION,
        BUILD_IMPORT_ADMISSION_MIGRATION,
    ] {
        sqlx_core::raw_sql::raw_sql(migration)
            .execute(&pool)
            .await?;
    }
    let package = common::published::Package::new(artifact_fixture()?).await?;
    let ledger = Ledger::connect(&url).await?;
    let readback = hft_research_platform::release_publisher::read_build_release(
        &package.artifact.build.id()?,
        &h('2'),
        &package.proof_sha,
        &package.trust,
        &package.gateway,
    )
    .await?;
    ensure!(
        readback.artifact() == &package.artifact,
        "positive actual TLS package readback failed"
    );
    std::fs::remove_file(
        package
            .root
            .join(&package.signed.receipt.source.archive.key),
    )?;
    package.trust.verify(&package.artifact, &package.signed)?;
    let artifact_path = package.root.join("artifact-input.json");
    let release_path = package.root.join("release-input.json");
    let trust_path = package.root.join("trust-input.json");
    std::fs::write(&artifact_path, serde_json::to_vec(&package.artifact)?)?;
    std::fs::write(&release_path, serde_json::to_vec(&package.signed)?)?;
    std::fs::write(&trust_path, serde_json::to_vec(&package.trust)?)?;
    let legacy = Command::new(env!("CARGO_BIN_EXE_researchctl"))
        .args([
            "register-build",
            artifact_path.to_str().unwrap(),
            release_path.to_str().unwrap(),
        ])
        .env("MONDAY_RESEARCH_BUILD_TRUST_FILE", &trust_path)
        .env("MONDAY_RESEARCH_DATABASE_URL", &url)
        .output()?;
    ensure!(
        !legacy.status.success(),
        "removed legacy shortcut still accepted a signed fixture"
    );
    ensure!(
        String::from_utf8_lossy(&legacy.stderr).contains("usage: researchctl"),
        "legacy command must be absent"
    );
    ensure!(
        ledger
            .build_artifact(&package.artifact.id()?)
            .await
            .is_err(),
        "missing-source fixture reached PG"
    );
    let obsolete = Command::new(env!("CARGO_BIN_EXE_research-release-publisher"))
        .args([
            "import",
            "build",
            "image",
            "proof",
            "policy",
            "https://localhost",
            "token",
            "admission",
        ])
        .output()?;
    ensure!(
        !obsolete.status.success()
            && String::from_utf8_lossy(&obsolete.stderr)
                .contains("usage: research-release-publisher"),
        "obsolete Gateway CLI bypass is still callable"
    );
    let missing = package
        .import(&ledger)
        .await
        .expect_err("strong importer accepted absent source bytes")
        .to_string();
    ensure!(
        matches!(
            missing.as_str(),
            "release readback rejected"
                | "release readback exceeds bound"
                | "release bytes changed or missing"
        ),
        "failure did not reach actual missing object: {missing}"
    );
    ensure!(
        ledger
            .build_artifact(&package.artifact.id()?)
            .await
            .is_err(),
        "failed readback wrote PG Build"
    );
    std::fs::write(
        package
            .root
            .join(&package.signed.receipt.source.archive.key),
        b"source fixture",
    )?;
    ensure!(
        package.import(&ledger).await.is_err(),
        "unprojected signed approval admitted"
    );
    ledger
        .set_build_import_admission(&package.admission)
        .await?;
    let id = package.import(&ledger).await?;
    ensure!(
        package.import(&ledger).await? == id,
        "actual readback import retry changed identity"
    );
    ensure!(
        ledger.build_artifact(&id).await? == package.artifact,
        "actual package PG projection differs"
    );
    let executable = &package.artifact.executables[0].blob;
    std::fs::write(
        package.root.join(&executable.key),
        b"changed actual program",
    )?;
    ensure!(
        package.import(&ledger).await.is_err(),
        "existing PG row bypassed current program readback"
    );
    ensure!(
        ledger.build_artifact(&id).await? == package.artifact,
        "failed retry changed immutable PG projection"
    );
    println!("same valid signed fixture: legacy CLI rejected; missing source rejected without PG rows; complete actual TLS package imported idempotently");
    Ok(())
}

#[tokio::test]
async fn independent_readback_rejects_missing_tampered_and_replayed_evidence() -> Result<()> {
    let package = common::published::Package::new(artifact_fixture()?).await?;
    // Keep selector strings alive across each asynchronous read.
    let build = package.artifact.build.id()?;
    let image = h('2');
    let verify = || {
        hft_research_platform::release_publisher::read_build_release(
            &build,
            &image,
            &package.proof_sha,
            &package.trust,
            &package.gateway,
        )
    };
    verify().await?;
    let path = package.root.join(&package.artifact.executables[0].blob.key);
    let original = std::fs::read(&path)?;
    std::fs::write(&path, b"tampered")?;
    ensure!(verify().await.is_err(), "tampered program admitted");
    std::fs::write(&path, original)?;
    std::fs::remove_file(
        package
            .root
            .join(&package.signed.receipt.source.archive.key),
    )?;
    ensure!(verify().await.is_err(), "missing source admitted");
    ensure!(
        hft_research_platform::release_publisher::read_build_release(
            &build,
            &image,
            &h('f'),
            &package.trust,
            &package.gateway
        )
        .await
        .is_err(),
        "foreign proof selector replay admitted"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires disposable loopback MONDAY_TEST_DATABASE_URL ending /monday_foundation_build_admission_test"]
async fn independent_pg_admission_orders_revocation_and_rejects_replay_and_sql_bypass() -> Result<()>
{
    use sqlx_core::row::Row;
    let url = std::env::var("MONDAY_TEST_DATABASE_URL")?;
    ensure!(
        url.ends_with("/monday_foundation_build_admission_test") && url.contains("@127.0.0.1:"),
        "disposable loopback database required"
    );
    let pool = sqlx_postgres::PgPool::connect(&url).await?;
    for migration in [MIGRATION, BUILD_RELEASE_MIGRATION] {
        sqlx_core::raw_sql::raw_sql(migration)
            .execute(&pool)
            .await?;
    }
    let package = common::published::Package::new(artifact_fixture()?).await?;
    let ledger = Ledger::connect(&url).await?;
    let published = std::sync::Arc::new(
        hft_research_platform::release_publisher::read_build_release(
            &package.artifact.build.id()?,
            &h('2'),
            &package.proof_sha,
            &package.trust,
            &package.gateway,
        )
        .await?,
    );
    let admission = std::sync::Arc::new(common::published::admission_fixture(
        &package.artifact,
        &package.proof_sha,
        &package.trust,
        false,
        chrono::Utc::now().timestamp_millis() + 60_000,
        1,
    )?);
    ensure!(
        ledger.register_build(&published, &admission).await.is_err(),
        "missing security migration admitted on old schema"
    );
    let artifacts: i64 =
        sqlx_core::query_scalar::query_scalar("SELECT count(*) FROM research.build_artifacts")
            .fetch_one(&pool)
            .await?;
    ensure!(
        artifacts == 0,
        "missing security migration wrote an artifact"
    );
    sqlx_core::raw_sql::raw_sql(BUILD_IMPORT_ADMISSION_MIGRATION)
        .execute(&pool)
        .await?;
    let count = || async {
        let n: i64 =
            sqlx_core::query_scalar::query_scalar("SELECT count(*) FROM research.build_releases")
                .fetch_one(&pool)
                .await?;
        anyhow::Ok(n)
    };
    ensure!(
        ledger.register_build(&published, &admission).await.is_err(),
        "missing PG approval admitted"
    );
    ensure!(count().await? == 0, "missing approval left a release");
    ledger.set_build_import_admission(&admission).await?;
    // A newer independent approval replaces the stable selector row. Old signed
    // files remain cryptographically valid but are no longer active authority.
    let replacement = common::published::admission_fixture(
        &package.artifact,
        &package.proof_sha,
        &package.trust,
        false,
        chrono::Utc::now().timestamp_millis() + 120_000,
        2,
    )?;
    ledger.set_build_import_admission(&replacement).await?;
    ensure!(
        ledger.register_build(&published, &admission).await.is_err(),
        "older signed file replay admitted"
    );
    let revoked = common::published::admission_fixture(
        &package.artifact,
        &package.proof_sha,
        &package.trust,
        true,
        chrono::Utc::now().timestamp_millis() + 120_000,
        3,
    )?;
    ledger.set_build_import_admission(&revoked).await?;
    ensure!(
        ledger.register_build(&published, &admission).await.is_err(),
        "completed revocation admitted first import"
    );
    ensure!(count().await? == 0, "revoked approval left a release");
    ensure!(
        ledger.set_build_import_admission(&admission).await.is_err(),
        "older signed approval reactivated completed revocation"
    );
    ensure!(
        ledger
            .set_build_import_admission(&replacement)
            .await
            .is_err(),
        "superseded signed approval reactivated revocation"
    );
    let unused_older = common::published::admission_fixture(
        &package.artifact,
        &package.proof_sha,
        &package.trust,
        false,
        chrono::Utc::now().timestamp_millis() + 180_000,
        2,
    )?;
    ensure!(
        ledger
            .set_build_import_admission(&unused_older)
            .await
            .is_err(),
        "previously uninstalled older revision reactivated revocation"
    );
    let same_revision_changed = common::published::admission_fixture(
        &package.artifact,
        &package.proof_sha,
        &package.trust,
        false,
        chrono::Utc::now().timestamp_millis() + 120_000,
        3,
    )?;
    ensure!(
        ledger
            .set_build_import_admission(&same_revision_changed)
            .await
            .is_err(),
        "same revision rewrote revoked state"
    );

    ensure!(sqlx_core::query::query("INSERT INTO research.build_import_admissions(build_sha256,image_sha256,publication_proof_sha256,envelope_sha256,revision,expires_ms,revoked,document) VALUES($1,$2,$3,$4,1,123,false,'{}')")
        .bind(h('d')).bind(h('e')).bind(h('f')).bind(h('a')).execute(&pool).await.is_err(), "NULL document selectors bypassed CHECK");
    // Real role separation and direct SQL insert: no native-only gate.
    sqlx_core::raw_sql::raw_sql("CREATE ROLE fixture_build_importer NOLOGIN; GRANT USAGE ON SCHEMA research TO fixture_build_importer; GRANT SELECT ON research.build_import_admissions,research.build_artifacts,research.build_releases TO fixture_build_importer; GRANT INSERT ON research.build_artifacts,research.build_releases TO fixture_build_importer;").execute(&pool).await?;
    let mut restricted = pool.acquire().await?;
    sqlx_core::query::query("SET ROLE fixture_build_importer")
        .execute(&mut *restricted)
        .await?;
    ensure!(
        sqlx_core::query::query("UPDATE research.build_import_admissions SET revoked=false")
            .execute(&mut *restricted)
            .await
            .is_err(),
        "importer rewrote approval"
    );
    let id = package.artifact.id()?;
    sqlx_core::query::query("INSERT INTO research.build_artifacts(artifact_sha256,build_sha256,document) VALUES($1,$2,$3)").bind(&id).bind(package.artifact.build.id()?).bind(serde_json::to_value(&package.artifact)?).execute(&mut *restricted).await?;
    ensure!(sqlx_core::query::query("INSERT INTO research.build_releases(artifact_sha256,receipt_sha256,trust_sha256,document) VALUES($1,$2,$3,$4)").bind(&id).bind(&package.artifact.release_receipt_sha256).bind(h('a')).bind(serde_json::to_value(&package.signed)?).execute(&mut *restricted).await.is_err(), "direct INSERT bypassed revoked approval");
    sqlx_core::query::query("RESET ROLE")
        .execute(&mut *restricted)
        .await?;
    drop(restricted);
    // Advancing constraint timing cannot bypass the mandatory BEFORE gate.
    let mut immediate = pool.begin().await?;
    sqlx_core::query::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *immediate)
        .await?;
    ensure!(sqlx_core::query::query("INSERT INTO research.build_releases(artifact_sha256,receipt_sha256,trust_sha256,document) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(&id).bind(&package.artifact.release_receipt_sha256).bind(h('a')).bind(serde_json::to_value(&package.signed)?)
        .execute(&mut *immediate).await.is_err(), "immediate constraints bypassed revoked gate");
    immediate.rollback().await?;

    // Revocation owns the row first: the native import waits, then rejects.
    let admission = std::sync::Arc::new(common::published::admission_fixture(
        &package.artifact,
        &package.proof_sha,
        &package.trust,
        false,
        chrono::Utc::now().timestamp_millis() + 60_000,
        4,
    )?);
    ledger.set_build_import_admission(&admission).await?;
    let mut revoke_tx = pool.begin().await?;
    sqlx_core::query::query("UPDATE research.build_import_admissions SET revoked=true,revision=5,document=jsonb_set(jsonb_set(document,'{admission,revoked}','true'),'{admission,revision}','5')").execute(&mut *revoke_tx).await?;
    let import_task = {
        let ledger = ledger.clone();
        let published = published.clone();
        let admission = admission.clone();
        tokio::spawn(async move { ledger.register_build(&published, &admission).await })
    };
    wait_for_pg_wait(&pool, "transactionid").await?;
    revoke_tx.commit().await?;
    ensure!(import_task.await?.is_err(), "revoke-first import succeeded");
    ensure!(count().await? == 0, "revoke-first wrote a release");

    // Fixture-only barrier runs AFTER the real BEFORE trigger (alphabetical
    // order). It pauses native registration while its real approval lock lives.
    sqlx_core::raw_sql::raw_sql("CREATE FUNCTION research.fixture_import_barrier() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(13640001); RETURN NEW; END $$; CREATE TRIGGER zzz_fixture_import_barrier BEFORE INSERT ON research.build_releases FOR EACH ROW EXECUTE FUNCTION research.fixture_import_barrier();").execute(&pool).await?;
    let mut barrier = pool.acquire().await?;
    sqlx_core::query::query("SELECT pg_advisory_lock(13640001)")
        .execute(&mut *barrier)
        .await?;
    let expires = chrono::Utc::now().timestamp_millis() + 1_500;
    let expiring = std::sync::Arc::new(common::published::admission_fixture(
        &package.artifact,
        &package.proof_sha,
        &package.trust,
        false,
        expires,
        6,
    )?);
    ledger.set_build_import_admission(&expiring).await?;
    let expiring_task = {
        let ledger = ledger.clone();
        let published = published.clone();
        let expiring = expiring.clone();
        tokio::spawn(async move { ledger.register_build(&published, &expiring).await })
    };
    wait_for_pg_wait(&pool, "advisory").await?;
    while chrono::Utc::now().timestamp_millis() <= expires {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    sqlx_core::query::query("SELECT pg_advisory_unlock(13640001)")
        .execute(&mut *barrier)
        .await?;
    ensure!(
        expiring_task.await?.is_err(),
        "expiry before deferred commit check admitted"
    );
    ensure!(count().await? == 0, "expired transaction wrote a release");

    // Import owns the row first: revocation cannot complete until import commits.
    let admission = std::sync::Arc::new(common::published::admission_fixture(
        &package.artifact,
        &package.proof_sha,
        &package.trust,
        false,
        chrono::Utc::now().timestamp_millis() + 60_000,
        7,
    )?);
    let revoked = common::published::admission_fixture(
        &package.artifact,
        &package.proof_sha,
        &package.trust,
        true,
        chrono::Utc::now().timestamp_millis() + 60_000,
        8,
    )?;
    ledger.set_build_import_admission(&admission).await?;
    sqlx_core::query::query("SELECT pg_advisory_lock(13640001)")
        .execute(&mut *barrier)
        .await?;
    let import_task = {
        let ledger = ledger.clone();
        let published = published.clone();
        let admission = admission.clone();
        tokio::spawn(async move { ledger.register_build(&published, &admission).await })
    };
    wait_for_pg_wait(&pool, "advisory").await?;
    let revoke_task = {
        let ledger = ledger.clone();
        tokio::spawn(async move { ledger.set_build_import_admission(&revoked).await })
    };
    wait_for_pg_wait(&pool, "transactionid").await?;
    ensure!(
        !revoke_task.is_finished(),
        "revocation completed inside admitted import"
    );
    sqlx_core::query::query("SELECT pg_advisory_unlock(13640001)")
        .execute(&mut *barrier)
        .await?;
    ensure!(import_task.await?? == id, "import-first identity changed");
    revoke_task.await??;
    ensure!(
        ledger.register_build(&published, &admission).await.is_err(),
        "old file retry bypassed completed revocation"
    );
    ensure!(
        ledger.build_artifact(&id).await? == package.artifact,
        "revocation deleted historical immutable Build"
    );
    ensure!(count().await? == 1, "import/revoke produced extra Builds");
    let row = sqlx_core::query::query("SELECT revoked FROM research.build_import_admissions")
        .fetch_one(&pool)
        .await?;
    ensure!(row.get::<bool, _>("revoked"), "revocation not persisted");
    let audit_count: i64 = sqlx_core::query_scalar::query_scalar(
        "SELECT count(*) FROM research.build_import_admission_audits",
    )
    .fetch_one(&pool)
    .await?;
    ensure!(audit_count >= 7, "approval/revocation history lost");
    ensure!(
        sqlx_core::query::query("DELETE FROM research.build_import_admission_audits")
            .execute(&pool)
            .await
            .is_err(),
        "admission audit was mutable"
    );
    sqlx_core::raw_sql::raw_sql("DROP TRIGGER zzz_fixture_import_barrier ON research.build_releases; DROP FUNCTION research.fixture_import_barrier();").execute(&pool).await?;
    println!("real PG: missing/replaced/revoked approvals and direct SQL bypass rejected; revoke-first blocks import; import-first blocks revoke; expiry at commit rejected; historical Build retained without Run authority");
    Ok(())
}
async fn wait_for_pg_wait(pool: &sqlx_postgres::PgPool, event: &str) -> Result<()> {
    for _ in 0..150 {
        let waiting: bool=sqlx_core::query_scalar::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event=$1)").bind(event).fetch_one(pool).await?;
        if waiting {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    anyhow::bail!("expected real PG lock wait did not occur: {event}")
}
