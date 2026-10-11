use anyhow::{bail, Context};
use clap::{Args, Parser, Subcommand, ValueEnum};
use hft_cex_research_input::preparation::PreparationArgs;
use std::path::PathBuf;
#[cfg(test)]
pub const BUILD_SOURCE_REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[cfg(not(test))]
pub const BUILD_SOURCE_REVISION: &str = match option_env!("MONDAY_SOURCE_REVISION") {
    Some(value) => value,
    None => "unbound-source-revision",
};
#[derive(Debug, Parser)]
#[command(name="monday-cex-prepare", version=BUILD_SOURCE_REVISION, about="Bounded CEX data preparation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, Subcommand)]
enum Command {
    Sources,
    Acquire(AcquireDataArgs),
    FreezeInventory(FreezeInventoryArgs),
    VerifyMaterializationManifest(VerifyMaterializationManifestArgs),
    PrepareFreshInputs(Box<PreparationArgs>),
}
pub fn print_json(value: &impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
pub async fn run() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Sources => print_json(&hft_collector::source_catalog()),
        Command::Acquire(args) => {
            let mission = hft_collector::DataAcquisitionMission {
                mission_id: args.mission_id,
                source_id: args.source_id,
                symbol: args.symbol,
                interval: args.interval,
                limit: args.limit,
                artifact_dir: args.artifact_dir,
                quality_requirements: hft_collector::QualityRequirements {
                    max_parse_failures: args.max_parse_failures,
                    max_non_monotonic_events: args.max_non_monotonic_events,
                    max_non_finite_values: args.max_non_finite_values,
                },
            };
            let manifest = hft_collector::acquire_dataset(&mission)
                .await
                .map_err(anyhow::Error::msg)?;
            let output = args.manifest_out.unwrap_or_else(|| {
                manifest
                    .artifact_path
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .join(format!("{}.manifest.json", manifest.manifest_id))
            });
            hft_research_artifacts::write_json_atomic(&output, &manifest)?;
            print_json(&serde_json::json!({"manifest":manifest,"manifest_path":output}))
        }
        Command::FreezeInventory(args) => {
            tokio::task::spawn_blocking(move || crate::inventory::freeze_research_inventory(args))
                .await
                .context("inventory freezer worker failed")?
        }
        Command::VerifyMaterializationManifest(args) => tokio::task::spawn_blocking(move || {
            crate::inventory::verify_materialization_manifest(args)
        })
        .await
        .context("materialization manifest verifier worker failed")?,
        Command::PrepareFreshInputs(mut args) => {
            require_cloud_data_host(std::env::consts::OS)?;
            args.source_revision = BUILD_SOURCE_REVISION.into();
            tokio::task::spawn_blocking(move || crate::prepare::prepare(*args))
                .await
                .context("fresh Campaign input preparation worker failed")?
        }
    }
}
fn require_cloud_data_host(os: &str) -> anyhow::Result<()> {
    if os != "linux" {
        bail!("CEX raw inputs and preparation belong in the admitted ACK data Job");
    }
    Ok(())
}
#[derive(Debug, Clone, Args)]
pub struct VerifyMaterializationManifestArgs {
    #[arg(long)]
    pub manifest: PathBuf,
    /// SHA-256 from the frozen inventory, checked against the parsed bytes.
    #[arg(long)]
    pub manifest_sha256: String,
    #[arg(long, value_enum)]
    pub kind: MaterializationManifestKindArg,
    #[arg(long)]
    pub market: String,
    #[arg(long, requires = "end_received_at_ns")]
    pub start_received_at_ns: Option<u64>,
    #[arg(long, requires = "start_received_at_ns")]
    pub end_received_at_ns: Option<u64>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MaterializationManifestKindArg {
    Raw,
    Reference,
}

#[derive(Debug, Clone, Args)]
pub struct FreezeInventoryArgs {
    /// Read-only root of sealed raw collector triplets.
    #[arg(long)]
    pub raw_root: PathBuf,
    /// Read-only root of published Binance Spot or USD-M reference triplets.
    #[arg(long)]
    pub reference_root: PathBuf,
    /// Binance market whose raw and reference identities are frozen together.
    #[arg(long)]
    pub market: String,
    #[arg(long)]
    pub start_received_at_ns: u64,
    #[arg(long)]
    pub end_received_at_ns: u64,
    #[arg(long)]
    pub symbol: String,
    /// Paired research runner digest; source revision comes from this binary.
    #[arg(long)]
    pub image_ref: String,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long)]
    pub output_prefix: String,
    #[arg(long)]
    pub bucket_ms: u64,
    #[arg(long)]
    pub label_horizon_buckets: u64,
    #[arg(long)]
    pub top_depth: usize,
    #[arg(long, default_value_t = 100_000)]
    pub max_scan_entries: usize,
    #[arg(long, default_value_t = 8192)]
    pub max_inputs: usize,
    /// Explicit maximum total source bytes to verify; no source payload is copied.
    #[arg(long)]
    pub max_input_bytes: u64,
    /// New private frozen.env file; an existing inventory is never overwritten.
    #[arg(long)]
    pub output: PathBuf,
}

#[derive(Debug, Args)]
struct AcquireDataArgs {
    #[arg(long)]
    mission_id: String,
    #[arg(long, default_value = "binance-public")]
    source_id: String,
    #[arg(long)]
    symbol: String,
    #[arg(long, default_value = "1m")]
    interval: String,
    #[arg(long, default_value_t = 500)]
    limit: usize,
    #[arg(long)]
    artifact_dir: PathBuf,
    #[arg(long)]
    manifest_out: Option<PathBuf>,
    #[arg(long, default_value_t = 0)]
    max_parse_failures: usize,
    #[arg(long, default_value_t = 0)]
    max_non_monotonic_events: usize,
    #[arg(long, default_value_t = 0)]
    max_non_finite_values: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preparation_has_no_research_or_dispatch_command() {
        for command in [
            "mission",
            "dispatch",
            "campaign-execute",
            "loop",
            "approval",
            "deployment",
        ] {
            let error = Cli::try_parse_from(["monday-cex-prepare", command]).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
        }
    }
    #[test]
    fn source_version_is_compiled_and_cannot_be_rebound_by_arguments() {
        let version = Cli::try_parse_from(["monday-cex-prepare", "--version"]).unwrap_err();
        assert_eq!(version.kind(), clap::error::ErrorKind::DisplayVersion);
        assert_eq!(
            version.to_string().trim(),
            format!("monday-cex-prepare {BUILD_SOURCE_REVISION}")
        );
        assert!(Cli::try_parse_from([
            "monday-cex-prepare",
            "--source-revision",
            &"b".repeat(40),
            "sources"
        ])
        .is_err());
    }
}
