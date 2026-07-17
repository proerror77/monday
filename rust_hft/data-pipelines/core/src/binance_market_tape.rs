//! Shared contract for the immutable Binance market tape.

use std::collections::HashMap;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde_json::{Map, Value};

pub const LEGACY_LOB_TAPE_SCHEMA: &str = "binance.lob_tape.v2";
pub const MARKET_TAPE_SCHEMA: &str = "binance.market_tape.v1";
pub const MAX_SOURCE_DELAY_MS: u64 = 30_000;

pub fn supported_schema(schema: &str) -> bool {
    matches!(schema, LEGACY_LOB_TAPE_SCHEMA | MARKET_TAPE_SCHEMA)
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
        MARKET_TAPE_SCHEMA => matches!(
            event_type,
            "session_start"
                | "snapshot"
                | "diff"
                | "checkpoint"
                | "sequence_gap"
                | "symbol_excluded"
                | "agg_trade"
                | "aggregate_trade_gap"
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
        validate_receive_clock(event_time_ms, received_at_ns, "depth")?;
        if let Some(transaction_time_ms) = transaction_time_ms {
            validate_receive_clock(transaction_time_ms, received_at_ns, "depth")?;
        }
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

impl DepthSourceClockSequenceValidator {
    pub fn observe(&mut self, clock: &DepthSourceClock) -> Result<()> {
        if let Some((previous_event_time, previous_transaction_time, previous_final_update_id)) =
            self.last.get(&clock.symbol).copied()
        {
            if let Some(reported_previous_id) = clock.previous_final_update_id {
                if reported_previous_id > previous_final_update_id {
                    anyhow::bail!("{} depth previous-update gap", clock.symbol);
                }
                if reported_previous_id < previous_final_update_id {
                    anyhow::bail!("{} depth previous-update rollback", clock.symbol);
                }
            }
            let expected_update_id = previous_final_update_id
                .checked_add(1)
                .context("depth update id overflow")?;
            if clock.first_update_id > expected_update_id {
                anyhow::bail!(
                    "{} depth sequence gap expected={} received={}",
                    clock.symbol,
                    expected_update_id,
                    clock.first_update_id
                );
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
    pub received_at_ns: u64,
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
        let price = positive_decimal(data, "p")?;
        let quantity = positive_decimal(data, "q")?;
        let event_time_ms = required_u64(data, "E", "aggregate trade")?;
        let trade_time_ms = required_u64(data, "T", "aggregate trade")?;
        let is_buyer_maker = data
            .get("m")
            .and_then(Value::as_bool)
            .context("aggregate trade maker side is missing")?;
        if trade_time_ms > event_time_ms {
            anyhow::bail!("aggregate trade source clocks are reversed");
        }
        validate_receive_clock(event_time_ms, received_at_ns, "aggregate trade")?;
        validate_receive_clock(trade_time_ms, received_at_ns, "aggregate trade")?;
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

fn validate_receive_clock(event_time_ms: u64, received_at_ns: u64, kind: &str) -> Result<()> {
    let received_at_ms = received_at_ns / 1_000_000;
    if event_time_ms > received_at_ms {
        anyhow::bail!("{kind} source and receive clocks are reversed");
    }
    if received_at_ms - event_time_ms > MAX_SOURCE_DELAY_MS {
        anyhow::bail!("{kind} source-to-receive delay exceeds the governed limit");
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

fn positive_decimal(value: &Value, field: &str) -> Result<Decimal> {
    let decimal = required_string(value, field, "aggregate trade")?
        .parse::<Decimal>()
        .with_context(|| format!("aggregate trade field {field} is not decimal"))?;
    if decimal <= Decimal::ZERO {
        anyhow::bail!("aggregate trade field {field} is not positive");
    }
    Ok(decimal)
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
        assert!(sequence
            .observe(&gap)
            .unwrap_err()
            .to_string()
            .contains("gap"));
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
        assert!(gap_sequence
            .observe(&previous_id_ahead)
            .unwrap_err()
            .to_string()
            .contains("gap"));

        let mut rollback_sequence = DepthSourceClockSequenceValidator::default();
        rollback_sequence.observe(&first).unwrap();
        assert!(rollback_sequence
            .observe(&previous_id_behind)
            .unwrap_err()
            .to_string()
            .contains("rollback"));
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
            &depth_frame(1_700_000_000_101, None),
            received_at_ns,
        )
        .unwrap_err()
        .to_string()
        .contains("reversed"));
        assert!(DepthSourceClock::from_frame(
            &depth_frame(1_700_000_000_000, None),
            1_700_000_031_000_000_000,
        )
        .unwrap_err()
        .to_string()
        .contains("delay"));
    }

    #[test]
    fn depth_clock_rejects_stale_transaction_time_when_event_time_is_fresh() {
        assert!(DepthSourceClock::from_frame(
            &depth_frame(1_700_000_031_000, Some(1_700_000_000_999)),
            1_700_000_031_000_000_000,
        )
        .unwrap_err()
        .to_string()
        .contains("delay"));
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
        assert!(AggregateTrade::from_frame(
            &frame(1, 1_700_000_031_000, 1_700_000_000_999),
            1_700_000_031_000_000_000,
        )
        .unwrap_err()
        .to_string()
        .contains("delay"));
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
