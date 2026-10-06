use std::collections::BTreeSet;

use hft_core::{BaseSymbol, InstrumentSpec, Symbol, VenueId};
use tracing::info;

use super::{StrategyParams, StrategyType, SystemBuilder, VenueConfig, VenueType};

impl SystemBuilder {
    pub(crate) fn register_market_streams_from_config(mut self) -> Self {
        let requested = match self.requested_market_venues() {
            Ok(requested) => requested,
            Err(error) => {
                self.market_data_planning_error = Some(error);
                return self;
            }
        };
        let instruments = self.collect_market_stream_instruments();
        info!(
            "收集到 {} 個商品需要訂閱: {:?}",
            instruments.len(),
            instruments
        );

        let venues = self.config.venues.clone();
        for (index, venue) in venues.into_iter().enumerate() {
            if requested.contains(&index) && !self.has_market_plan_for(&venue) {
                self = self.register_market_streams_for_venue(&venue, &instruments);
            }
        }

        self
    }

    fn requested_market_venues(&self) -> Result<BTreeSet<usize>, String> {
        // Endpoints and catalogs describe connections and instruments. The v2
        // loader can fill both without requesting a market-data capability.
        if !self.config.strategies.is_empty() || !self.strategies.is_empty() {
            match &self.config.router {
                Some(ports::RouterConfig::SameVenue { default_venue }) => {
                    parse_router_venue(default_venue)?;
                }
                Some(ports::RouterConfig::StrategyMap {
                    strategy_venues,
                    default_venue,
                }) => {
                    parse_router_venue(default_venue)?;
                    for target in strategy_venues.values() {
                        parse_router_venue(target)?;
                    }
                }
                Some(ports::RouterConfig::RoundRobin { venues }) => {
                    if venues.is_empty() {
                        return Err("market-data planning rejects an empty RoundRobin route".into());
                    }
                    for target in venues {
                        parse_router_venue(target)?;
                    }
                }
                None => {}
            }
        }
        let quotes_only = super::quotes_only_enabled(&self.config);
        let mut requested = self
            .config
            .venues
            .iter()
            .enumerate()
            .filter(|(_, venue)| quotes_only || venue.data_config.is_some())
            .map(|(index, _)| index)
            .collect::<BTreeSet<_>>();
        let mut configured_ids = BTreeSet::new();
        for strategy in &self.config.strategies {
            let typed_venue = match &strategy.params {
                StrategyParams::Formula {
                    execution_contract: Some(contract),
                    ..
                }
                | StrategyParams::FrozenModel {
                    execution_contract: contract,
                    ..
                } => Some(contract.venue),
                StrategyParams::ProbabilityReversal { .. } => Some(VenueId::POLYMARKET),
                StrategyParams::LobFlowGrid { config } => config
                    .venue
                    .as_deref()
                    .map(parse_router_venue)
                    .transpose()?,
                _ => None,
            };
            let instances = match strategy.strategy_type {
                StrategyType::Trend
                | StrategyType::Imbalance
                | StrategyType::LobFlowGrid
                | StrategyType::Formula
                | StrategyType::FrozenModel => strategy
                    .symbols
                    .iter()
                    .map(|symbol| {
                        (
                            format!("{}:{}", strategy.name, symbol.as_str()),
                            vec![symbol.clone()],
                        )
                    })
                    .collect::<Vec<_>>(),
                _ => vec![(strategy.name.clone(), strategy.symbols.clone())],
            };
            for (id, symbols) in instances {
                configured_ids.insert(id.clone());
                if let Some(index) = self.strategy_market_venue(&id, typed_venue, &symbols)? {
                    requested.insert(index);
                }
            }
        }
        for strategy in &self.strategies {
            if configured_ids.contains(strategy.id()) {
                continue;
            }
            if strategy.venue_scope() == ports::VenueScope::Cross {
                // The trait declares cross-venue scope, not its exact sources.
                // Only caller-supplied concrete plans can provide those sources.
                if !self.has_nonempty_market_plan() {
                    return Err(format!(
                        "manual cross-venue strategy '{}' requires explicit market plans",
                        strategy.id()
                    ));
                }
                if let Some(index) = self.strategy_market_venue(strategy.id(), None, &[])? {
                    if !self.market_plan_covers(&self.config.venues[index], &[]) {
                        return Err(format!(
                            "manual cross-venue strategy '{}' requires a market plan covering its bound venue",
                            strategy.id()
                        ));
                    }
                }
                continue;
            }
            if let Some(index) = self.strategy_market_venue(strategy.id(), None, &[])? {
                requested.insert(index);
            }
        }
        Ok(requested)
    }

