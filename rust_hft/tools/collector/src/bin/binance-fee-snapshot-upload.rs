use anyhow::{Context, Result};
use clap::Parser;
use hft_collector::binance_fee_upload::upload_pending;
use hft_collector::binance_usdm_reference_upload::ReferenceUploadConfig;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(name = "binance-fee-snapshot-upload")]
struct Args {
    #[arg(long, default_value = "/data/monday/spool/binance-fee")]
    output_root: PathBuf,
    #[arg(long)]
    bucket: Option<String>,
    #[arg(long)]
    endpoint: Option<String>,
    #[arg(long)]
    region: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    oss_timeout_seconds: Option<u64>,
}

fn env_or(value: Option<String>, name: &str, default: &str) -> String {
    value
        .or_else(|| std::env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn env_u64(value: Option<u64>, name: &str, default: u64) -> Result<u64> {
    match value {
        Some(value) => Ok(value),
        None => std::env::var(name)
            .ok()
            .map(|value| value.trim().parse::<u64>())
            .transpose()
            .with_context(|| format!("invalid {name}"))
            .map(|parsed| parsed.unwrap_or(default)),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let config = ReferenceUploadConfig {
        output_root: args.output_root,
        bucket: env_or(args.bucket, "OSS_BUCKET", "monday-lob-apne1-1045353359"),
        endpoint: env_or(
            args.endpoint,
            "OSS_ENDPOINT",
            "oss-ap-northeast-1-internal.aliyuncs.com",
        ),
        region: env_or(args.region, "OSS_REGION", "ap-northeast-1"),
        profile: env_or(args.profile, "ALIYUN_PROFILE", "ecs-role"),
        oss_timeout: Duration::from_secs(env_u64(
            args.oss_timeout_seconds,
            "OSS_COPY_TIMEOUT_SECONDS",
            300,
        )?),
    };
    println!("{}", serde_json::to_string(&upload_pending(&config)?)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_accepts_bounded_upload_options() {
        let parsed = Args::try_parse_from([
            "binance-fee-snapshot-upload",
            "--output-root",
            "/tmp/fee",
            "--bucket",
            "bucket",
            "--endpoint",
            "endpoint",
            "--region",
            "region",
            "--profile",
            "profile",
            "--oss-timeout-seconds",
            "300",
        ])
        .unwrap();
        assert!(parsed.output_root.is_absolute());
        assert_eq!(parsed.oss_timeout_seconds, Some(300));
    }
}
