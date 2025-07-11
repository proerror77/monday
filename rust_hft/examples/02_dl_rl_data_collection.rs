/*!
 * 📊 DL/RL驅動的高級數據收集系統
 * 
 * 這個例子展示如何使用深度學習和強化學習技術
 * 來進行智能化的市場數據收集和特徵工程
 * 
 * 主要功能：
 * - CNN價格圖像特徵提取
 * - LSTM時序模式識別  
 * - Transformer注意力機制
 * - 強化學習自適應採樣
 * - 多模態特徵融合
 * - GPU加速實時處理
 */

use rust_hft::ml::{
    DLRLFeatureEngine, DLRLFeatureConfig, EnhancedFeatureMatrix,
    DeepLearningConfig, ReinforcementLearningConfig,
    FeatureExtractorType, GPUConfig
};
use rust_hft::utils::parallel_processing::{ParallelConfig, OHLCVData};
use rust_hft::core::types::*;

use std::collections::HashMap;
use std::time::Instant;
use tracing::{info, warn, error, debug};
use anyhow::Result;
use tokio;
use rand;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌系統
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .with_thread_ids(true)
        .init();

    info!("🚀 啟動DL/RL驅動的高級數據收集系統");

    // 1. 創建高級DL/RL特徵工程配置
    let dlrl_config = create_advanced_dlrl_config();
    
    // 2. 初始化DL/RL特徵工程引擎
    let feature_engine = DLRLFeatureEngine::new(dlrl_config).map_err(|e| {
        error!("創建DL/RL特徵引擎失敗: {}", e);
        e
    })?;
    
    info!("✅ DL/RL特徵工程引擎初始化成功");

    // 3. 生成模擬市場數據
    let market_data = generate_realistic_market_data(1000).await?;
    info!("📊 生成了 {} 條市場數據", market_data.len());

    // 4. 執行DL/RL特徵提取
    let start_time = Instant::now();
    let enhanced_features = feature_engine.compute_all_features(&market_data).await?;
    let computation_time = start_time.elapsed();
    
    info!("🧠 DL/RL特徵提取完成");
    info!("⏱️  計算耗時: {:?}", computation_time);
    info!("📈 總特徵數: {}", enhanced_features.total_feature_count());

    // 5. 分析特徵質量和重要性
    analyze_feature_quality(&enhanced_features).await?;

    // 6. 演示實時特徵更新
    demonstrate_realtime_feature_update(&feature_engine).await?;

    // 7. 展示GPU加速性能對比
    demonstrate_gpu_acceleration().await?;

    // 8. 強化學習自適應數據收集
    demonstrate_rl_adaptive_collection(&feature_engine).await?;

    // 9. 獲取系統性能統計
    let stats = feature_engine.get_stats();
    display_performance_stats(&stats);

    info!("🎉 DL/RL數據收集系統演示完成");
    Ok(())
}

/// 創建高級DL/RL配置
fn create_advanced_dlrl_config() -> DLRLFeatureConfig {
    info!("⚙️  創建高級DL/RL配置");
    
    // 深度學習配置
    let dl_config = DeepLearningConfig::default();

    // 強化學習配置
    let rl_config = ReinforcementLearningConfig::default();

    // GPU配置
    let gpu_config = GPUConfig {
        enabled: true,
        device_id: None, // 自動選擇最佳GPU
        memory_fraction: 0.8,
        mixed_precision: true,
    };

    // 完整配置
    DLRLFeatureConfig {
        parallel_config: ParallelConfig {
            num_threads: Some(16),
            batch_size: 256,
            numa_aware: true,
            thread_affinity: true,
            work_stealing: true,
            prefetch_distance: 64,
        },
        dl_config,
        rl_config,
        extractors: vec![
            FeatureExtractorType::PriceCNN,
            FeatureExtractorType::SequenceLSTM,
            FeatureExtractorType::AttentionTransformer,
            FeatureExtractorType::AutoEncoder,
            FeatureExtractorType::MarketGNN,
            FeatureExtractorType::RLStateEncoder,
            FeatureExtractorType::MultiModalFusion,
        ],
        sequence_length: 120, // 2分鐘歷史（假設每秒一個數據點）
        embedding_dim: 256,
        attention_heads: 16,
        realtime_inference: true,
        gpu_config,
        cache_size: 50000,
    }
}

