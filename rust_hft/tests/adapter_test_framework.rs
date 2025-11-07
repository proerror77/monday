/*!
 * Unified Adapter Test Framework
 *
 * 統一的 Adapter 測試基礎設施，提供：
 * - Mock WebSocket Server
 * - 標準測試 Trait
 * - 測試套件生成器
 */

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};
use anyhow::Result;

/// Mock WebSocket Server for Adapter Testing
pub struct MockWebSocketServer {
    addr: Option<SocketAddr>,
    message_queue: Arc<Mutex<Vec<Message>>>,
    disconnect_schedule: Option<Duration>,
    heartbeat_interval: Option<Duration>,
}

impl MockWebSocketServer {
    pub fn new() -> Self {
        Self {
            addr: None,
            message_queue: Arc::new(Mutex::new(Vec::new())),
            disconnect_schedule: None,
            heartbeat_interval: Some(Duration::from_secs(30)),
        }
    }

    /// 啟動 Mock WS Server
    pub async fn start(&mut self) -> Result<String> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        self.addr = Some(addr);

        let queue = self.message_queue.clone();
        let disconnect_at = self.disconnect_schedule;
        let heartbeat = self.heartbeat_interval;

        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let ws = match accept_async(stream).await {
                    Ok(ws) => ws,
                    Err(_) => continue,
                };

                let queue = queue.clone();

