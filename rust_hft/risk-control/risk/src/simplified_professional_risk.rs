//! 簡化版專業級 HFT 風控管理器
//! 基於現有 ports 定義的實際接口實現

use hft_core::{now_micros, HftError, Timestamp};
use ports::{
    AccountView, ExecutionEvent, OrderIntent, RiskConfigSnapshot, RiskConfigUpdate, RiskManager,
    RiskMetrics, VenueSpec,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{error, info, warn};

/// 簡化的專業風控配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimplifiedRiskConfig {
    /// 基礎限制
    pub max_order_notional: Decimal,
    pub max_global_notional: Decimal,

    /// 速率限制
    pub max_orders_per_second: u32,
    pub max_orders_per_minute: u32,

    /// 冷卻期（微秒）
    pub global_cooldown_us: u64,
    pub symbol_cooldown_us: u64,

    /// 損失控制
    pub max_daily_loss: Decimal,
    pub max_drawdown_pct: Decimal,

    /// 熔斷器
    pub circuit_breaker_enabled: bool,
    pub recovery_time_minutes: u64,
}

impl Default for SimplifiedRiskConfig {
    fn default() -> Self {
        Self {
            max_order_notional: Decimal::from(50_000),     // 50K USD
            max_global_notional: Decimal::from(1_000_000), // 1M USD
            max_orders_per_second: 100,
            max_orders_per_minute: 1000,
            global_cooldown_us: 1000,               // 1ms
            symbol_cooldown_us: 5000,               // 5ms
            max_daily_loss: Decimal::from(-10_000), // -10K USD
            max_drawdown_pct: Decimal::from(5),     // 5%
            circuit_breaker_enabled: true,
            recovery_time_minutes: 15,
        }
    }
}

/// 風控決策結果
#[derive(Debug, Clone)]
pub enum SimplifiedRiskDecision {
    Allow,
    Reject(String),
    Delay(u64),
}

/// 風控統計
#[derive(Debug, Clone, Default)]
pub struct SimplifiedRiskStats {
    pub orders_submitted: u64,
    pub orders_allowed: u64,
    pub orders_rejected: u64,
    pub orders_delayed: u64,
    pub reject_reasons: HashMap<String, u64>,
}

/// 簡化版專業風控管理器
pub struct SimplifiedProfessionalRiskManager {
    config: SimplifiedRiskConfig,
    stats: SimplifiedRiskStats,

    // 速率限制跟踪
    order_history: VecDeque<(Timestamp, String)>, // (時間戳, 品種)

    // 冷卻時間跟踪
    last_global_order: Timestamp,
    symbol_cooldowns: HashMap<String, Timestamp>,

    // 熔斷器狀態（原子操作，確保線程安全）
    circuit_breaker_active: AtomicBool,
    circuit_breaker_recovery_time: Timestamp,

    // 累計統計（用於計算每日損失等）
    daily_pnl: Decimal,
    max_drawdown_seen: Decimal,
    last_reset_time: Timestamp,
}

impl SimplifiedProfessionalRiskManager {
    pub fn new(config: SimplifiedRiskConfig) -> Self {
        Self {
            config,
            stats: SimplifiedRiskStats::default(),
            order_history: VecDeque::with_capacity(10000),
            last_global_order: 0,
            symbol_cooldowns: HashMap::new(),
            circuit_breaker_active: AtomicBool::new(false),
            circuit_breaker_recovery_time: 0,
            daily_pnl: Decimal::ZERO,
            max_drawdown_seen: Decimal::ZERO,
            last_reset_time: now_micros(),
        }
    }

