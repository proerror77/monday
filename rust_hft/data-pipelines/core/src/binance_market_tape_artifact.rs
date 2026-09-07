//! Externally anchored verification for immutable Binance market-tape segments.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read};
use std::ops::Range;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::binance_lob_replay::{
    Market, ReplayBookSnapshot, ReplaySequenceEvent, ReplaySequenceValidator,
};
use crate::binance_market_tape::{
    event_type_allowed, market_tape_schema, AggregateTrade, AggregateTradeSequenceValidator,
    AggregateTradeSummary, AggregateTradeSummaryBuilder, BookTicker, DepthSourceClock,
    DepthSourceClockSequenceValidator, ForceOrder, LobContinuitySummary,
    LobContinuitySummaryBuilder, RawTrade, RawTradeSequenceValidator,
    AGGREGATE_TRADE_SUMMARY_CONTRACT, LOB_CONTINUITY_SUMMARY_CONTRACT, MARKET_TAPE_SCHEMA,
    MARKET_TAPE_SCHEMA_V2, MAX_SOURCE_DELAY_MS,
};

const REPLAY_SCOPE: &str =
    "captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs";
const LOB_REPLAY_SCOPE: &str = "captured_snapshot_seed_plus_sequence_checked_diffs";
const USDM_LOB_DATASET: &str = "usdm_perpetual_top100_lob";
const USDM_LOB_SHADOW_DATASET: &str = "usdm_perpetual_top100_lob_rust_shadow";
const USDM_LOB_DEPTH_ONLY_STREAM_TYPES: [&str; 1] = ["depth@100ms"];
const USDM_LOB_HISTORICAL_STREAM_TYPES: [&str; 2] = ["depth@100ms", "bookTicker"];
const TRADE_REPRESENTATION: &str = "aggregate_trade_only";
const PRICE_SURFACE_DERIVATION: &str = "latest aggregate trade price";
const MAX_COMPRESSED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_DECOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ROW_BYTES: usize = 8 * 1024 * 1024;
const MAX_ROWS: usize = 10_000_000;

#[derive(Debug, Clone)]
pub struct BinanceMarketTapeTriplet {
    pub data: PathBuf,
    pub manifest: PathBuf,
    pub success: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinanceMarketTapeTrustAnchor {
    expected_content_sha256: [u8; 32],
    expected_manifest_sha256: [u8; 32],
}

impl BinanceMarketTapeTrustAnchor {
    pub fn from_lower_hex(content: &str, manifest: &str) -> Result<Self> {
        Ok(Self {
            expected_content_sha256: parse_digest(content)?,
            expected_manifest_sha256: parse_digest(manifest)?,
        })
    }
}

fn parse_digest(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        bail!("trusted SHA-256 must be 64 lowercase hex characters");
    }
    let decoded = hex::decode(value)?;
    decoded
        .try_into()
        .map_err(|_| anyhow!("trusted SHA-256 has the wrong length"))
}

#[derive(Debug)]
pub struct SealedBinanceMarketTapeTriplet {
    manifest: TapeManifest,
    manifest_sha256: String,
    decoded: Vec<u8>,
    rows: Vec<Range<usize>>,
}

#[derive(Debug, Default)]
pub struct BinanceAggregateTradeContinuityVerifier {
    symbols: Option<BTreeSet<String>>,
    market: Option<String>,
    dataset: Option<String>,
    shard_id: Option<String>,
    session_id: Option<String>,
    trade_receive_clocks: BTreeMap<String, Option<u64>>,
    aggregate_sequence: AggregateTradeSequenceValidator,
    previous_segment_end_received_at_ns: Option<u64>,
}

