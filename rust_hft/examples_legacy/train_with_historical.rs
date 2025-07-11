/*!
 * 歷史資料預訓練 + 實時微調系統
 * 
 * 支援：
 * 1. 從 ClickHouse 載入歷史資料
 * 2. 歷史資料預訓練
 * 3. 實時資料微調
 * 4. 模型效能評估
 */

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use clap::Parser;
use rust_hft::database::{ClickHouseClient, ClickHouseConfig, LobDepthRow, TradeDataRow};
use rust_hft::ml::features::FeatureExtractor;
use rust_hft::core::{OrderBook, OrderBookUpdate, PriceLevel, Side, Price, Quantity};
use rust_hft::integrations::bitget_connector::BitgetConnector;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use candle_core::{Device, Tensor, DType};
use candle_nn::{linear, Linear, Module, Optimizer, VarBuilder, VarMap};
use ordered_float::OrderedFloat;
use rust_decimal::Decimal;

/// 歷史資料訓練參數
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// 交易對符號
    #[arg(short, long)]
    symbol: String,

    /// ClickHouse 連接 URL
    #[arg(long, default_value = "http://localhost:8123")]
    clickhouse_url: String,

    /// 歷史資料天數
    #[arg(long, default_value = "7")]
    historical_days: i64,

    /// 預訓練批次大小
    #[arg(long, default_value = "512")]
    pretrain_batch_size: usize,

    /// 微調批次大小
    #[arg(long, default_value = "64")]
    finetune_batch_size: usize,

    /// 預訓練學習率
    #[arg(long, default_value = "0.001")]
    pretrain_lr: f64,

    /// 微調學習率
    #[arg(long, default_value = "0.0001")]
    finetune_lr: f64,

    /// 預訓練 epochs
    #[arg(long, default_value = "10")]
    pretrain_epochs: usize,

    /// 是否啟用實時微調
    #[arg(long)]
    enable_live_training: bool,

    /// 模型保存路徑
    #[arg(long, default_value = "./models")]
    model_save_path: String,

    /// 特徵維度
    #[arg(long, default_value = "64")]
    feature_dim: usize,

    /// 隱藏層維度
    #[arg(long, default_value = "128")]
    hidden_dim: usize,

    /// 預測時間窗口 (秒)
    #[arg(long, default_value = "5")]
    prediction_window: u64,
}

/// 訓練資料點
#[derive(Debug, Clone)]
struct TrainingDataPoint {
    /// 特徵向量
    features: Vec<f64>,
    /// 目標值 (價格變化)
    target: f64,
    /// 時間戳
    timestamp: DateTime<Utc>,
    /// 元資料
    metadata: HashMap<String, String>,
}

/// 訓練資料集
#[derive(Debug)]
struct TrainingDataset {
    data: Vec<TrainingDataPoint>,
    feature_dim: usize,
    normalize_features: bool,
    feature_mean: Vec<f64>,
    feature_std: Vec<f64>,
}

impl TrainingDataset {
    fn new(feature_dim: usize, normalize_features: bool) -> Self {
        Self {
            data: Vec::new(),
            feature_dim,
            normalize_features,
            feature_mean: vec![0.0; feature_dim],
            feature_std: vec![1.0; feature_dim],
        }
    }

    /// 添加訓練樣本
    fn add_sample(&mut self, sample: TrainingDataPoint) {
        if sample.features.len() != self.feature_dim {
            warn!("Feature dimension mismatch: expected {}, got {}", 
                  self.feature_dim, sample.features.len());
            return;
        }
        self.data.push(sample);
    }

    /// 計算特徵標準化參數
    fn compute_normalization(&mut self) {
        if !self.normalize_features || self.data.is_empty() {
            return;
        }

        // 計算均值
        for i in 0..self.feature_dim {
            let sum: f64 = self.data.iter()
                .map(|sample| sample.features[i])
                .sum();
            self.feature_mean[i] = sum / self.data.len() as f64;
        }

        // 計算標準差
        for i in 0..self.feature_dim {
            let variance: f64 = self.data.iter()
                .map(|sample| (sample.features[i] - self.feature_mean[i]).powi(2))
                .sum::<f64>() / self.data.len() as f64;
            self.feature_std[i] = variance.sqrt().max(1e-8); // 避免除零
        }
    }

    /// 標準化特徵
    fn normalize_sample(&self, features: &mut [f64]) {
        if !self.normalize_features {
            return;
        }

        for i in 0..features.len().min(self.feature_dim) {
            features[i] = (features[i] - self.feature_mean[i]) / self.feature_std[i];
        }
    }

