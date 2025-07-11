/*!
 * 統一數據收集系統示例
 * 
 * 展示如何使用新的Pipeline管理器和統一接口進行數據收集
 * 整合了原有的多個數據收集範例，提供統一的操作界面
 * 
 * 整合功能：
 * - YAML驅動工作流程
 * - 並行任務調度
 * - 智能資源分配
 * - 實時監控與告警
 */

use rust_hft::core::{
    Config, AdvancedPipelineManager, UnifiedInterfaceManager,
    UnifiedRequest, RequestType, RequestParameters, ClientInfo
};
use serde_json::json;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{sleep, Duration};
use tracing::{info, error};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt::init();
    
    info!("🚀 啟動統一數據收集系統示例");
    
    // 載入配置
    let config = Config::default();
    
    // 創建統一接口管理器
    let interface_manager = UnifiedInterfaceManager::new(config.clone()).await?;
    
    // 示例1：基本數據收集
    info!("📊 示例1：基本數據收集");
    basic_data_collection(&interface_manager).await?;
    
    sleep(Duration::from_secs(2)).await;
    
    // 示例2：高頻數據收集
    info!("⚡ 示例2：高頻數據收集");
    high_frequency_collection(&interface_manager).await?;
    
    sleep(Duration::from_secs(2)).await;
    
    // 示例3：多幣種並行收集
    info!("🔄 示例3：多幣種並行收集");
    multi_symbol_collection(&interface_manager).await?;
    
    sleep(Duration::from_secs(2)).await;
    
    // 示例4：智能數據質量監控
    info!("🎯 示例4：智能數據質量監控");
    quality_monitored_collection(&interface_manager).await?;
    
    sleep(Duration::from_secs(2)).await;
    
    // 示例5：系統性能監控
    info!("📈 示例5：系統性能監控");
    performance_monitoring(&interface_manager).await?;
    
    info!("✅ 統一數據收集系統示例完成");
    
    Ok(())
}

/// 基本數據收集示例
async fn basic_data_collection(manager: &UnifiedInterfaceManager) -> anyhow::Result<()> {
    let request = create_data_collection_request(
        "start_collection",
        "BTCUSDT",
        HashMap::from([
            ("duration_hours".to_string(), json!(1)),
            ("sample_rate_ms".to_string(), json!(100)),
            ("data_types".to_string(), json!(["orderbook", "trades", "ticker"])),
        ])
    );
    
    let response = manager.handle_request(request).await?;
    
    match response.status {
        rust_hft::core::ResponseStatus::Success => {
            if let Some(data) = response.data {
                info!("✅ 數據收集已啟動: {}", data.result);
                
                // 模擬監控收集進度
                monitor_collection_progress(manager, &data.resource_ids[0]).await?;
            }
        },
        _ => {
            error!("❌ 數據收集啟動失敗: {:?}", response.error);
        }
    }
    
    Ok(())
}

/// 高頻數據收集示例
async fn high_frequency_collection(manager: &UnifiedInterfaceManager) -> anyhow::Result<()> {
    let request = create_data_collection_request(
        "start_collection",
        "ETHUSDT",
        HashMap::from([
            ("duration_hours".to_string(), json!(2)),
            ("sample_rate_ms".to_string(), json!(10)), // 10ms高頻採樣
            ("data_types".to_string(), json!(["orderbook_l2"])),
            ("compression".to_string(), json!(true)),
            ("buffer_size".to_string(), json!(100000)),
        ])
    );
    
    let response = manager.handle_request(request).await?;
    
    if let rust_hft::core::ResponseStatus::Success = response.status {
        info!("⚡ 高頻數據收集已啟動，預計收集{}小時", 2);
        
        // 展示實時統計
        show_realtime_stats(manager).await?;
    }
    
    Ok(())
}

