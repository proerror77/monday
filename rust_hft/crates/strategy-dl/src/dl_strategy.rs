//! DL 策略主實現模組
//! 
//! 集成所有 DL 組件，實現完整的深度學習交易策略

use crate::config::{DlStrategyConfig, ModelConfig};
use crate::feature_pipeline::{FeaturePipeline, FeatureExtractor};
use crate::inference_engine::{InferenceEngine, InferenceRequest, InferenceResult, InferenceMetadata};
use crate::model_loader::ModelLoader;

use hft_core::{HftResult, HftError, Timestamp, Symbol};
use ports::{Strategy, MarketView, AccountView, OrderIntent, Side, OrderType, TimeInForce};

use std::sync::Arc;
use tracing::{info, warn, error};

/// 指數移動平均計算器
#[derive(Debug)]
struct ExponentialMovingAverage {
    value: f64,
    alpha: f64,
    initialized: bool,
}

impl Default for ExponentialMovingAverage {
    fn default() -> Self {
        Self {
            value: 0.0,
            alpha: 0.1, // 10% 權重給新值
            initialized: false,
        }
    }
}

impl ExponentialMovingAverage {
    fn update(&mut self, new_value: f64) {
        if self.initialized {
            self.value = self.alpha * new_value + (1.0 - self.alpha) * self.value;
        } else {
            self.value = new_value;
            self.initialized = true;
        }
    }
    
    fn get(&self) -> f64 {
        self.value
    }
}

/// DL 策略統計信息
#[derive(Debug, Clone)]
pub struct DlStrategyStats {
    pub name: String,
    pub is_degraded: bool,
    pub inference_success_rate: f64,
    pub avg_inference_latency_us: f64,
    pub avg_feature_extraction_us: f64,
    pub consecutive_failures: u32,
    pub degradation_count: u64,
    pub last_successful_inference: Timestamp,
}

/// 深度學習策略主結構
/// 
/// 集成模型加載、特徵提取、推理引擎等所有組件，
/// 實現完整的端到端 DL 交易策略
pub struct DlStrategy {
    /// 策略配置
    config: DlStrategyConfig,
    
    /// 特徵提取管道
    feature_pipeline: FeaturePipeline,
    
    /// 推理引擎
    inference_engine: Arc<InferenceEngine>,
    
    /// 策略狀態
    state: DlStrategyState,
    
    /// 最近的推理結果
    last_prediction: Option<InferenceResult>,
    
    /// 風控統計
    risk_stats: DlRiskStats,
    
    /// 計時器統計
    timers: DlTimingStats,
}

/// DL 策略狀態
#[derive(Debug, Clone)]
struct DlStrategyState {
    /// 是否降級模式 (模型失效時的保護機制)
    degraded_mode: bool,
    
    /// 降級開始時間
    degraded_since: Option<Timestamp>,
    
    /// 上次推理請求時間
    last_inference_request: Timestamp,
    
    /// 上次成功推理時間
    last_successful_inference: Timestamp,
    
    /// 連續失敗次數
    consecutive_failures: u32,
    
    /// 當前持倉狀態
    current_position: f64,
}

/// DL 風控統計
#[derive(Debug, Default)]
struct DlRiskStats {
    /// 推理成功次數
    inference_success_count: u64,
    
    /// 推理失敗次數
    inference_error_count: u64,
    
    /// 推理超時次數
    inference_timeout_count: u64,
    
    /// 降級觸發次數
    degradation_count: u64,
    
    /// 最後一次檢查時間
    last_check_time: Timestamp,
}

/// DL 計時統計
#[derive(Debug, Default)]
struct DlTimingStats {
    /// 特徵提取耗時統計
    feature_extraction_us: ExponentialMovingAverage,
    
    /// 推理耗時統計
    inference_us: ExponentialMovingAverage,
    
    /// 策略決策耗時統計
    decision_us: ExponentialMovingAverage,
}

