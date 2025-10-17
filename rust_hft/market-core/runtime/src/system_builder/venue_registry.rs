use std::collections::BTreeSet;

use hft_core::{BaseSymbol, Symbol, VenueId};
use tracing::info;

use super::{SystemBuilder, VenueConfig, VenueType};

impl SystemBuilder {
    pub(crate) fn register_market_streams_from_config(mut self) -> Self {
        let symbols = self.collect_market_stream_symbols();
        info!("收集到 {} 個符號需要訂閱: {:?}", symbols.len(), symbols);

        let venues = self.config.venues.clone();
        for venue in venues {
            self = self.register_market_streams_for_venue(&venue, &symbols);
        }

        self
    }

    fn collect_market_stream_symbols(&self) -> Vec<Symbol> {
        let mut symbol_set: BTreeSet<String> = BTreeSet::new();
        for venue in &self.config.venues {
            for instrument_id in &venue.symbol_catalog {
                if let Some(symbol) = instrument_id.symbol() {
                    symbol_set.insert(symbol.as_str().to_string());
                }
            }
        }
        for strat in &self.config.strategies {
            for symbol in &strat.symbols {
                symbol_set.insert(symbol.as_str().to_string());
            }
        }

        if symbol_set.is_empty() {
            symbol_set.insert("BTCUSDT".to_string());
        }

        symbol_set.into_iter().map(Symbol::from).collect()
    }

    fn register_market_streams_for_venue(self, venue: &VenueConfig, symbols: &[Symbol]) -> Self {
        let venue_id = to_venue_id(&venue.venue_type);

        let base_symbols: Vec<Symbol> = if !venue.symbol_catalog.is_empty() {
            venue
                .symbol_catalog
                .iter()
                .filter_map(|instrument_id| instrument_id.symbol())
                .collect()
        } else {
            symbols.to_vec()
        };

        let filtered_symbols: Vec<Symbol> = if let Some(ref shard_config) = self.shard_config {
            let filtered: Vec<Symbol> = base_symbols
                .into_iter()
                .filter(|symbol| {
                    let base_symbol = BaseSymbol::from(symbol.as_str());
                    shard_config.should_handle(&base_symbol, &venue_id)
                })
                .collect();

            info!(
                "分片過濾後，交易所 {} 需要處理 {} 個符號: {:?}",
                venue.name,
                filtered.len(),
                filtered
            );
            filtered
        } else {
            info!(
                "未配置分片，交易所 {} 處理所有 {} 個符號",
                venue.name,
                base_symbols.len()
            );
            base_symbols
        };

        if filtered_symbols.is_empty() {
            if self.shard_config.is_some() {
                info!("分片過濾後，交易所 {} 無符號需要處理，跳過註冊", venue.name);
            }
            return self;
        }

        self.register_market_stream_plan(
            venue.venue_type.clone(),
            venue.name.clone(),
            filtered_symbols,
        )
    }
}

fn to_venue_id(venue_type: &VenueType) -> VenueId {
    match venue_type {
        VenueType::Binance => VenueId::BINANCE,
        VenueType::Bitget => VenueId::BITGET,
        VenueType::Bybit => VenueId::BYBIT,
        VenueType::Hyperliquid => VenueId::HYPERLIQUID,
        VenueType::Grvt => VenueId::GRVT,
        VenueType::Backpack => VenueId::BACKPACK,
        VenueType::Asterdex => VenueId::ASTERDEX,
        VenueType::Lighter => VenueId::LIGHTER,
        VenueType::Mock => VenueId::MOCK,
        VenueType::Okx => VenueId::OKX,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        StrategyConfig, StrategyParams, StrategyRiskLimits, StrategyType, SystemConfig,
        VenueCapabilities,
    };
    use super::*;
    use rust_decimal::Decimal;
    use shared_instrument::InstrumentId;

    fn empty_risk_limits() -> StrategyRiskLimits {
        StrategyRiskLimits {
            max_notional: Decimal::ZERO,
            max_position: Decimal::ZERO,
            daily_loss_limit: Decimal::ZERO,
            cooldown_ms: 0,
        }
    }

    #[test]
    fn symbol_catalog_drives_market_plan() {
        let mut config = SystemConfig::default();
        config.venues.push(VenueConfig {
            name: "binance".into(),
            account_id: None,
            venue_type: VenueType::Binance,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            execution_mode: None,
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: false,
            symbol_catalog: vec![
                InstrumentId::new("BTCUSDT@BINANCE"),
                InstrumentId::new("ETHUSDT@BINANCE"),
            ],
            data_config: None,
            execution_config: None,
        });

        let builder = SystemBuilder::new(config).register_market_streams_from_config();

        assert_eq!(builder.market_stream_plans.len(), 1);
        let (venue, _name, symbols) = &builder.market_stream_plans[0];
        assert_eq!(*venue, VenueType::Binance);
        let collected: Vec<_> = symbols.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(collected, vec!["BTCUSDT", "ETHUSDT"]);
    }

    #[test]
    fn strategy_symbols_used_when_catalog_empty() {
        let mut config = SystemConfig::default();
        config.venues.push(VenueConfig {
            name: "mock".into(),
            account_id: None,
            venue_type: VenueType::Mock,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            execution_mode: None,
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: false,
            symbol_catalog: Vec::new(),
            data_config: None,
            execution_config: None,
        });
        config.strategies.push(StrategyConfig {
            name: "trend".into(),
            strategy_type: StrategyType::Trend,
            symbols: vec![Symbol::new("BTCUSDT")],
            params: StrategyParams::Trend {
                ema_fast: 12,
                ema_slow: 26,
                rsi_period: 14,
            },
            risk_limits: empty_risk_limits(),
        });

        let builder = SystemBuilder::new(config).register_market_streams_from_config();

        assert_eq!(builder.market_stream_plans.len(), 1);
        let (_, _name, symbols) = &builder.market_stream_plans[0];
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].0, "BTCUSDT");
    }
}
