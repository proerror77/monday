#[cfg(feature = "collector-asterdex")]
pub mod asterdex;
#[cfg(feature = "collector-binance")]
pub mod binance;
pub mod binance_common;
#[cfg(feature = "collector-binance-futures")]
pub mod binance_futures;
#[cfg(feature = "collector-bitget")]
pub mod bitget;
#[cfg(feature = "collector-bybit")]
pub mod bybit;
#[cfg(feature = "collector-hyperliquid")]
pub mod hyperliquid;
#[cfg(feature = "collector-okx")]
pub mod okx;

mod symbol_whitelist;
pub use symbol_whitelist::*;

use crate::db::DbSink;
use crate::spool;
use anyhow::Result;
use async_trait::async_trait;
use clickhouse::Client;
use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

/// 统一的深度配置枚举，避免依赖环境变量字符串
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthMode {
    Limited,
    Incremental,
    Both,
}

impl DepthMode {
    pub fn from_str(mode: &str) -> Self {
        match mode.to_ascii_lowercase().as_str() {
            "incremental" => DepthMode::Incremental,
            "both" => DepthMode::Both,
            _ => DepthMode::Limited,
        }
    }

    #[cfg_attr(
        not(any(
            feature = "collector-binance",
            feature = "collector-binance-futures",
            feature = "collector-okx"
        )),
        allow(dead_code)
    )]
    pub fn include_limited(self) -> bool {
        matches!(self, DepthMode::Limited | DepthMode::Both)
    }

    #[cfg_attr(
        not(any(
            feature = "collector-binance",
            feature = "collector-binance-futures",
            feature = "collector-okx"
        )),
        allow(dead_code)
    )]
    pub fn include_incremental(self) -> bool {
        matches!(self, DepthMode::Incremental | DepthMode::Both)
    }
}

#[derive(Clone, Debug)]
pub struct ExchangeContext {
    #[cfg_attr(
        not(any(
            feature = "collector-binance",
            feature = "collector-binance-futures",
            feature = "collector-okx"
        )),
        allow(dead_code)
    )]
    pub depth_mode: DepthMode,
    #[cfg_attr(
        not(any(
            feature = "collector-binance",
            feature = "collector-binance-futures",
            feature = "collector-okx"
        )),
        allow(dead_code)
    )]
    pub depth_levels: usize,
    #[allow(dead_code)]
    symbols_override: Option<Arc<Vec<String>>>,
    stream_profile: Option<StreamProfile>,
}

impl ExchangeContext {
    pub fn new(
        depth_mode: DepthMode,
        depth_levels: usize,
        symbols_override: Option<Arc<Vec<String>>>,
    ) -> Self {
        Self {
            depth_mode,
            depth_levels,
            symbols_override,
            stream_profile: None,
        }
    }

    #[allow(dead_code)]
    pub fn symbols_override(&self) -> Option<Arc<Vec<String>>> {
        self.symbols_override.as_ref().map(Arc::clone)
    }

    pub fn with_profile(
        depth_mode: DepthMode,
        depth_levels: usize,
        symbols_override: Option<Arc<Vec<String>>>,
        profile: StreamProfile,
    ) -> Self {
        Self {
            depth_mode,
            depth_levels,
            symbols_override,
            stream_profile: Some(profile),
        }
    }

    pub fn profile(&self, exchange: &str) -> StreamProfile {
        self.stream_profile
            .unwrap_or_else(|| StreamProfile::resolve(exchange))
    }
}

#[derive(Clone)]
pub struct WebsocketPlan {
    pub url: String,
    pub subscribe_messages: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StreamProfile {
    pub trades: bool,
    pub book: bool,
    pub depth: bool,
}

#[derive(Clone, Debug, Default)]
pub struct StreamDiagnostics {
    pub trades: bool,
    pub book: bool,
    pub depth: bool,
}

impl StreamProfile {
    fn raw_profile(exchange: &str) -> Option<String> {
        let specific_key = exchange.to_ascii_uppercase().replace('-', "_") + "_STREAM_PROFILE";
        std::env::var(&specific_key)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("STREAM_PROFILE").ok())
    }

    pub fn resolve(exchange: &str) -> Self {
        let raw = Self::raw_profile(exchange)
            .map(Cow::Owned)
            .unwrap_or_else(|| Cow::Borrowed("all"));
        let mut trades = false;
        let mut book = false;
        let mut depth = false;

        let tokens: Vec<String> = raw
            .split([',', '|', ';'])
            .flat_map(|segment| segment.split_whitespace())
            .map(|token| token.trim().to_ascii_lowercase())
            .filter(|token| !token.is_empty())
            .collect();

        if tokens.is_empty() {
            return Self {
                trades: true,
                book: true,
                depth: true,
            };
        }

        for token in &tokens {
            match token.as_str() {
                "all" | "full" | "default" => {
                    trades = true;
                    book = true;
                    depth = true;
                    break;
                }
                "trade" | "trades" | "t" => {
                    trades = true;
                }
                "book" | "books" | "ticker" | "tickers" | "bbo" | "quotes" => {
                    book = true;
                    depth = true;
                }
                "depth" | "l2" | "orderbook" | "lob" => {
                    depth = true;
                }
                "quote" | "mini" => {
                    book = true;
                }
                "none" | "off" => {
                    // Ignore; we'll fallback later if nothing else enabled
                }
                _ => {
                    // 未知值視為啟用全部，避免意外關閉所有流
                    trades = true;
                    book = true;
                    depth = true;
                    break;
                }
            }
        }

        if !trades && !book && !depth {
            // 若使用者設定導致全部關閉，回退為 trades
            trades = true;
        }

        Self {
            trades,
            book,
            depth,
        }
    }
}

