/*! 
 * 🧠 Reinforcement Learning Core Framework
 * 
 * 實現PPO(Proximal Policy Optimization)和SAC(Soft Actor-Critic)算法
 * 針對高頻交易環境優化的強化學習框架
 * 
 * 核心特性：
 * - 動態策略適應
 * - 市場狀態感知
 * - 風險約束優化
 * - 實時策略更新
 */

use crate::core::types::*;
use anyhow::Result;
use ndarray::{Array1, Array2};
use std::collections::VecDeque;
use candle_core::{Device, Tensor};
use serde::{Deserialize, Serialize};

pub mod environment;
pub mod agents;
pub mod replay_buffer;
pub mod rewards;

use environment::TradingEnvironment;
use agents::{PPOAgent, SACAgent};
use replay_buffer::ExperienceBuffer;
use rewards::RewardCalculator;

/// RL系統主控制器
pub struct RLTradingSystem {
    /// 主要智能體
    primary_agent: Box<dyn TradingAgent>,
    /// 交易環境
    environment: TradingEnvironment,
    /// 經驗回放緩衝區
    experience_buffer: ExperienceBuffer,
    /// 獎勵計算器
    reward_calculator: RewardCalculator,
    /// 市場狀態檢測器
    market_regime_detector: MarketRegimeDetector,
    /// 性能統計
    performance_stats: RLPerformanceStats,
}

/// 強化學習智能體接口
pub trait TradingAgent: Send + Sync {
    /// 根據狀態選擇動作
    fn select_action(&mut self, state: &MarketState) -> Result<TradingAction>;
    
    /// 更新策略
    fn update_policy(&mut self, experiences: &[Experience]) -> Result<()>;
    
    /// 獲取動作概率分布
    fn get_action_probabilities(&self, state: &MarketState) -> Result<Array1<f64>>;
    
    /// 評估狀態價值
    fn evaluate_state(&self, state: &MarketState) -> Result<f64>;
    
    /// 保存模型
    fn save_model(&self, path: &str) -> Result<()>;
    
    /// 載入模型
    fn load_model(&mut self, path: &str) -> Result<()>;
}

/// 市場狀態表示
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketState {
    /// 價格特徵 (OHLCV + 衍生指標)
    pub price_features: Array1<f64>,
    /// 訂單簿特徵 (深度、不平衡等)
    pub orderbook_features: Array1<f64>, 
    /// 市場微觀結構特徵
    pub microstructure_features: Array1<f64>,
    /// 技術指標特徵
    pub technical_features: Array1<f64>,
    /// 宏觀市場特徵
    pub macro_features: Array1<f64>,
    /// 時間特徵
    pub temporal_features: Array1<f64>,
    /// 狀態時間戳
    pub timestamp: Timestamp,
    /// 市場狀態 (牛市/熊市/震盪)
    pub market_regime: MarketRegime,
}

/// 交易動作定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingAction {
    /// 動作類型 (買入/賣出/持有)
    pub action_type: ActionType,
    /// 倉位大小 (-1.0 到 1.0, 負數為空頭)
    pub position_size: f64,
    /// 價格偏移 (相對於mid price的百分比)
    pub price_offset: f64,
    /// 動作信心度
    pub confidence: f64,
    /// 預期持有時間
    pub holding_duration: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionType {
    Buy,
    Sell, 
    Hold,
    ReducePosition,
    IncreasePosition,
}

/// 經驗樣本
#[derive(Debug, Clone)]
pub struct Experience {
    pub state: MarketState,
    pub action: TradingAction,
    pub reward: f64,
    pub next_state: MarketState,
    pub done: bool,
    pub info: ExperienceInfo,
}

#[derive(Debug, Clone)]
pub struct ExperienceInfo {
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub transaction_cost: f64,
    pub slippage: f64,
    pub market_impact: f64,
}

/// 市場狀態類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MarketRegime {
    Bull,      // 牛市
    Bear,      // 熊市
    Sideways,  // 震盪
    HighVol,   // 高波動
    LowVol,    // 低波動
    Crisis,    // 危機
}

/// 市場狀態檢測器
pub struct MarketRegimeDetector {
    volatility_window: VecDeque<f64>,
    trend_window: VecDeque<f64>,
    volume_window: VecDeque<f64>,
    regime_history: VecDeque<MarketRegime>,
    detection_threshold: f64,
}

impl MarketRegimeDetector {
    pub fn new() -> Self {
        Self {
            volatility_window: VecDeque::with_capacity(100),
            trend_window: VecDeque::with_capacity(100), 
            volume_window: VecDeque::with_capacity(100),
            regime_history: VecDeque::with_capacity(50),
            detection_threshold: 0.7,
        }
    }
    
