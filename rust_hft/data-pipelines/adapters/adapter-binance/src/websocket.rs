//! Binance WebSocket 連接管理
//!
//! WebSocket frames are exposed with receive metrics for the market-data path.

use crate::BinanceTradeStreams;
use adapters_common::ws_helpers::constants;
use bytes::Bytes;
use hft_core::{HftError, HftResult, Symbol};
use integration::ws::{WsClient, WsClientConfig};
use tracing::info;

pub const WS_BASE_URL: &str = "wss://data-stream.binance.vision/ws";
pub const WS_USDM_BASE_URL: &str = "wss://fstream.binance.com/ws";
const WS_SPOT_CATALOG_URL: &str = "wss://stream.binance.com:9443/ws";
const WS_SPOT_CATALOG_STREAM_URL: &str = "wss://stream.binance.com:9443/stream";

pub(crate) fn is_known_spot_endpoint(url: &str) -> bool {
    matches!(
        url.trim_end_matches('/'),
        WS_BASE_URL
            | WS_SPOT_CATALOG_URL
            | WS_SPOT_CATALOG_STREAM_URL
            | "wss://stream.binance.com/ws"
            | "wss://stream.binance.com"
    )
}

pub(crate) fn usdm_endpoint_for(url: &str) -> String {
    if is_known_spot_endpoint(url) {
        WS_USDM_BASE_URL.to_string()
    } else {
        url.to_string()
    }
}

pub(crate) fn uses_partial_depth_stream() -> bool {
    let mode = std::env::var("COLLECTOR_DEPTH_MODE")
        .or_else(|_| std::env::var("BINANCE_DEPTH_MODE"))
        .unwrap_or_else(|_| "partial20".to_string())
        .to_ascii_lowercase();
    let explicitly_diff = matches!(
        mode.as_str(),
        "diff" | "diff-depth" | "full" | "incremental"
    );
    let force_limited = matches!(
        std::env::var("BINANCE_USE_LIMITED")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    );
    !explicitly_diff || force_limited
}

pub(crate) fn validate_depth_frequency(usdm: bool) -> HftResult<()> {
    let configured = std::env::var("COLLECTOR_DEPTH_FREQ")
        .or_else(|_| std::env::var("BINANCE_DEPTH_FREQ"))
        .ok();
    depth_frequency(usdm, configured.as_deref()).map(|_| ())
}

pub struct BinanceWebSocket {
    client: WsClient,
    symbols: Vec<Symbol>,
    ws_base_url: String,
    usdm: bool,
    trade_streams: BinanceTradeStreams,
    depth_levels: Option<usize>,
    depth_enabled: bool,
    book_ticker_enabled: bool,
}

impl Default for BinanceWebSocket {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl BinanceWebSocket {
    pub fn new() -> Self {
        Self::with_base_url(WS_BASE_URL.to_string())
    }

    pub fn with_base_url(url: impl Into<String>) -> Self {
        let url_string = url.into();
        let config = WsClientConfig {
            url: url_string.clone(),
            heartbeat_interval: constants::ping_interval(),
            ..Default::default()
        };
        Self {
            client: WsClient::new(config),
            symbols: Vec::new(),
            ws_base_url: url_string,
            usdm: false,
            trade_streams: BinanceTradeStreams::default(),
            depth_levels: None,
            depth_enabled: true,
            book_ticker_enabled: true,
        }
    }

    pub fn with_usdm(mut self) -> Self {
        self.ws_base_url = usdm_endpoint_for(&self.ws_base_url);
        self.usdm = true;
        self
    }

    pub fn with_trade_streams(mut self, trade_streams: BinanceTradeStreams) -> Self {
        self.trade_streams = trade_streams;
        self
    }

    pub fn with_depth_levels(mut self, depth_levels: Option<usize>) -> Self {
        self.depth_levels = depth_levels;
        self
    }

    /// Enable or disable depth subscriptions for specialized collectors.
    #[must_use]
    pub const fn with_depth_stream(mut self, enabled: bool) -> Self {
        self.depth_enabled = enabled;
        self
    }

    /// Enable or disable per-symbol book-ticker subscriptions.
    #[must_use]
    pub const fn with_book_ticker(mut self, enabled: bool) -> Self {
        self.book_ticker_enabled = enabled;
        self
    }

