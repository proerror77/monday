//! Shared contract for the immutable Binance market tape.

use std::collections::{btree_map::Entry, BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const LEGACY_LOB_TAPE_SCHEMA: &str = "binance.lob_tape.v2";
pub const MARKET_TAPE_SCHEMA: &str = "binance.market_tape.v1";
pub const MARKET_TAPE_SCHEMA_V2: &str = "binance.market_tape.v2";
pub const AGGREGATE_TRADE_SUMMARY_CONTRACT: &str = "binance.aggregate_trade_summary.v1";
pub const LOB_CONTINUITY_SUMMARY_CONTRACT: &str = "binance.lob_continuity.v1";
pub const MAX_SOURCE_LEAD_MS: u64 = 1_000;
pub const MAX_SOURCE_DELAY_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LobContinuitySummary {
    pub contract: String,
    pub capture_session_id: String,
    pub reconnect_boundary: bool,
    pub sequence_gaps: u64,
    pub source_time_rollbacks: u64,
    pub declared_symbol_count: u64,
    pub covered_symbol_count: u64,
    pub missing_symbols: Vec<String>,
    pub symbols: BTreeMap<String, SymbolLobContinuitySummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolLobContinuitySummary {
    pub snapshot_seed_count: u64,
    pub diff_count: u64,
    pub checkpoint_count: u64,
    #[serde(default)]
    pub stream_coverage_verified: bool,
    pub first_update_id: Option<u64>,
    pub last_update_id: Option<u64>,
    pub first_source_time_ms: Option<u64>,
    pub last_source_time_ms: Option<u64>,
    /// Event observation range: WS delivery for diffs, REST completion for snapshots,
    /// and local generation for checkpoints; not a homogeneous receive-latency cohort.
    pub first_received_at_ns: Option<u64>,
    pub last_received_at_ns: Option<u64>,
    /// Venue E to WS-library complete-message delivery; not pure network latency.
    pub min_source_latency_ms: Option<i64>,
    pub max_source_latency_ms: Option<i64>,
    pub min_bid_levels: Option<u64>,
    pub max_bid_levels: Option<u64>,
    pub min_ask_levels: Option<u64>,
    pub max_ask_levels: Option<u64>,
}

#[derive(Debug)]
pub struct LobContinuitySummaryBuilder {
    declared_symbols: BTreeSet<String>,
    capture_session_id: Option<String>,
    reconnect_boundary: bool,
    sequence_gaps: u64,
    source_time_rollbacks: u64,
    symbols: BTreeMap<String, SymbolLobContinuitySummary>,
}

impl LobContinuitySummaryBuilder {
    pub fn new(symbols: impl IntoIterator<Item = String>) -> Result<Self> {
        let declared_symbols = symbols.into_iter().collect::<BTreeSet<_>>();
        if declared_symbols.is_empty()
            || declared_symbols
                .iter()
                .any(|symbol| symbol.is_empty() || symbol != &symbol.to_ascii_uppercase())
        {
            anyhow::bail!("LOB continuity summary requires uppercase declared symbols");
        }
        Ok(Self {
            symbols: declared_symbols
                .iter()
                .cloned()
                .map(|symbol| (symbol, SymbolLobContinuitySummary::default()))
                .collect(),
            declared_symbols,
            capture_session_id: None,
            reconnect_boundary: false,
            sequence_gaps: 0,
            source_time_rollbacks: 0,
        })
    }

    pub fn observe(&mut self, raw: &Map<String, Value>) -> Result<()> {
        let event_type = raw
            .get("type")
            .and_then(Value::as_str)
            .context("LOB continuity row is missing type")?;
        if let Some(session_id) = raw.get("session_id").and_then(Value::as_str) {
            if session_id.is_empty() {
                anyhow::bail!("LOB continuity row has an empty session_id");
            }
            if self
                .capture_session_id
                .as_deref()
                .is_some_and(|expected| expected != session_id)
            {
                anyhow::bail!("LOB continuity segment contains mixed session_id values");
            }
            self.capture_session_id
                .get_or_insert_with(|| session_id.to_owned());
        }
        let received_at_ns = raw
            .get("received_at_ns")
            .and_then(Value::as_u64)
            .context("LOB continuity row is missing received_at_ns")?;
        match event_type {
            "session_start" => self.reconnect_boundary = true,
            "snapshot" => {
                let symbol = self.row_symbol(raw)?;
                let snapshot = raw
                    .get("snapshot")
                    .and_then(Value::as_object)
                    .context("LOB continuity snapshot has no payload")?;
                let (bid_levels, ask_levels) = book_depth(snapshot)?;
                let summary = self.symbol_mut(&symbol)?;
                summary.snapshot_seed_count = summary
                    .snapshot_seed_count
                    .checked_add(1)
                    .context("LOB snapshot seed count overflow")?;
                summary.observe_received_at(received_at_ns);
                summary.observe_depth(bid_levels, ask_levels);
            }
            "diff" => {
                let clock = DepthSourceClock::from_archived_event(raw, received_at_ns)?;
                let summary = self.symbol_mut(&clock.symbol)?;
                summary.diff_count = summary
                    .diff_count
                    .checked_add(1)
                    .context("LOB diff count overflow")?;
                summary.first_update_id.get_or_insert(clock.first_update_id);
                summary.last_update_id = Some(clock.final_update_id);
                summary
                    .first_source_time_ms
                    .get_or_insert(clock.event_time_ms);
                summary.last_source_time_ms = Some(clock.event_time_ms);
                summary.observe_received_at(received_at_ns);
                summary.observe_latency(venue_to_userspace_ws_message_latency_ms(
                    received_at_ns,
                    clock.event_time_ms,
                )?);
            }
            "checkpoint" => {
                let symbol = self.row_symbol(raw)?;
                let (bid_levels, ask_levels) = book_depth(raw)?;
                let is_seed = raw.get("reason").and_then(Value::as_str) == Some("segment_open");
                let summary = self.symbol_mut(&symbol)?;
                if is_seed {
                    summary.snapshot_seed_count = summary
                        .snapshot_seed_count
                        .checked_add(1)
                        .context("LOB snapshot seed count overflow")?;
                }
                summary.checkpoint_count = summary
                    .checkpoint_count
                    .checked_add(1)
                    .context("LOB checkpoint count overflow")?;
                summary.stream_coverage_verified |=
                    raw.get("stream_coverage_verified").and_then(Value::as_bool) == Some(true);
                summary.observe_received_at(received_at_ns);
                summary.observe_depth(bid_levels, ask_levels);
            }
            "sequence_gap" => {
                self.sequence_gaps = self
                    .sequence_gaps
                    .checked_add(1)
                    .context("LOB sequence gap count overflow")?;
                if raw
                    .get("error")
                    .and_then(Value::as_str)
                    .is_some_and(|error| error.contains("source-time rollback"))
                {
                    self.source_time_rollbacks = self
                        .source_time_rollbacks
                        .checked_add(1)
                        .context("LOB source-time rollback count overflow")?;
                }
            }
            // Trade, ticker, and liquidation families carry no LOB continuity
            // state; they are validated by their own sequence/clock contracts.
            "stream_coverage"
            | "agg_trade"
            | "raw_trade"
            | "raw_trade_zero_price"
            | "stale_raw_trade"
            | "book_ticker"
            | "stale_book_ticker"
            | "force_order" => {}
            _ => {}
        }
        Ok(())
    }

    pub fn finish(self) -> Result<LobContinuitySummary> {
        let capture_session_id = self
            .capture_session_id
            .context("LOB continuity segment has no capture session")?;
        let missing_symbols = self
            .symbols
            .iter()
            .filter(|(_, summary)| {
                summary.snapshot_seed_count == 0
                    || summary.checkpoint_count == 0
                    || (summary.diff_count == 0 && !summary.stream_coverage_verified)
            })
            .map(|(symbol, _)| symbol.clone())
            .collect::<Vec<_>>();
        let covered_symbol_count =
            self.declared_symbols
                .len()
                .checked_sub(missing_symbols.len())
                .context("LOB covered symbol count underflow")? as u64;
        Ok(LobContinuitySummary {
            contract: LOB_CONTINUITY_SUMMARY_CONTRACT.to_owned(),
            capture_session_id,
            reconnect_boundary: self.reconnect_boundary,
            sequence_gaps: self.sequence_gaps,
            source_time_rollbacks: self.source_time_rollbacks,
            declared_symbol_count: self.declared_symbols.len() as u64,
            covered_symbol_count,
            missing_symbols,
            symbols: self.symbols,
        })
    }

    fn row_symbol(&self, raw: &Map<String, Value>) -> Result<String> {
        let symbol = raw
            .get("symbol")
            .and_then(Value::as_str)
            .context("LOB continuity row is missing symbol")?
            .to_ascii_uppercase();
        if !self.declared_symbols.contains(&symbol) {
            anyhow::bail!("LOB continuity row symbol is outside its declared scope");
        }
        Ok(symbol)
    }

    fn symbol_mut(&mut self, symbol: &str) -> Result<&mut SymbolLobContinuitySummary> {
        self.symbols
            .get_mut(symbol)
            .context("LOB continuity row symbol is outside its declared scope")
    }
}

impl SymbolLobContinuitySummary {
    fn observe_received_at(&mut self, received_at_ns: u64) {
        self.first_received_at_ns.get_or_insert(received_at_ns);
        self.last_received_at_ns = Some(received_at_ns);
    }

    fn observe_latency(&mut self, latency_ms: i64) {
        self.min_source_latency_ms = Some(
            self.min_source_latency_ms
                .map_or(latency_ms, |current| current.min(latency_ms)),
        );
        self.max_source_latency_ms = Some(
            self.max_source_latency_ms
                .map_or(latency_ms, |current| current.max(latency_ms)),
        );
    }

    fn observe_depth(&mut self, bid_levels: u64, ask_levels: u64) {
        self.min_bid_levels = Some(
            self.min_bid_levels
                .map_or(bid_levels, |current| current.min(bid_levels)),
        );
        self.max_bid_levels = Some(
            self.max_bid_levels
                .map_or(bid_levels, |current| current.max(bid_levels)),
        );
        self.min_ask_levels = Some(
            self.min_ask_levels
                .map_or(ask_levels, |current| current.min(ask_levels)),
        );
        self.max_ask_levels = Some(
            self.max_ask_levels
                .map_or(ask_levels, |current| current.max(ask_levels)),
        );
    }
}

fn book_depth(raw: &Map<String, Value>) -> Result<(u64, u64)> {
    let bids = raw
        .get("bids")
        .and_then(Value::as_array)
        .context("LOB book is missing bids")?;
    let asks = raw
        .get("asks")
        .and_then(Value::as_array)
        .context("LOB book is missing asks")?;
    Ok((bids.len() as u64, asks.len() as u64))
}

fn venue_to_userspace_ws_message_latency_ms(
    received_at_ns: u64,
    source_time_ms: u64,
) -> Result<i64> {
    let received_at_ms = received_at_ns / 1_000_000;
    let latency = i128::from(received_at_ms) - i128::from(source_time_ms);
    i64::try_from(latency).context("LOB source latency exceeds its numeric range")
}

pub fn supported_schema(schema: &str) -> bool {
    matches!(
        schema,
        LEGACY_LOB_TAPE_SCHEMA | MARKET_TAPE_SCHEMA | MARKET_TAPE_SCHEMA_V2
    )
}

pub fn market_tape_schema(schema: &str) -> bool {
    matches!(schema, MARKET_TAPE_SCHEMA | MARKET_TAPE_SCHEMA_V2)
}

pub fn event_type_allowed(schema: &str, event_type: &str) -> bool {
    match schema {
        LEGACY_LOB_TAPE_SCHEMA => matches!(
            event_type,
            "session_start"
                | "snapshot"
                | "diff"
                | "checkpoint"
                | "sequence_gap"
                | "symbol_excluded"
        ),
        // "kline" is a reserved event type name only; it has no implementation yet.
        MARKET_TAPE_SCHEMA => matches!(
            event_type,
            "session_start"
                | "stream_coverage"
                | "snapshot"
                | "diff"
                | "checkpoint"
                | "sequence_gap"
                | "symbol_excluded"
                | "agg_trade"
                | "aggregate_trade_gap"
        ),
        MARKET_TAPE_SCHEMA_V2 => matches!(
            event_type,
            "session_start"
                | "stream_coverage"
                | "snapshot"
                | "diff"
                | "checkpoint"
                | "sequence_gap"
                | "symbol_excluded"
                | "agg_trade"
                | "aggregate_trade_gap"
                | "raw_trade"
                | "raw_trade_zero_price"
                | "stale_raw_trade"
                | "book_ticker"
                | "stale_book_ticker"
                | "force_order"
        ),
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthSourceClock {
    pub symbol: String,
    pub first_update_id: u64,
    pub final_update_id: u64,
    pub previous_final_update_id: Option<u64>,
    pub event_time_ms: u64,
    pub transaction_time_ms: Option<u64>,
    /// WS-library complete-message delivery timestamp in userspace; not kernel or NIC RX.
    pub received_at_ns: u64,
}

impl DepthSourceClock {
    pub fn from_archived_event(raw: &Map<String, Value>, received_at_ns: u64) -> Result<Self> {
        Self::from_frame(
            raw.get("frame").context("depth event has no frame")?,
            received_at_ns,
        )
    }

    pub fn from_frame(frame: &Value, received_at_ns: u64) -> Result<Self> {
        let data = frame.get("data").unwrap_or(frame);
        if data.get("e").and_then(Value::as_str) != Some("depthUpdate") {
            anyhow::bail!("depth frame has the wrong event identity");
        }
        let symbol = required_string(data, "s", "depth")?.to_ascii_uppercase();
        validate_stream_identity(frame, &symbol, "depth")?;
        let first_update_id = required_u64(data, "U", "depth")?;
        let final_update_id = required_u64(data, "u", "depth")?;
        if first_update_id > final_update_id {
            anyhow::bail!("depth update id range is reversed");
        }
        let previous_final_update_id = optional_u64(data, "pu", "depth")?;
        let event_time_ms = required_u64(data, "E", "depth")?;
        let transaction_time_ms = optional_u64(data, "T", "depth")?;
        if transaction_time_ms
            .is_some_and(|transaction_time_ms| transaction_time_ms > event_time_ms)
        {
            anyhow::bail!("depth source clocks are reversed");
        }
        validate_receive_clock(event_time_ms, received_at_ns, "depth E")?;
        Ok(Self {
            symbol,
            first_update_id,
            final_update_id,
            previous_final_update_id,
            event_time_ms,
            transaction_time_ms,
            received_at_ns,
        })
    }
}

#[derive(Debug, Default)]
pub struct DepthSourceClockSequenceValidator {
    last: HashMap<String, (u64, Option<u64>, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthSequenceGap {
    pub symbol: String,
    pub expected: u64,
    pub received: u64,
}

impl std::fmt::Display for DepthSequenceGap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} depth sequence gap expected={} received={}",
            self.symbol, self.expected, self.received
        )
    }
}

impl std::error::Error for DepthSequenceGap {}

impl DepthSourceClockSequenceValidator {
    pub fn observe(&mut self, clock: &DepthSourceClock) -> Result<()> {
        if let Some((previous_event_time, previous_transaction_time, previous_final_update_id)) =
            self.last.get(&clock.symbol).copied()
        {
            let expected_update_id = previous_final_update_id
                .checked_add(1)
                .context("depth update id overflow")?;
            if let Some(reported_previous_id) = clock.previous_final_update_id {
                if reported_previous_id > previous_final_update_id {
                    return Err(DepthSequenceGap {
                        symbol: clock.symbol.clone(),
                        expected: previous_final_update_id,
                        received: reported_previous_id,
                    }
                    .into());
                }
                if reported_previous_id < previous_final_update_id {
                    anyhow::bail!("{} depth previous-update rollback", clock.symbol);
                }
            } else if clock.first_update_id > expected_update_id {
                return Err(DepthSequenceGap {
                    symbol: clock.symbol.clone(),
                    expected: expected_update_id,
                    received: clock.first_update_id,
                }
                .into());
            }
            if clock.final_update_id < expected_update_id {
                anyhow::bail!("{} depth sequence rollback", clock.symbol);
            }
            if clock.event_time_ms < previous_event_time
                || previous_transaction_time
                    .zip(clock.transaction_time_ms)
                    .is_some_and(|(previous, current)| current < previous)
            {
                anyhow::bail!("{} depth source-time rollback", clock.symbol);
            }
        }
        self.last.insert(
            clock.symbol.clone(),
            (
                clock.event_time_ms,
                clock.transaction_time_ms,
                clock.final_update_id,
            ),
        );
        Ok(())
    }

    pub fn reset_symbol(&mut self, symbol: &str) {
        self.last.remove(symbol);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateTrade {
    pub symbol: String,
    pub aggregate_trade_id: u64,
    pub first_trade_id: u64,
    pub last_trade_id: u64,
    pub price: Decimal,
    pub quantity: Decimal,
    pub event_time_ms: u64,
    pub trade_time_ms: u64,
    pub is_buyer_maker: bool,
    /// WS-library complete-message delivery timestamp in userspace; not kernel or NIC RX.
    pub received_at_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateTradeSummary {
    pub aggregate_trade_count: u64,
    pub venue_trade_count: u64,
    pub base_volume: String,
    pub quote_volume: String,
    pub buyer_aggressor_base_volume: String,
    pub buyer_aggressor_quote_volume: String,
    pub seller_aggressor_base_volume: String,
    pub seller_aggressor_quote_volume: String,
    pub vwap: String,
    pub first_event_time_ms: u64,
    pub last_event_time_ms: u64,
    pub first_trade_time_ms: u64,
    pub last_trade_time_ms: u64,
    pub first_received_at_ns: u64,
    pub last_received_at_ns: u64,
    pub first_aggregate_trade_id: u64,
    pub last_aggregate_trade_id: u64,
    pub first_trade_id: u64,
    pub last_trade_id: u64,
}

#[derive(Debug, Clone)]
struct SymbolTradeAccumulator {
    aggregate_trade_count: u64,
    venue_trade_count: u64,
    base_volume: Decimal,
    quote_volume: Decimal,
    buyer_aggressor_base_volume: Decimal,
    buyer_aggressor_quote_volume: Decimal,
    seller_aggressor_base_volume: Decimal,
    seller_aggressor_quote_volume: Decimal,
    first_event_time_ms: u64,
    last_event_time_ms: u64,
    first_trade_time_ms: u64,
    last_trade_time_ms: u64,
    first_received_at_ns: u64,
    last_received_at_ns: u64,
    first_aggregate_trade_id: u64,
    last_aggregate_trade_id: u64,
    first_trade_id: u64,
    last_trade_id: u64,
}

impl SymbolTradeAccumulator {
    fn new(trade: &AggregateTrade) -> Result<Self> {
        let quote_volume = trade
            .price
            .checked_mul(trade.quantity)
            .context("aggregate trade quote volume overflow")?;
        let venue_trade_count = trade
            .last_trade_id
            .checked_sub(trade.first_trade_id)
            .and_then(|count| count.checked_add(1))
            .context("aggregate trade venue trade count overflow")?;
        let (buyer_base, buyer_quote, seller_base, seller_quote) = if trade.is_buyer_maker {
            (Decimal::ZERO, Decimal::ZERO, trade.quantity, quote_volume)
        } else {
            (trade.quantity, quote_volume, Decimal::ZERO, Decimal::ZERO)
        };
        Ok(Self {
            aggregate_trade_count: 1,
            venue_trade_count,
            base_volume: trade.quantity,
            quote_volume,
            buyer_aggressor_base_volume: buyer_base,
            buyer_aggressor_quote_volume: buyer_quote,
            seller_aggressor_base_volume: seller_base,
            seller_aggressor_quote_volume: seller_quote,
            first_event_time_ms: trade.event_time_ms,
            last_event_time_ms: trade.event_time_ms,
            first_trade_time_ms: trade.trade_time_ms,
            last_trade_time_ms: trade.trade_time_ms,
            first_received_at_ns: trade.received_at_ns,
            last_received_at_ns: trade.received_at_ns,
            first_aggregate_trade_id: trade.aggregate_trade_id,
            last_aggregate_trade_id: trade.aggregate_trade_id,
            first_trade_id: trade.first_trade_id,
            last_trade_id: trade.last_trade_id,
        })
    }

    fn observe(&mut self, trade: &AggregateTrade) -> Result<()> {
        let quote_volume = trade
            .price
            .checked_mul(trade.quantity)
            .context("aggregate trade quote volume overflow")?;
        let venue_trade_count = trade
            .last_trade_id
            .checked_sub(trade.first_trade_id)
            .and_then(|count| count.checked_add(1))
            .context("aggregate trade venue trade count overflow")?;
        self.aggregate_trade_count = self
            .aggregate_trade_count
            .checked_add(1)
            .context("aggregate trade count overflow")?;
        self.venue_trade_count = self
            .venue_trade_count
            .checked_add(venue_trade_count)
            .context("venue trade count overflow")?;
        self.base_volume = self
            .base_volume
            .checked_add(trade.quantity)
            .context("aggregate trade base volume overflow")?;
        self.quote_volume = self
            .quote_volume
            .checked_add(quote_volume)
            .context("aggregate trade quote volume overflow")?;
        let (base, quote) = if trade.is_buyer_maker {
            (
                &mut self.seller_aggressor_base_volume,
                &mut self.seller_aggressor_quote_volume,
            )
        } else {
            (
                &mut self.buyer_aggressor_base_volume,
                &mut self.buyer_aggressor_quote_volume,
            )
        };
        *base = base
            .checked_add(trade.quantity)
            .context("aggregate trade aggressor base volume overflow")?;
        *quote = quote
            .checked_add(quote_volume)
            .context("aggregate trade aggressor quote volume overflow")?;
        self.last_event_time_ms = trade.event_time_ms;
        self.last_trade_time_ms = trade.trade_time_ms;
        self.last_received_at_ns = trade.received_at_ns;
        self.last_aggregate_trade_id = trade.aggregate_trade_id;
        self.last_trade_id = trade.last_trade_id;
        Ok(())
    }

    fn finish(self) -> Result<AggregateTradeSummary> {
        let vwap = self
            .quote_volume
            .checked_div(self.base_volume)
            .context("aggregate trade VWAP division failed")?;
        Ok(AggregateTradeSummary {
            aggregate_trade_count: self.aggregate_trade_count,
            venue_trade_count: self.venue_trade_count,
            base_volume: decimal_string(self.base_volume),
            quote_volume: decimal_string(self.quote_volume),
            buyer_aggressor_base_volume: decimal_string(self.buyer_aggressor_base_volume),
            buyer_aggressor_quote_volume: decimal_string(self.buyer_aggressor_quote_volume),
            seller_aggressor_base_volume: decimal_string(self.seller_aggressor_base_volume),
            seller_aggressor_quote_volume: decimal_string(self.seller_aggressor_quote_volume),
            vwap: decimal_string(vwap),
            first_event_time_ms: self.first_event_time_ms,
            last_event_time_ms: self.last_event_time_ms,
            first_trade_time_ms: self.first_trade_time_ms,
            last_trade_time_ms: self.last_trade_time_ms,
            first_received_at_ns: self.first_received_at_ns,
            last_received_at_ns: self.last_received_at_ns,
            first_aggregate_trade_id: self.first_aggregate_trade_id,
            last_aggregate_trade_id: self.last_aggregate_trade_id,
            first_trade_id: self.first_trade_id,
            last_trade_id: self.last_trade_id,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct AggregateTradeSummaryBuilder {
    symbols: BTreeMap<String, SymbolTradeAccumulator>,
}

impl AggregateTradeSummaryBuilder {
    pub fn observe(&mut self, trade: &AggregateTrade) -> Result<()> {
        match self.symbols.entry(trade.symbol.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(SymbolTradeAccumulator::new(trade)?);
            }
            Entry::Occupied(mut entry) => entry.get_mut().observe(trade)?,
        }
        Ok(())
    }

    pub fn finish(self) -> Result<BTreeMap<String, AggregateTradeSummary>> {
        self.symbols
            .into_iter()
            .map(|(symbol, summary)| Ok((symbol, summary.finish()?)))
            .collect()
    }
}

fn decimal_string(value: Decimal) -> String {
    value.normalize().to_string()
}

impl AggregateTrade {
    pub fn from_archived_event(raw: &Map<String, Value>, received_at_ns: u64) -> Result<Self> {
        Self::from_frame(
            raw.get("frame")
                .context("aggregate trade event has no frame")?,
            received_at_ns,
        )
    }

    pub fn from_frame(frame: &Value, received_at_ns: u64) -> Result<Self> {
        let data = frame.get("data").unwrap_or(frame);
        if data.get("e").and_then(Value::as_str) != Some("aggTrade") {
            anyhow::bail!("aggregate trade frame has the wrong event identity");
        }
        let symbol = required_string(data, "s", "aggregate trade")?.to_ascii_uppercase();
        validate_stream_identity(frame, &symbol, "aggTrade")?;
        let aggregate_trade_id = required_u64(data, "a", "aggregate trade")?;
        let first_trade_id = required_u64(data, "f", "aggregate trade")?;
        let last_trade_id = required_u64(data, "l", "aggregate trade")?;
        if first_trade_id > last_trade_id {
            anyhow::bail!("aggregate trade id range is reversed");
        }
        let price = positive_decimal(data, "p", "aggregate trade")?;
        let quantity = positive_decimal(data, "q", "aggregate trade")?;
        let event_time_ms = required_u64(data, "E", "aggregate trade")?;
        let trade_time_ms = required_u64(data, "T", "aggregate trade")?;
        let is_buyer_maker = data
            .get("m")
            .and_then(Value::as_bool)
            .context("aggregate trade maker side is missing")?;
        if trade_time_ms > event_time_ms {
            anyhow::bail!("aggregate trade source clocks are reversed");
        }
        validate_trade_clocks(
            event_time_ms,
            trade_time_ms,
            received_at_ns,
            "aggregate trade",
        )?;
        Ok(Self {
            symbol,
            aggregate_trade_id,
            first_trade_id,
            last_trade_id,
            price,
            quantity,
            event_time_ms,
            trade_time_ms,
            is_buyer_maker,
            received_at_ns,
        })
    }
}

#[derive(Debug, Default)]
pub struct AggregateTradeSequenceValidator {
    last: HashMap<String, (u64, u64, u64)>,
}

impl AggregateTradeSequenceValidator {
    pub fn observe(&mut self, trade: &AggregateTrade) -> Result<()> {
        if let Some((previous_id, previous_event_time, previous_trade_time)) =
            self.last.get(&trade.symbol).copied()
        {
            let expected = previous_id
                .checked_add(1)
                .context("aggregate trade id overflow")?;
            if trade.aggregate_trade_id != expected {
                anyhow::bail!(
                    "{} aggregate trade gap expected={} received={}",
                    trade.symbol,
                    expected,
                    trade.aggregate_trade_id
                );
            }
            if trade.event_time_ms < previous_event_time
                || trade.trade_time_ms < previous_trade_time
            {
                anyhow::bail!("{} aggregate trade source-time rollback", trade.symbol);
            }
        }
        self.last.insert(
            trade.symbol.clone(),
            (
                trade.aggregate_trade_id,
                trade.event_time_ms,
                trade.trade_time_ms,
            ),
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTrade {
    pub symbol: String,
    pub trade_id: u64,
    pub price: Decimal,
    pub quantity: Decimal,
    pub event_time_ms: u64,
    pub trade_time_ms: u64,
    pub is_buyer_maker: bool,
    /// WS-library complete-message delivery timestamp in userspace; not kernel or NIC RX.
    pub received_at_ns: u64,
}

impl RawTrade {
    pub fn from_archived_event(raw: &Map<String, Value>, received_at_ns: u64) -> Result<Self> {
        Self::from_frame(
            raw.get("frame").context("raw trade event has no frame")?,
            received_at_ns,
        )
    }

    pub fn from_frame(frame: &Value, received_at_ns: u64) -> Result<Self> {
        Self::from_frame_with_zero_price(frame, received_at_ns, false, false)
    }

    /// Parse a structurally valid raw trade while allowing a stale source
    /// clock so the collector can audit and drop it without terminating the
    /// websocket producer.
    pub fn from_frame_allow_stale(frame: &Value, received_at_ns: u64) -> Result<Self> {
        Self::from_frame_with_zero_price(frame, received_at_ns, false, true)
    }

    pub fn from_zero_price_frame(frame: &Value, received_at_ns: u64) -> Result<Self> {
        Self::from_frame_with_zero_price(frame, received_at_ns, true, false)
    }

    pub fn from_zero_price_frame_allow_stale(frame: &Value, received_at_ns: u64) -> Result<Self> {
        Self::from_frame_with_zero_price(frame, received_at_ns, true, true)
    }

    fn from_frame_with_zero_price(
        frame: &Value,
        received_at_ns: u64,
        allow_zero_price: bool,
        allow_stale: bool,
    ) -> Result<Self> {
        let data = frame.get("data").unwrap_or(frame);
        if data.get("e").and_then(Value::as_str) != Some("trade") {
            anyhow::bail!("raw trade frame has the wrong event identity");
        }
        let symbol = required_string(data, "s", "raw trade")?.to_ascii_uppercase();
        validate_stream_identity(frame, &symbol, "trade")?;
        let trade_id = required_u64(data, "t", "raw trade")?;
        let price = required_string(data, "p", "raw trade")?
            .parse::<Decimal>()
            .with_context(|| "raw trade field p is not decimal")?;
        if price < Decimal::ZERO || (!allow_zero_price && price == Decimal::ZERO) {
            anyhow::bail!("raw trade field p is not positive: {price}");
        }
        if allow_zero_price && price != Decimal::ZERO {
            anyhow::bail!("raw trade zero-price field p is not zero");
        }
        let quantity = if allow_zero_price {
            // Accept only the observed public USD-M sentinel shapes; this is not
            // a general non-positive-price fallback.
            let quantity = required_string(data, "q", "raw trade")?
                .parse::<Decimal>()
                .with_context(|| "raw trade field q is not decimal")?;
            if quantity != Decimal::ZERO {
                anyhow::bail!("raw trade zero-price field q is not zero: {quantity}");
            }
            let execution_type = required_string(data, "X", "raw trade")?;
            if !matches!(execution_type, "NA" | "INSURANCE_FUND") {
                anyhow::bail!("raw trade zero-price field X is unsupported: {execution_type}");
            }
            if required_u64(data, "st", "raw trade")? != 1 {
                anyhow::bail!("raw trade zero-price field st is not 1");
            }
            quantity
        } else {
            positive_decimal(data, "q", "raw trade")?
        };
        let event_time_ms = required_u64(data, "E", "raw trade")?;
        let trade_time_ms = required_u64(data, "T", "raw trade")?;
        let is_buyer_maker = data
            .get("m")
            .and_then(Value::as_bool)
            .context("raw trade maker side is missing")?;
        if trade_time_ms > event_time_ms {
            anyhow::bail!("raw trade source clocks are reversed");
        }
        if allow_stale {
            validate_receive_clock_without_delay(event_time_ms, received_at_ns, "raw trade E")?;
        } else {
            validate_trade_clocks(event_time_ms, trade_time_ms, received_at_ns, "raw trade")?;
        }
        Ok(Self {
            symbol,
            trade_id,
            price,
            quantity,
            event_time_ms,
            trade_time_ms,
            is_buyer_maker,
            received_at_ns,
        })
    }
}

#[derive(Debug, Default)]
pub struct RawTradeSequenceValidator {
    last: HashMap<String, (u64, u64, u64)>,
}

impl RawTradeSequenceValidator {
    pub fn observe(&mut self, trade: &RawTrade) -> Result<()> {
        if let Some((previous_id, previous_event_time, previous_trade_time)) =
            self.last.get(&trade.symbol).copied()
        {
            let expected = previous_id
                .checked_add(1)
                .context("raw trade id overflow")?;
            if trade.trade_id != expected {
                anyhow::bail!(
                    "{} raw trade gap expected={} received={}",
                    trade.symbol,
                    expected,
                    trade.trade_id
                );
            }
            if trade.event_time_ms < previous_event_time
                || trade.trade_time_ms < previous_trade_time
            {
                anyhow::bail!("{} raw trade source-time rollback", trade.symbol);
            }
        }
        self.last.insert(
            trade.symbol.clone(),
            (trade.trade_id, trade.event_time_ms, trade.trade_time_ms),
        );
        Ok(())
    }

    /// Advance over one or more explicitly audited stale trades without
    /// admitting their payloads to the normal raw-trade tape or applying their
    /// source clocks.
    pub fn observe_after_stale_range(
        &mut self,
        trade: &RawTrade,
        stale_start_id: u64,
        stale_end_id: u64,
    ) -> Result<()> {
        if stale_end_id < stale_start_id {
            anyhow::bail!("raw trade stale id range is reversed");
        }
        let Some((previous_id, previous_event_time, previous_trade_time)) =
            self.last.get(&trade.symbol).copied()
        else {
            return self.observe(trade);
        };
        let expected_stale = previous_id
            .checked_add(1)
            .context("raw trade id overflow")?;
        let expected_trade = stale_end_id
            .checked_add(1)
            .context("raw trade id overflow")?;
        if stale_start_id != expected_stale || trade.trade_id != expected_trade {
            anyhow::bail!(
                "{} raw trade gap expected={} received={}",
                trade.symbol,
                expected_stale,
                trade.trade_id
            );
        }
        if trade.event_time_ms < previous_event_time || trade.trade_time_ms < previous_trade_time {
            anyhow::bail!("{} raw trade source-time rollback", trade.symbol);
        }
        self.last.insert(
            trade.symbol.clone(),
            (trade.trade_id, trade.event_time_ms, trade.trade_time_ms),
        );
        Ok(())
    }

    pub fn observe_after_stale(&mut self, trade: &RawTrade, stale_trade_id: u64) -> Result<()> {
        self.observe_after_stale_range(trade, stale_trade_id, stale_trade_id)
    }
}

/// Best bid/ask ticker; carries an update id but no sequence guarantee, so only
/// source/receive clock bounds and payload sanity are enforced. Spot frames
/// carry no event identity or source clocks (`e`/`E`/`T` are USD-M only), so
/// for them `event_time_ms` falls back to the receive clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookTicker {
    pub symbol: String,
    pub update_id: u64,
    pub best_bid_price: Decimal,
    pub best_bid_quantity: Decimal,
    pub best_ask_price: Decimal,
    pub best_ask_quantity: Decimal,
    pub event_time_ms: u64,
    /// WS-library complete-message delivery timestamp in userspace; not kernel or NIC RX.
    pub received_at_ns: u64,
}

impl BookTicker {
    pub fn from_archived_event(raw: &Map<String, Value>, received_at_ns: u64) -> Result<Self> {
        Self::from_frame(
            raw.get("frame").context("book ticker event has no frame")?,
            received_at_ns,
        )
    }

    pub fn from_frame(frame: &Value, received_at_ns: u64) -> Result<Self> {
        Self::from_frame_with_clock_policy(frame, received_at_ns, false)
    }

    /// Parse a USD-M bookTicker while allowing a stale event clock. All shape,
    /// identity, price, and ordering checks remain strict; callers must only
    /// use this path to classify an event whose E-to-receive delay was already
    /// observed to exceed MAX_SOURCE_DELAY_MS.
    pub fn from_frame_allow_stale(frame: &Value, received_at_ns: u64) -> Result<Self> {
        Self::from_frame_with_clock_policy(frame, received_at_ns, true)
    }

    fn from_frame_with_clock_policy(
        frame: &Value,
        received_at_ns: u64,
        allow_stale: bool,
    ) -> Result<Self> {
        let data = frame.get("data").unwrap_or(frame);
        // The frame shape decides the market: USD-M frames carry an explicit
        // event identity and source clocks, spot frames carry neither.
        let usdm = match data.get("e").and_then(Value::as_str) {
            Some("bookTicker") => true,
            Some(_) => anyhow::bail!("book ticker frame has the wrong event identity"),
            None if allow_stale => {
                anyhow::bail!("stale book ticker frame has no USD-M event identity")
            }
            None => false,
        };
        let symbol = if usdm {
            required_string(data, "s", "book ticker")?.to_ascii_uppercase()
        } else {
            spot_symbol(frame, data, "book ticker")?
        };
        validate_stream_identity(frame, &symbol, "bookTicker")?;
        let update_id = required_u64(data, "u", "book ticker")?;
        let (best_bid_price, best_bid_quantity) =
            coherent_decimal_pair(data, "b", "B", "book ticker")?;
        let (best_ask_price, best_ask_quantity) =
            coherent_decimal_pair(data, "a", "A", "book ticker")?;
        let event_time_ms = if usdm {
            let event_time_ms = required_u64(data, "E", "book ticker")?;
            let transaction_time_ms = optional_u64(data, "T", "book ticker")?;
            if transaction_time_ms.is_some_and(|transaction| transaction > event_time_ms) {
                anyhow::bail!("book ticker source clocks are reversed");
            }
            if allow_stale {
                validate_receive_clock_without_delay(
                    event_time_ms,
                    received_at_ns,
                    "book ticker E",
                )?;
            } else {
                validate_receive_clock(event_time_ms, received_at_ns, "book ticker E")?;
            }
            event_time_ms
        } else {
            // Spot frames carry no source clock; only the receive clock bounds.
            received_at_ns / 1_000_000
        };
        Ok(Self {
            symbol,
            update_id,
            best_bid_price,
            best_bid_quantity,
            best_ask_price,
            best_ask_quantity,
            event_time_ms,
            received_at_ns,
        })
    }
}

/// USD-M liquidation order; carries no sequence guarantee, so only
/// source/receive clock bounds and payload sanity are enforced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceOrder {
    pub symbol: String,
    pub side: String,
    pub price: Decimal,
    pub quantity: Decimal,
    pub event_time_ms: u64,
    pub trade_time_ms: u64,
    /// WS-library complete-message delivery timestamp in userspace; not kernel or NIC RX.
    pub received_at_ns: u64,
}

impl ForceOrder {
    pub fn from_archived_event(raw: &Map<String, Value>, received_at_ns: u64) -> Result<Self> {
        Self::from_frame(
            raw.get("frame").context("force order event has no frame")?,
            received_at_ns,
        )
    }

    pub fn from_frame(frame: &Value, received_at_ns: u64) -> Result<Self> {
        let data = frame.get("data").unwrap_or(frame);
        if data.get("e").and_then(Value::as_str) != Some("forceOrder") {
            anyhow::bail!("force order frame has the wrong event identity");
        }
        let order = data
            .get("o")
            .filter(|order| order.is_object())
            .context("force order has no order payload")?;
        let symbol = required_string(order, "s", "force order")?.to_ascii_uppercase();
        validate_stream_identity(frame, &symbol, "forceOrder")?;
        let side = required_string(order, "S", "force order")?;
        if !matches!(side, "BUY" | "SELL") {
            anyhow::bail!("force order side is invalid");
        }
        let price = positive_decimal(order, "p", "force order")?;
        let quantity = positive_decimal(order, "q", "force order")?;
        let event_time_ms = required_u64(data, "E", "force order")?;
        let trade_time_ms = required_u64(order, "T", "force order")?;
        if trade_time_ms > event_time_ms {
            anyhow::bail!("force order source clocks are reversed");
        }
        validate_trade_clocks(event_time_ms, trade_time_ms, received_at_ns, "force order")?;
        Ok(Self {
            symbol,
            side: side.to_owned(),
            price,
            quantity,
            event_time_ms,
            trade_time_ms,
            received_at_ns,
        })
    }
}

fn validate_stream_identity(frame: &Value, symbol: &str, channel: &str) -> Result<()> {
    let Some(stream) = frame.get("stream").and_then(Value::as_str) else {
        return Ok(());
    };
    let mut parts = stream.split('@');
    let stream_symbol = parts.next().unwrap_or_default();
    let stream_channel = parts.next().unwrap_or_default();
    if !stream_symbol.eq_ignore_ascii_case(symbol) || !stream_channel.eq_ignore_ascii_case(channel)
    {
        anyhow::bail!("{channel} frame has the wrong stream identity");
    }
    Ok(())
}

/// Spot payloads carry no event identity, so `s` is the only symbol field;
/// fall back to the combined-stream name when it is absent.
fn spot_symbol(frame: &Value, data: &Value, kind: &str) -> Result<String> {
    if let Ok(symbol) = required_string(data, "s", kind) {
        return Ok(symbol.to_ascii_uppercase());
    }
    let symbol = frame
        .get("stream")
        .and_then(Value::as_str)
        .and_then(|stream| stream.split('@').next())
        .filter(|symbol| !symbol.is_empty())
        .with_context(|| format!("{kind} field s is missing"))?;
    Ok(symbol.to_ascii_uppercase())
}

fn validate_receive_clock(event_time_ms: u64, received_at_ns: u64, kind: &str) -> Result<()> {
    validate_receive_clock_without_delay(event_time_ms, received_at_ns, kind)?;
    let received_at_ms = received_at_ns / 1_000_000;
    if received_at_ms.saturating_sub(event_time_ms) > MAX_SOURCE_DELAY_MS {
        anyhow::bail!("{kind} source-to-receive delay exceeds the governed limit");
    }
    Ok(())
}

fn validate_trade_clocks(
    event_time_ms: u64,
    trade_time_ms: u64,
    received_at_ns: u64,
    kind: &str,
) -> Result<()> {
    let received_at_ms = received_at_ns / 1_000_000;
    let recv_minus_event_ms = received_at_ms.saturating_sub(event_time_ms);
    let event_minus_trade_ms = event_time_ms.saturating_sub(trade_time_ms);
    let recv_minus_trade_ms = received_at_ms.saturating_sub(trade_time_ms);
    if let Err(error) = validate_receive_clock(event_time_ms, received_at_ns, &format!("{kind} E"))
    {
        anyhow::bail!(
            "{error:#}: recv_minus_event_ms={recv_minus_event_ms} event_minus_trade_ms={event_minus_trade_ms} recv_minus_trade_ms={recv_minus_trade_ms}"
        );
    }
    if recv_minus_trade_ms > MAX_SOURCE_DELAY_MS {
        anyhow::bail!(
            "{kind} age exceeds the governed limit: recv_minus_event_ms={recv_minus_event_ms} event_minus_trade_ms={event_minus_trade_ms} recv_minus_trade_ms={recv_minus_trade_ms}",
        );
    }
    Ok(())
}

fn validate_receive_clock_without_delay(
    event_time_ms: u64,
    received_at_ns: u64,
    kind: &str,
) -> Result<()> {
    let received_at_ms = received_at_ns / 1_000_000;
    if event_time_ms.saturating_sub(received_at_ms) > MAX_SOURCE_LEAD_MS {
        anyhow::bail!("{kind} source clock lead exceeds the governed limit");
    }
    Ok(())
}

fn required_string<'a>(value: &'a Value, field: &str, kind: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{kind} field {field} is missing"))
}

fn required_u64(value: &Value, field: &str, kind: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("{kind} field {field} is missing"))
}

fn optional_u64(value: &Value, field: &str, kind: &str) -> Result<Option<u64>> {
    value
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .with_context(|| format!("{kind} field {field} is malformed"))
        })
        .transpose()
}

fn positive_decimal(value: &Value, field: &str, kind: &str) -> Result<Decimal> {
    let decimal = required_string(value, field, kind)?
        .parse::<Decimal>()
        .with_context(|| format!("{kind} field {field} is not decimal"))?;
    if decimal <= Decimal::ZERO {
        anyhow::bail!("{kind} field {field} is not positive");
    }
    Ok(decimal)
}

fn coherent_decimal_pair(
    value: &Value,
    price_field: &str,
    quantity_field: &str,
    kind: &str,
) -> Result<(Decimal, Decimal)> {
    let price = required_string(value, price_field, kind)?
        .parse::<Decimal>()
        .with_context(|| format!("{kind} field {price_field} is not decimal"))?;
    let quantity = required_string(value, quantity_field, kind)?
        .parse::<Decimal>()
        .with_context(|| format!("{kind} field {quantity_field} is not decimal"))?;
    if price < Decimal::ZERO {
        anyhow::bail!("{kind} field {price_field} is negative");
    }
    if quantity < Decimal::ZERO {
        anyhow::bail!("{kind} field {quantity_field} is negative");
    }
    anyhow::ensure!(
        (price == Decimal::ZERO) == (quantity == Decimal::ZERO),
        "{kind} fields {price_field}/{quantity_field} must be both zero or both positive"
    );
    Ok((price, quantity))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame(id: u64, event_time_ms: u64, trade_time_ms: u64) -> Value {
        json!({
            "stream": "btcusdt@aggTrade",
            "data": {
                "e": "aggTrade",
                "E": event_time_ms,
                "s": "BTCUSDT",
                "a": id,
                "p": "100.5",
                "q": "0.25",
                "f": id,
                "l": id,
                "T": trade_time_ms,
                "m": false
            }
        })
    }

    fn depth_frame(event_time_ms: u64, transaction_time_ms: Option<u64>) -> Value {
        depth_frame_with_sequence(event_time_ms, transaction_time_ms, 10, 11, None)
    }

    fn depth_frame_with_sequence(
        event_time_ms: u64,
        transaction_time_ms: Option<u64>,
        first_update_id: u64,
        final_update_id: u64,
        previous_final_update_id: Option<u64>,
    ) -> Value {
        let mut data = json!({
            "e": "depthUpdate",
            "E": event_time_ms,
            "s": "BTCUSDT",
            "U": first_update_id,
            "u": final_update_id,
            "b": [],
            "a": []
        });
        if let Some(transaction_time_ms) = transaction_time_ms {
            data["T"] = json!(transaction_time_ms);
        }
        if let Some(previous_final_update_id) = previous_final_update_id {
            data["pu"] = json!(previous_final_update_id);
        }
        json!({"stream": "btcusdt@depth@100ms", "data": data})
    }

    #[test]
    fn depth_clock_rejects_reversed_update_range() {
        assert!(DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_000, None, 12, 11, None),
            1_700_000_000_100_000_000,
        )
        .unwrap_err()
        .to_string()
        .contains("range"));
    }

    #[test]
    fn depth_clock_rejects_malformed_previous_update_id() {
        let mut malformed = depth_frame_with_sequence(1_700_000_000_000, None, 10, 11, Some(9));
        malformed["data"]["pu"] = json!("9");
        assert!(
            DepthSourceClock::from_frame(&malformed, 1_700_000_000_100_000_000)
                .unwrap_err()
                .to_string()
                .contains("pu")
        );
    }

    #[test]
    fn depth_clock_rejects_malformed_transaction_time() {
        let mut malformed = depth_frame(1_700_000_000_000, None);
        malformed["data"]["T"] = json!("1700000000000");
        assert!(
            DepthSourceClock::from_frame(&malformed, 1_700_000_000_100_000_000)
                .unwrap_err()
                .to_string()
                .contains("T")
        );
    }

    #[test]
    fn depth_sequence_rejects_gap_without_poisoning_state() {
        let received_at_ns = 1_700_000_000_100_000_000;
        let first = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_000, None, 10, 11, None),
            received_at_ns,
        )
        .unwrap();
        let gap = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_001, None, 13, 14, None),
            received_at_ns,
        )
        .unwrap();
        let recovered = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_002, None, 12, 12, None),
            received_at_ns,
        )
        .unwrap();

        let mut sequence = DepthSourceClockSequenceValidator::default();
        sequence.observe(&first).unwrap();
        let error = sequence.observe(&gap).unwrap_err();
        assert!(error.downcast_ref::<DepthSequenceGap>().is_some());
        sequence.observe(&recovered).unwrap();
    }

    #[test]
    fn depth_sequence_binds_futures_previous_update_id() {
        let received_at_ns = 1_700_000_000_100_000_000;
        let first = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_000, None, 10, 11, Some(9)),
            received_at_ns,
        )
        .unwrap();
        let previous_id_ahead = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_001, None, 12, 12, Some(13)),
            received_at_ns,
        )
        .unwrap();
        let previous_id_behind = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_001, None, 12, 12, Some(10)),
            received_at_ns,
        )
        .unwrap();

        let mut gap_sequence = DepthSourceClockSequenceValidator::default();
        gap_sequence.observe(&first).unwrap();
        let error = gap_sequence.observe(&previous_id_ahead).unwrap_err();
        assert!(error.downcast_ref::<DepthSequenceGap>().is_some());

        let mut rollback_sequence = DepthSourceClockSequenceValidator::default();
        rollback_sequence.observe(&first).unwrap();
        let error = rollback_sequence.observe(&previous_id_behind).unwrap_err();
        assert!(error.downcast_ref::<DepthSequenceGap>().is_none());
        assert!(error.to_string().contains("rollback"));
    }

    #[test]
    fn depth_sequence_accepts_reconnect_origin_and_overlap() {
        let received_at_ns = 1_700_000_000_100_000_000;
        let reconnect_origin = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_000, None, 100, 105, Some(99)),
            received_at_ns,
        )
        .unwrap();
        let overlapping_next = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_001, None, 104, 106, Some(105)),
            received_at_ns,
        )
        .unwrap();

        let mut sequence = DepthSourceClockSequenceValidator::default();
        sequence.observe(&reconnect_origin).unwrap();
        sequence.observe(&overlapping_next).unwrap();
    }

    #[test]
    fn depth_sequence_accepts_futures_pu_continuity_with_nonconsecutive_first_id() {
        let received_at_ns = 1_700_000_000_100_000_000;
        let first = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(
                1_700_000_000_000,
                None,
                11_074_967_399_926,
                11_074_967_403_842,
                Some(11_074_967_399_747),
            ),
            received_at_ns,
        )
        .unwrap();
        let next = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(
                1_700_000_000_001,
                None,
                11_074_967_403_847,
                11_074_967_407_986,
                Some(11_074_967_403_842),
            ),
            received_at_ns,
        )
        .unwrap();

        let mut sequence = DepthSourceClockSequenceValidator::default();
        sequence.observe(&first).unwrap();
        sequence.observe(&next).unwrap();
    }

    #[test]
    fn depth_sequence_rejects_stale_range_rollback() {
        let received_at_ns = 1_700_000_000_100_000_000;
        let first = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_000, None, 10, 11, None),
            received_at_ns,
        )
        .unwrap();
        let stale = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_001, None, 9, 10, None),
            received_at_ns,
        )
        .unwrap();

        let mut sequence = DepthSourceClockSequenceValidator::default();
        sequence.observe(&first).unwrap();
        assert!(sequence
            .observe(&stale)
            .unwrap_err()
            .to_string()
            .contains("rollback"));
    }

    #[test]
    fn depth_clock_enforces_receive_order_delay_and_per_symbol_rollback() {
        let received_at_ns = 1_700_000_000_100_000_000;
        let first = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_000, Some(1_700_000_000_000), 10, 11, None),
            received_at_ns,
        )
        .unwrap();
        let next = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_700_000_000_001, Some(1_700_000_000_001), 12, 12, None),
            received_at_ns,
        )
        .unwrap();
        let rollback = DepthSourceClock::from_frame(
            &depth_frame_with_sequence(1_699_999_999_999, Some(1_699_999_999_999), 13, 13, None),
            received_at_ns,
        )
        .unwrap();
        let mut sequence = DepthSourceClockSequenceValidator::default();
        sequence.observe(&first).unwrap();
        sequence.observe(&next).unwrap();
        assert!(sequence
            .observe(&rollback)
            .unwrap_err()
            .to_string()
            .contains("rollback"));
        assert!(DepthSourceClock::from_frame(
            &depth_frame(1_700_000_001_101, None),
            received_at_ns,
        )
        .unwrap_err()
        .to_string()
        .contains("lead exceeds"));
        assert!(DepthSourceClock::from_frame(
            &depth_frame(1_700_000_000_000, None),
            1_700_000_031_000_000_000,
        )
        .unwrap_err()
        .to_string()
        .contains("delay"));
    }

    #[test]
    fn depth_clock_accepts_fresh_event_with_old_transaction_time() {
        assert!(DepthSourceClock::from_frame(
            &depth_frame(1_700_000_031_000, Some(1_700_000_000_999)),
            1_700_000_031_000_000_000,
        )
        .is_ok());
    }

    #[test]
    fn depth_clock_allows_only_bounded_source_lead() {
        let received_at_ms = 1_700_000_000_000;
        let received_at_ns = received_at_ms * 1_000_000;
        let parse = |lead_ms| {
            DepthSourceClock::from_frame(
                &depth_frame(received_at_ms + lead_ms, Some(received_at_ms + lead_ms)),
                received_at_ns,
            )
        };
        parse(MAX_SOURCE_LEAD_MS).unwrap();
        assert!(parse(MAX_SOURCE_LEAD_MS + 1)
            .unwrap_err()
            .to_string()
            .contains("lead exceeds"));
    }

    #[test]
    fn aggregate_trade_enforces_dual_clocks_and_delay() {
        assert!(AggregateTrade::from_frame(
            &frame(1, 1_700_000_000_000, 1_700_000_000_000),
            1_700_000_000_100_000_000,
        )
        .is_ok());
        assert!(AggregateTrade::from_frame(
            &frame(1, 1_700_000_000_001, 1_700_000_000_002),
            1_700_000_000_100_000_000,
        )
        .unwrap_err()
        .to_string()
        .contains("reversed"));
        assert!(AggregateTrade::from_frame(
            &frame(1, 1_700_000_000_000, 1_700_000_000_000),
            1_700_000_031_000_000_000,
        )
        .unwrap_err()
        .to_string()
        .contains("delay"));
    }

    #[test]
    fn aggregate_trade_rejects_stale_trade_time_when_event_time_is_fresh() {
        let error = AggregateTrade::from_frame(
            &frame(1, 1_700_000_031_000, 1_700_000_000_999),
            1_700_000_031_000_000_000,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("aggregate trade age exceeds the governed limit"));
        assert!(error.contains("recv_minus_event_ms=0"));
        assert!(error.contains("event_minus_trade_ms=30001"));
        assert!(error.contains("recv_minus_trade_ms=30001"));
    }

    #[test]
    fn aggregate_trade_allows_only_bounded_source_lead() {
        let received_at_ms = 1_700_000_000_000;
        let received_at_ns = received_at_ms * 1_000_000;
        let parse = |lead_ms| {
            AggregateTrade::from_frame(
                &frame(1, received_at_ms + lead_ms, received_at_ms + lead_ms),
                received_at_ns,
            )
        };
        parse(MAX_SOURCE_LEAD_MS).unwrap();
        assert!(parse(MAX_SOURCE_LEAD_MS + 1)
            .unwrap_err()
            .to_string()
            .contains("lead exceeds"));
    }

    #[rustfmt::skip]
    fn raw_trade_frame(id: u64, event_time_ms: u64, trade_time_ms: u64) -> Value {
        json!({
            "stream": "btcusdt@trade",
            "data": {
                "e": "trade",
                "E": event_time_ms,
                "s": "BTCUSDT",
                "t": id,
                "p": "100.5",
                "q": "0.25",
                "T": trade_time_ms,
                "m": false
            }
        })
    }

    #[rustfmt::skip]
    fn book_ticker_frame(event_time_ms: u64) -> Value {
        json!({
            "stream": "btcusdt@bookTicker",
            "data": {
                "e": "bookTicker",
                "u": 400900217,
                "E": event_time_ms,
                "T": event_time_ms,
                "s": "BTCUSDT",
                "b": "100.5",
                "B": "31.21",
                "a": "100.6",
                "A": "40.66"
            }
        })
    }

    #[rustfmt::skip]
    fn spot_book_ticker_frame() -> Value {
        json!({
            "stream": "btcusdt@bookTicker",
            "data": {
                "u": 400900217,
                "s": "BTCUSDT",
                "b": "100.5",
                "B": "31.21",
                "a": "100.6",
                "A": "40.66"
            }
        })
    }

    #[rustfmt::skip]
    fn force_order_frame(event_time_ms: u64, trade_time_ms: u64) -> Value {
        json!({
            "stream": "btcusdt@forceOrder",
            "data": {
                "e": "forceOrder",
                "E": event_time_ms,
                "o": {
                    "s": "BTCUSDT",
                    "S": "SELL",
                    "o": "LIMIT",
                    "f": "IOC",
                    "q": "0.014",
                    "p": "9910",
                    "ap": "9910",
                    "X": "FILLED",
                    "l": "0.014",
                    "z": "0.014",
                    "T": trade_time_ms
                }
            }
        })
    }

    #[test]
    fn market_tape_v2_supports_new_event_families() {
        assert!(supported_schema(MARKET_TAPE_SCHEMA_V2));
        assert!(market_tape_schema(MARKET_TAPE_SCHEMA));
        assert!(market_tape_schema(MARKET_TAPE_SCHEMA_V2));
        assert!(!market_tape_schema(LEGACY_LOB_TAPE_SCHEMA));
        for event_type in [
            "raw_trade",
            "raw_trade_zero_price",
            "book_ticker",
            "stale_book_ticker",
            "force_order",
        ] {
            assert!(event_type_allowed(MARKET_TAPE_SCHEMA_V2, event_type));
            assert!(!event_type_allowed(MARKET_TAPE_SCHEMA, event_type));
            assert!(!event_type_allowed(LEGACY_LOB_TAPE_SCHEMA, event_type));
        }
        for schema in [MARKET_TAPE_SCHEMA, MARKET_TAPE_SCHEMA_V2] {
            for event_type in [
                "session_start",
                "snapshot",
                "diff",
                "checkpoint",
                "agg_trade",
            ] {
                assert!(event_type_allowed(schema, event_type));
            }
            // kline is a reserved event type name only; it has no implementation.
            assert!(!event_type_allowed(schema, "kline"));
        }
    }

    #[test]
    fn raw_trade_enforces_dual_clocks_and_delay() {
        assert!(RawTrade::from_frame(
            &raw_trade_frame(1, 1_700_000_000_000, 1_700_000_000_000),
            1_700_000_000_100_000_000,
        )
        .is_ok());
        assert!(RawTrade::from_frame(
            &raw_trade_frame(1, 1_700_000_000_001, 1_700_000_000_002),
            1_700_000_000_100_000_000,
        )
        .unwrap_err()
        .to_string()
        .contains("reversed"));
        assert!(RawTrade::from_frame(
            &raw_trade_frame(1, 1_700_000_000_000, 1_700_000_000_000),
            1_700_000_031_000_000_000,
        )
        .unwrap_err()
        .to_string()
        .contains("delay"));
        let error = RawTrade::from_frame(
            &raw_trade_frame(1, 1_700_000_031_000, 1_700_000_000_999),
            1_700_000_031_000_000_000,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("raw trade age exceeds the governed limit"));
        assert!(error.contains("recv_minus_event_ms=0"));
        assert!(error.contains("event_minus_trade_ms=30001"));
        assert!(error.contains("recv_minus_trade_ms=30001"));
        let mut wrong_identity = raw_trade_frame(1, 1_700_000_000_000, 1_700_000_000_000);
        wrong_identity["data"]["e"] = json!("aggTrade");
        assert!(
            RawTrade::from_frame(&wrong_identity, 1_700_000_000_100_000_000)
                .unwrap_err()
                .to_string()
                .contains("identity")
        );
    }

    #[test]
    fn stale_raw_trade_parser_keeps_shape_strict_but_allows_audit_clock() {
        let received_at_ns = 1_786_515_635_275_887_892;
        let frame = json!({
            "data":{"E":1_786_515_604_331_u64,"T":1_786_515_604_330_u64,"X":"MARKET","e":"trade","m":false,"p":"0.0664000","q":"506","s":"CUSDT","st":1,"t":140253949_u64},
            "stream":"cusdt@trade"
        });
        let trade = RawTrade::from_frame_allow_stale(&frame, received_at_ns).unwrap();
        assert_eq!(trade.symbol, "CUSDT");
        assert_eq!(trade.trade_id, 140253949);
        assert!(RawTrade::from_frame(&frame, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("delay"));
        let mut malformed = frame;
        malformed["data"]["q"] = Value::Null;
        assert!(RawTrade::from_frame_allow_stale(&malformed, received_at_ns).is_err());
    }

    #[test]
    fn raw_trade_zero_price_requires_observed_sentinel_shape() {
        let received_at_ns = 1_700_000_000_100_000_000;
        let mut zero = raw_trade_frame(1, 1_700_000_000_000, 1_700_000_000_000);
        zero["data"]["p"] = json!("0");
        zero["data"]["q"] = json!("0");
        zero["data"]["X"] = json!("NA");
        zero["data"]["st"] = json!(1);
        assert!(RawTrade::from_frame(&zero, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("not positive: 0"));
        assert_eq!(
            RawTrade::from_zero_price_frame(&zero, received_at_ns)
                .unwrap()
                .price,
            Decimal::ZERO
        );

        let mut negative = zero.clone();
        negative["data"]["p"] = json!("-1");
        assert!(RawTrade::from_zero_price_frame(&negative, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("not positive: -1"));

        let mut positive = zero.clone();
        positive["data"]["p"] = json!("100.5");
        assert!(RawTrade::from_zero_price_frame(&positive, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("not zero"));

        let mut nonzero_quantity = zero.clone();
        nonzero_quantity["data"]["q"] = json!("0.25");
        assert!(RawTrade::from_zero_price_frame(&nonzero_quantity, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("q is not zero"));

        let mut positive_with_zero_quantity = raw_trade_frame(
            1,
            1_700_000_000_000,
            1_700_000_000_000,
        );
        positive_with_zero_quantity["data"]["q"] = json!("0");
        assert!(RawTrade::from_frame(&positive_with_zero_quantity, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("q is not positive"));

        let mut malformed = zero.clone();
        malformed["data"]["q"] = Value::Null;
        assert!(RawTrade::from_zero_price_frame(&malformed, received_at_ns).is_err());

        let mut missing_status = zero.clone();
        missing_status["data"].as_object_mut().unwrap().remove("st");
        assert!(RawTrade::from_zero_price_frame(&missing_status, received_at_ns).is_err());

        let mut wrong_status = zero.clone();
        wrong_status["data"]["st"] = json!(2);
        assert!(RawTrade::from_zero_price_frame(&wrong_status, received_at_ns).is_err());

        let mut missing_execution = zero.clone();
        missing_execution["data"].as_object_mut().unwrap().remove("X");
        assert!(RawTrade::from_zero_price_frame(&missing_execution, received_at_ns).is_err());

        let mut wrong_execution = zero.clone();
        wrong_execution["data"]["X"] = json!("MARKET");
        assert!(RawTrade::from_zero_price_frame(&wrong_execution, received_at_ns).is_err());

        let mut unknown_execution = zero;
        unknown_execution["data"]["X"] = json!("UNKNOWN");
        assert!(RawTrade::from_zero_price_frame(&unknown_execution, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("X is unsupported"));

        let insurance_fund = json!({
            "stream": "cysusdt@trade",
            "data": {
                "e": "trade",
                "E": 1_786_437_637_139_u64,
                "T": 1_786_437_637_139_u64,
                "s": "CYSUSDT",
                "t": 110193464_u64,
                "p": "0",
                "q": "0",
                "X": "INSURANCE_FUND",
                "m": false,
                "st": 1_u64
            }
        });
        let insurance_trade =
            RawTrade::from_zero_price_frame(&insurance_fund, 1_786_437_637_144_000_000).unwrap();
        assert_eq!(insurance_trade.symbol, "CYSUSDT");
        assert_eq!(insurance_trade.trade_id, 110193464);
        assert_eq!(insurance_trade.price, Decimal::ZERO);
        assert_eq!(insurance_trade.quantity, Decimal::ZERO);
    }

    #[test]
    fn observed_usdm_zero_price_sentinel_shape_is_accepted() {
        let frame = json!({
            "stream": "btcusdc@trade",
            "data": {
                "e": "trade",
                "E": 1_786_430_320_180_u64,
                "s": "BTCUSDC",
                "t": 553104312_u64,
                "p": "0",
                "q": "0",
                "T": 1_786_430_320_180_u64,
                "X": "NA",
                "m": false,
                "st": 1_u64
            }
        });
        let trade = RawTrade::from_zero_price_frame(&frame, 1_786_430_320_300_000_000)
            .unwrap();
        assert_eq!(trade.symbol, "BTCUSDC");
        assert_eq!(trade.trade_id, 553104312);
        assert_eq!(trade.price, Decimal::ZERO);
        assert_eq!(trade.quantity, Decimal::ZERO);
    }

    #[test]
    fn raw_trade_sequence_rejects_gap_and_source_time_rollback() {
        let received_at_ns = 1_700_000_000_100_000_000;
        let first = RawTrade::from_frame(
            &raw_trade_frame(10, 1_700_000_000_000, 1_700_000_000_000),
            received_at_ns,
        )
        .unwrap();
        let next = RawTrade::from_frame(
            &raw_trade_frame(11, 1_700_000_000_001, 1_700_000_000_001),
            received_at_ns,
        )
        .unwrap();
        let gap = RawTrade::from_frame(
            &raw_trade_frame(13, 1_700_000_000_002, 1_700_000_000_002),
            received_at_ns,
        )
        .unwrap();
        let rollback = RawTrade::from_frame(
            &raw_trade_frame(12, 1_699_999_999_999, 1_699_999_999_999),
            received_at_ns,
        )
        .unwrap();

        let mut sequence = RawTradeSequenceValidator::default();
        sequence.observe(&first).unwrap();
        sequence.observe(&next).unwrap();
        assert!(sequence
            .observe(&gap)
            .unwrap_err()
            .to_string()
            .contains("gap"));
        assert!(sequence
            .observe(&rollback)
            .unwrap_err()
            .to_string()
            .contains("rollback"));
    }

    #[test]
    fn raw_trade_sequence_can_advance_over_one_audited_stale_id() {
        let received_at_ns = 1_786_515_635_275_887_892;
        let first = RawTrade::from_frame(
            &raw_trade_frame(140253948, 1_786_515_635_272, 1_786_515_635_272),
            received_at_ns,
        )
        .unwrap();
        let next = RawTrade::from_frame(
            &raw_trade_frame(140253950, 1_786_515_635_276, 1_786_515_635_276),
            received_at_ns,
        )
        .unwrap();
        let mut sequence = RawTradeSequenceValidator::default();
        sequence.observe(&first).unwrap();
        sequence.observe_after_stale(&next, 140253949).unwrap();
        assert!(sequence
            .observe_after_stale(&next, 140253949)
            .unwrap_err()
            .to_string()
            .contains("gap"));
    }

    #[test]
    fn book_ticker_enforces_clock_bounds_without_a_sequence_guarantee() {
        let received_at_ns = 1_700_000_000_100_000_000;
        let first =
            BookTicker::from_frame(&book_ticker_frame(1_700_000_000_000), received_at_ns).unwrap();
        assert_eq!(first.symbol, "BTCUSDT");
        // Non-consecutive update ids are valid: book tickers carry no sequence guarantee.
        let mut later = book_ticker_frame(1_700_000_000_001);
        later["data"]["u"] = json!(400900999);
        BookTicker::from_frame(&later, received_at_ns).unwrap();
        let mut stale_transaction = book_ticker_frame(1_700_000_000_099);
        stale_transaction["data"]["T"] = json!(1_700_000_000_000_u64);
        BookTicker::from_frame(&stale_transaction, received_at_ns).unwrap();
        assert!(BookTicker::from_frame(
            &book_ticker_frame(1_700_000_000_000),
            1_700_000_031_000_000_000
        )
        .unwrap_err()
        .to_string()
        .contains("book ticker E source-to-receive delay"));
        BookTicker::from_frame_allow_stale(
            &book_ticker_frame(1_700_000_000_000),
            1_700_000_031_000_000_000,
        )
        .unwrap();
        let mut reversed = book_ticker_frame(1_700_000_000_000);
        reversed["data"]["T"] = json!(1_700_000_000_001_u64);
        assert!(BookTicker::from_frame_allow_stale(
            &reversed,
            1_700_000_031_000_000_000,
        )
        .unwrap_err()
        .to_string()
        .contains("reversed"));
        assert!(BookTicker::from_frame_allow_stale(
            &book_ticker_frame(1_700_000_000_100 + MAX_SOURCE_LEAD_MS + 1),
            received_at_ns,
        )
        .unwrap_err()
        .to_string()
        .contains("book ticker E source clock lead"));
        let mut non_positive = book_ticker_frame(1_700_000_000_000);
        non_positive["data"]["b"] = json!("0");
        assert!(BookTicker::from_frame(&non_positive, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("positive"));
    }

    #[test]
    fn spot_book_ticker_frame_parses_without_source_clocks() {
        let received_at_ns = 1_700_000_000_100_000_000;
        // Spot bookTicker frames carry no e/E/T fields; the receive clock is
        // the only time anchor.
        let ticker = BookTicker::from_frame(&spot_book_ticker_frame(), received_at_ns).unwrap();
        assert_eq!(ticker.symbol, "BTCUSDT");
        assert_eq!(ticker.update_id, 400900217);
        assert_eq!(ticker.event_time_ms, received_at_ns / 1_000_000);

        // Without an `s` field the combined-stream name is the only symbol source.
        let mut stream_only = spot_book_ticker_frame();
        stream_only["data"].as_object_mut().unwrap().remove("s");
        let ticker = BookTicker::from_frame(&stream_only, received_at_ns).unwrap();
        assert_eq!(ticker.symbol, "BTCUSDT");
    }

    #[test]
    fn spot_book_ticker_frame_fails_closed_on_bad_payload() {
        let received_at_ns = 1_700_000_000_100_000_000;
        let mut non_positive = spot_book_ticker_frame();
        non_positive["data"]["a"] = json!("0");
        assert!(BookTicker::from_frame(&non_positive, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("both zero or both positive"));

        let mut negative = spot_book_ticker_frame();
        negative["data"]["a"] = json!("-0.1");
        assert!(BookTicker::from_frame(&negative, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("negative"));

        let mut malformed = spot_book_ticker_frame();
        malformed["data"]["A"] = json!("not-a-decimal");
        assert!(BookTicker::from_frame(&malformed, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("not decimal"));

        // No `s` field and no stream name to derive it from.
        let bare = json!({"data": {"u": 400900217, "b": "100.5", "B": "31.21", "a": "100.6", "A": "40.66"}});
        assert!(BookTicker::from_frame(&bare, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("missing"));

        // USD-M frames keep their strict source-clock validation.
        let mut no_event_time = book_ticker_frame(1_700_000_000_000);
        no_event_time["data"].as_object_mut().unwrap().remove("E");
        assert!(BookTicker::from_frame(&no_event_time, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("missing"));
        assert!(BookTicker::from_frame_allow_stale(
            &spot_book_ticker_frame(),
            received_at_ns,
        )
        .unwrap_err()
        .to_string()
        .contains("USD-M event identity"));
    }

    #[test]
    fn spot_book_ticker_accepts_observed_zero_ask_side() {
        let frame = json!({
            "stream": "chipusd1@bookTicker",
            "data": {
                "u": 14_756_445_u64,
                "s": "CHIPUSD1",
                "b": "0.02215000",
                "B": "3946.00000000",
                "a": "0.00000000",
                "A": "0.00000000"
            }
        });
        let ticker = BookTicker::from_frame(&frame, 1_786_486_642_461_639_000)
            .expect("coherent zero ask side is a valid spot book ticker");
        assert_eq!(ticker.symbol, "CHIPUSD1");
        assert_eq!(ticker.best_ask_price, Decimal::ZERO);
        assert_eq!(ticker.best_ask_quantity, Decimal::ZERO);
    }

    #[test]
    fn force_order_enforces_dual_clocks_and_delay() {
        let received_at_ns = 1_700_000_000_100_000_000;
        assert!(ForceOrder::from_frame(
            &force_order_frame(1_700_000_000_000, 1_700_000_000_000),
            received_at_ns,
        )
        .is_ok());
        assert!(ForceOrder::from_frame(
            &force_order_frame(1_700_000_000_001, 1_700_000_000_002),
            received_at_ns,
        )
        .unwrap_err()
        .to_string()
        .contains("reversed"));
        assert!(ForceOrder::from_frame(
            &force_order_frame(1_700_000_000_000, 1_700_000_000_000),
            1_700_000_031_000_000_000,
        )
        .unwrap_err()
        .to_string()
        .contains("delay"));
        let error = ForceOrder::from_frame(
            &force_order_frame(1_700_000_031_000, 1_700_000_000_999),
            1_700_000_031_000_000_000,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("force order age exceeds the governed limit"));
        assert!(error.contains("recv_minus_event_ms=0"));
        assert!(error.contains("event_minus_trade_ms=30001"));
        assert!(error.contains("recv_minus_trade_ms=30001"));
        let mut bad_side = force_order_frame(1_700_000_000_000, 1_700_000_000_000);
        bad_side["data"]["o"]["S"] = json!("HOLD");
        assert!(ForceOrder::from_frame(&bad_side, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("side"));
    }

    #[test]
    fn aggregate_trade_sequence_rejects_gap_and_source_time_rollback() {
        let first = AggregateTrade::from_frame(
            &frame(10, 1_700_000_000_000, 1_700_000_000_000),
            1_700_000_000_100_000_000,
        )
        .unwrap();
        let next = AggregateTrade::from_frame(
            &frame(11, 1_700_000_000_001, 1_700_000_000_001),
            1_700_000_000_100_000_000,
        )
        .unwrap();
        let gap = AggregateTrade::from_frame(
            &frame(13, 1_700_000_000_002, 1_700_000_000_002),
            1_700_000_000_100_000_000,
        )
        .unwrap();
        let rollback = AggregateTrade::from_frame(
            &frame(12, 1_699_999_999_999, 1_699_999_999_999),
            1_700_000_000_100_000_000,
        )
        .unwrap();

        let mut sequence = AggregateTradeSequenceValidator::default();
        sequence.observe(&first).unwrap();
        sequence.observe(&next).unwrap();
        assert!(sequence
            .observe(&gap)
            .unwrap_err()
            .to_string()
            .contains("gap"));
        assert!(sequence
            .observe(&rollback)
            .unwrap_err()
            .to_string()
            .contains("rollback"));
    }
}
