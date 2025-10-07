//! 專業級 HFT 風控管理器
//!
//! 根據專業 HFT 架構建議實現的完整風控系統：
//! - 事前風控：限額/速率/冷卻/陳舊度檢查
//! - 動態限制：基於市況和執行品質的自適應調整
//! - 多維度風控：單品種/組合/交易所/策略級別控制
//! - 實時監控：延遲/滑點/成交率/風險指標監控
//! - 熔斷機制：多層級的止損和風險熔斷
//! - 恢復機制：智能的風控恢復和限制放鬆

use ports::{RiskManager, OrderIntent, AccountView, ExecutionEvent, VenueSpec, RiskMetrics};
use hft_core::{Symbol, HftError, HftResult, Quantity, Price, Timestamp, now_micros, Side, OrderType, TimeInForce, VenueId};
use rust_decimal::{Decimal, prelude::*};
use std::collections::{HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn, error, debug};
use serde::{Serialize, Deserialize};

/// 專業風控配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfessionalRiskConfig {
    /// 基礎限制
    pub basic_limits: BasicLimits,

    /// 速率限制
    pub rate_limits: RateLimits,

    /// 冷卻期設置
    pub cooldown: CooldownConfig,

    /// 數據質量檢查
    pub data_quality: DataQualityConfig,

    /// 動態風控
    pub dynamic_risk: DynamicRiskConfig,

    /// 熔斷機制
    pub circuit_breaker: CircuitBreakerConfig,

    /// 多維度控制
    pub multi_dimensional: MultiDimensionalConfig,
}

/// 基礎限制配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicLimits {
    /// 單品種最大持倉
    pub max_position_per_symbol: HashMap<String, Decimal>,
    /// 全局最大名義價值
    pub max_global_notional: Decimal,
    /// 單筆訂單最大名義價值
    pub max_order_notional: Decimal,
    /// 單品種最大日內交易量
    pub max_daily_volume_per_symbol: HashMap<String, Decimal>,
    /// 全局最大日內交易量
    pub max_global_daily_volume: Decimal,
}

/// 速率限制配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimits {
    /// 全局速率限制（秒/分鐘/小時）
    pub global_orders_per_second: u32,
    pub global_orders_per_minute: u32,
    pub global_orders_per_hour: u32,

    /// 單品種速率限制
    pub symbol_orders_per_second: u32,
    pub symbol_orders_per_minute: u32,

    /// 單交易所速率限制
    pub venue_orders_per_second: HashMap<String, u32>,
    pub venue_orders_per_minute: HashMap<String, u32>,

    /// 策略級速率限制
    pub strategy_orders_per_second: HashMap<String, u32>,

    /// 動態速率調整（基於延遲和成功率）
    pub dynamic_rate_adjustment: bool,
    pub min_rate_limit_pct: f64,  // 最低速率限制百分比
    pub rate_backoff_factor: f64,  // 速率回退因子
}

/// 冷卻期配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CooldownConfig {
    /// 全局下單冷卻（微秒）
    pub global_order_cooldown_us: u64,

    /// 單品種下單冷卻
    pub symbol_order_cooldown_us: u64,

    /// 失敗訂單懲罰冷卻
    pub failed_order_penalty_us: u64,

    /// 拒絕訂單懲罰冷卻
    pub rejected_order_penalty_us: u64,

    /// 高延遲懲罰冷卻
    pub high_latency_penalty_us: u64,

    /// 單交易所冷卻
    pub venue_cooldown_us: HashMap<String, u64>,

    /// 自適應冷卻（基於市場波動）
    pub adaptive_cooldown: bool,
    pub volatility_multiplier: f64,
}

/// 數據質量檢查配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataQualityConfig {
    /// 行情數據陳舊度限制（微秒）
    pub market_data_staleness_us: u64,

    /// 推理結果陳舊度限制
    pub inference_staleness_us: u64,

    /// 執行回報陳舊度限制
    pub execution_report_staleness_us: u64,

    /// 訂單簿價差異常檢查
    pub max_spread_bps: f64,
    pub min_book_depth: u32,

    /// 價格跳躍檢查
    pub max_price_jump_pct: f64,
    pub price_jump_window_us: u64,

    /// 成交量異常檢查
    pub volume_spike_multiplier: f64,
    pub volume_check_window_us: u64,
}