    /// 開始連接並訂閱指定品種
    pub async fn connect_and_subscribe(&mut self, symbols: Vec<Symbol>) -> HftResult<()> {
        self.symbols = symbols.clone();

        // 構建訂閱流名稱
        let streams = self.build_stream_names(&symbols)?;
        info!("連接 Binance WebSocket，訂閱流: {:?}", streams);

        // 構建 WebSocket URL
        let url = self.build_connection_url(&streams);

        self.client.cfg.url = url;

        self.client
            .connect()
            .await
            .map_err(|e| HftError::Network(format!("Binance WebSocket 連接失敗: {}", e)))?;

        info!("Binance WebSocket 連接成功");
        Ok(())
    }

    /// Connect only to the USD-M force-order reference streams. Liquidation
    /// notifications are typed reference observations and are intentionally
    /// kept outside the generic MarketEvent trade/depth stream.
    pub async fn connect_and_subscribe_force_orders(
        &mut self,
        symbols: Vec<Symbol>,
    ) -> HftResult<()> {
        if !self.usdm {
            return Err(HftError::Config(
                "Binance force-order streams require the USD-M endpoint".to_string(),
            ));
        }
        if symbols.is_empty() {
            return Err(HftError::Config(
                "Binance force-order symbols cannot be empty".to_string(),
            ));
        }
        self.symbols = symbols.clone();
        let streams = Self::force_order_stream_names(&symbols);
        let url = self.build_connection_url(&streams);
        self.client.cfg.url = url;
        self.client.connect().await.map_err(|error| {
            HftError::Network(format!("Binance force-order connection failed: {error}"))
        })?;
        Ok(())
    }

    fn force_order_stream_names(symbols: &[Symbol]) -> Vec<String> {
        symbols
            .iter()
            .map(|symbol| format!("{}@forceOrder", symbol.as_str().to_ascii_lowercase()))
            .collect()
    }

    fn build_connection_url(&self, streams: &[String]) -> String {
        if streams.is_empty() {
            return self.ws_base_url.clone();
        }
        let configured = self.ws_base_url.trim_end_matches('/');
        let root = configured
            .strip_suffix("/ws")
            .or_else(|| configured.strip_suffix("/stream"))
            .unwrap_or(configured);
        format!("{root}/stream?streams={}", streams.join("/"))
    }

    /// 構建訂閱流名稱
    fn build_stream_names(&self, symbols: &[Symbol]) -> HftResult<Vec<String>> {
        // 允許通過環境變數控制深度模式
        // BINANCE_USE_LIMITED=true -> 使用 depth{levels}@{freq}
        // 否則使用 diff depth（symbol@depth）
        let use_limited = self.depth_enabled && uses_partial_depth_stream();
        let levels: usize = self.depth_levels.unwrap_or_else(|| {
            std::env::var("COLLECTOR_DEPTH_LEVELS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .or_else(|| {
                    std::env::var("BINANCE_DEPTH_LEVELS")
                        .ok()
                        .and_then(|s| s.parse::<usize>().ok())
                })
                .filter(|levels| matches!(*levels, 5 | 10 | 20))
                .unwrap_or(20)
        });
        if self.depth_enabled && !matches!(levels, 5 | 10 | 20) {
            return Err(HftError::Config(format!(
                "unsupported Binance partial depth {levels}; use 5, 10, or 20"
            )));
        }
        let freq = if use_limited {
            let configured_freq = std::env::var("COLLECTOR_DEPTH_FREQ")
                .or_else(|_| std::env::var("BINANCE_DEPTH_FREQ"))
                .ok();
            depth_frequency(self.usdm, configured_freq.as_deref())?
        } else {
            "100ms".to_string()
        };
        let mut streams = Vec::new();

