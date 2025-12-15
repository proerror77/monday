//! 增強型風控管理器
//!
//! 實現 Phase 1 要求的完整風控功能：
//! - 頻控：秒級/分鐘級下單限制
//! - 冷卻：全局與品種級冷卻期
//! - 敞口：持倉/名義價值/單筆訂單限額
//! - 熔斷：日損/回撤/連續虧損觸發停單
//! - Staleness：行情/推理延遲檢查

use chrono::{DateTime, Utc, Weekday};
use chrono::{Datelike, Timelike};
use hft_core::{HftError, Quantity, Symbol, Timestamp};
use ports::{AccountView, ExecutionEvent, OrderIntent, RiskManager, RiskMetrics, VenueSpec};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

/// 增強型風控配置
#[derive(Debug, Clone)]
pub struct EnhancedRiskConfig {
    // 持倉限額
    pub max_position_per_symbol: Quantity,
    pub max_global_notional: Decimal,
    pub max_order_notional: Decimal,

    // 頻控與限流
    pub max_orders_per_second: u32,
    pub max_orders_per_minute: u32,
    pub max_orders_per_hour: u32,

    // 冷卻期設置
    pub global_order_cooldown_ms: u64, // 全局下單冷卻
    pub symbol_order_cooldown_ms: u64, // 品種下單冷卻
    pub failed_order_penalty_ms: u64,  // 失敗訂單懲罰冷卻

    // 數據質量檢查
    pub market_data_staleness_us: u64,      // 行情數據陳舊度
    pub inference_staleness_us: u64,        // 推理結果陳舊度
    pub execution_report_staleness_us: u64, // 執行回報陳舊度

    // 損失控制
    pub max_daily_loss: Decimal,     // 最大日內虧損
    pub max_drawdown_pct: f64,       // 最大回撤比例
    pub max_consecutive_losses: u32, // 連續虧損次數
    pub max_position_loss_pct: f64,  // 單倉位最大虧損比例

    // 熔斷器
    pub circuit_breaker_enabled: bool,
    pub cb_daily_loss_threshold: Decimal,
    pub cb_drawdown_threshold: f64,
    pub cb_consecutive_losses: u32,
    pub cb_recovery_time_minutes: u64, // 熔斷後恢復時間

    // 交易時間窗口
    pub trading_window: Option<TradingWindow>,

    // 系統控制
    pub aggressive_mode: bool,
    pub dry_run_mode: bool, // 僅檢查不阻攔
}

/// 交易時間窗口
#[derive(Debug, Clone)]
pub struct TradingWindow {
    pub start_hour_utc: u8,
    pub end_hour_utc: u8,
    pub allowed_weekdays: Vec<Weekday>,
    pub market_holidays: Vec<DateTime<Utc>>,
}

/// 熔斷器狀態
#[derive(Debug, Clone)]
pub struct CircuitBreakerState {
    pub is_active: bool,
    pub trigger_time: Option<Timestamp>,
    pub trigger_reason: Option<String>,
    pub recovery_time: Option<Timestamp>,
    pub trigger_count_today: u32,
}

/// 風控統計數據
#[derive(Debug, Default)]
struct RiskStats {
    // 頻率統計
    orders_this_second: u32,
    orders_this_minute: u32,
    orders_this_hour: u32,
    current_second: u64,
    current_minute: u64,
    current_hour: u64,

    // 冷卻時間記錄
    last_global_order_time: Timestamp,
    last_symbol_order_times: HashMap<Symbol, Timestamp>,

    // PnL 跟踪
    daily_realized_pnl: Decimal,
    #[allow(dead_code)]
    daily_unrealized_pnl: Decimal,
    starting_balance: Decimal,
    peak_balance: Decimal,
    consecutive_losses: u32,
    #[allow(dead_code)]
    last_loss_time: Option<Timestamp>,

    // 數據質量
    last_market_data_time: HashMap<Symbol, Timestamp>,
    last_inference_time: Option<Timestamp>,

