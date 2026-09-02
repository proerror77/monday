use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, ValueEnum};
use data::binance_lob_replay::{
    source_revision as governed_source_revision, Market as LobMarket, ReplaySequenceEvent,
};
use data::binance_market_tape::AggregateTrade;
use data::binance_market_tape_artifact::{
    seal_binance_market_tape_triplet, verify_binance_market_tape_series_for_strict_gate,
    BinanceMarketTapeTriplet, BinanceMarketTapeTrustAnchor, ReplayedBinanceBookEvent,
    VerifiedBinanceMarketTapeSeries,
};
use hft_collector::binance_usdm_reference_artifact::{
    verify_reference_artifact_read_only_current_batch, PublishedReferenceArtifact,
};
use hft_collector::{DataModality, PointInTimeFeatureRow};
use hft_core::{top5_book_features, TOP5_DEPTH};
use hft_research_manifest::{
    CexArtifactTripletV2, CexInstrumentRulesV2, CexPitSeriesEvidenceV2, CexReplaySegmentIdentity,
    CexReplaySeriesV1, CexReplaySnapshotV5, BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7,
    CEX_DERIVATIVES_MAX_GAP_NS, CEX_FEATURE_AVAILABILITY_POLICY, CEX_MODALITY_AGGREGATE_TRADE,
    CEX_MODALITY_LOB, CEX_REPLAY_CLOCK_RECEIVED_AT_NS, CEX_REPLAY_SNAPSHOT_SCHEMA_V5,
};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Market {
    Spot,
    Usdm,
}

impl Market {
    fn as_str(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Usdm => "usdm",
        }
    }

    fn as_lob_market(self) -> LobMarket {
        match self {
            Self::Spot => LobMarket::Spot,
            Self::Usdm => LobMarket::Usdm,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "lob-pit-materializer",
    about = "Replay Binance LOB segments into immutable PIT feature artifacts"
)]
struct Args {
    #[arg(long)]
    mission_id: String,
    #[arg(long)]
    symbol: String,
    #[arg(long, value_enum)]
    market: Market,
    #[arg(long, default_value_t = 1_000)]
    bucket_ms: u64,
    #[arg(long, default_value_t = 5)]
    label_horizon_buckets: usize,
    #[arg(long, default_value_t = 5)]
    top_depth: usize,
    #[arg(long, required = true)]
    segment: Vec<PathBuf>,
    #[arg(long, required = true)]
    segment_content_sha256: Vec<String>,
    #[arg(long, required = true)]
    segment_manifest_sha256: Vec<String>,
    #[arg(long)]
    artifact_dir: PathBuf,
    #[arg(long)]
    reference_data: Vec<PathBuf>,
    #[arg(long)]
    reference_data_sha256: Vec<String>,
    #[arg(long)]
    reference_manifest_sha256: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SourceSegmentEvidence {
    path: PathBuf,
    sha256: String,
    collector_manifest_path: PathBuf,
    collector_manifest_sha256: String,
    success_marker_path: PathBuf,
    success_marker_sha256: String,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    events: u64,
}

#[derive(Debug, Clone, PartialEq)]
struct BookState {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
}

#[derive(Debug, Clone)]
struct BookSample {
    series_id: u64,
    time_ns: u64,
    mid_price: f64,
    spread_bps: f64,
    bid_depth: f64,
    ask_depth: f64,
    top_depth_imbalance: f64,
    weighted_book_imbalance_top5: Option<f64>,
    near_depth_concentration_skew_top5: Option<f64>,
    vwap_center_deviation_top5_bps: Option<f64>,
    book_imbalance: f64,
}

#[derive(Debug, Serialize)]
struct MaterializationReport {
    dataset_kind: String,
    schema_version: String,
    mission_id: String,
    symbol: String,
    market: String,
    bucket_ms: u64,
    label_horizon_buckets: usize,
    top_depth: usize,
    source_revision: String,
    source_segments: Vec<SourceSegmentEvidence>,
    series_count: usize,
    rows: usize,
    first_event_time: DateTime<Utc>,
    last_event_time: DateTime<Utc>,
    artifact_path: PathBuf,
    artifact_sha256: String,
    snapshot: CexReplaySnapshotV5,
    snapshot_sha256: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct PublishedMaterialization {
    report: MaterializationReport,
    report_path: PathBuf,
    report_sha256: String,
}

#[derive(Debug, Clone)]
struct RuleObservation {
    available_at: DateTime<Utc>,
    triplet: CexArtifactTripletV2,
}

fn log_event(event: &str, details: Value) {
    eprintln!(
        "{}",
        json!({
            "schema_version": "monday.research_event.v1",
            "timestamp": Utc::now().to_rfc3339(),
            "component": "lob-pit-materializer",
            "event": event,
            "details": details,
        })
    );
}

struct Replay {
    bucket_ns: u64,
    depth: usize,
    state: Option<BookState>,
    samples: Vec<BookSample>,
    next_bucket_ns: Option<u64>,
    series_id: u64,
}

impl Replay {
    fn new(bucket_ns: u64, depth: usize) -> Self {
        Self {
            bucket_ns,
            depth,
            state: None,
            samples: Vec::new(),
            next_bucket_ns: None,
            series_id: 0,
        }
    }

    fn start_series(&mut self, state: BookState, received_at_ns: u64) -> Result<()> {
        self.state = Some(state);
        self.series_id = self
            .series_id
            .checked_add(1)
            .context("series id overflow")?;
        self.next_bucket_ns = Some(ceil_bucket(received_at_ns, self.bucket_ns)?);
        Ok(())
    }

    fn emit_before(&mut self, received_at_ns: u64) -> Result<()> {
        self.emit_until(received_at_ns, false)
    }

    fn emit_at(&mut self, received_at_ns: u64) -> Result<()> {
        self.emit_until(received_at_ns, true)
    }

    fn emit_until(&mut self, received_at_ns: u64, inclusive: bool) -> Result<()> {
        loop {
            let Some(next_bucket_ns) = self.next_bucket_ns else {
                return Ok(());
            };
            let due = if inclusive {
                next_bucket_ns <= received_at_ns
            } else {
                next_bucket_ns < received_at_ns
            };
            if !due {
                return Ok(());
            }
            let state = self.state.as_ref().context("bucket has no replay state")?;
            self.samples.push(sample_book(
                state,
                self.series_id,
                next_bucket_ns,
                self.depth,
            )?);
            self.next_bucket_ns = Some(
                next_bucket_ns
                    .checked_add(self.bucket_ns)
                    .context("bucket time overflow")?,
            );
        }
    }

    fn consume(&mut self, events: &[ReplayedBinanceBookEvent]) -> Result<()> {
        let mut start = 0;
        while start < events.len() {
            let received_at_ns = events[start].received_at_ns();
            let mut end = start + 1;
            while end < events.len() && events[end].received_at_ns() == received_at_ns {
                end += 1;
            }
            let batch = &events[start..end];
            let has_snapshot = batch.iter().any(|event| {
                matches!(
                    event,
                    ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot { .. })
                )
            });
            let has_diff = batch.iter().any(|event| {
                matches!(
                    event,
                    ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Diff { .. })
                )
            });
            if has_diff && !has_snapshot {
                self.emit_before(received_at_ns)?;
            }
            for event in batch {
                match event {
                    ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot {
                        bids,
                        asks,
                        ..
                    }) => self.start_series(
                        BookState {
                            bids: parse_levels(bids, "bid")?,
                            asks: parse_levels(asks, "ask")?,
                        },
                        received_at_ns,
                    )?,
                    ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Diff {
                        bids,
                        asks,
                        ..
                    }) => {
                        let state = self.state.as_mut().context("diff has no replay state")?;
                        apply_levels(&mut state.bids, bids, "bid")?;
                        apply_levels(&mut state.asks, asks, "ask")?;
                    }
                    ReplayedBinanceBookEvent::Checkpoint { .. } => {}
                }
            }
            if has_snapshot || has_diff {
                self.emit_at(received_at_ns)?;
            }
            start = end;
        }
        Ok(())
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    log_event(
        "pit_materialization_started",
        json!({
            "mission_id": &args.mission_id,
            "market": args.market.as_str(),
            "symbol": &args.symbol,
            "bucket_ms": args.bucket_ms,
            "label_horizon_buckets": args.label_horizon_buckets,
            "top_depth": args.top_depth,
            "segment_count": args.segment.len(),
            "reference_count": args.reference_data.len(),
        }),
    );
    let published = materialize(&args).inspect_err(|_| {
        log_event(
            "pit_materialization_failed",
            json!({"reason_code": "materialization_failed"}),
        );
    })?;
    log_event(
        "pit_materialization_completed",
        json!({
            "mission_id": &published.report.mission_id,
            "market": &published.report.market,
            "symbol": &published.report.symbol,
            "source_revision": &published.report.source_revision,
            "series_count": published.report.series_count,
            "rows": published.report.rows,
            "first_event_time": published.report.first_event_time,
            "last_event_time": published.report.last_event_time,
            "artifact_sha256": &published.report.artifact_sha256,
            "snapshot_sha256": &published.report.snapshot_sha256,
            "report_sha256": &published.report_sha256,
        }),
    );
    serde_json::to_writer_pretty(std::io::stdout().lock(), &published)?;
    println!();
    Ok(())
}

