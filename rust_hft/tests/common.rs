/*! 
 * 🧪 測試通用模組
 * 
 * 這個模組提供測試所需的通用函數和數據生成器
 */

use rust_hft::utils::parallel_processing::OHLCVData;
use rust_hft::ml::{DLRLFeatureConfig, ModelEngineConfig};
use std::collections::HashMap;
use rand::Rng;

/// 創建測試用的OHLCV數據樣本
pub fn create_sample_ohlcv_data(count: usize) -> Vec<OHLCVData> {
    let mut data = Vec::with_capacity(count);
    let mut rng = rand::thread_rng();
    let mut price = 50000.0;
    
    for i in 0..count {
        let volatility = 0.001;
        let change = (rng.gen::<f64>() - 0.5) * volatility * 2.0;
        price *= 1.0 + change;
        
        let high = price * (1.0 + volatility * rng.gen::<f64>());
        let low = price * (1.0 - volatility * rng.gen::<f64>());
        
        data.push(OHLCVData {
            timestamp: i as u64,
            open: price,
            high,
            low,
            close: price,
            volume: 100.0 + rng.gen::<f64>() * 900.0, // 100-1000 volume
        });
    }
    
    data
}

/// 創建測試用的真實市場數據（帶有更符合現實的價格波動）
pub fn create_realistic_market_data(count: usize) -> Vec<OHLCVData> {
    let mut data = Vec::with_capacity(count);
    let mut rng = rand::thread_rng();
    let mut price = 50000.0;
    let mut trend = 0.0;
    
    for i in 0..count {
        // 添加趨勢成分
        if i % 50 == 0 {
            trend = (rng.gen::<f64>() - 0.5) * 0.002; // 隨機趨勢
        }
        
        let volatility = 0.001;
        let noise = (rng.gen::<f64>() - 0.5) * volatility * 2.0;
        let change = trend + noise;
        
        price *= 1.0 + change;
        
        let spread = price * 0.0001; // 0.01% 點差
        let high = price + spread + rng.gen::<f64>() * spread;
        let low = price - spread - rng.gen::<f64>() * spread;
        
        data.push(OHLCVData {
            timestamp: i as u64,
            open: price,
            high,
            low,
            close: price,
            volume: 50.0 + rng.gen::<f64>() * 450.0, // 50-500 volume
        });
    }
    
    data
}

/// 創建測試用的DL/RL特徵配置
pub fn create_test_dlrl_config() -> DLRLFeatureConfig {
    DLRLFeatureConfig {
        lookback_window: 60,
        prediction_horizon: 5,
        feature_dimensions: 128,
        use_technical_indicators: true,
        use_microstructure: true,
        use_orderbook_features: true,
        use_time_features: true,
        parallel_processing: true,
        batch_size: 32,
        enable_caching: false, // 在測試中禁用緩存
        device: rust_hft::ml::Device::CPU,
        ..Default::default()
    }
}

/// 創建測試用的模型引擎配置
pub fn create_test_model_config() -> ModelEngineConfig {
    ModelEngineConfig {
        model_directory: std::env::temp_dir().to_string_lossy().to_string(),
        max_models: 5,
        inference_timeout_ms: 100,
        enable_model_caching: false, // 在測試中禁用緩存
        parallel_inference: false,   // 在測試中禁用並行推理
        device: rust_hft::ml::ModelDevice::CPU,
        ..Default::default()
    }
}

/// 創建測試用的異常數據
pub fn create_malformed_ohlcv_data() -> Vec<OHLCVData> {
    vec![
        OHLCVData {
            timestamp: 1,
            open: f64::NAN,
            high: f64::INFINITY,
            low: f64::NEG_INFINITY,
            close: 0.0,
            volume: -100.0,
        },
        OHLCVData {
            timestamp: 2,
            open: 1000000.0,
            high: 999999.0,  // high < open (錯誤)
            low: 1000001.0,  // low > open (錯誤)
            close: 50000.0,
            volume: 0.0,
        },
    ]
}

/// 創建測試用的空數據
pub fn create_empty_ohlcv_data() -> Vec<OHLCVData> {
    vec![]
}

/// 生成測試用的模型預測數據
pub fn generate_mock_predictions(count: usize) -> Vec<f64> {
    let mut rng = rand::thread_rng();
    (0..count)
        .map(|_| rng.gen::<f64>())
        .collect()
}

/// 生成測試用的交易信號
pub fn generate_mock_trading_signals(predictions: &[f64]) -> Vec<String> {
    predictions.iter()
        .enumerate()
        .filter_map(|(i, &pred)| {
            if pred > 0.7 {
                Some(format!("STRONG_BUY_{}", i))
            } else if pred > 0.6 {
                Some(format!("BUY_{}", i))
            } else if pred < 0.3 {
                Some(format!("STRONG_SELL_{}", i))
            } else if pred < 0.4 {
                Some(format!("SELL_{}", i))
            } else {
                None
            }
        })
        .collect()
}