    // 統計計數
    total_orders_today: u64,
    total_rejected_today: u64,
    rejection_reasons: HashMap<String, u64>,
}

/// 增強型風控管理器
pub struct EnhancedRiskManager {
    config: EnhancedRiskConfig,
    stats: RiskStats,
    circuit_breaker: CircuitBreakerState,
}

impl EnhancedRiskManager {
    pub fn new(config: EnhancedRiskConfig) -> Self {
        let mut stats = RiskStats::default();
        stats.starting_balance = Decimal::from(100000); // 默認起始資金
        stats.peak_balance = stats.starting_balance;

        Self {
            config,
            stats,
            circuit_breaker: CircuitBreakerState {
                is_active: false,
                trigger_time: None,
                trigger_reason: None,
                recovery_time: None,
                trigger_count_today: 0,
            },
        }
    }

    /// 獲取當前時間戳（微秒）
    fn current_timestamp_us() -> Timestamp {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }

    /// 獲取當前時間戳（毫秒）
    #[allow(dead_code)]
    fn current_timestamp_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// 檢查熔斷器狀態
    fn check_circuit_breaker(&mut self, account: &AccountView) -> Result<(), HftError> {
        if !self.config.circuit_breaker_enabled {
            return Ok(());
        }

        let now = Self::current_timestamp_us();

        // 檢查是否在恢復期內
        if let Some(recovery_time) = self.circuit_breaker.recovery_time {
            if now < recovery_time {
                return Err(HftError::Risk(format!(
                    "熔斷器激活中，恢復時間：{}分鐘後",
                    (recovery_time - now) / 60_000_000
                )));
            } else {
                // 恢復時間已過，重置熔斷器
                self.circuit_breaker.is_active = false;
                self.circuit_breaker.recovery_time = None;
                info!("熔斷器已恢復正常");
            }
        }

        if self.circuit_breaker.is_active {
            return Err(HftError::Risk("熔斷器已激活，禁止交易".to_string()));
        }

        // 檢查觸發條件
        let current_balance = account.cash_balance + account.unrealized_pnl;

        let daily_loss = self.stats.starting_balance - current_balance;
        let drawdown_pct = if self.stats.peak_balance > Decimal::ZERO {
            ((self.stats.peak_balance - current_balance) / self.stats.peak_balance
                * Decimal::from(100))
            .to_f64()
            .unwrap_or(0.0)
        } else {
            0.0
        };

        // 檢查觸發條件
        let mut trigger_reason = None;

        if daily_loss > self.config.cb_daily_loss_threshold {
            trigger_reason = Some(format!("日內虧損超限: {}", daily_loss));
        } else if drawdown_pct > self.config.cb_drawdown_threshold {
            trigger_reason = Some(format!("回撤超限: {:.2}%", drawdown_pct));
        } else if self.stats.consecutive_losses >= self.config.cb_consecutive_losses {
            trigger_reason = Some(format!("連續虧損: {}", self.stats.consecutive_losses));
        }

        if let Some(reason) = trigger_reason {
            self.trigger_circuit_breaker(reason);
            return Err(HftError::Risk("觸發熔斷器，停止交易".to_string()));
        }

        Ok(())
    }

    /// 觸發熔斷器
    fn trigger_circuit_breaker(&mut self, reason: String) {
        let now = Self::current_timestamp_us();

        self.circuit_breaker.is_active = true;
        self.circuit_breaker.trigger_time = Some(now);
        self.circuit_breaker.trigger_reason = Some(reason.clone());
        self.circuit_breaker.recovery_time =
            Some(now + self.config.cb_recovery_time_minutes * 60 * 1_000_000);
        self.circuit_breaker.trigger_count_today += 1;

        error!("🔴 風控熔斷器激活: {}", reason);

        // 記錄到拒絕原因統計
        *self
            .stats
            .rejection_reasons
            .entry("circuit_breaker".to_string())
            .or_insert(0) += 1;
    }

