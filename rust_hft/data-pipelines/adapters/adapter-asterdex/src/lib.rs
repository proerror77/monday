//! Aster DEX 市場數據適配器
//! - 公共 WebSocket 端點（Binance 兼容）
//! - 解析為統一 MarketEvent 事件

use async_trait::async_trait;
use chrono::Utc;
use futures::stream;
use hft_core::{HftError, HftResult, Symbol};
use ports::{BoxStream, ConnectionHealth, MarketEvent, MarketStream};
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

mod converter;
mod message_types;
mod rest;
mod websocket;

use converter::MessageConverter;
use rest::AsterdexRestClient;
use websocket::*;

/// Aster DEX 市場數據流
pub struct AsterdexMarketStream {
    is_connected: bool,
    last_heartbeat: u64,
}

impl Default for AsterdexMarketStream {
    fn default() -> Self {
        Self::new()
    }
}

impl AsterdexMarketStream {
    pub fn new() -> Self {
        Self {
            is_connected: false,
            last_heartbeat: 0,
        }
    }
}

#[async_trait]
impl MarketStream for AsterdexMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        if symbols.is_empty() {
            return Err(HftError::new("品種列表不能為空"));
        }

        info!("訂閱 Aster DEX 市場數據，品種: {:?}", symbols);
        let (tx, mut rx) = mpsc::unbounded_channel();

        // 可控：是否在啟動時使用 REST 快照初始化
        let use_rest_init = matches!(
            std::env::var("ASTER_INIT_REST_SNAPSHOT")
                .unwrap_or_default()
                .to_lowercase()
                .as_str(),
            "1" | "true" | "yes"
        );
        if use_rest_init {
            if let Ok(rest_client) = AsterdexRestClient::new() {
                for symbol in &symbols {
                    match rest_client.get_depth(symbol, Some(100)).await {
                        Ok(depth) => match MessageConverter::convert_depth_snapshot(
                            symbol.clone(),
                            depth,
                            Utc::now().timestamp_micros() as u64,
                        ) {
                            Ok(snapshot) => {
                                if tx.send(Ok(MarketEvent::Snapshot(snapshot))).is_err() {
                                    return Err(HftError::new("事件通道發送失敗"));
                                }
                            }
                            Err(e) => {
                                warn!("轉換快照失敗: {}", e);
                            }
                        },
                        Err(e) => {
                            warn!("獲取 {} 深度快照失敗: {}", symbol, e);
                        }
                    }
                }
            } else {
                warn!("初始化 Aster DEX REST 客戶端失敗，跳過初始快照");
            }
        }

        let mut ws_client = AsterdexWebSocket::new();
        let symbols_clone = symbols.clone();

        tokio::spawn(async move {
            let mut attempts: u32 = 0;
            const MAX_ATTEMPTS: u32 = 5;
            const BASE_DELAY_MS: u64 = 500;
            loop {
                match ws_client.connect_and_subscribe(symbols_clone.clone()).await {
                    Ok(mut ws_stream) => {
                        attempts = 0;
                        loop {
                            match ws_stream.next().await {
                                Some(Ok(Message::Text(text))) => {
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
                                Some(Ok(Message::Close(_))) => {
                                    warn!("Aster DEX WS 關閉，準備重連");
                                    break;
                                }
                                Some(Err(e)) => {
                                    error!("Aster DEX WS 錯誤: {}，準備重連", e);
                                    break;
                                }
                                None => {
                                    warn!("Aster DEX WS 流結束，準備重連");
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        error!("Aster DEX WS 連接失敗: {}", e);
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

                attempts += 1;
                if attempts > MAX_ATTEMPTS {
                    let _ = tx.send(Err(HftError::Network("WS 重連失敗次數過多".to_string())));
                    return;
                }
                let delay = BASE_DELAY_MS * (1 << (attempts - 1).min(6)) as u64;
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
        });

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
        // 直接依賴 WS 連接時建立；此處僅標記
        self.is_connected = true;
        self.last_heartbeat = chrono::Utc::now().timestamp_millis() as u64;
        info!("Aster DEX 適配器就緒");
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.is_connected = false;
        info!("Aster DEX 適配器已斷開");
        Ok(())
    }
}