/// 统一的交易所接口
#[async_trait]
pub trait Exchange {
    /// 交易所名称
    fn name(&self) -> &'static str;

    /// 获取 WebSocket URL
    fn websocket_url(&self) -> String;

    /// 自定义 WebSocket 请求头（可选）。默认无。
    fn websocket_headers(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// 根據符號集生成連線計劃（預設直接使用 websocket_url + subscription_messages）
    async fn websocket_plan(&self, symbols: &[String]) -> Result<WebsocketPlan> {
        Ok(WebsocketPlan {
            url: self.websocket_url(),
            subscribe_messages: self.subscription_messages(symbols).await?,
        })
    }

    /// 获取热门交易对
    #[cfg_attr(feature = "collector-bitget", allow(dead_code))]
    async fn get_popular_symbols(&self, limit: usize) -> Result<Vec<String>>;

    /// 设置 ClickHouse 表结构
    async fn setup_tables(&self, client: &Client, database: &str) -> Result<()>;

    /// 处理 WebSocket 消息
    async fn process_message(&self, message: &str, buffers: &mut MessageBuffers) -> Result<()>;

    /// 刷新数据到 ClickHouse
    async fn flush_data(&self, database: &str, buffers: &mut MessageBuffers) -> Result<()>;

    /// 連線後的訂閱消息（預設不需要）
    async fn subscription_messages(&self, _symbols: &[String]) -> Result<Vec<String>> {
        Ok(vec![])
    }

    /// 心跳間隔（預設 30s）
    fn heartbeat_interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    /// 是否需要 futures 緩衝區
    fn requires_futures_buffers(&self) -> bool {
        false
    }

    /// 是否需要診斷資料（dry-run 模式使用）
    fn stream_diagnostics(&self) -> StreamDiagnostics {
        StreamDiagnostics::default()
    }
}

#[derive(Default)]
pub struct SpotBuffers {
    pub orderbook: Vec<String>,
    pub trades: Vec<String>,
    pub ticker: Vec<String>,
    pub l1: Vec<String>,
    pub snapshots: Vec<String>,
}

#[derive(Default)]
pub struct FuturesBuffers {
    pub orderbook: Vec<String>,
    pub trades: Vec<String>,
    pub ticker: Vec<String>,
    pub l1: Vec<String>,
    pub snapshots: Vec<String>,
}

/// 消息缓冲区
pub struct MessageBuffers {
    pub spot: SpotBuffers,
    pub futures: Option<FuturesBuffers>,
    pub raw_data: Vec<String>,
}

impl MessageBuffers {
    pub fn new(futures_enabled: bool) -> Self {
        Self {
            spot: SpotBuffers::default(),
            futures: futures_enabled.then(FuturesBuffers::default),
            raw_data: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        let spot_total = self.spot.orderbook.len()
            + self.spot.trades.len()
            + self.spot.ticker.len()
            + self.spot.l1.len()
            + self.spot.snapshots.len();
        let futures_total = self
            .futures
            .as_ref()
            .map(|f| {
                f.orderbook.len() + f.trades.len() + f.ticker.len() + f.l1.len() + f.snapshots.len()
            })
            .unwrap_or(0);
        spot_total + futures_total + self.raw_data.len()
    }

    pub fn futures_mut(&mut self) -> Option<&mut FuturesBuffers> {
        self.futures.as_mut()
    }

    pub fn futures_ref(&self) -> Option<&FuturesBuffers> {
        self.futures.as_ref()
    }

    pub fn push_spot_orderbook(&mut self, line: String) {
        self.spot.orderbook.push(line);
    }

    pub fn push_spot_trades(&mut self, line: String) {
        self.spot.trades.push(line);
    }

    pub fn push_spot_ticker(&mut self, line: String) {
        self.spot.ticker.push(line);
    }

    pub fn push_spot_l1(&mut self, line: String) {
        self.spot.l1.push(line);
    }

    pub fn push_spot_snapshot(&mut self, line: String) {
        self.spot.snapshots.push(line);
    }

    #[cfg(any(feature = "collector-binance-futures", feature = "collector-okx"))]
    pub fn push_futures_orderbook(&mut self, line: String) {
        if let Some(fut) = self.futures_mut() {
            fut.orderbook.push(line);
        }
    }

    pub fn push_futures_trades(&mut self, line: String) {
        if let Some(fut) = self.futures_mut() {
            fut.trades.push(line);
        }
    }

    pub fn push_futures_ticker(&mut self, line: String) {
        if let Some(fut) = self.futures_mut() {
            fut.ticker.push(line);
        }
    }

    pub fn push_futures_l1(&mut self, line: String) {
        if let Some(fut) = self.futures_mut() {
            fut.l1.push(line);
        }
    }

    pub fn push_futures_snapshot(&mut self, line: String) {
        if let Some(fut) = self.futures_mut() {
            fut.snapshots.push(line);
        }
    }

    pub async fn flush_spot_table<F>(
        &mut self,
        sink: &Arc<dyn DbSink>,
        table: &str,
        selector: F,
    ) -> Result<usize>
    where
        F: FnOnce(&mut SpotBuffers) -> &mut Vec<String>,
    {
        let rows = selector(&mut self.spot);
        if rows.is_empty() {
            return Ok(0);
        }
        let batch = std::mem::take(rows);
        let total = batch.len();
        let result = sink.insert_json_rows(table, &batch).await;
        let written = match result {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("插入 {} 條到 {} 失敗: {:?}", total, table, e);
                0
            }
        };
        if written < total {
            // 寫入失敗（或部分失敗）：將整批寫入本地簡易 spool
            let _ = spool::save(table, &batch);
        }
        Ok(written)
    }

