/*!
 * 无锁 OrderBook 演示程序
 * 
 * 演示如何使用新的无锁 OrderBook 实现，验证其基本功能
 */

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_hft::core::lockfree_orderbook::LockFreeOrderBook;
use rust_hft::core::types::{OrderBookUpdate, PriceLevel, Side};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 无锁 OrderBook 演示");
    println!("===================\n");

    // 创建无锁 OrderBook
    let orderbook = LockFreeOrderBook::new("BTCUSDT".to_string());
    println!("✅ 创建无锁 OrderBook: {}", orderbook.symbol);
    
    // 验证初始状态
    println!("📊 初始状态:");
    println!("   - 有效性: {}", orderbook.is_valid());
    println!("   - 更新计数: {}", orderbook.update_count());
    println!("   - 最佳买价: {:?}", orderbook.best_bid());
    println!("   - 最佳卖价: {:?}", orderbook.best_ask());
    println!();

    // 创建模拟的订单簿更新
    let update = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![
            PriceLevel {
                price: Decimal::from_f64_retain(50000.0).unwrap(),
                quantity: Decimal::from_f64_retain(1.5).unwrap(),
                side: Side::Bid,
            },
            PriceLevel {
                price: Decimal::from_f64_retain(49999.0).unwrap(),
                quantity: Decimal::from_f64_retain(2.0).unwrap(),
                side: Side::Bid,
            },
        ],
        asks: vec![
            PriceLevel {
                price: Decimal::from_f64_retain(50001.0).unwrap(),
                quantity: Decimal::from_f64_retain(1.2).unwrap(),
                side: Side::Ask,
            },
            PriceLevel {
                price: Decimal::from_f64_retain(50002.0).unwrap(),
                quantity: Decimal::from_f64_retain(0.8).unwrap(),
                side: Side::Ask,
            },
        ],
        timestamp: 1640995200000000, // 2022-01-01 00:00:00 UTC (微秒)
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };

    // 应用更新
    println!("📈 应用订单簿更新...");
    orderbook.init_snapshot(update)?;
    println!("✅ 更新成功应用");
    println!();

    // 验证更新后状态
    println!("📊 更新后状态:");
    println!("   - 有效性: {}", orderbook.is_valid());
    println!("   - 更新计数: {}", orderbook.update_count());
    
    if let Some(best_bid) = orderbook.best_bid() {
        println!("   - 最佳买价: ${:.2}", best_bid.to_f64().unwrap_or(0.0));
    }
    
    if let Some(best_ask) = orderbook.best_ask() {
        println!("   - 最佳卖价: ${:.2}", best_ask.to_f64().unwrap_or(0.0));
    }
    
    if let Some(mid) = orderbook.mid_price() {
        println!("   - 中间价: ${:.2}", mid.to_f64().unwrap_or(0.0));
    }
    
    if let Some(spread) = orderbook.spread() {
        println!("   - 价差: ${:.2}", spread);
    }
    
    if let Some(spread_bps) = orderbook.spread_bps() {
        println!("   - 价差 (基点): {:.2} bps", spread_bps);
    }
    println!();

    // 获取详细统计信息
    println!("📊 详细统计:");
    let stats = orderbook.get_stats();
    println!("   - 买盘档位: {}", stats.bid_levels);
    println!("   - 卖盘档位: {}", stats.ask_levels);
    println!("   - 数据质量分数: {:.2}", stats.data_quality_score);
    println!("   - 最后更新时间戳: {}", stats.last_update);
    println!();

    // 性能测试
    println!("🔥 性能测试 - 无锁读取:");
    let start = std::time::Instant::now();
    let iterations = 10000;
    
    for _ in 0..iterations {
        let _best_bid = orderbook.best_bid();
        let _best_ask = orderbook.best_ask();
        let _mid = orderbook.mid_price();
        let _spread = orderbook.spread();
    }
    
    let duration = start.elapsed();
    let ns_per_op = duration.as_nanos() / iterations as u128;
    println!("   - {} 次读取操作用时: {:?}", iterations, duration);
    println!("   - 每次操作平均耗时: {} ns", ns_per_op);
    println!("   - 每秒可执行操作数: {:.0}/s", 1_000_000_000.0 / ns_per_op as f64);
    println!();

    println!("✨ 无锁 OrderBook 演示完成！");
    println!("💡 无锁设计实现了微秒级别的极低延迟访问，非常适合 HFT 场景");
    
    Ok(())
}
// Archived legacy example