    /// 轉換為 Tensor
    fn to_tensors(&self, device: &Device) -> Result<(Tensor, Tensor)> {
        if self.data.is_empty() {
            return Err(anyhow::anyhow!("Empty dataset"));
        }

        let batch_size = self.data.len();
        let mut features_data = Vec::with_capacity(batch_size * self.feature_dim);
        let mut targets_data = Vec::with_capacity(batch_size);

        for sample in &self.data {
            let mut normalized_features = sample.features.clone();
            self.normalize_sample(&mut normalized_features);
            features_data.extend(normalized_features);
            targets_data.push(sample.target);
        }

        let features_tensor = Tensor::from_vec(
            features_data,
            (batch_size, self.feature_dim),
            device,
        )?;

        let targets_tensor = Tensor::from_vec(
            targets_data,
            (batch_size, 1),
            device,
        )?;

        Ok((features_tensor, targets_tensor))
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// 簡單的前饋神經網絡模型
#[derive(Debug)]
struct PricePredictionModel {
    layer1: Linear,
    layer2: Linear,
    layer3: Linear,
    device: Device,
}

impl PricePredictionModel {
    fn new(input_dim: usize, hidden_dim: usize, vs: VarBuilder) -> Result<Self> {
        let layer1 = linear(input_dim, hidden_dim, vs.pp("layer1"))?;
        let layer2 = linear(hidden_dim, hidden_dim, vs.pp("layer2"))?;
        let layer3 = linear(hidden_dim, 1, vs.pp("layer3"))?;
        let device = vs.device().clone();

        Ok(Self {
            layer1,
            layer2,
            layer3,
            device,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = self.layer1.forward(x)?;
        let x = x.relu()?;
        let x = self.layer2.forward(&x)?;
        let x = x.relu()?;
        let x = self.layer3.forward(&x)?;
        Ok(x)
    }
}

/// 歷史資料載入器
struct HistoricalDataLoader {
    client: Arc<ClickHouseClient>,
    feature_extractor: FeatureExtractor,
}

impl HistoricalDataLoader {
    fn new(client: Arc<ClickHouseClient>) -> Self {
        Self {
            client,
            feature_extractor: FeatureExtractor::new(),
        }
    }

    /// 從 ClickHouse 載入歷史資料
    async fn load_historical_data(
        &self,
        symbol: &str,
        days: i64,
        feature_dim: usize,
        prediction_window: u64,
    ) -> Result<TrainingDataset> {
        let end_time = Utc::now();
        let start_time = end_time - ChronoDuration::days(days);

        info!("Loading historical data from {} to {}", start_time, end_time);

        // 載入 LOB 深度資料
        let lob_data = self.client
            .query_lob_depth(symbol, start_time, end_time, None)
            .await?;

        info!("Loaded {} LOB depth records", lob_data.len());

        if lob_data.is_empty() {
            return Err(anyhow::anyhow!("No historical data found for symbol {}", symbol));
        }

        // 轉換為訓練資料集
        let mut dataset = TrainingDataset::new(feature_dim, true);
        let mut orderbook = OrderBook::new(symbol.to_string());

        for (i, row) in lob_data.iter().enumerate() {
            // 重建訂單簿狀態
            let update = self.convert_lob_row_to_update(row)?;
            orderbook.update(&update)?;

            // 提取特徵
            let features = self.extract_features_from_orderbook(&orderbook, &update)?;
            
            // 計算未來價格變化作為目標
            let target = if i + 1 < lob_data.len() {
                let future_mid_price = lob_data[i + 1].mid_price;
                let current_mid_price = row.mid_price;
                (future_mid_price - current_mid_price) / current_mid_price
            } else {
                0.0 // 最後一個樣本沒有目標
            };

            let data_point = TrainingDataPoint {
                features: features[..feature_dim.min(features.len())].to_vec(),
                target,
                timestamp: row.timestamp,
                metadata: HashMap::new(),
            };

            dataset.add_sample(data_point);
        }

        // 計算標準化參數
        dataset.compute_normalization();

        info!("Generated {} training samples", dataset.len());
        Ok(dataset)
    }

    /// 轉換 LOB 行為訂單簿更新
    fn convert_lob_row_to_update(&self, row: &LobDepthRow) -> Result<OrderBookUpdate> {
        let mut bids = Vec::new();
        let mut asks = Vec::new();

        // 處理買盤
        for (price, qty) in row.bid_prices.iter().zip(row.bid_quantities.iter()) {
            if *qty > 0.0 {
                bids.push(PriceLevel {
                    price: Price::from(OrderedFloat(*price)),
                    quantity: Quantity::try_from(*qty)?,
                    side: Side::Bid,
                });
            }
        }

        // 處理賣盤
        for (price, qty) in row.ask_prices.iter().zip(row.ask_quantities.iter()) {
            if *qty > 0.0 {
                asks.push(PriceLevel {
                    price: Price::from(OrderedFloat(*price)),
                    quantity: Quantity::try_from(*qty)?,
                    side: Side::Ask,
                });
            }
        }

        Ok(OrderBookUpdate {
            symbol: row.symbol.clone(),
            bids,
            asks,
            timestamp: row.timestamp.timestamp_micros() as u64,
            sequence_start: row.sequence,
            sequence_end: row.sequence,
            is_snapshot: row.is_snapshot != 0,
        })
    }

    /// 從訂單簿提取特徵
    fn extract_features_from_orderbook(&self, orderbook: &OrderBook, update: &OrderBookUpdate) -> Result<Vec<f64>> {
        // 基礎價格特徵
        let mid_price = orderbook.mid_price().unwrap_or(0.0);
        let spread = orderbook.spread().unwrap_or(0.0);
        let relative_spread = if mid_price > 0.0 { spread / mid_price } else { 0.0 };

        // 深度特徵
        let bid_volume: f64 = orderbook.bids.values().take(5).map(|level| level.quantity.to_f64().unwrap_or(0.0)).sum();
        let ask_volume: f64 = orderbook.asks.values().take(5).map(|level| level.quantity.to_f64().unwrap_or(0.0)).sum();
        let volume_imbalance = if bid_volume + ask_volume > 0.0 {
            (bid_volume - ask_volume) / (bid_volume + ask_volume)
        } else {
            0.0
        };

        // 價格水平特徵
        let best_bid = orderbook.best_bid_price().unwrap_or(0.0);
        let best_ask = orderbook.best_ask_price().unwrap_or(0.0);

        // 微觀結構特徵
        let bid_ask_ratio = if ask_volume > 0.0 { bid_volume / ask_volume } else { 1.0 };
        
        // 組合特徵向量
        let mut features = vec![
            mid_price,
            spread,
            relative_spread,
            bid_volume,
            ask_volume,
            volume_imbalance,
            best_bid,
            best_ask,
            bid_ask_ratio,
        ];

        // 添加前 5 檔價格和數量
        let mut bid_iter = orderbook.bids.iter().take(5);
        let mut ask_iter = orderbook.asks.iter().take(5);
        
        for _ in 0..5 {
            if let Some((price, level)) = bid_iter.next() {
                features.push(price.into_inner());
                features.push(level.quantity.to_f64().unwrap_or(0.0));
            } else {
                features.push(0.0);
                features.push(0.0);
            }
        }

        for _ in 0..5 {
            if let Some((price, level)) = ask_iter.next() {
                features.push(price.into_inner());
                features.push(level.quantity.to_f64().unwrap_or(0.0));
            } else {
                features.push(0.0);
                features.push(0.0);
            }
        }

        Ok(features)
    }
}

/// 歷史資料訓練器
struct HistoricalTrainer {
    data_loader: HistoricalDataLoader,
    model: Option<PricePredictionModel>,
    device: Device,
    varmap: VarMap,
}

impl HistoricalTrainer {
    fn new(client: Arc<ClickHouseClient>) -> Result<Self> {
        let device = Device::Cpu; // 可以改為 Device::Cuda(0) 如果有 GPU
        let varmap = VarMap::new();

        Ok(Self {
            data_loader: HistoricalDataLoader::new(client),
            model: None,
            device,
            varmap,
        })
    }

    /// 執行歷史資料預訓練
    async fn pretrain(
        &mut self,
        args: &Args,
    ) -> Result<()> {
        info!("Starting historical data pretraining...");

        // 載入歷史資料
        let dataset = self.data_loader
            .load_historical_data(&args.symbol, args.historical_days, args.feature_dim, args.prediction_window)
            .await?;

        if dataset.is_empty() {
            return Err(anyhow::anyhow!("No training data available"));
        }

        // 初始化模型
        let vs = VarBuilder::from_varmap(&self.varmap, DType::F32, &self.device);
        self.model = Some(PricePredictionModel::new(args.feature_dim, args.hidden_dim, vs)?);

        // 轉換為張量
        let (features, targets) = dataset.to_tensors(&self.device)?;
        
        info!("Features shape: {:?}", features.shape());
        info!("Targets shape: {:?}", targets.shape());

        // 設置優化器
        let mut optimizer = candle_nn::AdamW::new(
            self.varmap.all_vars(),
            candle_nn::ParamsAdamW {
                lr: args.pretrain_lr,
                ..Default::default()
            },
        )?;

        // 訓練循環
        for epoch in 0..args.pretrain_epochs {
            let start_time = Instant::now();
            
            if let Some(ref model) = self.model {
                // 前向傳播
                let predictions = model.forward(&features)?;
                
                // 計算損失 (MSE)
                let loss = predictions.sub(&targets)?.sqr()?.mean_all()?;
                
                // 反向傳播
                optimizer.backward_step(&loss)?;
                
                let loss_val: f32 = loss.to_scalar()?;
                let epoch_time = start_time.elapsed();
                
                info!("Epoch {}/{}: Loss = {:.6}, Time = {:?}", 
                      epoch + 1, args.pretrain_epochs, loss_val, epoch_time);
                
                // 每 10 個 epoch 保存檢查點
                if (epoch + 1) % 10 == 0 {
                    self.save_checkpoint(&args.model_save_path, epoch + 1).await?;
                }
            }
        }

        info!("Pretraining completed successfully!");
        Ok(())
    }

    /// 保存模型檢查點
    async fn save_checkpoint(&self, save_path: &str, epoch: usize) -> Result<()> {
        std::fs::create_dir_all(save_path)?;
        let checkpoint_path = format!("{}/checkpoint_epoch_{}.safetensors", save_path, epoch);
        
        self.varmap.save(&checkpoint_path)?;
        info!("Checkpoint saved: {}", checkpoint_path);
        Ok(())
    }

    /// 評估模型性能
    async fn evaluate(&self, dataset: &TrainingDataset) -> Result<f64> {
        if self.model.is_none() {
            return Err(anyhow::anyhow!("Model not initialized"));
        }

        let (features, targets) = dataset.to_tensors(&self.device)?;
        
        if let Some(ref model) = self.model {
            let predictions = model.forward(&features)?;
            let loss = predictions.sub(&targets)?.sqr()?.mean_all()?;
            let loss_val: f32 = loss.to_scalar()?;
            Ok(loss_val as f64)
        } else {
            Err(anyhow::anyhow!("Model evaluation failed"))
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    info!("Starting historical training workflow...");
    info!("Symbol: {}", args.symbol);
    info!("Historical days: {}", args.historical_days);
    info!("Pretrain batch size: {}", args.pretrain_batch_size);
    info!("Pretrain epochs: {}", args.pretrain_epochs);
    info!("Feature dimension: {}", args.feature_dim);
    info!("Hidden dimension: {}", args.hidden_dim);

    // 連接 ClickHouse
    let config = ClickHouseConfig {
        url: args.clickhouse_url.clone(),
        database: "hft_db".to_string(),
        username: "hft_user".to_string(),
        password: "hft_password".to_string(),
        batch_size: 10000,
        max_connections: 10,
        flush_interval_ms: 5000,
    };

    let client = Arc::new(ClickHouseClient::new(config)?);
    client.test_connection().await?;
    info!("ClickHouse connection established");

    // 創建訓練器
    let mut trainer = HistoricalTrainer::new(client.clone())?;

    // 執行歷史資料預訓練
    trainer.pretrain(&args).await?;

    // 保存最終模型
    trainer.save_checkpoint(&args.model_save_path, args.pretrain_epochs).await?;

    // 如果啟用實時微調
    if args.enable_live_training {
        info!("Starting live training mode...");
        
        // TODO: 實現實時資料流和微調邏輯
        // 這裡可以集成現有的 BitgetConnector 來獲取實時資料
        // 並進行增量學習
        
        warn!("Live training mode not yet implemented");
    }

    info!("Training workflow completed successfully!");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_training_dataset() {
        let mut dataset = TrainingDataset::new(3, true);
        
        let sample = TrainingDataPoint {
            features: vec![1.0, 2.0, 3.0],
            target: 0.1,
            timestamp: Utc::now(),
            metadata: HashMap::new(),
        };
        
        dataset.add_sample(sample);
        assert_eq!(dataset.len(), 1);
        assert!(!dataset.is_empty());
    }

    #[test]
    fn test_feature_normalization() {
        let mut dataset = TrainingDataset::new(2, true);
        
        // 添加測試樣本
        dataset.add_sample(TrainingDataPoint {
            features: vec![1.0, 2.0],
            target: 0.1,
            timestamp: Utc::now(),
            metadata: HashMap::new(),
        });
        
        dataset.add_sample(TrainingDataPoint {
            features: vec![3.0, 4.0],
            target: 0.2,
            timestamp: Utc::now(),
            metadata: HashMap::new(),
        });
        
        dataset.compute_normalization();
        
        // 檢查均值和標準差
        assert_eq!(dataset.feature_mean[0], 2.0);
        assert_eq!(dataset.feature_mean[1], 3.0);
    }
}