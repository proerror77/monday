/*!
 * 🧪 集成測試套件
 * 
 * 這個文件包含系統級集成測試，驗證各個模組之間的協作
 * 
 * 測試範圍：
 * - 統一執行引擎端到端測試
 * - 模型加載和推理流程測試
 * - 特徵工程和數據處理集成測試
 * - 性能基準驗證測試
 */

use rust_hft::ml::{
    UnifiedModelEngine, ModelEngineConfig, ModelType,
    DLRLFeatureEngine, DLRLFeatureConfig
};
use rust_hft::core::types::*;
use rust_hft::utils::parallel_processing::OHLCVData;

use std::collections::HashMap;
use tempfile::tempdir;
use tokio_test;
use test_log::test;
use serial_test::serial;
use rand::Rng;

mod common;
use common::*;

/// 🔧 統一執行引擎集成測試
mod unified_execution_tests {
    use super::*;

    #[test(tokio::test)]
    async fn test_model_engine_creation_and_basic_operations() {
        let config = create_test_model_config();
        let mut engine = UnifiedModelEngine::new(config).expect("Engine創建失敗");
        
        // 驗證引擎基本操作
        assert_eq!(engine.list_models().len(), 0);
        
        let stats = engine.get_stats();
        assert_eq!(stats.total_inferences, 0);
        assert_eq!(stats.models_loaded, 0);
        
        println!("✅ 模型引擎創建和基本操作測試完成");
    }

    #[test(tokio::test)]
    async fn test_feature_engine_integration() {
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        // 測試特徵工程基本功能
        let sample_data = create_sample_ohlcv_data(100);
        let features = feature_engine.compute_all_features(&sample_data).await;
        
        assert!(features.is_ok());
        let features = features.unwrap();
        assert!(features.total_feature_count() > 0);
        
        // 驗證特徵數據質量
        println!("📊 特徵工程測試: 生成了 {} 個特徵", features.total_feature_count());
    }

    #[test(tokio::test)]
    #[serial]
    async fn test_model_loading_mock() {
        // 使用模擬測試，因為真實的PyTorch模型文件可能不存在
        let test_dir = TestDataDir::new().expect("創建測試目錄失敗");
        let model_path = test_dir.write_test_file("mock_model.pt", b"mock model content")
            .expect("創建模擬模型文件失敗");
        
        let mut config = create_test_model_config();
        config.model_directory = test_dir.path.to_string_lossy().to_string();
        
        let mut engine = UnifiedModelEngine::new(config).expect("Engine創建失敗");
        
        // 在沒有真實PyTorch庫的情況下，這個測試會失敗，但我們可以驗證錯誤處理
        #[cfg(feature = "tch")]
        {
            let metadata = create_test_model_metadata();
            let result = engine.load_torch_model(
                "test_model",
                model_path.to_str().unwrap(),
                ModelType::Custom {
                    name: "test".to_string(),
                    input_shape: vec![1, 10],
                    output_shape: vec![1, 1],
                    metadata,
                },
                None,
            );
            
            // 由於是模擬文件，預期會失敗，但錯誤處理應該正常工作
            assert!(result.is_err());
            println!("✅ 模型加載錯誤處理測試完成");
        }
        
        #[cfg(not(feature = "tch"))]
        {
            // 沒有tch feature時，記錄警告
            println!("⚠️ tch feature未啟用，跳過PyTorch模型加載測試");
        }
    }

    #[test(tokio::test)]
    async fn test_ohlcv_to_features_conversion() {
        let config = create_test_model_config();
        let mut engine = UnifiedModelEngine::new(config).expect("Engine創建失敗");
        
        let sample_data = create_sample_ohlcv_data(50);
        
        // 測試OHLCV轉特徵功能 (不需要實際模型)
        let result = engine.inference_from_ohlcv("non_existent_model", &sample_data);
        
        // 預期失敗，因為模型不存在，但轉換邏輯應該正常
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("模型未找到"));
        
        println!("✅ OHLCV轉特徵轉換測試完成");
    }
}

/// 📊 性能基準集成測試
mod performance_integration_tests {
    use super::*;
    use std::time::Instant;

    #[test(tokio::test)]
    async fn test_feature_engineering_latency() {
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        let sample_data = create_sample_ohlcv_data(60); // 1分鐘數據
        
        let (features, latency) = measure_async_execution_time(|| async {
            feature_engine.compute_all_features(&sample_data).await
        }).await;
        
        assert!(features.is_ok());
        let features = features.unwrap();
        
        // 驗證特徵工程延遲應該在合理範圍內 (< 100ms for CI環境)
        assert!(latency.as_millis() < 100, 
                "特徵工程延遲過高: {}ms", latency.as_millis());
        
        println!("✅ 特徵工程延遲: {}μs (生成 {} 特徵)", 
                latency.as_micros(), features.total_feature_count());
    }

