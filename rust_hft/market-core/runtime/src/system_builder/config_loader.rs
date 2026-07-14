use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde_yaml::Value as YamlValue;
use tracing::{info, warn};

use thiserror::Error;

use engine::dataflow::FlipPolicy;
use hft_core::{Symbol, VenueId};
use rust_decimal::Decimal;
use shared_config as shared;
use shared_config::SystemConfig as SharedSystemConfig;
use shared_instrument::{InstrumentCatalog, InstrumentCatalogConfig, VenueMeta};

use super::{
    CpuAffinityConfig, ExecutionQueueSettings, LobFlowGridParams, PortfolioSpec, RiskConfig,
    StrategyConfig, StrategyParams, StrategyRiskLimits, StrategyRiskOverride, StrategyType,
    SystemConfig, SystemEngineConfig, TokenizedSecuritiesRiskConfig, VenueCapabilities,
    VenueConfig, VenueType,
};

use serde::{Deserialize, Serialize};
use serde_yaml;

#[derive(Debug, Error)]
enum LoaderError {
    #[error("配置驗證失敗: {0:?}")]
    Validation(Vec<String>),
}

struct ValidationContext {
    strict: bool,
    warnings: Vec<String>,
}

impl ValidationContext {
    fn new(strict: bool) -> Self {
        Self {
            strict,
            warnings: Vec::new(),
        }
    }

    fn warn(&mut self, message: impl Into<String>) {
        let message = message.into();
        warn!("{}", message);
        self.warnings.push(message);
    }

    fn finalize(self) -> Result<(), LoaderError> {
        if self.strict && !self.warnings.is_empty() {
            Err(LoaderError::Validation(self.warnings))
        } else {
            Ok(())
        }
    }
}

pub(crate) fn load_config_from_yaml(
    path: &str,
) -> Result<SystemConfig, Box<dyn std::error::Error>> {
    let yaml_content = fs::read_to_string(path)?;
    load_config_from_str(&yaml_content)
}

pub(crate) fn load_config_from_str(
    content: &str,
) -> Result<SystemConfig, Box<dyn std::error::Error>> {
    let expanded_content = expand_env_vars(content)?;

    // Legacy path first for backwards compatibility
    if let Ok(mut config) = serde_yaml::from_str::<SystemConfig>(&expanded_content) {
        normalize_accounts(&mut config);
        normalize_execution_modes(&mut config);
        validate_runtime_config(&config)
            .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
        return Ok(config);
    }

    // New schema (v2) using shared_config crate
    if let Ok(shared_cfg) = shared::SystemConfig::from_yaml_str(&expanded_content) {
        if shared_cfg.schema_version.as_deref() == Some("v2") {
            // 嘗試直接從原始 YAML 讀取 quotes_only（shared schema 可能未定義該欄位）
            let quotes_only_flag = serde_yaml::from_str::<serde_yaml::Value>(&expanded_content)
                .ok()
                .and_then(|v| v.get("quotes_only").and_then(|b| b.as_bool()))
                .unwrap_or(false);

            let mut cfg = convert_shared_config(shared_cfg)
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
            cfg.quotes_only = quotes_only_flag;
            normalize_execution_modes(&mut cfg);
            validate_runtime_config(&cfg)
                .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
            return Ok(cfg);
        }
    }

    // Fallback to legacy template expansion flow
    legacy_load_with_templates(&expanded_content)
}

fn legacy_load_with_templates(content: &str) -> Result<SystemConfig, Box<dyn std::error::Error>> {
    let mut root: YamlValue = serde_yaml::from_str(content)?;
    if let Some(expanded) = expand_templates_into_strategies(&root)? {
        if let YamlValue::Mapping(ref mut map) = root {
            map.insert(
                YamlValue::from("strategies"),
                serde_yaml::to_value(&expanded)?,
            );
            map.remove(YamlValue::from("instruments"));
        }
    }
    let mut config: SystemConfig = serde_yaml::from_value(root)?;
    normalize_accounts(&mut config);
    normalize_execution_modes(&mut config);
    validate_runtime_config(&config)
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
    Ok(config)
}

fn normalize_execution_modes(config: &mut SystemConfig) {
    for venue in &mut config.venues {
        let Some(mode) = venue.execution_mode.as_mut() else {
            continue;
        };
        if mode.eq_ignore_ascii_case("paper") {
            *mode = "Paper".to_string();
        } else if mode.eq_ignore_ascii_case("live") {
            *mode = "Live".to_string();
        } else if mode.eq_ignore_ascii_case("testnet") {
            *mode = "Testnet".to_string();
        }
    }
}

