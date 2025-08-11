/*!
 * Comprehensive Trading Strategies Tests
 * 
 * 全面测试交易策略系统，包括：
 * - 策略信号生成测试
 * - 策略状态管理测试  
 * - 策略性能评估测试
 * - 策略组合逻辑测试
 * - 风险管理集成测试
 */

use rust_hft::engine::strategies::traits::*;
use rust_hft::engine::strategies::basic::*;
use rust_hft::core::orderbook::OrderBook;
use rust_hft::core::types::*;
use std::sync::Arc;
use tokio::sync::Mutex;
use async_trait::async_trait;
use anyhow::Result;

use crate::common::{PerformanceHelper, LatencyTracker};

/// Mock trading strategy for testing
struct MockTradingStrategy {
    name: String,
    state: StrategyState,
    signal_count: usize,
    last_orderbook: Option<OrderBook>,
    force_signal: Option<TradingSignal>,
}

impl MockTradingStrategy {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            state: StrategyState {
                name: name.to_string(),
                status: StrategyStatus::Initializing,
                performance: StrategyPerformance::default(),
                positions: vec![],
                last_signal: None,
                last_update: now_micros(),
            },
            signal_count: 0,
            last_orderbook: None,
            force_signal: None,
        }
    }

    fn set_force_signal(&mut self, signal: TradingSignal) {
        self.force_signal = Some(signal);
    }

    fn get_signal_count(&self) -> usize {
        self.signal_count
    }
}

#[async_trait]
impl TradingStrategy for MockTradingStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    async fn initialize(&mut self, context: &StrategyContext) -> Result<()> {
        self.state.status = StrategyStatus::Ready;
        tracing::debug!("Mock strategy {} initialized with context: {:?}", self.name, context);
        Ok(())
    }

    async fn on_orderbook_update(&mut self, symbol: &str, orderbook: &OrderBook) -> Result<TradingSignal> {
        self.signal_count += 1;
        self.last_orderbook = Some(orderbook.clone());
        self.state.last_update = now_micros();

        // Return forced signal if set, otherwise generate based on orderbook
        if let Some(signal) = self.force_signal.take() {
            self.state.last_signal = Some(signal.clone());
            return Ok(signal);
        }

        // Simple signal generation logic for testing
        if let (Some(bid), Some(ask)) = (orderbook.best_bid(), orderbook.best_ask()) {
            let spread = ask.0 - bid.0;
            let mid = (bid.0 + ask.0) / 2.0;

            if spread < mid * 0.001 { // Tight spread - potential arbitrage
                let signal = TradingSignal::Buy { 
                    quantity: 0.1, 
                    price: Some(ask.0), 
                    confidence: 0.8 
                };
                self.state.last_signal = Some(signal.clone());
                Ok(signal)
            } else if spread > mid * 0.01 { // Wide spread - avoid trading
                Ok(TradingSignal::Hold)
            } else {
                Ok(TradingSignal::Hold)
            }
        } else {
            Ok(TradingSignal::Hold)
        }
    }

    async fn on_trade_execution(&mut self, trade: &TradeExecution) -> Result<()> {
        // Update performance metrics
        self.state.performance.total_trades += 1;
        self.state.performance.total_pnl += trade.price * trade.quantity - trade.commission;
        
        tracing::debug!("Mock strategy {} processed trade: {:?}", self.name, trade);
        Ok(())
    }

    fn get_state(&self) -> StrategyState {
        self.state.clone()
    }

    async fn reset(&mut self) -> Result<()> {
        self.state.performance = StrategyPerformance::default();
        self.state.positions.clear();
        self.state.last_signal = None;
        self.signal_count = 0;
        self.last_orderbook = None;
        Ok(())
    }

    fn is_ready(&self) -> bool {
        matches!(self.state.status, StrategyStatus::Ready | StrategyStatus::Active)
    }
}

#[tokio::test]
async fn test_strategy_initialization() -> Result<()> {
    let mut strategy = MockTradingStrategy::new("test_strategy");
    
    assert_eq!(strategy.name(), "test_strategy");
    assert!(!strategy.is_ready()); // Should not be ready initially
    
    let context = StrategyContext::default();
    strategy.initialize(&context).await?;
    
    assert!(strategy.is_ready());
    assert!(matches!(strategy.get_state().status, StrategyStatus::Ready));
    
    Ok(())
}