fn materialize(args: &Args) -> Result<PublishedMaterialization> {
    let mission_id = args.mission_id.trim();
    let symbol = args.symbol.trim().to_uppercase();
    if mission_id.is_empty() || symbol.is_empty() {
        bail!("mission id and symbol are required");
    }
    if args.bucket_ms == 0 || args.label_horizon_buckets == 0 || args.top_depth == 0 {
        bail!("bucket, label horizon, and top depth must be positive");
    }
    if args.market != Market::Usdm {
        bail!("credential-free canonical materialization currently supports USD-M only");
    }
    log_event(
        "segment_verification_started",
        json!({"segment_count": args.segment.len()}),
    );
    let (verified_series, segment_paths) = verify_segments(args)?;
    log_event(
        "segment_verification_completed",
        json!({
            "segment_count": args.segment.len(),
            "verified_series_count": verified_series.len(),
        }),
    );
    if verified_series.iter().any(|series| {
        series
            .verified()
            .segments()
            .iter()
            .any(|segment| segment.market != args.market.as_lob_market())
    }) {
        bail!("verified market-tape does not match requested market");
    }
    let source_segments = source_segment_evidence(&verified_series, segment_paths)?;
    let bucket_ns = args
        .bucket_ms
        .checked_mul(1_000_000)
        .context("bucket size overflow")?;
    let mut replay = Replay::new(bucket_ns, args.top_depth);
    let mut aggregate_trades = Vec::new();
    let mut has_aggregate_trades = None;
    log_event(
        "lob_replay_started",
        json!({"verified_series_count": verified_series.len(), "symbol": &symbol}),
    );
    for (series_index, series) in verified_series.iter().enumerate() {
        let verified = series.verified();
        let book = verified
            .replayed_books()
            .iter()
            .find(|book| book.symbol == symbol)
            .with_context(|| {
                format!(
                    "verified market-tape series {} does not contain requested symbol {symbol}",
                    series.session_id()
                )
            })?;
        let series_has_aggregate_trades = verified
            .segments()
            .iter()
            .any(|segment| segment.trade_summaries.contains_key(&symbol));
        if has_aggregate_trades
            .replace(series_has_aggregate_trades)
            .is_some_and(|previous| previous != series_has_aggregate_trades)
        {
            bail!("verified market-tape series mix LOB-only and aggregate-trade modalities");
        }
        aggregate_trades.extend(
            verified
                .aggregate_trades()
                .iter()
                .filter(|trade| trade.symbol == symbol)
                .cloned(),
        );
        replay.consume(book.events())?;
        log_event(
            "lob_replay_progress",
            json!({
                "completed_series": series_index + 1,
                "total_series": verified_series.len(),
                "session_id": series.session_id(),
                "book_events": book.events().len(),
                "samples": replay.samples.len(),
                "aggregate_trades": aggregate_trades.len(),
            }),
        );
    }
    let has_aggregate_trades = has_aggregate_trades.unwrap_or(false);
    log_event(
        "lob_replay_completed",
        json!({
            "series_count": verified_series.len(),
            "samples": replay.samples.len(),
            "aggregate_trades": aggregate_trades.len(),
            "has_aggregate_trades": has_aggregate_trades,
        }),
    );

    let revision = source_revision(&source_segments);
    let created_at = Utc::now();
    let ingestion_time = datetime_ns(
        source_segments
            .iter()
            .map(|segment| segment.end_received_at_ns)
            .max()
            .context("segments have no end time")?,
    )?;
    let source_revisions = BTreeMap::from([(
        format!("binance-{}-lob", args.market.as_str()),
        revision.clone(),
    )]);
    log_event(
        "pit_feature_rows_started",
        json!({
            "samples": replay.samples.len(),
            "aggregate_trades": aggregate_trades.len(),
            "has_aggregate_trades": has_aggregate_trades,
        }),
    );
    let rows = materialize_rows(
        &replay.samples,
        &aggregate_trades,
        has_aggregate_trades,
        args,
        &source_revisions,
        &symbol,
        ingestion_time,
    )?;
    log_event(
        "pit_feature_rows_completed",
        json!({"rows": rows.len(), "source_revision": &revision}),
    );
    let artifact_bytes = encode_rows(&rows)?;
    let artifact_sha256 = hex::encode(Sha256::digest(&artifact_bytes));
    let artifact_path = args.artifact_dir.join(format!("{artifact_sha256}.jsonl"));
    let first_event_time = rows.first().context("feature rows are empty")?.event_time;
    let last_event_time = rows.last().context("feature rows are empty")?.event_time;
    let label_available_through = last_event_time
        .checked_add_signed(
            chrono::TimeDelta::try_milliseconds(
                i64::try_from(
                    args.bucket_ms
                        .checked_mul(
                            u64::try_from(args.label_horizon_buckets)
                                .context("label horizon overflow")?,
                        )
                        .context("label horizon overflow")?,
                )
                .context("label horizon overflow")?,
            )
            .context("label horizon overflow")?,
        )
        .context("label availability time overflows")?;
    log_event(
        "reference_binding_started",
        json!({
            "reference_count": args.reference_data.len(),
            "first_event_time": first_event_time,
            "label_available_through": label_available_through,
        }),
    );
    let (instrument_rules, series) = bind_usdm_reference(
        args,
        &symbol,
        &rows,
        first_event_time,
        label_available_through,
    )?;
    log_event(
        "reference_binding_completed",
        json!({
            "reference_count": args.reference_data.len(),
            "replay_series_count": series.len(),
            "rules_available_at": instrument_rules.available_at,
            "rules_valid_through": instrument_rules.valid_through,
        }),
    );
    let snapshot = CexReplaySnapshotV5 {
        schema_version: CEX_REPLAY_SNAPSHOT_SCHEMA_V5.to_string(),
        venue: "binance".to_string(),
        instrument_type: args.market.as_str().to_string(),
        symbol: symbol.clone(),
        replay_clock: CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
        required_modalities: if has_aggregate_trades {
            BTreeSet::from([
                CEX_MODALITY_LOB.to_string(),
                CEX_MODALITY_AGGREGATE_TRADE.to_string(),
            ])
        } else {
            BTreeSet::from([CEX_MODALITY_LOB.to_string()])
        },
        source_segments: source_segments
            .iter()
            .map(|segment| CexReplaySegmentIdentity {
                content_sha256: segment.sha256.clone(),
                manifest_sha256: segment.collector_manifest_sha256.clone(),
                start_received_at_ns: segment.start_received_at_ns,
                end_received_at_ns: segment.end_received_at_ns,
                events: segment.events,
            })
            .collect(),
        first_event_time,
        last_event_time,
        feature_artifact_sha256: artifact_sha256.clone(),
        feature_availability_policy: CEX_FEATURE_AVAILABILITY_POLICY.to_string(),
        bucket_ms: args.bucket_ms,
        label_horizon_buckets: args.label_horizon_buckets,
        top_depth: args.top_depth,
        instrument_rules,
        series,
    };
    snapshot.validate().map_err(anyhow::Error::new)?;
    log_event(
        "pit_artifact_publish_started",
        json!({"artifact_sha256": &artifact_sha256, "rows": rows.len()}),
    );
    publish_immutable(&artifact_path, &artifact_bytes)?;
    log_event(
        "pit_artifact_publish_completed",
        json!({"artifact_sha256": &artifact_sha256}),
    );
    let snapshot_sha256 = snapshot.sha256();

    let report = MaterializationReport {
        dataset_kind: "lob_point_in_time_materialization".to_string(),
        schema_version: BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7.to_string(),
        mission_id: mission_id.to_string(),
        symbol,
        market: args.market.as_str().to_string(),
        bucket_ms: args.bucket_ms,
        label_horizon_buckets: args.label_horizon_buckets,
        top_depth: args.top_depth,
        source_revision: revision,
        source_segments,
        series_count: usize::try_from(replay.series_id).context("series count overflow")?,
        rows: rows.len(),
        first_event_time,
        last_event_time,
        artifact_path,
        artifact_sha256,
        snapshot,
        snapshot_sha256,
        created_at,
    };
    let report_bytes = serde_json::to_vec_pretty(&report)?;
    let report_sha256 = hex::encode(Sha256::digest(&report_bytes));
    let report_path = args
        .artifact_dir
        .join(format!("{report_sha256}.materialization.json"));
    log_event(
        "pit_report_publish_started",
        json!({"report_sha256": &report_sha256}),
    );
    publish_immutable(&report_path, &report_bytes)?;
    log_event(
        "pit_report_publish_completed",
        json!({"report_sha256": &report_sha256}),
    );
    Ok(PublishedMaterialization {
        report,
        report_path,
        report_sha256,
    })
}

fn verify_segments(
    args: &Args,
) -> Result<(
    Vec<VerifiedBinanceMarketTapeSeries>,
    BTreeMap<String, PathBuf>,
)> {
    let count = args.segment.len();
    if count == 0
        || args.segment_content_sha256.len() != count
        || args.segment_manifest_sha256.len() != count
    {
        bail!(
            "--segment, --segment-content-sha256, and --segment-manifest-sha256 must have equal nonzero lengths"
        );
    }
    let mut paths = BTreeMap::new();
    let mut sealed = Vec::with_capacity(count);
    for (index, ((path, content_sha256), manifest_sha256)) in args
        .segment
        .iter()
        .zip(&args.segment_content_sha256)
        .zip(&args.segment_manifest_sha256)
        .enumerate()
    {
        if paths.insert(content_sha256.clone(), path.clone()).is_some() {
            bail!("duplicate LOB segment supplied");
        }
        let triplet = BinanceMarketTapeTriplet {
            data: path.clone(),
            manifest: sibling(path, ".manifest.json")?,
            success: sibling(path, "._SUCCESS")?,
        };
        let trust = BinanceMarketTapeTrustAnchor::from_lower_hex(content_sha256, manifest_sha256)?;
        sealed.push(seal_binance_market_tape_triplet(&triplet, &trust)?);
        if (index + 1).is_multiple_of(10) || index + 1 == count {
            log_event(
                "segment_verification_progress",
                json!({
                    "completed_segments": index + 1,
                    "total_segments": count,
                    "content_sha256": content_sha256,
                    "manifest_sha256": manifest_sha256,
                }),
            );
        }
    }
    Ok((
        verify_binance_market_tape_series_for_strict_gate(sealed)?,
        paths,
    ))
}

