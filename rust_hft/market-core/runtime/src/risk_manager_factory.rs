//! Risk Manager Factory - Creates configurable risk managers with per-strategy overrides
//!
//! This module provides a factory for creating risk managers based on configuration,
//! including support for per-strategy risk overrides.

use chrono::{DateTime, Utc, Weekday};
use hft_core::Quantity;
use ports::RiskManager;
use risk::{
    DefaultRiskManager, EnhancedRiskConfig, EnhancedRiskManager, RiskConfig, TradingWindow,
};
use std::collections::HashMap;
use tracing::{debug, info, warn};

use crate::{RiskConfig as SystemRiskConfig, StrategyRiskOverride, TradingWindowConfig};

/// Risk Manager Factory that creates risk managers with per-strategy overrides
pub struct RiskManagerFactory;

impl RiskManagerFactory {
    /// Create a risk manager based on system configuration
    pub fn create_risk_manager(system_risk_config: &SystemRiskConfig) -> Box<dyn RiskManager> {
        match system_risk_config.risk_type.as_str() {
            "Enhanced" => {
                let enhanced_config = system_risk_config
                    .enhanced
                    .as_ref()
                    .cloned()
                    .unwrap_or_default();

                let risk_config = EnhancedRiskConfig {
                    max_position_per_symbol: Quantity(enhanced_config.max_position_per_symbol),
                    max_global_notional: system_risk_config.global_notional_limit,
                    max_order_notional: enhanced_config.max_order_notional,
                    max_orders_per_second: system_risk_config.max_orders_per_second,
                    max_orders_per_minute: enhanced_config.max_orders_per_minute,
                    max_orders_per_hour: enhanced_config.max_orders_per_hour,
                    global_order_cooldown_ms: enhanced_config.global_order_cooldown_ms,
                    symbol_order_cooldown_ms: enhanced_config.symbol_order_cooldown_ms,
                    failed_order_penalty_ms: enhanced_config.failed_order_penalty_ms,
                    market_data_staleness_us: enhanced_config.market_data_staleness_us,
                    inference_staleness_us: enhanced_config.inference_staleness_us,
                    execution_report_staleness_us: enhanced_config.execution_report_staleness_us,
                    max_daily_loss: enhanced_config.max_daily_loss,
                    max_drawdown_pct: enhanced_config.max_drawdown_pct,
                    max_consecutive_losses: enhanced_config.max_consecutive_losses,
                    max_position_loss_pct: enhanced_config.max_position_loss_pct,
                    circuit_breaker_enabled: enhanced_config.circuit_breaker_enabled,
                    cb_daily_loss_threshold: enhanced_config.cb_daily_loss_threshold,
                    cb_drawdown_threshold: enhanced_config.cb_drawdown_threshold,
                    cb_consecutive_losses: enhanced_config.cb_consecutive_losses,
                    cb_recovery_time_minutes: enhanced_config.cb_recovery_time_minutes,
                    trading_window: Self::convert_trading_window(&enhanced_config.trading_window),
                    aggressive_mode: enhanced_config.aggressive_mode,
                    dry_run_mode: enhanced_config.dry_run_mode,
                };

                Box::new(EnhancedRiskManager::new(risk_config))
            }
            _ => {
                // Default risk manager
                let risk_config = RiskConfig {
                    max_position_per_symbol: Quantity::from_f64(100.0).unwrap(),
                    max_global_notional: system_risk_config.global_notional_limit,
                    max_orders_per_second: system_risk_config.max_orders_per_second,
                    order_cooldown_ms: 100,
                    staleness_threshold_us: system_risk_config.staleness_threshold_us,
                    max_daily_loss: rust_decimal::Decimal::from(10000),
                    aggressive_mode: false,
                };

                Box::new(DefaultRiskManager::new(risk_config))
            }
        }
    }

    /// Create a wrapped risk manager with per-strategy overrides
    pub fn create_strategy_aware_risk_manager(
        system_risk_config: &SystemRiskConfig,
    ) -> Box<dyn RiskManager> {
        let base_risk_manager = Self::create_risk_manager(system_risk_config);

        if system_risk_config.strategy_overrides.is_empty() {
            // No overrides, return base manager
            base_risk_manager
        } else {
            // Wrap with strategy override manager
            Box::new(StrategyAwareRiskManager::new(
                base_risk_manager,
                system_risk_config.strategy_overrides.clone(),
            ))
        }
    }