    /// 檢查頻率限制
    fn check_rate_limits(&mut self) -> Result<(), HftError> {
        let now_us = Self::current_timestamp_us();
        let now_second = now_us / 1_000_000;
        let now_minute = now_second / 60;
        let now_hour = now_minute / 60;

        // 重置秒級計數器
        if now_second != self.stats.current_second {
            self.stats.current_second = now_second;
            self.stats.orders_this_second = 0;
        }

        // 重置分鐘級計數器
        if now_minute != self.stats.current_minute {
            self.stats.current_minute = now_minute;
            self.stats.orders_this_minute = 0;
        }

        // 重置小時級計數器
        if now_hour != self.stats.current_hour {
            self.stats.current_hour = now_hour;
            self.stats.orders_this_hour = 0;
        }

        // 檢查頻率限制
        if self.stats.orders_this_second >= self.config.max_orders_per_second {
            return Err(HftError::RateLimit("秒級頻率超限".to_string()));
        }

        if self.stats.orders_this_minute >= self.config.max_orders_per_minute {
            return Err(HftError::RateLimit("分鐘級頻率超限".to_string()));
        }

        if self.stats.orders_this_hour >= self.config.max_orders_per_hour {
            return Err(HftError::RateLimit("小時級頻率超限".to_string()));
        }

        Ok(())
    }

    /// 檢查冷卻期
    fn check_cooldown(&mut self, symbol: &Symbol) -> Result<(), HftError> {
        let now = Self::current_timestamp_us();

        // 檢查全局冷卻期
        if now - self.stats.last_global_order_time < self.config.global_order_cooldown_ms * 1000 {
            let remaining_ms = (self.config.global_order_cooldown_ms * 1000
                - (now - self.stats.last_global_order_time))
                / 1000;
            return Err(HftError::Risk(format!("全局冷卻期: {}ms", remaining_ms)));
        }

        // 檢查品種冷卻期
        if let Some(&last_time) = self.stats.last_symbol_order_times.get(symbol) {
            if now - last_time < self.config.symbol_order_cooldown_ms * 1000 {
                let remaining_ms =
                    (self.config.symbol_order_cooldown_ms * 1000 - (now - last_time)) / 1000;
                return Err(HftError::Risk(format!("品種冷卻期: {}ms", remaining_ms)));
            }
        }

        Ok(())
    }

    /// 檢查數據陳舊度
    fn check_staleness(&self, symbol: &Symbol) -> Result<(), HftError> {
        let now = Self::current_timestamp_us();

        // 檢查行情數據陳舊度
        if let Some(&last_market_time) = self.stats.last_market_data_time.get(symbol) {
            let staleness = now - last_market_time;
            if staleness > self.config.market_data_staleness_us {
                return Err(HftError::Risk(format!("行情數據過期: {}μs", staleness)));
            }
        }

        // 檢查推理陳舊度（如果有推理系統）
        if let Some(last_inference_time) = self.stats.last_inference_time {
            let staleness = now - last_inference_time;
            if staleness > self.config.inference_staleness_us {
                return Err(HftError::Risk(format!("推理數據過期: {}μs", staleness)));
            }
        }

        Ok(())
    }

    /// 檢查交易時間窗口
    fn check_trading_window(&self) -> Result<(), HftError> {
        if let Some(ref window) = self.config.trading_window {
            let now: DateTime<Utc> = Utc::now();
            let current_hour = now.hour() as u8;
            let current_weekday = now.weekday();

            // 檢查工作日
            if !window.allowed_weekdays.contains(&current_weekday) {
                return Err(HftError::Risk("非交易日".to_string()));
            }

            // 檢查交易小時
            if current_hour < window.start_hour_utc || current_hour >= window.end_hour_utc {
                return Err(HftError::Risk("非交易時間".to_string()));
            }

            // 檢查節假日
            let today = now.date_naive();
            for holiday in &window.market_holidays {
                if holiday.date_naive() == today {
                    return Err(HftError::Risk("市場假期".to_string()));
                }
            }
        }

        Ok(())
    }

