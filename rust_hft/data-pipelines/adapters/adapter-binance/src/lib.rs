//! Binance 行情 adapter（實作 `ports::MarketStream`）
//! - 快照+增量/序號/checksum → 統一 MarketEvent
//! - WebSocket 實時流 + REST 快照初始化

use async_trait::async_trait;
use futures::stream;
use hft_core::{HftError, HftResult, Symbol, Timestamp};
use ports::events::MarketSnapshot;
use ports::{BoxStream, ConnectionHealth, MarketEvent, MarketStream};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

mod converter;
mod message_types;
mod rest;
mod websocket;

// Re-export for benchmarks and external use
pub use converter::MessageConverter;
pub use message_types::{BookTickerEvent, DepthSnapshot};
pub use rest::BinanceRestClient;
pub use websocket::BinanceWebSocket;

const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 4096;

pub mod capabilities {
    #[derive(Debug, Clone)]
    pub struct BinanceCapabilities {
        pub snapshot_crc: bool,
        pub rest_fallback: bool,
        pub auto_reconnect: bool,
    }

    impl Default for BinanceCapabilities {
        fn default() -> Self {
            Self {
                snapshot_crc: true,
                rest_fallback: true,
                auto_reconnect: true,
            }
        }
    }
}

/// Binance 市場數據流
pub struct BinanceMarketStream {
    caps: capabilities::BinanceCapabilities,
    rest_client: BinanceRestClient,
    is_connected: bool,
    last_heartbeat: Timestamp,
    ws_base_url: String,
}

impl Default for BinanceMarketStream {
    fn default() -> Self {
        Self::new()
    }
}

impl BinanceMarketStream {
    pub fn new() -> Self {
        Self {
            caps: Default::default(),
            rest_client: BinanceRestClient::new(),
            is_connected: false,
            last_heartbeat: 0,
            ws_base_url: websocket::WS_BASE_URL.to_string(),
        }
    }

    pub fn with_capabilities(caps: capabilities::BinanceCapabilities) -> Self {
        Self {
            caps,
            rest_client: BinanceRestClient::new(),
            is_connected: false,
            last_heartbeat: 0,
            ws_base_url: websocket::WS_BASE_URL.to_string(),
        }
    }

    pub fn with_ws_base_url(mut self, url: impl Into<String>) -> Self {
        let url = url.into();
        self.ws_base_url = url;
        self
    }

    pub fn with_rest_base_url(mut self, url: impl Into<String>) -> Self {
        self.rest_client = BinanceRestClient::with_base_url(url);
        self
    }

