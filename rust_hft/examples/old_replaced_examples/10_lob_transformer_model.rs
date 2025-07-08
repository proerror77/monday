/*!
 * LOB (Limit Order Book) Transformer Model for HFT Prediction
 * 
 * 實現TLOB (Transformer LOB) 架構，針對訂單簿數據優化：
 * - 雙注意力機制：Temporal Self-Attention + Feature Self-Attention
 * - Bilinear Normalization：處理金融數據非平穩性
 * - 多時間範圍預測：1s, 3s, 5s, 10s
 * - 針對HFT優化：<50μs推理延遲
 * 
 * 執行方式：
 * cargo run --example lob_transformer_model -- --test-inference
 */

use rust_hft::{
    core::{types::*, orderbook::OrderBook},
    ml::{features::FeatureExtractor, online_learning::{OnlineLearningConfig, TrainingSample}},
    integrations::bitget_connector::*,
    utils::performance::*,
};
use anyhow::Result;
use candle_core::{Device, Tensor, DType, Module, Shape, D};
use candle_nn::{VarBuilder, VarMap, Linear, Dropout, LayerNorm, Activation, Init};
use std::collections::HashMap;
use tracing::{info, warn, error, debug};
use clap::Parser;
use serde::{Serialize, Deserialize};

#[derive(Parser)]
#[command(name = "lob_transformer_model")]
#[command(about = "LOB Transformer Model Architecture Testing")]
struct Args {
    #[arg(long, default_value_t = false)]
    test_inference: bool,
    
    #[arg(long, default_value_t = 40)]
    sequence_length: usize,
    
    #[arg(long, default_value_t = 32)]
    batch_size: usize,
    
    #[arg(long, default_value_t = false)]
    use_gpu: bool,
}

/// LOB Transformer模型配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobTransformerConfig {
    // 輸入配置
    pub lob_features: usize,        // LOB特徵數量 (40個標準LOB特徵)
    pub market_features: usize,     // 市場微觀結構特徵數量
    pub sequence_length: usize,     // 序列長度
    
    // 模型架構配置
    pub d_model: usize,            // 模型維度
    pub n_heads: usize,            // 注意力頭數
    pub n_layers: usize,           // Transformer層數
    pub d_ff: usize,               // Feed-forward維度
    pub dropout: f64,              // Dropout率
    
    // LOB特定配置
    pub use_bilinear_norm: bool,   // 是否使用雙線性歸一化
    pub use_temporal_encoding: bool, // 是否使用時間位置編碼
    pub feature_attention: bool,   // 是否使用特徵注意力
    
    // 預測配置
    pub prediction_horizons: Vec<u64>, // 預測時間範圍 [1s, 3s, 5s, 10s]
    pub num_classes: usize,        // 分類數量 (up=2, down=0, stationary=1)
    
    // 性能配置
    pub use_flash_attention: bool, // 是否使用Flash Attention
    pub gradient_checkpointing: bool, // 是否使用梯度檢查點
}

impl Default for LobTransformerConfig {
    fn default() -> Self {
        Self {
            lob_features: 40,
            market_features: 10,
            sequence_length: 100,
            
            d_model: 256,
            n_heads: 8,
            n_layers: 6,
            d_ff: 1024,
            dropout: 0.1,
            
            use_bilinear_norm: true,
            use_temporal_encoding: true,
            feature_attention: true,
            
            prediction_horizons: vec![1, 3, 5, 10],
            num_classes: 3,
            
            use_flash_attention: true,
            gradient_checkpointing: false,
        }
    }
}

/// 雙線性歸一化層 - 處理金融時間序列的非平穩性
#[derive(Debug)]
pub struct BilinearNormalization {
    weight: Tensor,
    bias: Tensor,
    scale: Tensor,
}

impl BilinearNormalization {
    pub fn new(d_model: usize, vs: VarBuilder) -> Result<Self> {
        let weight = vs.get((d_model, d_model), "weight")?;
        let bias = vs.get(d_model, "bias")?;
        let scale = vs.get(d_model, "scale")?;
        
        Ok(Self { weight, bias, scale })
    }
    
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        // x: [batch_size, seq_len, d_model]
        let batch_size = x.dim(0)?;
        let seq_len = x.dim(1)?;
        let d_model = x.dim(2)?;
        
