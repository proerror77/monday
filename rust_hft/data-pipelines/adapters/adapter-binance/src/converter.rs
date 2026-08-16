//! Binance 消息格式轉換器
//!
//! 使用統一的 feature-gated JSON 解析接口（adapters_common）
//! - 默認使用 serde_json（穩定、成熟）
//! - 啟用 json-simd feature 時使用 simd-json（2-4x 性能提升）

use crate::message_types::*;
use hft_core::{
    now_micros, ExchangeEventTimestamp, ExchangeTradeTimestamp, HftError, HftResult,
    MarketDataTimestamps, Price, Quantity, Side, Symbol, Timestamp, VenueId,
};
use integration::json::Value;
use ports::events::*;
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Deserialize};
use std::str::FromStr;
use tracing::{debug, warn};

/// Binance 消息轉換器
pub struct MessageConverter;

pub(crate) struct ParsedMarketEvent {
    pub event: MarketEvent,
    pub previous_update_id: Option<u64>,
}

#[derive(Deserialize)]
struct BookTickerStreamMessage {
    data: BookTickerEvent,
}

impl MessageConverter {
    /// 使用共用的 feature-gated JSON 解析
    #[inline]
    fn parse_json<T: DeserializeOwned>(text: &str) -> HftResult<T> {
        adapters_common::parse_json(text).map_err(Into::into)
    }

    #[inline]
    fn parse_bytes<T: DeserializeOwned>(bytes: &mut [u8]) -> HftResult<T> {
        adapters_common::parse_bytes(bytes).map_err(Into::into)
    }

    /// 從 Value 反序列化為目標類型
    #[inline]
    fn parse_value<T: DeserializeOwned>(value: Value) -> HftResult<T> {
        integration::json::from_value(value)
            .map_err(|error| HftError::Serialization(error.to_string()))
    }

    fn millis_to_micros(value: u64, field: &str) -> HftResult<u64> {
        value
            .checked_mul(1_000)
            .ok_or_else(|| HftError::Parse(format!("Binance {field} timestamp overflow")))
    }

    /// 轉換深度快照
    pub fn convert_depth_snapshot(
        symbol: Symbol,
        snapshot: DepthSnapshot,
        timestamp: Timestamp,
    ) -> HftResult<MarketSnapshot> {
        let bids = Self::convert_price_levels(&snapshot.bids)?;
        let asks = Self::convert_price_levels(&snapshot.asks)?;

        Ok(MarketSnapshot {
            symbol,
            timestamp,
            bids,
            asks,
            sequence: snapshot.last_update_id,
            source_venue: Some(VenueId::BINANCE),
            timestamps: MarketDataTimestamps::default(),
        })
    }

    /// 轉換深度更新
    pub fn convert_depth_update(update: DepthUpdate) -> HftResult<BookUpdate> {
        let symbol = Symbol::from(update.symbol);
        let bids = Self::convert_price_levels(&update.bids)?;
        let asks = Self::convert_price_levels(&update.asks)?;
        let exchange_event_time_us = Self::millis_to_micros(update.event_time, "E")?;

        Ok(BookUpdate {
            symbol,
            source_venue: Some(VenueId::BINANCE),
            // Binance 提供毫秒時間戳，統一轉換為微秒
            timestamp: exchange_event_time_us,
            bids,
            asks,
            first_sequence: Some(update.first_update_id),
            sequence: update.final_update_id,
            is_snapshot: false,
            timestamps: MarketDataTimestamps {
                exchange_event: Some(ExchangeEventTimestamp::new(exchange_event_time_us)),
                exchange_trade: None,
                local_receive: None,
            },
        })
    }

