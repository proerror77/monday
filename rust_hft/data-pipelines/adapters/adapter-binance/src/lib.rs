//! Binance 行情 adapter（實作 `ports::MarketStream`）
//! - 快照+增量/序號/checksum → 統一 MarketEvent
//! - WebSocket 實時流 + REST 快照初始化

use async_trait::async_trait;
use futures::stream;
use hft_core::{HftError, HftResult, Symbol, Timestamp};
use ports::events::MarketSnapshot;
use ports::{BoxStream, ConnectionHealth, MarketEvent, MarketStream};
use tokio::sync::mpsc;
use tokio::time::Duration;
use tracing::{error, info, warn};

mod converter;
mod message_types;
mod rest;
mod websocket;

use rest::*;
use websocket::*;

// Re-export for benchmarks and external use
pub use converter::MessageConverter;

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

#[async_trait]
impl MarketStream for BinanceMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        if symbols.is_empty() {
            return Err(HftError::new("品種列表不能為空"));
        }

        info!("訂閱 Binance 市場數據，品種: {:?}", symbols);

        // 創建事件通道
        let (tx, mut rx) = mpsc::unbounded_channel();

        // 如果啟用了初始快照，先獲取快照
        if self.caps.snapshot_crc {
            match self.get_initial_snapshots(&symbols).await {
                Ok(snapshots) => {
                    for snapshot in snapshots {
                        if tx.send(Ok(MarketEvent::Snapshot(snapshot))).is_err() {
                            return Err(HftError::new("事件通道發送失敗"));
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
            // 簡易自動重連循環
            let mut attempts: u32 = 0;
            const MAX_ATTEMPTS: u32 = 5;
            const BASE_DELAY_MS: u64 = 500;

            loop {
                match ws_client.connect_and_subscribe(symbols_clone.clone()).await {
                    Ok(()) => {
                        attempts = 0; // 成功連上，重置計數
                        loop {
                            match ws_client.receive_message().await {
                                Ok(Some(text)) => {
                                    match MessageConverter::parse_stream_message(&text) {
                                        Ok(Some(event)) => {
                                            if tx.send(Ok(event)).is_err() {
                                                return;
                                            }
                                        }
                                        Ok(None) => {}
                                        Err(e) => {
                                            let _ = tx.send(Err(e));
                                        }
                                    }
                                }
                                Ok(None) => {
                                    warn!("Binance WS 關閉，準備重連");
                                    break;
                                }
                                Err(e) => {
                                    error!("Binance WS 錯誤: {}，準備重連", e);
                                    let _ = tx.send(Err(e));
                                    break;
                                    // 其他控制訊息：忽略並繼續
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Binance WS 連接失敗: {}", e);
                        attempts += 1;
                        if attempts > MAX_ATTEMPTS {
                            let _ = tx.send(Err(HftError::Network(format!(
                                "WS 重連失敗次數過多: {}",
                                e
                            ))));
                            return;
                        }
                        let delay = BASE_DELAY_MS * (1 << (attempts - 1).min(6)) as u64;
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        continue;
                    }
                }

                // 斷開後的退避重連
                attempts += 1;
                if attempts > MAX_ATTEMPTS {
                    let _ = tx.send(Err(HftError::Network("WS 重連失敗次數過多".to_string())));
                    return;
                }
                let delay = BASE_DELAY_MS * (1 << (attempts - 1).min(6)) as u64;
                tokio::time::sleep(Duration::from_millis(delay)).await;
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