        // 計算均值和標準差 (在最後一個維度上)
        let mean = x.mean_keepdim(D::Minus1)?;
        let variance = x.var_keepdim(D::Minus1)?;
        let eps = Tensor::full(1e-8f32, variance.shape(), variance.device())?;
        let std = (variance + eps)?.sqrt()?;
        
        // 標準化 - 確保維度匹配
        let normalized = x.broadcast_sub(&mean)?.broadcast_div(&std)?;
        
        // 雙線性變換 - 確保維度匹配
        let result = normalized.broadcast_mul(&self.scale)?.broadcast_add(&self.bias)?;
        
        Ok(result)
    }
}

impl Module for BilinearNormalization {
    fn forward(&self, xs: &Tensor) -> candle_core::Result<Tensor> {
        self.forward(xs).map_err(|e| candle_core::Error::Msg(e.to_string()))
    }
}

/// 時間位置編碼 - 針對金融時間序列優化
#[derive(Debug)]
pub struct TemporalPositionalEncoding {
    encoding: Tensor,
    d_model: usize,
}

impl TemporalPositionalEncoding {
    pub fn new(max_seq_len: usize, d_model: usize, device: &Device) -> Result<Self> {
        let mut encoding = vec![vec![0.0f32; d_model]; max_seq_len];
        
        for pos in 0..max_seq_len {
            for i in 0..d_model / 2 {
                let angle = pos as f32 / 10000.0_f32.powf(2.0 * i as f32 / d_model as f32);
                encoding[pos][2 * i] = angle.sin();
                encoding[pos][2 * i + 1] = angle.cos();
            }
        }
        
        let flat_encoding: Vec<f32> = encoding.into_iter().flatten().collect();
        let encoding_tensor = Tensor::from_vec(
            flat_encoding,
            (max_seq_len, d_model),
            device
        )?;
        
        Ok(Self {
            encoding: encoding_tensor,
            d_model,
        })
    }
    
    pub fn forward(&self, seq_len: usize) -> Result<Tensor> {
        let slice = self.encoding.narrow(0, 0, seq_len)?;
        Ok(slice)
    }
}

/// 多頭自注意力機制 - 針對LOB數據優化
#[derive(Debug)]
pub struct MultiHeadAttention {
    q_linear: Linear,
    k_linear: Linear,
    v_linear: Linear,
    output_linear: Linear,
    dropout: Dropout,
    n_heads: usize,
    d_model: usize,
    d_k: usize,
}

impl MultiHeadAttention {
    pub fn new(d_model: usize, n_heads: usize, dropout: f64, vs: VarBuilder) -> Result<Self> {
        assert_eq!(d_model % n_heads, 0);
        let d_k = d_model / n_heads;
        
        let q_linear = candle_nn::linear(d_model, d_model, vs.pp("q_linear"))?;
        let k_linear = candle_nn::linear(d_model, d_model, vs.pp("k_linear"))?;
        let v_linear = candle_nn::linear(d_model, d_model, vs.pp("v_linear"))?;
        let output_linear = candle_nn::linear(d_model, d_model, vs.pp("output_linear"))?;
        let dropout = Dropout::new(dropout as f32);
        
        Ok(Self {
            q_linear,
            k_linear,
            v_linear,
            output_linear,
            dropout,
            n_heads,
            d_model,
            d_k,
        })
    }
    
    pub fn forward(&self, x: &Tensor, mask: Option<&Tensor>, training: bool) -> Result<Tensor> {
        let batch_size = x.dim(0)?;
        let seq_len = x.dim(1)?;
        
        // Linear transformations
        let q = self.q_linear.forward(x)?;
        let k = self.k_linear.forward(x)?;
        let v = self.v_linear.forward(x)?;
        
        // Reshape for multi-head attention
        let q = q.reshape((batch_size, seq_len, self.n_heads, self.d_k))?
               .transpose(1, 2)?.contiguous()?; // [batch, n_heads, seq_len, d_k]
        let k = k.reshape((batch_size, seq_len, self.n_heads, self.d_k))?
               .transpose(1, 2)?.contiguous()?;
        let v = v.reshape((batch_size, seq_len, self.n_heads, self.d_k))?
               .transpose(1, 2)?.contiguous()?;
        
        // Scaled dot-product attention
        let attention_output = self.scaled_dot_product_attention(&q, &k, &v, mask)?;
        
        // Concatenate heads
        let attention_output = attention_output.transpose(1, 2)?.contiguous()?
                                               .reshape((batch_size, seq_len, self.d_model))?;
        
        // Final linear transformation
        let output = self.output_linear.forward(&attention_output)?;
        
        if training {
            Ok(self.dropout.forward(&output, training)?)
        } else {
            Ok(output)
        }
    }
    