    /// 執行專業風控檢查
    pub fn execute_risk_check(
        &mut self,
        intent: &OrderIntent,
        account: &AccountView,
    ) -> SimplifiedRiskDecision {
        self.stats.orders_submitted += 1;
        let current_time = now_micros();

        // 1. 熔斷器檢查
        if self.circuit_breaker_active.load(Ordering::SeqCst) {
            if current_time < self.circuit_breaker_recovery_time {
                self.stats.orders_rejected += 1;
                self.increment_reject_reason("circuit_breaker");
                return SimplifiedRiskDecision::Reject("熔斷器活躍中".to_string());
            } else {
                // 恢復時間到，重置熔斷器
                self.circuit_breaker_active.store(false, Ordering::SeqCst);
                info!("熔斷器已恢復");
            }
        }

        // 2. 基礎限制檢查
        if let Some(reason) = self.check_basic_limits(intent, account) {
            self.stats.orders_rejected += 1;
            self.increment_reject_reason("basic_limits");
            return SimplifiedRiskDecision::Reject(reason);
        }

        // 3. 速率限制檢查
        if let Some(reason) = self.check_rate_limits(intent, current_time) {
            self.stats.orders_rejected += 1;
            self.increment_reject_reason("rate_limits");
            return SimplifiedRiskDecision::Reject(reason);
        }

        // 4. 冷卻期檢查
        if let Some(delay_us) = self.check_cooldowns(intent, current_time) {
            self.stats.orders_delayed += 1;
            return SimplifiedRiskDecision::Delay(delay_us);
        }

        // 5. 損失控制檢查
        if let Some(reason) = self.check_loss_controls(account) {
            // 觸發熔斷器
            self.trigger_circuit_breaker(&reason);
            self.stats.orders_rejected += 1;
            self.increment_reject_reason("loss_control");
            return SimplifiedRiskDecision::Reject(reason);
        }

        // 通過所有檢查
        self.stats.orders_allowed += 1;
        self.update_tracking_data(intent, current_time);

        SimplifiedRiskDecision::Allow
    }

    fn check_basic_limits(&self, intent: &OrderIntent, account: &AccountView) -> Option<String> {
        // 檢查單筆訂單名義價值
        let price = intent.price.as_ref()?.0;
        let qty = intent.quantity.0;
        let order_notional = price * qty;

        if order_notional > self.config.max_order_notional {
            return Some(format!(
                "單筆訂單名義價值超限: {} > {}",
                order_notional, self.config.max_order_notional
            ));
        }

        // 檢查全局名義價值（簡化實現）
        let total_notional = account.cash_balance.abs() + account.unrealized_pnl.abs();
        if total_notional > self.config.max_global_notional {
            return Some(format!(
                "全局名義價值超限: {} > {}",
                total_notional, self.config.max_global_notional
            ));
        }

        None
    }

    fn check_rate_limits(
        &mut self,
        _intent: &OrderIntent,
        current_time: Timestamp,
    ) -> Option<String> {
        // 清理過期記錄
        let one_minute_ago = current_time.saturating_sub(60_000_000);
        while let Some((ts, _)) = self.order_history.front() {
            if *ts < one_minute_ago {
                self.order_history.pop_front();
            } else {
                break;
            }
        }

        // 檢查每秒限制
        let one_second_ago = current_time.saturating_sub(1_000_000);
        let orders_last_second = self
            .order_history
            .iter()
            .filter(|(ts, _)| *ts > one_second_ago)
            .count();

        if orders_last_second >= self.config.max_orders_per_second as usize {
            return Some(format!(
                "每秒訂單限制: {} >= {}",
                orders_last_second, self.config.max_orders_per_second
            ));
        }

        // 檢查每分鐘限制
        if self.order_history.len() >= self.config.max_orders_per_minute as usize {
            return Some(format!(
                "每分鐘訂單限制: {} >= {}",
                self.order_history.len(),
                self.config.max_orders_per_minute
            ));
        }

        None
    }

    fn check_cooldowns(&self, intent: &OrderIntent, current_time: Timestamp) -> Option<u64> {
        let mut max_delay = 0u64;

        // 檢查全局冷卻
        let global_cooldown_end = self.last_global_order + self.config.global_cooldown_us;
        if current_time < global_cooldown_end {
            max_delay = max_delay.max(global_cooldown_end - current_time);
        }

        // 檢查品種冷卻
        let symbol_str = intent.symbol.to_string();
        if let Some(symbol_last_order) = self.symbol_cooldowns.get(&symbol_str) {
            let symbol_cooldown_end = symbol_last_order + self.config.symbol_cooldown_us;
            if current_time < symbol_cooldown_end {
                max_delay = max_delay.max(symbol_cooldown_end - current_time);
            }
        }

        if max_delay > 0 {
            Some(max_delay)
        } else {
            None
        }
    }

