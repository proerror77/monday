/*!
 * 統一回測系統 - 整合回測功能
 * 
 * 整合功能：
 * - 策略回測框架 (03_backtest_strategy.rs)
 * 
 * 支持多種回測模式和全面的性能評估
 */

use rust_hft::{BacktestArgs, WorkflowExecutor, WorkflowStep, StepResult, now_micros};
use rust_hft::ml::features::FeatureExtractor;
use rust_hft::core::orderbook::OrderBook;
use anyhow::Result;
use async_trait::async_trait;
use clap::Parser;
use tracing::{info, warn, debug};
use std::fs::File;
use std::io::{BufRead, BufReader};
use serde_json::Value;
use serde::{Serialize, Deserialize};
use std::collections::{HashMap, VecDeque};
use std::time::Instant;
use std::path::Path;

/// 模型類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelType {
    /// 簡單線性模型
    Linear,
    /// LOB Transformer
    LobTransformer,
    /// 輕量級梯度提升
    LightGBM,
    /// 深度神經網絡
    DeepNN,
    /// 強化學習模型
    ReinforcementLearning,
}

/// 動作空間定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionSpace {
    /// 二元動作空間 (買/賣)
    Binary { buy_threshold: f64, sell_threshold: f64 },
    /// 連續動作空間 (倉位大小)
    Continuous { min_position: f64, max_position: f64 },
    /// 離散動作空間 (多種動作)
    Discrete { actions: Vec<String> },
}

/// 交易動作類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionType {
    /// 持有現有倉位
    Hold,
    /// 買入
    Buy { size: f64 },
    /// 賣出
    Sell { size: f64 },
    /// 平倉
    Close,
}

/// 交易動作
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeAction {
    pub action_type: ActionType,
    pub confidence: f64,
    pub expected_return: f64,
}

/// 訂單邊
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// 成交結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderExecution {
    /// 實際成交數量
    pub filled_quantity: f64,
    /// 實際成交價格（加權平均）
    pub filled_price: f64,
    /// 成交延遲（微秒）
    pub execution_time_us: u64,
    /// 滑點（基點）
    pub slippage_bps: f64,
    /// 是否部分成交
    pub partial_fill: bool,
    /// 成交率 (0.0 - 1.0)
    pub fill_rate: f64,
    /// 剩餘未成交數量
    pub remaining_quantity: f64,
    /// 總交易費用
    pub total_fees: f64,
    /// 是否立即成交
    pub immediate_fill: bool,
    /// 流動性消耗（從訂單簿移除的數量）
    pub liquidity_consumed: f64,
}

/// 市場流動性狀態
#[derive(Debug, Clone)]
pub struct LiquidityState {
    /// 買方可用流動性
    pub bid_liquidity: f64,
    /// 賣方可用流動性  
    pub ask_liquidity: f64,
    /// 最佳買價
    pub best_bid: f64,
    /// 最佳賣價
    pub best_ask: f64,
    /// 買方深度（5檔）
    pub bid_depth_l5: f64,
    /// 賣方深度（5檔）
    pub ask_depth_l5: f64,
    /// 價差（基點）
    pub spread_bps: f64,
}

/// 成交模擬器
#[derive(Debug, Clone)]
pub struct OrderExecutionSimulator {
    /// 基礎成交延遲（微秒）
    pub base_latency_us: u64,
    /// 網絡抖動範圍（微秒）
    pub latency_jitter_us: u64,
    /// 滑點影響因子
    pub slippage_impact_factor: f64,
    /// 最大滑點限制（基點）
    pub max_slippage_bps: f64,
    /// 最小成交量
    pub min_fill_size: f64,
    /// 部分成交閾值
    pub partial_fill_threshold: f64,
}

impl Default for OrderExecutionSimulator {
    fn default() -> Self {
        Self {
            base_latency_us: 100,         // 100μs 基礎延遲
            latency_jitter_us: 50,        // ±50μs 抖動
            slippage_impact_factor: 0.01, // 1% 的市場衝擊
            max_slippage_bps: 20.0,       // 最大20bp滑點
            min_fill_size: 0.001,         // 最小成交0.001
            partial_fill_threshold: 0.8,  // 80%流動性閾值
        }
    }
}

impl OrderExecutionSimulator {
    pub fn new() -> Self {
        Self::default()
    }
    
    /// 從訂單簿提取流動性狀態
    pub fn extract_liquidity_state(&self, orderbook: &OrderBook) -> LiquidityState {
        let best_bid = orderbook.bids.iter().next().map(|(price, _)| price.0).unwrap_or(0.0);
        let best_ask = orderbook.asks.iter().next().map(|(price, _)| price.0).unwrap_or(0.0);
        
        // 計算5檔深度
        let bid_depth_l5: f64 = orderbook.bids.iter().take(5).map(|(_, qty)| {
            qty.to_string().parse::<f64>().unwrap_or(0.0)
        }).sum();
        let ask_depth_l5: f64 = orderbook.asks.iter().take(5).map(|(_, qty)| {
            qty.to_string().parse::<f64>().unwrap_or(0.0)
        }).sum();
        
        // 計算總流動性（10檔）
        let bid_liquidity: f64 = orderbook.bids.iter().take(10).map(|(_, qty)| {
            qty.to_string().parse::<f64>().unwrap_or(0.0)
        }).sum();
        let ask_liquidity: f64 = orderbook.asks.iter().take(10).map(|(_, qty)| {
            qty.to_string().parse::<f64>().unwrap_or(0.0)
        }).sum();
        
        let spread_bps = if best_bid > 0.0 && best_ask > 0.0 {
            (best_ask - best_bid) / best_bid * 10000.0
        } else {
            0.0
        };
        
        LiquidityState {
            bid_liquidity,
            ask_liquidity,
            best_bid,
            best_ask,
            bid_depth_l5,
            ask_depth_l5,
            spread_bps,
        }
    }
    