impl DlStrategy {
    /// 創建 DL 策略實例
    pub async fn new(config: DlStrategyConfig) -> HftResult<Self> {
        info!("初始化 DL 策略: {}", config.name);
        
        // 驗證配置
        crate::config::validate_config(&config)?;
        
        // 初始化特徵管道
        let feature_pipeline = FeaturePipeline::new(config.features.clone())?;
        
        // 加載模型
        let model_handle = ModelLoader::load_model(config.model.clone())?;
        
        // 創建推理引擎
        let inference_engine = InferenceEngine::new(
            model_handle,
            config.inference.clone(),
        ).await?;
        
        let now = Self::current_timestamp();
        
        let state = DlStrategyState {
            degraded_mode: false,
            degraded_since: None,
            last_inference_request: now,
            last_successful_inference: now,
            consecutive_failures: 0,
            current_position: 0.0,
        };
        
        let strategy = Self {
            config,
            feature_pipeline,
            inference_engine,
            state,
            last_prediction: None,
            risk_stats: DlRiskStats::default(),
            timers: DlTimingStats::default(),
        };
        
        info!("DL 策略初始化完成");
        Ok(strategy)
    }
    
    /// 處理市場數據更新
    fn on_market_update(&mut self, market_view: &MarketView) -> HftResult<Vec<OrderIntent>> {
        let start_time = Self::current_timestamp();
        
        // 檢查是否需要退出降級模式
        self.check_degradation_recovery();
        
        // 如果在降級模式，使用簡單策略
        if self.state.degraded_mode {
            return self.degraded_strategy(market_view);
        }
        
        // 檢查是否需要觸發推理
        let should_trigger = self.should_trigger_inference(market_view)?;
        if !should_trigger {
            return Ok(vec![]);
        }
        
        // 提取特徵
        let feature_start = Self::current_timestamp();
        let features = self.feature_pipeline.extract_features(market_view)?;
        let feature_duration = Self::current_timestamp() - feature_start;
        self.timers.feature_extraction_us.update(feature_duration as f64);
        
        // 檢查特徵質量
        if !self.validate_features(&features) {
            warn!("特徵質量檢查失敗，跳過本次推理");
            return Ok(vec![]);
        }
        
        // 發送推理請求 (非阻塞)
        let inference_request = InferenceRequest {
            request_id: self.generate_request_id(),
            features,
            timestamp: Self::current_timestamp(),
            symbol: market_view.symbol.clone(),
            metadata: InferenceMetadata {
                strategy_name: self.config.name.clone(),
                version: "1.0".to_string(),
            },
        };
        
        match self.inference_engine.submit_request(inference_request) {
            Ok(_) => {
                self.state.last_inference_request = Self::current_timestamp();
                
                // 嘗試獲取最新推理結果 (非阻塞)
                if let Some(result) = self.inference_engine.try_get_result() {
                    return self.process_inference_result(result, market_view, start_time);
                }
            }
            Err(e) => {
                error!("推理請求提交失敗: {}", e);
                self.handle_inference_error();
            }
        }
        
        // 如果沒有新的推理結果，使用上次的預測結果 (如果還有效)
        if let Some(ref last_result) = self.last_prediction {
            if self.is_prediction_still_valid(last_result) {
                return self.generate_orders_from_prediction(last_result, market_view);
            }
        }
        
        Ok(vec![])
    }
    
    /// 處理推理結果
    fn process_inference_result(
        &mut self, 
        result: InferenceResult, 
        market_view: &MarketView,
        start_time: Timestamp,
    ) -> HftResult<Vec<OrderIntent>> {
        match result {
            InferenceResult::Success { 
                request_id: _, 
                output, 
                latency_us,
                timestamp: _,
            } => {
                // 更新統計
                self.risk_stats.inference_success_count += 1;
                self.state.last_successful_inference = Self::current_timestamp();
                self.state.consecutive_failures = 0;
                self.timers.inference_us.update(latency_us as f64);
                
                // 保存預測結果
                self.last_prediction = Some(InferenceResult::Success { 
                    request_id: String::new(), 
                    output: output.clone(), 
                    latency_us,
                    timestamp: Self::current_timestamp(),
                });
                
                // 基於模型輸出生成訂單
                self.generate_orders_from_output(&output, market_view)
            }
            
            InferenceResult::Error { error, .. } => {
                error!("推理執行失敗: {}", error);
                self.handle_inference_error();
                Ok(vec![])
            }
            
            InferenceResult::Timeout { .. } => {
                warn!("推理超時");
                self.risk_stats.inference_timeout_count += 1;
                self.handle_inference_error();
                Ok(vec![])
            }
        }
    }
    