    /// 轉換交易事件
    pub fn convert_trade_event(trade: TradeEvent) -> HftResult<Trade> {
        let symbol = Symbol::from(trade.symbol);
        let price = Self::parse_price(&trade.price)?;
        let quantity = Self::parse_quantity(&trade.quantity)?;
        let exchange_event_time_us = Self::millis_to_micros(trade.event_time, "E")?;
        let exchange_trade_time_us = Self::millis_to_micros(trade.trade_time, "T")?;

        // Binance 用 is_buyer_maker 判斷方向
        // 如果買方是掛單方(maker)，則這筆交易是賣方吃單，方向為 Sell
        // 如果買方是吃單方(taker)，則方向為 Buy
        let side = if trade.is_buyer_maker {
            Side::Sell
        } else {
            Side::Buy
        };

        Ok(Trade {
            symbol,
            // 交易時間 (ms) → μs
            timestamp: exchange_trade_time_us,
            price,
            quantity,
            side,
            trade_id: trade.trade_id.to_string(),
            source_venue: Some(VenueId::BINANCE),
            timestamps: MarketDataTimestamps {
                exchange_event: Some(ExchangeEventTimestamp::new(exchange_event_time_us)),
                exchange_trade: Some(ExchangeTradeTimestamp::new(exchange_trade_time_us)),
                local_receive: None,
            },
        })
    }

    /// 轉換 K 線事件
    pub fn convert_kline_event(kline_event: KlineEvent) -> HftResult<AggregatedBar> {
        let symbol = Symbol::from(kline_event.symbol);
        let exchange_event_time_us = Self::millis_to_micros(kline_event.event_time, "kline event")?;
        let kline = &kline_event.kline;

        let open = Self::parse_price(&kline.open_price)?;
        let high = Self::parse_price(&kline.high_price)?;
        let low = Self::parse_price(&kline.low_price)?;
        let close = Self::parse_price(&kline.close_price)?;
        let volume = Self::parse_quantity(&kline.volume)?;

        // 解析間隔為毫秒
        let interval_ms = Self::parse_interval_ms(&kline.interval)?;

        Ok(AggregatedBar {
            symbol,
            interval_ms,
            // K線起訖時間 (ms) → μs
            open_time: Self::millis_to_micros(kline.start_time, "kline open")?,
            close_time: Self::millis_to_micros(kline.close_time, "kline close")?,
            open,
            high,
            low,
            close,
            volume,
            trade_count: kline.trade_count,
            source_venue: Some(VenueId::BINANCE),
            timestamps: MarketDataTimestamps {
                exchange_event: Some(ExchangeEventTimestamp::new(exchange_event_time_us)),
                exchange_trade: None,
                local_receive: None,
            },
        })
    }

    /// 轉換價格檔位數組
    fn convert_price_levels(levels: &[[String; 2]]) -> HftResult<Vec<BookLevel>> {
        let mut result = Vec::with_capacity(levels.len());

        for level in levels {
            if level[0].is_empty() || level[1].is_empty() {
                continue; // 跳過空檔位
            }

            let price = Self::parse_price(&level[0])?;
            let quantity = Self::parse_quantity(&level[1])?;

            // 如果數量為0，這表示該檔位被移除，我們仍然包含它以便處理
            result.push(BookLevel { price, quantity });
        }

        Ok(result)
    }

    /// 解析價格字符串
    fn parse_price(price_str: &str) -> HftResult<Price> {
        Decimal::from_str(price_str)
            .map_err(|e| HftError::Parse(format!("解析價格失敗 '{}': {}", price_str, e)))
            .map(Price)
    }

    /// 解析數量字符串
    fn parse_quantity(qty_str: &str) -> HftResult<Quantity> {
        Decimal::from_str(qty_str)
            .map_err(|e| HftError::Parse(format!("解析數量失敗 '{}': {}", qty_str, e)))
            .map(Quantity)
    }

