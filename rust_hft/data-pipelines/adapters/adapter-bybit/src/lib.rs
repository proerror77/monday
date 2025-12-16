//! Bybit v5 市場數據適配器（WS 公共流）

use async_trait::async_trait;
use hft_core::{HftError, HftResult, Price, Quantity, Symbol, VenueId};
use integration::ws::{WsClient, WsClientConfig};
use ports::{BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot, MarketStream, Trade};
use serde::{de::DeserializeOwned, Deserialize};
use std::collections::{BTreeMap, HashMap};
use tokio::sync::mpsc;
use tracing::{error, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Spot,
    Linear,
}

impl Category {
    fn from_env() -> Self {
        match std::env::var("BYBIT_CATEGORY")
            .unwrap_or_else(|_| "spot".to_string())
            .to_lowercase()
            .as_str()
        {
            "linear" | "usdt" | "perp" => Category::Linear,
            _ => Category::Spot,
        }
    }
    fn ws_base(&self) -> &'static str {
        match self {
            Category::Spot => "wss://stream.bybit.com/v5/public/spot",
            Category::Linear => "wss://stream.bybit.com/v5/public/linear",
        }
    }
}

#[derive(Debug, Deserialize)]
struct BybitWsMsg {
    topic: Option<String>,
    #[serde(rename = "type")]
    ty: Option<String>,
    data: Option<serde_json::Value>,
}

/// 使用共用的 JSON 解析函數
#[inline]
fn parse_json<T: DeserializeOwned>(text: &str) -> hft_core::HftResult<T> {
    adapters_common::parse_json(text).map_err(Into::into)
}

#[derive(Default)]
struct OrderBookState {
    bids: BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>,
    asks: BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>,
    seq: u64,
}

impl OrderBookState {
    fn apply_snapshot(&mut self, bids: &Vec<[String; 2]>, asks: &Vec<[String; 2]>, seq: u64) {
        self.bids.clear();
        self.asks.clear();
        self.ingest(bids, true);
        self.ingest(asks, false);
        self.seq = seq;
    }
    fn apply_delta(&mut self, bids: &Vec<[String; 2]>, asks: &Vec<[String; 2]>, seq: u64) {
        self.ingest(bids, true);
        self.ingest(asks, false);
        self.seq = seq;
    }
    fn ingest(&mut self, levels: &Vec<[String; 2]>, is_bid: bool) {
        for lvl in levels {
            if lvl[0].is_empty() || lvl[1].is_empty() {
                continue;
            }
            let px = rust_decimal::Decimal::from_str_exact(&lvl[0])
                .or_else(|_| {
                    lvl[0].parse::<f64>().map(|v| {
                        rust_decimal::Decimal::try_from(v).unwrap_or(rust_decimal::Decimal::ZERO)
                    })
                })
                .unwrap_or(rust_decimal::Decimal::ZERO);
            let sz = rust_decimal::Decimal::from_str_exact(&lvl[1])
                .or_else(|_| {
                    lvl[1].parse::<f64>().map(|v| {
                        rust_decimal::Decimal::try_from(v).unwrap_or(rust_decimal::Decimal::ZERO)
                    })
                })
                .unwrap_or(rust_decimal::Decimal::ZERO);
            if px.is_zero() {
                continue;
            }
            let book = if is_bid {
                &mut self.bids
            } else {
                &mut self.asks
            };
            if sz.is_zero() {
                book.remove(&px);
            } else {
                book.insert(px, sz);
            }
        }
    }
    fn to_snapshot(&self, symbol: Symbol, ts_ms: u64) -> MarketSnapshot {
        let mut bids = Vec::with_capacity(self.bids.len());
        for (p, q) in self.bids.iter().rev() {
            bids.push(ports::BookLevel {
                price: Price(*p),
                quantity: Quantity(*q),
            });
        }
        let mut asks = Vec::with_capacity(self.asks.len());
        for (p, q) in self.asks.iter() {
            asks.push(ports::BookLevel {
                price: Price(*p),
                quantity: Quantity(*q),
            });
        }
        MarketSnapshot {
            symbol,
            timestamp: ts_ms.saturating_mul(1000),
            bids,
            asks,
            sequence: self.seq,
            source_venue: Some(VenueId::BYBIT),
        }
    }
}

