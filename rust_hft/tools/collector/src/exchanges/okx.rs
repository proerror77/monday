use super::{okx_spot_pairs, okx_swap_pairs, Exchange, ExchangeContext, MessageBuffers};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use clickhouse::Client;
use ordered_float::OrderedFloat;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub struct OkxExchange {
    orderbooks: RwLock<HashMap<String, OrderBookState>>,
    top_n: usize,
    trades_cache: RwLock<DedupeCache>,
    ctx: Arc<ExchangeContext>,
}

impl OkxExchange {
    pub fn new(ctx: Arc<ExchangeContext>) -> Self {
        Self {
            orderbooks: RwLock::new(HashMap::new()),
            top_n: std::env::var("OKX_TOP_LEVELS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(20),
            trades_cache: RwLock::new(DedupeCache::with_capacity(200_000)),
            ctx,
        }
    }

    fn is_derivatives(inst_type: &str, inst_id: &str) -> bool {
        matches!(
            inst_type,
            "SWAP" | "FUTURES" | "MARGIN" | "OPTION" | "ANY" | "LINEAR" | "DELIVERY"
        ) || inst_id.to_uppercase().contains("-SWAP")
            || inst_id.split('-').count() >= 3
    }

    fn parse_level(level: &Value) -> Option<(f64, f64)> {
        let price = level.get(0)?.as_str()?.parse::<f64>().ok()?;
        let qty = level.get(1)?.as_str()?.parse::<f64>().ok()?;
        Some((price, qty))
    }

    fn ms_to_datetime(ts: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp_millis(ts).unwrap_or_else(|| {
            let secs = ts.div_euclid(1000);
            let millis = ts.rem_euclid(1000) as u32;
            DateTime::<Utc>::from_timestamp(secs, millis * 1_000_000).unwrap_or_else(|| Utc::now())
        })
    }

    fn fallback_symbols() -> Vec<String> {
        let mut list: Vec<String> = okx_spot_pairs()
            .into_iter()
            .chain(okx_swap_pairs().into_iter())
            .map(|s| s.to_uppercase())
            .collect();
        list.sort();
        list.dedup();
        list
    }

    async fn fetch_symbols(inst_type: &str) -> Result<Vec<String>> {
        #[derive(Debug, Deserialize)]
        struct OkxTickerEntry {
            #[serde(rename = "instId")]
            inst_id: String,
        }
        #[derive(Debug, Deserialize)]
        struct OkxTickerResp {
            data: Vec<OkxTickerEntry>,
        }

        let url = format!(
            "https://www.okx.com/api/v5/market/tickers?instType={}",
            inst_type
        );
        let client = reqwest::Client::new();
        let resp: OkxTickerResp = client
            .get(url)
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .json()
            .await?;
        Ok(resp.data.into_iter().map(|x| x.inst_id).collect())
    }

    fn normalize_symbol(sym: &str) -> (String, String) {
        let upper = sym.trim().to_uppercase();
        if let Some((inst_id, inst_type)) = upper.split_once('@') {
            return (inst_id.to_string(), inst_type.to_string());
        }
        if let Some((inst_id, inst_type)) = upper.split_once(':') {
            return (inst_id.to_string(), inst_type.to_string());
        }
        if upper.contains('-') {
            let parts: Vec<&str> = upper.split('-').collect();
            if parts.len() >= 3 {
                let last = parts.last().unwrap();
                if *last == "SWAP" {
                    return (upper, "SWAP".to_string());
                }
                if last.chars().all(|c| c.is_ascii_digit()) {
                    return (upper, "FUTURES".to_string());
                }
            }
            return (upper, "SPOT".to_string());
        }
        const QUOTES: [&str; 5] = ["USDT", "USDC", "USD", "BTC", "ETH"];
        for quote in QUOTES {
            if upper.ends_with(quote) {
                let base = &upper[..upper.len() - quote.len()];
                return (format!("{}-{}", base, quote), "SPOT".to_string());
            }
        }
        (upper, "SPOT".to_string())
    }
}

#[derive(Default, Debug)]
struct OrderBookState {
    bids: BTreeMap<OrderedFloat<f64>, f64>,
    asks: BTreeMap<OrderedFloat<f64>, f64>,
    last_update_id: i64,
}

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
    fn contains(&self, key: &str) -> bool {
        self.set.contains(key)
    }
    fn insert(&mut self, key: String) {
        if self.set.insert(key.clone()) {
            self.q.push_back(key);
            if self.q.len() > self.cap {
                if let Some(old) = self.q.pop_front() {
                    self.set.remove(&old);
                }
            }
        }
    }
}

