//! Compile a governed point-in-time snapshot from verified artifacts or Monday research tables.
//!
//! This binary is the boundary between collector storage and repeated research
//! scoring. Factor review, walk-forward, and optimizer jobs should consume the
//! resulting immutable snapshot artifacts instead of rebuilding raw joins.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use data::binance_market_tape_artifact::{
    seal_binance_market_tape_triplet, verify_binance_market_tape, BinanceMarketTapeTriplet,
    BinanceMarketTapeTrustAnchor, VerifiedBinanceMarketTape,
};
use ploy_market_data::diagnostics::PredictionMarketDataAuditReport;
use ploy_market_data::polymarket_evidence::{
    aggregate_verified_polymarket_evidence, seal_polymarket_evidence_triplet,
    verify_polymarket_evidence, PolymarketEvidenceTriplet, PolymarketEvidenceTrustAnchor,
    VerifiedPolymarketEvidence, VerifiedPolymarketEvidenceSet,
};
use ploy_research::research_snapshot::ResearchSnapshotInputArtifact;
use ploy_research::{
    build_research_snapshot_from_database,
    build_research_snapshot_from_polymarket_chainlink_baseline,
    build_research_snapshot_from_verified_artifacts, write_research_snapshot,
    ResearchSnapshotBuildOptions, VerifiedArtifactSnapshotBuildOptions,
};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;

