/*! 
 * 安全模塊集成演示 - 展示完整的安全工作流程
 * 
 * 本演示展示了如何使用 HFT 系統的完整安全基礎設施：
 * - 🔐 API 憑證管理和自動輪換
 * - 🔑 加密密鑰管理和數據保護
 * - 🚦 API 請求限流和防護
 * - ✍️ 請求簽名和身份驗證
 * - 🛡️ TLS/mTLS 配置和證書管理
 * - 📊 安全審計和健康監控
 */

use rust_hft::security::*;
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error, debug};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌系統
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("🚀 啟動 HFT 系統安全模塊集成演示");

    // 創建演示用安全配置（禁用 nonce 保護以避免重放檢測）
    let mut security_config = SecurityConfig::production();
    security_config.signing.enable_nonce_protection = false;
    
    info!("🔒 初始化安全管理器 (生產環境配置)");
    let security_manager = SecurityManager::new(security_config).await?;
    
    // 演示1: 添加交易所憑證
    demo_credential_management(&security_manager).await?;
    
    // 演示2: 數據加密和解密
    demo_data_encryption(&security_manager).await?;
    
    // 演示3: API 請求簽名
    demo_request_signing(&security_manager).await?;
    
    // 演示4: API 限流保護
    demo_rate_limiting(&security_manager).await?;
    
    // 演示5: 安全健康檢查
    demo_health_monitoring(&security_manager).await?;
    
    // 演示6: 憑證輪換演示
    demo_credential_rotation(&security_manager).await?;

    info!("✅ 安全模塊集成演示完成！");
    Ok(())
}

/// 演示1: 憑證管理工作流程
async fn demo_credential_management(security_manager: &SecurityManager) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n🔐 === 演示1: 憑證管理工作流程 ===");
    
    // 添加 Bitget 測試憑證
    info!("📝 添加 Bitget 測試憑證...");
    security_manager.add_exchange_credentials(
        "bitget".to_string(),
        "test_account_001".to_string(),
        "demo_api_key_12345".to_string(),
        "demo_secret_key_abcde".to_string(),
        Some("demo_passphrase_xyz".to_string()),
        true, // 測試網
    ).await?;
    
    // 添加 Binance 測試憑證
    info!("📝 添加 Binance 測試憑證...");
    security_manager.add_exchange_credentials(
        "binance".to_string(),
        "test_account_002".to_string(),
        "demo_binance_key_67890".to_string(),
        "demo_binance_secret_fghij".to_string(),
        None, // Binance 不需要 passphrase
        true, // 測試網
    ).await?;
    
    // 獲取並驗證憑證
    info!("🔍 驗證憑證可用性...");
    let bitget_creds = security_manager.get_exchange_credentials("bitget").await?;
    let binance_creds = security_manager.get_exchange_credentials("binance").await?;
    
    info!("✅ Bitget 憑證: 帳戶 {}, 密鑰 ID {}", 
          bitget_creds.account_id, bitget_creds.primary.key_id);
    info!("✅ Binance 憑證: 帳戶 {}, 密鑰 ID {}", 
          binance_creds.account_id, binance_creds.primary.key_id);
    
    Ok(())
}

/// 演示2: 數據加密和解密工作流程
async fn demo_data_encryption(security_manager: &SecurityManager) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n🔑 === 演示2: 數據加密和解密工作流程 ===");
    
    // 敏感交易數據
    let sensitive_data = vec![
        "Account Balance: $1,234,567.89",
        "API Secret: sk_live_1234567890abcdef",
        "Trading Strategy: Mean Reversion v2.1",
        "Position Size: 10,000 BTC",
    ];
    
    info!("🔐 加密敏感交易數據...");
    let mut encrypted_data = Vec::new();
    
    for (i, data) in sensitive_data.iter().enumerate() {
        let encrypted = security_manager.encrypt_sensitive_data(data.as_bytes()).await?;
        encrypted_data.push(encrypted);
        info!("✅ 數據 {} 加密完成 ({} 字節)", i + 1, encrypted_data[i].len());
    }
    
    info!("🔓 解密數據並驗證完整性...");
    for (i, encrypted) in encrypted_data.iter().enumerate() {
        let decrypted = security_manager.decrypt_sensitive_data(encrypted).await?;
        let decrypted_str = String::from_utf8(decrypted)?;
        
        if decrypted_str == sensitive_data[i] {
            info!("✅ 數據 {} 解密成功: {}", i + 1, decrypted_str);
        } else {
            error!("❌ 數據 {} 解密失敗: 內容不匹配", i + 1);
        }
    }
    
    Ok(())
}