    /// 檢查流動性是否充足
    pub fn check_liquidity_sufficiency(&self, 
        liquidity_state: &LiquidityState, 
        order_size: f64, 
        side: OrderSide
    ) -> (bool, f64) {
        let available_liquidity = match side {
            OrderSide::Buy => liquidity_state.ask_liquidity,
            OrderSide::Sell => liquidity_state.bid_liquidity,
        };
        
        let liquidity_ratio = order_size / available_liquidity.max(self.min_fill_size);
        let sufficient = liquidity_ratio <= self.partial_fill_threshold;
        
        (sufficient, liquidity_ratio)
    }
    
    /// 計算滑點
    pub fn calculate_slippage(&self, 
        liquidity_state: &LiquidityState,
        order_size: f64,
        side: OrderSide
    ) -> f64 {
        let available_liquidity = match side {
            OrderSide::Buy => liquidity_state.ask_liquidity,
            OrderSide::Sell => liquidity_state.bid_liquidity,
        };
        
        // 市場衝擊模型：滑點 = 訂單大小 / 可用流動性 * 影響因子
        let market_impact = (order_size / available_liquidity.max(0.001)) * self.slippage_impact_factor;
        
        // 加上基礎價差的一半
        let base_slippage = liquidity_state.spread_bps * 0.5;
        
        // 總滑點
        let total_slippage = base_slippage + market_impact * 10000.0; // 轉換為bp
        
        // 限制最大滑點
        total_slippage.min(self.max_slippage_bps)
    }
    
    /// 計算成交延遲
    pub fn calculate_execution_latency(&self, order_size: f64, liquidity_ratio: f64) -> u64 {
        let base_latency = self.base_latency_us;
        
        // 大單增加延遲
        let size_factor = (order_size * 10.0).min(2.0); // 最多增加2倍
        
        // 流動性緊張增加延遲
        let liquidity_factor = (liquidity_ratio * 5.0).min(3.0); // 最多增加3倍
        
        // 隨機抖動
        let jitter = fastrand::u64(0..self.latency_jitter_us);
        
        let total_latency = (base_latency as f64 * size_factor * liquidity_factor) as u64 + jitter;
        
        total_latency
    }
    
    /// 模擬訂單執行
    pub fn simulate_order_execution(&self,
        orderbook: &OrderBook,
        order_size: f64,
        side: OrderSide,
        trading_fee: f64
    ) -> OrderExecution {
        let liquidity_state = self.extract_liquidity_state(orderbook);
        let (sufficient_liquidity, liquidity_ratio) = self.check_liquidity_sufficiency(&liquidity_state, order_size, side);
        
        // 計算成交參數
        let slippage_bps = self.calculate_slippage(&liquidity_state, order_size, side);
        let execution_time_us = self.calculate_execution_latency(order_size, liquidity_ratio);
        
        // 決定成交數量
        let (filled_quantity, partial_fill) = if sufficient_liquidity {
            // 流動性充足，全部成交
            (order_size, false)
        } else {
            // 流動性不足，部分成交
            let available_liquidity = match side {
                OrderSide::Buy => liquidity_state.ask_liquidity,
                OrderSide::Sell => liquidity_state.bid_liquidity,
            };
            let max_fill = available_liquidity * self.partial_fill_threshold;
            (order_size.min(max_fill), true)
        };
        
        // 計算成交價格
        let reference_price = match side {
            OrderSide::Buy => liquidity_state.best_ask,
            OrderSide::Sell => liquidity_state.best_bid,
        };
        
        let slippage_adjustment = match side {
            OrderSide::Buy => slippage_bps / 10000.0,  // 買入時價格上升
            OrderSide::Sell => -slippage_bps / 10000.0, // 賣出時價格下降
        };
        
        let filled_price = reference_price * (1.0 + slippage_adjustment);
        
        // 計算費用和其他指標
        let total_fees = filled_quantity * filled_price * trading_fee;
        let fill_rate = filled_quantity / order_size;
        let remaining_quantity = order_size - filled_quantity;
        let immediate_fill = execution_time_us < 1000; // <1ms視為立即成交
        let liquidity_consumed = filled_quantity;
        
        OrderExecution {
            filled_quantity,
            filled_price,
            execution_time_us,
            slippage_bps,
            partial_fill,
            fill_rate,
            remaining_quantity,
            total_fees,
            immediate_fill,
            liquidity_consumed,
        }
    }
}

/// 模型推理引擎
#[derive(Debug, Clone)]
pub struct ModelInferenceEngine {
    pub model_path: String,
    pub model_type: ModelType,
    pub action_space: ActionSpace,
    pub state_history: VecDeque<rust_hft::FeatureSet>,
    pub max_history_length: usize,
    pub inference_latency_us: Vec<u64>,
}

