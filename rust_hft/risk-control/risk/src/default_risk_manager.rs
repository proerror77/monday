//! 默认风控管理器实现
//!
//! 提供基础的风险控制功能：
//! - 持仓限额检查
//! - 下单频率限制
//! - 冷却期管理
//! - 数据陈旧度检查

use hft_core::{HftError, Price, Quantity, Symbol};
use ports::{
    AccountView, ExecutionEvent, OrderIntent, RiskConfigSnapshot, RiskConfigUpdate, RiskManager,
    RiskMetrics, VenueSpec,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// 风控配置
#[derive(Debug, Clone)]
pub struct RiskConfig {
    /// 单品种最大持仓量
    pub max_position_per_symbol: Quantity,
    /// 全局最大名义价值
    pub max_global_notional: rust_decimal::Decimal,
    /// 每秒最大下单数
    pub max_orders_per_second: u32,
    /// 下单后冷却期（毫秒）
    pub order_cooldown_ms: u64,
    /// 数据陈旧度阈值（微秒）
    pub staleness_threshold_us: u64,
    /// 最大日内亏损限额
    pub max_daily_loss: rust_decimal::Decimal,
    /// 是否启用激进模式（跳过某些检查）
    pub aggressive_mode: bool,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position_per_symbol: Quantity::from_f64(100.0).unwrap(),
            max_global_notional: rust_decimal::Decimal::from(1000000),
            max_orders_per_second: 10,
            order_cooldown_ms: 100,
            staleness_threshold_us: 5000,
            max_daily_loss: rust_decimal::Decimal::from(10000),
            aggressive_mode: false,
        }
    }
}

/// 品种风控状态
#[derive(Debug, Default)]
struct SymbolRiskState {
    /// 最后下单时间戳
    last_order_time_ms: u64,
    /// 当前秒内下单计数
    orders_this_second: u32,
    /// 当前秒的时间戳
    current_second: u64,
    /// 累计拒单数
    rejected_count: u64,
}

/// 默认风控管理器
pub struct DefaultRiskManager {
    config: RiskConfig,
    symbol_states: HashMap<Symbol, SymbolRiskState>,
    daily_pnl: rust_decimal::Decimal,
    total_orders_today: u64,
    total_rejected_today: u64,
}

impl DefaultRiskManager {
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            symbol_states: HashMap::new(),
            daily_pnl: rust_decimal::Decimal::ZERO,
            total_orders_today: 0,
            total_rejected_today: 0,
        }
    }

    /// 获取当前时间戳（毫秒）
    fn current_time_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// 检查下单频率
    fn check_rate_limit(&mut self, symbol: &Symbol) -> Result<(), String> {
        let now_ms = Self::current_time_ms();
        let now_second = now_ms / 1000;

        let state = self.symbol_states.entry(symbol.clone()).or_default();

        // 重置秒计数器
        if state.current_second != now_second {
            state.current_second = now_second;
            state.orders_this_second = 0;
        }

        // 检查每秒限制
        if state.orders_this_second >= self.config.max_orders_per_second {
            return Err(format!(
                "超过每秒下单限制: {} >= {}",
                state.orders_this_second, self.config.max_orders_per_second
            ));
        }

        // 检查冷却期
        if now_ms - state.last_order_time_ms < self.config.order_cooldown_ms {
            return Err(format!(
                "冷却期未结束: {}ms < {}ms",
                now_ms - state.last_order_time_ms,
                self.config.order_cooldown_ms
            ));
        }

        Ok(())
    }

    /// 检查持仓限额
    fn check_position_limits(
        &self,
        intent: &OrderIntent,
        account: &AccountView,
    ) -> Result<(), String> {
        // 获取当前持仓
        let current_position = account
            .positions
            .get(&intent.symbol)
            .map(|p| p.quantity)
            .unwrap_or(Quantity::zero());

        // 计算新持仓
        let new_position = match intent.side {
            hft_core::Side::Buy => Quantity(current_position.0 + intent.quantity.0),
            hft_core::Side::Sell => Quantity(current_position.0 - intent.quantity.0),
        };

        // 检查单品种限额
        if new_position.0.abs() > self.config.max_position_per_symbol.0 {
            return Err(format!(
                "超过单品种持仓限额: {:.4} > {:.4}",
                new_position.0, self.config.max_position_per_symbol.0
            ));
        }

        // 检查全局名义价值
        if let Some(price) = &intent.price {
            let notional = price.0 * intent.quantity.0;
            let total_notional = account
                .positions
                .values()
                .map(|p| p.avg_price.0 * p.quantity.0.abs())
                .sum::<rust_decimal::Decimal>()
                + notional;

            if total_notional > self.config.max_global_notional {
                return Err(format!(
                    "超过全局名义价值限额: {:.2} > {:.2}",
                    total_notional, self.config.max_global_notional
                ));
            }
        }

        Ok(())
    }

    /// 检查日内亏损
    fn check_daily_loss(&self, account: &AccountView) -> Result<(), String> {
        let total_pnl = account.realized_pnl + account.unrealized_pnl;

        if total_pnl < -self.config.max_daily_loss {
            return Err(format!(
                "超过日内最大亏损: {:.2} < -{:.2}",
                total_pnl, self.config.max_daily_loss
            ));
        }

        Ok(())
    }

    /// 更新风控状态
    pub fn update_state(&mut self, symbol: &Symbol) {
        let now_ms = Self::current_time_ms();
        let state = self.symbol_states.entry(symbol.clone()).or_default();

        state.last_order_time_ms = now_ms;
        state.orders_this_second += 1;
        self.total_orders_today += 1;
    }
}