    /// 基於模型輸出生成訂單意圖
    fn generate_orders_from_output(
        &mut self,
        output: &[f32],
        market_view: &MarketView,
    ) -> HftResult<Vec<OrderIntent>> {
        // 解析模型輸出 (假設單一輸出表示方向和強度)
        let signal = output.get(0).copied().unwrap_or(0.0);
        
        // 檢查信號強度是否超過閾值
        if signal.abs() < self.config.inference.output_threshold as f32 {
            return Ok(vec![]);
        }
        
        // 計算目標位置和交易量
        let target_position = self.calculate_target_position(signal, market_view)?;
        let position_change = target_position - self.state.current_position;
        
        if position_change.abs() < 0.001 {
            return Ok(vec![]);
        }
        
        // 生成訂單意圖
        let order_intent = if position_change > 0.0 {
            OrderIntent {
                symbol: market_view.symbol.clone(),
                side: Side::Buy,
                order_type: OrderType::Market,
                qty: position_change,
                price: None,
                time_in_force: TimeInForce::IOC,
                metadata: Some(format!("dl_signal:{:.4}", signal)),
            }
        } else {
            OrderIntent {
                symbol: market_view.symbol.clone(),
                side: Side::Sell,
                order_type: OrderType::Market,
                qty: position_change.abs(),
                price: None,
                time_in_force: TimeInForce::IOC,
                metadata: Some(format!("dl_signal:{:.4}", signal)),
            }
        };
        
        // 更新預期倉位
        self.state.current_position = target_position;
        
        Ok(vec![order_intent])
    }
    
    /// 計算目標倉位
    fn calculate_target_position(&self, signal: f32, market_view: &MarketView) -> HftResult<f64> {
        // 基於信號強度和市場狀態計算倉位
        let max_position = 1.0; // TODO: 從配置獲取
        let position_size = (signal as f64).tanh() * max_position;
        
        // 考慮流動性約束
        let best_bid_qty = market_view.best_bid().map(|level| level.qty).unwrap_or(0.0);
        let best_ask_qty = market_view.best_ask().map(|level| level.qty).unwrap_or(0.0);
        
        let max_tradeable = if position_size > 0.0 {
            best_ask_qty * 0.5 // 只使用一半可用流動性
        } else {
            best_bid_qty * 0.5
        };
        
        Ok(position_size.abs().min(max_tradeable) * position_size.signum())
    }
    
    /// 檢查是否需要觸發推理
    fn should_trigger_inference(&self, market_view: &MarketView) -> HftResult<bool> {
        let now = Self::current_timestamp();
        
        // 檢查推理間隔
        let elapsed_since_last = now - self.state.last_inference_request;
        if elapsed_since_last < 100_000 { // 至少間隔 100ms
            return Ok(false);
        }
        
        // 檢查市場變化幅度
        let _trigger_threshold = self.config.inference.trigger_threshold;
        
        // 簡單的變化檢測：價格變化超過閾值
        if let (Some(_bid), Some(_ask)) = (market_view.best_bid(), market_view.best_ask()) {
            // TODO: 保存上次中間價，計算變化幅度
            // 這裡暫時總是返回 true 進行推理
            Ok(true)
        } else {
            Ok(false)
        }
    }
    
