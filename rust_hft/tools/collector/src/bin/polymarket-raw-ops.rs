use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use hft_collector::polymarket_parity::{verify_shadow_parity, ShadowParityConfig};
use hft_collector::polymarket_raw::{run_reference, ReferenceConfig, DEFAULT_MAX_MARKETS_PER_LANE};
use hft_collector::polymarket_upload::{run_upload, UploadConfig};
use std::env;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(
    name = "polymarket-raw-ops",
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
        #[arg(long, default_value_t = 30.0)]
        poll_interval_secs: f64,
        #[arg(long, default_value_t = 7_200)]
        market_lookback_secs: i64,
        #[arg(long, default_value_t = 86_400)]
        settlement_lookback_secs: i64,
        #[arg(long, default_value_t = DEFAULT_MAX_MARKETS_PER_LANE)]
        max_markets: usize,
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
    /// Compare a bounded Python reference lane with an isolated Rust shadow.
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

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::CollectReference {
            spool_dir,
            symbols,
            poll_interval_secs,
            market_lookback_secs,
            settlement_lookback_secs,
            max_markets,
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
                poll_interval: positive_duration(poll_interval_secs, "poll interval")?,
                market_lookback_secs,
                settlement_lookback_secs,
                max_markets,
                http_timeout: positive_duration(http_timeout, "HTTP timeout")?,
                stale_after: positive_duration(stale_after_secs, "stale interval")?,
                trade_finalization_lag_secs,
                trade_finalization_stable_polls,
                per_market_delay: Duration::from_millis(per_market_delay_ms),
            };
            run_reference(config, once).await
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
        } => {
            let config = ShadowParityConfig {
                legacy_spool,
                rust_spool,
                started_at_unix,
                ended_at_unix,
                output,
            };
            if !verify_shadow_parity(&config)? {
                bail!("byte/field/dedupe/settlement/rotation parity failed");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_reference_cli_uses_the_library_discovery_capacity_default() {
        let command = Cli::try_parse_from(["polymarket-raw-ops", "collect-reference"])
            .expect("default collector CLI must parse")
            .command;
        let Command::CollectReference { max_markets, .. } = command else {
            panic!("collect-reference must select the collector command");
        };
        assert_eq!(max_markets, ReferenceConfig::default().max_markets);
    }
}
