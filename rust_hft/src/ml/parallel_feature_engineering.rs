/*!
 * 🧠 DL/RL驅動的並行特徵工程引擎
 * 
 * 專為HFT量化交易設計的AI驅動特徵計算系統
 * 
 * 核心特性：
 * - 深度學習特徵提取 (CNN/LSTM/Transformer)
 * - 強化學習狀態表示
 * - 注意力機制和自監督學習
 * - 圖神經網絡(GNN)市場微觀結構建模
 * - 實時神經網絡推理
 * - GPU加速特徵計算
 * - 自適應特徵選擇和生成
 */

use crate::core::types::*;
use crate::utils::parallel_processing::{ParallelProcessor, ParallelConfig, FeatureTask, OHLCVData};
use crate::utils::memory_optimization::{ZeroAllocVec, CowPtr, MemoryPool};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use serde::{Serialize, Deserialize};
use tracing::{debug, info, warn, error, instrument};

// Deep Learning imports
use candle_core::{Tensor, Device, DType};
use candle_nn::{Module, VarBuilder, Linear, LSTM, Conv1d, Conv2d};
use candle_transformers::models::bert::BertModel;
use std::sync::Mutex;

/// DL/RL特徵工程配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DLRLFeatureConfig {
    /// 並行處理配置
    pub parallel_config: ParallelConfig,
    /// 深度學習模型配置
    pub dl_config: DeepLearningConfig,
    /// 強化學習配置
    pub rl_config: ReinforcementLearningConfig,
    /// 特徵提取器類型
    pub extractors: Vec<FeatureExtractorType>,
    /// 序列長度（用於時序模型）
    pub sequence_length: usize,
    /// 嵌入維度
    pub embedding_dim: usize,
    /// 注意力頭數
    pub attention_heads: usize,
    /// 實時推理模式
    pub realtime_inference: bool,
    /// GPU使用配置
    pub gpu_config: GPUConfig,
    /// 特徵緩存大小
    pub cache_size: usize,
}

/// 深度學習配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepLearningConfig {
    /// CNN配置（用於價格圖像特徵）
    pub cnn_config: CNNConfig,
    /// LSTM配置（用於時序特徵）
    pub lstm_config: LSTMConfig,
    /// Transformer配置（用於注意力特徵）
    pub transformer_config: TransformerConfig,
    /// 自編碼器配置（用於表示學習）
    pub autoencoder_config: AutoEncoderConfig,
    /// 圖神經網絡配置（用於市場微觀結構）
    pub gnn_config: GNNConfig,
}

/// 強化學習配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReinforcementLearningConfig {
    /// 狀態表示維度
    pub state_dim: usize,
    /// 動作空間大小
    pub action_space: usize,
    /// 獎勵函數類型
    pub reward_function: RewardFunctionType,
    /// Q網絡配置
    pub q_network_config: QNetworkConfig,
    /// 策略網絡配置
    pub policy_network_config: PolicyNetworkConfig,
    /// 經驗回放緩衝區大小
    pub replay_buffer_size: usize,
    /// 探索策略
    pub exploration_strategy: ExplorationStrategy,
}

/// 特徵提取器類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FeatureExtractorType {
    /// CNN價格圖像特徵提取器
    PriceCNN,
    /// LSTM時序特徵提取器
    SequenceLSTM,
    /// Transformer注意力特徵提取器
    AttentionTransformer,
    /// 自編碼器降維特徵提取器
    AutoEncoder,
    /// 圖神經網絡市場結構特徵提取器
    MarketGNN,
    /// 強化學習狀態特徵提取器
    RLStateEncoder,
    /// 多模態融合特徵提取器
    MultiModalFusion,
}

/// CNN配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CNNConfig {
    pub input_channels: usize,
    pub output_channels: Vec<usize>,
    pub kernel_sizes: Vec<usize>,
    pub pooling_sizes: Vec<usize>,
    pub dropout_rate: f64,
}

/// LSTM配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LSTMConfig {
    pub hidden_size: usize,
    pub num_layers: usize,
    pub dropout_rate: f64,
    pub bidirectional: bool,
}

/// Transformer配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformerConfig {
    pub d_model: usize,
    pub nhead: usize,
    pub num_layers: usize,
    pub dim_feedforward: usize,
    pub dropout_rate: f64,
}

/// 自編碼器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoEncoderConfig {
    pub encoder_dims: Vec<usize>,
    pub latent_dim: usize,
    pub decoder_dims: Vec<usize>,
    pub activation: String,
}

