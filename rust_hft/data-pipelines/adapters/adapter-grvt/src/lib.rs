//! GRVT 市場數據適配器（WS JSON-RPC + 可配置端點）
//!
//! 依據文檔：
//! - WS streams: v1.book.s / v1.book.d / v1.trade / v1.ticker.s / v1.ticker.d / v1.mini.s / v1.mini.d / v1.candle
//! - JSON-RPC 訂閱包裹：{"jsonrpc":"2.0","method":"subscribe","params":{"stream":"..","selectors":["..."]},"id":123}
//! - Feed 回傳：{"stream":"..","selector":"..","sequence_number":"..","feed":{...}}
//!
//! 注意：WS 端點域名依環境不同，請透過環境變量提供：
//! - GRVT_WS_PUBLIC 例如：wss://trades.testnet.grvt.io/ws/full 或 wss://trades.dev.gravitymarkets.io/ws/full
//! - GRVT_FEED_STREAM 可選（預設 v1.book.s）
//! - GRVT_BOOK_RATE 可選（預設 500）
//! - GRVT_BOOK_DEPTH 可選（預設 50，僅 v1.book.s 有效）

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use hft_core::{now_micros, HftError, HftResult, Symbol, VenueId};
use ports::{
    BookLevel, BookUpdate, BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot, MarketStream,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};
use integration::http::{HttpClient, HttpClientConfig};

#[derive(Clone, Debug, Default)]
struct InstrumentSpec {
    tick_size: Option<f64>,
    lot_size: Option<f64>,
    min_notional: Option<f64>,
}

fn grvt_rest_base() -> String {
    if let Ok(url) = std::env::var("GRVT_REST") {
        return url;
    }
    let env = std::env::var("GRVT_ENV")
        .unwrap_or_else(|_| "testnet".to_string())
        .to_lowercase();
    match env.as_str() {
        "dev" => "https://api.dev.gravitymarkets.io".to_string(),
        "staging" | "stg" => "https://api.staging.gravitymarkets.io".to_string(),
        "prod" | "production" => "https://api.grvt.io".to_string(),
        _ => "https://api.testnet.grvt.io".to_string(),
    }
}

async fn fetch_instruments_spec() -> std::collections::HashMap<String, InstrumentSpec> {
    let base = grvt_rest_base();
    let cfg = HttpClientConfig {
        base_url: base,
        timeout_ms: 5000,
        user_agent: "hft-client/1.0".to_string(),
    };
    let client = match HttpClient::new(cfg) {
        Ok(c) => c,
        Err(_) => return Default::default(),
    };
    let mut map = std::collections::HashMap::new();
    // POST full/v1/all_instruments with empty body
    let resp = match client.post("/full/v1/all_instruments", None, &serde_json::json!({})).await {
        Ok(r) => r,
        Err(_) => return map,
    };
    let text = match resp.text().await { Ok(t) => t, Err(_) => return map };
    let v: serde_json::Value = match serde_json::from_str(&text) { Ok(x) => x, Err(_) => return map };
    // 嘗試解析為陣列或物件含陣列
    let arr_opt = if let Some(a) = v.as_array() { Some(a.clone()) } else { v.get("instruments").and_then(|x| x.as_array()).cloned() };
    if let Some(arr) = arr_opt {
        for it in arr {
            // instrument 名稱
            let name = it.get("instrument").and_then(|x| x.as_str())
                .or_else(|| it.get("name").and_then(|x| x.as_str()))
                .unwrap_or("");
            if name.is_empty() { continue; }
            // 嘗試多種鍵名
            let tick = it.get("tick_size").and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok())
                .or_else(|| it.get("price_increment").and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()))
                .or_else(|| it.get("min_price_increment").and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()))
                .or_else(|| it.get("tick").and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()));
            let lot = it.get("lot_size").and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok())
                .or_else(|| it.get("size_increment").and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()))
                .or_else(|| it.get("quantity_increment").and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()))
                .or_else(|| it.get("step_size").and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()));
            let min_notional = it.get("min_notional").and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok())
                .or_else(|| it.get("min_order_value").and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()));
            map.insert(name.to_string(), InstrumentSpec { tick_size: tick, lot_size: lot, min_notional });
        }
    }
    map
}

