use super::{SystemBuilder, VenueConfig, VenueType};
use serde_yaml::{mapping::Mapping, Value as YamlValue};
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
            VenueType::Bybit => self.register_bybit_adapters(venue),
            VenueType::Okx => self.register_okx_adapters(venue),
            VenueType::Hyperliquid => self.register_hyperliquid_adapters(venue),
            VenueType::Grvt => self.register_grvt_adapters(venue),
            VenueType::Asterdex => self.register_asterdex_adapters(venue),
            VenueType::Lighter => self.register_lighter_adapters(venue),
            VenueType::Backpack => self.register_backpack_adapters(venue),
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
    pub(crate) fn register_binance_adapters(mut self, venue: &VenueConfig) -> Self {
        info!("註冊 Binance 適配器");
        #[cfg(feature = "adapter-binance-execution")]
        {
            use adapter_binance_execution as binance_exec;
            let exec_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
                "Live" => binance_exec::ExecutionMode::Live,
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
            };
            let execution_client = binance_exec::BinanceExecutionClient::new(cfg);
            let account = venue
                .account_id
                .as_ref()
                .map(|s| hft_core::AccountId(s.clone()));
            self = self.register_execution_client_with_key(
                execution_client,
                hft_core::VenueId::BINANCE,
                account,
            );
        }
        self
    }

    #[cfg(not(feature = "adapter-binance-data"))]
    pub(crate) fn register_binance_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Binance 適配器未啟用 (缺少 feature flag)");
        self
    }

    #[cfg(feature = "adapter-backpack-data")]
    pub(crate) fn register_backpack_adapters(mut self, venue: &VenueConfig) -> Self {
        info!("註冊 Backpack 適配器");
        #[cfg(feature = "adapter-backpack-execution")]
        {
            use adapter_backpack_execution as backpack_exec;
            let cfg = build_backpack_execution_config(venue);

            match backpack_exec::BackpackExecutionClient::new(cfg) {
                Ok(client) => {
                    let account = venue
                        .account_id
                        .as_ref()
                        .map(|s| hft_core::AccountId(s.clone()));
                    self = self.register_execution_client_with_key(
                        client,
                        hft_core::VenueId::BACKPACK,
                        account,
                    );
                }
                Err(e) => warn!("無法創建 Backpack 執行客戶端: {}", e),
            }
        }
        self
    }

    #[cfg(not(feature = "adapter-backpack-data"))]
    pub(crate) fn register_backpack_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Backpack 適配器未啟用 (缺少 feature flag)");
        self
    }

    #[cfg(feature = "adapter-hyperliquid-data")]
    pub(crate) fn register_hyperliquid_adapters(mut self, venue: &VenueConfig) -> Self {
        info!("註冊 Hyperliquid 適配器");
        #[cfg(feature = "adapter-hyperliquid-execution")]
        {
            use adapter_hyperliquid_execution as hl_exec;
            let exec_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
                "Live" => hl_exec::ExecutionMode::Live,
                _ => hl_exec::ExecutionMode::Paper,
            };
            let cfg = hl_exec::HyperliquidExecutionConfig {
                rest_base_url: venue.rest.clone().unwrap_or_default(),
                ws_private_url: venue.ws_private.clone().unwrap_or_default(),
                api_key: venue.api_key.clone().unwrap_or_default(),
                secret_key: venue.secret.clone().unwrap_or_default(),
                timeout_ms: 5000,
                mode: exec_mode,
            };
            let client = hl_exec::HyperliquidExecutionClient::new(cfg);
            let account = venue
                .account_id
                .as_ref()
                .map(|s| hft_core::AccountId(s.clone()));
            self = self.register_execution_client_with_key(
                client,
                hft_core::VenueId::HYPERLIQUID,
                account,
            );
        }
        self
    }

    #[cfg(not(feature = "adapter-hyperliquid-data"))]
    pub(crate) fn register_hyperliquid_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Hyperliquid 適配器未啟用 (缺少 feature flag)");
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

    #[cfg(feature = "adapter-lighter-execution")]
    pub(crate) fn register_lighter_adapters(mut self, venue: &VenueConfig) -> Self {
        info!("註冊 Lighter 執行適配器");
        use adapter_lighter_execution as lighter_exec;
        let exec_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
            "Live" => lighter_exec::ExecutionMode::Live,
            _ => lighter_exec::ExecutionMode::Paper,
        };
        let cfg = lighter_exec::LighterExecutionConfig {
            rest_base_url: venue.rest.clone().unwrap_or_else(|| {
                std::env::var("LIGHTER_BASE_URL")
                    .unwrap_or_else(|_| "https://mainnet.zklighter.elliot.ai".to_string())
            }),
            timeout_ms: 5000,
            mode: exec_mode,
            signer_lib_path: std::env::var("LIGHTER_SIGNER_LIB_PATH").ok(),
            api_key_private_key: std::env::var("LIGHTER_API_KEY_PRIVATE_KEY").ok(),
            api_key_index: std::env::var("LIGHTER_API_KEY_INDEX")
                .ok()
                .and_then(|s| s.parse::<i32>().ok()),
            account_index: std::env::var("LIGHTER_ACCOUNT_INDEX")
                .ok()
                .and_then(|s| s.parse::<i64>().ok()),
        };
        match lighter_exec::LighterExecutionClient::new(cfg) {
            Ok(mut client) => {
                // Connect immediately to validate FFI in Live mode
                let _ = futures::executor::block_on(async { client.connect().await });
                let account = venue
                    .account_id
                    .as_ref()
                    .map(|s| hft_core::AccountId(s.clone()));
                self = self.register_execution_client_with_key(
                    client,
                    hft_core::VenueId::LIGHTER,
                    account,
                );
            }
            Err(e) => warn!("無法創建 Lighter 執行客戶端: {}", e),
        }
        self
    }

    #[cfg(not(feature = "adapter-lighter-execution"))]
    pub(crate) fn register_lighter_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Lighter 執行適配器未啟用 (缺少 feature flag)");
        self
    }
}