/// 演示3: API 請求簽名工作流程
async fn demo_request_signing(security_manager: &SecurityManager) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n✍️ === 演示3: API 請求簽名工作流程 ===");
    
    // 獲取 Bitget 憑證用於簽名
    let bitget_creds = security_manager.get_exchange_credentials("bitget").await?;
    let credentials = &bitget_creds.primary;
    
    // 模擬不同類型的 API 請求
    let api_requests = vec![
        ("GET", "https://api.bitget.com/api/v2/spot/account/info", ""),
        ("POST", "https://api.bitget.com/api/v2/spot/trade/place-order", r#"{"symbol":"BTCUSDT","side":"buy","orderType":"limit","force":"gtc","size":"0.001","price":"50000"}"#),
        ("GET", "https://api.bitget.com/api/v2/spot/market/orderbook?symbol=BTCUSDT&limit=10", ""),
        ("DELETE", "https://api.bitget.com/api/v2/spot/trade/cancel-order", r#"{"orderId":"12345","symbol":"BTCUSDT"}"#),
    ];
    
    info!("🔏 為 {} 個 API 請求生成簽名...", api_requests.len());
    
    for (i, (method, url, body)) in api_requests.iter().enumerate() {
        let headers = security_manager.sign_request(method, url, body, credentials).await?;
        
        info!("✅ 請求 {} ({} {})", i + 1, method, url);
        debug!("   簽名頭部: {:?}", headers);
        
        // 驗證必要的頭部存在
        if headers.contains_key("ACCESS-KEY") && 
           headers.contains_key("ACCESS-SIGN") && 
           headers.contains_key("ACCESS-TIMESTAMP") {
            info!("   ✓ 簽名頭部完整");
        } else {
            warn!("   ⚠️ 簽名頭部不完整: {:?}", headers.keys().collect::<Vec<_>>());
        }
    }
    
    Ok(())
}

/// 演示4: API 限流保護工作流程
async fn demo_rate_limiting(security_manager: &SecurityManager) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n🚦 === 演示4: API 限流保護工作流程 ===");
    
    let api_key = "demo_api_key_12345";
    let endpoint = "/api/v2/spot/market/ticker";
    
    info!("🔄 模擬高頻 API 請求 (每 100ms 一次)...");
    
    let mut successful_requests = 0;
    let mut limited_requests = 0;
    
    // 模擬 50 個快速請求
    for i in 1..=50 {
        let allowed = security_manager.check_rate_limit(api_key, endpoint).await?;
        
        if allowed {
            successful_requests += 1;
            debug!("✅ 請求 {} 允許通過", i);
        } else {
            limited_requests += 1;
            warn!("🚫 請求 {} 被限流阻止", i);
        }
        
        sleep(Duration::from_millis(100)).await;
    }
    
    info!("📊 限流測試結果:");
    info!("   成功請求: {} 個", successful_requests);
    info!("   被限流阻止: {} 個", limited_requests);
    info!("   成功率: {:.1}%", 
          successful_requests as f64 / (successful_requests + limited_requests) as f64 * 100.0);
    
    Ok(())
}

/// 演示5: 安全健康監控工作流程
async fn demo_health_monitoring(security_manager: &SecurityManager) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n📊 === 演示5: 安全健康監控工作流程 ===");
    
    info!("🔍 執行安全健康檢查...");
    let health_report = security_manager.health_check().await?;
    
    info!("📋 安全健康報告:");
    info!("   總體健康分數: {:.2}", health_report.overall_health);
    info!("   憑證狀態: {}", health_report.credential_status);
    info!("   加密狀態: {}", health_report.encryption_status);
    info!("   證書狀態: {}", health_report.certificate_status);
    info!("   限流器狀態: {}", health_report.rate_limiter_status);
    info!("   檢查時間: {}", health_report.timestamp);
    
    // 根據健康分數給出建議
    match health_report.overall_health {
        score if score >= 0.9 => info!("🟢 系統安全狀態良好"),
        score if score >= 0.7 => warn!("🟡 系統安全狀態一般，建議檢查"),
        _ => error!("🔴 系統安全狀態不佳，需要立即處理"),
    }
    
    Ok(())
}

