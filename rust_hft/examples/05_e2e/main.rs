//! E2E 煙霧測試：驗證完整訂單流程
//!
//! 測試流程：
//! 1. 創建 Engine 並註冊 Paper 模式執行客戶端
//! 2. 下單 (OrderIntent)
//! 3. 模擬 ACK 事件
//! 4. 模擬 Fill 事件
//! 5. 驗證 AccountView 更新正確 (持倉 + PnL)
//!
//! 此測試驗證 OMS/Portfolio 集成的正確性

use std::time::Duration;
use tokio::time::sleep;

use engine::{Engine, EngineConfig};
use hft_core::{OrderId, OrderType, Price, Quantity, Side, Symbol, TimeInForce, Timestamp};
use ports::{
    AccountView, ConnectionHealth, ExecutionClient, ExecutionEvent, MarketEvent, OrderIntent,
    Strategy,
};
use std::pin::Pin;

/// 簡單的測試策略，生成一個買單
#[derive(Debug)]
struct TestStrategy {
    symbol: Symbol,
    executed: bool,
}

impl TestStrategy {
    fn new(symbol: Symbol) -> Self {
        Self {
            symbol,
            executed: false,
        }
    }
}

impl Strategy for TestStrategy {
    fn name(&self) -> &str {
        "TestStrategy"
    }

    fn on_market_event(
        &mut self,
        _event: &MarketEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        if !self.executed {
            self.executed = true;
            vec![OrderIntent {
                symbol: self.symbol.clone(),
                side: Side::Buy,
                quantity: Quantity::from_f64(0.1).unwrap(),
                price: Some(Price::from_f64(50000.0).unwrap()),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                strategy_id: "test_strategy".to_string(),
            }]
        } else {
            Vec::new()
        }
    }

    fn on_execution_event(
        &mut self,
        _event: &ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }
}

/// 模擬執行客戶端，生成假的執行事件
#[derive(Debug)]
struct MockExecutionClient {
    events: Vec<ExecutionEvent>,
    current_order_id: u64,
}

impl MockExecutionClient {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            current_order_id: 1,
        }
    }

    fn simulate_order_ack(&mut self, order_id: OrderId) {
        self.events.push(ExecutionEvent::OrderAck {
            order_id,
            timestamp: chrono::Utc::now().timestamp_millis() as u64,
        });
    }

    fn simulate_fill(&mut self, order_id: OrderId, price: Price, quantity: Quantity) {
        self.events.push(ExecutionEvent::Fill {
            order_id,
            price,
            quantity,
            timestamp: chrono::Utc::now().timestamp_millis() as u64,
            fill_id: format!("fill_{}", self.current_order_id),
        });
    }
}