                tokio::spawn(async move {
                    let _ = Self::handle_connection(ws, queue, disconnect_at, heartbeat).await;
                });
            }
        });

        Ok(format!("ws://{}", addr))
    }

    async fn handle_connection(
        mut ws: WebSocketStream<TcpStream>,
        queue: Arc<Mutex<Vec<Message>>>,
        disconnect_at: Option<Duration>,
        heartbeat_interval: Option<Duration>,
    ) -> Result<()> {
        let start = Instant::now();
        let mut last_heartbeat = Instant::now();

        loop {
            // 檢查是否需要斷線
            if let Some(duration) = disconnect_at {
                if start.elapsed() >= duration {
                    ws.close(None).await?;
                    break;
                }
            }

            // 處理接收的消息（心跳響應）
            tokio::select! {
                Some(msg) = ws.next() => {
                    match msg? {
                        Message::Ping(payload) => {
                            ws.send(Message::Pong(payload)).await?;
                        }
                        Message::Text(text) => {
                            // 處理訂閱請求等
                            if text.contains("\"op\":\"subscribe\"") {
                                let ack = json!({
                                    "event": "subscribe",
                                    "code": "0"
                                });
                                ws.send(Message::Text(ack.to_string())).await?;
                            }
                        }
                        _ => {}
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    // 發送預設消息
                    let mut q = queue.lock().await;
                    if let Some(msg) = q.pop() {
                        ws.send(msg).await?;
                    }
                    drop(q);

                    // 發送心跳
                    if let Some(interval) = heartbeat_interval {
                        if last_heartbeat.elapsed() >= interval {
                            ws.send(Message::Ping(vec![])).await?;
                            last_heartbeat = Instant::now();
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// 預設 OrderBook 快照消息 (Bitget 格式)
    pub async fn queue_orderbook_snapshot(&self, symbol: &str) {
        let msg = json!({
            "action": "snapshot",
            "arg": {"instId": symbol, "channel": "books15"},
            "data": [{
                "asks": [
                    ["67189.5", "0.25", 1],
                    ["67190.0", "0.30", 2],
                    ["67190.5", "0.35", 3],
                ],
                "bids": [
                    ["67188.1", "0.12", 1],
                    ["67187.5", "0.15", 2],
                    ["67187.0", "0.18", 3],
                ],
                "ts": chrono::Utc::now().timestamp_millis().to_string(),
            }]
        });

        let mut queue = self.message_queue.lock().await;
        queue.push(Message::Text(msg.to_string()));
    }

    /// 預設 OrderBook 增量更新消息
    pub async fn queue_orderbook_update(&self, symbol: &str) {
        let msg = json!({
            "action": "update",
            "arg": {"instId": symbol, "channel": "books15"},
            "data": [{
                "asks": [["67189.6", "0.28", 1]],
                "bids": [["67188.2", "0.14", 1]],
                "ts": chrono::Utc::now().timestamp_millis().to_string(),
            }]
        });

        let mut queue = self.message_queue.lock().await;
        queue.push(Message::Text(msg.to_string()));
    }

    /// 預設 Trades 消息
    pub async fn queue_trade(&self, symbol: &str, side: &str, price: f64, qty: f64) {
        let msg = json!({
            "action": "snapshot",
            "arg": {"instId": symbol, "channel": "trade"},
            "data": [{
                "px": price.to_string(),
                "sz": qty.to_string(),
                "side": side,
                "ts": chrono::Utc::now().timestamp_millis().to_string(),
                "tradeId": uuid::Uuid::new_v4().to_string(),
            }]
        });

        let mut queue = self.message_queue.lock().await;
        queue.push(Message::Text(msg.to_string()));
    }

    /// 預設 Ticker 消息
    pub async fn queue_ticker(&self, symbol: &str, last: f64, bid: f64, ask: f64) {
        let msg = json!({
            "action": "snapshot",
            "arg": {"instId": symbol, "channel": "ticker"},
            "data": [{
                "last": last.to_string(),
                "bestBid": bid.to_string(),
                "bestAsk": ask.to_string(),
                "ts": chrono::Utc::now().timestamp_millis().to_string(),
            }]
        });

        let mut queue = self.message_queue.lock().await;
        queue.push(Message::Text(msg.to_string()));
    }

    /// 計劃在指定時間後斷線
    pub fn schedule_disconnect_at(&mut self, duration: Duration) {
        self.disconnect_schedule = Some(duration);
    }

    /// 設置心跳間隔
    pub fn set_heartbeat_interval(&mut self, interval: Duration) {
        self.heartbeat_interval = Some(interval);
    }

    /// 獲取 Server 地址
    pub fn addr(&self) -> Option<SocketAddr> {
        self.addr
    }
}

impl Default for MockWebSocketServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Adapter 測試的標準 Trait
#[async_trait]
pub trait AdapterTestable {
    /// 連接測試
    async fn test_connection(&self) -> Result<()>;

    /// OrderBook 快照解析測試
    async fn test_orderbook_snapshot_parsing(&self) -> Result<()>;

    /// OrderBook 增量解析測試
    async fn test_orderbook_update_parsing(&self) -> Result<()>;

    /// Trades 解析測試
    async fn test_trades_parsing(&self) -> Result<()>;

    /// Ticker 解析測試
    async fn test_ticker_parsing(&self) -> Result<()>;

    /// 重連測試
    async fn test_reconnection(&self) -> Result<()>;

    /// 心跳測試
    async fn test_heartbeat(&self) -> Result<()>;

    /// 數據完整性測試
    async fn test_data_integrity(&self) -> Result<()>;
}

/// 測試輔助結構
pub struct AdapterTestHelper;

impl AdapterTestHelper {
    /// 等待特定數量的事件
    pub async fn wait_for_events<T>(
        mut rx: tokio::sync::mpsc::Receiver<T>,
        count: usize,
        timeout: Duration,
    ) -> Result<Vec<T>> {
        let mut events = Vec::new();
        let deadline = Instant::now() + timeout;

        while events.len() < count && Instant::now() < deadline {
            match tokio::time::timeout(deadline - Instant::now(), rx.recv()).await {
                Ok(Some(event)) => events.push(event),
                Ok(None) => break,
                Err(_) => break,
            }
        }

        if events.len() < count {
            anyhow::bail!(
                "Expected {} events but got {} within {:?}",
                count,
                events.len(),
                timeout
            );
        }

        Ok(events)
    }

    /// 驗證延遲
    pub fn assert_latency(latency_ns: u64, max_latency_us: f64) {
        let latency_us = latency_ns as f64 / 1000.0;
        assert!(
            latency_us <= max_latency_us,
            "Latency {:.2}μs exceeds max {:.2}μs",
            latency_us,
            max_latency_us
        );
    }

    /// 驗證數據完整性
    pub fn assert_data_completeness(received: usize, expected: usize, min_ratio: f64) {
        let ratio = received as f64 / expected as f64;
        assert!(
            ratio >= min_ratio,
            "Data completeness {:.2}% < {:.2}%",
            ratio * 100.0,
            min_ratio * 100.0
        );
    }
}

/// Adapter 測試套件生成器
#[macro_export]
macro_rules! adapter_test_suite {
    ($adapter_name:ident, $venue:expr) => {
        #[cfg(test)]
        mod adapter_tests {
            use super::*;
            use crate::tests::adapter_test_framework::*;
            use std::time::Duration;

            #[tokio::test]
            async fn connection_test() {
                let mut mock_server = MockWebSocketServer::new();
                let ws_url = mock_server.start().await.unwrap();

                // TODO: 實現實際的 adapter 連接測試
                // let adapter = $adapter_name::new(&ws_url, "BTCUSDT");
                // assert!(adapter.test_connection().await.is_ok());
            }

            #[tokio::test]
            async fn orderbook_snapshot_parsing_test() {
                let mut mock_server = MockWebSocketServer::new();
                mock_server.queue_orderbook_snapshot("BTCUSDT").await;
                let ws_url = mock_server.start().await.unwrap();

                // TODO: 實現實際的解析測試
                // let adapter = $adapter_name::new(&ws_url, "BTCUSDT");
                // assert!(adapter.test_orderbook_snapshot_parsing().await.is_ok());
            }

            #[tokio::test]
            async fn orderbook_update_parsing_test() {
                let mut mock_server = MockWebSocketServer::new();
                mock_server.queue_orderbook_update("BTCUSDT").await;
                let ws_url = mock_server.start().await.unwrap();

                // TODO: 實現實際的解析測試
            }

            #[tokio::test]
            async fn trades_parsing_test() {
                let mut mock_server = MockWebSocketServer::new();
                mock_server.queue_trade("BTCUSDT", "buy", 67189.5, 0.025).await;
                let ws_url = mock_server.start().await.unwrap();

                // TODO: 實現實際的解析測試
            }

            #[tokio::test]
            async fn reconnection_test() {
                let mut mock_server = MockWebSocketServer::new();
                mock_server.schedule_disconnect_at(Duration::from_secs(5));
                let ws_url = mock_server.start().await.unwrap();

                // TODO: 實現實際的重連測試
                // let adapter = $adapter_name::new(&ws_url, "BTCUSDT");
                // assert!(adapter.test_reconnection().await.is_ok());
            }

            #[tokio::test]
            async fn heartbeat_test() {
                let mut mock_server = MockWebSocketServer::new();
                mock_server.set_heartbeat_interval(Duration::from_secs(10));
                let ws_url = mock_server.start().await.unwrap();

                // TODO: 實現實際的心跳測試
            }

            #[tokio::test]
            async fn data_integrity_test() {
                let mut mock_server = MockWebSocketServer::new();

                // 預設一系列消息
                for i in 0..100 {
                    mock_server.queue_trade("BTCUSDT", "buy", 67189.5 + i as f64, 0.025).await;
                }

                let ws_url = mock_server.start().await.unwrap();

                // TODO: 實現實際的數據完整性測試
                // 驗證接收到至少 99 個消息（99% 完整性）
            }
        }
    };
}