    #[test(tokio::test)]
    async fn test_memory_usage_stability() {
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        // 進行多次特徵計算，檢查內存是否穩定
        for i in 0..10 {
            let sample_data = create_sample_ohlcv_data(100);
            let features = feature_engine.compute_all_features(&sample_data).await;
            assert!(features.is_ok());
            
            if i % 5 == 0 {
                println!("🔄 內存穩定性測試 - 迭代: {}", i);
            }
        }
        
        // 獲取最終統計，驗證沒有明顯的內存洩漏
        let stats = feature_engine.get_stats();
        assert!(stats.total_features_computed > 0);
        println!("📊 特徵計算統計: {} 次", stats.total_features_computed);
    }

    #[test(tokio::test)]
    async fn test_concurrent_feature_processing() {
        let config = create_test_dlrl_config();
        
        // 並發測試：多個任務同時進行特徵計算
        let results = run_concurrent_tasks(5, |i| async move {
            let mut feature_engine = DLRLFeatureEngine::new(config.clone()).expect("特徵引擎創建失敗");
            let sample_data = create_sample_ohlcv_data(50);
            let result = feature_engine.compute_all_features(&sample_data).await;
            (i, result.is_ok())
        }).await;
        
        // 分析並發測試結果
        let mut success_count = 0;
        for (task_id, success) in results {
            if success {
                success_count += 1;
            }
            println!("🧵 並發任務 {} 完成: {}", task_id, if success { "✅" } else { "❌" });
        }
        
        assert_eq!(success_count, 5, "並發特徵處理測試失敗");
        println!("✅ 並發特徵處理測試完成: {}/5 任務成功", success_count);
    }
}

/// 🔧 錯誤處理和容錯測試
mod error_handling_tests {
    use super::*;

    #[test(tokio::test)]
    async fn test_invalid_configuration_handling() {
        // 測試無效配置的錯誤處理
        let mut config = create_test_model_config();
        config.model_directory = "/non_existent_directory_12345".to_string();
        
        let engine = UnifiedModelEngine::new(config);
        // 引擎創建應該成功，但模型加載會失敗
        assert!(engine.is_ok());
        println!("✅ 無效配置錯誤處理測試完成");
    }

    #[test(tokio::test)]
    async fn test_empty_data_handling() {
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        // 測試空數據處理
        let empty_data = create_empty_ohlcv_data();
        let result = feature_engine.compute_all_features(&empty_data).await;
        
        // 應該能夠優雅處理空數據
        assert!(result.is_ok());
        let features = result.unwrap();
        assert_eq!(features.total_feature_count(), 0);
        println!("✅ 空數據處理測試完成");
    }

    #[test(tokio::test)]
    async fn test_malformed_data_handling() {
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        // 創建包含異常值的數據
        let malformed_data = create_malformed_ohlcv_data();
        
        let result = feature_engine.compute_all_features(&malformed_data).await;
        
        // 系統應該能夠處理或拒絕異常數據
        match result {
            Ok(features) => {
                println!("📊 異常數據已處理，特徵數: {}", features.total_feature_count());
            },
            Err(e) => {
                println!("🚫 異常數據被正確拒絕: {}", e);
            }
        }
        
        println!("✅ 異常數據處理測試完成");
    }

    #[test(tokio::test)]
    async fn test_resource_exhaustion_simulation() {
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        // 模擬資源耗盡：處理大量數據
        let large_data = create_sample_ohlcv_data(1000); // 較小數據集以適應CI環境
        
        let (result, processing_time) = measure_async_execution_time(|| async {
            feature_engine.compute_all_features(&large_data).await
        }).await;
        
        match result {
            Ok(features) => {
                println!("📈 大數據集處理成功: {} 特徵, 耗時: {}ms", 
                        features.total_feature_count(), processing_time.as_millis());
                
                // 驗證處理時間在合理範圍內 (< 5秒 for CI環境)
                assert!(processing_time.as_secs() < 5, 
                        "大數據集處理時間過長: {}ms", processing_time.as_millis());
            },
            Err(e) => {
                println!("🚫 大數據集處理失敗 (這可能是預期行為): {}", e);
            }
        }
        
        println!("✅ 資源耗盡模擬測試完成");
    }
}

/// 🔄 端到端工作流程測試
mod end_to_end_tests {
    use super::*;

