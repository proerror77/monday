//! Binance WebSocket 連接管理
//!
//! 提供兩種消息接收接口：
//! - `receive_message()`: 返回 String，向後兼容，易於使用
//! - `receive_message_bytes()`: 返回 Bytes，零拷貝，高性能

use adapters_common::ws_helpers::constants;
use bytes::Bytes;
use hft_core::{HftError, HftResult, Symbol};
use integration::ws::{WsClient, WsClientConfig};
use tracing::info;

pub const WS_BASE_URL: &str = "wss://data-stream.binance.vision/ws";

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

pub struct BinanceWebSocket {
    client: WsClient,
    symbols: Vec<Symbol>,
    ws_base_url: String,
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
        }
    }

    /// 開始連接並訂閱指定品種
    pub async fn connect_and_subscribe(&mut self, symbols: Vec<Symbol>) -> HftResult<()> {
        self.symbols = symbols.clone();

        // 構建訂閱流名稱
        let streams = self.build_stream_names(&symbols);
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
    fn build_stream_names(&self, symbols: &[Symbol]) -> Vec<String> {
        // 允許通過環境變數控制深度模式
        // BINANCE_USE_LIMITED=true -> 使用 depth{levels}@{freq}
        // 否則使用 diff depth（symbol@depth）
        let use_limited = uses_partial_depth_stream();
        let levels: usize = std::env::var("COLLECTOR_DEPTH_LEVELS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .or_else(|| {
                std::env::var("BINANCE_DEPTH_LEVELS")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
            })
            .filter(|levels| matches!(*levels, 5 | 10 | 20))
            .unwrap_or(20);
        let freq = std::env::var("COLLECTOR_DEPTH_FREQ")
            .or_else(|_| std::env::var("BINANCE_DEPTH_FREQ"))
            .ok()
            .filter(|freq| matches!(freq.as_str(), "100ms" | "1000ms"))
            .unwrap_or_else(|| "100ms".to_string());
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
            if use_limited {
                streams.push(format!("{}@depth{}@{}", symbol_lower, levels, freq));
            } else {
                streams.push(format!("{}@depth@100ms", symbol_lower));
            }

            // 實時交易
            streams.push(format!("{}@trade", symbol_lower));

            // Kline is derived from real-time trades in the engine; keep the duplicate feed opt-in.
            if sub_kline {
                streams.push(format!("{}@kline_1m", symbol_lower));
            }

            // per-symbol bookTicker（可選）
            if sub_book_ticker && !all_book_ticker {
                streams.push(format!("{}@bookTicker", symbol_lower));
            }
        }

        // 全市場最優買賣（可選）：!bookTicker（獨立連線在 adapter 中處理）
        if all_book_ticker {
            streams.push("!bookTicker".to_string());
        }

        streams
    }

    /// 接收消息 (String 接口 - 向後兼容)
    pub async fn receive_message(&mut self) -> HftResult<Option<String>> {
        match self.client.receive_message().await {
            Ok(Some((msg, _metrics))) => Ok(Some(msg)),
            Ok(None) => Ok(None),
            Err(e) => Err(HftError::Network(format!("接收消息失敗: {}", e))),
        }
    }

    /// 接收消息 (Bytes 接口 - 零拷貝高性能)
    ///
    /// 此方法返回 `Bytes` 而非 `String`，避免了 String 分配和 UTF-8 驗證開銷。
    /// 適合需要直接在原始字節上進行 JSON 解析的高性能場景。
    ///
    /// # 性能優勢
    /// - 零數據拷貝：直接返回 WebSocket 幀的底層緩衝區
    /// - 延遲 UTF-8 驗證：由調用方決定何時進行字符串轉換
    /// - SIMD JSON 友好：可直接傳遞給 simd-json 進行解析
    ///
    /// # 使用示例
    /// ```ignore
    /// # use data_adapter_binance::BinanceWebSocket;
    /// # use hft_core::Symbol;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut ws = BinanceWebSocket::new();
    /// ws.connect_and_subscribe(vec![Symbol::new("BTCUSDT")]).await?;
    ///
    /// while let Some(bytes) = ws.receive_message_bytes().await? {
    ///     // 直接在原始字節上進行 JSON 解析 (simd-json)
    ///     // let mut buf = bytes.to_vec();
    ///     // let value = simd_json::to_borrowed_value(&mut buf)?;
    ///
    ///     // 或轉換為 String (如需要)
    ///     // let text = String::from_utf8(bytes.to_vec())?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn receive_message_bytes(&mut self) -> HftResult<Option<Bytes>> {
        self.receive_message_bytes_with_metrics()
            .await
            .map(|message| message.map(|(bytes, _)| bytes))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_stream_names() {
        let ws = BinanceWebSocket::new();
        let symbols = vec![Symbol::new("BTCUSDT"), Symbol::new("ETHUSDT")];

        let streams = ws.build_stream_names(&symbols);

        assert_eq!(streams.len(), 6); // 每個品種 3 個流
        assert!(streams.contains(&"btcusdt@depth20@100ms".to_string()));
        assert!(streams.contains(&"btcusdt@trade".to_string()));
        assert!(streams.contains(&"btcusdt@bookTicker".to_string()));
        assert!(streams.contains(&"ethusdt@depth20@100ms".to_string()));
        assert!(streams.contains(&"ethusdt@trade".to_string()));
        assert!(streams.contains(&"ethusdt@bookTicker".to_string()));
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
}