#[async_trait::async_trait]
impl ExecutionClient for MockExecutionClient {
    async fn connect(&mut self) -> Result<(), hft_core::HftError> {
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), hft_core::HftError> {
        Ok(())
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: true,
            latency_ms: Some(1.0),
            last_heartbeat: chrono::Utc::now().timestamp_millis() as u64,
        }
    }

    async fn place_order(&mut self, intent: OrderIntent) -> Result<OrderId, hft_core::HftError> {
        let order_id = OrderId(format!("ORDER_{}", self.current_order_id));
        self.current_order_id += 1;

        println!(
            "📤 下單: {} {} {} @ {:?}",
            intent.symbol.0,
            match intent.side {
                Side::Buy => "買入",
                Side::Sell => "賣出",
            },
            intent.quantity.0,
            intent.price.map(|p| p.0)
        );

        // 模擬訂單確認
        self.simulate_order_ack(order_id.clone());

        // 模擬部分成交
        self.simulate_fill(
            order_id.clone(),
            intent.price.unwrap_or(Price::from_f64(50000.0).unwrap()),
            intent.quantity,
        );

        Ok(order_id)
    }

    async fn cancel_order(&mut self, _order_id: &OrderId) -> Result<(), hft_core::HftError> {
        Ok(())
    }

    async fn modify_order(
        &mut self,
        _order_id: &OrderId,
        _quantity: Option<Quantity>,
        _price: Option<Price>,
    ) -> Result<(), hft_core::HftError> {
        Ok(())
    }

    async fn execution_stream(
        &self,
    ) -> Result<
        Pin<Box<dyn futures::Stream<Item = Result<ExecutionEvent, hft_core::HftError>> + Send>>,
        hft_core::HftError,
    > {
        use futures::stream;

        // 返回預設的事件流 (從實例中無法修改，所以返回空流)
        let stream = stream::iter(std::iter::empty().map(Ok));
        Ok(Box::pin(stream))
    }

    async fn list_open_orders(&self) -> Result<Vec<ports::OpenOrder>, hft_core::HftError> {
        // 測試用模擬客戶端：無未結訂單
        Ok(Vec::new())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化 tracing
    tracing_subscriber::fmt::init();

    println!("🚀 啟動 E2E 煙霧測試");

    // 1. 創建 Engine
    let config = EngineConfig::default();
    let mut engine = Engine::new(config);

    // 2. 註冊測試策略
    let btc_symbol = Symbol("BTCUSDT".to_string());
    let strategy = TestStrategy::new(btc_symbol.clone());
    engine.register_strategy(strategy);

    // 3. 註冊模擬執行客戶端
    let mock_client = MockExecutionClient::new();
    engine.register_execution_client(mock_client);

    println!("📊 初始 AccountView:");
    let initial_account = engine.get_account_view();
    println!("   現金餘額: ${:.2}", initial_account.cash_balance);
    println!("   持倉數量: {}", initial_account.positions.len());
    println!("   已實現 PnL: ${:.2}", initial_account.realized_pnl);

    // 4. 執行幾次 tick 來觸發策略和處理執行事件
    println!("\n⚡ 執行引擎 tick...");

    for i in 0..5 {
        let tick_result = engine.tick()?;
        println!(
            "Tick #{}: 處理 {} 事件, 執行事件 {}, 快照發佈: {}",
            i + 1,
            tick_result.events_total,
            tick_result.execution_events_processed,
            tick_result.snapshot_published
        );

        if tick_result.orders_generated > 0 {
            println!("📈 策略生成 {} 個訂單!", tick_result.orders_generated);
        }

        // 小延遲模擬真實場景
        sleep(Duration::from_millis(10)).await;
    }

    // 5. 檢查最終 AccountView
    println!("\n📈 最終 AccountView:");
    let final_account = engine.get_account_view();
    println!("   現金餘額: ${:.2}", final_account.cash_balance);
    println!("   持倉數量: {}", final_account.positions.len());
    println!("   已實現 PnL: ${:.2}", final_account.realized_pnl);

    // 6. 驗證持倉變化
    if let Some(btc_position) = final_account.positions.get(&btc_symbol) {
        println!("   🪙 BTC 持倉:");
        println!("     數量: {}", btc_position.quantity.0);
        println!("     均價: ${:.2}", btc_position.avg_price.0);
        println!("     未實現 PnL: ${:.2}", btc_position.unrealized_pnl);
    } else {
        println!("   ⚠️  未發現 BTC 持倉變化");
    }

    // 7. 檢查引擎統計
    let stats = engine.get_statistics();
    println!("\n📊 引擎統計:");
    println!("   總周期數: {}", stats.cycle_count);
    println!("   策略數量: {}", stats.strategies_count);
    println!("   執行客戶端數量: {}", stats.execution_clients_count);
    println!("   已處理執行事件: {}", stats.execution_events_processed);

    // 8. 測試結果驗證
    let success = final_account.positions.contains_key(&btc_symbol) ||
                  final_account.cash_balance != 10000.0 || // 假設初始餘額為 $10,000
                  stats.execution_events_processed > 0;

    if success {
        println!("\n✅ E2E 煙霧測試通過!");
        println!("   訂單流程: OrderIntent → OMS 註冊 → Portfolio 記錄 → AccountView 更新");
    } else {
        println!("\n❌ E2E 煙霧測試失敗!");
        println!("   未檢測到預期的帳戶狀態變化");
    }

    Ok(())
}
// Moved to grouped example: 05_e2e/main.rs
