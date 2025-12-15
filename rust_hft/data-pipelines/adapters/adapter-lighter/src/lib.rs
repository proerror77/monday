//! Lighter DEX 市場數據適配器（公有 WS + REST 符號映射）
//! 參考官方 SDK：/stream 訂閱 order_book/{market_id}

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use hft_core::{HftError, HftResult, Symbol, VenueId};
use ports::{
    BookLevel, BookUpdate, BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot, MarketStream,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

/// 使用共用的 JSON 解析函數
#[inline]
fn parse_bytes(bytes: &mut [u8]) -> Result<serde_json::Value, HftError> {
    adapters_common::parse_bytes(bytes).map_err(Into::into)
}

#[derive(Clone, Debug)]
struct MarketMeta {
    symbol: String,
    market_id: i64,
}

async fn fetch_order_books_meta(rest_base: &str) -> HftResult<Vec<MarketMeta>> {
    // GET /api/v1/orderBooks
    let url = format!("{}/api/v1/orderBooks", rest_base.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| HftError::Network(e.to_string()))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| HftError::Network(e.to_string()))?;
    if !status.is_success() {
        return Err(HftError::Network(format!(
            "Lighter orderBooks HTTP {}: {}",
            status, text
        )));
    }
    let mut bytes = text.into_bytes();
    let v: serde_json::Value = parse_bytes(bytes.as_mut_slice())?;
    let mut out = Vec::new();
    if let Some(books) = v.get("order_books").and_then(|x| x.as_array()) {
        for it in books {
            if let (Some(sym), Some(mid)) = (
                it.get("symbol").and_then(|x| x.as_str()),
                it.get("market_id").and_then(|x| x.as_i64()),
            ) {
                out.push(MarketMeta {
                    symbol: sym.to_string(),
                    market_id: mid,
                });
            }
        }
    }
    Ok(out)
}

fn parse_levels(side: &serde_json::Value) -> Vec<BookLevel> {
    let mut lvls = Vec::new();
    if let Some(arr) = side.as_array() {
        for el in arr {
            // price can be string or number
            let price_opt = el.get("price").and_then(|x| {
                x.as_str()
                    .and_then(|s| s.parse::<f64>().ok())
                    .or_else(|| x.as_f64())
            });
            let size_opt = el.get("size").and_then(|x| {
                x.as_str()
                    .and_then(|s| s.parse::<f64>().ok())
                    .or_else(|| x.as_f64())
            });
            if let (Some(price), Some(qty)) = (price_opt, size_opt) {
                lvls.push(BookLevel::new_unchecked(price, qty));
            }
        }
    }
    lvls
}

/// Lighter 市場數據流
#[derive(Default)]
pub struct LighterMarketStream {
    is_connected: bool,
    last_heartbeat: u64,
}

impl LighterMarketStream {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MarketStream for LighterMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        if symbols.is_empty() {
            return Err(HftError::new("品種列表不能為空"));
        }

        // Endpoints via env or defaults
        let rest_base = std::env::var("LIGHTER_REST")
            .unwrap_or_else(|_| "https://mainnet.zklighter.elliot.ai".to_string());
        let ws_url = std::env::var("LIGHTER_WS_PUBLIC")
            .unwrap_or_else(|_| "wss://mainnet.zklighter.elliot.ai/stream".to_string());

