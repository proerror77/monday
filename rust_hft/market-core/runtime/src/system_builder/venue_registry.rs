use std::collections::BTreeSet;

use hft_core::{BaseSymbol, InstrumentSpec, Symbol, VenueId};
use tracing::info;

use super::{SystemBuilder, VenueConfig, VenueType};

impl SystemBuilder {
    pub(crate) fn register_market_streams_from_config(mut self) -> Self {
        let instruments = self.collect_market_stream_instruments();
        info!(
            "收集到 {} 個商品需要訂閱: {:?}",
            instruments.len(),
            instruments
        );

        let venues = self.config.venues.clone();
        for venue in venues {
            self = self.register_market_streams_for_venue(&venue, &instruments);
        }

        self
    }

    fn collect_market_stream_instruments(&self) -> Vec<InstrumentSpec> {
        let mut symbol_set: BTreeSet<String> = BTreeSet::new();
        for venue in &self.config.venues {
            for instrument_id in &venue.symbol_catalog {
                if let Some((symbol, venue_id)) = instrument_id.split() {
                    symbol_set.insert(format!("{}@{}", symbol.as_str(), venue_id.as_str()));
                }
            }
        }
        for strat in &self.config.strategies {
            for symbol in &strat.symbols {
                symbol_set.insert(format!("{}@{}", symbol.as_str(), VenueId::BINANCE.as_str()));
            }
        }

        if symbol_set.is_empty() {
            symbol_set.insert(format!("BTCUSDT@{}", VenueId::BINANCE.as_str()));
        }

        symbol_set
            .into_iter()
            .filter_map(|id| {
                let mut parts = id.split('@');
                let symbol = Symbol::new(parts.next()?);
                let venue = VenueId::from_str(parts.next()?)?;
                Some(instrument_for_venue(symbol, venue))
            })
            .collect()
    }

    fn register_market_streams_for_venue(
        self,
        venue: &VenueConfig,
        instruments: &[InstrumentSpec],
    ) -> Self {
        if venue.venue_type == VenueType::BinancePrediction {
            info!("Binance Prediction is execution-only; skipping streaming market adapter");
            return self;
        }
        let venue_id = to_venue_id(&venue.venue_type);

        let base_instruments: Vec<InstrumentSpec> = if !venue.symbol_catalog.is_empty() {
            venue
                .symbol_catalog
                .iter()
                .filter_map(|instrument_id| {
                    instrument_id
                        .split()
                        .map(|(symbol, venue)| instrument_for_venue(symbol, venue))
                })
                .collect()
        } else {
            instruments
                .iter()
                .map(|instrument| instrument_for_venue(instrument.symbol.clone(), venue_id))
                .collect()
        };

        let filtered_instruments: Vec<InstrumentSpec> =
            if let Some(ref shard_config) = self.shard_config {
                let filtered: Vec<InstrumentSpec> = base_instruments
                    .into_iter()
                    .filter(|instrument| {
                        let base_symbol = BaseSymbol::from(instrument.symbol.as_str());
                        shard_config.should_handle(&base_symbol, &instrument.venue)
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
                    base_instruments.len()
                );
                base_instruments
            };

        if filtered_instruments.is_empty() {
            if self.shard_config.is_some() {
                info!("分片過濾後，交易所 {} 無符號需要處理，跳過註冊", venue.name);
            }
            return self;
        }

        self.register_market_instrument_plan(
            venue.venue_type.clone(),
            venue.name.clone(),
            filtered_instruments,
        )
    }
}

fn instrument_for_venue(symbol: Symbol, venue: VenueId) -> InstrumentSpec {
    match venue {
        VenueId::BINANCE_TOKENIZED_SECURITIES => {
            InstrumentSpec::tokenized_security_spot(symbol, venue)
        }
        VenueId::ONDO_PERPS => InstrumentSpec::ondo_perp(symbol),
        VenueId::POLYMARKET => InstrumentSpec::polymarket_outcome(symbol),
        _ => InstrumentSpec::crypto_spot(symbol, venue),
    }
}

fn to_venue_id(venue_type: &VenueType) -> VenueId {
    match venue_type {
        VenueType::Binance => VenueId::BINANCE,
        VenueType::BinancePrediction => VenueId::BINANCE_PREDICTION,
        VenueType::Bitget => VenueId::BITGET,
        VenueType::Bybit => VenueId::BYBIT,
        VenueType::Hyperliquid => VenueId::HYPERLIQUID,
        VenueType::Grvt => VenueId::GRVT,
        VenueType::Backpack => VenueId::BACKPACK,
        VenueType::Asterdex => VenueId::ASTERDEX,
        VenueType::Lighter => VenueId::LIGHTER,
        VenueType::OndoPerps => VenueId::ONDO_PERPS,
        VenueType::Polymarket => VenueId::POLYMARKET,
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
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
        });