fn round_to_step(v: f64, step: f64) -> f64 {
    if step <= 0.0 { return v; }
    (v / step).round() * step
}

fn grvt_ws_url() -> HftResult<String> {
    if let Ok(url) = std::env::var("GRVT_WS_PUBLIC") {
        return Ok(url);
    }
    // 預設使用 WebSocket：依 GRVT_ENV 選擇域名，GRVT_WS_VARIANT 選擇 full/lite
    let env = std::env::var("GRVT_ENV")
        .unwrap_or_else(|_| "testnet".to_string())
        .to_lowercase();
    let variant = std::env::var("GRVT_WS_VARIANT").unwrap_or_else(|_| "full".to_string());
    let base = match env.as_str() {
        "dev" => "wss://trades.dev.gravitymarkets.io/ws",
        "staging" | "stg" => "wss://trades.staging.gravitymarkets.io/ws",
        "prod" | "production" => "wss://trades.grvt.io/ws",
        _ => "wss://trades.testnet.grvt.io/ws",
    };
    Ok(format!("{}/{}", base, variant))
}

fn to_grvt_instrument(sym: &str) -> String {
    // 粗略映射：BTCUSDT -> BTC_USDT_Perp；若已是 GRVT 名稱，直接返回
    if sym.contains('_') {
        return sym.to_string();
    }
    if sym.ends_with("USDT") && sym.len() > 5 {
        let base = &sym[..sym.len() - 4];
        return format!("{}_USDT_Perp", base);
    }
    sym.to_string()
}

fn parse_book_levels(arr: &serde_json::Value) -> Vec<BookLevel> {
    let mut out = Vec::new();
    if let Some(levels) = arr.as_array() {
        for lvl in levels {
            // 支援 full/lite：price|p, size|s
            let price = lvl
                .get("price")
                .and_then(|v| v.as_str())
                .or_else(|| lvl.get("p").and_then(|v| v.as_str()))
                .and_then(|s| s.parse::<f64>().ok());
            let size = lvl
                .get("size")
                .and_then(|v| v.as_str())
                .or_else(|| lvl.get("s").and_then(|v| v.as_str()))
                .and_then(|s| s.parse::<f64>().ok());
            if let (Some(px), Some(qty)) = (price, size) {
                out.push(BookLevel::new_unchecked(px, qty));
            }
        }
    }
    out
}

pub struct GrvtMarketStream {
    connected: bool,
    last_heartbeat: u64,
}

impl GrvtMarketStream {
    pub fn new() -> Self {
        Self {
            connected: false,
            last_heartbeat: 0,
        }
    }
}

