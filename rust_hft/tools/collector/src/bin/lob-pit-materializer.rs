use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, ValueEnum};
use data::binance_lob_replay::{
    source_revision as governed_source_revision, Market as LobMarket, ReplaySequenceEvent,
};
use data::binance_market_tape::AggregateTrade;
use data::binance_market_tape_artifact::{
    seal_binance_market_tape_triplet,
    verify_binance_market_tape_with_required_trade_and_lob_summaries, BinanceMarketTapeTriplet,
    BinanceMarketTapeTrustAnchor, ReplayedBinanceBookEvent, VerifiedBinanceMarketTape,
};
use hft_collector::binance_fee_artifact::{verify_fee_artifact, PublishedFeeArtifact};
use hft_collector::binance_usdm_reference_artifact::{
    verify_reference_artifact, PublishedReferenceArtifact,
};
use alpha_domain::runtime_latency_evidence::{
    verify_runtime_latency_evidence, RuntimeLatencyEvidenceSource, VerifiedRuntimeLatencyEvidence,
};
use hft_collector::{DataModality, PointInTimeFeatureRow};
use hft_research_manifest::{
    CexArtifactTripletV2, CexDerivativesReferenceV2, CexFeeScheduleV2, CexInstrumentRulesV2,
    CexLatencyCostV2, CexPitSeriesEvidenceV2, CexReplaySegmentIdentity, CexReplaySnapshotV2,
    BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3, CEX_FEATURE_AVAILABILITY_POLICY,
    CEX_MODALITY_AGGREGATE_TRADE, CEX_MODALITY_FUNDING, CEX_MODALITY_LOB,
    CEX_MODALITY_OPEN_INTEREST, CEX_REPLAY_CLOCK_RECEIVED_AT_NS, CEX_REPLAY_SNAPSHOT_SCHEMA_V2,
};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_FEE_EVIDENCE_GAP_NS: u64 = 90_000_000_000;

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
    runtime_feedback_log: PathBuf,
    #[arg(long)]
    runtime_feedback_log_sha256: String,
    #[arg(long)]
    runtime_feedback_trusted_keys: PathBuf,
    #[arg(long)]
    runtime_feedback_trusted_keys_sha256: String,
    #[arg(long)]
    runtime_feedback_deployment_id: String,
    #[arg(long)]
    runtime_feedback_account_id: String,
    #[arg(long, required = true)]
    fee_data: Vec<PathBuf>,
    #[arg(long, required = true)]
    fee_data_sha256: Vec<String>,
    #[arg(long, required = true)]
    fee_manifest_sha256: Vec<String>,
    #[arg(long)]
    reference_data: Vec<PathBuf>,
    #[arg(long)]
    reference_data_sha256: Vec<String>,
    #[arg(long)]
    reference_manifest_sha256: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct ResearchContextV2 {
    instrument_rules: Option<CexInstrumentRulesV2>,
    derivatives_reference: Option<CexDerivativesReferenceV2>,
    funding: Vec<FundingPointV2>,
    open_interest: Vec<OpenInterestPointV2>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FundingPointV2 {
    available_at: DateTime<Utc>,
    next_funding_at: DateTime<Utc>,
    funding_rate: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OpenInterestPointV2 {
    available_at: DateTime<Utc>,
    open_interest: String,
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
    rows: usize,
    first_event_time: DateTime<Utc>,
    last_event_time: DateTime<Utc>,
    artifact_path: PathBuf,
    artifact_sha256: String,
    snapshot: CexReplaySnapshotV2,
    snapshot_sha256: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct PublishedMaterialization {
    report: MaterializationReport,
    report_path: PathBuf,
    report_sha256: String,
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
            self.emit_before(received_at_ns)?;
            for event in &events[start..end] {
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
            self.emit_at(received_at_ns)?;
            start = end;
        }
        Ok(())
    }
}

fn main() -> Result<()> {
    let published = materialize(&Args::parse())?;
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
    let (verified, segment_paths) = verify_segments(args)?;
    if verified
        .segments()
        .iter()
        .any(|segment| segment.market != args.market.as_lob_market())
    {
        bail!("verified market-tape does not match requested market");
    }
    let source_segments = source_segment_evidence(&verified, segment_paths)?;
    let book = verified
        .replayed_books()
        .iter()
        .find(|book| book.symbol == symbol)
        .with_context(|| {
            format!("verified market-tape does not contain requested symbol {symbol}")
        })?;
    if verified
        .segments()
        .iter()
        .all(|segment| !segment.trade_summaries.contains_key(&symbol))
    {
        bail!(
            "verified market-tape does not contain aggregate trades for requested symbol {symbol}"
        );
    }
    let mut context = ResearchContextV2::default();
    bind_usdm_reference(args, &symbol, &mut context)?;

    let bucket_ns = args
        .bucket_ms
        .checked_mul(1_000_000)
        .context("bucket size overflow")?;
    let mut replay = Replay::new(bucket_ns, args.top_depth);
    replay.consume(book.events())?;

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
    let rows = materialize_rows(
        &replay.samples,
        verified.aggregate_trades(),
        args,
        &source_revisions,
        &symbol,
        ingestion_time,
        &context,
    )?;
    let artifact_bytes = encode_rows(&rows)?;
    let artifact_sha256 = hex::encode(Sha256::digest(&artifact_bytes));
    let artifact_path = args.artifact_dir.join(format!("{artifact_sha256}.jsonl"));
    let first_event_time = rows.first().context("feature rows are empty")?.event_time;
    let last_event_time = rows.last().context("feature rows are empty")?.event_time;
    let last_label_time = rows
        .last()
        .context("feature rows are empty")?
        .label_available_time;
    let (fee_schedule, spot_rules) =
        bind_fee_evidence(args, &symbol, first_event_time, last_label_time)?;
    let latency_cost = load_latency_cost(
        args,
        &symbol,
        &fee_schedule.runtime_account_id,
        &fee_schedule.account_fingerprint,
        first_event_time,
    )?;
    if args.market == Market::Spot {
        context.instrument_rules = spot_rules;
    }
    let derivatives_reference = context.derivatives_reference.map(|mut reference| {
        let funding_bound = rows
            .iter()
            .filter_map(|row| row.features.get("funding_cost_bps"))
            .copied()
            .fold(0.0_f64, f64::max);
        reference.evaluation_funding_bps_per_bucket = funding_bound.to_string();
        reference
    });
    let snapshot = CexReplaySnapshotV2 {
        schema_version: CEX_REPLAY_SNAPSHOT_SCHEMA_V2.to_string(),
        venue: "binance".to_string(),
        instrument_type: args.market.as_str().to_string(),
        symbol: symbol.clone(),
        replay_clock: CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
        required_modalities: BTreeSet::from_iter(
            [
                CEX_MODALITY_LOB.to_string(),
                CEX_MODALITY_AGGREGATE_TRADE.to_string(),
            ]
            .into_iter()
            .chain((args.market == Market::Usdm).then_some(CEX_MODALITY_FUNDING.to_string()))
            .chain((args.market == Market::Usdm).then_some(CEX_MODALITY_OPEN_INTEREST.to_string())),
        ),
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
        instrument_rules: context
            .instrument_rules
            .context("verified instrument rules are missing")?,
        fee_schedule,
        derivatives_reference,
        latency_cost,
    };
    snapshot.validate().map_err(anyhow::Error::new)?;
    publish_immutable(&artifact_path, &artifact_bytes)?;
    let snapshot_sha256 = snapshot.sha256();

    let report = MaterializationReport {
        dataset_kind: "lob_point_in_time_materialization".to_string(),
        schema_version: BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3.to_string(),
        mission_id: mission_id.to_string(),
        symbol,
        market: args.market.as_str().to_string(),
        bucket_ms: args.bucket_ms,
        label_horizon_buckets: args.label_horizon_buckets,
        top_depth: args.top_depth,
        source_revision: revision,
        source_segments,
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
    publish_immutable(&report_path, &report_bytes)?;
    Ok(PublishedMaterialization {
        report,
        report_path,
        report_sha256,
    })
}

fn verify_segments(args: &Args) -> Result<(VerifiedBinanceMarketTape, BTreeMap<String, PathBuf>)> {
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
    for ((path, content_sha256), manifest_sha256) in args
        .segment
        .iter()
        .zip(&args.segment_content_sha256)
        .zip(&args.segment_manifest_sha256)
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
    }
    Ok((
        verify_binance_market_tape_with_required_trade_and_lob_summaries(sealed)?,
        paths,
    ))
}

fn load_latency_cost(
    args: &Args,
    symbol: &str,
    runtime_account_id: &str,
    account_fingerprint: &str,
    first_event_time: DateTime<Utc>,
) -> Result<CexLatencyCostV2> {
    if args.runtime_feedback_account_id != runtime_account_id {
        bail!("fee and runtime latency evidence accounts differ");
    }
    let verified = verify_runtime_latency_evidence(
        RuntimeLatencyEvidenceSource {
            feedback_log: &args.runtime_feedback_log,
            feedback_log_sha256: &args.runtime_feedback_log_sha256,
            trusted_keys: &args.runtime_feedback_trusted_keys,
            trusted_keys_sha256: &args.runtime_feedback_trusted_keys_sha256,
        },
        &args.runtime_feedback_deployment_id,
        args.market.as_str(),
        symbol,
        &args.runtime_feedback_account_id,
        first_event_time,
    )?;
    if verified.account_id != runtime_account_id {
        bail!("verified runtime latency account does not match fee evidence");
    }
    let evidence = publish_latency_evidence(args, symbol, account_fingerprint, &verified)?;
    Ok(CexLatencyCostV2 {
        method: "verified_order_lifecycle_realized_slippage".to_string(),
        venue: "binance".to_string(),
        symbol: symbol.to_string(),
        runtime_account_id: verified.account_id.clone(),
        account_fingerprint: account_fingerprint.to_string(),
        evidence,
        first_observed_at: verified.first_observed_at,
        last_observed_at: verified.last_observed_at,
        available_at: verified.available_at,
        observations: verified.observations,
        p50_ns: verified.p50_ns,
        p95_ns: verified.p95_ns,
        p99_ns: verified.p99_ns,
        p50_cost_bps: verified.p50_cost_bps,
        p95_cost_bps: verified.p95_cost_bps,
        p99_cost_bps: verified.p99_cost_bps,
    })
}

fn publish_latency_evidence(
    args: &Args,
    symbol: &str,
    account_fingerprint: &str,
    verified: &VerifiedRuntimeLatencyEvidence,
) -> Result<CexArtifactTripletV2> {
    let data_sha256 = hex::encode(Sha256::digest(&verified.signed_events));
    let data_path = args
        .artifact_dir
        .join(format!("{data_sha256}.runtime-feedback.jsonl"));
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schema": "monday.runtime-feedback-evidence.v1",
        "data_sha256": data_sha256,
        "trusted_keys_sha256": args.runtime_feedback_trusted_keys_sha256,
        "deployment_id": args.runtime_feedback_deployment_id,
        "venue": "binance",
        "symbol": symbol,
        "runtime_account_id": verified.account_id.clone(),
        "account_fingerprint": account_fingerprint,
        "first_observed_at": verified.first_observed_at,
        "last_observed_at": verified.last_observed_at,
        "observations": verified.observations,
    }))?;
    let manifest_sha256 = hex::encode(Sha256::digest(&manifest));
    let manifest_path = args
        .artifact_dir
        .join(format!("{manifest_sha256}.runtime-feedback.manifest.json"));
    let success = format!("{data_sha256}\n");
    let success_path = args
        .artifact_dir
        .join(format!("{data_sha256}.runtime-feedback._SUCCESS"));
    publish_immutable(&data_path, &verified.signed_events)?;
    publish_immutable(&manifest_path, &manifest)?;
    publish_immutable(&success_path, success.as_bytes())?;
    Ok(CexArtifactTripletV2 {
        data_sha256: data_sha256.clone(),
        manifest_sha256,
        success_sha256: data_sha256.clone(),
    })
}