    /// 解析間隔字符串為毫秒
    fn parse_interval_ms(interval: &str) -> HftResult<u64> {
        match interval {
            "1m" => Ok(60 * 1000),
            "3m" => Ok(3 * 60 * 1000),
            "5m" => Ok(5 * 60 * 1000),
            "15m" => Ok(15 * 60 * 1000),
            "30m" => Ok(30 * 60 * 1000),
            "1h" => Ok(60 * 60 * 1000),
            "2h" => Ok(2 * 60 * 60 * 1000),
            "4h" => Ok(4 * 60 * 60 * 1000),
            "6h" => Ok(6 * 60 * 60 * 1000),
            "8h" => Ok(8 * 60 * 60 * 1000),
            "12h" => Ok(12 * 60 * 60 * 1000),
            "1d" => Ok(24 * 60 * 60 * 1000),
            "3d" => Ok(3 * 24 * 60 * 60 * 1000),
            "1w" => Ok(7 * 24 * 60 * 60 * 1000),
            "1M" => Ok(30 * 24 * 60 * 60 * 1000), // 近似值
            _ => Err(HftError::Parse(format!("未知的時間間隔: {}", interval))),
        }
    }

    /// 檢測並解析流消息
    pub fn parse_stream_message(text: &str) -> HftResult<Option<MarketEvent>> {
        // 首先嘗試解析為流消息
        if let Ok(stream_msg) = Self::parse_json::<StreamMessage>(text) {
            return Self::process_stream_data(&stream_msg.stream, &stream_msg.data);
        }

        // 然後嘗試直接解析為各種事件類型
        Self::parse_direct_message(text)
    }

    /// Parse the combined-stream envelope directly from the mutable WebSocket frame buffer.
    pub fn parse_stream_message_bytes(bytes: &mut [u8]) -> HftResult<Option<MarketEvent>> {
        Self::parse_stream_message_bytes_with_metadata(bytes)
            .map(|parsed| parsed.map(|parsed| parsed.event))
    }

    pub(crate) fn parse_stream_message_bytes_with_metadata(
        bytes: &mut [u8],
    ) -> HftResult<Option<ParsedMarketEvent>> {
        const BOOK_TICKER_MARKER: &[u8] = b"@bookTicker";
        if bytes
            .windows(BOOK_TICKER_MARKER.len())
            .any(|window| window == BOOK_TICKER_MARKER)
        {
            let envelope: BookTickerStreamMessage = serde_json::from_slice(bytes)
                .map_err(|error| HftError::Serialization(error.to_string()))?;
            return Self::convert_book_ticker_event(envelope.data)
                .map(MarketEvent::Quote)
                .map(|event| ParsedMarketEvent {
                    event,
                    previous_update_id: None,
                })
                .map(Some);
        }
        let stream_msg = Self::parse_bytes::<StreamMessage>(bytes)?;
        if stream_msg.stream.contains("@depth") {
            if let Ok(update) = Self::parse_value::<DepthUpdate>(stream_msg.data.clone()) {
                let previous_update_id = update.previous_final_update_id;
                return Self::convert_depth_update(update)
                    .map(MarketEvent::Update)
                    .map(|event| ParsedMarketEvent {
                        event,
                        previous_update_id,
                    })
                    .map(Some);
            }
        }
        Self::process_stream_data(&stream_msg.stream, &stream_msg.data).map(|event| {
            event.map(|event| ParsedMarketEvent {
                event,
                previous_update_id: None,
            })
        })
    }

