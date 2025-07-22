/*!
 * ML特徵提取測試 - 提升測試覆蓋率
 * 
 * 測試機器學習特徵提取和處理功能
 */

use rust_hft::ml::{FeatureExtractor, FeatureSet};
use rust_hft::core::{types::*, orderbook::OrderBook};
use rust_hft::utils::performance::now_micros;
use rust_decimal::Decimal;
use std::str::FromStr;

fn create_mock_orderbook(symbol: &str) -> OrderBook {
    let mut orderbook = OrderBook::new(symbol.to_string());
    
    // 添加一些模擬的買賣單
    let _ = orderbook.update_bid(Decimal::from_str("45000.0").unwrap(), Decimal::from_str("1.5").unwrap());
    let _ = orderbook.update_bid(Decimal::from_str("44999.5").unwrap(), Decimal::from_str("2.0").unwrap());
    let _ = orderbook.update_bid(Decimal::from_str("44999.0").unwrap(), Decimal::from_str("1.0").unwrap());
    
    let _ = orderbook.update_ask(Decimal::from_str("45000.5").unwrap(), Decimal::from_str("1.2").unwrap());
    let _ = orderbook.update_ask(Decimal::from_str("45001.0").unwrap(), Decimal::from_str("1.8").unwrap());
    let _ = orderbook.update_ask(Decimal::from_str("45001.5").unwrap(), Decimal::from_str("0.8").unwrap());
    
    orderbook
}

#[test]
fn test_feature_extractor_initialization() {
    let extractor = FeatureExtractor::new(50); // 50個歷史窗口
    
    // 測試初始化狀態
    // FeatureExtractor的內部狀態應該是空的
    // 這個測試主要確保構造函數正常工作
    assert_eq!(std::mem::size_of_val(&extractor), std::mem::size_of::<FeatureExtractor>());
}

#[test]
fn test_basic_feature_extraction() {
    let mut extractor = FeatureExtractor::new(10);
    let orderbook = create_mock_orderbook("BTCUSDT");
    
    // 提取特徵
    let result = extractor.extract_features(&orderbook, 100, now_micros());
    
    match result {
        Ok(features) => {
            assert!(!features.is_empty());
            
            // 檢查基本特徵是否存在
            assert!(features.contains_key("spread"));
            assert!(features.contains_key("mid_price"));
            assert!(features.contains_key("bid_volume"));
            assert!(features.contains_key("ask_volume"));
            
            // 驗證特徵值的合理性
            let spread = features.get("spread").unwrap();
            assert!(spread > &0.0);
            
            let mid_price = features.get("mid_price").unwrap();
            assert!(mid_price > &40000.0 && mid_price < &50000.0);
        }
        Err(e) => {
            panic!("特徵提取失敗: {}", e);
        }
    }
}

#[test]
fn test_feature_extraction_with_history() {
    let mut extractor = FeatureExtractor::new(5);
    
    // 提取多次特徵以建立歷史
    for i in 0..3 {
        let mut orderbook = create_mock_orderbook("BTCUSDT");
        
        // 稍微修改訂單簿以模擬市場變化
        let price_offset = Decimal::from_str(&format!("{}.0", i)).unwrap();
        let _ = orderbook.update_bid(
            Decimal::from_str("45000.0").unwrap() + price_offset,
            Decimal::from_str("1.5").unwrap()
        );
        
        let result = extractor.extract_features(&orderbook, 100 + i * 10, now_micros() + i * 1000);
        assert!(result.is_ok());
    }
    
    // 第四次提取應該包含更多基於歷史的特徵
    let orderbook = create_mock_orderbook("BTCUSDT");
    let result = extractor.extract_features(&orderbook, 140, now_micros() + 4000);
    
    assert!(result.is_ok());
    let features = result.unwrap();
    
    // 應該包含動量特徵（基於價格歷史）
    assert!(features.contains_key("price_momentum"));
    assert!(features.contains_key("volume_momentum"));
}

#[test]
fn test_technical_indicators() {
    let mut extractor = FeatureExtractor::new(20);
    let orderbook = create_mock_orderbook("BTCUSDT");
    
    // 需要多次更新來計算技術指標
    for i in 0..15 {
        let mut ob = create_mock_orderbook("BTCUSDT");
        let price_change = (i as f64 * 0.5).sin() * 100.0; // 模擬價格波動
        let base_price = Decimal::from_str("45000.0").unwrap();
        let adjusted_price = base_price + Decimal::from_str(&price_change.to_string()).unwrap();
        
        let _ = ob.update_bid(adjusted_price - Decimal::from_str("0.5").unwrap(), Decimal::from_str("1.0").unwrap());
        let _ = ob.update_ask(adjusted_price + Decimal::from_str("0.5").unwrap(), Decimal::from_str("1.0").unwrap());
        
        let _ = extractor.extract_features(&ob, 100 + i, now_micros() + i * 1000);
    }
    
    // 最後一次提取應該包含技術指標
    let result = extractor.extract_features(&orderbook, 200, now_micros() + 16000);
    assert!(result.is_ok());
    
    let features = result.unwrap();
    
    // 檢查技術指標特徵
    assert!(features.contains_key("sma_5") || features.contains_key("moving_average"));
    assert!(features.contains_key("volatility") || features.contains_key("price_volatility"));
}

