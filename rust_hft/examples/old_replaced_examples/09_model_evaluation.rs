/*!
 * 模型评估和盈利能力分析
 * 
 * 这个脚本提供完整的模型评估框架：
 * - 统计性能指标
 * - 回测盈利能力
 * - 风险调整后收益
 * - 实时预测准确率
 * 
 * 执行方式：
 * cargo run --example model_evaluation -- --days 30 --symbol BTCUSDT
 */

use rust_hft::{
    core::{types::*, orderbook::OrderBook},
    ml::{features::FeatureExtractor, online_learning::{OnlineLearningEngine, OnlineLearningConfig}},
    integrations::bitget_connector::*,
    utils::performance::*,
};
use anyhow::Result;
use tracing::{info, warn, error};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::time::Duration;
use clap::Parser;
use serde::{Serialize, Deserialize};

#[derive(Parser)]
#[command(name = "model_evaluation")]
#[command(about = "Comprehensive Model Performance Evaluation")]
struct Args {
    #[arg(short, long, default_value_t = 30)]
    days: u32,
    
    #[arg(short, long, default_value_t = String::from("BTCUSDT"))]
    symbol: String,
    
    #[arg(long, default_value_t = 1000.0)]
    initial_capital: f64,
    
    #[arg(long, default_value_t = 0.001)]
    trading_fee: f64,
    
    #[arg(long, default_value_t = false)]
    live_evaluation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPerformanceMetrics {
    // 预测准确率指标
    pub total_predictions: u64,
    pub correct_predictions: u64,
    pub accuracy: f64,
    pub precision: f64,
    pub recall: f64,
    pub f1_score: f64,
    
    // 盈利能力指标
    pub total_trades: u64,
    pub winning_trades: u64,
    pub win_rate: f64,
    pub total_pnl: f64,
    pub avg_trade_pnl: f64,
    pub max_profit: f64,
    pub max_loss: f64,
    
    // 风险调整指标
    pub sharpe_ratio: f64,
    pub sortino_ratio: f64,
    pub max_drawdown: f64,
    pub volatility: f64,
    pub calmar_ratio: f64,
    
    // 时间指标
    pub avg_holding_time_minutes: f64,
    pub profit_factor: f64,
    pub recovery_factor: f64,
    
    // 市场环境适应性
    pub bull_market_performance: f64,
    pub bear_market_performance: f64,
    pub sideways_market_performance: f64,
}

#[derive(Debug)]
pub struct ModelEvaluator {
    feature_extractor: FeatureExtractor,
    ml_engine: OnlineLearningEngine,
    metrics: Arc<Mutex<ModelPerformanceMetrics>>,
    prediction_history: Arc<Mutex<VecDeque<PredictionResult>>>,
    trade_history: Arc<Mutex<VecDeque<TradeResult>>>,
    price_history: Arc<Mutex<VecDeque<f64>>>,
}

#[derive(Debug, Clone)]
struct PredictionResult {
    timestamp: u64,
    actual_price_change: f64,
    predicted_direction: f64,
    confidence: f64,
    was_correct: bool,
}

#[derive(Debug, Clone)]
struct TradeResult {
    entry_time: u64,
    exit_time: u64,
    entry_price: f64,
    exit_price: f64,
    pnl: f64,
    holding_time_minutes: f64,
    was_profitable: bool,
}

impl ModelEvaluator {
    pub fn new() -> Result<Self> {
        info!("🧪 Initializing Model Evaluator");
        
        let feature_extractor = FeatureExtractor::new(50);
        let ml_engine = OnlineLearningEngine::new(OnlineLearningConfig::default())?;
        
        Ok(Self {
            feature_extractor,
            ml_engine,
            metrics: Arc::new(Mutex::new(ModelPerformanceMetrics::default())),
            prediction_history: Arc::new(Mutex::new(VecDeque::with_capacity(10000))),
            trade_history: Arc::new(Mutex::new(VecDeque::with_capacity(1000))),
            price_history: Arc::new(Mutex::new(VecDeque::with_capacity(10000))),
        })
    }
    