/// Re-publish a verified evidence triplet into the materialization artifact
/// directory under content-addressed names so the digests recorded in the
/// snapshot resolve to immutable local files.
#[allow(clippy::too_many_arguments)]
fn republish_evidence_triplet(
    artifact_dir: &Path,
    kind: &str,
    data_path: &Path,
    manifest_path: &Path,
    success_path: &Path,
    data_sha256: &str,
    manifest_sha256: &str,
) -> Result<()> {
    republish_verified_file(
        data_path,
        &artifact_dir.join(format!("{data_sha256}.{kind}.data")),
        data_sha256,
    )?;
    republish_verified_file(
        manifest_path,
        &artifact_dir.join(format!("{manifest_sha256}.{kind}.manifest.json")),
        manifest_sha256,
    )?;
    let success = std::fs::read(success_path)
        .with_context(|| format!("failed to read {kind} success marker"))?;
    if success != format!("{data_sha256}\n").as_bytes() {
        bail!("verified {kind} evidence success marker changed before publication");
    }
    publish_immutable(
        &artifact_dir.join(format!("{data_sha256}.{kind}._SUCCESS")),
        &success,
    )
}

fn republish_verified_file(source: &Path, target: &Path, expected_sha256: &str) -> Result<()> {
    let bytes = std::fs::read(source)
        .with_context(|| format!("failed to read verified evidence {}", source.display()))?;
    if hex::encode(Sha256::digest(&bytes)) != expected_sha256 {
        bail!("verified evidence changed before publication");
    }
    publish_immutable(target, &bytes)
}

fn bind_usdm_reference(
    args: &Args,
    symbol: &str,
    rows: &[PointInTimeFeatureRow],
    first_event_time: DateTime<Utc>,
    label_available_through: DateTime<Utc>,
) -> Result<(CexInstrumentRulesV2, Vec<CexReplaySeriesV1>)> {
    let count = args.reference_data.len();
    if count == 0
        || args.reference_data_sha256.len() != count
        || args.reference_manifest_sha256.len() != count
    {
        bail!("USD-M reference data and digest arguments must have equal nonzero lengths");
    }
    let mut rules = None;
    let mut rule_times = Vec::with_capacity(count);
    let mut evidence = Vec::with_capacity(count);
    let mut observations = Vec::with_capacity(count);
    for (index, ((data_path, data_sha256), manifest_sha256)) in args
        .reference_data
        .iter()
        .zip(&args.reference_data_sha256)
        .zip(&args.reference_manifest_sha256)
        .enumerate()
    {
        let published = PublishedReferenceArtifact {
            data_path: data_path.clone(),
            manifest_path: sibling(data_path, ".manifest.json")?,
            success_path: sibling(data_path, "._SUCCESS")?,
            data_sha256: data_sha256.clone(),
            manifest_sha256: manifest_sha256.clone(),
        };
        let batch = verify_reference_artifact_read_only_current_batch(
            &published,
            data_sha256,
            manifest_sha256,
        )?;
        let triplet = reference_artifact_triplet(&published)?;
        let contract = batch
            .contracts()
            .iter()
            .find(|row| row.symbol == symbol)
            .with_context(|| format!("reference artifact has no contract {symbol}"))?;
        let candidate = (
            contract.tick_size,
            contract.step_size,
            contract.min_notional,
        );
        if rules.is_some_and(|existing| existing != candidate) {
            bail!("instrument rules changed inside the requested PIT window");
        }
        republish_evidence_triplet(
            &args.artifact_dir,
            "reference",
            &published.data_path,
            &published.manifest_path,
            &published.success_path,
            &published.data_sha256,
            &published.manifest_sha256,
        )?;
        rules = Some(candidate);
        let available_at = datetime_ns(contract.received_at_ns)?;
        rule_times.push(available_at);
        push_unique_observation(&mut observations, available_at, &triplet);
        push_unique_triplet(&mut evidence, &triplet);
        if (index + 1).is_multiple_of(50) || index + 1 == count {
            log_event(
                "reference_binding_progress",
                json!({
                    "completed_references": index + 1,
                    "total_references": count,
                    "data_sha256": data_sha256,
                    "manifest_sha256": manifest_sha256,
                    "available_at": available_at,
                }),
            );
        }
    }
    rule_times.sort_unstable();
    observations.sort_by_key(|observation| observation.available_at);
    let (tick_size, step_size, min_notional) = rules.context("USD-M rules are missing")?;
    let instrument_rules = CexInstrumentRulesV2 {
        tick_size: tick_size.to_string(),
        step_size: step_size.to_string(),
        min_notional: min_notional.to_string(),
        available_at: *rule_times
            .first()
            .context("USD-M rules evidence is empty")?,
        valid_through: *rule_times.last().unwrap(),
        evidence,
    };
    if instrument_rules.available_at > first_event_time
        || instrument_rules.valid_through < label_available_through
    {
        bail!("PIT instrument-rule coverage does not span the replay window");
    }
    let series = replay_series_coverages(
        rows,
        &observations,
        args.label_horizon_buckets,
        args.bucket_ms,
    )?;
    Ok((instrument_rules, series))
}

fn reference_artifact_triplet(
    artifact: &PublishedReferenceArtifact,
) -> Result<CexArtifactTripletV2> {
    Ok(CexArtifactTripletV2 {
        data_sha256: artifact.data_sha256.clone(),
        manifest_sha256: artifact.manifest_sha256.clone(),
        success_sha256: artifact.data_sha256.clone(),
    })
}

fn push_unique_triplet(evidence: &mut Vec<CexArtifactTripletV2>, triplet: &CexArtifactTripletV2) {
    if !evidence.iter().any(|existing| existing == triplet) {
        evidence.push(triplet.clone());
    }
}

fn push_unique_observation(
    observations: &mut Vec<RuleObservation>,
    available_at: DateTime<Utc>,
    triplet: &CexArtifactTripletV2,
) {
    if observations
        .iter()
        .any(|existing| existing.available_at == available_at)
    {
        return;
    }
    observations.push(RuleObservation {
        available_at,
        triplet: triplet.clone(),
    });
}

fn replay_series_coverages(
    rows: &[PointInTimeFeatureRow],
    observations: &[RuleObservation],
    label_horizon_buckets: usize,
    bucket_ms: u64,
) -> Result<Vec<CexReplaySeriesV1>> {
    let mut by_series = BTreeMap::<u64, (DateTime<Utc>, DateTime<Utc>)>::new();
    for row in rows {
        by_series
            .entry(row.series_id)
            .and_modify(|window| window.1 = row.event_time)
            .or_insert((row.event_time, row.event_time));
    }
    let horizon = chrono::TimeDelta::try_milliseconds(
        i64::try_from(
            bucket_ms
                .checked_mul(
                    u64::try_from(label_horizon_buckets).context("label horizon overflow")?,
                )
                .context("label horizon overflow")?,
        )
        .context("label horizon overflow")?,
    )
    .context("label horizon overflow")?;
    by_series
        .into_iter()
        .map(|(series_id, (first_event_time, last_event_time))| {
            let required_through = last_event_time
                .checked_add_signed(horizon)
                .context("label availability time overflows")?;
            let coverage =
                select_series_rule_coverage(observations, first_event_time, required_through)?;
            Ok(CexReplaySeriesV1 {
                series_id: u32::try_from(series_id).context("series id overflow")?,
                first_event_time,
                last_event_time,
                instrument_rules_coverage: coverage,
            })
        })
        .collect()
}

fn select_series_rule_coverage(
    observations: &[RuleObservation],
    first_event_time: DateTime<Utc>,
    required_through: DateTime<Utc>,
) -> Result<CexPitSeriesEvidenceV2> {
    let before = observations
        .iter()
        .rev()
        .find(|observation| observation.available_at <= first_event_time);
    let after = observations
        .iter()
        .find(|observation| observation.available_at >= required_through);
    let (Some(before), Some(after)) = (before, after) else {
        bail!("PIT instrument-rule coverage does not span the replay window");
    };
    let selected = observations
        .iter()
        .filter(|observation| {
            observation.available_at >= before.available_at
                && observation.available_at <= after.available_at
        })
        .cloned()
        .collect::<Vec<_>>();
    let max_gap_ns = max_observation_gap_ns(&selected)?;
    if max_gap_ns > CEX_DERIVATIVES_MAX_GAP_NS {
        bail!("PIT instrument-rule coverage has a gap above 90s");
    }
    Ok(CexPitSeriesEvidenceV2 {
        evidence: selected
            .iter()
            .map(|observation| observation.triplet.clone())
            .collect(),
        first_available_at: selected.first().expect("selected not empty").available_at,
        last_available_at: selected.last().expect("selected not empty").available_at,
        observations: u64::try_from(selected.len()).context("coverage observation overflow")?,
        max_gap_ns,
    })
}

fn max_observation_gap_ns(observations: &[RuleObservation]) -> Result<u64> {
    let mut max_gap_ns = 0;
    for pair in observations.windows(2) {
        let gap_ns = pair[1]
            .available_at
            .signed_duration_since(pair[0].available_at)
            .num_nanoseconds()
            .context("PIT availability gap is outside i64 nanoseconds")?;
        max_gap_ns =
            max_gap_ns.max(u64::try_from(gap_ns).context("PIT availability is not ordered")?);
    }
    Ok(max_gap_ns)
}