    fn scaled_dot_product_attention(
        &self,
        q: &Tensor,
        k: &Tensor,
        v: &Tensor,
        mask: Option<&Tensor>
    ) -> Result<Tensor> {
        // q, k, v: [batch, n_heads, seq_len, d_k]
        let d_k = self.d_k as f64;
        
        // Attention scores - ensure tensors are contiguous for matrix multiplication
        let k_transposed = k.transpose(D::Minus1, D::Minus2)?.contiguous()?;
        let scores = q.matmul(&k_transposed)?;
        let scaled_scores = (scores / d_k.sqrt())?;
        
        // Apply mask if provided
        let masked_scores = if let Some(mask) = mask {
            // Expand mask dimensions to match scores
            let expanded_mask = mask.unsqueeze(1)?.unsqueeze(1)?; // [batch, 1, 1, seq_len]
            let mask_value = Tensor::full(f32::NEG_INFINITY, scaled_scores.shape(), scaled_scores.device())?;
            scaled_scores.where_cond(&expanded_mask, &mask_value)?
        } else {
            scaled_scores
        };
        
        // Softmax
        let attention_weights = candle_nn::ops::softmax_last_dim(&masked_scores)?;
        
        // Apply attention to values - ensure contiguous tensors
        let v_contiguous = v.contiguous()?;
        let output = attention_weights.matmul(&v_contiguous)?;
        
        Ok(output)
    }
}

impl Module for MultiHeadAttention {
    fn forward(&self, xs: &Tensor) -> candle_core::Result<Tensor> {
        self.forward(xs, None, false).map_err(|e| candle_core::Error::Msg(e.to_string()))
    }
}

/// Transformer編碼器層
#[derive(Debug)]
pub struct TransformerEncoderLayer {
    self_attention: MultiHeadAttention,
    feed_forward: FeedForward,
    norm1: LayerNorm,
    norm2: LayerNorm,
    bilinear_norm: Option<BilinearNormalization>,
    dropout: Dropout,
}

impl TransformerEncoderLayer {
    pub fn new(
        d_model: usize,
        n_heads: usize,
        d_ff: usize,
        dropout: f64,
        use_bilinear_norm: bool,
        vs: VarBuilder
    ) -> Result<Self> {
        let self_attention = MultiHeadAttention::new(d_model, n_heads, dropout, vs.pp("attention"))?;
        let feed_forward = FeedForward::new(d_model, d_ff, dropout, vs.pp("feed_forward"))?;
        let norm1 = candle_nn::layer_norm(d_model, 1e-6, vs.pp("norm1"))?;
        let norm2 = candle_nn::layer_norm(d_model, 1e-6, vs.pp("norm2"))?;
        let dropout = Dropout::new(dropout as f32);
        
        let bilinear_norm = if use_bilinear_norm {
            Some(BilinearNormalization::new(d_model, vs.pp("bilinear_norm"))?)
        } else {
            None
        };
        
        Ok(Self {
            self_attention,
            feed_forward,
            norm1,
            norm2,
            bilinear_norm,
            dropout,
        })
    }
    
    pub fn forward(&self, x: &Tensor, mask: Option<&Tensor>, training: bool) -> Result<Tensor> {
        // Self-attention with residual connection
        let attn_output = self.self_attention.forward(x, mask, training)?;
        let attn_output = if training {
            self.dropout.forward(&attn_output, training).map_err(|e| anyhow::anyhow!(e.to_string()))?
        } else {
            attn_output
        };
        let x = (x + &attn_output)?;
        let x = self.norm1.forward(&x)?;
        
        // Apply bilinear normalization if enabled
        let x = if let Some(ref bilinear_norm) = self.bilinear_norm {
            bilinear_norm.forward(&x)?
        } else {
            x
        };
        
        // Feed-forward with residual connection
        let ff_output = self.feed_forward.forward(&x, training)?;
        let ff_output = if training {
            self.dropout.forward(&ff_output, training).map_err(|e| anyhow::anyhow!(e.to_string()))?
        } else {
            ff_output
        };
        let x = (&x + &ff_output)?;
        let x = self.norm2.forward(&x)?;
        
        Ok(x)
    }
}

