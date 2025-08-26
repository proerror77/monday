/*!
 * Rust HFT - High-Frequency Trading System
 *
 * Pure Rust implementation targeting sub-100μs latency
 * Multi-threaded architecture with CPU affinity for maximum performance
 */

use anyhow::Result;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::RwLock;
use tracing::{debug, error, info};
use arc_swap::ArcSwap;

use rust_hft::core::monitoring::MonitoringServer;
use rust_hft::integrations::redis_bridge::RedisBridge;
// P0 修復：集成完整 OMS 和交易引擎
use rust_hft::engine::complete_oms::CompleteOMS;
use rust_hft::exchanges::{ExchangeManager, ExchangeInstance, ExchangeInstanceConfig, ExchangeSnapshot, HealthMetrics};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("rust_hft=debug,redis_bridge=debug")
        .init();

    info!("🚀 啟動 Rust HFT System");

    // 啟動監控服務 (Prometheus metrics)
    let monitoring = Arc::new(
        MonitoringServer::new(9090)
            .await
            .map_err(|e| anyhow::anyhow!("監控服務啟動失敗: {}", e))?,
    );

    // 更新組件健康狀態
    monitoring.update_health("system", true).await;
    info!("📊 監控服務已啟動在端口 9090");

    // 啟動Redis數據橋接服務
    let redis_bridge = RedisBridge::new().await?;
    info!("🔗 Redis橋接服務已初始化");

    // P0 修復：啟動完整交易系統
    info!("🏦 初始化交易所管理器");
    let exchange_manager = Arc::new(ExchangeManager::new());

    // 添加支持的交易所
    use rust_hft::exchanges::{BinanceExchange, BitgetExchange};

    info!("🔗 添加 Bitget 交易所");
    let bitget_exchange = Box::new(BitgetExchange::default());
    let bitget_instance = ExchangeInstance {
        config: ExchangeInstanceConfig {
            name: "bitget".to_string(),
            account_id: "main".to_string(),
            api_key: String::new(),
            secret_key: String::new(),
            passphrase: None,
            testnet: false,
            enabled: true,
            priority: 1,
        },
        exchange: Arc::new(RwLock::new(bitget_exchange)),
        health: arc_swap::ArcSwap::new(Arc::new(HealthMetrics::default())),
        last_selected: None,
    };

    info!("🔗 添加 Binance 交易所");
    let binance_exchange = Box::new(BinanceExchange::default());
    let binance_instance = ExchangeInstance {
        config: ExchangeInstanceConfig {
            name: "binance".to_string(),
            account_id: "main".to_string(),
            api_key: String::new(),
            secret_key: String::new(),
            passphrase: None,
            testnet: false,
            enabled: true,
            priority: 1,
        },
        exchange: Arc::new(RwLock::new(binance_exchange)),
        health: arc_swap::ArcSwap::new(Arc::new(HealthMetrics::default())),
        last_selected: None,
    };

    // 創建交易所映射
    let mut exchanges_map = HashMap::new();
    
    // 添加 bitget（暫時不指定具體的交易對）
    let bitget_symbols = HashMap::new();
    exchanges_map.insert("bitget".to_string(), Arc::new(ExchangeSnapshot::new(bitget_symbols)));
    
    // 添加 binance（暫時不指定具體的交易對）
    let binance_symbols = HashMap::new(); 
    exchanges_map.insert("binance".to_string(), Arc::new(ExchangeSnapshot::new(binance_symbols)));
    
    // 發布交易所
    exchange_manager.publish_exchanges(exchanges_map);

    info!(
        "✅ 交易所添加完成: {} 個交易所",
        exchange_manager.list_exchanges().await.len()
    );

    info!("📋 初始化完整 OMS 系統");
    let (oms, mut order_updates) = CompleteOMS::new_with_receiver(exchange_manager.clone());
    let oms = Arc::new(oms);
    oms.start()
        .await
        .map_err(|e| anyhow::anyhow!("OMS 啟動失敗: {}", e))?;

    // 啟動 OMS 事件處理
    let oms_clone = oms.clone();
    let oms_task = tokio::spawn(async move {
        info!("🎯 OMS 事件處理器已啟動");
        while let Some(order_update) = order_updates.recv().await {
            debug!("收到訂單更新: {:?}", order_update);
            // 可以在這裡添加額外的訂單更新處理邏輯
        }
    });

    info!("💹 完整交易系統已啟動");

    // 啟動真實市場數據橋接
    info!("🎯 啟動真實市場數據橋接模式");

    // 啟動真實數據流橋接
    let bridge_task = tokio::spawn(async move {
        if let Err(e) = redis_bridge.start_real_data_bridge().await {
            error!("真實數據流橋接失敗: {}", e);
        }
    });

    // 啟動健康檢查端點
    let monitoring_clone = monitoring.clone();
    let health_task = tokio::spawn(async move {
        monitoring_clone.start_health_endpoint().await;
    });

    info!("✅ 完整 HFT 交易系統已啟動:");
    info!("   • 完整 OMS: 訂單管理系統 ✅");
    info!("   • 交易所支持: Bitget, Binance ✅");
    info!("   • Redis橋接服務: 真實Bitget數據流 ✅");
    info!("   • Prometheus指標: http://localhost:9090/metrics");
    info!("   • 健康檢查: http://localhost:9091/health");
    info!("   • 系統狀態: http://localhost:9091/ready");
    info!("");
    info!("🚀 系統準備接收交易信號...");
    info!("📡 Redis channels:");
    info!("   • market.orderbook (真實訂單簿數據)");
    info!("   • market.ticker (真實價格數據)");
    info!("   • market.trade (真實交易數據)");
    info!("   • system.metrics (系統指標)");
    info!("");
    info!("🔴 警告: 使用真實交易所數據，請確保網絡連接穩定");
    info!("按 Ctrl+C 停止服務...");

    // 等待中斷信號
    tokio::select! {
        _ = signal::ctrl_c() => {
            info!("👋 接收到停止信號，正在關閉服務...");
        }
        result = bridge_task => {
            match result {
                Ok(_) => info!("Redis橋接任務正常結束"),
                Err(e) => error!("Redis橋接任務異常結束: {}", e),
            }
        }
        result = health_task => {
            match result {
                Ok(_) => info!("健康檢查任務正常結束"),
                Err(e) => error!("健康檢查任務異常結束: {}", e),
            }
        }
        result = oms_task => {
            match result {
                Ok(_) => info!("OMS 任務正常結束"),
                Err(e) => error!("OMS 任務異常結束: {}", e),
            }
        }
    }

    info!("🏁 Rust HFT System ready");
    Ok(())
}