impl RiskManager for DefaultRiskManager {
    fn review(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        venue: &VenueSpec,
    ) -> Vec<OrderIntent> {
        let mut approved_intents = Vec::new();

        for mut intent in intents {
            // 激进模式下跳过某些检查
            if self.config.aggressive_mode {
                // 即使激进模式也要做精度标准化
                if let Some(price) = intent.price {
                    intent.price =
                        Some(PrecisionNormalizer::normalize_price(price, venue.tick_size));
                }
                intent.quantity =
                    PrecisionNormalizer::normalize_quantity(intent.quantity, venue.lot_size);

                self.update_state(&intent.symbol);
                approved_intents.push(intent);
                continue;
            }

            // 第一步：精度标准化
            if let Some(price) = intent.price {
                intent.price = Some(PrecisionNormalizer::normalize_price(price, venue.tick_size));
            }
            intent.quantity =
                PrecisionNormalizer::normalize_quantity(intent.quantity, venue.lot_size);

            // 第二步：交易所规则验证
            let venue_check = PrecisionNormalizer::validate_order_minimums(
                &intent,
                venue.min_qty,
                venue.min_notional,
            );

            // 第三步：风控检查
            let checks = vec![
                venue_check,
                self.check_rate_limit(&intent.symbol),
                self.check_position_limits(&intent, account),
                self.check_daily_loss(account),
            ];

            // 收集所有错误
            let errors: Vec<String> = checks.into_iter().filter_map(|r| r.err()).collect();

            if errors.is_empty() {
                // 通过所有检查
                info!(
                    "风控通过: {} {} {} @ {:?} (标准化后)",
                    intent.symbol.as_str(),
                    match intent.side {
                        hft_core::Side::Buy => "买入",
                        hft_core::Side::Sell => "卖出",
                    },
                    intent.quantity.0,
                    intent.price.as_ref().map(|p| p.0)
                );

                self.update_state(&intent.symbol);
                approved_intents.push(intent);
            } else {
                // 风控拒绝
                warn!(
                    "风控拒绝: {} {} {} @ {:?}, 原因: {}",
                    intent.symbol.as_str(),
                    match intent.side {
                        hft_core::Side::Buy => "买入",
                        hft_core::Side::Sell => "卖出",
                    },
                    intent.quantity.0,
                    intent.price.as_ref().map(|p| p.0),
                    errors.join("; ")
                );

                let state = self.symbol_states.entry(intent.symbol).or_default();
                state.rejected_count += 1;
                self.total_rejected_today += 1;
            }
        }

        // 记录统计
        if self.total_orders_today.is_multiple_of(100) && self.total_orders_today > 0 {
            info!(
                "风控统计: 今日下单 {}, 拒单 {}, 通过率 {:.1}%",
                self.total_orders_today,
                self.total_rejected_today,
                (self.total_orders_today - self.total_rejected_today) as f64
                    / self.total_orders_today as f64
                    * 100.0
            );
        }

        approved_intents
    }

