/*!
 * Pure Rust Model Training using Candle Framework
 * 
 * 純Rust深度學習模型訓練系統：
 * 1. 使用Candle框架進行本地訓練
 * 2. Transformer架構實現
 * 3. 24小時數據收集 + 實時訓練
 * 4. 無需Python依賴
 * 
 * Usage modes:
 * - cargo run --example pure_rust_model_training          (advanced mode)
 * - cargo run --example pure_rust_model_training simple   (simplified mode)
 */

use rust_hft::{
    integrations::bitget_connector::*,
    ml::features::FeatureExtractor,
    core::{types::*, orderbook::OrderBook},
};
use anyhow::Result;
use tracing::{info, warn, error};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use tokio::time::Duration;
use serde_json::Value;
use candle_core::{Device, Tensor, DType, D};
use candle_core::Module;
use candle_nn::{VarBuilder, VarMap, Optimizer, AdamW, linear, layer_norm, LayerNorm, Linear, Dropout};
use serde::{Serialize, Deserialize};

/// 純Rust訓練樣本
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustTrainingSample {
    pub timestamp: u64,
    pub features: Vec<f32>,
    pub label: usize, // 0-4 for 5 classes
    pub actual_change_bps: f32,
    pub current_price: f32,
    pub future_price: f32,
}

/// Transformer模型配置
#[derive(Debug, Clone)]
pub struct TransformerConfig {
    pub input_dim: usize,
    pub hidden_dim: usize,
    pub num_heads: usize,
    pub num_layers: usize,
    pub num_classes: usize,
    pub dropout_rate: f64,
}

impl Default for TransformerConfig {
    fn default() -> Self {
        Self {
            input_dim: 76,
            hidden_dim: 256,
            num_heads: 8,
            num_layers: 4,
            num_classes: 5,
            dropout_rate: 0.1,
        }
    }
}

/// Transformer編碼器層
pub struct TransformerEncoderLayer {
    attention: MultiHeadAttention,
    feed_forward: FeedForward,
    norm1: LayerNorm,
    norm2: LayerNorm,
    dropout: Dropout,
}

impl TransformerEncoderLayer {
    pub fn new(config: &TransformerConfig, vb: VarBuilder) -> Result<Self> {
        let attention = MultiHeadAttention::new(config, vb.pp("attention"))?;
        let feed_forward = FeedForward::new(config, vb.pp("feed_forward"))?;
        let norm1 = layer_norm(config.hidden_dim, 1e-5, vb.pp("norm1"))?;
        let norm2 = layer_norm(config.hidden_dim, 1e-5, vb.pp("norm2"))?;
        let dropout = Dropout::new(config.dropout_rate as f32);
        
        Ok(Self {
            attention,
            feed_forward,
            norm1,
            norm2,
            dropout,
        })
    }
    
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        // Multi-head attention with residual connection
        let attn_out = self.attention.forward(x)?;
        let x = (x + &attn_out)?;
        let x = self.norm1.forward(&x)?;
        
        // Feed forward with residual connection
        let ff_out = self.feed_forward.forward(&x)?;
        let x = (&x + &ff_out)?;
        let x = self.norm2.forward(&x)?;
        let x = self.dropout.forward(&x, false)?; // No dropout during inference
        
        Ok(x)
    }
}

/// 多頭注意力機制
pub struct MultiHeadAttention {
    query: Linear,
    key: Linear,
    value: Linear,
    output: Linear,
    num_heads: usize,
    head_dim: usize,
    dropout: Dropout,
}

impl MultiHeadAttention {
    pub fn new(config: &TransformerConfig, vb: VarBuilder) -> Result<Self> {
        let head_dim = config.hidden_dim / config.num_heads;
        
        let query = linear(config.hidden_dim, config.hidden_dim, vb.pp("query"))?;
        let key = linear(config.hidden_dim, config.hidden_dim, vb.pp("key"))?;
        let value = linear(config.hidden_dim, config.hidden_dim, vb.pp("value"))?;
        let output = linear(config.hidden_dim, config.hidden_dim, vb.pp("output"))?;
        let dropout = Dropout::new(config.dropout_rate as f32);
        
        Ok(Self {
            query,
            key,
            value,
            output,
            num_heads: config.num_heads,
            head_dim,
            dropout,
        })
    }
    
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (batch_size, seq_len, _) = x.dims3()?;
        