    fn check_loss_controls(&self, account: &AccountView) -> Option<String> {
        if !self.config.circuit_breaker_enabled {
            return None;
        }

        // 檢查日內損失（使用 realized_pnl 作為代理）
        if account.realized_pnl < self.config.max_daily_loss {
            return Some(format!(
                "日內損失超限: {} < {}",
                account.realized_pnl, self.config.max_daily_loss
            ));
        }

        // 檢查回撤（簡化計算）
        let current_total_pnl = account.realized_pnl + account.unrealized_pnl;
        let current_drawdown_pct = if self.max_drawdown_seen > Decimal::ZERO {
            (self.max_drawdown_seen - current_total_pnl) / self.max_drawdown_seen
                * Decimal::from(100)
        } else {
            Decimal::ZERO
        };

        if current_drawdown_pct > self.config.max_drawdown_pct {
            return Some(format!(
                "回撤超限: {} > {}",
                current_drawdown_pct, self.config.max_drawdown_pct
            ));
        }

        None
    }

    fn trigger_circuit_breaker(&mut self, reason: &str) {
        let current_time = now_micros();
        self.circuit_breaker_active.store(true, Ordering::SeqCst);
        self.circuit_breaker_recovery_time =
            current_time + (self.config.recovery_time_minutes * 60 * 1_000_000);

        error!(
            "觸發熔斷器: {} (恢復時間: {} 分鐘)",
            reason, self.config.recovery_time_minutes
        );
    }

    fn update_tracking_data(&mut self, intent: &OrderIntent, current_time: Timestamp) {
        // 更新全局最後下單時間
        self.last_global_order = current_time;

        // 更新品種冷卻時間
        let symbol_str = intent.symbol.to_string();
        self.symbol_cooldowns
            .insert(symbol_str.clone(), current_time);

        // 更新訂單歷史
        self.order_history.push_back((current_time, symbol_str));

        // 限制歷史記錄大小
        while self.order_history.len() > 10000 {
            self.order_history.pop_front();
        }
    }

    fn increment_reject_reason(&mut self, reason: &str) {
        *self
            .stats
            .reject_reasons
            .entry(reason.to_string())
            .or_insert(0) += 1;
    }

    /// 獲取風控統計
    pub fn get_stats(&self) -> &SimplifiedRiskStats {
        &self.stats
    }

    /// 重置統計
    pub fn reset_stats(&mut self) {
        self.stats = SimplifiedRiskStats::default();
        self.last_reset_time = now_micros();
    }

    /// 手動觸發熔斷器
    pub fn manual_circuit_breaker(&mut self, reason: &str) {
        self.trigger_circuit_breaker(reason);
    }

    /// 檢查是否應該暫停交易
    pub fn should_halt(&self) -> bool {
        self.circuit_breaker_active.load(Ordering::SeqCst)
            && now_micros() < self.circuit_breaker_recovery_time
    }
}

impl RiskManager for SimplifiedProfessionalRiskManager {
    fn review_orders(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        _venue_specs: &HashMap<String, VenueSpec>,
    ) -> Vec<OrderIntent> {
        let mut approved_orders = Vec::new();

        for intent in intents {
            match self.execute_risk_check(&intent, account) {
                SimplifiedRiskDecision::Allow => {
                    approved_orders.push(intent);
                }
                SimplifiedRiskDecision::Reject(reason) => {
                    warn!("風控拒絕訂單: {}", reason);
                }
                SimplifiedRiskDecision::Delay(_delay_us) => {
                    info!("訂單被延遲，暫時跳過");
                    // 實際實現中應該設置定時器延遲執行
                }
            }
        }

        approved_orders
    }