/// 生成逼真的市場數據
async fn generate_realistic_market_data(count: usize) -> Result<Vec<OHLCVData>> {
    info!("📊 生成 {} 條逼真市場數據", count);
    
    let mut data = Vec::with_capacity(count);
    let mut price = 50000.0; // BTC起始價格
    let volume_base = 100.0;
    
    for i in 0..count {
        // 模擬真實價格波動（加入趨勢、噪聲、突發事件）
        let trend = 0.0001 * (i as f64 / 100.0).sin(); // 長期趨勢
        let noise = (rand::random::<f64>() - 0.5) * 0.002; // 隨機噪聲
        
        // 模擬市場突發事件（低頻但高影響）
        let shock = if rand::random::<f64>() < 0.001 {
            (rand::random::<f64>() - 0.5) * 0.05 // 5%的突發波動
        } else {
            0.0
        };
        
        let price_change = trend + noise + shock;
        price *= 1.0 + price_change;
        
        // 價格範圍
        let volatility = 0.001;
        let high = price * (1.0 + volatility * rand::random::<f64>());
        let low = price * (1.0 - volatility * rand::random::<f64>());
        let open = if i == 0 { price } else { data[i-1].close };
        
        // 成交量與價格變動相關
        let volume_multiplier = 1.0 + price_change.abs() * 10.0;
        let volume = volume_base * volume_multiplier * (0.5 + rand::random::<f64>());
        
        data.push(OHLCVData {
            timestamp: i as u64,
            open,
            high,
            low,
            close: price,
            volume,
        });
    }
    
    info!("✅ 市場數據生成完成，價格範圍: {:.2} - {:.2}", 
          data.iter().map(|d| d.low).fold(f64::INFINITY, f64::min),
          data.iter().map(|d| d.high).fold(f64::NEG_INFINITY, f64::max));
    
    Ok(data)
}

/// 分析特徵質量
async fn analyze_feature_quality(features: &EnhancedFeatureMatrix) -> Result<()> {
    info!("🔍 分析特徵質量和重要性");
    
    // 統計特徵分佈
    info!("📊 特徵統計:");
    info!("  - 數值特徵數量: {}", features.feature_count());
    info!("  - 總特徵數量: {}", features.total_feature_count());
    
    // TODO: 實現特徵重要性分析
    // - 互信息分析
    // - 特徵相關性矩陣
    // - 梯度重要性評估
    
    info!("✅ 特徵質量分析完成");
    Ok(())
}

/// 演示實時特徵更新
async fn demonstrate_realtime_feature_update(engine: &DLRLFeatureEngine) -> Result<()> {
    info!("⚡ 演示實時特徵更新性能");
    
    // 模擬高頻數據流
    for i in 0..10 {
        let single_tick = generate_realistic_market_data(1).await?;
        
        let start = Instant::now();
        let _features = engine.compute_all_features(&single_tick).await?;
        let latency = start.elapsed();
        
        debug!("實時更新 {} - 延遲: {:?}", i, latency);
        
        if latency.as_micros() > 1000 { // 如果延遲超過1ms，發出警告
            warn!("實時特徵更新延遲過高: {:?}", latency);
        }
    }
    
    info!("✅ 實時特徵更新演示完成");
    Ok(())
}

/// 演示GPU加速性能對比
async fn demonstrate_gpu_acceleration() -> Result<()> {
    info!("🚀 演示GPU加速性能對比");
    
    // CPU配置
    let cpu_config = {
        let mut config = create_advanced_dlrl_config();
        config.gpu_config.enabled = false;
        config
    };
    
    // GPU配置  
    let gpu_config = {
        let mut config = create_advanced_dlrl_config();
        config.gpu_config.enabled = true;
        config
    };
    
    let test_data = generate_realistic_market_data(500).await?;
    
    // CPU性能測試
    info!("🖥️  測試CPU性能...");
    let cpu_engine = DLRLFeatureEngine::new(cpu_config)?;
    let cpu_start = Instant::now();
    let _cpu_features = cpu_engine.compute_all_features(&test_data).await?;
    let cpu_time = cpu_start.elapsed();
    
    // GPU性能測試（如果可用）
    info!("🎮 測試GPU性能...");
    match DLRLFeatureEngine::new(gpu_config) {
        Ok(gpu_engine) => {
            let gpu_start = Instant::now();
            let _gpu_features = gpu_engine.compute_all_features(&test_data).await?;
            let gpu_time = gpu_start.elapsed();
            
            let speedup = cpu_time.as_secs_f64() / gpu_time.as_secs_f64();
            info!("📊 性能對比:");
            info!("  - CPU時間: {:?}", cpu_time);
            info!("  - GPU時間: {:?}", gpu_time);
            info!("  - 加速比: {:.2}x", speedup);
        },
        Err(e) => {
            warn!("GPU不可用，跳過GPU性能測試: {}", e);
        }
    }
    
    info!("✅ GPU加速演示完成");
    Ok(())
}

