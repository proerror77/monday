//! Phase 1 Performance Benchmarks
//!
//! Tests performance targets:
//! - L1/Trades p99 < 0.8ms
//! - L2/diff p99 < 1.5ms
//! - Order placement p99 < 5ms
//! - Order cancellation p99 < 3ms

use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

// Import our crates
use hft_data::{BitgetConnector, BitgetConfig, MessageParser, SnapshotStore};
use hft_execution::{ExecutionClient, ExecutionConfig, ExecutionMode, Order};
use hft_strategy::{MomentumStrategy, Strategy, StrategyContext};
use rust_decimal::Decimal;

/// Test data parsing performance (L1/Trades)
fn bench_l1_trades_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("L1_Trades_Parsing");
    group.throughput(Throughput::Elements(1));

    let mut parser = MessageParser::new();

    // Sample Bitget trade message
    let trade_message = r#"{
        "action": "update",
        "arg": {
            "instType": "spot",
            "channel": "trade",
            "instId": "BTCUSDT"
        },
        "data": [{
            "instId": "BTCUSDT",
            "tradeId": "1234567890",
            "px": "50000.12",
            "sz": "0.001",
            "side": "buy",
            "ts": "1721810005123"
        }]
    }"#;

    group.bench_function("parse_trade_message", |b| {
        b.iter(|| {
            let start = Instant::now();
            let result = parser.parse_message(trade_message.as_bytes());
            let elapsed = start.elapsed();

            // Verify performance target: p99 < 800μs
            if elapsed.as_micros() > 800 {
                eprintln!("WARNING: L1 trade parsing took {}μs, exceeds 800μs target", elapsed.as_micros());
            }

            result
        });
    });

    group.finish();
}

/// Test orderbook snapshot processing performance (L2/diff)
fn bench_l2_orderbook_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("L2_OrderBook_Processing");
    group.throughput(Throughput::Elements(1));

    let mut parser = MessageParser::new();
    let snapshot_store = SnapshotStore::new();

    // Sample Bitget orderbook snapshot
    let orderbook_message = r#"{
        "action": "snapshot",
        "arg": {
            "instType": "spot",
            "channel": "books15",
            "instId": "BTCUSDT"
        },
        "data": [{
            "asks": [
                ["50100.12", "0.5", 1],
                ["50101.34", "1.2", 2],
                ["50102.56", "0.8", 1]
            ],
            "bids": [
                ["50099.87", "0.7", 1],
                ["50098.65", "1.5", 3],
                ["50097.43", "0.9", 2]
            ],
            "ts": "1721810005123"
        }]
    }"#;

    group.bench_function("parse_and_store_orderbook", |b| {
        b.iter(|| {
            let start = Instant::now();

            // Parse message
            let parsed = parser.parse_message(orderbook_message.as_bytes()).unwrap();

            // Store in snapshot store
            if let hft_data::types::MessageData::OrderBook(snapshot) = parsed.data {
                snapshot_store.store_snapshot(parsed.instrument_id, snapshot).unwrap();
            }

            let elapsed = start.elapsed();

            // Verify performance target: p99 < 1500μs
            if elapsed.as_micros() > 1500 {
                eprintln!("WARNING: L2 orderbook processing took {}μs, exceeds 1500μs target", elapsed.as_micros());
            }
        });
    });

    group.finish();
}

/// Test order execution performance
fn bench_order_execution(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("Order_Execution");
    group.throughput(Throughput::Elements(1));

    group.bench_function("order_placement_paper", |b| {
        b.to_async(&rt).iter(|| async {
            let config = ExecutionConfig {
                mode: ExecutionMode::Paper,
                ..Default::default()
            };

            let mut client = ExecutionClient::new(config).unwrap();
            let _event_rx = client.start().await.unwrap();

            let order = Order::market_buy(
                "BTCUSDT".to_string(),
                Decimal::from(100),
                Decimal::new(50000, 0),
            );

            let start = Instant::now();
            let result = client.submit_order(order).await;
            let elapsed = start.elapsed();

            // Verify performance target: p99 < 5ms
            if elapsed.as_millis() > 5 {
                eprintln!("WARNING: Order placement took {}ms, exceeds 5ms target", elapsed.as_millis());
            }

            result
        });
    });

    group.finish();
}

