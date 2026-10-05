//! Historical terminal audit. Only independent platform, provider and native
//! result readback may produce the private evidence passed to the source ledger.
use clap::Args;
use std::path::PathBuf;

#[derive(Debug, Clone, Args)]
pub struct PlatformTerminalArgs {
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub control: PathBuf,
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
    /// Host-owned readonly observer release, public trust and transport paths.
    #[arg(long)]
    pub observation: PathBuf,
    /// Existing private directory retaining exact observation and native bytes.
    #[arg(long)]
    pub output: PathBuf,
    /// Retain a durable observation and list finite publication keys. This step
    /// issues no cleanup witness; rerun publication against the same audit.
    #[arg(long)]
    pub retain_only: bool,
}

#[cfg(feature = "scientific")]
mod observer;
#[cfg(feature = "scientific")]
mod platform_facts;
#[cfg(feature = "scientific")]
mod publication;
#[cfg(feature = "scientific")]
mod retained_files;
#[cfg(feature = "scientific")]
mod scientific_results;
#[cfg(any(test, feature = "scientific"))]
mod snapshot_transport;
#[cfg(any(test, feature = "scientific"))]
mod stopped_execution;

pub fn audit(args: PlatformTerminalArgs) -> anyhow::Result<()> {
    #[cfg(feature = "scientific")]
    {
        observer::audit(args)
    }
    #[cfg(not(feature = "scientific"))]
    {
        let _ = args;
        anyhow::bail!("native platform terminal audit requires the scientific validator build")
    }
}

#[cfg(all(test, feature = "scientific"))]
pub(in crate::mission_dispatch) mod tests;