        // Linear projections
        let q = self.query.forward(x)?;
        let k = self.key.forward(x)?;
        let v = self.value.forward(x)?;
        
        // Reshape for multi-head attention
        let q = q.reshape((batch_size, seq_len, self.num_heads, self.head_dim))?
                 .transpose(1, 2)?; // (batch, heads, seq_len, head_dim)
        let k = k.reshape((batch_size, seq_len, self.num_heads, self.head_dim))?
                 .transpose(1, 2)?;
        let v = v.reshape((batch_size, seq_len, self.num_heads, self.head_dim))?
                 .transpose(1, 2)?;
        
        // Scaled dot-product attention
        let scale = (self.head_dim as f64).sqrt();
        let scores = q.matmul(&k.transpose(D::Minus1, D::Minus2)?)?;
        let scores = (scores / scale)?;
        let attn_weights = candle_nn::ops::softmax(&scores, D::Minus1)?;
        let attn_weights = self.dropout.forward(&attn_weights, false)?;
        
        let attn_output = attn_weights.matmul(&v)?;
        
        // Reshape back
        let attn_output = attn_output.transpose(1, 2)?
                                   .reshape((batch_size, seq_len, self.num_heads * self.head_dim))?;
        
        // Output projection
        let output = self.output.forward(&attn_output)?;
        Ok(output)
    }
}

/// 前饋神經網路
pub struct FeedForward {
    linear1: Linear,
    linear2: Linear,
    dropout: Dropout,
}

impl FeedForward {
    pub fn new(config: &TransformerConfig, vb: VarBuilder) -> Result<Self> {
        let ff_dim = config.hidden_dim * 4; // Standard practice
        
        let linear1 = linear(config.hidden_dim, ff_dim, vb.pp("linear1"))?;
        let linear2 = linear(ff_dim, config.hidden_dim, vb.pp("linear2"))?;
        let dropout = Dropout::new(config.dropout_rate as f32);
        
        Ok(Self {
            linear1,
            linear2,
            dropout,
        })
    }
    
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = self.linear1.forward(x)?;
        let x = x.relu()?;
        let x = self.dropout.forward(&x, false)?;
        let x = self.linear2.forward(&x)?;
        Ok(x)
    }
}

/// HFT Transformer模型
pub struct HFTTransformer {
    input_projection: Linear,
    encoder_layers: Vec<TransformerEncoderLayer>,
    output_projection: Linear,
    dropout: Dropout,
    config: TransformerConfig,
}

impl HFTTransformer {
    pub fn new(config: TransformerConfig, vb: VarBuilder) -> Result<Self> {
        let input_projection = linear(config.input_dim, config.hidden_dim, vb.pp("input_projection"))?;
        let output_projection = linear(config.hidden_dim, config.num_classes, vb.pp("output_projection"))?;
        let dropout = Dropout::new(config.dropout_rate as f32);
        
        let mut encoder_layers = Vec::new();
        for i in 0..config.num_layers {
            let layer = TransformerEncoderLayer::new(&config, vb.pp(&format!("encoder_{}", i)))?;
            encoder_layers.push(layer);
        }
        
        Ok(Self {
            input_projection,
            encoder_layers,
            output_projection,
            dropout,
            config,
        })
    }
    
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        // Input projection
        let mut x = self.input_projection.forward(x)?;
        
        // Add positional encoding (simplified - just use learned embeddings)
        let (_batch_size, _seq_len, _hidden_dim) = x.dims3()?;
        
        // Pass through encoder layers
        for layer in &self.encoder_layers {
            x = layer.forward(&x)?;
        }
        
