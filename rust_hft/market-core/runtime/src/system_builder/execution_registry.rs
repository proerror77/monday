#[cfg(feature = "adapter-binance-data")]
use super::{binance_execution_market, execution_config_value, BinanceMarketIdentity};
use super::{SystemBuilder, VenueConfig, VenueType};
use tracing::{info, warn};

#[cfg(feature = "adapter-binance-data")]
const BINANCE_USDM_REST_LIVE: &str = "https://fapi.binance.com";
#[cfg(feature = "adapter-binance-data")]
const BINANCE_USDM_WS_LIVE: &str = "wss://fstream.binance.com/private/ws";
#[cfg(feature = "adapter-binance-data")]
const BINANCE_USDM_REST_TESTNET: &str = "https://demo-fapi.binance.com";
#[cfg(feature = "adapter-binance-data")]
const BINANCE_USDM_WS_TESTNET: &str = "wss://demo-fstream.binance.com/private/ws";

#[cfg(feature = "adapter-binance-data")]
fn is_known_binance_spot_rest_endpoint(url: &str) -> bool {
    matches!(
        url.trim_end_matches('/'),
        "https://api.binance.com"
            | "https://api1.binance.com"
            | "https://api2.binance.com"
            | "https://api3.binance.com"
            | "https://api4.binance.com"
            | "https://data-api.binance.vision"
    )
}

#[cfg(feature = "adapter-binance-data")]
fn is_known_binance_spot_ws_endpoint(url: &str) -> bool {
    matches!(
        url.trim_end_matches('/'),
        "wss://data-stream.binance.vision/ws"
            | "wss://stream.binance.com:9443/ws"
            | "wss://stream.binance.com:9443/stream"
            | "wss://stream.binance.com/ws"
            | "wss://stream.binance.com"
    )
}

#[cfg(feature = "adapter-binance-data")]
fn is_binance_usdm_live_rest_endpoint(url: &str) -> bool {
    url.trim_end_matches('/') == BINANCE_USDM_REST_LIVE
}

#[cfg(feature = "adapter-binance-data")]
fn is_binance_usdm_testnet_rest_endpoint(url: &str) -> bool {
    url.trim_end_matches('/') == BINANCE_USDM_REST_TESTNET
}

#[cfg(feature = "adapter-binance-data")]
fn is_binance_usdm_live_ws_endpoint(url: &str) -> bool {
    url.trim_end_matches('/') == BINANCE_USDM_WS_LIVE
}

#[cfg(feature = "adapter-binance-data")]
fn is_binance_usdm_testnet_ws_endpoint(url: &str) -> bool {
    url.trim_end_matches('/') == BINANCE_USDM_WS_TESTNET
}

#[cfg(feature = "adapter-binance-data")]
fn configured_binance_usdm_endpoint(
    configured: Option<&str>,
    testnet: bool,
    rest: bool,
) -> Result<String, String> {
    let configured = configured.map(str::trim).filter(|value| !value.is_empty());
    if rest {
        if testnet {
            if configured.is_none() || configured.is_some_and(is_known_binance_spot_rest_endpoint) {
                return Ok(BINANCE_USDM_REST_TESTNET.to_string());
            }
            if configured.is_some_and(is_binance_usdm_live_rest_endpoint) {
                return Err(format!(
                    "Binance USD-M Testnet cannot use the production REST endpoint {BINANCE_USDM_REST_LIVE}"
                ));
            }
            if configured.is_some_and(is_binance_usdm_testnet_rest_endpoint) {
                return Ok(BINANCE_USDM_REST_TESTNET.to_string());
            }
            if let Some(value) = configured {
                return Ok(value.to_string());
            }
        } else {
            if configured.is_none()
                || configured.is_some_and(is_known_binance_spot_rest_endpoint)
                || configured.is_some_and(is_binance_usdm_live_rest_endpoint)
            {
                return Ok(BINANCE_USDM_REST_LIVE.to_string());
            }
            if configured.is_some_and(is_binance_usdm_testnet_rest_endpoint) {
                return Err(format!(
                    "Binance USD-M Live cannot use the Testnet REST endpoint {BINANCE_USDM_REST_TESTNET}"
                ));
            }
            if let Some(value) = configured {
                return Ok(value.to_string());
            }
        }
    } else if testnet {
        if configured.is_none() || configured.is_some_and(is_known_binance_spot_ws_endpoint) {
            return Ok(BINANCE_USDM_WS_TESTNET.to_string());
        }
        if configured.is_some_and(is_binance_usdm_live_ws_endpoint) {
            return Err(format!(
                "Binance USD-M Testnet cannot use the production private stream endpoint {BINANCE_USDM_WS_LIVE}"
            ));
        }
        if configured.is_some_and(is_binance_usdm_testnet_ws_endpoint) {
            return Ok(BINANCE_USDM_WS_TESTNET.to_string());
        }
        if let Some(value) = configured {
            return Ok(value.to_string());
        }
    } else {
        if configured.is_none()
            || configured.is_some_and(is_known_binance_spot_ws_endpoint)
            || configured.is_some_and(is_binance_usdm_live_ws_endpoint)
        {
            return Ok(BINANCE_USDM_WS_LIVE.to_string());
        }
        if configured.is_some_and(is_binance_usdm_testnet_ws_endpoint) {
            return Err(format!(
                "Binance USD-M Live cannot use the Testnet private stream endpoint {BINANCE_USDM_WS_TESTNET}"
            ));
        }
        if let Some(value) = configured {
            return Ok(value.to_string());
        }
    }
    unreachable!("configured Binance USD-M endpoint branch must return")
}