/// 圖神經網絡配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GNNConfig {
    pub node_features: usize,
    pub edge_features: usize,
    pub hidden_dims: Vec<usize>,
    pub aggregation_type: String,
    pub num_layers: usize,
}

/// 獎勵函數類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RewardFunctionType {
    SharpeRatio,
    PnL,
    MaxDrawdown,
    CalmarRatio,
    CustomMultiObjective,
}

/// Q網絡配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QNetworkConfig {
    pub hidden_dims: Vec<usize>,
    pub learning_rate: f64,
    pub discount_factor: f64,
    pub target_update_freq: usize,
}

/// 策略網絡配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyNetworkConfig {
    pub hidden_dims: Vec<usize>,
    pub learning_rate: f64,
    pub entropy_coeff: f64,
}

/// 探索策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExplorationStrategy {
    EpsilonGreedy { epsilon: f64, decay: f64 },
    UCB { c: f64 },
    ThompsonSampling,
    NoisyNetworks,
}

/// GPU配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GPUConfig {
    pub enabled: bool,
    pub device_id: Option<usize>,
    pub memory_fraction: f64,
    pub mixed_precision: bool,
}

impl Default for DLRLFeatureConfig {
    fn default() -> Self {
        Self {
            parallel_config: ParallelConfig::default(),
            dl_config: DeepLearningConfig::default(),
            rl_config: ReinforcementLearningConfig::default(),
            extractors: vec![
                FeatureExtractorType::PriceCNN,
                FeatureExtractorType::SequenceLSTM,
                FeatureExtractorType::AttentionTransformer,
                FeatureExtractorType::RLStateEncoder,
            ],
            sequence_length: 60,
            embedding_dim: 128,
            attention_heads: 8,
            realtime_inference: true,
            gpu_config: GPUConfig::default(),
            cache_size: 10000,
        }
    }
}

impl Default for DeepLearningConfig {
    fn default() -> Self {
        Self {
            cnn_config: CNNConfig::default(),
            lstm_config: LSTMConfig::default(),
            transformer_config: TransformerConfig::default(),
            autoencoder_config: AutoEncoderConfig::default(),
            gnn_config: GNNConfig::default(),
        }
    }
}

impl Default for ReinforcementLearningConfig {
    fn default() -> Self {
        Self {
            state_dim: 256,
            action_space: 3, // 买入、卖出、持有
            reward_function: RewardFunctionType::SharpeRatio,
            q_network_config: QNetworkConfig::default(),
            policy_network_config: PolicyNetworkConfig::default(),
            replay_buffer_size: 100000,
            exploration_strategy: ExplorationStrategy::EpsilonGreedy { epsilon: 0.1, decay: 0.995 },
        }
    }
}

impl Default for CNNConfig {
    fn default() -> Self {
        Self {
            input_channels: 5, // OHLCV
            output_channels: vec![32, 64, 128],
            kernel_sizes: vec![3, 5, 7],
            pooling_sizes: vec![2, 2, 2],
            dropout_rate: 0.1,
        }
    }
}

impl Default for LSTMConfig {
    fn default() -> Self {
        Self {
            hidden_size: 128,
            num_layers: 2,
            dropout_rate: 0.1,
            bidirectional: true,
        }
    }
}

impl Default for TransformerConfig {
    fn default() -> Self {
        Self {
            d_model: 256,
            nhead: 8,
            num_layers: 6,
            dim_feedforward: 1024,
            dropout_rate: 0.1,
        }
    }
}

impl Default for AutoEncoderConfig {
    fn default() -> Self {
        Self {
            encoder_dims: vec![256, 128, 64],
            latent_dim: 32,
            decoder_dims: vec![64, 128, 256],
            activation: "relu".to_string(),
        }
    }
}

impl Default for GNNConfig {
    fn default() -> Self {
        Self {
            node_features: 64,
            edge_features: 16,
            hidden_dims: vec![128, 256, 128],
            aggregation_type: "mean".to_string(),
            num_layers: 3,
        }
    }
}

impl Default for QNetworkConfig {
    fn default() -> Self {
        Self {
            hidden_dims: vec![256, 512, 256],
            learning_rate: 0.001,
            discount_factor: 0.99,
            target_update_freq: 1000,
        }
    }
}

impl Default for PolicyNetworkConfig {
    fn default() -> Self {
        Self {
            hidden_dims: vec![256, 512, 256],
            learning_rate: 0.0003,
            entropy_coeff: 0.01,
        }
    }
}