const VERIFIED_ARTIFACT_VALUE_FLAGS: [&str; 6] = [
    "--segment",
    "--segment-content-sha256",
    "--segment-manifest-sha256",
    "--polymarket-artifact",
    "--polymarket-content-sha256",
    "--polymarket-manifest-sha256",
];
const VERIFIED_MODE_FORBIDDEN_FLAGS: [&str; 7] = [
    "--allow-missing-official-settlement",
    "--data-audit-report",
    "--data-audit-sha256",
    "--data-audit-status",
    "--data-requirements",
    "--pm-book-archive-dir",
    "--skip-deribit",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnchoredArtifact {
    data: PathBuf,
    content_sha256: String,
    manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedArtifactArgs {
    binance: Vec<AnchoredArtifact>,
    polymarket: Vec<AnchoredArtifact>,
    polymarket_chainlink_baseline: bool,
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn flag_present(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn checked_flag_values(args: &[String], flag: &str) -> anyhow::Result<Vec<String>> {
    args.iter()
        .enumerate()
        .filter(|(_, argument)| argument.as_str() == flag)
        .map(|(index, _)| {
            let value = args
                .get(index + 1)
                .with_context(|| format!("{flag} requires a value"))?;
            if value.trim().is_empty() || value.starts_with("--") {
                anyhow::bail!("{flag} requires a value");
            }
            Ok(value.clone())
        })
        .collect()
}

fn parse_anchored_artifacts(
    args: &[String],
    data_flag: &str,
    content_flag: &str,
    manifest_flag: &str,
) -> anyhow::Result<Vec<AnchoredArtifact>> {
    let data = checked_flag_values(args, data_flag)?;
    let content = checked_flag_values(args, content_flag)?;
    let manifests = checked_flag_values(args, manifest_flag)?;
    if data.is_empty() || content.len() != data.len() || manifests.len() != data.len() {
        anyhow::bail!(
            "{data_flag}, {content_flag}, and {manifest_flag} must have equal nonzero lengths"
        );
    }
    Ok(data
        .into_iter()
        .zip(content)
        .zip(manifests)
        .map(
            |((data, content_sha256), manifest_sha256)| AnchoredArtifact {
                data: PathBuf::from(data),
                content_sha256,
                manifest_sha256,
            },
        )
        .collect())
}

fn parse_verified_artifact_args(
    args: &[String],
    symbols: &[String],
) -> anyhow::Result<Option<VerifiedArtifactArgs>> {
    let full_mode_count = args
        .iter()
        .filter(|argument| argument.as_str() == "--verified-artifacts")
        .count();
    let baseline_mode_count = args
        .iter()
        .filter(|argument| argument.as_str() == "--polymarket-chainlink-baseline")
        .count();
    let mode_count = full_mode_count + baseline_mode_count;
    let has_artifact_value = VERIFIED_ARTIFACT_VALUE_FLAGS
        .iter()
        .any(|flag| flag_present(args, flag));
    if mode_count == 0 {
        if has_artifact_value {
            anyhow::bail!(
                "verified artifact inputs require --verified-artifacts or --polymarket-chainlink-baseline"
            );
        }
        return Ok(None);
    }
    if mode_count != 1 {
        anyhow::bail!(
            "exactly one of --verified-artifacts or --polymarket-chainlink-baseline is required"
        );
    }
    if let Some(flag) = VERIFIED_MODE_FORBIDDEN_FLAGS
        .iter()
        .find(|flag| flag_present(args, flag))
    {
        anyhow::bail!("{flag} cannot be combined with verified artifact modes");
    }
    if symbols.len() != 1 || !matches!(symbols[0].as_str(), "BTCUSDT" | "SOLUSDT") {
        anyhow::bail!("verified-artifact mode requires exactly one BTCUSDT or SOLUSDT symbol");
    }

    let polymarket_chainlink_baseline = baseline_mode_count == 1;
    let binance = if polymarket_chainlink_baseline {
        if [
            "--segment",
            "--segment-content-sha256",
            "--segment-manifest-sha256",
        ]
        .iter()
        .any(|flag| flag_present(args, flag))
        {
            anyhow::bail!(
                "Binance segment arguments cannot be combined with --polymarket-chainlink-baseline"
            );
        }
        Vec::new()
    } else {
        parse_anchored_artifacts(
            args,
            "--segment",
            "--segment-content-sha256",
            "--segment-manifest-sha256",
        )?
    };

    Ok(Some(VerifiedArtifactArgs {
        binance,
        polymarket: parse_anchored_artifacts(
            args,
            "--polymarket-artifact",
            "--polymarket-content-sha256",
            "--polymarket-manifest-sha256",
        )?,
        polymarket_chainlink_baseline,
    }))
}

fn parse_polymarket_verify_args(args: &[String]) -> anyhow::Result<Vec<AnchoredArtifact>> {
    if args.get(1).map(String::as_str) != Some("--verify-polymarket-evidence") {
        anyhow::bail!("--verify-polymarket-evidence must be supplied exactly once");
    }
    let groups = &args[2..];
    if groups.is_empty() || groups.len() % 6 != 0 {
        anyhow::bail!(
            "--verify-polymarket-evidence requires complete artifact groups: +             --polymarket-artifact PATH --polymarket-content-sha256 SHA256 +             --polymarket-manifest-sha256 SHA256"
        );
    }
    groups
        .chunks_exact(6)
        .map(|group| {
            if group[0] != "--polymarket-artifact"
                || group[2] != "--polymarket-content-sha256"
                || group[4] != "--polymarket-manifest-sha256"
                || [&group[1], &group[3], &group[5]]
                    .iter()
                    .any(|value| value.trim().is_empty() || value.starts_with("--"))
            {
                anyhow::bail!("--verify-polymarket-evidence requires complete artifact groups");
            }
            Ok(AnchoredArtifact {
                data: PathBuf::from(&group[1]),
                content_sha256: group[3].clone(),
                manifest_sha256: group[5].clone(),
            })
        })
        .collect()
}

fn artifact_triplet(path: &Path) -> anyhow::Result<(PathBuf, PathBuf, PathBuf)> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("artifact path has no UTF-8 file name")?;
    Ok((
        path.to_path_buf(),
        path.with_file_name(format!("{name}.manifest.json")),
        path.with_file_name(format!("{name}._SUCCESS")),
    ))
}

fn verify_polymarket_artifact(
    artifact: &AnchoredArtifact,
) -> anyhow::Result<VerifiedPolymarketEvidence> {
    let (data, manifest, success) = artifact_triplet(&artifact.data)?;
    let trust = PolymarketEvidenceTrustAnchor::from_lower_hex(
        &artifact.content_sha256,
        &artifact.manifest_sha256,
    )?;
    verify_polymarket_evidence(seal_polymarket_evidence_triplet(
        &PolymarketEvidenceTriplet {
            data,
            manifest,
            success,
        },
        &trust,
    )?)
    .with_context(|| format!("verify Polymarket artifact {}", artifact.data.display()))
}

fn verify_binance_artifacts(
    artifacts: &[AnchoredArtifact],
) -> anyhow::Result<VerifiedBinanceMarketTape> {
    let sealed = artifacts
        .iter()
        .map(|artifact| {
            let (data, manifest, success) = artifact_triplet(&artifact.data)?;
            let trust = BinanceMarketTapeTrustAnchor::from_lower_hex(
                &artifact.content_sha256,
                &artifact.manifest_sha256,
            )?;
            seal_binance_market_tape_triplet(
                &BinanceMarketTapeTriplet {
                    data,
                    manifest,
                    success,
                },
                &trust,
            )
            .with_context(|| format!("seal Binance artifact {}", artifact.data.display()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    verify_binance_market_tape(sealed)
}

fn verify_polymarket_artifacts(
    artifacts: &[AnchoredArtifact],
) -> anyhow::Result<VerifiedPolymarketEvidenceSet> {
    let verified = artifacts
        .iter()
        .map(verify_polymarket_artifact)
        .collect::<anyhow::Result<Vec<_>>>()?;
    aggregate_verified_polymarket_evidence(verified)
}

fn verify_polymarket_only(args: &[String]) -> anyhow::Result<()> {
    let artifacts = parse_polymarket_verify_args(args)?;
    let members = artifacts
        .iter()
        .map(|artifact| {
            let verified = verify_polymarket_artifact(artifact)?;
            let identity = verified.identity();
            let market_ids = verified
                .contracts()
                .iter()
                .map(|contract| contract.market_id.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            Ok(serde_json::json!({
                "data": artifact.data,
                "content_sha256": identity.content_sha256,
                "manifest_sha256": identity.manifest_sha256,
                "event_start_gte": identity.event_start_gte.to_rfc3339(),
                "event_start_lt": identity.event_start_lt.to_rfc3339(),
                "rows": identity.rows,
                "events": identity.events,
                "market_ids": market_ids,
                "contracts": verified.contracts().len(),
                "books": verified.books().len(),
                "references": verified.references().len(),
                "trades": verified.trades().len(),
                "settlements": verified.settlements().len(),
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "schema": "monday.polymarket.evidence_verification.v1",
            "members": members,
        }))?
    );
    Ok(())
}

fn parse_csv(raw: Option<String>, default: &str) -> Vec<String> {
    raw.unwrap_or_else(|| default.to_string())
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    let next_day = date
        .succ_opt()
        .unwrap_or_else(|| panic!("invalid end date: {raw}"));
    Utc.from_utc_datetime(&next_day.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    raw.parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| panic!("invalid timestamp: {raw}"))
}

fn validated_data_audit(
    report_path: Option<String>,
    supplied_status: Option<String>,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> anyhow::Result<(Option<String>, Option<String>, Option<String>)> {
    let Some(report_path) = report_path else {
        anyhow::bail!(
            "--data-audit-report is required; prediction snapshots cannot be compiled without typed audit evidence"
        );
    };
    let bytes = std::fs::read(&report_path)
        .map_err(|error| anyhow::anyhow!("read data audit report {report_path}: {error}"))?;
    let report: PredictionMarketDataAuditReport = serde_json::from_slice(&bytes)
        .map_err(|error| anyhow::anyhow!("parse typed data audit report {report_path}: {error}"))?;
    report
        .validate_for_prediction_snapshot(symbols, start, end)
        .map_err(|error| anyhow::anyhow!("validate data audit report {report_path}: {error}"))?;
    let report_status = report.data_audit_status.as_str().to_string();
    if supplied_status
        .as_deref()
        .is_some_and(|status| status != report_status)
    {
        anyhow::bail!("--data-audit-status does not match typed report status {report_status}");
    }
    let report_hash = format!("sha256:{:x}", Sha256::digest(&bytes));
    Ok((Some(report_status), Some(report_path), Some(report_hash)))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if flag_present(&args, "--verify-polymarket-evidence") {
        return verify_polymarket_only(&args);
    }
    if args
        .iter()
        .any(|arg| arg == "--db-url" || arg.starts_with("--db-url="))
    {
        anyhow::bail!(
            "--db-url is forbidden because it exposes credentials; use MONDAY_RESEARCH_DATABASE_URL or DATABASE_URL"
        );
    }
    let output_dir =
        PathBuf::from(flag_value(&args, "--output-dir").expect("--output-dir required"));
    let start = flag_value(&args, "--start-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_start(&flag_value(&args, "--start-date").expect("--start-date required"))
        });
    let end = flag_value(&args, "--end-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_end(&flag_value(&args, "--end-date").expect("--end-date required"))
        });
    let symbols: Vec<String> = parse_csv(flag_value(&args, "--symbols"), "BTCUSDT");
    let lob_sample_secs: i32 = flag_value(&args, "--lob-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let pm_book_sample_secs: i32 = flag_value(&args, "--pm-book-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(lob_sample_secs);
    let max_quote_age_secs: i64 = flag_value(&args, "--max-quote-age-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let observation_sample_secs: i64 = flag_value(&args, "--observation-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let stake_usd = flag_value(&args, "--stake-usd")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(15.0);
    let optimizer_data_dir =
        flag_value(&args, "--optimizer-data-dir").expect("--optimizer-data-dir required");
    let verified_artifacts = parse_verified_artifact_args(&args, &symbols)?;

    let manifest = if let Some(artifacts) = verified_artifacts {
        let polymarket = verify_polymarket_artifacts(&artifacts.polymarket)?;
        let options = VerifiedArtifactSnapshotBuildOptions {
            symbol: symbols[0].clone(),
            start,
            end,
            lob_sample_secs,
            pm_book_sample_secs,
            observation_sample_secs,
            max_quote_age_secs,
            stake_usd,
            optimizer_data_dir,
            git_sha: std::env::var("GITHUB_SHA").ok(),
        };
        let snapshot = if artifacts.polymarket_chainlink_baseline {
            eprintln!(
                "monday-prediction-snapshot: {} -> {} for {:?}, source=verified_polymarket_chainlink_baseline, polymarket_members={}, stake_usd={:.2}, output={}, pm_book_sample_secs={}",
            start,
            end,
            symbols,
            artifacts.polymarket.len(),
            stake_usd,
            output_dir.display(),
            pm_book_sample_secs,
        );
            build_research_snapshot_from_polymarket_chainlink_baseline(&polymarket, options)?
        } else {
            let binance = verify_binance_artifacts(&artifacts.binance)?;
            eprintln!(
                "monday-prediction-snapshot: {} -> {} for {:?}, source=verified_immutable_artifacts, binance_segments={}, polymarket_members={}, stake_usd={:.2}, output={}, pm_book_sample_secs={}",
                start,
                end,
                symbols,
                artifacts.binance.len(),
                artifacts.polymarket.len(),
                stake_usd,
                output_dir.display(),
                pm_book_sample_secs,
            );
            build_research_snapshot_from_verified_artifacts(&binance, &polymarket, options)?
        };
        write_research_snapshot(&output_dir, snapshot)?
    } else {
        let db_url = std::env::var("MONDAY_RESEARCH_DATABASE_URL")
            .ok()
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "database URL is required via MONDAY_RESEARCH_DATABASE_URL or DATABASE_URL"
                )
            })?;
        let require_official_settlement =
            !flag_present(&args, "--allow-missing-official-settlement");
        let data_requirements = parse_csv(flag_value(&args, "--data-requirements"), "all");
        let (data_audit_status, data_audit_report, data_audit_report_hash) = validated_data_audit(
            flag_value(&args, "--data-audit-report"),
            flag_value(&args, "--data-audit-status"),
            &symbols,
            start,
            end,
        )?;
        let include_deribit = !flag_present(&args, "--skip-deribit");
        let pm_book_archive_dir = flag_value(&args, "--pm-book-archive-dir")
            .or_else(|| std::env::var("MONDAY_PREDICTION_BOOK_ARCHIVE_DIR").ok())
            .filter(|raw| !raw.trim().is_empty())
            .map(PathBuf::from);

        eprintln!(
            "monday-prediction-snapshot: {} -> {} for {:?}, stake_usd={:.2}, output={}, data_requirements={}, include_deribit={}, pm_book_sample_secs={}, pm_book_archive_dir={}",
            start,
            end,
            symbols,
            stake_usd,
            output_dir.display(),
            data_requirements.join(","),
            include_deribit,
            pm_book_sample_secs,
            pm_book_archive_dir
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<not-configured>".to_string())
        );

        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(120))
            .connect(&db_url)
            .await?;

        let mut snapshot = build_research_snapshot_from_database(
            &pool,
            ResearchSnapshotBuildOptions {
                symbols,
                start,
                end,
                lob_sample_secs,
                pm_book_sample_secs,
                observation_sample_secs,
                max_quote_age_secs,
                stake_usd,
                require_official_settlement,
                optimizer_data_dir: Some(optimizer_data_dir),
                git_sha: std::env::var("GITHUB_SHA").ok(),
                data_requirements,
                data_audit_status,
                data_audit_report: data_audit_report.clone(),
                include_deribit,
                pm_book_archive_dir,
            },
        )
        .await?;
        if let (Some(path), Some(content_hash)) =
            (data_audit_report.as_ref(), data_audit_report_hash.as_ref())
        {
            snapshot
                .manifest
                .input_artifacts
                .push(ResearchSnapshotInputArtifact {
                    name: "prediction_market_data_audit".to_string(),
                    path: path.clone(),
                    content_hash: Some(content_hash.clone()),
                    row_count: None,
                });
        }
        write_research_snapshot(&output_dir, snapshot)?
    };

    eprintln!("research snapshot written: {}", output_dir.display());
    eprintln!(
        "rows: observations={} deribit={} pm_books={}",
        manifest.row_counts.observations,
        manifest.row_counts.deribit_snapshots,
        manifest.row_counts.pm_book_snapshots
    );
    for timing in &manifest.phase_timings {
        eprintln!(
            "phase {:<24} {:>8} ms rows={}",
            timing.phase,
            timing.elapsed_ms,
            timing
                .rows
                .map(|rows| rows.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_polymarket_verify_args, parse_verified_artifact_args, validated_data_audit};
    use chrono::{TimeZone, Utc};

    fn digest(byte: char) -> String {
        byte.to_string().repeat(64)
    }

    fn verified_args() -> Vec<String> {
        [
            "--verified-artifacts".to_string(),
            "--segment".to_string(),
            "/cloud/binance/part-1.jsonl.zst".to_string(),
            "--segment-content-sha256".to_string(),
            digest('1'),
            "--segment-manifest-sha256".to_string(),
            digest('2'),
            "--polymarket-artifact".to_string(),
            "/cloud/polymarket/hour-14.ndjson".to_string(),
            "--polymarket-content-sha256".to_string(),
            digest('3'),
            "--polymarket-manifest-sha256".to_string(),
            digest('4'),
            "--polymarket-artifact".to_string(),
            "/cloud/polymarket/hour-15.ndjson".to_string(),
            "--polymarket-content-sha256".to_string(),
            digest('5'),
            "--polymarket-manifest-sha256".to_string(),
            digest('6'),
        ]
        .into()
    }

    fn polymarket_chainlink_baseline_args() -> Vec<String> {
        [
            "--polymarket-chainlink-baseline".to_string(),
            "--polymarket-artifact".to_string(),
            "/cloud/polymarket/hour-14.ndjson".to_string(),
            "--polymarket-content-sha256".to_string(),
            digest('3'),
            "--polymarket-manifest-sha256".to_string(),
            digest('4'),
        ]
        .into()
    }

    #[test]
    fn prediction_snapshot_requires_typed_audit_evidence() {
        let start = Utc
            .with_ymd_and_hms(2026, 7, 1, 0, 0, 0)
            .single()
            .expect("valid start");
        let end = Utc
            .with_ymd_and_hms(2026, 7, 2, 0, 0, 0)
            .single()
            .expect("valid end");

        let error = validated_data_audit(None, None, &["BTCUSDT".to_string()], start, end)
            .expect_err("missing typed audit must fail closed");

        assert!(error
            .to_string()
            .contains("--data-audit-report is required"));
    }

    #[test]
    fn verified_artifact_mode_accepts_repeated_anchored_triplets() {
        let parsed = parse_verified_artifact_args(&verified_args(), &["BTCUSDT".to_string()])
            .unwrap()
            .expect("verified artifact mode");

        assert_eq!(parsed.binance.len(), 1);
        assert_eq!(parsed.polymarket.len(), 2);
        assert_eq!(parsed.polymarket[1].manifest_sha256, digest('6'));
    }

    #[test]
    fn polymarket_chainlink_baseline_is_explicit_and_forbids_binance_segments() {
        let parsed = parse_verified_artifact_args(
            &polymarket_chainlink_baseline_args(),
            &["BTCUSDT".to_string()],
        )
        .unwrap()
        .expect("explicit Polymarket + Chainlink baseline");

        assert!(parsed.binance.is_empty());
        assert_eq!(parsed.polymarket.len(), 1);

        let mut mixed = polymarket_chainlink_baseline_args();
        mixed.extend([
            "--segment".to_string(),
            "/cloud/binance/part-1.jsonl.zst".to_string(),
            "--segment-content-sha256".to_string(),
            digest('1'),
            "--segment-manifest-sha256".to_string(),
            digest('2'),
        ]);
        assert!(
            parse_verified_artifact_args(&mixed, &["BTCUSDT".to_string()])
                .unwrap_err()
                .to_string()
                .contains("cannot be combined")
        );
    }

    #[test]
    fn polymarket_verify_mode_accepts_only_repeated_polymarket_anchors() {
        let args = [
            "monday-prediction-snapshot".to_string(),
            "--verify-polymarket-evidence".to_string(),
            "--polymarket-artifact".to_string(),
            "/cloud/polymarket/one.ndjson".to_string(),
            "--polymarket-content-sha256".to_string(),
            digest('1'),
            "--polymarket-manifest-sha256".to_string(),
            digest('2'),
            "--polymarket-artifact".to_string(),
            "/cloud/polymarket/two.ndjson".to_string(),
            "--polymarket-content-sha256".to_string(),
            digest('3'),
            "--polymarket-manifest-sha256".to_string(),
            digest('4'),
        ];

        let parsed = parse_polymarket_verify_args(&args).unwrap();

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].content_sha256, digest('3'));

        let mut mixed = args.to_vec();
        mixed.extend(["--output-dir".to_string(), "/tmp/snapshot".to_string()]);
        assert!(parse_polymarket_verify_args(&mixed)
            .unwrap_err()
            .to_string()
            .contains("complete artifact groups"));

        let ungrouped = [
            "monday-prediction-snapshot".to_string(),
            "--verify-polymarket-evidence".to_string(),
            "--polymarket-artifact".to_string(),
            "/cloud/polymarket/one.ndjson".to_string(),
            "--polymarket-artifact".to_string(),
            "/cloud/polymarket/two.ndjson".to_string(),
            "--polymarket-content-sha256".to_string(),
            digest('1'),
            "--polymarket-content-sha256".to_string(),
            digest('3'),
            "--polymarket-manifest-sha256".to_string(),
            digest('2'),
            "--polymarket-manifest-sha256".to_string(),
            digest('4'),
        ];
        assert!(parse_polymarket_verify_args(&ungrouped)
            .unwrap_err()
            .to_string()
            .contains("complete artifact groups"));
    }

    #[test]
    fn polymarket_verify_mode_rejects_malformed_trust_anchor_before_file_access() {
        let artifact = super::AnchoredArtifact {
            data: "/does/not/exist.ndjson".into(),
            content_sha256: digest('A'),
            manifest_sha256: digest('b'),
        };

        let error = super::verify_polymarket_artifact(&artifact).unwrap_err();

        assert!(error
            .to_string()
            .contains("expected content SHA-256 must be 64 lowercase hexadecimal characters"));
    }

    #[test]
    fn verified_artifact_mode_requires_parallel_external_anchors() {
        let mut args = verified_args();
        let index = args
            .iter()
            .rposition(|argument| argument == "--polymarket-manifest-sha256")
            .unwrap();
        args.drain(index..=index + 1);

        assert!(
            parse_verified_artifact_args(&args, &["BTCUSDT".to_string()])
                .unwrap_err()
                .to_string()
                .contains("equal nonzero lengths")
        );
    }

    #[test]
    fn verified_artifact_mode_rejects_implicit_mixed_or_multi_symbol_inputs() {
        let mut implicit = verified_args();
        implicit.remove(0);
        assert!(
            parse_verified_artifact_args(&implicit, &["BTCUSDT".to_string()])
                .unwrap_err()
                .to_string()
                .contains("require --verified-artifacts")
        );

        for flag in ["--data-audit-report", "--data-audit-sha256"] {
            let mut mixed = verified_args();
            mixed.extend([flag.to_string(), "legacy-external-audit".to_string()]);
            assert!(
                parse_verified_artifact_args(&mixed, &["BTCUSDT".to_string()])
                    .unwrap_err()
                    .to_string()
                    .contains("cannot be combined")
            );
        }

        assert!(parse_verified_artifact_args(
            &verified_args(),
            &["BTCUSDT".to_string(), "SOLUSDT".to_string()],
        )
        .unwrap_err()
        .to_string()
        .contains("exactly one BTCUSDT or SOLUSDT symbol"));
    }
}