/// 多幣種並行收集示例
async fn multi_symbol_collection(manager: &UnifiedInterfaceManager) -> anyhow::Result<()> {
    let symbols = vec!["BTCUSDT", "ETHUSDT", "BNBUSDT", "ADAUSDT", "DOTUSDT"];
    let mut pipeline_ids = Vec::new();
    
    for symbol in symbols {
        let request = create_data_collection_request(
            "start_collection",
            symbol,
            HashMap::from([
                ("duration_hours".to_string(), json!(1)),
                ("sample_rate_ms".to_string(), json!(50)),
                ("data_types".to_string(), json!(["orderbook", "trades"])),
                ("parallel_mode".to_string(), json!(true)),
            ])
        );
        
        let response = manager.handle_request(request).await?;
        
        if let rust_hft::core::ResponseStatus::Success = response.status {
            if let Some(data) = response.data {
                pipeline_ids.push(data.resource_ids[0].clone());
                info!("🔄 {} 數據收集已啟動", symbol);
            }
        }
        
        // 避免請求過於頻繁
        sleep(Duration::from_millis(100)).await;
    }
    
    info!("📊 所有幣種並行收集已啟動，總計{}個Pipeline", pipeline_ids.len());
    
    // 監控所有Pipeline狀態
    monitor_multiple_pipelines(manager, &pipeline_ids).await?;
    
    Ok(())
}

/// 智能數據質量監控示例
async fn quality_monitored_collection(manager: &UnifiedInterfaceManager) -> anyhow::Result<()> {
    let request = create_data_collection_request(
        "start_collection",
        "SOLUSDT",
        HashMap::from([
            ("duration_hours".to_string(), json!(3)),
            ("sample_rate_ms".to_string(), json!(20)),
            ("quality_monitoring".to_string(), json!(true)),
            ("min_quality_score".to_string(), json!(0.95)),
            ("auto_quality_adjustment".to_string(), json!(true)),
            ("anomaly_detection".to_string(), json!(true)),
        ])
    );
    
    let response = manager.handle_request(request).await?;
    
    if let rust_hft::core::ResponseStatus::Success = response.status {
        info!("🎯 智能質量監控數據收集已啟動");
        
        // 模擬質量監控報告
        simulate_quality_monitoring().await?;
    }
    
    Ok(())
}

/// 系統性能監控示例
async fn performance_monitoring(manager: &UnifiedInterfaceManager) -> anyhow::Result<()> {
    let request = UnifiedRequest {
        request_id: Uuid::new_v4().to_string(),
        api_version: "v2.0".to_string(),
        request_type: RequestType::PerformanceMonitoring,
        parameters: RequestParameters {
            action: "get_system_metrics".to_string(),
            symbol: None,
            params: HashMap::from([
                ("include_detailed_stats".to_string(), json!(true)),
                ("time_window_minutes".to_string(), json!(10)),
            ]),
            config_overrides: None,
        },
        timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        client_info: create_client_info(),
        priority: 5,
        timeout_seconds: Some(30),
    };
    
    let response = manager.handle_request(request).await?;
    
    if let rust_hft::core::ResponseStatus::Success = response.status {
        if let Some(data) = response.data {
            info!("📈 系統性能指標:");
            info!("  - 總Pipeline數: {}", data.result["total_pipelines"]);
            info!("  - 總任務數: {}", data.result["total_tasks"]);
            info!("  - 活躍Pipeline: {}", data.result["active_pipelines"]);
            info!("  - 活躍任務: {}", data.result["active_tasks"]);
        }
    }
    
    // 展示請求統計
    let stats = manager.get_request_stats();
    info!("📊 請求統計:");
    info!("  - 總請求數: {}", stats.total_requests.load(std::sync::atomic::Ordering::Relaxed));
    info!("  - 成功請求: {}", stats.successful_requests.load(std::sync::atomic::Ordering::Relaxed));
    info!("  - 失敗請求: {}", stats.failed_requests.load(std::sync::atomic::Ordering::Relaxed));
    info!("  - 活躍請求: {}", stats.active_requests_count.load(std::sync::atomic::Ordering::Relaxed));
    
    Ok(())
}

/// 創建數據收集請求
fn create_data_collection_request(action: &str, symbol: &str, params: HashMap<String, serde_json::Value>) -> UnifiedRequest {
    UnifiedRequest {
        request_id: Uuid::new_v4().to_string(),
        api_version: "v2.0".to_string(),
        request_type: RequestType::DataCollection,
        parameters: RequestParameters {
            action: action.to_string(),
            symbol: Some(symbol.to_string()),
            params,
            config_overrides: None,
        },
        timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        client_info: create_client_info(),
        priority: 7,
        timeout_seconds: Some(300),
    }
}