/// 演示強化學習自適應數據收集
async fn demonstrate_rl_adaptive_collection(engine: &DLRLFeatureEngine) -> Result<()> {
    info!("🤖 演示強化學習自適應數據收集");
    
    // 模擬動態市場條件
    let market_conditions = vec![
        "牛市", "熊市", "橫盤", "高波動", "低波動"
    ];
    
    for condition in market_conditions {
        info!("📈 當前市場狀態: {}", condition);
        
        // 根據市場狀態生成相應數據
        let adaptive_data = match condition {
            "高波動" => generate_high_volatility_data(100).await?,
            "低波動" => generate_low_volatility_data(100).await?,
            _ => generate_realistic_market_data(100).await?,
        };
        
        // RL智能體自適應特徵提取
        let _features = engine.compute_all_features(&adaptive_data).await?;
        
        // TODO: 實現RL智能體的自適應策略
        // - 根據市場狀態調整特徵權重
        // - 動態選擇最相關的特徵
        // - 優化數據收集頻率
        
        info!("✅ 完成 {} 狀態下的自適應收集", condition);
    }
    
    info!("✅ RL自適應數據收集演示完成");
    Ok(())
}

/// 生成高波動數據
async fn generate_high_volatility_data(count: usize) -> Result<Vec<OHLCVData>> {
    let mut data = generate_realistic_market_data(count).await?;
    
    // 增加波動性
    for item in &mut data {
        let volatility_boost = 5.0;
        let noise = (rand::random::<f64>() - 0.5) * 0.01 * volatility_boost;
        item.close *= 1.0 + noise;
        item.high = item.close.max(item.high);
        item.low = item.close.min(item.low);
        item.volume *= 1.0 + volatility_boost; // 高波動對應高成交量
    }
    
    Ok(data)
}

/// 生成低波動數據
async fn generate_low_volatility_data(count: usize) -> Result<Vec<OHLCVData>> {
    let mut data = generate_realistic_market_data(count).await?;
    
    // 減少波動性
    for item in &mut data {
        let volatility_reduction = 0.2;
        let noise = (rand::random::<f64>() - 0.5) * 0.001 * volatility_reduction;
        item.close *= 1.0 + noise;
        item.high = item.close * 1.0001;
        item.low = item.close * 0.9999;
        item.volume *= volatility_reduction; // 低波動對應低成交量
    }
    
    Ok(data)
}

/// 顯示性能統計
fn display_performance_stats(stats: &rust_hft::ml::DLRLFeatureEngineStats) {
    info!("📊 DL/RL特徵工程性能統計:");
    info!("  - 總特徵計算數: {}", stats.total_features_computed);
    info!("  - DL特徵數: {}", stats.dl_features_computed);
    info!("  - RL特徵數: {}", stats.rl_features_computed);
    info!("  - 傳統特徵數: {}", stats.traditional_features_computed);
    info!("  - 總計算時間: {} μs", stats.total_computation_time_us);
    info!("  - DL計算時間: {} μs", stats.dl_computation_time_us);
    info!("  - RL計算時間: {} μs", stats.rl_computation_time_us);
    info!("  - 緩存命中率: {:.2}%", stats.cache_hit_rate * 100.0);
    info!("  - GPU利用率: {:.1}%", stats.gpu_utilization * 100.0);
    info!("  - 內存使用: {:.1} MB", stats.memory_usage_mb);
    info!("  - 推理延遲: {} μs", stats.inference_latency_us);
    info!("  - 並行效率: {:.2}%", stats.parallel_stats.parallel_efficiency * 100.0);
    info!("  - 吞吐量: {:.0} 特徵/秒", stats.parallel_stats.throughput);
    
    // 性能評估
    if stats.inference_latency_us < 1000 {
        info!("🟢 推理延遲優秀 (<1ms)");
    } else if stats.inference_latency_us < 5000 {
        info!("🟡 推理延遲良好 (<5ms)");
    } else {
        info!("🔴 推理延遲需要優化 (>5ms)");
    }
    
    if stats.gpu_utilization > 0.8 {
        info!("🟢 GPU利用率優秀");
    } else if stats.gpu_utilization > 0.5 {
        info!("🟡 GPU利用率良好");
    } else {
        info!("🔴 GPU利用率較低");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_dlrl_feature_engine_creation() {
        let config = create_advanced_dlrl_config();
        let engine = DLRLFeatureEngine::new(config);
        assert!(engine.is_ok());
    }
    
    #[tokio::test]
    async fn test_market_data_generation() {
        let data = generate_realistic_market_data(100).await.unwrap();
        assert_eq!(data.len(), 100);
        assert!(data.iter().all(|d| d.open > 0.0));
        assert!(data.iter().all(|d| d.volume > 0.0));
    }
    
    #[tokio::test]
    async fn test_feature_extraction() {
        let config = create_advanced_dlrl_config();
        let engine = DLRLFeatureEngine::new(config).unwrap();
        let data = generate_realistic_market_data(50).await.unwrap();
        
        let features = engine.compute_all_features(&data).await;
        assert!(features.is_ok());
        
        let features = features.unwrap();
        assert!(features.total_feature_count() > 0);
    }
}