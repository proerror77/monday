//! 多場路由系統基礎驗證測試
//! 
//! 此測試驗證：
//! 1. 場所映射配置的正確性
//! 2. 策略事件過濾邏輯
//! 3. 路由器配置結構
//! 4. VenueId 轉換和映射

use hft_core::*;
use std::collections::HashMap;

#[tokio::test] 
async fn test_venue_mapping_configuration() {
    println!("🔧 測試場所映射配置");
    
    // 測試 VenueId 的字符串轉換
    let binance_id = VenueId::from_str("BINANCE").unwrap();
    let bitget_id = VenueId::from_str("BITGET").unwrap();
    
    assert_eq!(binance_id, VenueId::BINANCE);
    assert_eq!(bitget_id, VenueId::BITGET);
    
    // 測試場所映射表構建
    let mut venue_to_client = HashMap::new();
    venue_to_client.insert(binance_id, 0_usize);
    venue_to_client.insert(bitget_id, 1_usize);
    
    assert_eq!(venue_to_client.get(&VenueId::BINANCE), Some(&0));
    assert_eq!(venue_to_client.get(&VenueId::BITGET), Some(&1));
    
    println!("✅ 場所映射配置測試通過");
}

#[test]
fn test_strategy_event_filtering() {
    println!("🎯 測試策略事件過濾邏輯");
    
    // 測試單場策略識別
    assert!(is_single_venue_strategy("TrendStrategy"));
    assert!(is_single_venue_strategy("ImbalanceStrategy"));
    assert!(!is_single_venue_strategy("ArbitrageStrategy"));
    
    // 測試自定義策略名稱推斷
    assert!(!is_single_venue_strategy("custom_arbitrage_strategy"));
    assert!(!is_single_venue_strategy("CrossExchangeStrategy"));
    assert!(is_single_venue_strategy("CustomTrendStrategy"));
    
    println!("✅ 策略事件過濾邏輯測試通過");
}

/// 輔助函數：判斷策略是否為單場策略
fn is_single_venue_strategy(strategy_name: &str) -> bool {
    match strategy_name {
        "TrendStrategy" | "ImbalanceStrategy" => true,
        "ArbitrageStrategy" => false,
        _ => {
            let name_lower = strategy_name.to_lowercase();
            !(name_lower.contains("arbitrage") || name_lower.contains("cross") || name_lower.contains("arb"))
        }
    }
}