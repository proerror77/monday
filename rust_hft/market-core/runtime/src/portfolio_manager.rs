use std::collections::HashMap;

use tracing::warn;

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