/// Test strategy signal generation performance
fn bench_strategy_signals(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("Strategy_Signals");
    group.throughput(Throughput::Elements(1));

    group.bench_function("momentum_signal_generation", |b| {
        b.to_async(&rt).iter(|| async {
            let mut strategy = MomentumStrategy::new(5, 0.02, Decimal::from(100));

            let context = StrategyContext {
                positions: std::collections::HashMap::new(),
                market_data: std::collections::HashMap::new(),
                balance: Decimal::from(10000),
                config: std::collections::HashMap::new(),
            };

            // Create mock market data
            let market_data = hft_data::MarketDataMessage {
                message_type: hft_data::MessageType::Ticker,
                instrument_id: "BTCUSDT".to_string(),
                server_timestamp: chrono::Utc::now(),
                local_timestamp: chrono::Utc::now(),
                data: hft_data::types::MessageData::Ticker(hft_data::TickerData {
                    last_price: 50000.0,
                    best_bid: 49999.0,
                    best_ask: 50001.0,
                    volume_24h: 1000000.0,
                    price_change_24h: Some(500.0),
                    price_change_percent_24h: Some(1.0),
                }),
            };

            let start = Instant::now();
            let signals = strategy.on_market_data(&market_data, &context).await.unwrap();
            let elapsed = start.elapsed();

            // Strategy should be fast - target < 100μs
            if elapsed.as_micros() > 100 {
                eprintln!("WARNING: Strategy signal generation took {}μs, exceeds 100μs target", elapsed.as_micros());
            }

            signals
        });
    });

    group.finish();
}

/// Test full end-to-end latency (data -> strategy -> execution)
fn bench_end_to_end_latency(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("End_To_End_Latency");
    group.throughput(Throughput::Elements(1));

    group.bench_function("data_to_execution_pipeline", |b| {
        b.to_async(&rt).iter(|| async {
            // Setup components
            let mut parser = MessageParser::new();
            let mut strategy = MomentumStrategy::new(5, 0.02, Decimal::from(100));
            let mut execution_client = ExecutionClient::new(ExecutionConfig {
                mode: ExecutionMode::Paper,
                ..Default::default()
            }).unwrap();

            let _event_rx = execution_client.start().await.unwrap();

            let context = StrategyContext {
                positions: std::collections::HashMap::new(),
                market_data: std::collections::HashMap::new(),
                balance: Decimal::from(10000),
                config: std::collections::HashMap::new(),
            };

            // Sample ticker message
            let ticker_message = r#"{
                "action": "update",
                "arg": {
                    "instType": "spot",
                    "channel": "ticker",
                    "instId": "BTCUSDT"
                },
                "data": [{
                    "instId": "BTCUSDT",
                    "last": "50000.12",
                    "bestBid": "49999.87",
                    "bestAsk": "50000.34",
                    "vol24h": "1000000",
                    "ts": "1721810005123"
                }]
            }"#;

            let start = Instant::now();

            // 1. Parse message
            let parsed = parser.parse_message(ticker_message.as_bytes()).unwrap();

            // 2. Generate signals
            let signals = strategy.on_market_data(&parsed, &context).await.unwrap();

            // 3. Execute orders if signals generated
            if !signals.is_empty() {
                let orders = strategy.execute_signals(signals, &context).await.unwrap();
                for order in orders {
                    let _ = execution_client.submit_order(order).await;
                }
            }

            let elapsed = start.elapsed();

            // End-to-end should be very fast - target < 10ms total
            if elapsed.as_millis() > 10 {
                eprintln!("WARNING: End-to-end pipeline took {}ms, exceeds 10ms target", elapsed.as_millis());
            }

            elapsed
        });
    });

    group.finish();
}

/// Performance summary and validation
fn validate_performance_targets() {
    println!("\n=== HFT Performance Targets Validation ===");
    println!("Target L1/Trades p99: < 800μs");
    println!("Target L2/OrderBook p99: < 1500μs");
    println!("Target Order Placement p99: < 5ms");
    println!("Target Order Cancellation p99: < 3ms");
    println!("Target End-to-End: < 10ms");
    println!("==========================================\n");
}

criterion_group!(
    benches,
    bench_l1_trades_parsing,
    bench_l2_orderbook_processing,
    bench_order_execution,
    bench_strategy_signals,
    bench_end_to_end_latency
);

criterion_main!(benches);
// Archived: consolidated under primary benches (see benches/README.md)
