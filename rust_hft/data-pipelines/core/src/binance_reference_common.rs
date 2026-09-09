//! Shared fail-closed parsing and receive-clock checks for Binance reference data.

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::binance_market_tape::{MAX_SOURCE_DELAY_MS, MAX_SOURCE_LEAD_MS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReferenceKind {
    Metadata,
    MarkIndexFunding,
    OpenInterest,
}

/// Market discriminator for reference clocks. Metadata for the same symbol is
/// independent across Spot and USD-M and must never share a monotonicity key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReferenceMarket {
    Spot,
    Usdm,
}

#[derive(Debug, Default)]
pub struct ReferenceClockValidator {
    clocks: BTreeMap<(ReferenceMarket, ReferenceKind, String), (u64, u64)>,
}

impl ReferenceClockValidator {
    pub fn observe(
        &mut self,
        market: ReferenceMarket,
        kind: ReferenceKind,
        symbol: &str,
        source_time_ms: u64,
        received_at_ns: u64,
    ) -> Result<()> {
        match kind {
            ReferenceKind::OpenInterest => {
                validate_source_clock_not_future(source_time_ms, received_at_ns)?
            }
            ReferenceKind::Metadata | ReferenceKind::MarkIndexFunding => {
                validate_receive_clock(source_time_ms, received_at_ns)?
            }
        }
        let key = (market, kind, symbol.to_owned());
        if self
            .clocks
            .get(&key)
            .is_some_and(|(last_source, last_received)| {
                source_time_ms < *last_source || received_at_ns < *last_received
            })
        {
            bail!("Binance reference source time regressed");
        }
        self.clocks.insert(key, (source_time_ms, received_at_ns));
        Ok(())
    }
}

pub fn validate_receive_clock(source_time_ms: u64, received_at_ns: u64) -> Result<()> {
    let received_at_ms = received_at_ns / 1_000_000;
    if source_time_ms > received_at_ms.saturating_add(MAX_SOURCE_LEAD_MS) {
        bail!("Binance reference source clock leads received clock");
    }
    if received_at_ms > source_time_ms.saturating_add(MAX_SOURCE_DELAY_MS) {
        bail!("Binance reference source clock is stale at receipt");
    }
    Ok(())
}

// Open-interest `time` is an instrument's last-change timestamp rather than a
// per-request clock.  Quiet instruments may legitimately lag; only a future
// source timestamp is rejected here.  Consumers can report observed lag as
// coverage evidence without treating it as a missing observation.
pub fn validate_source_clock_not_future(source_time_ms: u64, received_at_ns: u64) -> Result<()> {
    let received_at_ms = received_at_ns / 1_000_000;
    if source_time_ms > received_at_ms.saturating_add(MAX_SOURCE_LEAD_MS) {
        bail!("Binance reference source clock leads received clock");
    }
    Ok(())
}

pub fn validate_symbol(symbol: &str) -> Result<()> {
    if symbol.is_empty()
        || symbol.chars().count() > 32
        || !symbol.chars().all(|ch| {
            ch.is_ascii_uppercase()
                || ch.is_ascii_digit()
                || ch == '_'
                || is_cjk_unified_ideograph(ch)
        })
    {
        bail!("invalid Binance symbol identity");
    }
    Ok(())
}

fn is_cjk_unified_ideograph(ch: char) -> bool {
    ('\u{3400}'..='\u{4DBF}').contains(&ch) || ('\u{4E00}'..='\u{9FFF}').contains(&ch)
}

pub fn required_string<'a>(raw: &'a Value, field: &str, endpoint: &str) -> Result<&'a str> {
    raw.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{endpoint} response has invalid {field}"))
}

pub fn required_u64(raw: &Value, field: &str, endpoint: &str) -> Result<u64> {
    raw.get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("{endpoint} response has invalid {field}"))
}

pub fn required_decimal(raw: &Value, field: &str, endpoint: &str) -> Result<Decimal> {
    Decimal::from_str(required_string(raw, field, endpoint)?)
        .with_context(|| format!("{endpoint} response has invalid decimal {field}"))
}

pub fn required_filter_decimal(raw: &Value, filter_type: &str, field: &str) -> Result<Decimal> {
    required_filter_decimal_with_zero_policy(raw, filter_type, field, false)
}

/// Parse a symbol filter field whose zero value disables that component of
/// the rule, as with Binance Spot PRICE_FILTER and MARKET_LOT_SIZE fields.
pub fn required_filter_decimal_allow_zero(
    raw: &Value,
    filter_type: &str,
    field: &str,
) -> Result<Decimal> {
    required_filter_decimal_with_zero_policy(raw, filter_type, field, true)
}

fn required_filter_decimal_with_zero_policy(
    raw: &Value,
    filter_type: &str,
    field: &str,
    allow_zero: bool,
) -> Result<Decimal> {
    let filters = raw
        .get("filters")
        .and_then(Value::as_array)
        .context("exchangeInfo filters must be an array")?;
    let mut matches = filters
        .iter()
        .filter(|filter| filter.get("filterType").and_then(Value::as_str) == Some(filter_type));
    let filter = matches
        .next()
        .with_context(|| format!("exchangeInfo is missing {filter_type}"))?;
    if matches.next().is_some() {
        bail!("exchangeInfo has duplicate {filter_type}");
    }
    let value = required_decimal(filter, field, "exchangeInfo")?;
    if (allow_zero && value < Decimal::ZERO) || (!allow_zero && value <= Decimal::ZERO) {
        let requirement = if allow_zero {
            "non-negative"
        } else {
            "positive"
        };
        bail!("exchangeInfo {filter_type} {field} must be {requirement}");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_clock_monotonicity_is_scoped_by_market() {
        let mut clocks = ReferenceClockValidator::default();
        clocks
            .observe(
                ReferenceMarket::Spot,
                ReferenceKind::Metadata,
                "BTCUSDT",
                1_700_000_000_000,
                1_700_000_000_100_000_000,
            )
            .unwrap();
        // A separate USD-M stream may begin at an earlier source clock for
        // the same symbol without regressing the Spot stream.
        clocks
            .observe(
                ReferenceMarket::Usdm,
                ReferenceKind::Metadata,
                "BTCUSDT",
                1_699_999_999_999,
                1_700_000_000_100_000_000,
            )
            .unwrap();
        assert!(clocks
            .observe(
                ReferenceMarket::Spot,
                ReferenceKind::Metadata,
                "BTCUSDT",
                1_699_999_999_999,
                1_700_000_000_101_000_000,
            )
            .is_err());
    }
}