fn source_segment_evidence(
    verified_series: &[VerifiedBinanceMarketTapeSeries],
    mut paths: BTreeMap<String, PathBuf>,
) -> Result<Vec<SourceSegmentEvidence>> {
    verified_series
        .iter()
        .flat_map(|series| series.verified().segments().iter())
        .map(|segment| {
            let path = paths
                .remove(&segment.content_sha256)
                .context("verified segment has no matching CLI path")?;
            Ok(SourceSegmentEvidence {
                collector_manifest_path: sibling(&path, ".manifest.json")?,
                success_marker_path: sibling(&path, "._SUCCESS")?,
                success_marker_sha256: hex::encode(Sha256::digest(format!(
                    "{}\n",
                    segment.content_sha256
                ))),
                path,
                sha256: segment.content_sha256.clone(),
                collector_manifest_sha256: segment.manifest_sha256.clone(),
                start_received_at_ns: segment.start_received_at_ns,
                end_received_at_ns: segment.end_received_at_ns,
                events: segment.events,
            })
        })
        .collect()
}

fn parse_levels(levels: &[[String; 2]], side: &str) -> Result<BTreeMap<Decimal, Decimal>> {
    let mut parsed = BTreeMap::new();
    for (price, quantity) in validated_levels(levels, side)? {
        if !quantity.is_zero() {
            parsed.insert(price, quantity);
        }
    }
    Ok(parsed)
}

fn apply_levels(
    book: &mut BTreeMap<Decimal, Decimal>,
    levels: &[[String; 2]],
    side: &str,
) -> Result<()> {
    for (price, quantity) in validated_levels(levels, side)? {
        if quantity.is_zero() {
            book.remove(&price);
        } else {
            book.insert(price, quantity);
        }
    }
    Ok(())
}

fn validated_levels(levels: &[[String; 2]], side: &str) -> Result<Vec<(Decimal, Decimal)>> {
    levels
        .iter()
        .map(|[price, quantity]| {
            let price = price
                .parse::<Decimal>()
                .with_context(|| format!("invalid decimal in {side} levels"))?;
            let quantity = quantity
                .parse::<Decimal>()
                .with_context(|| format!("invalid decimal in {side} levels"))?;
            if price <= Decimal::ZERO || quantity < Decimal::ZERO {
                bail!("invalid {side} price or quantity");
            }
            Ok((price, quantity))
        })
        .collect()
}

fn sample_book(
    state: &BookState,
    series_id: u64,
    time_ns: u64,
    depth: usize,
) -> Result<BookSample> {
    let bid_levels = state.bids.iter().rev().take(depth).collect::<Vec<_>>();
    let ask_levels = state.asks.iter().take(depth).collect::<Vec<_>>();
    let top5 = if depth == TOP5_DEPTH {
        Some(
            top5_book_features(
                state.bids.iter().rev().map(|(price, quantity)| {
                    (
                        price.to_f64().unwrap_or(f64::NAN),
                        quantity.to_f64().unwrap_or(f64::NAN),
                    )
                }),
                state.asks.iter().map(|(price, quantity)| {
                    (
                        price.to_f64().unwrap_or(f64::NAN),
                        quantity.to_f64().unwrap_or(f64::NAN),
                    )
                }),
            )
            .context("order book needs five valid positive levels per side")?,
        )
    } else {
        None
    };
    let (best_bid, best_bid_quantity) = bid_levels.first().context("order book has no bids")?;
    let (best_ask, best_ask_quantity) = ask_levels.first().context("order book has no asks")?;
    if best_bid >= best_ask {
        bail!("replayed order book is crossed");
    }
    let mid = (**best_bid + **best_ask) / Decimal::TWO;
    let bid_depth = bid_levels
        .iter()
        .fold(Decimal::ZERO, |total, (_, quantity)| total + **quantity);
    let ask_depth = ask_levels
        .iter()
        .fold(Decimal::ZERO, |total, (_, quantity)| total + **quantity);
    let total_depth = bid_depth + ask_depth;
    if mid <= Decimal::ZERO || total_depth <= Decimal::ZERO {
        bail!("replayed order book has invalid depth");
    }
    let (bid_depth, ask_depth, top_depth_imbalance) = match top5 {
        Some(top5) => (top5.bid_depth, top5.ask_depth, top5.book_imbalance),
        None => (
            decimal_f64(bid_depth)?,
            decimal_f64(ask_depth)?,
            decimal_f64((bid_depth - ask_depth) / total_depth)?,
        ),
    };
    Ok(BookSample {
        series_id,
        time_ns,
        mid_price: decimal_f64(mid)?,
        spread_bps: decimal_f64((**best_ask - **best_bid) / mid * Decimal::from(10_000))?,
        bid_depth,
        ask_depth,
        top_depth_imbalance,
        weighted_book_imbalance_top5: top5.map(|features| features.weighted_book_imbalance),
        near_depth_concentration_skew_top5: top5
            .map(|features| features.near_depth_concentration_skew),
        vwap_center_deviation_top5_bps: top5.map(|features| features.vwap_center_deviation_bps),
        book_imbalance: {
            let bid_size = decimal_f64(**best_bid_quantity)?;
            let ask_size = decimal_f64(**best_ask_quantity)?;
            (bid_size - ask_size) / (bid_size + ask_size)
        },
    })
}

fn materialize_rows(
    samples: &[BookSample],
    aggregate_trades: &[AggregateTrade],
    has_aggregate_trades: bool,
    args: &Args,
    source_revisions: &BTreeMap<String, String>,
    symbol: &str,
    ingestion_time: DateTime<Utc>,
) -> Result<Vec<PointInTimeFeatureRow>> {
    let mut rows = Vec::new();
    let aggregate_trades = aggregate_trades
        .iter()
        .filter(|trade| trade.symbol == symbol)
        .collect::<Vec<_>>();
    for index in 1..samples.len().saturating_sub(args.label_horizon_buckets) {
        if index.is_multiple_of(100_000) {
            log_event(
                "pit_feature_rows_progress",
                json!({
                    "processed_samples": index,
                    "total_samples": samples.len(),
                    "materialized_rows": rows.len(),
                }),
            );
        }
        let previous = &samples[index - 1];
        let current = &samples[index];
        let future = &samples[index + args.label_horizon_buckets];
        if previous.series_id != current.series_id || current.series_id != future.series_id {
            continue;
        }
        let previous_total = previous.bid_depth + previous.ask_depth;
        if previous.mid_price <= 0.0 || current.mid_price <= 0.0 || previous_total <= 0.0 {
            continue;
        }
        let label = future.mid_price / current.mid_price - 1.0;
        let event_time = datetime_ns(current.time_ns)?;
        let future_time = datetime_ns(future.time_ns)?;
        let trade_start =
            aggregate_trades.partition_point(|trade| trade.received_at_ns <= previous.time_ns);
        let trade_end =
            aggregate_trades.partition_point(|trade| trade.received_at_ns <= current.time_ns);
        let bucket_trades = &aggregate_trades[trade_start..trade_end];
        let (base_volume, quote_volume, signed_volume) = bucket_trades.iter().try_fold(
            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
            |(base, quote, signed), trade| {
                let quote_delta = trade
                    .price
                    .checked_mul(trade.quantity)
                    .context("aggregate-trade notional overflow")?;
                let signed_delta = if trade.is_buyer_maker {
                    -trade.quantity
                } else {
                    trade.quantity
                };
                Ok::<_, anyhow::Error>((
                    base + trade.quantity,
                    quote + quote_delta,
                    signed + signed_delta,
                ))
            },
        )?;
        let mut features = BTreeMap::from([
            (
                "aggregate_trade_base_volume".to_string(),
                decimal_f64(base_volume)?,
            ),
            (
                "aggregate_trade_count".to_string(),
                bucket_trades.len() as f64,
            ),
            (
                "aggregate_trade_flow_imbalance".to_string(),
                if base_volume.is_zero() {
                    0.0
                } else {
                    decimal_f64(signed_volume / base_volume)?
                },
            ),
            (
                "aggregate_trade_quote_volume".to_string(),
                decimal_f64(quote_volume)?,
            ),
            (
                format!("ask_depth_top{}", args.top_depth),
                current.ask_depth,
            ),
            (
                format!("bid_depth_top{}", args.top_depth),
                current.bid_depth,
            ),
            ("book_imbalance".to_string(), current.book_imbalance),
            (
                format!("book_imbalance_top{}", args.top_depth),
                current.top_depth_imbalance,
            ),
            ("mid_price".to_string(), current.mid_price),
            (
                "mid_return_1".to_string(),
                current.mid_price / previous.mid_price - 1.0,
            ),
            (
                format!("ofi_top{}", args.top_depth),
                ((current.bid_depth - previous.bid_depth)
                    - (current.ask_depth - previous.ask_depth))
                    / previous_total,
            ),
            ("spread_bps".to_string(), current.spread_bps),
        ]);
        if !has_aggregate_trades {
            for field in [
                "aggregate_trade_base_volume",
                "aggregate_trade_count",
                "aggregate_trade_flow_imbalance",
                "aggregate_trade_quote_volume",
            ] {
                features.remove(field);
            }
        }
        if let Some(value) = current.weighted_book_imbalance_top5 {
            features.insert("weighted_book_imbalance_top5".to_string(), value);
        }
        if let Some(value) = current.near_depth_concentration_skew_top5 {
            features.insert("near_depth_concentration_skew_top5".to_string(), value);
        }
        if let Some(value) = current.vwap_center_deviation_top5_bps {
            features.insert("vwap_center_deviation_top5_bps".to_string(), value);
        }
        let modalities = if has_aggregate_trades {
            BTreeSet::from([DataModality::Lob, DataModality::TradeTick])
        } else {
            BTreeSet::from([DataModality::Lob])
        };
        if !label.is_finite() || features.values().any(|value| !value.is_finite()) {
            bail!("materialized feature or label is not finite");
        }
        rows.push(PointInTimeFeatureRow {
            series_id: current.series_id,
            event_time,
            feature_available_time: event_time,
            label_available_time: future_time,
            ingestion_time,
            symbol: symbol.to_string(),
            source_revisions: source_revisions.clone(),
            modalities,
            features,
            label,
        });
    }
    if rows.len() < 3 {
        bail!("materialization produced fewer than three PIT rows");
    }
    Ok(rows)
}

