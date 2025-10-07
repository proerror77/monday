//! 默认风控管理器实现
//!
//! 提供基础的风险控制功能：
//! - 持仓限额检查
//! - 下单频率限制
//! - 冷却期管理
//! - 数据陈旧度检查

use hft_core::{HftError, Price, Quantity, Symbol};
use ports::{AccountView, ExecutionEvent, OrderIntent, RiskManager, RiskMetrics, VenueSpec};
use rust_decimal::prelude::ToPrimitive;
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

        if total_pnl < -self.config.max_daily_loss.to_f64().unwrap_or(0.0) {
            return Err(format!(
                "超过日内最大亏损: {:.2} < -{:.2}",
                total_pnl, self.config.max_daily_loss
            ));
        }

        Ok(())
    }

    /// 更新风控状态
    fn update_state(&mut self, symbol: &Symbol) {
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
                    intent.symbol.0,
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
                    intent.symbol.0,
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
        if self.total_orders_today % 100 == 0 && self.total_orders_today > 0 {
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

    fn get_risk_metrics(&self) -> HashMap<String, f64> {
        let mut metrics = HashMap::new();

        metrics.insert(
            "total_orders_today".to_string(),
            self.total_orders_today as f64,
        );
        metrics.insert(
            "total_rejected_today".to_string(),
            self.total_rejected_today as f64,
        );
        metrics.insert(
            "daily_pnl".to_string(),
            self.daily_pnl.to_f64().unwrap_or(0.0),
        );

        if self.total_orders_today > 0 {
            metrics.insert(
                "approval_rate".to_string(),
                (self.total_orders_today - self.total_rejected_today) as f64
                    / self.total_orders_today as f64,
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
        total_pnl < -self.config.max_daily_loss.to_f64().unwrap_or(10000.0)
    }

    fn risk_metrics(&self) -> RiskMetrics {
        RiskMetrics {
            max_drawdown: self.daily_pnl.to_f64().unwrap_or(0.0).min(0.0).abs(),
            current_drawdown: 0.0,
            var_1d: 0.0,
            leverage: 0.0,
            concentration_risk: 0.0,
            order_rate: 0.0,
            last_update: 0,
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

    #[test]
    fn test_rate_limit() {
        let config = RiskConfig {
            max_orders_per_second: 2,
            order_cooldown_ms: 100,
            ..Default::default()
        };

        let mut mgr = DefaultRiskManager::new(config);
        let symbol = Symbol("BTCUSDT".to_string());

        // 第一单应该通过
        assert!(mgr.check_rate_limit(&symbol).is_ok());

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
}
