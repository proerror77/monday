//! 配置驗證示例：測試多交易所多符號配置
//!
//! 確保 Bitget + Binance 同時訂閱 ETHUSDT/SOLUSDT/SUIUSDT
//!
//! 運行方式:
//! ```bash
//! cargo run --example config_validation --features "redis,clickhouse"
//! ```

use std::collections::HashSet;
use runtime::system_builder::{SystemConfig, VenueType};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== HFT 系統配置驗證 ===\n");

    // 讀取配置文件
    let config_content = std::fs::read_to_string("../config/dev/system.yaml")?;
    println!("✓ 成功讀取配置文件: config/dev/system.yaml");

    // 解析配置
    let config: SystemConfig = serde_yaml::from_str(&config_content)?;
    println!("✓ 成功解析 YAML 配置");

    // 驗證交易所配置
    println!("\n--- 交易所配置驗證 ---");
    let mut venues = Vec::new();
    for venue in &config.venues {
        venues.push(&venue.venue_type);
        println!("✓ {} ({:?}): {}", venue.name, venue.venue_type,
                venue.ws_public.as_ref().unwrap_or(&"N/A".to_string()));
    }

    // 檢查必需的交易所
    let required_venues = vec![VenueType::Bitget, VenueType::Binance];
    let mut missing_venues = Vec::new();
    for required in &required_venues {
        if !venues.contains(&required) {
            missing_venues.push(format!("{:?}", required));
        }
    }

    if missing_venues.is_empty() {
        println!("✅ 所有必需的交易所已配置: {:?}", required_venues);
    } else {
        println!("❌ 缺少交易所: {:?}", missing_venues);
        return Err("交易所配置不完整".into());
    }

    // 驗證符號配置
    println!("\n--- 交易符號配置驗證 ---");
    let required_symbols = vec!["ETHUSDT", "SOLUSDT", "SUIUSDT"];
    let mut found_symbols = HashSet::new();

    for strategy in &config.strategies {
        println!("策略 '{}' ({:?}) 訂閱符號:", strategy.name, strategy.strategy_type);
        for symbol in &strategy.symbols {
            found_symbols.insert(symbol.as_str());
            println!("  - {}", symbol.as_str());
        }
    }

    // 檢查必需的符號
    let mut missing_symbols = Vec::new();
    for required in &required_symbols {
        if !found_symbols.contains(required) {
            missing_symbols.push(*required);
        }
    }

    if missing_symbols.is_empty() {
        println!("✅ 所有必需的符號已配置: {:?}", required_symbols);
    } else {
        println!("❌ 缺少符號: {:?}", missing_symbols);
        return Err("符號配置不完整".into());
    }

    // 驗證跨交易所套利策略
    println!("\n--- 跨交易所套利策略驗證 ---");
    let arbitrage_strategies: Vec<_> = config.strategies.iter()
        .filter(|s| matches!(s.strategy_type, runtime::system_builder::StrategyType::Arbitrage))
        .collect();

    if arbitrage_strategies.is_empty() {
        println!("⚠️  未發現套利策略");
    } else {
        for strategy in arbitrage_strategies {
            println!("✓ 套利策略 '{}' 配置符號: {:?}", strategy.name, strategy.symbols);
        }
    }

    // 驗證基礎設施配置
    println!("\n--- 基礎設施配置驗證 ---");
    if let Some(infra) = &config.infra {
        if let Some(redis) = &infra.redis {
            println!("✓ Redis 配置: {}", redis.url);
        } else {
            println!("⚠️  Redis 未配置");
        }

        if let Some(clickhouse) = &infra.clickhouse {
            println!("✓ ClickHouse 配置: {} (database: {})",
                    clickhouse.url,
                    clickhouse.database.as_ref().unwrap_or(&"default".to_string()));
        } else {
            println!("⚠️  ClickHouse 未配置");
        }
    } else {
        println!("⚠️  基礎設施配置未找到");
    }

    // 驗證風控配置
    println!("\n--- 風控配置驗證 ---");
    println!("✓ 全局持倉限制: {}", config.risk.global_position_limit);
    println!("✓ 全局名義價值限制: {}", config.risk.global_notional_limit);
    println!("✓ 最大日交易筆數: {}", config.risk.max_daily_trades);
    println!("✓ 最大每秒訂單數: {}", config.risk.max_orders_per_second);
    println!("✓ 數據過期閾值 (μs): {}", config.risk.staleness_threshold_us);

    println!("\n=== 配置驗證完成 ✅ ===");
    println!("系統已準備好同時連接 Bitget + Binance，訂閱 ETHUSDT/SOLUSDT/SUIUSDT");

    Ok(())
}
// Archived legacy example; see grouped examples