        info!("訂閱 Lighter 市場數據，品種: {:?}", symbols);
        // Build symbol->market_id map
        let metas = fetch_order_books_meta(&rest_base).await?;
        let mut sym_to_mid = std::collections::HashMap::new();
        let mut mid_to_sym = std::collections::HashMap::new();
        let symbol_set: std::collections::HashSet<String> =
            symbols.iter().map(|s| s.as_str().to_string()).collect();
        for m in metas {
            if symbol_set.contains(&m.symbol) {
                sym_to_mid.insert(m.symbol.clone(), m.market_id);
                mid_to_sym.insert(m.market_id, m.symbol.clone());
            }
        }
        if sym_to_mid.is_empty() {
            warn!("Lighter: 無對應的 market_id（檢查符號或 REST 端點）");
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let ws_url_clone = ws_url.clone();
        tokio::spawn(async move {
            // connect and subscribe
            let (mut ws, _) = match tokio_tungstenite::connect_async(&ws_url_clone).await {
                Ok(x) => x,
                Err(e) => {
                    let _ = tx.send(Err(HftError::Network(format!("WS 連線失敗: {}", e))));
                    return;
                }
            };

            // Send subscribe on connected
            // The server initially sends a {type:"connected"}; handle both paths
            // Proactively subscribe as soon as connected
            for (_sym, mid) in sym_to_mid.iter() {
                let msg = serde_json::json!({"type": "subscribe", "channel": format!("order_book/{}", mid)});
                let _ = ws.send(Message::Text(msg.to_string())).await;
            }

            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        let mut bytes = text.into_bytes();
                        let parsed: serde_json::Value = match parse_bytes(bytes.as_mut_slice()) {
                            Ok(v) => v,
                            Err(e) => {
                                let _ = tx.send(Err(e));
                                continue;
                            }
                        };
                        let typ = parsed.get("type").and_then(|x| x.as_str()).unwrap_or("");
                        match typ {
                            "connected" => {
                                // subscribe again to be safe
                                for (_sym, mid) in sym_to_mid.iter() {
                                    let msg = serde_json::json!({"type": "subscribe", "channel": format!("order_book/{}", mid)});
                                    let _ = ws.send(Message::Text(msg.to_string())).await;
                                }
                            }
                            t if t.starts_with("subscribed/") => {
                                // Example: subscribed/order_book:MARKET_ID
                                let ch =
                                    parsed.get("channel").and_then(|x| x.as_str()).unwrap_or("");
                                if ch.starts_with("order_book:") {
                                    let mid_str = ch.split(':').nth(1).unwrap_or("");
                                    if let Ok(mid) = mid_str.parse::<i64>() {
                                        if let Some(sym) = mid_to_sym.get(&mid) {
                                            let now = hft_core::now_micros();
                                            let ob = parsed
                                                .get("order_book")
                                                .unwrap_or(&serde_json::Value::Null);
                                            let mut bids = parse_levels(
                                                ob.get("bids").unwrap_or(&serde_json::Value::Null),
                                            );
                                            let mut asks = parse_levels(
                                                ob.get("asks").unwrap_or(&serde_json::Value::Null),
                                            );
                                            // Lighter does not guarantee ordering; we order here
                                            bids.sort_by(|a, b| b.price.cmp(&a.price));
                                            asks.sort_by(|a, b| a.price.cmp(&b.price));
                                            let snapshot = MarketSnapshot {
                                                symbol: Symbol::from(sym.clone()),
                                                timestamp: now,
                                                bids,
                                                asks,
                                                sequence: 0,
                                                source_venue: Some(VenueId::LIGHTER),
                                            };
                                            let _ = tx.send(Ok(MarketEvent::Snapshot(snapshot)));
                                        }
                                    }
                                }
                            }
                            "update/order_book" => {
                                // Partial updates, we forward changed levels
                                let ch =
                                    parsed.get("channel").and_then(|x| x.as_str()).unwrap_or("");
                                let mid_opt =
                                    ch.split(':').nth(1).and_then(|s| s.parse::<i64>().ok());
                                if let Some(mid) = mid_opt {
                                    if let Some(sym) = mid_to_sym.get(&mid) {
                                        let now = hft_core::now_micros();
                                        let ob = parsed
                                            .get("order_book")
                                            .unwrap_or(&serde_json::Value::Null);
                                        let bids = parse_levels(
                                            ob.get("bids").unwrap_or(&serde_json::Value::Null),
                                        );
                                        let asks = parse_levels(
                                            ob.get("asks").unwrap_or(&serde_json::Value::Null),
                                        );
                                        let upd = BookUpdate {
                                            symbol: Symbol::from(sym.clone()),
                                            timestamp: now,
                                            bids,
                                            asks,
                                            sequence: 0,
                                            is_snapshot: false,
                                            source_venue: Some(VenueId::LIGHTER),
                                        };
                                        let _ = tx.send(Ok(MarketEvent::Update(upd)));
                                    }
                                }
                            }
                            other => {
                                // ignore unknown
                                if !other.is_empty() {
                                    warn!("Lighter: 未處理訊息型別: {}", other);
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        let _ = tx.send(Ok(MarketEvent::Disconnect {
                            reason: "WS closed".to_string(),
                        }));
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let _ = tx.send(Err(HftError::Network(e.to_string())));
                        break;
                    }
                }
            }
        });

        let stream = futures::stream::poll_fn(move |cx| match rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(event)) => std::task::Poll::Ready(Some(event)),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        });

        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.is_connected,
            latency_ms: None,
            last_heartbeat: self.last_heartbeat,
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        self.is_connected = true;
        self.last_heartbeat = hft_core::now_micros();
        info!("Lighter 適配器就緒");
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.is_connected = false;
        info!("Lighter 適配器已斷開");
        Ok(())
    }
}