/// 回測結果 (擴展版本)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResults {
    // 傳統交易指標
    pub total_trades: u64,
    pub winning_trades: u64,
    pub total_pnl: f64,
    pub max_drawdown: f64,
    pub sharpe_ratio: f64,
    pub win_rate: f64,
    pub total_return: f64,
    pub volatility: f64,
    pub calmar_ratio: f64,
    pub sortino_ratio: f64,
    pub average_trade_pnl: f64,
    pub largest_win: f64,
    pub largest_loss: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub profit_factor: f64,
    pub recovery_factor: f64,
    
    // 模型特定指標
    pub avg_inference_latency_us: f64,
    pub p99_inference_latency_us: f64,
    pub prediction_accuracy: f64,
    pub model_confidence_avg: f64,
    pub total_predictions: u64,
    
    // RL 特定指標
    pub action_distribution: HashMap<String, f64>,
    pub reward_cumulative: f64,
    pub exploration_rate: f64,
    
    // 成交執行指標
    pub avg_fill_rate: f64,
    pub avg_slippage_bps: f64,
    pub avg_execution_time_us: f64,
    pub partial_fill_rate: f64,
    pub immediate_fill_rate: f64,
    pub total_liquidity_consumed: f64,
}

/// 交易記錄 (擴展版本)
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub timestamp: u64,
    pub signal_type: String,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub quantity: f64,
    pub pnl: f64,
    pub fees: f64,
    pub holding_period: u64,
    
    // 模型相關資訊
    pub model_confidence: f64,
    pub expected_return: f64,
    pub actual_return: f64,
    pub inference_latency_us: u64,
    
    // 成交執行詳細信息
    pub execution_details: Option<OrderExecution>,
    pub requested_quantity: f64,
    pub fill_rate: f64,
    pub slippage_bps: f64,
    pub execution_time_us: u64,
    pub partial_fill: bool,
}

impl ModelInferenceEngine {
    pub fn new(model_path: String, model_type: ModelType, action_space: ActionSpace) -> Self {
        Self {
            model_path,
            model_type,
            action_space,
            state_history: VecDeque::new(),
            max_history_length: 50,
            inference_latency_us: Vec::new(),
        }
    }
    
    /// 更新狀態歷史
    pub fn update_state(&mut self, features: rust_hft::FeatureSet) {
        self.state_history.push_back(features);
        if self.state_history.len() > self.max_history_length {
            self.state_history.pop_front();
        }
    }
    
    /// 從訓練的模型進行預測
    pub async fn predict_with_trained_model(&mut self, features: &rust_hft::FeatureSet) -> Result<TradeAction> {
        let start_time = Instant::now();
        
        // 更新狀態歷史
        self.update_state(features.clone());
        
        // 檢查模型文件是否存在
        if !Path::new(&self.model_path).exists() {
            warn!("模型文件不存在: {}, 使用模擬預測", self.model_path);
            return self.generate_mock_prediction(features).await;
        }
        
        // 根據模型類型進行預測
        let prediction = match self.model_type {
            ModelType::LobTransformer => self.predict_lob_transformer(features).await?,
            ModelType::ReinforcementLearning => self.predict_rl_model(features).await?,
            ModelType::DeepNN => self.predict_deep_nn(features).await?,
            _ => self.generate_mock_prediction(features).await?,
        };
        
        // 記錄推理延遲
        let latency = start_time.elapsed().as_micros() as u64;
        self.inference_latency_us.push(latency);
        
        debug!("模型預測完成: {:?}, 延遲: {}μs", prediction.action_type, latency);
        
        Ok(prediction)
    }
    
    /// LOB Transformer 模型預測
    async fn predict_lob_transformer(&self, features: &rust_hft::FeatureSet) -> Result<TradeAction> {
        // 這裡會集成你的 DlTrendPredictor
        // 模擬基於置信度的預測邏輯
        
        let trend_signal = features.obi_l5; // 使用 OBI 作為模擬信號
        let confidence = trend_signal.abs().min(1.0);
        
        // 模擬 LOB Transformer 的 5 類預測
        let trend_class = if trend_signal > 0.6 {
            "StrongUp"
        } else if trend_signal > 0.2 {
            "WeakUp"
        } else if trend_signal > -0.2 {
            "Neutral"
        } else if trend_signal > -0.6 {
            "WeakDown"
        } else {
            "StrongDown"
        };
        
        // 根據趨勢類別生成動作
        let action_type = match trend_class {
            "StrongUp" => ActionType::Buy { size: 0.2 },
            "WeakUp" => ActionType::Buy { size: 0.1 },
            "Neutral" => ActionType::Hold,
            "WeakDown" => ActionType::Sell { size: 0.1 },
            "StrongDown" => ActionType::Sell { size: 0.2 },
            _ => ActionType::Hold,
        };
        
        // 只有高置信度時才執行交易
        let final_action = if confidence > 0.7 {
            action_type
        } else {
            ActionType::Hold
        };
        
        Ok(TradeAction {
            action_type: final_action,
            confidence,
            expected_return: trend_signal * 0.01,
        })
    }
    