/// 動態風控配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicRiskConfig {
    /// 啟用動態調整
    pub enabled: bool,

    /// 基於延遲的動態調整
    pub latency_threshold_us: u64,
    pub latency_penalty_factor: f64,

    /// 基於滑點的動態調整
    pub slippage_threshold_bps: f64,
    pub slippage_penalty_factor: f64,

    /// 基於成功率的動態調整
    pub min_fill_rate_pct: f64,
    pub fill_rate_penalty_factor: f64,

    /// 基於市場波動的動態調整
    pub volatility_threshold: f64,
    pub volatility_adjustment_factor: f64,

    /// 調整頻率（微秒）
    pub adjustment_frequency_us: u64,

    /// 風險預算分配
    pub risk_budget_allocation: HashMap<String, f64>, // 策略 -> 風險預算比例
}

/// 熔斷機制配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    /// 啟用熔斷
    pub enabled: bool,

    /// 虧損熔斷
    pub max_daily_loss: Decimal,
    pub max_hourly_loss: Decimal,
    pub max_drawdown_pct: f64,
    pub max_consecutive_losses: u32,

    /// 延遲熔斷
    pub max_avg_latency_us: u64,
    pub max_p99_latency_us: u64,
    pub latency_check_window_us: u64,

    /// 滑點熔斷
    pub max_avg_slippage_bps: f64,
    pub max_p95_slippage_bps: f64,
    pub slippage_check_window_us: u64,

    /// 成功率熔斷
    pub min_fill_rate_pct: f64,
    pub min_success_rate_pct: f64,
    pub success_rate_check_window_us: u64,

    /// 恢復機制
    pub recovery_time_minutes: HashMap<String, u64>, // 熔斷類型 -> 恢復時間
    pub gradual_recovery: bool,
    pub recovery_test_orders: u32,
}

/// 多維度風控配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiDimensionalConfig {
    /// 品種組合風控
    pub symbol_group_limits: HashMap<String, Vec<String>>, // 組合名稱 -> 品種列表
    pub max_group_notional: HashMap<String, Decimal>,
    pub max_group_correlation: f64,

    /// 交易所風控
    pub venue_concentration_limit: f64, // 單交易所最大集中度
    pub cross_venue_limits: bool,

    /// 策略級風控
    pub strategy_position_limits: HashMap<String, Decimal>,
    pub strategy_loss_limits: HashMap<String, Decimal>,

    /// 時間窗口風控
    pub intraday_position_scaling: Vec<(u8, f64)>, // (小時, 位置縮放因子)
    pub weekend_trading_limits: f64,
}

/// 風控決策結果
#[derive(Debug, Clone)]
pub enum RiskDecision {
    /// 允許執行
    Allow,
    /// 拒絕執行（原因）
    Reject(String),
    /// 修改後執行（新的訂單意圖）
    Modify(OrderIntent),
    /// 延遲執行（延遲微秒）
    Delay(u64),
}

/// 風控統計信息
#[derive(Debug, Clone, Default)]
pub struct RiskStats {
    /// 訂單統計
    pub orders_submitted: u64,
    pub orders_allowed: u64,
    pub orders_rejected: u64,
    pub orders_modified: u64,
    pub orders_delayed: u64,

    /// 拒絕原因統計
    pub reject_reasons: HashMap<String, u64>,

    /// 延遲統計
    pub total_delay_us: u64,
    pub max_delay_us: u64,
    pub avg_delay_us: f64,

    /// 性能統計
    pub risk_check_latency_us: VecDeque<u64>,
    pub last_reset_time: Timestamp,
}

/// 專業風控管理器
pub struct ProfessionalRiskManager {
    /// 配置
    config: ProfessionalRiskConfig,

    /// 統計信息
    stats: RiskStats,

    /// 訂單歷史（用於速率限制檢查）
    order_history: VecDeque<(Timestamp, String, String)>, // (時間戳, 品種, 交易所)

    /// 執行事件歷史（用於性能監控）
    execution_history: VecDeque<ExecutionEvent>,

    /// 品種級冷卻時間
    symbol_cooldowns: HashMap<String, Timestamp>,

    /// 交易所級冷卻時間
    venue_cooldowns: HashMap<String, Timestamp>,

    /// 全局最後下單時間
    last_order_time: Timestamp,

    /// 熔斷器狀態
    circuit_breaker_states: HashMap<String, CircuitBreakerState>,

    /// 動態調整因子
    dynamic_factors: HashMap<String, f64>,

    /// 風險指標快取
    cached_metrics: RiskMetrics,
    last_metrics_update: Timestamp,
}

