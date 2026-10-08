//! One USD-M positioning artifact from public frames this repository already parses.
//!
//! Admitted shapes are `@aggTrade`, `@forceOrder`, and `@bookTicker`. The public
//! long/short ratio endpoint is not admitted here, so that metric stays
//! unavailable instead of being written as zero.

use crate::binance_usdm_reference_artifact::{read_bound_file, rename_noreplace, write_new};
use crate::binance_usdm_reference_collector::OFFICIAL_USDM_SOURCE_ORIGIN;
use crate::polymarket_upload::ensure_canonical_directory;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use data::binance_market_tape::{
    AggregateTrade, AggregateTradeSequenceValidator, AggregateTradeSummary,
    AggregateTradeSummaryBuilder, BookTicker, MAX_SOURCE_LEAD_MS,
};
use data::binance_usdm_reference::{
    force_order_observation, ForceOrderObservation, FORCE_ORDER_COVERAGE,
};
use rand::random;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, DirBuilder, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const MANIFEST_SCHEMA: &str = "binance.usdm_positioning_manifest.v1";
const VENUE: &str = "binance_usdm";
const DATASET: &str = "positioning";
const DATA_NAME: &str = "positioning.ndjson";
const LONG_SHORT_REASON: &str = "endpoint_shape_not_admitted";
const MAX_DATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SUCCESS_BYTES: u64 = 65;
const MAX_STALENESS_MS: u64 = 300_000;