    pub async fn flush_futures_table<F>(
        &mut self,
        sink: &Arc<dyn DbSink>,
        table: &str,
        selector: F,
    ) -> Result<usize>
    where
        F: FnOnce(&mut FuturesBuffers) -> &mut Vec<String>,
    {
        if let Some(f) = self.futures_mut() {
            let rows = selector(f);
            if rows.is_empty() {
                return Ok(0);
            }
            let batch = std::mem::take(rows);
            let total = batch.len();
            let result = sink.insert_json_rows(table, &batch).await;
            let written = match result {
                Ok(n) => n,
                Err(e) => {
                    tracing::error!("插入 {} 條到 {} 失敗: {:?}", total, table, e);
                    0
                }
            };
            if written < total {
                let _ = spool::save(table, &batch);
            }
            Ok(written)
        } else {
            Ok(0)
        }
    }
}

/// 创建交易所实例
pub fn create_exchange(
    exchange_name: &str,
    ctx: Arc<ExchangeContext>,
) -> Result<Box<dyn Exchange + Send + Sync>> {
    match exchange_name.to_lowercase().as_str() {
        "binance" => {
            #[cfg(feature = "collector-binance")]
            {
                Ok(Box::new(binance::BinanceExchange::new(ctx)))
            }
            #[cfg(not(feature = "collector-binance"))]
            {
                Err(anyhow::anyhow!("binance collector feature not enabled"))
            }
        }
        "binance_futures" | "binance-futures" | "binancefutures" => {
            #[cfg(feature = "collector-binance-futures")]
            {
                Ok(Box::new(binance_futures::BinanceFuturesExchange::new(ctx)))
            }
            #[cfg(not(feature = "collector-binance-futures"))]
            {
                Err(anyhow::anyhow!(
                    "binance-futures collector feature not enabled"
                ))
            }
        }
        "bitget" => {
            #[cfg(feature = "collector-bitget")]
            {
                Ok(Box::new(bitget::BitgetExchange::new(ctx)))
            }
            #[cfg(not(feature = "collector-bitget"))]
            {
                Err(anyhow::anyhow!("bitget collector feature not enabled"))
            }
        }
        "bybit" => {
            #[cfg(feature = "collector-bybit")]
            {
                Ok(Box::new(bybit::BybitExchange::new(ctx)))
            }
            #[cfg(not(feature = "collector-bybit"))]
            {
                Err(anyhow::anyhow!("bybit collector feature not enabled"))
            }
        }
        "asterdex" => {
            #[cfg(feature = "collector-asterdex")]
            {
                Ok(Box::new(asterdex::AsterdexExchange::new(ctx)))
            }
            #[cfg(not(feature = "collector-asterdex"))]
            {
                Err(anyhow::anyhow!("asterdex collector feature not enabled"))
            }
        }
        "hyperliquid" => {
            #[cfg(feature = "collector-hyperliquid")]
            {
                Ok(Box::new(hyperliquid::HyperliquidExchange::new(ctx)))
            }
            #[cfg(not(feature = "collector-hyperliquid"))]
            {
                Err(anyhow::anyhow!("hyperliquid collector feature not enabled"))
            }
        }
        "okx" => {
            #[cfg(feature = "collector-okx")]
            {
                Ok(Box::new(okx::OkxExchange::new(ctx)))
            }
            #[cfg(not(feature = "collector-okx"))]
            {
                Err(anyhow::anyhow!("okx collector feature not enabled"))
            }
        }
        _ => Err(anyhow::anyhow!("Unsupported exchange: {}", exchange_name)),
    }
}