#[cfg(feature = "adapter-backpack-execution")]
fn build_backpack_execution_config(
    venue: &VenueConfig,
) -> adapter_backpack_execution::BackpackExecutionConfig {
    use adapter_backpack_execution::{BackpackExecutionConfig, ExecutionMode};

    let mut mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
        s if s.eq_ignore_ascii_case("Live") => ExecutionMode::Live,
        _ => ExecutionMode::Paper,
    };

    let mut rest_base_url = venue
        .rest
        .clone()
        .unwrap_or_else(|| "https://api.backpack.exchange".to_string());
    let mut ws_public_url = venue
        .ws_public
        .clone()
        .unwrap_or_else(|| "wss://ws.backpack.exchange".to_string());
    let mut api_key = venue.api_key.clone().unwrap_or_default();
    let mut secret_key = venue.secret.clone().unwrap_or_default();
    let mut timeout_ms: u64 = 5000;
    let mut window_ms: Option<u64> = Some(5000);

    if let Some(value) = venue.execution_config.clone() {
        if let Some(parsed) = parse_backpack_execution_yaml(&value, &venue.name) {
            if let Some(rest) = parsed.rest_base_url {
                rest_base_url = rest;
            }
            if let Some(ws) = parsed.ws_public_url {
                ws_public_url = ws;
            }
            if let Some(key) = parsed.api_key {
                api_key = key;
            }
            if let Some(secret) = parsed.secret_key {
                secret_key = secret;
            }
            if let Some(timeout) = parsed.timeout_ms {
                timeout_ms = timeout;
            }
            if let Some(win) = parsed.window_ms {
                window_ms = Some(win);
            }
            if let Some(mode_str) = parsed.mode {
                mode = match mode_str.as_str() {
                    s if s.eq_ignore_ascii_case("Live") => ExecutionMode::Live,
                    _ => ExecutionMode::Paper,
                };
            }
        }
    }

    BackpackExecutionConfig {
        rest_base_url,
        ws_public_url,
        api_key,
        secret_key,
        timeout_ms,
        mode,
        window_ms,
    }
}