#[async_trait]
impl MarketStream for GrvtMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        if symbols.is_empty() {
            return Err(HftError::new("GRVT: 符號列表不得為空"));
        }

        let ws_url = grvt_ws_url()?;
        let streams_env = std::env::var("GRVT_FEED_STREAM").unwrap_or_else(|_| "v1.book.s".to_string());
        let streams: Vec<String> = streams_env
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let book_rate = std::env::var("GRVT_BOOK_RATE")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(500);
        let depth = std::env::var("GRVT_BOOK_DEPTH")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(50);
        let ticker_rate = std::env::var("GRVT_TICKER_RATE").ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(500);
        let trade_limit = std::env::var("GRVT_TRADE_LIMIT").ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(500);

        // 為每個 stream 構建對應 selectors
        let symbols_clone = symbols.clone();
        let build_selectors = move |stream_name: &str| -> Vec<String> {
            symbols_clone
                .iter()
                .map(|s| {
                    let ins = to_grvt_instrument(&s.0);
                    match stream_name {
                        "v1.book.s" => format!("{}@{}-{}", ins, book_rate, depth),
                        "v1.book.d" => format!("{}@{}", ins, book_rate),
                        name if name.starts_with("v1.ticker") || name.starts_with("v1.mini") => {
                            format!("{}@{}", ins, ticker_rate)
                        }
                        name if name == "v1.trade" => format!("{}@{}", ins, trade_limit),
                        _ => format!("{}@{}", ins, book_rate),
                    }
                })
                .collect()
        };

        // 動態載入 instruments 規格（tick/lot/min_notional）
        let specs = fetch_instruments_spec().await;

        let (tx, rx) = mpsc::unbounded_channel::<HftResult<MarketEvent>>();
        let ws_url_clone = ws_url.clone();
        let specs_clone = specs.clone();
        tokio::spawn(async move {
            let (mut socket, _resp) = match tokio_tungstenite::connect_async(&ws_url_clone).await {
                Ok(x) => x,
                Err(e) => {
                    let _ = tx.send(Err(HftError::Network(format!("GRVT WS 連線失敗: {}", e))));
                    return;
                }
            };

            // 訂閱一個或多個 stream
            let mut req_id: u64 = 1;
            for sname in &streams {
                let selectors = build_selectors(sname);
                let subscribe = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "subscribe",
                    "params": { "stream": sname, "selectors": selectors },
                    "id": req_id,
                });
                req_id += 1;
                if let Err(e) = socket.send(Message::Text(subscribe.to_string())).await {
                    let _ = tx.send(Err(HftError::Network(format!("GRVT WS 訂閱失敗: {}", e))));
                    return;
                }
            }

            while let Some(msg) = socket.next().await {
                match msg {
                    Ok(Message::Text(txt)) => {
                        let v: serde_json::Value = match serde_json::from_str(&txt) {
                            Ok(x) => x,
                            Err(e) => {
                                let _ = tx.send(Err(HftError::Serialization(e.to_string())));
                                continue;
                            }
                        };

                        // 訂閱回覆包裝：有 result/stream/subs
                        if v.get("result").is_some() && v.get("method").is_some() {
                            // 訂閱 ack，不派發
                            continue;
                        }

                        // Feed: {stream, selector, sequence_number, feed}
                        let stream_name = v.get("stream").and_then(|x| x.as_str()).unwrap_or("");
                        let selector = v.get("selector").and_then(|x| x.as_str()).unwrap_or("");
                        let sequence = v
                            .get("sequence_number")
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(0);
                        let feed = v.get("feed").cloned().unwrap_or(serde_json::Value::Null);

                        // 行情分流處理：book.s 快照 / book.d 增量 / ticker / trade
                        if stream_name.starts_with("v1.book.s") {
                            // 允許 full/lite 欄位：bids/asks 或 b/a
                            let bids_v = feed
                                .get("bids")
                                .cloned()
                                .or_else(|| feed.get("b").cloned())
                                .unwrap_or(serde_json::Value::Null);
                            let asks_v = feed
                                .get("asks")
                                .cloned()
                                .or_else(|| feed.get("a").cloned())
                                .unwrap_or(serde_json::Value::Null);
                            let ts = feed
                                .get("event_time")
                                .and_then(|x| x.as_str())
                                .and_then(|s| s.parse::<u64>().ok())
                                .unwrap_or_else(now_micros);
                            let mut bids = parse_book_levels(&bids_v);
                            let mut asks = parse_book_levels(&asks_v);

                            // 反向映射 selector 的 instrument -> symbol，用 @ 前的 primary
                            let inst = selector.split('@').next().unwrap_or("");
                            // 嘗試還原 base/quote：僅做最小處理
                            let symbol = Symbol(inst.replace('_', "").replace("Perp", ""));

                            // 動態四捨五入
                            if let Some(spec) = specs_clone.get(inst) {
                                if let Some(tsz) = spec.tick_size {
                                    for lvl in &mut bids { if let Some(px) = lvl.price.to_f64() { lvl.price = hft_core::Price::from_f64(round_to_step(px, tsz)).unwrap(); } }
                                    for lvl in &mut asks { if let Some(px) = lvl.price.to_f64() { lvl.price = hft_core::Price::from_f64(round_to_step(px, tsz)).unwrap(); } }
                                }
                                if let Some(ls) = spec.lot_size {
                                    for lvl in &mut bids { if let Some(q) = lvl.quantity.to_f64() { lvl.quantity = hft_core::Quantity::from_f64(round_to_step(q, ls)).unwrap(); } }
                                    for lvl in &mut asks { if let Some(q) = lvl.quantity.to_f64() { lvl.quantity = hft_core::Quantity::from_f64(round_to_step(q, ls)).unwrap(); } }
                                }
                            }

                            let snapshot = MarketSnapshot {
                                symbol,
                                timestamp: ts,
                                bids,
                                asks,
                                sequence,
                                source_venue: Some(VenueId::GRVT),
                            };
                            let _ = tx.send(Ok(MarketEvent::Snapshot(snapshot)));
                        } else if stream_name.starts_with("v1.book.d") {
                            let bids_v = feed.get("bids").cloned().or_else(|| feed.get("b").cloned()).unwrap_or(serde_json::Value::Null);
                            let asks_v = feed.get("asks").cloned().or_else(|| feed.get("a").cloned()).unwrap_or(serde_json::Value::Null);
                            let ts = feed.get("event_time").and_then(|x| x.as_str()).and_then(|s| s.parse::<u64>().ok()).unwrap_or_else(now_micros);
                            let mut bids = parse_book_levels(&bids_v);
                            let mut asks = parse_book_levels(&asks_v);
                            let inst = selector.split('@').next().unwrap_or("");
                            let symbol = Symbol(inst.replace('_', "").replace("Perp", ""));
                            if let Some(spec) = specs_clone.get(inst) {
                                if let Some(tsz) = spec.tick_size {
                                    for lvl in &mut bids { if let Some(px) = lvl.price.to_f64() { lvl.price = hft_core::Price::from_f64(round_to_step(px, tsz)).unwrap(); } }
                                    for lvl in &mut asks { if let Some(px) = lvl.price.to_f64() { lvl.price = hft_core::Price::from_f64(round_to_step(px, tsz)).unwrap(); } }
                                }
                                if let Some(ls) = spec.lot_size {
                                    for lvl in &mut bids { if let Some(q) = lvl.quantity.to_f64() { lvl.quantity = hft_core::Quantity::from_f64(round_to_step(q, ls)).unwrap(); } }
                                    for lvl in &mut asks { if let Some(q) = lvl.quantity.to_f64() { lvl.quantity = hft_core::Quantity::from_f64(round_to_step(q, ls)).unwrap(); } }
                                }
                            }
                            let upd = BookUpdate { symbol, timestamp: ts, bids, asks, sequence, is_snapshot: false, source_venue: Some(VenueId::GRVT) };
                            let _ = tx.send(Ok(MarketEvent::Update(upd)));
                        } else if stream_name.starts_with("v1.ticker.") {
                            // 以 ticker 的頂檔生成輕量級 BookUpdate
                            let ts = feed.get("event_time").and_then(|x| x.as_str()).and_then(|s| s.parse::<u64>().ok()).unwrap_or_else(now_micros);
                            let bb_px = feed.get("best_bid_price").and_then(|x| x.as_str()).or_else(|| feed.get("bb").and_then(|x| x.as_str()));
                            let bb_sz = feed.get("best_bid_size").and_then(|x| x.as_str()).or_else(|| feed.get("bb1").and_then(|x| x.as_str()));
                            let ba_px = feed.get("best_ask_price").and_then(|x| x.as_str()).or_else(|| feed.get("ba").and_then(|x| x.as_str()));
                            let ba_sz = feed.get("best_ask_size").and_then(|x| x.as_str()).or_else(|| feed.get("ba1").and_then(|x| x.as_str()));
                            let mut bids = Vec::new();
                            let mut asks = Vec::new();
                            if let (Some(px), Some(sz)) = (bb_px, bb_sz) { if let (Ok(p), Ok(q)) = (px.parse::<f64>(), sz.parse::<f64>()) { bids.push(BookLevel::new_unchecked(p, q)); } }
                            if let (Some(px), Some(sz)) = (ba_px, ba_sz) { if let (Ok(p), Ok(q)) = (px.parse::<f64>(), sz.parse::<f64>()) { asks.push(BookLevel::new_unchecked(p, q)); } }
                            let inst = selector.split('@').next().unwrap_or("");
                            let symbol = Symbol(inst.replace('_', "").replace("Perp", ""));
                            if let Some(spec) = specs_clone.get(inst) {
                                if let Some(tsz) = spec.tick_size {
                                    for lvl in &mut bids { if let Some(px) = lvl.price.to_f64() { lvl.price = hft_core::Price::from_f64(round_to_step(px, tsz)).unwrap(); } }
                                    for lvl in &mut asks { if let Some(px) = lvl.price.to_f64() { lvl.price = hft_core::Price::from_f64(round_to_step(px, tsz)).unwrap(); } }
                                }
                                if let Some(ls) = spec.lot_size {
                                    for lvl in &mut bids { if let Some(q) = lvl.quantity.to_f64() { lvl.quantity = hft_core::Quantity::from_f64(round_to_step(q, ls)).unwrap(); } }
                                    for lvl in &mut asks { if let Some(q) = lvl.quantity.to_f64() { lvl.quantity = hft_core::Quantity::from_f64(round_to_step(q, ls)).unwrap(); } }
                                }
                            }
                            let upd = BookUpdate { symbol, timestamp: ts, bids, asks, sequence, is_snapshot: false, source_venue: Some(VenueId::GRVT) };
                            let _ = tx.send(Ok(MarketEvent::Update(upd)));
                        } else if stream_name == "v1.trade" || stream_name.starts_with("v1.trade") {
                            let inst = selector.split('@').next().unwrap_or("");
                            let symbol = Symbol(inst.replace('_', "").replace("Perp", ""));
                            if let Some(arr) = feed.as_array() {
                                for t in arr { if let Some(ev) = parse_trade_obj(t, &symbol) { let _ = tx.send(Ok(ev)); } }
                            } else {
                                if let Some(ev) = parse_trade_obj(&feed, &symbol) { let _ = tx.send(Ok(ev)); }
                            }
                        } else {
                            // 其他 stream 暫不處理
                        }
                    }
                    Ok(Message::Ping(p)) => {
                        let _ = socket.send(Message::Pong(p)).await;
                    }
                    Ok(Message::Close(_)) => {
                        let _ = tx.send(Ok(MarketEvent::Disconnect {
                            reason: "WS closed".into(),
                        }));
                        break;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(HftError::Network(format!("GRVT WS 錯誤: {}", e))));
                        break;
                    }
                    _ => {}
                }
            }
        });

        let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.connected,
            latency_ms: None,
            last_heartbeat: self.last_heartbeat,
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        self.connected = true;
        self.last_heartbeat = now_micros();
        Ok(())
    }
    async fn disconnect(&mut self) -> HftResult<()> {
        self.connected = false;
        Ok(())
    }
}