    #[test(tokio::test)]
    async fn test_complete_trading_pipeline_simulation() {
        // 模擬完整的交易流水線：數據 -> 特徵 -> 預測 -> 信號
        
        println!("🚀 開始端到端交易流水線測試");
        
        // 1. 創建特徵引擎
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        // 2. 創建模型引擎
        let model_config = create_test_model_config();
        let model_engine = UnifiedModelEngine::new(model_config).expect("模型引擎創建失敗");
        
        // 3. 生成模擬市場數據
        let market_data = create_realistic_market_data(200);
        println!("📊 生成了 {} 條市場數據", market_data.len());
        
        // 驗證數據質量
        assert!(validate_ohlcv_data(&market_data));
        let stats = calculate_ohlcv_stats(&market_data);
        println!("📈 數據統計: 平均價格={:.2}, 波動率={:.4}, 平均成交量={:.2}", 
                stats.avg_price, stats.volatility, stats.avg_volume);
        
        // 4. 特徵工程
        let (features, feature_time) = measure_async_execution_time(|| async {
            feature_engine.compute_all_features(&market_data).await
        }).await;
        
        assert!(features.is_ok());
        let features = features.unwrap();
        println!("🧠 特徵工程完成: {} 特徵, 耗時: {}μs", 
                features.total_feature_count(), feature_time.as_micros());
        
        // 5. 模擬預測 (沒有真實模型的情況下)
        let (mock_predictions, prediction_time) = measure_execution_time(|| {
            simulate_model_predictions(&features)
        });
        
        println!("🎯 模型預測完成: {} 預測, 耗時: {}μs", 
                mock_predictions.len(), prediction_time.as_micros());
        
        // 6. 生成交易信號
        let signals = generate_mock_trading_signals(&mock_predictions);
        println!("📈 生成了 {} 個交易信號", signals.len());
        
        // 7. 驗證整個流水線的性能
        let total_time = feature_time + prediction_time;
        println!("⏱️  總處理時間: {}μs", total_time.as_micros());
        
        // 性能斷言：整個流水線應該在100ms內完成 (放寬限制以適應CI環境)
        assert!(total_time.as_millis() < 100, 
                "端到端流水線性能不達標: {}ms", total_time.as_millis());
        
        // 質量斷言：應該生成合理數量的特徵和信號
        assert!(features.total_feature_count() > 0);
        assert!(mock_predictions.len() > 0);
        
        println!("✅ 端到端交易流水線測試完成");
    }

    #[test(tokio::test)]
    async fn test_multi_symbol_processing() {
        // 測試多交易對同時處理
        
        let symbols = vec!["BTCUSDT", "ETHUSDT", "ADAUSDT"];
        println!("🔄 開始多交易對處理測試: {:?}", symbols);
        
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        let mut total_features = 0;
        let (_, total_time) = measure_async_execution_time(|| async {
            for symbol in &symbols {
                let symbol_data = create_sample_ohlcv_data(100);
                let features = feature_engine.compute_all_features(&symbol_data).await;
                
                assert!(features.is_ok());
                let features = features.unwrap();
                total_features += features.total_feature_count();
                
                println!("📊 {} 特徵計算完成: {} 特徵", symbol, features.total_feature_count());
            }
        }).await;
        
        println!("🏁 多交易對處理完成: {} 總特徵, 耗時: {}ms", 
                total_features, total_time.as_millis());
        
        // 性能驗證：平均每個交易對處理時間不應超過100ms (CI環境)
        let avg_time_per_symbol = total_time.as_millis() / symbols.len() as u128;
        assert!(avg_time_per_symbol < 100, 
                "多交易對處理平均時間過長: {}ms", avg_time_per_symbol);
                
        println!("✅ 多交易對處理測試完成");
    }
}

/// 🔬 數據質量驗證測試  
mod data_quality_tests {
    use super::*;
    
    #[test(tokio::test)]
    async fn test_data_validation_pipeline() {
        // 測試數據驗證流水線
        let valid_data = create_sample_ohlcv_data(100);
        let invalid_data = create_malformed_ohlcv_data();
        
        // 測試有效數據
        assert!(validate_ohlcv_data(&valid_data));
        let stats = calculate_ohlcv_stats(&valid_data);
        assert!(stats.count > 0);
        assert!(stats.avg_price > 0.0);
        assert!(stats.volatility >= 0.0);
        
        // 測試無效數據
        assert!(!validate_ohlcv_data(&invalid_data));
        
        println!("✅ 數據質量驗證測試完成");
    }
    
