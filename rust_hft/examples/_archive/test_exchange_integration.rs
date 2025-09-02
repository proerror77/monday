/*!
 * 多交易所 API 集成測試
 * 
 * 驗證 Bitget, Binance, Bybit 真實 API 連接和基本功能
 */

use anyhow::Result;
use rust_hft::exchanges::{
    BitgetExchange, BinanceExchange, BybitExchange,
    ConnectionStatus
};
use rust_hft::exchanges::exchange_trait::ExchangeTrait;
use rust_hft::exchanges::bitget::BitgetConfig;
use rust_hft::exchanges::binance::BinanceConfig;
use rust_hft::exchanges::bybit::BybitConfig;
use tracing::{info, warn, error};
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_env_filter("rust_hft=info,test_exchange_integration=info")
        .init();

    info!("🚀 開始多交易所 API 集成測試");

    // 測試結果統計
    let mut test_results = Vec::new();

    // 測試 Bitget 集成
    info!("\n=== 🟨 測試 Bitget Exchange ===");
    let bitget_result = test_bitget_integration().await;
    test_results.push(("Bitget", bitget_result.is_ok()));
    if let Err(e) = bitget_result {
        error!("Bitget 測試失敗: {}", e);
    }

    // 測試 Binance 集成
    info!("\n=== 🟡 測試 Binance Exchange ===");
    let binance_result = test_binance_integration().await;
    test_results.push(("Binance", binance_result.is_ok()));
    if let Err(e) = binance_result {
        error!("Binance 測試失敗: {}", e);
    }

    // 測試 Bybit 集成
    info!("\n=== 🟣 測試 Bybit Exchange ===");
    let bybit_result = test_bybit_integration().await;
    test_results.push(("Bybit", bybit_result.is_ok()));
    if let Err(e) = bybit_result {
        error!("Bybit 測試失敗: {}", e);
    }

    // 輸出測試總結
    print_test_summary(&test_results);

    Ok(())
}

/// 測試 Bitget 集成
async fn test_bitget_integration() -> Result<()> {
    let config = BitgetConfig::default();
    let mut exchange = BitgetExchange::new(config);

    // 測試連接
    info!("📡 測試 Bitget 連接...");
    let start_time = Instant::now();
    
    match exchange.connect().await {
        Ok(_) => {
            let latency = start_time.elapsed();
            info!("✅ Bitget 連接成功 - 延遲: {:?}", latency);
            
            // 驗證連接狀態
            let status = exchange.get_connection_status().await;
            match status {
                ConnectionStatus::Connected => {
                    info!("✅ Bitget 連接狀態正確");
                }
                _ => {
                    warn!("⚠️ Bitget 連接狀態異常: {:?}", status);
                }
            }
        }
        Err(e) => {
            error!("❌ Bitget 連接失敗: {}", e);
            return Err(e.into());
        }
    }

    // 測試服務器時間
    info!("🕒 測試 Bitget 服務器時間...");
    match exchange.get_server_time().await {
        Ok(server_time) => {
            let current_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as u64;
            let time_diff = (server_time as i64 - current_time as i64).abs();
            
            info!("✅ Bitget 服務器時間: {} (偏差: {}ms)", server_time, time_diff);
            
            if time_diff > 5000 {
                warn!("⚠️ Bitget 時間偏差過大: {}ms", time_diff);
            }
        }
        Err(e) => {
            error!("❌ 獲取 Bitget 服務器時間失敗: {}", e);
            return Err(e.into());
        }
    }

    // 測試訂單簿獲取
    info!("📊 測試 Bitget 訂單簿獲取...");
    match exchange.get_orderbook("BTCUSDT").await {
        Ok(orderbook) => {
            info!("✅ Bitget 訂單簿獲取成功");
            info!("   - 標的: {}", orderbook.symbol);
            info!("   - 買盤檔數: {}", orderbook.bids.len());
            info!("   - 賣盤檔數: {}", orderbook.asks.len());
            info!("   - 最佳買價: {:?}", orderbook.best_bid());
            info!("   - 最佳賣價: {:?}", orderbook.best_ask());
            info!("   - 價差: {:?}", orderbook.spread());
            info!("   - 有效性: {}", orderbook.is_valid);
        }
        Err(e) => {
            error!("❌ 獲取 Bitget 訂單簿失敗: {}", e);
            return Err(e.into());
        }
    }

    // 測試健康檢查
    info!("💊 測試 Bitget 健康檢查...");
    match exchange.health_check().await {
        Ok(health) => {
            info!("✅ Bitget 健康檢查成功");
            info!("   - 健康狀態: {}", health.is_healthy);
            info!("   - 延遲: {:.2}ms", health.latency_ms);
            info!("   - 錯誤計數: {}", health.error_count);
        }
        Err(e) => {
            error!("❌ Bitget 健康檢查失敗: {}", e);
            return Err(e.into());
        }
    }

    // 測試斷開連接
    info!("🔌 測試 Bitget 斷開連接...");
    match exchange.disconnect().await {
        Ok(_) => {
            info!("✅ Bitget 斷開連接成功");
        }
        Err(e) => {
            error!("❌ Bitget 斷開連接失敗: {}", e);
            return Err(e.into());
        }
    }

    Ok(())
}