    fn review(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        _venue: &VenueSpec,
    ) -> Vec<OrderIntent> {
        let mut approved_orders = Vec::new();

        for intent in intents {
            match self.execute_risk_check(&intent, account) {
                SimplifiedRiskDecision::Allow => {
                    approved_orders.push(intent);
                }
                SimplifiedRiskDecision::Reject(reason) => {
                    warn!("風控拒絕訂單: {}", reason);
                }
                SimplifiedRiskDecision::Delay(_delay_us) => {
                    info!("訂單被延遲，暫時跳過");
                }
            }
        }

        approved_orders
    }

    fn on_execution_event(&mut self, event: &ExecutionEvent) {
        // 更新 PNL 跟踪（簡化實現）
        if let ExecutionEvent::Fill {
            price, quantity, ..
        } = event
        {
            let fill_amount = price.0 * quantity.0;

            // 簡單的 PNL 累計（實際實現會更複雜）
            self.daily_pnl += fill_amount * Decimal::from_str_exact("0.001").unwrap(); // 假設 0.1% 的利潤率
            self.max_drawdown_seen = self.max_drawdown_seen.max(self.daily_pnl);
        }
    }

    fn emergency_stop(&mut self) -> Result<(), HftError> {
        self.manual_circuit_breaker("緊急停止");
        Ok(())
    }

    fn get_risk_metrics(&self) -> HashMap<String, Decimal> {
        let mut metrics = HashMap::new();

        metrics.insert(
            "orders_submitted".to_string(),
            Decimal::from(self.stats.orders_submitted),
        );
        metrics.insert(
            "orders_allowed".to_string(),
            Decimal::from(self.stats.orders_allowed),
        );
        metrics.insert(
            "orders_rejected".to_string(),
            Decimal::from(self.stats.orders_rejected),
        );
        metrics.insert(
            "orders_delayed".to_string(),
            Decimal::from(self.stats.orders_delayed),
        );

        let total = Decimal::from(self.stats.orders_submitted);
        if total > Decimal::ZERO {
            metrics.insert(
                "approval_rate".to_string(),
                Decimal::from(self.stats.orders_allowed) / total,
            );
            metrics.insert(
                "rejection_rate".to_string(),
                Decimal::from(self.stats.orders_rejected) / total,
            );
        }

        metrics.insert(
            "circuit_breaker_active".to_string(),
            if self.circuit_breaker_active.load(Ordering::SeqCst) {
                Decimal::ONE
            } else {
                Decimal::ZERO
            },
        );
        metrics.insert("daily_pnl".to_string(), self.daily_pnl);

        metrics
    }

    fn should_halt_trading(&self, _account: &AccountView) -> bool {
        self.should_halt()
    }

    fn risk_metrics(&self) -> RiskMetrics {
        RiskMetrics {
            max_drawdown: self.max_drawdown_seen,
            current_drawdown: self.max_drawdown_seen - self.daily_pnl,
            var_1d: self.daily_pnl * Decimal::from_str_exact("0.05").unwrap(), // 簡化的 VaR 計算
            leverage: Decimal::ONE,                                            // 簡化
            concentration_risk: Decimal::ZERO,                                 // 簡化
            order_rate: Decimal::from(self.stats.orders_submitted) / Decimal::from(3600), // 每小時訂單數
            last_update: now_micros(),
        }
    }

    fn update_config(&mut self, update: RiskConfigUpdate) -> Result<(), HftError> {
        if let Some(max_drawdown_pct) = update.max_drawdown_pct {
            self.config.max_drawdown_pct =
                Decimal::from_f64_retain(max_drawdown_pct).unwrap_or(Decimal::from(5));
            info!("風控配置更新: max_drawdown_pct = {}", self.config.max_drawdown_pct);
        }

        if let Some(max_position_usd) = update.max_position_usd {
            self.config.max_global_notional =
                Decimal::from_f64_retain(max_position_usd).unwrap_or(Decimal::ZERO);
            info!(
                "風控配置更新: max_global_notional = {}",
                self.config.max_global_notional
            );
        }

        if let Some(max_order_size_usd) = update.max_order_size_usd {
            self.config.max_order_notional =
                Decimal::from_f64_retain(max_order_size_usd).unwrap_or(Decimal::ZERO);
            info!(
                "風控配置更新: max_order_notional = {}",
                self.config.max_order_notional
            );
        }

        if let Some(max_orders_per_second) = update.max_orders_per_second {
            self.config.max_orders_per_second = max_orders_per_second as u32;
            info!(
                "風控配置更新: max_orders_per_second = {}",
                self.config.max_orders_per_second
            );
        }

        Ok(())
    }