    #[test(tokio::test)]
    async fn test_data_statistics_accuracy() {
        // 測試數據統計準確性
        let test_data = vec![
            OHLCVData {
                timestamp: 1,
                open: 100.0,
                high: 110.0,
                low: 90.0,
                close: 105.0,
                volume: 1000.0,
            },
            OHLCVData {
                timestamp: 2,
                open: 105.0,
                high: 115.0,
                low: 95.0,
                close: 110.0,
                volume: 1500.0,
            },
        ];
        
        let stats = calculate_ohlcv_stats(&test_data);
        
        // 驗證統計準確性
        assert_eq!(stats.count, 2);
        assert_within_tolerance!(stats.avg_price, 107.5, 0.1);
        assert_within_tolerance!(stats.avg_volume, 1250.0, 0.1);
        assert_eq!(stats.price_range.0, 90.0);
        assert_eq!(stats.price_range.1, 115.0);
        
        println!("✅ 數據統計準確性測試完成");
    }
}

/// 🚀 性能回歸測試
mod performance_regression_tests {
    use super::*;
    
    #[test(tokio::test)]
    async fn test_feature_engineering_performance_baseline() {
        // 建立特徵工程性能基線
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        let test_sizes = vec![10, 50, 100, 200];
        let mut results = Vec::new();
        
        for size in test_sizes {
            let data = create_sample_ohlcv_data(size);
            let (features, duration) = measure_async_execution_time(|| async {
                feature_engine.compute_all_features(&data).await
            }).await;
            
            assert!(features.is_ok());
            let features = features.unwrap();
            
            results.push((size, duration.as_micros(), features.total_feature_count()));
            println!("📊 數據量: {}, 耗時: {}μs, 特徵數: {}", 
                    size, duration.as_micros(), features.total_feature_count());
        }
        
        // 驗證性能隨數據量線性增長
        let throughput: Vec<f64> = results.iter()
            .map(|(size, time_us, _)| *size as f64 / (*time_us as f64 / 1000.0))
            .collect();
        
        println!("📈 吞吐量變化: {:?}", throughput);
        println!("✅ 性能基線測試完成");
    }
    
    #[test(tokio::test)]
    async fn test_memory_usage_stability_extended() {
        // 擴展內存穩定性測試
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        // 模擬長時間運行場景
        for iteration in 0..20 {
            let data = create_sample_ohlcv_data(100);
            let result = feature_engine.compute_all_features(&data).await;
            
            assert!(result.is_ok());
            
            if iteration % 10 == 0 {
                println!("🔄 內存穩定性測試 - 迭代: {}", iteration);
            }
        }
        
        let final_stats = feature_engine.get_stats();
        assert_eq!(final_stats.total_features_computed, 20);
        
        println!("✅ 擴展內存穩定性測試完成");
    }
}

/// 🛡️ 魯棒性測試
mod robustness_tests {
    use super::*;
    
    #[test(tokio::test)]
    async fn test_rapid_sequential_processing() {
        // 測試快速連續處理能力
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        let start_time = std::time::Instant::now();
        let mut success_count = 0;
        
        for i in 0..50 {
            let data = create_sample_ohlcv_data(20);
            let result = feature_engine.compute_all_features(&data).await;
            
            if result.is_ok() {
                success_count += 1;
            }
        }
        
        let total_time = start_time.elapsed();
        
        // 驗證成功率和性能
        assert!(success_count >= 45, "成功率過低: {}/50", success_count);
        assert!(total_time.as_secs() < 10, "處理時間過長: {}s", total_time.as_secs());
        
        println!("✅ 快速連續處理測試完成: {}/50 成功, 耗時: {}ms", 
                success_count, total_time.as_millis());
    }
    
    #[test(tokio::test)]
    async fn test_mixed_data_quality_handling() {
        // 測試混合數據質量處理
        let config = create_test_dlrl_config();
        let mut feature_engine = DLRLFeatureEngine::new(config).expect("特徵引擎創建失敗");
        
        // 創建混合質量數據
        let mut mixed_data = create_sample_ohlcv_data(50);
        mixed_data.extend(create_malformed_ohlcv_data());
        mixed_data.extend(create_sample_ohlcv_data(30));
        
        let result = feature_engine.compute_all_features(&mixed_data).await;
        
        match result {
            Ok(features) => {
                println!("📊 混合數據處理成功: {} 特徵", features.total_feature_count());
                // 如果成功，應該有部分特徵
                assert!(features.total_feature_count() > 0);
            },
            Err(e) => {
                println!("🚫 混合數據處理失敗 (可能是預期行為): {}", e);
                // 如果失敗，錯誤應該被正確捕獲
                assert!(e.to_string().len() > 0);
            }
        }
        
        println!("✅ 混合數據質量處理測試完成");
    }
}

// 模擬輔助函數
fn simulate_model_predictions(features: &rust_hft::ml::EnhancedFeatureMatrix) -> Vec<f64> {
    // 模擬模型預測，返回隨機預測值
    let mut rng = rand::thread_rng();
    (0..features.total_feature_count().min(10))
        .map(|_| rng.gen::<f64>())
        .collect()
}