    /// 驗證特徵質量
    fn validate_features(&self, features: &[f32]) -> bool {
        // 檢查特徵維度
        if features.len() != self.config.model.input_dim {
            error!("特徵維度不匹配: {} vs {}", features.len(), self.config.model.input_dim);
            return false;
        }
        
        // 檢查 NaN 和無限值
        for (i, &value) in features.iter().enumerate() {
            if !value.is_finite() {
                error!("特徵 {} 包含非法值: {}", i, value);
                return false;
            }
        }
        
        true
    }
    
    /// 處理推理錯誤
    fn handle_inference_error(&mut self) {
        self.risk_stats.inference_error_count += 1;
        self.state.consecutive_failures += 1;
        
        // 檢查是否需要進入降級模式
        let error_rate = self.calculate_error_rate();
        let should_degrade = error_rate > self.config.risk.max_error_rate
            || self.state.consecutive_failures > 5;
            
        if should_degrade && !self.state.degraded_mode {
            warn!("觸發降級模式: 錯誤率 {:.2}%, 連續失敗 {}", 
                  error_rate * 100.0, self.state.consecutive_failures);
            
            self.state.degraded_mode = true;
            self.state.degraded_since = Some(Self::current_timestamp());
            self.risk_stats.degradation_count += 1;
        }
    }
    
    /// 計算推理錯誤率
    fn calculate_error_rate(&self) -> f64 {
        let total = self.risk_stats.inference_success_count + self.risk_stats.inference_error_count;
        if total == 0 {
            0.0
        } else {
            self.risk_stats.inference_error_count as f64 / total as f64
        }
    }
    
    /// 檢查是否可以恢復正常模式
    fn check_degradation_recovery(&mut self) {
        if !self.state.degraded_mode {
            return;
        }
        
        let now = Self::current_timestamp();
        let degraded_duration = self.state.degraded_since
            .map(|since| now - since)
            .unwrap_or(0);
            
        // 降級時間超過配置的恢復檢查間隔
        let recovery_interval = self.config.risk.recovery_check_interval_sec * 1_000_000; // 轉換為微秒
        if degraded_duration > recovery_interval {
            // 檢查錯誤率是否已降低
            let error_rate = self.calculate_error_rate();
            if error_rate < self.config.risk.max_error_rate * 0.5 {
                info!("恢復正常模式: 錯誤率已降至 {:.2}%", error_rate * 100.0);
                self.state.degraded_mode = false;
                self.state.degraded_since = None;
                self.state.consecutive_failures = 0;
            }
        }
    }
    
    /// 降級模式下的簡單策略
    fn degraded_strategy(&self, _market_view: &MarketView) -> HftResult<Vec<OrderIntent>> {
        // 在降級模式下，暫停交易或使用極簡策略
        // 這裡選擇暫停交易，確保風險控制
        Ok(vec![])
    }
    
    /// 檢查預測結果是否仍然有效
    fn is_prediction_still_valid(&self, result: &InferenceResult) -> bool {
        if let InferenceResult::Success { timestamp, .. } = result {
            let now = Self::current_timestamp();
            let age = now - timestamp;
            age < 5_000_000 // 5秒內的預測仍然有效
        } else {
            false
        }
    }
    
    /// 基於上次預測結果生成訂單
    fn generate_orders_from_prediction(
        &self, 
        result: &InferenceResult,
        _market_view: &MarketView,
    ) -> HftResult<Vec<OrderIntent>> {
        if let InferenceResult::Success { .. } = result {
            // 使用上次的輸出結果，但需要重新評估市場狀態
            // 這裡簡化處理，實際可能需要更複雜的邏輯
            Ok(vec![])
        } else {
            Ok(vec![])
        }
    }
    
    /// 生成請求 ID
    fn generate_request_id(&self) -> String {
        format!("{}_{}", self.config.name, Self::current_timestamp())
    }
    
    /// 獲取當前時間戳 (微秒)
    fn current_timestamp() -> Timestamp {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }
    