#[cfg(feature = "adapter-binance-data")]
fn validate_binance_usdm_execution_config(
    execution_config: Option<&serde_yaml::Value>,
) -> Result<(), String> {
    let one_way = ["one_way", "one-way", "oneway", "both", "single"];
    if let Some(account_mode) = execution_config_value(execution_config, "account_mode") {
        if !one_way
            .iter()
            .any(|allowed| account_mode.trim().eq_ignore_ascii_case(allowed))
        {
            return Err(format!(
                "unsupported Binance USD-M account_mode '{account_mode}'; only one-way/BOTH is supported"
            ));
        }
    }
    if let Some(position_mode) = execution_config_value(execution_config, "position_mode") {
        if !one_way
            .iter()
            .any(|allowed| position_mode.trim().eq_ignore_ascii_case(allowed))
        {
            return Err(format!(
                "unsupported Binance USD-M position_mode '{position_mode}'; only one-way/BOTH is supported"
            ));
        }
    }
    if let Some(order_mode) = execution_config_value(execution_config, "order_mode") {
        if !order_mode.trim().eq_ignore_ascii_case("standard") {
            return Err(format!(
                "unsupported Binance USD-M order_mode '{order_mode}'; only standard MARKET/LIMIT orders are supported"
            ));
        }
    }
    if let Some(account_type) = execution_config_value(execution_config, "account_type") {
        let accepted = [
            "usdm",
            "usd-m",
            "usdⓈ-m",
            "usdt_futures",
            "usdt-futures",
            "usdm_futures",
        ];
        if !accepted
            .iter()
            .any(|allowed| account_type.trim().eq_ignore_ascii_case(allowed))
        {
            return Err(format!(
                "unsupported Binance USD-M account_type '{account_type}'"
            ));
        }
    }
    Ok(())
}

impl SystemBuilder {
    pub(crate) fn register_execution_clients_from_config(mut self) -> Self {
        let venues = self.config.venues.clone();
        for venue in venues {
            self = self.register_execution_clients_for_venue(&venue);
        }
        self
    }

    fn register_execution_clients_for_venue(self, venue: &VenueConfig) -> Self {
        match venue.venue_type {
            VenueType::Bitget => self.register_bitget_adapters(venue),
            VenueType::Binance => self.register_binance_adapters(venue),
            VenueType::BinancePrediction => self.register_binance_prediction_adapters(venue),
            VenueType::Bybit => self.register_bybit_adapters(venue),
            VenueType::Okx => self.register_okx_adapters(venue),
            VenueType::Grvt => self.register_grvt_adapters(venue),
            VenueType::Asterdex => self.register_asterdex_adapters(venue),
            VenueType::Hyperliquid | VenueType::Lighter | VenueType::Backpack => {
                warn!(
                    "已退役的 runtime venue 不註冊執行客戶端: {:?}",
                    venue.venue_type
                );
                self
            }
            VenueType::OndoPerps => self.register_ondo_perps_adapters(venue),
            VenueType::Polymarket => self.register_polymarket_adapters(venue),
            VenueType::Mock => {
                if venue.simulate_execution {
                    info!("Mock: 使用模擬執行客戶端 (SimulatedExecutionClient)");
                    self.register_simulated_execution_client(hft_core::VenueId::MOCK)
                } else {
                    info!("Mock 適配器不註冊執行客戶端（僅行情）");
                    self
                }
            }
        }
    }
}

