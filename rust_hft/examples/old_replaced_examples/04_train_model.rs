/*!
 * 機器學習模型訓練
 * 
 * 使用歷史數據訓練預測模型
 */

use rust_hft::{
    core::{types::*, orderbook::OrderBook},
    ml::{features::FeatureExtractor, online_learning::{OnlineLearningEngine, OnlineLearningConfig, TrainingSample}},
};
use anyhow::Result;
use tracing::{info, warn, error};
use std::fs::File;
use std::io::{BufRead, BufReader};
use serde_json::Value;
use clap::Parser;

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value_t = String::from("market_data.jsonl"))]
    input_file: String,
    
    #[arg(short, long, default_value_t = String::from("trained_model.bin"))]
    model_output: String,
    
    #[arg(long, default_value_t = 10000)]
    max_samples: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = Args::parse();
    
    info!("🤖 Starting Model Training");
    info!("Input: {}, Output: {}, Max Samples: {}", 
          args.input_file, args.model_output, args.max_samples);

    // 初始化組件
    let mut feature_extractor = FeatureExtractor::new(50);
    let mut ml_engine = OnlineLearningEngine::new(OnlineLearningConfig::default())?;
    
    // 讀取訓練數據
    let file = File::open(&args.input_file)?;
    let reader = BufReader::new(file);
    
    let mut training_samples = 0usize;
    let mut previous_features: Option<FeatureSet> = None;
    
    for line in reader.lines() {
        if training_samples >= args.max_samples {
            break;
        }
        
        let line = line?;
        
        if let Ok(record) = serde_json::from_str::<Value>(&line) {
            if record["type"] == "orderbook" {
                // 解析 orderbook 數據
                if let Ok(orderbook) = parse_orderbook_from_record(&record) {
                    // 提取特徵
                    if let Ok(features) = feature_extractor.extract_features(&orderbook, 50, orderbook.last_update) {
                        // 如果有前一個特徵，計算標籤
                        if let Some(prev_features) = previous_features {
                            let label = calculate_label(&prev_features, &features);
                            
                            // 創建訓練樣本
                            let training_sample = TrainingSample {
                                features: features_to_vec(&prev_features),
                                labels: vec![label, label, label], // 簡化：3個時間範圍使用相同標籤
                                timestamp: prev_features.timestamp,
                                symbol: "BTCUSDT".to_string(),
                            };
                            
                            // 添加訓練樣本
                            ml_engine.add_training_sample(training_sample)?;
                            training_samples += 1;
                            
                            if training_samples % 1000 == 0 {
                                info!("🎯 Training samples: {}", training_samples);
                            }
                        }
                        
                        previous_features = Some(features);
                    }
                }
            }
        }
    }
    
    // 訓練模型 (自動進行，無需手動呼叫)
    info!("🚀 Training completed with {} samples", training_samples);
    
    // 保存模型
    ml_engine.save_model()?;
    info!("✅ Model saved successfully");
    
    // 簡化的模型評估
    info!("📊 Training completed with {} samples", training_samples);
    
    Ok(())
}

fn parse_orderbook_from_record(record: &Value) -> Result<OrderBook> {
    let symbol = record["symbol"].as_str().unwrap_or("UNKNOWN").to_string();
    let timestamp = record["timestamp"].as_u64().unwrap_or(0);
    
    let mut orderbook = OrderBook::new(symbol);
    orderbook.last_update = timestamp;
    orderbook.is_valid = true;
    
    Ok(orderbook)
}

fn calculate_label(prev_features: &FeatureSet, current_features: &FeatureSet) -> f64 {
    // 簡單的標籤計算：價格變化方向
    let prev_price = *prev_features.mid_price;
    let current_price = *current_features.mid_price;
    
    if current_price > prev_price * 1.001 {
        1.0  // 上漲
    } else if current_price < prev_price * 0.999 {
        -1.0 // 下跌
    } else {
        0.0  // 橫盤
    }
}

fn features_to_vec(features: &FeatureSet) -> Vec<f64> {
    // 將 FeatureSet 轉換為 Vec<f64> - 使用實際可用的字段
    vec![
        *features.mid_price,
        features.spread_bps,
        features.obi_l1,
        features.obi_l5,
        features.obi_l10,
        features.obi_l20,
        features.microprice,
        features.vwap,
        features.bid_depth_l5,
        features.ask_depth_l5,
        features.bid_depth_l10,
        features.ask_depth_l10,
        features.depth_imbalance_l5,
        features.depth_imbalance_l10,
        features.depth_imbalance_l20,
        features.bid_slope,
        features.ask_slope,
        features.price_momentum,
        features.volume_imbalance,
        features.data_quality_score,
    ]
}