fn bind_fee_evidence(
    args: &Args,
    symbol: &str,
    first_event_time: DateTime<Utc>,
    last_required_time: DateTime<Utc>,
) -> Result<(CexFeeScheduleV2, Option<CexInstrumentRulesV2>)> {
    let count = args.fee_data.len();
    if count == 0 || args.fee_data_sha256.len() != count || args.fee_manifest_sha256.len() != count
    {
        bail!("fee data, data SHA-256, and manifest SHA-256 must have equal nonzero lengths");
    }
    let mut snapshots = Vec::with_capacity(count);
    for ((data_path, data_sha256), manifest_sha256) in args
        .fee_data
        .iter()
        .zip(&args.fee_data_sha256)
        .zip(&args.fee_manifest_sha256)
    {
        let published = PublishedFeeArtifact {
            data_path: data_path.clone(),
            manifest_path: sibling(data_path, ".manifest.json")?,
            success_path: sibling(data_path, "._SUCCESS")?,
            data_sha256: data_sha256.clone(),
            manifest_sha256: manifest_sha256.clone(),
        };
        let snapshot = verify_fee_artifact(&published, data_sha256, manifest_sha256)?;
        if snapshot.market != args.market.as_str() || !snapshot.symbol.eq_ignore_ascii_case(symbol)
        {
            bail!("fee artifact identity does not match materialization");
        }
        republish_evidence_triplet(
            &args.artifact_dir,
            "fee",
            &published.data_path,
            &published.manifest_path,
            &published.success_path,
            &published.data_sha256,
            &published.manifest_sha256,
        )?;
        snapshots.push((snapshot, artifact_triplet(&published)?));
    }
    snapshots.sort_by_key(|(snapshot, _)| snapshot.received_at);
    let first = &snapshots[0].0;
    let last = &snapshots.last().unwrap().0;
    if first.received_at > first_event_time || last.received_at < last_required_time {
        bail!("fee evidence does not bracket the materialization window");
    }
    if max_available_gap_ns(snapshots.iter().map(|(snapshot, _)| snapshot.received_at))?
        > MAX_FEE_EVIDENCE_GAP_NS
    {
        bail!("fee evidence gap exceeds the bounded PIT interval");
    }
    if snapshots.iter().any(|(snapshot, _)| {
        snapshot.runtime_account_id != first.runtime_account_id
            || snapshot.account_fingerprint != first.account_fingerprint
            || snapshot.maker_fee_bps != first.maker_fee_bps
            || snapshot.taker_fee_bps != first.taker_fee_bps
            || snapshot.calculation != first.calculation
    }) {
        bail!("fee schedule changed inside the materialization window");
    }
    let evidence = snapshots
        .iter()
        .map(|(_, evidence)| evidence.clone())
        .collect::<Vec<_>>();
    let rules = match args.market {
        Market::Spot => {
            let first_rules = first
                .instrument_rules
                .as_ref()
                .context("Spot fee evidence has no instrument rules")?;
            if snapshots
                .iter()
                .any(|(snapshot, _)| snapshot.instrument_rules.as_ref() != Some(first_rules))
            {
                bail!("Spot instrument rules changed inside the materialization window");
            }
            Some(CexInstrumentRulesV2 {
                tick_size: first_rules.tick_size.clone(),
                step_size: first_rules.step_size.clone(),
                min_notional: first_rules.min_notional.clone(),
                available_at: first.received_at,
                valid_through: last.received_at,
                evidence: evidence.clone(),
            })
        }
        Market::Usdm => None,
    };
    Ok((
        CexFeeScheduleV2 {
            runtime_account_id: first.runtime_account_id.clone(),
            account_fingerprint: first.account_fingerprint.clone(),
            maker_buy_fee_bps: first.maker_fee_bps.buy.clone(),
            maker_sell_fee_bps: first.maker_fee_bps.sell.clone(),
            taker_buy_fee_bps: first.taker_fee_bps.buy.clone(),
            taker_sell_fee_bps: first.taker_fee_bps.sell.clone(),
            available_at: first.received_at,
            valid_through: last.received_at,
            evidence,
        },
        rules,
    ))
}

