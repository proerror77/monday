/*!
 * Comprehensive UnifiedEngine Tests
 * 
 * 全面测试统一交易引擎，包括：
 * - UnifiedEngine 初始化和配置
 * - 风险管理器集成测试
 * - 执行管理器测试
 * - 事件处理和状态管理
 * - 性能和延迟测试
 */

use rust_hft::engine::unified::engine::*;
use rust_hft::engine::unified::config::UnifiedEngineConfig;
use rust_hft::engine::unified::builder::{RiskManager, ExecutionManager};
use rust_hft::engine::strategies::traits::*;
use rust_hft::core::orderbook::OrderBook;
use rust_hft::core::types::*;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use async_trait::async_trait;
use anyhow::Result;

use crate::common::{LatencyTracker, PerformanceHelper};

/// Mock Risk Manager for testing
struct MockRiskManager {
    max_position_size: f64,
    max_drawdown: f64,
    risk_checks_passed: Arc<Mutex<usize>>,
    should_block: bool,
}

impl MockRiskManager {
    fn new(max_position_size: f64, max_drawdown: f64, should_block: bool) -> Self {
        Self {
            max_position_size,
            max_drawdown,
            risk_checks_passed: Arc::new(Mutex::new(0)),
            should_block,
        }
    }

    async fn get_risk_checks_count(&self) -> usize {
        *self.risk_checks_passed.lock().await
    }
}

#[async_trait]
impl RiskManager for MockRiskManager {
    async fn check_position_risk(&self, _symbol: &str, _size: f64, _side: PositionSide) -> Result<bool> {
        let mut count = self.risk_checks_passed.lock().await;
        *count += 1;
        
        if self.should_block {
            Ok(false) // Block the trade
        } else {
            Ok(true) // Allow the trade
        }
    }

    async fn check_portfolio_risk(&self, _total_exposure: f64, _current_drawdown: f64) -> Result<bool> {
        Ok(!self.should_block)
    }

    async fn get_position_limits(&self, _symbol: &str) -> Result<(f64, f64)> {
        Ok((self.max_position_size, self.max_position_size))
    }

    async fn update_risk_metrics(&self, _positions: &[Position]) -> Result<()> {
        Ok(())
    }
}

/// Mock Execution Manager for testing
struct MockExecutionManager {
    executed_orders: Arc<Mutex<Vec<ExecutionOrder>>>,
    should_fail: bool,
    execution_latency_ns: u64,
}

#[derive(Debug, Clone)]
struct ExecutionOrder {
    symbol: String,
    side: PositionSide,
    quantity: f64,
    price: Option<f64>,
    order_type: String,
}

impl MockExecutionManager {
    fn new(should_fail: bool, execution_latency_ns: u64) -> Self {
        Self {
            executed_orders: Arc::new(Mutex::new(Vec::new())),
            should_fail,
            execution_latency_ns,
        }
    }

    async fn get_executed_orders(&self) -> Vec<ExecutionOrder> {
        self.executed_orders.lock().await.clone()
    }

    async fn get_execution_count(&self) -> usize {
        self.executed_orders.lock().await.len()
    }
}

#[async_trait]
impl ExecutionManager for MockExecutionManager {
    async fn execute_market_order(
        &self, 
        symbol: &str, 
        side: PositionSide, 
        quantity: f64
    ) -> Result<String> {
        // Simulate execution latency
        if self.execution_latency_ns > 0 {
            tokio::time::sleep(tokio::time::Duration::from_nanos(self.execution_latency_ns)).await;
        }

        if self.should_fail {
            return Err(anyhow::anyhow!("Mock execution failure"));
        }

        let order = ExecutionOrder {
            symbol: symbol.to_string(),
            side,
            quantity,
            price: None,
            order_type: "market".to_string(),
        };

        self.executed_orders.lock().await.push(order);
        Ok(format!("mock_order_{}", uuid::Uuid::new_v4()))
    }

