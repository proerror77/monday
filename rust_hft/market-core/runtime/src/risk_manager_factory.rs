//! Risk Manager Factory - Creates configurable risk managers with per-strategy overrides
//!
//! This module provides a factory for creating risk managers based on configuration,
//! including support for per-strategy risk overrides.

use chrono::{DateTime, Utc, Weekday};
use hft_core::{AssetClass, ProductType, Quantity, VenueId};
use ports::RiskManager;
use risk::{
    DefaultRiskManager, EnhancedRiskConfig, EnhancedRiskManager, RiskConfig, TradingWindow,
};
use std::collections::HashMap;
use tracing::{debug, info, warn};

use crate::exposure_projection::ExposureProjector;
use crate::{
    RiskConfig as SystemRiskConfig, StrategyRiskOverride, TokenizedSecuritiesRiskConfig,
    TradingWindowConfig,
};

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
                    max_position_per_symbol: Quantity(system_risk_config.global_position_limit),
                    max_global_notional: system_risk_config.global_notional_limit,
                    max_orders_per_second: system_risk_config.max_orders_per_second,
                    order_cooldown_ms: 100,
                    staleness_threshold_us: system_risk_config.staleness_threshold_us,
                    max_daily_loss: system_risk_config.max_daily_loss,
                    max_drawdown_pct: system_risk_config.max_drawdown_pct,
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
        let max_position = system_risk_config
            .enhanced
            .as_ref()
            .map(|config| config.max_position_per_symbol)
            .filter(|limit| *limit > rust_decimal::Decimal::ZERO)
            .unwrap_or(system_risk_config.global_position_limit);
        let base_risk_manager: Box<dyn RiskManager> = Box::new(ProjectedExposureRiskManager::new(
            base_risk_manager,
            max_position,
            system_risk_config.global_notional_limit,
        ));

        let strategy_aware = if system_risk_config.strategy_overrides.is_empty() {
            // No overrides, return base manager
            base_risk_manager
        } else {
            // Wrap with strategy override manager
            Box::new(StrategyAwareRiskManager::new(
                base_risk_manager,
                system_risk_config.strategy_overrides.clone(),
            ))
        };

        Box::new(TokenizedSecuritiesRiskManager::new(
            strategy_aware,
            system_risk_config.tokenized_securities.clone(),
        ))
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

/// Enforces batch exposure on venue + product + symbol identities before legacy account views can
/// net same-named instruments across venues.
pub struct ProjectedExposureRiskManager {
    base_risk_manager: Box<dyn RiskManager>,
    max_position_per_symbol: rust_decimal::Decimal,
    max_global_notional: rust_decimal::Decimal,
}

impl ProjectedExposureRiskManager {
    fn new(
        base_risk_manager: Box<dyn RiskManager>,
        max_position_per_symbol: rust_decimal::Decimal,
        max_global_notional: rust_decimal::Decimal,
    ) -> Self {
        Self {
            base_risk_manager,
            max_position_per_symbol,
            max_global_notional,
        }
    }

    fn filter(
        &self,
        intents: Vec<ports::OrderIntent>,
        account: &ports::AccountView,
    ) -> Vec<ports::OrderIntent> {
        let mut projector = ExposureProjector::new(account);
        let mut approved = Vec::with_capacity(intents.len());
        for intent in intents {
            let mut next_projector = projector.clone();
            let Ok(projected) = next_projector.project(&intent) else {
                warn!(symbol = %intent.symbol, "全局敞口无法投影，拒绝订单意图");
                continue;
            };
            if projected.symbol_gross_quantity > self.max_position_per_symbol
                || projected.gross_notional > self.max_global_notional
            {
                warn!(symbol = %intent.symbol, "跨 venue projected exposure 超过全局限额");
                continue;
            }
            projector = next_projector;
            approved.push(intent);
        }
        approved
    }
}

impl RiskManager for ProjectedExposureRiskManager {
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

    fn on_execution_event(&mut self, event: &ports::ExecutionEvent) {
        self.base_risk_manager.on_execution_event(event)
    }

    fn emergency_stop(&mut self) -> Result<(), hft_core::HftError> {
        self.base_risk_manager.emergency_stop()
    }