impl BinanceAggregateTradeContinuityVerifier {
    pub fn observe_segment(&mut self, segment: SealedBinanceMarketTapeTriplet) -> Result<()> {
        let symbols = segment
            .manifest
            .symbols
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if symbols.is_empty() {
            bail!("aggregate-trade continuity segment has no declared symbols");
        }
        if self
            .previous_segment_end_received_at_ns
            .is_some_and(|last| segment.manifest.start_received_at_ns < last)
        {
            bail!("aggregate-trade receive time moved backwards across segments");
        }
        if self.symbols.is_none() {
            self.trade_receive_clocks = symbols
                .iter()
                .map(|symbol| (symbol.clone(), None))
                .collect();
            self.symbols = Some(symbols.clone());
            self.market = Some(segment.manifest.market.clone());
            self.dataset = Some(segment.manifest.dataset.clone());
            self.shard_id = Some(segment.manifest.shard_id.clone());
        } else if self.symbols.as_ref() != Some(&symbols)
            || self.market.as_deref() != Some(segment.manifest.market.as_str())
            || self.dataset.as_deref() != Some(segment.manifest.dataset.as_str())
            || self.shard_id.as_deref() != Some(segment.manifest.shard_id.as_str())
        {
            bail!("aggregate-trade continuity segments do not share one scope");
        }

        let mut aggregate_trade_count = 0_u64;
        for (index, range) in segment.rows.iter().enumerate() {
            let raw: Value = serde_json::from_slice(&segment.decoded[range.clone()])
                .with_context(|| format!("parse {} row {}", segment.manifest.file, index + 1))?;
            let raw = raw
                .as_object()
                .ok_or_else(|| anyhow!("market-tape row must be an object"))?;
            let (event_type, row_session_id, received_at_ns) =
                validate_row(raw, &segment.manifest)?;
            if self
                .session_id
                .get_or_insert_with(|| row_session_id.to_owned())
                != row_session_id
            {
                bail!("aggregate-trade rows do not share one session_id");
            }
            if event_type != "agg_trade" {
                continue;
            }
            let trade = AggregateTrade::from_archived_event(raw, received_at_ns)?;
            require_symbol(
                self.symbols.as_ref().expect("initialized above"),
                &trade.symbol,
            )?;
            observe_receive_clock(
                &mut self.trade_receive_clocks,
                &trade.symbol,
                received_at_ns,
            )?;
            self.aggregate_sequence.observe(&trade)?;
            aggregate_trade_count = aggregate_trade_count
                .checked_add(1)
                .context("aggregate-trade count overflow")?;
        }
        if aggregate_trade_count == 0 {
            bail!("market-tape segment is missing aggregate trades");
        }
        self.previous_segment_end_received_at_ns = Some(segment.manifest.end_received_at_ns);
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct BinanceRawTradeContinuityVerifier {
    symbols: Option<BTreeSet<String>>,
    market: Option<String>,
    dataset: Option<String>,
    shard_id: Option<String>,
    session_id: Option<String>,
    trade_receive_clocks: BTreeMap<String, Option<u64>>,
    raw_trade_sequence: RawTradeSequenceValidator,
    previous_segment_end_received_at_ns: Option<u64>,
}

impl BinanceRawTradeContinuityVerifier {
    pub fn observe_segment(&mut self, segment: SealedBinanceMarketTapeTriplet) -> Result<()> {
        if segment.manifest.schema != MARKET_TAPE_SCHEMA_V2 {
            bail!("raw-trade continuity requires binance.market_tape.v2 segments");
        }
        if !segment.manifest.raw_trade_incomplete_symbols.is_empty() {
            bail!("raw-trade data is incomplete for a declared symbol");
        }
        let symbols = segment
            .manifest
            .symbols
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if symbols.is_empty() {
            bail!("raw-trade continuity segment has no declared symbols");
        }
        if self
            .previous_segment_end_received_at_ns
            .is_some_and(|last| segment.manifest.start_received_at_ns < last)
        {
            bail!("raw-trade receive time moved backwards across segments");
        }
        if self.symbols.is_none() {
            self.trade_receive_clocks = symbols
                .iter()
                .map(|symbol| (symbol.clone(), None))
                .collect();
            self.symbols = Some(symbols.clone());
            self.market = Some(segment.manifest.market.clone());
            self.dataset = Some(segment.manifest.dataset.clone());
            self.shard_id = Some(segment.manifest.shard_id.clone());
        } else if self.symbols.as_ref() != Some(&symbols)
            || self.market.as_deref() != Some(segment.manifest.market.as_str())
            || self.dataset.as_deref() != Some(segment.manifest.dataset.as_str())
            || self.shard_id.as_deref() != Some(segment.manifest.shard_id.as_str())
        {
            bail!("raw-trade continuity segments do not share one scope");
        }

        let mut raw_trade_count = 0_u64;
        for (index, range) in segment.rows.iter().enumerate() {
            let raw: Value = serde_json::from_slice(&segment.decoded[range.clone()])
                .with_context(|| format!("parse {} row {}", segment.manifest.file, index + 1))?;
            let raw = raw
                .as_object()
                .ok_or_else(|| anyhow!("market-tape row must be an object"))?;
            let (event_type, row_session_id, received_at_ns) =
                validate_row(raw, &segment.manifest)?;
            if self
                .session_id
                .get_or_insert_with(|| row_session_id.to_owned())
                != row_session_id
            {
                bail!("raw-trade rows do not share one session_id");
            }
            if event_type == "raw_trade_zero_price"
                && segment.manifest.market != Market::Usdm.as_str()
            {
                bail!("market-tape zero-price raw trades are USD-M only");
            }
            if !matches!(event_type, "raw_trade" | "raw_trade_zero_price") {
                continue;
            }
            let trade = if event_type == "raw_trade_zero_price" {
                RawTrade::from_zero_price_frame(
                    raw.get("frame").context("raw trade event has no frame")?,
                    received_at_ns,
                )?
            } else {
                RawTrade::from_archived_event(raw, received_at_ns)?
            };
            require_symbol(
                self.symbols.as_ref().expect("initialized above"),
                &trade.symbol,
            )?;
            observe_receive_clock(
                &mut self.trade_receive_clocks,
                &trade.symbol,
                received_at_ns,
            )?;
            self.raw_trade_sequence.observe(&trade)?;
            if event_type == "raw_trade" {
                raw_trade_count = raw_trade_count
                    .checked_add(1)
                    .context("raw-trade count overflow")?;
            }
        }
        if raw_trade_count == 0 {
            bail!("market-tape segment is missing raw trades");
        }
        self.previous_segment_end_received_at_ns = Some(segment.manifest.end_received_at_ns);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinanceMarketTapeSegmentIdentity {
    pub market: Market,
    pub file: String,
    pub content_sha256: String,
    pub manifest_sha256: String,
    pub start_received_at_ns: u64,
    pub end_received_at_ns: u64,
    pub events: u64,
    pub trade_summaries: BTreeMap<String, AggregateTradeSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayedBinanceBookEvent {
    Replay(ReplaySequenceEvent),
    Checkpoint { received_at_ns: u64 },
}

impl ReplayedBinanceBookEvent {
    pub fn received_at_ns(&self) -> u64 {
        match self {
            Self::Replay(ReplaySequenceEvent::Snapshot { received_at_ns, .. })
            | Self::Replay(ReplaySequenceEvent::Diff { received_at_ns, .. })
            | Self::Checkpoint { received_at_ns } => *received_at_ns,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayedBinanceBook {
    pub symbol: String,
    pub book: ReplayBookSnapshot,
    events: Vec<ReplayedBinanceBookEvent>,
}

impl ReplayedBinanceBook {
    pub fn events(&self) -> &[ReplayedBinanceBookEvent] {
        &self.events
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBinanceLobObservation {
    pub symbol: String,
    pub source_time_ms: u64,
    pub received_at_ns: u64,
}

#[derive(Debug)]
pub struct VerifiedBinanceMarketTape {
    segments: Vec<BinanceMarketTapeSegmentIdentity>,
    aggregate_trades: Vec<AggregateTrade>,
    lob_observations: Vec<VerifiedBinanceLobObservation>,
    replayed_books: Vec<ReplayedBinanceBook>,
}

impl VerifiedBinanceMarketTape {
    pub fn segments(&self) -> &[BinanceMarketTapeSegmentIdentity] {
        &self.segments
    }

    pub fn aggregate_trades(&self) -> &[AggregateTrade] {
        &self.aggregate_trades
    }

    pub fn lob_observations(&self) -> &[VerifiedBinanceLobObservation] {
        &self.lob_observations
    }

    pub fn replayed_books(&self) -> &[ReplayedBinanceBook] {
        &self.replayed_books
    }
}

#[derive(Debug)]
pub struct VerifiedBinanceMarketTapeSeries {
    session_id: String,
    verified: VerifiedBinanceMarketTape,
}

impl VerifiedBinanceMarketTapeSeries {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn verified(&self) -> &VerifiedBinanceMarketTape {
        &self.verified
    }
}

pub fn seal_binance_market_tape_triplet(
    triplet: &BinanceMarketTapeTriplet,
    trust: &BinanceMarketTapeTrustAnchor,
) -> Result<SealedBinanceMarketTapeTriplet> {
    validate_triplet_paths(triplet)?;
    let data_bytes = read_bound(&triplet.data, MAX_COMPRESSED_BYTES)?;
    let manifest_bytes = read_bound(&triplet.manifest, MAX_MANIFEST_BYTES)?;
    let success_bytes = read_bound(&triplet.success, 65)?;
    if <[u8; 32]>::from(Sha256::digest(&data_bytes)) != trust.expected_content_sha256
        || <[u8; 32]>::from(Sha256::digest(&manifest_bytes)) != trust.expected_manifest_sha256
    {
        bail!("market-tape bytes do not match the trusted digest anchor");
    }
    let parsed = parse_manifest(&manifest_bytes)?;
    validate_manifest_identity(&parsed, triplet, trust, data_bytes.len(), &success_bytes)?;
    let decoded = decode_bounded(&data_bytes)?;
    let rows = frame_rows(&decoded)?;
    Ok(SealedBinanceMarketTapeTriplet {
        manifest: parsed,
        manifest_sha256: hex::encode(trust.expected_manifest_sha256),
        decoded,
        rows,
    })
}

pub fn verify_binance_market_tape(
    sealed: Vec<SealedBinanceMarketTapeTriplet>,
) -> Result<VerifiedBinanceMarketTape> {
    verify_binance_market_tape_with_requirements(sealed, false, false)
}

pub fn verify_binance_market_tape_with_required_trade_summaries(
    sealed: Vec<SealedBinanceMarketTapeTriplet>,
) -> Result<VerifiedBinanceMarketTape> {
    verify_binance_market_tape_with_requirements(sealed, true, false)
}

pub fn verify_binance_market_tape_with_required_lob_continuity(
    sealed: Vec<SealedBinanceMarketTapeTriplet>,
) -> Result<VerifiedBinanceMarketTape> {
    verify_binance_market_tape_with_requirements(sealed, false, true)
}

pub fn verify_binance_market_tape_series_with_required_lob_continuity(
    sealed: Vec<SealedBinanceMarketTapeTriplet>,
) -> Result<Vec<VerifiedBinanceMarketTapeSeries>> {
    verify_binance_market_tape_series_with_requirements(sealed, false, true)
}

pub fn verify_binance_market_tape_series_with_required_trade_and_lob_summaries(
    sealed: Vec<SealedBinanceMarketTapeTriplet>,
) -> Result<Vec<VerifiedBinanceMarketTapeSeries>> {
    verify_binance_market_tape_series_with_requirements(sealed, true, true)
}

pub fn verify_binance_market_tape_with_required_trade_and_lob_summaries(
    sealed: Vec<SealedBinanceMarketTapeTriplet>,
) -> Result<VerifiedBinanceMarketTape> {
    verify_binance_market_tape_with_requirements(sealed, true, true)
}

pub fn verify_binance_market_tape_for_strict_gate(
    sealed: Vec<SealedBinanceMarketTapeTriplet>,
) -> Result<()> {
    let require_trade_summaries = sealed
        .first()
        .map(|segment| !manifest_is_usdm_lob_only(&segment.manifest))
        .ok_or_else(|| anyhow!("market-tape segment set is empty"))?;
    verify_binance_market_tape_with_requirements_and_surfaces(
        sealed,
        require_trade_summaries,
        true,
        false,
    )
    .map(|_| ())
}

pub fn verify_binance_market_tape_series_for_strict_gate(
    sealed: Vec<SealedBinanceMarketTapeTriplet>,
) -> Result<Vec<VerifiedBinanceMarketTapeSeries>> {
    let require_trade_summaries = sealed
        .first()
        .map(|segment| !manifest_is_usdm_lob_only(&segment.manifest))
        .ok_or_else(|| anyhow!("market-tape segment set is empty"))?;
    verify_binance_market_tape_series_with_requirements(sealed, require_trade_summaries, true)
}

fn verify_binance_market_tape_with_requirements(
    sealed: Vec<SealedBinanceMarketTapeTriplet>,
    require_trade_summaries: bool,
    require_lob_continuity: bool,
) -> Result<VerifiedBinanceMarketTape> {
    verify_binance_market_tape_with_requirements_and_surfaces(
        sealed,
        require_trade_summaries,
        require_lob_continuity,
        true,
    )
}

#[derive(Debug)]
struct SegmentSeriesScope {
    session_id: String,
    market: String,
    dataset: String,
    shard_id: String,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
}

fn verify_binance_market_tape_series_with_requirements(
    mut sealed: Vec<SealedBinanceMarketTapeTriplet>,
    require_trade_summaries: bool,
    require_lob_continuity: bool,
) -> Result<Vec<VerifiedBinanceMarketTapeSeries>> {
    if sealed.is_empty() {
        bail!("market-tape segment set is empty");
    }
    sealed.sort_by(|left, right| {
        left.manifest
            .start_received_at_ns
            .cmp(&right.manifest.start_received_at_ns)
            .then_with(|| left.manifest.file.cmp(&right.manifest.file))
    });

    let mut grouped = Vec::<(String, Vec<SealedBinanceMarketTapeTriplet>)>::new();
    let mut seen_sessions = BTreeSet::new();
    let mut global_market = None;
    let mut global_dataset = None;
    let mut global_shard_id = None;
    let mut previous_segment_end_received_at_ns = None;

    for segment in sealed {
        let scope = segment_series_scope(&segment)?;
        if global_market
            .get_or_insert_with(|| scope.market.clone())
            .as_str()
            != scope.market
            || global_dataset
                .get_or_insert_with(|| scope.dataset.clone())
                .as_str()
                != scope.dataset
            || global_shard_id
                .get_or_insert_with(|| scope.shard_id.clone())
                .as_str()
                != scope.shard_id
        {
            bail!("market-tape segments do not share one market/dataset/shard scope");
        }
        if previous_segment_end_received_at_ns.is_some_and(|last| scope.start_received_at_ns < last)
        {
            bail!("market-tape receive time moved backwards across segments");
        }
        previous_segment_end_received_at_ns = Some(scope.end_received_at_ns);

        match grouped.last_mut() {
            Some((current_session_id, grouped_segments))
                if current_session_id == &scope.session_id =>
            {
                grouped_segments.push(segment);
            }
            _ => {
                if !seen_sessions.insert(scope.session_id.clone()) {
                    bail!("market-tape capture session reappeared after another series");
                }
                grouped.push((scope.session_id, vec![segment]));
            }
        }
    }

    grouped
        .into_iter()
        .map(|(session_id, series)| {
            Ok(VerifiedBinanceMarketTapeSeries {
                session_id,
                verified: verify_binance_market_tape_with_requirements_and_surfaces(
                    series,
                    require_trade_summaries,
                    require_lob_continuity,
                    true,
                )?,
            })
        })
        .collect()
}

fn segment_series_scope(segment: &SealedBinanceMarketTapeTriplet) -> Result<SegmentSeriesScope> {
    let mut session_id = None;
    for (index, range) in segment.rows.iter().enumerate() {
        let raw: Value = serde_json::from_slice(&segment.decoded[range.clone()])
            .with_context(|| format!("parse {} row {}", segment.manifest.file, index + 1))?;
        let raw = raw
            .as_object()
            .ok_or_else(|| anyhow!("market-tape row must be an object"))?;
        let (_, row_session_id, _) = validate_row(raw, &segment.manifest)?;
        if session_id.get_or_insert_with(|| row_session_id.to_owned()) != row_session_id {
            bail!("market-tape rows do not share one session_id");
        }
    }
    Ok(SegmentSeriesScope {
        session_id: session_id.context("market-tape segment has no session_id")?,
        market: segment.manifest.market.clone(),
        dataset: segment.manifest.dataset.clone(),
        shard_id: segment.manifest.shard_id.clone(),
        start_received_at_ns: segment.manifest.start_received_at_ns,
        end_received_at_ns: segment.manifest.end_received_at_ns,
    })
}

fn verify_binance_market_tape_with_requirements_and_surfaces(
    mut sealed: Vec<SealedBinanceMarketTapeTriplet>,
    require_trade_summaries: bool,
    require_lob_continuity: bool,
    collect_surfaces: bool,
) -> Result<VerifiedBinanceMarketTape> {
    if !collect_surfaces && !require_lob_continuity {
        bail!("surface-free market-tape verification requires LOB continuity mode");
    }
    if sealed.is_empty() {
        bail!("market-tape segment set is empty");
    }
    sealed.sort_by(|left, right| {
        left.manifest
            .start_received_at_ns
            .cmp(&right.manifest.start_received_at_ns)
            .then_with(|| left.manifest.file.cmp(&right.manifest.file))
    });
    let market = Market::from_str(&sealed[0].manifest.market).map_err(anyhow::Error::msg)?;
    let dataset = sealed[0].manifest.dataset.clone();
    let shard_id = sealed[0].manifest.shard_id.clone();
    let symbols = sealed[0]
        .manifest
        .symbols
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut aggregate_sequence = AggregateTradeSequenceValidator::default();
    let mut raw_trade_sequence = RawTradeSequenceValidator::default();
    let mut depth_sequence = DepthSourceClockSequenceValidator::default();
    let mut replay = symbols
        .iter()
        .map(|symbol| {
            Ok((
                symbol.clone(),
                ReplaySequenceValidator::new(market, symbol)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut identities = Vec::with_capacity(sealed.len());
    let mut aggregate_trades = Vec::new();
    let mut lob_observations = Vec::new();
    let mut replayed_events: BTreeMap<String, Vec<ReplayedBinanceBookEvent>> = if collect_surfaces {
        symbols
            .iter()
            .map(|symbol| (symbol.clone(), Vec::new()))
            .collect()
    } else {
        BTreeMap::new()
    };
    let mut session_id = None;
    let mut depth_receive_clocks = symbols
        .iter()
        .map(|symbol| (symbol.clone(), None))
        .collect::<BTreeMap<_, _>>();
    let mut trade_receive_clocks = depth_receive_clocks.clone();
    let mut raw_trade_receive_clocks = depth_receive_clocks.clone();
    let mut book_ticker_receive_clocks = depth_receive_clocks.clone();
    let mut force_order_receive_clocks = depth_receive_clocks.clone();
    let stream_contract = sealed[0].manifest.stream_types.as_ref().map(|types| {
        types.iter().cloned().collect::<BTreeSet<_>>()
    });
    let mut highest_received_at_ns = None;
    let mut previous_segment_end_received_at_ns = None;

    for segment in sealed {
        let segment_stream_contract = segment.manifest.stream_types.as_ref().map(|types| {
            types.iter().cloned().collect::<BTreeSet<_>>()
        });
        if segment_stream_contract != stream_contract {
            bail!("market-tape segments do not share one stream contract");
        }
        if Market::from_str(&segment.manifest.market).map_err(anyhow::Error::msg)? != market
            || segment.manifest.dataset != dataset
            || segment.manifest.shard_id != shard_id
            || segment
                .manifest
                .symbols
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                != symbols
        {
            bail!("market-tape segments do not share one market/dataset/shard scope");
        }
        validate_manifest_quality(&segment.manifest, require_lob_continuity)?;
        let has_trade_summary_contract = segment.manifest.trade_summary_contract.as_deref()
            == Some(AGGREGATE_TRADE_SUMMARY_CONTRACT);
        if require_trade_summaries && !has_trade_summary_contract {
            bail!("market-tape segment is missing the aggregate-trade summary contract");
        }
        if has_trade_summary_contract != segment.manifest.trade_summaries.is_some() {
            bail!("market-tape aggregate-trade summary contract is incomplete");
        }
        if previous_segment_end_received_at_ns
            .is_some_and(|last| segment.manifest.start_received_at_ns < last)
        {
            bail!("market-tape receive time moved backwards across segments");
        }
        previous_segment_end_received_at_ns = Some(segment.manifest.end_received_at_ns);
        let mut counts = BTreeMap::<String, u64>::new();
        let mut trade_summaries = AggregateTradeSummaryBuilder::default();
        let mut lob_continuity = LobContinuitySummaryBuilder::new(symbols.iter().cloned())?;
        let mut checkpoints = BTreeSet::new();
        let mut audited_stale_raw_trade_symbols = BTreeSet::new();
        let mut snapshot_seeds = BTreeSet::new();
        let mut coverage_shard_count = None;
        let mut session_shard_count = None;
        for (index, range) in segment.rows.iter().enumerate() {
            let raw: Value = serde_json::from_slice(&segment.decoded[range.clone()])
                .with_context(|| format!("parse {} row {}", segment.manifest.file, index + 1))?;
            let raw = raw
                .as_object()
                .ok_or_else(|| anyhow!("market-tape row must be an object"))?;
            let (event_type, row_session_id, received_at_ns) =
                validate_row(raw, &segment.manifest)?;
            lob_continuity.observe(raw)?;
            if event_type == "session_start"
                && highest_received_at_ns.is_some_and(|last| received_at_ns < last)
            {
                bail!("market-tape receive time moved backwards for session_start");
            }
            highest_received_at_ns = Some(
                highest_received_at_ns.map_or(received_at_ns, |last: u64| last.max(received_at_ns)),
            );
            if session_id.get_or_insert_with(|| row_session_id.to_owned()) != row_session_id {
                bail!("market-tape rows do not share one session_id");
            }
            *counts.entry(event_type.to_owned()).or_default() += 1;
            match event_type {
                "diff" => {
                    let clock = DepthSourceClock::from_archived_event(raw, received_at_ns)?;
                    require_symbol(&symbols, &clock.symbol)?;
                    observe_receive_clock(
                        &mut depth_receive_clocks,
                        &clock.symbol,
                        received_at_ns,
                    )?;
                    depth_sequence.observe(&clock)?;
                    let events = observe_replay(
                        &mut replay,
                        &clock.symbol,
                        event_type,
                        raw,
                        received_at_ns,
                    )?;
                    if collect_surfaces {
                        record_replay_events(&mut replayed_events, &clock.symbol, events)?;
                        lob_observations.push(VerifiedBinanceLobObservation {
                            symbol: clock.symbol,
                            source_time_ms: clock.event_time_ms,
                            received_at_ns: clock.received_at_ns,
                        });
                    }
                }
                "snapshot" | "checkpoint" => {
                    let symbol = required_string(raw, "symbol")?.to_ascii_uppercase();
                    require_symbol(&symbols, &symbol)?;
                    observe_receive_clock(&mut depth_receive_clocks, &symbol, received_at_ns)?;
                    if event_type == "snapshot" {
                        snapshot_seeds.insert(symbol.clone());
                    } else {
                        if identities.is_empty()
                            && !snapshot_seeds.contains(&symbol)
                            && !require_lob_continuity
                        {
                            bail!(
                                "first market-tape segment checkpoint cannot establish replay \
                                 state before a snapshot seed"
                            );
                        }
                        if raw.get("replay_safe").and_then(Value::as_bool) != Some(true)
                            || raw.get("synced").and_then(Value::as_bool) != Some(true)
                            || if require_lob_continuity {
                                raw.get("continuity_complete").and_then(Value::as_bool)
                                    != Some(true)
                            } else {
                                raw.get("bridged").and_then(Value::as_bool) != Some(true)
                            }
                            || (require_lob_continuity
                                && raw.get("stream_coverage_verified").and_then(Value::as_bool)
                                    != Some(true))
                        {
                            bail!("market-tape checkpoint is not replay safe");
                        }
                        checkpoints.insert(symbol.clone());
                    }
                    let events = if event_type == "checkpoint" && require_lob_continuity {
                        replay
                            .get_mut(&symbol)
                            .context("market-tape symbol is outside its declared scope")?
                            .observe_verified_stream_coverage_checkpoint(raw, received_at_ns)?
                    } else {
                        observe_replay(&mut replay, &symbol, event_type, raw, received_at_ns)?
                    };
                    if collect_surfaces {
                        record_replay_events(&mut replayed_events, &symbol, events)?;
                        if event_type == "checkpoint" {
                            replayed_events
                                .get_mut(&symbol)
                                .context("market-tape replay event symbol is undeclared")?
                                .push(ReplayedBinanceBookEvent::Checkpoint { received_at_ns });
                        }
                    }
                }
                "agg_trade" => {
                    let trade = AggregateTrade::from_archived_event(raw, received_at_ns)?;
                    require_symbol(&symbols, &trade.symbol)?;
                    observe_receive_clock(
                        &mut trade_receive_clocks,
                        &trade.symbol,
                        received_at_ns,
                    )?;
                    aggregate_sequence.observe(&trade)?;
                    trade_summaries.observe(&trade)?;
                    if collect_surfaces {
                        aggregate_trades.push(trade);
                    }
                }
                "raw_trade" => {
                    let trade = RawTrade::from_archived_event(raw, received_at_ns)?;
                    require_symbol(&symbols, &trade.symbol)?;
                    observe_receive_clock(
                        &mut raw_trade_receive_clocks,
                        &trade.symbol,
                        received_at_ns,
                    )?;
                    raw_trade_sequence.observe(&trade)?;
                }
                "raw_trade_zero_price" => {
                    if market != Market::Usdm {
                        bail!("market-tape zero-price raw trades are USD-M only");
                    }
                    let trade = RawTrade::from_zero_price_frame(
                        raw.get("frame").context("raw trade event has no frame")?,
                        received_at_ns,
                    )?;
                    require_symbol(&symbols, &trade.symbol)?;
                    observe_receive_clock(
                        &mut raw_trade_receive_clocks,
                        &trade.symbol,
                        received_at_ns,
                    )?;
                    raw_trade_sequence.observe(&trade)?;
                }
                "stale_raw_trade" => {
                    if market != Market::Usdm {
                        bail!("stale raw trades are USD-M only");
                    }
                    let frame = raw
                        .get("frame")
                        .context("stale raw trade event has no frame")?;
                    let data = frame.get("data").unwrap_or(frame);
                    let zero_price = data
                        .get("p")
                        .and_then(Value::as_str)
                        .and_then(|value| value.parse::<Decimal>().ok())
                        == Some(Decimal::ZERO);
                    let trade = if zero_price {
                        RawTrade::from_zero_price_frame_allow_stale(frame, received_at_ns)?
                    } else {
                        RawTrade::from_frame_allow_stale(frame, received_at_ns)?
                    };
                    let symbol = required_string(raw, "symbol")?.to_ascii_uppercase();
                    require_symbol(&symbols, &symbol)?;
                    if trade.symbol != symbol {
                        bail!("stale raw trade symbol does not match its frame");
                    }
                    let stream = required_string(raw, "stream")?;
                    if frame.get("stream").and_then(Value::as_str) != Some(stream) {
                        bail!("stale raw trade stream does not match its frame");
                    }
                    let event_time_ms = data
                        .get("E")
                        .and_then(Value::as_u64)
                        .context("stale raw trade frame is missing E")?;
                    let trade_time_ms = data
                        .get("T")
                        .and_then(Value::as_u64)
                        .context("stale raw trade frame is missing T")?;
                    if raw.get("E").and_then(Value::as_u64) != Some(event_time_ms)
                        || raw.get("T").and_then(Value::as_u64) != Some(trade_time_ms)
                    {
                        bail!("stale raw trade source clocks do not match its frame");
                    }
                    let received_at_ms = received_at_ns / 1_000_000;
                    let recv_minus_event_ms = received_at_ms
                        .checked_sub(event_time_ms)
                        .context("stale raw trade receive clock is not behind E")?;
                    let recv_minus_trade_ms = received_at_ms
                        .checked_sub(trade_time_ms)
                        .context("stale raw trade receive clock is not behind T")?;
                    let event_minus_trade_ms = event_time_ms - trade_time_ms;
                    if recv_minus_event_ms <= MAX_SOURCE_DELAY_MS {
                        bail!("stale raw trade does not exceed the governed delay");
                    }
                    if raw.get("recv_minus_event_ms").and_then(Value::as_u64)
                        != Some(recv_minus_event_ms)
                        || raw.get("event_minus_trade_ms").and_then(Value::as_u64)
                            != Some(event_minus_trade_ms)
                        || raw.get("recv_minus_trade_ms").and_then(Value::as_u64)
                            != Some(recv_minus_trade_ms)
                    {
                        bail!("stale raw trade clock audit does not match its frame");
                    }
                    let producer_id = raw
                        .get("producer_id")
                        .and_then(Value::as_u64)
                        .context("stale raw trade row is missing producer_id")?;
                    let declared_shards = session_shard_count
                        .context("stale raw trade row has no session shard declaration")?;
                    if producer_id >= declared_shards {
                        bail!(
                            "stale raw trade producer_id {producer_id} is outside declared websocket_shards {declared_shards}"
                        );
                    }
                    observe_receive_clock(
                        &mut raw_trade_receive_clocks,
                        &trade.symbol,
                        received_at_ns,
                    )?;
                    audited_stale_raw_trade_symbols.insert(symbol);
                }
                "book_ticker" => {
                    let ticker = BookTicker::from_archived_event(raw, received_at_ns)?;
                    require_symbol(&symbols, &ticker.symbol)?;
                    observe_receive_clock(
                        &mut book_ticker_receive_clocks,
                        &ticker.symbol,
                        received_at_ns,
                    )?;
                }
                "stale_book_ticker" => {
                    if market != Market::Usdm {
                        bail!("stale book tickers are USD-M only");
                    }
                    let frame = raw
                        .get("frame")
                        .context("stale book ticker event has no frame")?;
                    let ticker = BookTicker::from_frame_allow_stale(frame, received_at_ns)?;
                    let symbol = required_string(raw, "symbol")?.to_ascii_uppercase();
                    require_symbol(&symbols, &symbol)?;
                    if ticker.symbol != symbol {
                        bail!("stale book ticker symbol does not match its frame");
                    }
                    let data = frame.get("data").unwrap_or(frame);
                    let event_time_ms = data
                        .get("E")
                        .and_then(Value::as_u64)
                        .context("stale book ticker frame is missing E")?;
                    let transaction_time_ms = data
                        .get("T")
                        .map(|value| {
                            value
                                .as_u64()
                                .context("stale book ticker frame has malformed T")
                        })
                        .transpose()?;
                    if raw.get("E").and_then(Value::as_u64) != Some(event_time_ms) {
                        bail!("stale book ticker E does not match its frame");
                    }
                    let audited_transaction_time_ms = raw
                        .get("T")
                        .map(|value| {
                            value
                                .as_u64()
                                .context("stale book ticker row has malformed T")
                        })
                        .transpose()?;
                    if audited_transaction_time_ms != transaction_time_ms {
                        bail!("stale book ticker T does not match its frame");
                    }
                    if transaction_time_ms.is_some_and(|transaction| transaction > event_time_ms) {
                        bail!("stale book ticker source clocks are reversed");
                    }
                    let receive_minus_event_ms = received_at_ns
                        .checked_div(1_000_000)
                        .and_then(|received| received.checked_sub(event_time_ms))
                        .context("stale book ticker receive clock is not behind E")?;
                    if receive_minus_event_ms <= MAX_SOURCE_DELAY_MS {
                        bail!("stale book ticker does not exceed the governed delay");
                    }
                    if raw.get("receive_minus_event_ms").and_then(Value::as_u64)
                        != Some(receive_minus_event_ms)
                    {
                        bail!("stale book ticker receive delay does not match its frame");
                    }
                    let event_minus_transaction_ms =
                        transaction_time_ms.map(|transaction| event_time_ms - transaction);
                    let audited_event_minus_transaction_ms = raw
                        .get("event_minus_transaction_ms")
                        .map(|value| {
                            value.as_u64().context(
                                "stale book ticker row has malformed event_minus_transaction_ms",
                            )
                        })
                        .transpose()?;
                    if audited_event_minus_transaction_ms != event_minus_transaction_ms {
                        bail!("stale book ticker transaction delay does not match its frame");
                    }
                    let producer_id = raw
                        .get("producer_id")
                        .and_then(Value::as_u64)
                        .context("stale book ticker row is missing producer_id")?;
                    let declared_shards = session_shard_count
                        .context("stale book ticker row has no session shard declaration")?;
                    if producer_id >= declared_shards {
                        bail!(
                            "stale book ticker producer_id {producer_id} is outside declared websocket_shards {declared_shards}"
                        );
                    }
                    observe_receive_clock(
                        &mut book_ticker_receive_clocks,
                        &ticker.symbol,
                        received_at_ns,
                    )?;
                }
                "force_order" => {
                    if market != Market::Usdm {
                        bail!("market-tape force orders are USD-M only");
                    }
                    let order = ForceOrder::from_archived_event(raw, received_at_ns)?;
                    require_symbol(&symbols, &order.symbol)?;
                    observe_receive_clock(
                        &mut force_order_receive_clocks,
                        &order.symbol,
                        received_at_ns,
                    )?;
                }
                "session_start" => {
                    if required_string(raw, "market")? != market.as_str() {
                        bail!("market-tape session market does not match its manifest");
                    }
                    let declared_count = u64::try_from(symbols.len())?;
                    let expected_streams = match segment.manifest.schema.as_str() {
                        MARKET_TAPE_SCHEMA_V2 => {
                            let declared_types = segment
                                .manifest
                                .stream_types
                                .as_ref()
                                .expect("v2 manifest declares stream types");
                            let row_types =
                                raw.get("stream_types")
                                    .and_then(Value::as_array)
                                    .map(|types| {
                                        types.iter().map(Value::as_str).collect::<Option<Vec<_>>>()
                                    });
                            let row_types = match row_types {
                                Some(Some(row_types))
                                    if row_types.len() == declared_types.len()
                                        && row_types.iter().all(|value| !value.is_empty()) =>
                                {
                                    row_types
                                }
                                _ => bail!(
                                    "market-tape session stream types do not match its manifest"
                                ),
                            };
                            if row_types.iter().copied().collect::<BTreeSet<_>>()
                                != declared_types
                                    .iter()
                                    .map(String::as_str)
                                    .collect::<BTreeSet<_>>()
                            {
                                bail!("market-tape session stream types do not match its manifest");
                            }
                            Some(declared_count * u64::try_from(declared_types.len())?)
                        }
                        _ if require_lob_continuity => Some(declared_count.saturating_mul(2)),
                        _ => None,
                    };
                    if raw.get("symbols").and_then(Value::as_u64) != Some(declared_count)
                        || raw.get("websocket_shards").and_then(Value::as_u64) == Some(0)
                        || raw
                            .get("websocket_shards")
                            .and_then(Value::as_u64)
                            .is_none()
                        || expected_streams.is_some_and(|expected| {
                            raw.get("websocket_streams").and_then(Value::as_u64) != Some(expected)
                        })
                        || (expected_streams.is_none()
                            && raw
                                .get("websocket_streams")
                                .is_some_and(|value| value.as_u64() != Some(declared_count * 2)))
                    {
                        bail!("market-tape session stream counts do not match its manifest");
                    }
                    session_shard_count = raw.get("websocket_shards").and_then(Value::as_u64);
                }
                "stream_coverage" => {
                    let shard_count =
                        validate_stream_coverage_row(raw, &symbols, &segment.manifest)?;
                    if coverage_shard_count.replace(shard_count).is_some() {
                        bail!("market-tape segment has duplicate stream coverage evidence");
                    }
                }
                _ => bail!("incomplete market-tape event {event_type}"),
            }
        }
        if audited_stale_raw_trade_symbols
            != segment
                .manifest
                .raw_trade_incomplete_symbols
                .iter()
                .cloned()
                .collect()
        {
            bail!("market-tape raw-trade incompleteness does not match raw rows");
        }
        if counts != segment.manifest.event_types
            || segment.rows.len() as u64 != segment.manifest.events
        {
            bail!("market-tape event counts do not match the manifest");
        }
        let manifest_declares_aggregate_trades = segment
            .manifest
            .stream_types
            .as_ref()
            .is_none_or(|stream_types| stream_types.iter().any(|ty| ty == "aggTrade"));
        if require_lob_continuity
            && (require_trade_summaries || manifest_declares_aggregate_trades)
            && counts.get("agg_trade").copied().unwrap_or(0) == 0
        {
            bail!("market-tape segment is missing aggregate trades");
        }
        if require_lob_continuity && coverage_shard_count.is_none() {
            bail!("market-tape segment is missing stream coverage evidence");
        }
        if session_shard_count
            .zip(coverage_shard_count)
            .is_some_and(|(declared, proven)| declared != proven)
        {
            bail!("market-tape stream coverage shard count does not match session start");
        }
        let trade_summaries = trade_summaries.finish()?;
        if segment
            .manifest
            .trade_summaries
            .as_ref()
            .is_some_and(|manifest| manifest != &trade_summaries)
        {
            bail!("market-tape trade summaries do not match raw aggregate trades");
        }
        if checkpoints != symbols {
            bail!("market-tape segment is missing a replay-safe checkpoint");
        }
        let lob_continuity = lob_continuity.finish()?;
        match segment.manifest.lob_continuity.as_ref() {
            Some(manifest) if manifest != &lob_continuity => {
                bail!("market-tape LOB continuity summary does not match raw rows")
            }
            None if require_lob_continuity => {
                bail!("market-tape segment is missing the LOB continuity contract")
            }
            _ => {}
        }
        identities.push(segment.identity(market, trade_summaries));
    }
    if !require_lob_continuity {
        let aggregate_trade_symbols = aggregate_trades
            .iter()
            .map(|trade| trade.symbol.as_str())
            .collect::<BTreeSet<_>>();
        if symbols
            .iter()
            .any(|symbol| !aggregate_trade_symbols.contains(symbol.as_str()))
        {
            bail!("verified market-tape is missing aggregate trades for a declared symbol");
        }
    }
    let mut replayed_books = Vec::with_capacity(if collect_surfaces { replay.len() } else { 0 });
    for (symbol, validator) in replay {
        validator.finish()?;
        let book = validator.book_snapshot()?;
        if book.bids.is_empty() || book.asks.is_empty() {
            bail!("verified market-tape contains an empty replayed book");
        }
        for [price, quantity] in book.bids.iter().chain(book.asks.iter()) {
            if price.parse::<Decimal>()? <= Decimal::ZERO
                || quantity.parse::<Decimal>()? <= Decimal::ZERO
            {
                bail!("verified market-tape contains a non-positive replayed book level");
            }
        }
        if collect_surfaces {
            let events = replayed_events
                .remove(&symbol)
                .context("verified market-tape is missing replay events for a declared symbol")?;
            replayed_books.push(ReplayedBinanceBook {
                symbol,
                book,
                events,
            });
        }
    }
    Ok(VerifiedBinanceMarketTape {
        segments: identities,
        aggregate_trades,
        lob_observations,
        replayed_books,
    })
}

fn observe_receive_clock(
    clocks: &mut BTreeMap<String, Option<u64>>,
    symbol: &str,
    received_at_ns: u64,
) -> Result<()> {
    let last = clocks
        .get_mut(symbol)
        .context("market-tape receive clock symbol is undeclared")?;
    if last.as_ref().is_some_and(|last| received_at_ns < *last) {
        bail!("market-tape receive time moved backwards for {symbol}");
    }
    *last = Some(received_at_ns);
    Ok(())
}

impl SealedBinanceMarketTapeTriplet {
    fn identity(
        &self,
        market: Market,
        trade_summaries: BTreeMap<String, AggregateTradeSummary>,
    ) -> BinanceMarketTapeSegmentIdentity {
        BinanceMarketTapeSegmentIdentity {
            market,
            file: self.manifest.file.clone(),
            content_sha256: self.manifest.sha256.clone(),
            manifest_sha256: self.manifest_sha256.clone(),
            start_received_at_ns: self.manifest.start_received_at_ns,
            end_received_at_ns: self.manifest.end_received_at_ns,
            events: self.manifest.events,
            trade_summaries,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TapeManifest {
    schema: String,
    venue: String,
    market: String,
    dataset: String,
    shard_id: String,
    mode: String,
    symbols: Vec<String>,
    security_token_symbols: Vec<String>,
    excluded_symbols: Vec<String>,
    snapshot_limit: u64,
    replay_scope: String,
    venue_depth_complete: bool,
    events: u64,
    event_types: BTreeMap<String, u64>,
    has_replay_safe_checkpoint: bool,
    #[serde(default)]
    raw_trade_incomplete_symbols: Vec<String>,
    snapshot_ready_count: u64,
    bridged_count: u64,
    #[serde(default)]
    stream_coverage_verified_count: u64,
    snapshot_only_symbols: Vec<String>,
    all_symbols_bridged: bool,
    #[serde(default)]
    all_stream_coverage_verified: bool,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    date: String,
    hour: String,
    file: String,
    bytes: u64,
    sha256: String,
    #[serde(default)]
    trade_representation: Option<String>,
    #[serde(default)]
    price_surface_derivation: Option<String>,
    trade_summary_contract: Option<String>,
    trade_summaries: Option<BTreeMap<String, AggregateTradeSummary>>,
    #[serde(default)]
    lob_continuity: Option<LobContinuitySummary>,
    #[serde(default)]
    stream_types: Option<Vec<String>>,
}

fn parse_manifest(bytes: &[u8]) -> Result<TapeManifest> {
    if bytes.is_empty()
        || bytes.last() != Some(&b'\n')
        || bytes[..bytes.len() - 1].contains(&b'\n')
        || bytes.contains(&b'\r')
    {
        bail!("market-tape manifest must be one JSON line ending in one newline");
    }
    serde_json::from_slice(bytes).context("parse market-tape manifest")
}

fn validate_manifest_identity(
    manifest: &TapeManifest,
    triplet: &BinanceMarketTapeTriplet,
    trust: &BinanceMarketTapeTrustAnchor,
    data_bytes: usize,
    success: &[u8],
) -> Result<()> {
    let data_name = triplet
        .data
        .file_name()
        .and_then(|name| name.to_str())
        .context("market-tape data file has no UTF-8 name")?;
    if !data_name.ends_with(".jsonl.zst")
        || triplet.manifest.file_name().and_then(|name| name.to_str())
            != Some(format!("{data_name}.manifest.json").as_str())
        || triplet.success.file_name().and_then(|name| name.to_str())
            != Some(format!("{data_name}._SUCCESS").as_str())
    {
        bail!("market-tape triplet names do not share one segment identity");
    }
    let content_sha256 = hex::encode(trust.expected_content_sha256);
    let requires_trade_contract = !manifest_is_usdm_lob_only(manifest);
    let expected_replay_scope = if requires_trade_contract {
        REPLAY_SCOPE
    } else {
        LOB_REPLAY_SCOPE
    };
    if !market_tape_schema(&manifest.schema)
        || manifest.venue != "binance"
        || manifest.mode != "diff"
        || manifest.replay_scope != expected_replay_scope
        || (requires_trade_contract
            && (manifest.trade_representation.as_deref() != Some(TRADE_REPRESENTATION)
                || manifest.price_surface_derivation.as_deref()
                    != Some(PRICE_SURFACE_DERIVATION)))
        || (!requires_trade_contract
            && (manifest.trade_representation.is_some()
                || manifest.price_surface_derivation.is_some()))
        || manifest
            .trade_summary_contract
            .as_deref()
            .is_some_and(|contract| contract != AGGREGATE_TRADE_SUMMARY_CONTRACT)
        || (!requires_trade_contract
            && (manifest.trade_summary_contract.is_some() || manifest.trade_summaries.is_some()))
        || manifest
            .lob_continuity
            .as_ref()
            .is_some_and(|summary| summary.contract != LOB_CONTINUITY_SUMMARY_CONTRACT)
        || manifest.venue_depth_complete
        || manifest.file != data_name
        || manifest.bytes != data_bytes as u64
        || manifest.sha256 != content_sha256
        || success != format!("{content_sha256}\n").as_bytes()
        || manifest.start_received_at_ns > manifest.end_received_at_ns
        || manifest.dataset.is_empty()
        || manifest.shard_id.is_empty()
        || manifest.snapshot_limit == 0
        || manifest.date.is_empty()
        || manifest.hour.is_empty()
    {
        bail!("market-tape manifest identity is inconsistent");
    }
    // The v2 manifest declares its per-symbol stream-type list; v1 must not
    // carry the field, keeping v1 verification byte-identical.
    let stream_types_declared = match manifest.schema.as_str() {
        MARKET_TAPE_SCHEMA => manifest.stream_types.is_none(),
        MARKET_TAPE_SCHEMA_V2 => manifest.stream_types.as_ref().is_some_and(|types| {
            !types.is_empty()
                && types.iter().all(|value| !value.is_empty())
                && types.iter().collect::<BTreeSet<_>>().len() == types.len()
        }),
        _ => false,
    };
    if !stream_types_declared {
        bail!("market-tape manifest stream types are inconsistent with its schema");
    }
    parse_digest(&manifest.sha256)?;
    Market::from_str(&manifest.market).map_err(anyhow::Error::msg)?;
    Ok(())
}

fn manifest_is_usdm_lob_only(manifest: &TapeManifest) -> bool {
    manifest.market == "usdm"
        && matches!(
            manifest.dataset.as_str(),
            USDM_LOB_DATASET | USDM_LOB_SHADOW_DATASET
        )
        && manifest.schema == MARKET_TAPE_SCHEMA_V2
        && manifest.stream_types.as_ref().is_some_and(|stream_types| {
            let declared = stream_types.iter().map(String::as_str).collect::<BTreeSet<_>>();
            declared == USDM_LOB_DEPTH_ONLY_STREAM_TYPES.iter().copied().collect::<BTreeSet<_>>()
                || (manifest.dataset == USDM_LOB_DATASET
                    && declared
                        == USDM_LOB_HISTORICAL_STREAM_TYPES
                            .iter()
                            .copied()
                            .collect::<BTreeSet<_>>())
        })
}

fn validate_manifest_quality(manifest: &TapeManifest, require_stream_coverage: bool) -> Result<()> {
    if !manifest.raw_trade_incomplete_symbols.is_empty() {
        bail!("market-tape raw-trade data is incomplete for a declared symbol");
    }
    if !manifest.has_replay_safe_checkpoint {
        bail!("market-tape segment is missing a replay-safe checkpoint");
    }
    let symbols = manifest.symbols.iter().cloned().collect::<BTreeSet<_>>();
    if symbols.is_empty()
        || symbols.len() != manifest.symbols.len()
        || symbols
            .iter()
            .any(|symbol| symbol.is_empty() || symbol != &symbol.to_ascii_uppercase())
        || manifest.snapshot_ready_count != symbols.len() as u64
        || manifest.bridged_count != symbols.len() as u64
        || !manifest.snapshot_only_symbols.is_empty()
        || !manifest.all_symbols_bridged
        || (require_stream_coverage
            && (manifest.stream_coverage_verified_count != symbols.len() as u64
                || !manifest.all_stream_coverage_verified))
        || manifest
            .security_token_symbols
            .iter()
            .any(|symbol| !symbols.contains(symbol))
        || manifest
            .excluded_symbols
            .iter()
            .any(|symbol| symbols.contains(symbol))
    {
        bail!("market-tape manifest does not describe complete replayable symbols");
    }
    let counted = manifest
        .event_types
        .values()
        .try_fold(0_u64, |total, count| {
            total
                .checked_add(*count)
                .context("market-tape event count overflow")
        })?;
    if counted != manifest.events || manifest.events == 0 {
        bail!("market-tape manifest event count mismatch");
    }
    for event_type in manifest.event_types.keys() {
        if !complete_event_type(&manifest.schema, event_type) {
            bail!("incomplete market-tape event {event_type}");
        }
    }
    Ok(())
}

fn complete_event_type(schema: &str, event_type: &str) -> bool {
    event_type_allowed(schema, event_type)
        && !matches!(
            event_type,
            "sequence_gap" | "aggregate_trade_gap" | "symbol_excluded"
        )
}

#[rustfmt::skip]
fn allowed_fields(event_type: &str, schema: &str) -> &'static [&'static str] {
    match event_type {
        "session_start" if schema == MARKET_TAPE_SCHEMA_V2 => &["schema", "received_at_ns", "type", "session_id", "market", "symbols", "websocket_shards", "websocket_streams", "stream_types"],
        "session_start" => &["schema", "received_at_ns", "type", "session_id", "market", "symbols", "websocket_shards", "websocket_streams"],
        "stream_coverage" => &["schema", "received_at_ns", "type", "session_id", "shards"],
        "snapshot" => &["schema", "received_at_ns", "type", "session_id", "archived_only", "symbol", "request_started_at_ns", "snapshot"],
        "diff" | "agg_trade" | "raw_trade" | "raw_trade_zero_price" | "book_ticker" | "force_order" => &["schema", "received_at_ns", "type", "session_id", "archived_only", "frame"],
        "stale_book_ticker" => &[
            "schema",
            "received_at_ns",
            "type",
            "session_id",
            "archived_only",
            "frame",
            "producer_id",
            "symbol",
            "E",
            "T",
            "receive_minus_event_ms",
            "event_minus_transaction_ms",
        ],
        "stale_raw_trade" => &[
            "schema",
            "received_at_ns",
            "type",
            "session_id",
            "archived_only",
            "frame",
            "producer_id",
            "stream",
            "symbol",
            "E",
            "T",
            "recv_minus_event_ms",
            "event_minus_trade_ms",
            "recv_minus_trade_ms",
        ],
        "checkpoint" => &["schema", "received_at_ns", "type", "session_id", "symbol", "last_update_id", "synced", "bridged", "continuity_complete", "stream_coverage_verified", "bids", "asks", "reason", "replay_safe"],
        _ => unreachable!("event type checked above"),
    }
}

fn validate_stream_coverage_row(
    raw: &Map<String, Value>,
    symbols: &BTreeSet<String>,
    manifest: &TapeManifest,
) -> Result<u64> {
    let shards = raw
        .get("shards")
        .and_then(Value::as_array)
        .context("market-tape stream coverage has no shard array")?;
    if shards.is_empty() {
        bail!("market-tape stream coverage has no shards");
    }
    let listed = shards
        .iter()
        .map(|shard| {
            let shard = shard
                .as_array()
                .context("market-tape stream coverage shard is not an array")?;
            if shard.is_empty() {
                bail!("market-tape stream coverage shard is empty");
            }
            shard
                .iter()
                .map(|stream| {
                    stream
                        .as_str()
                        .map(str::to_owned)
                        .context("market-tape stream coverage contains a non-string stream")
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    let stream_count = listed.iter().map(Vec::len).sum::<usize>();
    let actual = listed.into_iter().flatten().collect::<BTreeSet<_>>();
    if actual.len() != stream_count {
        bail!("market-tape stream coverage contains duplicate streams");
    }
    let expected = match manifest.schema.as_str() {
        MARKET_TAPE_SCHEMA_V2 => {
            let stream_types = manifest
                .stream_types
                .as_ref()
                .expect("v2 manifest declares stream types");
            symbols
                .iter()
                .flat_map(|symbol| {
                    let symbol = symbol.to_ascii_lowercase();
                    stream_types
                        .iter()
                        .map(move |stream_type| format!("{symbol}@{stream_type}"))
                })
                .collect::<BTreeSet<_>>()
        }
        _ => symbols
            .iter()
            .flat_map(|symbol| {
                let symbol = symbol.to_ascii_lowercase();
                [
                    format!("{symbol}@depth@100ms"),
                    format!("{symbol}@aggTrade"),
                ]
            })
            .collect::<BTreeSet<_>>(),
    };
    if actual != expected {
        bail!("market-tape stream coverage does not match declared symbols");
    }
    Ok(u64::try_from(shards.len())?)
}

fn validate_row<'a>(
    raw: &'a Map<String, Value>,
    manifest: &TapeManifest,
) -> Result<(&'a str, &'a str, u64)> {
    if required_string(raw, "schema")? != manifest.schema {
        bail!("market-tape row schema mismatch");
    }
    let event_type = required_string(raw, "type")?;
    if !complete_event_type(&manifest.schema, event_type) {
        bail!("incomplete market-tape event {event_type}");
    }
    if manifest.schema == MARKET_TAPE_SCHEMA_V2
        && !event_type_matches_declared_stream_types(
            event_type,
            manifest
                .stream_types
                .as_ref()
                .expect("v2 manifest declares stream types"),
        )
    {
        bail!("market-tape event {event_type} is outside its declared stream contract");
    }
    if raw
        .get("archived_only")
        .is_some_and(|value| value.as_bool() != Some(false))
    {
        bail!("market-tape contains archived_only data");
    }
    let allowed = allowed_fields(event_type, &manifest.schema);
    if raw.keys().any(|key| !allowed.contains(&key.as_str())) {
        bail!("market-tape row contains an unknown field");
    }
    let session_id = required_string(raw, "session_id")?;
    let received = raw
        .get("received_at_ns")
        .and_then(Value::as_u64)
        .context("market-tape row is missing received_at_ns")?;
    if !(manifest.start_received_at_ns..=manifest.end_received_at_ns).contains(&received) {
        bail!("market-tape row is outside manifest receive bounds");
    }
    Ok((event_type, session_id, received))
}

fn event_type_matches_declared_stream_types(event_type: &str, stream_types: &[String]) -> bool {
    let has = |stream_type: &str| stream_types.iter().any(|value| value == stream_type);
    match event_type {
        "diff" => has("depth@100ms"),
        "agg_trade" | "aggregate_trade_gap" => has("aggTrade"),
        "raw_trade" | "raw_trade_zero_price" | "stale_raw_trade" => has("trade"),
        "book_ticker" | "stale_book_ticker" => has("bookTicker"),
        "force_order" => has("forceOrder"),
        "session_start" | "stream_coverage" | "snapshot" | "checkpoint" => true,
        _ => false,
    }
}

fn observe_replay(
    validators: &mut BTreeMap<String, ReplaySequenceValidator>,
    symbol: &str,
    event_type: &str,
    raw: &Map<String, Value>,
    received_at_ns: u64,
) -> Result<Vec<ReplaySequenceEvent>> {
    let validator = validators
        .get_mut(symbol)
        .ok_or_else(|| anyhow!("market-tape symbol is outside its declared scope"))?;
    validator.observe(event_type, raw, received_at_ns)
}

fn record_replay_events(
    replayed: &mut BTreeMap<String, Vec<ReplayedBinanceBookEvent>>,
    symbol: &str,
    events: Vec<ReplaySequenceEvent>,
) -> Result<()> {
    replayed
        .get_mut(symbol)
        .context("market-tape replay event symbol is undeclared")?
        .extend(events.into_iter().map(ReplayedBinanceBookEvent::Replay));
    Ok(())
}

fn require_symbol(symbols: &BTreeSet<String>, symbol: &str) -> Result<()> {
    if !symbols.contains(symbol) {
        bail!("market-tape row symbol is outside its declared scope");
    }
    Ok(())
}

fn required_string<'a>(raw: &'a Map<String, Value>, field: &str) -> Result<&'a str> {
    raw.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("market-tape row is missing {field}"))
}

fn decode_bounded(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decoded = Vec::new();
    zstd::stream::read::Decoder::new(Cursor::new(compressed))?
        .take(MAX_DECOMPRESSED_BYTES + 1)
        .read_to_end(&mut decoded)?;
    if decoded.len() as u64 > MAX_DECOMPRESSED_BYTES {
        bail!("market-tape decompressed data exceeds its resource bound");
    }
    Ok(decoded)
}

fn frame_rows(data: &[u8]) -> Result<Vec<Range<usize>>> {
    if data.is_empty() || data.last() != Some(&b'\n') || data.contains(&b'\r') {
        bail!("market-tape data must be non-empty newline-delimited JSON");
    }
    let mut rows = Vec::new();
    let mut start = 0;
    for (end, byte) in data.iter().enumerate() {
        if *byte == b'\n' {
            if end == start || end - start > MAX_ROW_BYTES || rows.len() == MAX_ROWS {
                bail!("market-tape row violates its resource bound");
            }
            rows.push(start..end);
            start = end + 1;
        }
    }
    Ok(rows)
}

fn validate_triplet_paths(triplet: &BinanceMarketTapeTriplet) -> Result<()> {
    for path in [&triplet.data, &triplet.manifest, &triplet.success] {
        if !path.is_absolute()
            || path.components().any(|part| {
                matches!(
                    part,
                    Component::CurDir | Component::ParentDir | Component::Prefix(_)
                )
            })
        {
            bail!("market-tape triplet paths must be absolute and normalized");
        }
    }
    let parent = triplet
        .data
        .parent()
        .context("market-tape data path has no parent")?;
    if triplet.manifest.parent() != Some(parent) || triplet.success.parent() != Some(parent) {
        bail!("market-tape triplet must share one directory");
    }
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || fs::canonicalize(parent)? != parent
    {
        bail!("market-tape triplet directory must be canonical and non-symlinked");
    }
    Ok(())
}

fn read_bound(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open market-tape file {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        bail!(
            "market-tape file is not a bounded regular file: {}",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    file.by_ref().take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes || file.metadata()?.len() != metadata.len() {
        bail!(
            "market-tape file changed or exceeded its resource bound: {}",
            path.display()
        );
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Cursor;
    use std::path::Path;

    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};

    use super::*;

    const START_NS: u64 = 1_700_000_000_000_000_000;

    fn tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("binance-market-tape-")
            .tempdir()
            .unwrap()
    }

    #[rustfmt::skip]
    fn valid_rows() -> Vec<Value> {
        vec![
            json!({"schema":"binance.market_tape.v1","received_at_ns":START_NS,"type":"session_start","session_id":"session-1","market":"usdm","symbols":1,"websocket_shards":1,"websocket_streams":2}),
            json!({"schema":"binance.market_tape.v1","received_at_ns":START_NS+100_000_000,"type":"snapshot","session_id":"session-1","symbol":"BTCUSDT","request_started_at_ns":START_NS+50_000_000,"snapshot":{"lastUpdateId":100,"bids":[["100","1"]],"asks":[["101","1"]]}}),
            depth_row(START_NS + 200_000_000, 101, 100),
            trade_row(START_NS + 300_000_000, 10),
            checkpoint_row(START_NS + 400_000_000, 101),
        ]
    }

    #[rustfmt::skip]
    fn depth_row(received_at_ns: u64, update_id: u64, previous_id: u64) -> Value {
        json!({"schema":"binance.market_tape.v1","received_at_ns":received_at_ns,"type":"diff","session_id":"session-1","frame":{"data":{"e":"depthUpdate","E":received_at_ns/1_000_000,"T":received_at_ns/1_000_000,"s":"BTCUSDT","U":update_id,"u":update_id,"pu":previous_id,"b":[["100","2"]],"a":[]}}})
    }

    #[rustfmt::skip]
    fn trade_row(received_at_ns: u64, id: u64) -> Value {
        json!({"schema":"binance.market_tape.v1","received_at_ns":received_at_ns,"type":"agg_trade","session_id":"session-1","frame":{"stream":"btcusdt@aggTrade","data":{"e":"aggTrade","E":received_at_ns/1_000_000,"s":"BTCUSDT","a":id,"p":"100.5","q":"2","f":id,"l":id,"T":received_at_ns/1_000_000,"m":false}}})
    }

    #[rustfmt::skip]
    fn checkpoint_row(received_at_ns: u64, last_update_id: u64) -> Value {
        json!({"schema":"binance.market_tape.v1","received_at_ns":received_at_ns,"type":"checkpoint","session_id":"session-1","symbol":"BTCUSDT","last_update_id":last_update_id,"synced":true,"bridged":true,"continuity_complete":true,"stream_coverage_verified":true,"bids":[["100","2"]],"asks":[["101","1"]],"replay_safe":true,"reason":"test"})
    }

    fn two_symbol_rows_without_sol_trade() -> Vec<Value> {
        let mut rows = valid_rows();
        rows[0]["symbols"] = json!(2);
        rows[0]["websocket_streams"] = json!(4);
        let mut sol_snapshot = rows[1].clone();
        sol_snapshot["received_at_ns"] = json!(START_NS + 150_000_000);
        sol_snapshot["symbol"] = json!("SOLUSDT");
        sol_snapshot["snapshot"]["lastUpdateId"] = json!(200);
        let mut sol_checkpoint = checkpoint_row(START_NS + 450_000_000, 200);
        sol_checkpoint["symbol"] = json!("SOLUSDT");
        sol_checkpoint["bridged"] = json!(false);
        sol_checkpoint["bids"][0][1] = json!("1");
        rows.extend([sol_snapshot, sol_checkpoint]);
        rows.sort_by_key(|row| row["received_at_ns"].as_u64().unwrap());
        rows
    }

    fn with_stream_coverage(mut rows: Vec<Value>, symbols: &[&str]) -> Vec<Value> {
        let received_at_ns = rows
            .iter()
            .find(|row| row["type"] == "session_start")
            .map_or_else(
                || {
                    rows.iter()
                        .map(|row| row["received_at_ns"].as_u64().unwrap())
                        .min()
                        .unwrap()
                },
                |row| row["received_at_ns"].as_u64().unwrap() + 1,
            );
        if let Some(session) = rows.iter_mut().find(|row| row["type"] == "session_start") {
            session["websocket_shards"] = json!(2);
            session["websocket_streams"] = json!(symbols.len() * 2);
        }
        let depth = symbols
            .iter()
            .map(|symbol| format!("{}@depth@100ms", symbol.to_ascii_lowercase()))
            .collect::<Vec<_>>();
        let trades = symbols
            .iter()
            .map(|symbol| format!("{}@aggTrade", symbol.to_ascii_lowercase()))
            .collect::<Vec<_>>();
        rows.push(json!({"schema":"binance.market_tape.v1","received_at_ns":received_at_ns,"type":"stream_coverage","session_id":"session-1","shards":[depth,trades]}));
        rows.sort_by_key(|row| row["received_at_ns"].as_u64().unwrap());
        rows
    }

    const V2_STREAM_TYPES: [&str; 5] = [
        "depth@100ms",
        "aggTrade",
        "trade",
        "bookTicker",
        "forceOrder",
    ];

    fn v2_schema(mut row: Value) -> Value {
        row["schema"] = json!(MARKET_TAPE_SCHEMA_V2);
        row
    }

    #[rustfmt::skip]
    fn valid_v2_rows() -> Vec<Value> {
        vec![
            json!({"schema":MARKET_TAPE_SCHEMA_V2,"received_at_ns":START_NS,"type":"session_start","session_id":"session-1","market":"usdm","symbols":1,"websocket_shards":1,"websocket_streams":5,"stream_types":V2_STREAM_TYPES}),
            v2_schema(valid_rows()[1].clone()),
            v2_schema(depth_row(START_NS + 200_000_000, 101, 100)),
            v2_schema(trade_row(START_NS + 300_000_000, 10)),
            raw_trade_row(START_NS + 320_000_000, 10),
            book_ticker_row(START_NS + 340_000_000),
            force_order_row(START_NS + 360_000_000),
            v2_schema(checkpoint_row(START_NS + 400_000_000, 101)),
        ]
    }

    #[rustfmt::skip]
    fn raw_trade_row(received_at_ns: u64, id: u64) -> Value {
        json!({"schema":MARKET_TAPE_SCHEMA_V2,"received_at_ns":received_at_ns,"type":"raw_trade","session_id":"session-1","frame":{"stream":"btcusdt@trade","data":{"e":"trade","E":received_at_ns/1_000_000,"s":"BTCUSDT","t":id,"p":"100.5","q":"2","T":received_at_ns/1_000_000,"m":false}}})
    }

    fn zero_raw_trade_row(received_at_ns: u64, id: u64) -> Value {
        let mut row = raw_trade_row(received_at_ns, id);
        row["type"] = json!("raw_trade_zero_price");
        row["frame"]["data"]["p"] = json!("0");
        row["frame"]["data"]["q"] = json!("0");
        row["frame"]["data"]["X"] = json!("NA");
        row["frame"]["data"]["st"] = json!(1);
        row
    }

    fn observed_window_raw_trade_row(
        received_at_ns: u64,
        id: u64,
        event_time_ms: u64,
        trade_time_ms: u64,
        price: &str,
        quantity: &str,
        execution_type: &str,
    ) -> Value {
        json!({
            "schema": MARKET_TAPE_SCHEMA_V2,
            "received_at_ns": received_at_ns,
            "type": if price == "0" { "raw_trade_zero_price" } else { "raw_trade" },
            "session_id": "session-1",
            "frame": {
                "stream": "btcusdc@trade",
                "data": {
                    "e": "trade",
                    "E": event_time_ms,
                    "s": "BTCUSDC",
                    "t": id,
                    "p": price,
                    "q": quantity,
                    "T": trade_time_ms,
                    "X": execution_type,
                    "m": false,
                    "st": 1
                }
            }
        })
    }

    #[rustfmt::skip]
    fn book_ticker_row(received_at_ns: u64) -> Value {
        json!({"schema":MARKET_TAPE_SCHEMA_V2,"received_at_ns":received_at_ns,"type":"book_ticker","session_id":"session-1","frame":{"stream":"btcusdt@bookTicker","data":{"e":"bookTicker","u":400900217,"E":received_at_ns/1_000_000,"T":received_at_ns/1_000_000,"s":"BTCUSDT","b":"100.5","B":"31.21","a":"100.6","A":"40.66"}}})
    }

    fn stale_book_ticker_row(received_at_ns: u64) -> Value {
        let event_time_ms = received_at_ns / 1_000_000 - 31_000;
        let transaction_time_ms = event_time_ms - 5;
        json!({
            "schema": MARKET_TAPE_SCHEMA_V2,
            "received_at_ns": received_at_ns,
            "type": "stale_book_ticker",
            "session_id": "session-1",
            "producer_id": 0,
            "symbol": "BTCUSDT",
            "E": event_time_ms,
            "T": transaction_time_ms,
            "receive_minus_event_ms": 31_000,
            "event_minus_transaction_ms": 5,
            "frame": {
                "stream": "btcusdt@bookTicker",
                "data": {
                    "e": "bookTicker",
                    "u": 400900217,
                    "E": event_time_ms,
                    "T": transaction_time_ms,
                    "s": "BTCUSDT",
                    "b": "100.5",
                    "B": "31.21",
                    "a": "100.6",
                    "A": "40.66"
                }
            }
        })
    }

    fn stale_raw_trade_row(received_at_ns: u64) -> Value {
        let event_time_ms = received_at_ns / 1_000_000 - 30_944;
        let trade_time_ms = event_time_ms - 1;
        json!({
            "schema": MARKET_TAPE_SCHEMA_V2,
            "received_at_ns": received_at_ns,
            "type": "stale_raw_trade",
            "session_id": "session-1",
            "producer_id": 0,
            "stream": "btcusdt@trade",
            "symbol": "BTCUSDT",
            "E": event_time_ms,
            "T": trade_time_ms,
            "recv_minus_event_ms": 30_944,
            "event_minus_trade_ms": 1,
            "recv_minus_trade_ms": 30_945,
            "frame": {
                "stream": "btcusdt@trade",
                "data": {
                    "e": "trade",
                    "E": event_time_ms,
                    "T": trade_time_ms,
                    "X": "MARKET",
                    "m": false,
                    "p": "0.0664000",
                    "q": "506",
                    "s": "BTCUSDT",
                    "st": 1,
                    "t": 11
                }
            }
        })
    }

    fn stale_book_ticker_without_transaction_row(received_at_ns: u64) -> Value {
        let mut row = stale_book_ticker_row(received_at_ns);
        row.as_object_mut().unwrap().remove("T");
        row.as_object_mut()
            .unwrap()
            .remove("event_minus_transaction_ms");
        row["frame"]["data"].as_object_mut().unwrap().remove("T");
        row
    }

    #[rustfmt::skip]
    fn spot_book_ticker_row(received_at_ns: u64) -> Value {
        json!({"schema":MARKET_TAPE_SCHEMA_V2,"received_at_ns":received_at_ns,"type":"book_ticker","session_id":"session-1","frame":{"stream":"btcusdt@bookTicker","data":{"u":400900217,"s":"BTCUSDT","b":"100.5","B":"31.21","a":"100.6","A":"40.66"}}})
    }

    #[rustfmt::skip]
    fn force_order_row(received_at_ns: u64) -> Value {
        json!({"schema":MARKET_TAPE_SCHEMA_V2,"received_at_ns":received_at_ns,"type":"force_order","session_id":"session-1","frame":{"stream":"btcusdt@forceOrder","data":{"e":"forceOrder","E":received_at_ns/1_000_000,"o":{"s":"BTCUSDT","S":"SELL","o":"LIMIT","f":"IOC","q":"0.014","p":"9910","ap":"9910","X":"FILLED","l":"0.014","z":"0.014","T":received_at_ns/1_000_000}}}})
    }

    fn with_stream_coverage_v2(
        mut rows: Vec<Value>,
        symbols: &[&str],
        stream_types: &[&str],
    ) -> Vec<Value> {
        let received_at_ns = rows
            .iter()
            .find(|row| row["type"] == "session_start")
            .map_or_else(
                || {
                    rows.iter()
                        .map(|row| row["received_at_ns"].as_u64().unwrap())
                        .min()
                        .unwrap()
                },
                |row| row["received_at_ns"].as_u64().unwrap() + 1,
            );
        if let Some(session) = rows.iter_mut().find(|row| row["type"] == "session_start") {
            session["websocket_streams"] = json!(symbols.len() * stream_types.len());
        }
        let mut streams = symbols
            .iter()
            .flat_map(|symbol| {
                let symbol = symbol.to_ascii_lowercase();
                stream_types
                    .iter()
                    .map(move |stream_type| format!("{symbol}@{stream_type}"))
            })
            .collect::<Vec<_>>();
        let second_shard = streams.split_off((streams.len() + 1) / 2);
        let mut shards = vec![streams];
        if !second_shard.is_empty() {
            shards.push(second_shard);
        }
        if let Some(session) = rows.iter_mut().find(|row| row["type"] == "session_start") {
            session["websocket_shards"] = json!(shards.len());
        }
        rows.push(json!({"schema":MARKET_TAPE_SCHEMA_V2,"received_at_ns":received_at_ns,"type":"stream_coverage","session_id":"session-1","shards":shards}));
        rows.sort_by_key(|row| row["received_at_ns"].as_u64().unwrap());
        rows
    }

    fn shift_rows(rows: &mut [Value], delta_ns: u64) {
        for row in rows {
            let received_at_ns = row["received_at_ns"].as_u64().unwrap() + delta_ns;
            row["received_at_ns"] = json!(received_at_ns);
            match row["type"].as_str().unwrap() {
                "snapshot" => {
                    row["request_started_at_ns"] =
                        json!(row["request_started_at_ns"].as_u64().unwrap() + delta_ns);
                }
                "diff"
                | "agg_trade"
                | "raw_trade"
                | "raw_trade_zero_price"
                | "book_ticker"
                | "force_order" => {
                    let frame = row["frame"].get_mut("data").unwrap();
                    if let Some(event_time_ms) = frame.get("E").and_then(Value::as_u64) {
                        frame["E"] = json!(event_time_ms + delta_ns / 1_000_000);
                    }
                    if let Some(event_time_ms) = frame.get("T").and_then(Value::as_u64) {
                        frame["T"] = json!(event_time_ms + delta_ns / 1_000_000);
                    }
                }
                _ => {}
            }
        }
    }

    fn retag_session(rows: &mut [Value], session_id: &str) {
        for row in rows {
            row["session_id"] = json!(session_id);
        }
    }

    fn write_triplet(
        root: &Path,
        rows: &[Value],
    ) -> (BinanceMarketTapeTriplet, BinanceMarketTapeTrustAnchor) {
        write_triplet_for_symbols(root, rows, &["BTCUSDT"])
    }

    fn rewrite_manifest(
        triplet: &BinanceMarketTapeTriplet,
        update: impl FnOnce(&mut Value),
    ) -> BinanceMarketTapeTrustAnchor {
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&triplet.manifest).unwrap()).unwrap();
        update(&mut manifest);
        let mut manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        manifest_bytes.push(b'\n');
        fs::write(&triplet.manifest, &manifest_bytes).unwrap();
        BinanceMarketTapeTrustAnchor::from_lower_hex(
            manifest["sha256"].as_str().unwrap(),
            &format!("{:x}", Sha256::digest(&manifest_bytes)),
        )
        .unwrap()
    }

    fn add_trade_summaries(
        triplet: &BinanceMarketTapeTriplet,
        summaries: Value,
    ) -> BinanceMarketTapeTrustAnchor {
        rewrite_manifest(triplet, |manifest| {
            manifest["trade_summary_contract"] = json!(AGGREGATE_TRADE_SUMMARY_CONTRACT);
            manifest["trade_summaries"] = summaries;
        })
    }

    fn add_lob_continuity(
        triplet: &BinanceMarketTapeTriplet,
        rows: &[Value],
        symbols: &[&str],
    ) -> BinanceMarketTapeTrustAnchor {
        let mut summary =
            LobContinuitySummaryBuilder::new(symbols.iter().map(|symbol| (*symbol).to_owned()))
                .unwrap();
        for row in rows {
            summary.observe(row.as_object().unwrap()).unwrap();
        }
        let summary = summary.finish().unwrap();
        rewrite_manifest(triplet, |manifest| {
            manifest["lob_continuity"] = serde_json::to_value(summary).unwrap();
        })
    }

    fn one_trade_summary(base_volume: &str) -> Value {
        json!({
            "BTCUSDT":{
                "aggregate_trade_count":1,
                "venue_trade_count":1,
                "base_volume":base_volume,
                "quote_volume":"201",
                "buyer_aggressor_base_volume":"2",
                "buyer_aggressor_quote_volume":"201",
                "seller_aggressor_base_volume":"0",
                "seller_aggressor_quote_volume":"0",
                "vwap":"100.5",
                "first_event_time_ms":(START_NS + 300_000_000) / 1_000_000,
                "last_event_time_ms":(START_NS + 300_000_000) / 1_000_000,
                "first_trade_time_ms":(START_NS + 300_000_000) / 1_000_000,
                "last_trade_time_ms":(START_NS + 300_000_000) / 1_000_000,
                "first_received_at_ns":START_NS + 300_000_000,
                "last_received_at_ns":START_NS + 300_000_000,
                "first_aggregate_trade_id":10,
                "last_aggregate_trade_id":10,
                "first_trade_id":10,
                "last_trade_id":10
            }
        })
    }

    #[rustfmt::skip]
    fn write_triplet_for_symbols(root: &Path, rows: &[Value], symbols: &[&str]) -> (BinanceMarketTapeTriplet, BinanceMarketTapeTrustAnchor) {
        write_triplet_with_schema(root, rows, symbols, MARKET_TAPE_SCHEMA, None)
    }

    fn write_triplet_v2(
        root: &Path,
        rows: &[Value],
        symbols: &[&str],
        stream_types: &[&str],
    ) -> (BinanceMarketTapeTriplet, BinanceMarketTapeTrustAnchor) {
        write_triplet_with_schema(
            root,
            rows,
            symbols,
            MARKET_TAPE_SCHEMA_V2,
            Some(stream_types),
        )
    }

    #[rustfmt::skip]
    fn write_triplet_with_schema(root: &Path, rows: &[Value], symbols: &[&str], schema: &str, stream_types: Option<&[&str]>) -> (BinanceMarketTapeTriplet, BinanceMarketTapeTrustAnchor) {
        let root = fs::canonicalize(root).unwrap();
        let start = rows.iter().map(|row| row["received_at_ns"].as_u64().unwrap()).min().unwrap();
        let end = rows.iter().map(|row| row["received_at_ns"].as_u64().unwrap()).max().unwrap();
        let name = format!("part-{start}.jsonl.zst");
        let data_path = root.join(&name);
        let jsonl = rows.iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
        let compressed = zstd::stream::encode_all(Cursor::new(jsonl), 1).unwrap();
        fs::write(&data_path, &compressed).unwrap();
        let counts = rows.iter().fold(BTreeMap::<String, u64>::new(), |mut counts, row| {
            *counts.entry(row["type"].as_str().unwrap().to_owned()).or_default() += 1;
            counts
        });
        let checkpointed_symbols = rows
            .iter()
            .filter(|row| row["type"] == "checkpoint")
            .filter_map(|row| row["symbol"].as_str())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let declared_symbols = symbols.iter().map(|symbol| (*symbol).to_owned()).collect::<BTreeSet<_>>();
        let snapshot_only_symbols = declared_symbols.difference(&checkpointed_symbols).cloned().collect::<Vec<_>>();
        let has_checkpoint = counts.get("checkpoint").copied().unwrap_or(0) > 0;
        let data_sha = format!("{:x}", Sha256::digest(&compressed));
        let mut manifest = json!({
            "schema":schema,"venue":"binance","market":"usdm","dataset":"usdm_all","shard_id":"all","mode":"diff",
            "symbols":symbols,"security_token_symbols":[],"excluded_symbols":[],"snapshot_limit":1000,
            "replay_scope":"captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs","venue_depth_complete":false,
            "events":rows.len(),"event_types":counts,"has_replay_safe_checkpoint":has_checkpoint,
            "snapshot_ready_count":checkpointed_symbols.len(),"bridged_count":checkpointed_symbols.len(),
            "stream_coverage_verified_count":checkpointed_symbols.len(),
            "snapshot_only_symbols":snapshot_only_symbols,"all_symbols_bridged":checkpointed_symbols == declared_symbols,
            "all_stream_coverage_verified":checkpointed_symbols == declared_symbols,
            "start_received_at_ns":start,"end_received_at_ns":end,"date":"2023-11-14","hour":"22","file":name,"bytes":compressed.len(),"sha256":data_sha,
            "trade_representation":"aggregate_trade_only","price_surface_derivation":"latest aggregate trade price"
        });
        if let Some(stream_types) = stream_types {
            manifest["stream_types"] = json!(stream_types);
        }
        let mut manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        manifest_bytes.push(b'\n');
        let manifest_path = root.join(format!("{name}.manifest.json"));
        fs::write(&manifest_path, &manifest_bytes).unwrap();
        let success_path = root.join(format!("{name}._SUCCESS"));
        fs::write(&success_path, format!("{data_sha}\n")).unwrap();
        let anchor = BinanceMarketTapeTrustAnchor::from_lower_hex(
            &data_sha,
            &format!("{:x}", Sha256::digest(&manifest_bytes)),
        ).unwrap();
        (BinanceMarketTapeTriplet { data: data_path, manifest: manifest_path, success: success_path }, anchor)
    }

    #[test]
    fn rewritten_siblings_cannot_self_authenticate() {
        let root = tempdir();
        let (triplet, external_anchor) = write_triplet(root.path(), &valid_rows());
        let mut rewritten = valid_rows();
        rewritten[3]["frame"]["data"]["p"] = json!("999.0");
        write_triplet(root.path(), &rewritten);

        let error = seal_binance_market_tape_triplet(&triplet, &external_anchor).unwrap_err();
        assert!(error.to_string().contains("trusted digest anchor"));
    }

    #[test]
    fn verified_segment_exposes_manifest_matched_trade_summary() {
        let root = tempdir();
        let rows = valid_rows();
        let (triplet, _) = write_triplet(root.path(), &rows);
        let anchor = add_trade_summaries(&triplet, one_trade_summary("2"));
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let verified = verify_binance_market_tape(vec![sealed]).unwrap();
        assert_eq!(
            verified.segments()[0].trade_summaries["BTCUSDT"],
            serde_json::from_value(one_trade_summary("2")["BTCUSDT"].clone()).unwrap()
        );
    }

    #[test]
    fn manifest_trade_summary_must_match_raw_aggregate_trades() {
        let root = tempdir();
        let rows = valid_rows();
        let (triplet, _) = write_triplet(root.path(), &rows);
        let anchor = add_trade_summaries(&triplet, one_trade_summary("999"));
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let error = verify_binance_market_tape(vec![sealed]).unwrap_err();
        assert!(error.to_string().contains("trade summaries"));
    }

    #[test]
    fn strict_summary_verifier_rejects_legacy_manifest_without_contract() {
        let root = tempdir();
        let (triplet, anchor) = write_triplet(root.path(), &valid_rows());
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let error =
            verify_binance_market_tape_with_required_trade_summaries(vec![sealed]).unwrap_err();
        assert!(error.to_string().contains("summary contract"));
    }

    #[test]
    fn generic_verifier_preserves_legacy_market_tape_v1() {
        let root = tempdir();
        let mut rows = valid_rows();
        rows[0].as_object_mut().unwrap().remove("websocket_streams");
        rows[4]
            .as_object_mut()
            .unwrap()
            .remove("stream_coverage_verified");
        let (triplet, _) = write_triplet(root.path(), &rows);
        let anchor = rewrite_manifest(&triplet, |manifest| {
            manifest
                .as_object_mut()
                .unwrap()
                .remove("stream_coverage_verified_count");
            manifest
                .as_object_mut()
                .unwrap()
                .remove("all_stream_coverage_verified");
        });
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        verify_binance_market_tape(vec![sealed]).unwrap();
    }

    #[test]
    fn strict_lob_verifier_rejects_manifest_without_continuity_contract() {
        let root = tempdir();
        let rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        let (triplet, anchor) = write_triplet(root.path(), &rows);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let error =
            verify_binance_market_tape_with_required_lob_continuity(vec![sealed]).unwrap_err();
        assert!(error.to_string().contains("LOB continuity contract"));
    }

    #[test]
    fn strict_lob_verifier_rejects_manifest_latency_not_derived_from_raw_rows() {
        let root = tempdir();
        let rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        let (triplet, _) = write_triplet(root.path(), &rows);
        let valid_anchor = add_lob_continuity(&triplet, &rows, &["BTCUSDT"]);
        let sealed = seal_binance_market_tape_triplet(&triplet, &valid_anchor).unwrap();
        verify_binance_market_tape_with_required_lob_continuity(vec![sealed]).unwrap();

        let anchor = rewrite_manifest(&triplet, |manifest| {
            manifest["lob_continuity"]["symbols"]["BTCUSDT"]["max_source_latency_ms"] = json!(999);
        });
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let error =
            verify_binance_market_tape_with_required_lob_continuity(vec![sealed]).unwrap_err();
        assert!(error.to_string().contains("does not match raw rows"));
    }

    #[test]
    fn declared_summary_contract_requires_manifest_summaries() {
        let root = tempdir();
        let (triplet, _) = write_triplet(root.path(), &valid_rows());
        let anchor = rewrite_manifest(&triplet, |manifest| {
            manifest["trade_summary_contract"] = json!(AGGREGATE_TRADE_SUMMARY_CONTRACT);
        });
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let error = verify_binance_market_tape(vec![sealed]).unwrap_err();
        assert!(error.to_string().contains("contract is incomplete"));
    }

    #[test]
    fn cross_segment_sequence_gap_is_rejected() {
        let root = tempdir();
        let (first, first_anchor) = write_triplet(root.path(), &valid_rows());
        let second_rows = vec![
            depth_row(START_NS + 1_000_000_000, 103, 102),
            trade_row(START_NS + 1_100_000_000, 11),
            checkpoint_row(START_NS + 1_200_000_000, 103),
        ];
        let (second, second_anchor) = write_triplet(root.path(), &second_rows);
        let sealed = vec![
            seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
        ];

        let error = verify_binance_market_tape(sealed).unwrap_err();
        assert!(error.to_string().contains("gap"));
    }

    #[test]
    fn receive_time_cannot_move_backwards_across_segments() {
        let root = tempdir();
        let (first, first_anchor) = write_triplet(root.path(), &valid_rows());
        let second_rows = vec![
            depth_row(START_NS + 300_000_000, 102, 101),
            trade_row(START_NS + 350_000_000, 11),
            checkpoint_row(START_NS + 400_000_000, 102),
        ];
        let (second, second_anchor) = write_triplet(root.path(), &second_rows);
        let sealed = vec![
            seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
        ];

        let error = verify_binance_market_tape(sealed).unwrap_err();
        assert!(error.to_string().contains("receive time moved backwards"));
    }

    #[test]
    fn independent_symbol_streams_may_interleave_receive_times() {
        let root = tempdir();
        let mut rows = two_symbol_rows_without_sol_trade();
        let mut sol_trade = trade_row(START_NS + 350_000_081, 20);
        sol_trade["frame"]["stream"] = json!("solusdt@aggTrade");
        sol_trade["frame"]["data"]["s"] = json!("SOLUSDT");
        let mut sol_diff = depth_row(START_NS + 250_000_000, 201, 200);
        sol_diff["frame"]["data"]["s"] = json!("SOLUSDT");
        let sol_checkpoint = rows
            .iter_mut()
            .find(|row| row["type"] == "checkpoint" && row["symbol"] == "SOLUSDT")
            .unwrap();
        sol_checkpoint["last_update_id"] = json!(201);
        sol_checkpoint["bridged"] = json!(true);
        sol_checkpoint["bids"][0][1] = json!("2");
        let checkpoint = rows
            .iter()
            .position(|row| row["type"] == "checkpoint" && row["symbol"] == "BTCUSDT")
            .unwrap();
        rows[checkpoint]["received_at_ns"] = json!(START_NS + 350_000_000);
        rows.extend([sol_diff, sol_trade]);
        rows.sort_by_key(|row| row["received_at_ns"].as_u64().unwrap());
        let (triplet, anchor) =
            write_triplet_for_symbols(root.path(), &rows, &["BTCUSDT", "SOLUSDT"]);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        verify_binance_market_tape(vec![sealed]).unwrap();
    }

    #[test]
    fn backdated_session_start_is_rejected() {
        let root = tempdir();
        let mut rows = valid_rows();
        let mut late_session = rows[0].clone();
        late_session["received_at_ns"] = json!(START_NS + 250_000_000);
        rows.insert(4, late_session);
        let (triplet, anchor) = write_triplet(root.path(), &rows);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let error = verify_binance_market_tape(vec![sealed]).unwrap_err();
        assert!(error.to_string().contains("receive time moved backwards"));
    }

    #[test]
    fn invalid_depth_source_clocks_never_return_a_verified_handle() {
        for (event_time, transaction_time, expected) in [
            (Value::Null, Value::Null, "depth field E is missing"),
            (json!(0), json!(0), "governed limit"),
        ] {
            let root = tempdir();
            let mut rows = valid_rows();
            rows[2]["frame"]["data"]["E"] = event_time;
            rows[2]["frame"]["data"]["T"] = transaction_time;
            let (triplet, anchor) = write_triplet(root.path(), &rows);
            let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

            let error = verify_binance_market_tape(vec![sealed]).unwrap_err();
            assert!(error.to_string().contains(expected));
        }
    }

    #[test]
    fn lob_projection_preserves_each_verified_depth_clock() {
        let root = tempdir();
        let mut rows = valid_rows();
        rows.insert(3, depth_row(START_NS + 250_000_000, 102, 101));
        rows[5]["last_update_id"] = json!(102);
        let (triplet, anchor) = write_triplet(root.path(), &rows);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let verified = verify_binance_market_tape(vec![sealed]).unwrap();
        assert_eq!(
            verified.lob_observations(),
            [
                VerifiedBinanceLobObservation {
                    symbol: "BTCUSDT".to_owned(),
                    source_time_ms: (START_NS + 200_000_000) / 1_000_000,
                    received_at_ns: START_NS + 200_000_000,
                },
                VerifiedBinanceLobObservation {
                    symbol: "BTCUSDT".to_owned(),
                    source_time_ms: (START_NS + 250_000_000) / 1_000_000,
                    received_at_ns: START_NS + 250_000_000,
                },
            ]
        );
    }

    #[test]
    fn equal_receive_times_across_segments_remain_in_the_timeline() {
        let root = tempdir();
        let (first, first_anchor) = write_triplet(root.path(), &valid_rows());
        let second_rows = vec![
            depth_row(START_NS + 400_000_000, 102, 101),
            trade_row(START_NS + 450_000_000, 11),
            checkpoint_row(START_NS + 500_000_000, 102),
        ];
        let (second, second_anchor) = write_triplet(root.path(), &second_rows);
        let verified = verify_binance_market_tape(vec![
            seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
        ])
        .unwrap();

        let event_times = verified.replayed_books()[0]
            .events()
            .iter()
            .map(ReplayedBinanceBookEvent::received_at_ns)
            .collect::<Vec<_>>();
        assert_eq!(
            event_times,
            vec![
                START_NS + 100_000_000,
                START_NS + 200_000_000,
                START_NS + 400_000_000,
                START_NS + 400_000_000,
                START_NS + 500_000_000,
            ]
        );
    }

    #[test]
    fn mixed_capture_sessions_are_rejected() {
        let root = tempdir();
        let mut rows = valid_rows();
        rows[0]["session_id"] = json!("session-2");
        let (triplet, anchor) = write_triplet(root.path(), &rows);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let error = verify_binance_market_tape(vec![sealed]).unwrap_err();
        assert!(error.to_string().contains("session_id"));
    }

    #[test]
    fn incomplete_v1_never_returns_a_verified_handle() {
        let root = tempdir();
        let mut rows = valid_rows();
        rows.pop();
        let (triplet, anchor) = write_triplet(root.path(), &rows);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let error = verify_binance_market_tape(vec![sealed]).unwrap_err();
        assert!(error.to_string().contains("replay-safe checkpoint"));
    }

    #[test]
    fn first_segment_checkpoint_cannot_replace_snapshot_seed() {
        let root = tempdir();
        let mut rows = valid_rows();
        rows.retain(|row| !matches!(row["type"].as_str(), Some("snapshot") | Some("diff")));
        let (triplet, anchor) = write_triplet(root.path(), &rows);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let error = verify_binance_market_tape(vec![sealed]).unwrap_err();
        assert!(error.to_string().contains("snapshot seed"));
    }

    #[test]
    fn first_segment_replay_safe_checkpoint_seeds_replay() {
        let root = tempdir();
        let mut rows = valid_rows();
        rows.retain(|row| !matches!(row["type"].as_str(), Some("snapshot") | Some("diff")));
        let rows = with_stream_coverage(rows, &["BTCUSDT"]);
        let (triplet, _) = write_triplet(root.path(), &rows);
        let _ = add_lob_continuity(&triplet, &rows, &["BTCUSDT"]);
        let anchor = add_trade_summaries(&triplet, one_trade_summary("2"));
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let verified =
            verify_binance_market_tape_with_required_trade_and_lob_summaries(vec![sealed]).unwrap();
        assert!(matches!(
            verified.replayed_books()[0].events(),
            [
                ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot { .. }),
                ReplayedBinanceBookEvent::Checkpoint { .. },
            ]
        ));
    }

    #[test]
    fn non_positive_replayed_book_levels_never_return_a_verified_handle() {
        for (field, value) in [(0usize, "0"), (1usize, "-1")] {
            let root = tempdir();
            let mut rows = valid_rows();
            rows[1]["snapshot"]["asks"][0][field] = json!(value);
            rows[4]["asks"][0][field] = json!(value);
            let (triplet, anchor) = write_triplet(root.path(), &rows);
            let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

            let error = verify_binance_market_tape(vec![sealed]).unwrap_err();
            assert!(error.to_string().contains("non-positive"));
        }
    }

    #[test]
    fn verified_stream_coverage_accepts_a_static_declared_symbol() {
        let root = tempdir();
        let rows =
            with_stream_coverage(two_symbol_rows_without_sol_trade(), &["BTCUSDT", "SOLUSDT"]);
        let (triplet, _) = write_triplet_for_symbols(root.path(), &rows, &["BTCUSDT", "SOLUSDT"]);
        let anchor = add_lob_continuity(&triplet, &rows, &["BTCUSDT", "SOLUSDT"]);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        verify_binance_market_tape(vec![sealed]).unwrap_err();

        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();
        let verified =
            verify_binance_market_tape_with_required_lob_continuity(vec![sealed]).unwrap();
        assert_eq!(verified.aggregate_trades().len(), 1);
        assert_eq!(verified.replayed_books().len(), 2);
    }

    #[test]
    fn multi_series_wrapper_verifies_each_capture_session_independently() {
        let root = tempdir();
        let first_rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        let (first, _) = write_triplet(root.path(), &first_rows);
        let first_anchor = add_lob_continuity(&first, &first_rows, &["BTCUSDT"]);

        let mut second_rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        shift_rows(&mut second_rows, 1_000_000_000);
        retag_session(&mut second_rows, "session-2");
        let (second, _) = write_triplet(root.path(), &second_rows);
        let second_anchor = add_lob_continuity(&second, &second_rows, &["BTCUSDT"]);

        let verified = verify_binance_market_tape_series_with_required_lob_continuity(vec![
            seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
        ])
        .unwrap();

        assert_eq!(verified.len(), 2);
        assert_eq!(verified[0].session_id(), "session-1");
        assert_eq!(verified[1].session_id(), "session-2");
        assert!(matches!(
            verified[0].verified().replayed_books()[0].events()[0],
            ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot { .. })
        ));
        assert!(matches!(
            verified[1].verified().replayed_books()[0].events()[0],
            ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot { .. })
        ));
    }

    #[test]
    fn multi_series_wrapper_rejects_a_non_replay_safe_series() {
        let root = tempdir();
        let first_rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        let (first, _) = write_triplet(root.path(), &first_rows);
        let first_anchor = add_lob_continuity(&first, &first_rows, &["BTCUSDT"]);

        let mut second_rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        shift_rows(&mut second_rows, 1_000_000_000);
        retag_session(&mut second_rows, "session-2");
        second_rows
            .iter_mut()
            .find(|row| row["type"] == "checkpoint")
            .unwrap()["replay_safe"] = json!(false);
        let (second, _) = write_triplet(root.path(), &second_rows);
        let second_anchor = add_lob_continuity(&second, &second_rows, &["BTCUSDT"]);

        let error = verify_binance_market_tape_series_with_required_lob_continuity(vec![
            seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
        ])
        .unwrap_err();

        assert!(error.to_string().contains("not replay safe"));
    }

    #[test]
    fn multi_series_wrapper_rejects_overlapping_series_receive_windows() {
        let root = tempdir();
        let first_rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        let (first, _) = write_triplet(root.path(), &first_rows);
        let first_anchor = add_lob_continuity(&first, &first_rows, &["BTCUSDT"]);

        let mut second_rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        shift_rows(&mut second_rows, 250_000_000);
        retag_session(&mut second_rows, "session-2");
        let (second, _) = write_triplet(root.path(), &second_rows);
        let second_anchor = add_lob_continuity(&second, &second_rows, &["BTCUSDT"]);

        let error = verify_binance_market_tape_series_with_required_lob_continuity(vec![
            seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
        ])
        .unwrap_err();

        assert!(error.to_string().contains("receive time moved backwards"));
    }

    #[test]
    fn multi_series_wrapper_rejects_a_reappearing_capture_session() {
        let root = tempdir();
        let first_rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        let (first, _) = write_triplet(root.path(), &first_rows);
        let first_anchor = add_lob_continuity(&first, &first_rows, &["BTCUSDT"]);

        let mut second_rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        shift_rows(&mut second_rows, 1_000_000_000);
        retag_session(&mut second_rows, "session-2");
        let (second, _) = write_triplet(root.path(), &second_rows);
        let second_anchor = add_lob_continuity(&second, &second_rows, &["BTCUSDT"]);

        let mut third_rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        shift_rows(&mut third_rows, 2_000_000_000);
        let (third, _) = write_triplet(root.path(), &third_rows);
        let third_anchor = add_lob_continuity(&third, &third_rows, &["BTCUSDT"]);

        let error = verify_binance_market_tape_series_with_required_lob_continuity(vec![
            seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
            seal_binance_market_tape_triplet(&third, &third_anchor).unwrap(),
        ])
        .unwrap_err();

        assert!(error.to_string().contains("reappeared"));
    }

    #[test]
    fn static_usdm_checkpoint_preserves_the_initial_overlap_bridge_across_segments() {
        let root = tempdir();
        let mut static_checkpoint = checkpoint_row(START_NS + 400_000_000, 100);
        static_checkpoint["bridged"] = json!(false);
        static_checkpoint["bids"][0][1] = json!("1");
        let first_rows = with_stream_coverage(
            vec![
                valid_rows()[0].clone(),
                valid_rows()[1].clone(),
                trade_row(START_NS + 300_000_000, 10),
                static_checkpoint,
            ],
            &["BTCUSDT"],
        );
        let mut overlap = depth_row(START_NS + 1_000_000_000, 105, 90);
        overlap["frame"]["data"]["U"] = json!(95);
        let second_rows = with_stream_coverage(
            vec![
                overlap,
                trade_row(START_NS + 1_100_000_000, 11),
                checkpoint_row(START_NS + 1_200_000_000, 105),
            ],
            &["BTCUSDT"],
        );
        let (first, _) = write_triplet(root.path(), &first_rows);
        let first_anchor = add_lob_continuity(&first, &first_rows, &["BTCUSDT"]);
        let (second, _) = write_triplet(root.path(), &second_rows);
        let second_anchor = add_lob_continuity(&second, &second_rows, &["BTCUSDT"]);
        let verified = verify_binance_market_tape_with_required_lob_continuity(vec![
            seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
        ])
        .unwrap();

        assert_eq!(verified.replayed_books()[0].book.last_update_id, 105);
    }

    #[test]
    fn session_and_exact_stream_coverage_must_match_declared_symbols() {
        let root = tempdir();
        let mut rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        rows.iter_mut()
            .find(|row| row["type"] == "session_start")
            .unwrap()["websocket_streams"] = json!(1);
        let (triplet, anchor) = write_triplet(root.path(), &rows);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();
        assert!(verify_binance_market_tape(vec![sealed])
            .unwrap_err()
            .to_string()
            .contains("stream counts"));

        let mut rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        rows.iter_mut()
            .find(|row| row["type"] == "stream_coverage")
            .unwrap()["shards"][0][0] = json!("ethusdt@depth@100ms");
        let (triplet, anchor) = write_triplet(root.path(), &rows);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();
        assert!(verify_binance_market_tape(vec![sealed])
            .unwrap_err()
            .to_string()
            .contains("does not match declared symbols"));
    }

    #[test]
    fn checkpoint_without_stream_coverage_never_returns_a_verified_handle() {
        let root = tempdir();
        let mut rows = valid_rows();
        rows[4]["stream_coverage_verified"] = json!(false);
        let rows = with_stream_coverage(rows, &["BTCUSDT"]);
        let (triplet, _) = write_triplet(root.path(), &rows);
        let anchor = add_lob_continuity(&triplet, &rows, &["BTCUSDT"]);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let error =
            verify_binance_market_tape_with_required_lob_continuity(vec![sealed]).unwrap_err();
        assert!(error.to_string().contains("checkpoint is not replay safe"));
    }

    #[test]
    fn each_segment_requires_a_real_aggregate_trade() {
        let root = tempdir();
        let first_rows =
            with_stream_coverage(two_symbol_rows_without_sol_trade(), &["BTCUSDT", "SOLUSDT"]);
        let mut sol_diff = depth_row(START_NS + 1_100_000_000, 201, 200);
        sol_diff["frame"]["data"]["s"] = json!("SOLUSDT");
        let mut sol_checkpoint = checkpoint_row(START_NS + 1_400_000_000, 201);
        sol_checkpoint["symbol"] = json!("SOLUSDT");
        let second_rows = with_stream_coverage(
            vec![
                depth_row(START_NS + 1_000_000_000, 102, 101),
                sol_diff,
                checkpoint_row(START_NS + 1_300_000_000, 102),
                sol_checkpoint,
            ],
            &["BTCUSDT", "SOLUSDT"],
        );
        let (first, _) =
            write_triplet_for_symbols(root.path(), &first_rows, &["BTCUSDT", "SOLUSDT"]);
        let first_anchor = add_lob_continuity(&first, &first_rows, &["BTCUSDT", "SOLUSDT"]);
        let (second, _) =
            write_triplet_for_symbols(root.path(), &second_rows, &["BTCUSDT", "SOLUSDT"]);
        let second_anchor = add_lob_continuity(&second, &second_rows, &["BTCUSDT", "SOLUSDT"]);
        let sealed = vec![
            seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
        ];

        let error = verify_binance_market_tape_with_required_lob_continuity(sealed).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("segment is missing aggregate trades"),
            "{error:#}"
        );
    }

    #[test]
    fn complete_v1_returns_read_only_typed_surfaces() {
        let root = tempdir();
        let (triplet, anchor) = write_triplet(root.path(), &valid_rows());
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let verified = verify_binance_market_tape(vec![sealed]).unwrap();
        assert_eq!(verified.segments().len(), 1);
        assert_eq!(verified.segments()[0].start_received_at_ns, START_NS);
        assert_eq!(
            verified.segments()[0].end_received_at_ns,
            START_NS + 400_000_000
        );
        assert_eq!(verified.segments()[0].events, 5);
        assert_eq!(verified.aggregate_trades().len(), 1);
        assert_eq!(
            verified.segments()[0].trade_summaries["BTCUSDT"].quote_volume,
            "201"
        );
        assert_eq!(verified.replayed_books().len(), 1);
        assert_eq!(verified.replayed_books()[0].symbol, "BTCUSDT");
        assert_eq!(verified.replayed_books()[0].book.last_update_id, 101);
        let events = verified.replayed_books()[0].events();
        assert_eq!(
            events
                .iter()
                .map(ReplayedBinanceBookEvent::received_at_ns)
                .collect::<Vec<_>>(),
            vec![
                START_NS + 100_000_000,
                START_NS + 200_000_000,
                START_NS + 400_000_000,
            ]
        );
        assert!(matches!(
            events,
            [
                ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot { .. }),
                ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Diff { .. }),
                ReplayedBinanceBookEvent::Checkpoint { .. },
            ]
        ));
    }

    #[test]
    fn strict_gate_verifier_accepts_complete_v1_without_collecting_surfaces() {
        let root = tempdir();
        let rows = with_stream_coverage(valid_rows(), &["BTCUSDT"]);
        let (triplet, _) = write_triplet(root.path(), &rows);
        let _ = add_trade_summaries(&triplet, one_trade_summary("2"));
        let lob_anchor = add_lob_continuity(&triplet, &rows, &["BTCUSDT"]);
        let sealed = seal_binance_market_tape_triplet(&triplet, &lob_anchor).unwrap();

        verify_binance_market_tape_for_strict_gate(vec![sealed]).unwrap();
    }

    #[test]
    fn strict_gate_verifier_accepts_exact_usdm_lob_datasets_without_trade_rows() {
        let root = tempdir();
        let mut rows = valid_v2_rows()
            .into_iter()
            .filter(|row| {
                !matches!(
                    row["type"].as_str(),
                    Some("agg_trade")
                        | Some("raw_trade")
                        | Some("force_order")
                        | Some("book_ticker")
                        | Some("stale_book_ticker")
                )
            })
            .collect::<Vec<_>>();
        if let Some(session) = rows.iter_mut().find(|row| row["type"] == "session_start") {
            session["stream_types"] = json!(["depth@100ms"]);
        }
        rows = with_stream_coverage_v2(rows, &["BTCUSDT"], &["depth@100ms"]);
        let (triplet, _) = write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &["depth@100ms"]);
        let _ = rewrite_manifest(&triplet, |manifest| {
            manifest["dataset"] = json!(USDM_LOB_SHADOW_DATASET);
            manifest["replay_scope"] = json!(LOB_REPLAY_SCOPE);
            manifest
                .as_object_mut()
                .unwrap()
                .remove("trade_representation");
            manifest
                .as_object_mut()
                .unwrap()
                .remove("price_surface_derivation");
        });
        let anchor = add_lob_continuity(&triplet, &rows, &["BTCUSDT"]);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        verify_binance_market_tape_for_strict_gate(vec![sealed]).unwrap();

        let production_anchor = rewrite_manifest(&triplet, |manifest| {
            manifest["dataset"] = json!(USDM_LOB_DATASET);
        });
        let production =
            seal_binance_market_tape_triplet(&triplet, &production_anchor).unwrap();
        verify_binance_market_tape_for_strict_gate(vec![production]).unwrap();

        let lookalike_anchor = rewrite_manifest(&triplet, |manifest| {
            manifest["dataset"] = json!("usdm_perpetual_top100_lob_rust_shadow_extra");
        });
        let error = seal_binance_market_tape_triplet(&triplet, &lookalike_anchor).unwrap_err();
        assert!(error.to_string().contains("manifest identity"), "{error:#}");
    }

    #[test]
    fn strict_gate_verifier_accepts_historical_usdm_lob_book_ticker_contract() {
        let root = tempdir();
        let mut rows = valid_v2_rows()
            .into_iter()
            .filter(|row| {
                !matches!(
                    row["type"].as_str(),
                    Some("agg_trade") | Some("raw_trade") | Some("force_order")
                )
            })
            .collect::<Vec<_>>();
        if let Some(session) = rows.iter_mut().find(|row| row["type"] == "session_start") {
            session["stream_types"] = json!(["depth@100ms", "bookTicker"]);
        }
        rows = with_stream_coverage_v2(rows, &["BTCUSDT"], &["depth@100ms", "bookTicker"]);
        let (triplet, _) = write_triplet_v2(
            root.path(),
            &rows,
            &["BTCUSDT"],
            &["depth@100ms", "bookTicker"],
        );
        let _ = rewrite_manifest(&triplet, |manifest| {
            manifest["dataset"] = json!("usdm_perpetual_top100_lob");
            manifest["replay_scope"] = json!(LOB_REPLAY_SCOPE);
            manifest
                .as_object_mut()
                .unwrap()
                .remove("trade_representation");
            manifest
                .as_object_mut()
                .unwrap()
                .remove("price_surface_derivation");
        });
        let anchor = add_lob_continuity(&triplet, &rows, &["BTCUSDT"]);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        verify_binance_market_tape_for_strict_gate(vec![sealed]).unwrap();

        let shadow_anchor = rewrite_manifest(&triplet, |manifest| {
            manifest["dataset"] = json!(USDM_LOB_SHADOW_DATASET);
        });
        let error = seal_binance_market_tape_triplet(&triplet, &shadow_anchor).unwrap_err();
        assert!(error.to_string().contains("manifest identity"), "{error:#}");
    }

    #[test]
    fn strict_gate_verifier_rejects_lob_scope_outside_usdm_dataset_identity() {
        let root = tempdir();
        let mut rows = valid_v2_rows()
            .into_iter()
            .filter(|row| {
                !matches!(
                    row["type"].as_str(),
                    Some("agg_trade") | Some("raw_trade") | Some("force_order")
                )
            })
            .collect::<Vec<_>>();
        if let Some(session) = rows.iter_mut().find(|row| row["type"] == "session_start") {
            session["market"] = json!("spot");
            session["stream_types"] = json!(["depth@100ms", "bookTicker"]);
        }
        rows = with_stream_coverage_v2(rows, &["BTCUSDT"], &["depth@100ms", "bookTicker"]);
        let (triplet, _) = write_triplet_v2(
            root.path(),
            &rows,
            &["BTCUSDT"],
            &["depth@100ms", "bookTicker"],
        );
        let anchor = rewrite_manifest(&triplet, |manifest| {
            manifest["market"] = json!("spot");
            manifest["dataset"] = json!(USDM_LOB_DATASET);
            manifest["replay_scope"] = json!(LOB_REPLAY_SCOPE);
            manifest
                .as_object_mut()
                .unwrap()
                .remove("trade_representation");
            manifest
                .as_object_mut()
                .unwrap()
                .remove("price_surface_derivation");
        });
        assert!(
            seal_binance_market_tape_triplet(&triplet, &anchor)
                .unwrap_err()
                .to_string()
                .contains("manifest identity")
        );
    }

    #[test]
    fn aggregate_trade_continuity_survives_segments_without_a_symbol_trade() {
        fn rows(start: u64, symbol: &str, aggregate_trade_id: u64) -> Vec<Value> {
            let mut trade = trade_row(start + 100_000_000, aggregate_trade_id);
            trade["frame"]["stream"] = json!(format!("{}@aggTrade", symbol.to_ascii_lowercase()));
            trade["frame"]["data"]["s"] = json!(symbol);
            vec![
                json!({"schema":"binance.market_tape.v1","received_at_ns":start,"type":"session_start","session_id":"session-1","market":"usdm","symbols":2,"websocket_shards":2,"websocket_streams":4}),
                trade,
            ]
        }

        let root = tempdir();
        let first_rows = rows(START_NS, "BTCUSDT", 10);
        let middle_rows = rows(START_NS + 500_000_000, "SOLUSDT", 20);
        let last_rows = rows(START_NS + 1_000_000_000, "BTCUSDT", 12);
        let (first, first_anchor) =
            write_triplet_for_symbols(root.path(), &first_rows, &["BTCUSDT", "SOLUSDT"]);
        let (middle, middle_anchor) =
            write_triplet_for_symbols(root.path(), &middle_rows, &["BTCUSDT", "SOLUSDT"]);
        let (last, last_anchor) =
            write_triplet_for_symbols(root.path(), &last_rows, &["BTCUSDT", "SOLUSDT"]);

        let mut verifier = BinanceAggregateTradeContinuityVerifier::default();
        verifier
            .observe_segment(seal_binance_market_tape_triplet(&first, &first_anchor).unwrap())
            .unwrap();
        verifier
            .observe_segment(seal_binance_market_tape_triplet(&middle, &middle_anchor).unwrap())
            .unwrap();
        let error = verifier
            .observe_segment(seal_binance_market_tape_triplet(&last, &last_anchor).unwrap())
            .unwrap_err();
        assert!(error.to_string().contains("BTCUSDT aggregate trade gap"));
    }

    #[test]
    fn surface_free_verifier_requires_lob_continuity_mode() {
        let error = verify_binance_market_tape_with_requirements_and_surfaces(
            Vec::new(),
            false,
            false,
            false,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("surface-free market-tape verification requires LOB continuity mode"));
    }

    #[test]
    fn complete_v2_with_new_event_families_verifies() {
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows.insert(5, stale_book_ticker_row(START_NS + 330_000_000));
        let rows = with_stream_coverage_v2(rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let event_count = rows.len() as u64;
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let verified = verify_binance_market_tape(vec![sealed]).unwrap();
        assert_eq!(verified.segments()[0].events, event_count);
        assert_eq!(verified.aggregate_trades().len(), 1);
        assert_eq!(verified.replayed_books()[0].book.last_update_id, 101);
    }

    #[test]
    fn stale_raw_trade_rows_must_declare_symbol_incompleteness() {
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows.insert(5, stale_raw_trade_row(START_NS + 330_000_000));
        let rows = with_stream_coverage_v2(rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let error =
            verify_binance_market_tape(vec![
                seal_binance_market_tape_triplet(&triplet, &anchor).unwrap()
            ])
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("raw-trade incompleteness does not match raw rows"));
    }

    #[test]
    fn raw_trade_continuity_rejects_declared_incomplete_symbols() {
        let root = tempdir();
        let rows = with_stream_coverage_v2(valid_v2_rows(), &["BTCUSDT"], &V2_STREAM_TYPES);
        let (triplet, _) = write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let anchor = rewrite_manifest(&triplet, |manifest| {
            manifest["raw_trade_incomplete_symbols"] = json!(["BTCUSDT"]);
        });
        let error = BinanceRawTradeContinuityVerifier::default()
            .observe_segment(seal_binance_market_tape_triplet(&triplet, &anchor).unwrap())
            .unwrap_err();
        assert!(error.to_string().contains("raw-trade data is incomplete"));
    }

    #[test]
    fn stale_book_ticker_without_transaction_clock_verifies() {
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows.insert(
            5,
            stale_book_ticker_without_transaction_row(START_NS + 330_000_000),
        );
        let rows = with_stream_coverage_v2(rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);

        verify_binance_market_tape(vec![
            seal_binance_market_tape_triplet(&triplet, &anchor).unwrap()
        ])
        .unwrap();

        let root = tempdir();
        let mut invalid_rows = valid_v2_rows();
        let mut invalid = stale_book_ticker_without_transaction_row(START_NS + 330_000_000);
        invalid["event_minus_transaction_ms"] = Value::Null;
        invalid_rows.insert(5, invalid);
        let invalid_rows = with_stream_coverage_v2(invalid_rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &invalid_rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let error =
            verify_binance_market_tape(vec![
                seal_binance_market_tape_triplet(&triplet, &anchor).unwrap()
            ])
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("malformed event_minus_transaction_ms"));
    }

    #[test]
    fn stale_book_ticker_producer_id_must_be_declared() {
        let root = tempdir();
        let mut rows = valid_v2_rows();
        let mut stale = stale_book_ticker_row(START_NS + 330_000_000);
        stale["producer_id"] = json!(2);
        rows.insert(5, stale);
        let rows = with_stream_coverage_v2(rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);

        let error =
            verify_binance_market_tape(vec![
                seal_binance_market_tape_triplet(&triplet, &anchor).unwrap()
            ])
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("outside declared websocket_shards"));
    }

    #[test]
    fn v2_zero_price_raw_trade_is_continuous_but_not_a_valid_trade_surface() {
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows.insert(5, zero_raw_trade_row(START_NS + 330_000_000, 11));
        rows.insert(6, raw_trade_row(START_NS + 335_000_000, 12));
        let rows = with_stream_coverage_v2(rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let manifest: Value =
            serde_json::from_slice(&fs::read(&triplet.manifest).unwrap()).unwrap();
        assert_eq!(manifest["event_types"]["raw_trade"], 2);
        assert_eq!(manifest["event_types"]["raw_trade_zero_price"], 1);

        let mut raw_trade_verifier = BinanceRawTradeContinuityVerifier::default();
        raw_trade_verifier
            .observe_segment(seal_binance_market_tape_triplet(&triplet, &anchor).unwrap())
            .unwrap();
        let verified =
            verify_binance_market_tape(vec![
                seal_binance_market_tape_triplet(&triplet, &anchor).unwrap()
            ])
            .unwrap();
        assert_eq!(verified.segments()[0].events, rows.len() as u64);
        assert_eq!(verified.aggregate_trades().len(), 1);

        let root = tempdir();
        let zero_only_rows = vec![zero_raw_trade_row(START_NS, 1)];
        let (zero_only, zero_only_anchor) =
            write_triplet_v2(root.path(), &zero_only_rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let error = BinanceRawTradeContinuityVerifier::default()
            .observe_segment(
                seal_binance_market_tape_triplet(&zero_only, &zero_only_anchor).unwrap(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("missing raw trades"));
    }

    #[test]
    fn observed_usdm_raw_trade_window_preserves_zero_price_continuity() {
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows.retain(|row| row["type"] != "raw_trade");
        for row in &mut rows {
            if row["symbol"] == "BTCUSDT" {
                row["symbol"] = json!("BTCUSDC");
            }
            if let Some(stream) = row["frame"]["stream"].as_str() {
                row["frame"]["stream"] = json!(stream.replace("btcusdt", "btcusdc"));
            }
            if row["frame"]["data"]["s"] == "BTCUSDT" {
                row["frame"]["data"]["s"] = json!("BTCUSDC");
            }
            if row["frame"]["data"]["o"]["s"] == "BTCUSDT" {
                row["frame"]["data"]["o"]["s"] = json!("BTCUSDC");
            }
        }
        rows.extend([
            observed_window_raw_trade_row(
                1_786_430_318_869_000_000,
                553104311,
                1_786_430_318_864,
                1_786_430_318_864,
                "63841.2",
                "0.013",
                "MARKET",
            ),
            observed_window_raw_trade_row(
                1_786_430_320_185_000_000,
                553104312,
                1_786_430_320_180,
                1_786_430_320_180,
                "0",
                "0",
                "NA",
            ),
            observed_window_raw_trade_row(
                1_786_430_322_211_000_000,
                553104313,
                1_786_430_322_206,
                1_786_430_322_206,
                "0",
                "0",
                "NA",
            ),
            observed_window_raw_trade_row(
                1_786_430_322_678_000_000,
                553104314,
                1_786_430_322_673,
                1_786_430_322_673,
                "0",
                "0",
                "NA",
            ),
            observed_window_raw_trade_row(
                1_786_430_322_755_000_000,
                553104315,
                1_786_430_322_750,
                1_786_430_322_749,
                "63841.2",
                "0.001",
                "MARKET",
            ),
        ]);
        rows.sort_by_key(|row| row["received_at_ns"].as_u64().unwrap());
        let rows = with_stream_coverage_v2(rows, &["BTCUSDC"], &V2_STREAM_TYPES);
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDC"], &V2_STREAM_TYPES);
        BinanceRawTradeContinuityVerifier::default()
            .observe_segment(seal_binance_market_tape_triplet(&triplet, &anchor).unwrap())
            .unwrap();
        let verified =
            verify_binance_market_tape(vec![
                seal_binance_market_tape_triplet(&triplet, &anchor).unwrap()
            ])
            .unwrap();
        assert_eq!(verified.segments()[0].events, rows.len() as u64);
        let manifest: Value =
            serde_json::from_slice(&fs::read(&triplet.manifest).unwrap()).unwrap();
        assert_eq!(manifest["event_types"]["raw_trade"], 2);
        assert_eq!(manifest["event_types"]["raw_trade_zero_price"], 3);
    }

    #[test]
    fn zero_price_raw_trade_is_usdm_v2_only_and_strictly_shaped() {
        const SPOT_STREAM_TYPES: [&str; 4] = ["depth@100ms", "aggTrade", "trade", "bookTicker"];

        let root = tempdir();
        let mut spot_rows = valid_v2_rows();
        spot_rows.retain(|row| row["type"] != "force_order");
        spot_rows[0]["market"] = json!("spot");
        spot_rows[0]["stream_types"] = json!(SPOT_STREAM_TYPES);
        spot_rows.insert(5, zero_raw_trade_row(START_NS + 330_000_000, 11));
        spot_rows.insert(6, raw_trade_row(START_NS + 335_000_000, 12));
        let spot_rows = with_stream_coverage_v2(spot_rows, &["BTCUSDT"], &SPOT_STREAM_TYPES);
        let (spot_triplet, _) =
            write_triplet_v2(root.path(), &spot_rows, &["BTCUSDT"], &SPOT_STREAM_TYPES);
        let spot_anchor = rewrite_manifest(&spot_triplet, |manifest| {
            manifest["market"] = json!("spot");
        });
        assert!(
            verify_binance_market_tape(vec![seal_binance_market_tape_triplet(
                &spot_triplet,
                &spot_anchor
            )
            .unwrap(),])
            .unwrap_err()
            .to_string()
            .contains("USD-M")
        );

        let root = tempdir();
        let mut v1_rows = valid_rows();
        let mut v1_zero = zero_raw_trade_row(START_NS + 350_000_000, 11);
        v1_zero["schema"] = json!(MARKET_TAPE_SCHEMA);
        v1_rows.push(v1_zero);
        v1_rows.sort_by_key(|row| row["received_at_ns"].as_u64().unwrap());
        let (v1_triplet, v1_anchor) = write_triplet(root.path(), &v1_rows);
        assert!(
            verify_binance_market_tape(vec![seal_binance_market_tape_triplet(
                &v1_triplet,
                &v1_anchor
            )
            .unwrap(),])
            .unwrap_err()
            .to_string()
            .contains("raw_trade_zero_price")
        );

        let root = tempdir();
        let mut negative_rows = valid_v2_rows();
        let mut negative = zero_raw_trade_row(START_NS + 330_000_000, 11);
        negative["frame"]["data"]["p"] = json!("-1");
        negative_rows.insert(5, negative);
        let (negative_triplet, negative_anchor) =
            write_triplet_v2(root.path(), &negative_rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        assert!(
            verify_binance_market_tape(vec![seal_binance_market_tape_triplet(
                &negative_triplet,
                &negative_anchor
            )
            .unwrap(),])
            .unwrap_err()
            .to_string()
            .contains("not positive")
        );

        let root = tempdir();
        let mut unknown_rows = valid_v2_rows();
        let mut unknown = zero_raw_trade_row(START_NS + 330_000_000, 11);
        unknown["surprise"] = json!(true);
        unknown_rows.insert(5, unknown);
        let (unknown_triplet, unknown_anchor) =
            write_triplet_v2(root.path(), &unknown_rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        assert!(
            verify_binance_market_tape(vec![seal_binance_market_tape_triplet(
                &unknown_triplet,
                &unknown_anchor
            )
            .unwrap(),])
            .unwrap_err()
            .to_string()
            .contains("unknown field")
        );
    }

    #[test]
    fn v2_rejects_unknown_fields_and_v1_rejects_new_event_types() {
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows[4]["surprise"] = json!(true);
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();
        assert!(verify_binance_market_tape(vec![sealed])
            .unwrap_err()
            .to_string()
            .contains("unknown field"));

        let root = tempdir();
        let mut rows = valid_rows();
        rows.push(v2_schema(raw_trade_row(START_NS + 320_000_000, 10)));
        rows[5]["schema"] = json!(MARKET_TAPE_SCHEMA);
        rows.sort_by_key(|row| row["received_at_ns"].as_u64().unwrap());
        let (triplet, anchor) = write_triplet(root.path(), &rows);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();
        assert!(verify_binance_market_tape(vec![sealed])
            .unwrap_err()
            .to_string()
            .contains("incomplete market-tape event raw_trade"));
    }

    #[test]
    fn v2_session_stream_counts_must_match_declared_stream_types() {
        // websocket_streams no longer equals symbols x declared stream types.
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows[0]["websocket_streams"] = json!(4);
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();
        assert!(verify_binance_market_tape(vec![sealed])
            .unwrap_err()
            .to_string()
            .contains("stream counts"));

        // session_start does not declare its stream-type list.
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows[0].as_object_mut().unwrap().remove("stream_types");
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();
        assert!(verify_binance_market_tape(vec![sealed])
            .unwrap_err()
            .to_string()
            .contains("stream"));

        // session_start stream-type list disagrees with the manifest declaration.
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows[0]["stream_types"] =
            json!(["depth@100ms", "aggTrade", "trade", "bookTicker", "kline"]);
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();
        assert!(verify_binance_market_tape(vec![sealed])
            .unwrap_err()
            .to_string()
            .contains("stream"));

        // A v1 manifest must not carry the v2 stream-type declaration.
        let root = tempdir();
        let (triplet, _) = write_triplet(root.path(), &valid_rows());
        let anchor = rewrite_manifest(&triplet, |manifest| {
            manifest["stream_types"] = json!(["depth@100ms", "aggTrade"]);
        });
        assert!(seal_binance_market_tape_triplet(&triplet, &anchor)
            .unwrap_err()
            .to_string()
            .contains("stream"));

        // A v2 manifest must declare its stream-type list.
        let root = tempdir();
        let rows = valid_v2_rows();
        let (triplet, _) = write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let anchor = rewrite_manifest(&triplet, |manifest| {
            manifest.as_object_mut().unwrap().remove("stream_types");
        });
        assert!(seal_binance_market_tape_triplet(&triplet, &anchor)
            .unwrap_err()
            .to_string()
            .contains("stream"));
    }

    #[test]
    fn v2_stream_coverage_must_cover_declared_stream_types() {
        let root = tempdir();
        let mut rows = with_stream_coverage_v2(valid_v2_rows(), &["BTCUSDT"], &V2_STREAM_TYPES);
        let coverage = rows
            .iter_mut()
            .find(|row| row["type"] == "stream_coverage")
            .unwrap();
        coverage["shards"][1]
            .as_array_mut()
            .unwrap()
            .retain(|stream| stream != "btcusdt@forceOrder");
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        assert!(verify_binance_market_tape(vec![sealed])
            .unwrap_err()
            .to_string()
            .contains("does not match declared symbols"));
    }

    #[test]
    fn v2_raw_trade_gap_is_rejected_within_and_across_segments() {
        // Within one segment.
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows.insert(6, raw_trade_row(START_NS + 330_000_000, 12));
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();
        assert!(verify_binance_market_tape(vec![sealed])
            .unwrap_err()
            .to_string()
            .contains("BTCUSDT raw trade gap"));

        // Across segments, with book and aggregate-trade continuity intact.
        let root = tempdir();
        let (first, first_anchor) = write_triplet_v2(
            root.path(),
            &valid_v2_rows(),
            &["BTCUSDT"],
            &V2_STREAM_TYPES,
        );
        let second_rows = vec![
            v2_schema(depth_row(START_NS + 1_000_000_000, 102, 101)),
            v2_schema(trade_row(START_NS + 1_100_000_000, 11)),
            raw_trade_row(START_NS + 1_150_000_000, 12),
            v2_schema(checkpoint_row(START_NS + 1_200_000_000, 102)),
        ];
        let (second, second_anchor) =
            write_triplet_v2(root.path(), &second_rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let sealed = vec![
            seal_binance_market_tape_triplet(&first, &first_anchor).unwrap(),
            seal_binance_market_tape_triplet(&second, &second_anchor).unwrap(),
        ];
        assert!(verify_binance_market_tape(sealed)
            .unwrap_err()
            .to_string()
            .contains("BTCUSDT raw trade gap"));
    }

    #[test]
    fn v2_force_order_is_usdm_only() {
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows[0]["market"] = json!("spot");
        let (triplet, _) = write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let anchor = rewrite_manifest(&triplet, |manifest| {
            manifest["market"] = json!("spot");
        });
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        assert!(verify_binance_market_tape(vec![sealed])
            .unwrap_err()
            .to_string()
            .contains("USD-M"));
    }

    #[test]
    fn v2_spot_book_ticker_rows_verify_for_spot_market() {
        const SPOT_STREAM_TYPES: [&str; 4] = ["depth@100ms", "aggTrade", "trade", "bookTicker"];
        let root = tempdir();
        let rows = with_stream_coverage_v2(
            vec![
                json!({"schema":MARKET_TAPE_SCHEMA_V2,"received_at_ns":START_NS,"type":"session_start","session_id":"session-1","market":"spot","symbols":1,"websocket_shards":1,"websocket_streams":SPOT_STREAM_TYPES.len(),"stream_types":SPOT_STREAM_TYPES}),
                v2_schema(valid_rows()[1].clone()),
                v2_schema(depth_row(START_NS + 200_000_000, 101, 100)),
                v2_schema(trade_row(START_NS + 300_000_000, 10)),
                raw_trade_row(START_NS + 320_000_000, 10),
                spot_book_ticker_row(START_NS + 340_000_000),
                v2_schema(checkpoint_row(START_NS + 400_000_000, 101)),
            ],
            &["BTCUSDT"],
            &SPOT_STREAM_TYPES,
        );
        let event_count = rows.len() as u64;
        let (triplet, _) = write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &SPOT_STREAM_TYPES);
        let anchor = rewrite_manifest(&triplet, |manifest| {
            manifest["market"] = json!("spot");
        });
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        let verified = verify_binance_market_tape(vec![sealed]).unwrap();
        assert_eq!(verified.segments()[0].events, event_count);
    }

    #[test]
    fn v2_kline_is_a_reserved_name_without_implementation() {
        let root = tempdir();
        let mut rows = valid_v2_rows();
        rows.push(
            json!({"schema":MARKET_TAPE_SCHEMA_V2,"received_at_ns":START_NS + 380_000_000,"type":"kline","session_id":"session-1","frame":{}}),
        );
        let (triplet, anchor) =
            write_triplet_v2(root.path(), &rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let sealed = seal_binance_market_tape_triplet(&triplet, &anchor).unwrap();

        assert!(verify_binance_market_tape(vec![sealed])
            .unwrap_err()
            .to_string()
            .contains("incomplete market-tape event kline"));
    }

    #[test]
    fn raw_trade_continuity_verifier_tracks_v2_segments() {
        let root = tempdir();
        let (first, first_anchor) = write_triplet_v2(
            root.path(),
            &valid_v2_rows(),
            &["BTCUSDT"],
            &V2_STREAM_TYPES,
        );
        let second_rows = vec![
            raw_trade_row(START_NS + 1_000_000_000, 11),
            raw_trade_row(START_NS + 1_100_000_000, 12),
        ];
        let (second, second_anchor) =
            write_triplet_v2(root.path(), &second_rows, &["BTCUSDT"], &V2_STREAM_TYPES);
        let gap_rows = vec![raw_trade_row(START_NS + 2_000_000_000, 14)];
        let (gap, gap_anchor) =
            write_triplet_v2(root.path(), &gap_rows, &["BTCUSDT"], &V2_STREAM_TYPES);

        let mut verifier = BinanceRawTradeContinuityVerifier::default();
        verifier
            .observe_segment(seal_binance_market_tape_triplet(&first, &first_anchor).unwrap())
            .unwrap();
        verifier
            .observe_segment(seal_binance_market_tape_triplet(&second, &second_anchor).unwrap())
            .unwrap();
        let error = verifier
            .observe_segment(seal_binance_market_tape_triplet(&gap, &gap_anchor).unwrap())
            .unwrap_err();
        assert!(error.to_string().contains("BTCUSDT raw trade gap"));

        // The raw-trade verifier fails closed on v1 tapes.
        let root = tempdir();
        let (v1, v1_anchor) = write_triplet(root.path(), &valid_rows());
        let mut verifier = BinanceRawTradeContinuityVerifier::default();
        assert!(verifier
            .observe_segment(seal_binance_market_tape_triplet(&v1, &v1_anchor).unwrap())
            .unwrap_err()
            .to_string()
            .contains("market_tape.v2"));
    }
}
