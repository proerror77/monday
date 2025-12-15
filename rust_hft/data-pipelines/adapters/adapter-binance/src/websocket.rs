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

pub const WS_BASE_URL: &str = "wss://stream.binance.com:9443/ws";

pub struct BinanceWebSocket {
    client: WsClient,
    symbols: Vec<Symbol>,
    ws_base_url: String,
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
        let url = if streams.is_empty() {
            self.ws_base_url.clone()
        } else {
            let stream_names = streams.join("/");
            format!("{}/{}", self.ws_base_url, stream_names)
        };

        self.client.cfg.url = url;

        self.client
            .connect()
            .await
            .map_err(|e| HftError::Network(format!("Binance WebSocket 連接失敗: {}", e)))?;

        info!("Binance WebSocket 連接成功");
        Ok(())
    }

    /// 構建訂閱流名稱
    fn build_stream_names(&self, symbols: &[Symbol]) -> Vec<String> {
        // 允許通過環境變數控制深度模式
        // BINANCE_USE_LIMITED=true -> 使用 depth{levels}@{freq}
        // 否則使用 diff depth（symbol@depth）
        let mode = std::env::var("COLLECTOR_DEPTH_MODE")
            .unwrap_or_default()
            .to_lowercase();
        let use_limited_generic = matches!(mode.as_str(), "limited" | "depth_limited");
        let use_limited = use_limited_generic
            || matches!(
                std::env::var("BINANCE_USE_LIMITED")
                    .unwrap_or_default()
                    .to_lowercase()
                    .as_str(),
                "1" | "true" | "yes"
            );
        let levels: usize = std::env::var("COLLECTOR_DEPTH_LEVELS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .or_else(|| {
                std::env::var("BINANCE_DEPTH_LEVELS")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
            })
            .unwrap_or(20);
        let freq = std::env::var("COLLECTOR_DEPTH_FREQ")
            .or_else(|_| std::env::var("BINANCE_DEPTH_FREQ"))
            .unwrap_or_else(|_| "100ms".to_string());
        let mut streams = Vec::new();

        let sub_book_ticker = matches!(
            std::env::var("COLLECTOR_SUB_BOOK_TICKER")
                .unwrap_or_default()
                .to_lowercase()
                .as_str(),
            "1" | "true" | "yes"
        ) || matches!(
            std::env::var("BINANCE_SUB_BOOK_TICKER")
                .unwrap_or_default()
                .to_lowercase()
                .as_str(),
            "1" | "true" | "yes"
        );
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
                streams.push(format!("{}@depth", symbol_lower));
            }

            // 實時交易
            streams.push(format!("{}@trade", symbol_lower));

            // 1分鐘K線
            streams.push(format!("{}@kline_1m", symbol_lower));

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
    /// ```no_run
    /// # use adapter_binance::BinanceWebSocket;
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
        match self.client.receive_message_bytes().await {
            Ok(Some((bytes, _metrics))) => Ok(Some(bytes)),
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
        assert!(streams.contains(&"btcusdt@depth".to_string()));
        assert!(streams.contains(&"btcusdt@trade".to_string()));
        assert!(streams.contains(&"btcusdt@kline_1m".to_string()));
        assert!(streams.contains(&"ethusdt@depth".to_string()));
        assert!(streams.contains(&"ethusdt@trade".to_string()));
        assert!(streams.contains(&"ethusdt@kline_1m".to_string()));
    }
}