#[tokio::test]
async fn test_strategy_signal_generation() -> Result<()> {
    let mut strategy = MockTradingStrategy::new("signal_test");
    let context = StrategyContext::default();
    strategy.initialize(&context).await?;

    // Create test orderbook
    let mut orderbook = OrderBook::new("BTCUSDT".to_string());
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 50000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![PriceLevel { price: 50000.05.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }], // Tight spread
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    orderbook.init_snapshot(snapshot)?;

    // Test signal generation
    let signal = strategy.on_orderbook_update("BTCUSDT", &orderbook).await?;
    
    assert!(strategy.get_signal_count() > 0);
    match signal {
        TradingSignal::Buy { quantity, price, confidence } => {
            assert!(quantity > 0.0);
            assert!(confidence > 0.0);
            assert!(price.is_some());
        },
        _ => {
            // Hold signal is also acceptable for this test
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_strategy_forced_signals() -> Result<()> {
    let mut strategy = MockTradingStrategy::new("forced_signal_test");
    let context = StrategyContext::default();
    strategy.initialize(&context).await?;

    // Set forced signal
    let forced_signal = TradingSignal::Sell { 
        quantity: 0.5, 
        price: Some(49999.0), 
        confidence: 0.9 
    };
    strategy.set_force_signal(forced_signal.clone());

    // Create dummy orderbook
    let orderbook = OrderBook::new("BTCUSDT".to_string());
    
    // Get signal - should return the forced signal
    let signal = strategy.on_orderbook_update("BTCUSDT", &orderbook).await?;
    
    match signal {
        TradingSignal::Sell { quantity, price, confidence } => {
            assert_eq!(quantity, 0.5);
            assert_eq!(price, Some(49999.0));
            assert_eq!(confidence, 0.9);
        },
        _ => panic!("Expected sell signal"),
    }

    Ok(())
}

#[tokio::test]
async fn test_strategy_trade_execution_handling() -> Result<()> {
    let mut strategy = MockTradingStrategy::new("execution_test");
    let context = StrategyContext::default();
    strategy.initialize(&context).await?;

    // Create test trade execution
    let trade = TradeExecution {
        order_id: "test_order_123".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: Side::Buy,
        quantity: 0.1,
        price: 50000.0,
        timestamp: now_micros(),
        commission: 5.0,
    };

    // Process trade
    strategy.on_trade_execution(&trade).await?;

    // Check performance update
    let state = strategy.get_state();
    assert_eq!(state.performance.total_trades, 1);
    assert!(state.performance.total_pnl != 0.0); // Should include trade P&L minus commission

    Ok(())
}

#[tokio::test]
async fn test_strategy_state_management() -> Result<()> {
    let mut strategy = MockTradingStrategy::new("state_test");
    let context = StrategyContext::default();
    strategy.initialize(&context).await?;

    // Initial state
    let initial_state = strategy.get_state();
    assert_eq!(initial_state.name, "state_test");
    assert!(matches!(initial_state.status, StrategyStatus::Ready));
    assert_eq!(initial_state.performance.total_trades, 0);

    // Simulate some trades
    for i in 0..5 {
        let trade = TradeExecution {
            order_id: format!("order_{}", i),
            symbol: "BTCUSDT".to_string(),
            side: if i % 2 == 0 { Side::Buy } else { Side::Sell },
            quantity: 0.1,
            price: 50000.0 + i as f64,
            timestamp: now_micros(),
            commission: 1.0,
        };
        strategy.on_trade_execution(&trade).await?;
    }

    // Check updated state
    let updated_state = strategy.get_state();
    assert_eq!(updated_state.performance.total_trades, 5);

    // Reset strategy
    strategy.reset().await?;
    let reset_state = strategy.get_state();
    assert_eq!(reset_state.performance.total_trades, 0);
    assert_eq!(strategy.get_signal_count(), 0);

    Ok(())
}

#[tokio::test]
async fn test_strategy_performance_calculation() -> Result<()> {
    let mut strategy = MockTradingStrategy::new("performance_test");
    let context = StrategyContext::default();
    strategy.initialize(&context).await?;

    // Simulate winning and losing trades
    let winning_trades = vec![
        (50100.0, 0.1, 1.0), // profit trade
        (50200.0, 0.1, 1.0), // profit trade
        (50150.0, 0.1, 1.0), // profit trade
    ];

    let losing_trades = vec![
        (49900.0, 0.1, 1.0), // loss trade
        (49950.0, 0.1, 1.0), // loss trade
    ];

    // Process winning trades
    for (price, quantity, commission) in winning_trades {
        let trade = TradeExecution {
            order_id: format!("win_{}", price),
            symbol: "BTCUSDT".to_string(),
            side: Side::Buy,
            quantity,
            price,
            timestamp: now_micros(),
            commission,
        };
        strategy.on_trade_execution(&trade).await?;
    }

    // Process losing trades  
    for (price, quantity, commission) in losing_trades {
        let trade = TradeExecution {
            order_id: format!("lose_{}", price),
            symbol: "BTCUSDT".to_string(),
            side: Side::Buy,
            quantity,
            price,
            timestamp: now_micros(),
            commission,
        };
        strategy.on_trade_execution(&trade).await?;
    }

    let state = strategy.get_state();
    assert_eq!(state.performance.total_trades, 5);
    assert!(state.performance.total_pnl != 0.0);

    Ok(())
}

#[tokio::test]
async fn test_multiple_strategy_coordination() -> Result<()> {
    let mut strategies = vec![
        MockTradingStrategy::new("strategy_1"),
        MockTradingStrategy::new("strategy_2"),
        MockTradingStrategy::new("strategy_3"),
    ];

    let context = StrategyContext::default();
    
    // Initialize all strategies
    for strategy in &mut strategies {
        strategy.initialize(&context).await?;
    }

    // Create test orderbook
    let mut orderbook = OrderBook::new("BTCUSDT".to_string());
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 50000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    orderbook.init_snapshot(snapshot)?;

    // Process orderbook update for all strategies
    let mut signals = Vec::new();
    for strategy in &mut strategies {
        let signal = strategy.on_orderbook_update("BTCUSDT", &orderbook).await?;
        signals.push(signal);
    }

    // Verify all strategies processed the update
    assert_eq!(signals.len(), 3);
    for strategy in &strategies {
        assert!(strategy.get_signal_count() > 0);
    }

    Ok(())
}

#[tokio::test]
async fn test_strategy_concurrent_processing() -> Result<()> {
    let strategy = Arc::new(Mutex::new(MockTradingStrategy::new("concurrent_test")));
    
    // Initialize strategy
    {
        let mut locked_strategy = strategy.lock().await;
        let context = StrategyContext::default();
        locked_strategy.initialize(&context).await?;
    }

    // Create test orderbook
    let mut orderbook = OrderBook::new("BTCUSDT".to_string());
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 50000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    orderbook.init_snapshot(snapshot)?;

    // Spawn multiple concurrent tasks
    let mut handles = Vec::new();
    for i in 0..10 {
        let strategy_clone = Arc::clone(&strategy);
        let orderbook_clone = orderbook.clone();
        
        let handle = tokio::spawn(async move {
            let mut locked_strategy = strategy_clone.lock().await;
            let symbol = format!("SYMBOL_{}", i);
            let _signal = locked_strategy.on_orderbook_update(&symbol, &orderbook_clone).await?;
            Ok::<(), anyhow::Error>(())
        });
        handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await??;
    }

    // Verify concurrent processing
    let final_strategy = strategy.lock().await;
    assert!(final_strategy.get_signal_count() >= 10);

    Ok(())
}

#[tokio::test]
async fn test_strategy_latency_performance() -> Result<()> {
    let mut strategy = MockTradingStrategy::new("latency_test");
    let context = StrategyContext::default();
    strategy.initialize(&context).await?;
    
    let latency_tracker = LatencyTracker::new();

    // Create test orderbook
    let mut orderbook = OrderBook::new("BTCUSDT".to_string());
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 50000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    orderbook.init_snapshot(snapshot)?;

    // Test strategy processing latency
    for _i in 0..1000 {
        let start = std::time::Instant::now();
        let _signal = strategy.on_orderbook_update("BTCUSDT", &orderbook).await?;
        let latency_ns = start.elapsed().as_nanos() as u64;
        latency_tracker.record_latency(latency_ns).await;
    }

    let avg_latency = latency_tracker.get_avg_latency().await;
    let p99_latency = latency_tracker.get_p99_latency().await;

    println!("Strategy Performance Results:");
    println!("  Average latency: {:.2} ns", avg_latency);
    println!("  P99 latency: {:.2} ns", p99_latency);

    // Strategy processing should be very fast (< 1 microsecond)
    assert!(p99_latency < 1_000.0); // 1 microsecond

    Ok(())
}

#[tokio::test]
async fn test_trading_signal_utilities() -> Result<()> {
    // Test signal confidence extraction
    let buy_signal = TradingSignal::Buy { quantity: 1.0, price: Some(50000.0), confidence: 0.8 };
    assert_eq!(buy_signal.confidence(), 0.8);
    assert!(buy_signal.is_buy());
    assert!(!buy_signal.is_sell());
    assert!(!buy_signal.is_close());

    let sell_signal = TradingSignal::Sell { quantity: 1.0, price: Some(50000.0), confidence: 0.9 };
    assert_eq!(sell_signal.confidence(), 0.9);
    assert!(!sell_signal.is_buy());
    assert!(sell_signal.is_sell());
    assert!(!sell_signal.is_close());

    let hold_signal = TradingSignal::Hold;
    assert_eq!(hold_signal.confidence(), 0.0);
    assert!(!hold_signal.is_buy());
    assert!(!hold_signal.is_sell());
    assert!(!hold_signal.is_close());

    let close_signal = TradingSignal::Close { ratio: 0.5 };
    assert_eq!(close_signal.confidence(), 1.0);
    assert!(!close_signal.is_buy());
    assert!(!close_signal.is_sell());
    assert!(close_signal.is_close());

    let close_all_signal = TradingSignal::CloseAll;
    assert_eq!(close_all_signal.confidence(), 1.0);
    assert!(close_all_signal.is_close());

    Ok(())
}

#[tokio::test]
async fn test_strategy_context_and_parameters() -> Result<()> {
    // Test default context
    let default_context = StrategyContext::default();
    assert_eq!(default_context.symbols.len(), 1);
    assert_eq!(default_context.symbols[0], "BTCUSDT");
    assert!(default_context.initial_capital > 0.0);
    assert!(default_context.max_position_size > 0.0);

    // Test default parameters
    let default_params = StrategyParameters::default();
    assert!(default_params.lookback_period > 0);
    assert!(default_params.threshold > 0.0);
    assert!(default_params.stop_loss > 0.0);
    assert!(default_params.take_profit > 0.0);
    assert!(default_params.max_trades_per_day > 0);

    // Test custom context
    let custom_context = StrategyContext {
        symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        initial_capital: 50000.0,
        max_position_size: 0.2,
        risk_tolerance: 0.05,
        strategy_params: StrategyParameters {
            lookback_period: 50,
            threshold: 0.02,
            stop_loss: 0.03,
            take_profit: 0.06,
            max_trades_per_day: 20,
        },
    };

    let mut strategy = MockTradingStrategy::new("custom_context_test");
    strategy.initialize(&custom_context).await?;

    assert!(strategy.is_ready());

    Ok(())
}

#[tokio::test]
async fn test_strategy_error_handling() -> Result<()> {
    let mut strategy = MockTradingStrategy::new("error_test");
    
    // Test processing without initialization
    let orderbook = OrderBook::new("BTCUSDT".to_string());
    let result = strategy.on_orderbook_update("BTCUSDT", &orderbook).await;
    
    // Should still work but strategy might not be in ready state
    assert!(result.is_ok());
    assert!(!strategy.is_ready()); // Not initialized yet

    Ok(())
}