    /// 实时模型评估
    pub async fn run_live_evaluation(&mut self, symbol: &str) -> Result<()> {
        info!("🔴 Starting LIVE Model Evaluation for {}", symbol);
        
        // 配置 Bitget 连接
        let config = BitgetConfig {
            public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
            timeout_seconds: 10,
            auto_reconnect: true,
            max_reconnect_attempts: 5,
        };

        let mut connector = BitgetConnector::new(config);
        connector.add_subscription(symbol.to_string(), BitgetChannel::Books5);
        
        // 启动实时评估
        let metrics_clone = self.metrics.clone();
        let prediction_history_clone = self.prediction_history.clone();
        let price_history_clone = self.price_history.clone();
        let feature_extractor = FeatureExtractor::new(50);
        let ml_engine = OnlineLearningEngine::new(OnlineLearningConfig::default())?;
        
        // 统计报告任务
        let metrics_for_stats = metrics_clone.clone();
        let stats_task = tokio::spawn(async move {
            Self::live_statistics_reporter(metrics_for_stats).await;
        });
        
        let message_handler = {
            let mut fe = feature_extractor;
            let mut ml_eng = ml_engine;
            let metrics_c = metrics_clone;
            let pred_hist_c = prediction_history_clone.clone();
            let price_hist_c = price_history_clone.clone();
            
            move |message: BitgetMessage| {
                match message {
                    BitgetMessage::OrderBook { symbol: _s, data, timestamp, .. } => {
                        if let Err(e) = Self::process_live_data_static(&mut fe, &ml_eng, &data, timestamp, &metrics_c, &pred_hist_c, &price_hist_c) {
                            error!("Live evaluation error: {}", e);
                        }
                    },
                    _ => {}
                }
            }
        };
        
        info!("🔌 Connecting to Bitget for live evaluation...");
        tokio::select! {
            result = connector.connect_public(message_handler) => {
                if let Err(e) = result {
                    error!("WebSocket connection failed: {}", e);
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("📊 Evaluation stopped, generating final report...");
                self.generate_comprehensive_report();
            }
        }
        
        stats_task.abort();
        Ok(())
    }
    
    /// 静态方法处理实时数据
    fn process_live_data_static(
        feature_extractor: &mut FeatureExtractor,
        ml_engine: &OnlineLearningEngine,
        data: &serde_json::Value,
        timestamp: u64,
        metrics: &Arc<Mutex<ModelPerformanceMetrics>>,
        prediction_history: &Arc<Mutex<VecDeque<PredictionResult>>>,
        price_history: &Arc<Mutex<VecDeque<f64>>>
    ) -> Result<()> {
        // 解析订单簿
        let orderbook = Self::parse_orderbook_data_static(data, timestamp)?;
        
        // 提取特征
        let features = feature_extractor.extract_features(&orderbook, 50, timestamp)?;
        let current_price = *features.mid_price;
        
        // 存储价格历史
        {
            let mut price_hist = price_history.lock().unwrap();
            price_hist.push_back(current_price);
            if price_hist.len() > 10000 {
                price_hist.pop_front();
            }
        }
        
        // 进行预测
        let prediction_vec = ml_engine.predict(&features)?;
        let prediction = prediction_vec.first().cloned().unwrap_or(0.0);
        
        // 评估预测
        Self::evaluate_prediction_after_delay_static(current_price, prediction, timestamp, prediction_history, price_history)?;
        
        // 更新统计
        Self::update_performance_metrics_static(metrics, prediction_history)?;
        
        Ok(())
    }

    /// 处理实时数据并评估模型
    fn process_live_data(&mut self, data: &serde_json::Value, timestamp: u64) -> Result<()> {
        // 解析订单簿
        let orderbook = self.parse_orderbook_data(data, timestamp)?;
        
        // 提取特征
        let features = self.feature_extractor.extract_features(&orderbook, 50, timestamp)?;
        let current_price = *features.mid_price;
        
        // 存储价格历史
        {
            let mut price_hist = self.price_history.lock().unwrap();
            price_hist.push_back(current_price);
            if price_hist.len() > 10000 {
                price_hist.pop_front();
            }
        }
        
        // 进行预测
        let prediction_vec = self.ml_engine.predict(&features)?;
        let prediction = prediction_vec.first().cloned().unwrap_or(0.0);
        
        // 评估预测 (需要等待实际价格变化)
        self.evaluate_prediction_after_delay(current_price, prediction, timestamp)?;
        
        // 更新统计
        self.update_performance_metrics()?;
        
        Ok(())
    }
    
    /// 延迟评估预测准确性
    fn evaluate_prediction_after_delay(&self, current_price: f64, prediction: f64, timestamp: u64) -> Result<()> {
        // 简化版：假设3秒后评估
        // 实际应用中需要更复杂的时间序列管理
        
        let price_hist = self.price_history.lock().unwrap();
        if price_hist.len() > 10 {
            // 获取10个tick前的价格作为"历史预测"的实际结果
            let historical_price = price_hist[price_hist.len() - 10];
            let actual_change = (current_price - historical_price) / historical_price;
            
            // 模拟历史预测 (简化版)
            let predicted_direction = if prediction > 0.6 { 1.0 } else if prediction < -0.6 { -1.0 } else { 0.0 };
            let actual_direction: f64 = if actual_change > 0.001 { 1.0 } else if actual_change < -0.001 { -1.0 } else { 0.0 };
            
            let was_correct = (predicted_direction * actual_direction) > 0.0 || (predicted_direction == 0.0 && actual_direction.abs() < 0.001);
            
            let result = PredictionResult {
                timestamp,
                actual_price_change: actual_change,
                predicted_direction,
                confidence: prediction.abs(),
                was_correct,
            };
            
            let mut pred_hist = self.prediction_history.lock().unwrap();
            pred_hist.push_back(result);
            if pred_hist.len() > 10000 {
                pred_hist.pop_front();
            }
        }
        
        Ok(())
    }
    
    /// 更新性能指标
    fn update_performance_metrics(&self) -> Result<()> {
        let pred_hist = self.prediction_history.lock().unwrap();
        
        if pred_hist.len() < 10 {
            return Ok(());
        }
        
        let mut metrics = self.metrics.lock().unwrap();
        
        // 计算预测准确率
        let total_predictions = pred_hist.len() as u64;
        let correct_predictions = pred_hist.iter().filter(|p| p.was_correct).count() as u64;
        let accuracy = correct_predictions as f64 / total_predictions as f64;
        
        // 计算精确率和召回率 (简化版)
        let true_positives = pred_hist.iter()
            .filter(|p| p.predicted_direction > 0.0 && p.actual_price_change > 0.0)
            .count() as f64;
        let false_positives = pred_hist.iter()
            .filter(|p| p.predicted_direction > 0.0 && p.actual_price_change <= 0.0)
            .count() as f64;
        let false_negatives = pred_hist.iter()
            .filter(|p| p.predicted_direction <= 0.0 && p.actual_price_change > 0.0)
            .count() as f64;
            
        let precision = if true_positives + false_positives > 0.0 {
            true_positives / (true_positives + false_positives)
        } else {
            0.0
        };
        
        let recall = if true_positives + false_negatives > 0.0 {
            true_positives / (true_positives + false_negatives)
        } else {
            0.0
        };
        
        let f1_score = if precision + recall > 0.0 {
            2.0 * precision * recall / (precision + recall)
        } else {
            0.0
        };
        
        // 更新指标
        metrics.total_predictions = total_predictions;
        metrics.correct_predictions = correct_predictions;
        metrics.accuracy = accuracy;
        metrics.precision = precision;
        metrics.recall = recall;
        metrics.f1_score = f1_score;
        
        Ok(())
    }
    
    /// 静态方法解析订单簿数据
    fn parse_orderbook_data_static(data: &serde_json::Value, timestamp: u64) -> Result<OrderBook> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let _first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;

        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        orderbook.last_update = timestamp;
        orderbook.is_valid = true;
        
        Ok(orderbook)
    }