    fn event_queue_capacity() -> usize {
        std::env::var("BINANCE_EVENT_QUEUE_CAPACITY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|capacity| *capacity > 0)
            .unwrap_or(DEFAULT_EVENT_QUEUE_CAPACITY)
    }

    /// 獲取訂單簿快照（用於初始化）
    async fn get_initial_snapshots(&self, symbols: &[Symbol]) -> HftResult<Vec<MarketSnapshot>> {
        let mut snapshots = Vec::new();

        for symbol in symbols {
            info!("獲取 {:?} 的初始快照", symbol);
            let depth = self.rest_client.get_depth(symbol, Some(100)).await?;
            // 使用本地時間（毫秒）轉換為微秒
            let timestamp = (chrono::Utc::now().timestamp_millis() as u64) * 1000;

            let snapshot =
                MessageConverter::convert_depth_snapshot(symbol.clone(), depth, timestamp)?;

            snapshots.push(snapshot);
        }

        Ok(snapshots)
    }
}

fn try_send_stream_event(
    tx: &mpsc::Sender<HftResult<MarketEvent>>,
    event: HftResult<MarketEvent>,
) -> bool {
    match tx.try_send(event) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            warn!("Binance event queue full, dropping stale stream event");
            true
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

#[async_trait]
impl MarketStream for BinanceMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        if symbols.is_empty() {
            return Err(HftError::new("品種列表不能為空"));
        }

        info!("訂閱 Binance 市場數據，品種: {:?}", symbols);

        // 創建事件通道
        let (tx, mut rx) = mpsc::channel(Self::event_queue_capacity());

        // 如果啟用了初始快照，先獲取快照
        if self.caps.snapshot_crc {
            match self.get_initial_snapshots(&symbols).await {
                Ok(snapshots) => {
                    for snapshot in snapshots {
                        match tx.try_send(Ok(MarketEvent::Snapshot(snapshot))) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                return Err(HftError::new("Binance 初始快照事件通道已滿"));
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                return Err(HftError::new("Binance 事件通道已關閉"));
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("獲取初始快照失敗: {}，將僅使用 WebSocket 增量數據", e);
                }
            }
        }

        // 啟動 WebSocket 流
        // 注意：這裡我們需要 clone self，但是 self 是 &self，所以需要重新設計
        // 為了演示目的，我們先創建一個簡單的流
        let mut ws_client = BinanceWebSocket::with_base_url(self.ws_base_url.clone());
        let symbols_clone = symbols.clone();

        tokio::spawn(async move {
            // 使用共用重連配置
            use adapters_common::calculate_exponential_backoff;
            use adapters_common::ws_helpers::constants::{
                DEFAULT_BASE_DELAY_MS, DEFAULT_MAX_ATTEMPTS,
            };

            let mut attempts: u32 = 0;

            loop {
                match ws_client.connect_and_subscribe(symbols_clone.clone()).await {
                    Ok(()) => {
                        attempts = 0; // 成功連上，重置計數
                        loop {
                            match ws_client.receive_message().await {
                                Ok(Some(text)) => {
                                    match MessageConverter::parse_stream_message(&text) {
                                        Ok(Some(event)) => {
                                            if !try_send_stream_event(&tx, Ok(event)) {
                                                return;
                                            }
                                        }
                                        Ok(None) => {}
                                        Err(e) => {
                                            if !try_send_stream_event(&tx, Err(e)) {
                                                return;
                                            }
                                        }
                                    }
                                }
                                Ok(None) => {
                                    warn!("Binance WS 關閉，準備重連");
                                    break;
                                }
                                Err(e) => {
                                    error!("Binance WS 錯誤: {}，準備重連", e);
                                    if !try_send_stream_event(&tx, Err(e)) {
                                        return;
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Binance WS 連接失敗: {}", e);
                        attempts += 1;
                        if attempts > DEFAULT_MAX_ATTEMPTS {
                            let _ = try_send_stream_event(
                                &tx,
                                Err(HftError::Network(format!("WS 重連失敗次數過多: {}", e))),
                            );
                            return;
                        }
                        let delay = calculate_exponential_backoff(attempts, DEFAULT_BASE_DELAY_MS);
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                }

                // 斷開後的退避重連
                attempts += 1;
                if attempts > DEFAULT_MAX_ATTEMPTS {
                    let _ = try_send_stream_event(
                        &tx,
                        Err(HftError::Network("WS 重連失敗次數過多".to_string())),
                    );
                    return;
                }
                let delay = calculate_exponential_backoff(attempts, DEFAULT_BASE_DELAY_MS);
                tokio::time::sleep(delay).await;
            }
        });

        // 創建流
        let stream = stream::poll_fn(move |cx| match rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(event)) => std::task::Poll::Ready(Some(event)),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        });

        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.is_connected,
            latency_ms: None,
            last_heartbeat: self.last_heartbeat,
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        // 測試連通性
        self.rest_client.ping().await?;
        self.is_connected = true;
        self.last_heartbeat = chrono::Utc::now().timestamp_millis() as u64;
        info!("Binance 適配器連接成功");
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.is_connected = false;
        info!("Binance 適配器已斷開");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capabilities::BinanceCapabilities;

    #[test]
    fn test_binance_market_stream_default() {
        let stream = BinanceMarketStream::default();
        assert!(!stream.is_connected);
        assert_eq!(stream.last_heartbeat, 0);
        assert!(stream.caps.snapshot_crc);
        assert!(stream.caps.rest_fallback);
        assert!(stream.caps.auto_reconnect);
    }

    #[test]
    fn test_binance_market_stream_new() {
        let stream = BinanceMarketStream::new();
        assert!(!stream.is_connected);
        assert_eq!(stream.ws_base_url, websocket::WS_BASE_URL);
    }

    #[test]
    fn test_binance_capabilities_default() {
        let caps = BinanceCapabilities::default();
        assert!(caps.snapshot_crc);
        assert!(caps.rest_fallback);
        assert!(caps.auto_reconnect);
    }

    #[test]
    fn test_binance_capabilities_custom() {
        let caps = BinanceCapabilities {
            snapshot_crc: false,
            rest_fallback: false,
            auto_reconnect: true,
        };
        assert!(!caps.snapshot_crc);
        assert!(!caps.rest_fallback);
        assert!(caps.auto_reconnect);
    }

    #[test]
    fn test_with_capabilities() {
        let caps = BinanceCapabilities {
            snapshot_crc: false,
            rest_fallback: true,
            auto_reconnect: false,
        };
        let stream = BinanceMarketStream::with_capabilities(caps.clone());
        assert_eq!(stream.caps.snapshot_crc, caps.snapshot_crc);
        assert_eq!(stream.caps.rest_fallback, caps.rest_fallback);
        assert_eq!(stream.caps.auto_reconnect, caps.auto_reconnect);
    }

    #[test]
    fn test_with_ws_base_url() {
        let stream = BinanceMarketStream::new().with_ws_base_url("wss://custom.binance.com/ws");
        assert_eq!(stream.ws_base_url, "wss://custom.binance.com/ws");
    }

    #[test]
    fn test_with_rest_base_url() {
        let stream = BinanceMarketStream::new().with_rest_base_url("https://custom.binance.com");
        // The rest_client is updated internally
        assert!(!stream.is_connected);
    }

    #[tokio::test]
    async fn test_health_check_initial_state() {
        let stream = BinanceMarketStream::new();
        let health = stream.health().await;

        assert!(!health.connected);
        assert!(health.latency_ms.is_none());
        assert_eq!(health.last_heartbeat, 0);
    }

    #[tokio::test]
    async fn test_disconnect() {
        let mut stream = BinanceMarketStream::new();
        stream.is_connected = true;

        let result = stream.disconnect().await;
        assert!(result.is_ok());
        assert!(!stream.is_connected);
    }

    #[tokio::test]
    async fn test_subscribe_empty_symbols_fails() {
        let stream = BinanceMarketStream::new();
        let result = stream.subscribe(vec![]).await;

        assert!(result.is_err());
    }

    #[test]
    fn test_capabilities_debug() {
        let caps = BinanceCapabilities::default();
        let debug_str = format!("{:?}", caps);
        assert!(debug_str.contains("BinanceCapabilities"));
        assert!(debug_str.contains("snapshot_crc"));
    }

    #[test]
    fn test_capabilities_clone() {
        let caps = BinanceCapabilities {
            snapshot_crc: false,
            rest_fallback: true,
            auto_reconnect: true,
        };
        let cloned = caps.clone();
        assert_eq!(cloned.snapshot_crc, caps.snapshot_crc);
        assert_eq!(cloned.rest_fallback, caps.rest_fallback);
        assert_eq!(cloned.auto_reconnect, caps.auto_reconnect);
    }
}
