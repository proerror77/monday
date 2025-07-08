/*!
 * Model Training Guide for Ultra-Think HFT System
 * 
 * 完整的深度學習模型訓練指南：
 * 1. 數據收集和預處理
 * 2. 特徵工程和標籤生成
 * 3. 模型架構設計
 * 4. 訓練流程和超參數優化
 * 5. 模型驗證和評估
 * 6. 生產部署
 */

use rust_hft::{
    integrations::bitget_connector::*,
    ml::{
        features::FeatureExtractor,
        lob_time_series_extractor::{LobTimeSeriesExtractor, LobTimeSeriesConfig},
        dl_trend_predictor::{DlTrendPredictor, DlTrendPredictorConfig, TrendClass},
    },
    core::{types::*, orderbook::OrderBook},
};
use anyhow::Result;
use tracing::{info, warn, error};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use tokio::time::Duration;
use serde_json::Value;
use std::time::Instant;
use serde::{Serialize, Deserialize};
use std::fs::File;
use std::io::{Write, BufWriter};
use candle_core::{Device, Tensor, DType};

/// 訓練數據收集器
pub struct TrainingDataCollector {
    /// LOB時間序列提取器
    lob_extractor: LobTimeSeriesExtractor,
    
    /// 特徵提取器
    feature_extractor: FeatureExtractor,
    
    /// 收集的訓練樣本
    training_samples: Arc<Mutex<Vec<TrainingSample>>>,
    
    /// 價格歷史用於標籤生成
    price_history: Arc<Mutex<VecDeque<PricePoint>>>,
    
    /// 收集統計
    collection_stats: Arc<Mutex<CollectionStats>>,
}

/// 訓練樣本
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingSample {
    /// 時間戳
    pub timestamp: u64,
    
    /// 輸入特徵 (76維)
    pub features: Vec<f64>,
    
    /// 未來價格變化標籤 (10秒後)
    pub label: TrendClass,
    
    /// 實際價格變化 (bps)
    pub actual_change_bps: f64,
    
    /// 當前價格
    pub current_price: f64,
    
    /// 未來價格
    pub future_price: f64,
    
    /// 數據質量分數
    pub quality_score: f64,
}

/// 價格點記錄
#[derive(Debug, Clone)]
pub struct PricePoint {
    pub timestamp: u64,
    pub price: f64,
}

/// 收集統計
#[derive(Debug, Default)]
pub struct CollectionStats {
    pub total_lob_updates: u64,
    pub feature_extractions: u64,
    pub training_samples_created: u64,
    pub labeled_samples: u64,
    pub class_distribution: [u64; 5], // 各類別數量
    pub avg_quality_score: f64,
}

impl TrainingDataCollector {
    pub fn new() -> Result<Self> {
        let lob_config = LobTimeSeriesConfig::default();
        let lob_extractor = LobTimeSeriesExtractor::new("BTCUSDT".to_string(), lob_config);
        let feature_extractor = FeatureExtractor::new(100);
        
        Ok(Self {
            lob_extractor,
            feature_extractor,
            training_samples: Arc::new(Mutex::new(Vec::new())),
            price_history: Arc::new(Mutex::new(VecDeque::with_capacity(1000))),
            collection_stats: Arc::new(Mutex::new(CollectionStats::default())),
        })
    }
    
