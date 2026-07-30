use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use hft_collector::polymarket_evidence_artifact::{
    publish_polymarket_evidence, PolymarketEvidenceArtifactConfig,
    PolymarketProducerQualificationConfig,
};
use hft_collector::polymarket_parity::{verify_shadow_parity, ShadowParityConfig};
use hft_collector::polymarket_raw::{
    finalize_reference_tape, run_reference, ReferenceConfig, DEFAULT_MAX_CONCURRENT_TRADE_POLLS,
    DEFAULT_MAX_MARKETS_PER_LANE, DEFAULT_MAX_TRADE_POLLS_PER_CYCLE,
};
use hft_collector::polymarket_research_import::{
    validate_research_segments, ArtifactTriplet, ResearchSegmentValidationConfig,
};
use hft_collector::polymarket_research_normalize::PolymarketEvidenceConfig;
use hft_collector::polymarket_upload::{run_upload, UploadConfig};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const BUILD_SOURCE_REVISION: &str = match option_env!("MONDAY_SOURCE_REVISION") {
    Some(revision) => revision,
    None => "unbound-source-revision",
};

#[derive(Debug, Parser)]
#[command(
    name = "polymarket-raw-ops",
    version = BUILD_SOURCE_REVISION,
    about = "Fail-closed Polymarket reference collection and raw tape archival"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Collect public market metadata, trades, and settlement evidence.
    CollectReference {
        #[arg(long, default_value = "/data/monday/spool/polymarket-reference")]
        spool_dir: PathBuf,
        #[arg(
            long,
            value_delimiter = ',',
            default_value = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,HYPEUSDT,BNBUSDT"
        )]
        symbols: Vec<String>,
        #[arg(long = "market-id")]
        market_ids: Vec<String>,
        #[arg(long, default_value_t = 30.0)]
        poll_interval_secs: f64,
        #[arg(long, default_value_t = 7_200)]
        market_lookback_secs: i64,
        #[arg(long, default_value_t = 86_400)]
        settlement_lookback_secs: i64,
        #[arg(long, default_value_t = DEFAULT_MAX_MARKETS_PER_LANE)]
        max_markets: usize,
        #[arg(long, default_value_t = DEFAULT_MAX_TRADE_POLLS_PER_CYCLE)]
        max_trade_polls_per_cycle: usize,
        #[arg(long, default_value_t = DEFAULT_MAX_CONCURRENT_TRADE_POLLS)]
        max_concurrent_trade_polls: usize,
        #[arg(long, default_value_t = 20.0)]
        http_timeout: f64,
        #[arg(long, default_value_t = 180.0)]
        stale_after_secs: f64,
        #[arg(long, default_value_t = 1_800)]
        trade_finalization_lag_secs: i64,
        #[arg(long, default_value_t = 3)]
        trade_finalization_stable_polls: u64,
        #[arg(long, default_value_t = 100)]
        per_market_delay_ms: u64,
        #[arg(long)]
        once: bool,
    },
    /// Finalize one stopped active reference tape into a closed segment.
    FinalizeReferenceTape {
        #[arg(long, default_value = "/data/monday/spool/polymarket-reference")]
        spool_dir: PathBuf,
    },
    /// Validate, compress, upload, and read back all closed tape segments.
    Upload {
        #[arg(long, default_value = "/data/monday/spool/polymarket")]
        spool_dir: PathBuf,
        #[arg(long, default_value = "crypto_expiry")]
        dataset: String,
        #[arg(long, default_value_t = 0)]
        quote_depth_levels: usize,
        #[arg(long, default_value_t = 0)]
        quote_sample_ms: u64,
        #[arg(long)]
        bucket: Option<String>,
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long)]
        region: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        zstd_timeout: Option<u64>,
        #[arg(long)]
        oss_timeout: Option<u64>,
    },
    /// Compare a bounded legacy reference lane with an isolated Rust shadow.
    VerifyShadowParity {
        #[arg(long)]
        legacy_spool: PathBuf,
        #[arg(long)]
        rust_spool: PathBuf,
        #[arg(long)]
        started_at_unix: i64,
        #[arg(long)]
        ended_at_unix: i64,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        allow_empty_legacy: bool,
    },
    /// Fail closed unless staged market/reference segments are research-safe.
    ValidateResearchSegments {
        #[arg(long)]
        market_data: PathBuf,
        #[arg(long)]
        market_manifest: PathBuf,
        #[arg(long)]
        market_success: PathBuf,
        #[arg(long)]
        reference_data: Vec<PathBuf>,
        #[arg(long)]
        reference_manifest: Vec<PathBuf>,
        #[arg(long)]
        reference_success: Vec<PathBuf>,
    },
    /// Publish validated five-minute evidence for explicit Polymarket episodes.
    PublishPolymarketEvidence {
        #[arg(long)]
        market_data: PathBuf,
        #[arg(long)]
        market_manifest: PathBuf,
        #[arg(long)]
        market_success: PathBuf,
        #[arg(long)]
        reference_data: Vec<PathBuf>,
        #[arg(long)]
        reference_manifest: Vec<PathBuf>,
        #[arg(long)]
        reference_success: Vec<PathBuf>,
        #[arg(long)]
        event_start_gte: String,
        #[arg(long)]
        event_start_lt: String,
        #[arg(long = "market-id", required = true)]
        market_ids: Vec<String>,
        #[arg(long)]
        output_root: PathBuf,
        /// Producer provenance and event-local request/clock/sequence evidence JSON.
        #[arg(long)]
        qualification: PathBuf,
    },
}