impl SystemBuilder {
    #[cfg(feature = "adapter-polymarket-execution")]
    pub(crate) fn register_polymarket_adapters(mut self, venue: &VenueConfig) -> Self {
        use adapter_polymarket_execution::{
            PolymarketExecutionClient, PolymarketExecutionConfig, WalletSignatureType,
        };
        use secrecy::SecretString;
        use std::str::FromStr;

        if venue.simulate_execution {
            info!(
                "Polymarket simulate_execution is enabled; registering Monday simulated execution"
            );
            return self.register_simulated_execution_client(hft_core::VenueId::POLYMARKET);
        }
        if !venue
            .execution_mode
            .as_deref()
            .is_some_and(|mode| mode.eq_ignore_ascii_case("live"))
        {
            info!("Polymarket live execution is disabled; registering Monday simulated execution");
            return self.register_simulated_execution_client(hft_core::VenueId::POLYMARKET);
        }

        let settings = match venue.execution_config.clone() {
            Some(value) => {
                match serde_yaml::from_value::<PolymarketRuntimeExecutionConfig>(value) {
                    Ok(settings) => settings,
                    Err(error) => {
                        warn!(%error, "Polymarket execution_config is invalid; signature_type must be explicit");
                        return self;
                    }
                }
            }
            None => {
                warn!("Polymarket Live execution requires execution_config.signature_type");
                return self;
            }
        };
        let signature_type = match WalletSignatureType::from_str(&settings.signature_type) {
            Ok(value) => value,
            Err(error) => {
                warn!(%error, "Polymarket execution signature_type is invalid");
                return self;
            }
        };
        let config = PolymarketExecutionConfig {
            host: venue
                .rest
                .clone()
                .unwrap_or_else(|| "https://clob.polymarket.com".to_string()),
            ws_url: venue
                .ws_private
                .clone()
                .or_else(|| venue.ws_public.clone())
                .unwrap_or_else(|| "wss://ws-subscriptions-clob.polymarket.com".to_string()),
            data_api_host: settings.data_api_host,
            private_key: venue
                .secret
                .clone()
                .filter(|value| !value.trim().is_empty() && !value.contains("${"))
                .map(|value| SecretString::new(value.into())),
            funder: settings.funder,
            signature_type,
            use_server_time: settings.use_server_time,
            minimum_collateral: settings.minimum_collateral,
        };
        match PolymarketExecutionClient::new(config) {
            Ok(client) => {
                let account = venue
                    .account_id
                    .as_ref()
                    .map(|value| hft_core::AccountId(value.clone()));
                self = self.register_execution_client_with_key(
                    client,
                    hft_core::VenueId::POLYMARKET,
                    account,
                );
                info!("registered Monday-native Polymarket live execution client");
            }
            Err(error) => warn!(%error, "Polymarket live execution is not configured"),
        }
        self
    }

    #[cfg(not(feature = "adapter-polymarket-execution"))]
    pub(crate) fn register_polymarket_adapters(self, venue: &VenueConfig) -> Self {
        if venue
            .execution_mode
            .as_deref()
            .is_some_and(|mode| mode.eq_ignore_ascii_case("live"))
        {
            warn!("Polymarket live execution adapter is not enabled");
            self
        } else {
            self.register_simulated_execution_client(hft_core::VenueId::POLYMARKET)
        }
    }