    /// 處理流數據
    ///
    /// 使用統一的 Value 類型（根據 json-simd feature 自動切換）
    fn process_stream_data(stream: &str, data: &Value) -> HftResult<Option<MarketEvent>> {
        if stream == "!serverShutdown" {
            return Err(HftError::Network(
                "Binance announced WebSocket server shutdown; reconnect immediately".to_string(),
            ));
        } else if stream.contains("@depth") {
            if let Ok(update) = Self::parse_value::<DepthUpdate>(data.clone()) {
                let book_update = Self::convert_depth_update(update)?;
                return Ok(Some(MarketEvent::Update(book_update)));
            }
            if let Ok(snapshot) = Self::parse_value::<DepthSnapshot>(data.clone()) {
                let symbol = stream
                    .split('@')
                    .next()
                    .filter(|symbol| !symbol.is_empty())
                    .ok_or_else(|| {
                        HftError::Parse("Binance depth stream has no symbol".to_string())
                    })?
                    .to_ascii_uppercase();
                return Self::convert_depth_snapshot(Symbol::from(symbol), snapshot, now_micros())
                    .map(MarketEvent::Snapshot)
                    .map(Some);
            }
        } else if stream.contains("@trade") {
            if let Ok(trade) = Self::parse_value::<TradeEvent>(data.clone()) {
                let trade_event = Self::convert_trade_event(trade)?;
                return Ok(Some(MarketEvent::Trade(trade_event)));
            }
        } else if stream.contains("bookTicker") {
            if let Ok(bt) = Self::parse_value::<BookTickerEvent>(data.clone()) {
                let quote = Self::convert_book_ticker_event(bt)?;
                return Ok(Some(MarketEvent::Quote(quote)));
            }
        } else if stream.contains("@kline") {
            if let Ok(kline) = Self::parse_value::<KlineEvent>(data.clone()) {
                let bar_event = Self::convert_kline_event(kline)?;
                return Ok(Some(MarketEvent::Bar(bar_event)));
            }
        }

        warn!("未知的流類型: {}", stream);
        Ok(None)
    }

    /// 解析直接消息
    fn parse_direct_message(text: &str) -> HftResult<Option<MarketEvent>> {
        // 嘗試解析為深度更新
        if let Ok(update) = Self::parse_json::<DepthUpdate>(text) {
            let book_update = Self::convert_depth_update(update)?;
            return Ok(Some(MarketEvent::Update(book_update)));
        }

        // 嘗試解析為交易事件
        if let Ok(trade) = Self::parse_json::<TradeEvent>(text) {
            let trade_event = Self::convert_trade_event(trade)?;
            return Ok(Some(MarketEvent::Trade(trade_event)));
        }

        // 嘗試解析為 bookTicker
        if let Ok(bt) = Self::parse_json::<BookTickerEvent>(text) {
            let quote = Self::convert_book_ticker_event(bt)?;
            return Ok(Some(MarketEvent::Quote(quote)));
        }

        // 嘗試解析為 K 線事件
        if let Ok(kline) = Self::parse_json::<KlineEvent>(text) {
            let bar_event = Self::convert_kline_event(kline)?;
            return Ok(Some(MarketEvent::Bar(bar_event)));
        }

        // 如果都無法解析，返回 None
        debug!("無法解析的消息: {}", text);
        Ok(None)
    }
}

impl MessageConverter {
    fn convert_book_ticker_level(
        side: &str,
        price: String,
        quantity: String,
    ) -> HftResult<BookLevel> {
        if price.trim().is_empty() || quantity.trim().is_empty() {
            return Err(HftError::Parse(format!(
                "Binance bookTicker {side} is missing price or quantity"
            )));
        }
        let level = BookLevel {
            price: Self::parse_price(&price).map_err(|error| {
                HftError::Parse(format!("Binance bookTicker {side} price: {error}"))
            })?,
            quantity: Self::parse_quantity(&quantity).map_err(|error| {
                HftError::Parse(format!("Binance bookTicker {side} quantity: {error}"))
            })?,
        };
        if level.price <= Price::zero() || level.quantity <= Quantity::zero() {
            return Err(HftError::Parse(format!(
                "Binance bookTicker {side} must be positive"
            )));
        }
        Ok(level)
    }