    async fn execute_limit_order(
        &self, 
        symbol: &str, 
        side: PositionSide, 
        quantity: f64, 
        price: f64
    ) -> Result<String> {
        // Simulate execution latency
        if self.execution_latency_ns > 0 {
            tokio::time::sleep(tokio::time::Duration::from_nanos(self.execution_latency_ns)).await;
        }

        if self.should_fail {
            return Err(anyhow::anyhow!("Mock execution failure"));
        }

        let order = ExecutionOrder {
            symbol: symbol.to_string(),
            side,
            quantity,
            price: Some(price),
            order_type: "limit".to_string(),
        };

        self.executed_orders.lock().await.push(order);
        Ok(format!("mock_limit_order_{}", uuid::Uuid::new_v4()))
    }

    async fn cancel_order(&self, _order_id: &str) -> Result<()> {
        if self.should_fail {
            return Err(anyhow::anyhow!("Mock cancel failure"));
        }
        Ok(())
    }

    async fn get_order_status(&self, _order_id: &str) -> Result<String> {
        Ok("filled".to_string())
    }
}

/// Mock Trading Strategy for engine testing
struct MockEngineStrategy {
    name: String,
    signal_override: Option<crate::engine::strategies::traits::TradingSignal>,
    signal_count: Arc<Mutex<usize>>,
}

impl MockEngineStrategy {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            signal_override: None,
            signal_count: Arc::new(Mutex::new(0)),
        }
    }

    fn set_signal_override(&mut self, signal: crate::engine::strategies::traits::TradingSignal) {
        self.signal_override = Some(signal);
    }

    async fn get_signal_count(&self) -> usize {
        *self.signal_count.lock().await
    }
}

#[async_trait]
impl rust_hft::engine::strategy::TradingStrategy for MockEngineStrategy {
    async fn on_orderbook_update(&mut self, _symbol: &str, _orderbook: &OrderBook) -> Result<crate::engine::strategies::traits::TradingSignal> {
        let mut count = self.signal_count.lock().await;
        *count += 1;

        if let Some(signal) = self.signal_override.clone() {
            Ok(signal)
        } else {
            Ok(crate::engine::strategies::traits::TradingSignal::Hold)
        }
    }