fn validate_runtime_config(config: &SystemConfig) -> Result<(), LoaderError> {
    let mut errors = Vec::new();
    if let Err(error) = config.engine.validate_intent_execution_limits() {
        errors.push(error);
    }
    if config.engine.balance_reconcile_tolerance_usd < Decimal::ZERO {
        errors.push("engine.balance_reconcile_tolerance_usd must be non-negative".to_string());
    }
    if config.engine.auto_cancel_exchange_only {
        errors.push(
            "engine.auto_cancel_exchange_only is deprecated and unsupported because only the runtime OMS control plane can safely classify exchange-only orders"
                .to_string(),
        );
    }
    for venue in &config.venues {
        if let Some(mode) = venue.execution_mode.as_deref() {
            if !mode.eq_ignore_ascii_case("paper")
                && !mode.eq_ignore_ascii_case("live")
                && !mode.eq_ignore_ascii_case("testnet")
            {
                errors.push(format!(
                    "venue '{}' execution_mode '{}' is invalid; expected Paper, Live, or Testnet",
                    venue.name, mode
                ));
            }
        }
        if venue.venue_type == VenueType::Polymarket
            && venue
                .execution_mode
                .as_deref()
                .is_some_and(|mode| mode.eq_ignore_ascii_case("testnet"))
        {
            errors.push(format!(
                "venue '{}' Polymarket does not support Testnet execution_mode",
                venue.name
            ));
        }
        if venue.venue_type == VenueType::Polymarket
            && venue
                .execution_mode
                .as_deref()
                .is_some_and(|mode| mode.eq_ignore_ascii_case("live"))
        {
            if venue.simulate_execution {
                errors.push(format!(
                    "venue '{}' Polymarket Live execution cannot set simulate_execution=true",
                    venue.name
                ));
            }
            let signature_type = venue
                .execution_config
                .as_ref()
                .and_then(|value| value.get("signature_type"))
                .and_then(YamlValue::as_str);
            if !signature_type
                .is_some_and(|value| !value.trim().is_empty() && !value.contains("${"))
            {
                errors.push(format!(
                    "venue '{}' Polymarket Live execution requires an explicit execution_config.signature_type",
                    venue.name
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(LoaderError::Validation(errors))
    }
}

fn convert_shared_config(shared_cfg: SharedSystemConfig) -> Result<SystemConfig, LoaderError> {
    let engine = SystemEngineConfig {
        queue_capacity: shared_cfg.engine.queue_capacity,
        stale_us: shared_cfg.engine.stale_us,
        intent_max_latency_us: 3_000,
        intent_max_slippage_bps: None,
        intent_max_order_notional: None,
        top_n: shared_cfg.engine.top_n,
        flip_policy: FlipPolicy::OnUpdate,
        cpu_affinity: CpuAffinityConfig::default(),
        ack_timeout_ms: shared_cfg.engine.ack_timeout_ms,
        reconcile_interval_ms: shared_cfg.engine.reconcile_interval_ms,
        balance_reconcile_tolerance_usd: shared_cfg.engine.balance_reconcile_tolerance_usd,
        auto_cancel_exchange_only: shared_cfg.engine.auto_cancel_exchange_only,
        execution_queue: ExecutionQueueSettings::default(),
    };

    let strict = is_strict_mode();
    let auto_fill = allow_autofill_symbols();
    let instrument_catalog = load_instrument_catalog();
    let venue_catalog = load_venue_catalog();

    let mut validation = ValidationContext::new(strict);

    let venues: Vec<VenueConfig> = shared_cfg
        .venues
        .into_iter()
        .map(|cfg| {
            convert_venue_config(
                cfg,
                instrument_catalog.as_ref(),
                venue_catalog.as_ref(),
                auto_fill,
                &mut validation,
            )
        })
        .collect();

    let symbol_whitelist: std::collections::HashSet<Symbol> = venues
        .iter()
        .flat_map(|venue| venue.symbol_catalog.iter().filter_map(|id| id.symbol()))
        .collect();

    let strategies = shared_cfg
        .strategies
        .into_iter()
        .map(|cfg| {
            convert_strategy_config(
                cfg,
                instrument_catalog.as_ref(),
                Some(&symbol_whitelist),
                &mut validation,
            )
        })
        .collect();

    let risk = convert_risk_config(shared_cfg.risk);

    let portfolios = shared_cfg
        .portfolios
        .into_iter()
        .map(|p| PortfolioSpec {
            name: p.name,
            strategies: p.strategies,
            max_notional: p.max_notional,
            max_position: p.max_position,
            max_drawdown_pct: None,
            notes: None,
        })
        .collect();

    validation.finalize()?;

    Ok(SystemConfig {
        engine,
        venues,
        strategies,
        risk,
        quotes_only: false,
        router: None,
        infra: None,
        strategy_accounts: shared_cfg.strategy_accounts,
        accounts: Vec::new(),
        portfolios,
    })
}

fn convert_venue_config(
    cfg: shared::VenueConfig,
    instrument_catalog: Option<&InstrumentCatalog>,
    venue_catalog: Option<&HashMap<VenueId, VenueMeta>>,
    auto_fill: bool,
    ctx: &mut ValidationContext,
) -> VenueConfig {
    let shared::VenueConfig {
        name,
        account_id,
        venue_type,
        mut symbol_catalog,
        capabilities: cfg_caps,
        inst_type,
        simulate_execution,
        execution_mode,
        data_config,
        execution_config,
    } = cfg;

    let mut venue_meta: Option<VenueMeta> = None;
    if let Some(cat) = instrument_catalog {
        match cat.venue(&venue_type) {
            Ok(meta) => venue_meta = Some(meta.clone()),
            Err(_) => ctx.warn(format!(
                "venue {:?} 未在 instrument catalog 中記錄，將沿用配置預設",
                venue_type
            )),
        }
    }

    if venue_meta.is_none() {
        if let Some(map) = venue_catalog {
            if let Some(meta) = map.get(&venue_type) {
                venue_meta = Some(meta.clone());
            } else {
                ctx.warn(format!(
                    "venue {:?} 未在 venue catalog 中記錄，維持基本配置",
                    venue_type
                ));
            }
        }
    }

    if symbol_catalog.is_empty() {
        if auto_fill {
            if let Some(cat) = instrument_catalog {
                let fallback = cat.instrument_ids_for_venue(&venue_type);
                if fallback.is_empty() {
                    ctx.warn(format!(
                        "symbol_catalog 缺失且 Instrument catalog 中沒有 {:?} 的商品，請補充",
                        venue_type
                    ));
                } else {
                    info!(
                        "symbol_catalog 未設定，從 Instrument catalog 為 {:?} 自動載入 {} 個商品",
                        venue_type,
                        fallback.len()
                    );
                    symbol_catalog = fallback;
                }
            } else {
                ctx.warn(format!(
                    "symbol_catalog 未設定，且缺少 Instrument catalog 以供推導: {:?}",
                    venue_type
                ));
            }
        } else {
            ctx.warn(format!(
                "symbol_catalog 未設定，且未啟用自動補齊 (HFT_AUTOFILL_SYMBOLS=1) - venue {:?}",
                venue_type
            ));
        }
    }

    if let Some(cat) = instrument_catalog {
        for instrument_id in &symbol_catalog {
            if cat.instrument(instrument_id).is_err() {
                ctx.warn(format!(
                    "instrument {} 未在 instrument catalog 中定義",
                    instrument_id.0
                ));
            }
        }
    }

    let capabilities = venue_meta
        .as_ref()
        .map(|meta| &meta.capabilities)
        .cloned()
        .unwrap_or_default();

    let combined_caps = VenueCapabilities {
        ws_order_placement: cfg_caps.supports_private_ws || capabilities.supports_private_ws,
        snapshot_crc: false,
        all_in_one_topics: cfg_caps.supports_incremental_book
            || capabilities.supports_incremental_book,
        private_ws_heartbeat: false,
        use_incremental_books: cfg_caps.supports_incremental_book
            || capabilities.supports_incremental_book,
    };

    VenueConfig {
        name,
        account_id,
        venue_type: map_venue_type(venue_type),
        ws_public: venue_meta
            .as_ref()
            .and_then(|meta| meta.ws_public_endpoint.clone()),
        ws_private: venue_meta
            .as_ref()
            .and_then(|meta| meta.ws_private_endpoint.clone()),
        rest: venue_meta
            .as_ref()
            .and_then(|meta| meta.rest_endpoint.clone()),
        api_key: None,
        secret: None,
        passphrase: None,
        secret_ref_api_key: None,
        secret_ref_secret: None,
        secret_ref_passphrase: None,
        execution_mode,
        capabilities: combined_caps,
        inst_type,
        simulate_execution,
        symbol_catalog,
        data_config,
        execution_config,
    }
}

fn map_strategy_type(strategy_type: shared::StrategyType) -> StrategyType {
    match strategy_type {
        shared::StrategyType::Trend => StrategyType::Trend,
        shared::StrategyType::Arbitrage => StrategyType::Arbitrage,
        shared::StrategyType::MarketMaking => StrategyType::MarketMaking,
        shared::StrategyType::Dl => StrategyType::Dl,
        shared::StrategyType::Imbalance => StrategyType::Imbalance,
        shared::StrategyType::LobFlowGrid => StrategyType::LobFlowGrid,
    }
}
fn convert_strategy_config(
    cfg: shared::StrategyConfig,
    catalog: Option<&InstrumentCatalog>,
    venue_symbols: Option<&std::collections::HashSet<Symbol>>,
    ctx: &mut ValidationContext,
) -> StrategyConfig {
    let name = cfg.name;
    let strategy_type = cfg.strategy_type;
    let symbols = cfg.symbols;
    let params = cfg.params;
    let risk_limits_cfg = cfg.risk_limits;

    if let Some(cat) = catalog {
        for sym in &symbols {
            if !cat.has_symbol(sym) {
                ctx.warn(format!(
                    "策略 {} 參考的商品 {} 未在 instrument catalog 中定義",
                    name,
                    sym.as_str()
                ));
            }
        }
    }

    if let Some(whitelist) = venue_symbols {
        for sym in &symbols {
            if !whitelist.contains(sym) {
                ctx.warn(format!(
                    "策略 {} 使用的商品 {} 未出現在任何 venue 的 symbol_catalog 中",
                    name,
                    sym.as_str()
                ));
            }
        }
    }

    let risk_limits = risk_limits_cfg
        .clone()
        .map(|limits| StrategyRiskLimits {
            max_notional: limits.max_notional.unwrap_or(Decimal::ZERO),
            max_position: limits.max_position.unwrap_or(Decimal::ZERO),
            daily_loss_limit: limits.daily_loss_limit.unwrap_or(Decimal::ZERO),
            cooldown_ms: limits.cooldown_ms.unwrap_or(0),
        })
        .unwrap_or_else(|| StrategyRiskLimits {
            max_notional: Decimal::ZERO,
            max_position: Decimal::ZERO,
            daily_loss_limit: Decimal::ZERO,
            cooldown_ms: 0,
        });

    let params = match convert_strategy_params(&strategy_type, params) {
        Ok(p) => p,
        Err(msg) => {
            ctx.warn(format!("策略 {} 參數配置錯誤: {}", name, msg));
            StrategyParams::Trend {
                ema_fast: 12,
                ema_slow: 26,
                rsi_period: 14,
            }
        }
    };
    StrategyConfig {
        name,
        strategy_type: map_strategy_type(strategy_type),
        symbols,
        params,
        risk_limits,
    }
}

pub(super) fn convert_strategy_params(
    strategy_type: &shared::StrategyType,
    params: shared::StrategyParams,
) -> Result<StrategyParams, String> {
    match (strategy_type, params) {
        (
            shared::StrategyType::Trend,
            shared::StrategyParams::Trend {
                ema_fast,
                ema_slow,
                rsi_period,
            },
        ) => Ok(StrategyParams::Trend {
            ema_fast,
            ema_slow,
            rsi_period,
        }),
        (
            shared::StrategyType::Imbalance,
            shared::StrategyParams::Imbalance {
                obi_threshold,
                lot,
                top_levels,
            },
        ) => Ok(StrategyParams::Imbalance {
            obi_threshold,
            lot,
            top_levels,
        }),
        (
            shared::StrategyType::Dl,
            shared::StrategyParams::Dl {
                model_path,
                device,
                top_n,
                queue_capacity,
            },
        ) => Ok(StrategyParams::Dl {
            model_path,
            device,
            top_n,
            window_size: None,
            trigger_threshold: 0.0,
            output_threshold: 0.0,
            queue_capacity,
            timeout_ms: 0,
            max_error_rate: 0.0,
            degradation_mode: "disabled".to_string(),
        }),
        (shared::StrategyType::LobFlowGrid, shared::StrategyParams::LobFlowGrid { config }) => {
            let lob_params: LobFlowGridParams = serde_yaml::to_value(config)
                .ok()
                .and_then(|value| serde_yaml::from_value(value).ok())
                .unwrap_or_default();
            Ok(StrategyParams::LobFlowGrid {
                config: Box::new(lob_params),
            })
        }
        (shared::StrategyType::Arbitrage, _) => Ok(StrategyParams::Arbitrage {
            min_spread_bps: Decimal::ZERO,
            max_position: Decimal::ZERO,
            execution_timeout_ms: 0,
        }),
        (shared::StrategyType::MarketMaking, _) => Ok(StrategyParams::MarketMaking {
            spread_bps: Decimal::ZERO,
            max_inventory: Decimal::ZERO,
            skew_factor: Decimal::ZERO,
        }),
        (_, _) => Err(format!(
            "strategy_type {:?} 與提供的參數不匹配",
            strategy_type
        )),
    }
}

fn convert_risk_config(risk: shared::RiskConfig) -> RiskConfig {
    let enhanced = risk
        .enhanced
        .and_then(|value| serde_yaml::from_value(value).ok());

    let overrides: HashMap<String, StrategyRiskOverride> = risk
        .strategy_overrides
        .into_iter()
        .filter_map(|(name, value)| serde_yaml::from_value(value).ok().map(|v| (name, v)))
        .collect();

    RiskConfig {
        risk_type: risk.risk_type,
        global_position_limit: risk.global_position_limit,
        global_notional_limit: risk.global_notional_limit,
        max_daily_trades: risk.max_daily_trades,
        max_orders_per_second: risk.max_orders_per_second,
        staleness_threshold_us: risk.staleness_threshold_us,
        max_daily_loss: risk.max_daily_loss,
        max_drawdown_pct: risk.max_drawdown_pct,
        enhanced,
        strategy_overrides: overrides,
        tokenized_securities: TokenizedSecuritiesRiskConfig {
            allow_trading: risk.tokenized_securities.allow_trading,
            max_notional_per_symbol: risk.tokenized_securities.max_notional_per_symbol,
            max_asset_class_notional: risk.tokenized_securities.max_asset_class_notional,
            min_top_depth_usd: risk.tokenized_securities.min_top_depth_usd,
            max_spread_bps: risk.tokenized_securities.max_spread_bps,
            freeze_on_corporate_action: risk.tokenized_securities.freeze_on_corporate_action,
            restricted_jurisdictions: risk.tokenized_securities.restricted_jurisdictions,
        },
    }
}

fn map_venue_type(venue_id: VenueId) -> VenueType {
    match venue_id {
        VenueId::BINANCE => VenueType::Binance,
        VenueId::BINANCE_PREDICTION => VenueType::BinancePrediction,
        VenueId::BITGET => VenueType::Bitget,
        VenueId::BYBIT => VenueType::Bybit,
        VenueId::OKX => VenueType::Okx,
        VenueId::HYPERLIQUID => VenueType::Hyperliquid,
        VenueId::GRVT => VenueType::Grvt,
        VenueId::ASTERDEX => VenueType::Asterdex,
        VenueId::BACKPACK => VenueType::Backpack,
        VenueId::ONDO_PERPS => VenueType::OndoPerps,
        VenueId::POLYMARKET => VenueType::Polymarket,
        VenueId::MOCK => VenueType::Mock,
        _ => VenueType::Mock,
    }
}

fn normalize_accounts(config: &mut SystemConfig) {
    if config.venues.is_empty() && !config.accounts.is_empty() {
        let mut venues = Vec::new();
        for acc in &config.accounts {
            venues.push(VenueConfig {
                name: acc.id.clone(),
                account_id: Some(acc.id.clone()),
                venue_type: acc.venue_type.clone(),
                ws_public: acc.ws_public.clone(),
                ws_private: acc.ws_private.clone(),
                rest: acc.rest.clone(),
                api_key: acc.credentials.as_ref().and_then(|c| c.api_key.clone()),
                secret: acc.credentials.as_ref().and_then(|c| c.secret.clone()),
                passphrase: acc.credentials.as_ref().and_then(|c| c.passphrase.clone()),
                secret_ref_api_key: None,
                secret_ref_secret: None,
                secret_ref_passphrase: None,
                execution_mode: acc.execution_mode.clone(),
                capabilities: acc
                    .capabilities
                    .clone()
                    .unwrap_or_else(VenueCapabilities::default),
                inst_type: acc.inst_type.clone(),
                simulate_execution: false,
                symbol_catalog: Vec::new(),
                data_config: None,
                execution_config: None,
            });
        }
        config.venues = venues;
    }
}

/// 判斷給定的環境變量名是否是秘密管理器參考（不應該展開）
/// 秘密參考格式：「secret:venue::field」或「secret_ref:venue::field」
/// 例如：「secret:bitget::api_key」表示在秘密管理器中查找「bitget::api_key」
fn is_credential_reference(var_name: &str) -> bool {
    var_name.starts_with("secret:") || var_name.starts_with("secret_ref:")
}

fn expand_env_vars(content: &str) -> Result<String, Box<dyn std::error::Error>> {
    let re = regex::Regex::new(r"\$\{([^}]+)\}")?;
    let mut result = content.to_string();
    let mut replacements = HashMap::new();

    for cap in re.captures_iter(content) {
        let full_match = &cap[0];
        let var_name = &cap[1];
        if replacements.contains_key(full_match) {
            continue;
        }

        // 跳過秘密管理器參考 - 不在此處展開，而是在模運行時由 SecretsManager 解決
        if is_credential_reference(var_name) {
            info!(
                "跳過秘密參考擴展（稍後由 SecretsManager 解決）: {}",
                var_name
            );
            // 保留原始格式，不展開
            replacements.insert(full_match.to_string(), full_match.to_string());
            continue;
        }

        match std::env::var(var_name) {
            Ok(value) => {
                replacements.insert(full_match.to_string(), value);
            }
            Err(_) => {
                warn!("環境變量 {} 未設置，保留原始格式", var_name);
                replacements.insert(full_match.to_string(), full_match.to_string());
            }
        }
    }

    for (pattern, value) in replacements {
        result = result.replace(&pattern, &value);
    }

    Ok(result)
}

fn expand_templates_into_strategies(
    root: &YamlValue,
) -> Result<Option<Vec<StrategyConfig>>, Box<dyn std::error::Error>> {
    let mut group_map: HashMap<String, Vec<Symbol>> = HashMap::new();
    if let Some(instruments) = root.get("instruments") {
        if let Ok(sec) = serde_yaml::from_value::<InstrumentsSection>(instruments.clone()) {
            for g in sec.groups {
                if g.selector.is_some() {
                    warn!(
                        "instrument group '{}' 使用 selector 暫未實作，請使用 symbols 顯式列出",
                        g.name
                    );
                }
                group_map.insert(g.name, g.symbols);
            }
        }
    }

    let Some(node) = root.get("strategies") else {
        return Ok(None);
    };
    if node.is_sequence() {
        return Ok(None);
    }

    let sec: StrategiesSection = match serde_yaml::from_value(node.clone()) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    if sec.templates.is_empty() || sec.bindings.is_empty() {
        return Ok(None);
    }

    let tpl_map: HashMap<String, StrategyTemplate> = sec
        .templates
        .into_iter()
        .map(|t| (t.id.clone(), t))
        .collect();
    let mut out: Vec<StrategyConfig> = Vec::new();

    for binding in sec.bindings {
        let Some(tpl) = tpl_map.get(&binding.template) else {
            warn!("找不到策略模板: {}，跳過綁定", binding.template);
            continue;
        };

        let mut symbols = Vec::new();
        for target in &binding.apply_to {
            if let Some(rest) = target.strip_prefix("group:") {
                if let Some(gs) = group_map.get(rest) {
                    symbols.extend(gs.clone());
                } else {
                    warn!("未定義的商品組: {}，跳過", rest);
                }
            } else if let Some(sym) = target.strip_prefix("symbol:") {
                symbols.push(Symbol::new(sym));
            } else {
                warn!(
                    "未知的 apply_to 項: {}，應為 group:<name> 或 symbol:<SYM>",
                    target
                );
            }
        }

        for sym in symbols {
            let risk = binding
                .overrides
                .get(sym.as_str())
                .and_then(|o| o.risk.clone())
                .unwrap_or_else(|| tpl.risk.clone());
            out.push(StrategyConfig {
                name: format!("{}:{}", tpl.id, sym.as_str()),
                strategy_type: tpl.strategy_type.clone(),
                symbols: vec![sym],
                params: tpl.params.clone(),
                risk_limits: risk,
            });
        }
    }

    Ok(Some(out))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct InstrumentsSection {
    pub groups: Vec<InstrumentGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstrumentGroup {
    pub name: String,
    #[serde(default)]
    pub symbols: Vec<Symbol>,
    #[serde(default)]
    pub selector: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StrategiesSection {
    pub templates: Vec<StrategyTemplate>,
    pub bindings: Vec<StrategyBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StrategyTemplate {
    pub id: String,
    #[serde(rename = "type")]
    pub strategy_type: StrategyType,
    pub params: StrategyParams,
    #[serde(default)]
    pub risk: StrategyRiskLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StrategyBinding {
    pub template: String,
    #[serde(default)]
    pub apply_to: Vec<String>,
    #[serde(default)]
    pub overrides: HashMap<String, StrategyOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StrategyOverride {
    #[serde(default)]
    pub params: Option<StrategyParams>,
    #[serde(default)]
    pub risk: Option<StrategyRiskLimits>,
}

fn load_instrument_catalog() -> Option<InstrumentCatalog> {
    let path: PathBuf = std::env::var("HFT_INSTRUMENT_CATALOG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config/instruments.yaml"));

    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) => {
            warn!("無法讀取 instrument catalog {:?}: {}", path, err);
            return None;
        }
    };

    match serde_yaml::from_str::<InstrumentCatalogConfig>(&content) {
        Ok(cfg) => Some(InstrumentCatalog::new(cfg)),
        Err(err) => {
            warn!("instrument catalog 解析失敗 {:?}: {}", path, err);
            None
        }
    }
}

fn load_venue_catalog() -> Option<HashMap<VenueId, VenueMeta>> {
    #[derive(Debug, Deserialize)]
    struct VenueCatalogConfig {
        #[serde(default)]
        venues: Vec<VenueMeta>,
    }

    let path: PathBuf = std::env::var("HFT_VENUE_CATALOG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config/venues.yaml"));

    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) => {
            warn!("無法讀取 venue catalog {:?}: {}", path, err);
            return None;
        }
    };

    match serde_yaml::from_str::<VenueCatalogConfig>(&content) {
        Ok(cfg) => {
            let map = cfg
                .venues
                .into_iter()
                .map(|meta| (meta.venue_id, meta))
                .collect();
            Some(map)
        }
        Err(err) => {
            warn!("venue catalog 解析失敗 {:?}: {}", path, err);
            None
        }
    }
}

fn is_strict_mode() -> bool {
    match std::env::var("HFT_CONFIG_STRICT") {
        Ok(val) => matches!(val.as_str(), "1" | "true" | "TRUE"),
        Err(_) => false,
    }
}

fn allow_autofill_symbols() -> bool {
    match std::env::var("HFT_AUTOFILL_SYMBOLS") {
        Ok(val) => matches!(val.as_str(), "1" | "true" | "TRUE"),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;
    use tempfile::tempdir;

    // 初始化 tracing subscriber，避免測試卡住
    #[allow(dead_code)]
    fn init_test_tracing() {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_max_level(tracing::Level::WARN)
            .try_init();
    }

    struct EnvGuard {
        key: &'static str,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            std::env::set_var(key, value);
            Self { key }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.key);
        }
    }

    #[test]
    #[serial]
    fn load_v2_config_uses_instrument_catalog() {
        let dir = tempdir().expect("temp dir");
        let catalog_path = dir.path().join("catalog.yaml");
        let catalog_yaml = r#"
venues:
  - venue_id: BINANCE
    name: Binance
    rest_endpoint: https://api.binance.com
    ws_public_endpoint: wss://stream.binance.com:9443/ws
    ws_private_endpoint: wss://stream.binance.com:9443/stream
    capabilities:
      supports_incremental_book: true
      supports_private_ws: true
      allows_post_only: true
instruments:
  - symbol: BTCUSDT
    venue: BINANCE
    base: BTC
    quote: USDT
    tick_size: 0.1
    lot_size: 0.001
"#;
        fs::write(&catalog_path, catalog_yaml).expect("write catalog");
        let _guard = EnvGuard::set(
            "HFT_INSTRUMENT_CATALOG",
            catalog_path.to_str().expect("utf8 path"),
        );

        let system_yaml = r#"
schema_version: v2
engine:
  queue_capacity: 1024
  stale_us: 5000
  top_n: 10
  ack_timeout_ms: 3000
  reconcile_interval_ms: 5000
  auto_cancel_exchange_only: false

venues:
  - name: binance
    venue_type: BINANCE
    symbol_catalog:
      - BTCUSDT@BINANCE
    capabilities:
      supports_incremental_book: false
      supports_private_ws: false
      allows_post_only: false
    simulate_execution: false

strategies:
  - name: trend_btc
    strategy_type: Trend
    symbols: [BTCUSDT]
    params:
      kind: trend
      ema_fast: 12
      ema_slow: 26
      rsi_period: 14

risk:
  risk_type: Default
  global_position_limit: 1000
  global_notional_limit: 100000
  max_daily_trades: 100
  max_orders_per_second: 10
  staleness_threshold_us: 5000
"#;

        let shared =
            shared_config::SystemConfig::from_yaml_str(system_yaml).expect("parse shared config");
        assert_eq!(shared.schema_version.as_deref(), Some("v2"));
        let config = load_config_from_str(system_yaml).expect("load config");
        assert_eq!(config.venues.len(), 1);
        let venue = &config.venues[0];
        assert_eq!(
            venue.ws_public.as_deref(),
            Some("wss://stream.binance.com:9443/ws")
        );
        assert!(venue.capabilities.ws_order_placement);
        assert!(venue.capabilities.use_incremental_books);
        assert_eq!(venue.symbol_catalog.len(), 1);
        assert_eq!(
            venue.symbol_catalog[0]
                .symbol()
                .expect("catalog symbol")
                .as_str(),
            "BTCUSDT"
        );
        assert_eq!(config.strategies.len(), 1);
        assert_eq!(config.strategies[0].symbols[0].as_str(), "BTCUSDT");
    }

    #[test]
    #[serial]
    fn load_v2_config_uses_venue_catalog_when_instrument_catalog_missing() {
        let dir = tempdir().expect("temp dir");
        let venue_catalog_path = dir.path().join("venues.yaml");
        let venue_yaml = r#"
venues:
  - venue_id: BINANCE
    name: Binance
    rest_endpoint: https://api.binance.com
    ws_public_endpoint: wss://stream.binance.com:9443/ws
    ws_private_endpoint: wss://stream.binance.com:9443/stream
    capabilities:
      supports_incremental_book: true
      supports_private_ws: true
      allows_post_only: true
"#;
        fs::write(&venue_catalog_path, venue_yaml).expect("write venues");
        let _guard = EnvGuard::set(
            "HFT_VENUE_CATALOG",
            venue_catalog_path.to_str().expect("utf8 path"),
        );

        let system_yaml = r#"
schema_version: v2
engine:
  queue_capacity: 1024
  stale_us: 5000
  top_n: 10

venues:
  - name: binance
    venue_type: BINANCE
    symbol_catalog:
      - BTCUSDT@BINANCE
    capabilities:
      supports_incremental_book: false
      supports_private_ws: false
      allows_post_only: false
    simulate_execution: false

strategies:
  - name: trend_btc
    strategy_type: Trend
    symbols: [BTCUSDT]
    params:
      kind: trend
      ema_fast: 12
      ema_slow: 26
      rsi_period: 14

risk:
  risk_type: Default
  global_position_limit: 1000
  global_notional_limit: 100000
  max_daily_trades: 100
  max_orders_per_second: 10
  staleness_threshold_us: 5000
"#;

        let config = load_config_from_str(system_yaml).expect("load config");
        assert_eq!(config.venues.len(), 1);
        let venue = &config.venues[0];
        assert_eq!(venue.name, "binance");
        assert_eq!(
            venue.ws_public.as_deref(),
            Some("wss://stream.binance.com:9443/ws")
        );
        assert!(venue.capabilities.ws_order_placement);
        assert!(venue.capabilities.use_incremental_books);
    }

    #[test]
    #[serial]
    fn load_v2_config_populates_symbol_catalog_from_instrument_catalog() {
        let dir = tempdir().expect("temp dir");
        let instrument_catalog_path = dir.path().join("inst.yaml");
        let catalog_yaml = r#"
venues:
  - venue_id: BINANCE
    name: Binance
instruments:
  - symbol: BTCUSDT
    venue: BINANCE
    base: BTC
    quote: USDT
    tick_size: 0.1
    lot_size: 0.001
  - symbol: ETHUSDT
    venue: BINANCE
    base: ETH
    quote: USDT
    tick_size: 0.01
    lot_size: 0.001
"#;
        fs::write(&instrument_catalog_path, catalog_yaml).expect("write instrument catalog");
        let _inst_guard = EnvGuard::set(
            "HFT_INSTRUMENT_CATALOG",
            instrument_catalog_path.to_str().expect("utf8"),
        );
        let _autofill_guard = EnvGuard::set("HFT_AUTOFILL_SYMBOLS", "1");

        let system_yaml = r#"
schema_version: v2
engine:
  queue_capacity: 1024
  stale_us: 5000
  top_n: 10

venues:
  - name: binance
    venue_type: BINANCE
    capabilities:
      supports_incremental_book: true

strategies:
  - name: trend_btc
    strategy_type: Trend
    symbols: [BTCUSDT]
    params:
      kind: trend
      ema_fast: 12
      ema_slow: 26
      rsi_period: 14

risk:
  risk_type: Default
  global_position_limit: 1000
  global_notional_limit: 100000
  max_daily_trades: 100
  max_orders_per_second: 10
  staleness_threshold_us: 5000
"#;

        let config = load_config_from_str(system_yaml).expect("load config");
        assert_eq!(config.venues.len(), 1);
        let venue = &config.venues[0];
        assert_eq!(venue.symbol_catalog.len(), 2);
        let ids: Vec<_> = venue
            .symbol_catalog
            .iter()
            .map(|id| id.symbol().unwrap().as_str().to_string())
            .collect();
        assert!(ids.contains(&"BTCUSDT".to_string()));
        assert!(ids.contains(&"ETHUSDT".to_string()));
    }

    #[test]
    #[serial]
    fn strict_mode_reports_unknown_strategy_symbol() {
        let dir = tempdir().expect("temp dir");
        let instrument_catalog_path = dir.path().join("inst.yaml");
        let catalog_yaml = r#"
venues:
  - venue_id: BINANCE
    name: Binance
instruments:
  - symbol: BTCUSDT
    venue: BINANCE
    base: BTC
    quote: USDT
    tick_size: 0.1
    lot_size: 0.001
"#;
        fs::write(&instrument_catalog_path, catalog_yaml).expect("write instrument catalog");
        let _inst_guard = EnvGuard::set(
            "HFT_INSTRUMENT_CATALOG",
            instrument_catalog_path.to_str().expect("utf8"),
        );
        let _strict_guard = EnvGuard::set("HFT_CONFIG_STRICT", "1");

        let system_yaml = r#"
schema_version: v2
engine:
  queue_capacity: 1024
  stale_us: 5000
  top_n: 10

venues:
  - name: binance
    venue_type: BINANCE
    symbol_catalog:
      - BTCUSDT@BINANCE

strategies:
  - name: trend_eth
    strategy_type: Trend
    symbols: [ETHUSDT]
    params:
      kind: trend
      ema_fast: 12
      ema_slow: 26
      rsi_period: 14

risk:
  risk_type: Default
  global_position_limit: 1000
  global_notional_limit: 100000
  max_daily_trades: 100
  max_orders_per_second: 10
  staleness_threshold_us: 5000
"#;

        let result = load_config_from_str(system_yaml);
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn load_bstocks_quotes_only_config_preserves_tokenized_risk_boundary() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/dev/binance_bstocks_quotes_only.yaml"
        );

        let config = load_config_from_yaml(path).expect("load bstocks config");

        assert!(config.quotes_only);
        assert_eq!(config.venues.len(), 1);
        let venue = &config.venues[0];
        assert_eq!(venue.name, "binance-bstocks");
        assert_eq!(venue.venue_type, VenueType::Binance);
        assert!(venue
            .symbol_catalog
            .iter()
            .all(|id| id.venue_id() == Some(VenueId::BINANCE_TOKENIZED_SECURITIES)));
        assert!(!config.risk.tokenized_securities.allow_trading);
        assert!(config.strategies.is_empty());
    }

    #[test]
    fn load_ondo_perps_quotes_only_config_preserves_restricted_risk_boundary() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/dev/ondo_perps_quotes_only.yaml"
        );
        let config = load_config_from_yaml(path).expect("load Ondo Perps config");

        assert!(config.quotes_only);
        assert_eq!(config.venues[0].venue_type, VenueType::OndoPerps);
        assert!(config.venues[0]
            .symbol_catalog
            .iter()
            .all(|id| id.venue_id() == Some(VenueId::ONDO_PERPS)));
        assert!(!config.risk.tokenized_securities.allow_trading);
    }

    #[test]
    fn negative_balance_reconciliation_tolerance_is_rejected() {
        let mut config = SystemConfig::default();
        config.engine.balance_reconcile_tolerance_usd = Decimal::NEGATIVE_ONE;
        let yaml = serde_yaml::to_string(&config).expect("serialize config");

        let error = load_config_from_str(&yaml).expect_err("negative tolerance");
        assert!(error
            .to_string()
            .contains("balance_reconcile_tolerance_usd"));
    }

    #[test]
    fn invalid_intent_execution_limits_are_rejected() {
        let mut config = SystemConfig::default();
        config.engine.intent_max_slippage_bps = Some(0);
        let yaml = serde_yaml::to_string(&config).expect("serialize config");
        let error = load_config_from_str(&yaml).expect_err("invalid slippage limit");
        assert!(error.to_string().contains("intent_max_slippage_bps"));

        let mut config = SystemConfig::default();
        config.engine.intent_max_order_notional = Some(Decimal::ZERO);
        let yaml = serde_yaml::to_string(&config).expect("serialize config");
        let error = load_config_from_str(&yaml).expect_err("invalid order-notional limit");
        assert!(error.to_string().contains("intent_max_order_notional"));
    }

    #[test]
    fn unsafe_worker_only_exchange_order_auto_cancel_is_rejected() {
        let mut config = SystemConfig::default();
        config.engine.auto_cancel_exchange_only = true;
        let yaml = serde_yaml::to_string(&config).expect("serialize config");

        let error = load_config_from_str(&yaml).expect_err("unsupported auto-cancel");
        assert!(error.to_string().contains("auto_cancel_exchange_only"));
        assert!(error.to_string().contains("deprecated and unsupported"));
    }

    #[test]
    fn loads_binance_prediction_as_an_execution_only_venue() {
        let config = load_config_from_str(
            r#"
engine:
  queue_capacity: 1024
  stale_us: 1000000
  top_n: 20
venues:
  - name: binance-prediction
    venue_type: BinancePrediction
    rest: https://api.binance.com
    api_key: test-key
    secret: test-secret
    execution_mode: Paper
    capabilities:
      ws_order_placement: false
      snapshot_crc: false
      all_in_one_topics: false
      private_ws_heartbeat: false
    execution_config:
      wallet_address: "0x1234"
      wallet_id: wallet-1
      account_type: SPOT
      funding_source: CEX
risk:
  risk_type: Default
  global_position_limit: 0
  global_notional_limit: 0
  max_daily_trades: 0
  max_orders_per_second: 0
  staleness_threshold_us: 0
strategies: []
"#,
        )
        .expect("load Binance Prediction config");

        assert_eq!(config.venues[0].venue_type, VenueType::BinancePrediction);
        assert_eq!(
            config.venues[0]
                .execution_config
                .as_ref()
                .and_then(|value| value.get("wallet_id"))
                .and_then(serde_yaml::Value::as_str),
            Some("wallet-1")
        );
    }

    #[test]
    fn binance_prediction_live_example_stays_loadable() {
        let content = include_str!("../../../../config/dev/binance_prediction_live.yaml.example")
            .replace("${BINANCE_PREDICTION_API_KEY}", "test-key")
            .replace("${BINANCE_PREDICTION_API_SECRET}", "test-secret")
            .replace("${BINANCE_PREDICTION_WALLET_ADDRESS}", "0x1234")
            .replace("${BINANCE_PREDICTION_WALLET_ID}", "wallet-1");

        let config = load_config_from_str(&content).expect("load Binance Prediction live example");

        assert_eq!(config.venues[0].venue_type, VenueType::BinancePrediction);
        assert_eq!(config.venues[0].execution_mode.as_deref(), Some("Live"));
        assert!(config.strategies.is_empty());
        assert_eq!(config.risk.global_notional_limit, Decimal::ZERO);
    }

    #[test]
    fn polymarket_examples_keep_token_identity_and_live_fail_closed() {
        let quotes = include_str!("../../../../config/dev/polymarket_quotes_only.yaml.example")
            .replace("${POLYMARKET_TOKEN_ID}", "123456789");
        let quotes = load_config_from_str(&quotes).expect("load Polymarket quotes example");
        assert!(quotes.quotes_only);
        assert_eq!(quotes.venues[0].venue_type, VenueType::Polymarket);
        assert_eq!(
            quotes.venues[0].symbol_catalog[0].venue_id(),
            Some(VenueId::POLYMARKET)
        );

        let live = include_str!("../../../../config/dev/polymarket_live.yaml.example")
            .replace("${POLYMARKET_TOKEN_ID}", "123456789")
            .replace("${POLYMARKET_PRIVATE_KEY}", "not-a-real-private-key")
            .replace("${POLYMARKET_SIGNATURE_TYPE}", "eoa");
        let live = load_config_from_str(&live).expect("load Polymarket live example");
        assert!(!live.quotes_only);
        assert_eq!(live.venues[0].venue_type, VenueType::Polymarket);
        assert_eq!(live.venues[0].execution_mode.as_deref(), Some("Live"));
        assert!(live.strategies.is_empty());
        assert_eq!(live.risk.global_notional_limit, Decimal::ZERO);
    }

    #[test]
    fn polymarket_live_requires_explicit_signature_type() {
        let live = include_str!("../../../../config/dev/polymarket_live.yaml.example")
            .replace("${POLYMARKET_TOKEN_ID}", "123456789")
            .replace("${POLYMARKET_PRIVATE_KEY}", "not-a-real-private-key")
            .replace(
                "      signature_type: \"${POLYMARKET_SIGNATURE_TYPE}\"\n",
                "",
            );

        let error = load_config_from_str(&live).expect_err("missing signature_type");

        assert!(error.to_string().contains("signature_type"));
    }

    #[test]
    fn unknown_execution_mode_is_rejected() {
        let live = include_str!("../../../../config/dev/polymarket_live.yaml.example")
            .replace("${POLYMARKET_TOKEN_ID}", "123456789")
            .replace("${POLYMARKET_PRIVATE_KEY}", "not-a-real-private-key")
            .replace("${POLYMARKET_SIGNATURE_TYPE}", "eoa")
            .replace("execution_mode: Live", "execution_mode: Lvie");

        let error = load_config_from_str(&live).expect_err("unknown execution_mode");

        assert!(error.to_string().contains("execution_mode"));
        assert!(error.to_string().contains("Lvie"));
    }

    #[test]
    fn execution_mode_is_case_insensitive() {
        let live = include_str!("../../../../config/dev/polymarket_live.yaml.example")
            .replace("${POLYMARKET_TOKEN_ID}", "123456789")
            .replace("${POLYMARKET_PRIVATE_KEY}", "not-a-real-private-key")
            .replace("${POLYMARKET_SIGNATURE_TYPE}", "eoa")
            .replace("execution_mode: Live", "execution_mode: lIvE");

        let config = load_config_from_str(&live).expect("case-insensitive execution_mode");

        assert_eq!(config.venues[0].execution_mode.as_deref(), Some("Live"));
    }

    #[test]
    fn testnet_execution_mode_remains_available_for_supported_venues() {
        let config = load_config_from_str(
            r#"
engine: {}
venues:
  - name: binance-test
    venue_type: Binance
    execution_mode: tEsTnEt
    capabilities:
      ws_order_placement: false
      snapshot_crc: false
      all_in_one_topics: false
      private_ws_heartbeat: false
  - name: binance-prediction-test
    venue_type: BinancePrediction
    execution_mode: tEsTnEt
    capabilities:
      ws_order_placement: false
      snapshot_crc: false
      all_in_one_topics: false
      private_ws_heartbeat: false
  - name: bybit-test
    venue_type: Bybit
    execution_mode: tEsTnEt
    capabilities:
      ws_order_placement: false
      snapshot_crc: false
      all_in_one_topics: false
      private_ws_heartbeat: false
risk:
  risk_type: Default
  global_position_limit: 0
  global_notional_limit: 0
  max_daily_trades: 0
  max_orders_per_second: 0
  staleness_threshold_us: 0
strategies: []
"#,
        )
        .expect("supported Testnet mode");

        assert_eq!(config.venues.len(), 3);
        assert!(config
            .venues
            .iter()
            .all(|venue| venue.execution_mode.as_deref() == Some("Testnet")));
    }

    #[test]
    fn account_driven_config_preserves_testnet_execution_mode() {
        let config: YamlValue =
            serde_yaml::from_str(include_str!("../../../../config/dev/system_accounts.yaml"))
                .expect("parse account-driven config");
        let bybit = config["accounts"]
            .as_sequence()
            .expect("accounts list")
            .iter()
            .find(|account| account["id"].as_str() == Some("bybit_test"))
            .expect("Bybit account");

        assert_eq!(bybit["venue_type"].as_str(), Some("Bybit"));
        assert_eq!(bybit["execution_mode"].as_str(), Some("Testnet"));
    }

    #[test]
    fn polymarket_testnet_execution_mode_is_rejected() {
        let live = include_str!("../../../../config/dev/polymarket_live.yaml.example")
            .replace("${POLYMARKET_TOKEN_ID}", "123456789")
            .replace("${POLYMARKET_PRIVATE_KEY}", "not-a-real-private-key")
            .replace("${POLYMARKET_SIGNATURE_TYPE}", "eoa")
            .replace("execution_mode: Live", "execution_mode: Testnet");

        let error = load_config_from_str(&live).expect_err("Polymarket Testnet");

        assert!(error
            .to_string()
            .contains("Polymarket does not support Testnet"));
    }

    #[test]
    fn polymarket_live_cannot_skip_reconciliation_via_simulation_flag() {
        let live = include_str!("../../../../config/dev/polymarket_live.yaml.example")
            .replace("${POLYMARKET_TOKEN_ID}", "123456789")
            .replace("${POLYMARKET_PRIVATE_KEY}", "not-a-real-private-key")
            .replace("${POLYMARKET_SIGNATURE_TYPE}", "eoa")
            .replace("simulate_execution: false", "simulate_execution: true");

        let error = load_config_from_str(&live).expect_err("ambiguous Live simulation flag");

        assert!(error.to_string().contains("simulate_execution=true"));
    }

    #[test]
    fn polymarket_live_rejects_unexpanded_signature_type() {
        let live = include_str!("../../../../config/dev/polymarket_live.yaml.example")
            .replace("${POLYMARKET_TOKEN_ID}", "123456789")
            .replace("${POLYMARKET_PRIVATE_KEY}", "not-a-real-private-key")
            .replace(
                "${POLYMARKET_SIGNATURE_TYPE}",
                "${secret:polymarket::signature_type}",
            );

        let error = load_config_from_str(&live).expect_err("unexpanded signature_type");

        assert!(error.to_string().contains("signature_type"));
    }
}