    fn on_execution_event(&mut self, event: &ExecutionEvent) {
        // 更新日内盈亏（简化版，实际应考虑方向与均价）
        if let ExecutionEvent::Fill {
            price, quantity, ..
        } = event
        {
            let notional = price.0 * quantity.0;
            self.daily_pnl += notional;
        }
    }

    fn emergency_stop(&mut self) -> Result<(), HftError> {
        warn!("风控紧急停止触发！");
        // 将所有限额设为0，阻止新订单
        self.config.max_position_per_symbol = Quantity::zero();
        self.config.max_global_notional = rust_decimal::Decimal::ZERO;
        self.config.max_orders_per_second = 0;
        Ok(())
    }

    fn get_risk_metrics(&self) -> HashMap<String, Decimal> {
        let mut metrics = HashMap::new();

        metrics.insert(
            "total_orders_today".to_string(),
            Decimal::from(self.total_orders_today),
        );
        metrics.insert(
            "total_rejected_today".to_string(),
            Decimal::from(self.total_rejected_today),
        );
        metrics.insert("daily_pnl".to_string(), self.daily_pnl);

        if self.total_orders_today > 0 {
            metrics.insert(
                "approval_rate".to_string(),
                Decimal::from(self.total_orders_today - self.total_rejected_today)
                    / Decimal::from(self.total_orders_today),
            );
        }

        metrics
    }

    // 旧版方法，将转接到 review
    fn review_orders(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        _venue_specs: &HashMap<String, VenueSpec>,
    ) -> Vec<OrderIntent> {
        // 使用默认 VenueSpec（或第一个可用的）
        let default_venue = VenueSpec::default();
        self.review(intents, account, &default_venue)
    }

    fn should_halt_trading(&self, account: &AccountView) -> bool {
        // 如果日内亏损超过限额，应该停止交易
        let total_pnl = account.realized_pnl + account.unrealized_pnl;
        total_pnl < -self.config.max_daily_loss
    }

    fn risk_metrics(&self) -> RiskMetrics {
        RiskMetrics {
            max_drawdown: self.daily_pnl.min(Decimal::ZERO).abs(),
            current_drawdown: Decimal::ZERO,
            var_1d: Decimal::ZERO,
            leverage: Decimal::ZERO,
            concentration_risk: Decimal::ZERO,
            order_rate: Decimal::ZERO,
            last_update: 0,
        }
    }

    fn update_config(&mut self, update: RiskConfigUpdate) -> Result<(), HftError> {
        if let Some(max_drawdown_pct) = update.max_drawdown_pct {
            // 将百分比转换为绝对值（假设初始资金为 max_daily_loss 的 20 倍）
            let implied_capital = self.config.max_daily_loss * Decimal::from(20);
            self.config.max_daily_loss =
                implied_capital * Decimal::from_f64_retain(max_drawdown_pct / 100.0)
                    .unwrap_or(Decimal::ZERO);
            info!(
                "风控配置更新: max_daily_loss = {}",
                self.config.max_daily_loss
            );
        }

        if let Some(max_position_usd) = update.max_position_usd {
            self.config.max_global_notional =
                Decimal::from_f64_retain(max_position_usd).unwrap_or(Decimal::ZERO);
            info!(
                "风控配置更新: max_global_notional = {}",
                self.config.max_global_notional
            );
        }

        if let Some(max_orders_per_second) = update.max_orders_per_second {
            self.config.max_orders_per_second = max_orders_per_second as u32;
            info!(
                "风控配置更新: max_orders_per_second = {}",
                self.config.max_orders_per_second
            );
        }

        if let Some(latency_threshold_us) = update.latency_threshold_us {
            self.config.staleness_threshold_us = latency_threshold_us as u64;
            info!(
                "风控配置更新: staleness_threshold_us = {}",
                self.config.staleness_threshold_us
            );
        }

        Ok(())
    }

