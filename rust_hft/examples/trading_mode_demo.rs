/*!
 * 交易模式系統演示
 * 
 * 展示如何使用交易模式管理器進行安全的模式切換
 * 包括：數據模式、模擬模式、真實交易模式的切換流程
 */

use anyhow::Result;
use rust_hft::engine::{TradingMode, TradingModeManager};
use tracing::{info, warn, error};
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_env_filter("rust_hft=info,trading_mode_demo=info")
        .init();

    info!("🚀 交易模式系統演示開始");

    // 創建交易模式管理器
    let mode_manager = TradingModeManager::new();

    // 演示1: 查看初始狀態
    demonstrate_initial_state(&mode_manager).await?;

    // 演示2: 從數據模式切換到模擬模式
    demonstrate_data_to_dry_run(&mode_manager).await?;

    // 演示3: 在模擬模式下記錄交易
    demonstrate_trade_recording(&mode_manager).await?;

    // 演示4: 嘗試切換到真實交易模式
    demonstrate_dry_run_to_live(&mode_manager).await?;

    // 演示5: 緊急停止功能
    demonstrate_emergency_stop(&mode_manager).await?;

    // 演示6: 查看統計和歷史
    demonstrate_statistics_and_history(&mode_manager).await?;

    // 演示7: 併發切換保護
    demonstrate_concurrent_protection(&mode_manager).await?;

    info!("✅ 交易模式系統演示完成");
    Ok(())
}

/// 演示初始狀態
async fn demonstrate_initial_state(manager: &TradingModeManager) -> Result<()> {
    info!("\n=== 📊 演示1: 初始狀態檢查 ===");
    
    let current_mode = manager.current_mode();
    let state = manager.get_state();
    
    info!("當前模式: {}", current_mode);
    info!("允許真實交易: {}", manager.allows_real_trading());
    info!("允許模擬交易: {}", manager.allows_simulation());
    info!("是否正在切換: {}", manager.is_transitioning());
    info!("模式設置時間: {}", state.mode_set_at);
    
    assert_eq!(current_mode, TradingMode::DataOnly);
    assert!(!manager.allows_real_trading());
    assert!(!manager.allows_simulation());
    
    Ok(())
}

/// 演示從數據模式切換到模擬模式
async fn demonstrate_data_to_dry_run(manager: &TradingModeManager) -> Result<()> {
    info!("\n=== 🔄 演示2: 數據模式 -> 模擬模式 ===");
    
    let transition_result = manager
        .request_mode_transition(
            TradingMode::DryRun,
            Some("demo_user".to_string()),
            "演示模擬交易功能".to_string(),
        )
        .await;
    
    match transition_result {
        Ok(transition_id) => {
            info!("✅ 模式切換成功，切換ID: {}", transition_id);
            info!("新模式: {}", manager.current_mode());
            info!("允許模擬交易: {}", manager.allows_simulation());
            
            assert_eq!(manager.current_mode(), TradingMode::DryRun);
            assert!(manager.allows_simulation());
            assert!(!manager.allows_real_trading());
        }
        Err(error) => {
            error!("❌ 模式切換失敗: {}", error);
            return Err(anyhow::anyhow!("模式切換失敗: {}", error));
        }
    }
    
    Ok(())
}

/// 演示交易記錄功能
async fn demonstrate_trade_recording(manager: &TradingModeManager) -> Result<()> {
    info!("\n=== 📈 演示3: 模擬交易記錄 ===");
    
    // 模擬執行幾筆交易
    for i in 1..=5 {
        manager.record_trade_execution();
        info!("記錄模擬交易 #{}", i);
        sleep(Duration::from_millis(100)).await;
    }
    
    let stats = manager.get_statistics();
    info!("當前模式交易數: {}", stats.trades_in_current_mode);
    assert_eq!(stats.trades_in_current_mode, 5);
    
    Ok(())
}

/// 演示切換到真實交易模式
async fn demonstrate_dry_run_to_live(manager: &TradingModeManager) -> Result<()> {
    info!("\n=== ⚡ 演示4: 模擬模式 -> 真實交易模式 ===");
    
    info!("🔒 注意：這個切換需要管理員權限和更嚴格的檢查");
    
    let transition_result = manager
        .request_mode_transition(
            TradingMode::Live,
            Some("admin_user".to_string()),
            "啟動真實交易".to_string(),
        )
        .await;
    
    match transition_result {
        Ok(transition_id) => {
            info!("✅ 切換到真實交易模式成功，ID: {}", transition_id);
            info!("新模式: {}", manager.current_mode());
            info!("允許真實交易: {}", manager.allows_real_trading());
            
            assert_eq!(manager.current_mode(), TradingMode::Live);
            assert!(manager.allows_real_trading());
            assert!(manager.allows_simulation());
        }
        Err(error) => {
            warn!("⚠️ 切換到真實交易模式失敗（這是預期的）: {}", error);
            // 在演示中，這個失敗是預期的，因為我們沒有真實的前置條件檢查
        }
    }
    
    Ok(())
}