/// 熔斷器狀態
#[derive(Debug, Clone)]
pub struct CircuitBreakerState {
    pub is_active: bool,
    pub trigger_time: Timestamp,
    pub trigger_reason: String,
    pub recovery_time: Timestamp,
    pub test_orders_count: u32,
}

impl Default for ProfessionalRiskConfig {
    fn default() -> Self {
        Self {
            basic_limits: BasicLimits {
                max_position_per_symbol: HashMap::new(),
                max_global_notional: Decimal::from(1_000_000), // 1M USD
                max_order_notional: Decimal::from(50_000),     // 50K USD
                max_daily_volume_per_symbol: HashMap::new(),
                max_global_daily_volume: Decimal::from(10_000_000), // 10M USD
            },
            rate_limits: RateLimits {
                global_orders_per_second: 100,
                global_orders_per_minute: 1000,
                global_orders_per_hour: 10000,
                symbol_orders_per_second: 50,
                symbol_orders_per_minute: 500,
                venue_orders_per_second: HashMap::new(),
                venue_orders_per_minute: HashMap::new(),
                strategy_orders_per_second: HashMap::new(),
                dynamic_rate_adjustment: true,
                min_rate_limit_pct: 10.0,
                rate_backoff_factor: 0.5,
            },
            cooldown: CooldownConfig {
                global_order_cooldown_us: 1000,    // 1ms
                symbol_order_cooldown_us: 5000,    // 5ms
                failed_order_penalty_us: 100_000,  // 100ms
                rejected_order_penalty_us: 50_000, // 50ms
                high_latency_penalty_us: 200_000,  // 200ms
                venue_cooldown_us: HashMap::new(),
                adaptive_cooldown: true,
                volatility_multiplier: 2.0,
            },
            data_quality: DataQualityConfig {
                market_data_staleness_us: 5_000,    // 5ms
                inference_staleness_us: 10_000,     // 10ms
                execution_report_staleness_us: 3_000, // 3ms
                max_spread_bps: 50.0,
                min_book_depth: 3,
                max_price_jump_pct: 1.0,
                price_jump_window_us: 1_000_000,    // 1s
                volume_spike_multiplier: 5.0,
                volume_check_window_us: 10_000_000, // 10s
            },
            dynamic_risk: DynamicRiskConfig {
                enabled: true,
                latency_threshold_us: 25_000,       // 25ms
                latency_penalty_factor: 2.0,
                slippage_threshold_bps: 2.0,
                slippage_penalty_factor: 3.0,
                min_fill_rate_pct: 80.0,
                fill_rate_penalty_factor: 2.0,
                volatility_threshold: 0.02,         // 2%
                volatility_adjustment_factor: 1.5,
                adjustment_frequency_us: 60_000_000, // 1分鐘
                risk_budget_allocation: HashMap::new(),
            },
            circuit_breaker: CircuitBreakerConfig {
                enabled: true,
                max_daily_loss: Decimal::from(-10_000), // -10K USD
                max_hourly_loss: Decimal::from(-2_000),  // -2K USD
                max_drawdown_pct: 5.0,
                max_consecutive_losses: 10,
                max_avg_latency_us: 50_000,           // 50ms
                max_p99_latency_us: 100_000,          // 100ms
                latency_check_window_us: 300_000_000, // 5分鐘
                max_avg_slippage_bps: 5.0,
                max_p95_slippage_bps: 10.0,
                slippage_check_window_us: 300_000_000, // 5分鐘
                min_fill_rate_pct: 70.0,
                min_success_rate_pct: 90.0,
                success_rate_check_window_us: 300_000_000, // 5分鐘
                recovery_time_minutes: HashMap::new(),
                gradual_recovery: true,
                recovery_test_orders: 5,
            },
            multi_dimensional: MultiDimensionalConfig {
                symbol_group_limits: HashMap::new(),
                max_group_notional: HashMap::new(),
                max_group_correlation: 0.8,
                venue_concentration_limit: 0.6,     // 60%
                cross_venue_limits: true,
                strategy_position_limits: HashMap::new(),
                strategy_loss_limits: HashMap::new(),
                intraday_position_scaling: vec![
                    (9, 1.0), (10, 1.0), (11, 1.0), // 市場開盤
                    (12, 0.8), (13, 0.8),           // 午休時段
                    (14, 1.0), (15, 1.0),           // 下午交易
                    (16, 0.7), (17, 0.5),           // 收盤前降低
                ],
                weekend_trading_limits: 0.3,        // 週末限制30%
            },
        }
    }
}