        let sub_book_ticker = std::env::var("COLLECTOR_SUB_BOOK_TICKER")
            .or_else(|_| std::env::var("BINANCE_SUB_BOOK_TICKER"))
            .map(|value| !matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "no"))
            .unwrap_or(true);
        let sub_kline = std::env::var("COLLECTOR_SUB_KLINE")
            .or_else(|_| std::env::var("BINANCE_SUB_KLINE"))
            .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        let all_book_ticker = matches!(
            std::env::var("COLLECTOR_ALL_BOOK_TICKER")
                .unwrap_or_default()
                .to_lowercase()
                .as_str(),
            "1" | "true" | "yes"
        ) || matches!(
            std::env::var("BINANCE_ALL_BOOK_TICKER")
                .unwrap_or_default()
                .to_lowercase()
                .as_str(),
            "1" | "true" | "yes"
        );

        for symbol in symbols {
            let symbol_lower = symbol.to_string().to_lowercase();

            // 訂單簿增量更新 (100ms 推送)
            if self.depth_enabled {
                if use_limited {
                    streams.push(format!("{}@depth{}@{}", symbol_lower, levels, freq));
                } else {
                    streams.push(format!("{}@depth@100ms", symbol_lower));
                }
            }

            match self.trade_streams {
                BinanceTradeStreams::None => {}
                BinanceTradeStreams::Raw => {
                    streams.push(format!("{}@trade", symbol_lower));
                }
                BinanceTradeStreams::Aggregate => {
                    streams.push(format!("{}@aggTrade", symbol_lower));
                }
                BinanceTradeStreams::Both => {
                    streams.push(format!("{}@trade", symbol_lower));
                    streams.push(format!("{}@aggTrade", symbol_lower));
                }
            }

            // Kline is derived from real-time trades in the engine; keep the duplicate feed opt-in.
            if sub_kline {
                streams.push(format!("{}@kline_1m", symbol_lower));
            }

            // per-symbol bookTicker（可選）
            if self.book_ticker_enabled && sub_book_ticker && !all_book_ticker {
                streams.push(format!("{}@bookTicker", symbol_lower));
            }
        }

        // 全市場最優買賣（可選）：!bookTicker（獨立連線在 adapter 中處理）
        if self.book_ticker_enabled && all_book_ticker {
            streams.push("!bookTicker".to_string());
        }

        Ok(streams)
    }

    pub async fn receive_message_bytes_with_metrics(
        &mut self,
    ) -> HftResult<Option<(Bytes, integration::WsMessageMetrics)>> {
        match self.client.receive_message_bytes().await {
            Ok(Some(message)) => Ok(Some(message)),
            Ok(None) => Ok(None),
            Err(e) => Err(HftError::Network(format!("接收消息失敗: {}", e))),
        }
    }
}