#[async_trait]
impl Exchange for OkxExchange {
    fn name(&self) -> &'static str {
        "okx"
    }

    fn websocket_url(&self) -> String {
        "wss://ws.okx.com:8443/ws/v5/public".to_string()
    }

    fn requires_futures_buffers(&self) -> bool {
        true
    }

    async fn get_popular_symbols(&self, limit: usize) -> Result<Vec<String>> {
        if let Some(symbols) = self.ctx.symbols_override() {
            return Ok((*symbols).clone());
        }

        let explicit = std::env::var("OKX_SYMBOLS").ok().map(|csv| {
            csv.split(',')
                .map(|s| s.trim().to_uppercase())
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        });

        let mut whitelist = explicit.unwrap_or_else(Self::fallback_symbols);
        whitelist.iter_mut().for_each(|s| *s = s.to_uppercase());
        whitelist.sort();
        whitelist.dedup();

        if limit > 0 && limit < whitelist.len() {
            tracing::warn!(
                "OKX 白名單共 {} 個交易對，CLI top-limit={} 會截斷，將忽略 top-limit",
                whitelist.len(),
                limit
            );
        }

        let desired: HashSet<String> = whitelist.iter().cloned().collect();
        let mut available = HashSet::new();
        for inst_type in ["SPOT", "SWAP"] {
            match Self::fetch_symbols(inst_type).await {
                Ok(list) => {
                    for inst in list {
                        let uppercase = inst.to_uppercase();
                        if desired.contains(&uppercase) {
                            available.insert(uppercase);
                        }
                    }
                }
                Err(err) => tracing::warn!("OKX 拉取 {} 清單失敗: {}", inst_type, err),
            }
        }

        if available.is_empty() {
            tracing::warn!("OKX 白名單在 API 中皆不存在，改用固定列表");
            return Ok(whitelist);
        }

        let mut missing = Vec::new();
        let mut filtered = Vec::new();
        for sym in &whitelist {
            if available.contains(sym) {
                filtered.push(sym.clone());
            } else {
                missing.push(sym.clone());
            }
        }

        if !missing.is_empty() {
            tracing::warn!("OKX 找不到以下交易對：{:?}", missing);
        }

        Ok(filtered)
    }

    async fn setup_tables(&self, client: &Client, database: &str) -> Result<()> {
        let create_orderbook = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.okx_orderbook (
                ts DateTime64(3),
                symbol LowCardinality(String),
                side Enum8('bid'=1, 'ask'=2),
                price Float64,
                qty Float64,
                update_id Int64,
                checksum Int64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, side, price)
            "#,
            database
        );
        client.query(&create_orderbook).execute().await?;

        let create_trades = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.okx_trades (
                ts DateTime64(3),
                symbol LowCardinality(String),
                trade_id String,
                price Float64,
                qty Float64,
                side Enum8('buy'=1, 'sell'=2)
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, trade_id)
            "#,
            database
        );
        client.query(&create_trades).execute().await?;

        let create_l1 = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.okx_l1 (
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
        client.query(&create_l1).execute().await?;

        let create_ticker = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.okx_ticker (
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
        client.query(&create_ticker).execute().await?;

        let create_fut_orderbook = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.okx_futures_orderbook (
                ts DateTime64(3),
                symbol LowCardinality(String),
                side Enum8('bid'=1, 'ask'=2),
                price Float64,
                qty Float64,
                update_id Int64,
                checksum Int64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, side, price)
            "#,
            database
        );
        client.query(&create_fut_orderbook).execute().await?;

        let create_fut_trades = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.okx_futures_trades (
                ts DateTime64(3),
                symbol LowCardinality(String),
                trade_id String,
                price Float64,
                qty Float64,
                side Enum8('buy'=1, 'sell'=2)
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, trade_id)
            "#,
            database
        );
        client.query(&create_fut_trades).execute().await?;

        let create_fut_l1 = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.okx_futures_l1 (
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

        let create_fut_ticker = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.okx_futures_ticker (
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

        let create_lob_view = format!(
            r#"
            CREATE VIEW IF NOT EXISTS {}.okx_lob AS
            SELECT toDateTime64(ts / 1000000, 6) AS ts,
                   symbol,
                   last_id,
                   bids_px,
                   bids_qty,
                   asks_px,
                   asks_qty,
                   source
            FROM {}.snapshot_books
            WHERE venue LIKE 'okx%'
            "#,
            database, database
        );
        client.query(&create_lob_view).execute().await?;

        Ok(())
    }

    async fn process_message(&self, message: &str, buffers: &mut MessageBuffers) -> Result<()> {
        let parsed: Value = match serde_json::from_str(message) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };

        if parsed.get("event").is_some() {
            // Subscription ack / error / ping-pong handling is not required here
            return Ok(());
        }

        let arg = parsed.get("arg").and_then(|x| x.as_object());
        let channel = arg
            .and_then(|x| x.get("channel"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let inst_id = arg
            .and_then(|x| x.get("instId"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if inst_id.is_empty() {
            return Ok(());
        }
        let inst_type = arg
            .and_then(|x| x.get("instType"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let is_deriv = Self::is_derivatives(inst_type, inst_id);

        match channel {
            ch if ch.starts_with("books") => {
                if let Some(data_arr) = parsed.get("data").and_then(|x| x.as_array()) {
                    for entry in data_arr {
                        let ts_ms = entry
                            .get("ts")
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<i64>().ok())
                            .unwrap_or_else(|| Utc::now().timestamp_millis());
                        let checksum = entry
                            .get("checksum")
                            .and_then(|x| x.as_i64())
                            .or_else(|| {
                                entry
                                    .get("checksum")
                                    .and_then(|x| x.as_str())
                                    .and_then(|s| s.parse::<i64>().ok())
                            })
                            .unwrap_or(0);
                        let ts = Self::ms_to_datetime(ts_ms);

                        let mut depth_rows: Vec<(&'static str, f64, f64)> = Vec::new();
                        let (bids_px, bids_qty, asks_px, asks_qty, last_id, best_bid, best_ask) = {
                            let mut books = self.orderbooks.write().await;
                            let book = books.entry(inst_id.to_string()).or_default();

                            if let Some(bids) = entry.get("bids").and_then(|x| x.as_array()) {
                                for level in bids {
                                    if let Some((price, qty)) = Self::parse_level(level) {
                                        let key = OrderedFloat(price);
                                        if qty == 0.0 {
                                            book.bids.remove(&key);
                                        } else {
                                            book.bids.insert(key, qty);
                                        }
                                        depth_rows.push(("bid", price, qty));
                                    }
                                }
                            }
                            if let Some(asks) = entry.get("asks").and_then(|x| x.as_array()) {
                                for level in asks {
                                    if let Some((price, qty)) = Self::parse_level(level) {
                                        let key = OrderedFloat(price);
                                        if qty == 0.0 {
                                            book.asks.remove(&key);
                                        } else {
                                            book.asks.insert(key, qty);
                                        }
                                        depth_rows.push(("ask", price, qty));
                                    }
                                }
                            }
                            book.last_update_id = ts_ms;

                            let n = self.top_n;
                            let bids_vec: Vec<(f64, f64)> = book
                                .bids
                                .iter()
                                .rev()
                                .take(n)
                                .map(|(p, q)| (p.0, *q))
                                .collect();
                            let asks_vec: Vec<(f64, f64)> =
                                book.asks.iter().take(n).map(|(p, q)| (p.0, *q)).collect();
                            let bids_qty_vec: Vec<f64> =
                                bids_vec.iter().map(|(_, qty)| *qty).collect::<Vec<f64>>();
                            let asks_qty_vec: Vec<f64> =
                                asks_vec.iter().map(|(_, qty)| *qty).collect::<Vec<f64>>();
                            let best_bid = book
                                .bids
                                .iter()
                                .rev()
                                .next()
                                .map(|(p, q)| (p.0, *q))
                                .unwrap_or((0.0, 0.0));
                            let best_ask = book
                                .asks
                                .iter()
                                .next()
                                .map(|(p, q)| (p.0, *q))
                                .unwrap_or((0.0, 0.0));
                            (
                                bids_vec,
                                bids_qty_vec,
                                asks_vec,
                                asks_qty_vec,
                                book.last_update_id,
                                best_bid,
                                best_ask,
                            )
                        };

                        tracing::debug!(
                            "okx books update: channel={} instId={} bids={} asks={}",
                            channel,
                            inst_id,
                            entry
                                .get("bids")
                                .and_then(|x| x.as_array())
                                .map(|v| v.len())
                                .unwrap_or(0),
                            entry
                                .get("asks")
                                .and_then(|x| x.as_array())
                                .map(|v| v.len())
                                .unwrap_or(0)
                        );

                        for (side, price, qty) in depth_rows {
                            let row = serde_json::json!({
                                "ts": ts,
                                "symbol": inst_id,
                                "side": side,
                                "price": price,
                                "qty": qty,
                                "update_id": ts_ms,
                                "checksum": checksum,
                            });
                            if is_deriv {
                                buffers.push_futures_orderbook(serde_json::to_string(&row)?);
                            } else {
                                buffers.push_spot_orderbook(serde_json::to_string(&row)?);
                            }
                        }

                        let bids_px_only: Vec<f64> = bids_px.iter().map(|(px, _)| *px).collect();
                        let asks_px_only: Vec<f64> = asks_px.iter().map(|(px, _)| *px).collect();
                        let (bid_px, bid_qty) = best_bid;
                        let (ask_px, ask_qty) = best_ask;
                        if bid_px > 0.0 || ask_px > 0.0 {
                            let l1_row = serde_json::json!({
                                "ts": ts,
                                "symbol": inst_id,
                                "bid_px": bid_px,
                                "bid_qty": bid_qty,
                                "ask_px": ask_px,
                                "ask_qty": ask_qty,
                            });
                            if is_deriv {
                                buffers.push_futures_l1(serde_json::to_string(&l1_row)?);
                            } else {
                                buffers.push_spot_l1(serde_json::to_string(&l1_row)?);
                            }
                        }

                        let snapshot = serde_json::json!({
                            "ts": (ts_ms as u64) * 1000,
                            "symbol": inst_id,
                            "venue": if is_deriv { "okx_deriv" } else { "okx_spot" },
                            "last_id": last_id,
                            "bids_px": bids_px_only,
                            "bids_qty": bids_qty,
                            "asks_px": asks_px_only,
                            "asks_qty": asks_qty,
                            "source": channel,
                        });
                        buffers
                            .spot
                            .snapshots
                            .push(serde_json::to_string(&snapshot)?);
                    }
                }
            }
            ch if ch.contains("trade") => {
                if let Some(data_arr) = parsed.get("data").and_then(|x| x.as_array()) {
                    for trade in data_arr {
                        let trade_id = trade
                            .get("tradeId")
                            .or_else(|| trade.get("tsId"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        let ts_ms = trade
                            .get("ts")
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<i64>().ok())
                            .unwrap_or_else(|| Utc::now().timestamp_millis());
                        let price = trade
                            .get("px")
                            .or_else(|| trade.get("price"))
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let qty = trade
                            .get("sz")
                            .or_else(|| trade.get("size"))
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let side = trade.get("side").and_then(|x| x.as_str()).unwrap_or("buy");
                        if qty <= 0.0 {
                            continue;
                        }
                        let cache_key = format!("{}#{}", inst_id, trade_id);
                        let mut cache = self.trades_cache.write().await;
                        if cache.contains(&cache_key) {
                            continue;
                        }
                        cache.insert(cache_key);
                        drop(cache);

                        let ts = Self::ms_to_datetime(ts_ms);
                        let row = serde_json::json!({
                            "ts": ts,
                            "symbol": inst_id,
                            "trade_id": trade_id,
                            "price": price,
                            "qty": qty,
                            "side": if side.eq_ignore_ascii_case("buy") { "buy" } else { "sell" },
                        });
                        if is_deriv {
                            buffers.push_futures_trades(serde_json::to_string(&row)?);
                        } else {
                            buffers.push_spot_trades(serde_json::to_string(&row)?);
                        }
                    }
                }
            }
            ch if ch.contains("ticker") => {
                if let Some(data_arr) = parsed.get("data").and_then(|x| x.as_array()) {
                    for ticker in data_arr {
                        let ts_ms = ticker
                            .get("ts")
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<i64>().ok())
                            .unwrap_or_else(|| Utc::now().timestamp_millis());
                        let ts = Self::ms_to_datetime(ts_ms);
                        let last = ticker
                            .get("last")
                            .or_else(|| ticker.get("lastPx"))
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let open = ticker
                            .get("open24h")
                            .or_else(|| ticker.get("open"))
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let high = ticker
                            .get("high24h")
                            .or_else(|| ticker.get("high"))
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let low = ticker
                            .get("low24h")
                            .or_else(|| ticker.get("low"))
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let volume = ticker
                            .get("vol24h")
                            .or_else(|| ticker.get("vol"))
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let quote_volume = ticker
                            .get("volCcy24h")
                            .or_else(|| ticker.get("volUSDT"))
                            .and_then(|x| x.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let row = serde_json::json!({
                            "ts": ts,
                            "symbol": inst_id,
                            "last": last,
                            "open": open,
                            "high": high,
                            "low": low,
                            "volume": volume,
                            "quote_volume": quote_volume,
                        });
                        if is_deriv {
                            buffers.push_futures_ticker(serde_json::to_string(&row)?);
                        } else {
                            buffers.push_spot_ticker(serde_json::to_string(&row)?);
                        }
                    }
                }
            }
            _ => {}
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
            .flush_spot_table(&sink, "okx_orderbook", |spot| &mut spot.orderbook)
            .await?;
        buffers
            .flush_spot_table(&sink, "okx_trades", |spot| &mut spot.trades)
            .await?;
        buffers
            .flush_spot_table(&sink, "okx_l1", |spot| &mut spot.l1)
            .await?;
        buffers
            .flush_spot_table(&sink, "okx_ticker", |spot| &mut spot.ticker)
            .await?;

        buffers
            .flush_futures_table(&sink, "okx_futures_orderbook", |f| &mut f.orderbook)
            .await?;
        buffers
            .flush_futures_table(&sink, "okx_futures_trades", |f| &mut f.trades)
            .await?;
        buffers
            .flush_futures_table(&sink, "okx_futures_l1", |f| &mut f.l1)
            .await?;
        buffers
            .flush_futures_table(&sink, "okx_futures_ticker", |f| &mut f.ticker)
            .await?;

        buffers
            .flush_spot_table(&sink, "snapshot_books", |spot| &mut spot.snapshots)
            .await?;

        let _ = crate::spool::drain(&sink).await;
        Ok(())
    }

    async fn subscription_messages(&self, symbols: &[String]) -> Result<Vec<String>> {
        let base_channels: Vec<String> = std::env::var("OKX_CHANNELS")
            .unwrap_or_else(|_| "books5,trades,tickers".to_string())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        tracing::debug!("okx base channels: {:?}", base_channels);

        let mut args = Vec::new();
        for sym in symbols {
            let (inst_id, inst_type) = Self::normalize_symbol(sym);
            for channel in &base_channels {
                let mut obj = serde_json::json!({"channel": channel, "instId": inst_id.clone()});
                if !inst_type.is_empty() {
                    obj["instType"] = serde_json::Value::String(inst_type.clone());
                }
                args.push(obj);
            }
        }
        if args.is_empty() {
            return Ok(vec![]);
        }

        let chunk_size = std::env::var("OKX_SUB_CHUNK")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(20);

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

    fn heartbeat_interval(&self) -> Duration {
        Duration::from_secs(15)
    }
}
