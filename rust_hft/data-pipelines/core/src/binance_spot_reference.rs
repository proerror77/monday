//! Fail-closed normalization for public Binance Spot instrument rules.
//!
//! This profile deliberately contains only exchangeInfo rule and clock
//! evidence.  Funding, open interest, basis, and account commission data
//! belong to other products or private evidence surfaces and are not inferred
//! here.

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::binance_reference_common::{
    required_decimal, required_filter_decimal, required_filter_decimal_allow_zero, required_string,
    required_u64, validate_receive_clock, ReferenceClockValidator, ReferenceKind, ReferenceMarket,
};

pub const REFERENCE_SCHEMA: &str = "binance.spot_reference.v1";
pub const EXCHANGE_INFO_ENDPOINT: &str = "/api/v3/exchangeInfo";
pub const SERVER_TIME_ENDPOINT: &str = "/api/v3/time";
pub const OFFICIAL_SOURCE_ORIGIN: &str = "https://api.binance.com";
pub const VENUE: &str = "binance";
pub const MARKET: &str = "spot";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpotPriceFilter {
    /// A zero value preserves Binance's disabled PRICE_FILTER component.
    pub min_price: Decimal,
    /// A zero value preserves Binance's disabled PRICE_FILTER component.
    pub max_price: Decimal,
    /// A zero value preserves Binance's disabled PRICE_FILTER component.
    pub tick_size: Decimal,
}