fn depth_frequency(usdm: bool, configured: Option<&str>) -> HftResult<String> {
    let Some(configured) = configured else {
        return Ok("100ms".to_string());
    };
    if usdm {
        if matches!(configured, "100ms" | "250ms" | "500ms") {
            return Ok(configured.to_string());
        }
        return Err(HftError::Config(format!(
            "Binance USD-M partial depth does not support {configured}; use 100ms, 250ms, or 500ms"
        )));
    }
    Ok(if matches!(configured, "100ms" | "1000ms") {
        configured.to_string()
    } else {
        "100ms".to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_stream_names() {
        let ws = BinanceWebSocket::new();
        let symbols = vec![Symbol::new("BTCUSDT"), Symbol::new("ETHUSDT")];

        let streams = ws.build_stream_names(&symbols).unwrap();

        assert_eq!(streams.len(), 6); // 每個品種 3 個流（默认 raw trade）
        assert!(streams.contains(&"btcusdt@depth20@100ms".to_string()));
        assert!(streams.contains(&"btcusdt@trade".to_string()));
        assert!(streams.contains(&"btcusdt@bookTicker".to_string()));
        assert!(streams.contains(&"ethusdt@depth20@100ms".to_string()));
        assert!(streams.contains(&"ethusdt@trade".to_string()));
        assert!(streams.contains(&"ethusdt@bookTicker".to_string()));
    }

    #[test]
    fn aggregate_trade_subscription_is_explicit_and_distinct() {
        let ws = BinanceWebSocket::new().with_trade_streams(BinanceTradeStreams::Aggregate);
        let streams = ws.build_stream_names(&[Symbol::new("BTCUSDT")]).unwrap();
        assert_eq!(streams.len(), 3);
        assert!(streams.contains(&"btcusdt@aggTrade".to_string()));
        assert!(!streams.contains(&"btcusdt@trade".to_string()));

        let both = BinanceWebSocket::new().with_trade_streams(BinanceTradeStreams::Both);
        let streams = both.build_stream_names(&[Symbol::new("BTCUSDT")]).unwrap();
        assert!(streams.contains(&"btcusdt@aggTrade".to_string()));
        assert!(streams.contains(&"btcusdt@trade".to_string()));

        let none = BinanceWebSocket::new().with_trade_streams(BinanceTradeStreams::None);
        let streams = none.build_stream_names(&[Symbol::new("BTCUSDT")]).unwrap();
        assert!(!streams.iter().any(|stream| stream.contains("trade")));
    }

    #[tokio::test]
    async fn force_order_subscription_is_usdm_only_and_symbol_bounded() {
        let symbols = vec![Symbol::new("BTCUSDT"), Symbol::new("ETHUSDT")];
        assert_eq!(
            BinanceWebSocket::force_order_stream_names(&symbols),
            vec!["btcusdt@forceOrder", "ethusdt@forceOrder"]
        );
        assert!(BinanceWebSocket::new()
            .connect_and_subscribe_force_orders(symbols.clone())
            .await
            .is_err());
        assert!(BinanceWebSocket::new()
            .with_usdm()
            .connect_and_subscribe_force_orders(Vec::new())
            .await
            .is_err());
    }

    #[test]
    fn explicit_depth_level_configuration_is_reflected_in_the_subscription() {
        let ws = BinanceWebSocket::new().with_depth_levels(Some(5));
        let streams = ws.build_stream_names(&[Symbol::new("BTCUSDT")]).unwrap();
        assert!(streams.contains(&"btcusdt@depth5@100ms".to_string()));
        assert!(!streams.contains(&"btcusdt@depth20@100ms".to_string()));
    }

    #[test]
    fn specialized_subscription_can_disable_depth_and_quotes() {
        let ws = BinanceWebSocket::new()
            .with_trade_streams(BinanceTradeStreams::Raw)
            .with_depth_stream(false)
            .with_book_ticker(false);
        let streams = ws.build_stream_names(&[Symbol::new("BTCUSDT")]).unwrap();
        assert_eq!(streams, vec!["btcusdt@trade".to_string()]);
    }

    #[test]
    fn usdm_rejects_spot_only_partial_depth_frequency() {
        assert!(depth_frequency(true, Some("1000ms")).is_err());
        for frequency in ["100ms", "250ms", "500ms"] {
            assert_eq!(depth_frequency(true, Some(frequency)).unwrap(), frequency);
        }
    }

    #[test]
    fn combined_stream_uses_binance_stream_endpoint() {
        let ws = BinanceWebSocket::with_base_url("wss://stream.binance.com:9443/ws");
        let url =
            ws.build_connection_url(&["btcusdt@depth".to_string(), "btcusdt@trade".to_string()]);
        assert_eq!(
            url,
            "wss://stream.binance.com:9443/stream?streams=btcusdt@depth/btcusdt@trade"
        );
    }

    #[test]
    fn usdm_uses_futures_stream_endpoint_by_default() {
        let ws = BinanceWebSocket::new().with_usdm();
        let url = ws.build_connection_url(&["btcusdt@depth20@100ms".to_string()]);
        assert_eq!(
            url,
            "wss://fstream.binance.com/stream?streams=btcusdt@depth20@100ms"
        );
    }

    #[test]
    fn usdm_preserves_explicit_stream_endpoint_override() {
        let ws = BinanceWebSocket::with_base_url("ws://localhost:18081/ws").with_usdm();
        let url = ws.build_connection_url(&["btcusdt@depth20@100ms".to_string()]);
        assert_eq!(
            url,
            "ws://localhost:18081/stream?streams=btcusdt@depth20@100ms"
        );
    }

    #[test]
    fn usdm_replaces_the_schema_catalog_spot_endpoint() {
        let ws = BinanceWebSocket::with_base_url(WS_SPOT_CATALOG_URL).with_usdm();
        let url = ws.build_connection_url(&["btcusdt@depth20@100ms".to_string()]);
        assert_eq!(
            url,
            "wss://fstream.binance.com/stream?streams=btcusdt@depth20@100ms"
        );
    }
}