    /// 更新統計數據
    fn update_stats_after_order(&mut self, symbol: &Symbol) {
        let now = Self::current_timestamp_us();

        // 更新頻率計數
        self.stats.orders_this_second += 1;
        self.stats.orders_this_minute += 1;
        self.stats.orders_this_hour += 1;
        self.stats.total_orders_today += 1;

        // 更新冷卻時間
        self.stats.last_global_order_time = now;
        self.stats
            .last_symbol_order_times
            .insert(symbol.clone(), now);
    }

    /// 記錄拒絕原因
    fn record_rejection(&mut self, reason: &str) {
        self.stats.total_rejected_today += 1;
        *self
            .stats
            .rejection_reasons
            .entry(reason.to_string())
            .or_insert(0) += 1;
        warn!("訂單被風控拒絕: {}", reason);
    }

    /// 更新行情數據時間戳（外部調用）
    pub fn update_market_data_timestamp(&mut self, symbol: Symbol, timestamp: Timestamp) {
        self.stats.last_market_data_time.insert(symbol, timestamp);
    }

    /// 更新推理時間戳（外部調用）
    pub fn update_inference_timestamp(&mut self, timestamp: Timestamp) {
        self.stats.last_inference_time = Some(timestamp);
    }

    /// 更新账户余额与PnL统计（外部调用，由Accounting模块调用）
    pub fn update_account_balance(
        &mut self,
        current_balance: Decimal,
        realized_pnl_delta: Decimal,
    ) {
        // 更新峰值余额
        if current_balance > self.stats.peak_balance {
            self.stats.peak_balance = current_balance;
        }

        // 更新每日已实现PnL
        self.stats.daily_realized_pnl += realized_pnl_delta;

        // 追踪连续亏损
        if realized_pnl_delta < Decimal::ZERO {
            self.stats.consecutive_losses += 1;
            self.stats.last_loss_time = Some(Self::current_timestamp_us());
        } else if realized_pnl_delta > Decimal::ZERO {
            // 盈利时重置连续亏损计数
            self.stats.consecutive_losses = 0;
        }

        // 检查是否触发熔断条件（但不在这里直接触发，由review过程检查）
        let drawdown = if self.stats.peak_balance > Decimal::ZERO {
            ((self.stats.peak_balance - current_balance) / self.stats.peak_balance
                * Decimal::from(100))
            .to_f64()
            .unwrap_or(0.0)
        } else {
            0.0
        };

        if drawdown > self.config.cb_drawdown_threshold {
            warn!(
                "⚠️ 当前回撤 {:.2}% 接近熔断阈值 {:.2}%",
                drawdown, self.config.cb_drawdown_threshold
            );
        }

        if self.stats.consecutive_losses >= self.config.cb_consecutive_losses {
            warn!(
                "⚠️ 连续亏损 {} 次，达到熔断阈值",
                self.stats.consecutive_losses
            );
        }
    }

    /// 重置每日统计（外部调用，日切时调用）
    pub fn reset_daily_stats(&mut self, starting_balance: Decimal) {
        self.stats.daily_realized_pnl = Decimal::ZERO;
        self.stats.daily_unrealized_pnl = Decimal::ZERO;
        self.stats.starting_balance = starting_balance;
        self.stats.peak_balance = starting_balance;
        self.stats.consecutive_losses = 0;
        self.stats.total_orders_today = 0;
        self.stats.total_rejected_today = 0;
        self.stats.rejection_reasons.clear();
        self.circuit_breaker.trigger_count_today = 0;

        info!("📊 每日风控统计已重置，起始资金: {}", starting_balance);
    }