    /// 強化學習模型預測
    async fn predict_rl_model(&self, features: &rust_hft::FeatureSet) -> Result<TradeAction> {
        // 模擬 RL 模型的動作選擇
        // 使用狀態歷史來做序列決策
        
        let state_depth = self.state_history.len().min(10);
        if state_depth < 5 {
            // 狀態序列不足，保持觀望
            return Ok(TradeAction {
                action_type: ActionType::Hold,
                confidence: 0.3,
                expected_return: 0.0,
            });
        }
        
        // 計算狀態序列的趨勢
        let price_trend = if let (Some(first), Some(last)) = 
            (self.state_history.front(), self.state_history.back()) {
            (last.mid_price.0 - first.mid_price.0) / first.mid_price.0
        } else {
            0.0
        };
        
        // 計算動量指標
        let momentum = features.price_momentum;
        let obi_trend = features.obi_l5;
        
        // RL 模型的動作值函數 (Q-values)
        let q_hold = 0.1;
        let q_buy = price_trend * 10.0 + momentum * 5.0 + obi_trend * 3.0;
        let q_sell = -price_trend * 10.0 - momentum * 5.0 - obi_trend * 3.0;
        
        // 選擇最佳動作 (epsilon-greedy with low exploration)
        let exploration_rate = 0.05;
        let action_type = if fastrand::f64() < exploration_rate {
            // 隨機探索
            match fastrand::u32(0..3) {
                0 => ActionType::Hold,
                1 => ActionType::Buy { size: 0.1 },
                _ => ActionType::Sell { size: 0.1 },
            }
        } else {
            // 貪婪選擇
            if q_buy > q_hold && q_buy > q_sell {
                ActionType::Buy { size: (q_buy * 0.1).min(0.2).max(0.05) }
            } else if q_sell > q_hold && q_sell > q_buy {
                ActionType::Sell { size: (q_sell * 0.1).min(0.2).max(0.05) }
            } else {
                ActionType::Hold
            }
        };
        
        let confidence = match action_type {
            ActionType::Hold => 0.5,
            _ => (q_buy.max(q_sell) - q_hold).abs().min(1.0).max(0.1),
        };
        
        Ok(TradeAction {
            action_type,
            confidence,
            expected_return: price_trend * 100.0, // 轉換為 bps
        })
    }
    
    /// 深度神經網絡預測
    async fn predict_deep_nn(&self, features: &rust_hft::FeatureSet) -> Result<TradeAction> {
        // 模擬深度神經網絡的回歸預測
        
        // 特徵組合權重 (模擬訓練過的權重)
        let weights = [
            0.3, 0.2, 0.15, 0.12, 0.1, 0.08, 0.05, 0.04, 0.03, 0.02,
            0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.01
        ];
        
        let feature_vector = [
            features.mid_price.0 / 50000.0, // 正規化價格
            features.spread_bps / 50.0,
            features.obi_l1,
            features.obi_l5,
            features.obi_l10,
            features.obi_l20,
            features.bid_depth_l5 / 1000.0,
            features.ask_depth_l5 / 1000.0,
            features.depth_imbalance_l5,
            features.bid_slope,
            features.ask_slope,
            features.price_momentum,
            features.volume_imbalance,
            features.latency_network_us as f64 / 1000.0,
            features.latency_processing_us as f64 / 1000.0,
            features.data_quality_score,
            // 技術指標
            features.obi_l5 * features.price_momentum, // 組合特徵
            features.depth_imbalance_l5 * features.volume_imbalance,
            (features.bid_slope + features.ask_slope) / 2.0,
            features.spread_bps * features.obi_l1,
        ];
        
        // 計算神經網絡輸出 (簡化版)
        let mut output = 0.0;
        for (i, &feature) in feature_vector.iter().enumerate() {
            if i < weights.len() {
                output += feature * weights[i];
            }
        }
        
        // 應用 tanh 激活函數
        let prediction = output.tanh();
        let confidence = prediction.abs().min(1.0);
        
        // 將預測轉換為動作
        let action_type = if prediction > 0.3 {
            ActionType::Buy { size: (prediction * 0.3).min(0.2) }
        } else if prediction < -0.3 {
            ActionType::Sell { size: (-prediction * 0.3).min(0.2) }
        } else {
            ActionType::Hold
        };
        
        Ok(TradeAction {
            action_type,
            confidence,
            expected_return: prediction * 10.0, // 轉換為預期收益 (bps)
        })
    }
    
    /// 生成模擬預測 (當模型文件不存在時)
    async fn generate_mock_prediction(&self, features: &rust_hft::FeatureSet) -> Result<TradeAction> {
        match &self.action_space {
            ActionSpace::Binary { buy_threshold, sell_threshold } => {
                let signal = features.obi_l5;
                let confidence = signal.abs().min(1.0);
                
                if signal > *buy_threshold {
                    Ok(TradeAction {
                        action_type: ActionType::Buy { size: 0.1 },
                        confidence,
                        expected_return: signal * 0.01,
                    })
                } else if signal < *sell_threshold {
                    Ok(TradeAction {
                        action_type: ActionType::Sell { size: 0.1 },
                        confidence,
                        expected_return: -signal * 0.01,
                    })
                } else {
                    Ok(TradeAction {
                        action_type: ActionType::Hold,
                        confidence: 1.0 - confidence,
                        expected_return: 0.0,
                    })
                }
            },
            _ => {
                // 其他動作空間的默認處理
                Ok(TradeAction {
                    action_type: ActionType::Hold,
                    confidence: 0.5,
                    expected_return: 0.0,
                })
            }
        }
    }
    
    /// 計算推理性能指標
    pub fn get_inference_metrics(&self) -> (f64, f64) {
        if self.inference_latency_us.is_empty() {
            return (0.0, 0.0);
        }
        
        let avg_latency = self.inference_latency_us.iter().sum::<u64>() as f64 / self.inference_latency_us.len() as f64;
        let mut sorted_latencies = self.inference_latency_us.clone();
        sorted_latencies.sort();
        let p99_latency = sorted_latencies[(sorted_latencies.len() as f64 * 0.99) as usize] as f64;
        
        (avg_latency, p99_latency)
    }
}

/// 數據加載步驟
struct DataLoadingStep {
    input_file: String,
    max_samples: Option<usize>,
    loaded_data: Vec<(u64, OrderBook)>,
}

impl DataLoadingStep {
    fn new(input_file: String, max_samples: Option<usize>) -> Box<dyn WorkflowStep> {
        Box::new(Self {
            input_file,
            max_samples,
            loaded_data: Vec::new(),
        })
    }
}