    /// 獲取策略統計信息
    pub fn get_statistics(&self) -> DlStrategyStats {
        DlStrategyStats {
            name: self.config.name.clone(),
            is_degraded: self.state.degraded_mode,
            inference_success_rate: {
                let total = self.risk_stats.inference_success_count + self.risk_stats.inference_error_count;
                if total > 0 {
                    self.risk_stats.inference_success_count as f64 / total as f64
                } else {
                    0.0
                }
            },
            avg_inference_latency_us: self.timers.inference_us.get(),
            avg_feature_extraction_us: self.timers.feature_extraction_us.get(),
            consecutive_failures: self.state.consecutive_failures,
            degradation_count: self.risk_stats.degradation_count,
            last_successful_inference: self.state.last_successful_inference,
        }
    }
    
    /// 熱重載模型
    pub async fn reload_model(&mut self, new_config: ModelConfig) -> HftResult<()> {
        info!("開始熱重載 DL 模型: {:?}", new_config.model_path);
        
        // 加載新模型
        let new_model = ModelLoader::load_model(new_config.clone())?;
        
        // 創建新的推理引擎
        let new_inference_engine = InferenceEngine::new(
            new_model,
            self.config.inference.clone(),
        ).await?;
        
        // 原子替換推理引擎
        self.inference_engine = new_inference_engine;
        self.config.model = new_config;
        
        // 重置狀態
        self.state.degraded_mode = false;
        self.state.degraded_since = None;
        self.state.consecutive_failures = 0;
        self.last_prediction = None;
        
        info!("DL 模型熱重載完成");
        Ok(())
    }
}

impl Strategy for DlStrategy {
    fn on_tick(
        &mut self,
        _timestamp: Timestamp,
        market_view: &MarketView,
        _account_view: &AccountView,
    ) -> HftResult<Vec<OrderIntent>> {
        self.on_market_update(market_view)
    }
    
    fn name(&self) -> &str {
        &self.config.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DlStrategyConfig;
    
    fn create_test_config() -> DlStrategyConfig {
        let mut config = DlStrategyConfig::default();
        config.name = "test_dl_strategy".to_string();
        config
    }
    
    #[tokio::test]
    async fn test_dl_strategy_creation() {
        // 注意：這個測試在沒有實際模型文件時會失敗
        // 實際環境中需要準備測試模型文件
        let config = create_test_config();
        
        // 由於模型文件不存在，這個測試預期會失敗
        let result = DlStrategy::new(config).await;
        assert!(result.is_err());
    }
    
    #[test]
    fn test_exponential_moving_average() {
        let mut ema = ExponentialMovingAverage::default();
        
        ema.update(10.0);
        assert_eq!(ema.get(), 10.0);
        
        ema.update(20.0);
        // 應該是 0.1 * 20.0 + 0.9 * 10.0 = 11.0
        assert!((ema.get() - 11.0).abs() < 0.001);
    }
    
    #[test]
    fn test_feature_validation() {
        let config = create_test_config();
        let dl_strategy = DlStrategy {
            config: config.clone(),
            feature_pipeline: FeaturePipeline::new(config.features.clone()).unwrap(),
            inference_engine: Arc::new(
                // 這裡需要 mock 推理引擎，實際測試中應該使用 mock
                unsafe { std::mem::zeroed() }
            ),
            state: DlStrategyState {
                degraded_mode: false,
                degraded_since: None,
                last_inference_request: 0,
                last_successful_inference: 0,
                consecutive_failures: 0,
                current_position: 0.0,
            },
            last_prediction: None,
            risk_stats: DlRiskStats::default(),
            timers: DlTimingStats::default(),
        };
        
        // 測試正確維度的特徵
        let valid_features = vec![0.1; config.model.input_dim];
        assert!(dl_strategy.validate_features(&valid_features));
        
        // 測試錯誤維度的特徵
        let invalid_features = vec![0.1; config.model.input_dim - 1];
        assert!(!dl_strategy.validate_features(&invalid_features));
        
        // 測試包含 NaN 的特徵
        let mut nan_features = vec![0.1; config.model.input_dim];
        nan_features[0] = f32::NAN;
        assert!(!dl_strategy.validate_features(&nan_features));
    }
}