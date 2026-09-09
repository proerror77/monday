//! Fail-closed normalization for official Binance USD-M reference observations.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::binance_market_tape::MAX_SOURCE_LEAD_MS;
use crate::binance_reference_common::{
    required_decimal, required_filter_decimal, required_string, required_u64,
    validate_receive_clock, validate_source_clock_not_future, validate_symbol,
};
pub use crate::binance_reference_common::{
    ReferenceClockValidator, ReferenceKind, ReferenceMarket,
};

pub const REFERENCE_SCHEMA: &str = "binance.usdm_reference.v3";
pub const EXCHANGE_INFO_ENDPOINT: &str = "/fapi/v1/exchangeInfo";
pub const SERVER_TIME_ENDPOINT: &str = "/fapi/v1/time";
pub const PREMIUM_INDEX_ENDPOINT: &str = "/fapi/v1/premiumIndex";
pub const OPEN_INTEREST_ENDPOINT: &str = "/fapi/v1/openInterest";
pub const BASIS_ENDPOINT: &str = "/futures/data/basis";
pub const FORCE_ORDER_ENDPOINT: &str = "@forceOrder";
pub const FORCE_ORDER_COVERAGE: &str = "configured_symbols_only_not_full_market";
pub const BASIS_PERIODS: &[&str] = &[
    "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d", "3d",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivePerpetualContract {
    pub schema: String,
    pub symbol: String,
    pub pair: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub margin_asset: String,
    pub tick_size: Decimal,
    pub step_size: Decimal,
    pub min_notional: Decimal,
    pub contract_type: String,
    pub status: String,
    pub onboard_date_ms: u64,
    pub delivery_date_ms: u64,
    pub source_time_ms: u64,
    pub source_clock_received_at_ns: u64,
    pub received_at_ns: u64,
    pub source_endpoint: String,
    pub source_clock_endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkIndexFundingObservation {
    pub schema: String,
    pub symbol: String,
    pub mark_price: Decimal,
    pub index_price: Decimal,
    pub basis: Decimal,
    pub basis_rate: Decimal,
    pub last_funding_rate: Decimal,
    pub interest_rate: Decimal,
    pub next_funding_time_ms: u64,
    pub source_time_ms: u64,
    pub received_at_ns: u64,
    pub source_endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenInterestObservation {
    pub schema: String,
    pub symbol: String,
    pub open_interest: Decimal,
    pub source_time_ms: u64,
    pub received_at_ns: u64,
    pub source_endpoint: String,
}

/// One interval basis observation from Binance's public futures-data endpoint.
/// The interval and exchange timestamp stay attached to the row because basis
/// is an observation series, not a field that can be reconstructed from a
/// later mark/index response without changing its availability semantics. The
/// exchange timestamp is the start of the requested interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BasisObservation {
    pub schema: String,
    pub symbol: String,
    pub pair: String,
    pub contract_type: String,
    pub period: String,
    pub index_price: Decimal,
    pub futures_price: Decimal,
    pub basis: Decimal,
    pub basis_rate: Decimal,
    /// Binance returns an empty string for this field on normal PERPETUAL
    /// rows. Empty means not applicable; a missing or malformed field remains
    /// an error so absence is not silently converted into zero.
    pub annualized_basis_rate: Option<Decimal>,
    pub source_time_ms: u64,
    pub received_at_ns: u64,
    pub source_endpoint: String,
}

/// One public USD-M liquidation notification. This is an observation of the
/// venue's force-order stream; it carries no claim that the local process
/// executed or filled an order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForceOrderObservation {
    pub schema: String,
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub time_in_force: String,
    pub original_quantity: Decimal,
    pub price: Decimal,
    pub average_price: Decimal,
    pub status: String,
    pub last_filled_quantity: Decimal,
    pub cumulative_filled_quantity: Decimal,
    /// Binance wire field `o.T` (order/trade time), retained as order_time.
    pub order_time_ms: u64,
    /// Binance wire field `E` (event time).
    pub event_time_ms: u64,
    pub received_at_ns: u64,
    pub source_endpoint: String,
    /// The stream is configured per symbol; this observation cannot establish
    /// complete liquidation coverage for the whole USD-M market.
    pub coverage: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceCoverage {
    pub active_contracts: u64,
    pub metadata_observations: u64,
    pub mark_index_funding_observations: u64,
    pub open_interest_observations: u64,
    pub stale_metadata: u64,
    pub stale_mark_index_funding: u64,
    pub stale_open_interest: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteReferenceBatch {
    contracts: Vec<ActivePerpetualContract>,
    mark_index_funding: Vec<MarkIndexFundingObservation>,
    open_interest: Vec<OpenInterestObservation>,
}

impl CompleteReferenceBatch {
    pub fn new(
        mut contracts: Vec<ActivePerpetualContract>,
        mut mark_index_funding: Vec<MarkIndexFundingObservation>,
        mut open_interest: Vec<OpenInterestObservation>,
    ) -> Result<Self> {
        for row in &contracts {
            validate_contract(row)?;
        }
        for row in &mark_index_funding {
            validate_mark_index_funding(row)?;
        }
        for row in &open_interest {
            validate_open_interest(row)?;
        }
        let expected = unique_symbols(contracts.iter().map(|row| row.symbol.as_str()), "metadata")?;
        if expected.is_empty() {
            bail!("reference batch has no active contracts");
        }
        let mark_symbols = unique_symbols(
            mark_index_funding.iter().map(|row| row.symbol.as_str()),
            "mark/index/funding",
        )?;
        if mark_symbols != expected {
            bail!("reference batch has incomplete mark/index/funding coverage");
        }
        let open_interest_symbols = unique_symbols(
            open_interest.iter().map(|row| row.symbol.as_str()),
            "open-interest",
        )?;
        if open_interest_symbols != expected {
            bail!("reference batch has incomplete open-interest coverage");
        }
        contracts.sort_by(|left, right| left.symbol.cmp(&right.symbol));
        mark_index_funding.sort_by(|left, right| left.symbol.cmp(&right.symbol));
        open_interest.sort_by(|left, right| left.symbol.cmp(&right.symbol));
        Ok(Self {
            contracts,
            mark_index_funding,
            open_interest,
        })
    }

    pub fn contracts(&self) -> &[ActivePerpetualContract] {
        &self.contracts
    }

    pub fn mark_index_funding(&self) -> &[MarkIndexFundingObservation] {
        &self.mark_index_funding
    }

    pub fn open_interest(&self) -> &[OpenInterestObservation] {
        &self.open_interest
    }

    pub fn coverage(
        &self,
        observed_at_ns: u64,
        max_staleness_ms: u64,
    ) -> Result<ReferenceCoverage> {
        Ok(ReferenceCoverage {
            active_contracts: self.contracts.len() as u64,
            metadata_observations: self.contracts.len() as u64,
            mark_index_funding_observations: self.mark_index_funding.len() as u64,
            open_interest_observations: self.open_interest.len() as u64,
            stale_metadata: stale_count(
                self.contracts.iter().map(|row| row.source_time_ms),
                observed_at_ns,
                max_staleness_ms,
            )?,
            stale_mark_index_funding: stale_count(
                self.mark_index_funding.iter().map(|row| row.source_time_ms),
                observed_at_ns,
                max_staleness_ms,
            )?,
            stale_open_interest: stale_count(
                self.open_interest.iter().map(|row| row.source_time_ms),
                observed_at_ns,
                max_staleness_ms,
            )?,
        })
    }
}

fn validate_contract(row: &ActivePerpetualContract) -> Result<()> {
    validate_symbol(&row.symbol)?;
    validate_receive_clock(row.source_time_ms, row.source_clock_received_at_ns)?;
    validate_receive_clock(row.source_time_ms, row.received_at_ns)?;
    if row.source_clock_received_at_ns > row.received_at_ns {
        bail!("reference metadata precedes its source-clock receipt");
    }
    if row.schema != REFERENCE_SCHEMA
        || row.source_endpoint != EXCHANGE_INFO_ENDPOINT
        || row.source_clock_endpoint != SERVER_TIME_ENDPOINT
        || row.contract_type != "PERPETUAL"
        || row.status != "TRADING"
    {
        bail!("reference metadata identity is invalid");
    }
    if row.pair.is_empty()
        || row.base_asset.is_empty()
        || row.quote_asset.is_empty()
        || row.margin_asset.is_empty()
    {
        bail!("reference metadata has an empty contract identity");
    }
    if row.tick_size <= Decimal::ZERO
        || row.step_size <= Decimal::ZERO
        || row.min_notional <= Decimal::ZERO
    {
        bail!("reference trading rules must be positive");
    }
    Ok(())
}

fn validate_mark_index_funding(row: &MarkIndexFundingObservation) -> Result<()> {
    validate_symbol(&row.symbol)?;
    validate_receive_clock(row.source_time_ms, row.received_at_ns)?;
    if row.schema != REFERENCE_SCHEMA || row.source_endpoint != PREMIUM_INDEX_ENDPOINT {
        bail!("mark/index/funding source identity is invalid");
    }
    if row.mark_price <= Decimal::ZERO || row.index_price <= Decimal::ZERO {
        bail!("mark and index prices must be positive");
    }
    if row.next_funding_time_ms < row.source_time_ms {
        bail!("next funding time precedes source time");
    }
    let expected_basis = row
        .mark_price
        .checked_sub(row.index_price)
        .context("basis overflow")?;
    let expected_basis_rate = expected_basis
        .checked_div(row.index_price)
        .context("basis-rate overflow")?;
    if row.basis != expected_basis || row.basis_rate != expected_basis_rate {
        bail!("mark/index/funding derived basis is inconsistent");
    }
    Ok(())
}

fn validate_open_interest(row: &OpenInterestObservation) -> Result<()> {
    validate_symbol(&row.symbol)?;
    validate_source_clock_not_future(row.source_time_ms, row.received_at_ns)?;
    if row.schema != REFERENCE_SCHEMA || row.source_endpoint != OPEN_INTEREST_ENDPOINT {
        bail!("open-interest source identity is invalid");
    }
    if row.open_interest < Decimal::ZERO {
        bail!("open interest cannot be negative");
    }
    Ok(())
}

pub fn active_perpetual_contracts(
    exchange_info: &Value,
    source_time_ms: u64,
    source_clock_received_at_ns: u64,
    received_at_ns: u64,
) -> Result<Vec<ActivePerpetualContract>> {
    validate_receive_clock(source_time_ms, source_clock_received_at_ns)?;
    validate_receive_clock(source_time_ms, received_at_ns)?;
    if source_clock_received_at_ns > received_at_ns {
        bail!("exchangeInfo response precedes its source-clock receipt");
    }
    let symbols = exchange_info
        .get("symbols")
        .and_then(Value::as_array)
        .context("exchangeInfo symbols must be an array")?;
    let mut seen = BTreeSet::new();
    let mut contracts = Vec::new();
    for raw in symbols {
        let symbol = required_string(raw, "symbol", "exchangeInfo")?.to_ascii_uppercase();
        let contract_type = required_string(raw, "contractType", "exchangeInfo")?;
        let status = required_string(raw, "status", "exchangeInfo")?;
        if contract_type != "PERPETUAL" || status != "TRADING" {
            continue;
        }
        validate_symbol(&symbol)?;
        if !seen.insert(symbol.clone()) {
            bail!("duplicate active perpetual contract {symbol}");
        }
        contracts.push(ActivePerpetualContract {
            schema: REFERENCE_SCHEMA.to_owned(),
            symbol,
            pair: required_string(raw, "pair", "exchangeInfo")?.to_owned(),
            base_asset: required_string(raw, "baseAsset", "exchangeInfo")?.to_owned(),
            quote_asset: required_string(raw, "quoteAsset", "exchangeInfo")?.to_owned(),
            margin_asset: required_string(raw, "marginAsset", "exchangeInfo")?.to_owned(),
            tick_size: required_filter_decimal(raw, "PRICE_FILTER", "tickSize")?,
            step_size: required_filter_decimal(raw, "LOT_SIZE", "stepSize")?,
            min_notional: required_filter_decimal(raw, "MIN_NOTIONAL", "notional")?,
            contract_type: contract_type.to_owned(),
            status: status.to_owned(),
            onboard_date_ms: required_u64(raw, "onboardDate", "exchangeInfo")?,
            delivery_date_ms: required_u64(raw, "deliveryDate", "exchangeInfo")?,
            source_time_ms,
            source_clock_received_at_ns,
            received_at_ns,
            source_endpoint: EXCHANGE_INFO_ENDPOINT.to_owned(),
            source_clock_endpoint: SERVER_TIME_ENDPOINT.to_owned(),
        });
    }
    if contracts.is_empty() {
        bail!("exchangeInfo has no active USD-M perpetual contracts");
    }
    contracts.sort_by(|left, right| left.symbol.cmp(&right.symbol));
    Ok(contracts)
}

pub fn mark_index_funding_observations(
    premium_index: &Value,
    expected_symbols: &BTreeSet<String>,
    received_at_ns: u64,
) -> Result<Vec<MarkIndexFundingObservation>> {
    if expected_symbols.is_empty() {
        bail!("mark/index/funding expected symbol set is empty");
    }
    let rows = premium_index
        .as_array()
        .context("premiumIndex response must be an array")?;
    let mut observations = BTreeMap::new();
    for raw in rows {
        let symbol = required_string(raw, "symbol", "premiumIndex")?.to_ascii_uppercase();
        if !expected_symbols.contains(&symbol) {
            continue;
        }
        let source_time_ms = required_u64(raw, "time", "premiumIndex")?;
        validate_receive_clock(source_time_ms, received_at_ns)?;
        let mark_price = required_decimal(raw, "markPrice", "premiumIndex")?;
        let index_price = required_decimal(raw, "indexPrice", "premiumIndex")?;
        if mark_price <= Decimal::ZERO {
            bail!("mark price must be positive");
        }
        if index_price <= Decimal::ZERO {
            bail!("index price must be positive");
        }
        let next_funding_time_ms = required_u64(raw, "nextFundingTime", "premiumIndex")?;
        if next_funding_time_ms < source_time_ms {
            bail!("next funding time precedes source time");
        }
        let basis = mark_price
            .checked_sub(index_price)
            .context("basis overflow")?;
        let basis_rate = basis
            .checked_div(index_price)
            .context("basis-rate overflow")?;
        let observation = MarkIndexFundingObservation {
            schema: REFERENCE_SCHEMA.to_owned(),
            symbol: symbol.clone(),
            mark_price,
            index_price,
            basis,
            basis_rate,
            last_funding_rate: required_decimal(raw, "lastFundingRate", "premiumIndex")?,
            interest_rate: required_decimal(raw, "interestRate", "premiumIndex")?,
            next_funding_time_ms,
            source_time_ms,
            received_at_ns,
            source_endpoint: PREMIUM_INDEX_ENDPOINT.to_owned(),
        };
        if observations.insert(symbol.clone(), observation).is_some() {
            bail!("duplicate premiumIndex observation for {symbol}");
        }
    }
    if observations.keys().cloned().collect::<BTreeSet<_>>() != *expected_symbols {
        bail!("premiumIndex response has incomplete active-contract coverage");
    }
    Ok(observations.into_values().collect())
}

pub fn open_interest_observation(
    raw: &Value,
    expected_symbol: &str,
    received_at_ns: u64,
) -> Result<OpenInterestObservation> {
    let symbol = required_string(raw, "symbol", "openInterest")?.to_ascii_uppercase();
    validate_symbol(&symbol)?;
    if symbol != expected_symbol.to_ascii_uppercase() {
        bail!("openInterest response symbol does not match its request");
    }
    let source_time_ms = required_u64(raw, "time", "openInterest")?;
    validate_source_clock_not_future(source_time_ms, received_at_ns)?;
    let open_interest = required_decimal(raw, "openInterest", "openInterest")?;
    if open_interest < Decimal::ZERO {
        bail!("open interest cannot be negative");
    }
    Ok(OpenInterestObservation {
        schema: REFERENCE_SCHEMA.to_owned(),
        symbol,
        open_interest,
        source_time_ms,
        received_at_ns,
        source_endpoint: OPEN_INTEREST_ENDPOINT.to_owned(),
    })
}

/// Parse the single interval row requested from Binance's basis endpoint.
/// Callers must bind the requested period themselves; the period is retained
/// in the typed observation so rows from different interval requests cannot be
/// silently conflated.
pub fn basis_observation(
    raw: &Value,
    expected_symbol: &str,
    period: &str,
    received_at_ns: u64,
) -> Result<BasisObservation> {
    let period = period.trim();
    if !BASIS_PERIODS.contains(&period) {
        bail!("basis interval is not a supported Binance period: {period}");
    }
    let rows = raw.as_array().context("basis response must be an array")?;
    let row = match rows.as_slice() {
        [row] => row,
        [] => bail!("basis response has no interval row"),
        _ => bail!("basis response has more than one interval row"),
    };
    let symbol = required_string(row, "pair", "basis")?.to_ascii_uppercase();
    let expected_symbol = expected_symbol.to_ascii_uppercase();
    if symbol != expected_symbol {
        bail!("basis response symbol does not match its request");
    }
    let contract_type = required_string(row, "contractType", "basis")?;
    if contract_type != "PERPETUAL" {
        bail!("basis response is not a USD-M perpetual row");
    }
    let source_time_ms = required_positive_u64(row, "timestamp", "basis")?;
    // The endpoint reports the start of the requested interval. It can be older
    // than the HTTP receipt by more than the live tick delay bound; preserve
    // that interval clock rather than relabeling it.
    if received_at_ns == 0 {
        bail!("basis response has a non-positive receive clock");
    }
    validate_source_clock_not_future(source_time_ms, received_at_ns)?;
    let index_price = required_decimal(row, "indexPrice", "basis")?;
    let futures_price = required_decimal(row, "futuresPrice", "basis")?;
    let basis = required_decimal(row, "basis", "basis")?;
    let basis_rate = required_decimal(row, "basisRate", "basis")?;
    let annualized_basis_rate = required_optional_decimal(row, "annualizedBasisRate", "basis")?;
    if index_price <= Decimal::ZERO || futures_price <= Decimal::ZERO {
        bail!("basis prices must be positive");
    }
    Ok(BasisObservation {
        schema: REFERENCE_SCHEMA.to_owned(),
        symbol: symbol.clone(),
        pair: symbol,
        contract_type: contract_type.to_owned(),
        period: period.to_owned(),
        index_price,
        futures_price,
        basis,
        basis_rate,
        annualized_basis_rate,
        source_time_ms,
        received_at_ns,
        source_endpoint: BASIS_ENDPOINT.to_owned(),
    })
}

/// Parse one Binance combined-stream `@forceOrder` frame while retaining every
/// clock and order-state field required by the reference contract.
pub fn force_order_observation(raw: &Value, received_at_ns: u64) -> Result<ForceOrderObservation> {
    let data = raw.get("data").unwrap_or(raw);
    if data.get("e").and_then(Value::as_str) != Some("forceOrder") {
        bail!("force order frame has the wrong event identity");
    }
    let order = data
        .get("o")
        .filter(|value| value.is_object())
        .context("force order has no order payload")?;
    let raw_symbol = required_string(order, "s", "force order")?;
    validate_symbol(raw_symbol)?;
    let symbol = raw_symbol.to_owned();
    let stream = raw
        .get("stream")
        .and_then(Value::as_str)
        .context("force order frame is missing its combined-stream identity")?;
    let expected_stream = format!("{}{}", symbol.to_ascii_lowercase(), FORCE_ORDER_ENDPOINT);
    if stream != expected_stream {
        bail!("force order frame has the wrong stream identity");
    }
    let side = required_string(order, "S", "force order")?;
    if !matches!(side, "BUY" | "SELL") {
        bail!("force order side is invalid");
    }
    let order_type = required_string(order, "o", "force order")?;
    if !matches!(
        order_type,
        "LIMIT"
            | "MARKET"
            | "STOP"
            | "STOP_MARKET"
            | "TAKE_PROFIT"
            | "TAKE_PROFIT_MARKET"
            | "TRAILING_STOP_MARKET"
    ) {
        bail!("force order type is invalid");
    }
    let time_in_force = required_string(order, "f", "force order")?;
    if !matches!(time_in_force, "GTC" | "IOC" | "FOK" | "GTX" | "GTD") {
        bail!("force order time-in-force is invalid");
    }
    let status = required_string(order, "X", "force order")?;
    if !matches!(
        status,
        "NEW"
            | "PARTIALLY_FILLED"
            | "FILLED"
            | "CANCELED"
            | "REJECTED"
            | "EXPIRED"
            | "EXPIRED_IN_MATCH"
    ) {
        bail!("force order status is invalid");
    }
    let original_quantity = required_decimal(order, "q", "force order")?;
    let price = required_decimal(order, "p", "force order")?;
    let average_price = required_decimal(order, "ap", "force order")?;
    let last_filled_quantity = required_decimal(order, "l", "force order")?;
    let cumulative_filled_quantity = required_decimal(order, "z", "force order")?;
    if original_quantity <= Decimal::ZERO
        || price <= Decimal::ZERO
        || average_price < Decimal::ZERO
        || last_filled_quantity < Decimal::ZERO
        || cumulative_filled_quantity < Decimal::ZERO
    {
        bail!("force order quantities and prices are invalid");
    }
    if last_filled_quantity > cumulative_filled_quantity
        || cumulative_filled_quantity > original_quantity
    {
        bail!("force order fill quantities are inconsistent");
    }
    if cumulative_filled_quantity > Decimal::ZERO && average_price <= Decimal::ZERO {
        bail!("force order filled quantity requires a positive average price");
    }
    match status {
        "NEW"
            if cumulative_filled_quantity != Decimal::ZERO
                || last_filled_quantity != Decimal::ZERO
                || average_price != Decimal::ZERO =>
        {
            bail!("NEW force order cannot report filled quantity")
        }
        "PARTIALLY_FILLED"
            if cumulative_filled_quantity <= Decimal::ZERO
                || cumulative_filled_quantity >= original_quantity
                || last_filled_quantity <= Decimal::ZERO =>
        {
            bail!("PARTIALLY_FILLED force order has inconsistent cumulative quantity")
        }
        "FILLED"
            if cumulative_filled_quantity != original_quantity
                || last_filled_quantity <= Decimal::ZERO
                || average_price <= Decimal::ZERO =>
        {
            bail!("FILLED force order has inconsistent fill quantity")
        }
        _ => {}
    }
    let order_time_ms = required_positive_u64(order, "T", "force order")?;
    let event_time_ms = required_positive_u64(data, "E", "force order")?;
    if received_at_ns == 0 {
        bail!("force order frame has a non-positive receive clock");
    }
    if order_time_ms > event_time_ms {
        bail!("force order source clocks are reversed");
    }
    validate_receive_clock(event_time_ms, received_at_ns)?;
    validate_receive_clock(order_time_ms, received_at_ns)?;
    Ok(ForceOrderObservation {
        schema: REFERENCE_SCHEMA.to_owned(),
        symbol,
        side: side.to_owned(),
        order_type: order_type.to_owned(),
        time_in_force: time_in_force.to_owned(),
        original_quantity,
        price,
        average_price,
        status: status.to_owned(),
        last_filled_quantity,
        cumulative_filled_quantity,
        order_time_ms,
        event_time_ms,
        received_at_ns,
        source_endpoint: FORCE_ORDER_ENDPOINT.to_owned(),
        coverage: FORCE_ORDER_COVERAGE.to_owned(),
    })
}

impl ForceOrderObservation {
    /// Stable content identity for deduplication. The local receive clock is
    /// intentionally excluded so replaying the same venue event is idempotent.
    pub fn content_identity(&self) -> String {
        format!(
            "schema={:?};symbol={:?};side={:?};order_type={:?};time_in_force={:?};original_quantity={};price={};average_price={};status={:?};last_filled_quantity={};cumulative_filled_quantity={};order_time_ms={};event_time_ms={};source_endpoint={:?};coverage={:?}",
            self.schema,
            self.symbol,
            self.side,
            self.order_type,
            self.time_in_force,
            self.original_quantity.normalize(),
            self.price.normalize(),
            self.average_price.normalize(),
            self.status,
            self.last_filled_quantity.normalize(),
            self.cumulative_filled_quantity.normalize(),
            self.order_time_ms,
            self.event_time_ms,
            self.source_endpoint,
            self.coverage,
        )
    }
}

fn unique_symbols<'a>(
    symbols: impl Iterator<Item = &'a str>,
    kind: &str,
) -> Result<BTreeSet<String>> {
    let mut unique = BTreeSet::new();
    for symbol in symbols {
        if !unique.insert(symbol.to_owned()) {
            bail!("reference batch has duplicate {kind} symbol {symbol}");
        }
    }
    Ok(unique)
}

fn stale_count(
    source_times: impl Iterator<Item = u64>,
    observed_at_ns: u64,
    max_staleness_ms: u64,
) -> Result<u64> {
    let observed_at_ms = observed_at_ns / 1_000_000;
    let mut stale = 0;
    for source_time_ms in source_times {
        if source_time_ms > observed_at_ms.saturating_add(MAX_SOURCE_LEAD_MS) {
            bail!("reference source clock leads coverage clock");
        }
        if observed_at_ms.saturating_sub(source_time_ms) > max_staleness_ms {
            stale += 1;
        }
    }
    Ok(stale)
}

fn required_positive_u64(raw: &Value, field: &str, endpoint: &str) -> Result<u64> {
    let value = required_u64(raw, field, endpoint)?;
    if value == 0 {
        bail!("{endpoint} response has a non-positive {field}");
    }
    Ok(value)
}

fn required_optional_decimal(raw: &Value, field: &str, endpoint: &str) -> Result<Option<Decimal>> {
    let value = raw
        .get(field)
        .with_context(|| format!("{endpoint} response is missing {field}"))?;
    let text = value
        .as_str()
        .with_context(|| format!("{endpoint} response has invalid {field}"))?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    Decimal::from_str(text.trim())
        .map(Some)
        .with_context(|| format!("{endpoint} response has invalid decimal {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::str::FromStr;

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const SOURCE_CLOCK_RECEIVED_NS: u64 = 1_700_000_000_400_000_000;
    const RECEIVED_NS: u64 = 1_700_000_000_500_000_000;

    fn exchange_info() -> serde_json::Value {
        json!({
            "symbols": [
                {
                    "symbol": "BTCUSDT", "pair": "BTCUSDT", "contractType": "PERPETUAL",
                    "deliveryDate": 4133404800000_u64, "onboardDate": 1598252400000_u64,
                    "status": "TRADING", "baseAsset": "BTC", "quoteAsset": "USDT",
                    "marginAsset": "USDT",
                    "filters": [
                        {"filterType":"PRICE_FILTER", "tickSize":"0.10"},
                        {"filterType":"LOT_SIZE", "stepSize":"0.001"},
                        {"filterType":"MIN_NOTIONAL", "notional":"5"}
                    ]
                },
                {
                    "symbol": "ETHUSDT_250926", "pair": "ETHUSDT",
                    "contractType": "CURRENT_QUARTER", "deliveryDate": 1758873600000_u64,
                    "onboardDate": 1700000000000_u64, "status": "TRADING",
                    "baseAsset": "ETH", "quoteAsset": "USDT", "marginAsset": "USDT"
                }
            ]
        })
    }

    fn premium_index() -> serde_json::Value {
        json!([
            {
                "symbol": "BTCUSDT", "markPrice": "101.0", "indexPrice": "100.0",
                "lastFundingRate": "0.0001", "interestRate": "0.0001",
                "nextFundingTime": SOURCE_MS + 28_800_000, "time": SOURCE_MS
            },
            {
                "symbol": "ETHUSDT_250926", "markPrice": "2000", "indexPrice": "1999",
                "lastFundingRate": "0", "interestRate": "0",
                "nextFundingTime": 0, "time": SOURCE_MS
            }
        ])
    }

    fn open_interest() -> serde_json::Value {
        json!({"symbol":"BTCUSDT","openInterest":"10659.509","time":SOURCE_MS + 100})
    }

    #[test]
    fn builds_complete_reference_batch_with_exact_basis_and_endpoint_identity() {
        let contracts = active_perpetual_contracts(
            &exchange_info(),
            SOURCE_MS,
            SOURCE_CLOCK_RECEIVED_NS,
            RECEIVED_NS,
        )
        .unwrap();
        let expected = contracts
            .iter()
            .map(|contract| contract.symbol.clone())
            .collect::<BTreeSet<_>>();
        let marks =
            mark_index_funding_observations(&premium_index(), &expected, RECEIVED_NS).unwrap();
        let oi =
            vec![
                open_interest_observation(&open_interest(), "BTCUSDT", RECEIVED_NS + 100_000_000)
                    .unwrap(),
            ];
        let batch = CompleteReferenceBatch::new(contracts, marks, oi).unwrap();

        assert_eq!(batch.contracts().len(), 1);
        assert_eq!(batch.contracts()[0].source_endpoint, EXCHANGE_INFO_ENDPOINT);
        assert_eq!(
            batch.contracts()[0].source_clock_endpoint,
            SERVER_TIME_ENDPOINT
        );
        assert_eq!(
            batch.contracts()[0].source_clock_received_at_ns,
            SOURCE_CLOCK_RECEIVED_NS
        );
        assert_eq!(batch.contracts()[0].schema, "binance.usdm_reference.v3");
        assert_eq!(batch.contracts()[0].tick_size, Decimal::new(1, 1));
        assert_eq!(batch.contracts()[0].step_size, Decimal::new(1, 3));
        assert_eq!(batch.contracts()[0].min_notional, Decimal::new(5, 0));
        let mut legacy_v1 = serde_json::to_value(&batch.contracts()[0]).unwrap();
        legacy_v1["schema"] = json!("binance.usdm_reference.v1");
        legacy_v1
            .as_object_mut()
            .unwrap()
            .remove("source_clock_received_at_ns");
        assert!(serde_json::from_value::<ActivePerpetualContract>(legacy_v1).is_err());
        let mark = &batch.mark_index_funding()[0];
        assert_eq!(mark.source_endpoint, PREMIUM_INDEX_ENDPOINT);
        assert_eq!(mark.basis, Decimal::ONE);
        assert_eq!(mark.basis_rate, Decimal::from_str("0.01").unwrap());
        assert_eq!(mark.next_funding_time_ms, SOURCE_MS + 28_800_000);
        assert_eq!(
            batch.open_interest()[0].source_endpoint,
            OPEN_INTEREST_ENDPOINT
        );

        let coverage = batch.coverage(RECEIVED_NS + 200_000_000, 1_000).unwrap();
        assert_eq!(coverage.active_contracts, 1);
        assert_eq!(coverage.metadata_observations, 1);
        assert_eq!(coverage.mark_index_funding_observations, 1);
        assert_eq!(coverage.open_interest_observations, 1);
        assert_eq!(coverage.stale_metadata, 0);
        assert_eq!(coverage.stale_mark_index_funding, 0);
        assert_eq!(coverage.stale_open_interest, 0);

        let strict_coverage = batch.coverage(RECEIVED_NS + 200_000_000, 600).unwrap();
        assert_eq!(strict_coverage.stale_metadata, 1);
        assert_eq!(strict_coverage.stale_mark_index_funding, 1);
        assert_eq!(strict_coverage.stale_open_interest, 0);
    }

    #[test]
    fn complete_batch_rejects_missing_contract_observations() {
        let contracts = active_perpetual_contracts(
            &exchange_info(),
            SOURCE_MS,
            SOURCE_CLOCK_RECEIVED_NS,
            RECEIVED_NS,
        )
        .unwrap();
        let expected = BTreeSet::from(["BTCUSDT".to_owned()]);
        let marks =
            mark_index_funding_observations(&premium_index(), &expected, RECEIVED_NS).unwrap();
        let error = CompleteReferenceBatch::new(contracts, marks, Vec::new()).unwrap_err();
        assert!(error.to_string().contains("open-interest coverage"));
    }

    #[test]
    fn complete_batch_rejects_tampered_source_identity_and_derived_basis() {
        let contracts = active_perpetual_contracts(
            &exchange_info(),
            SOURCE_MS,
            SOURCE_CLOCK_RECEIVED_NS,
            RECEIVED_NS,
        )
        .unwrap();
        let expected = BTreeSet::from(["BTCUSDT".to_owned()]);
        let marks =
            mark_index_funding_observations(&premium_index(), &expected, RECEIVED_NS).unwrap();
        let oi =
            vec![
                open_interest_observation(&open_interest(), "BTCUSDT", RECEIVED_NS + 100_000_000)
                    .unwrap(),
            ];

        let mut wrong_endpoint = marks.clone();
        wrong_endpoint[0].source_endpoint = OPEN_INTEREST_ENDPOINT.to_owned();
        assert!(
            CompleteReferenceBatch::new(contracts.clone(), wrong_endpoint, oi.clone())
                .unwrap_err()
                .to_string()
                .contains("source identity")
        );

        let mut wrong_basis = marks;
        wrong_basis[0].basis = Decimal::ZERO;
        assert!(CompleteReferenceBatch::new(contracts, wrong_basis, oi)
            .unwrap_err()
            .to_string()
            .contains("derived basis"));
    }

    #[test]
    fn malformed_prices_and_future_source_clocks_fail_closed() {
        let expected = BTreeSet::from(["BTCUSDT".to_owned()]);
        let mut missing_tick = exchange_info();
        missing_tick["symbols"][0]["filters"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        assert!(active_perpetual_contracts(
            &missing_tick,
            SOURCE_MS,
            SOURCE_CLOCK_RECEIVED_NS,
            RECEIVED_NS,
        )
        .unwrap_err()
        .to_string()
        .contains("missing PRICE_FILTER"));

        let mut zero_index = premium_index();
        zero_index[0]["indexPrice"] = json!("0");
        assert!(
            mark_index_funding_observations(&zero_index, &expected, RECEIVED_NS)
                .unwrap_err()
                .to_string()
                .contains("index price must be positive")
        );

        let future_received = (SOURCE_MS - MAX_SOURCE_LEAD_MS - 1) * 1_000_000;
        assert!(
            open_interest_observation(&open_interest(), "BTCUSDT", future_received)
                .unwrap_err()
                .to_string()
                .contains("source clock leads received clock")
        );

        assert!(active_perpetual_contracts(
            &exchange_info(),
            SOURCE_MS,
            RECEIVED_NS + 1,
            RECEIVED_NS,
        )
        .unwrap_err()
        .to_string()
        .contains("precedes its source-clock receipt"));
    }

    #[test]
    fn open_interest_last_change_timestamps_are_evidence_not_a_liveness_clock() {
        // The exchange openInterest `time` is a per-instrument last-change
        // timestamp: a quiet instrument may legitimately lag minutes behind
        // receipt without making the batch fail closed.
        let mut stale_oi = open_interest();
        stale_oi["time"] = json!(SOURCE_MS - 3_600_000);
        let row = open_interest_observation(&stale_oi, "BTCUSDT", RECEIVED_NS).unwrap();
        assert_eq!(row.source_time_ms, SOURCE_MS - 3_600_000);

        let mut future_oi = open_interest();
        future_oi["time"] = json!(SOURCE_MS + MAX_SOURCE_LEAD_MS + 60_000);
        assert!(
            open_interest_observation(&future_oi, "BTCUSDT", RECEIVED_NS)
                .unwrap_err()
                .to_string()
                .contains("source clock leads received clock")
        );

        let contracts = active_perpetual_contracts(
            &exchange_info(),
            SOURCE_MS,
            SOURCE_CLOCK_RECEIVED_NS,
            RECEIVED_NS,
        )
        .unwrap();
        let expected = BTreeSet::from(["BTCUSDT".to_owned()]);
        let marks =
            mark_index_funding_observations(&premium_index(), &expected, RECEIVED_NS).unwrap();
        let batch = CompleteReferenceBatch::new(contracts, marks, vec![row]).unwrap();
        let coverage = batch.coverage(RECEIVED_NS + 200_000_000, 1_000).unwrap();
        assert_eq!(coverage.open_interest_observations, 1);
        assert_eq!(coverage.stale_open_interest, 1);
        assert_eq!(coverage.stale_metadata, 0);
        assert_eq!(coverage.stale_mark_index_funding, 0);
    }

    #[test]
    fn duplicate_active_contract_and_clock_regression_fail_closed() {
        let mut duplicate = exchange_info();
        let first_symbol = duplicate["symbols"][0].clone();
        duplicate["symbols"]
            .as_array_mut()
            .unwrap()
            .push(first_symbol);
        assert!(active_perpetual_contracts(
            &duplicate,
            SOURCE_MS,
            SOURCE_CLOCK_RECEIVED_NS,
            RECEIVED_NS,
        )
        .unwrap_err()
        .to_string()
        .contains("duplicate active perpetual contract"));

        let mut clocks = ReferenceClockValidator::default();
        clocks
            .observe(
                ReferenceMarket::Usdm,
                ReferenceKind::OpenInterest,
                "BTCUSDT",
                SOURCE_MS + 1,
                RECEIVED_NS,
            )
            .unwrap();
        assert!(clocks
            .observe(
                ReferenceMarket::Usdm,
                ReferenceKind::OpenInterest,
                "BTCUSDT",
                SOURCE_MS,
                RECEIVED_NS + 1,
            )
            .unwrap_err()
            .to_string()
            .contains("source time regressed"));
    }

    #[test]
    fn reference_clock_identity_keeps_spot_and_usdm_symbols_separate() {
        let mut clocks = ReferenceClockValidator::default();
        clocks
            .observe(
                ReferenceMarket::Spot,
                ReferenceKind::Metadata,
                "BTCUSDT",
                SOURCE_MS + 10,
                RECEIVED_NS,
            )
            .unwrap();
        clocks
            .observe(
                ReferenceMarket::Usdm,
                ReferenceKind::Metadata,
                "BTCUSDT",
                SOURCE_MS,
                RECEIVED_NS,
            )
            .unwrap();
    }

    #[test]
    fn symbol_identity_accepts_cjk_contract_names_and_rejects_malformed() {
        for valid in [
            "BTCUSDT",
            "ETHUSDT_250926",
            "币安人生USDT",
            "我踏马来了USDT",
            "龙虾USDT",
        ] {
            assert!(validate_symbol(valid).is_ok(), "{valid} must be accepted");
        }
        for invalid in [
            "",
            "btcusdt",
            "BTC/USDT",
            "BTC USDT",
            "BTC-USDT",
            "BTC.USDT",
            "BTCUSDT\u{200B}",
        ] {
            assert!(
                validate_symbol(invalid).is_err(),
                "{invalid:?} must be rejected"
            );
        }
        assert!(validate_symbol(&"A".repeat(33)).is_err());
        assert!(validate_symbol(&"币".repeat(33)).is_err());
        assert!(validate_symbol(&"币".repeat(32)).is_ok());
    }

    #[test]
    fn basis_observation_preserves_interval_and_exchange_clock() {
        let row = json!([{
            "pair": "BTCUSDT",
            "contractType": "PERPETUAL",
            "indexPrice": "100.0",
            "futuresPrice": "101.0",
            "basis": "1.0",
            "basisRate": "0.01",
            "annualizedBasisRate": "0.1",
            "timestamp": SOURCE_MS
        }]);
        let observation = basis_observation(&row, "BTCUSDT", "5m", RECEIVED_NS).unwrap();
        assert_eq!(observation.period, "5m");
        assert_eq!(observation.source_time_ms, SOURCE_MS);
        assert_eq!(observation.received_at_ns, RECEIVED_NS);
        assert_eq!(observation.basis, Decimal::ONE);
        assert_eq!(observation.annualized_basis_rate, Some(Decimal::new(1, 1)));
    }

    #[test]
    fn official_basis_perpetual_empty_annualized_rate_is_not_applicable() {
        let row = json!([{
            "pair": "BTCUSDT",
            "contractType": "PERPETUAL",
            "indexPrice": "100.0",
            "futuresPrice": "101.0",
            "basis": "1.0",
            "basisRate": "0.01",
            "annualizedBasisRate": "",
            "timestamp": SOURCE_MS
        }]);
        let observation = basis_observation(&row, "BTCUSDT", "5m", RECEIVED_NS).unwrap();
        assert_eq!(observation.annualized_basis_rate, None);

        let mut missing = row.clone();
        missing[0]
            .as_object_mut()
            .unwrap()
            .remove("annualizedBasisRate");
        let error = basis_observation(&missing, "BTCUSDT", "5m", RECEIVED_NS).unwrap_err();
        assert!(error.to_string().contains("missing annualizedBasisRate"));

        let mut malformed = row;
        malformed[0]["annualizedBasisRate"] = json!("not-a-rate");
        let error = basis_observation(&malformed, "BTCUSDT", "5m", RECEIVED_NS).unwrap_err();
        assert!(error
            .to_string()
            .contains("invalid decimal annualizedBasisRate"));
    }

    #[test]
    fn basis_period_and_clocks_are_positive_and_enum_bound() {
        let row = json!([{
            "pair": "BTCUSDT",
            "contractType": "PERPETUAL",
            "indexPrice": "100.0",
            "futuresPrice": "101.0",
            "basis": "1.0",
            "basisRate": "0.01",
            "annualizedBasisRate": "",
            "timestamp": SOURCE_MS
        }]);
        assert!(basis_observation(&row, "BTCUSDT", "2m", RECEIVED_NS)
            .unwrap_err()
            .to_string()
            .contains("supported Binance period"));

        let mut zero_timestamp = row.clone();
        zero_timestamp[0]["timestamp"] = json!(0);
        assert!(
            basis_observation(&zero_timestamp, "BTCUSDT", "5m", RECEIVED_NS)
                .unwrap_err()
                .to_string()
                .contains("non-positive timestamp")
        );
        assert!(basis_observation(&row, "BTCUSDT", "5m", 0)
            .unwrap_err()
            .to_string()
            .contains("non-positive receive clock"));
    }

    #[test]
    fn force_order_observation_keeps_order_state_and_rejects_missing_fields() {
        let frame = json!({
            "stream": "btcusdt@forceOrder",
            "data": {
                "e": "forceOrder",
                "E": SOURCE_MS,
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
                    "T": SOURCE_MS
                }
            }
        });
        let observation = force_order_observation(&frame, RECEIVED_NS).unwrap();
        assert_eq!(
            observation.original_quantity,
            Decimal::from_str("0.014").unwrap()
        );
        assert_eq!(
            observation.last_filled_quantity,
            observation.original_quantity
        );
        assert_eq!(observation.status, "FILLED");
        assert_eq!(observation.order_time_ms, SOURCE_MS);
        assert_eq!(observation.event_time_ms, SOURCE_MS);
        assert_eq!(observation.coverage, FORCE_ORDER_COVERAGE);

        let mut missing = frame;
        missing["data"]["o"].as_object_mut().unwrap().remove("ap");
        assert!(force_order_observation(&missing, RECEIVED_NS)
            .unwrap_err()
            .to_string()
            .contains("invalid ap"));
    }

    #[test]
    fn force_order_identity_includes_side_and_price_but_excludes_receive_clock() {
        let frame = json!({
            "stream": "btcusdt@forceOrder",
            "data": {
                "e": "forceOrder", "E": SOURCE_MS,
                "o": {
                    "s": "BTCUSDT", "S": "SELL", "o": "LIMIT", "f": "IOC",
                    "q": "0.014", "p": "9910", "ap": "9910", "X": "FILLED",
                    "l": "0.014", "z": "0.014", "T": SOURCE_MS
                }
            }
        });
        let first = force_order_observation(&frame, RECEIVED_NS).unwrap();
        let replay = force_order_observation(&frame, RECEIVED_NS + 1_000_000).unwrap();
        assert_eq!(first.content_identity(), replay.content_identity());

        let mut opposite_side = frame.clone();
        opposite_side["data"]["o"]["S"] = json!("BUY");
        opposite_side["stream"] = json!("btcusdt@forceOrder");
        // A BUY frame is still canonical only when the payload itself carries
        // the changed side; it must receive a distinct content identity.
        let opposite = force_order_observation(&opposite_side, RECEIVED_NS).unwrap();
        assert_ne!(first.content_identity(), opposite.content_identity());

        let mut opposite_price = frame;
        opposite_price["data"]["o"]["p"] = json!("9911");
        opposite_price["data"]["o"]["ap"] = json!("9911");
        let price = force_order_observation(&opposite_price, RECEIVED_NS).unwrap();
        assert_ne!(first.content_identity(), price.content_identity());
    }

    #[test]
    fn force_order_rejects_noncanonical_stream_and_unknown_state() {
        let frame = json!({
            "stream": "btcusdt@forceOrder",
            "data": {
                "e": "forceOrder", "E": SOURCE_MS,
                "o": {
                    "s": "BTCUSDT", "S": "SELL", "o": "LIMIT", "f": "IOC",
                    "q": "0.014", "p": "9910", "ap": "9910", "X": "FILLED",
                    "l": "0.014", "z": "0.014", "T": SOURCE_MS
                }
            }
        });
        let mut extra_stream = frame.clone();
        extra_stream["stream"] = json!("btcusdt@forceOrder@extra");
        assert!(force_order_observation(&extra_stream, RECEIVED_NS).is_err());

        let mut lowercase_symbol = frame.clone();
        lowercase_symbol["data"]["o"]["s"] = json!("btcusdt");
        assert!(force_order_observation(&lowercase_symbol, RECEIVED_NS).is_err());

        let mut unknown_status = frame;
        unknown_status["data"]["o"]["X"] = json!("DONE");
        assert!(force_order_observation(&unknown_status, RECEIVED_NS)
            .unwrap_err()
            .to_string()
            .contains("status is invalid"));

        let mut inconsistent_fill = json!({
            "stream": "btcusdt@forceOrder",
            "data": {
                "e": "forceOrder", "E": SOURCE_MS,
                "o": {
                    "s": "BTCUSDT", "S": "SELL", "o": "LIMIT", "f": "IOC",
                    "q": "0.014", "p": "9910", "ap": "9910", "X": "FILLED",
                    "l": "0.015", "z": "0.014", "T": SOURCE_MS
                }
            }
        });
        assert!(force_order_observation(&inconsistent_fill, RECEIVED_NS)
            .unwrap_err()
            .to_string()
            .contains("fill quantities are inconsistent"));

        inconsistent_fill["data"]["o"]["l"] = json!("0.013");
        inconsistent_fill["data"]["o"]["z"] = json!("0.013");
        assert!(force_order_observation(&inconsistent_fill, RECEIVED_NS)
            .unwrap_err()
            .to_string()
            .contains("FILLED force order"));
    }
}
