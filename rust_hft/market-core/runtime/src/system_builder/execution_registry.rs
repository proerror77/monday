use super::{SystemBuilder, VenueConfig, VenueType};
use tracing::{info, warn};

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
        if venue.simulate_execution {
            return self.register_simulated_execution_client(hft_core::VenueId::BINANCE);
        }
        #[cfg(feature = "adapter-binance-execution")]
        {
            use adapter_binance_execution as binance_exec;
            let exec_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
                "Live" => binance_exec::ExecutionMode::Live,
                "Testnet" => binance_exec::ExecutionMode::Testnet,
                _ => binance_exec::ExecutionMode::Paper,
            };
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