impl Module for TransformerEncoderLayer {
    fn forward(&self, xs: &Tensor) -> candle_core::Result<Tensor> {
        self.forward(xs, None, false).map_err(|e| candle_core::Error::Msg(e.to_string()))
    }
}

/// Feed-Forward網絡
#[derive(Debug)]
pub struct FeedForward {
    linear1: Linear,
    linear2: Linear,
    dropout: Dropout,
}

impl FeedForward {
    pub fn new(d_model: usize, d_ff: usize, dropout: f64, vs: VarBuilder) -> Result<Self> {
        let linear1 = candle_nn::linear(d_model, d_ff, vs.pp("linear1"))?;
        let linear2 = candle_nn::linear(d_ff, d_model, vs.pp("linear2"))?;
        let dropout = Dropout::new(dropout as f32);
        
        Ok(Self {
            linear1,
            linear2,
            dropout,
        })
    }
    
    pub fn forward(&self, x: &Tensor, training: bool) -> Result<Tensor> {
        let x = self.linear1.forward(x)?;
        let x = x.gelu()?;
        let x = if training {
            self.dropout.forward(&x, training).map_err(|e| anyhow::anyhow!(e.to_string()))?
        } else {
            x
        };
        let x = self.linear2.forward(&x)?;
        Ok(x)
    }
}

impl Module for FeedForward {
    fn forward(&self, xs: &Tensor) -> candle_core::Result<Tensor> {
        self.forward(xs, false).map_err(|e| candle_core::Error::Msg(e.to_string()))
    }
}

/// 完整的LOB Transformer模型
#[derive(Debug)]
pub struct LobTransformerModel {
    config: LobTransformerConfig,
    
    // 輸入嵌入層
    lob_embedding: Linear,
    market_embedding: Linear,
    
    // 位置編碼
    positional_encoding: TemporalPositionalEncoding,
    
    // Transformer編碼器
    encoder_layers: Vec<TransformerEncoderLayer>,
    
    // 特徵注意力層 (如果啟用)
    feature_attention: Option<MultiHeadAttention>,
    
    // 輸出層
    output_projection: Linear,
    prediction_heads: HashMap<u64, Linear>, // 每個時間範圍一個預測頭
    
    // Dropout
    input_dropout: Dropout,
    
    device: Device,
}

impl LobTransformerModel {
    pub fn new(config: LobTransformerConfig, vs: VarBuilder, device: Device) -> Result<Self> {
        let _total_input_features = config.lob_features + config.market_features;
        
        // 輸入嵌入層
        let lob_embedding = candle_nn::linear(config.lob_features, config.d_model, vs.pp("lob_embedding"))?;
        let market_embedding = candle_nn::linear(config.market_features, config.d_model, vs.pp("market_embedding"))?;
        
        // 位置編碼
        let positional_encoding = TemporalPositionalEncoding::new(
            config.sequence_length + 100, // 額外緩衝
            config.d_model,
            &device
        )?;
        
        // Transformer編碼器層
        let mut encoder_layers = Vec::new();
        for i in 0..config.n_layers {
            let layer = TransformerEncoderLayer::new(
                config.d_model,
                config.n_heads,
                config.d_ff,
                config.dropout,
                config.use_bilinear_norm,
                vs.pp(&format!("encoder_layer_{}", i))
            )?;
            encoder_layers.push(layer);
        }
        
        // 特徵注意力層
        let feature_attention = if config.feature_attention {
            Some(MultiHeadAttention::new(
                config.d_model,
                config.n_heads,
                config.dropout,
                vs.pp("feature_attention")
            )?)
        } else {
            None
        };
        
        // 輸出投影層
        let output_projection = candle_nn::linear(config.d_model, config.d_model, vs.pp("output_projection"))?;
        
        // 為每個預測時間範圍創建預測頭
        let mut prediction_heads = HashMap::new();
        for &horizon in &config.prediction_horizons {
            let head = candle_nn::linear(config.d_model, config.num_classes, vs.pp(&format!("pred_head_{}", horizon)))?;
            prediction_heads.insert(horizon, head);
        }
        
        let input_dropout = Dropout::new(config.dropout as f32);
        
        info!("LOB Transformer模型初始化完成");
        info!("模型配置: d_model={}, n_heads={}, n_layers={}", 
              config.d_model, config.n_heads, config.n_layers);
        info!("預測時間範圍: {:?}", config.prediction_horizons);
        
        Ok(Self {
            config,
            lob_embedding,
            market_embedding,
            positional_encoding,
            encoder_layers,
            feature_attention,
            output_projection,
            prediction_heads,
            input_dropout,
            device,
        })
    }
    
