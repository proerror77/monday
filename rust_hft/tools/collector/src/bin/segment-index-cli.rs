//! Segment index query and maintenance CLI
//!
//! Utility for querying, validating, and maintaining the Parquet segment index.

use anyhow::Result;
use clap::{Parser, Subcommand};
use hft_collector::segment_index::{diagnose_empty_result, query_segments};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[clap(name = "segment-index-cli")]
#[clap(about = "Query and maintain segment index")]
struct Args {
    /// Path to the Parquet index file
    #[clap(long, default_value = "/lake/output/metadata/segments.parquet")]
    index: PathBuf,

    #[clap(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Query segments
    Query {
        /// Market (e.g., usdm, spot)
        #[clap(long)]
        market: String,

        /// Symbol (e.g., SOLUSDT)
        #[clap(long)]
        symbol: String,

        /// Start time (nanoseconds)
        #[clap(long, default_value = "0")]
        start_ns: u64,

        /// End time (nanoseconds)
        #[clap(long, default_value = "9999999999999999999")]
        end_ns: u64,

        /// Only show replay-safe segments
        #[clap(long)]
        safe_only: bool,

        /// Output format
        #[clap(long, default_value = "table")]
        format: OutputFormat,
    },

    /// Show statistics
    Stats {
        /// Group by field (date, symbol, market)
        #[clap(long, default_value = "date")]
        group_by: String,

        /// Filter by symbol
        #[clap(long)]
        symbol: Option<String>,

        /// Limit results
        #[clap(long, default_value = "30")]
        limit: usize,
    },

    /// Find problematic segments
    Problems {
        /// Start date (YYYY-MM-DD)
        #[clap(long)]
        start_date: Option<String>,

        /// End date (YYYY-MM-DD)
        #[clap(long)]
        end_date: Option<String>,

        /// Filter by symbol
        #[clap(long)]
        symbol: Option<String>,
    },

    /// Validate index integrity
    Validate,

    /// Show index metadata
    Info,
}

#[derive(Debug, Clone)]
enum OutputFormat {
    Table,
    Json,
    Csv,
}

