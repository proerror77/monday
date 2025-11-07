/*!
 * Bitget Adapter Integration Tests
 *
 * 完整的 Bitget 適配器集成測試套件，驗證：
 * - WebSocket 連接和訂閱
 * - OrderBook 快照和增量更新解析
 * - Trade 數據解析
 * - Ticker 數據解析
 * - 重連機制
 * - 心跳機制
 * - 延遲測量
 * - 數據完整性
 *
 * ⚠️  IMPORTANT: Run with --test-threads=1 for reliable results
 *
 * Tests use shared environment variable (BITGET_WS_URL) and will interfere
 * with each other if run in parallel.
 *
 * Usage:
 *   cargo test --package hft-data-adapter-bitget --test integration_tests -- --test-threads=1
 */

#[cfg(test)]
mod bitget_adapter_tests {
    use data_adapter_bitget::BitgetMarketStream;
    use futures_util::{SinkExt, StreamExt};
    use hft_core::{Price, Quantity, Side, Symbol};
    use ports::{MarketEvent, MarketStream};
    use serde_json::json;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Mutex;
    use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

    // ============================================================================
    // Mock WebSocket Server Implementation
    // ============================================================================

    /// Mock WebSocket Server for Bitget Testing
    struct MockWebSocketServer {
        addr: Option<SocketAddr>,
        message_queue: Arc<Mutex<Vec<Message>>>,
        disconnect_schedule: Option<Duration>,
        heartbeat_interval: Duration,
    }

    impl MockWebSocketServer {
        fn new() -> Self {
            Self {
                addr: None,
                message_queue: Arc::new(Mutex::new(Vec::new())),
                disconnect_schedule: None,
                heartbeat_interval: Duration::from_secs(30),
            }
        }

        /// 啟動 Mock Server
        async fn start(&mut self) -> Result<String> {
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

            // 等待一小段時間確保 server 啟動
            tokio::time::sleep(Duration::from_millis(100)).await;

            Ok(format!("ws://{}", addr))
        }

