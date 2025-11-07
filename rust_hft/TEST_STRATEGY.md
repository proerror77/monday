# HFT 系統測試策略與缺口分析

## 1. 測試覆蓋缺口分析

### 1.1 關鍵路徑缺口 (Critical - P0)

**Strategy → Risk → Execution 完整鏈路**
```
❌ 缺失: 策略→風控→執行的集成測試
❌ 缺失: 多策略並行執行測試
❌ 缺失: 風控熔斷觸發測試
❌ 缺失: 執行失敗重試邏輯測試
```

**Adapter 測試標準化**
```
✅ 有測試: adapter-backpack, adapter-hyperliquid, adapter-binance (部分)
❌ 缺失: adapter-bitget, adapter-bybit, adapter-okx, adapter-lighter
❌ 缺失: 統一的 Adapter 測試框架
❌ 缺失: Mock WebSocket 服務器
```

**Risk Manager 集成測試**
```
✅ 有測試: 8 個單元測試 (風控邏輯)
❌ 缺失: 與 Strategy 集成測試
❌ 缺失: 實時風控響應測試
❌ 缺失: 風控降級場景測試
```

**Portfolio & OMS**
```
❌ 缺失: Portfolio 狀態更新測試
❌ 缺失: OMS 訂單生命週期測試
❌ 缺失: 多帳戶會計測試
```

### 1.2 性能與穩定性缺口 (High - P1)

```
❌ 缺失: 數據回放框架 (Replay Mode)
❌ 缺失: Paper Trading 端到端測試
❌ 缺失: 延遲監控與告警測試
❌ 缺失: 連接中斷與恢復測試
❌ 缺失: 內存洩漏與資源管理測試
```

### 1.3 可觀測性缺口 (Medium - P2)

```
❌ 缺失: Metrics 導出集成測試
❌ 缺失: 事件發佈與訂閱測試
❌ 缺失: ClickHouse 數據持久化測試
❌ 缺失: Redis Pub/Sub 可靠性測試
```

---

## 2. E2E 測試案例設計

### 2.1 單策略完整流程 (Scenario 1)

**目標**: 驗證 Trend 策略從數據接入到訂單執行的完整鏈路

```rust
/// E2E Test: Trend Strategy Complete Flow
///
/// Flow: WS Data → OrderBook → Trend Strategy → Risk Check → Paper Execution
/// Duration: 60s
/// Expected: 至少 1 個有效信號 + 0 風控拒絕
#[tokio::test]
#[serial]
async fn e2e_trend_strategy_paper_trading() -> Result<()> {
    // 1. Setup: 初始化 Paper Trading 環境
    let config = TestConfigBuilder::paper_trading()
        .with_venue(Venue::BITGET)
        .with_symbol("BTCUSDT")
        .with_strategy(StrategyType::Trend {
            ema_fast: 12,
            ema_slow: 26,
            rsi_period: 14
        })
        .build();

    // 2. 啟動系統
    let system = HftSystem::new(config).await?;
    system.start().await?;

    // 3. 運行 60 秒並收集事件
    let events = system.run_for_duration(Duration::from_secs(60)).await?;

    // 4. 驗證
    assert!(events.signals.len() >= 1, "至少產生 1 個信號");
    assert_eq!(events.risk_rejections.len(), 0, "無風控拒絕");
    assert!(events.executions.len() >= 1, "至少執行 1 個訂單");

    // 5. 性能驗證
    assert!(events.latency_p99_us() <= 25.0, "p99 延遲 <= 25μs");

    Ok(())
}
```

### 2.2 跨交易所套利 (Scenario 2)

**目標**: 驗證 Arbitrage 策略的雙邊同步執行

```rust
/// E2E Test: Cross-Exchange Arbitrage
///
/// Flow: Binance + Bitget → TopN Joiner → Arb Strategy → Dual-Leg Execution
/// Duration: 120s
/// Expected: 至少 1 個套利機會 + 雙腿同步執行
#[tokio::test]
#[serial]
async fn e2e_cross_exchange_arbitrage() -> Result<()> {
    let config = TestConfigBuilder::paper_trading()
        .with_venues(vec![Venue::BINANCE, Venue::BITGET])
        .with_symbol("BTCUSDT")
        .with_strategy(StrategyType::Arbitrage {
            net_spread_bps: 1.2,
            min_qty: 0.002,
            tif: "IOC",
        })
        .build();

    let system = HftSystem::new(config).await?;
    system.start().await?;

    let events = system.run_for_duration(Duration::from_secs(120)).await?;

    // 驗證套利執行
    assert!(events.arb_opportunities.len() >= 1, "至少 1 個套利機會");

    for arb in events.arb_opportunities.iter() {
        // 檢查雙腿延遲差
        let skew_ms = arb.leg2_exec_ts - arb.leg1_exec_ts;
        assert!(skew_ms <= 5, "雙腿執行延遲差 <= 5ms");
    }

    Ok(())
}
```