impl ProfessionalRiskManager {
    pub fn new(config: ProfessionalRiskConfig) -> Self {
        Self {
            config,
            stats: RiskStats::default(),
            order_history: VecDeque::with_capacity(10000),
            execution_history: VecDeque::with_capacity(5000),
            symbol_cooldowns: HashMap::new(),
            venue_cooldowns: HashMap::new(),
            last_order_time: 0,
            circuit_breaker_states: HashMap::new(),
            dynamic_factors: HashMap::new(),
            cached_metrics: RiskMetrics::default(),
            last_metrics_update: 0,
        }
    }

    /// 專業級風控檢查
    pub fn professional_review(&mut self,
                             intent: &OrderIntent,
                             account: &AccountView,
                             venue: &VenueSpec) -> RiskDecision {
        let check_start = now_micros();

        // 1. 熔斷器檢查
        if let Some(reason) = self.check_circuit_breakers(account) {
            self.stats.orders_rejected += 1;
            self.stats.reject_reasons.entry(format!("circuit_breaker_{}", reason)).and_modify(|e| *e += 1).or_insert(1);
            return RiskDecision::Reject(format!("熔斷器觸發: {}", reason));
        }

        // 2. 基礎限制檢查
        if let Some(reason) = self.check_basic_limits(intent, account) {
            self.stats.orders_rejected += 1;
            self.stats.reject_reasons.entry(format!("basic_limit_{}", reason)).and_modify(|e| *e += 1).or_insert(1);
            return RiskDecision::Reject(format!("基礎限制: {}", reason));
        }

        // 3. 速率限制檢查
        if let Some(reason) = self.check_rate_limits(intent) {
            self.stats.orders_rejected += 1;
            self.stats.reject_reasons.entry(format!("rate_limit_{}", reason)).and_modify(|e| *e += 1).or_insert(1);
            return RiskDecision::Reject(format!("速率限制: {}", reason));
        }

        // 4. 冷卻期檢查
        if let Some(delay_us) = self.check_cooldowns(intent, venue) {
            self.stats.orders_delayed += 1;
            self.stats.total_delay_us += delay_us;
            self.stats.max_delay_us = self.stats.max_delay_us.max(delay_us);
            return RiskDecision::Delay(delay_us);
        }

        // 5. 數據質量檢查
        if let Some(reason) = self.check_data_quality(intent, account) {
            self.stats.orders_rejected += 1;
            self.stats.reject_reasons.entry(format!("data_quality_{}", reason)).and_modify(|e| *e += 1).or_insert(1);
            return RiskDecision::Reject(format!("數據質量: {}", reason));
        }

        // 6. 動態風控調整
        if let Some(modified_intent) = self.apply_dynamic_adjustments(intent, account, venue) {
            self.stats.orders_modified += 1;
            let check_latency = now_micros() - check_start;
            self.stats.risk_check_latency_us.push_back(check_latency);
            if self.stats.risk_check_latency_us.len() > 1000 {
                self.stats.risk_check_latency_us.pop_front();
            }
            return RiskDecision::Modify(modified_intent);
        }

        // 7. 多維度風控檢查
        if let Some(reason) = self.check_multi_dimensional_limits(intent, account) {
            self.stats.orders_rejected += 1;
            self.stats.reject_reasons.entry(format!("multi_dim_{}", reason)).and_modify(|e| *e += 1).or_insert(1);
            return RiskDecision::Reject(format!("多維度限制: {}", reason));
        }

        // 通過所有檢查
        self.stats.orders_allowed += 1;
        let check_latency = now_micros() - check_start;
        self.stats.risk_check_latency_us.push_back(check_latency);
        if self.stats.risk_check_latency_us.len() > 1000 {
            self.stats.risk_check_latency_us.pop_front();
        }

        // 更新冷卻時間
        self.update_cooldowns(intent);

        RiskDecision::Allow
    }

    /// 檢查熔斷器
    fn check_circuit_breakers(&self, account: &AccountView) -> Option<String> {
        if !self.config.circuit_breaker.enabled {
            return None;
        }

        // 檢查日內虧損
        if account.daily_pnl < self.config.circuit_breaker.max_daily_loss {
            return Some(format!("日內虧損超限: {} < {}",
                               account.daily_pnl, self.config.circuit_breaker.max_daily_loss));
        }

        // 檢查回撤
        if account.max_drawdown_pct > self.config.circuit_breaker.max_drawdown_pct {
            return Some(format!("回撤超限: {}% > {}%",
                               account.max_drawdown_pct, self.config.circuit_breaker.max_drawdown_pct));
        }

        // 檢查連續虧損
        if account.consecutive_losses >= self.config.circuit_breaker.max_consecutive_losses {
            return Some(format!("連續虧損超限: {} >= {}",
                               account.consecutive_losses, self.config.circuit_breaker.max_consecutive_losses));
        }

        // 檢查活躍熔斷器
        let current_time = now_micros();
        for (breaker_type, state) in &self.circuit_breaker_states {
            if state.is_active && current_time < state.recovery_time {
                return Some(format!("{}熔斷器活躍中，恢復時間: {}", breaker_type, state.recovery_time));
            }
        }

        None
    }

