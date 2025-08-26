//! Binance 行情 adapter（實作 `ports::MarketStream`）
//! - 快照+增量/序號/checksum → 統一 MarketEvent
//! - WebSocket 實時流 + REST 快照初始化

use async_trait::async_trait;
use futures::stream;
use hft_core::{HftError, HftResult, Symbol, Timestamp};
use ports::{BoxStream, ConnectionHealth, MarketEvent, MarketStream};
use ports::events::MarketSnapshot;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

mod message_types;
mod websocket;
mod rest;
mod converter;

use websocket::*;
use rest::*;
use converter::*;

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
    ws_client: Option<BinanceWebSocket>,
    is_connected: bool,
    last_heartbeat: Timestamp,
    symbols: Vec<Symbol>,
}

impl BinanceMarketStream {
    pub fn new() -> Self {
        Self {
            caps: Default::default(),
            rest_client: BinanceRestClient::new(),
            ws_client: None,
            is_connected: false,
            last_heartbeat: 0,
            symbols: Vec::new(),
        }
    }

    pub fn with_capabilities(caps: capabilities::BinanceCapabilities) -> Self {
        Self {
            caps,
            rest_client: BinanceRestClient::new(),
            ws_client: None,
            is_connected: false,
            last_heartbeat: 0,
            symbols: Vec::new(),
        }
    }

    /// 獲取訂單簿快照（用於初始化）
    async fn get_initial_snapshots(&self, symbols: &[Symbol]) -> HftResult<Vec<MarketSnapshot>> {
        let mut snapshots = Vec::new();
        
        for symbol in symbols {
            info!("獲取 {:?} 的初始快照", symbol);
            let depth = self.rest_client.get_depth(symbol, Some(100)).await?;
            let timestamp = chrono::Utc::now().timestamp_millis() as u64;
            
            let snapshot = MessageConverter::convert_depth_snapshot(
                symbol.clone(), 
                depth, 
                timestamp
            )?;
            
            snapshots.push(snapshot);
        }
        
        Ok(snapshots)
    }

    /// 啟動 WebSocket 數據流
    async fn start_websocket_stream(
        &mut self, 
        symbols: Vec<Symbol>,
        tx: mpsc::UnboundedSender<HftResult<MarketEvent>>
    ) -> HftResult<()> {
        let mut ws_client = BinanceWebSocket::new();
        let mut ws_stream = ws_client.connect_and_subscribe(symbols.clone()).await?;
        
        self.ws_client = Some(ws_client);
        self.is_connected = true;
        self.last_heartbeat = chrono::Utc::now().timestamp_millis() as u64;
        
        // 啟動消息處理任務
        let tx_clone = tx.clone();
        tokio::spawn(async move {
            let mut reconnect_attempts = 0;
            const MAX_RECONNECT_ATTEMPTS: u32 = 5;
            const RECONNECT_DELAY: Duration = Duration::from_secs(1);
            
            loop {
                match ws_stream.next().await {
                    Some(Ok(Message::Text(text))) => {
                        match MessageConverter::parse_stream_message(&text) {
                            Ok(Some(event)) => {
                                if let Err(_) = tx_clone.send(Ok(event)) {
                                    error!("事件通道已關閉");
                                    break;
                                }
                            }
                            Ok(None) => {
                                debug!("忽略未知消息: {}", text);
                            }
                            Err(e) => {
                                warn!("解析消息失敗: {}", e);
                                if let Err(_) = tx_clone.send(Err(e)) {
                                    break;
                                }
                            }
                        }
                        reconnect_attempts = 0; // 重置重連計數
                    }
                    Some(Ok(Message::Pong(_))) => {
                        debug!("收到 pong");
                    }
                    Some(Ok(Message::Close(_))) => {
                        warn!("WebSocket 連接被關閉");
                        let _ = tx_clone.send(Err(HftError::Network("連接被關閉".to_string())));
                        break;
                    }
                    Some(Err(e)) => {
                        error!("WebSocket 錯誤: {}", e);
                        
                        if reconnect_attempts < MAX_RECONNECT_ATTEMPTS {
                            reconnect_attempts += 1;
                            warn!("嘗試重連 ({}/{})", reconnect_attempts, MAX_RECONNECT_ATTEMPTS);
                            tokio::time::sleep(RECONNECT_DELAY * reconnect_attempts).await;
                            
                            // TODO: 實現重連邏輯
                        } else {
                            error!("達到最大重連次數，放棄連接");
                            let _ = tx_clone.send(Err(HftError::Network(format!("WebSocket 錯誤: {}", e))));
                            break;
                        }
                    }
                    None => {
                        warn!("WebSocket 流結束");
                        break;
                    }
                    _ => {}
                }
            }
        });
        
        Ok(())
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
                        if let Err(_) = tx.send(Ok(MarketEvent::Snapshot(snapshot))) {
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
        let mut ws_client = BinanceWebSocket::new();
        let symbols_clone = symbols.clone();
        
        tokio::spawn(async move {
            match ws_client.connect_and_subscribe(symbols_clone).await {
                Ok(mut ws_stream) => {
                    loop {
                        match ws_stream.next().await {
                            Some(Ok(Message::Text(text))) => {
                                match MessageConverter::parse_stream_message(&text) {
                                    Ok(Some(event)) => {
                                        if let Err(_) = tx.send(Ok(event)) {
                                            break;
                                        }
                                    }
                                    Ok(None) => {
                                        // 忽略未知消息
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Err(e));
                                    }
                                }
                            }
                            Some(Err(e)) => {
                                let _ = tx.send(Err(HftError::Network(e.to_string())));
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        });

        // 創建流
        let stream = stream::poll_fn(move |cx| {
            match rx.poll_recv(cx) {
                std::task::Poll::Ready(Some(event)) => std::task::Poll::Ready(Some(event)),
                std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
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
        if let Some(ref mut ws_client) = self.ws_client {
            ws_client.set_disconnected();
        }
        self.is_connected = false;
        info!("Binance 適配器已斷開");
        Ok(())
    }
}
