#![cfg(feature = "import")]
use anyhow::{ensure, Context, Result};
use data::market_columnar::{Manifest, SealedParquet};
use hft_market_pipeline::{
    market_clickhouse::{DataClickHouse, MIGRATION as CH_MIGRATION},
    market_import::{import_sealed, read_json},
    market_postgres::{Claim, DataLedger, MIGRATION as PG_MIGRATION},
};
use sqlx_core::{query::query, query_scalar::query_scalar};

async fn ch_sql(sql: &str) -> Result<String> {
    ensure!(
        std::env::var("MONDAY_TEST_CH_ENDPOINT")? == "http://127.0.0.1:18123",
        "not disposable CH fixture"
    );
    let response = reqwest::Client::new()
        .post("http://127.0.0.1:18123/")
        .basic_auth("fixture", Some(std::env::var("MONDAY_TEST_CH_PASSWORD")?))
        .body(sql.to_owned())
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    ensure!(
        status.is_success(),
        "synthetic fixture SQL rejected HTTP {}: {}",
        status,
        body
    );
    Ok(body)
}
async fn count(sql: &str) -> Result<u64> {
    Ok(ch_sql(sql).await?.trim().parse()?)
}

/// Full raw→Parquet fixture is produced by hft-data's real protocol/columnar
/// test. The service test never reads business archives or a remote DB.
#[tokio::test]
#[ignore = "requires exact disposable PG/CH services and the tiny raw protocol fixture"]
async fn real_parquet_import_is_idempotent_fenced_and_recovers_ch_before_pg() -> Result<()> {
    let url = std::env::var("MONDAY_TEST_DATABASE_URL")?;
    let parsed = reqwest::Url::parse(&url)?;
    ensure!(
        parsed.scheme() == "postgres"
            && parsed.username() == "fixture"
            && parsed.host_str() == Some("127.0.0.1")
            && parsed.port() == Some(5432)
            && parsed.path() == "/monday_foundation_test",
        "not disposable PG fixture"
    );
    let root = std::path::PathBuf::from(std::env::var("MONDAY_TEST_MARKET_COLUMNAR_DIR")?);
    ensure!(
        root.file_name().context("fixture path missing")? == "monday_market_columnar_test",
        "fixture identity mismatch"
    );
    let manifest: Manifest = read_json(&root.join("batch.manifest.json"))?;
    let sealed = SealedParquet::open(
        manifest.clone(),
        &manifest.id()?,
        &root.join("batch.parquet"),
    )?;
    assert_eq!(sealed.rows().len(), 3);
    let pool = sqlx_postgres::PgPool::connect(&url).await?;
    sqlx_core::raw_sql::raw_sql(PG_MIGRATION)
        .execute(&pool)
        .await?;
    let ledger = DataLedger::connect(&url).await?;
    assert!(ledger.claim(&manifest).await.is_err()); // installation stays paused
    let sql = CH_MIGRATION
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    for statement in sql.split(';').filter(|s| !s.trim().is_empty()) {
        ch_sql(statement).await?;
    }
    query("UPDATE market_data.authority SET mode='import',writer_activation_receipt_sha256=$1,legacy_writer_quiescence_sha256=$2").bind("a".repeat(64)).bind("b".repeat(64)).execute(&pool).await?;
    let ch = DataClickHouse::disposable_fixture(
        &std::env::var("MONDAY_TEST_CH_ENDPOINT")?,
        std::env::var("MONDAY_TEST_CH_PASSWORD")?,
    )?;
    let Claim::Permit(mut partial) = ledger.claim(&manifest).await? else {
        anyhow::bail!("unexpected preexisting fixture publication")
    };
    assert!(ledger.claim(&manifest).await.is_err()); // concurrent scope owner rejected
    let stage = ch.stage(&mut partial, &sealed).await?;
    let partial_table = format!("market_data.stage_{}", partial.generation());
    ch_sql(&format!(
        "ALTER TABLE {partial_table} DELETE WHERE ordinal>1 SETTINGS mutations_sync=2"
    ))
    .await?;
    assert!(ch
        .publish_staged(&mut partial, &sealed, stage)
        .await
        .is_err());
    assert!(ch
        .verify_target(&sealed, partial.generation())
        .await?
        .is_none());
    assert!(ledger.receipt(&manifest.batch_id).await.is_err());
    let watermarks: i64 = query_scalar("SELECT count(*) FROM market_data.watermarks")
        .fetch_one(&pool)
        .await?;
    assert_eq!(watermarks, 0);
    partial.event("failed").await?;
    // Simulate a superseding durable fence. A still-live old permit stops before
    // any further CH/PG publication, even if its session lock remains held.
    query("INSERT INTO market_data.attempts(generation,batch_id) VALUES($1,$2)")
        .bind("e".repeat(64))
        .bind(&manifest.batch_id)
        .execute(&pool)
        .await?;
    assert!(partial.check().await.is_err());
    (*partial).release().await?;

    let Claim::Permit(mut before_pg) = ledger.claim(&manifest).await? else {
        anyhow::bail!("unexpected fixture receipt")
    };
    let old_generation = before_pg.generation().to_owned();
    let stage = ch.stage(&mut before_pg, &sealed).await?;
    let proof = ch.publish_staged(&mut before_pg, &sealed, stage).await?;
    drop(proof); // inject process loss after atomic CH publish, before PG commit
    assert!(ledger.receipt(&manifest.batch_id).await.is_err());
    assert_eq!(count("SELECT count() FROM market_data.events_v1").await?, 3);
    let stages_before=count("SELECT count() FROM system.tables WHERE database='market_data' AND startsWith(name,'stage_')").await?;
    (*before_pg).release().await?;
    let receipt = import_sealed(&ledger, &ch, &sealed).await?;
    assert_ne!(receipt.generation, old_generation);
    assert_eq!(receipt.rows, 3);
    assert_eq!(count("SELECT count() FROM market_data.events_v1").await?, 3);
    assert_eq!(count("SELECT count() FROM system.tables WHERE database='market_data' AND startsWith(name,'stage_')").await?,stages_before); // recovery did not INSERT/create a stage
    let reopened = DataLedger::connect(&url).await?;
    assert_eq!(import_sealed(&reopened, &ch, &sealed).await?, receipt);
    assert_eq!(reopened.receipt(&manifest.batch_id).await?, receipt);
    assert_eq!(count("SELECT count() FROM market_data.events_v1").await?, 3);
    let watermark: i64 =
        query_scalar("SELECT source_end_ns FROM market_data.watermarks WHERE scope_sha256=$1")
            .bind(manifest.spec.scope()?)
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        watermark as u64,
        manifest.spec.sources.last().unwrap().end_received_ns
    );
    assert!(
        query("UPDATE market_data.watermarks SET source_end_ns=source_end_ns-1")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(query("DELETE FROM market_data.publications")
        .execute(&pool)
        .await
        .is_err());
    assert!(query("UPDATE market_data.batches SET manifest='{}'")
        .execute(&pool)
        .await
        .is_err());
    let mut overlap = manifest.clone();
    overlap.spec.sources[0].content_sha256 = "c".repeat(64);
    overlap.batch_id = overlap.spec.id()?;
    assert!(ledger.claim(&overlap).await.is_err());
    let mut gap = overlap.clone();
    let end = manifest.spec.sources.last().unwrap().end_received_ns;
    gap.spec.sources[0].start_received_ns = end + 1;
    gap.spec.sources[0].end_received_ns = end + 1000;
    gap.batch_id = gap.spec.id()?;
    assert!(ledger.claim(&gap).await.is_err());
    assert_eq!(count("SELECT count() FROM market_data.events_v1").await?, 3);
    println!("PASS: genuine Decimal/LIST Parquet→CH columns→PG receipt; partial stage rejected; old fence rejected; CH-before-PG recovered without another INSERT; restart/duplicate preserved 3 rows and receipt; gap/overlap/watermark regression and evidence rewrite rejected");
    Ok(())
}
