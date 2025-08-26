/*!
 * API 限流統計監控和告警機制演示
 * 
 * 展示增強版的 API 限流系統功能：
 * - 📊 實時統計信息和性能指標
 * - 🚨 智能告警系統和閾值檢測
 * - 📈 趨勢分析和容量規劃建議
 * - 🔍 詳細的性能報告和瓶頸分析
 * - 📋 API 密鑰和端點級別的統計
 */

use rust_hft::security::{
    ApiLimiter, RateLimitConfig, AlertType, AlertSeverity, AlertThresholds
};
use tokio::time::{sleep, Duration, Instant};
use tracing::{info, warn, error};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌系統
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("🚀 啟動 API 限流統計監控和告警機制演示");

    // 創建嚴格的限流配置（便於觸發告警）
    let mut config = RateLimitConfig::strict();
    config.per_api_key_requests_per_second = 5;  // 非常低的限制便於演示
    config.global_requests_per_second = 20;       // 全局限制

    let limiter = Arc::new(ApiLimiter::new(config));
    
    info!("📊 開始 API 限流監控演示...");

    // 演示1: 基本統計收集
    demo_basic_statistics(&limiter).await?;
    
    // 演示2: 告警系統測試
    demo_alert_system(&limiter).await?;
    
    // 演示3: 性能報告生成
    demo_performance_report(&limiter).await?;
    
    // 演示4: 趨勢分析
    demo_trend_analysis(&limiter).await?;
    
    // 演示5: 多 API 密鑰監控
    demo_multi_api_key_monitoring(&limiter).await?;

    info!("✅ API 限流統計監控和告警機制演示完成！");
    Ok(())
}

/// 演示1: 基本統計收集
async fn demo_basic_statistics(limiter: &Arc<ApiLimiter>) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n📊 === 演示1: 基本統計收集 ===");
    
    // 模擬不同 API 密鑰的請求
    let api_keys = vec!["trading_bot_1", "market_data_client", "risk_monitor"];
    let endpoints = vec!["/api/v2/spot/trade/place-order", "/api/v2/spot/market/orderbook", "/api/v2/spot/account/info"];
    
    info!("🔄 模擬 API 請求流量...");
    
    // 發送 50 個請求
    for i in 0..50 {
        let api_key = &api_keys[i % api_keys.len()];
        let endpoint = &endpoints[i % endpoints.len()];
        
        match limiter.check_request(api_key, endpoint).await {
            Ok(allowed) => {
                if allowed {
                    // 記錄成功的請求
                    limiter.record_request_result(api_key, true).await;
                } else {
                    info!("🚫 請求被熔斷器拒絕: {} -> {}", api_key, endpoint);
                }
            }
            Err(e) => {
                warn!("⚠️ 請求被限流: {} -> {} ({})", api_key, endpoint, e);
                // 記錄失敗的請求
                limiter.record_request_result(api_key, false).await;
            }
        }
        
        // 模擬請求間隔
        sleep(Duration::from_millis(50)).await;
    }
    
    // 獲取基本統計信息
    let stats = limiter.get_statistics().await;
    info!("📋 基本統計信息:");
    info!("   總請求數: {}", stats.total_requests);
    info!("   成功請求數: {}", stats.successful_requests);
    info!("   被限流請求數: {}", stats.limited_requests);
    info!("   當前請求率: {:.2} req/s", stats.current_requests_per_second);
    info!("   峰值請求率: {:.2} req/s", stats.peak_requests_per_second);
    info!("   限流命中率: {:.1}%", stats.limit_hit_rate * 100.0);
    
    // 顯示 API 密鑰級別統計
    info!("📊 API 密鑰統計:");
    for (api_key, api_stats) in &stats.api_key_stats {
        info!("   {}: {} 請求, {} 被限制, 令牌: {}/{}", 
              api_key, api_stats.total_requests, api_stats.limited_requests,
              api_stats.current_token_count, api_stats.bucket_capacity);
    }
    
    Ok(())
}