    /// 處理LOB數據並生成訓練樣本
    pub fn process_lob_for_training(&mut self, symbol: &str, data: &Value, timestamp: u64) -> Result<()> {
        if symbol != "BTCUSDT" {
            return Ok(());
        }
        
        // 更新統計
        {
            let mut stats = self.collection_stats.lock().unwrap();
            stats.total_lob_updates += 1;
        }
        
        // 解析並創建OrderBook
        let orderbook = self.parse_orderbook(data, timestamp)?;
        
        // 記錄價格點
        let mid_price = self.calculate_mid_price(&orderbook);
        {
            let mut price_history = self.price_history.lock().unwrap();
            price_history.push_back(PricePoint {
                timestamp,
                price: mid_price,
            });
            
            // 保持最近1000個價格點
            if price_history.len() > 1000 {
                price_history.pop_front();
            }
        }
        
        // 嘗試特徵提取
        match self.feature_extractor.extract_features(&orderbook, 100, timestamp) {
            Ok(features) => {
                {
                    let mut stats = self.collection_stats.lock().unwrap();
                    stats.feature_extractions += 1;
                }
                
                // 嘗試創建訓練樣本
                if let Some(sample) = self.create_training_sample(&features, mid_price, timestamp)? {
                    self.add_training_sample(sample);
                }
            }
            Err(e) => {
                warn!("Feature extraction failed: {}", e);
            }
        }
        
        Ok(())
    }
    
    /// 創建訓練樣本
    fn create_training_sample(
        &self, 
        features: &FeatureSet, 
        current_price: f64, 
        timestamp: u64
    ) -> Result<Option<TrainingSample>> {
        // 檢查是否有10秒後的價格數據
        let future_timestamp = timestamp + 10_000_000; // 10秒後 (微秒)
        
        let future_price = {
            let price_history = self.price_history.lock().unwrap();
            
            // 找到最接近10秒後的價格
            price_history.iter()
                .find(|p| p.timestamp >= future_timestamp)
                .map(|p| p.price)
        };
        
        if let Some(future_price) = future_price {
            // 計算價格變化
            let price_change_bps = (future_price - current_price) / current_price * 10000.0;
            
            // 生成標籤
            let label = TrendClass::from_price_change_bps(price_change_bps);
            
            // 轉換特徵為向量
            let feature_vector = self.features_to_vector(features);
            
            // 計算數據質量分數
            let quality_score = self.calculate_quality_score(features);
            
            let sample = TrainingSample {
                timestamp,
                features: feature_vector,
                label,
                actual_change_bps: price_change_bps,
                current_price,
                future_price,
                quality_score,
            };
            
            return Ok(Some(sample));
        }
        
        Ok(None)
    }
    
    /// 添加訓練樣本
    fn add_training_sample(&self, sample: TrainingSample) {
        {
            let mut samples = self.training_samples.lock().unwrap();
            samples.push(sample.clone());
        }
        
        // 更新統計
        {
            let mut stats = self.collection_stats.lock().unwrap();
            stats.training_samples_created += 1;
            stats.labeled_samples += 1;
            stats.class_distribution[sample.label.to_label()] += 1;
            
            // 更新平均質量分數
            let total_samples = stats.labeled_samples as f64;
            stats.avg_quality_score = (stats.avg_quality_score * (total_samples - 1.0) + sample.quality_score) / total_samples;
        }
        
        info!("📊 Created training sample: {:?} | Change: {:.1}bps | Quality: {:.3}", 
              sample.label, sample.actual_change_bps, sample.quality_score);
    }
    
    /// 特徵轉換為向量
    fn features_to_vector(&self, features: &FeatureSet) -> Vec<f64> {
        vec![
            // 基礎特徵
            *features.mid_price,
            features.spread_bps,
            features.obi_l1, features.obi_l5, features.obi_l10, features.obi_l20,
            features.price_momentum,
            features.price_momentum,
            features.realized_volatility,
            features.vwap,
            
            // 微結構特徵
            features.microprice,
            features.effective_spread,
            features.realized_volatility,
            features.bid_slope, features.ask_slope,
            features.market_impact,
            features.liquidity_score,
            features.order_arrival_rate,
            
            // 訂單流特徵
            features.order_flow_imbalance,
            features.trade_intensity,
            features.volume_acceleration,
            features.depth_pressure_bid, features.depth_pressure_ask,
            features.cancellation_rate,
            features.trade_intensity,
            features.order_flow_imbalance,
            
            // 動量特徵
            features.momentum_5_tick, features.momentum_10_tick, features.momentum_20_tick,
            features.volatility_5_tick, features.volatility_10_tick, features.volatility_20_tick,
            
            // 深度特徵
            features.bid_depth_l5, features.ask_depth_l5,
            features.bid_depth_l10, features.ask_depth_l10,
            features.depth_imbalance_l5, features.depth_imbalance_l10,
            
            // 狀態指標
            features.volatility_5_tick,
            features.liquidity_score,
            features.momentum_5_tick,
            features.momentum_10_tick,
            features.momentum_20_tick,
            
            // 網路延遲
            features.latency_network_us as f64,
            
            // LOB張量特徵 (簡化為基礎特徵)
            features.bid_depth_l20, features.ask_depth_l20,
            features.depth_imbalance_l20,
            features.bid_slope, features.ask_slope,
            features.price_acceleration, features.volume_acceleration,
            features.bid_ask_correlation,
            features.data_quality_score,
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ]
    }
    