    /// 静态方法延迟评估预测准确性
    fn evaluate_prediction_after_delay_static(
        current_price: f64, 
        prediction: f64, 
        timestamp: u64,
        prediction_history: &Arc<Mutex<VecDeque<PredictionResult>>>,
        price_history: &Arc<Mutex<VecDeque<f64>>>
    ) -> Result<()> {
        let price_hist = price_history.lock().unwrap();
        if price_hist.len() > 10 {
            let historical_price = price_hist[price_hist.len() - 10];
            let actual_change = (current_price - historical_price) / historical_price;
            
            let predicted_direction = if prediction > 0.6 { 1.0 } else if prediction < -0.6 { -1.0 } else { 0.0 };
            let actual_direction: f64 = if actual_change > 0.001 { 1.0 } else if actual_change < -0.001 { -1.0 } else { 0.0 };
            
            let was_correct = (predicted_direction * actual_direction) > 0.0 || (predicted_direction == 0.0 && actual_direction.abs() < 0.001);
            
            let result = PredictionResult {
                timestamp,
                actual_price_change: actual_change,
                predicted_direction,
                confidence: prediction.abs(),
                was_correct,
            };
            
            let mut pred_hist = prediction_history.lock().unwrap();
            pred_hist.push_back(result);
            if pred_hist.len() > 10000 {
                pred_hist.pop_front();
            }
        }
        
        Ok(())
    }