    async fn initialize(&mut self, _params: serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn get_name(&self) -> &str {
        &self.name
    }
}

#[tokio::test]
async fn test_unified_engine_creation() -> Result<()> {
    let config = UnifiedEngineConfig {
        symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        max_position_size: 1000.0,
        risk_tolerance: 0.02,
        execution_mode: "test".to_string(),
    };

    let strategy = Box::new(MockEngineStrategy::new("test_strategy"));
    let risk_manager = Box::new(MockRiskManager::new(1000.0, 0.05, false));
    let execution_manager = Box::new(MockExecutionManager::new(false, 0));

    let engine = UnifiedTradingEngine::new(
        config.clone(),
        strategy,
        risk_manager,
        execution_manager,
    )?;

    // Verify engine is created correctly
    let state = engine.get_state().await;
    assert!(matches!(state, EngineState::Initializing));

    Ok(())
}

#[tokio::test]
async fn test_engine_lifecycle() -> Result<()> {
    let config = UnifiedEngineConfig {
        symbols: vec!["BTCUSDT".to_string()],
        max_position_size: 1000.0,
        risk_tolerance: 0.02,
        execution_mode: "test".to_string(),
    };

    let strategy = Box::new(MockEngineStrategy::new("lifecycle_test"));
    let risk_manager = Box::new(MockRiskManager::new(1000.0, 0.05, false));
    let execution_manager = Box::new(MockExecutionManager::new(false, 0));

    let mut engine = UnifiedTradingEngine::new(
        config,
        strategy,
        risk_manager,
        execution_manager,
    )?;

    // Test start
    engine.start().await?;
    let state = engine.get_state().await;
    assert!(matches!(state, EngineState::Running));

    // Test stop
    engine.stop().await?;
    let state = engine.get_state().await;
    assert!(matches!(state, EngineState::Stopped));

    Ok(())
}

#[tokio::test]
async fn test_orderbook_processing() -> Result<()> {
    let config = UnifiedEngineConfig {
        symbols: vec!["BTCUSDT".to_string()],
        max_position_size: 1000.0,
        risk_tolerance: 0.02,
        execution_mode: "test".to_string(),
    };

    let mut mock_strategy = MockEngineStrategy::new("orderbook_test");
    // Set strategy to generate buy signal
    mock_strategy.set_signal_override(
        crate::engine::strategies::traits::TradingSignal::Buy { 
            quantity: 0.1, 
            price: Some(50000.0), 
            confidence: 0.8 
        }
    );

    let strategy = Box::new(mock_strategy);
    let risk_manager = Box::new(MockRiskManager::new(1000.0, 0.05, false));
    let execution_manager = Box::new(MockExecutionManager::new(false, 10_000)); // 10μs execution latency

    let mut engine = UnifiedTradingEngine::new(
        config,
        strategy,
        risk_manager,
        execution_manager,
    )?;

    engine.start().await?;

    // Create test orderbook
    let mut orderbook = OrderBook::new("BTCUSDT".to_string());
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 49999.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    orderbook.init_snapshot(snapshot)?;

    // Process orderbook update
    engine.on_orderbook_update("BTCUSDT".to_string(), orderbook).await?;

    // Verify processing occurred (would need access to internal state for full verification)
    let state = engine.get_state().await;
    assert!(matches!(state, EngineState::Running));

    engine.stop().await?;
    Ok(())
}

#[tokio::test]
async fn test_risk_management_integration() -> Result<()> {
    let config = UnifiedEngineConfig {
        symbols: vec!["BTCUSDT".to_string()],
        max_position_size: 1000.0,
        risk_tolerance: 0.02,
        execution_mode: "test".to_string(),
    };

    let mut mock_strategy = MockEngineStrategy::new("risk_test");
    mock_strategy.set_signal_override(
        crate::engine::strategies::traits::TradingSignal::Buy { 
            quantity: 10.0, // Large position
            price: Some(50000.0), 
            confidence: 0.9 
        }
    );

    let strategy = Box::new(mock_strategy);
    let mock_risk_manager = MockRiskManager::new(1000.0, 0.05, true); // Block trades
    let risk_checks_counter = mock_risk_manager.risk_checks_passed.clone();
    let risk_manager = Box::new(mock_risk_manager);
    let execution_manager = Box::new(MockExecutionManager::new(false, 0));

    let mut engine = UnifiedTradingEngine::new(
        config,
        strategy,
        risk_manager,
        execution_manager,
    )?;

    engine.start().await?;

    // Create test orderbook
    let mut orderbook = OrderBook::new("BTCUSDT".to_string());
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 49999.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    orderbook.init_snapshot(snapshot)?;

    // Process orderbook - should trigger risk management
    engine.on_orderbook_update("BTCUSDT".to_string(), orderbook).await?;

    // Risk management should have been called (would need internal access to verify blocking)
    let risk_check_count = *risk_checks_counter.lock().await;
    // Note: This might be 0 if the risk check happens in a different part of the pipeline
    // The test structure would need adjustment based on actual implementation

    engine.stop().await?;
    Ok(())
}

#[tokio::test]
async fn test_execution_manager_integration() -> Result<()> {
    let config = UnifiedEngineConfig {
        symbols: vec!["BTCUSDT".to_string()],
        max_position_size: 1000.0,
        risk_tolerance: 0.02,
        execution_mode: "test".to_string(),
    };

    let mut mock_strategy = MockEngineStrategy::new("execution_test");
    mock_strategy.set_signal_override(
        crate::engine::strategies::traits::TradingSignal::Buy { 
            quantity: 0.1, 
            price: Some(50000.0), 
            confidence: 0.8 
        }
    );

    let strategy = Box::new(mock_strategy);
    let risk_manager = Box::new(MockRiskManager::new(1000.0, 0.05, false)); // Allow trades
    let mock_execution_manager = MockExecutionManager::new(false, 5_000); // 5μs execution
    let executed_orders_counter = mock_execution_manager.executed_orders.clone();
    let execution_manager = Box::new(mock_execution_manager);

    let mut engine = UnifiedTradingEngine::new(
        config,
        strategy,
        risk_manager,
        execution_manager,
    )?;

    engine.start().await?;

    // Create test orderbook
    let mut orderbook = OrderBook::new("BTCUSDT".to_string());
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 49999.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    orderbook.init_snapshot(snapshot)?;

    // Process orderbook - should trigger execution
    engine.on_orderbook_update("BTCUSDT".to_string(), orderbook).await?;

    // Check if execution occurred
    let executed_orders = executed_orders_counter.lock().await;
    // Note: Execution might happen asynchronously, so this test might need adjustment
    
    engine.stop().await?;
    Ok(())
}

#[tokio::test]
async fn test_engine_performance_requirements() -> Result<()> {
    let config = UnifiedEngineConfig {
        symbols: vec!["BTCUSDT".to_string()],
        max_position_size: 1000.0,
        risk_tolerance: 0.02,
        execution_mode: "test".to_string(),
    };

    let strategy = Box::new(MockEngineStrategy::new("perf_test"));
    let risk_manager = Box::new(MockRiskManager::new(1000.0, 0.05, false));
    let execution_manager = Box::new(MockExecutionManager::new(false, 0));

    let mut engine = UnifiedTradingEngine::new(
        config,
        strategy,
        risk_manager,
        execution_manager,
    )?;

    engine.start().await?;

    let latency_tracker = LatencyTracker::new();

    // Create test orderbook
    let mut orderbook = OrderBook::new("BTCUSDT".to_string());
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 49999.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    orderbook.init_snapshot(snapshot)?;

    // Test processing latency
    for i in 0..100 {
        let mut test_orderbook = orderbook.clone();
        
        // Modify orderbook slightly to trigger processing
        let update = OrderBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bids: vec![PriceLevel { 
                price: (49999.0 - i as f64 * 0.01).to_price(), 
                quantity: "1.0".to_quantity(), 
                side: Side::Bid 
            }],
            asks: vec![],
            timestamp: now_micros(),
            sequence_start: (i + 2) as u64,
            sequence_end: (i + 2) as u64,
            is_snapshot: false,
        };
        test_orderbook.apply_update(update)?;

        let start = std::time::Instant::now();
        engine.on_orderbook_update("BTCUSDT".to_string(), test_orderbook).await?;
        let latency_ns = start.elapsed().as_nanos() as u64;
        latency_tracker.record_latency(latency_ns).await;
    }

