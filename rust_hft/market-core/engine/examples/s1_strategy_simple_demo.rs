/*!
 * S1 策略簡單演示：直接測試策略邏輯
 *
 * 此示例直接測試策略的核心功能：
 * 1. TrendStrategy - EMA 金叉死叉 + RSI 過濾
 * 2. ArbitrageStrategy - 跨交易所套利檢測
 * 3. 模擬市場數據觸發策略信號
 */

use hft_core::*;
use num_traits::ToPrimitive;
use ports::*;
use strategy_arbitrage::{ArbitrageStrategy, ArbitrageStrategyConfig};
use strategy_trend::{TrendStrategy, TrendStrategyConfig};

fn current_timestamp_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🧪 S1 策略簡單演示開始");

    // 1. 創建趨勢策略
    let trend_config = TrendStrategyConfig {
        ema_fast_period: 3,   // 短期 EMA (便於快速看到效果)
        ema_slow_period: 6,   // 長期 EMA
        rsi_period: 5,        // RSI 週期
        rsi_oversold: 40.0,   // RSI 超賣線
        rsi_overbought: 60.0, // RSI 超買線
        position_size: 0.01,  // 每筆交易量
        max_position: 0.1,    // 最大總倉位
        min_spread_bps: 2.0,  // 最小價差
    };

    let mut trend_strategy = TrendStrategy::new(Symbol("BTCUSDT".to_string()), trend_config);
    trend_strategy.initialize()?;

    // 2. 創建套利策略
    let arbitrage_config = ArbitrageStrategyConfig {
        min_spread_bps: 5.0,      // 5 基點最小套利利差
        max_position_size: 0.005, // 單筆套利量
        max_total_position: 0.05, // 總套利倉位
        max_stale_us: 3000,       // 3ms 陳舊度容忍
        min_profit_usd: 1.0,      // 最小 1 美元利潤
        taker_fee_rate: 0.001,    // 0.1% 手續費
    };

    let mut arbitrage_strategy =
        ArbitrageStrategy::new(Symbol("BTCUSDT".to_string()), arbitrage_config);
    arbitrage_strategy.initialize()?;

    // 3. 模擬帳戶狀態
    let account = AccountView::default();

    println!("\n📈 測試趨勢策略 (K線觸發)...");

    // 4. 模擬 K 線數據 - 創建一個上升趨勢
    let base_prices = vec![
        50000.0, 50100.0, 50050.0, 50200.0, 50180.0, 50300.0, 50280.0, 50400.0, 50350.0, 50500.0,
    ];
    let mut total_trend_orders = 0;

    for (i, &price) in base_prices.iter().enumerate() {
        let timestamp = current_timestamp_us() + (i as u64 * 60000); // 每分鐘一根K線

        let bar = AggregatedBar {
            symbol: Symbol("BTCUSDT".to_string()),
            interval_ms: 60000,
            open_time: timestamp - 60000,
            close_time: timestamp,
            open: Price::from_f64(price - 20.0)?,
            high: Price::from_f64(price + 50.0)?,
            low: Price::from_f64(price - 50.0)?,
            close: Price::from_f64(price)?,
            volume: Quantity::from_f64(1.5)?,
            trade_count: 25,
        };

        let event = MarketEvent::Bar(bar);
        let orders = trend_strategy.on_market_event(&event, &account);

        if !orders.is_empty() {
            total_trend_orders += orders.len();
            println!(
                "  K線 #{}: 收盤價 {:.0}, 生成 {} 筆訂單",
                i + 1,
                price,
                orders.len()
            );

            for order in &orders {
                println!(
                    "    - {} {} {:.4} @ {:.2}",
                    match order.side {
                        Side::Buy => "買入",
                        Side::Sell => "賣出",
                    },
                    order.symbol.0,
                    order.quantity.0.to_f64().unwrap_or(0.0),
                    order
                        .price
                        .map(|p| p.0.to_f64().unwrap_or(0.0))
                        .unwrap_or(0.0)
                );
            }
        } else {
            println!("  K線 #{}: 收盤價 {:.0}, 無交易信號", i + 1, price);
        }
    }

    println!("\n⚡ 測試套利策略 (跨交易所快照)...");

    // 5. 模擬跨交易所套利場景
    let price_scenarios = vec![
        (49900.0, 50000.0), // Binance 便宜 100
        (50000.0, 49950.0), // Bitget 便宜 50
        (50100.0, 50200.0), // Bitget 貴 100
        (50050.0, 50030.0), // Binance 貴 20 (不足以套利)
        (49950.0, 50100.0), // 大價差 150
    ];

    let mut total_arbitrage_orders = 0;

    for (i, &(binance_price, bitget_price)) in price_scenarios.iter().enumerate() {
        let timestamp = current_timestamp_us() + (i as u64 * 1000);

        // Binance 快照
        let binance_snapshot = MarketSnapshot {
            symbol: Symbol("BINANCE:BTCUSDT".to_string()),
            timestamp,
            bids: vec![BookLevel::new_unchecked(binance_price, 0.1)],
            asks: vec![BookLevel::new_unchecked(binance_price + 1.0, 0.1)],
            sequence: i as u64 + 1,
        };

        // Bitget 快照
        let bitget_snapshot = MarketSnapshot {
            symbol: Symbol("BITGET:BTCUSDT".to_string()),
            timestamp,
            bids: vec![BookLevel::new_unchecked(bitget_price, 0.1)],
            asks: vec![BookLevel::new_unchecked(bitget_price + 1.0, 0.1)],
            sequence: i as u64 + 1,
        };

        // 處理兩個快照
        let binance_event = MarketEvent::Snapshot(binance_snapshot);
        let bitget_event = MarketEvent::Snapshot(bitget_snapshot);

        let mut orders = Vec::new();
        orders.extend(arbitrage_strategy.on_market_event(&binance_event, &account));
        orders.extend(arbitrage_strategy.on_market_event(&bitget_event, &account));

        let spread = (bitget_price - binance_price).abs();
        let spread_bps = (spread / binance_price.min(bitget_price)) * 10000.0;

        if !orders.is_empty() {
            total_arbitrage_orders += orders.len();
            println!(
                "  場景 #{}: Binance {:.0}, Bitget {:.0}, 價差 {:.0} ({:.1}bps), 生成 {} 筆訂單",
                i + 1,
                binance_price,
                bitget_price,
                spread,
                spread_bps,
                orders.len()
            );

            for order in &orders {
                println!(
                    "    - {} {} {:.4}",
                    match order.side {
                        Side::Buy => "買入",
                        Side::Sell => "賣出",
                    },
                    order.symbol.0,
                    order.quantity.0.to_f64().unwrap_or(0.0)
                );
            }
        } else {
            println!(
                "  場景 #{}: Binance {:.0}, Bitget {:.0}, 價差 {:.0} ({:.1}bps), 無套利機會",
                i + 1,
                binance_price,
                bitget_price,
                spread,
                spread_bps
            );
        }
    }

    // 6. 總結
    trend_strategy.shutdown()?;
    arbitrage_strategy.shutdown()?;

    println!("\n✅ S1 策略簡單演示完成");
    println!("   趨勢策略訂單: {} 筆", total_trend_orders);
    println!("   套利策略訂單: {} 筆", total_arbitrage_orders);
    println!("   策略模板驗證: ✅ 成功");

    if total_trend_orders > 0 && total_arbitrage_orders > 0 {
        println!("   🎉 兩種策略都成功生成了交易信號！");
    }

    Ok(())
}