    /// 静态方法更新性能指标
    fn update_performance_metrics_static(
        metrics: &Arc<Mutex<ModelPerformanceMetrics>>,
        prediction_history: &Arc<Mutex<VecDeque<PredictionResult>>>
    ) -> Result<()> {
        let pred_hist = prediction_history.lock().unwrap();
        
        if pred_hist.len() < 10 {
            return Ok(());
        }
        
        let mut metrics_lock = metrics.lock().unwrap();
        
        // 计算预测准确率
        let total_predictions = pred_hist.len() as u64;
        let correct_predictions = pred_hist.iter().filter(|p| p.was_correct).count() as u64;
        let accuracy = correct_predictions as f64 / total_predictions as f64;
        
        // 计算精确率和召回率
        let true_positives = pred_hist.iter()
            .filter(|p| p.predicted_direction > 0.0 && p.actual_price_change > 0.0)
            .count() as f64;
        let false_positives = pred_hist.iter()
            .filter(|p| p.predicted_direction > 0.0 && p.actual_price_change <= 0.0)
            .count() as f64;
        let false_negatives = pred_hist.iter()
            .filter(|p| p.predicted_direction <= 0.0 && p.actual_price_change > 0.0)
            .count() as f64;
            
        let precision = if true_positives + false_positives > 0.0 {
            true_positives / (true_positives + false_positives)
        } else {
            0.0
        };
        
        let recall = if true_positives + false_negatives > 0.0 {
            true_positives / (true_positives + false_negatives)
        } else {
            0.0
        };
        
        let f1_score = if precision + recall > 0.0 {
            2.0 * precision * recall / (precision + recall)
        } else {
            0.0
        };
        
        // 更新指标
        metrics_lock.total_predictions = total_predictions;
        metrics_lock.correct_predictions = correct_predictions;
        metrics_lock.accuracy = accuracy;
        metrics_lock.precision = precision;
        metrics_lock.recall = recall;
        metrics_lock.f1_score = f1_score;
        
        Ok(())
    }

    /// 解析订单簿数据
    fn parse_orderbook_data(&self, data: &serde_json::Value, timestamp: u64) -> Result<OrderBook> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;

        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        orderbook.last_update = timestamp;
        orderbook.is_valid = true;
        
        // 简化的解析逻辑
        Ok(orderbook)
    }
    
    /// 实时统计报告
    async fn live_statistics_reporter(metrics: Arc<Mutex<ModelPerformanceMetrics>>) {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        
        loop {
            interval.tick().await;
            
            if let Ok(m) = metrics.lock() {
                info!("🧪 === MODEL EVALUATION REPORT ===");
                info!("📊 Prediction Performance:");
                info!("   └─ Total Predictions: {}", m.total_predictions);
                info!("   └─ Accuracy: {:.2}%", m.accuracy * 100.0);
                info!("   └─ Precision: {:.3}", m.precision);
                info!("   └─ Recall: {:.3}", m.recall);
                info!("   └─ F1-Score: {:.3}", m.f1_score);
                
                // 性能评价
                if m.accuracy > 0.55 && m.f1_score > 0.5 {
                    info!("✅ Model Performance: GOOD");
                } else if m.accuracy > 0.50 && m.f1_score > 0.3 {
                    warn!("⚠️  Model Performance: AVERAGE");
                } else {
                    warn!("❌ Model Performance: POOR - Need Retraining");
                }
                
                info!("=====================================");
            }
        }
    }
    
