use super::{
    bitget_futures_pairs, bitget_spot_pairs, Exchange, ExchangeContext, MessageBuffers,
    StreamDiagnostics,
};
use anyhow::Result;
use async_trait::async_trait;
use chrono::prelude::*;
use clickhouse::Client;
use ordered_float::OrderedFloat;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BitgetChannelKind {
    Trades,
    Depth,
    Quote,
    Other,
}

fn classify_bitget_channel(channel: &str) -> BitgetChannelKind {
    let lower = channel.to_ascii_lowercase();
    if lower.contains("trade") {
        BitgetChannelKind::Trades
    } else if lower.starts_with("books") || lower.starts_with("depth") {
        BitgetChannelKind::Depth
    } else if lower.contains("ticker") {
        BitgetChannelKind::Quote
    } else {
        BitgetChannelKind::Other
    }
}

pub struct BitgetExchange {
    ob: RwLock<HashMap<String, OrderBookState>>, // 內存訂單簿狀態
    top_n: usize,
    // 最近成交去重（防止重連後重放/重複）
    trades_cache: RwLock<DedupeCache>,
    forced_market: Option<String>,
    forced_is_futures: bool,
    ctx: Arc<ExchangeContext>,
}

impl BitgetExchange {
    pub fn new(ctx: Arc<ExchangeContext>) -> Self {
        let forced_market = std::env::var("BITGET_MARKET")
            .ok()
            .map(|v| v.to_ascii_uppercase());
        let forced_is_futures = matches!(
            forced_market.as_deref(),
            Some(
                "USDT-FUTURES"
                    | "COIN-FUTURES"
                    | "USDC-FUTURES"
                    | "USDT-SWAP"
                    | "USDC-SWAP"
                    | "COIN-SWAP"
            )
        );
        Self {
            ob: RwLock::new(HashMap::new()),
            top_n: std::env::var("BITGET_TOP_LEVELS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(20),
            trades_cache: RwLock::new(DedupeCache::with_capacity(200_000)),
            forced_market,
            forced_is_futures,
            ctx,
        }
    }

    /// 规范化 symbol 表达，返回 (instId, instType)
    /// 支持以下形式：
    /// - "BTCUSDT:USDT-FUTURES"（推荐）
    /// - "BTCUSDT_UMCBL"（等价于 USDT-FUTURES）
    /// - "BTCUSDT"（默认 SPOT）
    fn normalize_symbol(symbol: &str) -> (String, String) {
        let mut upper = symbol.trim().to_uppercase();
        if upper.is_empty() {
            return (upper, "SPOT".to_string());
        }
        if let Some((inst_id, inst_type)) = upper.split_once('@') {
            return (inst_id.to_string(), inst_type.to_string());
        }
        if let Some((inst_id, mut inst_type)) = upper.split_once(':') {
            // 纠正常见误写，如 USDT-FUTURESUSDT、USDT-FUTURES-USDT
            if inst_type.ends_with("USDT") && inst_type.starts_with("USDT-") {
                inst_type = "USDT-FUTURES";
            }
            return (inst_id.to_string(), inst_type.to_string());
        }

        if upper.ends_with("_SPBL") {
            upper.truncate(upper.len() - 5);
            return (upper, "SPOT".to_string());
        }
        if upper.ends_with("_UMCBL") {
            upper.truncate(upper.len() - 6);
            return (upper, "USDT-FUTURES".to_string());
        }
        if upper.ends_with("_DMCBL") {
            upper.truncate(upper.len() - 6);
            return (upper, "COIN-FUTURES".to_string());
        }

        const QUOTES: [&str; 3] = ["USDT", "USDC", "USD"];
        if !upper.contains('-') && !upper.contains('_') {
            for quote in QUOTES {
                if upper.ends_with(quote) {
                    return (upper, "SPOT".to_string());
                }
            }
            // 默认补 USDT 现货
            upper.push_str("USDT");
        }
        (upper, "SPOT".to_string())
    }

    fn is_derivatives(inst_type: &str, inst_id: &str) -> bool {
        matches!(
            inst_type,
            "FUTURES"
                | "USDT-FUTURES"
                | "USDC-FUTURES"
                | "COIN-FUTURES"
                | "SWAP"
                | "USDT-SWAP"
                | "USDC-SWAP"
                | "COIN-SWAP"
        ) || inst_id.ends_with("_UMCBL")
            || inst_id.ends_with("_DMCBL")
            || inst_id.to_uppercase().contains("PERP")
    }

    fn value_to_f64(value: &Value) -> Option<f64> {
        match value {
            Value::String(s) => s.parse::<f64>().ok(),
            Value::Number(n) => n.as_f64(),
            _ => None,
        }
    }

    fn value_to_i64(value: &Value) -> Option<i64> {
        match value {
            Value::String(s) => {
                if let Ok(v) = s.parse::<i128>() {
                    Some(v as i64)
                } else {
                    s.parse::<f64>().ok().map(|f| f as i64)
                }
            }
            Value::Number(n) => n.as_i64().or_else(|| n.as_u64().map(|u| u as i64)),
            _ => None,
        }
    }

    fn value_to_u64(value: &Value) -> Option<u64> {
        match value {
            Value::String(s) => {
                if let Ok(v) = s.parse::<u128>() {
                    Some(v as u64)
                } else {
                    s.parse::<f64>().ok().map(|f| f as u64)
                }
            }
            Value::Number(n) => n.as_u64().or_else(|| n.as_i64().map(|i| i.max(0) as u64)),
            _ => None,
        }
    }

    fn value_to_string(value: &Value) -> Option<String> {
        match value {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    fn parse_level(level: &Value) -> Option<(f64, f64)> {
        if let Some(arr) = level.as_array() {
            if arr.len() >= 2 {
                let price = arr.first().and_then(Self::value_to_f64)?;
                let qty = arr.get(1).and_then(Self::value_to_f64)?;
                return Some((price, qty));
            }
        }
        if let Some(obj) = level.as_object() {
            let price = obj
                .get("px")
                .or_else(|| obj.get("price"))
                .and_then(Self::value_to_f64)?;
            let qty = obj
                .get("sz")
                .or_else(|| obj.get("size"))
                .or_else(|| obj.get("qty"))
                .or_else(|| obj.get("quantity"))
                .and_then(Self::value_to_f64)?;
            return Some((price, qty));
        }
        None
    }

    fn parse_timestamp(value: Option<&Value>, fallback: u64) -> u64 {
        value
            .and_then(|v| {
                if let Some(ms) = Self::value_to_u64(v) {
                    Some(ms)
                } else if let Some(s) = v.as_str() {
                    s.parse::<f64>().ok().map(|f| f as u64)
                } else {
                    None
                }
            })
            .unwrap_or(fallback)
    }
}

#[derive(Default, Debug)]
struct OrderBookState {
    bids: BTreeMap<OrderedFloat<f64>, f64>,
    asks: BTreeMap<OrderedFloat<f64>, f64>,
    last_update_id: i64,
}

/// 簡單的固定容量 LRU 去重集合（以 FIFO 近似），用於 trade_id 去重
#[derive(Default, Debug)]
struct DedupeCache {
    set: HashSet<String>,
    q: VecDeque<String>,
    cap: usize,
}

impl DedupeCache {
    fn with_capacity(cap: usize) -> Self {
        Self {
            set: HashSet::with_capacity(cap),
            q: VecDeque::with_capacity(cap),
            cap,
        }
    }
    fn contains(&self, k: &str) -> bool {
        self.set.contains(k)
    }
    fn insert(&mut self, k: String) {
        if self.set.insert(k.clone()) {
            self.q.push_back(k);
            if self.q.len() > self.cap {
                if let Some(old) = self.q.pop_front() {
                    self.set.remove(&old);
                }
            }
        }
    }
}

#[async_trait]
impl Exchange for BitgetExchange {
    fn name(&self) -> &'static str {
        "bitget"
    }

    fn websocket_url(&self) -> String {
        // Bitget 公共行情 WS（v2 公共频道）
        "wss://ws.bitget.com/v2/ws/public".to_string()
    }

    fn requires_futures_buffers(&self) -> bool {
        true
    }

    async fn get_popular_symbols(&self, limit: usize) -> Result<Vec<String>> {
        if let Some(mut symbols) = self.ctx.symbols_override().map(|arc| (*arc).clone()) {
            for sym in symbols.iter_mut() {
                if sym.contains(':') || sym.contains('@') {
                    continue;
                }
                if !sym.ends_with("USDT") {
                    sym.push_str("USDT");
                }
                sym.push_str(":SPOT");
            }
            symbols.sort();
            symbols.dedup();
            return Ok(symbols);
        }

        // 允许通过环境变量过滤业务线：BITGET_MARKET=SPOT|USDT-FUTURES|COIN-FUTURES
        let market_filter = self.forced_market.as_deref();

        let mut whitelist = Vec::new();
        match market_filter {
            Some("SPOT") => {
                whitelist.extend(bitget_spot_pairs().into_iter().map(|s| s.to_uppercase()));
            }
            Some("USDT-FUTURES") => {
                whitelist.extend(bitget_futures_pairs().into_iter().map(|s| s.to_uppercase()));
            }
            Some("COIN-FUTURES") => {
                // 暂无单独 COIN-FUTURES 白名单，默认使用 USDT-FUTURES 基底，改 instType 在 normalize 阶段处理
                whitelist.extend(bitget_futures_pairs().into_iter().map(|s| s.to_uppercase()));
            }
            _ => {
                // 默认：同时返回现货+永续，向后兼容
                whitelist.extend(bitget_spot_pairs().into_iter().map(|s| s.to_uppercase()));
                whitelist.extend(bitget_futures_pairs().into_iter().map(|s| s.to_uppercase()));
            }
        }
        whitelist.sort();
        whitelist.dedup();

        if limit > 0 && limit < whitelist.len() {
            tracing::warn!(
                "Bitget 白名單共 {} 個交易對，CLI top-limit={} 會截斷，將忽略 top-limit",
                whitelist.len(),
                limit
            );
        }

        Ok(whitelist)
    }

    async fn setup_tables(&self, client: &Client, database: &str) -> Result<()> {
        // 僅在缺失時創建（IF NOT EXISTS），與現有線上表結構對齊
        let create_spot_ob = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bitget_orderbook (
                exchange_ts UInt64,
                local_ts UInt64,
                symbol String,
                side Enum8('bid' = 1, 'ask' = 2),
                price Float64,
                qty Float64,
                checksum UInt32
            ) ENGINE = MergeTree()
            ORDER BY (symbol, exchange_ts)
            "#,
            database
        );
        client.query(&create_spot_ob).execute().await?;

        let create_spot_trades = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bitget_trades (
                exchange_ts UInt64,
                local_ts UInt64,
                symbol String,
                trade_id String,
                side Enum8('buy' = 1, 'sell' = 2),
                price Float64,
                qty Float64,
                checksum UInt32
            ) ENGINE = MergeTree()
            ORDER BY (symbol, exchange_ts, trade_id)
            "#,
            database
        );
        client.query(&create_spot_trades).execute().await?;

        let create_spot_ticker = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bitget_ticker (
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
        client.query(&create_spot_ticker).execute().await?;

        let create_spot_l1 = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bitget_l1 (
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
        client.query(&create_spot_l1).execute().await?;

        // 期貨表
        let create_fut_ob = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bitget_futures_orderbook (
                exchange_ts UInt64,
                local_ts UInt64,
                symbol String,
                side Enum8('bid' = 1, 'ask' = 2),
                price Float64,
                qty Float64,
                checksum UInt32
            ) ENGINE = MergeTree()
            ORDER BY (symbol, exchange_ts)
            "#,
            database
        );
        client.query(&create_fut_ob).execute().await?;

        let create_fut_trades = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bitget_futures_trades (
                exchange_ts UInt64,
                local_ts UInt64,
                symbol String,
                trade_id String,
                side Enum8('buy' = 1, 'sell' = 2),
                price Float64,
                qty Float64,
                checksum UInt32
            ) ENGINE = MergeTree()
            ORDER BY (symbol, exchange_ts, trade_id)
            "#,
            database
        );
        client.query(&create_fut_trades).execute().await?;

        let create_fut_ticker = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bitget_futures_ticker (
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
        client.query(&create_fut_ticker).execute().await?;

        let create_fut_l1 = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bitget_futures_l1 (
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
        client.query(&create_fut_l1).execute().await?;

        // 通用快照表（若缺失，建立）
        let create_snapshots = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.snapshot_books (
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
        client.query(&create_snapshots).execute().await?;

        Ok(())
    }

    async fn process_message(&self, message: &str, buffers: &mut MessageBuffers) -> Result<()> {
        if let Ok(v) = serde_json::from_str::<Value>(message) {
            // 調試：短期打印訂閱確認/錯誤（可配合日志級別控制）
            if let Some(ev) = v.get("event").and_then(|x| x.as_str()) {
                if ev.eq_ignore_ascii_case("subscribe") || ev.eq_ignore_ascii_case("error") {
                    tracing::info!("bitget event: {}", message);
                }
            }
            if v.get("code").is_some() || v.get("msg").is_some() {
                tracing::warn!("bitget response: {}", message);
            }

            // 調試：識別頻道類型
            if let Some(arg) = v.get("arg").and_then(|x| x.as_object()) {
                if let Some(channel) = arg.get("channel").and_then(|x| x.as_str()) {
                    let inst_type = arg.get("instType").and_then(|x| x.as_str()).unwrap_or("");
                    let inst_id = arg.get("instId").and_then(|x| x.as_str()).unwrap_or("");
                    tracing::debug!(
                        "Processing channel={} instType={} instId={} action={}",
                        channel,
                        inst_type,
                        inst_id,
                        v.get("action").and_then(|x| x.as_str()).unwrap_or("")
                    );
                }
            }
            let Some(arg) = v.get("arg").and_then(|x| x.as_object()) else {
                return Ok(());
            };

            let channel = arg
                .get("channel")
                .or_else(|| arg.get("topic"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let channel_lower = channel.to_ascii_lowercase();

            let symbol = arg
                .get("instId")
                .or_else(|| arg.get("symbol"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            if symbol.is_empty() {
                return Ok(());
            }

            let inst_type_raw = arg
                .get("instType")
                .or_else(|| arg.get("productType"))
                .and_then(|x| x.as_str())
                .unwrap_or("SPOT");
            let inst_type = inst_type_raw.to_ascii_uppercase();
            let is_futures = self.forced_is_futures || Self::is_derivatives(&inst_type, symbol);
            let now_ms = Utc::now().timestamp_millis() as u64;

            let symbol_key = symbol.to_string();
            let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
            let is_snapshot = action.eq_ignore_ascii_case("snapshot");

            // 區分不同的深度頻道類型
            let is_incremental_books = channel_lower == "books"; // 增量更新
            let is_snapshot_books = channel_lower.starts_with("books") && channel_lower != "books"; // books1/5/15 快照

            if channel_lower.contains("books") {
                if let Some(data_arr) = v.get("data").and_then(|x| x.as_array()) {
                    for item in data_arr {
                        let ex_ms = Self::parse_timestamp(
                            item.get("ts")
                                .or_else(|| item.get("T"))
                                .or_else(|| item.get("time")),
                            now_ms,
                        );
                        let seq_id = item
                            .get("seq")
                            .or_else(|| item.get("sequence"))
                            .or_else(|| item.get("S"))
                            .and_then(Self::value_to_i64)
                            .unwrap_or(ex_ms as i64);
                        let checksum = item
                            .get("checksum")
                            .and_then(Self::value_to_u64)
                            .unwrap_or(0) as u32;

                        {
                            let mut books = self.ob.write().await;
                            let book = books.entry(symbol_key.clone()).or_default();

                            // 根據頻道類型決定是否清空
                            if is_snapshot || is_snapshot_books {
                                // books1/5/15 永遠是快照，總是清空
                                book.bids.clear();
                                book.asks.clear();
                            } else if is_incremental_books
                                && action.eq_ignore_ascii_case("snapshot")
                            {
                                // books 頻道只在明確標記為 snapshot 時清空
                                book.bids.clear();
                                book.asks.clear();
                            }
                            // 對於 books 的增量更新，不清空，直接更新
                            if let Some(levels_val) = item.get("bids").or_else(|| item.get("b")) {
                                if let Some(levels) = levels_val.as_array() {
                                    for level in levels {
                                        if let Some((price, qty)) = Self::parse_level(level) {
                                            let k = OrderedFloat(price);
                                            if qty == 0.0 {
                                                book.bids.remove(&k);
                                            } else {
                                                book.bids.insert(k, qty);
                                            }
                                        }
                                    }
                                }
                            }
                            if let Some(levels_val) = item.get("asks").or_else(|| item.get("a")) {
                                if let Some(levels) = levels_val.as_array() {
                                    for level in levels {
                                        if let Some((price, qty)) = Self::parse_level(level) {
                                            let k = OrderedFloat(price);
                                            if qty == 0.0 {
                                                book.asks.remove(&k);
                                            } else {
                                                book.asks.insert(k, qty);
                                            }
                                        }
                                    }
                                }
                            }
                            book.last_update_id = seq_id;
                        }

                        if let Some(levels_val) = item.get("bids").or_else(|| item.get("b")) {
                            if let Some(levels) = levels_val.as_array() {
                                for level in levels {
                                    if let Some((price, qty)) = Self::parse_level(level) {
                                        let row = serde_json::json!({
                                            "exchange_ts": ex_ms,
                                            "local_ts": now_ms,
                                            "symbol": symbol,
                                            "side": "bid",
                                            "price": price,
                                            "qty": qty,
                                            "checksum": checksum,
                                        });
                                        let line = serde_json::to_string(&row)?;
                                        if is_futures {
                                            if let Some(fut) = buffers.futures_mut() {
                                                fut.orderbook.push(line);
                                            }
                                        } else {
                                            buffers.push_spot_orderbook(line);
                                        }
                                    }
                                }
                            }
                        }

                        if let Some(levels_val) = item.get("asks").or_else(|| item.get("a")) {
                            if let Some(levels) = levels_val.as_array() {
                                for level in levels {
                                    if let Some((price, qty)) = Self::parse_level(level) {
                                        let row = serde_json::json!({
                                            "exchange_ts": ex_ms,
                                            "local_ts": now_ms,
                                            "symbol": symbol,
                                            "side": "ask",
                                            "price": price,
                                            "qty": qty,
                                            "checksum": checksum,
                                        });
                                        let line = serde_json::to_string(&row)?;
                                        if is_futures {
                                            if let Some(fut) = buffers.futures_mut() {
                                                fut.orderbook.push(line);
                                            }
                                        } else {
                                            buffers.push_spot_orderbook(line);
                                        }
                                    }
                                }
                            }
                        }

                        let (b_px, b_qty, a_px, a_qty, last_id) = {
                            let books = self.ob.read().await;
                            if let Some(book) = books.get(symbol) {
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
                                (Vec::new(), Vec::new(), Vec::new(), Vec::new(), seq_id)
                            }
                        };
                        let snap = serde_json::json!({
                            "ts": ex_ms * 1000u64,
                            "symbol": symbol,
                            "venue": if is_futures { "bitget_futures" } else { "bitget_spot" },
                            "last_id": last_id,
                            "bids_px": b_px,
                            "bids_qty": b_qty,
                            "asks_px": a_px,
                            "asks_qty": a_qty,
                            "source": "WS",
                        });
                        if is_futures {
                            buffers.push_futures_snapshot(serde_json::to_string(&snap)?);
                        } else {
                            buffers.push_spot_snapshot(serde_json::to_string(&snap)?);
                        }

                        let (best_bid_px, best_bid_qty, best_ask_px, best_ask_qty) = {
                            let books = self.ob.read().await;
                            if let Some(book) = books.get(symbol) {
                                let bb = book.bids.iter().next_back().map(|(p, q)| (p.0, *q));
                                let ba = book.asks.iter().next().map(|(p, q)| (p.0, *q));
                                match (bb, ba) {
                                    (Some(b), Some(a)) => (b.0, b.1, a.0, a.1),
                                    _ => (0.0, 0.0, 0.0, 0.0),
                                }
                            } else {
                                (0.0, 0.0, 0.0, 0.0)
                            }
                        };
                        if best_bid_px > 0.0 && best_ask_px > 0.0 {
                            if let Some(ts) =
                                chrono::DateTime::<Utc>::from_timestamp_millis(ex_ms as i64)
                            {
                                let l1_row = serde_json::json!({
                                    "ts": ts,
                                    "symbol": symbol,
                                    "bid_px": best_bid_px,
                                    "bid_qty": best_bid_qty,
                                    "ask_px": best_ask_px,
                                    "ask_qty": best_ask_qty,
                                });
                                if is_futures {
                                    buffers.push_futures_l1(serde_json::to_string(&l1_row)?);
                                } else {
                                    buffers.push_spot_l1(serde_json::to_string(&l1_row)?);
                                }
                            }
                        }
                    }
                }
            } else if channel_lower.contains("trade") {
                if let Some(data_arr) = v.get("data").and_then(|x| x.as_array()) {
                    let inst_type_str = inst_type.as_str();
                    for item in data_arr {
                        let ex_ms = Self::parse_timestamp(
                            item.get("ts")
                                .or_else(|| item.get("T"))
                                .or_else(|| item.get("time")),
                            now_ms,
                        );
                        let price = item
                            .get("price")
                            .or_else(|| item.get("px"))
                            .or_else(|| item.get("p"))
                            .and_then(Self::value_to_f64);
                        let qty = item
                            .get("size")
                            .or_else(|| item.get("sz"))
                            .or_else(|| item.get("v"))
                            .or_else(|| item.get("volume"))
                            .and_then(Self::value_to_f64);
                        let side_raw = item
                            .get("side")
                            .or_else(|| item.get("S"))
                            .or_else(|| item.get("sd"))
                            .or_else(|| item.get("direction"))
                            .and_then(Self::value_to_string)
                            .unwrap_or_default();
                        let side_norm = match side_raw.to_ascii_lowercase().as_str() {
                            "buy" | "b" | "bid" => "buy",
                            "sell" | "s" | "ask" => "sell",
                            other if other.contains("sell") => "sell",
                            _ => "buy",
                        };
                        let trade_id = item
                            .get("tradeId")
                            .or_else(|| item.get("tid"))
                            .or_else(|| item.get("t"))
                            .or_else(|| item.get("i"))
                            .or_else(|| item.get("id"))
                            .and_then(Self::value_to_string)
                            .unwrap_or_default();
                        if let (Some(price), Some(qty)) = (price, qty) {
                            let key = if !trade_id.is_empty() {
                                format!("{}#{}#{}", inst_type_str, symbol, trade_id)
                            } else {
                                format!("{}#{}#{}#{}#{}", inst_type_str, symbol, ex_ms, price, qty)
                            };
                            let mut cache = self.trades_cache.write().await;
                            if !cache.contains(&key) {
                                cache.insert(key);
                                let row = serde_json::json!({
                                    "exchange_ts": ex_ms,
                                    "local_ts": now_ms,
                                    "symbol": symbol,
                                    "trade_id": trade_id,
                                    "side": side_norm,
                                    "price": price,
                                    "qty": qty,
                                    "checksum": 0,
                                });
                                if is_futures {
                                    buffers.push_futures_trades(serde_json::to_string(&row)?);
                                } else {
                                    buffers.push_spot_trades(serde_json::to_string(&row)?);
                                }
                            }
                        }
                    }
                }
            } else if channel_lower.contains("ticker") {
                if let Some(data_arr) = v.get("data").and_then(|x| x.as_array()) {
                    for item in data_arr {
                        let ex_ms = item
                            .get("ts")
                            .or_else(|| item.get("T"))
                            .and_then(Self::value_to_i64)
                            .unwrap_or(Utc::now().timestamp_millis());
                        if let chrono::LocalResult::Single(ts) = Utc.timestamp_millis_opt(ex_ms) {
                            // Bitget WebSocket v2 ticker field names
                            let last = item
                                .get("lastPr") // v2 primary field
                                .or_else(|| item.get("last"))
                                .or_else(|| item.get("close"))
                                .or_else(|| item.get("price"))
                                .or_else(|| item.get("c"))
                                .and_then(Self::value_to_f64)
                                .unwrap_or(0.0);
                            let open = item
                                .get("open24h") // v2 primary field
                                .or_else(|| item.get("openUtc0"))
                                .or_else(|| item.get("open"))
                                .or_else(|| item.get("o"))
                                .and_then(Self::value_to_f64)
                                .unwrap_or(0.0);
                            let high = item
                                .get("high24h") // v2 primary field
                                .or_else(|| item.get("highUtc0"))
                                .or_else(|| item.get("high"))
                                .or_else(|| item.get("h"))
                                .and_then(Self::value_to_f64)
                                .unwrap_or(0.0);
                            let low = item
                                .get("low24h") // v2 primary field
                                .or_else(|| item.get("lowUtc0"))
                                .or_else(|| item.get("low"))
                                .or_else(|| item.get("l"))
                                .and_then(Self::value_to_f64)
                                .unwrap_or(0.0);
                            let volume = item
                                .get("baseVolume") // v2 primary field
                                .or_else(|| item.get("baseVol"))
                                .or_else(|| item.get("vol24h"))
                                .or_else(|| item.get("v"))
                                .and_then(Self::value_to_f64)
                                .unwrap_or(0.0);
                            let quote_volume = item
                                .get("quoteVolume") // v2 primary field
                                .or_else(|| item.get("quoteVol"))
                                .or_else(|| item.get("quoteVol24h"))
                                .or_else(|| item.get("q"))
                                .and_then(Self::value_to_f64)
                                .unwrap_or(0.0);
                            let row = serde_json::json!({
                                "ts": ts,
                                "symbol": symbol,
                                "last": last,
                                "open": open,
                                "high": high,
                                "low": low,
                                "volume": volume,
                                "quote_volume": quote_volume,
                            });
                            if is_futures {
                                buffers.push_futures_ticker(serde_json::to_string(&row)?);
                            } else {
                                buffers.push_spot_ticker(serde_json::to_string(&row)?);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn flush_data(&self, database: &str, buffers: &mut MessageBuffers) -> Result<()> {
        let sink = crate::db::get_sink_async(database).await?;

        buffers
            .flush_spot_table(&sink, "bitget_orderbook", |spot| &mut spot.orderbook)
            .await?;
        buffers
            .flush_spot_table(&sink, "bitget_trades", |spot| &mut spot.trades)
            .await?;
        buffers
            .flush_spot_table(&sink, "bitget_ticker", |spot| &mut spot.ticker)
            .await?;

        buffers
            .flush_futures_table(&sink, "bitget_futures_orderbook", |f| &mut f.orderbook)
            .await?;
        buffers
            .flush_futures_table(&sink, "bitget_futures_trades", |f| &mut f.trades)
            .await?;
        buffers
            .flush_futures_table(&sink, "bitget_futures_ticker", |f| &mut f.ticker)
            .await?;
        buffers
            .flush_futures_table(&sink, "bitget_futures_l1", |f| &mut f.l1)
            .await?;

        buffers
            .flush_spot_table(&sink, "bitget_l1", |spot| &mut spot.l1)
            .await?;
        // 通用快照：現貨 + 永續
        buffers
            .flush_spot_table(&sink, "snapshot_books", |spot| &mut spot.snapshots)
            .await?;
        buffers
            .flush_futures_table(&sink, "snapshot_books", |f| &mut f.snapshots)
            .await?;

        let _ = crate::spool::drain(&sink).await;
        Ok(())
    }

    async fn subscription_messages(&self, symbols: &[String]) -> Result<Vec<String>> {
        // 可通过 BITGET_CHANNELS 指定，如：books,books1,publicTrade,ticker
        // 策略建議：
        // - 期货：books（增量）+ books1（L1快照）組合，適合高頻交易
        // - 现货：books5（5檔快照）平衡數據量和深度
        let profile = self.ctx.profile("bitget");
        let market = self
            .forced_market
            .clone()
            .unwrap_or_else(|| "SPOT".to_string());
        let default_channels = match market.as_str() {
            // 永續：使用增量 books + L1 ticker + 成交；books1 會觸發 30001 錯誤
            "USDT-FUTURES" | "COIN-FUTURES" | "USDC-FUTURES" => "books,publicTrade,ticker",
            // 現貨：保留輕量 books5
            _ => "books5,publicTrade,ticker",
        };
        let raw_channels = std::env::var("BITGET_CHANNELS").ok();
        let mut channels: Vec<String> = raw_channels
            .as_deref()
            .unwrap_or(default_channels)
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if channels.is_empty() {
            tracing::warn!(
                "BITGET_CHANNELS is empty, falling back to default channels: {}",
                default_channels
            );
            channels = default_channels
                .split(',')
                .map(|s| s.trim().to_string())
                .collect();
        }

        let original_channels = channels.clone();
        if !(profile.trades && profile.book && profile.depth) {
            let mut filtered = Vec::new();
            for channel in &channels {
                match classify_bitget_channel(channel) {
                    BitgetChannelKind::Trades if profile.trades => filtered.push(channel.clone()),
                    BitgetChannelKind::Depth if profile.depth => filtered.push(channel.clone()),
                    BitgetChannelKind::Quote if profile.book => filtered.push(channel.clone()),
                    BitgetChannelKind::Other => filtered.push(channel.clone()),
                    _ => {}
                }
            }
            if filtered.is_empty() {
                tracing::warn!(
                    "[bitget] stream profile 過濾後無頻道，恢復為原始配置: {:?}",
                    original_channels
                );
                channels = original_channels;
            } else {
                channels = filtered;
            }
        }

        let mut seen = HashSet::new();
        let mut args = Vec::new();
        let forced_inst_type = std::env::var("BITGET_MARKET")
            .ok()
            .map(|s| s.trim().to_ascii_uppercase())
            .filter(|s| !s.is_empty());
        for sym in symbols {
            let (inst_id, inst_type_raw) = Self::normalize_symbol(sym);
            let inst_type = forced_inst_type
                .as_deref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    if inst_type_raw.is_empty() {
                        "SPOT".to_string()
                    } else {
                        inst_type_raw
                    }
                });
            for channel in &channels {
                let channel_name = match channel.as_str() {
                    "trade" | "publictrade" => "publicTrade",
                    other => other,
                };
                let key = format!("{}|{}|{}", inst_type.as_str(), channel_name, inst_id);
                if !seen.insert(key) {
                    continue;
                }
                args.push(serde_json::json!({
                    "instType": inst_type.clone(),
                    "channel": channel_name,
                    "instId": inst_id.clone(),
                }));
            }
        }

        if args.is_empty() {
            return Ok(vec![]);
        }

        let chunk_size = std::env::var("BITGET_SUB_CHUNK")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(50);

        let mut messages = Vec::new();
        for chunk in args.chunks(chunk_size) {
            let msg = serde_json::json!({
                "op": "subscribe",
                "args": chunk,
            });
            messages.push(msg.to_string());
        }
        Ok(messages)
    }

    fn stream_diagnostics(&self) -> StreamDiagnostics {
        let profile = self.ctx.profile("bitget");
        StreamDiagnostics {
            trades: profile.trades,
            book: profile.book,
            depth: profile.depth,
        }
    }
}
