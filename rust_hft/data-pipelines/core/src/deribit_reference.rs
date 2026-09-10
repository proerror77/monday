//! Typed Deribit option reference observations.
//!
//! Transport and HTTP receipt timing live in adapter-deribit-reference. This
//! module owns the stable instrument identity, unit contract, null handling,
//! and source-clock validation shared by the collector and research loader.

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;

pub const DERIBIT_IV_SOURCE_UNIT: &str = "percent_points";
pub const DERIBIT_IV_STORAGE_UNIT: &str = "decimal_fraction";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeribitOptionIdentity {
    pub currency: String,
    pub instrument_name: String,
    pub expiry_timestamp_ms: i64,
    pub strike: Decimal,
    pub option_type: String,
    /// Instrument metadata creation time, when supplied by an instrument
    /// metadata endpoint. Market-data responses do not provide this field.
    pub creation_timestamp_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeribitIvObservation {
    pub identity: DeribitOptionIdentity,
    pub source_iv_unit: String,
    pub iv_unit: String,
    pub mark_iv_raw: Option<Decimal>,
    pub bid_iv_raw: Option<Decimal>,
    pub ask_iv_raw: Option<Decimal>,
    pub mark_iv: Option<Decimal>,
    pub bid_iv: Option<Decimal>,
    pub ask_iv: Option<Decimal>,
    pub underlying_price: Option<Decimal>,
    pub index_price: Option<Decimal>,
    pub mark_price: Option<Decimal>,
    pub best_bid_price: Option<Decimal>,
    pub best_ask_price: Option<Decimal>,
    pub open_interest: Option<Decimal>,
    pub volume: Option<Decimal>,
    /// The source clock exposed by the summary response. It is retained as a
    /// source clock without being used as instrument metadata.
    pub source_timestamp_ms: Option<i64>,
    pub received_at_us: u64,
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeribitGreeksObservation {
    pub identity: DeribitOptionIdentity,
    pub source_iv_unit: String,
    pub iv_unit: String,
    pub source_timestamp_ms: i64,
    pub received_at_us: u64,
    pub mark_iv_raw: Option<Decimal>,
    pub bid_iv_raw: Option<Decimal>,
    pub ask_iv_raw: Option<Decimal>,
    pub mark_iv: Option<Decimal>,
    pub bid_iv: Option<Decimal>,
    pub ask_iv: Option<Decimal>,
    pub delta: Option<Decimal>,
    pub gamma: Option<Decimal>,
    pub vega: Option<Decimal>,
    pub theta: Option<Decimal>,
    pub rho: Option<Decimal>,
    pub mark_price: Option<Decimal>,
    pub underlying_price: Option<Decimal>,
    pub index_price: Option<Decimal>,
    pub best_bid_price: Option<Decimal>,
    pub best_ask_price: Option<Decimal>,
    pub open_interest: Option<Decimal>,
    pub raw: Value,
}

pub fn parse_iv_summary_row(
    row: &Value,
    requested_currency: &str,
    received_at_us: u64,
) -> Result<DeribitIvObservation> {
    require_receive_clock(received_at_us)?;
    let instrument_name = required_string(row, "instrument_name")?;
    let source_timestamp_ms = required_positive_i64(row, "creation_timestamp")?;
    let identity = parse_option_identity(instrument_name, requested_currency, None)?;
    let (mark_iv_raw, mark_iv) = parse_iv(row, "mark_iv")?;
    let (bid_iv_raw, bid_iv) = parse_iv(row, "bid_iv")?;
    let (ask_iv_raw, ask_iv) = parse_iv(row, "ask_iv")?;
    Ok(DeribitIvObservation {
        identity,
        source_iv_unit: DERIBIT_IV_SOURCE_UNIT.to_owned(),
        iv_unit: DERIBIT_IV_STORAGE_UNIT.to_owned(),
        mark_iv_raw,
        bid_iv_raw,
        ask_iv_raw,
        mark_iv,
        bid_iv,
        ask_iv,
        underlying_price: optional_nonnegative_decimal(row, "underlying_price")?,
        index_price: optional_nonnegative_decimal(row, "index_price")?,
        mark_price: optional_nonnegative_decimal(row, "mark_price")?,
        best_bid_price: optional_nonnegative_decimal(row, "bid_price")?,
        best_ask_price: optional_nonnegative_decimal(row, "ask_price")?,
        open_interest: optional_nonnegative_decimal(row, "open_interest")?,
        volume: optional_nonnegative_decimal(row, "volume")?,
        source_timestamp_ms: Some(source_timestamp_ms),
        received_at_us,
        raw: row.clone(),
    })
}

pub fn parse_greeks_result(
    result: &Value,
    requested_currency: &str,
    requested_instrument: &str,
    received_at_us: u64,
) -> Result<DeribitGreeksObservation> {
    require_receive_clock(received_at_us)?;
    let instrument_name = required_string(result, "instrument_name")?;
    if instrument_name != requested_instrument {
        bail!(
            "Deribit Greeks instrument mismatch: requested {requested_instrument}, got {instrument_name}"
        );
    }
    let source_timestamp_ms = required_positive_i64(result, "timestamp")?;
    let identity = parse_option_identity(instrument_name, requested_currency, None)?;
    let (mark_iv_raw, mark_iv) = parse_iv(result, "mark_iv")?;
    let (bid_iv_raw, bid_iv) = parse_iv(result, "bid_iv")?;
    let (ask_iv_raw, ask_iv) = parse_iv(result, "ask_iv")?;
    let greeks = result.get("greeks");
    Ok(DeribitGreeksObservation {
        identity,
        source_iv_unit: DERIBIT_IV_SOURCE_UNIT.to_owned(),
        iv_unit: DERIBIT_IV_STORAGE_UNIT.to_owned(),
        source_timestamp_ms,
        received_at_us,
        mark_iv_raw,
        bid_iv_raw,
        ask_iv_raw,
        mark_iv,
        bid_iv,
        ask_iv,
        delta: optional_decimal_object(greeks, "delta")?,
        gamma: optional_decimal_object(greeks, "gamma")?,
        vega: optional_decimal_object(greeks, "vega")?,
        theta: optional_decimal_object(greeks, "theta")?,
        rho: optional_decimal_object(greeks, "rho")?,
        mark_price: optional_nonnegative_decimal(result, "mark_price")?,
        underlying_price: optional_nonnegative_decimal(result, "underlying_price")?,
        index_price: optional_nonnegative_decimal(result, "index_price")?,
        best_bid_price: optional_nonnegative_decimal(result, "best_bid_price")?,
        best_ask_price: optional_nonnegative_decimal(result, "best_ask_price")?,
        open_interest: optional_nonnegative_decimal(result, "open_interest")?,
        raw: result.clone(),
    })
}

pub fn parse_option_identity(
    instrument_name: &str,
    requested_currency: &str,
    creation_timestamp_ms: Option<i64>,
) -> Result<DeribitOptionIdentity> {
    if let Some(timestamp_ms) = creation_timestamp_ms {
        if timestamp_ms <= 0 {
            bail!("Deribit instrument creation timestamp must be positive");
        }
    }
    let parts: Vec<&str> = instrument_name.split('-').collect();
    if parts.len() != 4 {
        bail!("Deribit option instrument name has invalid shape: {instrument_name}");
    }
    let currency = parts[0].to_ascii_uppercase();
    if currency != requested_currency.trim().to_ascii_uppercase() {
        bail!("Deribit instrument currency does not match requested currency");
    }
    let expiry_timestamp_ms = parse_deribit_expiry_ms(parts[1])?;
    let strike = Decimal::from_str(parts[2]).context("Deribit option strike is invalid")?;
    if strike <= Decimal::ZERO {
        bail!("Deribit option strike must be positive");
    }
    let option_type = parts[3].to_ascii_uppercase();
    if option_type != "C" && option_type != "P" {
        bail!("Deribit option type must be C or P");
    }
    Ok(DeribitOptionIdentity {
        currency,
        instrument_name: instrument_name.to_owned(),
        expiry_timestamp_ms,
        strike,
        option_type,
        creation_timestamp_ms,
    })
}

pub fn parse_deribit_expiry_ms(code: &str) -> Result<i64> {
    let day_len = match code.len() {
        6 => 1,
        7 => 2,
        _ => {
            bail!("Deribit expiry code has invalid length");
        }
    };
    if !code.is_ascii() {
        bail!("Deribit expiry code must be ASCII");
    }
    let month_start = day_len;
    let month_end = month_start + 3;
    let year_start = month_end;
    let day: u32 = code[..day_len]
        .parse()
        .context("Deribit expiry day is invalid")?;
    let month = match code[month_start..month_end].to_ascii_uppercase().as_str() {
        "JAN" => 1,
        "FEB" => 2,
        "MAR" => 3,
        "APR" => 4,
        "MAY" => 5,
        "JUN" => 6,
        "JUL" => 7,
        "AUG" => 8,
        "SEP" => 9,
        "OCT" => 10,
        "NOV" => 11,
        "DEC" => 12,
        _ => bail!("Deribit expiry month is invalid"),
    };
    let short_year: i32 = code[year_start..]
        .parse()
        .context("Deribit expiry year is invalid")?;
    let year = 2000 + short_year;
    let date = chrono::NaiveDate::from_ymd_opt(year, month, day)
        .context("Deribit expiry date is invalid")?;
    let timestamp = date
        .and_hms_opt(8, 0, 0)
        .context("Deribit expiry time is invalid")?
        .and_utc()
        .timestamp_millis();
    if timestamp <= 0 {
        bail!("Deribit expiry timestamp is invalid");
    }
    Ok(timestamp)
}

fn parse_iv(row: &Value, key: &str) -> Result<(Option<Decimal>, Option<Decimal>)> {
    let raw = optional_nonnegative_decimal(row, key)?;
    let normalized = raw.map(|value| value / Decimal::from(100));
    Ok((raw, normalized))
}

fn optional_decimal(row: &Value, key: &str) -> Result<Option<Decimal>> {
    match row.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let text = value
                .as_str()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| value.to_string());
            Decimal::from_str(&text)
                .map(Some)
                .with_context(|| format!("Deribit {key} is malformed"))
        }
    }
}