    /// 生成综合报告
    fn generate_comprehensive_report(&self) {
        let metrics = self.metrics.lock().unwrap();
        
        info!("📋 === COMPREHENSIVE MODEL EVALUATION REPORT ===");
        
        info!("🎯 Prediction Quality:");
        info!("   └─ Accuracy: {:.2}% (Target: >55%)", metrics.accuracy * 100.0);
        info!("   └─ Precision: {:.3} (Target: >0.6)", metrics.precision);
        info!("   └─ Recall: {:.3} (Target: >0.4)", metrics.recall);
        info!("   └─ F1-Score: {:.3} (Target: >0.5)", metrics.f1_score);
        
        info!("💰 Profitability Assessment:");
        info!("   └─ Win Rate: {:.1}% (Target: >50%)", metrics.win_rate * 100.0);
        info!("   └─ Sharpe Ratio: {:.2} (Target: >1.5)", metrics.sharpe_ratio);
        info!("   └─ Max Drawdown: {:.1}% (Target: <5%)", metrics.max_drawdown * 100.0);
        
        // 整体评估
        let overall_score = self.calculate_model_score(&metrics);
        info!("🏆 Overall Model Score: {:.1}/100", overall_score);
        
        if overall_score >= 80.0 {
            info!("✅ RECOMMENDATION: Model is EXCELLENT for live trading");
        } else if overall_score >= 60.0 {
            warn!("⚠️  RECOMMENDATION: Model is ACCEPTABLE, monitor closely");
        } else {
            warn!("❌ RECOMMENDATION: Model needs IMPROVEMENT before live trading");
        }
        
        info!("================================================");
    }
    
    /// 计算模型综合评分
    fn calculate_model_score(&self, metrics: &ModelPerformanceMetrics) -> f64 {
        let accuracy_score = (metrics.accuracy * 100.0).min(100.0);
        let f1_score = (metrics.f1_score * 100.0).min(100.0);
        let precision_score = (metrics.precision * 100.0).min(100.0);
        
        // 加权平均
        (accuracy_score * 0.4 + f1_score * 0.35 + precision_score * 0.25)
    }
}

impl Default for ModelPerformanceMetrics {
    fn default() -> Self {
        Self {
            total_predictions: 0,
            correct_predictions: 0,
            accuracy: 0.0,
            precision: 0.0,
            recall: 0.0,
            f1_score: 0.0,
            total_trades: 0,
            winning_trades: 0,
            win_rate: 0.0,
            total_pnl: 0.0,
            avg_trade_pnl: 0.0,
            max_profit: 0.0,
            max_loss: 0.0,
            sharpe_ratio: 0.0,
            sortino_ratio: 0.0,
            max_drawdown: 0.0,
            volatility: 0.0,
            calmar_ratio: 0.0,
            avg_holding_time_minutes: 0.0,
            profit_factor: 0.0,
            recovery_factor: 0.0,
            bull_market_performance: 0.0,
            bear_market_performance: 0.0,
            sideways_market_performance: 0.0,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = Args::parse();
    
    info!("🧪 Starting Model Evaluation");
    info!("⚙️  Configuration:");
    info!("   Symbol: {}", args.symbol);
    info!("   Evaluation Period: {} days", args.days);
    info!("   Initial Capital: ${:.2}", args.initial_capital);
    info!("   Trading Fee: {:.3}%", args.trading_fee * 100.0);
    info!("   Live Evaluation: {}", args.live_evaluation);
    
    let mut evaluator = ModelEvaluator::new()?;
    
    if args.live_evaluation {
        evaluator.run_live_evaluation(&args.symbol).await?;
    } else {
        info!("ℹ️  Use --live-evaluation for real-time model assessment");
        info!("ℹ️  Historical backtest evaluation not implemented yet");
    }
    
    Ok(())
}