    #[cfg(feature = "adapter-binance-prediction-execution")]
    pub(crate) fn register_binance_prediction_adapters(mut self, venue: &VenueConfig) -> Self {
        use adapter_binance_prediction_execution as prediction;

        let settings = venue.execution_config.clone().and_then(|value| {
            serde_yaml::from_value::<prediction::BinancePredictionVenueConfig>(value).ok()
        });
        let Some(settings) = settings else {
            warn!("Binance Prediction requires execution_config with wallet and funding settings");
            return self;
        };
        let mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
            "Live" => prediction::ExecutionMode::Live,
            "Testnet" => prediction::ExecutionMode::Testnet,
            _ => prediction::ExecutionMode::Paper,
        };
        let config = prediction::BinancePredictionExecutionConfig {
            api_key: venue.api_key.clone().unwrap_or_default(),
            api_secret: venue.secret.clone().unwrap_or_default(),
            wallet_address: settings.wallet_address,
            wallet_id: settings.wallet_id,
            rest_base_url: venue
                .rest
                .clone()
                .unwrap_or_else(|| "https://api.binance.com".to_string()),
            timeout_ms: settings.timeout_ms,
            mode,
            account_type: settings.account_type,
            funding_source: settings.funding_source,
        };
        match prediction::BinancePredictionExecutionClient::new(config) {
            Ok(client) => {
                let account = venue
                    .account_id
                    .as_ref()
                    .map(|value| hft_core::AccountId(value.clone()));
                self = self.register_execution_client_with_key(
                    client,
                    hft_core::VenueId::BINANCE_PREDICTION,
                    account,
                );
            }
            Err(error) => warn!(%error, "failed to configure Binance Prediction execution"),
        }
        self
    }

    #[cfg(not(feature = "adapter-binance-prediction-execution"))]
    pub(crate) fn register_binance_prediction_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Binance Prediction execution adapter is not enabled");
        self
    }

    #[cfg(feature = "adapter-bitget-data")]
    pub(crate) fn register_bitget_adapters(mut self, venue: &VenueConfig) -> Self {
        info!("註冊 Bitget 適配器");
        #[cfg(feature = "adapter-bitget-execution")]
        {
            use adapter_bitget_execution as bitget_exec;
            let execution_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
                "Live" => bitget_exec::ExecutionMode::Live,
                _ => bitget_exec::ExecutionMode::Paper,
            };

            let credentials = integration::signing::BitgetCredentials::new(
                venue.api_key.clone().unwrap_or_default(),
                venue.secret.clone().unwrap_or_default(),
                venue.passphrase.clone().unwrap_or_default(),
            );

            let cfg = bitget_exec::BitgetExecutionConfig {
                credentials,
                mode: execution_mode,
                rest_base_url: venue
                    .rest
                    .clone()
                    .unwrap_or_else(|| "https://api.bitget.com".to_string()),
                ws_private_url: venue
                    .ws_private
                    .clone()
                    .unwrap_or_else(|| "wss://ws.bitget.com/v2/ws/private".to_string()),
                timeout_ms: 5000,
            };

            match bitget_exec::BitgetExecutionClient::new(cfg) {
                Ok(client) => {
                    let account = venue
                        .account_id
                        .as_ref()
                        .map(|s| hft_core::AccountId(s.clone()));
                    self = self.register_execution_client_with_key(
                        client,
                        hft_core::VenueId::BITGET,
                        account,
                    );
                }
                Err(e) => warn!("無法創建 Bitget 執行客戶端: {}", e),
            }
        }
        self
    }

    #[cfg(not(feature = "adapter-bitget-data"))]
    pub(crate) fn register_bitget_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Bitget 適配器未啟用 (缺少 feature flag)");
        self
    }

    #[cfg(feature = "adapter-binance-data")]
    pub(crate) fn register_binance_adapters(self, venue: &VenueConfig) -> Self {
        info!("註冊 Binance 適配器");
        if let Err(error) = super::validate_binance_market_config(venue) {
            warn!(%error, "Binance market identity is invalid; adapters not registered");
            return self;
        }
        if venue.simulate_execution {
            let market = match binance_execution_market(venue.execution_config.as_ref()) {
                Ok(market) => market,
                Err(error) => {
                    warn!(%error, "Binance execution_config market identity is unsupported; simulated execution client not registered");
                    return self;
                }
            };
            if market == BinanceMarketIdentity::Usdm {
                if let Err(error) =
                    validate_binance_usdm_execution_config(venue.execution_config.as_ref())
                {
                    warn!(%error, "Binance USD-M execution_config is unsupported; simulated execution client not registered");
                    return self;
                }
            }
            let simulated_venue = if market == BinanceMarketIdentity::Usdm {
                hft_core::VenueId::BINANCE_FUTURES
            } else {
                hft_core::VenueId::BINANCE
            };
            return self.register_simulated_execution_client(simulated_venue);
        }
        #[cfg(feature = "adapter-binance-execution")]
        {
            use adapter_binance_execution as binance_exec;
            let market = match binance_execution_market(venue.execution_config.as_ref()) {
                Ok(market) => market,
                Err(error) => {
                    warn!(%error, "Binance execution_config market identity is unsupported; execution client not registered");
                    return self;
                }
            };
            let exec_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
                "Live" => binance_exec::ExecutionMode::Live,
                "Testnet" => binance_exec::ExecutionMode::Testnet,
                _ => binance_exec::ExecutionMode::Paper,
            };
            if market == BinanceMarketIdentity::Usdm {
                if let Err(error) =
                    validate_binance_usdm_execution_config(venue.execution_config.as_ref())
                {
                    warn!(%error, "Binance USD-M execution_config is unsupported; execution client not registered");
                    return self;
                }
                let testnet = exec_mode == binance_exec::ExecutionMode::Testnet;
                let rest_base_url = match configured_binance_usdm_endpoint(
                    venue.rest.as_deref(),
                    testnet,
                    true,
                ) {
                    Ok(endpoint) => endpoint,
                    Err(error) => {
                        warn!(%error, "Binance USD-M REST endpoint is incompatible with execution mode; execution client not registered");
                        return self;
                    }
                };
                let ws_base_url = match configured_binance_usdm_endpoint(
                    venue.ws_private.as_deref(),
                    testnet,
                    false,
                ) {
                    Ok(endpoint) => endpoint,
                    Err(error) => {
                        warn!(%error, "Binance USD-M private stream endpoint is incompatible with execution mode; execution client not registered");
                        return self;
                    }
                };
                let cfg = binance_exec::BinanceUsdMExecutionConfig {
                    credentials: integration::signing::BinanceCredentials::new(
                        venue.api_key.clone().unwrap_or_default(),
                        venue.secret.clone().unwrap_or_default(),
                    ),
                    rest_base_url,
                    ws_base_url,
                    timeout_ms: 5000,
                    mode: exec_mode,
                    account_capability: hft_core::AccountCapability::default(),
                };
                let execution_client = binance_exec::BinanceUsdMExecutionClient::new(cfg);
                let account = venue
                    .account_id
                    .as_ref()
                    .map(|s| hft_core::AccountId(s.clone()));
                return self
                    .register_binance_usdm_execution_client_with_key(execution_client, account);
            }
            let cfg = binance_exec::BinanceExecutionConfig {
                credentials: integration::signing::BinanceCredentials::new(
                    venue.api_key.clone().unwrap_or_default(),
                    venue.secret.clone().unwrap_or_default(),
                ),
                rest_base_url: venue
                    .rest
                    .clone()
                    .unwrap_or_else(|| "https://api.binance.com".to_string()),
                ws_base_url: venue
                    .ws_private
                    .clone()
                    .unwrap_or_else(|| "wss://stream.binance.com:9443/ws".to_string()),
                timeout_ms: 5000,
                mode: exec_mode,
                account_capability: hft_core::AccountCapability::default(),
            };
            let execution_client = binance_exec::BinanceExecutionClient::new(cfg);
            let account = venue
                .account_id
                .as_ref()
                .map(|s| hft_core::AccountId(s.clone()));
            self.register_execution_client_with_key(
                execution_client,
                hft_core::VenueId::BINANCE,
                account,
            )
        }
        #[cfg(not(feature = "adapter-binance-execution"))]
        {
            self
        }
    }

    #[cfg(not(feature = "adapter-binance-data"))]
    pub(crate) fn register_binance_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Binance 適配器未啟用 (缺少 feature flag)");
        self
    }

    #[cfg(feature = "adapter-grvt-data")]
    pub(crate) fn register_grvt_adapters(mut self, venue: &VenueConfig) -> Self {
        info!("註冊 GRVT 適配器");
        #[cfg(feature = "adapter-grvt-execution")]
        {
            use adapter_grvt_execution as grvt_exec;
            let exec_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
                "Live" => grvt_exec::ExecutionMode::Live,
                _ => grvt_exec::ExecutionMode::Testnet,
            };
            let cfg = grvt_exec::GrvtExecutionConfig {
                auth_endpoint: std::env::var("GRVT_AUTH_ENDPOINT").ok(),
                rest_base_url: venue.rest.clone().unwrap_or_else(|| {
                    std::env::var("GRVT_REST")
                        .unwrap_or_else(|_| "https://api.testnet.grvt.io".to_string())
                }),
                ws_private_url: venue
                    .ws_private
                    .clone()
                    .or_else(|| std::env::var("GRVT_WS_PRIVATE").ok()),
                api_key: venue
                    .api_key
                    .clone()
                    .or_else(|| std::env::var("GRVT_API_KEY").ok()),
                timeout_ms: 5000,
                mode: exec_mode,
            };
            let client = grvt_exec::GrvtExecutionClient::new(cfg);
            let account = venue
                .account_id
                .as_ref()
                .map(|s| hft_core::AccountId(s.clone()));
            self =
                self.register_execution_client_with_key(client, hft_core::VenueId::GRVT, account);
        }
        self
    }

    #[cfg(not(feature = "adapter-grvt-data"))]
    pub(crate) fn register_grvt_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("GRVT 適配器未啟用 (缺少 feature flag)");
        self
    }

    #[cfg(feature = "adapter-ondo-perps-data")]
    pub(crate) fn register_ondo_perps_adapters(mut self, venue: &VenueConfig) -> Self {
        info!("註冊 Ondo Perps 適配器");
        #[cfg(feature = "adapter-ondo-perps-execution")]
        {
            use adapter_ondo_perps_execution::{
                OndoPerpsExecutionClient, OndoPerpsExecutionConfig,
            };
            let cfg = OndoPerpsExecutionConfig {
                rest_base_url: venue
                    .rest
                    .clone()
                    .unwrap_or_else(|| "https://api.ondoperps.xyz".to_string()),
                key_id: venue.api_key.clone().unwrap_or_default(),
                api_secret: venue.secret.clone().unwrap_or_default(),
                timeout_ms: 5_000,
            };
            match OndoPerpsExecutionClient::new(cfg) {
                Ok(client) => {
                    let account = venue
                        .account_id
                        .as_ref()
                        .map(|id| hft_core::AccountId(id.clone()));
                    self = self.register_execution_client_with_key(
                        client,
                        hft_core::VenueId::ONDO_PERPS,
                        account,
                    );
                }
                Err(error) => warn!("Ondo Perps 執行未註冊: {}", error),
            }
        }
        self
    }

    #[cfg(not(feature = "adapter-ondo-perps-data"))]
    pub(crate) fn register_ondo_perps_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Ondo Perps 適配器未啟用 (缺少 feature flag)");
        self
    }

    #[cfg(feature = "adapter-asterdex-data")]
    pub(crate) fn register_asterdex_adapters(mut self, venue: &VenueConfig) -> Self {
        info!("註冊 Aster DEX 適配器");
        let mut registered_execution = false;

        #[cfg(feature = "adapter-asterdex-execution")]
        {
            use adapter_asterdex_execution as ast_exec;
            let exec_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
                "Live" => ast_exec::ExecutionMode::Live,
                _ => ast_exec::ExecutionMode::Paper,
            };
            let api_key = venue.api_key.clone().unwrap_or_default();
            let secret = venue.secret.clone().unwrap_or_default();
            let has_credentials = !api_key.trim().is_empty()
                && !secret.trim().is_empty()
                && !api_key.contains("${")
                && !secret.contains("${");

            if has_credentials {
                let cfg = ast_exec::AsterdexExecutionConfig {
                    credentials: integration::signing::AsterdexCredentials::new(api_key, secret),
                    rest_base_url: venue
                        .rest
                        .clone()
                        .unwrap_or_else(|| "https://fapi.asterdex.com".to_string()),
                    ws_base_url: venue
                        .ws_private
                        .clone()
                        .unwrap_or_else(|| "wss://fstream.asterdex.com/ws".to_string()),
                    timeout_ms: 5000,
                    mode: exec_mode,
                };
                let execution_client = ast_exec::AsterdexExecutionClient::new(cfg);
                let account = venue
                    .account_id
                    .as_ref()
                    .map(|s| hft_core::AccountId(s.clone()));
                self = self.register_execution_client_with_key(
                    execution_client,
                    hft_core::VenueId::ASTERDEX,
                    account,
                );
                registered_execution = true;
            }
        }

        if !registered_execution {
            if venue.simulate_execution {
                info!("Aster DEX: 使用模擬執行客戶端 (dry-run)");
                self = self.register_simulated_execution_client(hft_core::VenueId::ASTERDEX);
            } else {
                info!("Aster DEX: 未提供有效 API 憑證，僅註冊行情");
            }
        }

        self
    }

    #[cfg(not(feature = "adapter-asterdex-data"))]
    pub(crate) fn register_asterdex_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Aster DEX 適配器未啟用 (缺少 feature flag)");
        self
    }

    #[cfg(feature = "adapter-bybit-data")]
    pub(crate) fn register_bybit_adapters(mut self, venue: &VenueConfig) -> Self {
        #[cfg(feature = "adapter-bybit-execution")]
        {
            use adapter_bybit_execution as bybit_exec;
            let exec_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
                "Live" => bybit_exec::ExecutionMode::Live,
                "Testnet" => bybit_exec::ExecutionMode::Testnet,
                _ => bybit_exec::ExecutionMode::Paper,
            };
            let cfg = bybit_exec::BybitExecutionConfig {
                credentials: integration::signing::BybitCredentials::new(
                    venue.api_key.clone().unwrap_or_default(),
                    venue.secret.clone().unwrap_or_default(),
                ),
                mode: exec_mode,
                rest_base_url: venue
                    .rest
                    .clone()
                    .unwrap_or_else(|| "https://api.bybit.com".to_string()),
                ws_private_url: venue
                    .ws_private
                    .clone()
                    .unwrap_or_else(|| "wss://stream.bybit.com/v5/private".to_string()),
                timeout_ms: 5000,
            };
            match bybit_exec::BybitExecutionClient::new(cfg) {
                Ok(client) => {
                    let account = venue
                        .account_id
                        .as_ref()
                        .map(|s| hft_core::AccountId(s.clone()));
                    self = self.register_execution_client_with_key(
                        client,
                        hft_core::VenueId::BYBIT,
                        account,
                    );
                }
                Err(e) => warn!("無法創建 Bybit 執行客戶端: {}", e),
            }
        }
        self
    }

    #[cfg(not(feature = "adapter-bybit-data"))]
    pub(crate) fn register_bybit_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Bybit 適配器未啟用 (缺少 feature flag)");
        self
    }

    #[cfg(feature = "adapter-okx-execution")]
    pub(crate) fn register_okx_adapters(mut self, venue: &VenueConfig) -> Self {
        use adapter_okx_execution as okx_exec;
        let exec_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
            "Live" => okx_exec::ExecutionMode::Live,
            _ => okx_exec::ExecutionMode::Paper,
        };
        let cfg = okx_exec::OkxExecutionConfig {
            credentials: integration::signing::OkxCredentials::new(
                venue.api_key.clone().unwrap_or_default(),
                venue.secret.clone().unwrap_or_default(),
                venue.passphrase.clone().unwrap_or_default(),
            ),
            rest_base_url: venue
                .rest
                .clone()
                .unwrap_or_else(|| "https://www.okx.com".to_string()),
            ws_private_url: venue
                .ws_private
                .clone()
                .unwrap_or_else(|| "wss://ws.okx.com:8443/ws/v5/private".to_string()),
            timeout_ms: 5000,
            mode: exec_mode,
        };
        match okx_exec::OkxExecutionClient::new(cfg) {
            Ok(client) => {
                let account = venue
                    .account_id
                    .as_ref()
                    .map(|s| hft_core::AccountId(s.clone()));
                self = self.register_execution_client_with_key(
                    client,
                    hft_core::VenueId::OKX,
                    account,
                );
            }
            Err(e) => warn!("無法創建 OKX 執行客戶端: {}", e),
        }
        self
    }

    #[cfg(not(feature = "adapter-okx-execution"))]
    pub(crate) fn register_okx_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("OKX 適配器未啟用 (缺少 feature flag)");
        self
    }
}