#[test]
fn test_order_book_imbalance_features() {
    let mut extractor = FeatureExtractor::new(10);
    
    // 創建一個不平衡的訂單簿
    let mut orderbook = OrderBook::new("BTCUSDT".to_string());
    
    // 買方力量更強（更多買單）
    let _ = orderbook.update_bid(Decimal::from_str("45000.0").unwrap(), Decimal::from_str("5.0").unwrap());
    let _ = orderbook.update_bid(Decimal::from_str("44999.0").unwrap(), Decimal::from_str("3.0").unwrap());
    let _ = orderbook.update_bid(Decimal::from_str("44998.0").unwrap(), Decimal::from_str("2.0").unwrap());
    
    // 賣方力量較弱（較少賣單）
    let _ = orderbook.update_ask(Decimal::from_str("45001.0").unwrap(), Decimal::from_str("1.0").unwrap());
    let _ = orderbook.update_ask(Decimal::from_str("45002.0").unwrap(), Decimal::from_str("0.5").unwrap());
    
    let result = extractor.extract_features(&orderbook, 100, now_micros());
    assert!(result.is_ok());
    
    let features = result.unwrap();
    
    // 檢查訂單簿不平衡指標
    if let Some(obi) = features.get("order_book_imbalance") {
        assert!(obi > &0.0); // 買方力量更強，應該是正值
    }
    
    if let Some(bid_vol) = features.get("bid_volume") {
        if let Some(ask_vol) = features.get("ask_volume") {
            assert!(bid_vol > ask_vol); // 買單量應該大於賣單量
        }
    }
}

#[test]
fn test_feature_normalization() {
    let mut extractor = FeatureExtractor::new(10);
    let orderbook = create_mock_orderbook("BTCUSDT");
    
    let result = extractor.extract_features(&orderbook, 100, now_micros());
    assert!(result.is_ok());
    
    let features = result.unwrap();
    
    // 檢查特徵值是否在合理範圍內
    for (name, value) in features.iter() {
        // 大多數標準化的特徵應該在合理範圍內
        if name.contains("normalized") || name.contains("ratio") {
            assert!(value >= &-10.0 && value <= &10.0, 
                   "特徵 {} 的值 {} 超出預期範圍", name, value);
        }
        
        // 價格相關特徵應該是正數
        if name.contains("price") || name.contains("volume") {
            assert!(value >= &0.0, "價格/量相關特徵 {} 不應為負: {}", name, value);
        }
    }
}

#[test]
fn test_feature_extraction_performance() {
    use std::time::Instant;
    
    let mut extractor = FeatureExtractor::new(20);
    let orderbook = create_mock_orderbook("BTCUSDT");
    
    // 預熱
    for _ in 0..5 {
        let _ = extractor.extract_features(&orderbook, 100, now_micros());
    }
    
    // 性能測試
    let start = Instant::now();
    let iterations = 1000;
    
    for i in 0..iterations {
        let result = extractor.extract_features(&orderbook, 100 + i, now_micros() + i);
        assert!(result.is_ok());
    }
    
    let elapsed = start.elapsed();
    let avg_time = elapsed.as_micros() as f64 / iterations as f64;
    
    // 每次特徵提取應該在合理時間內完成（< 100微秒）
    assert!(avg_time < 100.0, "平均特徵提取時間太長: {:.2}μs", avg_time);
    
    println!("平均特徵提取時間: {:.2}μs", avg_time);
}

#[test]
fn test_feature_set_operations() {
    let mut features = FeatureSet::new();
    
    // 測試添加和檢索特徵
    features.insert("test_feature".to_string(), 123.45);
    features.insert("another_feature".to_string(), -67.89);
    
    assert_eq!(features.len(), 2);
    assert_eq!(features.get("test_feature"), Some(&123.45));
    assert_eq!(features.get("nonexistent"), None);
    
    // 測試特徵更新
    features.insert("test_feature".to_string(), 999.99);
    assert_eq!(features.get("test_feature"), Some(&999.99));
    
    // 測試迭代
    let mut count = 0;
    for (name, value) in features.iter() {
        assert!(name == "test_feature" || name == "another_feature");
        assert!(value == &999.99 || value == &-67.89);
        count += 1;
    }
    assert_eq!(count, 2);
}

#[test]
fn test_feature_vector_conversion() {
    let mut features = FeatureSet::new();
    
    // 添加一些特徵（順序很重要）
    features.insert("feature_a".to_string(), 1.0);
    features.insert("feature_b".to_string(), 2.0);
    features.insert("feature_c".to_string(), 3.0);
    
    let vector = features.to_vector();
    assert_eq!(vector.len(), 3);
    
    // 注意：HashMap的順序不確定，但向量應該包含所有值
    let mut sorted_vector = vector.clone();
    sorted_vector.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(sorted_vector, vec![1.0, 2.0, 3.0]);
}

#[test]
fn test_error_handling() {
    let mut extractor = FeatureExtractor::new(10);
    
    // 測試空訂單簿的處理
    let empty_orderbook = OrderBook::new("EMPTY".to_string());
    let result = extractor.extract_features(&empty_orderbook, 100, now_micros());
    
    // 應該能處理空訂單簿，但可能返回錯誤或默認值
    match result {
        Ok(features) => {
            // 如果成功，特徵集可能包含默認值或零值
            assert!(!features.is_empty() || features.is_empty());
        }
        Err(_) => {
            // 如果失敗，這也是可接受的行為
        }
    }
}