#[derive(Debug, Clone)]
pub struct PositioningArtifactConfig {
    pub output_root: PathBuf,
    pub observed_at_ns: u64,
    pub max_staleness_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedPositioningArtifact {
    pub data_path: PathBuf,
    pub manifest_path: PathBuf,
    pub success_path: PathBuf,
    pub data_sha256: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPositioningCounts {
    pub rows: u64,
    pub empty_books: u64,
    pub one_sided_books: u64,
    pub executable_books: u64,
    pub liquidations: u64,
    pub taker_trades: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BookState {
    Empty,
    OneSided,
    Executable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PositioningRow {
    Liquidation {
        sequence: u64,
        venue: String,
        symbol: String,
        source_time_ms: u64,
        order_time_ms: u64,
        received_at_ns: u64,
        source_endpoint: String,
        coverage: String,
        side: String,
        price: String,
        quantity: String,
        executable: bool,
        raw: Value,
    },
    TakerFlow {
        sequence: u64,
        venue: String,
        symbol: String,
        source_time_ms: u64,
        trade_time_ms: u64,
        received_at_ns: u64,
        source_endpoint: String,
        aggregate_trade_id: u64,
        is_buyer_maker: bool,
        price: String,
        quantity: String,
        executable: bool,
        raw: Value,
    },
    Book {
        sequence: u64,
        venue: String,
        symbol: String,
        source_time_ms: u64,
        received_at_ns: u64,
        source_endpoint: String,
        update_id: u64,
        book_state: BookState,
        executable: bool,
        bid_price: String,
        bid_quantity: String,
        ask_price: String,
        ask_quantity: String,
        raw: Value,
    },
}

impl PositioningRow {
    fn sequence(&self) -> u64 {
        match self {
            Self::Liquidation { sequence, .. }
            | Self::TakerFlow { sequence, .. }
            | Self::Book { sequence, .. } => *sequence,
        }
    }

    fn received_at_ns(&self) -> u64 {
        match self {
            Self::Liquidation { received_at_ns, .. }
            | Self::TakerFlow { received_at_ns, .. }
            | Self::Book { received_at_ns, .. } => *received_at_ns,
        }
    }

    fn source_time_ms(&self) -> u64 {
        match self {
            Self::Liquidation { source_time_ms, .. }
            | Self::TakerFlow { source_time_ms, .. }
            | Self::Book { source_time_ms, .. } => *source_time_ms,
        }
    }

    fn raw(&self) -> &Value {
        match self {
            Self::Liquidation { raw, .. }
            | Self::TakerFlow { raw, .. }
            | Self::Book { raw, .. } => raw,
        }
    }

    fn endpoint(&self) -> &str {
        match self {
            Self::Liquidation {
                source_endpoint, ..
            }
            | Self::TakerFlow {
                source_endpoint, ..
            }
            | Self::Book {
                source_endpoint, ..
            } => source_endpoint,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Coverage {
    liquidation_observations: u64,
    taker_flow_observations: u64,
    empty_books: u64,
    one_sided_books: u64,
    executable_books: u64,
    stale_rows: u64,
    api_error_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LongShortRatioStatus {
    status: String,
    reason: String,
    ratio: Option<String>,
    long_account: Option<String>,
    short_account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum TakerFlowStatus {
    Observed {
        summaries: BTreeMap<String, AggregateTradeSummary>,
    },
    NoEvents,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LiquidationStatus {
    status: String,
    coverage: String,
    inferred: bool,
    observations: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TimeBounds {
    min_source_time_ms: u64,
    max_source_time_ms: u64,
    min_received_at_ns: u64,
    max_received_at_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PositioningManifest {
    schema: String,
    venue: String,
    dataset: String,
    source_origin: String,
    admitted_endpoints: Vec<String>,
    observed_endpoints: Vec<String>,
    file: String,
    bytes: u64,
    sha256: String,
    rows: u64,
    observed_at_ns: u64,
    max_staleness_ms: u64,
    coverage: Coverage,
    long_short_ratio: LongShortRatioStatus,
    taker_flow: TakerFlowStatus,
    liquidation: LiquidationStatus,
    time_bounds: TimeBounds,
}

struct NormalizedBatch {
    rows: Vec<PositioningRow>,
    manifest_body: ManifestBody,
}

struct ManifestBody {
    observed_endpoints: Vec<String>,
    coverage: Coverage,
    long_short_ratio: LongShortRatioStatus,
    taker_flow: TakerFlowStatus,
    liquidation: LiquidationStatus,
    time_bounds: TimeBounds,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputRow {
    sequence: u64,
    received_at_ns: u64,
    frame: Value,
}

struct KindClock {
    event_ms: u64,
    secondary_ms: u64,
}

pub fn publish_positioning_rows(
    rows: &[Value],
    config: &PositioningArtifactConfig,
) -> Result<PublishedPositioningArtifact> {
    validate_config(config)?;
    let normalized = normalize_rows(rows, config.observed_at_ns, config.max_staleness_ms)?;
    publish_normalized(&normalized, config)
}

pub fn verify_positioning_artifact(
    published: &PublishedPositioningArtifact,
) -> Result<VerifiedPositioningCounts> {
    let data = read_bound_file(&published.data_path, MAX_DATA_BYTES)?;
    let manifest_bytes = read_bound_file(&published.manifest_path, MAX_MANIFEST_BYTES)?;
    let success = read_bound_file(&published.success_path, MAX_SUCCESS_BYTES)?;
    let data_sha256 = digest(&data);
    let manifest_sha256 = digest(&manifest_bytes);
    if published.data_sha256 != data_sha256 || published.manifest_sha256 != manifest_sha256 {
        bail!("positioning artifact SHA does not match the published paths");
    }
    let success_text =
        std::str::from_utf8(&success).context("positioning _SUCCESS is not utf-8")?;
    if success_text != format!("{data_sha256}\n") {
        bail!("positioning _SUCCESS SHA does not match the data file");
    }
    let manifest: PositioningManifest = serde_json::from_slice(&manifest_bytes)
        .context("positioning manifest cannot be decoded")?;
    if manifest.schema != MANIFEST_SCHEMA
        || manifest.venue != VENUE
        || manifest.dataset != DATASET
        || manifest.source_origin != OFFICIAL_USDM_SOURCE_ORIGIN
        || manifest.file != DATA_NAME
        || manifest.bytes != data.len() as u64
        || manifest.sha256 != data_sha256
        || manifest.admitted_endpoints != admitted_endpoints()
        || manifest.long_short_ratio.status != "unavailable"
        || manifest.long_short_ratio.reason != LONG_SHORT_REASON
        || manifest.long_short_ratio.ratio.is_some()
        || manifest.long_short_ratio.long_account.is_some()
        || manifest.long_short_ratio.short_account.is_some()
        || manifest.liquidation.inferred
        || manifest.liquidation.coverage != FORCE_ORDER_COVERAGE
        || manifest.coverage.stale_rows != 0
        || manifest.coverage.api_error_count != 0
    {
        bail!("positioning manifest identity or unavailable metrics are invalid");
    }
    let stored = parse_data_rows(&data)?;
    if manifest.rows != stored.len() as u64 {
        bail!("positioning manifest row count does not match the data file");
    }
    let inputs = stored
        .iter()
        .map(|row| {
            serde_json::json!({
                "sequence": row.sequence(),
                "received_at_ns": row.received_at_ns(),
                "frame": row.raw().clone(),
            })
        })
        .collect::<Vec<_>>();
    let rebuilt = normalize_rows(&inputs, manifest.observed_at_ns, manifest.max_staleness_ms)?;
    if rebuilt.rows != stored
        || rebuilt.manifest_body.coverage != manifest.coverage
        || rebuilt.manifest_body.taker_flow != manifest.taker_flow
        || rebuilt.manifest_body.liquidation != manifest.liquidation
        || rebuilt.manifest_body.time_bounds != manifest.time_bounds
        || rebuilt.manifest_body.observed_endpoints != manifest.observed_endpoints
        || rebuilt.manifest_body.long_short_ratio != manifest.long_short_ratio
    {
        bail!("positioning artifact does not match its venue frames");
    }
    for row in &stored {
        check_book_invariant(row)?;
    }
    Ok(VerifiedPositioningCounts {
        rows: manifest.rows,
        empty_books: manifest.coverage.empty_books,
        one_sided_books: manifest.coverage.one_sided_books,
        executable_books: manifest.coverage.executable_books,
        liquidations: manifest.coverage.liquidation_observations,
        taker_trades: manifest.coverage.taker_flow_observations,
    })
}

fn validate_config(config: &PositioningArtifactConfig) -> Result<()> {
    if !config.output_root.is_absolute() {
        bail!("positioning output root must be absolute");
    }
    if config.observed_at_ns == 0 {
        bail!("positioning coverage clock must be positive");
    }
    if config.max_staleness_ms == 0 || config.max_staleness_ms > MAX_STALENESS_MS {
        bail!("positioning staleness bound is outside 1..={MAX_STALENESS_MS}ms");
    }
    Ok(())
}

fn admitted_endpoints() -> Vec<String> {
    vec![
        "@aggTrade".to_owned(),
        "@bookTicker".to_owned(),
        "@forceOrder".to_owned(),
    ]
}

fn unavailable_long_short() -> LongShortRatioStatus {
    LongShortRatioStatus {
        status: "unavailable".to_owned(),
        reason: LONG_SHORT_REASON.to_owned(),
        ratio: None,
        long_account: None,
        short_account: None,
    }
}

fn normalize_rows(
    rows: &[Value],
    observed_at_ns: u64,
    max_staleness_ms: u64,
) -> Result<NormalizedBatch> {
    if rows.is_empty() {
        bail!("positioning batch has no observations");
    }
    if max_staleness_ms == 0 || max_staleness_ms > MAX_STALENESS_MS || observed_at_ns == 0 {
        bail!("positioning coverage clock is invalid");
    }
    let mut normalized = Vec::with_capacity(rows.len());
    let mut clocks: BTreeMap<(String, String), KindClock> = BTreeMap::new();
    let mut last_received_at_ns = 0_u64;
    let mut trade_ids = AggregateTradeSequenceValidator::default();
    let mut trades = AggregateTradeSummaryBuilder::default();
    let mut liquidation_ids = BTreeSet::new();
    let mut book_ids: BTreeSet<(String, u64)> = BTreeSet::new();
    let mut endpoints = BTreeSet::new();
    let mut liquidations = 0_u64;
    let mut taker_trades = 0_u64;
    let mut empty_books = 0_u64;
    let mut one_sided_books = 0_u64;
    let mut executable_books = 0_u64;

    for (index, raw_row) in rows.iter().enumerate() {
        let input: InputRow = serde_json::from_value(raw_row.clone())
            .with_context(|| format!("positioning row {index} is missing required fields"))?;
        if input.sequence != index as u64 {
            bail!(
                "positioning sequence gap expected={index} received={}",
                input.sequence
            );
        }
        if input.received_at_ns == 0 || input.received_at_ns < last_received_at_ns {
            bail!(
                "positioning receive time reversal at sequence {}",
                input.sequence
            );
        }
        if input.received_at_ns > observed_at_ns {
            bail!("positioning receive clock is after the coverage clock");
        }
        last_received_at_ns = input.received_at_ns;
        let event_name = input
            .frame
            .get("data")
            .and_then(|data| data.get("e"))
            .and_then(Value::as_str)
            .context("positioning frame is missing its venue event identity")?;
        let row = match event_name {
            "aggTrade" => normalize_trade(
                &input,
                &mut trade_ids,
                &mut trades,
                &mut clocks,
                observed_at_ns,
                max_staleness_ms,
            )?,
            "forceOrder" => normalize_liquidation(
                &input,
                &mut liquidation_ids,
                &mut clocks,
                observed_at_ns,
                max_staleness_ms,
            )?,
            "bookTicker" => normalize_book(
                &input,
                &mut book_ids,
                &mut clocks,
                observed_at_ns,
                max_staleness_ms,
            )?,
            other => bail!("unsupported positioning frame {other}"),
        };
        match &row {
            PositioningRow::Liquidation { .. } => liquidations += 1,
            PositioningRow::TakerFlow { .. } => taker_trades += 1,
            PositioningRow::Book { book_state, .. } => match book_state {
                BookState::Empty => empty_books += 1,
                BookState::OneSided => one_sided_books += 1,
                BookState::Executable => executable_books += 1,
            },
        }
        endpoints.insert(row.endpoint().to_owned());
        check_book_invariant(&row)?;
        normalized.push(row);
    }

    let taker_flow = if taker_trades == 0 {
        TakerFlowStatus::NoEvents
    } else {
        TakerFlowStatus::Observed {
            summaries: trades.finish()?,
        }
    };
    let liquidation = LiquidationStatus {
        status: if liquidations == 0 {
            "no_events".to_owned()
        } else {
            "observed".to_owned()
        },
        coverage: FORCE_ORDER_COVERAGE.to_owned(),
        inferred: false,
        observations: liquidations,
    };
    let time_bounds = time_bounds(&normalized)?;
    Ok(NormalizedBatch {
        rows: normalized,
        manifest_body: ManifestBody {
            observed_endpoints: endpoints.into_iter().collect(),
            coverage: Coverage {
                liquidation_observations: liquidations,
                taker_flow_observations: taker_trades,
                empty_books,
                one_sided_books,
                executable_books,
                stale_rows: 0,
                api_error_count: 0,
            },
            long_short_ratio: unavailable_long_short(),
            taker_flow,
            liquidation,
            time_bounds,
        },
    })
}

fn normalize_trade(
    input: &InputRow,
    trade_ids: &mut AggregateTradeSequenceValidator,
    trades: &mut AggregateTradeSummaryBuilder,
    clocks: &mut BTreeMap<(String, String), KindClock>,
    observed_at_ns: u64,
    max_staleness_ms: u64,
) -> Result<PositioningRow> {
    let trade = AggregateTrade::from_frame(&input.frame, input.received_at_ns)?;
    trade_ids.observe(&trade)?;
    trades.observe(&trade)?;
    remember_clock(
        clocks,
        "aggTrade",
        &trade.symbol,
        trade.event_time_ms,
        trade.trade_time_ms,
    )?;
    check_staleness(
        trade.event_time_ms,
        observed_at_ns,
        max_staleness_ms,
        "aggTrade",
    )?;
    Ok(PositioningRow::TakerFlow {
        sequence: input.sequence,
        venue: VENUE.to_owned(),
        symbol: trade.symbol,
        source_time_ms: trade.event_time_ms,
        trade_time_ms: trade.trade_time_ms,
        received_at_ns: trade.received_at_ns,
        source_endpoint: "@aggTrade".to_owned(),
        aggregate_trade_id: trade.aggregate_trade_id,
        is_buyer_maker: trade.is_buyer_maker,
        price: decimal_string(trade.price),
        quantity: decimal_string(trade.quantity),
        executable: false,
        raw: input.frame.clone(),
    })
}

fn normalize_liquidation(
    input: &InputRow,
    seen: &mut BTreeSet<String>,
    clocks: &mut BTreeMap<(String, String), KindClock>,
    observed_at_ns: u64,
    max_staleness_ms: u64,
) -> Result<PositioningRow> {
    let observation = force_order_observation(&input.frame, input.received_at_ns)?;
    if observation.coverage != FORCE_ORDER_COVERAGE {
        bail!("liquidation coverage is outside the admitted force-order scope");
    }
    if !seen.insert(observation.content_identity()) {
        bail!(
            "duplicate liquidation observation for {}",
            observation.symbol
        );
    }
    remember_clock(
        clocks,
        "forceOrder",
        &observation.symbol,
        observation.event_time_ms,
        observation.order_time_ms,
    )?;
    check_staleness(
        observation.event_time_ms,
        observed_at_ns,
        max_staleness_ms,
        "forceOrder",
    )?;
    Ok(liquidation_row(input, &observation))
}

fn liquidation_row(input: &InputRow, observation: &ForceOrderObservation) -> PositioningRow {
    PositioningRow::Liquidation {
        sequence: input.sequence,
        venue: VENUE.to_owned(),
        symbol: observation.symbol.clone(),
        source_time_ms: observation.event_time_ms,
        order_time_ms: observation.order_time_ms,
        received_at_ns: observation.received_at_ns,
        source_endpoint: observation.source_endpoint.clone(),
        coverage: observation.coverage.clone(),
        side: observation.side.clone(),
        price: decimal_string(observation.price),
        quantity: decimal_string(observation.original_quantity),
        executable: false,
        raw: input.frame.clone(),
    }
}

fn normalize_book(
    input: &InputRow,
    seen: &mut BTreeSet<(String, u64)>,
    clocks: &mut BTreeMap<(String, String), KindClock>,
    observed_at_ns: u64,
    max_staleness_ms: u64,
) -> Result<PositioningRow> {
    let ticker = BookTicker::from_frame(&input.frame, input.received_at_ns)?;
    if !seen.insert((ticker.symbol.clone(), ticker.update_id)) {
        bail!(
            "duplicate book ticker {} update {}",
            ticker.symbol,
            ticker.update_id
        );
    }
    remember_clock(
        clocks,
        "bookTicker",
        &ticker.symbol,
        ticker.event_time_ms,
        ticker.event_time_ms,
    )?;
    check_staleness(
        ticker.event_time_ms,
        observed_at_ns,
        max_staleness_ms,
        "bookTicker",
    )?;
    let (book_state, executable) = classify_book(&ticker);
    Ok(PositioningRow::Book {
        sequence: input.sequence,
        venue: VENUE.to_owned(),
        symbol: ticker.symbol,
        source_time_ms: ticker.event_time_ms,
        received_at_ns: ticker.received_at_ns,
        source_endpoint: "@bookTicker".to_owned(),
        update_id: ticker.update_id,
        book_state,
        executable,
        bid_price: decimal_string(ticker.best_bid_price),
        bid_quantity: decimal_string(ticker.best_bid_quantity),
        ask_price: decimal_string(ticker.best_ask_price),
        ask_quantity: decimal_string(ticker.best_ask_quantity),
        raw: input.frame.clone(),
    })
}

fn classify_book(ticker: &BookTicker) -> (BookState, bool) {
    let bid = ticker.best_bid_quantity > Decimal::ZERO;
    let ask = ticker.best_ask_quantity > Decimal::ZERO;
    match (bid, ask) {
        (false, false) => (BookState::Empty, false),
        (true, false) | (false, true) => (BookState::OneSided, false),
        (true, true) => (BookState::Executable, true),
    }
}

fn remember_clock(
    clocks: &mut BTreeMap<(String, String), KindClock>,
    kind: &str,
    symbol: &str,
    event_ms: u64,
    secondary_ms: u64,
) -> Result<()> {
    let key = (kind.to_owned(), symbol.to_owned());
    if let Some(previous) = clocks.get(&key) {
        if event_ms < previous.event_ms || secondary_ms < previous.secondary_ms {
            bail!("{symbol} {kind} source time reversal");
        }
    }
    clocks.insert(
        key,
        KindClock {
            event_ms,
            secondary_ms,
        },
    );
    Ok(())
}

fn check_staleness(
    source_time_ms: u64,
    observed_at_ns: u64,
    max_staleness_ms: u64,
    kind: &str,
) -> Result<()> {
    let observed_at_ms = observed_at_ns / 1_000_000;
    if source_time_ms > observed_at_ms.saturating_add(MAX_SOURCE_LEAD_MS) {
        bail!("{kind} source clock leads the positioning coverage clock");
    }
    if observed_at_ms.saturating_sub(source_time_ms) > max_staleness_ms {
        bail!("{kind} source clock is stale");
    }
    Ok(())
}

fn check_book_invariant(row: &PositioningRow) -> Result<()> {
    let PositioningRow::Book {
        book_state,
        executable,
        bid_quantity,
        ask_quantity,
        ..
    } = row
    else {
        return Ok(());
    };
    let bid_empty = decimal_is_zero(bid_quantity)?;
    let ask_empty = decimal_is_zero(ask_quantity)?;
    match book_state {
        BookState::Empty => {
            if *executable || !bid_empty || !ask_empty {
                bail!("empty book was filled or marked executable");
            }
        }
        BookState::OneSided => {
            if *executable || bid_empty == ask_empty {
                bail!("one-sided book lost its empty side or was marked executable");
            }
        }
        BookState::Executable => {
            if !*executable || bid_empty || ask_empty {
                bail!("executable book is missing a side");
            }
        }
    }
    Ok(())
}

fn decimal_is_zero(value: &str) -> Result<bool> {
    let parsed = Decimal::from_str(value).context("positioning size is not decimal")?;
    Ok(parsed == Decimal::ZERO)
}

fn decimal_string(value: Decimal) -> String {
    value.normalize().to_string()
}

fn time_bounds(rows: &[PositioningRow]) -> Result<TimeBounds> {
    let mut bounds = rows
        .iter()
        .map(|row| (row.source_time_ms(), row.received_at_ns()));
    let Some((first_source, first_received)) = bounds.next() else {
        bail!("positioning batch has no observations");
    };
    let mut time_bounds = TimeBounds {
        min_source_time_ms: first_source,
        max_source_time_ms: first_source,
        min_received_at_ns: first_received,
        max_received_at_ns: first_received,
    };
    for (source_time_ms, received_at_ns) in bounds {
        time_bounds.min_source_time_ms = time_bounds.min_source_time_ms.min(source_time_ms);
        time_bounds.max_source_time_ms = time_bounds.max_source_time_ms.max(source_time_ms);
        time_bounds.min_received_at_ns = time_bounds.min_received_at_ns.min(received_at_ns);
        time_bounds.max_received_at_ns = time_bounds.max_received_at_ns.max(received_at_ns);
    }
    Ok(time_bounds)
}

fn parse_data_rows(data: &[u8]) -> Result<Vec<PositioningRow>> {
    let text = std::str::from_utf8(data).context("positioning data is not utf-8")?;
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            bail!("positioning data has a blank row");
        }
        let row: PositioningRow = serde_json::from_str(line)
            .with_context(|| format!("positioning data row {index} is invalid"))?;
        if row.sequence() != index as u64 {
            bail!("positioning data sequence gap at row {index}");
        }
        rows.push(row);
    }
    if rows.is_empty() {
        bail!("positioning data has no observations");
    }
    Ok(rows)
}

fn publish_normalized(
    normalized: &NormalizedBatch,
    config: &PositioningArtifactConfig,
) -> Result<PublishedPositioningArtifact> {
    let mut data = Vec::new();
    for row in &normalized.rows {
        serde_json::to_writer(&mut data, row)?;
        data.push(b'\n');
    }
    let data_sha256 = digest(&data);
    let (date, hour) = utc_partition(config.observed_at_ns)?;
    let hour_dir = config
        .output_root
        .join("lake/raw")
        .join(format!("venue={VENUE}"))
        .join(format!("dataset={DATASET}"))
        .join(format!("date={date}"))
        .join(format!("hour={hour}"));
    ensure_canonical_directory(&hour_dir)?;
    let final_dir = hour_dir.join(format!("batch={}", config.observed_at_ns));
    if fs::symlink_metadata(&final_dir).is_ok() {
        bail!("positioning artifact batch already exists");
    }
    let body = &normalized.manifest_body;
    let manifest = PositioningManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        venue: VENUE.to_owned(),
        dataset: DATASET.to_owned(),
        source_origin: OFFICIAL_USDM_SOURCE_ORIGIN.to_owned(),
        admitted_endpoints: admitted_endpoints(),
        observed_endpoints: body.observed_endpoints.clone(),
        file: DATA_NAME.to_owned(),
        bytes: data.len() as u64,
        sha256: data_sha256.clone(),
        rows: normalized.rows.len() as u64,
        observed_at_ns: config.observed_at_ns,
        max_staleness_ms: config.max_staleness_ms,
        coverage: body.coverage.clone(),
        long_short_ratio: body.long_short_ratio.clone(),
        taker_flow: body.taker_flow.clone(),
        liquidation: body.liquidation.clone(),
        time_bounds: body.time_bounds.clone(),
    };
    let mut manifest_bytes = serde_json::to_vec(&manifest)?;
    manifest_bytes.push(b'\n');
    let manifest_sha256 = digest(&manifest_bytes);
    let mut staging = StagingDir::create(&hour_dir)?;
    write_new(&staging.path.join(DATA_NAME), &data)?;
    write_new(
        &staging.path.join(format!("{DATA_NAME}.manifest.json")),
        &manifest_bytes,
    )?;
    write_new(
        &staging.path.join(format!("{DATA_NAME}._SUCCESS")),
        format!("{data_sha256}\n").as_bytes(),
    )?;
    File::open(&staging.path)?.sync_all()?;
    rename_noreplace(&staging.path, &final_dir)?;
    staging.published = true;
    File::open(&hour_dir)?.sync_all()?;
    Ok(PublishedPositioningArtifact {
        data_path: final_dir.join(DATA_NAME),
        manifest_path: final_dir.join(format!("{DATA_NAME}.manifest.json")),
        success_path: final_dir.join(format!("{DATA_NAME}._SUCCESS")),
        data_sha256,
        manifest_sha256,
    })
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn utc_partition(timestamp_ns: u64) -> Result<(String, String)> {
    let seconds = i64::try_from(timestamp_ns / 1_000_000_000)?;
    let nanos = u32::try_from(timestamp_ns % 1_000_000_000)?;
    let observed = DateTime::<Utc>::from_timestamp(seconds, nanos)
        .context("positioning observed time is outside UTC range")?;
    Ok((
        observed.format("%Y-%m-%d").to_string(),
        observed.format("%H").to_string(),
    ))
}

struct StagingDir {
    path: PathBuf,
    published: bool,
}

impl StagingDir {
    fn create(parent: &Path) -> Result<Self> {
        for _ in 0..32 {
            let path = parent.join(format!(".positioning-staging.{:016x}", random::<u64>()));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        published: false,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        bail!("could not allocate positioning artifact staging directory")
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const RECEIVED_NS: u64 = 1_700_000_000_500_000_000;
    const OBSERVED_NS: u64 = RECEIVED_NS + 5_000_000_000;
    const STALENESS_MS: u64 = 60_000;

    fn config(root: &Path) -> PositioningArtifactConfig {
        PositioningArtifactConfig {
            output_root: fs::canonicalize(root).unwrap(),
            observed_at_ns: OBSERVED_NS,
            max_staleness_ms: STALENESS_MS,
        }
    }

    fn input(sequence: u64, step: u64, frame: Value) -> Value {
        json!({
            "sequence": sequence,
            "received_at_ns": RECEIVED_NS + step * 1_000_000_000,
            "frame": frame,
        })
    }

    fn agg(id: u64, event_ms: u64, buyer_maker: bool) -> Value {
        json!({
            "stream": "btcusdt@aggTrade",
            "data": {
                "e": "aggTrade",
                "E": event_ms,
                "s": "BTCUSDT",
                "a": id,
                "f": id,
                "l": id,
                "p": "100",
                "q": "2",
                "T": event_ms,
                "m": buyer_maker
            }
        })
    }

    fn liquidation(event_ms: u64) -> Value {
        json!({
            "stream": "btcusdt@forceOrder",
            "data": {
                "e": "forceOrder",
                "E": event_ms,
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
                    "T": event_ms
                }
            }
        })
    }

    fn book(
        event_ms: u64,
        update_id: u64,
        bid_price: &str,
        bid_quantity: &str,
        ask_price: &str,
        ask_quantity: &str,
    ) -> Value {
        json!({
            "stream": "btcusdt@bookTicker",
            "data": {
                "e": "bookTicker",
                "u": update_id,
                "E": event_ms,
                "T": event_ms,
                "s": "BTCUSDT",
                "b": bid_price,
                "B": bid_quantity,
                "a": ask_price,
                "A": ask_quantity
            }
        })
    }

    fn happy_rows() -> Vec<Value> {
        vec![
            input(0, 0, agg(10, SOURCE_MS, true)),
            input(1, 1, agg(11, SOURCE_MS + 1_000, false)),
            input(2, 2, liquidation(SOURCE_MS + 2_000)),
            input(3, 3, book(SOURCE_MS + 3_000, 1, "0", "0", "0", "0")),
            input(4, 4, book(SOURCE_MS + 4_000, 2, "100.5", "1", "0", "0")),
            input(5, 5, book(SOURCE_MS + 5_000, 3, "100.5", "1", "100.6", "2")),
        ]
    }

    fn assert_no_success(root: &Path) {
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let entries = fs::read_dir(&dir).unwrap();
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let name = path.file_name().unwrap().to_string_lossy();
                    assert!(
                        !name.contains("_SUCCESS"),
                        "fail-closed path wrote {path:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn positioning_artifact_fail_closed_fixture_keeps_clocks_sha_and_empty_books() {
        let root = tempfile::tempdir().unwrap();
        let published = publish_positioning_rows(&happy_rows(), &config(root.path())).unwrap();
        let counts = verify_positioning_artifact(&published).unwrap();

        assert_eq!(counts.rows, 6);
        assert_eq!(counts.taker_trades, 2);
        assert_eq!(counts.liquidations, 1);
        assert_eq!(counts.empty_books, 1);
        assert_eq!(counts.one_sided_books, 1);
        assert_eq!(counts.executable_books, 1);
        assert_eq!(
            fs::read_to_string(&published.success_path).unwrap(),
            format!("{}\n", published.data_sha256)
        );

        let manifest: PositioningManifest =
            serde_json::from_slice(&fs::read(&published.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.venue, VENUE);
        assert_eq!(manifest.source_origin, OFFICIAL_USDM_SOURCE_ORIGIN);
        assert_eq!(manifest.sha256, published.data_sha256);
        assert_eq!(manifest.long_short_ratio.status, "unavailable");
        assert_eq!(manifest.long_short_ratio.ratio, None);
        assert_eq!(manifest.long_short_ratio.long_account, None);
        assert_eq!(manifest.long_short_ratio.short_account, None);
        assert!(!manifest.liquidation.inferred);
        assert_eq!(manifest.liquidation.coverage, FORCE_ORDER_COVERAGE);
        let TakerFlowStatus::Observed { summaries } = manifest.taker_flow else {
            panic!("observed aggTrade taker flow must stay on the manifest");
        };
        let summary = summaries.get("BTCUSDT").unwrap();
        assert_eq!(summary.aggregate_trade_count, 2);
        assert_eq!(summary.buyer_aggressor_base_volume, "2");
        assert_eq!(summary.seller_aggressor_base_volume, "2");
        assert_eq!(summary.vwap, "100");

        let data = fs::read_to_string(&published.data_path).unwrap();
        let rows: Vec<PositioningRow> = data
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        match &rows[3] {
            PositioningRow::Book {
                book_state,
                executable,
                bid_price,
                bid_quantity,
                ask_price,
                ask_quantity,
                source_time_ms,
                received_at_ns,
                venue,
                ..
            } => {
                assert_eq!(*book_state, BookState::Empty);
                assert!(!executable);
                assert_eq!(bid_price, "0");
                assert_eq!(bid_quantity, "0");
                assert_eq!(ask_price, "0");
                assert_eq!(ask_quantity, "0");
                assert_eq!(*source_time_ms, SOURCE_MS + 3_000);
                assert_eq!(*received_at_ns, RECEIVED_NS + 3_000_000_000);
                assert_eq!(venue, VENUE);
            }
            other => panic!("expected an empty book, got {other:?}"),
        }
        match &rows[4] {
            PositioningRow::Book {
                book_state,
                executable,
                bid_quantity,
                ask_price,
                ask_quantity,
                ..
            } => {
                assert_eq!(*book_state, BookState::OneSided);
                assert!(!executable);
                assert_eq!(bid_quantity, "1");
                assert_eq!(ask_price, "0");
                assert_eq!(ask_quantity, "0");
            }
            other => panic!("expected a one-sided book, got {other:?}"),
        }

        let mut tampered = fs::read(&published.success_path).unwrap();
        tampered[0] = b'0';
        fs::write(&published.success_path, tampered).unwrap();
        let error = verify_positioning_artifact(&published)
            .unwrap_err()
            .to_string();
        assert!(error.contains("SHA"), "{error}");
    }

    #[test]
    fn positioning_artifact_fail_closed_on_missing_success_sequence_gap_time_and_fields() {
        let root = tempfile::tempdir().unwrap();
        let published = publish_positioning_rows(&happy_rows(), &config(root.path())).unwrap();
        fs::remove_file(&published.success_path).unwrap();
        let error = verify_positioning_artifact(&published)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("_SUCCESS") || error.contains("open"),
            "{error}"
        );

        let gap_root = tempfile::tempdir().unwrap();
        let mut gap_rows = happy_rows();
        gap_rows[1] = input(1, 1, agg(12, SOURCE_MS + 1_000, false));
        let gap_error = publish_positioning_rows(&gap_rows, &config(gap_root.path()))
            .unwrap_err()
            .to_string();
        assert!(gap_error.contains("gap"), "{gap_error}");
        assert_no_success(gap_root.path());

        let reverse_root = tempfile::tempdir().unwrap();
        let mut reverse_rows = happy_rows();
        reverse_rows[1] = input(1, 1, agg(11, SOURCE_MS - 1, false));
        let reverse_error = publish_positioning_rows(&reverse_rows, &config(reverse_root.path()))
            .unwrap_err()
            .to_string();
        assert!(
            reverse_error.contains("rollback") || reverse_error.contains("reversal"),
            "{reverse_error}"
        );
        assert_no_success(reverse_root.path());

        let missing_root = tempfile::tempdir().unwrap();
        let mut missing = liquidation(SOURCE_MS);
        missing
            .pointer_mut("/data/o")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("q");
        let missing_error =
            publish_positioning_rows(&[input(0, 0, missing)], &config(missing_root.path()))
                .unwrap_err()
                .to_string();
        assert!(missing_error.contains('q'), "{missing_error}");
        assert_no_success(missing_root.path());
    }

    #[test]
    fn positioning_artifact_fail_closed_keeps_absent_metrics_unset() {
        let root = tempfile::tempdir().unwrap();
        let rows = vec![input(0, 0, book(SOURCE_MS, 1, "0", "0", "0", "0"))];
        let published = publish_positioning_rows(&rows, &config(root.path())).unwrap();
        let counts = verify_positioning_artifact(&published).unwrap();
        assert_eq!(counts.empty_books, 1);
        assert_eq!(counts.executable_books, 0);
        assert_eq!(counts.liquidations, 0);
        assert_eq!(counts.taker_trades, 0);

        let manifest: Value =
            serde_json::from_slice(&fs::read(&published.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["taker_flow"]["status"], "no_events");
        assert_eq!(manifest["liquidation"]["status"], "no_events");
        assert_eq!(manifest["liquidation"]["inferred"], false);
        assert_eq!(manifest["liquidation"]["observations"], 0);
        assert!(manifest["taker_flow"].get("summaries").is_none());
        assert_eq!(manifest["long_short_ratio"]["ratio"], Value::Null);
        let text = fs::read_to_string(&published.manifest_path).unwrap();
        assert!(!text.contains("\"ratio\":\"0\""));
        assert!(!text.contains("\"buyer_aggressor_base_volume\":\"0\""));
    }
}
