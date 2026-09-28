//! Segment index backfill utility
//!
//! Scans existing manifest.json files and builds a Parquet index for fast querying.
//! This is a one-time operation; afterward the collector maintains the index incrementally.

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Parser, Debug)]
#[clap(name = "segment-index-backfill")]
#[clap(about = "Build Parquet segment index from existing manifests")]
struct Args {
    /// Root directory of raw market data (e.g., /lake/raw)
    #[clap(long)]
    raw_root: PathBuf,

    /// Output path for Parquet index file
    #[clap(long)]
    index_output: PathBuf,

    /// Market to process (e.g., usdm, spot)
    #[clap(long)]
    market: String,

    /// Start date (YYYY-MM-DD)
    #[clap(long)]
    start_date: String,

    /// End date (YYYY-MM-DD, inclusive)
    #[clap(long)]
    end_date: String,

    /// Batch size for processing
    #[clap(long, default_value = "1000")]
    batch_size: usize,

    /// Dry run (don't write output)
    #[clap(long)]
    dry_run: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    tracing::info!("Starting segment index backfill");
    tracing::info!("  Raw root: {}", args.raw_root.display());
    tracing::info!("  Index output: {}", args.index_output.display());
    tracing::info!("  Market: {}", args.market);
    tracing::info!("  Date range: {} to {}", args.start_date, args.end_date);
    tracing::info!("  Batch size: {}", args.batch_size);
    tracing::info!("  Dry run: {}", args.dry_run);

    // Parse dates
    let start_date = chrono::NaiveDate::parse_from_str(&args.start_date, "%Y-%m-%d")
        .context("Invalid start date format")?;
    let end_date = chrono::NaiveDate::parse_from_str(&args.end_date, "%Y-%m-%d")
        .context("Invalid end date format")?;

    if start_date > end_date {
        anyhow::bail!("Start date must be before or equal to end date");
    }

    // Ensure output directory exists
    if let Some(parent) = args.index_output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output directory: {}", parent.display()))?;
    }

    // Scan manifests
    tracing::info!("Scanning manifests...");
    let manifests = find_manifests(&args.raw_root, &args.market, start_date, end_date)?;
    tracing::info!("Found {} manifest files", manifests.len());

    if manifests.is_empty() {
        tracing::warn!("No manifests found in the specified range");
        return Ok(());
    }

    // Process in batches
    let processed = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let replay_safe = Arc::new(AtomicU64::new(0));
    let replay_unsafe = Arc::new(AtomicU64::new(0));

    let temp_index = if args.dry_run {
        None
    } else {
        Some(args.index_output.with_extension("tmp"))
    };

    for (batch_idx, chunk) in manifests.chunks(args.batch_size).enumerate() {
        tracing::info!(
            "Processing batch {}/{} ({} manifests)",
            batch_idx + 1,
            (manifests.len() + args.batch_size - 1) / args.batch_size,
            chunk.len()
        );

        for manifest_path in chunk {
            match process_manifest(manifest_path, temp_index.as_ref()) {
                Ok(metadata) => {
                    processed.fetch_add(1, Ordering::Relaxed);
                    if metadata.replay_safe {
                        replay_safe.fetch_add(1, Ordering::Relaxed);
                    } else {
                        replay_unsafe.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!("Failed to process {}: {}", manifest_path.display(), e);
                }
            }
        }

        let p = processed.load(Ordering::Relaxed);
        let f = failed.load(Ordering::Relaxed);
        let safe = replay_safe.load(Ordering::Relaxed);
        let unsafe_count = replay_unsafe.load(Ordering::Relaxed);

        tracing::info!(
            "Progress: {} processed, {} failed, {} safe, {} unsafe",
            p,
            f,
            safe,
            unsafe_count
        );
    }

    // Finalize
    if !args.dry_run {
        if let Some(temp) = temp_index {
            tracing::info!("Finalizing index...");
            std::fs::rename(&temp, &args.index_output)
                .context("Failed to move temporary index to final location")?;
        }
    }

    let total_processed = processed.load(Ordering::Relaxed);
    let total_failed = failed.load(Ordering::Relaxed);
    let total_safe = replay_safe.load(Ordering::Relaxed);
    let total_unsafe = replay_unsafe.load(Ordering::Relaxed);

    tracing::info!("=== Backfill Complete ===");
    tracing::info!("  Total processed: {}", total_processed);
    tracing::info!("  Failed: {}", total_failed);
    tracing::info!("  Replay safe: {}", total_safe);
    tracing::info!("  Replay unsafe: {}", total_unsafe);
    tracing::info!(
        "  Safety rate: {:.2}%",
        (total_safe as f64 / total_processed as f64) * 100.0
    );

    if !args.dry_run {
        tracing::info!("  Index written to: {}", args.index_output.display());

        // Verify output
        let metadata = std::fs::metadata(&args.index_output)?;
        tracing::info!("  Index size: {} bytes", metadata.len());
    }

    Ok(())
}

fn find_manifests(
    raw_root: &PathBuf,
    market: &str,
    start_date: chrono::NaiveDate,
    end_date: chrono::NaiveDate,
) -> Result<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    let mut current_date = start_date;

    while current_date <= end_date {
        let date_str = current_date.format("%Y-%m-%d").to_string();

        // Try standard partitioning: venue=binance_{market}/date={date}/hour={hour}
        for venue_name in ["binance_usdm", "binance_spot"] {
            if (market == "usdm" && venue_name == "binance_usdm")
                || (market == "spot" && venue_name == "binance_spot")
            {
                for hour in 0..24 {
                    let manifest_path = raw_root
                        .join(format!("venue={}", venue_name))
                        .join(format!("date={}", date_str))
                        .join(format!("hour={:02}", hour))
                        .join("manifest.json");

                    if manifest_path.exists() {
                        manifests.push(manifest_path);
                    }
                }
            }
        }

        current_date = current_date.succ_opt().context("Date overflow")?;
    }

    Ok(manifests)
}

fn process_manifest(
    manifest_path: &PathBuf,
    index_path: Option<&PathBuf>,
) -> Result<hft_collector::segment_index::SegmentMetadata> {
    // Read manifest
    let content = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;

    let manifest: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse manifest: {}", manifest_path.display()))?;

    // Derive tape path from manifest path
    let tape_path = manifest_path
        .parent()
        .unwrap()
        .join("tape.jsonl.zst")
        .to_string_lossy()
        .to_string();

    // Create metadata
    let metadata = hft_collector::segment_index::SegmentMetadata::from_manifest(
        &manifest,
        &manifest_path.to_string_lossy(),
        &tape_path,
    )?;

    // Append to index if not dry run
    if let Some(index) = index_path {
        hft_collector::segment_index::append_to_parquet_index(index, &metadata)
            .context("Failed to append to index")?;
    }

    Ok(metadata)
}