    /// 檢測當前市場狀態
    pub fn detect_regime(&mut self, price_data: &[f64], volume_data: &[f64]) -> MarketRegime {
        // 計算波動率
        let volatility = self.calculate_volatility(price_data);
        self.volatility_window.push_back(volatility);
        
        // 計算趨勢強度
        let trend_strength = self.calculate_trend_strength(price_data);
        self.trend_window.push_back(trend_strength);
        
        // 計算成交量模式
        let volume_pattern = self.calculate_volume_pattern(volume_data);
        self.volume_window.push_back(volume_pattern);
        
        // 綜合判斷市場狀態
        let regime = self.classify_regime(volatility, trend_strength, volume_pattern);
        self.regime_history.push_back(regime.clone());
        
        if self.regime_history.len() > 50 {
            self.regime_history.pop_front();
        }
        
        regime
    }
    
    fn calculate_volatility(&self, prices: &[f64]) -> f64 {
        if prices.len() < 2 { return 0.0; }
        
        let returns: Vec<f64> = prices.windows(2)
            .map(|w| (w[1] / w[0] - 1.0))
            .collect();
            
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns.iter()
            .map(|r| (r - mean).powi(2))
            .sum::<f64>() / returns.len() as f64;
            
        variance.sqrt() * (252.0_f64).sqrt() // 年化波動率
    }
    
    fn calculate_trend_strength(&self, prices: &[f64]) -> f64 {
        if prices.len() < 10 { return 0.0; }
        
        // 使用線性回歸計算趨勢強度
        let n = prices.len() as f64;
        let x_sum = (0..prices.len()).sum::<usize>() as f64;
        let y_sum = prices.iter().sum::<f64>();
        let xy_sum = prices.iter().enumerate()
            .map(|(i, &y)| i as f64 * y)
            .sum::<f64>();
        let x2_sum = (0..prices.len())
            .map(|i| (i as f64).powi(2))
            .sum::<f64>();
            
        let slope = (n * xy_sum - x_sum * y_sum) / (n * x2_sum - x_sum.powi(2));
        let avg_price = y_sum / n;
        
        // 歸一化趨勢強度
        slope / avg_price
    }
    
    fn calculate_volume_pattern(&self, volumes: &[f64]) -> f64 {
        if volumes.len() < 5 { return 0.0; }
        
        let avg_volume = volumes.iter().sum::<f64>() / volumes.len() as f64;
        let recent_volume = volumes.iter().rev().take(5).sum::<f64>() / 5.0;
        
        recent_volume / avg_volume - 1.0
    }
    
    fn classify_regime(&self, volatility: f64, trend: f64, volume: f64) -> MarketRegime {
        match (volatility, trend.abs(), volume) {
            (v, _, _) if v > 0.3 => MarketRegime::HighVol,
            (v, _, _) if v < 0.1 => MarketRegime::LowVol,
            (_, t, _) if t > 0.02 && trend > 0.0 => MarketRegime::Bull,
            (_, t, _) if t > 0.02 && trend < 0.0 => MarketRegime::Bear,
            (v, _, _) if v > 0.5 => MarketRegime::Crisis,
            _ => MarketRegime::Sideways,
        }
    }
}

/// RL性能統計
#[derive(Debug, Clone)]
pub struct RLPerformanceStats {
    pub total_episodes: u64,
    pub average_reward: f64,
    pub win_rate: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub profit_factor: f64,
    pub learning_progress: f64,
    pub exploration_rate: f64,
}

impl Default for RLPerformanceStats {
    fn default() -> Self {
        Self {
            total_episodes: 0,
            average_reward: 0.0,
            win_rate: 0.0,
            sharpe_ratio: 0.0,
            max_drawdown: 0.0,
            profit_factor: 0.0,
            learning_progress: 0.0,
            exploration_rate: 1.0,
        }
    }
}

impl RLTradingSystem {
    /// 創建新的RL交易系統
    pub fn new(config: &RLConfig) -> Result<Self> {
        let environment = TradingEnvironment::new(config)?;
        let experience_buffer = ExperienceBuffer::new(config.buffer_size);
        let reward_calculator = RewardCalculator::new(config.reward_config.clone());
        let market_regime_detector = MarketRegimeDetector::new();
        
        // 根據配置選擇智能體類型
        let primary_agent: Box<dyn TradingAgent> = match config.agent_type {
            AgentType::PPO => Box::new(PPOAgent::new(config)?),
            AgentType::SAC => Box::new(SACAgent::new(config)?),
        };
        
        Ok(Self {
            primary_agent,
            environment,
            experience_buffer,
            reward_calculator,
            market_regime_detector,
            performance_stats: RLPerformanceStats::default(),
        })
    }
    