    fn get_config_snapshot(&self) -> RiskConfigSnapshot {
        RiskConfigSnapshot {
            max_drawdown_pct: self.config.max_drawdown_pct.to_string().parse().unwrap_or(5.0),
            max_position_usd: self
                .config
                .max_global_notional
                .to_string()
                .parse()
                .unwrap_or(0.0),
            max_order_size_usd: self
                .config
                .max_order_notional
                .to_string()
                .parse()
                .unwrap_or(0.0),
            latency_threshold_us: 0, // 此實現不使用延遲閾值
            max_orders_per_second: self.config.max_orders_per_second as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderType, Price, Quantity, Side, Symbol, TimeInForce, VenueId};
    use rust_decimal::Decimal;

    #[test]
    fn test_simplified_risk_manager() {
        let config = SimplifiedRiskConfig::default();
        let mut risk_manager = SimplifiedProfessionalRiskManager::new(config);

        let intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity(Decimal::from(1)),
            price: Some(Price(Decimal::from(50000))), // 50K USD，在限制內
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(VenueId::BINANCE),
        };

        let account = AccountView {
            cash_balance: Decimal::from(100_000),
            positions: std::collections::HashMap::new(),
            unrealized_pnl: Decimal::ZERO,
            realized_pnl: Decimal::ZERO,
        };

        // 第一筆訂單應該通過
        match risk_manager.execute_risk_check(&intent, &account) {
            SimplifiedRiskDecision::Allow => {
                // 預期結果
            }
            other => panic!("第一筆訂單應該通過，得到: {:?}", other),
        }

        assert_eq!(risk_manager.stats.orders_submitted, 1);
        assert_eq!(risk_manager.stats.orders_allowed, 1);
        assert_eq!(risk_manager.stats.orders_rejected, 0);
    }

    #[test]
    fn test_order_notional_limit() {
        let config = SimplifiedRiskConfig {
            max_order_notional: Decimal::from(1000), // 很低的限制
            ..Default::default()
        };

        let mut risk_manager = SimplifiedProfessionalRiskManager::new(config);

        let intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity(Decimal::from(1)),
            price: Some(Price(Decimal::from(50000))), // 50K USD，超過限制
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(VenueId::BINANCE),
        };

        let account = AccountView::default();

        match risk_manager.execute_risk_check(&intent, &account) {
            SimplifiedRiskDecision::Reject(reason) => {
                assert!(reason.contains("單筆訂單名義價值超限"));
            }
            other => panic!("應該拒絕超限訂單，得到: {:?}", other),
        }

        assert_eq!(risk_manager.stats.orders_rejected, 1);
    }

    #[test]
    fn test_rate_limits() {
        let config = SimplifiedRiskConfig {
            max_orders_per_second: 1, // 非常嚴格的限制
            ..Default::default()
        };

        let mut risk_manager = SimplifiedProfessionalRiskManager::new(config);

        let intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity(Decimal::from(1)),
            price: Some(Price(Decimal::from(100))),
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(VenueId::BINANCE),
        };

        let account = AccountView::default();

        // 第一筆訂單應該通過
        match risk_manager.execute_risk_check(&intent, &account) {
            SimplifiedRiskDecision::Allow => {}
            other => panic!("第一筆訂單應該通過，得到: {:?}", other),
        }

        // 第二筆訂單應該被拒絕
        match risk_manager.execute_risk_check(&intent, &account) {
            SimplifiedRiskDecision::Reject(reason) => {
                assert!(reason.contains("每秒訂單限制"));
            }
            other => panic!("第二筆訂單應該被拒絕，得到: {:?}", other),
        }

        assert_eq!(risk_manager.stats.orders_allowed, 1);
        assert_eq!(risk_manager.stats.orders_rejected, 1);
    }
}