    fn get_risk_metrics(&self) -> HashMap<String, rust_decimal::Decimal> {
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

/// Fail-closed policy layer for securities-like tokens.
///
/// This code-owned source identity cannot be changed through YAML while no runtime attestation
/// publisher exists.
const BINANCE_BSTOCKS_EVIDENCE_SOURCE: &str = "licensed-reference-feed";

pub struct TokenizedSecuritiesRiskManager {
    base_risk_manager: Box<dyn RiskManager>,
    config: TokenizedSecuritiesRiskConfig,
}

impl TokenizedSecuritiesRiskManager {
    pub fn new(
        base_risk_manager: Box<dyn RiskManager>,
        config: TokenizedSecuritiesRiskConfig,
    ) -> Self {
        Self {
            base_risk_manager,
            config,
        }
    }

    fn is_tokenized(intent: &ports::OrderIntent) -> bool {
        matches!(intent.asset_class, AssetClass::TokenizedSecurity)
            || matches!(intent.product_type, ProductType::TokenizedSecuritySpot)
    }

    fn filter(
        &self,
        intents: Vec<ports::OrderIntent>,
        account: &ports::AccountView,
    ) -> Vec<ports::OrderIntent> {
        let mut approved = Vec::with_capacity(intents.len());
        let mut pending_symbol_notionals = HashMap::new();
        let mut pending_asset_class_notional = rust_decimal::Decimal::ZERO;
        let mut account_asset_class_notional = None;

        for mut intent in intents {
            if !Self::is_tokenized(&intent) {
                approved.push(intent);
                continue;
            }

            let pending_symbol_notional = pending_symbol_notionals
                .get(&intent.symbol)
                .copied()
                .unwrap_or(rust_decimal::Decimal::ZERO);
            if let Some((notional, attested_asset_class_notional)) = self.admit_tokenized_intent(
                &mut intent,
                account,
                pending_symbol_notional,
                pending_asset_class_notional,
                account_asset_class_notional,
            ) {
                *pending_symbol_notionals
                    .entry(intent.symbol.clone())
                    .or_insert(rust_decimal::Decimal::ZERO) += notional;
                pending_asset_class_notional += notional;
                account_asset_class_notional = Some(attested_asset_class_notional);
                approved.push(intent);
            }
        }

        approved
    }

    fn admit_tokenized_intent(
        &self,
        intent: &mut ports::OrderIntent,
        account: &ports::AccountView,
        pending_symbol_notional: rust_decimal::Decimal,
        pending_asset_class_notional: rust_decimal::Decimal,
        expected_account_asset_class_notional: Option<rust_decimal::Decimal>,
    ) -> Option<(rust_decimal::Decimal, rust_decimal::Decimal)> {
        match self.runtime_attestation_context(
            intent,
            account,
            pending_symbol_notional,
            pending_asset_class_notional,
            expected_account_asset_class_notional,
        ) {
            Ok((context, notional, attested_asset_class_notional)) => {
                // The base RiskManager's stable admission interface still consumes a
                // ComplianceContext. Replace every strategy-supplied value only after the
                // runtime-owned attestation has been verified.
                intent.compliance_context = context;
                Some((notional, attested_asset_class_notional))
            }
            Err(reason) => {
                warn!(
                    symbol = %intent.symbol,
                    %reason,
                    "证券 token 运行时证明不足，拒绝意图"
                );
                None
            }
        }
    }

    fn runtime_attestation_context(
        &self,
        intent: &ports::OrderIntent,
        account: &ports::AccountView,
        pending_symbol_notional: rust_decimal::Decimal,
        pending_asset_class_notional: rust_decimal::Decimal,
        expected_account_asset_class_notional: Option<rust_decimal::Decimal>,
    ) -> Result<
        (
            hft_core::ComplianceContext,
            rust_decimal::Decimal,
            rust_decimal::Decimal,
        ),
        &'static str,
    > {
        use rust_decimal::Decimal;

        if !self.config.allow_trading {
            return Err("tokenized trading is disabled");
        }
        if !self.config.freeze_on_corporate_action {
            return Err("corporate-action freeze is not enabled");
        }
        if self.config.evidence_max_age_us == 0
            || self.config.max_notional_per_symbol <= Decimal::ZERO
            || self.config.max_asset_class_notional <= Decimal::ZERO
            || self.config.min_top_depth_usd <= Decimal::ZERO
            || self.config.max_spread_bps <= Decimal::ZERO
        {
            return Err("tokenized risk limits are not configured");
        }
        if intent.asset_class != AssetClass::TokenizedSecurity
            || intent.product_type != ProductType::TokenizedSecuritySpot
            || intent.target_venue != Some(VenueId::BINANCE_TOKENIZED_SECURITIES)
        {
            return Err("intent is not a Binance tokenized-security spot order");
        }

        let account_id = account
            .account_id
            .as_ref()
            .ok_or("runtime account identity is missing")?;
        let attestation_key = hft_core::InstrumentKey::tokenized_security_spot(
            intent.symbol.clone(),
            VenueId::BINANCE_TOKENIZED_SECURITIES,
        );
        let attestation = account
            .tokenized_securities_attestations
            .get(&attestation_key)
            .ok_or("runtime tokenized attestation is missing")?;
        if attestation.account_id != *account_id {
            return Err("attestation account does not match runtime account");
        }
        if attestation.venue != VenueId::BINANCE_TOKENIZED_SECURITIES
            || attestation.product_type != ProductType::TokenizedSecuritySpot
            || attestation.symbol != intent.symbol
        {
            return Err("attestation scope does not match intent");
        }
        if attestation.source_id != BINANCE_BSTOCKS_EVIDENCE_SOURCE {
            return Err("attestation source is not approved");
        }

        let now = hft_core::now_micros();
        if attestation.observed_at > now
            || now.saturating_sub(attestation.observed_at) > self.config.evidence_max_age_us
        {
            return Err("attestation is stale or future dated");
        }

        let jurisdiction = attestation
            .jurisdiction
            .as_deref()
            .map(str::trim)
            .filter(|jurisdiction| !jurisdiction.is_empty())
            .ok_or("attestation jurisdiction is unknown")?;
        if self
            .config
            .restricted_jurisdictions
            .iter()
            .any(|restricted| restricted.trim().eq_ignore_ascii_case(jurisdiction))
        {
            return Err("attestation jurisdiction is restricted");
        }
        if attestation
            .account_capability
            .jurisdiction
            .as_deref()
            .map(str::trim)
            != Some(jurisdiction)
        {
            return Err("account jurisdiction does not match attestation");
        }
        let kyc_level = attestation
            .account_capability
            .kyc_level
            .as_deref()
            .unwrap_or_default();
        if !attestation.account_eligible
            || !attestation
                .account_capability
                .can_trade_tokenized_securities
            || kyc_level.trim().is_empty()
        {
            return Err("account is not eligible for tokenized trading");
        }
        if attestation.corporate_action_active {
            return Err("corporate action is active");
        }
        if attestation.top_depth_usd < self.config.min_top_depth_usd {
            return Err("top-of-book depth is insufficient");
        }
        if attestation.spread_bps < Decimal::ZERO
            || attestation.spread_bps > self.config.max_spread_bps
        {
            return Err("spread is too wide");
        }
        if attestation.account_symbol_notional < Decimal::ZERO
            || attestation.account_asset_class_notional < Decimal::ZERO
        {
            return Err("reconciled tokenized exposure is invalid");
        }
        if attestation.account_asset_class_notional < attestation.account_symbol_notional {
            return Err("tokenized asset-class exposure is inconsistent");
        }
        if let Some(expected) = expected_account_asset_class_notional {
            if attestation.account_asset_class_notional != expected {
                return Err("tokenized asset-class exposure is inconsistent");
            }
        }

        let price = intent
            .price
            .ok_or("tokenized admission requires a priced order")?
            .0;
        if price <= Decimal::ZERO || intent.quantity.0 <= Decimal::ZERO {
            return Err("tokenized admission requires positive price and quantity");
        }
        let notional = price * intent.quantity.0;
        let projected_symbol_notional =
            attestation.account_symbol_notional + pending_symbol_notional + notional;
        let projected_asset_class_notional = attestation.account_asset_class_notional
            + pending_asset_class_notional
            + notional;
        if projected_symbol_notional > self.config.max_notional_per_symbol
            || projected_asset_class_notional > self.config.max_asset_class_notional
        {
            return Err("order exceeds tokenized notional limit");
        }

        Ok((
            hft_core::ComplianceContext {
                jurisdiction: Some(jurisdiction.to_string()),
                eligibility_confirmed: true,
                allow_tokenized_securities: true,
                top_depth_usd: Some(attestation.top_depth_usd),
                spread_bps: Some(attestation.spread_bps),
                corporate_action_active: Some(false),
                evidence_source: Some(attestation.source_id.clone()),
                evidence_venue: Some(attestation.venue),
                evidence_observed_at: Some(attestation.observed_at),
                ..Default::default()
            },
            notional,
            attestation.account_asset_class_notional,
        ))
    }
}

impl RiskManager for TokenizedSecuritiesRiskManager {
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
        let mut filtered = Vec::with_capacity(envelopes.len());
        let mut pending_symbol_notionals = HashMap::new();
        let mut pending_asset_class_notional = rust_decimal::Decimal::ZERO;
        let mut account_asset_class_notional = None;

        for mut envelope in envelopes {
            if !Self::is_tokenized(&envelope.intent) {
                filtered.push(envelope);
                continue;
            }

            let pending_symbol_notional = pending_symbol_notionals
                .get(&envelope.intent.symbol)
                .copied()
                .unwrap_or(rust_decimal::Decimal::ZERO);
            if let Some((notional, attested_asset_class_notional)) = self.admit_tokenized_intent(
                &mut envelope.intent,
                account,
                pending_symbol_notional,
                pending_asset_class_notional,
                account_asset_class_notional,
            ) {
                *pending_symbol_notionals
                    .entry(envelope.intent.symbol.clone())
                    .or_insert(rust_decimal::Decimal::ZERO) += notional;
                pending_asset_class_notional += notional;
                account_asset_class_notional = Some(attested_asset_class_notional);
                filtered.push(envelope);
            }
        }

        self.base_risk_manager
            .review_envelopes_with_venue_specs(filtered, account, venue_specs)
    }

    fn on_execution_event(&mut self, event: &ports::ExecutionEvent) {
        self.base_risk_manager.on_execution_event(event)
    }

    fn emergency_stop(&mut self) -> Result<(), hft_core::HftError> {
        self.base_risk_manager.emergency_stop()
    }

    fn get_risk_metrics(&self) -> HashMap<String, rust_decimal::Decimal> {
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

    fn get_risk_metrics(&self) -> HashMap<String, rust_decimal::Decimal> {
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
    use hft_core::{
        AccountCapability, AccountId, ComplianceContext, InstrumentKey, OrderType, Price,
        RegulatoryProfile, Side, Symbol, TimeInForce,
    };
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
            max_daily_loss: Decimal::from(1234),
            max_drawdown_pct: 3.5,
            enhanced: None,
            strategy_overrides: HashMap::new(),
            tokenized_securities: Default::default(),
        };

        let manager = RiskManagerFactory::create_risk_manager(&risk_config);

        // 產生 Default 風控，且保持風控類型不被意外修改
        assert_eq!(risk_config.risk_type, "Default");
        assert_eq!(manager.get_config_snapshot().max_drawdown_pct, 3.5);
        let losing_account = ports::AccountView {
            realized_pnl: Decimal::from(-1234),
            ..Default::default()
        };
        assert!(manager.should_halt_trading(&losing_account));
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
            max_daily_loss: Decimal::from(10000),
            max_drawdown_pct: 5.0,
            enhanced: Some(enhanced_settings),
            strategy_overrides: HashMap::new(),
            tokenized_securities: Default::default(),
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
            max_daily_loss: Decimal::from(10000),
            max_drawdown_pct: 5.0,
            enhanced: None,
            strategy_overrides: HashMap::new(),
            tokenized_securities: Default::default(),
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

    #[test]
    fn tokenized_security_requires_runtime_owned_attestation() {
        let risk_config = SystemRiskConfig {
            risk_type: "Default".to_string(),
            global_position_limit: Decimal::from(1000),
            global_notional_limit: Decimal::from(100_000),
            max_daily_trades: 100,
            max_orders_per_second: 10,
            staleness_threshold_us: 60_000_000,
            max_daily_loss: Decimal::from(10_000),
            max_drawdown_pct: 5.0,
            enhanced: None,
            strategy_overrides: HashMap::new(),
            tokenized_securities: TokenizedSecuritiesRiskConfig {
                allow_trading: true,
                evidence_max_age_us: 60_000_000,
                max_notional_per_symbol: Decimal::from(1_000),
                max_asset_class_notional: Decimal::from(2_000),
                min_top_depth_usd: Decimal::from(10_000),
                max_spread_bps: Decimal::from(10),
                freeze_on_corporate_action: true,
                restricted_jurisdictions: vec!["US".to_string()],
            },
        };

        let intent = ports::OrderIntent::crypto_spot(
            Symbol::new("TSLAUSDT"),
            Side::Buy,
            Quantity(Decimal::from(1)),
            OrderType::Limit,
            Some(Price(Decimal::from(100))),
            TimeInForce::GTC,
            "token-alpha".to_string(),
            Some(VenueId::BINANCE_TOKENIZED_SECURITIES),
        )
        .tokenized_security_spot(ComplianceContext {
            regulatory_profile: RegulatoryProfile::AdgmTokenizedSecurity,
            jurisdiction: Some("US".to_string()),
            eligibility_confirmed: true,
            allow_tokenized_securities: true,
            top_depth_usd: Some(Decimal::from(1_000_000)),
            spread_bps: Some(Decimal::ZERO),
            corporate_action_active: Some(false),
            evidence_source: Some("strategy-self-report".to_string()),
            evidence_venue: Some(VenueId::BINANCE_TOKENIZED_SECURITIES),
            evidence_observed_at: Some(0),
        });
        let specs = HashMap::from([(
            VenueId::BINANCE_TOKENIZED_SECURITIES,
            ports::VenueSpec::binance_spot(),
        )]);
        let account_id = AccountId::from("binance-testnet-spot");
        let attestation_key = InstrumentKey::tokenized_security_spot(
            Symbol::new("TSLAUSDT"),
            VenueId::BINANCE_TOKENIZED_SECURITIES,
        );
        let now = hft_core::now_micros();
        let valid_attestation = ports::TokenizedSecuritiesRuntimeAttestation {
            account_id: account_id.clone(),
            venue: VenueId::BINANCE_TOKENIZED_SECURITIES,
            product_type: ProductType::TokenizedSecuritySpot,
            symbol: Symbol::new("TSLAUSDT"),
            source_id: BINANCE_BSTOCKS_EVIDENCE_SOURCE.to_string(),
            observed_at: now,
            jurisdiction: Some("AE".to_string()),
            account_eligible: true,
            account_capability: AccountCapability {
                can_trade_tokenized_securities: true,
                jurisdiction: Some("AE".to_string()),
                kyc_level: Some("verified".to_string()),
                ..Default::default()
            },
            account_symbol_notional: Decimal::ZERO,
            account_asset_class_notional: Decimal::ZERO,
            corporate_action_active: false,
            top_depth_usd: Decimal::from(20_000),
            spread_bps: Decimal::from(5),
        };
        let review_with_config =
            |config: &SystemRiskConfig,
             attestation: Option<ports::TokenizedSecuritiesRuntimeAttestation>| {
                let account = ports::AccountView {
                    account_id: Some(account_id.clone()),
                    tokenized_securities_attestations: attestation
                        .map(|attestation| HashMap::from([(attestation_key.clone(), attestation)]))
                        .unwrap_or_default(),
                    ..Default::default()
                };
                let mut manager = RiskManagerFactory::create_strategy_aware_risk_manager(config);
                manager.review_with_venue_specs(vec![intent.clone()], &account, &specs)
            };
        let review = |attestation| review_with_config(&risk_config, attestation);

        let approved = review(Some(valid_attestation.clone()));
        assert_eq!(approved.len(), 1);
        assert_eq!(
            approved[0].compliance_context.regulatory_profile,
            RegulatoryProfile::None
        );
        assert_eq!(
            approved[0].compliance_context.jurisdiction.as_deref(),
            Some("AE")
        );
        assert_eq!(
            approved[0].compliance_context.evidence_source.as_deref(),
            Some(BINANCE_BSTOCKS_EVIDENCE_SOURCE)
        );
        assert!(review(None).is_empty(), "missing runtime attestation");

        let mut stale = valid_attestation.clone();
        stale.observed_at = now.saturating_sub(60_000_001);
        assert!(review(Some(stale)).is_empty(), "stale attestation");

        let mut future_dated = valid_attestation.clone();
        future_dated.observed_at = now.saturating_add(60_000_000);
        assert!(review(Some(future_dated)).is_empty(), "future-dated attestation");

        let mut wrong_venue = valid_attestation.clone();
        wrong_venue.venue = VenueId::BINANCE;
        assert!(review(Some(wrong_venue)).is_empty(), "wrong venue");

        let mut wrong_product = valid_attestation.clone();
        wrong_product.product_type = ProductType::Spot;
        assert!(review(Some(wrong_product)).is_empty(), "wrong product");

        let mut wrong_account = valid_attestation.clone();
        wrong_account.account_id = AccountId::from("other-account");
        assert!(review(Some(wrong_account)).is_empty(), "wrong account");

        let mut wrong_symbol = valid_attestation.clone();
        wrong_symbol.symbol = Symbol::new("AAPLUSDT");
        assert!(review(Some(wrong_symbol)).is_empty(), "wrong symbol");

        let mut unapproved_source = valid_attestation.clone();
        unapproved_source.source_id = "unapproved".to_string();
        assert!(review(Some(unapproved_source)).is_empty(), "unapproved source");

        let mut restricted_jurisdiction = valid_attestation.clone();
        restricted_jurisdiction.jurisdiction = Some("US".to_string());
        restricted_jurisdiction.account_capability.jurisdiction = Some("US".to_string());
        assert!(
            review(Some(restricted_jurisdiction)).is_empty(),
            "restricted jurisdiction"
        );

        let mut padded_restricted_jurisdiction = valid_attestation.clone();
        padded_restricted_jurisdiction.jurisdiction = Some(" US ".to_string());
        padded_restricted_jurisdiction
            .account_capability
            .jurisdiction = Some(" US ".to_string());
        assert!(
            review(Some(padded_restricted_jurisdiction)).is_empty(),
            "whitespace must not bypass restricted jurisdiction"
        );

        let mut padded_allowed_jurisdiction = valid_attestation.clone();
        padded_allowed_jurisdiction.jurisdiction = Some(" AE ".to_string());
        padded_allowed_jurisdiction
            .account_capability
            .jurisdiction = Some(" AE ".to_string());
        let padded_allowed = review(Some(padded_allowed_jurisdiction));
        assert_eq!(padded_allowed.len(), 1);
        assert_eq!(
            padded_allowed[0].compliance_context.jurisdiction.as_deref(),
            Some("AE"),
            "runtime context must emit the normalized jurisdiction"
        );

        let mut unknown_jurisdiction = valid_attestation.clone();
        unknown_jurisdiction.jurisdiction = None;
        unknown_jurisdiction.account_capability.jurisdiction = None;
        assert!(
            review(Some(unknown_jurisdiction)).is_empty(),
            "unknown jurisdiction"
        );

        let mut ineligible = valid_attestation.clone();
        ineligible.account_eligible = false;
        assert!(review(Some(ineligible)).is_empty(), "ineligible account");

        let mut capability_disabled = valid_attestation.clone();
        capability_disabled
            .account_capability
            .can_trade_tokenized_securities = false;
        assert!(
            review(Some(capability_disabled)).is_empty(),
            "tokenized capability disabled"
        );

        let mut corporate_action = valid_attestation.clone();
        corporate_action.corporate_action_active = true;
        assert!(review(Some(corporate_action)).is_empty(), "corporate action active");

        let mut insufficient_depth = valid_attestation.clone();
        insufficient_depth.top_depth_usd = Decimal::from(9_999);
        assert!(review(Some(insufficient_depth)).is_empty(), "insufficient depth");

        let mut wide_spread = valid_attestation.clone();
        wide_spread.spread_bps = Decimal::from(11);
        assert!(review(Some(wide_spread)).is_empty(), "wide spread");

        let mut negative_spread = valid_attestation.clone();
        negative_spread.spread_bps = Decimal::from(-1);
        assert!(review(Some(negative_spread)).is_empty(), "negative spread");

        let mut negative_symbol_exposure = valid_attestation.clone();
        negative_symbol_exposure.account_symbol_notional = Decimal::from(-1);
        assert!(
            review(Some(negative_symbol_exposure)).is_empty(),
            "negative symbol exposure"
        );

        let mut negative_asset_class_exposure = valid_attestation.clone();
        negative_asset_class_exposure.account_asset_class_notional = Decimal::from(-1);
        assert!(
            review(Some(negative_asset_class_exposure)).is_empty(),
            "negative asset-class exposure"
        );

        let mut unpriced_intent = intent.clone();
        unpriced_intent.price = None;
        let unpriced_account = ports::AccountView {
            account_id: Some(account_id.clone()),
            tokenized_securities_attestations: HashMap::from([(
                attestation_key.clone(),
                valid_attestation.clone(),
            )]),
            ..Default::default()
        };
        assert!(
            RiskManagerFactory::create_strategy_aware_risk_manager(&risk_config)
                .review_with_venue_specs(
                    vec![unpriced_intent],
                    &unpriced_account,
                    &specs,
                )
                .is_empty(),
            "unpriced tokenized intent"
        );

        let mut symbol_near_limit = valid_attestation.clone();
        symbol_near_limit.account_symbol_notional = Decimal::from(950);
        assert!(
            review(Some(symbol_near_limit)).is_empty(),
            "existing symbol exposure must count toward the cap"
        );

        let mut asset_class_near_limit = valid_attestation.clone();
        asset_class_near_limit.account_asset_class_notional = Decimal::from(1_950);
        assert!(
            review(Some(asset_class_near_limit)).is_empty(),
            "existing asset-class exposure must count toward the cap"
        );

        let mut sell_near_limit = intent.clone();
        sell_near_limit.side = Side::Sell;
        let mut sell_near_limit_attestation = valid_attestation.clone();
        sell_near_limit_attestation.account_symbol_notional = Decimal::from(950);
        sell_near_limit_attestation.account_asset_class_notional = Decimal::from(1_950);
        let sell_near_limit_account = ports::AccountView {
            account_id: Some(account_id.clone()),
            tokenized_securities_attestations: HashMap::from([(
                attestation_key.clone(),
                sell_near_limit_attestation,
            )]),
            ..Default::default()
        };
        assert!(
            RiskManagerFactory::create_strategy_aware_risk_manager(&risk_config)
                .review_with_venue_specs(
                    vec![sell_near_limit],
                    &sell_near_limit_account,
                    &specs,
                )
                .is_empty(),
            "attestation-only admission must not infer covered sells"
        );

        let mut inconsistent_asset_class_exposure = valid_attestation.clone();
        inconsistent_asset_class_exposure.account_symbol_notional = Decimal::from(900);
        assert!(
            review(Some(inconsistent_asset_class_exposure)).is_empty(),
            "asset-class exposure cannot be below one symbol exposure"
        );

        let mut batched_intent = intent.clone();
        batched_intent.quantity = Quantity(Decimal::from(6));
        let batched_account = ports::AccountView {
            account_id: Some(account_id.clone()),
            tokenized_securities_attestations: HashMap::from([(
                attestation_key.clone(),
                valid_attestation.clone(),
            )]),
            ..Default::default()
        };
        assert_eq!(
            RiskManagerFactory::create_strategy_aware_risk_manager(&risk_config)
                .review_with_venue_specs(
                    vec![batched_intent.clone(), batched_intent],
                    &batched_account,
                    &specs,
                )
                .len(),
            1,
            "cumulative batch notional must not exceed the symbol cap"
        );

        let mut batched_envelope_intent = intent.clone();
        batched_envelope_intent.quantity = Quantity(Decimal::from(6));
        assert_eq!(
            RiskManagerFactory::create_strategy_aware_risk_manager(&risk_config)
                .review_envelopes_with_venue_specs(
                    vec![
                        ports::OrderIntentEnvelope::new(
                            batched_envelope_intent.clone(),
                            ports::OrderIntentLifecycle::new(now, u64::MAX),
                        ),
                        ports::OrderIntentEnvelope::new(
                            batched_envelope_intent,
                            ports::OrderIntentLifecycle::new(now, u64::MAX),
                        ),
                    ],
                    &batched_account,
                    &specs,
                )
                .len(),
            1,
            "envelope admission must retain the tokenized batch cap"
        );

        let mut second_intent = intent.clone();
        second_intent.symbol = Symbol::new("AAPLUSDT");
        let second_key = InstrumentKey::tokenized_security_spot(
            second_intent.symbol.clone(),
            VenueId::BINANCE_TOKENIZED_SECURITIES,
        );
        let mut inconsistent_asset_class_attestation = valid_attestation.clone();
        inconsistent_asset_class_attestation.symbol = second_intent.symbol.clone();
        inconsistent_asset_class_attestation.account_asset_class_notional = Decimal::ONE;
        let inconsistent_asset_class_account = ports::AccountView {
            account_id: Some(account_id.clone()),
            tokenized_securities_attestations: HashMap::from([
                (attestation_key.clone(), valid_attestation.clone()),
                (second_key, inconsistent_asset_class_attestation),
            ]),
            ..Default::default()
        };
        assert_eq!(
            RiskManagerFactory::create_strategy_aware_risk_manager(&risk_config)
                .review_with_venue_specs(
                    vec![intent.clone(), second_intent],
                    &inconsistent_asset_class_account,
                    &specs,
                )
                .len(),
            1,
            "batch attestation must agree on asset-class exposure"
        );

        let non_tokenized = ports::OrderIntent::crypto_spot(
            Symbol::new("BTCUSDT"),
            Side::Buy,
            Quantity(Decimal::from(1)),
            OrderType::Limit,
            Some(Price(Decimal::from(100))),
            TimeInForce::GTC,
            "crypto-alpha".to_string(),
            Some(VenueId::BINANCE),
        );
        let non_tokenized_specs =
            HashMap::from([(VenueId::BINANCE, ports::VenueSpec::binance_spot())]);
        assert_eq!(
            RiskManagerFactory::create_strategy_aware_risk_manager(&risk_config)
                .review_with_venue_specs(
                    vec![non_tokenized],
                    &ports::AccountView::default(),
                    &non_tokenized_specs,
                )
                .len(),
            1
        );

        let no_corporate_action_freeze = SystemRiskConfig {
            tokenized_securities: TokenizedSecuritiesRiskConfig {
                freeze_on_corporate_action: false,
                ..risk_config.tokenized_securities.clone()
            },
            ..risk_config.clone()
        };
        assert!(
            review_with_config(
                &no_corporate_action_freeze,
                Some(valid_attestation.clone()),
            )
                .is_empty(),
            "corporate-action freeze must be configured"
        );

        let unconfigured_limit = SystemRiskConfig {
            tokenized_securities: TokenizedSecuritiesRiskConfig {
                min_top_depth_usd: Decimal::ZERO,
                ..risk_config.tokenized_securities.clone()
            },
            ..risk_config.clone()
        };
        assert!(
            review_with_config(&unconfigured_limit, Some(valid_attestation.clone()))
                .is_empty(),
            "all tokenized limits must be configured"
        );

        let unconfigured_evidence_age = SystemRiskConfig {
            tokenized_securities: TokenizedSecuritiesRiskConfig {
                evidence_max_age_us: 0,
                ..risk_config.tokenized_securities.clone()
            },
            ..risk_config.clone()
        };
        assert!(
            review_with_config(&unconfigured_evidence_age, Some(valid_attestation.clone()))
                .is_empty(),
            "attestation freshness limit must be configured"
        );

        let independent_evidence_age = SystemRiskConfig {
            staleness_threshold_us: u64::MAX,
            ..risk_config.clone()
        };
        let mut stale_with_fresh_market_data = valid_attestation.clone();
        stale_with_fresh_market_data.observed_at = now.saturating_sub(60_000_001);
        assert!(
            review_with_config(&independent_evidence_age, Some(stale_with_fresh_market_data))
                .is_empty(),
            "market-data staleness must not extend attestation freshness"
        );

        let disabled_config = SystemRiskConfig {
            tokenized_securities: TokenizedSecuritiesRiskConfig {
                allow_trading: false,
                ..risk_config.tokenized_securities.clone()
            },
            ..risk_config.clone()
        };
        assert!(
            review_with_config(
                &disabled_config,
                Some(ports::TokenizedSecuritiesRuntimeAttestation {
                    observed_at: hft_core::now_micros(),
                    ..valid_attestation
                }),
            )
                .is_empty(),
            "allow_trading remains a fail-closed gate"
        );
    }

    #[test]
    fn projected_exposure_does_not_net_opposite_orders_across_venues() {
        let config = SystemRiskConfig {
            risk_type: "Default".to_string(),
            global_position_limit: Decimal::from(100),
            global_notional_limit: Decimal::from(1_000),
            max_orders_per_second: 100,
            staleness_threshold_us: u64::MAX,
            max_daily_loss: Decimal::from(10_000),
            max_drawdown_pct: 5.0,
            ..Default::default()
        };
        let base = RiskManagerFactory::create_risk_manager(&config);
        let manager = ProjectedExposureRiskManager::new(
            base,
            config.global_position_limit,
            config.global_notional_limit,
        );
        let intent = |venue, side| {
            ports::OrderIntent::crypto_spot(
                Symbol::new("BTCUSDT"),
                side,
                Quantity(Decimal::from(60)),
                OrderType::Limit,
                Some(Price(Decimal::ONE)),
                TimeInForce::GTC,
                "cross-venue".to_string(),
                Some(venue),
            )
        };

        let approved = manager.filter(
            vec![
                intent(VenueId::BINANCE, Side::Buy),
                intent(VenueId::BITGET, Side::Sell),
            ],
            &ports::AccountView::default(),
        );

        assert_eq!(approved.len(), 1);
    }
}