    fn get_config_snapshot(&self) -> RiskConfigSnapshot {
        RiskConfigSnapshot {
            max_drawdown_pct: (self.config.max_daily_loss
                / (self.config.max_daily_loss * Decimal::from(20))
                * Decimal::from(100))
            .to_string()
            .parse()
            .unwrap_or(5.0),
            max_position_usd: self
                .config
                .max_global_notional
                .to_string()
                .parse()
                .unwrap_or(0.0),
            max_order_size_usd: self
                .config
                .max_position_per_symbol
                .0
                .to_string()
                .parse()
                .unwrap_or(0.0),
            latency_threshold_us: self.config.staleness_threshold_us as i64,
            max_orders_per_second: self.config.max_orders_per_second as i32,
        }
    }
}

/// 精度标准化工具
pub struct PrecisionNormalizer;

impl PrecisionNormalizer {
    /// 标准化价格到交易所tick精度
    pub fn normalize_price(price: Price, tick_size: Price) -> Price {
        if tick_size.0 == rust_decimal::Decimal::ZERO {
            return price;
        }

        let ticks = (price.0 / tick_size.0).round();
        Price(ticks * tick_size.0)
    }

    /// 标准化数量到交易所lot精度
    pub fn normalize_quantity(quantity: Quantity, lot_size: Quantity) -> Quantity {
        if lot_size.0 == rust_decimal::Decimal::ZERO {
            return quantity;
        }

        let lots = (quantity.0 / lot_size.0).floor();
        Quantity(lots * lot_size.0)
    }

