/*!
 * 數據處理器 Python 綁定
 */

use pyo3::prelude::*;
use numpy::PyArray2;
use std::collections::HashMap;
use std::sync::Arc;
use redis;
use serde_json;
use crate::integrations::unified_bitget_connector::{UnifiedBitgetConnector, UnifiedBitgetConfig};
use crate::integrations::unified_orderbook_manager::{UnifiedOrderBookManager, create_high_performance_manager};
use tokio::sync::RwLock;
use tokio::runtime::Runtime;

/// 數據處理模式
#[derive(Debug, Clone)]
pub enum DataMode {
    Mock,      // 模擬數據（測試用）
    RealTime,  // 實時數據
    Historical // 歷史數據
}

/// 數據處理器
#[pyclass]
pub struct DataProcessor {
    buffer_size: usize,
    mode: DataMode,
    initialized: bool,
    // 使用現有的Rust組件！
    bitget_connector: Option<Arc<RwLock<UnifiedBitgetConnector>>>,
    orderbook_manager: Option<Arc<UnifiedOrderBookManager>>,
    runtime: Option<Arc<Runtime>>,
}

#[pymethods]
impl DataProcessor {
    #[new]
    #[pyo3(signature = (buffer_size=1000, mode="mock"))]
    pub fn new(buffer_size: usize, mode: &str) -> PyResult<Self> {
        let data_mode = match mode {
            "mock" => DataMode::Mock,
            "realtime" => DataMode::RealTime,
            "historical" => DataMode::Historical,
            _ => return Err(pyo3::exceptions::PyValueError::new_err("Invalid mode. Use 'mock', 'realtime', or 'historical'"))
        };
        
        Ok(Self {
            buffer_size,
            mode: data_mode,
            initialized: false,
            bitget_connector: None,
            orderbook_manager: None,
            runtime: None,
        })
    }
    