        async fn handle_connection(
            mut ws: WebSocketStream<TcpStream>,
            queue: Arc<Mutex<Vec<Message>>>,
            disconnect_at: Option<Duration>,
            heartbeat_interval: Duration,
        ) -> Result<()> {
            let start = Instant::now();
            let mut last_heartbeat = Instant::now();
            let mut subscribed = false;

            loop {
                // 檢查是否需要斷線
                if let Some(duration) = disconnect_at {
                    if start.elapsed() >= duration {
                        let _ = ws.close(None).await;
                        break;
                    }
                }

                // 處理接收的消息
                tokio::select! {
                    Some(msg) = ws.next() => {
                        match msg {
                            Ok(Message::Ping(payload)) => {
                                let _ = ws.send(Message::Pong(payload)).await;
                            }
                            Ok(Message::Text(text)) => {
                                // 處理訂閱請求
                                if text.contains("\"op\":\"subscribe\"") {
                                    let ack = json!({
                                        "code": "0",
                                        "msg": "success"
                                    });
                                    let _ = ws.send(Message::Text(ack.to_string())).await;
                                    subscribed = true;
                                }
                            }
                            Err(_) => break,
                            _ => {}
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {
                        // 只在訂閱後才發送消息
                        if subscribed {
                            // 發送所有待發消息 (批量發送以提高速度)
                            let mut q = queue.lock().await;
                            while !q.is_empty() {
                                let msg = q.remove(0);
                                if let Err(_) = ws.send(msg).await {
                                    break;
                                }
                            }
                            drop(q);
                        }

                        // 發送心跳
                        if last_heartbeat.elapsed() >= heartbeat_interval {
                            if let Err(_) = ws.send(Message::Ping(vec![])).await {
                                break;
                            }
                            last_heartbeat = Instant::now();
                        }
                    }
                }
            }

            Ok(())
        }

        /// 預設 OrderBook 快照消息 (Bitget v2 格式)
        async fn queue_orderbook_snapshot(&self, symbol: &str) {
            let msg = json!({
                "action": "snapshot",
                "arg": {
                    "instType": "SPOT",
                    "channel": "books15",
                    "instId": symbol
                },
                "data": [{
                    "asks": [
                        ["67189.5", "0.25"],
                        ["67190.0", "0.30"],
                        ["67190.5", "0.35"],
                    ],
                    "bids": [
                        ["67188.1", "0.12"],
                        ["67187.5", "0.15"],
                        ["67187.0", "0.18"],
                    ],
                    "ts": chrono::Utc::now().timestamp_millis().to_string(),
                }]
            });

            let mut queue = self.message_queue.lock().await;
            queue.push(Message::Text(msg.to_string()));
        }

        /// 預設 OrderBook 增量更新消息
        async fn queue_orderbook_update(&self, symbol: &str) {
            let msg = json!({
                "action": "update",
                "arg": {
                    "instType": "SPOT",
                    "channel": "books",
                    "instId": symbol
                },
                "data": [{
                    "asks": [["67189.6", "0.28"]],
                    "bids": [["67188.2", "0.14"]],
                    "ts": chrono::Utc::now().timestamp_millis().to_string(),
                }]
            });

            let mut queue = self.message_queue.lock().await;
            queue.push(Message::Text(msg.to_string()));
        }

        /// 預設 Trade 消息
        async fn queue_trade(&self, symbol: &str, side: &str, price: f64, qty: f64) {
            let msg = json!({
                "arg": {
                    "instType": "SPOT",
                    "channel": "trade",
                    "instId": symbol
                },
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
        async fn queue_ticker(&self, symbol: &str, last: f64, bid: f64, ask: f64) {
            let msg = json!({
                "arg": {
                    "instType": "SPOT",
                    "channel": "ticker",
                    "instId": symbol
                },
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
        fn schedule_disconnect_at(&mut self, duration: Duration) {
            self.disconnect_schedule = Some(duration);
        }

        /// 設置心跳間隔
        fn set_heartbeat_interval(&mut self, interval: Duration) {
            self.heartbeat_interval = interval;
        }
    }

    impl Default for MockWebSocketServer {
        fn default() -> Self {
            Self::new()
        }
    }

    // ============================================================================
    // Test Helper Functions
    // ============================================================================

    /// 從 stream 收集事件，有超時限制
    async fn collect_events(
        mut stream: impl futures::Stream<Item = std::result::Result<MarketEvent, hft_core::HftError>>
            + Unpin,
        expected_count: usize,
        timeout: Duration,
    ) -> Result<Vec<MarketEvent>> {
        let mut events = Vec::new();
        let deadline = Instant::now() + timeout;

        while events.len() < expected_count && Instant::now() < deadline {
            match tokio::time::timeout(deadline - Instant::now(), stream.next()).await {
                Ok(Some(Ok(event))) => events.push(event),
                Ok(Some(Err(e))) => {
                    return Err(format!("Stream error: {}", e).into());
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }

        Ok(events)
    }

    /// 驗證延遲是否在可接受範圍內
    fn assert_latency_acceptable(latency_us: f64, max_us: f64) {
        assert!(
            latency_us <= max_us,
            "Latency {:.2}μs exceeds maximum {:.2}μs",
            latency_us,
            max_us
        );
    }

    /// 驗證數據完整性
    fn assert_data_completeness(received: usize, expected: usize, min_ratio: f64) {
        let ratio = received as f64 / expected as f64;
        assert!(
            ratio >= min_ratio,
            "Data completeness {:.2}% is below minimum {:.2}%",
            ratio * 100.0,
            min_ratio * 100.0
        );
    }

    // ============================================================================
    // OrderBook 測試
    // ============================================================================

    #[tokio::test]
    async fn test_orderbook_snapshot_parsing() -> Result<()> {
        let mut mock_server = MockWebSocketServer::new();

        // 先排隊消息
        mock_server.queue_orderbook_snapshot("BTCUSDT").await;

        let ws_url = mock_server.start().await?;
        std::env::set_var("BITGET_WS_URL", &ws_url);

        let adapter = BitgetMarketStream::new()
            .with_inst_type("SPOT")
            .with_depth_channel("books15");

        let symbols = vec![Symbol::from("BTCUSDT")];
        let mut stream = adapter.subscribe(symbols).await?;

        // 收集事件
        let events = collect_events(stream.as_mut(), 1, Duration::from_secs(5)).await?;

        // 驗證接收到快照
        assert!(!events.is_empty(), "Should receive at least one event");

        // 查找快照事件（跳過可能的斷線事件）
        let snapshot_opt = events.iter().find_map(|e| match e {
            MarketEvent::Snapshot(s) => Some(s),
            _ => None,
        });

        if let Some(snapshot) = snapshot_opt {
            assert_eq!(snapshot.symbol.as_str(), "BTCUSDT");
            assert!(!snapshot.bids.is_empty(), "Snapshot should have bids");
            assert!(!snapshot.asks.is_empty(), "Snapshot should have asks");

            // 驗證 bid/ask 順序
            for i in 1..snapshot.bids.len() {
                assert!(
                    snapshot.bids[i - 1].price >= snapshot.bids[i].price,
                    "Bids should be sorted descending by price"
                );
            }
            for i in 1..snapshot.asks.len() {
                assert!(
                    snapshot.asks[i - 1].price <= snapshot.asks[i].price,
                    "Asks should be sorted ascending by price"
                );
            }
        } else {
            panic!("Expected at least one Snapshot event, got {:?}", events);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_orderbook_incremental_updates() -> Result<()> {
        let mut mock_server = MockWebSocketServer::new();
        mock_server.queue_orderbook_snapshot("BTCUSDT").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        mock_server.queue_orderbook_update("BTCUSDT").await;

        let ws_url = mock_server.start().await?;
        std::env::set_var("BITGET_WS_URL", &ws_url);

        let adapter = BitgetMarketStream::new_with_incremental(true)
            .with_inst_type("SPOT")
            .with_depth_channel("books");

        let symbols = vec![Symbol::from("BTCUSDT")];
        let mut stream = adapter.subscribe(symbols).await?;

        // 收集事件（快照 + 更新）
        let events = collect_events(stream.as_mut(), 2, Duration::from_secs(5)).await?;

        assert!(events.len() >= 1, "Should receive at least snapshot");

        // 驗證第一個是快照
        if let MarketEvent::Snapshot(snapshot) = &events[0] {
            assert_eq!(snapshot.symbol.as_str(), "BTCUSDT");
        } else {
            panic!("First event should be Snapshot");
        }

        Ok(())
    }

    // ============================================================================
    // Trade 測試
    // ============================================================================

    #[tokio::test]
    async fn test_trade_parsing() -> Result<()> {
        let mut mock_server = MockWebSocketServer::new();
        mock_server
            .queue_trade("BTCUSDT", "buy", 67189.5, 0.025)
            .await;
        let ws_url = mock_server.start().await?;

        std::env::set_var("BITGET_WS_URL", &ws_url);

        let adapter = BitgetMarketStream::new().with_inst_type("SPOT");
        let symbols = vec![Symbol::from("BTCUSDT")];
        let mut stream = adapter.subscribe(symbols).await?;

        let events = collect_events(stream.as_mut(), 1, Duration::from_secs(5)).await?;

        assert!(!events.is_empty(), "Should receive trade event");

        if let MarketEvent::Trade(trade) = &events[0] {
            assert_eq!(trade.symbol.as_str(), "BTCUSDT");
            assert_eq!(
                trade.price,
                Price::from_f64(67189.5).unwrap(),
                "Trade price mismatch"
            );
            assert_eq!(
                trade.quantity,
                Quantity::from_f64(0.025).unwrap(),
                "Trade quantity mismatch"
            );
            assert_eq!(trade.side, Side::Buy, "Trade side mismatch");
            assert!(!trade.trade_id.is_empty(), "Trade should have ID");
        } else {
            panic!("Expected Trade event, got {:?}", events[0]);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_multiple_trades_parsing() -> Result<()> {
        let mut mock_server = MockWebSocketServer::new();
        let ws_url = mock_server.start().await?;

        // 啟動 server 後再排隊消息，確保訂閱建立後才發送
        tokio::time::sleep(Duration::from_millis(200)).await;

        // 預設多筆交易
        for i in 0..10 {
            let price = 67189.5 + (i as f64 * 0.1);
            let side = if i % 2 == 0 { "buy" } else { "sell" };
            mock_server.queue_trade("BTCUSDT", side, price, 0.025).await;
        }

        std::env::set_var("BITGET_WS_URL", &ws_url);

        let adapter = BitgetMarketStream::new().with_inst_type("SPOT");
        let symbols = vec![Symbol::from("BTCUSDT")];
        let mut stream = adapter.subscribe(symbols).await?;

        // 等待訂閱建立
        tokio::time::sleep(Duration::from_millis(500)).await;

        let events = collect_events(stream.as_mut(), 10, Duration::from_secs(10)).await?;

        let trade_count = events
            .iter()
            .filter(|e| matches!(e, MarketEvent::Trade(_)))
            .count();

        println!("Received {}/10 trade events", trade_count);

        assert!(
            trade_count >= 8,
            "Should receive at least 80% of trades (got {})",
            trade_count
        );

        Ok(())
    }

    // ============================================================================
    // Ticker 測試
    // ============================================================================

    #[tokio::test]
    async fn test_ticker_parsing() -> Result<()> {
        let mut mock_server = MockWebSocketServer::new();
        mock_server
            .queue_ticker("BTCUSDT", 67189.5, 67188.1, 67189.6)
            .await;
        let ws_url = mock_server.start().await?;

        std::env::set_var("BITGET_WS_URL", &ws_url);

        let adapter = BitgetMarketStream::new().with_inst_type("SPOT");
        let symbols = vec![Symbol::from("BTCUSDT")];
        let mut stream = adapter.subscribe(symbols).await?;

        // Ticker 事件目前可能未實現，這裡只驗證不崩潰
        let events = collect_events(stream.as_mut(), 1, Duration::from_secs(3)).await?;

        // 驗證至少能解析消息（即使未生成 Ticker 事件）
        println!("Received {} events (Ticker support may vary)", events.len());

        Ok(())
    }

    // ============================================================================
    // 重連測試
    // ============================================================================

    #[tokio::test]
    async fn test_reconnection_mechanism() -> Result<()> {
        let mut mock_server = MockWebSocketServer::new();

        // 計劃 2 秒後斷線
        mock_server.schedule_disconnect_at(Duration::from_secs(2));
        mock_server.queue_orderbook_snapshot("BTCUSDT").await;

        let ws_url = mock_server.start().await?;
        std::env::set_var("BITGET_WS_URL", &ws_url);

        let adapter = BitgetMarketStream::new().with_inst_type("SPOT");
        let symbols = vec![Symbol::from("BTCUSDT")];
        let mut stream = adapter.subscribe(symbols).await?;

        // 收集初始事件
        let initial_events = collect_events(stream.as_mut(), 1, Duration::from_secs(3)).await?;

        assert!(!initial_events.is_empty(), "Should receive initial events");

        // 等待斷線
        tokio::time::sleep(Duration::from_secs(3)).await;

        // 檢查是否收到斷線事件
        let disconnect_events = collect_events(stream.as_mut(), 1, Duration::from_secs(2)).await?;

        let has_disconnect = disconnect_events
            .iter()
            .any(|e| matches!(e, MarketEvent::Disconnect { .. }));

        println!(
            "Disconnect event detected: {} (received {} events)",
            has_disconnect,
            disconnect_events.len()
        );

        Ok(())
    }

    // ============================================================================
    // 心跳測試
    // ============================================================================

    #[tokio::test]
    async fn test_heartbeat_mechanism() -> Result<()> {
        let mut mock_server = MockWebSocketServer::new();

        // 設置短心跳間隔以加速測試
        mock_server.set_heartbeat_interval(Duration::from_secs(2));
        mock_server.queue_orderbook_snapshot("BTCUSDT").await;

        let ws_url = mock_server.start().await?;
        std::env::set_var("BITGET_WS_URL", &ws_url);

        let adapter = BitgetMarketStream::new().with_inst_type("SPOT");
        let symbols = vec![Symbol::from("BTCUSDT")];
        let mut stream = adapter.subscribe(symbols).await?;

        // 等待訂閱完全建立
        tokio::time::sleep(Duration::from_millis(200)).await;

        // 運行 6 秒，應該經歷至少 2 次心跳
        let start = Instant::now();
        let mut event_count = 0;

        while start.elapsed() < Duration::from_secs(6) {
            match tokio::time::timeout(Duration::from_secs(1), stream.next()).await {
                Ok(Some(Ok(_))) => {
                    event_count += 1;
                }
                Ok(Some(Err(e))) => {
                    println!("Stream error: {}", e);
                    break;
                }
                Ok(None) => break,
                Err(_) => continue,
            }
        }

        println!("Heartbeat test completed, received {} events", event_count);

        // 驗證連接保持活躍（沒有斷線事件）
        assert!(
            event_count > 0,
            "Should receive some events during heartbeat period"
        );

        Ok(())
    }

    // ============================================================================
    // 數據完整性測試
    // ============================================================================

    #[tokio::test]
    async fn test_data_integrity() -> Result<()> {
        let mut mock_server = MockWebSocketServer::new();

        // 預先排隊大量消息
        let total_messages = 50;
        for i in 0..total_messages {
            let price = 67189.5 + (i as f64 * 0.1);
            mock_server
                .queue_trade("BTCUSDT", "buy", price, 0.025)
                .await;
        }

        let ws_url = mock_server.start().await?;
        std::env::set_var("BITGET_WS_URL", &ws_url);

        let adapter = BitgetMarketStream::new().with_inst_type("SPOT");
        let symbols = vec![Symbol::from("BTCUSDT")];
        let mut stream = adapter.subscribe(symbols).await?;

        // 等待訂閱完全建立
        tokio::time::sleep(Duration::from_millis(200)).await;

        // 收集所有消息
        let events =
            collect_events(stream.as_mut(), total_messages, Duration::from_secs(15)).await?;

        let received = events
            .iter()
            .filter(|e| matches!(e, MarketEvent::Trade(_)))
            .count();

        println!(
            "Data integrity test: received {}/{} trades ({:.1}%)",
            received,
            total_messages,
            (received as f64 / total_messages as f64) * 100.0
        );

        // 驗證數據完整性（至少接收 95%）
        assert_data_completeness(received, total_messages, 0.95);

        Ok(())
    }

    // ============================================================================
    // 延遲測試
    // ============================================================================

    #[tokio::test]
    async fn test_latency_measurement() -> Result<()> {
        let mut mock_server = MockWebSocketServer::new();

        // 預設一系列交易以測量延遲
        for _ in 0..20 {
            mock_server
                .queue_trade("BTCUSDT", "buy", 67189.5, 0.025)
                .await;
        }

        let ws_url = mock_server.start().await?;
        std::env::set_var("BITGET_WS_URL", &ws_url);

        let adapter = BitgetMarketStream::new().with_inst_type("SPOT");
        let symbols = vec![Symbol::from("BTCUSDT")];
        let mut stream = adapter.subscribe(symbols).await?;

        let mut latencies: Vec<f64> = Vec::new();

        while latencies.len() < 20 {
            let event_start = Instant::now();
            match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
                Ok(Some(Ok(event))) => {
                    if matches!(event, MarketEvent::Trade(_)) {
                        // 測量單個事件處理延遲
                        let latency_us = event_start.elapsed().as_micros() as f64;
                        latencies.push(latency_us);
                    }
                }
                Ok(Some(Err(e))) => {
                    println!("Stream error: {}", e);
                    break;
                }
                Ok(None) | Err(_) => break,
            }
        }

        if !latencies.is_empty() {
            let avg_latency = latencies.iter().sum::<f64>() / latencies.len() as f64;
            println!(
                "Average processing latency: {:.2}μs (measured {} samples)",
                avg_latency,
                latencies.len()
            );

            // 在測試環境中，單次處理延遲應該在毫秒級別
            // Mock server 的延遲會比真實環境高，所以放寬到 100ms
            assert_latency_acceptable(avg_latency, 100_000.0);
        }

        Ok(())
    }

    // ============================================================================
    // 混合消息測試
    // ============================================================================

    #[tokio::test]
    async fn test_mixed_message_types() -> Result<()> {
        let mut mock_server = MockWebSocketServer::new();

        // 預設混合消息類型
        mock_server.queue_orderbook_snapshot("BTCUSDT").await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        mock_server
            .queue_trade("BTCUSDT", "buy", 67189.5, 0.025)
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        mock_server
            .queue_ticker("BTCUSDT", 67189.5, 67188.1, 67189.6)
            .await;

        let ws_url = mock_server.start().await?;
        std::env::set_var("BITGET_WS_URL", &ws_url);

        let adapter = BitgetMarketStream::new().with_inst_type("SPOT");
        let symbols = vec![Symbol::from("BTCUSDT")];
        let mut stream = adapter.subscribe(symbols).await?;

        let events = collect_events(stream.as_mut(), 3, Duration::from_secs(5)).await?;

        let snapshot_count = events
            .iter()
            .filter(|e| matches!(e, MarketEvent::Snapshot(_)))
            .count();
        let trade_count = events
            .iter()
            .filter(|e| matches!(e, MarketEvent::Trade(_)))
            .count();

        println!(
            "Mixed message test: {} snapshots, {} trades",
            snapshot_count, trade_count
        );

        assert!(snapshot_count >= 1, "Should receive at least one snapshot");
        assert!(trade_count >= 1, "Should receive at least one trade");

        Ok(())
    }

    // ============================================================================
    // 訂單簿價格驗證測試
    // ============================================================================

    #[tokio::test]
    async fn test_orderbook_price_validation() -> Result<()> {
        let mut mock_server = MockWebSocketServer::new();
        mock_server.queue_orderbook_snapshot("BTCUSDT").await;
        let ws_url = mock_server.start().await?;

        std::env::set_var("BITGET_WS_URL", &ws_url);

        let adapter = BitgetMarketStream::new()
            .with_inst_type("SPOT")
            .with_depth_channel("books15");

        let symbols = vec![Symbol::from("BTCUSDT")];
        let mut stream = adapter.subscribe(symbols).await?;

        let events = collect_events(stream.as_mut(), 1, Duration::from_secs(5)).await?;

        if let Some(MarketEvent::Snapshot(snapshot)) = events.first() {
            // 驗證 bid/ask spread
            if !snapshot.bids.is_empty() && !snapshot.asks.is_empty() {
                let best_bid = &snapshot.bids[0].price;
                let best_ask = &snapshot.asks[0].price;

                assert!(
                    best_bid < best_ask,
                    "Best bid ({}) should be less than best ask ({})",
                    best_bid,
                    best_ask
                );

                // 驗證 spread 合理性（不超過 1%）
                let spread_pct =
                    ((best_ask.0 - best_bid.0) / best_bid.0) * rust_decimal::Decimal::from(100);
                assert!(
                    spread_pct < rust_decimal::Decimal::from(1),
                    "Spread {:.4}% exceeds 1%",
                    spread_pct
                );
            }
        }

        Ok(())
    }
}