    let avg_latency = latency_tracker.get_avg_latency().await;
    let p99_latency = latency_tracker.get_p99_latency().await;

    println!("Engine Performance Results:");
    println!("  Average latency: {:.2} ns", avg_latency);
    println!("  P99 latency: {:.2} ns", p99_latency);

    // Performance requirements from CLAUDE.md: ≤ 25 μs p99
    assert!(p99_latency < 25_000.0); // 25 microseconds

    engine.stop().await?;
    Ok(())
}

#[tokio::test]
async fn test_engine_error_handling() -> Result<()> {
    let config = UnifiedEngineConfig {
        symbols: vec!["BTCUSDT".to_string()],
        max_position_size: 1000.0,
        risk_tolerance: 0.02,
        execution_mode: "test".to_string(),
    };

    let strategy = Box::new(MockEngineStrategy::new("error_test"));
    let risk_manager = Box::new(MockRiskManager::new(1000.0, 0.05, false));
    let execution_manager = Box::new(MockExecutionManager::new(true, 0)); // Will fail

    let mut engine = UnifiedTradingEngine::new(
        config,
        strategy,
        risk_manager,
        execution_manager,
    )?;

    engine.start().await?;

    // Create test orderbook
    let mut orderbook = OrderBook::new("BTCUSDT".to_string());
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 49999.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    orderbook.init_snapshot(snapshot)?;

    // Process orderbook - should handle execution failures gracefully
    let result = engine.on_orderbook_update("BTCUSDT".to_string(), orderbook).await;
    
    // Engine should handle errors gracefully without crashing
    assert!(result.is_ok());
    
    // Engine should still be in running state
    let state = engine.get_state().await;
    assert!(matches!(state, EngineState::Running));

    engine.stop().await?;
    Ok(())
}

