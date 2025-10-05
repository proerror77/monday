use super::{hyperliquid_pairs, Exchange, ExchangeContext, MessageBuffers};
use anyhow::Result;
use async_trait::async_trait;
use chrono::prelude::*;
use clickhouse::Client;
use ordered_float::OrderedFloat;
use serde_json::Value;
use tracing::debug;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Default, Debug)]
struct OrderBookState {
    bids: BTreeMap<OrderedFloat<f64>, f64>,
    asks: BTreeMap<OrderedFloat<f64>, f64>,
    // 最近一次已知的最優買/賣（即使某側臨時為空也保留上次值，保證 L1 穩定輸出）
    last_bid_px: Option<f64>,
    last_bid_qty: Option<f64>,
    last_ask_px: Option<f64>,
    last_ask_qty: Option<f64>,
}

pub struct HyperliquidExchange {
    #[allow(dead_code)]
    ctx: Arc<ExchangeContext>,
    ob: RwLock<HashMap<String, OrderBookState>>, // 內存訂單簿狀態（保證 L1 穩定）
}

impl HyperliquidExchange {
    pub fn new(ctx: Arc<ExchangeContext>) -> Self {
        Self { ctx, ob: RwLock::new(HashMap::new()) }
    }
}

#[async_trait]
impl Exchange for HyperliquidExchange {
    fn name(&self) -> &'static str {
        "hyperliquid"
    }

    fn websocket_url(&self) -> String {
        std::env::var("HYPERLIQUID_WS_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "wss://api.hyperliquid.xyz/ws".to_string())
    }

    fn websocket_headers(&self) -> Vec<(String, String)> {
        // 直连时按需附带 Origin 头
        vec![
            (
                "Origin".to_string(),
                std::env::var("HYPERLIQUID_ORIGIN")
                    .unwrap_or_else(|_| "https://app.hyperliquid.xyz".to_string()),
            ),
            (
                "User-Agent".to_string(),
                std::env::var("HYPERLIQUID_UA").unwrap_or_else(|_|
                    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36".to_string()
                ),
            ),
            (
                "Sec-WebSocket-Extensions".to_string(),
                std::env::var("HYPERLIQUID_WS_EXT").unwrap_or_else(|_|
                    "permessage-deflate; client_max_window_bits".to_string()
                ),
            ),
        ]
    }

    fn heartbeat_interval(&self) -> std::time::Duration {
        // 官方建議：60s 無消息需 ping，設置 55s 主動心跳
        std::time::Duration::from_secs(55)
    }

    async fn get_popular_symbols(&self, limit: usize) -> Result<Vec<String>> {
        if let Some(symbols) = self.ctx.symbols_override() {
            return Ok((*symbols).clone());
        }

        let mut whitelist: Vec<String> = hyperliquid_pairs()
            .into_iter()
            .map(|s| s.to_uppercase())
            .collect();
        whitelist.sort();
        whitelist.dedup();

        if limit > 0 && limit < whitelist.len() {
            whitelist.truncate(limit);
        }

        Ok(whitelist)
    }

    async fn setup_tables(&self, client: &Client, database: &str) -> Result<()> {
        let create_orderbook_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.hyperliquid_orderbook (
                ts DateTime64(3),
                symbol LowCardinality(String),
                side Enum8('bid'=1, 'ask'=2),
                price Decimal64(10),
                qty Decimal64(10)
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, side, price)
            "#,
            database
        );
        client.query(&create_orderbook_table).execute().await?;

        let create_trades_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.hyperliquid_trades (
                ts DateTime64(3),
                symbol LowCardinality(String),
                trade_id String,
                price Decimal64(10),
                qty Decimal64(10),
                side Enum8('buy'=1, 'sell'=2)
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, trade_id)
            "#,
            database
        );
        client.query(&create_trades_table).execute().await?;

        // L1 表（衍生自當前更新的最佳買賣）
        let create_l1_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.hyperliquid_l1 (
                ts DateTime64(3),
                symbol LowCardinality(String),
                bid_px Float64,
                bid_qty Float64,
                ask_px Float64,
                ask_qty Float64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts)
            "#,
            database
        );
        client.query(&create_l1_table).execute().await?;

        Ok(())
    }

    async fn process_message(&self, message: &str, buffers: &mut MessageBuffers) -> Result<()> {
        let v: Value = match serde_json::from_str(message) {
            Ok(x) => x,
            Err(_) => return Ok(()),
        };
        let channel = v.get("channel").and_then(|x| x.as_str()).unwrap_or("");
        if channel.eq_ignore_ascii_case("subscriptionResponse")
            || channel.eq_ignore_ascii_case("pong")
        {
            return Ok(());
        }
        if channel == "l2Book" {
            if let Some(data) = v.get("data") {
                let symbol = data.get("coin").and_then(|x| x.as_str()).unwrap_or("");
                if let Some(levels) = data.get("levels").and_then(|x| x.as_array()) {
                    let ts = Utc::now().timestamp_millis();
                    let mut updated_bids = false;
                    let mut updated_asks = false;
                    for lvl in levels {
                        let px = lvl.get(0).and_then(|x| x.as_f64()).unwrap_or(0.0);
                        let qty = lvl.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0);
                        let side = lvl.get(2).and_then(|x| x.as_str()).unwrap_or("b");
                        let row = serde_json::json!({
                            "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                            "symbol": format!("{}-USDT", symbol),
                            "side": if side.starts_with('b'){"bid"} else {"ask"},
                            "price": px,
                            "qty": qty,
                            "update_id": 0,
                        });
                        buffers.push_spot_orderbook(serde_json::to_string(&row)?);

                        // 更新內存訂單簿（用於跨增量保持 L1 穩定）
                        {
                            let mut books = self.ob.write().await;
                            let book = books.entry(symbol.to_string()).or_default();
                            let k = OrderedFloat(px);
                            if side.eq_ignore_ascii_case("b") {
                                if qty == 0.0 { book.bids.remove(&k); } else { book.bids.insert(k, qty); }
                                updated_bids = true;
                            } else {
                                if qty == 0.0 { book.asks.remove(&k); } else { book.asks.insert(k, qty); }
                                updated_asks = true;
                            }
                        }
                    }
                    // 由內存簿推導穩定 L1：若本次只更新一側，另一側採用最近一次已知 BBO
                    let (emit_bp, emit_bq, emit_ap, emit_aq) = {
                        let mut books = self.ob.write().await;
                        if let Some(book) = books.get_mut(symbol) {
                            let cur_bb = book.bids.iter().rev().next().map(|(p,q)| (p.0, *q));
                            let cur_ba = book.asks.iter().next().map(|(p,q)| (p.0, *q));

                            // 更新最近一次 BBO 記憶
                            if let Some((px, qty)) = cur_bb {
                                book.last_bid_px = Some(px);
                                book.last_bid_qty = Some(qty);
                            }
                            if let Some((px, qty)) = cur_ba {
                                book.last_ask_px = Some(px);
                                book.last_ask_qty = Some(qty);
                            }

                            // 推導輸出（允許一側使用最近一次 BBO）
                            let bp = cur_bb.map(|t| t.0).or(book.last_bid_px).unwrap_or(0.0);
                            let bq = cur_bb.map(|t| t.1).or(book.last_bid_qty).unwrap_or(0.0);
                            let ap = cur_ba.map(|t| t.0).or(book.last_ask_px).unwrap_or(0.0);
                            let aq = cur_ba.map(|t| t.1).or(book.last_ask_qty).unwrap_or(0.0);
                            (bp, bq, ap, aq)
                        } else {
                            (0.0, 0.0, 0.0, 0.0)
                        }
                    };
                    // 僅當本次有更新且能組成有效 BBO 才輸出，避免重複刷屏
                    debug!(
                        "[HL L1] sym={} updated_bids={} updated_asks={} bp={} bq={} ap={} aq={}",
                        symbol, updated_bids, updated_asks, emit_bp, emit_bq, emit_ap, emit_aq
                    );
                    if (updated_bids || updated_asks) && emit_bp > 0.0 && emit_ap > 0.0 {
                        let l1 = serde_json::json!({
                            "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                            "symbol": format!("{}-USDT", symbol),
                            "bid_px": emit_bp,
                            "bid_qty": emit_bq,
                            "ask_px": emit_ap,
                            "ask_qty": emit_aq,
                        });
                        buffers.push_spot_l1(serde_json::to_string(&l1)?);
                    }
                }
            }
        } else if channel == "trades" {
            if let Some(list) = v.get("data").and_then(|x| x.as_array()) {
                for tr in list {
                    let symbol = tr.get("coin").and_then(|x| x.as_str()).unwrap_or("");
                    let px = tr.get("px").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let sz = tr.get("sz").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let side = tr.get("side").and_then(|x| x.as_str()).unwrap_or("B");
                    let ts = tr
                        .get("time")
                        .and_then(|x| x.as_i64())
                        .unwrap_or_else(|| Utc::now().timestamp_millis());
                    let tid = tr
                        .get("tid")
                        .or_else(|| tr.get("id"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let row = serde_json::json!({
                        "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                        "symbol": format!("{}-USDT", symbol),
                        "trade_id": tid,
                        "price": px,
                        "qty": sz,
                        "side": if side.to_lowercase().starts_with('b'){"buy"} else {"sell"},
                    });
                    buffers.push_spot_trades(serde_json::to_string(&row)?);
                }
            }
        } else if channel == "bbo" {
            if let Some(obj) = v.get("data").and_then(|x| x.as_object()) {
                let coin = obj.get("coin").and_then(|x| x.as_str()).unwrap_or("");
                let ts = Utc::now().timestamp_millis();
                // 支持多種字段命名（價格必須，數量可使用上次 BBO 回退）
                let bid_px = obj
                    .get("bp")
                    .and_then(|x| x.as_f64())
                    .or_else(|| obj.get("bidPx").and_then(|x| x.as_f64()));
                let ask_px = obj
                    .get("ap")
                    .and_then(|x| x.as_f64())
                    .or_else(|| obj.get("askPx").and_then(|x| x.as_f64()));
                if let (Some(bp), Some(ap)) = (bid_px, ask_px) {
                    // 數量：優先採用當前字段，否則使用最近一次 BBO 記憶
                    let (bq, aq) = {
                        let mut books = self.ob.write().await;
                        let book = books.entry(coin.to_string()).or_default();
                        let cur_bq = obj
                            .get("bq").and_then(|x| x.as_f64())
                            .or_else(|| obj.get("bidSz").and_then(|x| x.as_f64()));
                        let cur_aq = obj
                            .get("aq").and_then(|x| x.as_f64())
                            .or_else(|| obj.get("askSz").and_then(|x| x.as_f64()));
                        let bq = cur_bq.or(book.last_bid_qty).unwrap_or(0.0);
                        let aq = cur_aq.or(book.last_ask_qty).unwrap_or(0.0);
                        // 更新最近一次 BBO 記憶
                        book.last_bid_px = Some(bp);
                        book.last_ask_px = Some(ap);
                        book.last_bid_qty = Some(bq);
                        book.last_ask_qty = Some(aq);
                        (bq, aq)
                    };
                    let l1 = serde_json::json!({
                        "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                        "symbol": format!("{}-USDT", coin),
                        "bid_px": bp,
                        "bid_qty": bq,
                        "ask_px": ap,
                        "ask_qty": aq,
                    });
                    buffers.push_spot_l1(serde_json::to_string(&l1)?);
                }
            }
        } else if channel == "allMids" {
            // 可選：忽略或寫入專用表；當前忽略。
        }
        Ok(())
    }

    async fn flush_data(
        &self,
        database: &str,
        buffers: &mut MessageBuffers,
    ) -> Result<()> {
        let sink = crate::db::get_sink_async(database).await?;
        buffers
            .flush_spot_table(&sink, "hyperliquid_orderbook", |spot| &mut spot.orderbook)
            .await?;
        buffers
            .flush_spot_table(&sink, "hyperliquid_trades", |spot| &mut spot.trades)
            .await?;
        buffers
            .flush_spot_table(&sink, "hyperliquid_l1", |spot| &mut spot.l1)
            .await?;
        let _ = crate::spool::drain(&sink).await;
        Ok(())
    }

    async fn subscription_messages(&self, symbols: &[String]) -> Result<Vec<String>> {
        // 額外主題透過環境變數配置，預設訂閱 l2Book,trades
        let topics_csv = std::env::var("HYPERLIQUID_WS_TOPICS")
            .unwrap_or_else(|_| "l2Book,trades,bbo".to_string());
        let topics: Vec<String> = topics_csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let coins: Vec<String> = symbols
            .iter()
            .map(|sym| {
                if let Some(pos) = sym.find('-') {
                    sym[..pos].to_string()
                } else if sym.ends_with("USDT") {
                    sym.trim_end_matches("USDT").to_string()
                } else {
                    sym.clone()
                }
            })
            .collect();

        let mut msgs = Vec::new();
        for coin in coins {
            for topic in &topics {
                if topic == "allMids" {
                    // 全市場中價不需要 coin
                    let sub = serde_json::json!({
                        "method":"subscribe",
                        "subscription": {"type": "allMids"}
                    });
                    msgs.push(sub.to_string());
                    continue;
                }
                let sub = serde_json::json!({
                    "method":"subscribe",
                    "subscription": {"type": topic, "coin": coin}
                });
                msgs.push(sub.to_string());
            }
        }
        Ok(msgs)
    }
}