impl std::str::FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "table" => Ok(OutputFormat::Table),
            "json" => Ok(OutputFormat::Json),
            "csv" => Ok(OutputFormat::Csv),
            _ => Err(format!("Invalid format: {}", s)),
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    if !args.index.exists() {
        anyhow::bail!("Index file not found: {}", args.index.display());
    }

    match args.command {
        Command::Query {
            market,
            symbol,
            start_ns,
            end_ns,
            safe_only,
            format,
        } => {
            let segments =
                query_segments(&args.index, &market, &symbol, start_ns, end_ns, safe_only)?;

            if segments.is_empty() {
                eprintln!("No segments found.");
                if safe_only {
                    let diagnosis =
                        diagnose_empty_result(&args.index, &market, &symbol, start_ns, end_ns)?;
                    eprintln!("\n{}", diagnosis);
                }
                return Ok(());
            }

            match format {
                OutputFormat::Table => {
                    println!(
                        "{:<12} {:<5} {:<10} {:<20} {:<10} {:<15}",
                        "Date", "Hour", "Symbol", "Start (ns)", "Safe", "Bytes"
                    );
                    println!("{}", "-".repeat(90));
                    for seg in segments {
                        println!(
                            "{:<12} {:<5} {:<10} {:<20} {:<10} {:<15}",
                            seg.date,
                            seg.hour,
                            seg.symbol,
                            seg.start_received_at_ns,
                            seg.replay_safe,
                            seg.verified_bytes
                        );
                    }
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&segments)?);
                }
                OutputFormat::Csv => {
                    println!(
                        "date,hour,symbol,start_ns,end_ns,replay_safe,verified_bytes,tape_path"
                    );
                    for seg in segments {
                        println!(
                            "{},{},{},{},{},{},{},{}",
                            seg.date,
                            seg.hour,
                            seg.symbol,
                            seg.start_received_at_ns,
                            seg.end_received_at_ns,
                            seg.replay_safe,
                            seg.verified_bytes,
                            seg.tape_path
                        );
                    }
                }
            }

            println!("\nTotal: {} segments", segments.len());
        }

        Command::Stats {
            group_by,
            symbol,
            limit,
        } => {
            let conn = duckdb::Connection::open_in_memory()?;

            let symbol_filter = symbol
                .map(|s| format!("AND symbol = '{}'", s))
                .unwrap_or_default();

            let query = format!(
                r#"
                SELECT
                    {} as group_key,
                    COUNT(*) as total,
                    SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe,
                    ROUND(100.0 * SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) / COUNT(*), 2) as safe_pct,
                    SUM(verified_bytes) / 1024 / 1024 / 1024.0 as total_gb
                FROM read_parquet('{}')
                WHERE 1=1 {}
                GROUP BY {}
                ORDER BY {} DESC
                LIMIT {}
                "#,
                group_by,
                args.index.display(),
                symbol_filter,
                group_by,
                group_by,
                limit
            );

            let mut stmt = conn.prepare(&query)?;
            let mut rows = stmt.query([])?;

            println!(
                "{:<15} {:<10} {:<10} {:<12} {:<10}",
                group_by.to_uppercase(),
                "Total",
                "Safe",
                "Safe %",
                "Total GB"
            );
            println!("{}", "-".repeat(65));

            while let Some(row) = rows.next()? {
                let group_key: String = row.get(0)?;
                let total: i64 = row.get(1)?;
                let safe: i64 = row.get(2)?;
                let safe_pct: f64 = row.get(3)?;
                let total_gb: f64 = row.get(4)?;

                println!(
                    "{:<15} {:<10} {:<10} {:<11.2}% {:<10.2}",
                    group_key, total, safe, safe_pct, total_gb
                );
            }
        }

        Command::Problems {
            start_date,
            end_date,
            symbol,
        } => {
            let conn = duckdb::Connection::open_in_memory()?;

            let mut conditions = vec!["replay_safe = FALSE".to_string()];

            if let Some(start) = start_date {
                conditions.push(format!("date >= '{}'", start));
            }
            if let Some(end) = end_date {
                conditions.push(format!("date <= '{}'", end));
            }
            if let Some(sym) = symbol {
                conditions.push(format!("symbol = '{}'", sym));
            }

            let where_clause = conditions.join(" AND ");

            let query = format!(
                r#"
                SELECT
                    date,
                    hour,
                    symbol,
                    CASE
                        WHEN NOT has_replay_safe_checkpoint THEN 'missing_checkpoint'
                        WHEN NOT all_symbols_bridged THEN 'not_bridged'
                        WHEN NOT all_stream_coverage_verified THEN 'coverage_unverified'
                        WHEN venue_depth_complete THEN 'wrong_depth_flag'
                        ELSE 'unknown'
                    END as issue,
                    manifest_path
                FROM read_parquet('{}')
                WHERE {}
                ORDER BY date, hour
                "#,
                args.index.display(),
                where_clause
            );

            let mut stmt = conn.prepare(&query)?;
            let mut rows = stmt.query([])?;

            println!(
                "{:<12} {:<5} {:<10} {:<25} {:<50}",
                "Date", "Hour", "Symbol", "Issue", "Manifest Path"
            );
            println!("{}", "-".repeat(110));

            let mut count = 0;
            while let Some(row) = rows.next()? {
                let date: String = row.get(0)?;
                let hour: i32 = row.get(1)?;
                let symbol: String = row.get(2)?;
                let issue: String = row.get(3)?;
                let path: String = row.get(4)?;

                println!(
                    "{:<12} {:<5} {:<10} {:<25} {:<50}",
                    date,
                    hour,
                    symbol,
                    issue,
                    &path[..path.len().min(50)]
                );
                count += 1;
            }

            println!("\nTotal problems: {}", count);
        }

        Command::Validate => {
            let conn = duckdb::Connection::open_in_memory()?;

            println!("Validating index: {}", args.index.display());
            println!();

            // Check 1: File integrity
            let metadata = std::fs::metadata(&args.index)?;
            println!("✓ File exists: {} bytes", metadata.len());

            // Check 2: Can read as Parquet
            let count: i64 = conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM read_parquet('{}')",
                    args.index.display()
                ),
                [],
                |row| row.get(0),
            )?;
            println!("✓ Parquet readable: {} segments", count);

            // Check 3: Schema validation
            let schema_check: i64 = conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM read_parquet('{}') WHERE segment_id IS NULL",
                    args.index.display()
                ),
                [],
                |row| row.get(0),
            )?;
            if schema_check == 0 {
                println!("✓ No null segment IDs");
            } else {
                println!("⚠ Found {} segments with null IDs", schema_check);
            }

            // Check 4: Date range
            let (min_date, max_date): (String, String) = conn.query_row(
                &format!(
                    "SELECT MIN(date), MAX(date) FROM read_parquet('{}')",
                    args.index.display()
                ),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            println!("✓ Date range: {} to {}", min_date, max_date);

            // Check 5: Safety statistics
            let (total, safe): (i64, i64) = conn.query_row(
                &format!(
                    "SELECT COUNT(*), SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) FROM read_parquet('{}')",
                    args.index.display()
                ),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let safe_pct = (safe as f64 / total as f64) * 100.0;
            println!("✓ Safety rate: {}/{} ({:.2}%)", safe, total, safe_pct);

            if safe_pct < 95.0 {
                println!("⚠ Warning: Safety rate is below 95%");
            }

            println!();
            println!("Validation complete.");
        }

        Command::Info => {
            let conn = duckdb::Connection::open_in_memory()?;
            let metadata = std::fs::metadata(&args.index)?;

            println!("Index: {}", args.index.display());
            println!("Size: {} MB", metadata.len() / 1024 / 1024);
            println!();

            // Get summary stats
            let query = format!(
                r#"
                SELECT
                    COUNT(*) as total_segments,
                    COUNT(DISTINCT symbol) as unique_symbols,
                    COUNT(DISTINCT date) as unique_dates,
                    MIN(date) as first_date,
                    MAX(date) as last_date,
                    SUM(verified_bytes) / 1024 / 1024 / 1024.0 as total_gb,
                    SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe_segments
                FROM read_parquet('{}')
                "#,
                args.index.display()
            );

            let (total, symbols, dates, first, last, gb, safe): (
                i64,
                i64,
                i64,
                String,
                String,
                f64,
                i64,
            ) = conn.query_row(&query, [], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })?;

            println!("Total segments: {}", total);
            println!("Unique symbols: {}", symbols);
            println!("Date range: {} to {} ({} days)", first, last, dates);
            println!("Total data: {:.2} GB", gb);
            println!(
                "Replay safe: {}/{} ({:.2}%)",
                safe,
                total,
                (safe as f64 / total as f64) * 100.0
            );
        }
    }

    Ok(())
}
