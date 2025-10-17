//! Binance 消息格式轉換器

use crate::message_types::*;
use hft_core::{HftError, HftResult, Price, Quantity, Side, Symbol, Timestamp, VenueId};
use ports::events::*;
use rust_decimal::Decimal;
use serde::de::DeserializeOwned;
use std::str::FromStr;
use tracing::{debug, warn};

/// Binance 消息轉換器
pub struct MessageConverter;

impl MessageConverter {
    #[inline]
    fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, simd_json::Error> {
        let mut bytes = text.as_bytes().to_vec();
        simd_json::serde::from_slice(bytes.as_mut_slice())
    }

    #[inline]
    fn parse_value<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, simd_json::Error> {
        let owned: simd_json::OwnedValue = value.try_into()?;
        simd_json::serde::from_owned_value(owned)
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
        })
    }

    /// 轉換深度更新
    pub fn convert_depth_update(update: DepthUpdate) -> HftResult<BookUpdate> {
        let symbol = Symbol::from(update.symbol);
        let bids = Self::convert_price_levels(&update.bids)?;
        let asks = Self::convert_price_levels(&update.asks)?;

        Ok(BookUpdate {
            symbol,
            source_venue: Some(VenueId::BINANCE),
            // Binance 提供毫秒時間戳，統一轉換為微秒
            timestamp: update.event_time * 1000,
            bids,
            asks,
            sequence: update.final_update_id,
            is_snapshot: false,
        })
    }

    /// 轉換交易事件
    pub fn convert_trade_event(trade: TradeEvent) -> HftResult<Trade> {
        let symbol = Symbol::from(trade.symbol);
        let price = Self::parse_price(&trade.price)?;
        let quantity = Self::parse_quantity(&trade.quantity)?;

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
            timestamp: trade.trade_time * 1000,
            price,
            quantity,
            side,
            trade_id: trade.trade_id.to_string(),
            source_venue: Some(VenueId::BINANCE),
        })
    }

    /// 轉換 K 線事件
    pub fn convert_kline_event(kline_event: KlineEvent) -> HftResult<AggregatedBar> {
        let symbol = Symbol::from(kline_event.symbol);
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
            open_time: kline.start_time * 1000,
            close_time: kline.close_time * 1000,
            open,
            high,
            low,
            close,
            volume,
            trade_count: kline.trade_count,
            source_venue: Some(VenueId::BINANCE),
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

    /// 處理流數據
    fn process_stream_data(
        stream: &str,
        data: &serde_json::Value,
    ) -> HftResult<Option<MarketEvent>> {
        if stream.contains("@depth") {
            if let Ok(update) = Self::parse_value::<DepthUpdate>(data.clone()) {
                let book_update = Self::convert_depth_update(update)?;
                return Ok(Some(MarketEvent::Update(book_update)));
            }
        } else if stream.contains("@trade") {
            if let Ok(trade) = Self::parse_value::<TradeEvent>(data.clone()) {
                let trade_event = Self::convert_trade_event(trade)?;
                return Ok(Some(MarketEvent::Trade(trade_event)));
            }
        } else if stream.contains("bookTicker") {
            if let Ok(bt) = Self::parse_value::<BookTickerEvent>(data.clone()) {
                let upd = Self::convert_book_ticker_event(bt)?;
                return Ok(Some(MarketEvent::Update(upd)));
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
            let upd = Self::convert_book_ticker_event(bt)?;
            return Ok(Some(MarketEvent::Update(upd)));
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
    pub fn convert_book_ticker_event(bt: BookTickerEvent) -> HftResult<BookUpdate> {
        let symbol = Symbol::from(bt.symbol);
        let bid = [bt.best_bid_price, bt.best_bid_qty];
        let ask = [bt.best_ask_price, bt.best_ask_qty];
        let bids = Self::convert_price_levels(&[bid])?;
        let asks = Self::convert_price_levels(&[ask])?;
        Ok(BookUpdate {
            symbol,
            // bookTicker 事件時間 (ms) → μs
            timestamp: bt.event_time * 1000,
            bids,
            asks,
            sequence: 0,
            is_snapshot: false,
            source_venue: Some(VenueId::BINANCE),
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
            event_type: "depthUpdate".to_string(),
            event_time: 123456789,
            symbol: "BTCUSDT".to_string(),
            first_update_id: 100,
            final_update_id: 101,
            bids: vec![["45000.00".to_string(), "0.1".to_string()]],
            asks: vec![["45100.00".to_string(), "0.2".to_string()]],
        };

        let book_update = MessageConverter::convert_depth_update(update).unwrap();
        assert_eq!(book_update.symbol.to_string(), "BTCUSDT");
        assert_eq!(book_update.sequence, 101);
        assert!(!book_update.is_snapshot);
        assert_eq!(book_update.bids.len(), 1);
        assert_eq!(book_update.asks.len(), 1);
        // ms → μs 轉換
        assert_eq!(book_update.timestamp, 123456789 * 1000);
    }

    #[test]
    fn test_convert_trade_event() {
        let trade = TradeEvent {
            event_type: "trade".to_string(),
            event_time: 123456789,
            symbol: "BTCUSDT".to_string(),
            trade_id: 12345,
            price: "45000.00".to_string(),
            quantity: "0.1".to_string(),
            buyer_order_id: 111,
            seller_order_id: 222,
            trade_time: 123456789,
            is_buyer_maker: false,
            ignore: false,
        };

        let trade_event = MessageConverter::convert_trade_event(trade).unwrap();
        assert_eq!(trade_event.symbol.to_string(), "BTCUSDT");
        assert_eq!(trade_event.side, Side::Buy); // is_buyer_maker=false 表示買方吃單
        assert_eq!(trade_event.trade_id, "12345");
        // ms → μs 轉換
        assert_eq!(trade_event.timestamp, 123456789 * 1000);
    }
}
