#![allow(unexpected_cfgs)]

mod db;
mod exchanges;
mod spool;

use anyhow::Result;
use clap::Parser;
use clickhouse::Client;
use exchanges::{
    create_exchange, parse_symbols_override, DepthMode, Exchange, ExchangeContext, MessageBuffers,
};
use futures::{SinkExt, StreamExt};
use http::Request;
use rand::Rng;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

#[derive(Default, Clone, Debug)]
struct SymbolGap {
    last_ts: Option<i64>,
    max_gap_ms: i64,
    count: u64,
}

#[derive(Default, Clone, Debug)]
struct GapStats {
    by_trade: HashMap<String, SymbolGap>,
    by_book: HashMap<String, SymbolGap>,
}

impl GapStats {
    fn process(&mut self, payload: &str, session: &str) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
            if let Some(stream) = value.get("stream").and_then(|s| s.as_str()) {
                let data = value.get("data");
                if stream.ends_with("@trade") {
                    let symbol = stream.split('@').next().unwrap_or("").to_ascii_uppercase();
                    if let Some(event_time) = Self::extract_ts(data, &["E", "eventTime", "T"]) {
                        self.update_trade(&symbol, event_time, session);
                    }
                } else if stream.contains("@bookTicker") || stream.contains("@depth") {
                    let symbol = stream.split('@').next().unwrap_or("").to_ascii_uppercase();
                    if let Some(event_time) = Self::extract_ts(data, &["E", "eventTime", "T"]) {
                        self.update_book(&symbol, event_time, session);
                    }
                }
            } else if let Some(arg) = value.get("arg").and_then(|v| v.as_object()) {
                if let Some(channel) = arg.get("channel").and_then(|v| v.as_str()) {
                    let channel_lower = channel.to_ascii_lowercase();
                    if let Some(data_arr) = value.get("data").and_then(|d| d.as_array()) {
                        if channel_lower.contains("trade") {
                            for item in data_arr {
                                let symbol = Self::extract_symbol(item)
                                    .or_else(|| {
                                        arg.get("instId")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    .or_else(|| {
                                        arg.get("symbol")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    });
                                if let Some(ts) = Self::extract_ts_from_value(
                                    item,
                                    &["ts", "T", "time", "timestamp"],
                                ) {
                                    if let Some(sym) = symbol.clone() {
                                        self.update_trade(&sym, ts, session);
                                    }
                                    break;
                                }
                            }
                        } else if channel_lower.contains("books")
                            || channel_lower.contains("ticker")
                            || channel_lower.contains("quote")
                        {
                            for item in data_arr {
                                let symbol = Self::extract_symbol(item)
                                    .or_else(|| {
                                        arg.get("instId")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    .or_else(|| {
                                        arg.get("symbol")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    });
                                if let Some(ts) = Self::extract_ts_from_value(
                                    item,
                                    &["ts", "T", "time", "timestamp"],
                                ) {
                                    if let Some(sym) = symbol.clone() {
                                        self.update_book(&sym, ts, session);
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
            } else if let Some(event_type) = value.get("e").and_then(|v| v.as_str()) {
                if let Some(ts) =
                    Self::extract_ts_from_value(&value, &["E", "eventTime", "T", "ts", "time"])
                {
                    let symbol = Self::extract_symbol(&value);
                    let lower = event_type.to_ascii_lowercase();
                    if lower.contains("trade") {
                        if let Some(sym) = symbol {
                            self.update_trade(&sym, ts, session);
                        }
                    } else if lower.contains("book") || lower.contains("depth") {
                        if let Some(sym) = symbol {
                            self.update_book(&sym, ts, session);
                        }
                    }
                }
            }
        }
    }

    fn extract_ts(data: Option<&serde_json::Value>, keys: &[&str]) -> Option<i64> {
        let obj = data?.as_object()?;
        for key in keys {
            if let Some(v) = obj.get(*key) {
                if let Some(ms) = v.as_i64() {
                    return Some(ms);
                }
                if let Some(s) = v.as_str() {
                    if let Ok(parsed) = s.parse::<f64>() {
                        return Some(parsed as i64);
                    }
                }
            }
        }
        None
    }

    fn extract_ts_from_value(value: &serde_json::Value, keys: &[&str]) -> Option<i64> {
        if let Some(obj) = value.as_object() {
            for key in keys {
                if let Some(v) = obj.get(*key) {
                    if let Some(ms) = v.as_i64() {
                        return Some(ms);
                    }
                    if let Some(s) = v.as_str() {
                        if let Ok(parsed) = s.parse::<f64>() {
                            return Some(parsed as i64);
                        }
                    }
                }
            }
        }
        None
    }

    fn extract_symbol(value: &serde_json::Value) -> Option<String> {
        // Try common symbol keys in order
        value
            .get("s")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                value
                    .get("symbol")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .or_else(|| {
                value
                    .get("instId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
    }

    fn bump(
        map: &mut HashMap<String, SymbolGap>,
        symbol: &str,
        ts: i64,
        session: &str,
        kind: &str,
    ) {
        let entry = map.entry(symbol.to_string()).or_default();
        if let Some(prev) = entry.last_ts.replace(ts) {
            let gap = ts - prev;
            if gap > entry.max_gap_ms {
                entry.max_gap_ms = gap;
                info!(
                    "[{}|{}|{}] 新最大 gap = {} ms (t0={}, t1={})",
                    session, kind, symbol, gap, prev, ts
                );
            }
        }
        entry.count += 1;
    }

    fn update_trade(&mut self, symbol: &str, event_time: i64, session: &str) {
        Self::bump(&mut self.by_trade, symbol, event_time, session, "trade");
    }

    fn update_book(&mut self, symbol: &str, event_time: i64, session: &str) {
        Self::bump(&mut self.by_book, symbol, event_time, session, "book");
    }

    fn summary(&self) -> (u64, i64, u64, i64) {
        let trade_count: u64 = self.by_trade.values().map(|s| s.count).sum();
        let max_trade_gap = self
            .by_trade
            .values()
            .map(|s| s.max_gap_ms)
            .max()
            .unwrap_or(0);
        let book_count: u64 = self.by_book.values().map(|s| s.count).sum();
        let max_book_gap = self
            .by_book
            .values()
            .map(|s| s.max_gap_ms)
            .max()
            .unwrap_or(0);
        (trade_count, max_trade_gap, book_count, max_book_gap)
    }
}

fn resolve_max_backoff_secs(exchange: &str) -> u64 {
    let env_override = std::env::var("COLLECTOR_MAX_BACKOFF_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0);
    if let Some(value) = env_override {
        return value;
    }

    match exchange.to_ascii_lowercase().as_str() {
        // 高頻來源，一旦掉線應迅速重連
        "binance" | "binance_futures" | "binance-futures" | "binancefutures" => 5,
        "bybit" | "bitget" | "bitget_futures" | "bitget-futures" | "bitgetfutures" => 5,
        "okx" | "okx_futures" | "okx-futures" | "okxfutures" => 5,
        "asterdex" | "hyperliquid" => 5,
        _ => 30,
    }
}

fn chunk_size_for_exchange(exchange: &str) -> usize {
    let env_key = format!(
        "{}_WS_SYMBOLS_PER_CONN",
        exchange.to_ascii_uppercase().replace('-', "_")
    );
    if let Ok(value) = std::env::var(&env_key) {
        if let Ok(parsed) = value.parse::<usize>() {
            if parsed > 0 {
                return parsed;
            }
        }
    }
    match exchange.to_ascii_lowercase().as_str() {
        "binance" | "binance_futures" | "binance-futures" | "binancefutures" => 3,
        "bitget" | "bitget_futures" | "bitget-futures" | "bitgetfutures" => 4,
        "bybit" => 8,
        "asterdex" => 12,
        _ => 0,
    }
}

fn chunk_symbols(symbols: &[String], chunk_size: usize) -> Vec<Vec<String>> {
    if chunk_size == 0 || symbols.len() <= chunk_size {
        return vec![symbols.to_vec()];
    }
    symbols
        .chunks(chunk_size)
        .map(|chunk| chunk.to_vec())
        .collect()
}

fn normalize_symbols(symbols: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ordered = Vec::new();
    for sym in symbols {
        let upper = sym.to_ascii_uppercase();
        if seen.insert(upper.clone()) {
            ordered.push(upper);
        }
    }
    ordered
}

fn filter_with_supported(
    exchange: &str,
    symbols: Vec<String>,
    supported: &HashSet<String>,
) -> Vec<String> {
    if supported.is_empty() {
        return symbols;
    }
    let mut kept = Vec::with_capacity(symbols.len());
    let mut skipped = Vec::new();
    for sym in symbols {
        if supported.contains(&sym) {
            kept.push(sym);
        } else {
            skipped.push(sym);
        }
    }
    if !skipped.is_empty() {
        warn!("[{}] 移除不支援的交易對: {:?}", exchange, skipped);
    }
    kept
}

async fn filter_exchange_symbols(exchange: &str, symbols: Vec<String>) -> Vec<String> {
    if symbols.is_empty() {
        return symbols;
    }
    let normalized = normalize_symbols(symbols);
    let exchange_lc = exchange.to_ascii_lowercase();

    match exchange_lc.as_str() {
        "binance" | "binance_futures" | "binance-futures" | "binancefutures" => {
            match fetch_binance_symbols().await {
                Ok(supported) => filter_with_supported(exchange, normalized, &supported),
                Err(err) => {
                    warn!(
                        "[{}] 無法取得 Binance 交易對列表: {}，使用原始白名單",
                        exchange, err
                    );
                    normalized
                }
            }
        }
        "bybit" => match fetch_bybit_symbols().await {
            Ok(supported) => filter_with_supported(exchange, normalized, &supported),
            Err(err) => {
                warn!(
                    "[{}] 無法取得 Bybit 交易對列表: {}，使用原始白名單",
                    exchange, err
                );
                normalized
            }
        },
        "bitget" | "bitget_futures" | "bitget-futures" | "bitgetfutures" => {
            match fetch_bitget_symbols().await {
                Ok(supported) => filter_with_supported(exchange, normalized, &supported),
                Err(err) => {
                    warn!(
                        "[{}] 無法取得 Bitget 交易對列表: {}，使用原始白名單",
                        exchange, err
                    );
                    normalized
                }
            }
        }
        _ => normalized,
    }
}

async fn fetch_binance_symbols() -> Result<HashSet<String>> {
    #[derive(Deserialize)]
    struct BinanceSymbolInfo {
        symbol: String,
        status: String,
        #[serde(rename = "quoteAsset")]
        quote_asset: String,
    }

    #[derive(Deserialize)]
    struct BinanceExchangeInfo {
        symbols: Vec<BinanceSymbolInfo>,
    }

    let resp: BinanceExchangeInfo = reqwest::Client::new()
        .get("https://api.binance.com/api/v3/exchangeInfo")
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .json()
        .await?;

    let set = resp
        .symbols
        .into_iter()
        .filter(|s| s.status == "TRADING" && s.quote_asset.eq_ignore_ascii_case("USDT"))
        .map(|s| s.symbol.to_ascii_uppercase())
        .collect();
    Ok(set)
}

async fn fetch_bybit_symbols() -> Result<HashSet<String>> {
    #[derive(Deserialize)]
    struct BybitTickerItem {
        symbol: String,
    }

    #[derive(Deserialize)]
    struct BybitTickerResult {
        list: Vec<BybitTickerItem>,
    }

    #[derive(Deserialize)]
    struct BybitTickerResponse {
        result: BybitTickerResult,
    }

    let resp: BybitTickerResponse = reqwest::Client::new()
        .get("https://api.bybit.com/v5/market/tickers?category=spot")
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .json()
        .await?;

    let set = resp
        .result
        .list
        .into_iter()
        .map(|item| item.symbol.to_ascii_uppercase())
        .collect();
    Ok(set)
}

async fn fetch_bitget_symbols() -> Result<HashSet<String>> {
    #[derive(Deserialize)]
    struct BitgetSymbolItem {
        symbol: String,
        #[serde(rename = "quoteCoin")]
        quote_coin: String,
        status: String,
    }

    #[derive(Deserialize)]
    struct BitgetSymbolResponse {
        data: Vec<BitgetSymbolItem>,
    }

    let resp: BitgetSymbolResponse = reqwest::Client::new()
        .get("https://api.bitget.com/api/v2/spot/public/symbols")
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .json()
        .await?;

    let set = resp
        .data
        .into_iter()
        .filter(|item| item.quote_coin.eq_ignore_ascii_case("USDT") && item.status == "online")
        .map(|item| item.symbol.to_ascii_uppercase())
        .collect();
    Ok(set)
}

#[derive(Parser, Debug)]
#[command(author, version, about = "多交易所數據收集器：WebSocket → ClickHouse")]
struct Args {
    /// 交易所列表（逗號分隔，支持: bitget, binance, bybit, okx 等）
    #[arg(long, default_value = "bitget")]
    exchanges: String,

    /// 交易對列表（逗號分隔）
    #[arg(long, default_value = "BTCUSDT,ETHUSDT")]
    symbols: String,

    /// 深度模式 (limited/incremental/both)
    #[arg(long, default_value = "limited")]
    depth_mode: String,

    /// 深度檔位數
    #[arg(long, default_value = "15")]
    depth_levels: usize,

    /// ClickHouse URL
    #[arg(
        long,
        default_value = "https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
    )]
    ch_url: String,

    /// ClickHouse 資料庫
    #[arg(long, default_value = "hft_db")]
    database: String,

    /// 批量大小
    #[arg(long, default_value = "1000")]
    batch_size: usize,

    /// Flush 間隔（毫秒）
    #[arg(long, default_value = "2000")]
    flush_ms: u64,

    /// Dry-run 模式
    #[arg(long, default_value_t = false)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

    // 解析交易所列表
    let exchange_names: Vec<String> = args
        .exchanges
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    // 環境變數覆寫（SYMBOLS/SYMBOLS_FILE）優先於 CLI
    let symbols: Vec<String> = parse_symbols_override().unwrap_or_else(|| {
        args.symbols
            .split(',')
            .map(|s| s.trim().to_string())
            .collect()
    });

    // ClickHouse 配置
    let ch_user = std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "default".to_string());
    let ch_password = std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_else(|_| "".to_string());

    db::init_db_config(db::DbConfig::clickhouse(
        args.ch_url.clone(),
        args.database.clone(),
        ch_user.clone(),
        ch_password.clone(),
        args.dry_run,
    ));

    // dry-run 模式提示
    if args.dry_run {
        info!("🔬 Dry-run 模式：跳過 ClickHouse 連接");
    }

    // 為每個交易所啟動數據收集任務
    let mut tasks = vec![];

    let is_dry_run = args.dry_run;

    for exchange_name in &exchange_names {
        let exchange_name = exchange_name.clone();
        let symbols = symbols.clone();
        let depth_mode = DepthMode::from_str(&args.depth_mode);
        let depth_levels = args.depth_levels;
        let ch_url = args.ch_url.clone();
        let database = args.database.clone();
        let ch_user = ch_user.clone();
        let ch_password = ch_password.clone();
        let batch_size = args.batch_size;
        let flush_ms = args.flush_ms;
        let dry_run = is_dry_run;

        let task = tokio::spawn(async move {
            if let Err(e) = run_exchange_collector(
                exchange_name.clone(),
                symbols,
                depth_mode,
                depth_levels,
                database,
                ch_url,
                ch_user,
                ch_password,
                batch_size,
                flush_ms,
                dry_run,
            )
            .await
            {
                error!("交易所 {} 收集器失敗: {:?}", exchange_name, e);
            }
        });

        tasks.push(task);
    }

    if is_dry_run {
        for task in tasks {
            let _ = task.await;
        }
        Ok(())
    } else {
        futures::future::join_all(tasks).await;
        Ok(())
    }
}

async fn run_exchange_collector(
    exchange_name: String,
    symbols: Vec<String>,
    depth_mode: DepthMode,
    depth_levels: usize,
    database: String,
    ch_url: String,
    ch_user: String,
    ch_password: String,
    batch_size: usize,
    flush_ms: u64,
    dry_run: bool,
) -> Result<()> {
    let symbols = filter_exchange_symbols(&exchange_name, symbols).await;
    if symbols.is_empty() {
        warn!("[{}] 無可用交易對，跳過啟動", exchange_name);
        return Ok(());
    }

    let chunk_size = chunk_size_for_exchange(&exchange_name);
    let symbol_chunks = chunk_symbols(&symbols, chunk_size);
    info!(
        "[{}] 分配 {} 組 WebSocket 連線（每組上限 {} 個交易對）",
        exchange_name,
        symbol_chunks.len(),
        if chunk_size == 0 {
            symbols.len()
        } else {
            chunk_size
        }
    );

    // 確保資料庫與表存在（IF NOT EXISTS，僅缺失時創建）
    if !dry_run {
        let mut client = Client::default().with_url(&ch_url);
        if !ch_password.is_empty() {
            client = client.with_user(&ch_user).with_password(&ch_password);
        }
        client
            .query(&format!("CREATE DATABASE IF NOT EXISTS {}", database))
            .execute()
            .await?;
        let ctx = Arc::new(ExchangeContext::new(
            depth_mode,
            depth_levels,
            Some(Arc::new(symbols.clone())),
        ));
        let exchange_for_setup = create_exchange(&exchange_name, ctx)?;
        exchange_for_setup.setup_tables(&client, &database).await?;
    }

    // 驗證模式（僅日誌匯總，不寫入 CH）
    let validate_mode = matches!(
        std::env::var("COLLECTOR_VALIDATE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    );

    let chunk_total = symbol_chunks.len();
    let mut session_handles = Vec::new();
    // 是否在單一進程內拆分不同資料流（trades vs book/depth）為獨立連線
    let split_profiles = matches!(
        std::env::var("COLLECTOR_SPLIT_PROFILES")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    );
    // 解析當前 exchange 的整體 profile，以便決定如何拆分
    let base_profile = exchanges::StreamProfile::resolve(&exchange_name);
    for (idx, chunk) in symbol_chunks.into_iter().enumerate() {
        let session_base = if chunk_total == 1 {
            exchange_name.clone()
        } else {
            format!("{}#{}", exchange_name, idx + 1)
        };

        // 如果開啟拆分，且同時包含 trades 與 (book 或 depth)，則開兩條連線
        if split_profiles && base_profile.trades && (base_profile.book || base_profile.depth) {
            let mut spawn_profile =
                |label_suffix: &str, prof: exchanges::StreamProfile| -> Result<()> {
                    let ctx = Arc::new(exchanges::ExchangeContext::with_profile(
                        depth_mode,
                        depth_levels,
                        Some(Arc::new(chunk.clone())),
                        prof,
                    ));
                    let exchange = create_exchange(&exchange_name, ctx)?;
                    let session_label = format!("{}|{}", session_base, label_suffix);
                    info!(
                        "啟動 {} 收集器: {:?} (深度模式: {:?}, 檔位: {}, profile: {:?})",
                        session_label, chunk, depth_mode, depth_levels, label_suffix
                    );
                    let database_clone = database.clone();
                    let session_symbols = chunk.clone();
                    let handle = tokio::spawn(run_exchange_session(
                        session_label,
                        exchange,
                        session_symbols,
                        database_clone,
                        batch_size,
                        flush_ms,
                        dry_run,
                        validate_mode,
                    ));
                    if dry_run {
                        session_handles.push(handle);
                    }
                    Ok(())
                };

            // trades-only
            spawn_profile(
                "trades",
                exchanges::StreamProfile {
                    trades: true,
                    book: false,
                    depth: false,
                },
            )?;
            // book/depth-only
            spawn_profile(
                "book",
                exchanges::StreamProfile {
                    trades: false,
                    book: base_profile.book,
                    depth: base_profile.depth,
                },
            )?;
        } else {
            // 不拆分：按原本的單一連線處理
            let ctx = Arc::new(ExchangeContext::with_profile(
                depth_mode,
                depth_levels,
                Some(Arc::new(chunk.clone())),
                base_profile,
            ));
            let exchange = create_exchange(&exchange_name, ctx)?;
            let session_label = session_base.clone();
            info!(
                "啟動 {} 收集器: {:?} (深度模式: {:?}, 檔位: {})",
                session_label, chunk, depth_mode, depth_levels
            );
            let database_clone = database.clone();
            let session_symbols = chunk.clone();
            let handle = tokio::spawn(run_exchange_session(
                session_label,
                exchange,
                session_symbols,
                database_clone,
                batch_size,
                flush_ms,
                dry_run,
                validate_mode,
            ));
            if dry_run {
                session_handles.push(handle);
            }
        }
    }

    if dry_run {
        for handle in session_handles {
            let _ = handle.await;
        }
        Ok(())
    } else {
        futures::future::pending::<()>().await;
        #[allow(unreachable_code)]
        Ok(())
    }
}

async fn run_exchange_session(
    session_label: String,
    exchange: Box<dyn Exchange + Send + Sync>,
    symbols: Vec<String>,
    database: String,
    batch_size: usize,
    flush_ms: u64,
    dry_run: bool,
    validate_mode: bool,
) {
    let exchange_name = exchange.name();
    let futures_enabled = exchange.requires_futures_buffers();
    let mut backoff_secs: u64 = 1;
    let max_backoff: u64 = resolve_max_backoff_secs(exchange_name);
    let diagnostics = exchange.stream_diagnostics();

    let mut gap_stats = GapStats::default();
    let log_gap_stats = dry_run && (diagnostics.trades || diagnostics.book || diagnostics.depth);

    loop {
        let plan = match exchange.websocket_plan(&symbols).await {
            Ok(p) => p,
            Err(e) => {
                error!("[{}] 生成訂閱計劃失敗: {:?}", session_label, e);
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(max_backoff);
                continue;
            }
        };

        let ws_url = plan.url;
        info!("[{}] 連接 WebSocket: {}", session_label, ws_url);

        let custom_headers = exchange.websocket_headers();
        let connect_res = if !custom_headers.is_empty() {
            let mut req = Request::builder()
                .uri(&ws_url)
                .header(
                    "Host",
                    ws_url
                        .split("://")
                        .nth(1)
                        .unwrap_or("")
                        .split('/')
                        .next()
                        .unwrap_or(""),
                )
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Version", "13");

            let key = {
                use base64::Engine;
                let mut nonce = [0u8; 16];
                rand::thread_rng().fill(&mut nonce);
                base64::engine::general_purpose::STANDARD.encode(&nonce)
            };
            req = req.header("Sec-WebSocket-Key", key);

            for (k, v) in custom_headers {
                req = req.header(k, v);
            }

            match req.body(()) {
                Ok(r) => connect_async(r).await,
                Err(e) => {
                    error!("[{}] 構建 WebSocket 請求失敗: {:?}", session_label, e);
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(max_backoff);
                    continue;
                }
            }
        } else {
            connect_async(&ws_url).await
        };

        let (mut ws_sender, mut ws_receiver) = match connect_res {
            Ok((ws_stream, _)) => {
                backoff_secs = 1;
                ws_stream.split()
            }
            Err(e) => {
                error!("[{}] 連接 WebSocket 失敗: {:?}", session_label, e);
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(max_backoff);
                continue;
            }
        };

        for payload in plan.subscribe_messages {
            info!("[{}] 發送訂閱消息: {}", session_label, payload);
            if let Err(e) = ws_sender.send(Message::Text(payload.clone())).await {
                error!("[{}] 發送訂閱消息失敗: {:?}", session_label, e);
            }
        }

        let mut buffers = MessageBuffers::new(futures_enabled);
        let mut heartbeat_timer = interval(exchange.heartbeat_interval());
        let mut flush_timer = interval(Duration::from_millis(flush_ms));

        loop {
            tokio::select! {
                Some(msg) = ws_receiver.next() => {
                    match msg {
                        Ok(Message::Text(text)) => {
                            // Bitget JSON ping -> pong
                            if exchange_name == "bitget" && text.contains("\"op\":\"ping\"") {
                                if let Err(e) = ws_sender.send(Message::Text("{\"op\":\"pong\"}".to_string())).await {
                                    warn!("[{}] 回應 Bitget pong 失敗: {:?}", session_label, e);
                                    break;
                                }
                                continue;
                            }
                            if log_gap_stats {
                                gap_stats.process(&text, &session_label);
                            }
                            if let Err(e) = exchange.process_message(&text, &mut buffers).await {
                                warn!(
                                    "[{}] 處理消息失敗: {:?}",
                                    session_label, e
                                );
                            }
                            if buffers.len() >= batch_size {
                                if !dry_run {
                                    if let Err(e) = exchange.flush_data(&database, &mut buffers).await {
                                        error!(
                                            "[{}] Flush 失敗: {:?}",
                                            session_label, e
                                        );
                                    }
                                } else if validate_mode {
                                    let spot = &buffers.spot;
                                    info!(
                                        "✅ [{}] spot: ob={} trd={} l1={} tkr={} snap={}",
                                        session_label,
                                        spot.orderbook.len(),
                                        spot.trades.len(),
                                        spot.l1.len(),
                                        spot.ticker.len(),
                                        spot.snapshots.len()
                                    );
                                    if let Some(f) = buffers.futures_ref() {
                                        info!(
                                            "✅ [{}] futures: ob={} trd={} l1={} tkr={} snap={}",
                                            session_label,
                                            f.orderbook.len(),
                                            f.trades.len(),
                                            f.l1.len(),
                                            f.ticker.len(),
                                            f.snapshots.len()
                                        );
                                    }
                                    buffers = MessageBuffers::new(futures_enabled);
                                } else {
                                    info!(
                                        "📦 [{}] 緩衝區達到 {} 條",
                                        session_label,
                                        buffers.len()
                                    );
                                    buffers = MessageBuffers::new(futures_enabled);
                                }
                                if log_gap_stats {
                                    let (tc, tg, bc, bg) = gap_stats.summary();
                                    info!(
                                        "[{}] gap統計 trades={} max_gap={}ms book={} max_gap={}ms",
                                        session_label, tc, tg, bc, bg
                                    );
                                }
                            }
                        }
                        Ok(Message::Binary(_)) => {}
                        Ok(Message::Ping(payload)) => {
                            if let Err(e) = ws_sender.send(Message::Pong(payload)).await {
                                error!(
                                    "[{}] Pong 失敗: {:?}",
                                    session_label, e
                                );
                                break;
                            }
                        }
                        Ok(Message::Pong(_)) => {}
                        Ok(Message::Close(_)) => {
                            warn!(
                                "[{}] WebSocket 連接關閉",
                                session_label
                            );
                            break;
                        }
                        Err(e) => {
                            error!(
                                "[{}] WebSocket 錯誤: {:?}",
                                session_label, e
                            );
                            break;
                        }
                        _ => {}
                    }
                }
                _ = heartbeat_timer.tick() => {
                    if let Err(e) = ws_sender.send(Message::Ping(vec![])).await {
                        error!(
                            "[{}] 心跳失敗: {:?}",
                            session_label, e
                        );
                        break;
                    }
                }
                _ = flush_timer.tick() => {
                    if buffers.len() > 0 {
                        if !dry_run {
                            if let Err(e) = exchange.flush_data(&database, &mut buffers).await {
                                error!(
                                    "[{}] 定期 Flush 失敗: {:?}",
                                    session_label, e
                                );
                            }
                        } else if validate_mode {
                            let spot = &buffers.spot;
                            info!(
                                "✅ [{}|Tick] spot: ob={} trd={} l1={} tkr={} snap={}",
                                session_label,
                                spot.orderbook.len(),
                                spot.trades.len(),
                                spot.l1.len(),
                                spot.ticker.len(),
                                spot.snapshots.len()
                            );
                            if let Some(f) = buffers.futures_ref() {
                                info!(
                                    "✅ [{}|Tick] futures: ob={} trd={} l1={} tkr={} snap={}",
                                    session_label,
                                    f.orderbook.len(),
                                    f.trades.len(),
                                    f.l1.len(),
                                    f.ticker.len(),
                                    f.snapshots.len()
                                );
                            }
                            buffers = MessageBuffers::new(futures_enabled);
                            if log_gap_stats {
                                let (tc, tg, bc, bg) = gap_stats.summary();
                                info!(
                                    "[{}] gap統計 trades={} max_gap={}ms book={} max_gap={}ms",
                                    session_label, tc, tg, bc, bg
                                );
                            }
                        }
                    }
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(max_backoff);
    }
}