    pub fn convert_book_ticker_event(bt: BookTickerEvent) -> HftResult<TopOfBook> {
        let symbol = Symbol::from(bt.symbol);
        let bid = Self::convert_book_ticker_level("bid", bt.best_bid_price, bt.best_bid_qty)?;
        let ask = Self::convert_book_ticker_level("ask", bt.best_ask_price, bt.best_ask_qty)?;
        if bid.price >= ask.price {
            return Err(HftError::Parse(format!(
                "crossed Binance bookTicker for {}",
                symbol.as_str()
            )));
        }
        Ok(TopOfBook {
            symbol,
            // Spot bookTicker has no exchange timestamp; stamp the frame at the adapter boundary.
            timestamp: now_micros(),
            sequence: bt.update_id,
            bid,
            ask,
            source_venue: Some(VenueId::BINANCE),
            timestamps: MarketDataTimestamps::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_price() {
        let price = MessageConverter::parse_price("45123.45").unwrap();
        assert_eq!(price.to_string(), "45123.45");
    }

    #[test]
    fn test_parse_quantity() {
        let qty = MessageConverter::parse_quantity("0.123456").unwrap();
        assert_eq!(qty.to_string(), "0.123456");
    }

    #[test]
    fn test_parse_interval_ms() {
        assert_eq!(MessageConverter::parse_interval_ms("1m").unwrap(), 60000);
        assert_eq!(MessageConverter::parse_interval_ms("1h").unwrap(), 3600000);
        assert_eq!(MessageConverter::parse_interval_ms("1d").unwrap(), 86400000);
    }

    #[test]
    fn test_convert_depth_update() {
        let update = DepthUpdate {
            _event_type: "depthUpdate".to_string(),
            event_time: 123456789,
            symbol: "BTCUSDT".to_string(),
            first_update_id: 100,
            final_update_id: 101,
            previous_final_update_id: None,
            bids: vec![["45000.00".to_string(), "0.1".to_string()]],
            asks: vec![["45100.00".to_string(), "0.2".to_string()]],
        };

        let book_update = MessageConverter::convert_depth_update(update).unwrap();
        assert_eq!(book_update.symbol.to_string(), "BTCUSDT");
        assert_eq!(book_update.sequence, 101);
        assert_eq!(book_update.first_sequence, Some(100));
        assert!(!book_update.is_snapshot);
        assert_eq!(book_update.bids.len(), 1);
        assert_eq!(book_update.asks.len(), 1);
        // ms → μs 轉換
        assert_eq!(book_update.timestamp, 123456789 * 1000);
    }

    #[test]
    fn test_convert_trade_event() {
        let trade = TradeEvent {
            _event_type: "trade".to_string(),
            event_time: 123456789,
            symbol: "BTCUSDT".to_string(),
            trade_id: 12345,
            price: "45000.00".to_string(),
            quantity: "0.1".to_string(),
            trade_time: 123456789,
            is_buyer_maker: false,
        };

        let trade_event = MessageConverter::convert_trade_event(trade).unwrap();
        assert_eq!(trade_event.symbol.to_string(), "BTCUSDT");
        assert_eq!(trade_event.side, Side::Buy); // is_buyer_maker=false 表示買方吃單
        assert_eq!(trade_event.trade_id, "12345");
        // ms → μs 轉換
        assert_eq!(trade_event.timestamp, 123456789 * 1000);
    }

    #[test]
    fn timestamp_conversion_rejects_millisecond_overflow() {
        let update = DepthUpdate {
            _event_type: "depthUpdate".to_string(),
            event_time: u64::MAX,
            symbol: "BTCUSDT".to_string(),
            first_update_id: 100,
            final_update_id: 101,
            previous_final_update_id: None,
            bids: vec![],
            asks: vec![],
        };

        assert!(MessageConverter::convert_depth_update(update).is_err());
    }

    #[test]
    fn test_parse_wrapped_stream_message() {
        let message = r#"{
            "stream":"btcusdt@trade",
            "data":{
                "e":"trade","E":123456789,"s":"BTCUSDT","t":12345,
                "p":"45000.00","q":"0.1","b":111,"a":222,
                "T":123456789,"m":false,"M":false
            }
        }"#;

        let event = MessageConverter::parse_stream_message(message)
            .unwrap()
            .expect("trade event");
        assert!(matches!(event, MarketEvent::Trade(_)));
    }

    #[test]
    fn current_trade_payload_does_not_require_legacy_order_ids() {
        let message = r#"{
            "stream":"btcusdt@trade",
            "data":{
                "e":"trade","E":123456789,"s":"BTCUSDT","t":12345,
                "p":"45000.00","q":"0.1","T":123456789,"m":false
            }
        }"#;

        let event = MessageConverter::parse_stream_message(message)
            .unwrap()
            .expect("trade event");
        assert!(matches!(event, MarketEvent::Trade(_)));
    }

    #[test]
    fn test_parse_wrapped_stream_message_from_mutable_bytes() {
        let mut message = br#"{"stream":"btcusdt@depth","data":{"e":"depthUpdate","E":123456789,"s":"BTCUSDT","U":100,"u":101,"b":[["45000.00","0.1"]],"a":[["45100.00","0.2"]]}}"#.to_vec();

        let event = MessageConverter::parse_stream_message_bytes(&mut message)
            .unwrap()
            .expect("depth event");
        assert!(matches!(event, MarketEvent::Update(_)));
    }

    #[test]
    fn partial_depth_stream_is_a_ws_only_snapshot() {
        let mut message = br#"{"stream":"btcusdt@depth20@100ms","data":{"lastUpdateId":101,"bids":[["45000.00","0.1"]],"asks":[["45100.00","0.2"]]}}"#.to_vec();

        let event = MessageConverter::parse_stream_message_bytes(&mut message)
            .unwrap()
            .expect("depth snapshot");
        let MarketEvent::Snapshot(snapshot) = event else {
            panic!("expected websocket depth snapshot");
        };
        assert_eq!(snapshot.symbol, Symbol::new("BTCUSDT"));
        assert_eq!(snapshot.sequence, 101);
        assert_eq!(snapshot.bids.len(), 1);
    }

    #[test]
    fn book_ticker_is_a_sequence_tagged_quote_not_an_l2_delta() {
        let mut message = br#"{"stream":"btcusdt@bookTicker","data":{"u":400900217,"s":"BTCUSDT","b":"25.35190000","B":"31.21000000","a":"25.36520000","A":"40.66000000"}}"#.to_vec();

        let event = MessageConverter::parse_stream_message_bytes(&mut message)
            .unwrap()
            .expect("book ticker quote");
        let MarketEvent::Quote(quote) = event else {
            panic!("expected top-of-book quote");
        };
        assert_eq!(quote.symbol, Symbol::new("BTCUSDT"));
        assert_eq!(quote.sequence, 400900217);
        assert_eq!(quote.bid.price.to_string(), "25.35190000");
        assert_eq!(quote.ask.quantity.to_string(), "40.66000000");
    }

    #[test]
    fn malformed_book_ticker_fails_closed_instead_of_panicking() {
        let ticker = BookTickerEvent {
            update_id: 400900217,
            symbol: "BTCUSDT".to_string(),
            best_bid_price: String::new(),
            best_bid_qty: "31.21".to_string(),
            best_ask_price: "25.3652".to_string(),
            best_ask_qty: "40.66".to_string(),
        };

        assert!(matches!(
            MessageConverter::convert_book_ticker_event(ticker),
            Err(HftError::Parse(message)) if message.contains("bookTicker bid")
        ));
    }

    #[test]
    fn server_shutdown_event_forces_immediate_reconnect() {
        let mut message =
            br#"{"stream":"!serverShutdown","data":{"e":"serverShutdown","E":1770123456789}}"#
                .to_vec();

        let error = MessageConverter::parse_stream_message_bytes(&mut message).unwrap_err();
        assert!(matches!(error, HftError::Network(message) if message.contains("server shutdown")));
    }
}
