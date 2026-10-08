#![cfg(feature = "publisher")]
mod common;
use anyhow::{ensure, Result};
use hft_research_platform::{
    build::{BuildArtifact, BuildSpec, BuiltExecutable},
    orchestrator::Artifact,
    postgres::{Ledger, BUILD_RELEASE_MIGRATION, MIGRATION},
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
    for migration in [MIGRATION, BUILD_RELEASE_MIGRATION] {
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
