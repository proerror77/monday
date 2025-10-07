//! Aster DEX 消息格式轉換器（Binance 兼容）

use crate::message_types::*;
use hft_core::{HftError, HftResult, Price, Quantity, Side, Symbol, VenueId};
use ports::events::*;
use rust_decimal::Decimal;
use serde::de::DeserializeOwned;
use std::str::FromStr;
use tracing::debug;

pub struct MessageConverter;

impl MessageConverter {
    pub fn convert_depth_update(update: DepthUpdate) -> HftResult<BookUpdate> {
        let symbol = Symbol(update.symbol);
        let bids = Self::convert_price_levels(&update.bids)?;
        let asks = Self::convert_price_levels(&update.asks)?;

        Ok(BookUpdate {
            symbol,
            source_venue: Some(VenueId::ASTERDEX),
            timestamp: update.event_time.saturating_mul(1000),
            bids,
            asks,
            sequence: update.final_update_id,
            is_snapshot: false,
        })
    }

    pub fn convert_depth_snapshot(
        symbol: Symbol,
        snapshot: DepthSnapshot,
        timestamp: u64,
    ) -> HftResult<MarketSnapshot> {
        let bids = Self::convert_price_levels(&snapshot.bids)?;
        let asks = Self::convert_price_levels(&snapshot.asks)?;

        Ok(MarketSnapshot {
            symbol,
            timestamp,
            bids,
            asks,
            sequence: snapshot.last_update_id,
            source_venue: Some(VenueId::ASTERDEX),
        })
    }

    pub fn convert_trade_event(trade: TradeEvent) -> HftResult<Trade> {
        let symbol = Symbol(trade.symbol);
        let price = Self::parse_price(&trade.price)?;
        let quantity = Self::parse_quantity(&trade.quantity)?;

        // 同 Binance：is_buyer_maker = true -> Sell（賣方吃單）
        let side = if trade.is_buyer_maker {
            Side::Sell
        } else {
            Side::Buy
        };

        Ok(Trade {
            symbol,
            timestamp: trade.trade_time.saturating_mul(1000),
            price,
            quantity,
            side,
            trade_id: trade.trade_id.to_string(),
            source_venue: Some(VenueId::ASTERDEX),
        })
    }

    pub fn convert_kline_event(kline_event: KlineEvent) -> HftResult<AggregatedBar> {
        let symbol = Symbol(kline_event.symbol);
        let kline = &kline_event.kline;

        let open = Self::parse_price(&kline.open_price)?;
        let high = Self::parse_price(&kline.high_price)?;
        let low = Self::parse_price(&kline.low_price)?;
        let close = Self::parse_price(&kline.close_price)?;
        let volume = Self::parse_quantity(&kline.volume)?;

        let interval_ms = Self::parse_interval_ms(&kline.interval)?;

        Ok(AggregatedBar {
            symbol,
            interval_ms,
            open_time: kline.start_time.saturating_mul(1000),
            close_time: kline.close_time.saturating_mul(1000),
            open,
            high,
            low,
            close,
            volume,
            trade_count: kline.trade_count,
            source_venue: Some(VenueId::ASTERDEX),
        })
    }

    fn convert_price_levels(levels: &[[String; 2]]) -> HftResult<Vec<BookLevel>> {
        let mut result = Vec::with_capacity(levels.len());
        for level in levels {
            if level[0].is_empty() || level[1].is_empty() {
                continue;
            }
            let price = Self::parse_price(&level[0])?;
            let quantity = Self::parse_quantity(&level[1])?;
            result.push(BookLevel { price, quantity });
        }
        Ok(result)
    }

    fn parse_price(price_str: &str) -> HftResult<Price> {
        Decimal::from_str(price_str)
            .map_err(|e| HftError::Parse(format!("解析價格失敗 '{}': {}", price_str, e)))
            .map(Price)
    }

    fn parse_quantity(qty_str: &str) -> HftResult<Quantity> {
        Decimal::from_str(qty_str)
            .map_err(|e| HftError::Parse(format!("解析數量失敗 '{}': {}", qty_str, e)))
            .map(Quantity)
    }

    fn parse_interval_ms(interval: &str) -> HftResult<u64> {
        match interval {
            "1m" => Ok(60_000),
            "3m" => Ok(180_000),
            "5m" => Ok(300_000),
            "15m" => Ok(900_000),
            "30m" => Ok(1_800_000),
            "1h" => Ok(3_600_000),
            "2h" => Ok(7_200_000),
            "4h" => Ok(14_400_000),
            "6h" => Ok(21_600_000),
            "8h" => Ok(28_800_000),
            "12h" => Ok(43_200_000),
            "1d" => Ok(86_400_000),
            "3d" => Ok(259_200_000),
            "1w" => Ok(604_800_000),
            "1M" => Ok(2_592_000_000),
            _ => Err(HftError::Parse(format!("未知的時間間隔: {}", interval))),
        }
    }