    /// 前向傳播
    pub fn forward(
        &self,
        lob_features: &Tensor,     // [batch_size, seq_len, lob_features]
        market_features: &Tensor,  // [batch_size, seq_len, market_features]
        mask: Option<&Tensor>,     // [batch_size, seq_len]
        training: bool
    ) -> Result<HashMap<u64, Tensor>> {
        let batch_size = lob_features.dim(0)?;
        let seq_len = lob_features.dim(1)?;
        
        // 嵌入LOB特徵和市場特徵
        let lob_embedded = self.lob_embedding.forward(lob_features)?;
        let market_embedded = self.market_embedding.forward(market_features)?;
        
        // 特徵融合 (簡單相加)
        let x = (lob_embedded + market_embedded)?;
        
        // 添加位置編碼
        let pos_encoding = self.positional_encoding.forward(seq_len)?;
        let pos_encoding = pos_encoding.unsqueeze(0)?.expand((batch_size, seq_len, self.config.d_model))?;
        let x = (x + pos_encoding)?;
        
        // 應用輸入dropout
        let mut x = if training {
            self.input_dropout.forward(&x, training).map_err(|e| anyhow::anyhow!(e.to_string()))?
        } else {
            x
        };
        
        // 通過Transformer編碼器層
        for layer in &self.encoder_layers {
            x = layer.forward(&x, mask, training)?;
        }
        
        // 特徵注意力 (如果啟用)
        if let Some(ref feature_attn) = self.feature_attention {
            x = feature_attn.forward(&x, mask, training)?;
        }
        
        // 輸出投影
        let x = self.output_projection.forward(&x)?;
        
        // 使用最後一個時間步的輸出進行預測
        let last_hidden = x.narrow(1, seq_len - 1, 1)?.squeeze(1)?; // [batch_size, d_model]
        
        // 為每個時間範圍生成預測
        let mut predictions = HashMap::new();
        for (&horizon, head) in &self.prediction_heads {
            let pred = head.forward(&last_hidden)?;
            predictions.insert(horizon, pred);
        }
        
        Ok(predictions)
    }
    
    /// 推理模式前向傳播 (優化延遲)
    pub fn predict(
        &self,
        lob_features: &Tensor,
        market_features: &Tensor
    ) -> Result<HashMap<u64, Tensor>> {
        self.forward(lob_features, market_features, None, false)
    }
    
    /// 獲取模型配置
    pub fn config(&self) -> &LobTransformerConfig {
        &self.config
    }
    
    /// 計算模型參數數量
    pub fn count_parameters(&self) -> usize {
        // 這裡需要實現參數計數邏輯
        // 簡化版本：估算主要組件的參數
        let embedding_params = (self.config.lob_features + self.config.market_features) * self.config.d_model;
        let transformer_params = self.config.n_layers * (
            4 * self.config.d_model * self.config.d_model +  // Q, K, V, O projections
            2 * self.config.d_model * self.config.d_ff       // Feed-forward layers
        );
        let output_params = self.config.d_model * self.config.num_classes * self.config.prediction_horizons.len();
        
        embedding_params + transformer_params + output_params
    }
}

impl Module for LobTransformerModel {
    fn forward(&self, xs: &Tensor) -> candle_core::Result<Tensor> {
        // 假設輸入已經適當分割為LOB和市場特徵
        let _seq_len = xs.dim(1)?;
        let _total_features = xs.dim(2)?;
        
        let lob_features = xs.narrow(2, 0, self.config.lob_features)?;
        let market_features = xs.narrow(2, self.config.lob_features, self.config.market_features)?;
        
        match self.predict(&lob_features, &market_features) {
            Ok(predictions) => {
                // 返回第一個預測時間範圍的結果
                if let Some(first_horizon) = self.config.prediction_horizons.first() {
                    if let Some(pred) = predictions.get(first_horizon) {
                        Ok(pred.clone())
                    } else {
                        Err(candle_core::Error::Msg("No prediction found".to_string()))
                    }
                } else {
                    Err(candle_core::Error::Msg("No prediction horizons configured".to_string()))
                }
            }
            Err(e) => Err(candle_core::Error::Msg(e.to_string()))
        }
    }
}