    /// 初始化真實數據連接 - 驅動現有Rust組件
    pub fn initialize_real_data(&mut self, symbol: String) -> PyResult<bool> {
        match self.mode {
            DataMode::Mock => {
                // 測試模式，不需要真實連接
                self.initialized = true;
                Ok(true)
            },
            DataMode::RealTime => {
                println!("🚀 初始化高性能 OrderBook 管理器...");
                
                // 使用現有的高性能 OrderBook 管理器
                let orderbook_manager = create_high_performance_manager();
                self.orderbook_manager = Some(Arc::new(orderbook_manager));
                
                // 創建Tokio運行時（用於異步操作）
                let runtime = Runtime::new()
                    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("創建運行時失敗: {}", e)))?;
                self.runtime = Some(Arc::new(runtime));
                
                self.initialized = true;
                println!("✅ OrderBook 管理器初始化成功");
                
                Ok(true)
            },
            DataMode::Historical => {
                // TODO: 使用現有的ClickHouse客戶端
                self.initialized = true;
                Ok(true)
            }
        }
    }
    
    /// 處理LOB數據
    pub fn process_lob_data<'py>(
        &self,
        py: Python<'py>,
        data: Vec<Vec<f64>>,
    ) -> PyResult<Bound<'py, PyArray2<f64>>> {
        // 將數據轉換為numpy數組
        if data.is_empty() {
            return Ok(PyArray2::zeros_bound(py, (0, 0), false));
        }
        
        // 創建numpy數組
        let array = PyArray2::from_vec2_bound(py, &data)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("Failed to create array: {}", e)))?;
        
        Ok(array)
    }
    
    /// 提取特徵
    pub fn extract_features(&self, symbol: String) -> PyResult<HashMap<String, f64>> {
        let mut features = HashMap::new();
        
        match &self.mode {
            DataMode::Mock => {
                // 模擬數據模式 - 用於測試
                features.insert("spread".to_string(), 0.001);
                features.insert("mid_price".to_string(), 45000.0);
                features.insert("volume".to_string(), 1000.0);
                features.insert("data_source".to_string(), 0.0); // 0 = mock
            },
            DataMode::RealTime => {
                // 實時數據模式
                if self.initialized {
                    // 嘗試提取真實特徵
                    match self.extract_real_features(&symbol) {
                        Ok(real_features) => {
                            features.extend(real_features);
                            // data_source 已經在 real_features 中設置了
                        },
                        Err(e) => {
                            println!("⚠️ OrderBook 特徵提取失敗: {}", e);
                            // 如果無法獲取真實數據，回退到模擬數據但標記為降級
                            features.insert("spread".to_string(), 0.5);
                            features.insert("mid_price".to_string(), 50000.0); // 更接近真實價格
                            features.insert("volume".to_string(), 100.0);
                            features.insert("data_source".to_string(), -1.0); // -1 = degraded
                            features.insert("status".to_string(), 0.5); // 降級狀態
                        }
                    }
                } else {
                    return Err(pyo3::exceptions::PyRuntimeError::new_err("Real-time mode not initialized. Call initialize_real_data() first."));
                }
            },
            DataMode::Historical => {
                // 歷史數據模式
                // TODO: 從ClickHouse獲取歷史數據
                features.insert("spread".to_string(), 0.0015);
                features.insert("mid_price".to_string(), 42000.0);
                features.insert("volume".to_string(), 1500.0);
                features.insert("data_source".to_string(), 2.0); // 2 = historical
            }
        }
        
        // 添加時間戳
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        features.insert("timestamp".to_string(), timestamp);
        
        Ok(features)
    }
    
    /// 從真實數據源提取特徵 - 驅動現有Rust組件
    fn extract_real_features(&self, symbol: &str) -> PyResult<HashMap<String, f64>> {
        let start_time = std::time::Instant::now();
        
        // 🛡️ 符號驗證
        if symbol.is_empty() || (!symbol.contains("USDT") && !symbol.contains("BTC")) {
            return Err(pyo3::exceptions::PyValueError::new_err("無效的交易對符號"));
        }
        
        // 🚀 Connect to Redis to get fast data from Rust processor
        let redis_client = match redis::Client::open("redis://127.0.0.1:6379/") {
            Ok(client) => client,
            Err(e) => {
                println!("❌ Redis 連接失敗: {}", e);
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!("Redis connection failed: {}", e)));
            }
        };
        
        let mut con = match redis_client.get_connection() {
            Ok(conn) => conn,
            Err(e) => {
                println!("❌ Redis 連接失敗: {}", e);
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!("Redis connection failed: {}", e)));
            }
        };
        
        let redis_key = format!("hft:orderbook:{}", symbol);
        
        // Get latest OrderBook data from Redis
        let redis_data: String = match redis::Commands::get(&mut con, &redis_key) {
            Ok(data) => data,
            Err(e) => {
                println!("⚠️ Redis 中沒有 {} 的數據: {}", symbol, e);
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!("No data in Redis for {}: {}", symbol, e)));
            }
        };
        
        // Parse JSON data from Rust processor
        let data: serde_json::Value = match serde_json::from_str(&redis_data) {
            Ok(parsed) => parsed,
            Err(e) => {
                println!("❌ JSON 解析失敗: {}", e);
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!("JSON parse error: {}", e)));
            }
        };
        
        let latency_ms = start_time.elapsed().as_micros() as f64 / 1000.0;
        
        println!("📡 从 Redis 获取快速 OrderBook 数据 - 符號: {}, 延遲: {:.3}ms", symbol, latency_ms);
        
        let mut features = HashMap::new();
        
        if let Some(mid_price) = data["mid_price"].as_f64() {
            features.insert("mid_price".to_string(), mid_price);
            features.insert("best_bid".to_string(), data["best_bid"].as_f64().unwrap_or(0.0));
            features.insert("best_ask".to_string(), data["best_ask"].as_f64().unwrap_or(0.0));
            features.insert("spread".to_string(), data["spread"].as_f64().unwrap_or(0.0));
            features.insert("spread_bps".to_string(), data["spread_bps"].as_f64().unwrap_or(0.0));
            features.insert("bid_levels".to_string(), data["bid_levels"].as_f64().unwrap_or(0.0));
            features.insert("ask_levels".to_string(), data["ask_levels"].as_f64().unwrap_or(0.0));
            features.insert("latency_ms".to_string(), latency_ms);
            features.insert("data_source".to_string(), 1.0); // 1 = 真實快速數據
            features.insert("timestamp".to_string(), data["timestamp_us"].as_f64().unwrap_or(0.0));
            features.insert("is_valid".to_string(), if data["is_valid"].as_bool().unwrap_or(false) { 1.0 } else { 0.0 });
            features.insert("data_quality_score".to_string(), data["data_quality_score"].as_f64().unwrap_or(0.0));
            features.insert("status".to_string(), 1.0); // 成功獲取真實數據
            
            println!("✅ 成功提取快速 OrderBook 特徵: 價格=${:.2}, 來源: {}", 
                     mid_price, data["source"].as_str().unwrap_or("unknown"));
        } else {
            println!("⚠️ Redis 中的 OrderBook 數據無效");
            return Err(pyo3::exceptions::PyRuntimeError::new_err("Invalid OrderBook data from Redis"));
        }
        
        Ok(features)
    }
}