fn optional_nonnegative_decimal(row: &Value, key: &str) -> Result<Option<Decimal>> {
    let value = optional_decimal(row, key)?;
    if value.is_some_and(|value| value < Decimal::ZERO) {
        bail!("Deribit {key} must be non-negative");
    }
    Ok(value)
}

fn optional_decimal_object(object: Option<&Value>, key: &str) -> Result<Option<Decimal>> {
    match object {
        None | Some(Value::Null) => Ok(None),
        Some(value) if value.is_object() => optional_decimal(value, key),
        Some(_) => bail!("Deribit greeks object is malformed"),
    }
}

fn required_string<'a>(row: &'a Value, key: &str) -> Result<&'a str> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("Deribit {key} is missing or empty"))
}

fn required_i64(row: &Value, key: &str) -> Result<i64> {
    row.get(key)
        .and_then(Value::as_i64)
        .with_context(|| format!("Deribit {key} is missing or invalid"))
}

fn required_positive_i64(row: &Value, key: &str) -> Result<i64> {
    let value = required_i64(row, key)?;
    if value <= 0 {
        bail!("Deribit {key} must be positive");
    }
    Ok(value)
}

fn require_receive_clock(received_at_us: u64) -> Result<()> {
    if received_at_us == 0 {
        bail!("Deribit receive clock must be positive");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_identity_and_expiry_without_losing_strike_or_side() {
        let identity =
            parse_option_identity("BTC-29MAR24-50000-C", "BTC", Some(1_700_000_000_000)).unwrap();
        assert_eq!(identity.strike, Decimal::from(50_000));
        assert_eq!(identity.option_type, "C");
        assert_eq!(
            identity.expiry_timestamp_ms,
            parse_deribit_expiry_ms("29MAR24").unwrap()
        );
    }

    #[test]
    fn iv_preserves_percent_points_and_normalizes_explicitly() {
        let row = json!({
            "instrument_name":"BTC-6SEP24-50000-C",
            "creation_timestamp":1_700_000_000_000_i64,
            "mark_iv":77.72,
            "bid_iv":null,
            "ask_iv":90.26,
            "unknown":"preserve"
        });
        let observation = parse_iv_summary_row(&row, "BTC", 1_700_000_001_000_000).unwrap();
        assert_eq!(
            observation.mark_iv_raw,
            Some(Decimal::from_str("77.72").unwrap())
        );
        assert_eq!(
            observation.mark_iv,
            Some(Decimal::from_str("0.7772").unwrap())
        );
        assert_eq!(observation.source_iv_unit, DERIBIT_IV_SOURCE_UNIT);
        assert_eq!(observation.iv_unit, DERIBIT_IV_STORAGE_UNIT);
        assert_eq!(observation.bid_iv, None);
        assert_eq!(observation.raw["unknown"], "preserve");
        assert_eq!(observation.source_timestamp_ms, Some(1_700_000_000_000));
        assert_eq!(observation.identity.creation_timestamp_ms, None);
        assert_eq!(
            parse_deribit_expiry_ms("6SEP24").unwrap(),
            parse_deribit_expiry_ms("06SEP24").unwrap()
        );
    }

    #[test]
    fn greeks_requires_instrument_and_source_clocks_but_preserves_nulls() {
        let result = json!({
            "instrument_name":"BTC-6SEP24-50000-C",
            "timestamp":1_700_000_002_000_i64,
            "mark_iv":77.72,
            "greeks":{"delta":null,"gamma":0.01,"vega":0.2,"theta":-0.1,"rho":null}
        });
        let observation =
            parse_greeks_result(&result, "BTC", "BTC-6SEP24-50000-C", 1_700_000_003_000_000)
                .unwrap();
        assert_eq!(observation.delta, None);
        assert_eq!(observation.gamma, Some(Decimal::from_str("0.01").unwrap()));
        assert_eq!(observation.identity.creation_timestamp_ms, None);
        assert_eq!(observation.source_timestamp_ms, 1_700_000_002_000);
        assert_eq!(
            observation.mark_iv,
            Some(Decimal::from_str("0.7772").unwrap())
        );
        assert_eq!(observation.source_iv_unit, DERIBIT_IV_SOURCE_UNIT);
        assert_eq!(observation.iv_unit, DERIBIT_IV_STORAGE_UNIT);
        assert!(parse_greeks_result(&result, "BTC", "ETH-6SEP24-50000-C", 1).is_err());
        assert!(parse_greeks_result(&result, "BTC", "BTC-6SEP24-50000-C", 0).is_err());
    }

    #[test]
    fn malformed_non_null_values_do_not_default_to_null() {
        let mut row = json!({
            "instrument_name":"BTC-6SEP24-50000-C",
            "creation_timestamp":1_700_000_000_000_i64,
            "mark_iv": "bad"
        });
        assert!(parse_iv_summary_row(&row, "BTC", 1).is_err());
        row["mark_iv"] = json!(null);
        assert!(parse_iv_summary_row(&row, "BTC", 1).is_ok());

        let mut greeks = row.clone();
        greeks["instrument_name"] = json!("BTC-6SEP24-50000-C");
        greeks["mark_iv"] = json!(77.72);
        greeks["timestamp"] = json!(1_700_000_002_000_i64);
        greeks["greeks"] = json!("bad");
        assert!(parse_greeks_result(&greeks, "BTC", "BTC-6SEP24-50000-C", 1).is_err());
    }

    #[test]
    fn rejects_negative_iv_prices_and_open_interest_but_keeps_empty_book_zero() {
        let mut row = json!({
            "instrument_name":"BTC-6SEP24-50000-C",
            "creation_timestamp":1_700_000_000_000_i64,
            "mark_iv":77.72,
            "bid_price":0,
            "open_interest":1
        });
        assert!(parse_iv_summary_row(&row, "BTC", 1).is_ok());

        row["mark_iv"] = json!(-0.01);
        assert!(parse_iv_summary_row(&row, "BTC", 1).is_err());
        row["mark_iv"] = json!(77.72);
        row["bid_price"] = json!(-1);
        assert!(parse_iv_summary_row(&row, "BTC", 1).is_err());
        row["bid_price"] = json!(0);
        row["open_interest"] = json!(-1);
        assert!(parse_iv_summary_row(&row, "BTC", 1).is_err());
    }
}