/// 演示緊急停止功能
async fn demonstrate_emergency_stop(manager: &TradingModeManager) -> Result<()> {
    info!("\n=== 🚨 演示5: 緊急停止功能 ===");
    
    // 如果當前不在真實交易模式，先切換到真實交易模式（用於演示）
    if manager.current_mode() != TradingMode::Live {
        info!("為演示緊急停止，先模擬切換到 Live 模式...");
        // 這裡我們直接模擬，實際生產環境中不應該這樣做
    }
    
    let emergency_result = manager
        .emergency_stop("檢測到異常市場波動".to_string())
        .await;
    
    match emergency_result {
        Ok(stop_id) => {
            info!("✅ 緊急停止成功，停止ID: {}", stop_id);
            info!("當前模式: {}", manager.current_mode());
            
            assert_eq!(manager.current_mode(), TradingMode::DataOnly);
            assert!(!manager.allows_real_trading());
        }
        Err(error) => {
            error!("❌ 緊急停止失敗: {}", error);
            return Err(anyhow::anyhow!("緊急停止失敗: {}", error));
        }
    }
    
    Ok(())
}

/// 演示統計和歷史查看
async fn demonstrate_statistics_and_history(manager: &TradingModeManager) -> Result<()> {
    info!("\n=== 📊 演示6: 統計和歷史查看 ===");
    
    // 獲取統計信息
    let stats = manager.get_statistics();
    info!("=== 交易模式統計 ===");
    info!("當前模式: {}", stats.current_mode);
    info!("當前模式持續時間: {}秒", stats.current_mode_duration);
    info!("當前模式交易數: {}", stats.trades_in_current_mode);
    info!("總切換次數: {}", stats.total_transitions);
    info!("數據模式總時長: {}秒", stats.data_only_total_duration);
    info!("模擬模式總時長: {}秒", stats.dry_run_total_duration);
    info!("真實交易總時長: {}秒", stats.live_total_duration);
    
    // 獲取切換歷史
    let history = manager.get_transition_history();
    info!("\n=== 切換歷史 (最近{}條) ===", history.len());
    for (i, record) in history.iter().enumerate() {
        info!(
            "{}. {} -> {} | 用戶: {:?} | 原因: {} | 成功: {} | 時間: {}",
            i + 1,
            record.from_mode,
            record.to_mode,
            record.user_id,
            record.reason,
            record.success,
            record.timestamp
        );
    }
    
    Ok(())
}

/// 演示併發切換保護
async fn demonstrate_concurrent_protection(manager: &TradingModeManager) -> Result<()> {
    info!("\n=== 🛡️ 演示7: 併發切換保護 ===");
    
    // 啟動兩個併發的模式切換請求
    let manager1 = manager;
    let manager2 = manager;
    
    info!("啟動兩個併發的模式切換請求...");
    
    let task1 = async {
        manager1
            .request_mode_transition(
                TradingMode::DryRun,
                Some("user1".to_string()),
                "併發測試1".to_string(),
            )
            .await
    };
    
    let task2 = async {
        // 稍微延遲以確保第一個請求先開始
        sleep(Duration::from_millis(10)).await;
        manager2
            .request_mode_transition(
                TradingMode::Live,
                Some("user2".to_string()),
                "併發測試2".to_string(),
            )
            .await
    };
    
    let (result1, result2) = tokio::join!(task1, task2);
    
    info!("併發測試結果:");
    match &result1 {
        Ok(id) => info!("✅ 第一個請求成功: {}", id),
        Err(e) => info!("❌ 第一個請求失敗: {}", e),
    }
    
    match &result2 {
        Ok(id) => info!("✅ 第二個請求成功: {}", id),
        Err(e) => info!("❌ 第二個請求失敗: {}", e),
    }
    
    // 應該只有一個成功
    let success_count = [&result1, &result2].iter().filter(|r| r.is_ok()).count();
    info!("成功的請求數: {}/2", success_count);
    
    if success_count != 1 {
        warn!("⚠️ 併發保護可能有問題，預期只有1個成功，實際: {}", success_count);
    } else {
        info!("✅ 併發保護正常工作");
    }
    
    Ok(())
}

/// 輔助函數：演示模式安全性檢查
#[allow(dead_code)]
fn demonstrate_mode_safety_checks() {
    info!("\n=== 🔒 交易模式安全性檢查 ===");
    
    let modes = [TradingMode::DataOnly, TradingMode::DryRun, TradingMode::Live];
    
    for mode in modes {
        info!("模式: {} (安全等級: {})", mode, mode.safety_level());
        info!("  - 允許真實交易: {}", mode.allows_real_trading());
        info!("  - 允許模擬交易: {}", mode.allows_simulation());
        info!("  - 允許市場數據: {}", mode.allows_market_data());
    }
}