    fn strategy_market_venue(
        &self,
        id: &str,
        typed_venue: Option<VenueId>,
        symbols: &[Symbol],
    ) -> Result<Option<usize>, String> {
        // Router filters use the exact instance ID. Account lookup follows the
        // engine: exact ID first, then the actual ID's first colon prefix.
        // Neither lookup uses a manual strategy's display name.
        let filtered_venue = match &self.config.router {
            Some(ports::RouterConfig::StrategyMap {
                strategy_venues, ..
            }) => strategy_venues
                .get(id)
                .map(|target| parse_router_venue(target))
                .transpose()?,
            _ => None,
        };
        if let (Some(typed), Some(filtered)) = (typed_venue, filtered_venue) {
            if normalize_market_venue(typed) != normalize_market_venue(filtered) {
                return Err(format!(
                    "strategy '{id}' execution contract and actual-ID router name different market venues"
                ));
            }
        }
        let account = self.config.strategy_accounts.get(id).or_else(|| {
            id.split_once(':')
                .and_then(|(prefix, _)| self.config.strategy_accounts.get(prefix))
        });
        if let Some(account) = account {
            let matching = self
                .config
                .venues
                .iter()
                .enumerate()
                .filter(|(_, venue)| venue.account_id.as_deref().unwrap_or(&venue.name) == account)
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let [index] = matching.as_slice() else {
                return Err(format!(
                    "strategy '{id}' account '{account}' must identify exactly one market venue"
                ));
            };
            if let Some(typed) = typed_venue {
                let actual = super::venue_config_to_market_venue_id(&self.config.venues[*index])?;
                if normalize_market_venue(actual) != normalize_market_venue(typed) {
                    return Err(format!(
                        "strategy '{id}' account and execution contract name different market venues"
                    ));
                }
            }
            if let Some(filtered) = filtered_venue {
                let actual = super::venue_config_to_market_venue_id(&self.config.venues[*index])?;
                if normalize_market_venue(actual) != normalize_market_venue(filtered) {
                    return Err(format!(
                        "strategy '{id}' account and actual-ID router name different market venues"
                    ));
                }
            }
            self.require_existing_plan_coverage(id, *index, symbols)?;
            return Ok(Some(*index));
        }
        let targets = if let Some(venue) = typed_venue {
            vec![venue]
        } else {
            self.router_market_venues(id)?
        };
        if !targets.is_empty() {
            let mut matching = BTreeSet::new();
            let mut covered = true;
            for target in targets {
                let mut found = false;
                let mut target_covered = false;
                for (index, venue) in self.config.venues.iter().enumerate() {
                    if normalize_market_venue(super::venue_config_to_market_venue_id(venue)?)
                        == normalize_market_venue(target)
                    {
                        matching.insert(index);
                        found = true;
                        target_covered |= self.market_plan_covers(venue, symbols);
                    }
                }
                if !found {
                    return Err(format!(
                        "strategy '{id}' market venue '{target}' is absent from the runtime configuration"
                    ));
                }
                covered &= target_covered;
            }
            if matching.len() == 1 {
                let index = *matching.first().expect("one matching venue");
                self.require_existing_plan_coverage(id, index, symbols)?;
                return Ok(Some(index));
            }
            if covered {
                return Ok(None);
            }
            return Err(format!(
                "strategy '{id}' has ambiguous market venues without matching market and instrument coverage in its explicit plans"
            ));
        } else if self.config.venues.len() == 1 {
            self.require_existing_plan_coverage(id, 0, symbols)?;
            return Ok(Some(0));
        }
        if self.has_nonempty_market_plan() {
            return Ok(None);
        }
        Err(format!(
            "strategy '{id}' has no unique market venue; bind its account or route, or register explicit market plans"
        ))
    }