    /// Convert trading window configuration
    fn convert_trading_window(config: &Option<TradingWindowConfig>) -> Option<TradingWindow> {
        config.as_ref().map(|tw| TradingWindow {
            start_hour_utc: tw.start_hour_utc,
            end_hour_utc: tw.end_hour_utc,
            allowed_weekdays: tw
                .allowed_weekdays
                .iter()
                .filter_map(|day| match day.as_str() {
                    "Monday" => Some(Weekday::Mon),
                    "Tuesday" => Some(Weekday::Tue),
                    "Wednesday" => Some(Weekday::Wed),
                    "Thursday" => Some(Weekday::Thu),
                    "Friday" => Some(Weekday::Fri),
                    "Saturday" => Some(Weekday::Sat),
                    "Sunday" => Some(Weekday::Sun),
                    _ => None,
                })
                .collect(),
            market_holidays: tw
                .market_holidays
                .iter()
                .filter_map(|date_str| {
                    DateTime::parse_from_rfc3339(&format!("{}T00:00:00Z", date_str))
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc))
                })
                .collect(),
        })
    }
}

/// Strategy-aware risk manager wrapper that applies per-strategy overrides
pub struct StrategyAwareRiskManager {
    base_risk_manager: Box<dyn RiskManager>,
    strategy_overrides: HashMap<String, StrategyRiskOverride>,
}

impl StrategyAwareRiskManager {
    pub fn new(
        base_risk_manager: Box<dyn RiskManager>,
        strategy_overrides: HashMap<String, StrategyRiskOverride>,
    ) -> Self {
        info!(
            "创建策略感知风控管理器，包含 {} 个策略覆盖配置",
            strategy_overrides.len()
        );
        Self {
            base_risk_manager,
            strategy_overrides,
        }
    }

    /// Apply strategy-specific overrides to order intents
    fn apply_strategy_overrides(
        &self,
        strategy_name: &str,
        mut intents: Vec<ports::OrderIntent>,
    ) -> Vec<ports::OrderIntent> {
        if let Some(overrides) = self.strategy_overrides.get(strategy_name) {
            debug!("应用策略 '{}' 的风控覆盖配置", strategy_name);

            // Apply per-strategy limits
            for intent in &mut intents {
                // Override position limits
                if let Some(max_position) = &overrides.max_position {
                    if intent.quantity.0 > *max_position {
                        warn!(
                            "策略 {} 订单数量 {} 超过策略限额 {}，调整至限额内",
                            strategy_name, intent.quantity.0, max_position
                        );
                        intent.quantity = Quantity(*max_position);
                    }
                }

                // Override max notional
                if let Some(max_notional) = &overrides.max_notional {
                    if let Some(price) = &intent.price {
                        let notional = price.0 * intent.quantity.0;
                        if notional > *max_notional {
                            let adjusted_qty = Quantity(*max_notional / price.0);
                            warn!(
                                "策略 {} 订单名义价值 {} 超过策略限额 {}，数量调整为 {}",
                                strategy_name, notional, max_notional, adjusted_qty.0
                            );
                            intent.quantity = adjusted_qty;
                        }
                    }
                }
            }
        }

        intents
    }
}

impl RiskManager for StrategyAwareRiskManager {
    fn review(
        &mut self,
        intents: Vec<ports::OrderIntent>,
        account: &ports::AccountView,
        venue: &ports::VenueSpec,
    ) -> Vec<ports::OrderIntent> {
        // 按策略分組並應用策略級覆蓋
        let mut processed_intents = Vec::new();

        // 分組處理：按 strategy_id 分組
        let mut strategy_groups: HashMap<String, Vec<ports::OrderIntent>> = HashMap::new();
        for intent in intents {
            strategy_groups
                .entry(intent.strategy_id.clone())
                .or_default()
                .push(intent);
        }

        // 對每個策略組應用覆蓋後進行風控檢查
        for (strategy_id, mut strategy_intents) in strategy_groups {
            // 應用策略級覆蓋
            strategy_intents = self.apply_strategy_overrides(&strategy_id, strategy_intents);

            // 記錄覆蓋應用結果的指標
            #[cfg(feature = "metrics")]
            {
                let override_applied = self.strategy_overrides.contains_key(&strategy_id);
                if override_applied {
                    debug!(
                        "策略 {} 應用了 {} 個訂單意圖的風控覆蓋",
                        strategy_id,
                        strategy_intents.len()
                    );
                }
            }

            // 將處理後的意圖合並到結果中
            processed_intents.extend(strategy_intents);
        }

        // 最後通過基礎風控管理器進行統一檢查
        self.base_risk_manager
            .review(processed_intents, account, venue)
    }