#[cfg(feature = "adapter-backpack-execution")]
struct ParsedBackpackExecutionOptions {
    mode: Option<String>,
    rest_base_url: Option<String>,
    ws_public_url: Option<String>,
    api_key: Option<String>,
    secret_key: Option<String>,
    timeout_ms: Option<u64>,
    window_ms: Option<u64>,
}

#[cfg(feature = "adapter-backpack-execution")]
fn parse_backpack_execution_yaml(
    value: &YamlValue,
    venue_name: &str,
) -> Option<ParsedBackpackExecutionOptions> {
    let map = match value {
        YamlValue::Mapping(map) => map.clone(),
        _ => {
            warn!(
                "Backpack execution_config 不是 mapping 結構 (venue: {}), 將忽略",
                venue_name
            );
            return None;
        }
    };

    if let Some(adapter) = map
        .get(&YamlValue::from("adapter_type"))
        .and_then(|v| v.as_str())
    {
        if !adapter.eq_ignore_ascii_case("backpack") {
            warn!(
                "Backpack execution_config adapter_type = {} 與預期不符 (venue: {}), 使用預設設定",
                adapter, venue_name
            );
            return None;
        }
    }

    Some(ParsedBackpackExecutionOptions {
        mode: yaml_get_str(&map, "mode"),
        rest_base_url: yaml_get_str(&map, "rest_base_url"),
        ws_public_url: yaml_get_str(&map, "ws_public_url"),
        api_key: yaml_get_str(&map, "api_key"),
        secret_key: yaml_get_str(&map, "secret_key"),
        timeout_ms: yaml_get_u64(&map, "timeout_ms"),
        window_ms: yaml_get_u64(&map, "window_ms"),
    })
}

#[cfg(feature = "adapter-backpack-execution")]
fn yaml_get_str(map: &Mapping, key: &str) -> Option<String> {
    map.get(&YamlValue::from(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(feature = "adapter-backpack-execution")]
fn yaml_get_u64(map: &Mapping, key: &str) -> Option<u64> {
    map.get(&YamlValue::from(key)).and_then(|v| match v {
        YamlValue::Number(n) => n.as_u64(),
        YamlValue::String(s) => s.parse::<u64>().ok(),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VenueCapabilities;
    use shared_instrument::InstrumentId;

    #[cfg(feature = "adapter-backpack-execution")]
    #[test]
    fn backpack_execution_config_overrides_defaults() {
        let exec_yaml = serde_yaml::from_str::<YamlValue>(
            r#"
adapter_type: backpack
mode: Live
rest_base_url: https://api.test
ws_public_url: wss://ws.test
timeout_ms: 8000
window_ms: 7000
api_key: TESTKEY
secret_key: TESTSECRET
"#,
        )
        .unwrap();

        let venue = VenueConfig {
            name: "backpack".to_string(),
            account_id: None,
            venue_type: VenueType::Backpack,
            ws_public: Some("wss://default".to_string()),
            ws_private: None,
            rest: Some("https://default".to_string()),
            api_key: Some("ENV_KEY".to_string()),
            secret: Some("ENV_SECRET".to_string()),
            passphrase: None,
            execution_mode: Some("Paper".to_string()),
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: false,
            symbol_catalog: Vec::<InstrumentId>::new(),
            data_config: None,
            execution_config: Some(exec_yaml),
        };

        let cfg = build_backpack_execution_config(&venue);
        assert!(matches!(
            cfg.mode,
            adapter_backpack_execution::ExecutionMode::Live
        ));
        assert_eq!(cfg.rest_base_url, "https://api.test");
        assert_eq!(cfg.ws_public_url, "wss://ws.test");
        assert_eq!(cfg.timeout_ms, 8000);
        assert_eq!(cfg.window_ms, Some(7000));
        assert_eq!(cfg.api_key, "TESTKEY");
        assert_eq!(cfg.secret_key, "TESTSECRET");
    }
}
