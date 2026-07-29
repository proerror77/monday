use std::collections::HashMap;

use hft_core::VenueId;
use ports::RiskManager;
use rust_decimal::Decimal;
use tracing::warn;

use crate::exposure_projection::ExposureProjector;
use crate::system_builder::{PortfolioSpec, StrategyConfig};

/// 聚合策略與組合設定的管理器
#[derive(Debug, Clone)]
pub struct PortfolioManager {
    definitions: Vec<PortfolioSpec>,
    strategy_to_portfolio: HashMap<String, Vec<String>>, // strategy -> portfolios
}

impl PortfolioManager {
    pub fn new(definitions: Vec<PortfolioSpec>, strategies: &[StrategyConfig]) -> Self {
        let mut strategy_to_portfolio: HashMap<String, Vec<String>> = HashMap::new();
        let known: HashMap<&str, &StrategyConfig> =
            strategies.iter().map(|s| (s.name.as_str(), s)).collect();

        for spec in &definitions {
            for strategy_name in &spec.strategies {
                if !known.contains_key(strategy_name.as_str()) {
                    warn!(
                        "Portfolio '{}' 參考了未知策略 '{}'",
                        spec.name, strategy_name
                    );
                }
                strategy_to_portfolio
                    .entry(strategy_name.clone())
                    .or_default()
                    .push(spec.name.clone());
            }
        }

        Self {
            definitions,
            strategy_to_portfolio,
        }
    }

    pub fn has_portfolios(&self) -> bool {
        !self.definitions.is_empty()
    }

    pub fn portfolio_specs(&self) -> &[PortfolioSpec] {
        &self.definitions
    }

    pub fn portfolios_for_strategy(&self, strategy_name: &str) -> Vec<String> {
        self.strategy_to_portfolio
            .get(strategy_name)
            .cloned()
            .unwrap_or_default()
    }
}

/// Applies configured cross-strategy portfolio budgets before the venue/global risk manager.
/// Until positions carry strategy attribution, existing account exposure is conservatively
/// charged to every portfolio instead of assuming that un-attributed exposure is harmless.
pub struct PortfolioBudgetRiskManager {
    base_risk_manager: Box<dyn RiskManager>,
    manager: PortfolioManager,
}

impl PortfolioBudgetRiskManager {
    pub fn new(
        base_risk_manager: Box<dyn RiskManager>,
        definitions: Vec<PortfolioSpec>,
        strategies: &[StrategyConfig],
    ) -> Self {
        Self {
            base_risk_manager,
            manager: PortfolioManager::new(definitions, strategies),
        }
    }

    fn filter_items<T>(
        &self,
        items: Vec<T>,
        account: &ports::AccountView,
        intent_of: impl Fn(&T) -> &ports::OrderIntent,
    ) -> Vec<T> {
        let specs: HashMap<&str, &PortfolioSpec> = self
            .manager
            .portfolio_specs()
            .iter()
            .map(|spec| (spec.name.as_str(), spec))
            .collect();
        let mut projectors: HashMap<String, ExposureProjector> = HashMap::new();
        let mut approved = Vec::with_capacity(items.len());

        for item in items {
            let intent = intent_of(&item);
            let portfolio_names = self.manager.portfolios_for_strategy(&intent.strategy_id);
            if portfolio_names.is_empty() {
                approved.push(item);
                continue;
            }

            let mut projections = Vec::with_capacity(portfolio_names.len());
            let mut reject_reason = None;

            for portfolio_name in &portfolio_names {
                let Some(spec) = specs.get(portfolio_name.as_str()) else {
                    reject_reason = Some("missing portfolio definition");
                    break;
                };
                let mut projector = projectors
                    .get(portfolio_name)
                    .cloned()
                    .unwrap_or_else(|| ExposureProjector::new(account));
                let projected = match projector.project(&intent) {
                    Ok(projected) => projected,
                    Err(reason) => {
                        reject_reason = Some(reason);
                        break;
                    }
                };
                if spec
                    .max_position
                    .is_some_and(|limit| projected.symbol_gross_quantity > limit)
                {
                    reject_reason = Some("portfolio position budget exceeded");
                    break;
                }
                if spec
                    .max_notional
                    .is_some_and(|limit| projected.gross_notional > limit)
                {
                    reject_reason = Some("portfolio notional budget exceeded");
                    break;
                }
                projections.push((portfolio_name.clone(), projector));
            }

            if let Some(reason) = reject_reason {
                warn!(strategy = %intent.strategy_id, symbol = %intent.symbol, %reason, "投资组合预算拒绝订单意图");
                continue;
            }
            for (portfolio_name, projector) in projections {
                projectors.insert(portfolio_name, projector);
            }
            approved.push(item);
        }

        approved
    }

