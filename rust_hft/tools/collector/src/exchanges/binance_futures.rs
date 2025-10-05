use super::{
    binance_common::{BookTicker, DepthUpdate, MiniTicker, OrderBookState, Trade},
    binance_futures_pairs, Exchange, ExchangeContext, MessageBuffers, WebsocketPlan,
};
use anyhow::Result;
use async_trait::async_trait;
use chrono::prelude::*;
use clickhouse::Client;
use ordered_float::OrderedFloat;
// use rust_decimal::Decimal; // removed: we output Float64 to ClickHouse
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub struct BinanceFuturesExchange {
    ob: RwLock<HashMap<String, OrderBookState>>, // 內存訂單簿狀態
    top_n: usize,
    ctx: Arc<ExchangeContext>,
}

impl BinanceFuturesExchange {
    pub fn new(ctx: Arc<ExchangeContext>) -> Self {
        Self {
            ob: RwLock::new(HashMap::new()),
            top_n: 20,
            ctx,
        }
    }

    fn fallback_symbols() -> Vec<String> {
        let mut list = binance_futures_pairs();
        list.iter_mut().for_each(|s| *s = s.to_uppercase());
        list.sort();
        list.dedup();
        list
    }
}

#[async_trait]
impl Exchange for BinanceFuturesExchange {
    fn name(&self) -> &'static str {
        "binance_futures"
    }

    fn websocket_url(&self) -> String {
        // 基底：USDⓉ-M Futures WS 多流入口
        "wss://fstream.binance.com/stream?streams=".to_string()
    }

    fn requires_futures_buffers(&self) -> bool {
        true
    }

    async fn websocket_plan(&self, symbols: &[String]) -> Result<WebsocketPlan> {
        let use_miniticker_arr = matches!(
            std::env::var("BINANCE_FUT_MINITICKER_ARR")
                .or_else(|_| std::env::var("COLLECTOR_FUT_MINITICKER_ARR"))
                .unwrap_or_default()
                .to_lowercase()
                .as_str(),
            "1" | "true" | "yes"
        );

        let mut streams: Vec<String> = Vec::with_capacity(symbols.len() * 4 + 1);
        for symbol in symbols {
            let lower_symbol = symbol.to_lowercase();
            streams.push(format!("{}@trade", lower_symbol));
            streams.push(format!("{}@bookTicker", lower_symbol));
            if !use_miniticker_arr {
                streams.push(format!("{}@miniTicker", lower_symbol));
            }
            if self.ctx.depth_mode.include_limited() {
                streams.push(format!(
                    "{}@depth{}@100ms",
                    lower_symbol, self.ctx.depth_levels
                ));
            }
            if self.ctx.depth_mode.include_incremental() {
                streams.push(format!("{}@depth@100ms", lower_symbol));
            }
        }
        if use_miniticker_arr {
            streams.push("!miniTicker@arr".to_string());
        }

        let url = format!(
            "wss://fstream.binance.com/stream?streams={}",
            streams.join("/")
        );

        Ok(WebsocketPlan {
            url,
            subscribe_messages: Vec::new(),
        })
    }

    async fn get_popular_symbols(&self, limit: usize) -> Result<Vec<String>> {
        let client = reqwest::Client::new();

        if let Some(symbols) = self.ctx.symbols_override() {
            return Ok((*symbols).clone());
        }

        let mut whitelist = binance_futures_pairs();
        whitelist.iter_mut().for_each(|s| *s = s.to_uppercase());
        whitelist.sort();
        whitelist.dedup();

        if limit > 0 && limit < whitelist.len() {
            tracing::warn!(
                "Binance 永續白名單共 {} 個交易對，CLI top-limit={} 會截斷，將忽略 top-limit",
                whitelist.len(),
                limit
            );
        }

        #[derive(Deserialize)]
        struct ExchangeInfoResp {
            symbols: Vec<BinanceSymbolInfo>,
        }
        #[derive(Deserialize)]
        struct BinanceSymbolInfo {
            symbol: String,
            status: String,
            #[serde(rename = "quoteAsset")]
            quote_asset: String,
            #[serde(rename = "contractType")]
            contract_type: String,
        }

        let url = "https://fapi.binance.com/fapi/v1/exchangeInfo";
        let resp = match client
            .get(url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!("binance futures exchangeInfo 請求失敗: {}", e);
                return Ok(Self::fallback_symbols());
            }
        };
        let info: ExchangeInfoResp = match resp.json().await {
            Ok(val) => val,
            Err(e) => {
                tracing::warn!("binance futures exchangeInfo 解析失敗: {}", e);
                return Ok(Self::fallback_symbols());
            }
        };

        let whitelist_set: HashSet<String> = whitelist.iter().cloned().collect();
        let available: HashSet<String> = info
            .symbols
            .into_iter()
            .filter(|s| s.status == "TRADING")
            .filter(|s| s.quote_asset.eq_ignore_ascii_case("USDT"))
            .filter(|s| s.contract_type == "PERPETUAL")
            .map(|s| s.symbol.to_uppercase())
            .filter(|sym| whitelist_set.contains(sym))
            .collect();

        let mut missing = Vec::new();
        let mut filtered = Vec::new();
        for sym in &whitelist {
            if available.contains(sym) {
                filtered.push(sym.clone());
            } else {
                missing.push(sym.clone());
            }
        }

        if filtered.is_empty() {
            tracing::warn!("Binance 永續白名單在 exchangeInfo 中皆不存在，改用原始列表");
            return Ok(whitelist);
        }

        if !missing.is_empty() {
            tracing::warn!("Binance 永續找不到以下交易對：{:?}", missing);
        }

        Ok(filtered)
    }

    async fn setup_tables(&self, client: &Client, database: &str) -> Result<()> {
        // 仅创建需要直写的表：orderbook / trades / ticker / 专用快照（合约）
        let create_orderbook = format!(
            r#"CREATE TABLE IF NOT EXISTS {}.binance_futures_orderbook (
                ts DateTime64(3),
                symbol LowCardinality(String),
                side Enum8('bid' = 1, 'ask' = 2),
                price Float64,
                qty Float64,
                update_id Int64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, side, price)"#,
            database
        );
        client.query(&create_orderbook).execute().await?;

        // 交易 / ticker / L1 表
        let create_trades = format!(
            r#"CREATE TABLE IF NOT EXISTS {}.binance_futures_trades (
                ts DateTime64(3),
                symbol LowCardinality(String),
                trade_id Int64,
                price Float64,
                qty Float64,
                is_buyer_maker Bool
            ) ENGINE=MergeTree() ORDER BY (symbol, ts, trade_id)"#,
            database
        );
        client.query(&create_trades).execute().await?;

        let create_ticker = format!(
            r#"CREATE TABLE IF NOT EXISTS {}.binance_futures_ticker (
                ts DateTime64(3),
                symbol LowCardinality(String),
                last Float64, open Float64, high Float64, low Float64,
                volume Float64, quote_volume Float64
            ) ENGINE=MergeTree() ORDER BY (symbol, ts)"#,
            database
        );
        client.query(&create_ticker).execute().await?;

        // L1 表（最优买卖一档）
        let create_l1 = format!(
            r#"CREATE TABLE IF NOT EXISTS {}.binance_futures_l1 (
                ts DateTime64(3),
                symbol LowCardinality(String),
                bid_px Float64, bid_qty Float64,
                ask_px Float64, ask_qty Float64
            ) ENGINE=MergeTree() ORDER BY (symbol, ts)"#,
            database
        );
        client.query(&create_l1).execute().await?;

        // 不创建任何视图或逐价位/L1 行表

        // 专用快照表（合约）
        let create_fut_snapshots = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.binance_futures_snapshot_books (
                ts UInt64,
                symbol String,
                venue String,
                last_id UInt64,
                bids_px Array(Float64),
                bids_qty Array(Float64),
                asks_px Array(Float64),
                asks_qty Array(Float64),
                source String
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts)
            SETTINGS index_granularity = 8192
            "#,
            database
        );
        client.query(&create_fut_snapshots).execute().await?;

        // 不创建任何视图（按要求仅使用表）

        Ok(())
    }

    async fn process_message(&self, message: &str, buffers: &mut MessageBuffers) -> Result<()> {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(message) {
            // 支持 !miniTicker@arr 批量小行情（合约）
            if let Some(stream) = val.get("stream").and_then(|x| x.as_str()) {
                if stream == "!miniTicker@arr" {
                    if let Some(arr) = val.get("data").and_then(|x| x.as_array()) {
                        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
                        for it in arr {
                            let symbol = it.get("s").and_then(|x| x.as_str()).unwrap_or("");
                            let ex_ms = it
                                .get("E")
                                .and_then(|x| x.as_i64())
                                .map(|v| v as u64)
                                .unwrap_or(now_ms);
                            let ts_dt = chrono::Utc
                                .timestamp_millis_opt(ex_ms as i64)
                                .single()
                                .unwrap_or_else(|| chrono::Utc::now());
                            let f = |k: &str| {
                                it.get(k)
                                    .and_then(|x| x.as_str())
                                    .and_then(|s| s.parse::<f64>().ok())
                            };
                            if let (
                                Some(last),
                                Some(open),
                                Some(high),
                                Some(low),
                                Some(volume),
                                Some(quote_volume),
                            ) = (f("c"), f("o"), f("h"), f("l"), f("v"), f("q"))
                            {
                                let obj = json!({
                                    "ts": ts_dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
                                    "symbol": symbol,
                                    "last": last, "open": open, "high": high, "low": low,
                                    "volume": volume, "quote_volume": quote_volume,
                                });
                                let line = serde_json::to_string(&obj)?;
                                buffers.push_futures_ticker(line);
                            }
                        }
                    }
                    return Ok(());
                }
            }

            let payload = val.get("data").cloned().unwrap_or(val.clone());
            let now_ms = chrono::Utc::now().timestamp_millis() as u64;

            if let Ok(depth) = serde_json::from_value::<DepthUpdate>(payload.clone()) {
                if depth.event_type == "depthUpdate" {
                    let ex_ms = depth.event_time as u64;
                    let _ts_dt = chrono::Utc
                        .timestamp_millis_opt(ex_ms as i64)
                        .single()
                        .unwrap_or_else(|| chrono::Utc::now());

                    // 更新內存訂單簿狀態
                    {
                        let mut books = self.ob.write().await;
                        let book = books.entry(depth.symbol.clone()).or_default();
                        for (p, q) in &depth.bids {
                            if let (Ok(price), Ok(qty)) = (p.parse::<f64>(), q.parse::<f64>()) {
                                let k = OrderedFloat(price);
                                if qty == 0.0 {
                                    book.bids.remove(&k);
                                } else {
                                    book.bids.insert(k, qty);
                                }
                            }
                        }
                        for (p, q) in &depth.asks {
                            if let (Ok(price), Ok(qty)) = (p.parse::<f64>(), q.parse::<f64>()) {
                                let k = OrderedFloat(price);
                                if qty == 0.0 {
                                    book.asks.remove(&k);
                                } else {
                                    book.asks.insert(k, qty);
                                }
                            }
                        }
                        book.last_update_id = depth.final_update_id;
                    }

                    let ts_dt = chrono::Utc
                        .timestamp_millis_opt(depth.event_time)
                        .single()
                        .unwrap_or_else(|| chrono::Utc::now());

                    for (p, q) in &depth.bids {
                        if let (Ok(price), Ok(qty)) = (p.parse::<f64>(), q.parse::<f64>()) {
                            if qty > 0.0 {
                                let row = json!({
                                    "ts": ts_dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
                                    "symbol": depth.symbol,
                                    "side": "bid",
                                    "price": price,
                                    "qty": qty,
                                    "update_id": depth.final_update_id,
                                });
                                let line = serde_json::to_string(&row)?;
                                buffers.push_futures_orderbook(line);
                            }
                        }
                    }
                    for (p, q) in &depth.asks {
                        if let (Ok(price), Ok(qty)) = (p.parse::<f64>(), q.parse::<f64>()) {
                            if qty > 0.0 {
                                let row = json!({
                                    "ts": ts_dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
                                    "symbol": depth.symbol,
                                    "side": "ask",
                                    "price": price,
                                    "qty": qty,
                                    "update_id": depth.final_update_id,
                                });
                                let line = serde_json::to_string(&row)?;
                                buffers.push_futures_orderbook(line);
                            }
                        }
                    }

                    // 生成 Top-N 快照（至少 20 層）
                    let (b_px, b_qty, a_px, a_qty, last_id) = {
                        let books = self.ob.read().await;
                        if let Some(book) = books.get(&depth.symbol) {
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
                            (
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                Vec::new(),
                                depth.final_update_id,
                            )
                        }
                    };

                    let snap = json!({
                        "ts": (ex_ms as u64) * 1000u64, // 微秒
                        "symbol": depth.symbol,
                        "venue": "binance_futures",
                        "last_id": last_id,
                        "bids_px": b_px,
                        "bids_qty": b_qty,
                        "asks_px": a_px,
                        "asks_qty": a_qty,
                        "source": "WS",
                    });
                    let line = serde_json::to_string(&snap)?;
                    buffers.push_futures_snapshot(line);
                }
            } else if let Ok(trade) = serde_json::from_value::<Trade>(payload.clone()) {
                if trade.event_type == "trade" {
                    let ex_ms = trade.trade_time as u64;
                    let ts_dt = chrono::Utc
                        .timestamp_millis_opt(ex_ms as i64)
                        .single()
                        .unwrap_or_else(|| chrono::Utc::now());
                    if let (Ok(price), Ok(qty)) =
                        (trade.price.parse::<f64>(), trade.quantity.parse::<f64>())
                    {
                        let obj = json!({
                            "ts": ts_dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
                            "symbol": trade.symbol,
                            "trade_id": trade.trade_id,
                            "price": price,
                            "qty": qty,
                            "is_buyer_maker": trade.is_buyer_maker,
                        });
                        let line = serde_json::to_string(&obj)?;
                        buffers.push_futures_trades(line);
                    }
                }
            } else if let Ok(bt) = serde_json::from_value::<BookTicker>(payload.clone()) {
                let ex_ms = bt.event_time.map(|t| t as u64).unwrap_or(now_ms);
                let ts_dt = chrono::Utc
                    .timestamp_millis_opt(ex_ms as i64)
                    .single()
                    .unwrap_or_else(|| chrono::Utc::now());
                if let (Ok(bid_px), Ok(bid_qty), Ok(ask_px), Ok(ask_qty)) = (
                    bt.best_bid_price.parse::<f64>(),
                    bt.best_bid_qty.parse::<f64>(),
                    bt.best_ask_price.parse::<f64>(),
                    bt.best_ask_qty.parse::<f64>(),
                ) {
                    let obj = json!({
                        "ts": ts_dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
                        "symbol": bt.symbol,
                        "bid_px": bid_px,
                        "bid_qty": bid_qty,
                        "ask_px": ask_px,
                        "ask_qty": ask_qty,
                    });
                    // futures L1 数据应写入专用缓存
                    let line = serde_json::to_string(&obj)?;
                    buffers.push_futures_l1(line);
                }
            } else if let Ok(tk) = serde_json::from_value::<MiniTicker>(payload) {
                let ex_ms = tk.event_time.map(|t| t as u64).unwrap_or(now_ms);
                let ts_dt = chrono::Utc
                    .timestamp_millis_opt(ex_ms as i64)
                    .single()
                    .unwrap_or_else(|| chrono::Utc::now());
                if let (Ok(last), Ok(open), Ok(high), Ok(low), Ok(volume), Ok(quote_volume)) = (
                    tk.last_price.parse::<f64>(),
                    tk.open_price.parse::<f64>(),
                    tk.high_price.parse::<f64>(),
                    tk.low_price.parse::<f64>(),
                    tk.volume.parse::<f64>(),
                    tk.quote_volume.parse::<f64>(),
                ) {
                    let obj = json!({
                        "ts": ts_dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
                        "symbol": tk.symbol,
                        "last": last,
                        "open": open,
                        "high": high,
                        "low": low,
                        "volume": volume,
                        "quote_volume": quote_volume,
                    });
                    let line = serde_json::to_string(&obj)?;
                    buffers.push_futures_ticker(line);
                }
            }
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
            .flush_futures_table(&sink, "binance_futures_orderbook", |f| &mut f.orderbook)
            .await?;
        buffers
            .flush_futures_table(&sink, "binance_futures_trades", |f| &mut f.trades)
            .await?;
        buffers
            .flush_futures_table(&sink, "binance_futures_ticker", |f| &mut f.ticker)
            .await?;
        buffers
            .flush_futures_table(&sink, "binance_futures_snapshot_books", |f| {
                &mut f.snapshots
            })
            .await?;
        buffers
            .flush_futures_table(&sink, "binance_futures_l1", |f| &mut f.l1)
            .await?;

        let _ = crate::spool::drain(&sink).await;
        Ok(())
    }
}