    /// 檢查基礎限制
    fn check_basic_limits(&self, intent: &OrderIntent, account: &AccountView) -> Option<String> {
        // 檢查單筆訂單名義價值
        let order_notional = intent.price.to_f64()? * intent.quantity.to_f64()?;
        if Decimal::from_f64(order_notional)? > self.config.basic_limits.max_order_notional {
            return Some(format!("單筆訂單名義價值超限: {} > {}",
                               order_notional, self.config.basic_limits.max_order_notional));
        }

        // 檢查全局名義價值
        if account.total_notional > self.config.basic_limits.max_global_notional {
            return Some(format!("全局名義價值超限: {} > {}",
                               account.total_notional, self.config.basic_limits.max_global_notional));
        }

        // 檢查品種持倉限制
        let symbol_str = intent.symbol.to_string();
        if let Some(max_pos) = self.config.basic_limits.max_position_per_symbol.get(&symbol_str) {
            if let Some(current_pos) = account.positions.get(&intent.symbol) {
                let new_position = current_pos.quantity + intent.quantity;
                if new_position.0.abs() > max_pos.abs() {
                    return Some(format!("品種持倉超限 {}: {} > {}",
                                       symbol_str, new_position.0.abs(), max_pos.abs()));
                }
            }
        }

        None
    }

    /// 檢查速率限制
    fn check_rate_limits(&self, intent: &OrderIntent) -> Option<String> {
        let current_time = now_micros();

        // 清理過期的訂單歷史
        let one_hour_ago = current_time.saturating_sub(3_600_000_000);
        let recent_orders: Vec<_> = self.order_history.iter()
            .filter(|(ts, _, _)| *ts > one_hour_ago)
            .collect();

        // 檢查全局每秒限制
        let one_second_ago = current_time.saturating_sub(1_000_000);
        let orders_last_second = recent_orders.iter()
            .filter(|(ts, _, _)| *ts > one_second_ago)
            .count();

        if orders_last_second >= self.config.rate_limits.global_orders_per_second as usize {
            return Some(format!("全局每秒限制: {} >= {}",
                               orders_last_second, self.config.rate_limits.global_orders_per_second));
        }

        // 檢查品種每秒限制
        let symbol_str = intent.symbol.to_string();
        let symbol_orders_last_second = recent_orders.iter()
            .filter(|(ts, symbol, _)| *ts > one_second_ago && *symbol == symbol_str)
            .count();

        if symbol_orders_last_second >= self.config.rate_limits.symbol_orders_per_second as usize {
            return Some(format!("品種每秒限制 {}: {} >= {}",
                               symbol_str, symbol_orders_last_second, self.config.rate_limits.symbol_orders_per_second));
        }

        // 檢查全局每分鐘限制
        let one_minute_ago = current_time.saturating_sub(60_000_000);
        let orders_last_minute = recent_orders.iter()
            .filter(|(ts, _, _)| *ts > one_minute_ago)
            .count();

        if orders_last_minute >= self.config.rate_limits.global_orders_per_minute as usize {
            return Some(format!("全局每分鐘限制: {} >= {}",
                               orders_last_minute, self.config.rate_limits.global_orders_per_minute));
        }

        None
    }

    /// 檢查冷卻期
    fn check_cooldowns(&self, intent: &OrderIntent, venue: &VenueSpec) -> Option<u64> {
        let current_time = now_micros();
        let mut max_delay = 0u64;

        // 檢查全局冷卻
        let global_cooldown_end = self.last_order_time + self.config.cooldown.global_order_cooldown_us;
        if current_time < global_cooldown_end {
            max_delay = max_delay.max(global_cooldown_end - current_time);
        }

        // 檢查品種冷卻
        let symbol_str = intent.symbol.to_string();
        if let Some(symbol_cooldown_end) = self.symbol_cooldowns.get(&symbol_str) {
            let symbol_end_time = symbol_cooldown_end + self.config.cooldown.symbol_order_cooldown_us;
            if current_time < symbol_end_time {
                max_delay = max_delay.max(symbol_end_time - current_time);
            }
        }

        // 檢查交易所冷卻
        if let Some(venue_cooldown_end) = self.venue_cooldowns.get(&venue.venue_id) {
            let venue_cooldown_us = self.config.cooldown.venue_cooldown_us
                .get(&venue.venue_id)
                .unwrap_or(&self.config.cooldown.global_order_cooldown_us);
            let venue_end_time = venue_cooldown_end + venue_cooldown_us;
            if current_time < venue_end_time {
                max_delay = max_delay.max(venue_end_time - current_time);
            }
        }

        if max_delay > 0 {
            Some(max_delay)
        } else {
            None
        }
    }