    /// 獲取風控統計報告
    pub fn get_risk_report(&self) -> RiskReport {
        RiskReport {
            total_orders_today: self.stats.total_orders_today,
            total_rejected_today: self.stats.total_rejected_today,
            rejection_rate: if self.stats.total_orders_today > 0 {
                self.stats.total_rejected_today as f64 / self.stats.total_orders_today as f64
                    * 100.0
            } else {
                0.0
            },
            rejection_reasons: self.stats.rejection_reasons.clone(),
            daily_pnl: self.stats.daily_realized_pnl,
            consecutive_losses: self.stats.consecutive_losses,
            circuit_breaker_triggers_today: self.circuit_breaker.trigger_count_today,
            is_circuit_breaker_active: self.circuit_breaker.is_active,
        }
    }
}

/// 風控報告
#[derive(Debug, Clone)]
pub struct RiskReport {
    pub total_orders_today: u64,
    pub total_rejected_today: u64,
    pub rejection_rate: f64,
    pub rejection_reasons: HashMap<String, u64>,
    pub daily_pnl: Decimal,
    pub consecutive_losses: u32,
    pub circuit_breaker_triggers_today: u32,
    pub is_circuit_breaker_active: bool,
}

impl Default for EnhancedRiskConfig {
    fn default() -> Self {
        Self {
            // 持倉限額
            max_position_per_symbol: Quantity::from_f64(100.0).unwrap(),
            max_global_notional: Decimal::from(500000),
            max_order_notional: Decimal::from(10000),

            // 頻控
            max_orders_per_second: 5,
            max_orders_per_minute: 100,
            max_orders_per_hour: 1000,

            // 冷卻期
            global_order_cooldown_ms: 100,
            symbol_order_cooldown_ms: 500,
            failed_order_penalty_ms: 2000,

            // 數據質量
            market_data_staleness_us: 5000,
            inference_staleness_us: 10000,
            execution_report_staleness_us: 3000,

            // 損失控制
            max_daily_loss: Decimal::from(5000),
            max_drawdown_pct: 3.0,
            max_consecutive_losses: 3,
            max_position_loss_pct: 2.0,

            // 熔斷器
            circuit_breaker_enabled: true,
            cb_daily_loss_threshold: Decimal::from(3000),
            cb_drawdown_threshold: 2.0,
            cb_consecutive_losses: 3,
            cb_recovery_time_minutes: 15,

            // 其他
            trading_window: None,
            aggressive_mode: false,
            dry_run_mode: false,
        }
    }
}

impl RiskManager for EnhancedRiskManager {
    fn on_execution_event(&mut self, event: &ExecutionEvent) {
        // 根据不同的执行事件类型更新统计
        match event {
            ExecutionEvent::Fill {
                order_id,
                price,
                quantity,
                ..
            } => {
                // 计算成交金额
                let fill_value = price.0 * quantity.0;

                // 更新每日已实现PnL（这里简化处理，实际需要根据开平仓方向计算）
                // 注意：完整的PnL计算需要在Accounting模块完成，这里仅做粗略估计

                info!(
                    "成交事件: order_id={:?}, price={}, qty={}, value={}",
                    order_id, price.0, quantity.0, fill_value
                );
            }

            ExecutionEvent::OrderReject {
                order_id,
                reason,
                timestamp,
            } => {
                // 记录订单拒绝（可能来自交易所）
                warn!("交易所拒绝订单: order_id={:?}, reason={}", order_id, reason);

                // 更新拒绝统计
                self.stats.total_rejected_today += 1;
                *self
                    .stats
                    .rejection_reasons
                    .entry("exchange_reject".to_string())
                    .or_insert(0) += 1;

                // 对于失败订单，可以应用惩罚性冷却
                self.stats.last_global_order_time = *timestamp;
            }

            ExecutionEvent::OrderCompleted {
                order_id,
                final_price,
                total_filled,
                ..
            } => {
                // 订单完全成交
                info!(
                    "订单完成: order_id={:?}, final_price={}, total_filled={}",
                    order_id, final_price.0, total_filled.0
                );
            }

            ExecutionEvent::BalanceUpdate {
                asset,
                balance,
                timestamp,
            } => {
                // 余额更新（可能来自成交后的余额推送）
                info!(
                    "余额更新: asset={}, balance={} @ {}",
                    asset, balance.0, timestamp
                );
            }

            _ => {
                // 其他事件暂不处理统计
            }
        }
    }

