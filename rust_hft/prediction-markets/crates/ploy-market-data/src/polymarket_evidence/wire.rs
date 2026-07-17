use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{de::Error as _, Deserialize, Deserializer};
use serde_json::{Map, Value};
use std::str::FromStr;

pub(super) const ROW_SCHEMA: &str = "monday.polymarket.evidence_row.v1";

const CONTEXT_FIELDS: [&str; 8] = [
    "schema",
    "surface",
    "market_id",
    "condition_id",
    "symbol",
    "event_start",
    "event_end",
    "window_secs",
];

const CONTRACT_FIELDS: [&str; 11] = [
    "source_token_ids",
    "source_outcomes",
    "price_to_beat",
    "resolution_source",
    "metadata_retrieved_at",
    "discovery_recorded_at",
    "metadata_recorded_at",
    "available_at",
    "discovery_source_sequence",
    "metadata_source_sequence",
    "source_datasets",
];

const BOOK_FIELDS: [&str; 12] = [
    "token_id",
    "ts",
    "recorded_at",
    "available_at",
    "source_sequence",
    "source_dataset",
    "bid",
    "ask",
    "bid_size",
    "ask_size",
    "bid_levels",
    "ask_levels",
];

const REFERENCE_FIELDS: [&str; 12] = [
    "source",
    "asset_class",
    "source_symbol",
    "price",
    "full_accuracy_value",
    "is_carried_forward",
    "ts",
    "received_at",
    "available_at",
    "recorded_at",
    "source_sequence",
    "source_dataset",
];

const TRADE_FIELDS: [&str; 18] = [
    "record_id",
    "record_id_version",
    "token_id",
    "source_outcome",
    "outcome_index",
    "side",
    "size",
    "price",
    "trade_ts",
    "trade_ts_unix",
    "transaction_hash",
    "proxy_wallet",
    "source",
    "received_at",
    "available_at",
    "recorded_at",
    "source_sequence",
    "source_dataset",
];