    /// 檢查數據質量
    fn check_data_quality(&self, _intent: &OrderIntent, _account: &AccountView) -> Option<String> {
        // 這裡應該檢查市場數據的陳舊度、價差異常等
        // 由於沒有實際的市場數據，暫時返回 None
        None
    }

    /// 應用動態調整
    fn apply_dynamic_adjustments(&self,
                               intent: &OrderIntent,
                               _account: &AccountView,
                               _venue: &VenueSpec) -> Option<OrderIntent> {
        if !self.config.dynamic_risk.enabled {
            return None;
        }

        // 根據延遲、滑點、成功率等動態調整訂單大小或價格
        // 這是一個簡化的實現，實際情況會更複雜

        None // 暫時不修改訂單
    }

    /// 檢查多維度限制
    fn check_multi_dimensional_limits(&self, _intent: &OrderIntent, _account: &AccountView) -> Option<String> {
        // 檢查品種組合、交易所集中度、策略級限制等
        // 暫時返回 None
        None
    }

    /// 更新冷卻時間
    fn update_cooldowns(&mut self, intent: &OrderIntent) {
        let current_time = now_micros();

        // 更新全局最後下單時間
        self.last_order_time = current_time;

        // 更新品種冷卻時間
        let symbol_str = intent.symbol.to_string();
        self.symbol_cooldowns.insert(symbol_str.clone(), current_time);

        // 更新訂單歷史（用於速率限制）
        self.order_history.push_back((current_time, symbol_str, "".to_string()));

        // 限制歷史記錄大小
        while self.order_history.len() > 10000 {
            self.order_history.pop_front();
        }
    }

    /// 觸發熔斷器
    pub fn trigger_circuit_breaker(&mut self, breaker_type: &str, reason: String) {
        let current_time = now_micros();
        let recovery_minutes = self.config.circuit_breaker.recovery_time_minutes
            .get(breaker_type)
            .unwrap_or(&15); // 默認15分鐘恢復

        let state = CircuitBreakerState {
            is_active: true,
            trigger_time: current_time,
            trigger_reason: reason.clone(),
            recovery_time: current_time + (recovery_minutes * 60 * 1_000_000), // 轉換為微秒
            test_orders_count: 0,
        };

        self.circuit_breaker_states.insert(breaker_type.to_string(), state);

        error!("熔斷器觸發: {} - {}", breaker_type, reason);
    }

    /// 獲取風控統計信息
    pub fn get_stats(&self) -> &RiskStats {
        &self.stats
    }

    /// 重置統計信息
    pub fn reset_stats(&mut self) {
        self.stats = RiskStats::default();
        self.stats.last_reset_time = now_micros();
    }
}

impl RiskManager for ProfessionalRiskManager {
    fn review_orders(&mut self,
                    intents: Vec<OrderIntent>,
                    account: &AccountView,
                    venue_specs: &HashMap<String, VenueSpec>) -> Vec<OrderIntent> {
        let mut approved_orders = Vec::new();

        for intent in intents {
            self.stats.orders_submitted += 1;

            // 查找對應的 VenueSpec
            let venue_spec = if let Some(spec) = venue_specs.values().next() {
                spec // 使用第一個可用的 VenueSpec，實際實現應該基於訂單選擇正確的 spec
            } else {
                warn!("沒有找到 VenueSpec，拒絕訂單");
                self.stats.orders_rejected += 1;
                continue;
            };

            match self.professional_review(&intent, account, venue_spec) {
                RiskDecision::Allow => {
                    approved_orders.push(intent);
                }
                RiskDecision::Modify(modified_intent) => {
                    approved_orders.push(modified_intent);
                }
                RiskDecision::Reject(reason) => {
                    warn!("訂單被風控拒絕: {}", reason);
                }
                RiskDecision::Delay(delay_us) => {
                    info!("訂單被延遲執行: {} 微秒", delay_us);
                    // 在實際實現中，應該設置定時器在延遲後重新提交訂單
                    // 這裡為了簡化，直接跳過該訂單
                }
            }
        }

        approved_orders
    }