/// 演示2: 告警系統測試
async fn demo_alert_system(limiter: &Arc<ApiLimiter>) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n🚨 === 演示2: 告警系統測試 ===");
    
    info!("⚡ 模擬高頻請求流量以觸發告警...");
    
    // 快速發送大量請求以觸發限流
    for i in 0..30 {
        let api_key = "aggressive_trader";
        let endpoint = "/api/v2/spot/trade/place-order";
        
        let result = limiter.check_request(api_key, endpoint).await;
        match result {
            Ok(_) => limiter.record_request_result(api_key, true).await,
            Err(_) => limiter.record_request_result(api_key, false).await,
        }
        
        // 非常短的間隔
        sleep(Duration::from_millis(10)).await;
    }
    
    // 手動觸發告警檢查
    info!("🔍 執行告警檢查...");
    limiter.check_and_trigger_alerts().await;
    
    // 獲取告警歷史
    let stats = limiter.get_statistics().await;
    if !stats.alert_history.is_empty() {
        info!("📢 觸發的告警:");
        for alert in stats.alert_history.iter().take(5) {
            let severity_emoji = match alert.severity {
                AlertSeverity::Critical => "🔴",
                AlertSeverity::Error => "🟠", 
                AlertSeverity::Warning => "🟡",
                AlertSeverity::Info => "🟢",
            };
            info!("   {} {} - {}", severity_emoji, alert.alert_type.as_str(), alert.message);
        }
    } else {
        info!("ℹ️ 未觸發任何告警（可能需要更多請求）");
    }
    
    Ok(())
}

/// 演示3: 性能報告生成
async fn demo_performance_report(limiter: &Arc<ApiLimiter>) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n📈 === 演示3: 性能報告生成 ===");
    
    info!("📊 生成詳細性能報告...");
    let report = limiter.get_performance_report().await;
    
    info!("📋 性能報告摘要:");
    info!("   總請求數: {}", report.summary.total_requests);
    info!("   成功率: {:.1}%", report.summary.success_rate * 100.0);
    info!("   平均響應時間: {:.2} ms", report.summary.average_response_time_ms);
    info!("   峰值請求率: {:.2} req/s", report.summary.peak_requests_per_second);
    info!("   告警數量: {}", report.summary.alert_count);
    
    // 顯示頂級消費者
    if !report.top_consumers.is_empty() {
        info!("🏆 頂級 API 消費者:");
        for (i, consumer) in report.top_consumers.iter().take(3).enumerate() {
            info!("   {}. {}: {} 請求 ({} 被限制)", 
                  i + 1, consumer.api_key, consumer.total_requests, consumer.limited_requests);
        }
    }
    
    // 顯示瓶頸端點
    if !report.bottleneck_endpoints.is_empty() {
        info!("🔍 瓶頸端點:");
        for (i, endpoint) in report.bottleneck_endpoints.iter().take(3).enumerate() {
            info!("   {}. {}: {:.1}% 利用率", 
                  i + 1, endpoint.endpoint, endpoint.current_utilization * 100.0);
        }
    }
    
    // 趨勢分析
    info!("📊 趨勢分析:");
    info!("   請求率趨勢: {}", report.trend_analysis.request_rate_trend);
    info!("   限流率趨勢: {}", report.trend_analysis.limit_rate_trend);
    
    if !report.trend_analysis.recommendations.is_empty() {
        info!("💡 建議:");
        for recommendation in &report.trend_analysis.recommendations {
            info!("   • {}", recommendation);
        }
    }
    
    Ok(())
}