#[async_trait]
impl WorkflowStep for DataLoadingStep {
    fn name(&self) -> &str {
        "Historical Data Loading"
    }
    
    fn description(&self) -> &str {
        "Load and parse historical market data for backtesting"
    }
    
    fn estimated_duration(&self) -> u64 {
        120
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("📂 Loading historical data from: {}", self.input_file);
        
        let file = File::open(&self.input_file)
            .map_err(|e| anyhow::anyhow!("Failed to open input file: {}", e))?;
        
        let reader = BufReader::new(file);
        let mut count = 0;
        let mut valid_records = 0;
        
        for line in reader.lines() {
            if let Some(max) = self.max_samples {
                if count >= max {
                    break;
                }
            }
            
            let line = line?;
            count += 1;
            
            if count % 10000 == 0 {
                info!("Processed {} lines", count);
                tokio::task::yield_now().await;
            }
            
            // 解析 JSON 行
            if let Ok(data) = serde_json::from_str::<Value>(&line) {
                if let Ok(orderbook) = parse_historical_orderbook(&data) {
                    self.loaded_data.push((orderbook.last_update, orderbook));
                    valid_records += 1;
                }
            }
        }
        
        // 按時間戳排序
        self.loaded_data.sort_by_key(|(timestamp, _)| *timestamp);
        
        info!("📊 Data Loading Results:");
        info!("   Total lines processed: {}", count);
        info!("   Valid orderbook records: {}", valid_records);
        info!("   Time range: {} to {}", 
              self.loaded_data.first().map(|(t, _)| *t).unwrap_or(0),
              self.loaded_data.last().map(|(t, _)| *t).unwrap_or(0));
        
        let data_quality = (valid_records as f64 / count as f64) * 100.0;
        
        Ok(StepResult::success(&format!("Loaded {} valid records ({:.1}% quality)", valid_records, data_quality))
           .with_metric("total_lines", count as f64)
           .with_metric("valid_records", valid_records as f64)
           .with_metric("data_quality", data_quality))
    }
}

/// 模型驅動回測步驟
struct ModelBacktestStep {
    model_engine: ModelInferenceEngine,
    execution_simulator: OrderExecutionSimulator,
    data: Vec<(u64, OrderBook)>,
    initial_capital: f64,
    trading_fee: f64,
    results: Option<BacktestResults>,
    trades: Vec<TradeRecord>,
}

impl ModelBacktestStep {
    fn new(model_engine: ModelInferenceEngine, initial_capital: f64, trading_fee: f64) -> Box<dyn WorkflowStep> {
        Box::new(Self {
            model_engine,
            execution_simulator: OrderExecutionSimulator::new(),
            data: Vec::new(),
            initial_capital,
            trading_fee,
            results: None,
            trades: Vec::new(),
        })
    }
}

#[async_trait]
impl WorkflowStep for ModelBacktestStep {
    fn name(&self) -> &str {
        "Strategy Backtesting"
    }
    
    fn description(&self) -> &str {
        "Execute trading strategy on historical data"
    }
    