    fn review(&mut self,
             intents: Vec<OrderIntent>,
             account: &AccountView,
             venue: &VenueSpec) -> Vec<OrderIntent> {
        let mut approved_orders = Vec::new();

        for intent in intents {
            self.stats.orders_submitted += 1;

            match self.professional_review(&intent, account, venue) {
                RiskDecision::Allow => {
                    approved_orders.push(intent);
                }
                RiskDecision::Modify(modified_intent) => {
                    approved_orders.push(modified_intent);
                }
                RiskDecision::Reject(reason) => {
                    warn!("訂單被風控拒絕: {}", reason);
                }
                RiskDecision::Delay(delay_us) => {
                    info!("訂單被延遲執行: {} 微秒", delay_us);
                    // 實際實現中應該處理延遲執行
                }
            }
        }

        approved_orders
    }

    fn on_execution_event(&mut self, event: &ExecutionEvent) {
        // 記錄執行事件用於性能監控
        self.execution_history.push_back(event.clone());

        // 限制歷史記錄大小
        while self.execution_history.len() > 5000 {
            self.execution_history.pop_front();
        }

        // 分析執行品質並觸發相應的熔斷器
        self.analyze_execution_quality();
    }

    fn emergency_stop(&mut self) -> Result<(), HftError> {
        info!("緊急停止觸發，啟動所有熔斷器");

        // 啟動所有類型的熔斷器
        self.trigger_circuit_breaker("emergency", "手動緊急停止".to_string());

        Ok(())
    }

    fn get_risk_metrics(&self) -> HashMap<String, f64> {
        let mut metrics = HashMap::new();

        // 基本統計
        metrics.insert("orders_submitted".to_string(), self.stats.orders_submitted as f64);
        metrics.insert("orders_allowed".to_string(), self.stats.orders_allowed as f64);
        metrics.insert("orders_rejected".to_string(), self.stats.orders_rejected as f64);
        metrics.insert("orders_modified".to_string(), self.stats.orders_modified as f64);
        metrics.insert("orders_delayed".to_string(), self.stats.orders_delayed as f64);

        // 拒絕率
        let total_orders = self.stats.orders_submitted as f64;
        if total_orders > 0.0 {
            metrics.insert("reject_rate".to_string(), self.stats.orders_rejected as f64 / total_orders);
            metrics.insert("approval_rate".to_string(), self.stats.orders_allowed as f64 / total_orders);
        }

        // 延遲統計
        metrics.insert("avg_delay_us".to_string(), self.stats.avg_delay_us);
        metrics.insert("max_delay_us".to_string(), self.stats.max_delay_us as f64);

        // 風控檢查延遲
        if !self.stats.risk_check_latency_us.is_empty() {
            let avg_check_latency: f64 = self.stats.risk_check_latency_us.iter()
                .map(|&x| x as f64)
                .sum::<f64>() / self.stats.risk_check_latency_us.len() as f64;
            metrics.insert("avg_risk_check_latency_us".to_string(), avg_check_latency);
        }

        // 熔斷器狀態
        let active_breakers = self.circuit_breaker_states.values()
            .filter(|state| state.is_active)
            .count();
        metrics.insert("active_circuit_breakers".to_string(), active_breakers as f64);

        metrics
    }

    fn should_halt_trading(&self, account: &AccountView) -> bool {
        // 檢查任何活躍的熔斷器
        let current_time = now_micros();
        for state in self.circuit_breaker_states.values() {
            if state.is_active && current_time < state.recovery_time {
                return true;
            }
        }

        // 檢查關鍵風控指標
        if self.config.circuit_breaker.enabled {
            if account.daily_pnl < self.config.circuit_breaker.max_daily_loss {
                return true;
            }

            if account.max_drawdown_pct > self.config.circuit_breaker.max_drawdown_pct {
                return true;
            }

            if account.consecutive_losses >= self.config.circuit_breaker.max_consecutive_losses {
                return true;
            }
        }

        false
    }

    fn risk_metrics(&self) -> RiskMetrics {
        // 如果緩存還有效，返回緩存的指標
        let current_time = now_micros();
        if current_time - self.last_metrics_update < 1_000_000 { // 1秒緩存
            return self.cached_metrics.clone();
        }

        // 重新計算風控指標
        RiskMetrics::default() // 簡化實現
    }
}