### 2.3 風控熔斷與恢復 (Scenario 3)

**目標**: 驗證風控系統的熔斷和恢復機制

```rust
/// E2E Test: Risk Circuit Breaker
///
/// Scenario:
/// 1. 策略快速產生訂單
/// 2. 觸發風控限額 → 熔斷
/// 3. 冷卻期後 → 恢復
#[tokio::test]
#[serial]
async fn e2e_risk_circuit_breaker() -> Result<()> {
    let config = TestConfigBuilder::paper_trading()
        .with_strategy(StrategyType::Trend { /* aggressive params */ })
        .with_risk_config(RiskConfig {
            max_notional: 5000.0,
            max_orders_per_sec: 5,
            cooldown_ms: 5000,
        })
        .build();

    let system = HftSystem::new(config).await?;
    system.start().await?;

    let events = system.run_for_duration(Duration::from_secs(30)).await?;

    // 驗證熔斷觸發
    assert!(events.circuit_breaker_triggered, "熔斷必須被觸發");

    // 驗證冷卻期
    let recovery_time = events.circuit_breaker_recovery_ts - events.circuit_breaker_trigger_ts;
    assert!(recovery_time >= 5000, "冷卻期至少 5 秒");

    Ok(())
}
```

### 2.4 連接中斷與數據完整性 (Scenario 4)

**目標**: 驗證 WS 斷線重連與數據補全

```rust
/// E2E Test: Connection Resilience
///
/// Scenario:
/// 1. 正常接收數據 30s
/// 2. 模擬 WS 斷線
/// 3. 觸發重連 + REST 補數據
/// 4. 驗證數據完整性
#[tokio::test]
#[serial]
async fn e2e_connection_resilience() -> Result<()> {
    let mut mock_ws = MockWebSocketServer::new();
    mock_ws.schedule_disconnect_at(Duration::from_secs(30));

    let config = TestConfigBuilder::paper_trading()
        .with_mock_websocket(mock_ws)
        .build();

    let system = HftSystem::new(config).await?;
    system.start().await?;

    let events = system.run_for_duration(Duration::from_secs(90)).await?;

    // 驗證重連
    assert!(events.reconnections.len() >= 1, "至少 1 次重連");

    // 驗證數據完整性
    let gap_duration_ms = events.max_data_gap_ms();
    assert!(gap_duration_ms <= 500, "數據缺口 <= 500ms");

    Ok(())
}
```

### 2.5 性能與資源管理 (Scenario 5)

**目標**: 長時間運行的性能和資源穩定性

```rust
/// E2E Test: Long-Running Performance
///
/// Duration: 1 小時
/// 驗證: 延遲穩定性 + 內存穩定性
#[tokio::test]
#[serial]
#[ignore] // 手動執行
async fn e2e_long_running_stability() -> Result<()> {
    let config = TestConfigBuilder::paper_trading()
        .with_venues(vec![Venue::BINANCE, Venue::BITGET])
        .with_symbols(vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"])
        .build();

    let system = HftSystem::new(config).await?;

    // 啟動資源監控
    let resource_monitor = ResourceMonitor::new();
    resource_monitor.start();

    system.start().await?;
    let events = system.run_for_duration(Duration::from_secs(3600)).await?;

    // 驗證延遲穩定性
    let latency_trend = events.latency_over_time();
    assert!(latency_trend.is_stable(), "延遲無明顯上升趨勢");

    // 驗證內存穩定性
    let memory_usage = resource_monitor.memory_stats();
    assert!(memory_usage.is_stable(), "內存無洩漏");

    Ok(())
}
```

---

## 3. Adapter 測試腳手架

### 3.1 統一 Adapter 測試框架

