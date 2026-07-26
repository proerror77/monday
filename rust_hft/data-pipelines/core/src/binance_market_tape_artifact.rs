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
    event_type_allowed, AggregateTrade, AggregateTradeSequenceValidator, AggregateTradeSummary,
    AggregateTradeSummaryBuilder, DepthSourceClock, DepthSourceClockSequenceValidator,
    LobContinuitySummary, LobContinuitySummaryBuilder, AGGREGATE_TRADE_SUMMARY_CONTRACT,
    LOB_CONTINUITY_SUMMARY_CONTRACT, MARKET_TAPE_SCHEMA,
};

const REPLAY_SCOPE: &str =
    "captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs";
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

pub fn verify_binance_market_tape_with_required_trade_and_lob_summaries(
    sealed: Vec<SealedBinanceMarketTapeTriplet>,
) -> Result<VerifiedBinanceMarketTape> {
    verify_binance_market_tape_with_requirements(sealed, true, true)
}

pub fn verify_binance_market_tape_for_strict_gate(
    sealed: Vec<SealedBinanceMarketTapeTriplet>,
) -> Result<()> {
    verify_binance_market_tape_with_requirements_and_surfaces(sealed, true, true, false).map(|_| ())
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

fn verify_binance_market_tape_with_requirements_and_surfaces(
    mut sealed: Vec<SealedBinanceMarketTapeTriplet>,
    require_trade_summaries: bool,
    require_lob_continuity: bool,
    collect_surfaces: bool,
) -> Result<VerifiedBinanceMarketTape> {
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
    let mut highest_received_at_ns = None;
    let mut previous_segment_end_received_at_ns = None;

    for segment in sealed {
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
                "session_start" => {
                    if required_string(raw, "market")? != market.as_str() {
                        bail!("market-tape session market does not match its manifest");
                    }
                    let declared_count = u64::try_from(symbols.len())?;
                    if raw.get("symbols").and_then(Value::as_u64) != Some(declared_count)
                        || raw.get("websocket_shards").and_then(Value::as_u64) == Some(0)
                        || raw
                            .get("websocket_shards")
                            .and_then(Value::as_u64)
                            .is_none()
                        || (require_lob_continuity
                            && raw.get("websocket_streams").and_then(Value::as_u64)
                                != Some(declared_count.saturating_mul(2)))
                        || raw
                            .get("websocket_streams")
                            .is_some_and(|value| value.as_u64() != Some(declared_count * 2))
                    {
                        bail!("market-tape session stream counts do not match its manifest");
                    }
                    session_shard_count = raw.get("websocket_shards").and_then(Value::as_u64);
                }
                "stream_coverage" => {
                    let shard_count = validate_stream_coverage_row(raw, &symbols)?;
                    if coverage_shard_count.replace(shard_count).is_some() {
                        bail!("market-tape segment has duplicate stream coverage evidence");
                    }
                }
                _ => bail!("incomplete market-tape event {event_type}"),
            }
        }
        if counts != segment.manifest.event_types
            || segment.rows.len() as u64 != segment.manifest.events
        {
            bail!("market-tape event counts do not match the manifest");
        }
        if require_lob_continuity && counts.get("agg_trade").copied().unwrap_or(0) == 0 {
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
    trade_representation: String,
    price_surface_derivation: String,
    trade_summary_contract: Option<String>,
    trade_summaries: Option<BTreeMap<String, AggregateTradeSummary>>,
    #[serde(default)]
    lob_continuity: Option<LobContinuitySummary>,
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
    if manifest.schema != MARKET_TAPE_SCHEMA
        || manifest.venue != "binance"
        || manifest.mode != "diff"
        || manifest.replay_scope != REPLAY_SCOPE
        || manifest.trade_representation != TRADE_REPRESENTATION
        || manifest.price_surface_derivation != PRICE_SURFACE_DERIVATION
        || manifest
            .trade_summary_contract
            .as_deref()
            .is_some_and(|contract| contract != AGGREGATE_TRADE_SUMMARY_CONTRACT)
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
    parse_digest(&manifest.sha256)?;
    Market::from_str(&manifest.market).map_err(anyhow::Error::msg)?;
    Ok(())
}

fn validate_manifest_quality(manifest: &TapeManifest, require_stream_coverage: bool) -> Result<()> {
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
        if !complete_event_type(event_type) {
            bail!("incomplete market-tape event {event_type}");
        }
    }
    Ok(())
}

fn complete_event_type(event_type: &str) -> bool {
    event_type_allowed(MARKET_TAPE_SCHEMA, event_type)
        && !matches!(
            event_type,
            "sequence_gap" | "aggregate_trade_gap" | "symbol_excluded"
        )
}

#[rustfmt::skip]
fn allowed_fields(event_type: &str) -> &'static [&'static str] {
    match event_type {
        "session_start" => &["schema", "received_at_ns", "type", "session_id", "market", "symbols", "websocket_shards", "websocket_streams"],
        "stream_coverage" => &["schema", "received_at_ns", "type", "session_id", "shards"],
        "snapshot" => &["schema", "received_at_ns", "type", "session_id", "archived_only", "symbol", "request_started_at_ns", "snapshot"],
        "diff" | "agg_trade" => &["schema", "received_at_ns", "type", "session_id", "archived_only", "frame"],
        "checkpoint" => &["schema", "received_at_ns", "type", "session_id", "symbol", "last_update_id", "synced", "bridged", "continuity_complete", "stream_coverage_verified", "bids", "asks", "reason", "replay_safe"],
        _ => unreachable!("event type checked above"),
    }
}

fn validate_stream_coverage_row(
    raw: &Map<String, Value>,
    symbols: &BTreeSet<String>,
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
    let expected = symbols
        .iter()
        .flat_map(|symbol| {
            let symbol = symbol.to_ascii_lowercase();
            [
                format!("{symbol}@depth@100ms"),
                format!("{symbol}@aggTrade"),
            ]
        })
        .collect::<BTreeSet<_>>();
    if actual != expected {
        bail!("market-tape stream coverage does not match declared symbols");
    }
    Ok(u64::try_from(shards.len())?)
}

fn validate_row<'a>(
    raw: &'a Map<String, Value>,
    manifest: &TapeManifest,
) -> Result<(&'a str, &'a str, u64)> {
    if required_string(raw, "schema")? != MARKET_TAPE_SCHEMA {
        bail!("market-tape row schema mismatch");
    }
    let event_type = required_string(raw, "type")?;
    if !complete_event_type(event_type) {
        bail!("incomplete market-tape event {event_type}");
    }
    if raw
        .get("archived_only")
        .is_some_and(|value| value.as_bool() != Some(false))
    {
        bail!("market-tape contains archived_only data");
    }
    let allowed = allowed_fields(event_type);
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
        let manifest = json!({
            "schema":"binance.market_tape.v1","venue":"binance","market":"usdm","dataset":"usdm_all","shard_id":"all","mode":"diff",
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
}
