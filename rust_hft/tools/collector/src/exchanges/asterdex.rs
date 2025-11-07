use super::{asterdex_pairs, Exchange, ExchangeContext, MessageBuffers};
use anyhow::Result;
use async_trait::async_trait;
use chrono::prelude::*;
use clickhouse::Client;
use ordered_float::OrderedFloat;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct AsterdexExchange {
    ob: RwLock<HashMap<String, OrderBookState>>, // 內存訂單簿狀態
    top_n: usize,
    ctx: Arc<ExchangeContext>,
}

impl AsterdexExchange {
    pub fn new(ctx: Arc<ExchangeContext>) -> Self {
        Self {
            ob: RwLock::new(HashMap::new()),
            top_n: 20,
            ctx,
        }
    }
}

#[derive(Default, Debug)]
struct OrderBookState {
    bids: BTreeMap<OrderedFloat<f64>, f64>,
    asks: BTreeMap<OrderedFloat<f64>, f64>,
    last_update_id: i64,
}

#[async_trait]
impl Exchange for AsterdexExchange {
    fn name(&self) -> &'static str {
        "asterdex"
    }

    fn websocket_url(&self) -> String {
        // Asterdex Futures base (Binance 風格)
        // 使用 /ws 以支持 SUBSCRIBE 命令
        "wss://fstream.asterdex.com/ws".to_string()
    }

    async fn get_popular_symbols(&self, limit: usize) -> Result<Vec<String>> {
        if let Some(symbols) = self.ctx.symbols_override() {
            return Ok((*symbols).clone());
        }

        let mut whitelist: Vec<String> = asterdex_pairs()
            .into_iter()
            .map(|s| s.to_uppercase())
            .collect();
        whitelist.sort();
        whitelist.dedup();

        if limit > 0 && limit < whitelist.len() {
            tracing::warn!(
                "Asterdex 白名單共 {} 個交易對，CLI top-limit={} 會截斷，將忽略 top-limit",
                whitelist.len(),
                limit
            );
        }

        Ok(whitelist)
    }

    async fn setup_tables(&self, client: &Client, database: &str) -> Result<()> {
        // L2 行表（逐價位更新）
        let create_orderbook_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.asterdex_orderbook (
                ts DateTime64(3),
                symbol LowCardinality(String),
                side Enum8('bid'=1, 'ask'=2),
                price Float64,
                qty Float64,
                update_id Int64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, side, price)
            "#,
            database
        );
        client.query(&create_orderbook_table).execute().await?;

        // L1 行表（最優買賣）
        let create_l1_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.asterdex_l1 (
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

        // 成交表（aggTrade）
        let create_trades_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.asterdex_trades (
                ts DateTime64(3),
                symbol LowCardinality(String),
                trade_id String,
                price Float64,
                qty Float64,
                is_buyer_maker Bool
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, trade_id)
            "#,
            database
        );
        client.query(&create_trades_table).execute().await?;

        // Ticker 表（目前使用 L1 或交易推導，暫不提供 24h 聚合）
        let create_ticker_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.asterdex_ticker (
                ts DateTime64(3),
                symbol LowCardinality(String),
                last Float64,
                open Float64,
                high Float64,
                low Float64,
                volume Float64,
                quote_volume Float64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts)
            "#,
            database
        );
        client.query(&create_ticker_table).execute().await?;

        // 視圖：bbo / l2 / lob
        let create_bbo_view = format!(
            r#"
            CREATE VIEW IF NOT EXISTS {}.asterdex_bbo AS
            SELECT ts, symbol, bid_px, bid_qty, ask_px, ask_qty FROM {}.asterdex_l1
            "#,
            database, database
        );
        client.query(&create_bbo_view).execute().await?;

        let create_l2_view = format!(
            r#"
            CREATE VIEW IF NOT EXISTS {}.asterdex_l2 AS
            SELECT ts, symbol, side, price, qty, update_id FROM {}.asterdex_orderbook
            "#,
            database, database
        );
        client.query(&create_l2_view).execute().await?;
        // 快照視圖（從通用 snapshot_books 對應 asterdex）
        let create_lob_view = format!(
            r#"
            CREATE VIEW IF NOT EXISTS {}.asterdex_lob AS
            SELECT toDateTime64(ts / 1000000, 6) AS ts,
                   symbol,
                   last_id,
                   bids_px, bids_qty,
                   asks_px, asks_qty,
                   source
            FROM {}.snapshot_books
            WHERE venue = 'asterdex'
            "#,
            database, database
        );
        client.query(&create_lob_view).execute().await?;

        Ok(())
    }

    async fn process_message(&self, message: &str, buffers: &mut MessageBuffers) -> Result<()> {
        // Asterdex 與 Binance Futures 兼容：depthUpdate / bookTicker
        let v: Value = match serde_json::from_str(message) {
            Ok(x) => x,
            Err(_) => return Ok(()),
        };
        // 組合流包裝 {stream, data}
        let payload = v.get("data").cloned().unwrap_or(v);

        // depthUpdate
        #[derive(Deserialize)]
        struct DepthUpdate {
            #[serde(rename = "e")]
            e: Option<String>,
            #[serde(rename = "E")]
            event_time: Option<i64>,
            #[serde(rename = "s")]
            symbol: Option<String>,
            #[serde(rename = "u")]
            final_update_id: Option<i64>,
            #[serde(rename = "b")]
            bids: Option<Vec<(String, String)>>,
            #[serde(rename = "a")]
            asks: Option<Vec<(String, String)>>,
        }

        if let Ok(dep) = serde_json::from_value::<DepthUpdate>(payload.clone()) {
            if dep.e.as_deref() == Some("depthUpdate") {
                let symbol = dep.symbol.unwrap_or_default();
                let update_id = dep.final_update_id.unwrap_or(0);
                let ts_ms = dep
                    .event_time
                    .unwrap_or_else(|| Utc::now().timestamp_millis());

                // 更新內存訂單簿
                if let Some(bids) = dep.bids.as_ref() {
                    let mut books = self.ob.write().await;
                    let book = books.entry(symbol.clone()).or_default();
                    for (p, q) in bids {
                        if let (Ok(px), Ok(qty)) = (p.parse::<f64>(), q.parse::<f64>()) {
                            let k = OrderedFloat(px);
                            if qty == 0.0 {
                                book.bids.remove(&k);
                            } else {
                                book.bids.insert(k, qty);
                            }
                            // L2 行插入
                            let row = serde_json::json!({
                                "ts": DateTime::<Utc>::from_timestamp_millis(ts_ms).unwrap(),
                                "symbol": symbol,
                                "side": "bid",
                                "price": px,
                                "qty": qty,
                                "update_id": update_id,
                            });
                            buffers.push_spot_orderbook(serde_json::to_string(&row)?);
                        }
                    }
                    book.last_update_id = update_id;
                }
                if let Some(asks) = dep.asks.as_ref() {
                    let mut books = self.ob.write().await;
                    let book = books.entry(symbol.clone()).or_default();
                    for (p, q) in asks {
                        if let (Ok(px), Ok(qty)) = (p.parse::<f64>(), q.parse::<f64>()) {
                            let k = OrderedFloat(px);
                            if qty == 0.0 {
                                book.asks.remove(&k);
                            } else {
                                book.asks.insert(k, qty);
                            }
                            let row = serde_json::json!({
                                "ts": DateTime::<Utc>::from_timestamp_millis(ts_ms).unwrap(),
                                "symbol": symbol,
                                "side": "ask",
                                "price": px,
                                "qty": qty,
                                "update_id": update_id,
                            });
                            buffers.push_spot_orderbook(serde_json::to_string(&row)?);
                        }
                    }
                    book.last_update_id = update_id;
                }

                // 生成 Top-N 快照並寫入通用 snapshot_books
                let (b_px, b_qty, a_px, a_qty, last_id) = {
                    let books = self.ob.read().await;
                    if let Some(book) = books.get(&symbol) {
                        let n = self.top_n;
                        let mut bids_px = Vec::with_capacity(n);
                        let mut bids_qty = Vec::with_capacity(n);
                        for (price, qty) in book.bids.iter().rev().take(n) {
                            bids_px.push(price.0);
                            bids_qty.push(*qty);
                        }
                        let mut asks_px = Vec::with_capacity(n);
                        let mut asks_qty = Vec::with_capacity(n);
                        for (price, qty) in book.asks.iter().take(n) {
                            asks_px.push(price.0);
                            asks_qty.push(*qty);
                        }
                        (bids_px, bids_qty, asks_px, asks_qty, book.last_update_id)
                    } else {
                        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), update_id)
                    }
                };
                let snap = serde_json::json!({
                    "ts": (ts_ms as u64) * 1000u64,
                    "symbol": symbol,
                    "venue": "asterdex",
                    "last_id": last_id,
                    "bids_px": b_px,
                    "bids_qty": b_qty,
                    "asks_px": a_px,
                    "asks_qty": a_qty,
                    "source": "WS",
                });
                buffers.push_spot_snapshot(serde_json::to_string(&snap)?);
                return Ok(());
            }
        }

        // bookTicker -> L1
        #[derive(Deserialize)]
        struct BookTicker {
            #[serde(rename = "e")]
            e: Option<String>,
            #[serde(rename = "E")]
            event_time: Option<i64>,
            #[serde(rename = "s")]
            symbol: Option<String>,
            #[serde(rename = "b")]
            b: String,
            #[serde(rename = "B")]
            bq: String,
            #[serde(rename = "a")]
            a: String,
            #[serde(rename = "A")]
            aq: String,
        }
        if let Ok(bt) = serde_json::from_value::<BookTicker>(payload.clone()) {
            if bt.e.as_deref() == Some("bookTicker") {
                let ts = bt
                    .event_time
                    .and_then(|ms| DateTime::<Utc>::from_timestamp_millis(ms))
                    .unwrap_or_else(|| Utc::now());
                let symbol = bt.symbol.unwrap_or_default();
                if let (Ok(bid_px), Ok(bid_qty), Ok(ask_px), Ok(ask_qty)) = (
                    bt.b.parse::<f64>(),
                    bt.bq.parse::<f64>(),
                    bt.a.parse::<f64>(),
                    bt.aq.parse::<f64>(),
                ) {
                    let row = serde_json::json!({
                        "ts": ts,
                        "symbol": symbol,
                        "bid_px": bid_px,
                        "bid_qty": bid_qty,
                        "ask_px": ask_px,
                        "ask_qty": ask_qty,
                    });
                    buffers.push_spot_l1(serde_json::to_string(&row)?);

                    // 以最優價近似 ticker，volume 暫設為 0
                    let mid = if ask_px > 0.0 && bid_px > 0.0 {
                        (ask_px + bid_px) / 2.0
                    } else {
                        ask_px.max(bid_px)
                    };
                    let ticker_row = serde_json::json!({
                        "ts": ts,
                        "symbol": symbol,
                        "last": mid,
                        "open": mid,
                        "high": mid,
                        "low": mid,
                        "volume": 0.0_f64,
                        "quote_volume": 0.0_f64,
                    });
                    buffers
                        .spot
                        .ticker
                        .push(serde_json::to_string(&ticker_row)?);
                }
            }
        }

        // aggTrade -> 成交
        #[derive(Deserialize)]
        struct AggTrade {
            #[serde(rename = "e")]
            e: Option<String>,
            #[serde(rename = "s")]
            symbol: Option<String>,
            #[serde(rename = "a")]
            agg_id: Option<i64>,
            #[serde(rename = "p")]
            price: String,
            #[serde(rename = "q")]
            qty: String,
            #[serde(rename = "T")]
            trade_time: Option<i64>,
            #[serde(rename = "m")]
            is_buyer_maker: Option<bool>,
        }
        if let Ok(tr) = serde_json::from_value::<AggTrade>(payload) {
            if tr.e.as_deref() == Some("aggTrade") {
                let ts = tr
                    .trade_time
                    .and_then(|ms| DateTime::<Utc>::from_timestamp_millis(ms))
                    .unwrap_or_else(|| Utc::now());
                let symbol = tr.symbol.unwrap_or_default();
                if let (Ok(price), Ok(qty)) = (tr.price.parse::<f64>(), tr.qty.parse::<f64>()) {
                    let row = serde_json::json!({
                        "ts": ts,
                        "symbol": symbol,
                        "trade_id": tr.agg_id.unwrap_or_default().to_string(),
                        "price": price,
                        "qty": qty,
                        "is_buyer_maker": tr.is_buyer_maker.unwrap_or(false),
                    });
                    buffers.push_spot_trades(serde_json::to_string(&row)?);
                }
            }
        }
        Ok(())
    }

    async fn flush_data(&self, database: &str, buffers: &mut MessageBuffers) -> Result<()> {
        let sink = crate::db::get_sink_async(database).await?;
        buffers
            .flush_spot_table(&sink, "asterdex_orderbook", |spot| &mut spot.orderbook)
            .await?;
        buffers
            .flush_spot_table(&sink, "asterdex_l1", |spot| &mut spot.l1)
            .await?;
        buffers
            .flush_spot_table(&sink, "asterdex_trades", |spot| &mut spot.trades)
            .await?;
        buffers
            .flush_spot_table(&sink, "asterdex_ticker", |spot| &mut spot.ticker)
            .await?;
        buffers
            .flush_spot_table(&sink, "snapshot_books", |spot| &mut spot.snapshots)
            .await?;
        Ok(())
    }

    async fn subscription_messages(&self, symbols: &[String]) -> Result<Vec<String>> {
        // 订阅 L2 增量 + L1 bookTicker + aggTrade
        let mut params: Vec<String> = Vec::new();
        for s in symbols {
            let lower = s.to_lowercase();
            // 使用有限檔深度，避免依賴 REST 快照橋接
            params.push(format!("{}@depth20@100ms", lower));
            params.push(format!("{}@bookTicker", lower));
            params.push(format!("{}@aggTrade", lower));
        }
        let msg = serde_json::json!({
            "method": "SUBSCRIBE",
            "params": params,
            "id": 1
        });
        Ok(vec![msg.to_string()])
    }
}