const SETTLEMENT_FIELDS: [&str; 11] = [
    "source_token_ids",
    "source_outcomes",
    "source_outcome_prices",
    "winning_token_id",
    "winning_outcome",
    "resolution_source",
    "retrieved_at",
    "available_at",
    "recorded_at",
    "source_sequence",
    "source_dataset",
];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct RowContext {
    pub(super) schema: String,
    pub(super) market_id: String,
    pub(super) condition_id: String,
    pub(super) symbol: String,
    pub(super) event_start: DateTime<Utc>,
    pub(super) event_end: DateTime<Utc>,
    pub(super) window_secs: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawContract {
    #[serde(flatten)]
    pub(super) context: RowContext,
    pub(super) source_token_ids: [String; 2],
    pub(super) source_outcomes: [String; 2],
    #[serde(deserialize_with = "deserialize_decimal")]
    pub(super) price_to_beat: Decimal,
    pub(super) resolution_source: String,
    pub(super) metadata_retrieved_at: DateTime<Utc>,
    pub(super) discovery_recorded_at: DateTime<Utc>,
    pub(super) metadata_recorded_at: DateTime<Utc>,
    pub(super) available_at: DateTime<Utc>,
    pub(super) discovery_source_sequence: u64,
    pub(super) metadata_source_sequence: u64,
    pub(super) source_datasets: [String; 2],
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawBookLevel {
    #[serde(deserialize_with = "deserialize_decimal")]
    pub(super) price: Decimal,
    #[serde(deserialize_with = "deserialize_decimal")]
    pub(super) size: Decimal,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawBook {
    #[serde(flatten)]
    pub(super) context: RowContext,
    pub(super) token_id: String,
    pub(super) ts: DateTime<Utc>,
    pub(super) recorded_at: DateTime<Utc>,
    pub(super) available_at: DateTime<Utc>,
    pub(super) source_sequence: u64,
    pub(super) source_dataset: String,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub(super) bid: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub(super) ask: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub(super) bid_size: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_optional_decimal")]
    pub(super) ask_size: Option<Decimal>,
    pub(super) bid_levels: Option<Vec<RawBookLevel>>,
    pub(super) ask_levels: Option<Vec<RawBookLevel>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawReference {
    #[serde(flatten)]
    pub(super) context: RowContext,
    pub(super) source: String,
    pub(super) asset_class: String,
    pub(super) source_symbol: String,
    #[serde(deserialize_with = "deserialize_decimal")]
    pub(super) price: Decimal,
    #[serde(rename = "full_accuracy_value")]
    _full_accuracy_value: Option<String>,
    pub(super) is_carried_forward: bool,
    pub(super) ts: DateTime<Utc>,
    pub(super) received_at: Option<DateTime<Utc>>,
    pub(super) available_at: DateTime<Utc>,
    pub(super) recorded_at: DateTime<Utc>,
    pub(super) source_sequence: u64,
    pub(super) source_dataset: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawTrade {
    #[serde(flatten)]
    pub(super) context: RowContext,
    pub(super) record_id: String,
    pub(super) record_id_version: String,
    pub(super) token_id: String,
    pub(super) source_outcome: String,
    pub(super) outcome_index: u8,
    pub(super) side: String,
    #[serde(deserialize_with = "deserialize_decimal")]
    pub(super) size: Decimal,
    #[serde(deserialize_with = "deserialize_decimal")]
    pub(super) price: Decimal,
    pub(super) trade_ts: DateTime<Utc>,
    pub(super) trade_ts_unix: i64,
    pub(super) transaction_hash: String,
    pub(super) proxy_wallet: String,
    pub(super) source: String,
    pub(super) received_at: DateTime<Utc>,
    pub(super) available_at: DateTime<Utc>,
    pub(super) recorded_at: DateTime<Utc>,
    pub(super) source_sequence: u64,
    pub(super) source_dataset: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawSettlement {
    #[serde(flatten)]
    pub(super) context: RowContext,
    pub(super) source_token_ids: [String; 2],
    pub(super) source_outcomes: [String; 2],
    #[serde(deserialize_with = "deserialize_decimal_pair")]
    pub(super) source_outcome_prices: [Decimal; 2],
    pub(super) winning_token_id: String,
    pub(super) winning_outcome: String,
    pub(super) resolution_source: String,
    pub(super) retrieved_at: DateTime<Utc>,
    pub(super) available_at: DateTime<Utc>,
    pub(super) recorded_at: DateTime<Utc>,
    pub(super) source_sequence: u64,
    pub(super) source_dataset: String,
}

#[derive(Debug)]
pub(super) enum RawRow {
    Contract(RawContract),
    Book(RawBook),
    Reference(RawReference),
    Trade(RawTrade),
    Settlement(RawSettlement),
}

pub(super) fn parse_row(frame: &[u8]) -> Result<RawRow> {
    let value: Value = serde_json::from_slice(frame)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("evidence row must be an object"))?;
    for forbidden in ["outcome", "up_token_id", "down_token_id"] {
        if object.contains_key(forbidden) {
            bail!("source-neutral evidence must not contain {forbidden}");
        }
    }
    if object.get("schema").and_then(Value::as_str) != Some(ROW_SCHEMA) {
        bail!("unsupported evidence row schema");
    }
    let surface = object
        .get("surface")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("evidence row surface must be a string"))?;
    let fields = match surface {
        "market_contract" => &CONTRACT_FIELDS[..],
        "orderbook_snapshot" => &BOOK_FIELDS[..],
        "chainlink_reference" => &REFERENCE_FIELDS[..],
        "polymarket_trade" => &TRADE_FIELDS[..],
        "official_settlement_evidence" => &SETTLEMENT_FIELDS[..],
        _ => bail!("unsupported evidence surface"),
    };
    reject_unknown_fields(object, fields)?;
    Ok(match surface {
        "market_contract" => RawRow::Contract(serde_json::from_value(value)?),
        "orderbook_snapshot" => RawRow::Book(serde_json::from_value(value)?),
        "chainlink_reference" => RawRow::Reference(serde_json::from_value(value)?),
        "polymarket_trade" => RawRow::Trade(serde_json::from_value(value)?),
        "official_settlement_evidence" => RawRow::Settlement(serde_json::from_value(value)?),
        _ => unreachable!("surface was checked above"),
    })
}

fn reject_unknown_fields(object: &Map<String, Value>, fields: &[&str]) -> Result<()> {
    for field in object.keys() {
        if !CONTEXT_FIELDS.contains(&field.as_str()) && !fields.contains(&field.as_str()) {
            bail!("unsupported {field} field in evidence row");
        }
    }
    Ok(())
}

fn decimal_value(value: Value) -> std::result::Result<Decimal, String> {
    let text = match value {
        Value::String(text) => text,
        Value::Number(number) => number.to_string(),
        _ => return Err("decimal must be a JSON string or number".to_owned()),
    };
    Decimal::from_str(&text).map_err(|error| error.to_string())
}

fn deserialize_decimal<'de, D>(deserializer: D) -> std::result::Result<Decimal, D::Error>
where
    D: Deserializer<'de>,
{
    decimal_value(Value::deserialize(deserializer)?).map_err(D::Error::custom)
}

fn deserialize_optional_decimal<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Decimal>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<Value>::deserialize(deserializer)?
        .map(decimal_value)
        .transpose()
        .map_err(D::Error::custom)
}

fn deserialize_decimal_pair<'de, D>(deserializer: D) -> std::result::Result<[Decimal; 2], D::Error>
where
    D: Deserializer<'de>,
{
    let [left, right] = <[Value; 2]>::deserialize(deserializer)?;
    Ok([
        decimal_value(left).map_err(D::Error::custom)?,
        decimal_value(right).map_err(D::Error::custom)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn book_row() -> Value {
        json!({
            "schema": ROW_SCHEMA,
            "surface": "orderbook_snapshot",
            "market_id": "market-1",
            "condition_id": "condition-1",
            "symbol": "BTCUSDT",
            "event_start": "2026-07-17T05:30:00Z",
            "event_end": "2026-07-17T05:35:00Z",
            "window_secs": 300,
            "token_id": "token-1",
            "ts": "2026-07-17T05:30:01Z",
            "recorded_at": "2026-07-17T05:30:02Z",
            "available_at": "2026-07-17T05:30:02Z",
            "source_sequence": 1,
            "source_dataset": "crypto_expiry",
            "bid": "0.4",
            "ask": 0.5,
            "bid_size": "10",
            "ask_size": 11,
            "bid_levels": [{"price":"0.4","size":10}],
            "ask_levels": [{"price":0.5,"size":"11"}]
        })
    }

    #[test]
    fn parses_current_book_row_with_mixed_decimal_encodings() {
        let RawRow::Book(book) = parse_row(&serde_json::to_vec(&book_row()).unwrap()).unwrap()
        else {
            panic!("expected book row");
        };
        assert_eq!(book.bid, Some(Decimal::new(4, 1)));
        assert_eq!(book.ask, Some(Decimal::new(5, 1)));
        assert_eq!(book.bid_levels.unwrap()[0].size, Decimal::TEN);
    }

    #[test]
    fn rejects_stale_schema_derived_or_unknown_fields() {
        let mut row = book_row();
        row["schema"] = json!("monday.polymarket.research_row.v1");
        assert!(parse_row(&serde_json::to_vec(&row).unwrap()).is_err());

        let mut row = book_row();
        row["up_token_id"] = json!("token-1");
        assert!(parse_row(&serde_json::to_vec(&row).unwrap()).is_err());

        let mut row = book_row();
        row["unexpected"] = json!(true);
        assert!(parse_row(&serde_json::to_vec(&row).unwrap()).is_err());
    }
}