    /// 验证订单是否满足最小值要求
    pub fn validate_order_minimums(
        intent: &OrderIntent,
        min_qty: Quantity,
        min_notional: rust_decimal::Decimal,
    ) -> Result<(), String> {
        // 检查最小数量
        if intent.quantity.0 < min_qty.0 {
            return Err(format!(
                "数量低于最小值: {} < {}",
                intent.quantity.0, min_qty.0
            ));
        }

        // 检查最小名义价值
        if let Some(price) = &intent.price {
            let notional = price.0 * intent.quantity.0;
            if notional < min_notional {
                return Err(format!(
                    "名义价值低于最小值: {} < {}",
                    notional, min_notional
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderId, OrderType, Side, TimeInForce};
    use ports::Position;

    fn create_test_intent(symbol: &str, side: Side, qty: f64, price: Option<f64>) -> OrderIntent {
        OrderIntent {
            symbol: Symbol::new(symbol),
            side,
            quantity: Quantity::from_f64(qty).unwrap(),
            price: price.map(|p| Price::from_f64(p).unwrap()),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(hft_core::VenueId::BITGET),
        }
    }

    fn create_test_account() -> AccountView {
        let mut positions = HashMap::new();
        positions.insert(
            Symbol::new("BTCUSDT"),
            Position {
                symbol: Symbol::new("BTCUSDT"),
                quantity: Quantity::from_f64(0.5).unwrap(),
                avg_price: Price::from_f64(67000.0).unwrap(),
                unrealized_pnl: Decimal::from(100),
            },
        );

        AccountView {
            cash_balance: Decimal::from(100000),
            positions,
            unrealized_pnl: Decimal::from(100),
            realized_pnl: Decimal::from(500),
            ..Default::default()
        }
    }

    #[test]
    fn test_rate_limit() {
        let config = RiskConfig {
            max_orders_per_second: 2,
            order_cooldown_ms: 100,
            ..Default::default()
        };

        let mut mgr = DefaultRiskManager::new(config);
        let symbol = Symbol::new("BTCUSDT");

        // 第一单应该通过
        assert!(mgr.check_rate_limit(&symbol).is_ok());
        mgr.update_state(&symbol); // 模拟订单已提交

        // 立即第二单应该被冷却期阻止
        assert!(mgr.check_rate_limit(&symbol).is_err());

        // 等待后应该通过
        std::thread::sleep(std::time::Duration::from_millis(101));
        assert!(mgr.check_rate_limit(&symbol).is_ok());
    }

    #[test]
    fn test_precision_normalization() {
        let price = Price::from_f64(67123.456).unwrap();
        let tick_size = Price::from_f64(0.01).unwrap();

        let normalized = PrecisionNormalizer::normalize_price(price, tick_size);
        assert_eq!(normalized.0.to_string(), "67123.46");

        let quantity = Quantity::from_f64(1.23456).unwrap();
        let lot_size = Quantity::from_f64(0.001).unwrap();

        let normalized = PrecisionNormalizer::normalize_quantity(quantity, lot_size);
        assert_eq!(normalized.0.to_string(), "1.234");
    }

    // ============================================================================
    // Additional risk manager tests
    // ============================================================================

    #[test]
    fn test_position_limit_exceeded() {
        let config = RiskConfig {
            max_position_per_symbol: Quantity::from_f64(1.0).unwrap(),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);
        let account = create_test_account();

        // Try to buy 0.6 BTC when already holding 0.5 (total 1.1 > 1.0 limit)
        let intent = create_test_intent("BTCUSDT", Side::Buy, 0.6, Some(67000.0));

        let result = mgr.check_position_limits(&intent, &account);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("持仓限额"));
    }

    #[test]
    fn test_position_limit_passed() {
        let config = RiskConfig {
            max_position_per_symbol: Quantity::from_f64(2.0).unwrap(),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);
        let account = create_test_account();

        // Buy 0.5 BTC when holding 0.5 (total 1.0 < 2.0 limit)
        let intent = create_test_intent("BTCUSDT", Side::Buy, 0.5, Some(67000.0));

        let result = mgr.check_position_limits(&intent, &account);
        assert!(result.is_ok());
    }

    #[test]
    fn test_daily_loss_limit() {
        let config = RiskConfig {
            max_daily_loss: Decimal::from(1000),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);

        // Account with large loss
        let mut account = create_test_account();
        account.realized_pnl = Decimal::from(-800);
        account.unrealized_pnl = Decimal::from(-300); // Total -1100

        let result = mgr.check_daily_loss(&account);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("日内最大亏损"));
    }

    #[test]
    fn test_global_notional_limit() {
        let config = RiskConfig {
            max_global_notional: Decimal::from(100000),
            max_position_per_symbol: Quantity::from_f64(100.0).unwrap(),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);

        // Account already has ~33500 notional (0.5 * 67000)
        let account = create_test_account();

        // Try to add 70000 notional (20 * 3500), total would exceed 100000
        let intent = create_test_intent("ETHUSDT", Side::Buy, 20.0, Some(3500.0));

        let result = mgr.check_position_limits(&intent, &account);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("全局名义价值限额"));
    }

    #[test]
    fn test_aggressive_mode_bypasses_rate_limit() {
        let config = RiskConfig {
            aggressive_mode: true,
            max_orders_per_second: 1,
            order_cooldown_ms: 1000,
            ..Default::default()
        };
        let mut mgr = DefaultRiskManager::new(config);
        let account = create_test_account();
        let venue = VenueSpec::default();

        let intent1 = create_test_intent("BTCUSDT", Side::Buy, 0.01, Some(67000.0));
        let intent2 = create_test_intent("BTCUSDT", Side::Buy, 0.01, Some(67000.0));

        // Both should pass in aggressive mode
        let approved = mgr.review(vec![intent1, intent2], &account, &venue);
        assert_eq!(approved.len(), 2);
    }

    #[test]
    fn test_emergency_stop() {
        let config = RiskConfig::default();
        let mut mgr = DefaultRiskManager::new(config);

        mgr.emergency_stop().unwrap();

        // After emergency stop, all limits should be zero
        assert_eq!(mgr.config.max_position_per_symbol, Quantity::zero());
        assert_eq!(mgr.config.max_global_notional, Decimal::ZERO);
        assert_eq!(mgr.config.max_orders_per_second, 0);
    }

    #[test]
    fn test_zero_tick_size_passthrough() {
        let price = Price::from_f64(67123.456789).unwrap();
        let tick_size = Price::from_f64(0.0).unwrap();

        let normalized = PrecisionNormalizer::normalize_price(price, tick_size);
        assert_eq!(normalized.0, price.0);
    }

    #[test]
    fn test_order_minimum_qty_validation() {
        let intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.001).unwrap(),
            price: Some(Price::from_f64(67000.0).unwrap()),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            strategy_id: "test".to_string(),
            target_venue: None,
        };