    fn router_market_venues(&self, id: &str) -> Result<Vec<VenueId>, String> {
        match &self.config.router {
            Some(ports::RouterConfig::SameVenue { default_venue }) => {
                Ok(vec![parse_router_venue(default_venue)?])
            }
            Some(ports::RouterConfig::StrategyMap {
                strategy_venues,
                default_venue,
            }) => {
                let target = strategy_venues.get(id).unwrap_or(default_venue);
                Ok(vec![parse_router_venue(target)?])
            }
            Some(ports::RouterConfig::RoundRobin { venues }) => {
                if venues.is_empty() {
                    return Err("market-data planning rejects an empty RoundRobin route".into());
                }
                venues
                    .iter()
                    .map(|venue| parse_router_venue(venue))
                    .collect()
            }
            None => Ok(Vec::new()),
        }
    }

    fn has_nonempty_market_plan(&self) -> bool {
        self.market_stream_plans
            .iter()
            .any(|(_, _, instruments)| !instruments.is_empty())
    }

    fn require_existing_plan_coverage(
        &self,
        id: &str,
        index: usize,
        symbols: &[Symbol],
    ) -> Result<(), String> {
        let venue = &self.config.venues[index];
        if self.has_market_plan_for(venue) && !self.market_plan_covers(venue, symbols) {
            return Err(format!(
                "strategy '{id}' explicit market plan does not cover venue '{}' and its required market/instruments",
                venue.name
            ));
        }
        Ok(())
    }

    fn market_plan_covers(&self, venue: &VenueConfig, symbols: &[Symbol]) -> bool {
        let Ok(expected) = super::venue_config_to_market_venue_id(venue) else {
            return false;
        };
        let expected = normalize_market_venue(expected);
        self.market_stream_plans
            .iter()
            .any(|(kind, name, instruments)| {
                kind == &venue.venue_type
                    && name == &venue.name
                    && !instruments.is_empty()
                    && instruments.iter().all(|instrument| {
                        normalize_market_venue(instrument.venue) == expected
                            && match expected {
                                VenueId::BINANCE => {
                                    instrument.product_type == hft_core::ProductType::Spot
                                }
                                VenueId::BINANCE_FUTURES => {
                                    instrument.product_type == hft_core::ProductType::Perp
                                }
                                _ => true,
                            }
                    })
                    && symbols.iter().all(|symbol| {
                        instruments
                            .iter()
                            .any(|instrument| &instrument.symbol == symbol)
                    })
            })
    }