fn artifact_triplet(artifact: &PublishedFeeArtifact) -> Result<CexArtifactTripletV2> {
    Ok(CexArtifactTripletV2 {
        data_sha256: artifact.data_sha256.clone(),
        manifest_sha256: artifact.manifest_sha256.clone(),
        success_sha256: artifact.data_sha256.clone(),
    })
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

fn bind_usdm_reference(args: &Args, symbol: &str, context: &mut ResearchContextV2) -> Result<()> {
    let count = args.reference_data.len();
    if args.market == Market::Spot {
        if count != 0
            || !args.reference_data_sha256.is_empty()
            || !args.reference_manifest_sha256.is_empty()
        {
            bail!("Spot materialization does not accept USD-M reference artifacts");
        }
        return Ok(());
    }
    if count == 0
        || args.reference_data_sha256.len() != count
        || args.reference_manifest_sha256.len() != count
    {
        bail!("USD-M reference data and digest arguments must have equal nonzero lengths");
    }
    let mut rules = None;
    let mut rule_times = Vec::with_capacity(count);
    let mut funding = Vec::with_capacity(count);
    let mut open_interest = Vec::with_capacity(count);
    let mut evidence = Vec::with_capacity(count);
    for ((data_path, data_sha256), manifest_sha256) in args
        .reference_data
        .iter()
        .zip(&args.reference_data_sha256)
        .zip(&args.reference_manifest_sha256)
    {
        let published = PublishedReferenceArtifact {
            data_path: data_path.clone(),
            manifest_path: sibling(data_path, ".manifest.json")?,
            success_path: sibling(data_path, "._SUCCESS")?,
            data_sha256: data_sha256.clone(),
            manifest_sha256: manifest_sha256.clone(),
        };
        let batch = verify_reference_artifact(&published, data_sha256, manifest_sha256)?;
        republish_evidence_triplet(
            &args.artifact_dir,
            "reference",
            &published.data_path,
            &published.manifest_path,
            &published.success_path,
            &published.data_sha256,
            &published.manifest_sha256,
        )?;
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
        rules = Some(candidate);
        let rule_available_at = datetime_ns(contract.received_at_ns)?;
        rule_times.push(rule_available_at);
        let mark = batch
            .mark_index_funding()
            .iter()
            .find(|row| row.symbol == symbol)
            .with_context(|| format!("reference artifact has no funding row {symbol}"))?;
        funding.push(FundingPointV2 {
            available_at: datetime_ns(mark.received_at_ns)?,
            next_funding_at: datetime_ns(
                mark.next_funding_time_ms
                    .checked_mul(1_000_000)
                    .context("next funding time overflow")?,
            )?,
            funding_rate: mark.last_funding_rate.to_string(),
        });
        let oi = batch
            .open_interest()
            .iter()
            .find(|row| row.symbol == symbol)
            .with_context(|| format!("reference artifact has no open-interest row {symbol}"))?;
        open_interest.push(OpenInterestPointV2 {
            available_at: datetime_ns(oi.received_at_ns)?,
            open_interest: oi.open_interest.to_string(),
        });
        evidence.push((rule_available_at, reference_artifact_triplet(&published)?));
    }
    rule_times.sort_unstable();
    evidence.sort_by_key(|(available_at, _)| *available_at);
    let evidence = evidence
        .into_iter()
        .map(|(_, triplet)| triplet)
        .collect::<Vec<_>>();
    funding.sort_by_key(|point| point.available_at);
    open_interest.sort_by_key(|point| point.available_at);
    let (tick_size, step_size, min_notional) = rules.context("USD-M rules are missing")?;
    context.instrument_rules = Some(CexInstrumentRulesV2 {
        tick_size: tick_size.to_string(),
        step_size: step_size.to_string(),
        min_notional: min_notional.to_string(),
        available_at: *rule_times
            .first()
            .context("USD-M rules evidence is empty")?,
        valid_through: *rule_times.last().unwrap(),
        evidence: evidence.clone(),
    });
    context.derivatives_reference = Some(CexDerivativesReferenceV2 {
        funding: CexPitSeriesEvidenceV2 {
            evidence: evidence.clone(),
            first_available_at: funding[0].available_at,
            last_available_at: funding.last().unwrap().available_at,
            observations: funding.len() as u64,
            max_gap_ns: max_available_gap_ns(funding.iter().map(|point| point.available_at))?,
        },
        open_interest: CexPitSeriesEvidenceV2 {
            evidence,
            first_available_at: open_interest[0].available_at,
            last_available_at: open_interest.last().unwrap().available_at,
            observations: open_interest.len() as u64,
            max_gap_ns: max_available_gap_ns(open_interest.iter().map(|point| point.available_at))?,
        },
        evaluation_funding_bps_per_bucket: "0".to_string(),
    });
    context.funding = funding;
    context.open_interest = open_interest;
    Ok(())
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

fn max_available_gap_ns(times: impl IntoIterator<Item = DateTime<Utc>>) -> Result<u64> {
    let mut previous = None;
    let mut max_gap = 0;
    for current in times {
        if let Some(previous) = previous {
            let gap = current
                .signed_duration_since(previous)
                .num_nanoseconds()
                .context("PIT availability gap is outside i64 nanoseconds")?;
            max_gap = max_gap.max(u64::try_from(gap).context("PIT availability is not ordered")?);
        }
        previous = Some(current);
    }
    Ok(max_gap)
}

fn source_segment_evidence(
    verified: &VerifiedBinanceMarketTape,
    mut paths: BTreeMap<String, PathBuf>,
) -> Result<Vec<SourceSegmentEvidence>> {
    verified
        .segments()
        .iter()
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
    Ok(BookSample {
        series_id,
        time_ns,
        mid_price: decimal_f64(mid)?,
        spread_bps: decimal_f64((**best_ask - **best_bid) / mid * Decimal::from(10_000))?,
        bid_depth: decimal_f64(bid_depth)?,
        ask_depth: decimal_f64(ask_depth)?,
        top_depth_imbalance: decimal_f64((bid_depth - ask_depth) / total_depth)?,
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
    args: &Args,
    source_revisions: &BTreeMap<String, String>,
    symbol: &str,
    ingestion_time: DateTime<Utc>,
    context: &ResearchContextV2,
) -> Result<Vec<PointInTimeFeatureRow>> {
    let mut rows = Vec::new();
    let aggregate_trades = aggregate_trades
        .iter()
        .filter(|trade| trade.symbol == symbol)
        .collect::<Vec<_>>();
    for index in 1..samples.len().saturating_sub(args.label_horizon_buckets) {
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
        let mut modalities = BTreeSet::from([DataModality::Lob, DataModality::TradeTick]);
        if args.market == Market::Usdm {
            let funding = context
                .funding
                .iter()
                .rev()
                .find(|point| point.available_at <= event_time)
                .context("PIT funding coverage is missing")?;
            let open_interest = context
                .open_interest
                .iter()
                .rev()
                .find(|point| point.available_at <= event_time)
                .context("PIT open-interest coverage is missing")?;
            features.insert("funding_rate".to_string(), funding.funding_rate.parse()?);
            let previous_time = datetime_ns(previous.time_ns)?;
            let crossed_funding = context
                .funding
                .iter()
                .map(|point| point.next_funding_at)
                .filter(|scheduled| *scheduled > previous_time && *scheduled <= event_time)
                .collect::<BTreeSet<_>>();
            let funding_cost_bps = crossed_funding
                .into_iter()
                .filter_map(|scheduled| {
                    context
                        .funding
                        .iter()
                        .rev()
                        .find(|point| point.available_at < scheduled)
                        .filter(|point| point.next_funding_at == scheduled)
                })
                .map(|point| {
                    point
                        .funding_rate
                        .parse::<f64>()
                        .map(|rate| rate.abs() * 10_000.0)
                })
                .sum::<std::result::Result<f64, _>>()?;
            features.insert("funding_cost_bps".to_string(), funding_cost_bps);
            features.insert(
                "open_interest".to_string(),
                open_interest.open_interest.parse()?,
            );
            modalities.extend([DataModality::Funding, DataModality::OpenInterest]);
        }
        if !label.is_finite() || features.values().any(|value| !value.is_finite()) {
            bail!("materialized feature or label is not finite");
        }
        rows.push(PointInTimeFeatureRow {
            event_time,
            feature_available_time: event_time,
            label_available_time: datetime_ns(future.time_ns)?,
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
    use alpha_domain::{
        sign_runtime_attribution_event, AttributionKind, AttributionMode, AttributionOutcome,
        RuntimeAttributionEvent,
    };
    use data::binance_market_tape::{
        AggregateTrade, AggregateTradeSummaryBuilder, LobContinuitySummaryBuilder,
        AGGREGATE_TRADE_SUMMARY_CONTRACT,
    };
    use data::binance_usdm_reference::{
        ActivePerpetualContract, CompleteReferenceBatch, MarkIndexFundingObservation,
        OpenInterestObservation, EXCHANGE_INFO_ENDPOINT, OPEN_INTEREST_ENDPOINT,
        PREMIUM_INDEX_ENDPOINT, REFERENCE_SCHEMA, SERVER_TIME_ENDPOINT,
    };
    use ed25519_dalek::SigningKey;
    use hft_collector::binance_fee_artifact::{
        publish_fee_snapshot, BinanceFeeSnapshot, BinanceInstrumentRules, SideFeeBps, FEE_SCHEMA,
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
    fn fee_snapshot(market: Market, milliseconds: u64) -> BinanceFeeSnapshot {
        let observed_at = datetime_ns(event_ns(milliseconds)).unwrap();
        let (calculation, source_endpoint, instrument_rules, rules_source_endpoint) = match market {
            Market::Spot => (
                "liquidity_plus_side_standard_special_tax_without_asset_discount".to_string(),
                "/api/v3/account/commission".to_string(),
                Some(BinanceInstrumentRules {
                    tick_size: "0.1".to_string(),
                    step_size: "0.001".to_string(),
                    min_notional: "5".to_string(),
                }),
                Some("/api/v3/exchangeInfo".to_string()),
            ),
            Market::Usdm => (
                "account_commission_rate".to_string(),
                "/fapi/v1/commissionRate".to_string(),
                None,
                None,
            ),
        };
        BinanceFeeSnapshot {
            schema: FEE_SCHEMA.to_string(), venue: "binance".to_string(),
            market: market.as_str().to_string(), symbol: "BTCUSDT".to_string(),
            runtime_account_id: "binance-main".to_string(),
            account_fingerprint: "a".repeat(64),
            maker_fee_bps: SideFeeBps { buy: "2".into(), sell: "2".into() },
            taker_fee_bps: SideFeeBps { buy: "5".into(), sell: "5".into() },
            calculation, source_endpoint,
            instrument_rules, rules_source_endpoint,
            requested_at: observed_at, received_at: observed_at,
        }
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
            json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(100),"type":"snapshot","session_id":"session-1","symbol":"BTCUSDT","request_started_at_ns":event_ns(50),"snapshot":{"lastUpdateId":100,"bids":[["100","10"],["99","5"]],"asks":[["102","4"],["103","6"]]}}),
            diff(600, 101, 175, 100, json!([["100", "10"]]), json!([["101", "8"]])),
            trade(700, 10),
            diff(1_400, 176, 176, 175, json!([]), json!([["101", "0"]])),
            trade(1_700, 11),
            diff(2_400, 177, 177, 176, json!([["101", "3"]]), json!([])),
            diff(3_400, 178, 178, 177, json!([]), json!([["101.5", "4"]])),
            diff(4_400, 179, 179, 178, json!([["101", "0"]]), json!([])),
            diff(5_400, 180, 180, 179, json!([["100", "12"]]), json!([])),
            diff(6_400, 181, 181, 180, json!([]), json!([["101.5", "5"]])),
            json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(6_500),"type":"checkpoint","session_id":"session-1","symbol":"BTCUSDT","last_update_id":181,"synced":true,"bridged":true,"continuity_complete":true,"stream_coverage_verified":true,"bids":[["100","12"],["99","5"]],"asks":[["101.5","5"],["102","4"],["103","6"]],"reason":"test","replay_safe":true}),
        ]
    }

    struct Fixture {
        directory: PathBuf,
        market: Market,
        data: PathBuf,
        manifest: PathBuf,
        success: PathBuf,
        content_sha256: String,
        manifest_sha256: String,
        runtime_feedback_log: PathBuf,
        runtime_feedback_log_sha256: String,
        runtime_feedback_trusted_keys: PathBuf,
        runtime_feedback_trusted_keys_sha256: String,
        fees: Vec<PublishedFeeArtifact>,
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
            let manifest_value = json!({
                "schema": "binance.market_tape.v1",
                "venue": "binance",
                "market": market.as_str(),
                "dataset": format!("{}_all", market.as_str()),
                "shard_id": "all",
                "mode": "diff",
                "symbols": ["BTCUSDT"],
                "security_token_symbols": [],
                "excluded_symbols": [],
                "snapshot_limit": 1_000,
                "replay_scope": "captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs",
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
                "trade_representation": "aggregate_trade_only",
                "price_surface_derivation": "latest aggregate trade price",
                "trade_summary_contract": AGGREGATE_TRADE_SUMMARY_CONTRACT,
                "trade_summaries": trade_summaries.finish().unwrap(),
                "lob_continuity": lob_continuity.finish().unwrap()
            });
            let mut manifest_bytes = serde_json::to_vec(&manifest_value).unwrap();
            manifest_bytes.push(b'\n');
            let manifest = sibling(&data, ".manifest.json").unwrap();
            std::fs::write(&manifest, &manifest_bytes).unwrap();
            let manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
            let success = sibling(&data, "._SUCCESS").unwrap();
            std::fs::write(&success, format!("{content_sha256}\n")).unwrap();
            let signing_key = SigningKey::from_bytes(&[7; 32]);
            let runtime_feedback_trusted_keys = directory.join("runtime-feedback-keys.json");
            std::fs::write(
                &runtime_feedback_trusted_keys,
                serde_json::to_vec(&BTreeMap::from([(
                    "runtime-feedback-1".to_string(),
                    hex::encode(signing_key.verifying_key().to_bytes()),
                )]))
                .unwrap(),
            )
            .unwrap();
            let runtime_feedback_trusted_keys_sha256 =
                sha256_file(&runtime_feedback_trusted_keys).unwrap();
            let runtime_feedback_log = directory.join("runtime-feedback.jsonl");
            let signed = sign_runtime_attribution_event(
                RuntimeAttributionEvent {
                    event_id: "fill-1".to_string(),
                    deployment_id: "deployment-1".to_string(),
                    asset_revision_id: "candidate-1".to_string(),
                    mission_id: Some("data-btc-usdm-1".to_string()),
                    mode: AttributionMode::LiveSmall,
                    outcome: AttributionOutcome::Healthy,
                    kind: AttributionKind::Fill,
                    strategy_id: Some("strategy-1".to_string()),
                    order_id: Some("order-1".to_string()),
                    account_id: Some("binance-main".to_string()),
                    venue: Some("binance".to_string()),
                    symbol: Some("BTCUSDT".to_string()),
                    metrics: BTreeMap::from([
                        ("intent_to_private_report_us".to_string(), 75.0),
                        ("arrival_slippage_bps".to_string(), 1.25),
                        (
                            "evidence_available_at_us".to_string(),
                            event_ns(0) as f64 / 1_000.0,
                        ),
                        ("instrument_market_".to_string() + market.as_str(), 1.0),
                    ]),
                    reason: None,
                    observed_at: datetime_ns(event_ns(0)).unwrap(),
                },
                "runtime-feedback-1",
                &signing_key,
            )
            .unwrap();
            let mut feedback_bytes = serde_json::to_vec(&signed).unwrap();
            feedback_bytes.push(b'\n');
            std::fs::write(&runtime_feedback_log, feedback_bytes).unwrap();
            let runtime_feedback_log_sha256 = sha256_file(&runtime_feedback_log).unwrap();
            let fees = [0, 6_000]
                .into_iter()
                .map(|milliseconds| {
                    publish_fee_snapshot(
                        &directory.join("fees"),
                        &fee_snapshot(market, milliseconds),
                    )
                    .unwrap()
                })
                .collect();
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
                runtime_feedback_log,
                runtime_feedback_log_sha256,
                runtime_feedback_trusted_keys,
                runtime_feedback_trusted_keys_sha256,
                fees,
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
                artifact_dir: self.directory.join("artifacts"), runtime_feedback_log: self.runtime_feedback_log.clone(),
                runtime_feedback_log_sha256: self.runtime_feedback_log_sha256.clone(), runtime_feedback_trusted_keys: self.runtime_feedback_trusted_keys.clone(),
                runtime_feedback_trusted_keys_sha256: self.runtime_feedback_trusted_keys_sha256.clone(),
                runtime_feedback_deployment_id: "deployment-1".to_string(), runtime_feedback_account_id: "binance-main".to_string(),
                fee_data: self.fees.iter().map(|fee| fee.data_path.clone()).collect(), fee_data_sha256: self.fees.iter().map(|fee| fee.data_sha256.clone()).collect(),
                fee_manifest_sha256: self.fees.iter().map(|fee| fee.manifest_sha256.clone()).collect(), reference_data: self.references.iter().map(|reference| reference.data_path.clone()).collect(),
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
        for field in [
            "trade_summary_contract",
            "trade_summaries",
            "lob_continuity",
        ] {
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
                bids: vec![level("100", "10")],
                asks: vec![level("102", "4")],
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
        assert!((replay.samples[0].ask_depth - 4.0).abs() < 1e-12);
    }

    #[test]
    fn rejects_fee_and_runtime_evidence_from_different_accounts() {
        let fixture = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let mut args = fixture.args();
        args.runtime_feedback_account_id = "different-account".to_string();

        let error = materialize(&args).unwrap_err().to_string();

        assert!(error.contains("accounts differ"));
    }

    #[test]
    fn rejects_sparse_fee_evidence() {
        let fixture = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let late = publish_fee_snapshot(
            &fixture.directory.join("late-fee"),
            &fee_snapshot(Market::Usdm, 120_000),
        )
        .unwrap();
        let mut args = fixture.args();
        args.fee_data[1] = late.data_path;
        args.fee_data_sha256[1] = late.data_sha256;
        args.fee_manifest_sha256[1] = late.manifest_sha256;

        let error = materialize(&args).unwrap_err().to_string();

        assert!(error.contains("fee evidence gap"));
    }

    #[test]
    fn rejects_usdm_without_a_derivatives_execution_path() {
        let fixture = Fixture::new(Market::Usdm, &valid_rows("usdm"));
        let args = fixture.args();

        let error = materialize(&args).unwrap_err().to_string();

        assert!(error.contains("USD-M runtime latency evidence is unavailable"));
        assert!(args
            .artifact_dir
            .join(format!("{}.reference.data", fixture.references[0].data_sha256))
            .is_file());
    }

    #[test]
    fn preserves_report_evidence_and_point_in_time_rows() {
        let fixture = Fixture::new(Market::Spot, &valid_rows("spot"));
        let published = materialize(&fixture.args()).unwrap();
        let output = serde_json::to_value(&published).unwrap();
        let source = &output["report"]["source_segments"][0];
        let snapshot: hft_research_manifest::CexReplaySnapshotV2 =
            serde_json::from_value(output["report"]["snapshot"].clone()).unwrap();
        let rows = BufReader::new(File::open(&published.report.artifact_path).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<PointInTimeFeatureRow>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            published.report.schema_version,
            BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3
        );
        assert_eq!(published.report.rows, 3);
        snapshot.validate().unwrap();
        assert_eq!(output["report"]["snapshot_sha256"], snapshot.sha256());
        assert!(snapshot.derivatives_reference.is_none());
        assert_eq!(snapshot.instrument_rules.tick_size, "0.1");
        assert_eq!(snapshot.instrument_rules.step_size, "0.001");
        assert_eq!(snapshot.instrument_rules.min_notional, "5");
        assert_eq!(snapshot.fee_schedule.maker_buy_fee_bps, "2");
        assert_eq!(snapshot.fee_schedule.taker_buy_fee_bps, "5");
        assert_eq!(snapshot.fee_schedule.runtime_account_id, "binance-main");
        assert_eq!(snapshot.latency_cost.p95_cost_bps, "1.25");
        assert_eq!(snapshot.latency_cost.runtime_account_id, "binance-main");
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
                "events": valid_rows("spot").len()
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
        assert_eq!(rows[1].features["aggregate_trade_count"], 0.0);
        assert_eq!(rows[0].event_time.to_rfc3339(), "2026-07-14T00:00:02+00:00");
        assert_eq!(
            rows[0].label_available_time.to_rfc3339(),
            "2026-07-14T00:00:04+00:00"
        );
        assert!((rows[0].features["mid_price"] - 101.0).abs() < 1e-12);
        assert!((rows[0].label - (101.25 / 101.0 - 1.0)).abs() < 1e-12);
        for fee in &fixture.fees {
            let artifact_dir = fixture.directory.join("artifacts");
            assert!(artifact_dir
                .join(format!("{}.fee.data", fee.data_sha256))
                .is_file());
            assert!(artifact_dir
                .join(format!("{}.fee.manifest.json", fee.manifest_sha256))
                .is_file());
            assert!(artifact_dir
                .join(format!("{}.fee._SUCCESS", fee.data_sha256))
                .is_file());
        }
    }
}