/// 創建客戶端信息
fn create_client_info() -> ClientInfo {
    ClientInfo {
        client_id: "data_collection_example".to_string(),
        client_version: "2.0.0".to_string(),
        auth_token: Some("example_token_12345".to_string()),
        user_agent: "RustHFT-DataCollector/2.0".to_string(),
    }
}

/// 監控收集進度
async fn monitor_collection_progress(manager: &UnifiedInterfaceManager, pipeline_id: &str) -> anyhow::Result<()> {
    for i in 1..=5 {
        sleep(Duration::from_secs(1)).await;
        
        let status_request = UnifiedRequest {
            request_id: Uuid::new_v4().to_string(),
            api_version: "v2.0".to_string(),
            request_type: RequestType::StatusQuery,
            parameters: RequestParameters {
                action: "get_pipeline_status".to_string(),
                symbol: None,
                params: HashMap::from([
                    ("pipeline_id".to_string(), json!(pipeline_id)),
                ]),
                config_overrides: None,
            },
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            client_info: create_client_info(),
            priority: 3,
            timeout_seconds: Some(10),
        };
        
        let response = manager.handle_request(status_request).await?;
        
        if let rust_hft::core::ResponseStatus::Success = response.status {
            if let Some(data) = response.data {
                info!("📊 收集進度 {}/5: 狀態 = {:?}", i, data.result["status"]);
            }
        }
    }
    
    info!("✅ 數據收集監控完成");
    Ok(())
}

/// 展示實時統計
async fn show_realtime_stats(manager: &UnifiedInterfaceManager) -> anyhow::Result<()> {
    for i in 1..=3 {
        sleep(Duration::from_secs(1)).await;
        
        let stats = manager.get_request_stats();
        info!("⚡ 實時統計 {}/3:", i);
        info!("  - 處理請求: {}", stats.total_requests.load(std::sync::atomic::Ordering::Relaxed));
        info!("  - 成功率: {:.1}%", 
              if stats.total_requests.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                  stats.successful_requests.load(std::sync::atomic::Ordering::Relaxed) as f64 
                  / stats.total_requests.load(std::sync::atomic::Ordering::Relaxed) as f64 * 100.0
              } else { 0.0 });
    }
    
    Ok(())
}

/// 監控多個Pipeline
async fn monitor_multiple_pipelines(manager: &UnifiedInterfaceManager, pipeline_ids: &[String]) -> anyhow::Result<()> {
    for (i, pipeline_id) in pipeline_ids.iter().enumerate() {
        let status_request = UnifiedRequest {
            request_id: Uuid::new_v4().to_string(),
            api_version: "v2.0".to_string(),
            request_type: RequestType::StatusQuery,
            parameters: RequestParameters {
                action: "get_pipeline_status".to_string(),
                symbol: None,
                params: HashMap::from([
                    ("pipeline_id".to_string(), json!(pipeline_id)),
                ]),
                config_overrides: None,
            },
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            client_info: create_client_info(),
            priority: 3,
            timeout_seconds: Some(10),
        };
        
        let response = manager.handle_request(status_request).await?;
        
        if let rust_hft::core::ResponseStatus::Success = response.status {
            if let Some(data) = response.data {
                info!("🔄 Pipeline {}: 狀態 = {:?}", i + 1, data.result["status"]);
            }
        }
        
        sleep(Duration::from_millis(200)).await;
    }
    
    Ok(())
}

/// 模擬質量監控
async fn simulate_quality_monitoring() -> anyhow::Result<()> {
    let quality_metrics = vec![
        ("數據完整性", 99.8),
        ("延遲穩定性", 96.5),
        ("價格精度", 99.9),
        ("成交量準確性", 98.7),
        ("異常檢測", 99.1),
    ];
    
    for (metric, score) in quality_metrics {
        info!("🎯 {} = {:.1}%", metric, score);
        sleep(Duration::from_millis(300)).await;
    }
    
    info!("✅ 數據質量監控: 整體評分 98.8%");
    
    Ok(())
}