/// 測試 Binance 集成
async fn test_binance_integration() -> Result<()> {
    let config = BinanceConfig::default();
    let mut exchange = BinanceExchange::new(config);

    info!("📡 測試 Binance 連接...");
    let start_time = Instant::now();
    
    match exchange.connect().await {
        Ok(_) => {
            let latency = start_time.elapsed();
            info!("✅ Binance 連接成功 - 延遲: {:?}", latency);
        }
        Err(e) => {
            error!("❌ Binance 連接失敗: {}", e);
            return Err(e.into());
        }
    }

    info!("🕒 測試 Binance 服務器時間...");
    match exchange.get_server_time().await {
        Ok(server_time) => {
            info!("✅ Binance 服務器時間: {}", server_time);
        }
        Err(e) => {
            error!("❌ 獲取 Binance 服務器時間失敗: {}", e);
            return Err(e.into());
        }
    }

    info!("📊 測試 Binance 訂單簿獲取...");
    match exchange.get_orderbook("BTCUSDT").await {
        Ok(orderbook) => {
            info!("✅ Binance 訂單簿獲取成功");
            info!("   - 買盤檔數: {}, 賣盤檔數: {}", orderbook.bids.len(), orderbook.asks.len());
        }
        Err(e) => {
            error!("❌ 獲取 Binance 訂單簿失敗: {}", e);
            return Err(e.into());
        }
    }

    info!("💊 測試 Binance 健康檢查...");
    match exchange.health_check().await {
        Ok(health) => {
            info!("✅ Binance 健康檢查成功 - 延遲: {:.2}ms", health.latency_ms);
        }
        Err(e) => {
            error!("❌ Binance 健康檢查失敗: {}", e);
            return Err(e.into());
        }
    }

    match exchange.disconnect().await {
        Ok(_) => info!("✅ Binance 斷開連接成功"),
        Err(e) => error!("❌ Binance 斷開連接失敗: {}", e),
    }

    Ok(())
}

/// 測試 Bybit 集成
async fn test_bybit_integration() -> Result<()> {
    let config = BybitConfig::default();
    let mut exchange = BybitExchange::new(config);

    info!("📡 測試 Bybit 連接...");
    let start_time = Instant::now();
    
    match exchange.connect().await {
        Ok(_) => {
            let latency = start_time.elapsed();
            info!("✅ Bybit 連接成功 - 延遲: {:?}", latency);
        }
        Err(e) => {
            error!("❌ Bybit 連接失敗: {}", e);
            return Err(e.into());
        }
    }

    info!("🕒 測試 Bybit 服務器時間...");
    match exchange.get_server_time().await {
        Ok(server_time) => {
            info!("✅ Bybit 服務器時間: {}", server_time);
        }
        Err(e) => {
            error!("❌ 獲取 Bybit 服務器時間失敗: {}", e);
            return Err(e.into());
        }
    }

    info!("📊 測試 Bybit 訂單簿獲取...");
    match exchange.get_orderbook("BTCUSDT").await {
        Ok(orderbook) => {
            info!("✅ Bybit 訂單簿獲取成功");
            info!("   - 買盤檔數: {}, 賣盤檔數: {}", orderbook.bids.len(), orderbook.asks.len());
        }
        Err(e) => {
            error!("❌ 獲取 Bybit 訂單簿失敗: {}", e);
            return Err(e.into());
        }
    }

    info!("💊 測試 Bybit 健康檢查...");
    match exchange.health_check().await {
        Ok(health) => {
            info!("✅ Bybit 健康檢查成功 - 延遲: {:.2}ms", health.latency_ms);
        }
        Err(e) => {
            error!("❌ Bybit 健康檢查失敗: {}", e);
            return Err(e.into());
        }
    }

    match exchange.disconnect().await {
        Ok(_) => info!("✅ Bybit 斷開連接成功"),
        Err(e) => error!("❌ Bybit 斷開連接失敗: {}", e),
    }

    Ok(())
}

/// 打印測試總結
fn print_test_summary(results: &[(&str, bool)]) {
    info!("\n🎯 ===== 多交易所 API 集成測試總結 =====");
    
    let total_tests = results.len();
    let passed_tests = results.iter().filter(|(_, passed)| *passed).count();
    let failed_tests = total_tests - passed_tests;

    for (exchange, passed) in results {
        let status = if *passed { "✅ 通過" } else { "❌ 失敗" };
        info!("   {} Exchange: {}", exchange, status);
    }

    info!("\n📊 測試統計:");
    info!("   • 總測試數: {}", total_tests);
    info!("   • 通過數: {}", passed_tests);
    info!("   • 失敗數: {}", failed_tests);
    info!("   • 成功率: {:.1}%", (passed_tests as f64 / total_tests as f64) * 100.0);

    if failed_tests == 0 {
        info!("\n🎉 所有交易所 API 集成測試通過！");
    } else {
        warn!("\n⚠️ 有 {} 個交易所測試失敗，請檢查網絡連接和 API 狀態", failed_tests);
    }

    info!("========================================\n");
}
// Archived legacy example; use 03_execution/