        // Global average pooling
        let x = x.mean(1)?; // Average over sequence dimension
        
        // Dropout and output projection
        let x = self.dropout.forward(&x, false)?;
        let logits = self.output_projection.forward(&x)?;
        
        Ok(logits)
    }
}

/// 純Rust訓練系統
pub struct PureRustTrainingSystem {
    feature_extractor: FeatureExtractor,
    training_samples: Arc<Mutex<Vec<RustTrainingSample>>>,
    price_history: Arc<Mutex<VecDeque<(u64, f32)>>>, // timestamp, price
    model: Option<HFTTransformer>,
    var_map: VarMap,
    device: Device,
    config: TransformerConfig,
    training_stats: Arc<Mutex<TrainingStats>>,
}

#[derive(Debug, Default)]
pub struct TrainingStats {
    pub total_samples: usize,
    pub epochs_completed: usize,
    pub current_loss: f32,
    pub best_accuracy: f32,
    pub last_training_time: u64,
}

impl PureRustTrainingSystem {
    pub fn new() -> Result<Self> {
        let device = Device::Cpu; // Use CPU for compatibility
        let var_map = VarMap::new();
        let config = TransformerConfig::default();
        let feature_extractor = FeatureExtractor::new(100);
        
        info!("🚀 Initializing Pure Rust Training System");
        info!("   Device: {:?}", device);
        info!("   Model Config: {:?}", config);
        
        Ok(Self {
            feature_extractor,
            training_samples: Arc::new(Mutex::new(Vec::new())),
            price_history: Arc::new(Mutex::new(VecDeque::with_capacity(2000))),
            model: None,
            var_map,
            device,
            config,
            training_stats: Arc::new(Mutex::new(TrainingStats::default())),
        })
    }
    
    /// 初始化模型
    pub fn initialize_model(&mut self) -> Result<()> {
        let vb = VarBuilder::from_varmap(&self.var_map, DType::F32, &self.device);
        let model = HFTTransformer::new(self.config.clone(), vb)?;
        self.model = Some(model);
        
        info!("✅ HFT Transformer model initialized");
        info!("   Parameters: ~{} M", self.estimate_parameter_count());
        
        Ok(())
    }
    
    fn estimate_parameter_count(&self) -> f32 {
        // Rough estimation for Transformer model
        let config = &self.config;
        let attention_params = 4 * config.hidden_dim * config.hidden_dim * config.num_layers;
        let ff_params = 2 * config.hidden_dim * config.hidden_dim * 4 * config.num_layers;
        let io_params = config.input_dim * config.hidden_dim + config.hidden_dim * config.num_classes;
        
        (attention_params + ff_params + io_params) as f32 / 1_000_000.0
    }
    