/// 演示4: 趨勢分析
async fn demo_trend_analysis(limiter: &Arc<ApiLimiter>) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n📈 === 演示4: 趨勢分析 ===");
    
    info!("⏰ 模擬時間序列數據收集...");
    
    // 模擬不同時間段的請求模式
    let patterns = [
        ("低流量期", 2, 200),   // 每200ms一個請求
        ("正常流量", 5, 100),   // 每100ms一個請求
        ("高峰期", 10, 50),     // 每50ms一個請求
        ("突發流量", 20, 25),   // 每25ms一個請求
    ];
    
    for (phase_name, request_count, interval_ms) in patterns {
        info!("📊 模擬 {} ({} 請求，間隔 {}ms)", phase_name, request_count, interval_ms);
        
        for i in 0..request_count {
            let api_key = format!("trader_{}", i % 3);
            let endpoint = "/api/v2/spot/market/ticker";
            
            let result = limiter.check_request(&api_key, endpoint).await;
            match result {
                Ok(_) => limiter.record_request_result(&api_key, true).await,
                Err(_) => limiter.record_request_result(&api_key, false).await,
            }
            
            sleep(Duration::from_millis(interval_ms)).await;
        }
        
        // 獲取當前統計
        let stats = limiter.get_statistics().await;
        info!("   當前請求率: {:.2} req/s, 限流率: {:.1}%", 
              stats.current_requests_per_second, stats.limit_hit_rate * 100.0);
    }
    
    info!("📊 生成趨勢分析報告...");
    let report = limiter.get_performance_report().await;
    
    info!("📈 趨勢分析結果:");
    info!("   請求率變化: {}", report.trend_analysis.request_rate_trend);
    info!("   限流率變化: {}", report.trend_analysis.limit_rate_trend);
    
    Ok(())
}

/// 演示5: 多 API 密鑰監控
async fn demo_multi_api_key_monitoring(limiter: &Arc<ApiLimiter>) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n👥 === 演示5: 多 API 密鑰監控 ===");
    
    // 定義不同類型的客戶端
    let clients = [
        ("algorithmic_trader_1", 15, "/api/v2/spot/trade/place-order"),
        ("market_maker_bot", 10, "/api/v2/spot/trade/cancel-order"), 
        ("data_analytics", 8, "/api/v2/spot/market/orderbook"),
        ("risk_monitor", 5, "/api/v2/spot/account/info"),
        ("backup_system", 3, "/api/v2/spot/market/ticker"),
    ];
    
    info!("🚀 模擬多客戶端並發請求...");
    
    // 並發執行不同客戶端的請求
    let mut handles = Vec::new();
    
    for (client_name, request_count, endpoint) in clients {
        let limiter_clone = limiter.clone();
        let client_name = client_name.to_string();
        let endpoint = endpoint.to_string();
        
        let handle = tokio::spawn(async move {
            let mut successful = 0;
            let mut limited = 0;
            
            for i in 0..request_count {
                let result = limiter_clone.check_request(&client_name, &endpoint).await;
                match result {
                    Ok(allowed) => {
                        if allowed {
                            successful += 1;
                            limiter_clone.record_request_result(&client_name, true).await;
                        }
                    }
                    Err(_) => {
                        limited += 1;
                        limiter_clone.record_request_result(&client_name, false).await;
                    }
                }
                
                // 隨機間隔以模擬真實流量
                let interval = 50 + (i * 10) % 100;
                sleep(Duration::from_millis(interval)).await;
            }
            
            (client_name, successful, limited)
        });
        
        handles.push(handle);
    }
    
    // 等待所有客戶端完成
    let mut results = Vec::new();
    for handle in handles {
        if let Ok(result) = handle.await {
            results.push(result);
        }
    }
    
    // 顯示結果
    info!("📊 多客戶端監控結果:");
    for (client_name, successful, limited) in results {
        let total = successful + limited;
        let success_rate = if total > 0 { successful as f64 / total as f64 * 100.0 } else { 0.0 };
        info!("   {}: {}/{} 成功 ({:.1}%)", client_name, successful, total, success_rate);
    }
    
    // 獲取最終統計
    let final_stats = limiter.get_statistics().await;
    info!("📈 最終系統統計:");
    info!("   總請求: {}", final_stats.total_requests);
    info!("   成功請求: {}", final_stats.successful_requests);
    info!("   限流請求: {}", final_stats.limited_requests);
    info!("   整體成功率: {:.1}%", 
          if final_stats.total_requests > 0 {
              final_stats.successful_requests as f64 / final_stats.total_requests as f64 * 100.0
          } else {
              0.0
          });
    
    // 健康檢查
    match limiter.health_check().await {
        Ok(health_status) => {
            info!("🏥 系統健康狀態: {}", health_status);
        }
        Err(e) => {
            error!("❌ 健康檢查失敗: {}", e);
        }
    }
    
    Ok(())
}