impl Default for GPUConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            device_id: None,
            memory_fraction: 0.8,
            mixed_precision: true,
        }
    }
}

/// DL/RL驅動的並行特徵工程器
pub struct DLRLFeatureEngine {
    /// 並行處理器
    processor: ParallelProcessor,
    /// 配置
    config: DLRLFeatureConfig,
    /// 特徵緩存
    feature_cache: Arc<FeatureCache>,
    /// 統計計數器
    stats: Arc<FeatureStats>,
    /// 深度學習模型
    dl_models: Arc<Mutex<DLModels>>,
    /// 強化學習智能體
    rl_agent: Arc<Mutex<RLAgent>>,
    /// GPU設備
    device: Device,
    /// 特徵緩衝區（用於批處理）
    feature_buffer: Arc<Mutex<FeatureBuffer>>,
}

/// 特徵緩存
struct FeatureCache {
    cache: parking_lot::RwLock<HashMap<String, CachedFeature>>,
    max_size: usize,
    hits: AtomicU64,
    misses: AtomicU64,
}

/// 緩存的特徵
#[derive(Debug, Clone)]
struct CachedFeature {
    data: Vec<f64>,
    timestamp: u64,
    computation_cost: u64, // 微秒
}

/// 特徵統計
struct FeatureStats {
    total_features_computed: AtomicU64,
    total_computation_time: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
}

