use anyhow::{bail, Result};
use clap::Parser;
use hft_collector::binance_usdm_reference_artifact::{
    verify_reference_artifact_read_only, PublishedReferenceArtifact,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

const OUTPUT_SCHEMA: &str = "monday.binance_usdm_reference_artifact_verification.v1";
const DATA_NAME: &str = "reference.ndjson";

#[derive(Debug, Parser)]
#[command(name = "binance-usdm-reference-artifact-verifier")]
struct Args {
    #[arg(long)]
    data_path: PathBuf,
    #[arg(long)]
    data_sha256: String,
    #[arg(long)]
    manifest_sha256: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct VerificationOutput {
    schema: &'static str,
    data_path: PathBuf,
    data_sha256: String,
    manifest_sha256: String,
    metadata_observations: usize,
    mark_index_funding_observations: usize,
    open_interest_observations: usize,
    historical_read_only: bool,
    content_rows_verified: bool,
}

fn artifact_paths(data_path: &Path) -> Result<(PathBuf, PathBuf)> {
    if !data_path.is_absolute()
        || data_path.file_name().and_then(|name| name.to_str()) != Some(DATA_NAME)
    {
        bail!("data path must be an absolute canonical reference.ndjson path");
    }
    let parent = data_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("data path has no batch directory"))?;
    Ok((
        parent.join(format!("{DATA_NAME}.manifest.json")),
        parent.join(format!("{DATA_NAME}._SUCCESS")),
    ))
}

fn verify(args: Args) -> Result<VerificationOutput> {
    let (manifest_path, success_path) = artifact_paths(&args.data_path)?;
    let published = PublishedReferenceArtifact {
        data_path: args.data_path.clone(),
        manifest_path,
        success_path,
        data_sha256: args.data_sha256.clone(),
        manifest_sha256: args.manifest_sha256.clone(),
    };
    let counts = verify_reference_artifact_read_only(
        &published,
        &args.data_sha256,
        &args.manifest_sha256,
    )?;
    Ok(VerificationOutput {
        schema: OUTPUT_SCHEMA,
        data_path: args.data_path,
        data_sha256: args.data_sha256,
        manifest_sha256: args.manifest_sha256,
        metadata_observations: counts.metadata,
        mark_index_funding_observations: counts.mark_index_funding,
        open_interest_observations: counts.open_interest,
        historical_read_only: counts.historical_read_only,
        content_rows_verified: true,
    })
}

fn main() -> Result<()> {
    let output = verify(Args::parse())?;
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::binance_usdm_reference::{
        ActivePerpetualContract, CompleteReferenceBatch, MarkIndexFundingObservation,
        OpenInterestObservation, EXCHANGE_INFO_ENDPOINT, OPEN_INTEREST_ENDPOINT,
        PREMIUM_INDEX_ENDPOINT, REFERENCE_SCHEMA, SERVER_TIME_ENDPOINT,
    };
    use hft_collector::binance_usdm_reference_artifact::{
        publish_reference_batch, ReferenceArtifactConfig,
    };
    use hft_collector::binance_usdm_reference_collector::OFFICIAL_USDM_SOURCE_ORIGIN;
    use rust_decimal::Decimal;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const RECEIVED_NS: u64 = 1_700_000_000_500_000_000;

    fn sample_batch() -> CompleteReferenceBatch {
        CompleteReferenceBatch::new(
            vec![ActivePerpetualContract {
                schema: REFERENCE_SCHEMA.to_owned(),
                symbol: "BTCUSDT".to_owned(),
                pair: "BTCUSDT".to_owned(),
                base_asset: "BTC".to_owned(),
                quote_asset: "USDT".to_owned(),
                margin_asset: "USDT".to_owned(),
                tick_size: Decimal::new(1, 1),
                step_size: Decimal::new(1, 3),
                min_notional: Decimal::new(5, 0),
                contract_type: "PERPETUAL".to_owned(),
                status: "TRADING".to_owned(),
                onboard_date_ms: 1,
                delivery_date_ms: 4_133_404_800_000,
                source_time_ms: SOURCE_MS,
                source_clock_received_at_ns: RECEIVED_NS - 100,
                received_at_ns: RECEIVED_NS - 50,
                source_endpoint: EXCHANGE_INFO_ENDPOINT.to_owned(),
                source_clock_endpoint: SERVER_TIME_ENDPOINT.to_owned(),
            }],
            vec![MarkIndexFundingObservation {
                schema: REFERENCE_SCHEMA.to_owned(),
                symbol: "BTCUSDT".to_owned(),
                mark_price: Decimal::new(101, 0),
                index_price: Decimal::new(100, 0),
                basis: Decimal::ONE,
                basis_rate: Decimal::new(1, 2),
                last_funding_rate: Decimal::new(1, 4),
                interest_rate: Decimal::new(1, 4),
                next_funding_time_ms: SOURCE_MS + 28_800_000,
                source_time_ms: SOURCE_MS,
                received_at_ns: RECEIVED_NS,
                source_endpoint: PREMIUM_INDEX_ENDPOINT.to_owned(),
            }],
            vec![OpenInterestObservation {
                schema: REFERENCE_SCHEMA.to_owned(),
                symbol: "BTCUSDT".to_owned(),
                open_interest: Decimal::new(12345, 3),
                source_time_ms: SOURCE_MS,
                received_at_ns: RECEIVED_NS + 50,
                source_endpoint: OPEN_INTEREST_ENDPOINT.to_owned(),
            }],
        )
        .unwrap()
    }

    fn fixture() -> (tempfile::TempDir, PublishedReferenceArtifact) {
        let temp = tempdir().unwrap();
        let output_root = fs::canonicalize(temp.path()).unwrap();
        let published = publish_reference_batch(
            &ReferenceArtifactConfig {
                output_root,
                observed_at_ns: RECEIVED_NS + 100,
                max_staleness_ms: 1_000,
            },
            OFFICIAL_USDM_SOURCE_ORIGIN,
            &sample_batch(),
        )
        .unwrap();
        (temp, published)
    }

    fn args(published: &PublishedReferenceArtifact) -> Args {
        Args {
            data_path: published.data_path.clone(),
            data_sha256: published.data_sha256.clone(),
            manifest_sha256: published.manifest_sha256.clone(),
        }
    }

    fn rewrite_as_historical_v2(published: &mut PublishedReferenceArtifact) {
        let mut data = Vec::new();
        for line in fs::read(&published.data_path).unwrap().split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let mut row: serde_json::Value = serde_json::from_slice(line).unwrap();
            row["observation"]["schema"] = serde_json::json!("binance.usdm_reference.v2");
            if row["kind"] == "metadata" {
                let observation = row["observation"].as_object_mut().unwrap();
                observation.remove("tick_size");
                observation.remove("step_size");
                observation.remove("min_notional");
            }
            serde_json::to_writer(&mut data, &row).unwrap();
            data.push(b'\n');
        }
        fs::write(&published.data_path, &data).unwrap();
        published.data_sha256 = hex::encode(Sha256::digest(&data));
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&published.manifest_path).unwrap()).unwrap();
        manifest["data_schema"] = serde_json::json!("binance.usdm_reference.v2");
        manifest["bytes"] = serde_json::json!(data.len());
        manifest["sha256"] = serde_json::json!(published.data_sha256);
        let mut manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        manifest_bytes.push(b'\n');
        fs::write(&published.manifest_path, &manifest_bytes).unwrap();
        published.manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
        fs::write(
            &published.success_path,
            format!("{}\n", published.data_sha256),
        )
        .unwrap();
    }

    #[test]
    fn verifies_canonical_triplet_and_reports_content_counts() {
        let (_temp, published) = fixture();
        let output = verify(args(&published)).unwrap();
        assert_eq!(output.schema, OUTPUT_SCHEMA);
        assert_eq!(output.metadata_observations, 1);
        assert_eq!(output.mark_index_funding_observations, 1);
        assert_eq!(output.open_interest_observations, 1);
        assert!(output.content_rows_verified);
        assert!(!output.historical_read_only);
    }

    #[test]
    fn historical_v2_triplet_remains_decodable_but_is_marked_read_only() {
        let (_temp, mut published) = fixture();
        rewrite_as_historical_v2(&mut published);

        let output = verify(args(&published)).unwrap();

        assert_eq!(output.metadata_observations, 1);
        assert_eq!(output.mark_index_funding_observations, 1);
        assert_eq!(output.open_interest_observations, 1);
        assert!(output.historical_read_only);
        assert!(output.content_rows_verified);
    }

    #[test]
    fn rejects_tampered_data_even_when_the_caller_supplies_the_old_anchor() {
        let (_temp, published) = fixture();
        fs::write(&published.data_path, b"{\"tampered\":true}\n").unwrap();
        assert!(verify(args(&published)).is_err());
    }

    #[test]
    fn rejects_symlinked_manifest() {
        let (_temp, published) = fixture();
        let real = published.manifest_path.with_extension("json.real");
        fs::rename(&published.manifest_path, &real).unwrap();
        symlink(&real, &published.manifest_path).unwrap();
        assert!(verify(args(&published)).is_err());
    }

    #[test]
    fn rejects_noncanonical_or_wrongly_anchored_inputs() {
        let (_temp, published) = fixture();
        let mut relative = args(&published);
        relative.data_path = PathBuf::from("reference.ndjson");
        assert!(verify(relative).is_err());

        let mut wrong = args(&published);
        wrong.data_sha256 = "0".repeat(64);
        assert!(verify(wrong).is_err());
    }
}