#[cfg(feature = "adapter-polymarket-execution")]
#[derive(Debug, serde::Deserialize)]
struct PolymarketRuntimeExecutionConfig {
    #[serde(default = "default_polymarket_data_api_host")]
    data_api_host: String,
    #[serde(default)]
    funder: Option<String>,
    signature_type: String,
    #[serde(default = "default_true")]
    use_server_time: bool,
    #[serde(default)]
    minimum_collateral: rust_decimal::Decimal,
}

#[cfg(feature = "adapter-polymarket-execution")]
fn default_polymarket_data_api_host() -> String {
    "https://data-api.polymarket.com".to_string()
}

#[cfg(feature = "adapter-polymarket-execution")]
const fn default_true() -> bool {
    true
}

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::super::{SystemConfig, VenueCapabilities};
    use super::*;
    use shared_instrument::InstrumentId;

    #[cfg(feature = "adapter-binance-data")]
    #[test]
    fn binance_usdm_endpoint_selection_is_mode_bound_and_catalog_aware() {
        assert_eq!(
            configured_binance_usdm_endpoint(None, true, true).unwrap(),
            BINANCE_USDM_REST_TESTNET
        );
        assert_eq!(
            configured_binance_usdm_endpoint(Some("https://api.binance.com"), true, true).unwrap(),
            BINANCE_USDM_REST_TESTNET
        );
        assert!(
            configured_binance_usdm_endpoint(Some(BINANCE_USDM_REST_LIVE), true, true).is_err()
        );
        assert_eq!(
            configured_binance_usdm_endpoint(
                Some("wss://stream.binance.com:9443/stream"),
                true,
                false
            )
            .unwrap(),
            BINANCE_USDM_WS_TESTNET
        );
        assert_eq!(
            configured_binance_usdm_endpoint(None, false, false).unwrap(),
            BINANCE_USDM_WS_LIVE
        );
        assert!(
            configured_binance_usdm_endpoint(Some(BINANCE_USDM_WS_TESTNET), false, false).is_err()
        );
        assert_eq!(
            configured_binance_usdm_endpoint(Some("http://127.0.0.1:18080"), true, true).unwrap(),
            "http://127.0.0.1:18080"
        );
    }

    #[cfg(all(
        feature = "adapter-binance-data",
        feature = "adapter-binance-execution"
    ))]
    #[test]
    fn binance_usdm_routes_to_the_dedicated_futures_execution_client() {
        let venue = VenueConfig {
            name: "binance-usdm".to_string(),
            account_id: Some("usdm-main".to_string()),
            venue_type: VenueType::Binance,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
            execution_mode: Some("Paper".to_string()),
            capabilities: VenueCapabilities::default(),
            inst_type: Some("usdm".to_string()),
            simulate_execution: false,
            symbol_catalog: vec![InstrumentId::new("BTCUSDT@BINANCE")],
            data_config: None,
            execution_config: Some(serde_yaml::from_str("market: usdm").unwrap()),
        };
        let mut config = SystemConfig::default();
        config.venues.push(venue);

        let builder = SystemBuilder::new(config).register_execution_clients_from_config();

        assert_eq!(builder.execution_clients.len(), 1);
        assert_eq!(
            builder.execution_client_venues,
            vec![hft_core::VenueId::BINANCE_FUTURES]
        );
        assert_eq!(builder.execution_client_is_binance_usdm, vec![true]);
    }

    #[cfg(all(
        feature = "adapter-binance-data",
        feature = "adapter-binance-execution"
    ))]
    #[test]
    fn binance_without_explicit_usdm_execution_config_keeps_spot_routing() {
        let venue = VenueConfig {
            name: "binance-spot".to_string(),
            account_id: None,
            venue_type: VenueType::Binance,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
            execution_mode: Some("Paper".to_string()),
            capabilities: VenueCapabilities::default(),
            inst_type: Some("usdm".to_string()),
            simulate_execution: false,
            symbol_catalog: Vec::new(),
            data_config: None,
            execution_config: None,
        };
        let mut config = SystemConfig::default();
        config.venues.push(venue);

        let builder = SystemBuilder::new(config).register_execution_clients_from_config();

        assert_eq!(builder.execution_clients.len(), 1);
        assert_eq!(
            builder.execution_client_venues,
            vec![hft_core::VenueId::BINANCE]
        );
        assert_eq!(builder.execution_client_is_binance_usdm, vec![false]);
    }

    #[cfg(all(
        feature = "adapter-binance-data",
        feature = "adapter-binance-execution"
    ))]
    #[test]
    fn binance_usdm_rejects_unsupported_account_mode_before_registration() {
        let venue = VenueConfig {
            name: "binance-usdm-hedge".to_string(),
            account_id: None,
            venue_type: VenueType::Binance,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
            execution_mode: Some("Paper".to_string()),
            capabilities: VenueCapabilities::default(),
            inst_type: Some("usdm".to_string()),
            simulate_execution: false,
            symbol_catalog: Vec::new(),
            data_config: None,
            execution_config: Some(
                serde_yaml::from_str("market: usdm\naccount_mode: hedge").unwrap(),
            ),
        };
        let mut config = SystemConfig::default();
        config.venues.push(venue);

        let builder = SystemBuilder::new(config).register_execution_clients_from_config();

        assert!(builder.execution_clients.is_empty());
        assert!(builder.execution_client_venues.is_empty());
    }

    #[cfg(feature = "adapter-binance-data")]
    #[test]
    fn simulated_usdm_uses_the_futures_venue_identity_without_a_live_client_marker() {
        let venue = VenueConfig {
            name: "binance-usdm-paper".to_string(),
            account_id: None,
            venue_type: VenueType::Binance,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
            execution_mode: Some("Paper".to_string()),
            capabilities: VenueCapabilities::default(),
            inst_type: Some("usdm".to_string()),
            simulate_execution: true,
            symbol_catalog: Vec::<InstrumentId>::new(),
            data_config: None,
            execution_config: Some(serde_yaml::from_str("market: usdm").unwrap()),
        };
        let mut config = SystemConfig::default();
        config.venues.push(venue);

        let builder = SystemBuilder::new(config).register_execution_clients_from_config();

        assert_eq!(
            builder.execution_client_venues,
            vec![hft_core::VenueId::BINANCE_FUTURES]
        );
        assert_eq!(builder.execution_client_is_binance_usdm, vec![false]);
    }

    #[cfg(feature = "adapter-binance-prediction-execution")]
    #[test]
    fn binance_prediction_registers_as_execution_only_client() {
        let venue = VenueConfig {
            name: "binance-prediction".to_string(),
            account_id: Some("prediction-main".to_string()),
            venue_type: VenueType::BinancePrediction,
            ws_public: None,
            ws_private: None,
            rest: Some("https://api.binance.com".to_string()),
            api_key: None,
            secret: None,
            passphrase: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
            execution_mode: Some("Paper".to_string()),
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: false,
            symbol_catalog: Vec::<InstrumentId>::new(),
            data_config: None,
            execution_config: Some(
                serde_yaml::from_str(
                    r#"
wallet_address: "0x1234"
wallet_id: wallet-1
account_type: SPOT
funding_source: CEX
"#,
                )
                .unwrap(),
            ),
        };
        let mut config = SystemConfig::default();
        config.venues.push(venue);

        let builder = SystemBuilder::new(config).register_execution_clients_from_config();

        assert_eq!(builder.execution_clients.len(), 1);
        assert_eq!(
            builder.execution_client_venues,
            vec![hft_core::VenueId::BINANCE_PREDICTION]
        );
    }
}