impl FeatureCache {
    fn new(max_size: usize) -> Self {
        Self {
            cache: parking_lot::RwLock::new(HashMap::new()),
            max_size,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    fn get(&self, key: &str) -> Option<CachedFeature> {
        let cache = self.cache.read();
        if let Some(feature) = cache.get(key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            Some(feature.clone())
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    fn put(&self, key: String, feature: CachedFeature) {
        let mut cache = self.cache.write();
        
        // 簡單的LRU策略：如果緩存滿了，清除最舊的條目
        if cache.len() >= self.max_size {
            if let Some(oldest_key) = cache.keys()
                .min_by_key(|k| cache.get(*k).map(|f| f.timestamp).unwrap_or(0))
                .cloned() {
                cache.remove(&oldest_key);
            }
        }
        
        cache.insert(key, feature);
    }

    fn get_hit_rate(&self) -> f64 {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        if hits + misses > 0 {
            hits as f64 / (hits + misses) as f64
        } else {
            0.0
        }
    }
}

impl FeatureStats {
    fn new() -> Self {
        Self {
            total_features_computed: AtomicU64::new(0),
            total_computation_time: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
        }
    }

    fn record_computation(&self, count: u64, time_us: u64) {
        self.total_features_computed.fetch_add(count, Ordering::Relaxed);
        self.total_computation_time.fetch_add(time_us, Ordering::Relaxed);
    }
}

impl ParallelFeatureEngine {
    /// 創建新的並行特徵工程器
    pub fn new(config: FeatureEngineeringConfig) -> anyhow::Result<Self> {
        let processor = ParallelProcessor::new(config.parallel_config.clone())?;
        let feature_cache = Arc::new(FeatureCache::new(config.cache_size));
        let stats = Arc::new(FeatureStats::new());

        Ok(Self {
            processor,
            config,
            feature_cache,
            stats,
        })
    }

    /// 並行計算所有特徵
    #[instrument(skip(self, ohlcv_data))]
    pub fn compute_all_features(&self, ohlcv_data: &[OHLCVData]) -> FeatureMatrix {
        let start_time = std::time::Instant::now();

        // 並行計算基礎特徵
        let (price_features, volume_features, technical_features, statistical_features) = rayon::join_all([
            || self.compute_price_features(ohlcv_data),
            || self.compute_volume_features(ohlcv_data),
            || self.compute_technical_features(ohlcv_data),
            || self.compute_statistical_features(ohlcv_data),
        ]);

        let mut features = FeatureMatrix::new();
        features.merge(price_features());
        features.merge(volume_features());
        features.merge(technical_features());
        features.merge(statistical_features());

        // 計算交叉特徵（如果啟用）
        if self.config.cross_features {
            let cross_features = self.compute_cross_features(&features);
            features.merge(cross_features);
        }

        let elapsed = start_time.elapsed();
        self.stats.record_computation(
            features.feature_count() as u64,
            elapsed.as_micros() as u64
        );

        info!(
            "計算了 {} 個特徵，耗時 {:?}",
            features.feature_count(),
            elapsed
        );

        features
    }

    /// 並行計算價格特徵
    fn compute_price_features(&self, data: &[OHLCVData]) -> FeatureMatrix {
        let mut features = FeatureMatrix::new();

        // 提取價格序列
        let opens: Vec<f64> = data.iter().map(|d| d.open).collect();
        let highs: Vec<f64> = data.iter().map(|d| d.high).collect();
        let lows: Vec<f64> = data.iter().map(|d| d.low).collect();
        let closes: Vec<f64> = data.iter().map(|d| d.close).collect();

        // 並行計算價格變化特徵
        let price_features = vec![
            ("returns", self.calculate_returns(&closes)),
            ("log_returns", self.calculate_log_returns(&closes)),
            ("price_range", self.calculate_price_range(&highs, &lows)),
            ("hl_ratio", self.calculate_hl_ratio(&highs, &lows)),
            ("oc_ratio", self.calculate_oc_ratio(&opens, &closes)),
        ].into_par_iter()
        .map(|(name, values)| (name.to_string(), values))
        .collect::<HashMap<String, Vec<f64>>>();

        for (name, values) in price_features {
            features.add_feature(name, values);
        }

        features
    }

    /// 並行計算成交量特徵
    fn compute_volume_features(&self, data: &[OHLCVData]) -> FeatureMatrix {
        let mut features = FeatureMatrix::new();

        let volumes: Vec<f64> = data.iter().map(|d| d.volume).collect();
        let closes: Vec<f64> = data.iter().map(|d| d.close).collect();

        // 並行計算成交量特徵
        let volume_features = vec![
            ("volume_sma_20", self.processor.parallel_moving_average(&volumes, 20)),
            ("volume_ratio", self.calculate_volume_ratio(&volumes)),
            ("vwap", self.processor.calculate_vwap(&closes, &volumes)),
            ("volume_oscillator", self.calculate_volume_oscillator(&volumes)),
        ].into_par_iter()
        .filter(|(_, values)| !values.is_empty())
        .collect::<HashMap<String, Vec<f64>>>();

        for (name, values) in volume_features {
            features.add_feature(name, values);
        }

        features
    }

    /// 並行計算技術指標特徵
    fn compute_technical_features(&self, data: &[OHLCVData]) -> FeatureMatrix {
        let mut features = FeatureMatrix::new();

        let closes: Vec<f64> = data.iter().map(|d| d.close).collect();
        let highs: Vec<f64> = data.iter().map(|d| d.high).collect();
        let lows: Vec<f64> = data.iter().map(|d| d.low).collect();
        let volumes: Vec<f64> = data.iter().map(|d| d.volume).collect();

        // 並行計算多個窗口的移動平均
        let sma_features: HashMap<String, Vec<f64>> = self.config.feature_windows
            .par_iter()
            .map(|&window| {
                let key = format!("sma_{}", window);
                let values = self.get_cached_or_compute(&key, || {
                    self.processor.parallel_moving_average(&closes, window)
                });
                (key, values)
            })
            .filter(|(_, values)| !values.is_empty())
            .collect();

        // 並行計算EMA
        let ema_features: HashMap<String, Vec<f64>> = self.config.feature_windows
            .par_iter()
            .map(|&window| {
                let key = format!("ema_{}", window);
                let values = self.get_cached_or_compute(&key, || {
                    self.calculate_ema(&closes, window)
                });
                (key, values)
            })
            .filter(|(_, values)| !values.is_empty())
            .collect();

        // 其他技術指標
        let other_features = vec![
            ("rsi_14", self.calculate_rsi(&closes, 14)),
            ("macd", self.calculate_macd(&closes)),
            ("atr_14", self.calculate_atr(&highs, &lows, &closes, 14)),
            ("stoch_k", self.calculate_stochastic_k(&highs, &lows, &closes, 14)),
            ("williams_r", self.calculate_williams_r(&highs, &lows, &closes, 14)),
        ].into_par_iter()
        .filter(|(_, values)| !values.is_empty())
        .collect::<HashMap<String, Vec<f64>>>();

        // 合併所有特徵
        for (name, values) in sma_features.into_iter()
            .chain(ema_features.into_iter())
            .chain(other_features.into_iter()) {
            features.add_feature(name, values);
        }

        features
    }

    /// 並行計算統計特徵
    fn compute_statistical_features(&self, data: &[OHLCVData]) -> FeatureMatrix {
        let mut features = FeatureMatrix::new();

        let closes: Vec<f64> = data.iter().map(|d| d.close).collect();

        // 並行計算滾動統計特徵
        let rolling_stats: HashMap<String, Vec<f64>> = self.config.feature_windows
            .par_iter()
            .flat_map(|&window| {
                vec![
                    (format!("std_{}", window), self.calculate_rolling_std(&closes, window)),
                    (format!("skew_{}", window), self.calculate_rolling_skewness(&closes, window)),
                    (format!("kurt_{}", window), self.calculate_rolling_kurtosis(&closes, window)),
                    (format!("var_{}", window), self.calculate_rolling_variance(&closes, window)),
                ]
            })
            .filter(|(_, values)| !values.is_empty())
            .collect();

        for (name, values) in rolling_stats {
            features.add_feature(name, values);
        }

        features
    }

    /// 計算交叉特徵
    fn compute_cross_features(&self, base_features: &FeatureMatrix) -> FeatureMatrix {
        let mut cross_features = FeatureMatrix::new();

        // 獲取基礎特徵名稱
        let feature_names: Vec<String> = base_features.get_feature_names();

        // 並行計算特徵比率
        let ratio_features: HashMap<String, Vec<f64>> = feature_names
            .par_iter()
            .flat_map(|name1| {
                feature_names.par_iter().filter_map(|name2| {
                    if name1 != name2 {
                        if let (Some(values1), Some(values2)) = (
                            base_features.get_feature(name1),
                            base_features.get_feature(name2)
                        ) {
                            let ratios = self.calculate_feature_ratio(values1, values2);
                            if !ratios.is_empty() {
                                return Some((format!("{}_{}_ratio", name1, name2), ratios));
                            }
                        }
                    }
                    None
                })
            })
            .collect();

        for (name, values) in ratio_features {
            cross_features.add_feature(name, values);
        }

        cross_features
    }

    /// 帶緩存的特徵計算
    fn get_cached_or_compute<F>(&self, key: &str, compute_fn: F) -> Vec<f64>
    where
        F: FnOnce() -> Vec<f64>,
    {
        if let Some(cached) = self.feature_cache.get(key) {
            return cached.data;
        }

        let start_time = std::time::Instant::now();
        let result = compute_fn();
        let computation_time = start_time.elapsed().as_micros() as u64;

        // 緩存結果
        let cached_feature = CachedFeature {
            data: result.clone(),
            timestamp: chrono::Utc::now().timestamp() as u64,
            computation_cost: computation_time,
        };
        self.feature_cache.put(key.to_string(), cached_feature);

        result
    }

    // 各種技術指標計算方法
    fn calculate_returns(&self, prices: &[f64]) -> Vec<f64> {
        prices.par_windows(2)
            .map(|w| (w[1] - w[0]) / w[0])
            .collect()
    }

    fn calculate_log_returns(&self, prices: &[f64]) -> Vec<f64> {
        prices.par_windows(2)
            .map(|w| (w[1] / w[0]).ln())
            .collect()
    }

    fn calculate_price_range(&self, highs: &[f64], lows: &[f64]) -> Vec<f64> {
        highs.par_iter()
            .zip(lows.par_iter())
            .map(|(&h, &l)| h - l)
            .collect()
    }

    fn calculate_hl_ratio(&self, highs: &[f64], lows: &[f64]) -> Vec<f64> {
        highs.par_iter()
            .zip(lows.par_iter())
            .map(|(&h, &l)| if l > 0.0 { h / l } else { 0.0 })
            .collect()
    }

    fn calculate_oc_ratio(&self, opens: &[f64], closes: &[f64]) -> Vec<f64> {
        opens.par_iter()
            .zip(closes.par_iter())
            .map(|(&o, &c)| if o > 0.0 { c / o } else { 0.0 })
            .collect()
    }

    fn calculate_volume_ratio(&self, volumes: &[f64]) -> Vec<f64> {
        if volumes.len() < 2 {
            return Vec::new();
        }

        volumes.par_windows(2)
            .map(|w| if w[0] > 0.0 { w[1] / w[0] } else { 0.0 })
            .collect()
    }

    fn calculate_volume_oscillator(&self, volumes: &[f64]) -> Vec<f64> {
        let short_ma = self.processor.parallel_moving_average(volumes, 5);
        let long_ma = self.processor.parallel_moving_average(volumes, 20);

        if short_ma.len() != long_ma.len() {
            return Vec::new();
        }

        short_ma.par_iter()
            .zip(long_ma.par_iter())
            .map(|(&short, &long)| {
                if long > 0.0 {
                    ((short - long) / long) * 100.0
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn calculate_ema(&self, prices: &[f64], period: usize) -> Vec<f64> {
        if prices.len() < period {
            return Vec::new();
        }

        let multiplier = 2.0 / (period + 1) as f64;
        let mut ema = Vec::with_capacity(prices.len() - period + 1);
        
        // 初始EMA值使用SMA
        let initial_sma: f64 = prices[..period].iter().sum::<f64>() / period as f64;
        ema.push(initial_sma);

        // 計算後續EMA值
        for i in period..prices.len() {
            let current_ema = (prices[i] * multiplier) + (ema.last().unwrap() * (1.0 - multiplier));
            ema.push(current_ema);
        }

        ema
    }

    fn calculate_rsi(&self, prices: &[f64], period: usize) -> Vec<f64> {
        self.processor.calculate_rsi(prices, period)
    }

    fn calculate_macd(&self, prices: &[f64]) -> Vec<f64> {
        let ema_12 = self.calculate_ema(prices, 12);
        let ema_26 = self.calculate_ema(prices, 26);

        if ema_12.len() != ema_26.len() || ema_12.is_empty() {
            return Vec::new();
        }

        ema_12.par_iter()
            .zip(ema_26.par_iter())
            .map(|(&fast, &slow)| fast - slow)
            .collect()
    }

    fn calculate_atr(&self, highs: &[f64], lows: &[f64], closes: &[f64], period: usize) -> Vec<f64> {
        if highs.len() != lows.len() || lows.len() != closes.len() || closes.len() < period + 1 {
            return Vec::new();
        }

        // 計算真實範圍
        let true_ranges: Vec<f64> = (1..closes.len())
            .into_par_iter()
            .map(|i| {
                let hl = highs[i] - lows[i];
                let hc = (highs[i] - closes[i - 1]).abs();
                let lc = (lows[i] - closes[i - 1]).abs();
                hl.max(hc).max(lc)
            })
            .collect();

        // 計算ATR（真實範圍的移動平均）
        self.processor.parallel_moving_average(&true_ranges, period)
    }

    fn calculate_stochastic_k(&self, highs: &[f64], lows: &[f64], closes: &[f64], period: usize) -> Vec<f64> {
        if highs.len() != lows.len() || lows.len() != closes.len() || closes.len() < period {
            return Vec::new();
        }

        (period - 1..closes.len())
            .into_par_iter()
            .map(|i| {
                let window_start = i - period + 1;
                let highest = highs[window_start..=i].iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
                let lowest = lows[window_start..=i].iter().fold(f64::INFINITY, |a, &b| a.min(b));
                
                if highest - lowest > 0.0 {
                    ((closes[i] - lowest) / (highest - lowest)) * 100.0
                } else {
                    50.0
                }
            })
            .collect()
    }

    fn calculate_williams_r(&self, highs: &[f64], lows: &[f64], closes: &[f64], period: usize) -> Vec<f64> {
        let stoch_k = self.calculate_stochastic_k(highs, lows, closes, period);
        stoch_k.par_iter().map(|&k| k - 100.0).collect()
    }

    fn calculate_rolling_std(&self, data: &[f64], window: usize) -> Vec<f64> {
        if data.len() < window {
            return Vec::new();
        }

        (window - 1..data.len())
            .into_par_iter()
            .map(|i| {
                let window_data = &data[i - window + 1..=i];
                let mean = window_data.iter().sum::<f64>() / window as f64;
                let variance = window_data.iter()
                    .map(|x| (x - mean).powi(2))
                    .sum::<f64>() / window as f64;
                variance.sqrt()
            })
            .collect()
    }

    fn calculate_rolling_skewness(&self, data: &[f64], window: usize) -> Vec<f64> {
        if data.len() < window {
            return Vec::new();
        }

        (window - 1..data.len())
            .into_par_iter()
            .map(|i| {
                let window_data = &data[i - window + 1..=i];
                let mean = window_data.iter().sum::<f64>() / window as f64;
                let std = {
                    let variance = window_data.iter()
                        .map(|x| (x - mean).powi(2))
                        .sum::<f64>() / window as f64;
                    variance.sqrt()
                };
                
                if std > 0.0 {
                    let skewness = window_data.iter()
                        .map(|x| ((x - mean) / std).powi(3))
                        .sum::<f64>() / window as f64;
                    skewness
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn calculate_rolling_kurtosis(&self, data: &[f64], window: usize) -> Vec<f64> {
        if data.len() < window {
            return Vec::new();
        }

        (window - 1..data.len())
            .into_par_iter()
            .map(|i| {
                let window_data = &data[i - window + 1..=i];
                let mean = window_data.iter().sum::<f64>() / window as f64;
                let std = {
                    let variance = window_data.iter()
                        .map(|x| (x - mean).powi(2))
                        .sum::<f64>() / window as f64;
                    variance.sqrt()
                };
                
                if std > 0.0 {
                    let kurtosis = window_data.iter()
                        .map(|x| ((x - mean) / std).powi(4))
                        .sum::<f64>() / window as f64;
                    kurtosis - 3.0 // 超額峰度
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn calculate_rolling_variance(&self, data: &[f64], window: usize) -> Vec<f64> {
        if data.len() < window {
            return Vec::new();
        }

        (window - 1..data.len())
            .into_par_iter()
            .map(|i| {
                let window_data = &data[i - window + 1..=i];
                let mean = window_data.iter().sum::<f64>() / window as f64;
                window_data.iter()
                    .map(|x| (x - mean).powi(2))
                    .sum::<f64>() / window as f64
            })
            .collect()
    }

    fn calculate_feature_ratio(&self, values1: &[f64], values2: &[f64]) -> Vec<f64> {
        if values1.len() != values2.len() {
            return Vec::new();
        }

        values1.par_iter()
            .zip(values2.par_iter())
            .map(|(&v1, &v2)| if v2.abs() > f64::EPSILON { v1 / v2 } else { 0.0 })
            .collect()
    }

    /// 獲取統計信息
    pub fn get_stats(&self) -> FeatureEngineStats {
        FeatureEngineStats {
            total_features_computed: self.stats.total_features_computed.load(Ordering::Relaxed),
            total_computation_time_us: self.stats.total_computation_time.load(Ordering::Relaxed),
            cache_hit_rate: self.feature_cache.get_hit_rate(),
            parallel_stats: self.processor.get_stats(),
        }
    }
}

/// 特徵矩陣
#[derive(Debug, Clone)]
pub struct FeatureMatrix {
    features: HashMap<String, Vec<f64>>,
}

impl FeatureMatrix {
    pub fn new() -> Self {
        Self {
            features: HashMap::new(),
        }
    }

    pub fn add_feature(&mut self, name: String, values: Vec<f64>) {
        self.features.insert(name, values);
    }

    pub fn get_feature(&self, name: &str) -> Option<&Vec<f64>> {
        self.features.get(name)
    }

    pub fn get_feature_names(&self) -> Vec<String> {
        self.features.keys().cloned().collect()
    }

    pub fn feature_count(&self) -> usize {
        self.features.len()
    }

    pub fn merge(&mut self, other: FeatureMatrix) {
        self.features.extend(other.features);
    }

    /// 轉換為二維矩陣（行=時間點，列=特徵）
    pub fn to_matrix(&self) -> Vec<Vec<f64>> {
        if self.features.is_empty() {
            return Vec::new();
        }

        // 獲取最小長度
        let min_length = self.features.values()
            .map(|v| v.len())
            .min()
            .unwrap_or(0);

        if min_length == 0 {
            return Vec::new();
        }

        let feature_names: Vec<String> = self.features.keys().cloned().collect();
        
        (0..min_length)
            .map(|i| {
                feature_names.iter()
                    .map(|name| self.features[name][i])
                    .collect()
            })
            .collect()
    }
}

/// 特徵工程統計
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureEngineStats {
    pub total_features_computed: u64,
    pub total_computation_time_us: u64,
    pub cache_hit_rate: f64,
    pub parallel_stats: crate::utils::parallel_processing::ParallelStats,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_sample_data(count: usize) -> Vec<OHLCVData> {
        (0..count)
            .map(|i| OHLCVData {
                timestamp: i as u64,
                open: 100.0 + (i as f64 * 0.1),
                high: 102.0 + (i as f64 * 0.1),
                low: 98.0 + (i as f64 * 0.1),
                close: 101.0 + (i as f64 * 0.1),
                volume: 1000.0 + (i as f64 * 10.0),
            })
            .collect()
    }

    #[test]
    fn test_parallel_feature_engine_creation() {
        let config = FeatureEngineeringConfig::default();
        let engine = ParallelFeatureEngine::new(config).unwrap();
        assert!(engine.config.enabled_indicators.len() > 0);
    }

    #[test]
    fn test_price_features_computation() {
        let config = FeatureEngineeringConfig::default();
        let engine = ParallelFeatureEngine::new(config).unwrap();
        let data = create_sample_data(100);

        let features = engine.compute_price_features(&data);
        assert!(features.feature_count() > 0);
        assert!(features.get_feature("returns").is_some());
        assert!(features.get_feature("log_returns").is_some());
    }

    #[test]
    fn test_technical_features_computation() {
        let config = FeatureEngineeringConfig::default();
        let engine = ParallelFeatureEngine::new(config).unwrap();
        let data = create_sample_data(100);

        let features = engine.compute_technical_features(&data);
        assert!(features.feature_count() > 0);
    }

    #[test]
    fn test_complete_feature_computation() {
        let config = FeatureEngineeringConfig::default();
        let engine = ParallelFeatureEngine::new(config).unwrap();
        let data = create_sample_data(100);

        let features = engine.compute_all_features(&data);
        assert!(features.feature_count() > 10);
        
        let matrix = features.to_matrix();
        assert!(matrix.len() > 0);
        assert!(matrix[0].len() > 10);
    }

    #[test]
    fn test_feature_caching() {
        let config = FeatureEngineeringConfig::default();
        let engine = ParallelFeatureEngine::new(config).unwrap();

        // 第一次計算
        let result1 = engine.get_cached_or_compute("test_feature", || vec![1.0, 2.0, 3.0]);
        
        // 第二次計算（應該從緩存獲取）
        let result2 = engine.get_cached_or_compute("test_feature", || vec![4.0, 5.0, 6.0]);
        
        assert_eq!(result1, result2);
        assert_eq!(result1, vec![1.0, 2.0, 3.0]);
    }
}

// 需要實現的DL/RL核心結構（將在後續文件中實現）
struct PriceCNNExtractor;
struct SequenceLSTMExtractor;
struct AttentionTransformerExtractor;
struct AutoEncoderExtractor;
struct MarketGNNExtractor;
struct MultiModalFusionExtractor;
struct QNetwork;
struct PolicyNetwork;
struct ValueNetwork;
struct ReplayBuffer {
    capacity: usize,
}
struct StateEncoder;
struct ActionDecoder;

// 臨時實現（待後續完善）
impl DLModels {
    fn new(_config: &DLRLFeatureConfig, _device: &Device) -> anyhow::Result<Self> {
        Ok(Self {
            price_cnn: None,
            sequence_lstm: None,
            attention_transformer: None,
            autoencoder: None,
            market_gnn: None,
            multimodal_fusion: None,
        })
    }
}

impl RLAgent {
    fn new(_config: &DLRLFeatureConfig, _device: &Device) -> anyhow::Result<Self> {
        Ok(Self {
            q_network: None,
            target_q_network: None,
            policy_network: None,
            value_network: None,
            replay_buffer: ReplayBuffer { capacity: 100000 },
            state_encoder: StateEncoder,
            action_decoder: ActionDecoder,
        })
    }
}

impl FeatureBuffer {
    fn new(capacity: usize) -> anyhow::Result<Self> {
        Ok(Self {
            raw_data: Vec::with_capacity(capacity),
            features: Vec::new(),
            labels: Vec::new(),
            states: Vec::new(),
            actions: Vec::new(),
            rewards: Vec::new(),
            capacity,
            current_size: 0,
        })
    }
}

// DLRLFeatureEngine的核心方法（待實現）
impl DLRLFeatureEngine {
    async fn compute_dl_features(&self, _data: &[OHLCVData]) -> anyhow::Result<EnhancedFeatureMatrix> {
        // 待實現：深度學習特徵提取
        Ok(EnhancedFeatureMatrix::new())
    }
    
    async fn compute_rl_features(&self, _data: &[OHLCVData]) -> anyhow::Result<EnhancedFeatureMatrix> {
        // 待實現：強化學習特徵提取
        Ok(EnhancedFeatureMatrix::new())
    }
    
    async fn compute_traditional_features(&self, data: &[OHLCVData]) -> anyhow::Result<FeatureMatrix> {
        // 保留原有的傳統技術指標計算
        let mut features = FeatureMatrix::new();
        
        let closes: Vec<f64> = data.iter().map(|d| d.close).collect();
        let returns = self.calculate_returns(&closes);
        features.add_feature("returns".to_string(), returns);
        
        Ok(features)
    }
    
    async fn multimodal_feature_fusion(
        &self,
        _dl_features: &EnhancedFeatureMatrix,
        _rl_features: &EnhancedFeatureMatrix,
        traditional_features: &FeatureMatrix,
    ) -> anyhow::Result<EnhancedFeatureMatrix> {
        // 待實現：多模態特徵融合
        Ok(traditional_features.to_enhanced())
    }
}