fn encode_rows(rows: &[PointInTimeFeatureRow]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn publish_immutable(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        if sha256_file(path)? != hex::encode(Sha256::digest(bytes)) {
            bail!(
                "immutable artifact already exists with different content: {}",
                path.display()
            );
        }
        return Ok(());
    }
    let file_name = file_name(path)?;
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    drop(output);
    match std::fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) if path.exists() => {
            let _ = std::fs::remove_file(&temporary);
            if sha256_file(path)? == hex::encode(Sha256::digest(bytes)) {
                Ok(())
            } else {
                Err(error.into())
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn source_revision(segments: &[SourceSegmentEvidence]) -> String {
    governed_source_revision(segments.iter().map(|segment| segment.sha256.as_str()))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut source = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn sibling(path: &Path, suffix: &str) -> Result<PathBuf> {
    Ok(path.with_file_name(format!("{}{suffix}", file_name(path)?)))
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .context("path has no UTF-8 file name")
}

fn decimal_f64(value: Decimal) -> Result<f64> {
    value
        .to_f64()
        .context("decimal cannot be represented as f64")
}

fn ceil_bucket(value: u64, bucket: u64) -> Result<u64> {
    value
        .checked_add(bucket - 1)
        .map(|adjusted| adjusted / bucket * bucket)
        .context("bucket time overflow")
}

fn datetime_ns(value: u64) -> Result<DateTime<Utc>> {
    let seconds = i64::try_from(value / 1_000_000_000).context("timestamp seconds overflow")?;
    let nanoseconds = u32::try_from(value % 1_000_000_000).expect("nanoseconds fit u32");
    DateTime::from_timestamp(seconds, nanoseconds).context("timestamp is out of range")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use data::binance_market_tape::{
        AggregateTrade, AggregateTradeSummaryBuilder, LobContinuitySummaryBuilder,
        AGGREGATE_TRADE_SUMMARY_CONTRACT, MARKET_TAPE_SCHEMA_V2,
    };
    use data::binance_usdm_reference::{
        ActivePerpetualContract, CompleteReferenceBatch, MarkIndexFundingObservation,
        OpenInterestObservation, EXCHANGE_INFO_ENDPOINT, OPEN_INTEREST_ENDPOINT,
        PREMIUM_INDEX_ENDPOINT, REFERENCE_SCHEMA, SERVER_TIME_ENDPOINT,
    };
    use hft_collector::binance_usdm_reference_artifact::{
        publish_reference_batch, ReferenceArtifactConfig,
    };
    use hft_collector::binance_usdm_reference_collector::OFFICIAL_USDM_SOURCE_ORIGIN;
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader},
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    const START_NS: u64 = 1_783_987_200_000_000_000;
    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    fn event_ns(milliseconds: u64) -> u64 {
        START_NS + milliseconds * 1_000_000
    }

    fn level(price: &str, quantity: &str) -> [String; 2] {
        [price.to_string(), quantity.to_string()]
    }

    #[rustfmt::skip]
    fn reference_batch(milliseconds: u64, next_funding_ms: u64, funding: i64, oi: i64) -> CompleteReferenceBatch {
        let received_at_ns = event_ns(milliseconds);
        let source_time_ms = received_at_ns / 1_000_000;
        CompleteReferenceBatch::new(
            vec![ActivePerpetualContract {
                schema: REFERENCE_SCHEMA.to_string(), symbol: "BTCUSDT".to_string(), pair: "BTCUSDT".to_string(),
                base_asset: "BTC".to_string(), quote_asset: "USDT".to_string(), margin_asset: "USDT".to_string(),
                tick_size: Decimal::new(1, 1), step_size: Decimal::new(1, 3), min_notional: Decimal::new(5, 0),
                contract_type: "PERPETUAL".to_string(), status: "TRADING".to_string(),
                onboard_date_ms: 1, delivery_date_ms: u64::MAX, source_time_ms,
                source_clock_received_at_ns: received_at_ns, received_at_ns,
                source_endpoint: EXCHANGE_INFO_ENDPOINT.to_string(), source_clock_endpoint: SERVER_TIME_ENDPOINT.to_string(),
            }],
            vec![MarkIndexFundingObservation {
                schema: REFERENCE_SCHEMA.to_string(), symbol: "BTCUSDT".to_string(),
                mark_price: Decimal::new(101, 0), index_price: Decimal::new(100, 0),
                basis: Decimal::ONE, basis_rate: Decimal::new(1, 2),
                last_funding_rate: Decimal::new(funding, 4), interest_rate: Decimal::new(1, 4),
                next_funding_time_ms: event_ns(next_funding_ms) / 1_000_000,
                source_time_ms, received_at_ns,
                source_endpoint: PREMIUM_INDEX_ENDPOINT.to_string(),
            }],
            vec![OpenInterestObservation {
                schema: REFERENCE_SCHEMA.to_string(), symbol: "BTCUSDT".to_string(),
                open_interest: Decimal::new(oi, 1), source_time_ms, received_at_ns,
                source_endpoint: OPEN_INTEREST_ENDPOINT.to_string(),
            }],
        )
        .unwrap()
    }

    #[rustfmt::skip]
    fn diff(
        milliseconds: u64,
        first: u64,
        final_id: u64,
        previous: u64,
        bids: Value,
        asks: Value,
    ) -> Value {
        let received_at_ns = event_ns(milliseconds);
        json!({
            "schema": "binance.market_tape.v1",
            "received_at_ns": received_at_ns,
            "type": "diff",
            "session_id": "session-1",
            "frame": {"stream": "btcusdt@depth@100ms", "data": {
                "e": "depthUpdate",
                "E": received_at_ns / 1_000_000,
                "T": received_at_ns / 1_000_000,
                "s": "BTCUSDT",
                "U": first,
                "u": final_id,
                "pu": previous,
                "b": bids,
                "a": asks
            }}
        })
    }

    #[rustfmt::skip]
    fn trade(milliseconds: u64, id: u64) -> Value {
        let received_at_ns = event_ns(milliseconds);
        json!({
            "schema": "binance.market_tape.v1",
            "received_at_ns": received_at_ns,
            "type": "agg_trade",
            "session_id": "session-1",
            "frame": {"stream": "btcusdt@aggTrade", "data": {
                "e": "aggTrade",
                "E": received_at_ns / 1_000_000,
                "s": "BTCUSDT",
                "a": id,
                "p": "100.5",
                "q": "2",
                "f": id,
                "l": id,
                "T": received_at_ns / 1_000_000,
                "m": false
            }}
        })
    }

    #[rustfmt::skip]
    fn valid_rows(market: &str) -> Vec<Value> {
        vec![
            json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(0),"type":"session_start","session_id":"session-1","market":market,"symbols":1,"websocket_shards":2,"websocket_streams":2}),
            json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(1),"type":"stream_coverage","session_id":"session-1","shards":[["btcusdt@aggTrade"],["btcusdt@depth@100ms"]]}),
            json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(100),"type":"snapshot","session_id":"session-1","symbol":"BTCUSDT","request_started_at_ns":event_ns(50),"snapshot":{"lastUpdateId":100,"bids":[["100","10"],["99","5"],["98","4"],["97","3"],["96","2"]],"asks":[["102","4"],["103","6"],["104","5"],["105","4"],["106","3"]]}}),
            diff(600, 101, 175, 100, json!([["100", "10"]]), json!([["101", "8"]])),
            trade(700, 10),
            diff(1_400, 176, 176, 175, json!([]), json!([["101", "0"]])),
            trade(1_700, 11),
            diff(2_400, 177, 177, 176, json!([["101", "3"]]), json!([])),
            diff(3_400, 178, 178, 177, json!([]), json!([["101.5", "4"]])),
            diff(4_400, 179, 179, 178, json!([["101", "0"]]), json!([])),
            diff(5_400, 180, 180, 179, json!([["100", "12"]]), json!([])),
            diff(6_400, 181, 181, 180, json!([]), json!([["101.5", "5"]])),
            json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(6_500),"type":"checkpoint","session_id":"session-1","symbol":"BTCUSDT","last_update_id":181,"synced":true,"bridged":true,"continuity_complete":true,"stream_coverage_verified":true,"bids":[["100","12"],["99","5"],["98","4"],["97","3"],["96","2"]],"asks":[["101.5","5"],["102","4"],["103","6"],["104","5"],["105","4"],["106","3"]],"reason":"test","replay_safe":true}),
        ]
    }

    fn lob_only_rows(market: &str) -> Vec<Value> {
        let mut rows = valid_rows(market)
            .into_iter()
            .filter(|row| row["type"] != "agg_trade")
            .collect::<Vec<_>>();
        for row in &mut rows {
            row["schema"] = json!(MARKET_TAPE_SCHEMA_V2);
        }
        let session = rows
            .iter_mut()
            .find(|row| row["type"] == "session_start")
            .unwrap();
        session["websocket_shards"] = json!(1);
        session["websocket_streams"] = json!(1);
        session["stream_types"] = json!(["depth@100ms"]);
        let coverage = rows
            .iter_mut()
            .find(|row| row["type"] == "stream_coverage")
            .unwrap();
        coverage["shards"] = json!([["btcusdt@depth@100ms"]]);
        rows
    }

    fn shift_rows(rows: &mut [Value], delta_ns: u64) {
        for row in rows {
            let object = row.as_object_mut().unwrap();
            let received_at_ns = object
                .get("received_at_ns")
                .and_then(Value::as_u64)
                .unwrap();
            object.insert(
                "received_at_ns".to_string(),
                json!(received_at_ns + delta_ns),
            );
            if let Some(request_started_at_ns) =
                object.get("request_started_at_ns").and_then(Value::as_u64)
            {
                object.insert(
                    "request_started_at_ns".to_string(),
                    json!(request_started_at_ns + delta_ns),
                );
            }
            if let Some(frame) = object.get_mut("frame").and_then(Value::as_object_mut) {
                if let Some(data) = frame.get_mut("data").and_then(Value::as_object_mut) {
                    for key in ["E", "T"] {
                        if let Some(value) = data.get(key).and_then(Value::as_u64) {
                            data.insert(key.to_string(), json!(value + delta_ns / 1_000_000));
                        }
                    }
                }
            }
        }
    }

    fn retag_session(rows: &mut [Value], session_id: &str) {
        for row in rows {
            row["session_id"] = json!(session_id);
        }
    }

    fn publish_reference_at_ns(
        output_root: &Path,
        observed_at_ns: u64,
        next_funding_at_ns: u64,
        funding: i64,
        oi: i64,
    ) -> PublishedReferenceArtifact {
        let source_time_ms = observed_at_ns / 1_000_000;
        publish_reference_batch(
            &ReferenceArtifactConfig {
                output_root: output_root.to_path_buf(),
                observed_at_ns,
                max_staleness_ms: 1_000,
            },
            OFFICIAL_USDM_SOURCE_ORIGIN,
            &CompleteReferenceBatch::new(
                vec![ActivePerpetualContract {
                    schema: REFERENCE_SCHEMA.to_string(),
                    symbol: "BTCUSDT".to_string(),
                    pair: "BTCUSDT".to_string(),
                    base_asset: "BTC".to_string(),
                    quote_asset: "USDT".to_string(),
                    margin_asset: "USDT".to_string(),
                    tick_size: Decimal::new(1, 1),
                    step_size: Decimal::new(1, 3),
                    min_notional: Decimal::new(5, 0),
                    contract_type: "PERPETUAL".to_string(),
                    status: "TRADING".to_string(),
                    onboard_date_ms: 1,
                    delivery_date_ms: u64::MAX,
                    source_time_ms,
                    source_clock_received_at_ns: observed_at_ns,
                    received_at_ns: observed_at_ns,
                    source_endpoint: EXCHANGE_INFO_ENDPOINT.to_string(),
                    source_clock_endpoint: SERVER_TIME_ENDPOINT.to_string(),
                }],
                vec![MarkIndexFundingObservation {
                    schema: REFERENCE_SCHEMA.to_string(),
                    symbol: "BTCUSDT".to_string(),
                    mark_price: Decimal::new(101, 0),
                    index_price: Decimal::new(100, 0),
                    basis: Decimal::ONE,
                    basis_rate: Decimal::new(1, 2),
                    last_funding_rate: Decimal::new(funding, 4),
                    interest_rate: Decimal::new(1, 4),
                    next_funding_time_ms: next_funding_at_ns / 1_000_000,
                    source_time_ms,
                    received_at_ns: observed_at_ns,
                    source_endpoint: PREMIUM_INDEX_ENDPOINT.to_string(),
                }],
                vec![OpenInterestObservation {
                    schema: REFERENCE_SCHEMA.to_string(),
                    symbol: "BTCUSDT".to_string(),
                    open_interest: Decimal::new(oi, 1),
                    source_time_ms,
                    received_at_ns: observed_at_ns,
                    source_endpoint: OPEN_INTEREST_ENDPOINT.to_string(),
                }],
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn rewrite_reference_manifest_v1(published: &mut PublishedReferenceArtifact) {
        let data = std::fs::read(&published.data_path).unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&published.manifest_path).unwrap()).unwrap();
        manifest["schema"] = json!("binance.usdm_reference_manifest.v1");
        let mut min_source_time_ms = u64::MAX;
        let mut max_source_time_ms = 0_u64;
        let mut min_received_at_ns = u64::MAX;
        let mut max_received_at_ns = 0_u64;
        for line in data.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let row: serde_json::Value = serde_json::from_slice(line).unwrap();
            let observation = &row["observation"];
            let source_time_ms = observation["source_time_ms"].as_u64().unwrap();
            min_source_time_ms = min_source_time_ms.min(source_time_ms);
            max_source_time_ms = max_source_time_ms.max(source_time_ms);
            let received_at_ns = observation["received_at_ns"].as_u64().unwrap();
            min_received_at_ns = min_received_at_ns.min(received_at_ns);
            max_received_at_ns = max_received_at_ns.max(received_at_ns);
            if row["kind"] == "metadata" {
                let clock_received_at_ns =
                    observation["source_clock_received_at_ns"].as_u64().unwrap();
                min_received_at_ns = min_received_at_ns.min(clock_received_at_ns);
                max_received_at_ns = max_received_at_ns.max(clock_received_at_ns);
            }
        }
        let object = manifest.as_object_mut().unwrap();
        object.remove("mark_index_funding");
        object.remove("open_interest");
        object.insert(
            "time_bounds".to_string(),
            json!({
                "min_source_time_ms": min_source_time_ms,
                "max_source_time_ms": max_source_time_ms,
                "min_received_at_ns": min_received_at_ns,
                "max_received_at_ns": max_received_at_ns,
            }),
        );
        let mut manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        manifest_bytes.push(b'\n');
        std::fs::write(&published.manifest_path, &manifest_bytes).unwrap();
        published.manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
    }

    struct Fixture {
        directory: PathBuf,
        market: Market,
        data: PathBuf,
        manifest: PathBuf,
        success: PathBuf,
        content_sha256: String,
        manifest_sha256: String,
        references: Vec<PublishedReferenceArtifact>,
    }

    impl Fixture {
        fn new(market: Market, rows: &[Value]) -> Self {
            let id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let requested_directory = std::env::temp_dir()
                .join(format!("lob-pit-materializer-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&requested_directory).unwrap();
            let directory = std::fs::canonicalize(requested_directory).unwrap();
            let raw = directory.join("part-1.jsonl");
            let data = directory.join("part-1.jsonl.zst");
            let mut raw_file = File::create(&raw).unwrap();
            for row in rows {
                serde_json::to_writer(&mut raw_file, row).unwrap();
                raw_file.write_all(b"\n").unwrap();
            }
            assert!(Command::new("zstd")
                .args(["-q", "-f"])
                .arg(&raw)
                .arg("-o")
                .arg(&data)
                .status()
                .unwrap()
                .success());
            std::fs::remove_file(raw).unwrap();

            let content_sha256 = sha256_file(&data).unwrap();
            let event_types = rows.iter().fold(BTreeMap::new(), |mut counts, row| {
                *counts
                    .entry(row["type"].as_str().unwrap().to_string())
                    .or_insert(0_u64) += 1;
                counts
            });
            let mut trade_summaries = AggregateTradeSummaryBuilder::default();
            let mut lob_continuity =
                LobContinuitySummaryBuilder::new(["BTCUSDT".to_string()]).unwrap();
            for row in rows {
                let raw = row.as_object().unwrap();
                lob_continuity.observe(raw).unwrap();
                if row["type"] == "agg_trade" {
                    let received_at_ns = row["received_at_ns"].as_u64().unwrap();
                    trade_summaries
                        .observe(&AggregateTrade::from_archived_event(raw, received_at_ns).unwrap())
                        .unwrap();
                }
            }
            let declared_symbols = BTreeSet::from(["BTCUSDT".to_string()]);
            let checkpointed_symbols = rows
                .iter()
                .filter(|row| row["type"] == "checkpoint")
                .filter_map(|row| row["symbol"].as_str())
                .map(str::to_string)
                .collect::<BTreeSet<_>>();
            let schema = rows[0]["schema"].as_str().unwrap();
            let lob_only = schema == MARKET_TAPE_SCHEMA_V2
                && !rows.iter().any(|row| row["type"] == "agg_trade");
            let manifest_value = json!({
                "schema": schema,
                "venue": "binance",
                "market": market.as_str(),
                "dataset": if lob_only { "usdm_perpetual_top100_lob".to_string() } else { format!("{}_all", market.as_str()) },
                "shard_id": "all",
                "mode": "diff",
                "symbols": ["BTCUSDT"],
                "security_token_symbols": [],
                "excluded_symbols": [],
                "snapshot_limit": 1_000,
                "replay_scope": if lob_only { "captured_snapshot_seed_plus_sequence_checked_diffs" } else { "captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs" },
                "venue_depth_complete": false,
                "events": rows.len(),
                "event_types": event_types,
                "has_replay_safe_checkpoint": true,
                "snapshot_ready_count": checkpointed_symbols.len(),
                "bridged_count": checkpointed_symbols.len(),
                "stream_coverage_verified_count": checkpointed_symbols.len(),
                "snapshot_only_symbols": [],
                "all_symbols_bridged": checkpointed_symbols == declared_symbols,
                "all_stream_coverage_verified": checkpointed_symbols == declared_symbols,
                "start_received_at_ns": rows.first().unwrap()["received_at_ns"],
                "end_received_at_ns": rows.last().unwrap()["received_at_ns"],
                "date": "2026-07-14",
                "hour": "00",
                "file": "part-1.jsonl.zst",
                "bytes": data.metadata().unwrap().len(),
                "sha256": content_sha256,
                "trade_representation": (!lob_only).then_some("aggregate_trade_only"),
                "price_surface_derivation": (!lob_only).then_some("latest aggregate trade price"),
                "trade_summary_contract": (!lob_only).then_some(AGGREGATE_TRADE_SUMMARY_CONTRACT),
                "trade_summaries": (!lob_only).then(|| trade_summaries.finish().unwrap()),
                "lob_continuity": lob_continuity.finish().unwrap(),
                "stream_types": (schema == MARKET_TAPE_SCHEMA_V2).then(|| vec!["depth@100ms"])
            });
            let mut manifest_bytes = serde_json::to_vec(&manifest_value).unwrap();
            manifest_bytes.push(b'\n');
            let manifest = sibling(&data, ".manifest.json").unwrap();
            std::fs::write(&manifest, &manifest_bytes).unwrap();
            let manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
            let success = sibling(&data, "._SUCCESS").unwrap();
            std::fs::write(&success, format!("{content_sha256}\n")).unwrap();
            let reference_root = directory.join("reference");
            let references = match market {
                Market::Spot => Vec::new(),
                Market::Usdm => [
                    (0, 3_000, 1, 123_455),
                    (2_500, 4_000, 2, 123_460),
                    (6_000, 9_000, 3, 123_465),
                ]
                .into_iter()
                .map(|(milliseconds, next_funding_ms, funding, oi)| {
                    publish_reference_batch(
                        &ReferenceArtifactConfig {
                            output_root: reference_root.clone(),
                            observed_at_ns: event_ns(milliseconds),
                            max_staleness_ms: 1_000,
                        },
                        OFFICIAL_USDM_SOURCE_ORIGIN,
                        &reference_batch(milliseconds, next_funding_ms, funding, oi),
                    )
                    .unwrap()
                })
                .collect(),
            };

            Self {
                directory,
                market,
                data,
                manifest,
                success,
                content_sha256,
                manifest_sha256,
                references,
            }
        }

        #[rustfmt::skip]
        fn args(&self) -> Args {
            Args {
                mission_id: "data-btc-usdm-1".to_string(), symbol: "BTCUSDT".to_string(),
                market: self.market, bucket_ms: 1_000, label_horizon_buckets: 2, top_depth: 5,
                segment: vec![self.data.clone()],
                segment_content_sha256: vec![self.content_sha256.clone()], segment_manifest_sha256: vec![self.manifest_sha256.clone()],
                artifact_dir: self.directory.join("artifacts"),
                reference_data: self.references.iter().map(|reference| reference.data_path.clone()).collect(),
                reference_data_sha256: self.references.iter().map(|reference| reference.data_sha256.clone()).collect(), reference_manifest_sha256: self.references.iter().map(|reference| reference.manifest_sha256.clone()).collect(),
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn rejects_mismatched_segment_argument_lengths_before_file_access() {
        let fixture = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let mut args = fixture.args();
        args.segment = vec![PathBuf::from("/does/not/exist.jsonl.zst")];
        args.segment_content_sha256.clear();

        let error = materialize(&args).unwrap_err().to_string();

        assert!(error.contains("equal nonzero lengths"));
    }

    #[test]
    fn rejects_bad_external_content_digest() {
        let fixture = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let mut args = fixture.args();
        args.segment_content_sha256[0] = "0".repeat(64);

        let error = materialize(&args).unwrap_err().to_string();

        assert!(error.contains("trusted digest anchor"));
        assert!(!args.artifact_dir.exists());
    }

    #[test]
    fn rejects_combined_tape_without_strict_modality_evidence() {
        let fixture = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(&fixture.manifest).unwrap()).unwrap();
        for field in ["trade_summary_contract", "trade_summaries"] {
            manifest.as_object_mut().unwrap().remove(field);
        }
        let mut bytes = serde_json::to_vec(&manifest).unwrap();
        bytes.push(b'\n');
        std::fs::write(&fixture.manifest, &bytes).unwrap();
        let mut args = fixture.args();
        args.segment_manifest_sha256[0] = hex::encode(Sha256::digest(&bytes));

        let error = materialize(&args).unwrap_err().to_string();

        assert!(error.contains("aggregate-trade summary contract"));
        assert!(!args.artifact_dir.exists());
    }

    #[test]
    fn same_timestamp_snapshot_and_diff_apply_before_one_sample() {
        let received_at_ns = event_ns(1_000);
        let events = vec![
            ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot {
                received_at_ns,
                bids: vec![
                    level("100", "10"),
                    level("99", "1"),
                    level("98", "1"),
                    level("97", "1"),
                    level("96", "1"),
                ],
                asks: vec![
                    level("102", "4"),
                    level("103", "1"),
                    level("104", "1"),
                    level("105", "1"),
                    level("106", "1"),
                ],
            }),
            ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Diff {
                received_at_ns,
                bids: vec![],
                asks: vec![level("102", "0"), level("101", "4")],
            }),
            ReplayedBinanceBookEvent::Checkpoint { received_at_ns },
        ];
        let mut replay = Replay::new(1_000_000_000, 5);

        replay.consume(&events).unwrap();

        assert_eq!(replay.samples.len(), 1);
        assert_eq!(replay.samples[0].time_ns, received_at_ns);
        assert!((replay.samples[0].mid_price - 100.5).abs() < 1e-12);
        assert!((replay.samples[0].ask_depth - 8.0).abs() < 1e-12);
    }

    #[test]
    fn samples_l1_and_weighted_top5_without_the_sixth_level() {
        let state = BookState {
            bids: BTreeMap::from([
                (Decimal::from(95), Decimal::from(1_000)),
                (Decimal::from(96), Decimal::from(2)),
                (Decimal::from(97), Decimal::from(4)),
                (Decimal::from(98), Decimal::from(6)),
                (Decimal::from(99), Decimal::from(8)),
                (Decimal::from(100), Decimal::from(10)),
            ]),
            asks: BTreeMap::from([
                (Decimal::from(101), Decimal::from(2)),
                (Decimal::from(102), Decimal::from(4)),
                (Decimal::from(103), Decimal::from(6)),
                (Decimal::from(104), Decimal::from(8)),
                (Decimal::from(105), Decimal::from(10)),
                (Decimal::from(106), Decimal::from(1_000)),
            ]),
        };

        let sample = sample_book(&state, 1, event_ns(1_000), 5).unwrap();

        assert_eq!(sample.bid_depth, 30.0);
        assert_eq!(sample.ask_depth, 30.0);
        assert_eq!(sample.top_depth_imbalance, 0.0);
        assert!((sample.weighted_book_imbalance_top5.unwrap() - 2.0 / 9.0).abs() < f64::EPSILON);
        assert!((sample.near_depth_concentration_skew_top5.unwrap() - 0.4).abs() < f64::EPSILON);
        assert!((sample.vwap_center_deviation_top5_bps.unwrap() - 66.33499170812604).abs() < 1e-12);
        assert!((sample.book_imbalance - 2.0 / 3.0).abs() < f64::EPSILON);

        let mut incomplete = state;
        incomplete.bids.remove(&Decimal::from(95));
        incomplete.bids.remove(&Decimal::from(96));
        assert!(sample_book(&incomplete, 1, event_ns(1_000), 5).is_err());
    }

    #[test]
    fn canonical_cli_has_no_account_fee_input() {
        let help = Args::command().render_long_help().to_string();

        assert!(!help.contains("--fee-data"));
        assert!(!help.contains("account"));
    }

    #[test]
    fn rejects_spot_until_public_instrument_rules_are_bound() {
        let fixture = Fixture::new(Market::Spot, &valid_rows("spot"));

        let error = materialize(&fixture.args()).unwrap_err().to_string();

        assert!(error.contains("currently supports USD-M only"));
        assert!(!fixture.directory.join("artifacts").exists());
    }

    #[test]
    fn materializes_usdm_before_live_execution_exists() {
        let fixture = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let args = fixture.args();

        materialize(&args).unwrap();

        assert!(args
            .artifact_dir
            .join(format!(
                "{}.reference.data",
                fixture.references[0].data_sha256
            ))
            .is_file());
    }

    #[test]
    fn materializes_production_top100_lob_without_synthetic_trades() {
        let fixture = Fixture::new(Market::Usdm, &lob_only_rows("usdm"));
        let published = materialize(&fixture.args()).unwrap();
        let rows = BufReader::new(File::open(&published.report.artifact_path).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<PointInTimeFeatureRow>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            published.report.snapshot.required_modalities,
            BTreeSet::from([CEX_MODALITY_LOB.to_string()])
        );
        assert_eq!(rows[0].modalities, BTreeSet::from([DataModality::Lob]));
        assert!(rows[0].features.contains_key("ofi_top5"));
        assert!(rows[0].features.contains_key("mid_return_1"));
        assert!(!rows[0]
            .features
            .keys()
            .any(|name| name.starts_with("aggregate_trade_")));
    }

    #[test]
    fn materializes_read_only_v1_reference_manifests() {
        let mut fixture = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        for reference in &mut fixture.references {
            rewrite_reference_manifest_v1(reference);
        }

        let published = materialize(&fixture.args()).unwrap();

        assert_eq!(
            published.report.snapshot.required_modalities,
            BTreeSet::from([
                CEX_MODALITY_LOB.to_string(),
                CEX_MODALITY_AGGREGATE_TRADE.to_string()
            ])
        );
    }

    #[test]
    fn rejects_materialization_when_instrument_rules_do_not_cover_full_label_window() {
        let first = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let mut second_rows = valid_rows("usdm");
        shift_rows(&mut second_rows, event_ns(10_000));
        retag_session(&mut second_rows, "session-2");
        let second = Fixture::new(Market::Usdm, &second_rows);
        let mut args = first.args();
        args.segment.push(second.data.clone());
        args.segment_content_sha256
            .push(second.content_sha256.clone());
        args.segment_manifest_sha256
            .push(second.manifest_sha256.clone());

        let error = materialize(&args).unwrap_err().to_string();

        assert!(
            error.contains("instrument-rule coverage does not span the replay window"),
            "{error}"
        );
    }

    #[test]
    fn materializes_two_series_when_instrument_rules_cover_full_label_window() {
        let first = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let mut second_rows = valid_rows("usdm");
        shift_rows(&mut second_rows, event_ns(10_000));
        retag_session(&mut second_rows, "session-2");
        let second = Fixture::new(Market::Usdm, &second_rows);
        let mut args = first.args();
        args.segment.push(second.data.clone());
        args.segment_content_sha256
            .push(second.content_sha256.clone());
        args.segment_manifest_sha256
            .push(second.manifest_sha256.clone());
        let extra_root = first.directory.join("reference-extra");
        let second_start_ns = second_rows[0]["received_at_ns"].as_u64().unwrap();
        let extra = [
            publish_reference_at_ns(
                &extra_root,
                second_start_ns + 500_000_000,
                second_start_ns + 3_000_000_000,
                4,
                123_470,
            ),
            publish_reference_at_ns(
                &extra_root,
                second_start_ns + 3_000_000_000,
                second_start_ns + 6_000_000_000,
                5,
                123_475,
            ),
            publish_reference_at_ns(
                &extra_root,
                second_start_ns + 6_000_000_000,
                second_start_ns + 9_000_000_000,
                6,
                123_480,
            ),
        ];
        for reference in &extra {
            args.reference_data.push(reference.data_path.clone());
            args.reference_data_sha256
                .push(reference.data_sha256.clone());
            args.reference_manifest_sha256
                .push(reference.manifest_sha256.clone());
        }

        let published = materialize(&args).unwrap();

        assert_eq!(published.report.series_count, 2);
        assert_eq!(published.report.snapshot.series.len(), 2);
        assert_eq!(published.report.snapshot.series[0].series_id, 1);
        assert_eq!(published.report.snapshot.series[1].series_id, 2);
        assert_eq!(
            published.report.snapshot.series[1]
                .instrument_rules_coverage
                .observations,
            3
        );
        assert_eq!(
            published.report.snapshot.instrument_rules.valid_through,
            datetime_ns(second_start_ns + 6_000_000_000).unwrap()
        );
    }

    #[test]
    fn rejects_series_with_bracketed_endpoints_but_a_middle_gap_above_90_seconds() {
        let fixture = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let mut args = fixture.args();
        let gap_root = fixture.directory.join("reference-gap");
        let custom = [
            publish_reference_at_ns(&gap_root, event_ns(0), event_ns(3_000), 1, 123_455),
            publish_reference_at_ns(&gap_root, event_ns(95_000), event_ns(96_000), 2, 123_460),
            publish_reference_at_ns(&gap_root, event_ns(96_000), event_ns(97_000), 3, 123_465),
        ];
        args.reference_data = custom
            .iter()
            .map(|reference| reference.data_path.clone())
            .collect();
        args.reference_data_sha256 = custom
            .iter()
            .map(|reference| reference.data_sha256.clone())
            .collect();
        args.reference_manifest_sha256 = custom
            .iter()
            .map(|reference| reference.manifest_sha256.clone())
            .collect();

        let error = materialize(&args).unwrap_err().to_string();

        assert!(
            error.contains("instrument-rule coverage has a gap above 90s"),
            "{error}"
        );
    }

    #[test]
    fn checkpoint_only_batch_does_not_advance_sample_clock_and_snapshot_reseeds_series() {
        let events = vec![
            ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot {
                received_at_ns: event_ns(1_000),
                bids: vec![
                    level("100", "10"),
                    level("99", "1"),
                    level("98", "1"),
                    level("97", "1"),
                    level("96", "1"),
                ],
                asks: vec![
                    level("101", "10"),
                    level("102", "1"),
                    level("103", "1"),
                    level("104", "1"),
                    level("105", "1"),
                ],
            }),
            ReplayedBinanceBookEvent::Checkpoint {
                received_at_ns: event_ns(1_500),
            },
            ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot {
                received_at_ns: event_ns(3_000),
                bids: vec![
                    level("200", "10"),
                    level("199", "1"),
                    level("198", "1"),
                    level("197", "1"),
                    level("196", "1"),
                ],
                asks: vec![
                    level("201", "10"),
                    level("202", "1"),
                    level("203", "1"),
                    level("204", "1"),
                    level("205", "1"),
                ],
            }),
        ];
        let mut replay = Replay::new(1_000_000_000, 5);

        replay.consume(&events).unwrap();

        assert_eq!(
            replay
                .samples
                .iter()
                .map(|sample| sample.time_ns)
                .collect::<Vec<_>>(),
            vec![event_ns(1_000), event_ns(3_000)]
        );
        assert_eq!(replay.samples[0].series_id, 1);
        assert_eq!(replay.samples[1].series_id, 2);
        assert!((replay.samples[0].mid_price - 100.5).abs() < 1e-12);
        assert!((replay.samples[1].mid_price - 200.5).abs() < 1e-12);
    }

    #[test]
    fn verify_segments_splits_two_capture_sessions_into_two_verified_series() {
        let first = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let mut second_rows = valid_rows("usdm");
        shift_rows(&mut second_rows, event_ns(10_000));
        retag_session(&mut second_rows, "session-2");
        let second = Fixture::new(Market::Usdm, &second_rows);
        let mut args = first.args();
        args.segment.push(second.data.clone());
        args.segment_content_sha256
            .push(second.content_sha256.clone());
        args.segment_manifest_sha256
            .push(second.manifest_sha256.clone());

        let (verified, _) = verify_segments(&args).unwrap();

        assert_eq!(verified.len(), 2);
        assert_eq!(verified[0].session_id(), "session-1");
        assert_eq!(verified[1].session_id(), "session-2");
    }

    #[test]
    fn non_top5_materialization_keeps_its_original_dynamic_features() {
        let fixture = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let mut args = fixture.args();
        args.top_depth = 1;

        let published = materialize(&args).unwrap();
        let row = BufReader::new(File::open(&published.report.artifact_path).unwrap())
            .lines()
            .next()
            .unwrap()
            .unwrap();
        let row: PointInTimeFeatureRow = serde_json::from_str(&row).unwrap();

        assert!(row.features.contains_key("bid_depth_top1"));
        assert!(row.features.contains_key("book_imbalance_top1"));
        assert!(!row.features.contains_key("weighted_book_imbalance_top5"));
        assert!(!row
            .features
            .contains_key("near_depth_concentration_skew_top5"));
        assert!(!row.features.contains_key("vwap_center_deviation_top5_bps"));
    }

    #[test]
    fn preserves_report_evidence_and_point_in_time_rows() {
        let fixture = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let published = materialize(&fixture.args()).unwrap();
        let output = serde_json::to_value(&published).unwrap();
        let source = &output["report"]["source_segments"][0];
        let snapshot: hft_research_manifest::CexReplaySnapshotV5 =
            serde_json::from_value(output["report"]["snapshot"].clone()).unwrap();
        let rows = BufReader::new(File::open(&published.report.artifact_path).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<PointInTimeFeatureRow>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            published.report.schema_version,
            BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7
        );
        assert_eq!(published.report.series_count, 1);
        assert_eq!(published.report.rows, 3);
        assert_eq!(rows[0].series_id, 1);
        assert_eq!(output["report"]["snapshot_sha256"], snapshot.sha256());
        assert_eq!(snapshot.schema_version, CEX_REPLAY_SNAPSHOT_SCHEMA_V5);
        assert_eq!(snapshot.instrument_rules.tick_size, "0.1");
        assert_eq!(snapshot.instrument_rules.step_size, "0.001");
        assert_eq!(snapshot.instrument_rules.min_notional, "5");
        assert_eq!(snapshot.series.len(), 1);
        assert_eq!(snapshot.series[0].series_id, 1);
        assert_eq!(snapshot.series[0].instrument_rules_coverage.observations, 3);
        let encoded_snapshot = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded_snapshot.contains("fee_schedule"));
        assert!(!encoded_snapshot.contains("runtime_account_id"));
        assert!(!encoded_snapshot.contains("account_fingerprint"));
        assert_eq!(
            source,
            &json!({
                "path": fixture.data,
                "sha256": fixture.content_sha256,
                "collector_manifest_path": fixture.manifest,
                "collector_manifest_sha256": fixture.manifest_sha256,
                "success_marker_path": fixture.success,
                "success_marker_sha256": hex::encode(Sha256::digest(format!("{}\n", fixture.content_sha256))),
                "start_received_at_ns": event_ns(0),
                "end_received_at_ns": event_ns(6_500),
                "events": valid_rows("usdm").len()
            })
        );
        assert_eq!(rows[0].symbol, "BTCUSDT");
        assert_eq!(
            rows[0].modalities,
            BTreeSet::from([DataModality::Lob, DataModality::TradeTick])
        );
        // Aggregate-trade buckets are (previous_sample, current_sample]: the 1.7s
        // trade lands in the first row's (1s, 2s] bucket and the 0.7s trade is
        // before the first sampled row.
        assert_eq!(rows[0].features["aggregate_trade_count"], 1.0);
        assert_eq!(rows[0].features["aggregate_trade_base_volume"], 2.0);
        assert_eq!(rows[0].features["aggregate_trade_quote_volume"], 201.0);
        assert_eq!(rows[0].features["aggregate_trade_flow_imbalance"], 1.0);
        assert!(rows[0]
            .features
            .contains_key("weighted_book_imbalance_top5"));
        assert!(
            (rows[0].features["near_depth_concentration_skew_top5"] - 0.17045454545454547).abs()
                < 1e-12
        );
        assert!(
            (rows[0].features["vwap_center_deviation_top5_bps"] - 28.12781278127806).abs() < 1e-9
        );
        assert_eq!(rows[1].features["aggregate_trade_count"], 0.0);
        assert!(!rows[0].features.contains_key("funding_rate"));
        assert!(!rows[0].features.contains_key("funding_cost_bps"));
        assert!(!rows[0].features.contains_key("open_interest"));
        assert_eq!(rows[0].event_time.to_rfc3339(), "2026-07-14T00:00:02+00:00");
        assert_eq!(
            rows[0].label_available_time.to_rfc3339(),
            "2026-07-14T00:00:04+00:00"
        );
        assert!((rows[0].features["mid_price"] - 101.0).abs() < 1e-12);
        assert!((rows[0].label - (101.25 / 101.0 - 1.0)).abs() < 1e-12);
    }
}