        let builder = SystemBuilder::new(config).register_market_streams_from_config();

        assert_eq!(builder.market_stream_plans.len(), 1);
        let (venue, _name, instruments) = &builder.market_stream_plans[0];
        assert_eq!(*venue, VenueType::Binance);
        let collected: Vec<_> = instruments
            .iter()
            .map(|instrument| instrument.symbol.as_str())
            .collect();
        assert_eq!(collected, vec!["BTCUSDT", "ETHUSDT"]);
    }

    #[test]
    fn bstock_catalog_drives_tokenized_security_market_plan() {
        let mut config = SystemConfig::default();
        config.venues.push(VenueConfig {
            name: "binance-bstocks".into(),
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
                InstrumentId::new("TSLABUSDT@BINANCE_TOKENIZED_SECURITIES"),
                InstrumentId::new("NVDABUSDT@BINANCE_TOKENIZED_SECURITIES"),
            ],
            data_config: None,
            execution_config: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
        });

        let builder = SystemBuilder::new(config).register_market_streams_from_config();

        let (_, _name, instruments) = &builder.market_stream_plans[0];
        assert_eq!(instruments.len(), 2);
        assert!(instruments.iter().all(|instrument| {
            instrument.asset_class == hft_core::AssetClass::TokenizedSecurity
                && instrument.product_type == hft_core::ProductType::TokenizedSecuritySpot
                && instrument.venue == VenueId::BINANCE_TOKENIZED_SECURITIES
        }));
    }

    #[test]
    fn ondo_catalog_drives_restricted_perp_market_plan() {
        let mut config = SystemConfig::default();
        config.venues.push(VenueConfig {
            name: "ondo-perps".into(),
            account_id: None,
            venue_type: VenueType::OndoPerps,
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
            symbol_catalog: vec![InstrumentId::new("TSLA@ONDO_PERPS")],
            data_config: None,
            execution_config: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
        });

        let builder = SystemBuilder::new(config).register_market_streams_from_config();
        let (venue, _, instruments) = &builder.market_stream_plans[0];

        assert_eq!(*venue, VenueType::OndoPerps);
        assert_eq!(
            instruments,
            &[InstrumentSpec::ondo_perp(Symbol::new("TSLA"))]
        );
    }

    #[test]
    fn polymarket_catalog_preserves_outcome_token_identity() {
        let mut config = SystemConfig::default();
        config.venues.push(VenueConfig {
            name: "polymarket".into(),
            account_id: None,
            venue_type: VenueType::Polymarket,
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
            symbol_catalog: vec![InstrumentId::new("123456789@POLYMARKET")],
            data_config: None,
            execution_config: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
        });

        let builder = SystemBuilder::new(config).register_market_streams_from_config();
        let (venue, _, instruments) = &builder.market_stream_plans[0];

        assert_eq!(*venue, VenueType::Polymarket);
        assert_eq!(
            instruments,
            &[InstrumentSpec::polymarket_outcome(Symbol::new("123456789"))]
        );
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
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
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
        let (_, _name, instruments) = &builder.market_stream_plans[0];
        assert_eq!(instruments.len(), 1);
        assert_eq!(instruments[0].symbol.as_str(), "BTCUSDT");
        assert_eq!(instruments[0].venue, VenueId::MOCK);
    }
}