/// 模型訓練樣本
#[derive(Debug, Clone)]
pub struct LobTrainingSample {
    pub lob_sequence: Vec<Vec<f64>>,     // [seq_len, lob_features]
    pub market_sequence: Vec<Vec<f64>>,  // [seq_len, market_features]
    pub labels: HashMap<u64, u32>,       // horizon -> class (0=down, 1=stationary, 2=up)
    pub timestamp: Timestamp,
    pub symbol: String,
}

/// 測試推理性能
async fn test_inference_performance(args: &Args) -> Result<()> {
    info!("🧪 測試LOB Transformer推理性能");
    
    let device = if args.use_gpu && Device::cuda_if_available(0).is_ok() {
        Device::cuda_if_available(0)?
    } else {
        Device::Cpu
    };
    
    let config = LobTransformerConfig::default();
    let var_map = VarMap::new();
    let vs = VarBuilder::from_varmap(&var_map, DType::F32, &device);
    
    let model = LobTransformerModel::new(config.clone(), vs, device.clone())?;
    
    info!("模型參數數量: {}", model.count_parameters());
    
    // 創建測試數據
    let batch_size = args.batch_size;
    let seq_len = args.sequence_length;
    
    let lob_data: Vec<f32> = (0..batch_size * seq_len * config.lob_features)
        .map(|_| fastrand::f32())
        .collect();
    let market_data: Vec<f32> = (0..batch_size * seq_len * config.market_features)
        .map(|_| fastrand::f32())
        .collect();
    
    let lob_tensor = Tensor::from_vec(
        lob_data,
        (batch_size, seq_len, config.lob_features),
        &device
    )?;
    let market_tensor = Tensor::from_vec(
        market_data,
        (batch_size, seq_len, config.market_features),
        &device
    )?;
    
    // 預熱
    info!("🔥 預熱階段...");
    for _ in 0..10 {
        let _ = model.predict(&lob_tensor, &market_tensor)?;
    }
    
    // 性能測試
    info!("⚡ 性能測試階段...");
    let num_iterations = 100;
    let mut total_time = 0u64;
    
    for i in 0..num_iterations {
        let start = now_micros();
        let predictions = model.predict(&lob_tensor, &market_tensor)?;
        let elapsed = now_micros() - start;
        total_time += elapsed;
        
        if i % 20 == 0 {
            info!("Iteration {}: {}μs", i, elapsed);
            
            // 顯示第一個預測結果
            if let Some((&horizon, pred)) = predictions.iter().next() {
                let pred_data = pred.to_vec2::<f32>()?;
                info!("Prediction for {}s horizon: {:?}", horizon, &pred_data[0]);
            }
        }
    }
    
    let avg_latency = total_time / num_iterations;
    let per_sample_latency = avg_latency; // 因為批處理，實際單樣本延遲可能更低
    
    info!("📊 性能測試結果:");
    info!("   平均批次延遲: {}μs", avg_latency);
    info!("   估計單樣本延遲: {}μs", per_sample_latency);
    info!("   每秒吞吐量: {:.0} samples/s", 1_000_000.0 / per_sample_latency as f64);
    
    // 性能評估
    if per_sample_latency < 50 {
        info!("✅ 性能目標達成: {}μs < 50μs", per_sample_latency);
    } else {
        warn!("⚠️  性能未達標: {}μs >= 50μs", per_sample_latency);
        info!("💡 優化建議:");
        info!("   - 減少模型層數或維度");
        info!("   - 使用模型量化");
        info!("   - 啟用GPU加速");
        info!("   - 使用Flash Attention");
    }
    
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = Args::parse();
    
    info!("🚀 LOB Transformer模型測試");
    info!("配置: sequence_length={}, batch_size={}, use_gpu={}", 
          args.sequence_length, args.batch_size, args.use_gpu);
    
    if args.test_inference {
        test_inference_performance(&args).await?;
    } else {
        info!("使用 --test-inference 來測試推理性能");
    }
    
    Ok(())
}