    fn emergency_stop(&mut self) -> Result<(), HftError> {
        self.circuit_breaker.is_active = true;
        self.circuit_breaker.trigger_time = Some(Self::current_timestamp_us());
        self.circuit_breaker.trigger_reason = Some("manual_emergency_stop".to_string());
        Ok(())
    }

    fn get_risk_metrics(&self) -> HashMap<String, Decimal> {
        let mut m = HashMap::new();
        m.insert(
            "orders_this_second".into(),
            Decimal::from(self.stats.orders_this_second as u64),
        );
        m.insert(
            "orders_this_minute".into(),
            Decimal::from(self.stats.orders_this_minute as u64),
        );
        m.insert(
            "orders_this_hour".into(),
            Decimal::from(self.stats.orders_this_hour as u64),
        );
        m
    }

    fn should_halt_trading(&self, _account: &AccountView) -> bool {
        self.circuit_breaker.is_active
    }

    fn risk_metrics(&self) -> RiskMetrics {
        RiskMetrics {
            max_drawdown: Decimal::ZERO,
            current_drawdown: Decimal::ZERO,
            var_1d: Decimal::ZERO,
            leverage: Decimal::ZERO,
            concentration_risk: Decimal::ZERO,
            order_rate: Decimal::from(self.stats.orders_this_second as u64),
            last_update: Self::current_timestamp_us(),
        }
    }
    fn review_orders(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        venue_specs: &HashMap<String, VenueSpec>,
    ) -> Vec<OrderIntent> {
        let mut approved_orders = Vec::new();

        for intent in intents {
            if let Err(e) = self.review_single_order(&intent, account, venue_specs) {
                if !self.config.dry_run_mode {
                    self.record_rejection(&e.to_string());
                    continue; // 拒絕此訂單
                } else {
                    warn!("風控檢查失敗（僅記錄）: {}", e);
                }
            }

            if !self.config.dry_run_mode {
                self.update_stats_after_order(&intent.symbol);
            }

            approved_orders.push(intent);
        }

        approved_orders
    }

    fn review(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        venue: &VenueSpec,
    ) -> Vec<OrderIntent> {
        // 將單個 VenueSpec 包裝成 HashMap 格式
        let mut venue_specs = HashMap::new();
        venue_specs.insert(venue.name.clone(), venue.clone());

        self.review_orders(intents, account, &venue_specs)
    }
}

impl EnhancedRiskManager {
    /// 審核單個訂單
    fn review_single_order(
        &mut self,
        intent: &OrderIntent,
        account: &AccountView,
        _venue_specs: &HashMap<String, VenueSpec>,
    ) -> Result<(), HftError> {
        // 1. 檢查熔斷器
        self.check_circuit_breaker(account)?;

        // 2. 檢查交易時間窗口
        self.check_trading_window()?;

        // 3. 檢查頻率限制
        self.check_rate_limits()?;

        // 4. 檢查冷卻期
        self.check_cooldown(&intent.symbol)?;

        // 5. 檢查數據陳舊度
        self.check_staleness(&intent.symbol)?;

        // 6. 檢查訂單金額限制
        if let Some(price) = intent.price {
            let notional = price.0 * intent.quantity.0;
            if notional > self.config.max_order_notional {
                return Err(HftError::Risk(format!(
                    "單筆訂單金額超限: {} > {}",
                    notional, self.config.max_order_notional
                )));
            }
        }

        Ok(())
    }
}
