use super::{
    binance_common::{
        BookTicker, DepthUpdate, L1Row as BinanceL1Row, MiniTicker, OrderBookState,
        OrderbookRow as BinanceOrderbookRow, PartialDepthSnapshot, TickerRow as BinanceTickerRow,
        Trade as BinanceTrade, TradesRow as BinanceTradesRow,
    },
    binance_spot_pairs, Exchange, ExchangeContext, MessageBuffers, WebsocketPlan,
};
use anyhow::Result;
use async_trait::async_trait;
use chrono::prelude::*;
use clickhouse::Client;
use ordered_float::OrderedFloat;
// use rust_decimal::Decimal; // removed: f64 is sufficient for ClickHouse Float64 columns
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub struct BinanceExchange {
    ob: RwLock<HashMap<String, OrderBookState>>, // 內存訂單簿狀態
    top_n: usize,
    ctx: Arc<ExchangeContext>,
}

impl BinanceExchange {
    pub fn new(ctx: Arc<ExchangeContext>) -> Self {
        let depth_levels = match ctx.depth_levels {
            5 | 10 | 20 => ctx.depth_levels,
            n if n < 5 => 5,
            n if n < 10 => 10,
            _ => 20,
        };
        Self {
            ob: RwLock::new(HashMap::new()),
            top_n: depth_levels,
            ctx,
        }
    }

    fn fallback_symbols() -> Vec<String> {
        let mut list = binance_spot_pairs();
        list.sort();
        list.dedup();
        list
    }
}

impl BinanceExchange {}