    fn has_market_plan_for(&self, venue: &VenueConfig) -> bool {
        self.market_stream_plans
            .iter()
            .any(|(kind, name, instruments)| {
                if instruments.is_empty() || kind != &venue.venue_type {
                    return false;
                }
                name == &venue.name
                    || super::venue_config_to_market_venue_id(venue).is_ok_and(|id| {
                        instruments.iter().any(|instrument| {
                            normalize_market_venue(instrument.venue) == normalize_market_venue(id)
                        })
                    })
            })
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
        if venue.venue_type == VenueType::BinancePrediction
            && (venue.symbol_catalog.is_empty() || venue.data_config.is_none())
        {
            info!(
                "Binance Prediction needs an explicit outcome catalog and data_config; keeping the venue execution-only"
            );
            return self;
        }
        let venue_id = match super::venue_config_to_market_venue_id(venue) {
            Ok(venue_id) => venue_id,
            Err(error) => {
                tracing::warn!(
                    venue = %venue.name,
                    %error,
                    "Binance market identity is invalid; market stream plan omitted"
                );
                return self;
            }
        };
        let explicit_usdm = venue_id == VenueId::BINANCE_FUTURES;

        let base_instruments: Vec<InstrumentSpec> = if !venue.symbol_catalog.is_empty() {
            venue
                .symbol_catalog
                .iter()
                .filter_map(|instrument_id| {
                    let (symbol, catalog_venue_id) = instrument_id.split()?;
                    let instrument_venue =
                        if matches!(catalog_venue_id, VenueId::BINANCE | VenueId::BINANCE_SPOT)
                            && explicit_usdm
                        {
                            VenueId::BINANCE_FUTURES
                        } else if catalog_venue_id == VenueId::BINANCE_FUTURES && !explicit_usdm {
                            return None;
                        } else if catalog_venue_id == VenueId::BINANCE_SPOT {
                            VenueId::BINANCE
                        } else {
                            catalog_venue_id
                        };
                    Some(instrument_for_venue(symbol, instrument_venue))
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

fn parse_router_venue(value: &str) -> Result<VenueId, String> {
    VenueId::from_str(value)
        .ok_or_else(|| format!("market-data planning rejects unknown router venue '{value}'"))
}

fn normalize_market_venue(venue: VenueId) -> VenueId {
    if venue == VenueId::BINANCE_SPOT {
        VenueId::BINANCE
    } else {
        venue
    }
}

fn instrument_for_venue(symbol: Symbol, venue: VenueId) -> InstrumentSpec {
    match venue {
        VenueId::BINANCE_FUTURES => {
            let mut instrument = InstrumentSpec::crypto_spot(symbol, venue);
            instrument.product_type = hft_core::ProductType::Perp;
            instrument
        }
        VenueId::BINANCE_TOKENIZED_SECURITIES => {
            InstrumentSpec::tokenized_security_spot(symbol, venue)
        }
        VenueId::ONDO_PERPS => InstrumentSpec::ondo_perp(symbol),
        VenueId::POLYMARKET => InstrumentSpec::polymarket_outcome(symbol),
        VenueId::BINANCE_PREDICTION => InstrumentSpec::prediction_market_outcome(symbol),
        _ => InstrumentSpec::crypto_spot(symbol, venue),
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
        let mut config = SystemConfig {
            quotes_only: true,
            ..Default::default()
        };
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
            inst_type: Some("usdm".to_string()),
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
        assert!(instruments
            .iter()
            .all(|instrument| instrument.venue == VenueId::BINANCE_FUTURES));
        let collected: Vec<_> = instruments
            .iter()
            .map(|instrument| instrument.symbol.as_str())
            .collect();
        assert_eq!(collected, vec!["BTCUSDT", "ETHUSDT"]);
        assert!(instruments
            .iter()
            .all(|instrument| instrument.product_type == hft_core::ProductType::Perp));
    }

    #[test]
    fn explicit_usdm_execution_market_uses_binance_futures_instrument_identity() {
        let mut config = SystemConfig {
            quotes_only: true,
            ..Default::default()
        };
        config.venues.push(VenueConfig {
            name: "binance-usdm".into(),
            account_id: None,
            venue_type: VenueType::Binance,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            execution_mode: Some("Paper".into()),
            capabilities: VenueCapabilities::default(),
            inst_type: Some("usdm".into()),
            simulate_execution: false,
            symbol_catalog: vec![InstrumentId::new("BTCUSDT@BINANCE")],
            data_config: None,
            execution_config: Some(serde_yaml::from_str("market: usdm").unwrap()),
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
        });

        let builder = SystemBuilder::new(config).register_market_streams_from_config();
        let instruments = &builder.market_stream_plans[0].2;

        assert_eq!(instruments[0].venue, VenueId::BINANCE_FUTURES);
        assert_eq!(instruments[0].product_type, hft_core::ProductType::Perp);
    }

    #[test]
    fn execution_market_alone_selects_usdm_market_data_identity() {
        let mut config = SystemConfig {
            quotes_only: true,
            ..Default::default()
        };
        config.venues.push(VenueConfig {
            name: "binance-usdm".into(),
            account_id: None,
            venue_type: VenueType::Binance,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            execution_mode: Some("Paper".into()),
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: false,
            symbol_catalog: vec![InstrumentId::new("BTCUSDT@BINANCE")],
            data_config: None,
            execution_config: Some(serde_yaml::from_str("market: usdm").unwrap()),
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
        });

        let builder = SystemBuilder::new(config).register_market_streams_from_config();
        let instruments = &builder.market_stream_plans[0].2;

        assert_eq!(instruments[0].venue, VenueId::BINANCE_FUTURES);
        assert_eq!(instruments[0].product_type, hft_core::ProductType::Perp);
    }

    #[test]
    fn bstock_catalog_drives_tokenized_security_market_plan() {
        let mut config = SystemConfig {
            quotes_only: true,
            ..Default::default()
        };
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
        let mut config = SystemConfig {
            quotes_only: true,
            ..Default::default()
        };
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
    fn binance_prediction_catalog_drives_prediction_market_plan() {
        let mut config = SystemConfig::default();
        config.venues.push(VenueConfig {
            name: "binance-prediction".into(),
            account_id: None,
            venue_type: VenueType::BinancePrediction,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            execution_mode: Some("Paper".into()),
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: false,
            symbol_catalog: vec![InstrumentId::new("112233@BINANCE_PREDICTION")],
            data_config: Some(
                serde_yaml::from_str(
                    "outcomes:\n  - token_id: '112233'\n    market_id: 1\n    vendor: predict_fun\n",
                )
                .expect("valid Binance Prediction outcome config"),
            ),
            execution_config: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
        });

        let builder = SystemBuilder::new(config).register_market_streams_from_config();
        let (venue, _, instruments) = &builder.market_stream_plans[0];
        assert_eq!(*venue, VenueType::BinancePrediction);
        assert_eq!(
            instruments,
            &[InstrumentSpec::prediction_market_outcome(Symbol::new(
                "112233"
            ))]
        );
    }

    #[test]
    fn binance_prediction_without_an_outcome_catalog_stays_execution_only() {
        let mut config = SystemConfig::default();
        config.venues.push(VenueConfig {
            name: "binance-prediction".into(),
            account_id: None,
            venue_type: VenueType::BinancePrediction,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            execution_mode: Some("Live".into()),
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

        let builder = SystemBuilder::new(config).register_market_streams_from_config();
        assert!(builder.market_stream_plans.is_empty());
    }

    #[test]
    fn binance_prediction_without_quote_data_stays_execution_only() {
        let mut config = SystemConfig::default();
        config.venues.push(VenueConfig {
            name: "binance-prediction".into(),
            account_id: None,
            venue_type: VenueType::BinancePrediction,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            execution_mode: Some("Live".into()),
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: false,
            symbol_catalog: vec![InstrumentId::new("112233@BINANCE_PREDICTION")],
            data_config: None,
            execution_config: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
        });

        let builder = SystemBuilder::new(config).register_market_streams_from_config();
        assert!(builder.market_stream_plans.is_empty());
    }

    #[test]
    fn polymarket_catalog_preserves_outcome_token_identity() {
        let mut config = SystemConfig {
            quotes_only: true,
            ..Default::default()
        };
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

    fn demand_venue(name: &str, kind: VenueType, account: &str) -> VenueConfig {
        VenueConfig {
            name: name.into(),
            account_id: Some(account.into()),
            venue_type: kind,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            execution_mode: Some("Paper".into()),
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: true,
            symbol_catalog: Vec::new(),
            data_config: None,
            execution_config: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
        }
    }

    fn demand_config() -> SystemConfig {
        SystemConfig {
            venues: vec![
                demand_venue("binance", VenueType::Binance, "binance-account"),
                demand_venue("okx-control", VenueType::Okx, "okx-account"),
            ],
            ..Default::default()
        }
    }

    struct ManualDemandStrategy(ports::VenueScope);

    impl ports::Strategy for ManualDemandStrategy {
        fn on_market_event(
            &mut self,
            _: &ports::MarketEvent,
            _: &ports::AccountView,
        ) -> Vec<ports::OrderIntent> {
            Vec::new()
        }

        fn on_execution_event(
            &mut self,
            _: &ports::ExecutionEvent,
            _: &ports::AccountView,
        ) -> Vec<ports::OrderIntent> {
            Vec::new()
        }

        fn name(&self) -> &str {
            "display-only-name"
        }

        fn id(&self) -> &str {
            "manual-instance"
        }

        fn venue_scope(&self) -> ports::VenueScope {
            self.0
        }
    }

    #[test]
    fn explicit_data_intent_does_not_come_from_endpoints() {
        let mut config = demand_config();
        config.venues[0].ws_public = Some("wss://catalog.example".into());
        config.venues[1].data_config = Some(serde_yaml::Value::Mapping(Default::default()));
        let requested = SystemBuilder::new(config)
            .requested_market_venues()
            .unwrap();
        assert_eq!(requested, BTreeSet::from([1]));
    }

    fn typed_formula_demand_config() -> SystemConfig {
        let mut config = demand_config();
        config.strategies.push(StrategyConfig {
            name: "factor".into(),
            strategy_type: StrategyType::Formula,
            symbols: vec![Symbol::new("BTCUSDT")],
            params: StrategyParams::Formula {
                ast: serde_json::from_value(serde_json::json!({
                    "Terminal":{"Field":"book_imbalance"}
                }))
                .unwrap(),
                max_order_notional: Decimal::ONE,
                signal_threshold: 0.0,
                target_position: false,
                evaluation_interval_millis: None,
                execution_contract: Some(super::super::FormulaExecutionContract {
                    venue: VenueId::BINANCE,
                    venue_spec: ports::VenueSpec::default(),
                    cross_spread: false,
                }),
            },
            risk_limits: empty_risk_limits(),
        });
        config
    }

    #[test]
    fn formula_instance_account_and_typed_contract_agree_on_one_venue() {
        let mut config = typed_formula_demand_config();
        config
            .strategy_accounts
            .insert("factor:BTCUSDT".into(), "binance-account".into());
        assert_eq!(
            SystemBuilder::new(config.clone())
                .requested_market_venues()
                .unwrap(),
            BTreeSet::from([0])
        );
        config
            .strategy_accounts
            .insert("factor:BTCUSDT".into(), "okx-account".into());
        assert!(SystemBuilder::new(config)
            .requested_market_venues()
            .unwrap_err()
            .contains("execution contract"));
    }

    #[test]
    fn account_id_prefix_binding_and_exact_override_follow_engine() {
        let mut config = typed_formula_demand_config();
        config
            .strategy_accounts
            .insert("factor".into(), "binance-account".into());
        assert_eq!(
            SystemBuilder::new(config.clone())
                .requested_market_venues()
                .unwrap(),
            BTreeSet::from([0])
        );
        config
            .strategy_accounts
            .insert("factor".into(), "okx-account".into());
        assert!(SystemBuilder::new(config.clone())
            .requested_market_venues()
            .is_err());
        config
            .strategy_accounts
            .insert("factor:BTCUSDT".into(), "binance-account".into());
        assert_eq!(
            SystemBuilder::new(config)
                .requested_market_venues()
                .unwrap(),
            BTreeSet::from([0])
        );
    }

    #[test]
    fn actual_id_router_must_agree_with_account_and_typed_contract() {
        let mut config = typed_formula_demand_config();
        config
            .strategy_accounts
            .insert("factor:BTCUSDT".into(), "binance-account".into());
        config.router = Some(ports::RouterConfig::StrategyMap {
            strategy_venues: std::collections::HashMap::from([(
                "factor:BTCUSDT".into(),
                "OKX".into(),
            )]),
            default_venue: "BINANCE".into(),
        });
        assert!(SystemBuilder::new(config)
            .requested_market_venues()
            .unwrap_err()
            .contains("actual-ID router"));
    }

    #[test]
    fn router_group_entry_does_not_replace_actual_id_default() {
        let mut config = typed_formula_demand_config();
        let StrategyParams::Formula {
            execution_contract, ..
        } = &mut config.strategies[0].params
        else {
            unreachable!();
        };
        *execution_contract = None;
        config.router = Some(ports::RouterConfig::StrategyMap {
            strategy_venues: std::collections::HashMap::from([("factor".into(), "BINANCE".into())]),
            default_venue: "OKX".into(),
        });
        assert_eq!(
            SystemBuilder::new(config)
                .requested_market_venues()
                .unwrap(),
            BTreeSet::from([1])
        );
    }

    #[test]
    fn ambiguous_typed_venue_requires_matching_market_and_symbol_coverage() {
        let mut config = typed_formula_demand_config();
        config.venues[1] = demand_venue("binance-second", VenueType::Binance, "second-account");
        for (kind, name, instrument, accepted) in [
            (
                VenueType::Bitget,
                "unrelated-bitget",
                InstrumentSpec::crypto_spot(Symbol::new("BTCUSDT"), VenueId::BITGET),
                false,
            ),
            (
                VenueType::Binance,
                "binance",
                InstrumentSpec::crypto_spot(Symbol::new("ETHUSDT"), VenueId::BINANCE),
                false,
            ),
            (
                VenueType::Binance,
                "binance",
                instrument_for_venue(Symbol::new("BTCUSDT"), VenueId::BINANCE_FUTURES),
                false,
            ),
            (
                VenueType::Binance,
                "binance",
                InstrumentSpec::crypto_spot(Symbol::new("BTCUSDT"), VenueId::BINANCE),
                true,
            ),
        ] {
            let builder = SystemBuilder::new(config.clone())
                .register_market_instrument_plan(kind, name.into(), vec![instrument])
                .register_market_streams_from_config();
            assert_eq!(builder.market_data_planning_error.is_none(), accepted);
            assert_eq!(builder.market_stream_plans.len(), 1);
        }
    }

    #[test]
    fn manual_router_mapping_uses_id_and_rejects_malformed_targets() {
        let mut config = demand_config();
        config.router = Some(ports::RouterConfig::StrategyMap {
            strategy_venues: std::collections::HashMap::from([(
                "manual-instance".into(),
                "BINANCE".into(),
            )]),
            default_venue: "OKX".into(),
        });
        let builder = SystemBuilder::new(config.clone())
            .register_strategy(ManualDemandStrategy(ports::VenueScope::Single))
            .register_market_streams_from_config();
        assert!(builder.market_data_planning_error.is_none());
        assert_eq!(builder.market_stream_plans.len(), 1);
        assert_eq!(builder.market_stream_plans[0].0, VenueType::Binance);

        config.router = Some(ports::RouterConfig::SameVenue {
            default_venue: "not-a-venue".into(),
        });
        let builder = SystemBuilder::new(config)
            .register_strategy(ManualDemandStrategy(ports::VenueScope::Single))
            .register_market_stream_plan(
                VenueType::Binance,
                "binance".into(),
                vec![Symbol::new("BTCUSDT")],
            )
            .register_market_streams_from_config();
        assert!(builder
            .market_data_planning_error
            .as_deref()
            .unwrap()
            .contains("unknown router venue"));
    }

    #[test]
    fn ambiguous_and_cross_manual_strategies_require_concrete_plans() {
        for scope in [ports::VenueScope::Single, ports::VenueScope::Cross] {
            let builder = SystemBuilder::new(demand_config())
                .register_strategy(ManualDemandStrategy(scope))
                .register_market_streams_from_config();
            assert!(builder.market_data_planning_error.is_some());

            let builder = SystemBuilder::new(demand_config())
                .register_strategy(ManualDemandStrategy(scope))
                .register_market_stream_plan(
                    VenueType::Binance,
                    "binance".into(),
                    vec![Symbol::new("BTCUSDT")],
                )
                .register_market_streams_from_config();
            assert!(builder.market_data_planning_error.is_none());
            assert_eq!(builder.market_stream_plans.len(), 1);
            assert_eq!(builder.market_stream_plans[0].0, VenueType::Binance);
        }
    }

    #[test]
    fn explicit_manual_plan_does_not_duplicate_automatic_quote_subscription() {
        let mut config = demand_config();
        config.venues.truncate(1);
        config.quotes_only = true;
        let builder = SystemBuilder::new(config)
            .register_market_stream_plan(
                VenueType::Binance,
                "binance".into(),
                vec![Symbol::new("BTCUSDT")],
            )
            .register_market_streams_from_config();
        assert!(builder.market_data_planning_error.is_none());
        assert_eq!(builder.market_stream_plans.len(), 1);
    }
}