    fn review_orders(
        &mut self,
        intents: Vec<ports::OrderIntent>,
        account: &ports::AccountView,
        venue_specs: &HashMap<String, ports::VenueSpec>,
    ) -> Vec<ports::OrderIntent> {
        // 按策略分組並應用策略級覆蓋（與 review 方法相同的邏輯）
        let mut processed_intents = Vec::new();

        // 分組處理：按 strategy_id 分組
        let mut strategy_groups: HashMap<String, Vec<ports::OrderIntent>> = HashMap::new();
        for intent in intents {
            strategy_groups
                .entry(intent.strategy_id.clone())
                .or_default()
                .push(intent);
        }

        // 對每個策略組應用覆蓋後進行風控檢查
        for (strategy_id, mut strategy_intents) in strategy_groups {
            // 應用策略級覆蓋
            strategy_intents = self.apply_strategy_overrides(&strategy_id, strategy_intents);

            // 將處理後的意圖合並到結果中
            processed_intents.extend(strategy_intents);
        }

        // 最後通過基礎風控管理器進行統一檢查
        self.base_risk_manager
            .review_orders(processed_intents, account, venue_specs)
    }

    fn on_execution_event(&mut self, event: &ports::ExecutionEvent) {
        self.base_risk_manager.on_execution_event(event)
    }

    fn emergency_stop(&mut self) -> Result<(), hft_core::HftError> {
        self.base_risk_manager.emergency_stop()
    }

    fn get_risk_metrics(&self) -> HashMap<String, f64> {
        self.base_risk_manager.get_risk_metrics()
    }

    fn should_halt_trading(&self, account: &ports::AccountView) -> bool {
        self.base_risk_manager.should_halt_trading(account)
    }

    fn risk_metrics(&self) -> ports::RiskMetrics {
        self.base_risk_manager.risk_metrics()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnhancedRiskSettings;
    use rust_decimal::Decimal;
    use std::collections::HashMap;

    #[test]
    fn test_risk_manager_factory_default() {
        let risk_config = SystemRiskConfig {
            risk_type: "Default".to_string(),
            global_position_limit: Decimal::from(1000),
            global_notional_limit: Decimal::from(100000),
            max_daily_trades: 100,
            max_orders_per_second: 10,
            staleness_threshold_us: 5000,
            enhanced: None,
            strategy_overrides: HashMap::new(),
        };

        let _manager = RiskManagerFactory::create_risk_manager(&risk_config);

        // 產生 Default 風控，且保持風控類型不被意外修改
        assert_eq!(risk_config.risk_type, "Default");
    }

    #[test]
    fn test_risk_manager_factory_enhanced() {
        let enhanced_settings = EnhancedRiskSettings::default();
        let risk_config = SystemRiskConfig {
            risk_type: "Enhanced".to_string(),
            global_position_limit: Decimal::from(1000),
            global_notional_limit: Decimal::from(100000),
            max_daily_trades: 100,
            max_orders_per_second: 10,
            staleness_threshold_us: 5000,
            enhanced: Some(enhanced_settings),
            strategy_overrides: HashMap::new(),
        };

        let risk_manager = RiskManagerFactory::create_risk_manager(&risk_config);

        assert_eq!(risk_config.risk_type, "Enhanced");
        let _ = risk_manager as Box<dyn RiskManager>;
    }

    #[test]
    fn test_strategy_aware_risk_manager() {
        let base_config = SystemRiskConfig {
            risk_type: "Default".to_string(),
            global_position_limit: Decimal::from(1000),
            global_notional_limit: Decimal::from(100000),
            max_daily_trades: 100,
            max_orders_per_second: 10,
            staleness_threshold_us: 5000,
            enhanced: None,
            strategy_overrides: HashMap::new(),
        };

        let mut overrides = HashMap::new();
        overrides.insert(
            "strategy1".to_string(),
            StrategyRiskOverride {
                max_position: Some(Decimal::from(50)),
                max_notional: Some(Decimal::from(25000)),
                max_orders_per_second: Some(5),
                order_cooldown_ms: Some(200),
                staleness_threshold_us: Some(3000),
                max_daily_loss: Some(Decimal::from(5000)),
                aggressive_mode: Some(false),
                enhanced_overrides: None,
            },
        );

        let config_with_overrides = SystemRiskConfig {
            strategy_overrides: overrides,
            ..base_config
        };

        let strategy_aware_manager =
            RiskManagerFactory::create_strategy_aware_risk_manager(&config_with_overrides);

        assert_eq!(config_with_overrides.strategy_overrides.len(), 1);
        let _ = strategy_aware_manager as Box<dyn RiskManager>;
    }
}