#[async_trait]
impl Exchange for BinanceExchange {
    fn name(&self) -> &'static str {
        "binance"
    }

    fn websocket_url(&self) -> String {
        "wss://stream.binance.com:9443/ws/".to_string()
    }

    async fn websocket_plan(&self, symbols: &[String]) -> Result<WebsocketPlan> {
        let use_miniticker_arr = matches!(
            std::env::var("BINANCE_MINITICKER_ARR")
                .or_else(|_| std::env::var("COLLECTOR_MINITICKER_ARR"))
                .unwrap_or_default()
                .to_lowercase()
                .as_str(),
            "1" | "true" | "yes"
        );

        let depth_levels = match self.ctx.depth_levels {
            5 | 10 | 20 => self.ctx.depth_levels,
            n if n < 5 => 5,
            n if n < 10 => 10,
            _ => 20,
        };

        let mut streams: Vec<String> = Vec::with_capacity(symbols.len() * 4 + 1);
        for symbol in symbols {
            let lower_symbol = symbol.to_lowercase();
            streams.push(format!("{}@trade", lower_symbol));
            streams.push(format!("{}@bookTicker", lower_symbol));
            if !use_miniticker_arr {
                streams.push(format!("{}@miniTicker", lower_symbol));
            }
            if self.ctx.depth_mode.include_limited() {
                streams.push(format!("{}@depth{}@100ms", lower_symbol, depth_levels));
            }
            if self.ctx.depth_mode.include_incremental() {
                streams.push(format!("{}@depth@100ms", lower_symbol));
            }
        }
        if use_miniticker_arr {
            streams.push("!miniTicker@arr".to_string());
        }

        let url = format!(
            "wss://stream.binance.com:9443/stream?streams={}",
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

        let mut whitelist = binance_spot_pairs();
        whitelist.iter_mut().for_each(|s| *s = s.to_uppercase());
        whitelist.sort();
        whitelist.dedup();

        if limit > 0 && limit < whitelist.len() {
            tracing::warn!(
                "Binance 現貨白名單共 {} 個交易對，CLI top-limit={} 會截斷，將忽略 top-limit",
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
        }

        let url = "https://api.binance.com/api/v3/exchangeInfo";
        let resp = match client
            .get(url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!("binance exchangeInfo 請求失敗: {}", e);
                return Ok(Self::fallback_symbols());
            }
        };
        let info: ExchangeInfoResp = match resp.json().await {
            Ok(val) => val,
            Err(e) => {
                tracing::warn!("binance exchangeInfo 解析失敗: {}", e);
                return Ok(Self::fallback_symbols());
            }
        };

        let whitelist_set: HashSet<String> = whitelist.iter().cloned().collect();
        let available: HashSet<String> = info
            .symbols
            .into_iter()
            .filter(|s| s.status == "TRADING")
            .filter(|s| s.quote_asset.eq_ignore_ascii_case("USDT"))
            .filter(|s| whitelist_set.contains(&s.symbol.to_uppercase()))
            .map(|s| s.symbol.to_uppercase())
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
            tracing::warn!("Binance 現貨白名單在 exchangeInfo 中皆不存在，改用原始列表");
            return Ok(whitelist);
        }

        if !missing.is_empty() {
            tracing::warn!("Binance 現貨找不到以下交易對：{:?}", missing);
        }

        Ok(filtered)
    }

    async fn setup_tables(&self, client: &Client, database: &str) -> Result<()> {
        // 创建 binance_orderbook 表（匹配现有表结构）
        let create_orderbook_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.binance_orderbook (
                ts DateTime64(3),
                symbol LowCardinality(String),
                side Enum8('bid' = 1, 'ask' = 2),
                price Float64,
                qty Float64,
                update_id Int64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, side, price)
            "#,
            database
        );
        client.query(&create_orderbook_table).execute().await?;

        // 创建 binance_trades 表
        let create_trades_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.binance_trades (
                ts DateTime64(3),
                symbol LowCardinality(String),
                trade_id Int64,
                price Float64,
                qty Float64,
                is_buyer_maker Bool
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, trade_id)
            "#,
            database
        );
        client.query(&create_trades_table).execute().await?;

        // 创建 binance_l1 表（与现有 ClickHouse 表结构保持一致）
        let create_l1_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.binance_l1 (
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

        // 创建 binance_ticker 表（24hr mini ticker）
        let create_ticker_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.binance_ticker (
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

        // 创建 futures 相关表
        // 专用快照表（现货）
        let create_spot_snapshots = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.binance_snapshot_books (
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
        client.query(&create_spot_snapshots).execute().await?;
        // 不创建任何视图（按要求仅使用表）

        Ok(())
    }

    async fn process_message(&self, message: &str, buffers: &mut MessageBuffers) -> Result<()> {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(message) {
            // 若是 !miniTicker@arr，data 為陣列，需逐條轉換
            if let Some(stream) = value.get("stream").and_then(|x| x.as_str()) {
                if stream == "!miniTicker@arr" {
                    if let Some(arr) = value.get("data").and_then(|x| x.as_array()) {
                        for it in arr {
                            let symbol = it.get("s").and_then(|x| x.as_str()).unwrap_or("");
                            let event_time = it
                                .get("E")
                                .and_then(|x| x.as_i64())
                                .unwrap_or_else(|| Utc::now().timestamp_millis());
                            let ts = Utc
                                .timestamp_millis_opt(event_time)
                                .single()
                                .unwrap_or_else(|| Utc::now());
                            let parse = |k: &str| {
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
                            ) = (
                                parse("c"),
                                parse("o"),
                                parse("h"),
                                parse("l"),
                                parse("v"),
                                parse("q"),
                            ) {
                                let row = BinanceTickerRow {
                                    ts,
                                    symbol: symbol.to_string(),
                                    last,
                                    open,
                                    high,
                                    low,
                                    volume,
                                    quote_volume,
                                };
                                buffers.push_spot_ticker(serde_json::to_string(&row)?);
                            }
                        }
                    }
                    return Ok(());
                }
            }

            // Binance 多流包裹格式 { stream: ..., data: {...} }
            let payload = value.get("data").cloned().unwrap_or(value.clone());

            // 处理部分深度快照 (depth{N}@100ms)
            if let Ok(snapshot) = serde_json::from_value::<PartialDepthSnapshot>(payload.clone()) {
                // 獲取 symbol - 從 stream 字段解析
                let symbol = match value.get("stream").and_then(|s| s.as_str()) {
                    Some(stream) => {
                        // stream 格式: "btcusdt@depth20@100ms"
                        stream.split('@').next().unwrap_or("").to_uppercase()
                    }
                    None => {
                        // 無法解析 symbol，跳過此消息
                        return Ok(());
                    }
                };

                if symbol.is_empty() {
                    return Ok(());
                }

                let ts = Utc::now();

                // 更新內存訂單簿 (完全替換，因為這是快照)
                {
                    let mut books = self.ob.write().await;
                    let book = books.entry(symbol.clone()).or_default();

                    // 清空並重建 bids
                    book.bids.clear();
                    for (price_str, qty_str) in &snapshot.bids {
                        if let (Ok(price), Ok(qty)) =
                            (price_str.parse::<f64>(), qty_str.parse::<f64>())
                        {
                            if qty > 0.0 {
                                book.bids.insert(OrderedFloat(price), qty);
                            }
                        }
                    }

                    // 清空並重建 asks
                    book.asks.clear();
                    for (price_str, qty_str) in &snapshot.asks {
                        if let (Ok(price), Ok(qty)) =
                            (price_str.parse::<f64>(), qty_str.parse::<f64>())
                        {
                            if qty > 0.0 {
                                book.asks.insert(OrderedFloat(price), qty);
                            }
                        }
                    }

                    book.last_update_id = snapshot.last_update_id;
                }

                // 寫入逐價位 L2 行表到 binance_orderbook
                tracing::debug!(
                    "[binance] snapshot {}: bids={} asks={}",
                    symbol,
                    snapshot.bids.len(),
                    snapshot.asks.len()
                );
                for (price_str, qty_str) in &snapshot.bids {
                    if let (Ok(price), Ok(qty)) = (price_str.parse::<f64>(), qty_str.parse::<f64>())
                    {
                        if qty > 0.0 {
                            let row = BinanceOrderbookRow {
                                ts,
                                symbol: symbol.clone(),
                                side: "bid".to_string(),
                                price,
                                qty,
                                update_id: snapshot.last_update_id,
                            };
                            buffers.push_spot_orderbook(serde_json::to_string(&row)?);
                        }
                    }
                }
                for (price_str, qty_str) in &snapshot.asks {
                    if let (Ok(price), Ok(qty)) = (price_str.parse::<f64>(), qty_str.parse::<f64>())
                    {
                        if qty > 0.0 {
                            let row = BinanceOrderbookRow {
                                ts,
                                symbol: symbol.clone(),
                                side: "ask".to_string(),
                                price,
                                qty,
                                update_id: snapshot.last_update_id,
                            };
                            buffers.push_spot_orderbook(serde_json::to_string(&row)?);
                        }
                    }
                }

                // 生成 Top-N 快照
                let ex_ms = ts.timestamp_millis() as u64;
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
                        (
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            snapshot.last_update_id,
                        )
                    }
                };
                let snap = serde_json::json!({
                    "ts": (ex_ms as u64) * 1000u64,
                    "symbol": symbol,
                    "venue": "binance",
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

            // 处理订单簿更新 (增量 depthUpdate)
            if let Ok(depth_update) = serde_json::from_value::<DepthUpdate>(payload.clone()) {
                if depth_update.event_type == "depthUpdate" {
                    let ts = Utc.timestamp_millis_opt(depth_update.event_time).unwrap();

                    // 先更新內存訂單簿（使用本次 payload）
                    {
                        let mut books = self.ob.write().await;
                        let book = books.entry(depth_update.symbol.clone()).or_default();
                        for (price_str, qty_str) in &depth_update.bids {
                            if let (Ok(price), Ok(qty)) =
                                (price_str.parse::<f64>(), qty_str.parse::<f64>())
                            {
                                let k = OrderedFloat(price);
                                if qty == 0.0 {
                                    book.bids.remove(&k);
                                } else {
                                    book.bids.insert(k, qty);
                                }
                            }
                        }
                        for (price_str, qty_str) in &depth_update.asks {
                            if let (Ok(price), Ok(qty)) =
                                (price_str.parse::<f64>(), qty_str.parse::<f64>())
                            {
                                let k = OrderedFloat(price);
                                if qty == 0.0 {
                                    book.asks.remove(&k);
                                } else {
                                    book.asks.insert(k, qty);
                                }
                            }
                        }
                        book.last_update_id = depth_update.final_update_id;
                    }

                    // 写入逐价位 L2 行表到 binance_orderbook
                    tracing::debug!(
                        "[binance] depthUpdate {}: bids={} asks={}",
                        depth_update.symbol,
                        depth_update.bids.len(),
                        depth_update.asks.len()
                    );
                    for (price_str, qty_str) in &depth_update.bids {
                        if let (Ok(price), Ok(qty)) =
                            (price_str.parse::<f64>(), qty_str.parse::<f64>())
                        {
                            if qty > 0.0 {
                                // 只记录有效数量的价位
                                let row = BinanceOrderbookRow {
                                    ts,
                                    symbol: depth_update.symbol.clone(),
                                    side: "bid".to_string(),
                                    price,
                                    qty,
                                    update_id: depth_update.final_update_id,
                                };
                                buffers.push_spot_orderbook(serde_json::to_string(&row)?);
                            }
                        }
                    }
                    for (price_str, qty_str) in &depth_update.asks {
                        if let (Ok(price), Ok(qty)) =
                            (price_str.parse::<f64>(), qty_str.parse::<f64>())
                        {
                            if qty > 0.0 {
                                // 只记录有效数量的价位
                                let row = BinanceOrderbookRow {
                                    ts,
                                    symbol: depth_update.symbol.clone(),
                                    side: "ask".to_string(),
                                    price,
                                    qty,
                                    update_id: depth_update.final_update_id,
                                };
                                buffers.push_spot_orderbook(serde_json::to_string(&row)?);
                            }
                        }
                    }

                    // 生成 Top-N 快照（至少 20 層）
                    let ex_ms = ts.timestamp_millis() as u64;
                    let (b_px, b_qty, a_px, a_qty, last_id) = {
                        let books = self.ob.read().await;
                        if let Some(book) = books.get(&depth_update.symbol) {
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
                                depth_update.final_update_id,
                            )
                        }
                    };
                    let snap = serde_json::json!({
                        "ts": (ex_ms as u64) * 1000u64, // 微秒
                        "symbol": depth_update.symbol,
                        "venue": "binance",
                        "last_id": last_id,
                        "bids_px": b_px,
                        "bids_qty": b_qty,
                        "asks_px": a_px,
                        "asks_qty": a_qty,
                        "source": "WS",
                    });
                    buffers.push_spot_snapshot(serde_json::to_string(&snap)?);
                }
            }
            // 处理交易数据
            else if let Ok(trade) = serde_json::from_value::<BinanceTrade>(payload.clone()) {
                if trade.event_type == "trade" {
                    let ts = Utc.timestamp_millis_opt(trade.trade_time).unwrap();

                    if let (Ok(price), Ok(qty)) =
                        (trade.price.parse::<f64>(), trade.quantity.parse::<f64>())
                    {
                        let row = BinanceTradesRow {
                            ts,
                            symbol: trade.symbol,
                            trade_id: trade.trade_id,
                            price,
                            qty,
                            is_buyer_maker: trade.is_buyer_maker,
                        };
                        buffers.push_spot_trades(serde_json::to_string(&row)?);
                    }
                }
            }
            // L1 不再单独入表，使用快照第一档即可
            // 一些 bookTicker 事件不包含 e/E（簡化格式）
            else if let Ok(bt) = serde_json::from_value::<BookTicker>(payload.clone()) {
                let ts = bt
                    .event_time
                    .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
                    .unwrap_or_else(Utc::now);
                if let (Ok(bid_px), Ok(bid_qty), Ok(ask_px), Ok(ask_qty)) = (
                    bt.best_bid_price.parse::<f64>(),
                    bt.best_bid_qty.parse::<f64>(),
                    bt.best_ask_price.parse::<f64>(),
                    bt.best_ask_qty.parse::<f64>(),
                ) {
                    let row = BinanceL1Row {
                        ts,
                        symbol: bt.symbol,
                        bid_px,
                        bid_qty,
                        ask_px,
                        ask_qty,
                    };
                    buffers.push_spot_l1(serde_json::to_string(&row)?);
                }
            }
            // 处理 24hr miniTicker
            else if let Ok(tk) = serde_json::from_value::<MiniTicker>(payload) {
                if matches!(tk.event_type.as_deref(), Some("24hrMiniTicker")) {
                    let ts = tk
                        .event_time
                        .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
                        .unwrap_or_else(Utc::now);
                    if let (Ok(last), Ok(open), Ok(high), Ok(low), Ok(volume), Ok(quote_volume)) = (
                        tk.last_price.parse::<f64>(),
                        tk.open_price.parse::<f64>(),
                        tk.high_price.parse::<f64>(),
                        tk.low_price.parse::<f64>(),
                        tk.volume.parse::<f64>(),
                        tk.quote_volume.parse::<f64>(),
                    ) {
                        let row = BinanceTickerRow {
                            ts,
                            symbol: tk.symbol,
                            last,
                            open,
                            high,
                            low,
                            volume,
                            quote_volume,
                        };
                        buffers.push_spot_ticker(serde_json::to_string(&row)?);
                    }
                }
            }
        }
        Ok(())
    }

    async fn flush_data(&self, database: &str, buffers: &mut MessageBuffers) -> Result<()> {
        let sink = crate::db::get_sink_async(database).await?;

        let orderbook_count = buffers
            .flush_spot_table(&sink, "binance_orderbook", |spot| &mut spot.orderbook)
            .await?;
        if orderbook_count > 0 {
            tracing::debug!("插入 {} 条 Binance 订单簿记录", orderbook_count);
        }

        let trades_count = buffers
            .flush_spot_table(&sink, "binance_trades", |spot| &mut spot.trades)
            .await?;
        if trades_count > 0 {
            tracing::debug!("插入 {} 条 Binance 交易记录", trades_count);
        }

        let l1_count = buffers
            .flush_spot_table(&sink, "binance_l1", |spot| &mut spot.l1)
            .await?;
        if l1_count > 0 {
            tracing::debug!("插入 {} 条 Binance L1 记录", l1_count);
        }

        let ticker_count = buffers
            .flush_spot_table(&sink, "binance_ticker", |spot| &mut spot.ticker)
            .await?;
        if ticker_count > 0 {
            tracing::debug!("插入 {} 条 Binance Ticker 记录", ticker_count);
        }

        // 快照雙寫：專用表 + 通用 snapshot_books
        if !buffers.spot.snapshots.is_empty() {
            let batch = std::mem::take(&mut buffers.spot.snapshots);
            let count = batch.len();
            if count > 0 {
                let _ = sink
                    .insert_json_rows("binance_snapshot_books", &batch)
                    .await?;
                let _ = sink.insert_json_rows("snapshot_books", &batch).await?;
                tracing::debug!("插入 {} 条 Binance LOB 快照（專用+通用）", count);
            }
        }

        // 嘗試回放本地緩存（若前次寫入失敗）
        let _ = crate::spool::drain(&sink).await;
        Ok(())
    }
}