pub struct BybitMarketStream;

impl Default for BybitMarketStream {
    fn default() -> Self {
        Self::new()
    }
}

impl BybitMarketStream {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MarketStream for BybitMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        if symbols.is_empty() {
            return Err(HftError::new("Bybit symbols cannot be empty"));
        }
        let (tx, mut rx) = mpsc::unbounded_channel();
        let category = Category::from_env();
        let levels: usize = std::env::var("BYBIT_DEPTH_LEVELS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(50);

        tokio::spawn(async move {
            let mut attempts = 0u32;
            loop {
                let url = category.ws_base();
                let config = WsClientConfig {
                    url: url.to_string(),
                    ..Default::default()
                };
                let mut ws = WsClient::new(config);

                match ws.connect().await {
                    Ok(()) => {
                        // build subscribe args
                        let args: Vec<String> = symbols
                            .iter()
                            .flat_map(|s| {
                                vec![
                                    format!("orderbook.{}.{}", levels, s.as_str()),
                                    format!("publicTrade.{}", s.as_str()),
                                ]
                            })
                            .collect();
                        let sub = serde_json::json!({"op":"subscribe","args":args});
                        let _ = ws.send_message(&sub.to_string()).await;

                        let mut ob_states: HashMap<String, OrderBookState> = HashMap::new();
                        loop {
                            match ws.receive_message().await {
                                Ok(Some((txt, _metrics))) => {
                                    if let Ok(m) = parse_json::<BybitWsMsg>(&txt) {
                                        if let Some(topic) = m.topic.as_deref() {
                                            if topic.starts_with("orderbook.") {
                                                if let Some(data) = m.data {
                                                    // data 可能是 object 或 array，統一處理為 object
                                                    if let Some(obj) = data.as_object() {
                                                        let sym = obj
                                                            .get("s")
                                                            .and_then(|v| v.as_str())
                                                            .unwrap_or("")
                                                            .to_string();
                                                        let ts = obj
                                                            .get("ts")
                                                            .and_then(|v| v.as_i64())
                                                            .unwrap_or(0)
                                                            as u64;
                                                        let b = obj
                                                            .get("b")
                                                            .and_then(|v| v.as_array())
                                                            .cloned()
                                                            .unwrap_or_default();
                                                        let a = obj
                                                            .get("a")
                                                            .and_then(|v| v.as_array())
                                                            .cloned()
                                                            .unwrap_or_default();
                                                        // 轉換到 [String;2]
                                                        let to_pairs = |arr: Vec<serde_json::Value>| -> Vec<[String;2]> {
                                                            arr.into_iter().filter_map(|v|{
                                                                if let Some(pair)=v.as_array(){
                                                                    if pair.len()>=2 { Some([pair[0].as_str().unwrap_or("").to_string(), pair[1].as_str().unwrap_or("").to_string()]) } else { None }
                                                                } else { None }
                                                            }).collect()
                                                        };
                                                        let bids = to_pairs(b);
                                                        let asks = to_pairs(a);
                                                        let typ =
                                                            m.ty.as_deref().unwrap_or("delta");
                                                        let state = ob_states
                                                            .entry(sym.clone())
                                                            .or_default();
                                                        if typ == "snapshot" {
                                                            state.apply_snapshot(&bids, &asks, 0);
                                                        } else {
                                                            state.apply_delta(&bids, &asks, 0);
                                                        }
                                                        let snap = state
                                                            .to_snapshot(Symbol::from(sym), ts);
                                                        let _ = tx
                                                            .send(Ok(MarketEvent::Snapshot(snap)));
                                                    }
                                                }
                                            } else if topic.starts_with("publicTrade.") {
                                                if let Some(data) = m.data {
                                                    if let Some(arr) = data.as_array() {
                                                        for t in arr {
                                                            let sym = t
                                                                .get("s")
                                                                .and_then(|v| v.as_str())
                                                                .unwrap_or("");
                                                            let price = t
                                                                .get("p")
                                                                .and_then(|v| v.as_str())
                                                                .and_then(|s| {
                                                                    s.parse::<f64>().ok()
                                                                });
                                                            let qty = t
                                                                .get("q")
                                                                .and_then(|v| v.as_str())
                                                                .and_then(|s| {
                                                                    s.parse::<f64>().ok()
                                                                });
                                                            let side = t
                                                                .get("S")
                                                                .and_then(|v| v.as_str())
                                                                .unwrap_or("Buy");
                                                            let ts = t
                                                                .get("T")
                                                                .and_then(|v| v.as_i64())
                                                                .unwrap_or(0)
                                                                as u64;
                                                            let id = t
                                                                .get("i")
                                                                .and_then(|v| v.as_str())
                                                                .unwrap_or("");
                                                            if let (Some(p), Some(q)) = (price, qty)
                                                            {
                                                                let trade = Trade {
                                                                    symbol: Symbol::from(
                                                                        sym.to_string(),
                                                                    ),
                                                                    timestamp: ts
                                                                        .saturating_mul(1000),
                                                                    price: Price::from_f64(p)
                                                                        .unwrap(),
                                                                    quantity: Quantity::from_f64(q)
                                                                        .unwrap(),
                                                                    side: if side
                                                                        .to_ascii_lowercase()
                                                                        .starts_with('b')
                                                                    {
                                                                        hft_core::Side::Buy
                                                                    } else {
                                                                        hft_core::Side::Sell
                                                                    },
                                                                    trade_id: id.to_string(),
                                                                    source_venue: Some(
                                                                        VenueId::BYBIT,
                                                                    ),
                                                                };
                                                                let _ = tx.send(Ok(
                                                                    MarketEvent::Trade(trade),
                                                                ));
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                Ok(None) => {
                                    warn!("Bybit WS disconnected");
                                    break;
                                }
                                Err(e) => {
                                    error!("Bybit WS error: {}", e);
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Bybit connect error: {}", e);
                    }
                }
                attempts += 1;
                if attempts > 5 {
                    let _ = tx.send(Err(HftError::Network("too many reconnects".to_string())));
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });

        let stream = async_stream::stream! {
            while let Some(ev) = rx.recv().await { yield ev; }
        };
        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: true,
            latency_ms: None,
            last_heartbeat: hft_core::now_micros(),
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        Ok(())
    }
    async fn disconnect(&mut self) -> HftResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::MarketStream;

    // Category tests
    #[test]
    fn test_category_default_is_spot() {
        // Clear any env var that might affect the test
        std::env::remove_var("BYBIT_CATEGORY");
        let category = Category::from_env();
        assert_eq!(category, Category::Spot);
    }

    #[test]
    fn test_category_linear_from_env() {
        std::env::set_var("BYBIT_CATEGORY", "linear");
        let category = Category::from_env();
        assert_eq!(category, Category::Linear);
        std::env::remove_var("BYBIT_CATEGORY");
    }

    #[test]
    fn test_category_usdt_maps_to_linear() {
        std::env::set_var("BYBIT_CATEGORY", "usdt");
        let category = Category::from_env();
        assert_eq!(category, Category::Linear);
        std::env::remove_var("BYBIT_CATEGORY");
    }

    #[test]
    fn test_category_perp_maps_to_linear() {
        std::env::set_var("BYBIT_CATEGORY", "perp");
        let category = Category::from_env();
        assert_eq!(category, Category::Linear);
        std::env::remove_var("BYBIT_CATEGORY");
    }

    #[test]
    fn test_category_unknown_defaults_to_spot() {
        std::env::set_var("BYBIT_CATEGORY", "unknown_value");
        let category = Category::from_env();
        assert_eq!(category, Category::Spot);
        std::env::remove_var("BYBIT_CATEGORY");
    }

    #[test]
    fn test_category_ws_base_spot() {
        let category = Category::Spot;
        assert_eq!(category.ws_base(), "wss://stream.bybit.com/v5/public/spot");
    }

    #[test]
    fn test_category_ws_base_linear() {
        let category = Category::Linear;
        assert_eq!(category.ws_base(), "wss://stream.bybit.com/v5/public/linear");
    }

    #[test]
    fn test_category_clone() {
        let original = Category::Linear;
        let cloned = original;
        assert_eq!(cloned, Category::Linear);
    }

    #[test]
    fn test_category_debug() {
        let category = Category::Spot;
        let debug_str = format!("{:?}", category);
        assert!(debug_str.contains("Spot"));
    }

    // OrderBookState tests
    #[test]
    fn test_order_book_state_default() {
        let state = OrderBookState::default();
        assert!(state.bids.is_empty());
        assert!(state.asks.is_empty());
        assert_eq!(state.seq, 0);
    }

    #[test]
    fn test_order_book_state_apply_snapshot() {
        let mut state = OrderBookState::default();
        let bids = vec![
            ["100.5".to_string(), "1.0".to_string()],
            ["100.0".to_string(), "2.0".to_string()],
        ];
        let asks = vec![
            ["101.0".to_string(), "1.5".to_string()],
            ["101.5".to_string(), "2.5".to_string()],
        ];

        state.apply_snapshot(&bids, &asks, 1000);

        assert_eq!(state.bids.len(), 2);
        assert_eq!(state.asks.len(), 2);
        assert_eq!(state.seq, 1000);
    }

    #[test]
    fn test_order_book_state_apply_delta() {
        let mut state = OrderBookState::default();
        // First apply a snapshot
        let bids = vec![["100.0".to_string(), "1.0".to_string()]];
        let asks = vec![["101.0".to_string(), "1.0".to_string()]];
        state.apply_snapshot(&bids, &asks, 1000);

        // Apply delta
        let delta_bids = vec![["100.5".to_string(), "0.5".to_string()]];
        let delta_asks = vec![["100.8".to_string(), "0.8".to_string()]];
        state.apply_delta(&delta_bids, &delta_asks, 1001);

        assert_eq!(state.bids.len(), 2); // Now has 2 bid levels
        assert_eq!(state.asks.len(), 2); // Now has 2 ask levels
        assert_eq!(state.seq, 1001);
    }

    #[test]
    fn test_order_book_state_remove_level_with_zero_size() {
        let mut state = OrderBookState::default();
        let bids = vec![
            ["100.0".to_string(), "1.0".to_string()],
            ["99.5".to_string(), "2.0".to_string()],
        ];
        let asks = vec![];
        state.apply_snapshot(&bids, &asks, 1000);
        assert_eq!(state.bids.len(), 2);

        // Remove level by setting size to 0
        let delta_bids = vec![["100.0".to_string(), "0".to_string()]];
        let empty: Vec<[String; 2]> = vec![];
        state.apply_delta(&delta_bids, &empty, 1001);
        assert_eq!(state.bids.len(), 1);
    }

    #[test]
    fn test_order_book_state_ignore_empty_strings() {
        let mut state = OrderBookState::default();
        let bids = vec![
            ["".to_string(), "1.0".to_string()],     // Empty price
            ["100.0".to_string(), "".to_string()],   // Empty size
            ["100.5".to_string(), "1.0".to_string()], // Valid
        ];
        let empty: Vec<[String; 2]> = vec![];
        state.apply_snapshot(&bids, &empty, 1000);

        // Only the valid level should be added
        assert_eq!(state.bids.len(), 1);
    }

    #[test]
    fn test_order_book_state_to_snapshot() {
        let mut state = OrderBookState::default();
        let bids = vec![
            ["100.5".to_string(), "1.0".to_string()],
            ["100.0".to_string(), "2.0".to_string()],
        ];
        let asks = vec![
            ["101.0".to_string(), "1.5".to_string()],
            ["101.5".to_string(), "2.5".to_string()],
        ];
        state.apply_snapshot(&bids, &asks, 1000);

        let snapshot = state.to_snapshot(Symbol::new("BTCUSDT"), 1234567890);

        assert_eq!(snapshot.symbol, Symbol::new("BTCUSDT"));
        assert_eq!(snapshot.timestamp, 1234567890 * 1000); // ms to us conversion
        assert_eq!(snapshot.bids.len(), 2);
        assert_eq!(snapshot.asks.len(), 2);
        assert_eq!(snapshot.source_venue, Some(VenueId::BYBIT));
    }

    #[test]
    fn test_order_book_state_bids_sorted_descending() {
        let mut state = OrderBookState::default();
        let bids = vec![
            ["99.0".to_string(), "1.0".to_string()],
            ["100.0".to_string(), "1.0".to_string()],
            ["99.5".to_string(), "1.0".to_string()],
        ];
        let empty: Vec<[String; 2]> = vec![];
        state.apply_snapshot(&bids, &empty, 1000);

        let snapshot = state.to_snapshot(Symbol::new("BTCUSDT"), 1000);

        // Bids should be sorted highest to lowest
        assert!(snapshot.bids[0].price.0 > snapshot.bids[1].price.0);
        assert!(snapshot.bids[1].price.0 > snapshot.bids[2].price.0);
    }

    #[test]
    fn test_order_book_state_asks_sorted_ascending() {
        let mut state = OrderBookState::default();
        let asks = vec![
            ["102.0".to_string(), "1.0".to_string()],
            ["100.0".to_string(), "1.0".to_string()],
            ["101.0".to_string(), "1.0".to_string()],
        ];
        let empty: Vec<[String; 2]> = vec![];
        state.apply_snapshot(&empty, &asks, 1000);

        let snapshot = state.to_snapshot(Symbol::new("BTCUSDT"), 1000);

        // Asks should be sorted lowest to highest
        assert!(snapshot.asks[0].price.0 < snapshot.asks[1].price.0);
        assert!(snapshot.asks[1].price.0 < snapshot.asks[2].price.0);
    }

    // BybitMarketStream tests
    #[test]
    fn test_bybit_market_stream_new() {
        let stream = BybitMarketStream::new();
        // BybitMarketStream is a unit struct, just verify construction works
        let _ = stream;
    }

    #[test]
    fn test_bybit_market_stream_default() {
        let stream = BybitMarketStream::default();
        let _ = stream;
    }

    #[tokio::test]
    async fn test_health_check() {
        let stream = BybitMarketStream::new();
        let health = stream.health().await;

        assert!(health.connected);
        assert!(health.latency_ms.is_none());
        assert!(health.last_heartbeat > 0);
    }

    #[tokio::test]
    async fn test_connect() {
        let mut stream = BybitMarketStream::new();
        let result = stream.connect().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_disconnect() {
        let mut stream = BybitMarketStream::new();
        let result = stream.disconnect().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_subscribe_empty_symbols_fails() {
        let stream = BybitMarketStream::new();
        let result = stream.subscribe(vec![]).await;

        assert!(result.is_err());
        if let Err(e) = result {
            let error_msg = format!("{}", e);
            assert!(error_msg.contains("empty"));
        }
    }

    // BybitWsMsg parsing tests
    #[test]
    fn test_parse_bybit_ws_msg_with_topic() {
        let json = r#"{"topic":"orderbook.50.BTCUSDT","type":"snapshot","data":{}}"#;
        let result: Result<BybitWsMsg, _> = serde_json::from_str(json);
        assert!(result.is_ok());
        let msg = result.unwrap();
        assert_eq!(msg.topic, Some("orderbook.50.BTCUSDT".to_string()));
        assert_eq!(msg.ty, Some("snapshot".to_string()));
    }

    #[test]
    fn test_parse_bybit_ws_msg_without_topic() {
        let json = r#"{"success":true,"ret_msg":"subscribe"}"#;
        let result: Result<BybitWsMsg, _> = serde_json::from_str(json);
        assert!(result.is_ok());
        let msg = result.unwrap();
        assert!(msg.topic.is_none());
    }

    #[test]
    fn test_parse_json_function() {
        let json = r#"{"topic":"test","type":"delta"}"#;
        let result: HftResult<BybitWsMsg> = parse_json(json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_json_invalid() {
        let json = "not valid json";
        let result: HftResult<BybitWsMsg> = parse_json(json);
        assert!(result.is_err());
    }
}