    fn filter(
        &self,
        intents: Vec<ports::OrderIntent>,
        account: &ports::AccountView,
    ) -> Vec<ports::OrderIntent> {
        self.filter_items(intents, account, |intent| intent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderType, Price, Quantity, Side, Symbol, TimeInForce};

    #[test]
    fn shared_portfolio_budget_accumulates_across_strategies() {
        let base = crate::RiskManagerFactory::create_risk_manager(&crate::RiskConfig {
            risk_type: "Default".to_string(),
            global_position_limit: Decimal::from(1_000),
            global_notional_limit: Decimal::from(100_000),
            max_orders_per_second: 100,
            staleness_threshold_us: u64::MAX,
            max_daily_loss: Decimal::from(10_000),
            max_drawdown_pct: 5.0,
            ..Default::default()
        });
        let mut manager = PortfolioBudgetRiskManager::new(
            base,
            vec![PortfolioSpec {
                name: "shared".to_string(),
                strategies: vec!["alpha-a".to_string(), "alpha-b".to_string()],
                max_notional: Some(Decimal::from(100)),
                max_position: None,
                ..Default::default()
            }],
            &[],
        );
        let intent = |strategy: &str| {
            ports::OrderIntent::crypto_spot(
                Symbol::new("BTCUSDT"),
                Side::Buy,
                Quantity(Decimal::from(60)),
                OrderType::Limit,
                Some(Price(Decimal::ONE)),
                TimeInForce::GTC,
                strategy.to_string(),
                Some(VenueId::BINANCE),
            )
        };

        let envelope = |intent, emitted_at| {
            let mut lifecycle = ports::OrderIntentLifecycle::default();
            lifecycle.timing.intent_emitted_mono_us = Some(emitted_at);
            ports::OrderIntentEnvelope::new(intent, lifecycle)
        };
        let specs = HashMap::from([(VenueId::BINANCE, ports::VenueSpec::binance_spot())]);
        let approved = manager.review_envelopes_with_venue_specs(
            vec![
                envelope(intent("alpha-a"), 1),
                envelope(intent("alpha-b"), 2),
            ],
            &ports::AccountView::default(),
            &specs,
        );

        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].lifecycle.timing.intent_emitted_mono_us, Some(1));
    }
}

impl RiskManager for PortfolioBudgetRiskManager {
    fn review_orders(
        &mut self,
        intents: Vec<ports::OrderIntent>,
        account: &ports::AccountView,
        venue_specs: &HashMap<String, ports::VenueSpec>,
    ) -> Vec<ports::OrderIntent> {
        let filtered = self.filter(intents, account);
        self.base_risk_manager
            .review_orders(filtered, account, venue_specs)
    }

    fn review(
        &mut self,
        intents: Vec<ports::OrderIntent>,
        account: &ports::AccountView,
        venue: &ports::VenueSpec,
    ) -> Vec<ports::OrderIntent> {
        let filtered = self.filter(intents, account);
        self.base_risk_manager.review(filtered, account, venue)
    }

    fn review_with_venue_specs(
        &mut self,
        intents: Vec<ports::OrderIntent>,
        account: &ports::AccountView,
        venue_specs: &HashMap<VenueId, ports::VenueSpec>,
    ) -> Vec<ports::OrderIntent> {
        let filtered = self.filter(intents, account);
        self.base_risk_manager
            .review_with_venue_specs(filtered, account, venue_specs)
    }

    fn review_envelopes_with_venue_specs(
        &mut self,
        envelopes: Vec<ports::OrderIntentEnvelope>,
        account: &ports::AccountView,
        venue_specs: &HashMap<VenueId, ports::VenueSpec>,
    ) -> Vec<ports::OrderIntentEnvelope> {
        let filtered = self.filter_items(envelopes, account, |envelope| &envelope.intent);
        self.base_risk_manager
            .review_envelopes_with_venue_specs(filtered, account, venue_specs)
    }

    fn on_execution_event(&mut self, event: &ports::ExecutionEvent) {
        self.base_risk_manager.on_execution_event(event)
    }

    fn emergency_stop(&mut self) -> Result<(), hft_core::HftError> {
        self.base_risk_manager.emergency_stop()
    }

    fn get_risk_metrics(&self) -> HashMap<String, Decimal> {
        self.base_risk_manager.get_risk_metrics()
    }

    fn should_halt_trading(&self, account: &ports::AccountView) -> bool {
        self.base_risk_manager.should_halt_trading(account)
    }

    fn risk_metrics(&self) -> ports::RiskMetrics {
        self.base_risk_manager.risk_metrics()
    }

    fn update_config(&mut self, update: ports::RiskConfigUpdate) -> Result<(), hft_core::HftError> {
        self.base_risk_manager.update_config(update)
    }

    fn get_config_snapshot(&self) -> ports::RiskConfigSnapshot {
        self.base_risk_manager.get_config_snapshot()
    }
}