/// 驗證OHLCV數據的有效性
pub fn validate_ohlcv_data(data: &[OHLCVData]) -> bool {
    data.iter().all(|d| {
        d.high >= d.open && d.high >= d.close && d.high >= d.low &&
        d.low <= d.open && d.low <= d.close &&
        d.volume >= 0.0 &&
        d.open > 0.0 && d.high > 0.0 && d.low > 0.0 && d.close > 0.0 &&
        d.open.is_finite() && d.high.is_finite() && d.low.is_finite() && d.close.is_finite()
    })
}

/// 計算OHLCV數據的基本統計信息
pub struct OHLCVStats {
    pub count: usize,
    pub avg_price: f64,
    pub volatility: f64,
    pub avg_volume: f64,
    pub price_range: (f64, f64),
}

pub fn calculate_ohlcv_stats(data: &[OHLCVData]) -> OHLCVStats {
    if data.is_empty() {
        return OHLCVStats {
            count: 0,
            avg_price: 0.0,
            volatility: 0.0,
            avg_volume: 0.0,
            price_range: (0.0, 0.0),
        };
    }
    
    let count = data.len();
    let avg_price = data.iter().map(|d| d.close).sum::<f64>() / count as f64;
    let avg_volume = data.iter().map(|d| d.volume).sum::<f64>() / count as f64;
    
    let variance = data.iter()
        .map(|d| (d.close - avg_price).powi(2))
        .sum::<f64>() / count as f64;
    let volatility = variance.sqrt();
    
    let min_price = data.iter().map(|d| d.low).fold(f64::INFINITY, f64::min);
    let max_price = data.iter().map(|d| d.high).fold(f64::NEG_INFINITY, f64::max);
    
    OHLCVStats {
        count,
        avg_price,
        volatility,
        avg_volume,
        price_range: (min_price, max_price),
    }
}

/// 測試輔助宏
#[macro_export]
macro_rules! assert_within_tolerance {
    ($actual:expr, $expected:expr, $tolerance:expr) => {
        assert!(
            ($actual - $expected).abs() <= $tolerance,
            "Expected {} to be within {} of {}, but was {}",
            $actual, $tolerance, $expected, $actual
        );
    };
}

/// 性能測試輔助函數
pub fn measure_execution_time<F, R>(f: F) -> (R, std::time::Duration)
where
    F: FnOnce() -> R,
{
    let start = std::time::Instant::now();
    let result = f();
    let duration = start.elapsed();
    (result, duration)
}

/// 異步性能測試輔助函數
pub async fn measure_async_execution_time<F, Fut, R>(f: F) -> (R, std::time::Duration)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = R>,
{
    let start = std::time::Instant::now();
    let result = f().await;
    let duration = start.elapsed();
    (result, duration)
}

/// 測試環境設置
pub fn setup_test_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    
    let _ = fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("rust_hft=debug".parse().unwrap()))
        .with_test_writer()
        .try_init();
}

/// 測試數據目錄管理
pub struct TestDataDir {
    pub path: std::path::PathBuf,
    _temp_dir: tempfile::TempDir,
}

impl TestDataDir {
    pub fn new() -> Result<Self, std::io::Error> {
        let temp_dir = tempfile::tempdir()?;
        let path = temp_dir.path().to_path_buf();
        
        Ok(TestDataDir {
            path,
            _temp_dir: temp_dir,
        })
    }
    
    pub fn create_subdir(&self, name: &str) -> Result<std::path::PathBuf, std::io::Error> {
        let subdir = self.path.join(name);
        std::fs::create_dir_all(&subdir)?;
        Ok(subdir)
    }
    
    pub fn write_test_file(&self, name: &str, content: &[u8]) -> Result<std::path::PathBuf, std::io::Error> {
        let file_path = self.path.join(name);
        std::fs::write(&file_path, content)?;
        Ok(file_path)
    }
}

/// 創建測試用的模型元數據
pub fn create_test_model_metadata() -> HashMap<String, serde_json::Value> {
    let mut metadata = HashMap::new();
    metadata.insert("model_version".to_string(), serde_json::Value::String("1.0.0".to_string()));
    metadata.insert("training_date".to_string(), serde_json::Value::String("2024-01-01".to_string()));
    metadata.insert("accuracy".to_string(), serde_json::Value::Number(serde_json::Number::from_f64(0.85).unwrap()));
    metadata.insert("features".to_string(), serde_json::Value::Array(vec![
        serde_json::Value::String("price".to_string()),
        serde_json::Value::String("volume".to_string()),
        serde_json::Value::String("volatility".to_string()),
    ]));
    metadata
}

/// 並發測試輔助函數
pub async fn run_concurrent_tasks<F, Fut, R>(
    task_count: usize,
    task_factory: F,
) -> Vec<R>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let mut handles = Vec::new();
    
    for i in 0..task_count {
        let handle = tokio::spawn(task_factory(i));
        handles.push(handle);
    }
    
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    
    results
}