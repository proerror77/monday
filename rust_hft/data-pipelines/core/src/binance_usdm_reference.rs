//! Fail-closed normalization for official Binance USD-M reference observations.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::binance_market_tape::{MAX_SOURCE_DELAY_MS, MAX_SOURCE_LEAD_MS};

pub const REFERENCE_SCHEMA: &str = "binance.usdm_reference.v1";
pub const EXCHANGE_INFO_ENDPOINT: &str = "/fapi/v1/exchangeInfo";
pub const SERVER_TIME_ENDPOINT: &str = "/fapi/v1/time";
pub const PREMIUM_INDEX_ENDPOINT: &str = "/fapi/v1/premiumIndex";
pub const OPEN_INTEREST_ENDPOINT: &str = "/fapi/v1/openInterest";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivePerpetualContract {
    pub schema: String,
    pub symbol: String,
    pub pair: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub margin_asset: String,
    pub contract_type: String,
    pub status: String,
    pub onboard_date_ms: u64,
    pub delivery_date_ms: u64,
    pub source_time_ms: u64,
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
    validate_receive_clock(row.source_time_ms, row.received_at_ns)?;
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
    validate_receive_clock(row.source_time_ms, row.received_at_ns)?;
    if row.schema != REFERENCE_SCHEMA || row.source_endpoint != OPEN_INTEREST_ENDPOINT {
        bail!("open-interest source identity is invalid");
    }
    if row.open_interest < Decimal::ZERO {
        bail!("open interest cannot be negative");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReferenceKind {
    Metadata,
    MarkIndexFunding,
    OpenInterest,
}

#[derive(Debug, Default)]
pub struct ReferenceClockValidator {
    clocks: BTreeMap<(ReferenceKind, String), (u64, u64)>,
}

impl ReferenceClockValidator {
    pub fn observe(
        &mut self,
        kind: ReferenceKind,
        symbol: &str,
        source_time_ms: u64,
        received_at_ns: u64,
    ) -> Result<()> {
        validate_receive_clock(source_time_ms, received_at_ns)?;
        let key = (kind, symbol.to_owned());
        if self
            .clocks
            .get(&key)
            .is_some_and(|(last_source, last_received)| {
                source_time_ms < *last_source || received_at_ns < *last_received
            })
        {
            bail!("USD-M reference source time regressed");
        }
        self.clocks.insert(key, (source_time_ms, received_at_ns));
        Ok(())
    }
}

pub fn active_perpetual_contracts(
    exchange_info: &Value,
    source_time_ms: u64,
    received_at_ns: u64,
) -> Result<Vec<ActivePerpetualContract>> {
    validate_receive_clock(source_time_ms, received_at_ns)?;
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
            contract_type: contract_type.to_owned(),
            status: status.to_owned(),
            onboard_date_ms: required_u64(raw, "onboardDate", "exchangeInfo")?,
            delivery_date_ms: required_u64(raw, "deliveryDate", "exchangeInfo")?,
            source_time_ms,
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
    validate_receive_clock(source_time_ms, received_at_ns)?;
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

fn validate_receive_clock(source_time_ms: u64, received_at_ns: u64) -> Result<()> {
    let received_at_ms = received_at_ns / 1_000_000;
    if source_time_ms > received_at_ms.saturating_add(MAX_SOURCE_LEAD_MS) {
        bail!("USD-M reference source clock leads received clock");
    }
    if received_at_ms > source_time_ms.saturating_add(MAX_SOURCE_DELAY_MS) {
        bail!("USD-M reference source clock is stale at receipt");
    }
    Ok(())
}

fn validate_symbol(symbol: &str) -> Result<()> {
    if symbol.is_empty()
        || symbol.len() > 32
        || !symbol
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        bail!("invalid USD-M symbol identity");
    }
    Ok(())
}

fn required_string<'a>(raw: &'a Value, field: &str, endpoint: &str) -> Result<&'a str> {
    raw.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{endpoint} response has invalid {field}"))
}

fn required_u64(raw: &Value, field: &str, endpoint: &str) -> Result<u64> {
    raw.get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("{endpoint} response has invalid {field}"))
}

fn required_decimal(raw: &Value, field: &str, endpoint: &str) -> Result<Decimal> {
    Decimal::from_str(required_string(raw, field, endpoint)?)
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
    const RECEIVED_NS: u64 = 1_700_000_000_500_000_000;

    fn exchange_info() -> serde_json::Value {
        json!({
            "symbols": [
                {
                    "symbol": "BTCUSDT", "pair": "BTCUSDT", "contractType": "PERPETUAL",
                    "deliveryDate": 4133404800000_u64, "onboardDate": 1598252400000_u64,
                    "status": "TRADING", "baseAsset": "BTC", "quoteAsset": "USDT",
                    "marginAsset": "USDT"
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
        let contracts =
            active_perpetual_contracts(&exchange_info(), SOURCE_MS, RECEIVED_NS).unwrap();
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
        let contracts =
            active_perpetual_contracts(&exchange_info(), SOURCE_MS, RECEIVED_NS).unwrap();
        let expected = BTreeSet::from(["BTCUSDT".to_owned()]);
        let marks =
            mark_index_funding_observations(&premium_index(), &expected, RECEIVED_NS).unwrap();
        let error = CompleteReferenceBatch::new(contracts, marks, Vec::new()).unwrap_err();
        assert!(error.to_string().contains("open-interest coverage"));
    }

    #[test]
    fn complete_batch_rejects_tampered_source_identity_and_derived_basis() {
        let contracts =
            active_perpetual_contracts(&exchange_info(), SOURCE_MS, RECEIVED_NS).unwrap();
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
    }

    #[test]
    fn duplicate_active_contract_and_clock_regression_fail_closed() {
        let mut duplicate = exchange_info();
        let first_symbol = duplicate["symbols"][0].clone();
        duplicate["symbols"]
            .as_array_mut()
            .unwrap()
            .push(first_symbol);
        assert!(
            active_perpetual_contracts(&duplicate, SOURCE_MS, RECEIVED_NS)
                .unwrap_err()
                .to_string()
                .contains("duplicate active perpetual contract")
        );

        let mut clocks = ReferenceClockValidator::default();
        clocks
            .observe(
                ReferenceKind::OpenInterest,
                "BTCUSDT",
                SOURCE_MS + 1,
                RECEIVED_NS,
            )
            .unwrap();
        assert!(clocks
            .observe(
                ReferenceKind::OpenInterest,
                "BTCUSDT",
                SOURCE_MS,
                RECEIVED_NS + 1,
            )
            .unwrap_err()
            .to_string()
            .contains("source time regressed"));
    }
}