fn parse_trade_obj(obj: &serde_json::Value, symbol: &Symbol) -> Option<MarketEvent> {
    let ts = obj
        .get("event_time")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| obj.get("et").and_then(|x| x.as_str()).and_then(|s| s.parse::<u64>().ok()))
        .unwrap_or_else(now_micros);
    let price = obj
        .get("price")
        .and_then(|x| x.as_str())
        .or_else(|| obj.get("px").and_then(|x| x.as_str()))
        .or_else(|| obj.get("p").and_then(|x| x.as_str()))?;
    let sz = obj
        .get("size")
        .and_then(|x| x.as_str())
        .or_else(|| obj.get("sz").and_then(|x| x.as_str()))
        .or_else(|| obj.get("s").and_then(|x| x.as_str()))?;
    let side_str = obj
        .get("side")
        .and_then(|x| x.as_str())
        .or_else(|| obj.get("S").and_then(|x| x.as_str()))
        .unwrap_or("B");
    let side = match side_str {
        "B" | "BUY" | "buy" => hft_core::Side::Buy,
        "A" | "SELL" | "sell" => hft_core::Side::Sell,
        _ => hft_core::Side::Buy,
    };
    let price_v = hft_core::Price::from_str(price).ok()?;
    let qty_v = hft_core::Quantity::from_str(sz).ok()?;
    let trade_id = obj
        .get("trade_id")
        .and_then(|x| x.as_str())
        .or_else(|| obj.get("id").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string();
    let trade = ports::Trade {
        symbol: symbol.clone(),
        timestamp: ts,
        price: price_v,
        quantity: qty_v,
        side,
        trade_id,
        source_venue: Some(VenueId::GRVT),
    };
    Some(MarketEvent::Trade(trade))
}