/// 演示6: 憑證輪換工作流程
async fn demo_credential_rotation(security_manager: &SecurityManager) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n🔄 === 演示6: 憑證輪換工作流程 ===");
    
    info!("📅 檢查憑證是否需要輪換...");
    
    // 模擬憑證輪換檢查
    match security_manager.check_credential_rotation_needed().await {
        Ok(_) => info!("✅ 憑證輪換檢查完成"),
        Err(e) => warn!("⚠️ 憑證輪換檢查時發現問題: {}", e),
    }
    
    info!("🔄 演示手動憑證輪換 (Bitget)...");
    
    // 注意: 在實際環境中，這會與交易所 API 交互生成新憑證
    // 這裡只是演示輪換流程
    match security_manager.rotate_credentials("bitget").await {
        Ok(_) => info!("✅ Bitget 憑證輪換成功"),
        Err(e) => warn!("⚠️ Bitget 憑證輪換失敗: {} (這在演示環境中是正常的)", e),
    }
    
    info!("📋 憑證輪換最佳實踐:");
    info!("   • 定期檢查憑證年齡和使用情況");
    info!("   • 在憑證過期前提前輪換");
    info!("   • 保留備用憑證以確保服務連續性");
    info!("   • 記錄所有輪換活動供審計使用");
    info!("   • 在檢測到安全威脅時立即輪換");
    
    Ok(())
}

/// 演示7: TLS 配置工作流程 (額外演示)
#[allow(dead_code)]
async fn demo_tls_configuration(security_manager: &SecurityManager) -> Result<(), Box<dyn std::error::Error>> {
    info!("\n🛡️ === 演示7: TLS 配置工作流程 ===");
    
    info!("🔐 獲取 TLS 客戶端配置...");
    
    let exchanges = vec!["bitget", "binance", "bybit"];
    
    for exchange in exchanges {
        match security_manager.get_tls_client_config(exchange).await {
            Ok(tls_config) => {
                info!("✅ {} TLS 配置獲取成功", exchange);
                debug!("   配置詳情: {:?}", tls_config);
            }
            Err(e) => {
                warn!("⚠️ {} TLS 配置獲取失敗: {}", exchange, e);
            }
        }
    }
    
    info!("🔒 TLS 安全建議:");
    info!("   • 使用 TLS 1.3 或最新版本");
    info!("   • 啟用證書固定 (Certificate Pinning)");
    info!("   • 定期更新 CA 證書庫");
    info!("   • 監控證書過期時間");
    info!("   • 使用強加密算法套件");
    
    Ok(())
}

/// 演示8: 安全事件響應工作流程 (額外演示)
#[allow(dead_code)]
async fn demo_security_incident_response() -> Result<(), Box<dyn std::error::Error>> {
    info!("\n🚨 === 演示8: 安全事件響應工作流程 ===");
    
    info!("🔍 模擬安全威脅檢測...");
    
    let security_events = vec![
        "異常 IP 位址頻繁訪問",
        "API 密鑰可能洩露",
        "異常大額交易嘗試",
        "帳戶餘額異常變化",
        "多次失敗的身份驗證",
    ];
    
    for (i, event) in security_events.iter().enumerate() {
        warn!("🚨 安全事件 {}: {}", i + 1, event);
        
        // 模擬響應措施
        match i {
            0 => info!("   📋 響應: 啟用 IP 白名單，阻止可疑 IP"),
            1 => info!("   📋 響應: 立即輪換 API 密鑰，撤銷舊密鑰"),
            2 => info!("   📋 響應: 暫停交易，人工審核"),
            3 => info!("   📋 響應: 凍結帳戶，聯繫用戶驗證"),
            4 => info!("   📋 響應: 臨時鎖定帳戶，發送安全警告"),
            _ => info!("   📋 響應: 記錄事件，增強監控"),
        }
        
        sleep(Duration::from_millis(500)).await;
    }
    
    info!("📋 安全響應流程完成");
    
    Ok(())
}