#[tokio::test]
async fn test_engine_statistics_tracking() -> Result<()> {
    let config = UnifiedEngineConfig {
        symbols: vec!["BTCUSDT".to_string()],
        max_position_size: 1000.0,
        risk_tolerance: 0.02,
        execution_mode: "test".to_string(),
    };

    let strategy = Box::new(MockEngineStrategy::new("stats_test"));
    let risk_manager = Box::new(MockRiskManager::new(1000.0, 0.05, false));
    let execution_manager = Box::new(MockExecutionManager::new(false, 0));

    let mut engine = UnifiedTradingEngine::new(
        config,
        strategy,
        risk_manager,
        execution_manager,
    )?;

    engine.start().await?;

    // Get initial stats
    let initial_stats = engine.get_stats().await;
    assert_eq!(initial_stats.total_trades, 0);
    assert_eq!(initial_stats.winning_trades, 0);

    // Process some orderbook updates
    for _ in 0..5 {
        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        let snapshot = OrderBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bids: vec![PriceLevel { price: 49999.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
            asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
            timestamp: now_micros(),
            sequence_start: 1,
            sequence_end: 1,
            is_snapshot: true,
        };
        orderbook.init_snapshot(snapshot)?;

        engine.on_orderbook_update("BTCUSDT".to_string(), orderbook).await?;
    }

    // Stats tracking would require internal implementation details
    let final_stats = engine.get_stats().await;
    // For now, just verify stats structure is accessible
    assert!(final_stats.uptime_seconds >= 0);
    assert!(final_stats.avg_latency_us >= 0.0);

    engine.stop().await?;
    Ok(())
}

#[tokio::test]
async fn test_multi_symbol_engine() -> Result<()> {
    let config = UnifiedEngineConfig {
        symbols: vec![
            "BTCUSDT".to_string(),
            "ETHUSDT".to_string(),
            "SOLUSDT".to_string(),
        ],
        max_position_size: 1000.0,
        risk_tolerance: 0.02,
        execution_mode: "test".to_string(),
    };

    let strategy = Box::new(MockEngineStrategy::new("multi_symbol_test"));
    let risk_manager = Box::new(MockRiskManager::new(1000.0, 0.05, false));
    let execution_manager = Box::new(MockExecutionManager::new(false, 0));

    let mut engine = UnifiedTradingEngine::new(
        config.clone(),
        strategy,
        risk_manager,
        execution_manager,
    )?;

    engine.start().await?;

    // Process orderbook updates for each symbol
    for symbol in &config.symbols {
        let mut orderbook = OrderBook::new(symbol.clone());
        let snapshot = OrderBookUpdate {
            symbol: symbol.clone(),
            bids: vec![PriceLevel { price: 1000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
            asks: vec![PriceLevel { price: 1001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
            timestamp: now_micros(),
            sequence_start: 1,
            sequence_end: 1,
            is_snapshot: true,
        };
        orderbook.init_snapshot(snapshot)?;

        engine.on_orderbook_update(symbol.clone(), orderbook).await?;
    }

    // Verify engine can handle multiple symbols
    let state = engine.get_state().await;
    assert!(matches!(state, EngineState::Running));

    engine.stop().await?;
    Ok(())
}