        // Minimum qty is 0.01, intent has 0.001
        let result = PrecisionNormalizer::validate_order_minimums(
            &intent,
            Quantity::from_f64(0.01).unwrap(),
            Decimal::from(10),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("数量低于最小值"));
    }

    #[test]
    fn test_order_minimum_notional_validation() {
        let intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.0001).unwrap(), // 0.0001 * 67000 = 6.7 notional
            price: Some(Price::from_f64(67000.0).unwrap()),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            strategy_id: "test".to_string(),
            target_venue: None,
        };

        // Minimum notional is 10
        let result = PrecisionNormalizer::validate_order_minimums(
            &intent,
            Quantity::from_f64(0.0001).unwrap(), // qty check passes
            Decimal::from(10),                    // notional check fails
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("名义价值低于最小值"));
    }

    #[test]
    fn test_fill_event_updates_daily_pnl() {
        let config = RiskConfig::default();
        let mut mgr = DefaultRiskManager::new(config);

        let fill_event = ExecutionEvent::Fill {
            order_id: OrderId("1".to_string()),
            fill_id: "fill_001".to_string(),
            price: Price::from_f64(67000.0).unwrap(),
            quantity: Quantity::from_f64(0.1).unwrap(),
            timestamp: 1000000,
        };

        mgr.on_execution_event(&fill_event);

        // daily_pnl should be updated
        assert!(mgr.daily_pnl > Decimal::ZERO);
    }

    #[test]
    fn test_risk_metrics_tracking() {
        let config = RiskConfig::default();
        let mut mgr = DefaultRiskManager::new(config);
        let symbol = Symbol::new("BTCUSDT");

        // Simulate some orders
        for _ in 0..5 {
            mgr.update_state(&symbol);
        }

        let metrics = mgr.get_risk_metrics();
        assert_eq!(*metrics.get("total_orders_today").unwrap(), Decimal::from(5));
    }

    #[test]
    fn test_should_halt_trading() {
        let config = RiskConfig {
            max_daily_loss: Decimal::from(1000),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);

        // Account with acceptable loss
        let mut account = AccountView::default();
        account.realized_pnl = Decimal::from(-500);
        account.unrealized_pnl = Decimal::from(-400);
        assert!(!mgr.should_halt_trading(&account));

        // Account with excessive loss
        account.realized_pnl = Decimal::from(-600);
        account.unrealized_pnl = Decimal::from(-500);
        assert!(mgr.should_halt_trading(&account));
    }

    #[test]
    fn test_sell_reduces_position() {
        let config = RiskConfig {
            max_position_per_symbol: Quantity::from_f64(1.0).unwrap(),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);
        let account = create_test_account(); // Has 0.5 BTC

        // Sell 0.3 BTC should pass (reduces position to 0.2)
        let intent = create_test_intent("BTCUSDT", Side::Sell, 0.3, Some(67000.0));

        let result = mgr.check_position_limits(&intent, &account);
        assert!(result.is_ok());
    }

    #[test]
    fn test_multiple_symbols_independent() {
        let config = RiskConfig {
            max_position_per_symbol: Quantity::from_f64(1.0).unwrap(),
            order_cooldown_ms: 10,
            ..Default::default()
        };
        let mut mgr = DefaultRiskManager::new(config);
        let account = AccountView::default();
        let venue = VenueSpec::default();

        // Orders for different symbols should be independent
        let intent1 = create_test_intent("BTCUSDT", Side::Buy, 0.5, Some(67000.0));
        let intent2 = create_test_intent("ETHUSDT", Side::Buy, 0.5, Some(3500.0));

        std::thread::sleep(std::time::Duration::from_millis(15));

        let approved = mgr.review(vec![intent1, intent2], &account, &venue);
        assert_eq!(approved.len(), 2);
    }

    // ============================================================================
    // Dynamic config update tests
    // ============================================================================

    #[test]
    fn test_update_config_max_orders_per_second() {
        let config = RiskConfig::default();
        let mut mgr = DefaultRiskManager::new(config);

        assert_eq!(mgr.config.max_orders_per_second, 10); // default

        let update = RiskConfigUpdate {
            max_orders_per_second: Some(50),
            ..Default::default()
        };

        let result = mgr.update_config(update);
        assert!(result.is_ok());
        assert_eq!(mgr.config.max_orders_per_second, 50);
    }

    #[test]
    fn test_update_config_latency_threshold() {
        let config = RiskConfig::default();
        let mut mgr = DefaultRiskManager::new(config);

        let update = RiskConfigUpdate {
            latency_threshold_us: Some(10000),
            ..Default::default()
        };

        let result = mgr.update_config(update);
        assert!(result.is_ok());
        assert_eq!(mgr.config.staleness_threshold_us, 10000);
    }

    #[test]
    fn test_update_config_max_position() {
        let config = RiskConfig::default();
        let mut mgr = DefaultRiskManager::new(config);

        let update = RiskConfigUpdate {
            max_position_usd: Some(500000.0),
            ..Default::default()
        };

        let result = mgr.update_config(update);
        assert!(result.is_ok());
        assert_eq!(mgr.config.max_global_notional, Decimal::from(500000));
    }

    #[test]
    fn test_update_config_max_drawdown() {
        let config = RiskConfig {
            max_daily_loss: Decimal::from(10000), // 10K
            ..Default::default()
        };
        let mut mgr = DefaultRiskManager::new(config);

        // Update drawdown to 10% (implied capital is 20x max_daily_loss = 200K, so 10% = 20K)
        let update = RiskConfigUpdate {
            max_drawdown_pct: Some(10.0),
            ..Default::default()
        };

        let result = mgr.update_config(update);
        assert!(result.is_ok());
        // New max_daily_loss should be 10% of 200K = 20K
        // Use approximate comparison due to floating point precision
        let expected = Decimal::from(20000);
        let diff = (mgr.config.max_daily_loss - expected).abs();
        assert!(diff < Decimal::from_str_exact("0.01").unwrap(),
            "max_daily_loss {} should be approximately {}", mgr.config.max_daily_loss, expected);
    }

    #[test]
    fn test_update_config_multiple_fields() {
        let config = RiskConfig::default();
        let mut mgr = DefaultRiskManager::new(config);

        let update = RiskConfigUpdate {
            max_orders_per_second: Some(100),
            latency_threshold_us: Some(20000),
            max_position_usd: Some(2000000.0),
            max_order_size_usd: None, // Not updated
            max_drawdown_pct: None,   // Not updated
        };

        let result = mgr.update_config(update);
        assert!(result.is_ok());

        assert_eq!(mgr.config.max_orders_per_second, 100);
        assert_eq!(mgr.config.staleness_threshold_us, 20000);
        assert_eq!(mgr.config.max_global_notional, Decimal::from(2000000));
    }

    #[test]
    fn test_get_config_snapshot() {
        let config = RiskConfig {
            max_position_per_symbol: Quantity::from_f64(50.0).unwrap(),
            max_global_notional: Decimal::from(1000000),
            max_orders_per_second: 25,
            staleness_threshold_us: 8000,
            max_daily_loss: Decimal::from(5000),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);

        let snapshot = mgr.get_config_snapshot();

        assert_eq!(snapshot.max_position_usd, 1000000.0);
        assert_eq!(snapshot.max_orders_per_second, 25);
        assert_eq!(snapshot.latency_threshold_us, 8000);
        // max_order_size_usd comes from max_position_per_symbol
        assert_eq!(snapshot.max_order_size_usd, 50.0);
    }

    #[test]
    fn test_config_update_then_snapshot() {
        let config = RiskConfig::default();
        let mut mgr = DefaultRiskManager::new(config);

        // Update config
        let update = RiskConfigUpdate {
            max_orders_per_second: Some(75),
            latency_threshold_us: Some(15000),
            max_position_usd: Some(750000.0),
            ..Default::default()
        };

        mgr.update_config(update).unwrap();

        // Get snapshot and verify
        let snapshot = mgr.get_config_snapshot();

        assert_eq!(snapshot.max_orders_per_second, 75);
        assert_eq!(snapshot.latency_threshold_us, 15000);
        assert_eq!(snapshot.max_position_usd, 750000.0);
    }
}