    /// 執行一個交易步驟
    pub fn step(&mut self, market_data: &MarketData) -> Result<Option<TradingSignal>> {
        // 1. 更新環境狀態
        let state = self.environment.update(market_data)?;
        
        // 2. 檢測市場狀態
        let market_regime = self.market_regime_detector.detect_regime(
            &market_data.price_history,
            &market_data.volume_history
        );
        
        // 3. 選擇動作
        let action = self.primary_agent.select_action(&state)?;
        
        // 4. 執行動作並獲取獎勵
        let (next_state, reward, done) = self.environment.execute_action(&action)?;
        
        // 5. 存儲經驗
        let experience = Experience {
            state: state.clone(),
            action: action.clone(),
            reward,
            next_state,
            done,
            info: ExperienceInfo {
                realized_pnl: 0.0, // 從環境獲取
                unrealized_pnl: 0.0,
                transaction_cost: 0.0,
                slippage: 0.0,
                market_impact: 0.0,
            },
        };
        
        self.experience_buffer.add(experience);
        
        // 6. 訓練智能體（如果有足夠經驗）
        if self.experience_buffer.is_ready_for_training() {
            let batch = self.experience_buffer.sample_batch(256)?;
            self.primary_agent.update_policy(&batch)?;
        }
        
        // 7. 轉換為交易信號
        self.convert_action_to_signal(&action, &state)
    }
    
    fn convert_action_to_signal(&self, action: &TradingAction, state: &MarketState) -> Result<Option<TradingSignal>> {
        match action.action_type {
            ActionType::Hold => Ok(None),
            ActionType::Buy | ActionType::Sell => {
                let signal_type = match action.action_type {
                    ActionType::Buy => SignalType::Buy,
                    ActionType::Sell => SignalType::Sell,
                    _ => return Ok(None),
                };
                
                // 計算建議價格和數量
                let base_price = state.price_features[0]; // 假設第一個是mid price
                let suggested_price = base_price * (1.0 + action.price_offset);
                let suggested_quantity = action.position_size.abs();
                
                Ok(Some(TradingSignal {
                    signal_type,
                    confidence: action.confidence,
                    suggested_price: suggested_price as Price,
                    suggested_quantity: suggested_quantity.to_quantity(),
                    timestamp: now_micros(),
                    features_timestamp: state.timestamp,
                    signal_latency_us: 0, // 實際測量
                }))
            },
            _ => Ok(None),
        }
    }
    
    /// 獲取性能統計
    pub fn get_performance_stats(&self) -> &RLPerformanceStats {
        &self.performance_stats
    }
    
    /// 保存RL模型
    pub fn save_models(&self, directory: &str) -> Result<()> {
        std::fs::create_dir_all(directory)?;
        self.primary_agent.save_model(&format!("{}/primary_agent.safetensors", directory))?;
        Ok(())
    }
    
    /// 載入RL模型
    pub fn load_models(&mut self, directory: &str) -> Result<()> {
        self.primary_agent.load_model(&format!("{}/primary_agent.safetensors", directory))?;
        Ok(())
    }
}

/// RL配置
#[derive(Debug, Clone)]
pub struct RLConfig {
    pub agent_type: AgentType,
    pub buffer_size: usize,
    pub learning_rate: f64,
    pub batch_size: usize,
    pub gamma: f64, // 折扣因子
    pub tau: f64,   // 軟更新參數
    pub reward_config: RewardConfig,
}

#[derive(Debug, Clone)]
pub enum AgentType {
    PPO,
    SAC,
}

#[derive(Debug, Clone)]
pub struct RewardConfig {
    pub profit_weight: f64,
    pub drawdown_penalty: f64,
    pub transaction_cost_penalty: f64,
    pub holding_penalty: f64,
    pub sharpe_bonus: f64,
}

impl Default for RLConfig {
    fn default() -> Self {
        Self {
            agent_type: AgentType::PPO,
            buffer_size: 100000,
            learning_rate: 3e-4,
            batch_size: 256,
            gamma: 0.99,
            tau: 0.005,
            reward_config: RewardConfig {
                profit_weight: 1.0,
                drawdown_penalty: 2.0,
                transaction_cost_penalty: 0.1,
                holding_penalty: 0.01,
                sharpe_bonus: 0.5,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_rl_system_creation() {
        let config = RLConfig::default();
        let rl_system = RLTradingSystem::new(&config);
        assert!(rl_system.is_ok());
    }
    
    #[test]
    fn test_market_regime_detection() {
        let mut detector = MarketRegimeDetector::new();
        
        // 模擬上漲趨勢數據
        let prices = vec![100.0, 101.0, 102.5, 104.0, 105.5];
        let volumes = vec![1000.0, 1100.0, 1200.0, 1300.0, 1400.0];
        
        let regime = detector.detect_regime(&prices, &volumes);
        println!("檢測到的市場狀態: {:?}", regime);
    }
}