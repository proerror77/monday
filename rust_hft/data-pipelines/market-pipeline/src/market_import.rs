//! One normalized source batch. Schema installation and data-writer activation
//! are explicit operator actions; neither normalization nor connection does them.
use anyhow::{ensure, Context, Result};
#[cfg(feature = "import")]
use data::market_columnar::SealedParquet;
use data::{
    binance_market_tape_artifact::{
        seal_binance_market_tape_triplet,
        verify_binance_market_tape_series_with_required_lob_continuity, BinanceMarketTapeTriplet,
        BinanceMarketTapeTrustAnchor,
    },
    market_columnar::{write_parquet, Batch, Manifest},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::PathBuf,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInput {
    pub raw: PathBuf,
    pub manifest: PathBuf,
    pub success: PathBuf,
    pub content_sha256: String,
    pub manifest_sha256: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizeConfig {
    pub instrument: String,
    pub sources: Vec<SourceInput>,
    pub output_directory: PathBuf,
}
#[derive(Debug, Serialize)]
pub struct NormalizedOutput {
    pub manifest: Manifest,
    pub manifest_sha256: String,
    pub parquet_path: PathBuf,
    pub manifest_path: PathBuf,
}

pub fn normalize(config: &NormalizeConfig) -> Result<NormalizedOutput> {
    ensure!(
        !config.sources.is_empty() && config.sources.len() <= 64,
        "source count outside pilot bound"
    );
    let sealed = config
        .sources
        .iter()
        .map(|s| {
            seal_binance_market_tape_triplet(
                &BinanceMarketTapeTriplet {
                    data: s.raw.clone(),
                    manifest: s.manifest.clone(),
                    success: s.success.clone(),
                },
                &BinanceMarketTapeTrustAnchor::from_lower_hex(
                    &s.content_sha256,
                    &s.manifest_sha256,
                )?,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let verified = verify_binance_market_tape_series_with_required_lob_continuity(sealed)?;
    let batch = Batch::from_verified(&verified, &config.instrument)?;
    std::fs::create_dir_all(&config.output_directory)?;
    let directory = std::fs::canonicalize(&config.output_directory)?;
    let id = batch.spec.id()?;
    let path = directory.join(format!("{id}.parquet"));
    let manifest_path = directory.join(format!("{id}.manifest.json"));
    if manifest_path.exists() {
        let manifest: Manifest = read_json(&manifest_path)?;
        ensure!(
            manifest.spec == batch.spec && manifest.logical_sha256 == batch.logical_sha256()?,
            "existing batch conflicts with verified raw"
        );
        manifest.verify_file(&path)?;
        return Ok(NormalizedOutput {
            manifest_sha256: manifest.id()?,
            manifest,
            parquet_path: path,
            manifest_path,
        });
    }
    // A task-owned attempt path prevents an interrupted final file from being
    // mistaken for a sealed artifact. Final publication never overwrites bytes.
    let temporary = directory.join(format!(".{id}.{}.partial", std::process::id()));
    let manifest = write_parquet(&batch, &temporary)?;
    if let Err(error) = std::fs::hard_link(&temporary, &path) {
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
        manifest
            .verify_file(&path)
            .context("partial/conflicting final Parquet blocks publication")?;
    }
    FileSync::directory(&directory)?;
    std::fs::remove_file(&temporary)?;
    let bytes = serde_json::to_vec(&manifest)?;
    let manifest_temporary =
        directory.join(format!(".{id}.{}.manifest.partial", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manifest_temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if let Err(error) = std::fs::hard_link(&manifest_temporary, &manifest_path) {
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
        let existing: Manifest = read_json(&manifest_path)?;
        ensure!(
            existing.id()? == manifest.id()?,
            "conflicting final manifest blocks publication"
        );
    }
    FileSync::directory(&directory)?;
    std::fs::remove_file(&manifest_temporary)?;
    FileSync::directory(&directory)?;
    Ok(NormalizedOutput {
        manifest_sha256: manifest.id()?,
        manifest,
        parquet_path: path,
        manifest_path,
    })
}
struct FileSync;
impl FileSync {
    fn directory(path: &std::path::Path) -> Result<()> {
        std::fs::File::open(path)?.sync_all()?;
        Ok(())
    }
}
pub fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<T> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1024 * 1024, "JSON metadata outside bound");
    Ok(serde_json::from_slice(&bytes)?)
}
#[cfg(feature = "import")]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportConfig {
    pub manifest_path: PathBuf,
    pub manifest_sha256: String,
    pub parquet_path: PathBuf,
    pub database_url_file: String,
    pub clickhouse_endpoint: String,
    pub clickhouse_user_file: String,
    pub clickhouse_password_file: String,
    pub clickhouse_instance_receipt_sha256: String,
}
#[cfg(feature = "import")]
pub async fn import(config: &ImportConfig) -> Result<crate::market_postgres::Receipt> {
    let manifest: Manifest = read_json(&config.manifest_path)?;
    let sealed = SealedParquet::open(manifest, &config.manifest_sha256, &config.parquet_path)?;
    let ledger = crate::market_postgres::DataLedger::connect(&crate::read_secret(
        &config.database_url_file,
    )?)
    .await?;
    let ch = crate::market_clickhouse::DataClickHouse::new(
        &config.clickhouse_endpoint,
        crate::read_secret(&config.clickhouse_user_file)?,
        crate::read_secret(&config.clickhouse_password_file)?,
        config.clickhouse_instance_receipt_sha256.clone(),
    )?;
    import_sealed(&ledger, &ch, &sealed).await
}
#[cfg(feature = "import")]
pub async fn import_sealed(
    ledger: &crate::market_postgres::DataLedger,
    ch: &crate::market_clickhouse::DataClickHouse,
    source: &SealedParquet,
) -> Result<crate::market_postgres::Receipt> {
    use crate::market_postgres::Claim;
    match ledger.claim(source.manifest()).await? {
        Claim::Published(receipt) => {
            ensure!(
                receipt.clickhouse_instance_receipt_sha256 == ch.instance_sha256(),
                "published batch belongs to another CH service identity"
            );
            let proof = ch
                .verify_target(source, &receipt.generation)
                .await?
                .context(
                    "PG published receipt exists but CH target is missing; joint recovery required",
                )?;
            proof.matches(source.manifest())?;
            Ok(receipt)
        }
        Claim::Permit(mut permit) => {
            let result = async {
                let proof =
                    if let Some(proof) = ch.verify_target(source, permit.generation()).await? {
                        permit.event("ch_receipt_recovered").await?;
                        proof
                    } else {
                        let stage = ch.stage(&mut permit, source).await?;
                        ch.publish_staged(&mut permit, source, stage).await?
                    };
                permit.publish(proof).await
            }
            .await;
            if result.is_err() {
                let _ = permit.event("failed").await;
            }
            let release = (*permit).release().await;
            match result {
                Ok(receipt) => {
                    release?;
                    Ok(receipt)
                }
                Err(error) => Err(error),
            }
        }
    }
}