    fn estimated_duration(&self) -> u64 {
        (self.data.len() / 1000) as u64 + 60
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("🎯 Starting strategy backtest with {} data points", self.data.len());
        
        let mut feature_extractor = FeatureExtractor::new(50);
        let mut current_capital = self.initial_capital;
        let mut current_position = 0.0;
        let mut entry_price = 0.0;
        let mut peak_capital = self.initial_capital;
        let mut max_drawdown = 0.0;
        let mut returns = Vec::new();
        let mut total_trades = 0;
        let mut winning_trades = 0;
        let mut total_pnl = 0.0;
        
        for (i, (timestamp, orderbook)) in self.data.iter().enumerate() {
            // 提取特徵
            if let Ok(features) = feature_extractor.extract_features(orderbook, 0, *timestamp) {
                // 使用訓練的模型進行預測
                if let Ok(action) = self.model_engine.predict_with_trained_model(&features).await {
                    let current_price = features.mid_price.0;
                    
                    match action.action_type {
                        ActionType::Buy { size } if current_position == 0.0 => {
                            // 開多倉 - 使用真實成交模擬
                            let requested_quantity = (current_capital * size) / current_price;
                            
                            // 模擬訂單執行
                            let execution = self.execution_simulator.simulate_order_execution(
                                orderbook,
                                requested_quantity,
                                OrderSide::Buy,
                                self.trading_fee
                            );
                            
                            // 檢查是否有足夠資金執行
                            let required_capital = execution.filled_quantity * execution.filled_price + execution.total_fees;
                            
                            if current_capital >= required_capital && execution.filled_quantity > 0.0 {
                                current_position = execution.filled_quantity;
                                entry_price = execution.filled_price;
                                current_capital -= required_capital;
                                
                                self.trades.push(TradeRecord {
                                    timestamp: *timestamp,
                                    signal_type: "BUY".to_string(),
                                    entry_price: execution.filled_price,
                                    exit_price: None,
                                    quantity: execution.filled_quantity,
                                    pnl: 0.0,
                                    fees: execution.total_fees,
                                    holding_period: 0,
                                    model_confidence: action.confidence,
                                    expected_return: action.expected_return,
                                    actual_return: 0.0,
                                    inference_latency_us: self.model_engine.inference_latency_us.last().copied().unwrap_or(0),
                                    
                                    // 成交執行詳細信息
                                    execution_details: Some(execution.clone()),
                                    requested_quantity,
                                    fill_rate: execution.fill_rate,
                                    slippage_bps: execution.slippage_bps,
                                    execution_time_us: execution.execution_time_us,
                                    partial_fill: execution.partial_fill,
                                });
                            }
                        },
                        ActionType::Sell { size: _ } if current_position > 0.0 => {
                            // 平倉 - 使用真實成交模擬
                            let execution = self.execution_simulator.simulate_order_execution(
                                orderbook,
                                current_position, // 賣出全部持倉
                                OrderSide::Sell,
                                self.trading_fee
                            );
                            
                            // 計算實際平倉結果
                            let sold_quantity = execution.filled_quantity;
                            let exit_price = execution.filled_price;
                            let fees = execution.total_fees;
                            let pnl = sold_quantity * (exit_price - entry_price) - fees;
                            let actual_return = (exit_price - entry_price) / entry_price * 10000.0; // bps
                            
                            current_capital += sold_quantity * exit_price - fees;
                            total_pnl += pnl;
                            total_trades += 1;
                            
                            if pnl > 0.0 {
                                winning_trades += 1;
                            }
                            
                            // 更新最後一筆交易記錄
                            if let Some(last_trade) = self.trades.last_mut() {
                                last_trade.exit_price = Some(exit_price);
                                last_trade.pnl = pnl;
                                last_trade.fees += fees;
                                last_trade.holding_period = timestamp - last_trade.timestamp;
                                last_trade.actual_return = actual_return;
                                
                                // 添加平倉的執行信息（如果部分成交）
                                if execution.partial_fill {
                                    debug!("部分平倉: 請求 {}, 實際成交 {}, 成交率 {:.2}%", 
                                           current_position, sold_quantity, execution.fill_rate * 100.0);
                                }
                            }
                            
                            // 更新持倉（如果部分成交，仍有剩餘持倉）
                            current_position -= sold_quantity;
                        },
                        ActionType::Close if current_position > 0.0 => {
                            // 強制平倉 - 使用真實成交模擬
                            let execution = self.execution_simulator.simulate_order_execution(
                                orderbook,
                                current_position,
                                OrderSide::Sell,
                                self.trading_fee
                            );
                            
                            let sold_quantity = execution.filled_quantity;
                            let exit_price = execution.filled_price;
                            let fees = execution.total_fees;
                            let pnl = sold_quantity * (exit_price - entry_price) - fees;
                            let actual_return = (exit_price - entry_price) / entry_price * 10000.0;
                            
                            current_capital += sold_quantity * exit_price - fees;
                            total_pnl += pnl;
                            total_trades += 1;
                            
                            if pnl > 0.0 {
                                winning_trades += 1;
                            }
                            
                            if let Some(last_trade) = self.trades.last_mut() {
                                last_trade.exit_price = Some(exit_price);
                                last_trade.pnl = pnl;
                                last_trade.fees += fees;
                                last_trade.holding_period = timestamp - last_trade.timestamp;
                                last_trade.actual_return = actual_return;
                            }
                            
                            current_position -= sold_quantity;
                        },
                        ActionType::Hold => {
                            // 持有，不執行任何操作
                        },
                        _ => {
                            // 其他情況（如已有倉位時嘗試買入）
                        }
                    }
                }
                
                // 計算當前總資產
                let current_price = features.mid_price.0;
                let total_value = current_capital + current_position * current_price;
                
                // 更新最大回撤
                if total_value > peak_capital {
                    peak_capital = total_value;
                }
                let drawdown = (peak_capital - total_value) / peak_capital;
                if drawdown > max_drawdown {
                    max_drawdown = drawdown;
                }
                
                // 記錄收益率
                let return_rate = (total_value - self.initial_capital) / self.initial_capital;
                returns.push(return_rate);
            }
            
            if i % 1000 == 0 {
                tokio::task::yield_now().await;
            }
        }
        
        // 計算最終統計指標
        let total_return = (current_capital - self.initial_capital) / self.initial_capital;
        let win_rate = if total_trades > 0 { winning_trades as f64 / total_trades as f64 } else { 0.0 };
        
        // 計算波動率和夏普比率
        let volatility = calculate_volatility(&returns);
        let sharpe_ratio = if volatility > 0.0 { total_return / volatility } else { 0.0 };
        
        // 計算模型性能指標
        let (avg_inference_latency_us, p99_inference_latency_us) = self.model_engine.get_inference_metrics();
        let prediction_accuracy = self.calculate_prediction_accuracy();
        let model_confidence_avg = self.calculate_average_confidence();
        let total_predictions = self.model_engine.inference_latency_us.len() as u64;
        
        // 計算動作分佈
        let mut action_distribution = HashMap::new();
        for trade in &self.trades {
            let action_type = trade.signal_type.as_str();
            *action_distribution.entry(action_type.to_string()).or_insert(0.0) += 1.0;
        }
        // 正規化為比例
        let total_actions = action_distribution.values().sum::<f64>();
        if total_actions > 0.0 {
            for value in action_distribution.values_mut() {
                *value /= total_actions;
            }
        }

        // 計算成交執行指標
        let (avg_fill_rate, avg_slippage_bps, avg_execution_time_us, 
             partial_fill_rate, immediate_fill_rate, total_liquidity_consumed) = 
            self.calculate_execution_metrics();

        self.results = Some(BacktestResults {
            // 傳統交易指標
            total_trades,
            winning_trades,
            total_pnl,
            max_drawdown,
            sharpe_ratio,
            win_rate,
            total_return,
            volatility,
            calmar_ratio: if max_drawdown > 0.0 { total_return / max_drawdown } else { 0.0 },
            sortino_ratio: calculate_sortino_ratio(&returns),
            average_trade_pnl: if total_trades > 0 { total_pnl / total_trades as f64 } else { 0.0 },
            largest_win: self.trades.iter().map(|t| t.pnl).fold(0.0, f64::max),
            largest_loss: self.trades.iter().map(|t| t.pnl).fold(0.0, f64::min),
            avg_win: calculate_avg_win(&self.trades),
            avg_loss: calculate_avg_loss(&self.trades),
            profit_factor: calculate_profit_factor(&self.trades),
            recovery_factor: if max_drawdown > 0.0 { total_pnl / max_drawdown } else { 0.0 },
            
            // 模型特定指標
            avg_inference_latency_us,
            p99_inference_latency_us,
            prediction_accuracy,
            model_confidence_avg,
            total_predictions,
            
            // RL 特定指標
            action_distribution,
            reward_cumulative: total_pnl, // 使用總PnL作為累計獎勵
            exploration_rate: 0.05, // 固定探索率
            
            // 成交執行指標
            avg_fill_rate,
            avg_slippage_bps,
            avg_execution_time_us,
            partial_fill_rate,
            immediate_fill_rate,
            total_liquidity_consumed,
        });
        
        let results = self.results.as_ref().unwrap();
        
        info!("📊 Model Backtest Results:");
        info!("   Total Trades: {}", results.total_trades);
        info!("   Win Rate: {:.1}%", results.win_rate * 100.0);
        info!("   Total Return: {:.2}%", results.total_return * 100.0);
        info!("   Sharpe Ratio: {:.2}", results.sharpe_ratio);
        info!("   Max Drawdown: {:.2}%", results.max_drawdown * 100.0);
        info!("   Profit Factor: {:.2}", results.profit_factor);
        info!("   🤖 Model Metrics:");
        info!("      Prediction Accuracy: {:.1}%", results.prediction_accuracy * 100.0);
        info!("      Avg Confidence: {:.3}", results.model_confidence_avg);
        info!("      Avg Inference Latency: {:.1}μs", results.avg_inference_latency_us);
        info!("      P99 Inference Latency: {:.1}μs", results.p99_inference_latency_us);
        info!("      Total Predictions: {}", results.total_predictions);
        info!("   📋 Execution Metrics:");
        info!("      Avg Fill Rate: {:.1}%", results.avg_fill_rate * 100.0);
        info!("      Avg Slippage: {:.2} bps", results.avg_slippage_bps);
        info!("      Avg Execution Time: {:.1}μs", results.avg_execution_time_us);
        info!("      Partial Fill Rate: {:.1}%", results.partial_fill_rate * 100.0);
        info!("      Immediate Fill Rate: {:.1}%", results.immediate_fill_rate * 100.0);
        info!("      Total Liquidity Consumed: {:.2}", results.total_liquidity_consumed);
        
        Ok(StepResult::success(&format!("Model backtest completed: {:.2}% return, {:.1}% accuracy", 
                                       results.total_return * 100.0, results.prediction_accuracy * 100.0))
           .with_metric("total_return", results.total_return)
           .with_metric("sharpe_ratio", results.sharpe_ratio)
           .with_metric("max_drawdown", results.max_drawdown)
           .with_metric("win_rate", results.win_rate)
           .with_metric("prediction_accuracy", results.prediction_accuracy)
           .with_metric("avg_inference_latency_us", results.avg_inference_latency_us)
           .with_metric("model_confidence_avg", results.model_confidence_avg))
    }
}