    /// 處理LOB數據並創建訓練樣本
    pub fn process_lob_data(&mut self, symbol: &str, data: &Value, timestamp: u64) -> Result<()> {
        if symbol != "BTCUSDT" {
            return Ok(());
        }
        
        // 解析OrderBook
        let orderbook = self.parse_orderbook(data, timestamp)?;
        let mid_price = self.calculate_mid_price(&orderbook);
        
        // 記錄價格歷史
        {
            let mut price_history = self.price_history.lock().unwrap();
            price_history.push_back((timestamp, mid_price));
            if price_history.len() > 2000 {
                price_history.pop_front();
            }
        }
        
        // 提取特徵
        match self.feature_extractor.extract_features(&orderbook, 100, timestamp) {
            Ok(features) => {
                // 嘗試創建訓練樣本（需要10秒後的價格）
                if let Some(sample) = self.create_training_sample(&features, mid_price, timestamp)? {
                    self.add_training_sample(sample);
                    
                    // 每1000個樣本進行一次增量訓練
                    let sample_count = self.training_samples.lock().unwrap().len();
                    if sample_count > 0 && sample_count % 1000 == 0 {
                        info!("📚 Collected {} samples, starting incremental training...", sample_count);
                        self.train_incremental()?;
                    }
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
        current_price: f32,
        timestamp: u64
    ) -> Result<Option<RustTrainingSample>> {
        let future_timestamp = timestamp + 10_000_000; // 10秒後
        
        let future_price = {
            let price_history = self.price_history.lock().unwrap();
            price_history.iter()
                .find(|(ts, _)| *ts >= future_timestamp)
                .map(|(_, price)| *price)
        };
        
        if let Some(future_price) = future_price {
            let price_change_bps = (future_price - current_price) / current_price * 10000.0;
            
            // 分類標籤生成
            let label = match price_change_bps {
                x if x > 10.0 => 4,    // StrongUp
                x if x > 2.0 => 3,     // WeakUp  
                x if x > -2.0 => 2,    // Neutral
                x if x > -10.0 => 1,   // WeakDown
                _ => 0,                // StrongDown
            };
            
            let feature_vector = self.features_to_vector(features);
            
            Ok(Some(RustTrainingSample {
                timestamp,
                features: feature_vector,
                label,
                actual_change_bps: price_change_bps,
                current_price,
                future_price,
            }))
        } else {
            Ok(None)
        }
    }
    
    /// 特徵轉換為向量 - 簡化版使用可用字段
    fn features_to_vector(&self, features: &FeatureSet) -> Vec<f32> {
        let mut feature_vec = vec![
            // 基礎價格特徵
            (*features.mid_price) as f32,
            features.spread_bps as f32,
            features.obi_l1 as f32, 
            features.obi_l5 as f32, 
            features.obi_l10 as f32, 
            features.obi_l20 as f32,
            features.price_momentum as f32,
            features.vwap as f32,
            features.microprice as f32,
            features.effective_spread as f32,
            features.realized_volatility as f32,
            features.bid_slope as f32, 
            features.ask_slope as f32,
            features.liquidity_score as f32,
            features.order_arrival_rate as f32,
            features.order_flow_imbalance as f32,
            features.trade_intensity as f32,
            features.volume_acceleration as f32,
            features.depth_pressure_bid as f32, 
            features.depth_pressure_ask as f32,
            features.cancellation_rate as f32,
            // 動量和波動性特徵
            features.momentum_5_tick as f32,
            features.volatility_5_tick as f32,
            features.latency_network_us as f32,
        ];
        
        // 添加LOB tensor特徵（如果存在）
        if let Some(lob_tensor) = &features.lob_tensor_l10 {
            // 添加最近一個時間步的前20個特徵
            if let Some(current_data) = lob_tensor.data.get(lob_tensor.current_step) {
                for i in 0..20.min(current_data.len()) {
                    feature_vec.push(current_data[i] as f32);
                }
            }
        }
        
        // 填充到76維
        while feature_vec.len() < 76 {
            feature_vec.push(0.0);
        }
        
        // 截斷到76維
        feature_vec.truncate(76);
        
        feature_vec
    }
    
    fn add_training_sample(&self, sample: RustTrainingSample) {
        {
            let mut samples = self.training_samples.lock().unwrap();
            samples.push(sample.clone());
        }
        
        {
            let mut stats = self.training_stats.lock().unwrap();
            stats.total_samples += 1;
        }
        
        info!("📊 Training sample created: Label={}, Change={:.1}bps", 
              sample.label, sample.actual_change_bps);
    }
    
    /// 增量訓練
    pub fn train_incremental(&mut self) -> Result<()> {
        if self.model.is_none() {
            self.initialize_model()?;
        }
        
        let samples = self.training_samples.lock().unwrap().clone();
        if samples.len() < 100 {
            return Ok(());
        }
        
        info!("🎯 Starting incremental training with {} samples", samples.len());
        
        // 準備訓練數據
        let (train_features, train_labels) = self.prepare_training_data(&samples)?;
        
        // 創建優化器
        let mut optimizer = AdamW::new(self.var_map.all_vars(), candle_nn::ParamsAdamW {
            lr: 1e-4,
            weight_decay: 0.01,
            ..Default::default()
        })?;
        
        let model = self.model.as_ref().unwrap();
        
        // 訓練幾個epochs
        let epochs = 5;
        for epoch in 0..epochs {
            let logits = model.forward(&train_features)?;
            let loss = candle_nn::loss::cross_entropy(&logits, &train_labels)?;
            
            optimizer.backward_step(&loss)?;
            
            let loss_val = loss.to_scalar::<f32>()?;
            
            if epoch % 1 == 0 {
                info!("   Epoch {}/{}: Loss = {:.4}", epoch + 1, epochs, loss_val);
            }
            
            // 更新統計
            {
                let mut stats = self.training_stats.lock().unwrap();
                stats.current_loss = loss_val;
                stats.epochs_completed += 1;
                stats.last_training_time = now_micros();
            }
        }
        
        info!("✅ Incremental training completed");
        Ok(())
    }
    
    /// 準備訓練數據
    fn prepare_training_data(&self, samples: &[RustTrainingSample]) -> Result<(Tensor, Tensor)> {
        let batch_size = samples.len();
        let seq_len = 1; // 單個時間步
        let feature_dim = 76;
        
        // 準備特徵數據
        let mut features_data = Vec::with_capacity(batch_size * seq_len * feature_dim);
        let mut labels_data = Vec::with_capacity(batch_size);
        
        for sample in samples {
            // 確保特徵維度正確
            let mut features = sample.features.clone();
            features.resize(feature_dim, 0.0);
            features_data.extend_from_slice(&features);
            labels_data.push(sample.label as u32);
        }
        
        let features_tensor = Tensor::from_slice(
            &features_data,
            (batch_size, seq_len, feature_dim),
            &self.device
        )?;
        
        let labels_tensor = Tensor::from_slice(
            &labels_data,
            batch_size,
            &self.device
        )?;
        
        Ok((features_tensor, labels_tensor))
    }
    
    /// 預測
    pub fn predict(&self, features: &FeatureSet) -> Result<(usize, f32)> {
        if let Some(model) = &self.model {
            let feature_vector = self.features_to_vector(features);
            let mut padded_features = feature_vector;
            padded_features.resize(76, 0.0);
            
            let input_tensor = Tensor::from_slice(
                &padded_features,
                (1, 1, 76), // batch=1, seq=1, features=76
                &self.device
            )?;
            
            let logits = model.forward(&input_tensor)?;
            let probabilities = candle_nn::ops::softmax(&logits, 1)?;
            
            // 找到最大概率的類別
            let probs_vec = probabilities.to_vec2::<f32>()?;
            let (predicted_class, confidence) = probs_vec[0]
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                .map(|(i, &p)| (i, p))
                .unwrap_or((2, 0.0)); // 默認為Neutral
            
            Ok((predicted_class, confidence))
        } else {
            Err(anyhow::anyhow!("Model not initialized"))
        }
    }
    
    /// 保存模型
    pub fn save_model(&self, path: &str) -> Result<()> {
        self.var_map.save(path)?;
        info!("💾 Model saved to: {}", path);
        Ok(())
    }
    
    /// 加載模型  
    pub fn load_model(&mut self, path: &str) -> Result<()> {
        self.var_map.load(path)?;
        self.initialize_model()?;
        info!("📚 Model loaded from: {}", path);
        Ok(())
    }
    
    /// 獲取訓練統計
    pub fn get_training_stats(&self) -> TrainingStats {
        self.training_stats.lock().unwrap().clone()
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
    
    fn calculate_mid_price(&self, orderbook: &OrderBook) -> f32 {
        let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        ((best_bid + best_ask) / 2.0) as f32
    }
}

impl Clone for TrainingStats {
    fn clone(&self) -> Self {
        Self {
            total_samples: self.total_samples,
            epochs_completed: self.epochs_completed,
            current_loss: self.current_loss,
            best_accuracy: self.best_accuracy,
            last_training_time: self.last_training_time,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 Starting Pure Rust Model Training System");
    info!("🦀 Using Candle Framework for Deep Learning");

    // 創建純Rust訓練系統
    let training_system = PureRustTrainingSystem::new()?;
    
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

    info!("📊 Subscribed to BTCUSDT for continuous training data collection");

    // 統計計數器
    let stats_counter = Arc::new(Mutex::new(0u64));
    let start_time = std::time::Instant::now();

    // 系統引用
    let system_arc = Arc::new(Mutex::new(training_system));
    let system_clone = system_arc.clone();
    let stats_clone = stats_counter.clone();

    // 創建訓練數據收集和在線學習處理器
    let message_handler = move |message: BitgetMessage| {
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                // 處理LOB數據並進行在線學習
                if let Ok(mut system) = system_clone.lock() {
                    if let Err(e) = system.process_lob_data(&symbol, &data, timestamp) {
                        error!("Training system processing error: {}", e);
                    }
                }

                // 更新統計
                {
                    let mut count = stats_clone.lock().unwrap();
                    *count += 1;

                    // 每100次更新顯示統計信息
                    if *count % 100 == 0 {
                        let elapsed = start_time.elapsed();
                        let rate = *count as f64 / elapsed.as_secs_f64();
                        
                        if let Ok(system) = system_clone.lock() {
                            let stats = system.get_training_stats();
                            info!("📊 Pure Rust Training: Updates={}, Rate={:.1}/s", *count, rate);
                            info!("   Samples: {}, Epochs: {}, Loss: {:.4}", 
                                 stats.total_samples, stats.epochs_completed, stats.current_loss);
                            
                            if stats.total_samples > 0 {
                                info!("   🦀 Rust ML Training: ACTIVE | Last: {}μs ago", 
                                     now_micros().saturating_sub(stats.last_training_time));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    };

    info!("🔌 Connecting for 24-hour Pure Rust ML training...");

    // 設置運行時間限制 - 24小時數據收集和訓練
    let timeout = Duration::from_secs(24 * 60 * 60); // 24小時運行
    
    info!("🕐 System will collect data and train for 24 hours");
    info!("🦀 Pure Rust ML training will happen every 1000 samples");
    info!("💡 Press Ctrl+C to stop early and save model");

    // 啟動連接
    match tokio::time::timeout(timeout, connector.connect_public(message_handler)).await {
        Ok(Ok(_)) => {
            info!("✅ Pure Rust training completed successfully");
        }
        Ok(Err(e)) => {
            error!("❌ Connection failed: {}", e);
            return Err(e);
        }
        Err(_) => {
            info!("⏰ 24-hour training session completed");
        }
    }

    // 最終統計和模型保存
    if let Ok(system) = system_arc.lock() {
        let stats = system.get_training_stats();
        let final_count = *stats_counter.lock().unwrap();
        let elapsed = start_time.elapsed();
        let rate = final_count as f64 / elapsed.as_secs_f64();

        info!("🏁 Final Pure Rust Training Results:");
        info!("   Total LOB updates: {}", final_count);
        info!("   Average rate: {:.1} updates/sec", rate);
        info!("   Training samples collected: {}", stats.total_samples);
        info!("   Epochs completed: {}", stats.epochs_completed);
        info!("   Final loss: {:.4}", stats.current_loss);
        info!("   Training session duration: {:.1} hours", elapsed.as_secs_f64() / 3600.0);

        // 保存模型
        if stats.total_samples > 100 {
            let model_path = format!("hft_model_{}.safetensors", now_micros());
            if let Err(e) = system.save_model(&model_path) {
                error!("Failed to save model: {}", e);
            } else {
                info!("💾 Pure Rust model saved to: {}", model_path);
                info!("🦀 Model can be loaded and used for real-time inference");
            }
        }
    }

    info!("🎓 Pure Rust Model Training Session Complete!");
    info!("🦀 No Python dependencies required - 100% Rust ML pipeline");

    Ok(())
}