```rust
/// tests/adapter_test_framework.rs
///
/// 統一的 Adapter 測試基礎設施

use async_trait::async_trait;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Mock WebSocket Server for Adapter Testing
pub struct MockWebSocketServer {
    addr: String,
    message_queue: Arc<Mutex<Vec<Message>>>,
    disconnect_schedule: Option<Duration>,
}

impl MockWebSocketServer {
    pub fn new() -> Self {
        Self {
            addr: "127.0.0.1:0".to_string(),
            message_queue: Arc::new(Mutex::new(Vec::new())),
            disconnect_schedule: None,
        }
    }

    /// 啟動 Mock WS Server
    pub async fn start(&mut self) -> Result<String> {
        let listener = TcpListener::bind(&self.addr).await?;
        let addr = listener.local_addr()?;
        self.addr = format!("ws://{}", addr);

        let queue = self.message_queue.clone();
        let disconnect_at = self.disconnect_schedule;

        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let ws = accept_async(stream).await.unwrap();
                let queue = queue.clone();

                tokio::spawn(async move {
                    Self::handle_connection(ws, queue, disconnect_at).await;
                });
            }
        });

        Ok(self.addr.clone())
    }

    async fn handle_connection(
        mut ws: WebSocketStream<TcpStream>,
        queue: Arc<Mutex<Vec<Message>>>,
        disconnect_at: Option<Duration>,
    ) {
        let start = Instant::now();

        loop {
            // 檢查是否需要斷線
            if let Some(duration) = disconnect_at {
                if start.elapsed() >= duration {
                    let _ = ws.close(None).await;
                    break;
                }
            }

            // 發送預設消息
            let mut q = queue.lock().await;
            if let Some(msg) = q.pop() {
                let _ = ws.send(msg).await;
            }
            drop(q);

            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 預設 OrderBook 快照消息
    pub async fn queue_orderbook_snapshot(&self, symbol: &str) {
        let msg = json!({
            "action": "snapshot",
            "arg": {"instId": symbol},
            "data": [{
                "asks": [["67189.5", "0.25", 1], ["67190.0", "0.30", 2]],
                "bids": [["67188.1", "0.12", 1], ["67187.5", "0.15", 2]],
                "ts": "1721810005123"
            }]
        });

        let mut queue = self.message_queue.lock().await;
        queue.push(Message::Text(msg.to_string()));
    }

    /// 預設 Trades 消息
    pub async fn queue_trade(&self, symbol: &str, side: &str, price: f64, qty: f64) {
        let msg = json!({
            "arg": {"instId": symbol},
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

    /// 計劃在指定時間後斷線
    pub fn schedule_disconnect_at(&mut self, duration: Duration) {
        self.disconnect_schedule = Some(duration);
    }
}

/// Adapter 測試的標準 Trait
#[async_trait]
pub trait AdapterTestable {
    /// 連接測試
    async fn test_connection(&self) -> Result<()>;

    /// OrderBook 解析測試
    async fn test_orderbook_parsing(&self) -> Result<()>;

    /// Trades 解析測試
    async fn test_trades_parsing(&self) -> Result<()>;

    /// 重連測試
    async fn test_reconnection(&self) -> Result<()>;

    /// 心跳測試
    async fn test_heartbeat(&self) -> Result<()>;
}

/// Adapter 測試套件生成器
#[macro_export]
macro_rules! adapter_test_suite {
    ($adapter_name:ident) => {
        mod adapter_tests {
            use super::*;
            use crate::tests::adapter_test_framework::*;

            #[tokio::test]
            async fn connection_test() {
                let mut mock_server = MockWebSocketServer::new();
                let ws_url = mock_server.start().await.unwrap();

                let adapter = $adapter_name::new(&ws_url, "BTCUSDT");
                assert!(adapter.test_connection().await.is_ok());
            }

            #[tokio::test]
            async fn orderbook_parsing_test() {
                let mut mock_server = MockWebSocketServer::new();
                mock_server.queue_orderbook_snapshot("BTCUSDT").await;
                let ws_url = mock_server.start().await.unwrap();

                let adapter = $adapter_name::new(&ws_url, "BTCUSDT");
                assert!(adapter.test_orderbook_parsing().await.is_ok());
            }

            #[tokio::test]
            async fn reconnection_test() {
                let mut mock_server = MockWebSocketServer::new();
                mock_server.schedule_disconnect_at(Duration::from_secs(5));
                let ws_url = mock_server.start().await.unwrap();

                let adapter = $adapter_name::new(&ws_url, "BTCUSDT");
                assert!(adapter.test_reconnection().await.is_ok());
            }
        }
    };
}
```

### 3.2 Adapter 實現範例

```rust
/// execution-gateway/adapters/adapter-bitget/tests/integration.rs

use crate::adapter_test_framework::*;

adapter_test_suite!(BitgetAdapter);

// 額外的 Bitget 特定測試
#[tokio::test]
async fn bitget_order_placement_test() {
    // Bitget 特定的訂單下單測試
}
```

---

## 4. 實施優先級

### Phase 1: 關鍵路徑測試 (Week 1-2)
1. ✅ Strategy → Risk → Execution 集成測試
2. ✅ Paper Trading E2E 測試
3. ✅ Adapter 測試框架

### Phase 2: 穩定性測試 (Week 3)
4. ✅ Replay 框架 + 數據回放測試
5. ✅ 連接中斷與恢復測試
6. ✅ 風控熔斷測試

### Phase 3: 完整性測試 (Week 4)
7. ✅ 所有 Adapter 測試覆蓋
8. ✅ 長時間穩定性測試
9. ✅ 可觀測性集成測試

---

## 5. 測試指標

### 5.1 覆蓋率目標
- **Unit Test**: >= 80%
- **Integration Test**: >= 60%
- **E2E Test**: >= 5 個關鍵場景

### 5.2 性能目標
- **測試執行時間**: 全套 < 10 分鐘
- **CI/CD Pipeline**: < 15 分鐘
- **E2E 測試**: 單個 < 3 分鐘

### 5.3 穩定性目標
- **測試穩定性**: >= 99%（無 flaky tests）
- **Mock Server 可靠性**: >= 99.9%
- **Replay 數據完整性**: >= 99.99%