impl ModelBacktestStep {
    /// 計算預測準確率
    fn calculate_prediction_accuracy(&self) -> f64 {
        let correct_predictions = self.trades.iter()
            .filter(|trade| {
                let expected_positive = trade.expected_return > 0.0;
                let actual_positive = trade.actual_return > 0.0;
                expected_positive == actual_positive
            })
            .count();
        
        if self.trades.is_empty() {
            0.5 // 基準準確率
        } else {
            correct_predictions as f64 / self.trades.len() as f64
        }
    }
    
    /// 計算平均置信度
    fn calculate_average_confidence(&self) -> f64 {
        if self.trades.is_empty() {
            0.0
        } else {
            self.trades.iter().map(|t| t.model_confidence).sum::<f64>() / self.trades.len() as f64
        }
    }
    
    /// 計算成交執行指標
    fn calculate_execution_metrics(&self) -> (f64, f64, f64, f64, f64, f64) {
        if self.trades.is_empty() {
            return (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        }
        
        let mut total_fill_rate = 0.0;
        let mut total_slippage_bps = 0.0;
        let mut total_execution_time_us = 0.0;
        let mut partial_fills = 0;
        let mut immediate_fills = 0;
        let mut total_liquidity_consumed = 0.0;
        let trade_count = self.trades.len() as f64;
        
        for trade in &self.trades {
            // 累積成交率
            total_fill_rate += trade.fill_rate;
            
            // 累積滑點
            total_slippage_bps += trade.slippage_bps;
            
            // 累積執行時間
            total_execution_time_us += trade.execution_time_us as f64;
            
            // 計算部分成交
            if trade.partial_fill {
                partial_fills += 1;
            }
            
            // 計算立即成交（執行時間 < 1000μs）
            if trade.execution_time_us < 1000 {
                immediate_fills += 1;
            }
            
            // 計算流動性消耗（基於成交數量和價格）
            if let Some(execution) = &trade.execution_details {
                total_liquidity_consumed += execution.liquidity_consumed;
            } else {
                // 回退計算：基於成交數量估算
                total_liquidity_consumed += trade.quantity;
            }
        }
        
        let avg_fill_rate = total_fill_rate / trade_count;
        let avg_slippage_bps = total_slippage_bps / trade_count;
        let avg_execution_time_us = total_execution_time_us / trade_count;
        let partial_fill_rate = partial_fills as f64 / trade_count;
        let immediate_fill_rate = immediate_fills as f64 / trade_count;
        
        (avg_fill_rate, avg_slippage_bps, avg_execution_time_us, 
         partial_fill_rate, immediate_fill_rate, total_liquidity_consumed)
    }
}

// 輔助函數

fn parse_historical_orderbook(_data: &Value) -> Result<OrderBook> {
    // 簡化的OrderBook解析邏輯
    let mut orderbook = OrderBook::new("BTCUSDT".to_string());
    orderbook.last_update = now_micros();
    orderbook.is_valid = true;
    // 這裡需要根據實際數據格式進行解析...
    Ok(orderbook)
}

fn generate_backtest_signal(features: &rust_hft::FeatureSet) -> Option<String> {
    // 簡單的交易信號生成邏輯
    if features.obi_l5 > 0.3 {
        Some("BUY".to_string())
    } else if features.obi_l5 < -0.3 {
        Some("SELL".to_string())
    } else {
        None
    }
}

fn calculate_volatility(returns: &[f64]) -> f64 {
    if returns.len() < 2 {
        return 0.0;
    }
    
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter()
        .map(|r| (r - mean).powi(2))
        .sum::<f64>() / (returns.len() - 1) as f64;
    
    variance.sqrt()
}

fn calculate_sortino_ratio(returns: &[f64]) -> f64 {
    if returns.is_empty() {
        return 0.0;
    }
    
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let downside_returns: Vec<f64> = returns.iter()
        .filter(|&&r| r < 0.0)
        .cloned()
        .collect();
    
    if downside_returns.is_empty() {
        return 0.0;
    }
    
    let downside_deviation = calculate_volatility(&downside_returns);
    
    if downside_deviation > 0.0 {
        mean / downside_deviation
    } else {
        0.0
    }
}

fn calculate_avg_win(trades: &[TradeRecord]) -> f64 {
    let winning_trades: Vec<f64> = trades.iter()
        .filter(|t| t.pnl > 0.0)
        .map(|t| t.pnl)
        .collect();
    
    if winning_trades.is_empty() {
        0.0
    } else {
        winning_trades.iter().sum::<f64>() / winning_trades.len() as f64
    }
}

fn calculate_avg_loss(trades: &[TradeRecord]) -> f64 {
    let losing_trades: Vec<f64> = trades.iter()
        .filter(|t| t.pnl < 0.0)
        .map(|t| t.pnl)
        .collect();
    
    if losing_trades.is_empty() {
        0.0
    } else {
        losing_trades.iter().sum::<f64>() / losing_trades.len() as f64
    }
}

fn calculate_profit_factor(trades: &[TradeRecord]) -> f64 {
    let gross_profit: f64 = trades.iter()
        .filter(|t| t.pnl > 0.0)
        .map(|t| t.pnl)
        .sum();
    
    let gross_loss: f64 = trades.iter()
        .filter(|t| t.pnl < 0.0)
        .map(|t| t.pnl.abs())
        .sum();
    
    if gross_loss > 0.0 {
        gross_profit / gross_loss
    } else {
        0.0
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = BacktestArgs::parse();
    
    info!("📈 DL/RL Model Backtesting System");
    info!("Input: {}, Capital: ${:.2}, Fee: {:.4}%", 
          args.backtest.input_file, args.backtest.initial_capital, args.backtest.trading_fee * 100.0);
    
    // 創建模型推理引擎
    let model_path = std::env::var("MODEL_PATH")
        .unwrap_or_else(|_| "models/lob_transformer.safetensors".to_string());
    let model_type = match std::env::var("MODEL_TYPE").as_deref() {
        Ok("lob_transformer") => ModelType::LobTransformer,
        Ok("reinforcement_learning") => ModelType::ReinforcementLearning,
        Ok("deep_nn") => ModelType::DeepNN,
        _ => ModelType::LobTransformer, // 默認使用 LOB Transformer
    };
    let action_space = ActionSpace::Binary { 
        buy_threshold: 0.3, 
        sell_threshold: -0.3 
    };
    
    let model_engine = ModelInferenceEngine::new(model_path.clone(), model_type.clone(), action_space);
    
    info!("🤖 Model Configuration:");
    info!("   Model Path: {}", model_path);
    info!("   Model Type: {:?}", model_type);
    
    // 構建回測工作流
    let mut workflow = WorkflowExecutor::new()
        .add_step(DataLoadingStep::new(
            args.backtest.input_file.clone(),
            args.backtest.max_samples
        ))
        .add_step(ModelBacktestStep::new(
            model_engine,
            args.backtest.initial_capital,
            args.backtest.trading_fee
        ));
    
    // 執行回測工作流
    let start_time = std::time::Instant::now();
    let report = workflow.execute().await?;
    let total_time = start_time.elapsed();
    
    report.print_detailed_report();
    
    info!("⏱️  Total backtest time: {:.2} minutes", total_time.as_secs_f64() / 60.0);
    
    // 保存回測報告
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let report_path = format!("backtest_report_{}.json", timestamp);
    report.save_to_file(&report_path)?;
    info!("📄 Backtest report saved: {}", report_path);
    
    if report.success {
        info!("🎉 Backtesting completed successfully!");
    } else {
        warn!("⚠️  Backtesting completed with issues");
    }
    
    Ok(())
}