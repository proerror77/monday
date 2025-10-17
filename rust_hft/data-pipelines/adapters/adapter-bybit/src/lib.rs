//! Bybit v5 市場數據適配器（WS 公共流）

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use hft_core::{HftError, HftResult, Price, Quantity, Symbol, VenueId};
use ports::{BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot, MarketStream, Trade};
use serde::{de::DeserializeOwned, Deserialize};
use simd_json;
use std::collections::{BTreeMap, HashMap};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, warn};
use url::Url;

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

#[inline]
fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, simd_json::Error> {
    let mut bytes = text.as_bytes().to_vec();
    simd_json::serde::from_slice(bytes.as_mut_slice())
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
                price: Price(p.clone()),
                quantity: Quantity(q.clone()),
            });
        }
        let mut asks = Vec::with_capacity(self.asks.len());
        for (p, q) in self.asks.iter() {
            asks.push(ports::BookLevel {
                price: Price(p.clone()),
                quantity: Quantity(q.clone()),
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
                let url = Url::parse(category.ws_base()).unwrap();
                match connect_async(url).await {
                    Ok((mut ws, _)) => {
                        // build subscribe args
                        let args: Vec<String> = symbols
                            .iter()
                            .flat_map(|s| {
                                vec![
                                    format!("orderbook.{}.{}", levels, s.0),
                                    format!("publicTrade.{}", s.0),
                                ]
                            })
                            .collect();
                        let sub = serde_json::json!({"op":"subscribe","args":args});
                        let _ = ws.send(Message::Text(sub.to_string())).await;

                        let mut ob_states: HashMap<String, OrderBookState> = HashMap::new();
                        while let Some(msg) = ws.next().await {
                            match msg {
                                Ok(Message::Text(txt)) => {
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
                                Ok(Message::Close(_)) => {
                                    warn!("Bybit WS closed");
                                    break;
                                }
                                Ok(_) => {}
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