    /// 計算數據質量分數
    fn calculate_quality_score(&self, features: &FeatureSet) -> f64 {
        let mut score: f64 = 1.0;
        
        // 點差質量 (越小越好)
        if features.spread_bps > 10.0 {
            score *= 0.8;
        }
        
        // 流動性質量
        if features.liquidity_score < 0.5 {
            score *= 0.9;
        }
        
        // 波動性檢查
        if features.realized_volatility > 100.0 { // 過高波動
            score *= 0.7;
        }
        
        // 網路延遲檢查
        if features.latency_network_us > 1000 {
            score *= 0.8;
        }
        
        score.max(0.0_f64).min(1.0_f64)
    }
    
    /// 解析OrderBook
    fn parse_orderbook(&self, data: &Value, timestamp: u64) -> Result<OrderBook> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;

        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        
        // 解析bids
        if let Some(bid_data) = first_item.get("bids").and_then(|v| v.as_array()) {
            for bid in bid_data.iter().take(20) {
                if let Some(bid_array) = bid.as_array() {
                    if bid_array.len() >= 2 {
                        let price: f64 = bid_array[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        let qty: f64 = bid_array[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        if price > 0.0 && qty > 0.0 {
                            use ordered_float::OrderedFloat;
                            use rust_decimal::Decimal;
                            orderbook.bids.insert(OrderedFloat(price), Decimal::from_f64_retain(qty).unwrap_or_default());
                        }
                    }
                }
            }
        }
        
        // 解析asks
        if let Some(ask_data) = first_item.get("asks").and_then(|v| v.as_array()) {
            for ask in ask_data.iter().take(20) {
                if let Some(ask_array) = ask.as_array() {
                    if ask_array.len() >= 2 {
                        let price: f64 = ask_array[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        let qty: f64 = ask_array[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        if price > 0.0 && qty > 0.0 {
                            use ordered_float::OrderedFloat;
                            use rust_decimal::Decimal;
                            orderbook.asks.insert(OrderedFloat(price), Decimal::from_f64_retain(qty).unwrap_or_default());
                        }
                    }
                }
            }
        }
        
        orderbook.last_update = timestamp;
        
        if !orderbook.bids.is_empty() && !orderbook.asks.is_empty() {
            orderbook.is_valid = true;
            orderbook.data_quality_score = 1.0;
        }
        
        Ok(orderbook)
    }
    
    /// 計算中間價
    fn calculate_mid_price(&self, orderbook: &OrderBook) -> f64 {
        let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        (best_bid + best_ask) / 2.0
    }
    
    /// 保存訓練數據到文件
    pub fn save_training_data(&self, filename: &str) -> Result<()> {
        let samples = self.training_samples.lock().unwrap();
        let file = File::create(filename)?;
        let mut writer = BufWriter::new(file);
        
        // 寫入JSON格式
        let json_data = serde_json::to_string_pretty(&*samples)?;
        writer.write_all(json_data.as_bytes())?;
        
        info!("💾 Saved {} training samples to {}", samples.len(), filename);
        Ok(())
    }
    
    /// 獲取收集統計
    pub fn get_collection_stats(&self) -> CollectionStats {
        self.collection_stats.lock().unwrap().clone()
    }
    
    /// 獲取訓練樣本數量
    pub fn get_sample_count(&self) -> usize {
        self.training_samples.lock().unwrap().len()
    }
}

impl Clone for CollectionStats {
    fn clone(&self) -> Self {
        Self {
            total_lob_updates: self.total_lob_updates,
            feature_extractions: self.feature_extractions,
            training_samples_created: self.training_samples_created,
            labeled_samples: self.labeled_samples,
            class_distribution: self.class_distribution,
            avg_quality_score: self.avg_quality_score,
        }
    }
}

/// 模型訓練指南
pub struct ModelTrainingGuide;

impl ModelTrainingGuide {
    /// 顯示完整的訓練流程指南
    pub fn show_training_guide() {
        info!("🎓 Ultra-Think HFT Model Training Complete Guide");
        info!("================================================");
        info!("");
        
        info!("📚 Step 1: Data Collection (目前運行中)");
        info!("   • 實時收集Bitget BTCUSDT LOB數據");
        info!("   • 提取76維特徵向量");
        info!("   • 生成10秒預測標籤");
        info!("   • 建議收集時間：24-48小時");
        info!("   • 目標樣本數：10萬+ (覆蓋不同市場狀態)");
        info!("");
        
        info!("📊 Step 2: Data Preprocessing");
        info!("   • 數據清洗和異常值處理");
        info!("   • 特徵標準化 (Z-score normalization)");
        info!("   • 類別平衡處理 (SMOTE/Under-sampling)");
        info!("   • 時間序列切分 (70% train, 15% val, 15% test)");
        info!("");
        
        info!("🏗️  Step 3: Model Architecture");
        info!("   • Transformer Encoder (4 layers, 8 heads, 256 hidden)");
        info!("   • Input: 76-dim features → 256-dim hidden");
        info!("   • Output: 5-class probability (StrongDown-StrongUp)");
        info!("   • Dropout: 0.1, LayerNorm, Residual connections");
        info!("");
        
        info!("⚙️  Step 4: Training Configuration");
        info!("   • Loss: CrossEntropyLoss with class weights");
        info!("   • Optimizer: AdamW (lr=1e-4, weight_decay=0.01)");
        info!("   • Scheduler: CosineAnnealing with warmup");
        info!("   • Batch size: 32, Epochs: 100-200");
        info!("   • Early stopping: patience=20");
        info!("");
        
        info!("🎯 Step 5: Training Process");
        info!("   1. 初始化權重 (Xavier/He initialization)");
        info!("   2. 前向傳播和損失計算");
        info!("   3. 反向傳播和梯度更新");
        info!("   4. 驗證集評估 (每10 epochs)");
        info!("   5. 模型檢查點保存 (best validation)");
        info!("");
        
        info!("📈 Step 6: Model Evaluation");
        info!("   • Accuracy, Precision, Recall, F1-score");
        info!("   • Confusion Matrix (5x5)");
        info!("   • ROC-AUC (multi-class)");
        info!("   • Sharpe ratio (trading simulation)");
        info!("   • 目標: >55% accuracy, >0.6 confidence");
        info!("");
        
        info!("🚀 Step 7: Production Deployment");
        info!("   • 模型量化 (FP16/INT8)");
        info!("   • ONNX格式轉換");
        info!("   • A/B測試框架");
        info!("   • 在線學習更新");
        info!("   • 性能監控和漂移檢測");
        info!("");
        
        info!("💡 Advanced Techniques:");
        info!("   • Ensemble methods (多模型投票)");
        info!("   • Meta-learning (快速適應新市場)");
        info!("   • Adversarial training (提高魯棒性)");
        info!("   • Feature importance analysis");
        info!("   • Model interpretability (SHAP, LIME)");
        info!("");
        
        info!("⚡ Optimization Tips:");
        info!("   • 使用GPU/TPU加速訓練");
        info!("   • Mixed precision training");
        info!("   • Gradient accumulation");
        info!("   • Learning rate finder");
        info!("   • Hyperparameter tuning (Optuna/Ray Tune)");
        info!("");
        
        info!("📋 Training Commands:");
        info!("   # 1. 收集訓練數據");
        info!("   cargo run --example model_training_guide --release");
        info!("   ");
        info!("   # 2. Python訓練腳本 (建議)");
        info!("   python train_model.py --data training_data.json --epochs 100");
        info!("   ");
        info!("   # 3. 或使用Rust Candle框架");
        info!("   cargo run --bin train_transformer --features gpu");
        info!("");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    // 顯示訓練指南
    ModelTrainingGuide::show_training_guide();
    
    info!("🚀 Starting Training Data Collection");

    // 創建訓練數據收集器
    let mut data_collector = TrainingDataCollector::new()?;
    
    // 創建Bitget配置
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 3,
    };

    // 創建連接器
    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);

    info!("📊 Subscribed to BTCUSDT for training data collection");

    // 統計計數器
    let stats_counter = Arc::new(Mutex::new(0u64));
    let start_time = Instant::now();

    // 收集器引用
    let collector_arc = Arc::new(Mutex::new(data_collector));
    let collector_clone = collector_arc.clone();
    let stats_clone = stats_counter.clone();

    // 創建訓練數據收集處理器
    let message_handler = move |message: BitgetMessage| {
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                // 收集訓練數據
                if let Ok(mut collector) = collector_clone.lock() {
                    if let Err(e) = collector.process_lob_for_training(&symbol, &data, timestamp) {
                        error!("Training data collection error: {}", e);
                    }
                }

                // 更新統計
                {
                    let mut count = stats_clone.lock().unwrap();
                    *count += 1;

                    // 每50次更新顯示收集統計
                    if *count % 50 == 0 {
                        let elapsed = start_time.elapsed();
                        let rate = *count as f64 / elapsed.as_secs_f64();
                        
                        if let Ok(collector) = collector_clone.lock() {
                            let stats = collector.get_collection_stats();
                            info!("📊 Collection: Updates={}, Rate={:.1}/s, Samples={}", 
                                 *count, rate, collector.get_sample_count());
                            info!("   Features: {}, Quality: {:.3}, Distribution: {:?}", 
                                 stats.feature_extractions, stats.avg_quality_score, stats.class_distribution);
                        }
                    }
                }
            }
            _ => {}
        }
    };