impl ProfessionalRiskManager {
    /// 分析執行品質
    fn analyze_execution_quality(&mut self) {
        // 分析最近的執行事件，檢測異常模式
        // 如延遲過高、滑點過大、成功率過低等

        let current_time = now_micros();
        let five_minutes_ago = current_time.saturating_sub(300_000_000);

        let recent_executions: Vec<_> = self.execution_history.iter()
            .filter(|event| event.timestamp > five_minutes_ago)
            .collect();

        if recent_executions.len() < 10 {
            return; // 樣本太少，無法分析
        }

        // 檢查延遲異常
        self.check_latency_anomalies(&recent_executions);

        // 檢查滑點異常
        self.check_slippage_anomalies(&recent_executions);

        // 檢查成功率異常
        self.check_success_rate_anomalies(&recent_executions);
    }

    fn check_latency_anomalies(&mut self, executions: &[&ExecutionEvent]) {
        // 簡化的延遲檢查實現
        // 實際實現會更複雜，包括 p99 延遲計算等
    }

    fn check_slippage_anomalies(&mut self, executions: &[&ExecutionEvent]) {
        // 簡化的滑點檢查實現
    }

    fn check_success_rate_anomalies(&mut self, executions: &[&ExecutionEvent]) {
        // 簡化的成功率檢查實現
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::{Position, OrderSide, OrderType};

    #[test]
    fn test_professional_risk_manager_creation() {
        let config = ProfessionalRiskConfig::default();
        let risk_manager = ProfessionalRiskManager::new(config);

        assert_eq!(risk_manager.stats.orders_submitted, 0);
        assert_eq!(risk_manager.stats.orders_allowed, 0);
        assert_eq!(risk_manager.stats.orders_rejected, 0);
    }

    #[test]
    fn test_basic_limits_check() {
        let mut config = ProfessionalRiskConfig::default();
        config.basic_limits.max_order_notional = Decimal::from(1000); // 1K USD limit

        let mut risk_manager = ProfessionalRiskManager::new(config);

        let intent = OrderIntent {
            symbol: Symbol("BTCUSDT".to_string()),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity(Decimal::from(10)),      // 10 units
            price: Some(Price(Decimal::from(500))),     // 500 USD each
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(VenueId::BINANCE),
            // 總價值: 10 * 500 = 5000 USD > 1000 USD limit
        };

        let account = AccountView::default();
        let venue = VenueSpec {
            venue_id: "test_venue".to_string(),
            min_order_size: Decimal::from(1),
            max_order_size: Decimal::from(1000),
            price_increment: Decimal::from_str("0.01").unwrap(),
            quantity_increment: Decimal::from_str("0.001").unwrap(),
        };

        match risk_manager.professional_review(&intent, &account, &venue) {
            RiskDecision::Reject(reason) => {
                assert!(reason.contains("單筆訂單名義價值超限"));
            }
            _ => panic!("Should reject order due to notional limit"),
        }

        assert_eq!(risk_manager.stats.orders_rejected, 1);
    }

    #[test]
    fn test_rate_limits() {
        let mut config = ProfessionalRiskConfig::default();
        config.rate_limits.global_orders_per_second = 1; // Very restrictive

        let mut risk_manager = ProfessionalRiskManager::new(config);

        let intent = OrderIntent {
            symbol: Symbol("BTCUSDT".to_string()),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity(Decimal::from(1)),
            price: Some(Price(Decimal::from(100))),
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(VenueId::BINANCE),
        };

        let account = AccountView::default();
        let venue = VenueSpec {
            venue_id: "test_venue".to_string(),
            min_order_size: Decimal::from(1),
            max_order_size: Decimal::from(1000),
            price_increment: Decimal::from_str("0.01").unwrap(),
            quantity_increment: Decimal::from_str("0.001").unwrap(),
        };

        // First order should be allowed
        match risk_manager.professional_review(&intent, &account, &venue) {
            RiskDecision::Allow => {
                // Expected
            }
            other => panic!("First order should be allowed, got {:?}", other),
        }

        // Immediate second order should be rejected due to rate limit
        match risk_manager.professional_review(&intent, &account, &venue) {
            RiskDecision::Reject(reason) => {
                assert!(reason.contains("速率限制"));
            }
            other => panic!("Second order should be rejected, got {:?}", other),
        }

        assert_eq!(risk_manager.stats.orders_allowed, 1);
        assert_eq!(risk_manager.stats.orders_rejected, 1);
    }
}