fn positive_duration(value: f64, name: &str) -> Result<Duration> {
    if !value.is_finite() || value <= 0.0 {
        bail!("{name} must be finite and positive");
    }
    Ok(Duration::from_secs_f64(value))
}

fn env_or(value: Option<String>, name: &str, fallback: &str) -> String {
    value.unwrap_or_else(|| env::var(name).unwrap_or_else(|_| fallback.to_owned()))
}

fn env_u64(value: Option<u64>, name: &str, fallback: u64) -> Result<u64> {
    match value {
        Some(value) => Ok(value),
        None => env::var(name)
            .ok()
            .map(|raw| {
                raw.parse::<u64>()
                    .with_context(|| format!("{name} must be an unsigned integer"))
            })
            .transpose()
            .map(|value| value.unwrap_or(fallback)),
    }
}

fn reference_triplets(
    data: Vec<PathBuf>,
    manifests: Vec<PathBuf>,
    successes: Vec<PathBuf>,
) -> Result<Vec<ArtifactTriplet>> {
    if data.is_empty() || data.len() != manifests.len() || data.len() != successes.len() {
        bail!("reference data/manifest/success counts must match and be nonzero");
    }
    Ok(data
        .into_iter()
        .zip(manifests)
        .zip(successes)
        .map(|((data, manifest), success)| ArtifactTriplet {
            data,
            manifest,
            success,
        })
        .collect())
}