    pub fn parse_stream_message(text: &str) -> HftResult<Option<MarketEvent>> {
        if let Ok(stream_msg) = Self::parse_json::<StreamMessage>(text) {
            return Self::process_stream_data(&stream_msg.stream, &stream_msg.data);
        }
        Self::parse_direct_message(text)
    }

    fn process_stream_data(
        stream: &str,
        data: &serde_json::Value,
    ) -> HftResult<Option<MarketEvent>> {
        if stream.contains("@depth") {
            if let Ok(update) = serde_json::from_value::<DepthUpdate>(data.clone()) {
                let book_update = Self::convert_depth_update(update)?;
                return Ok(Some(MarketEvent::Update(book_update)));
            }
        } else if stream.contains("@trade") {
            if let Ok(trade) = serde_json::from_value::<TradeEvent>(data.clone()) {
                let trade_event = Self::convert_trade_event(trade)?;
                return Ok(Some(MarketEvent::Trade(trade_event)));
            } else if let Ok(trades) = serde_json::from_value::<Vec<TradeEvent>>(data.clone()) {
                if let Some(trade) = trades.first() {
                    let trade_event = Self::convert_trade_event(trade.clone())?;
                    return Ok(Some(MarketEvent::Trade(trade_event)));
                }
            }
        } else if stream.contains("@kline") {
            if let Ok(kline) = serde_json::from_value::<KlineEvent>(data.clone()) {
                let bar_event = Self::convert_kline_event(kline)?;
                return Ok(Some(MarketEvent::Bar(bar_event)));
            }
        }
        debug!("忽略未支援的流: {}", stream);
        Ok(None)
    }

    fn parse_direct_message(text: &str) -> HftResult<Option<MarketEvent>> {
        if let Ok(update) = Self::parse_json::<DepthUpdate>(text) {
            let book_update = Self::convert_depth_update(update)?;
            return Ok(Some(MarketEvent::Update(book_update)));
        }
        if let Ok(trade) = Self::parse_json::<TradeEvent>(text) {
            let trade_event = Self::convert_trade_event(trade)?;
            return Ok(Some(MarketEvent::Trade(trade_event)));
        }
        if let Ok(kline) = Self::parse_json::<KlineEvent>(text) {
            let bar_event = Self::convert_kline_event(kline)?;
            return Ok(Some(MarketEvent::Bar(bar_event)));
        }
        debug!("無法解析的消息: {}", text);
        Ok(None)
    }

    #[inline]
    fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, simd_json::Error> {
        let mut bytes = text.as_bytes().to_vec();
        simd_json::serde::from_slice(bytes.as_mut_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::Side;
    use rust_decimal::Decimal;
    use serde_json::json;

    #[test]
    fn depth_update_parsing_scales_timestamp() {
        let message = json!({
            "stream": "btcusdt@depth",
            "data": {
                "e": "depthUpdate",
                "E": 1_700_000_123,
                "s": "BTCUSDT",
                "U": 100,
                "u": 105,
                "b": [["10000.50", "0.250"]],
                "a": [["10001.00", "0.125"]]
            }
        })
        .to_string();

        let event = MessageConverter::parse_stream_message(&message)
            .expect("parse should succeed")
            .expect("expected depth event");

        match event {
            MarketEvent::Update(book) => {
                assert_eq!(book.timestamp, 1_700_000_123_000);
                assert_eq!(book.sequence, 105);
                assert_eq!(book.symbol.0, "BTCUSDT");
                assert_eq!(book.bids.len(), 1);
                assert_eq!(book.asks.len(), 1);
                assert_eq!(book.bids[0].price.0, Decimal::from_str("10000.50").unwrap());
                assert_eq!(book.asks[0].quantity.0, Decimal::from_str("0.125").unwrap());
            }
            other => panic!("expected book update, got {:?}", other),
        }
    }

    #[test]
    fn trade_event_parsing_scales_timestamp_and_sets_side() {
        let message = json!({
            "stream": "btcusdt@trade",
            "data": {
                "e": "trade",
                "E": 1_700_000_456,
                "s": "BTCUSDT",
                "t": 42,
                "p": "10002.10",
                "q": "0.010",
                "b": 1,
                "a": 2,
                "T": 1_700_000_789,
                "m": true,
                "M": false
            }
        })
        .to_string();

        let event = MessageConverter::parse_stream_message(&message)
            .expect("parse should succeed")
            .expect("expected trade event");

        match event {
            MarketEvent::Trade(trade) => {
                assert_eq!(trade.timestamp, 1_700_000_789_000);
                assert_eq!(trade.symbol.0, "BTCUSDT");
                assert_eq!(trade.side, Side::Sell);
                assert_eq!(trade.price.0, Decimal::from_str("10002.10").unwrap());
                assert_eq!(trade.quantity.0, Decimal::from_str("0.010").unwrap());
            }
            other => panic!("expected trade event, got {:?}", other),
        }
    }
}