    info!("🔌 Connecting to collect training data...");

    // 設置收集時間 (建議24小時+)
    let timeout = Duration::from_secs(600); // 10分鐘示例

    // 啟動數據收集
    match tokio::time::timeout(timeout, connector.connect_public(message_handler)).await {
        Ok(Ok(_)) => {
            info!("✅ Training data collection completed successfully");
        }
        Ok(Err(e)) => {
            error!("❌ Connection failed: {}", e);
            return Err(e);
        }
        Err(_) => {
            info!("⏰ Training data collection completed after timeout");
        }
    }

    // 保存訓練數據
    if let Ok(collector) = collector_arc.lock() {
        let final_stats = collector.get_collection_stats();
        let sample_count = collector.get_sample_count();
        
        info!("🏁 Final Training Data Collection Results:");
        info!("   Total LOB updates: {}", final_stats.total_lob_updates);
        info!("   Feature extractions: {}", final_stats.feature_extractions);
        info!("   Training samples: {}", sample_count);
        info!("   Average quality: {:.3}", final_stats.avg_quality_score);
        info!("   Class distribution: {:?}", final_stats.class_distribution);
        
        if sample_count > 0 {
            // 保存到文件
            let filename = format!("training_data_{}.json", now_micros());
            collector.save_training_data(&filename)?;
            
            info!("💾 Training data saved to: {}", filename);
            info!("📚 Ready for model training!");
            info!("");
            info!("Next steps:");
            info!("1. Review the saved training data");
            info!("2. Set up Python training environment");
            info!("3. Run model training with the collected data");
            info!("4. Evaluate and deploy the trained model");
        } else {
            warn!("⚠️  No training samples collected. Run for longer duration.");
        }
    }

    Ok(())
}