#[tokio::main]
async fn main() -> Result<()> {
    run(Cli::parse()).await
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::CollectReference {
            spool_dir,
            symbols,
            market_ids,
            poll_interval_secs,
            market_lookback_secs,
            settlement_lookback_secs,
            max_markets,
            max_trade_polls_per_cycle,
            max_concurrent_trade_polls,
            http_timeout,
            stale_after_secs,
            trade_finalization_lag_secs,
            trade_finalization_stable_polls,
            per_market_delay_ms,
            once,
        } => {
            let config = ReferenceConfig {
                spool_dir,
                symbols,
                market_ids: market_ids.into_iter().collect(),
                poll_interval: positive_duration(poll_interval_secs, "poll interval")?,
                market_lookback_secs,
                settlement_lookback_secs,
                max_markets,
                max_trade_polls_per_cycle,
                max_concurrent_trade_polls,
                http_timeout: positive_duration(http_timeout, "HTTP timeout")?,
                stale_after: positive_duration(stale_after_secs, "stale interval")?,
                trade_finalization_lag_secs,
                trade_finalization_stable_polls,
                per_market_delay: Duration::from_millis(per_market_delay_ms),
            };
            run_reference(config, once).await
        }
        Command::FinalizeReferenceTape { spool_dir } => {
            println!("{}", finalize_reference_tape(&spool_dir)?.display());
            Ok(())
        }
        Command::Upload {
            spool_dir,
            dataset,
            quote_depth_levels,
            quote_sample_ms,
            bucket,
            endpoint,
            region,
            profile,
            zstd_timeout,
            oss_timeout,
        } => {
            let zstd_timeout = env_u64(zstd_timeout, "ZSTD_TIMEOUT_SECONDS", 300)?;
            let oss_timeout = env_u64(oss_timeout, "OSS_COPY_TIMEOUT_SECONDS", 300)?;
            let config = UploadConfig {
                spool_dir,
                dataset,
                quote_depth_levels,
                quote_sample_ms,
                bucket: env_or(bucket, "OSS_BUCKET", "monday-lob-apne1-1045353359"),
                endpoint: env_or(
                    endpoint,
                    "OSS_ENDPOINT",
                    "oss-ap-northeast-1-internal.aliyuncs.com",
                ),
                region: env_or(region, "OSS_REGION", "ap-northeast-1"),
                profile: env_or(profile, "ALIYUN_PROFILE", "ecs-role"),
                zstd_timeout: Duration::from_secs(zstd_timeout),
                oss_timeout: Duration::from_secs(oss_timeout),
            };
            println!("{}", serde_json::to_string(&run_upload(&config)?)?);
            Ok(())
        }
        Command::VerifyShadowParity {
            legacy_spool,
            rust_spool,
            started_at_unix,
            ended_at_unix,
            output,
            allow_empty_legacy,
        } => {
            let config = ShadowParityConfig {
                legacy_spool,
                rust_spool,
                started_at_unix,
                ended_at_unix,
                output,
                allow_empty_legacy,
            };
            if !verify_shadow_parity(&config)? {
                bail!("byte/field/dedupe/settlement/rotation parity failed");
            }
            Ok(())
        }
        Command::ValidateResearchSegments {
            market_data,
            market_manifest,
            market_success,
            reference_data,
            reference_manifest,
            reference_success,
        } => {
            let report = validate_research_segments(&ResearchSegmentValidationConfig {
                market: ArtifactTriplet {
                    data: market_data,
                    manifest: market_manifest,
                    success: market_success,
                },
                references: reference_triplets(
                    reference_data,
                    reference_manifest,
                    reference_success,
                )?,
            })?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        Command::PublishPolymarketEvidence {
            market_data,
            market_manifest,
            market_success,
            reference_data,
            reference_manifest,
            reference_success,
            event_start_gte,
            event_start_lt,
            market_ids,
            output_root,
            qualification,
        } => {
            let qualification: PolymarketProducerQualificationConfig =
                serde_json::from_slice(&fs::read(qualification)?)?;
            let report = publish_polymarket_evidence(&PolymarketEvidenceArtifactConfig {
                evidence: PolymarketEvidenceConfig {
                    segments: ResearchSegmentValidationConfig {
                        market: ArtifactTriplet {
                            data: market_data,
                            manifest: market_manifest,
                            success: market_success,
                        },
                        references: reference_triplets(
                            reference_data,
                            reference_manifest,
                            reference_success,
                        )?,
                    },
                    event_start_gte,
                    event_start_lt,
                    market_ids,
                },
                output_root,
                qualification,
            })?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_version_is_bound_to_the_build_source_revision() {
        let expected = option_env!("MONDAY_SOURCE_REVISION").unwrap_or("unbound-source-revision");
        assert_eq!(Cli::command().get_version(), Some(expected));
    }

    #[test]
    fn collect_reference_cli_uses_the_library_discovery_capacity_default() {
        let command = Cli::try_parse_from(["polymarket-raw-ops", "collect-reference"])
            .expect("default collector CLI must parse")
            .command;
        let Command::CollectReference {
            max_markets,
            max_trade_polls_per_cycle,
            max_concurrent_trade_polls,
            ..
        } = command
        else {
            panic!("collect-reference must select the collector command");
        };
        assert_eq!(max_markets, ReferenceConfig::default().max_markets);
        assert_eq!(
            max_trade_polls_per_cycle,
            ReferenceConfig::default().max_trade_polls_per_cycle
        );
        assert_eq!(
            max_concurrent_trade_polls,
            ReferenceConfig::default().max_concurrent_trade_polls
        );
    }

    #[test]
    fn collect_reference_cli_accepts_explicit_market_id_filter() {
        let command = Cli::try_parse_from([
            "polymarket-raw-ops",
            "collect-reference",
            "--market-id",
            "2959141",
            "--market-id",
            "2959146",
        ])
        .expect("market-id filtered collector CLI must parse")
        .command;
        let Command::CollectReference { market_ids, .. } = command else {
            panic!("collect-reference must select the collector command");
        };
        assert_eq!(
            market_ids,
            vec!["2959141".to_owned(), "2959146".to_owned()]
        );
    }

    #[test]
    fn finalize_reference_tape_cli_accepts_an_explicit_spool() {
        let command = Cli::try_parse_from([
            "polymarket-raw-ops",
            "finalize-reference-tape",
            "--spool-dir",
            "/tmp/polymarket-reference",
        ])
        .expect("finalize reference tape CLI must parse")
        .command;
        let Command::FinalizeReferenceTape { spool_dir } = command else {
            panic!("finalize-reference-tape must select the finalizer command");
        };
        assert_eq!(spool_dir, PathBuf::from("/tmp/polymarket-reference"));
    }

    #[test]
    fn finalize_reference_tape_cli_fails_closed_for_an_empty_spool() {
        let root = tempfile::tempdir().unwrap();
        let spool = fs::canonicalize(root.path()).unwrap();
        fs::write(spool.join("market-updates.ndjson"), b"").unwrap();
        let cli = Cli::try_parse_from([
            "polymarket-raw-ops",
            "finalize-reference-tape",
            "--spool-dir",
            spool.to_str().unwrap(),
        ])
        .unwrap();

        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(run(cli))
            .unwrap_err();

        assert!(error.to_string().contains("active tape"));
    }

    #[test]
    fn publish_polymarket_evidence_cli_requires_explicit_market_ids() {
        let required = [
            "polymarket-raw-ops",
            "publish-polymarket-evidence",
            "--market-data",
            "/tmp/market",
            "--market-manifest",
            "/tmp/market.manifest",
            "--market-success",
            "/tmp/market.success",
            "--reference-data",
            "/tmp/reference",
            "--reference-manifest",
            "/tmp/reference.manifest",
            "--reference-success",
            "/tmp/reference.success",
            "--event-start-gte",
            "2026-07-17T05:30:00Z",
            "--event-start-lt",
            "2026-07-17T05:40:00Z",
            "--output-root",
            "/tmp/output",
            "--qualification",
            "/tmp/qualification.json",
        ];
        assert!(Cli::try_parse_from(required).is_err());

        let command = Cli::try_parse_from([
            "polymarket-raw-ops",
            "publish-polymarket-evidence",
            "--market-data",
            "/tmp/market",
            "--market-manifest",
            "/tmp/market.manifest",
            "--market-success",
            "/tmp/market.success",
            "--reference-data",
            "/tmp/reference",
            "--reference-manifest",
            "/tmp/reference.manifest",
            "--reference-success",
            "/tmp/reference.success",
            "--event-start-gte",
            "2026-07-17T05:30:00Z",
            "--event-start-lt",
            "2026-07-17T05:40:00Z",
            "--market-id",
            "2985854",
            "--output-root",
            "/tmp/output",
            "--qualification",
            "/tmp/qualification.json",
        ])
        .expect("episode-scoped publisher CLI must parse")
        .command;
        let Command::PublishPolymarketEvidence { market_ids, .. } = command else {
            panic!("publish-polymarket-evidence must select the publisher command");
        };
        assert_eq!(market_ids, vec!["2985854".to_owned()]);
    }

    #[test]
    fn reference_triplets_preserve_order_and_reject_mismatched_counts() {
        let paths = |suffix| {
            vec![
                PathBuf::from(format!("/tmp/first.{suffix}")),
                PathBuf::from(format!("/tmp/second.{suffix}")),
            ]
        };
        let triplets = reference_triplets(paths("data"), paths("manifest"), paths("success"))
            .unwrap();
        assert_eq!(triplets[1].data, PathBuf::from("/tmp/second.data"));
        assert!(reference_triplets(paths("data"), vec![], paths("success")).is_err());
    }
}