impl SpotPriceFilter {
    fn validate(&self) -> Result<()> {
        if self.min_price < Decimal::ZERO
            || self.max_price < Decimal::ZERO
            || self.tick_size < Decimal::ZERO
            || (self.min_price > Decimal::ZERO
                && self.max_price > Decimal::ZERO
                && self.max_price < self.min_price)
        {
            bail!("Spot PRICE_FILTER bounds are invalid");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpotQuantityFilter {
    /// For MARKET_LOT_SIZE, zero preserves a disabled component.
    pub min_quantity: Decimal,
    /// For MARKET_LOT_SIZE, zero preserves a disabled component.
    pub max_quantity: Decimal,
    /// For MARKET_LOT_SIZE, zero preserves a disabled component.
    pub step_size: Decimal,
}

impl SpotQuantityFilter {
    fn validate(&self, allow_disabled_components: bool) -> Result<()> {
        if self.min_quantity < Decimal::ZERO
            || self.max_quantity < Decimal::ZERO
            || self.step_size < Decimal::ZERO
            || (!allow_disabled_components
                && (self.min_quantity == Decimal::ZERO
                    || self.max_quantity == Decimal::ZERO
                    || self.step_size == Decimal::ZERO))
            || (self.min_quantity > Decimal::ZERO
                && self.max_quantity > Decimal::ZERO
                && self.max_quantity < self.min_quantity)
        {
            bail!("Spot quantity filter bounds are invalid");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpotNotionalFilter {
    pub filter_type: String,
    pub min_notional: Decimal,
    pub max_notional: Option<Decimal>,
    pub apply_min_to_market: bool,
    pub apply_max_to_market: Option<bool>,
    /// Zero means that a MARKET order uses the latest price instead of an
    /// average over prior minutes.
    pub avg_price_mins: u64,
}

impl SpotNotionalFilter {
    fn validate(&self) -> Result<()> {
        let profile_shape_valid = match self.filter_type.as_str() {
            "MIN_NOTIONAL" => self.max_notional.is_none() && self.apply_max_to_market.is_none(),
            "NOTIONAL" => self.max_notional.is_some() && self.apply_max_to_market.is_some(),
            _ => false,
        };
        if !profile_shape_valid
            || self.min_notional <= Decimal::ZERO
            || self
                .max_notional
                .is_some_and(|max| max < self.min_notional || max <= Decimal::ZERO)
        {
            bail!("Spot notional filter bounds are invalid");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpotInstrumentRules {
    pub schema: String,
    pub venue: String,
    pub market: String,
    pub symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub status: String,
    pub is_spot_trading_allowed: bool,
    /// Zero is a valid integer precision and is retained as supplied.
    pub base_asset_precision: u64,
    /// Zero is a valid integer precision and is retained as supplied.
    pub quote_asset_precision: u64,
    pub price_filter: SpotPriceFilter,
    pub lot_size_filter: SpotQuantityFilter,
    pub market_lot_size_filter: Option<SpotQuantityFilter>,
    pub notional_filter: SpotNotionalFilter,
    pub source_time_ms: u64,
    pub source_clock_received_at_ns: u64,
    pub received_at_ns: u64,
    pub source_endpoint: String,
    pub source_clock_endpoint: String,
}

impl SpotInstrumentRules {
    pub fn validate(&self) -> Result<()> {
        validate_spot_symbol(&self.symbol)?;
        validate_asset(&self.base_asset, "base asset")?;
        validate_asset(&self.quote_asset, "quote asset")?;
        validate_spot_clocks(
            self.source_time_ms,
            self.source_clock_received_at_ns,
            self.received_at_ns,
        )?;
        validate_receive_clock(self.source_time_ms, self.source_clock_received_at_ns)?;
        validate_receive_clock(self.source_time_ms, self.received_at_ns)?;
        if self.source_clock_received_at_ns > self.received_at_ns {
            bail!("Spot exchangeInfo precedes its server-clock receipt");
        }
        if self.schema != REFERENCE_SCHEMA
            || self.venue != VENUE
            || self.market != MARKET
            || self.status != "TRADING"
            || !self.is_spot_trading_allowed
            || self.source_endpoint != EXCHANGE_INFO_ENDPOINT
            || self.source_clock_endpoint != SERVER_TIME_ENDPOINT
        {
            bail!("Spot instrument identity is invalid");
        }
        self.price_filter.validate()?;
        self.lot_size_filter.validate(false)?;
        if let Some(filter) = &self.market_lot_size_filter {
            filter.validate(true)?;
        }
        self.notional_filter.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotReferenceBatch {
    rules: Vec<SpotInstrumentRules>,
}

impl SpotReferenceBatch {
    pub fn new(mut rules: Vec<SpotInstrumentRules>) -> Result<Self> {
        let mut symbols = BTreeSet::new();
        for rule in &rules {
            rule.validate()?;
            if !symbols.insert(rule.symbol.clone()) {
                bail!("Spot reference batch has duplicate symbol {}", rule.symbol);
            }
        }
        if rules.is_empty() {
            bail!("Spot reference batch has no active instruments");
        }
        rules.sort_by(|left, right| left.symbol.cmp(&right.symbol));
        Ok(Self { rules })
    }

    pub fn rules(&self) -> &[SpotInstrumentRules] {
        &self.rules
    }

    pub fn symbols(&self) -> BTreeSet<String> {
        self.rules.iter().map(|rule| rule.symbol.clone()).collect()
    }

    pub fn source_time_bounds(&self) -> (u64, u64) {
        let mut times = self.rules.iter().map(|rule| rule.source_time_ms);
        let first = times.next().unwrap_or_default();
        times.fold((first, first), |(min, max), value| {
            (min.min(value), max.max(value))
        })
    }

    pub fn received_time_bounds(&self) -> (u64, u64) {
        let mut times = self
            .rules
            .iter()
            .flat_map(|rule| [rule.source_clock_received_at_ns, rule.received_at_ns]);
        let first = times.next().unwrap_or_default();
        times.fold((first, first), |(min, max), value| {
            (min.min(value), max.max(value))
        })
    }
}

/// Parse active Spot symbols from one public exchangeInfo response.  A
/// requested symbol that is absent, halted, or not Spot-enabled is an error;
/// when no allowlist is supplied, inactive symbols are excluded from the
/// active reference universe.
pub fn active_spot_instrument_rules(
    exchange_info: &Value,
    source_time_ms: u64,
    source_clock_received_at_ns: u64,
    received_at_ns: u64,
    requested_symbols: Option<&BTreeSet<String>>,
) -> Result<Vec<SpotInstrumentRules>> {
    validate_spot_clocks(source_time_ms, source_clock_received_at_ns, received_at_ns)?;
    validate_receive_clock(source_time_ms, source_clock_received_at_ns)?;
    validate_receive_clock(source_time_ms, received_at_ns)?;
    if source_clock_received_at_ns > received_at_ns {
        bail!("Spot exchangeInfo precedes its server-clock receipt");
    }
    let symbols = exchange_info
        .get("symbols")
        .and_then(Value::as_array)
        .context("Spot exchangeInfo symbols must be an array")?;
    let mut seen = BTreeSet::new();
    let mut rules = Vec::new();
    for raw in symbols {
        let symbol = required_string(raw, "symbol", EXCHANGE_INFO_ENDPOINT)?.to_ascii_uppercase();
        validate_spot_symbol(&symbol)?;
        let requested = requested_symbols.is_some_and(|symbols| symbols.contains(&symbol));
        if requested_symbols.is_some() && !requested {
            continue;
        }
        let status = required_string(raw, "status", EXCHANGE_INFO_ENDPOINT)?;
        let spot_allowed = raw
            .get("isSpotTradingAllowed")
            .and_then(Value::as_bool)
            .context("Spot exchangeInfo has invalid isSpotTradingAllowed")?;
        let has_spot_permission = permission_sets_include_spot(raw)?;
        if status != "TRADING" || !spot_allowed || !has_spot_permission {
            if requested {
                bail!("requested Spot symbol is not active and Spot-enabled: {symbol}");
            }
            continue;
        }
        if !seen.insert(symbol.clone()) {
            bail!("Spot exchangeInfo has duplicate active symbol {symbol}");
        }
        rules.push(SpotInstrumentRules {
            schema: REFERENCE_SCHEMA.to_owned(),
            venue: VENUE.to_owned(),
            market: MARKET.to_owned(),
            symbol,
            base_asset: required_string(raw, "baseAsset", EXCHANGE_INFO_ENDPOINT)?.to_owned(),
            quote_asset: required_string(raw, "quoteAsset", EXCHANGE_INFO_ENDPOINT)?.to_owned(),
            status: status.to_owned(),
            is_spot_trading_allowed: spot_allowed,
            base_asset_precision: required_u64(raw, "baseAssetPrecision", EXCHANGE_INFO_ENDPOINT)?,
            quote_asset_precision: required_u64(
                raw,
                "quoteAssetPrecision",
                EXCHANGE_INFO_ENDPOINT,
            )?,
            price_filter: SpotPriceFilter {
                min_price: required_filter_decimal_allow_zero(raw, "PRICE_FILTER", "minPrice")?,
                max_price: required_filter_decimal_allow_zero(raw, "PRICE_FILTER", "maxPrice")?,
                tick_size: required_filter_decimal_allow_zero(raw, "PRICE_FILTER", "tickSize")?,
            },
            lot_size_filter: SpotQuantityFilter {
                min_quantity: required_filter_decimal(raw, "LOT_SIZE", "minQty")?,
                max_quantity: required_filter_decimal(raw, "LOT_SIZE", "maxQty")?,
                step_size: required_filter_decimal(raw, "LOT_SIZE", "stepSize")?,
            },
            market_lot_size_filter: optional_quantity_filter(raw, "MARKET_LOT_SIZE")?,
            notional_filter: notional_filter(raw)?,
            source_time_ms,
            source_clock_received_at_ns,
            received_at_ns,
            source_endpoint: EXCHANGE_INFO_ENDPOINT.to_owned(),
            source_clock_endpoint: SERVER_TIME_ENDPOINT.to_owned(),
        });
    }
    if let Some(requested_symbols) = requested_symbols {
        let missing = requested_symbols
            .difference(&seen)
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            bail!("requested Spot symbols are missing from active exchangeInfo: {missing:?}");
        }
    }
    rules.sort_by(|left, right| left.symbol.cmp(&right.symbol));
    SpotReferenceBatch::new(rules.clone())?;
    Ok(rules)
}

pub fn observe_reference_clocks(
    clocks: &mut ReferenceClockValidator,
    rules: &[SpotInstrumentRules],
) -> Result<()> {
    for rule in rules {
        clocks.observe(
            ReferenceMarket::Spot,
            ReferenceKind::Metadata,
            &rule.symbol,
            rule.source_time_ms,
            rule.received_at_ns,
        )?;
    }
    Ok(())
}

fn permission_sets_include_spot(raw: &Value) -> Result<bool> {
    if let Some(permission_sets) = raw.get("permissionSets") {
        let groups = permission_sets
            .as_array()
            .context("Spot exchangeInfo permissionSets must be an array")?;
        let mut has_spot = false;
        for group in groups {
            let permissions = group
                .as_array()
                .context("Spot exchangeInfo permissionSets group must be an array")?;
            for permission in permissions {
                let permission = permission
                    .as_str()
                    .context("Spot exchangeInfo permission must be a string")?;
                has_spot |= permission == "SPOT";
            }
        }
        return Ok(has_spot);
    }
    if let Some(permissions) = raw.get("permissions") {
        let permissions = permissions
            .as_array()
            .context("Spot exchangeInfo permissions must be an array")?;
        return permissions
            .iter()
            .map(|permission| {
                permission
                    .as_str()
                    .context("Spot exchangeInfo permission must be a string")
            })
            .try_fold(false, |has_spot, permission| {
                let permission = permission?;
                Ok(has_spot || permission == "SPOT")
            });
    }
    bail!("Spot exchangeInfo is missing permissionSets or permissions")
}

fn optional_quantity_filter(raw: &Value, filter_type: &str) -> Result<Option<SpotQuantityFilter>> {
    let Some(_) = find_filter(raw, filter_type)? else {
        return Ok(None);
    };
    let min_quantity = required_filter_decimal_allow_zero(raw, filter_type, "minQty")?;
    let max_quantity = required_filter_decimal_allow_zero(raw, filter_type, "maxQty")?;
    let step_size = required_filter_decimal_allow_zero(raw, filter_type, "stepSize")?;
    let filter = SpotQuantityFilter {
        min_quantity,
        max_quantity,
        step_size,
    };
    filter.validate(true)?;
    Ok(Some(filter))
}

fn notional_filter(raw: &Value) -> Result<SpotNotionalFilter> {
    let notional = find_filter(raw, "NOTIONAL")?;
    let min_notional = find_filter(raw, "MIN_NOTIONAL")?;
    let (filter_type, filter) = match (notional, min_notional) {
        (Some(_), Some(_)) => bail!("Spot exchangeInfo has duplicate notional filter profiles"),
        (Some(filter), None) => ("NOTIONAL", filter),
        (None, Some(filter)) => ("MIN_NOTIONAL", filter),
        (None, None) => bail!("Spot exchangeInfo is missing NOTIONAL or MIN_NOTIONAL"),
    };
    let min = required_decimal(filter, "minNotional", EXCHANGE_INFO_ENDPOINT)?;
    let (max, apply_min_to_market, apply_max_to_market) = if filter_type == "NOTIONAL" {
        (
            Some(required_decimal(
                filter,
                "maxNotional",
                EXCHANGE_INFO_ENDPOINT,
            )?),
            required_bool(filter, "applyMinToMarket")?,
            Some(required_bool(filter, "applyMaxToMarket")?),
        )
    } else {
        (None, required_bool(filter, "applyToMarket")?, None)
    };
    let result = SpotNotionalFilter {
        filter_type: filter_type.to_owned(),
        min_notional: min,
        max_notional: max,
        apply_min_to_market,
        apply_max_to_market,
        avg_price_mins: required_u64(filter, "avgPriceMins", EXCHANGE_INFO_ENDPOINT)?,
    };
    result.validate()?;
    Ok(result)
}

fn find_filter<'a>(raw: &'a Value, filter_type: &str) -> Result<Option<&'a Value>> {
    let filters = raw
        .get("filters")
        .and_then(Value::as_array)
        .context("Spot exchangeInfo filters must be an array")?;
    let mut matches = filters
        .iter()
        .filter(|filter| filter.get("filterType").and_then(Value::as_str) == Some(filter_type));
    let filter = matches.next();
    if matches.next().is_some() {
        bail!("Spot exchangeInfo has duplicate {filter_type}");
    }
    Ok(filter)
}

fn required_bool(raw: &Value, field: &str) -> Result<bool> {
    raw.get(field)
        .and_then(Value::as_bool)
        .with_context(|| format!("Spot exchangeInfo has invalid {field}"))
}

fn validate_spot_symbol(symbol: &str) -> Result<()> {
    if symbol.is_empty()
        || symbol.len() > 32
        || !symbol
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        bail!("invalid Spot symbol identity");
    }
    Ok(())
}

fn validate_asset(asset: &str, label: &str) -> Result<()> {
    if asset.is_empty()
        || asset.len() > 32
        || !asset
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        bail!("invalid Spot {label} identity");
    }
    Ok(())
}

fn validate_spot_clocks(
    source_time_ms: u64,
    source_clock_received_at_ns: u64,
    received_at_ns: u64,
) -> Result<()> {
    if source_time_ms == 0 || source_clock_received_at_ns == 0 || received_at_ns == 0 {
        bail!("Spot reference clocks must be positive");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const CLOCK_RECEIVED_NS: u64 = 1_700_000_000_100_000_000;
    const EXCHANGE_RECEIVED_NS: u64 = 1_700_000_000_200_000_000;

    fn symbol(symbol: &str) -> Value {
        json!({
            "symbol": symbol,
            "status": "TRADING",
            "isSpotTradingAllowed": true,
            "permissionSets": [["SPOT"]],
            "baseAsset": "BTC",
            "quoteAsset": "USDT",
            "baseAssetPrecision": 8,
            "quoteAssetPrecision": 8,
            "filters": [
                {"filterType":"PRICE_FILTER","minPrice":"0.01","maxPrice":"1000000","tickSize":"0.01"},
                {"filterType":"LOT_SIZE","minQty":"0.00001","maxQty":"9000","stepSize":"0.00001"},
                {"filterType":"MIN_NOTIONAL","minNotional":"5","applyToMarket":true,"avgPriceMins":5}
            ]
        })
    }

    #[test]
    fn requested_symbols_are_a_strict_output_whitelist() {
        let exchange_info = json!({
            "symbols": [symbol("BTCUSDT"), symbol("ETHUSDT")]
        });
        let requested = BTreeSet::from(["BTCUSDT".to_owned()]);

        let rules = active_spot_instrument_rules(
            &exchange_info,
            SOURCE_MS,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            Some(&requested),
        )
        .unwrap();

        assert_eq!(
            rules
                .iter()
                .map(|rule| rule.symbol.as_str())
                .collect::<Vec<_>>(),
            ["BTCUSDT"]
        );
    }

    #[test]
    fn missing_or_malformed_spot_permissions_fail_closed() {
        let mut missing = symbol("BTCUSDT");
        missing.as_object_mut().unwrap().remove("permissionSets");
        assert!(active_spot_instrument_rules(
            &json!({"symbols": [missing]}),
            SOURCE_MS,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            None,
        )
        .is_err());

        let mut malformed_group = symbol("BTCUSDT");
        malformed_group["permissionSets"] = json!(["SPOT"]);
        assert!(active_spot_instrument_rules(
            &json!({"symbols": [malformed_group]}),
            SOURCE_MS,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            None,
        )
        .is_err());

        let mut malformed_permission = symbol("BTCUSDT");
        malformed_permission["permissionSets"] = json!([["SPOT", 1]]);
        assert!(active_spot_instrument_rules(
            &json!({"symbols": [malformed_permission]}),
            SOURCE_MS,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            None,
        )
        .is_err());

        let mut alternate = symbol("BTCUSDT");
        alternate.as_object_mut().unwrap().remove("permissionSets");
        alternate["permissions"] = json!(["SPOT"]);
        assert_eq!(
            active_spot_instrument_rules(
                &json!({"symbols": [alternate]}),
                SOURCE_MS,
                CLOCK_RECEIVED_NS,
                EXCHANGE_RECEIVED_NS,
                None,
            )
            .unwrap()
            .len(),
            1
        );

        let mut malformed_alternate = symbol("BTCUSDT");
        malformed_alternate
            .as_object_mut()
            .unwrap()
            .remove("permissionSets");
        malformed_alternate["permissions"] = json!(["SPOT", 1]);
        assert!(active_spot_instrument_rules(
            &json!({"symbols": [malformed_alternate]}),
            SOURCE_MS,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            None,
        )
        .is_err());
    }

    fn exchange_info() -> Value {
        json!({"symbols": [symbol("BTCUSDT")]})
    }

    #[test]
    fn parses_active_spot_rules_with_real_limits_and_clocks() {
        let rules = active_spot_instrument_rules(
            &exchange_info(),
            SOURCE_MS,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            None,
        )
        .unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].market, MARKET);
        assert_eq!(rules[0].price_filter.tick_size, Decimal::new(1, 2));
        assert_eq!(rules[0].lot_size_filter.max_quantity, Decimal::from(9000));
        assert_eq!(rules[0].notional_filter.min_notional, Decimal::from(5));
        assert_eq!(rules[0].notional_filter.avg_price_mins, 5);
        SpotReferenceBatch::new(rules).unwrap();
    }

    #[test]
    fn missing_or_invalid_spot_fields_fail_closed() {
        for mutate in [
            |row: &mut Value| {
                row.as_object_mut().unwrap().remove("baseAsset");
            },
            |row: &mut Value| {
                row["filters"][1]["maxQty"] = json!("bad");
            },
            |row: &mut Value| {
                row["filters"] = json!([]);
            },
            |row: &mut Value| {
                row["filters"][2]
                    .as_object_mut()
                    .unwrap()
                    .remove("avgPriceMins");
            },
        ] {
            let mut row = symbol("BTCUSDT");
            mutate(&mut row);
            assert!(active_spot_instrument_rules(
                &json!({"symbols": [row]}),
                SOURCE_MS,
                CLOCK_RECEIVED_NS,
                EXCHANGE_RECEIVED_NS,
                None,
            )
            .is_err());
        }
    }

    #[test]
    fn spot_disabled_filter_components_and_zero_precision_are_preserved() {
        let mut row = symbol("BTCUSDT");
        row["baseAssetPrecision"] = json!(0);
        row["quoteAssetPrecision"] = json!(0);
        row["filters"][0]["minPrice"] = json!("0");
        row["filters"][0]["maxPrice"] = json!("0");
        row["filters"][0]["tickSize"] = json!("0");
        row["filters"].as_array_mut().unwrap().push(json!({
            "filterType": "MARKET_LOT_SIZE",
            "minQty": "0",
            "maxQty": "9000",
            "stepSize": "0"
        }));

        let rules = active_spot_instrument_rules(
            &json!({"symbols": [row]}),
            SOURCE_MS,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            None,
        )
        .unwrap();
        let rule = &rules[0];
        assert_eq!(rule.base_asset_precision, 0);
        assert_eq!(rule.quote_asset_precision, 0);
        assert_eq!(rule.price_filter.min_price, Decimal::ZERO);
        assert_eq!(rule.price_filter.max_price, Decimal::ZERO);
        assert_eq!(rule.price_filter.tick_size, Decimal::ZERO);
        assert_eq!(rule.notional_filter.avg_price_mins, 5);
        assert_eq!(
            rule.market_lot_size_filter,
            Some(SpotQuantityFilter {
                min_quantity: Decimal::ZERO,
                max_quantity: Decimal::from(9_000),
                step_size: Decimal::ZERO,
            })
        );
    }

    #[test]
    fn spot_negative_and_reversed_filter_bounds_fail_closed() {
        let mutations: [fn(&mut Value); 5] = [
            |row| row["filters"][0]["minPrice"] = json!("-0.01"),
            |row| row["filters"][0]["maxPrice"] = json!("-0.01"),
            |row| {
                row["filters"][0]["minPrice"] = json!("2");
                row["filters"][0]["maxPrice"] = json!("1");
            },
            |row| {
                row["filters"].as_array_mut().unwrap().push(json!({
                    "filterType": "MARKET_LOT_SIZE",
                    "minQty": "-1",
                    "maxQty": "9000",
                    "stepSize": "0.1"
                }));
            },
            |row| {
                row["filters"].as_array_mut().unwrap().push(json!({
                    "filterType": "MARKET_LOT_SIZE",
                    "minQty": "2",
                    "maxQty": "1",
                    "stepSize": "0.1"
                }));
            },
        ];
        for mutate in mutations {
            let mut row = symbol("BTCUSDT");
            mutate(&mut row);
            assert!(active_spot_instrument_rules(
                &json!({"symbols": [row]}),
                SOURCE_MS,
                CLOCK_RECEIVED_NS,
                EXCHANGE_RECEIVED_NS,
                None,
            )
            .is_err());
        }
    }

    #[test]
    fn spot_notional_filter_profile_shape_is_fail_closed() {
        let rules = active_spot_instrument_rules(
            &exchange_info(),
            SOURCE_MS,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            None,
        )
        .unwrap();

        for (filter_type, max_notional, apply_max_to_market) in [
            ("MIN_NOTIONAL", Some(Decimal::from(100)), None),
            ("MIN_NOTIONAL", None, Some(false)),
            ("NOTIONAL", None, Some(false)),
            ("NOTIONAL", Some(Decimal::from(100)), None),
        ] {
            let mut rule = rules[0].clone();
            rule.notional_filter.filter_type = filter_type.to_owned();
            rule.notional_filter.max_notional = max_notional;
            rule.notional_filter.apply_max_to_market = apply_max_to_market;
            assert!(SpotReferenceBatch::new(vec![rule]).is_err());
        }

        let mut valid = rules[0].clone();
        valid.notional_filter.filter_type = "NOTIONAL".to_owned();
        valid.notional_filter.max_notional = Some(Decimal::from(100));
        valid.notional_filter.apply_min_to_market = false;
        valid.notional_filter.apply_max_to_market = Some(false);
        assert!(SpotReferenceBatch::new(vec![valid]).is_ok());
    }

    #[test]
    fn non_spot_requested_symbol_and_clock_regression_fail_closed() {
        let mut blocked = symbol("BLOCKEDUSDT");
        blocked["isSpotTradingAllowed"] = json!(false);
        let requested = BTreeSet::from(["BLOCKEDUSDT".to_owned()]);
        assert!(active_spot_instrument_rules(
            &json!({"symbols": [blocked]}),
            SOURCE_MS,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            Some(&requested),
        )
        .is_err());

        let mut clocks = ReferenceClockValidator::default();
        let rules = active_spot_instrument_rules(
            &exchange_info(),
            SOURCE_MS,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            None,
        )
        .unwrap();
        observe_reference_clocks(&mut clocks, &rules).unwrap();
        let mut regressed = rules[0].clone();
        regressed.source_time_ms -= 1;
        assert!(observe_reference_clocks(&mut clocks, &[regressed]).is_err());
    }

    #[test]
    fn inactive_symbols_are_excluded_without_faking_spot_rules() {
        let mut halted = symbol("HALTEDUSDT");
        halted["status"] = json!("BREAK");
        let rules = active_spot_instrument_rules(
            &json!({"symbols": [symbol("BTCUSDT"), halted]}),
            SOURCE_MS,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            None,
        )
        .unwrap();
        assert_eq!(
            rules
                .iter()
                .map(|row| row.symbol.as_str())
                .collect::<Vec<_>>(),
            ["BTCUSDT"]
        );
    }
}
