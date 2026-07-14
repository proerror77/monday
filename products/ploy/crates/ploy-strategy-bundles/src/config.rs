//! Unified TOML configuration for strategy runtime.
//!
//! A single config file drives backtest, dry-run, and live modes.
//! The `[runtime].mode` field selects which Feed and Executor are wired.

use chrono::{DateTime, Utc};
use ploy_market_contracts::{
    FeeAsset, FeeFormula, FeeRounding, FeeSchedule, InstrumentKind, LiquidityRole,
    PredictionFamily, VenueKind,
};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::Path;
use std::path::PathBuf;

use crate::engine::{RuntimeConfig, RuntimeMode};
use crate::executor::SimulatedExecutorConfig;
use crate::feed::{RecordingKind, RecordingLimits, RecordingPolicy};
use crate::strategies::directional::DirectionalConfig;

/// Top-level config deserialized from a TOML file.
///
/// ```toml
/// [runtime]
/// mode = "dryrun"
/// throttle_hz = 1
/// record_market_updates_to = "tmp/dryrun.ndjson"
///
/// [strategy]
/// symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
/// min_edge = 0.02
/// # ... (all DirectionalConfig fields)
///
/// [execution]
/// spread_pct = 0.02
/// enable_partial_fills = true
/// # ... (all SimulatedExecutorConfig fields)
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct FullConfig {
    pub runtime: RuntimeSection,
    pub strategy: DirectionalConfig,
    #[serde(default)]
    pub reference_data: ReferenceDataSection,
    #[serde(default)]
    pub backtest_data: BacktestDataSection,
    #[serde(default)]
    pub execution: SimExecutionSection,
    #[serde(default)]
    pub live_execution: LiveExecutionSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeSection {
    #[serde(default = "default_mode")]
    pub mode: String,
    pub throttle_hz: Option<u32>,
    pub max_updates: Option<u64>,
    /// Strategy variant.
    ///
    /// Strategy variant. Canonical variants and roadmap aliases are owned by
    /// `strategies::registry`; keep this config field as the TOML surface only.
    #[serde(default = "default_strategy_variant")]
    pub strategy_variant: String,
    /// Prediction family for the configured runtime.
    #[serde(default)]
    pub prediction_family: PredictionFamily,
    /// Instrument kind for the configured runtime.
    #[serde(default)]
    pub instrument_kind: InstrumentKind,
    /// Venue kind for the configured runtime.
    #[serde(default)]
    pub venue: VenueKind,
    /// Backtest start time (ISO 8601 format, e.g., "2026-04-01T00:00:00Z")
    pub from: Option<String>,
    /// Backtest end time (ISO 8601 format, e.g., "2026-04-01T23:59:59Z")
    pub to: Option<String>,
    /// Optional NDJSON log path for canonical `MarketUpdate` recording.
    pub record_market_updates_to: Option<PathBuf>,
    /// Optional hard cap for a bounded canonical `MarketUpdate` recording.
    pub record_market_updates_max_records: Option<u64>,
    /// Optional hard cap for a bounded canonical `MarketUpdate` recording.
    pub record_market_updates_max_bytes: Option<u64>,
    /// Optional allow-list of update kinds to persist. Empty records all kinds.
    #[serde(default)]
    pub record_market_updates_include_kinds: Vec<RecordingKind>,
    /// Optional per-token quote sampling interval for the persisted tape.
    pub record_market_updates_quote_sample_ms: Option<u64>,
    /// Optional maximum bid/ask depth persisted per quote.
    pub record_market_updates_quote_depth_levels: Option<usize>,
    /// Restrict persisted quotes to tokens belonging to an active discovered event.
    #[serde(default)]
    pub record_market_updates_event_scoped_quotes: bool,
    /// Source boundary for live/dry-run market data.
    ///
    /// Defaults to `local_db`, where strategy runners consume collector-persisted
    /// data and never open their own public Polymarket/RTDS/Gamma feeds.
    #[serde(default)]
    pub market_data_source: MarketDataSource,
    /// Required when `mode = "replay"`; points at a previously recorded NDJSON log.
    pub replay_market_updates_from: Option<PathBuf>,
    /// Override settlement-exit submission for parity replays.
    ///
    /// Live mode defaults to skipping settlement exits because Polymarket
    /// settles on-chain. Replay/backtest/dry-run default to preserving strategy
    /// exits unless this field is explicitly set.
    pub skip_settlement_exits: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketDataSource {
    /// Consume locally persisted collector data only.
    LocalDb,
    /// Open direct public feeds from this strategy runner.
    ExternalDirect,
    /// Open direct public feeds while retaining the local DB for persistence,
    /// metadata, and recovery services. Polled DB ticks are not merged into the
    /// strategy hot path because they can duplicate or reorder direct ticks.
    Dual,
}

impl Default for MarketDataSource {
    fn default() -> Self {
        Self::LocalDb
    }
}

impl MarketDataSource {
    #[must_use]
    pub fn uses_local_db(self) -> bool {
        matches!(self, Self::LocalDb | Self::Dual)
    }

    #[must_use]
    pub fn uses_external_direct(self) -> bool {
        matches!(self, Self::ExternalDirect | Self::Dual)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReferenceDataSection {
    /// Optional Pyth/RTDS symbols to capture alongside the existing crypto runtime.
    ///
    /// Symbols are case-insensitive in config. RTDS payloads are normalized to lowercase.
    #[serde(default)]
    pub pyth_symbols: Vec<String>,
    /// Enable Polymarket sports live-state capture for record/replay and persistence.
    #[serde(default)]
    pub capture_sports_state: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BacktestDataSection {
    /// Load additive reference-price ticks into historical backtests.
    #[serde(default)]
    pub include_reference_prices: bool,
    /// Load additive sports-state updates into historical backtests.
    #[serde(default)]
    pub include_sports_state: bool,
    /// When true, historical backtests skip markets without official settlement rows.
    #[serde(default)]
    pub require_official_settlement: bool,
    /// Optional symbol override for reference-price backtests.
    ///
    /// When empty, the historical loader falls back to `reference_data.pyth_symbols`.
    #[serde(default)]
    pub reference_symbols: Vec<String>,
}

impl BacktestDataSection {
    #[must_use]
    pub fn reference_symbols(&self, reference_data: &ReferenceDataSection) -> Vec<String> {
        let raw_symbols = if self.reference_symbols.is_empty() {
            &reference_data.pyth_symbols
        } else {
            &self.reference_symbols
        };

        raw_symbols
            .iter()
            .map(|symbol| symbol.trim().to_lowercase())
            .filter(|symbol| !symbol.is_empty())
            .collect()
    }
}

fn default_mode() -> String {
    "dryrun".into()
}

fn default_strategy_variant() -> String {
    "directional".into()
}

impl RuntimeSection {
    #[must_use]
    pub fn canonical_strategy_variant(&self) -> String {
        crate::strategies::registry::canonical_strategy_variant(&self.strategy_variant)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SimExecutionSection {
    #[serde(default)]
    pub use_spread: bool,
    #[serde(default = "default_spread")]
    pub spread_pct: f64,
    #[serde(default)]
    pub enable_partial_fills: bool,
    #[serde(default = "default_depth_multiple")]
    pub depth_multiple: f64,
    #[serde(default = "default_min_fill")]
    pub min_fill_pct: f64,
    #[serde(default)]
    pub enable_market_impact: bool,
    #[serde(default = "default_impact")]
    pub impact_coefficient: f64,
    #[serde(default = "default_depth_shares")]
    pub default_depth_shares: u64,
    #[serde(default)]
    pub require_lob_liquidity: bool,
    #[serde(default = "default_visible_depth_haircut")]
    pub visible_depth_haircut: f64,
    #[serde(default)]
    pub max_sweep_levels: usize,
    #[serde(default)]
    pub max_sweep_price_delta: f64,
    #[serde(default)]
    pub fee_formula: SimFeeFormula,
    pub taker_fee_rate: Option<f64>,
    pub maker_fee_rate: Option<f64>,
    pub taker_fee_rate_bps: Option<u32>,
    pub maker_fee_rate_bps: Option<u32>,
    pub fee_exponent: Option<u32>,
    pub fee_taker_only: Option<bool>,
    pub fee_rounding_dp: Option<u32>,
    pub minimum_fee: Option<f64>,
    pub fee_balance_precision_dp: Option<u32>,
    pub fee_asset: Option<FeeAsset>,
    #[serde(default = "default_liquidity_role")]
    pub liquidity_role: LiquidityRole,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SimFeeFormula {
    #[default]
    VenueDefault,
    ProbabilityPower,
    Notional,
    PerContract,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiveExecutionSection {
    /// Maximum permitted price movement from the strategy limit for live FAK/FOK
    /// execution. Buy orders cap at `limit * (1 + bps/10000)`; sell orders floor
    /// at `limit * (1 - bps/10000)`.
    #[serde(default = "default_live_max_slippage_bps")]
    pub max_slippage_bps: u32,
    /// Maximum total submission attempts for an acknowledged-but-unfilled FAK
    /// order before the runtime marks the intent terminal and lets the strategy
    /// cool down.
    #[serde(default = "default_live_max_attempts")]
    pub max_attempts: u8,
    /// Number of reconciliation passes to wait before treating an acknowledged
    /// FAK order with no new fill as terminal/unfilled for that attempt.
    #[serde(default = "default_live_reconcile_cycles_before_retry")]
    pub reconcile_cycles_before_retry: u8,
}

fn default_live_max_slippage_bps() -> u32 {
    150
}

fn default_live_max_attempts() -> u8 {
    2
}

fn default_live_reconcile_cycles_before_retry() -> u8 {
    2
}

fn default_spread() -> f64 {
    0.02
}
fn default_depth_multiple() -> f64 {
    5.0
}
fn default_min_fill() -> f64 {
    0.5
}
fn default_impact() -> f64 {
    0.1
}
fn default_depth_shares() -> u64 {
    500
}
fn default_visible_depth_haircut() -> f64 {
    1.0
}

fn default_liquidity_role() -> LiquidityRole {
    LiquidityRole::Taker
}

impl Default for SimExecutionSection {
    fn default() -> Self {
        Self {
            use_spread: false,
            spread_pct: 0.02,
            enable_partial_fills: false,
            depth_multiple: 5.0,
            min_fill_pct: 0.5,
            enable_market_impact: false,
            impact_coefficient: 0.1,
            default_depth_shares: 500,
            require_lob_liquidity: false,
            visible_depth_haircut: 1.0,
            max_sweep_levels: 0,
            max_sweep_price_delta: 0.0,
            fee_formula: SimFeeFormula::VenueDefault,
            taker_fee_rate: None,
            maker_fee_rate: None,
            taker_fee_rate_bps: None,
            maker_fee_rate_bps: None,
            fee_exponent: None,
            fee_taker_only: None,
            fee_rounding_dp: None,
            minimum_fee: None,
            fee_balance_precision_dp: None,
            fee_asset: None,
            liquidity_role: LiquidityRole::Taker,
        }
    }
}

impl Default for LiveExecutionSection {
    fn default() -> Self {
        Self {
            max_slippage_bps: default_live_max_slippage_bps(),
            max_attempts: default_live_max_attempts(),
            reconcile_cycles_before_retry: default_live_reconcile_cycles_before_retry(),
        }
    }
}

impl FullConfig {
    /// Parse from a TOML string.
    pub fn from_toml(toml_str: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_str)
    }

    /// Parse from a TOML file path.
    ///
    /// Rejects legacy-format configs (those with `[timing]` or `[entry]`
    /// sections) to prevent silent value misinterpretation.
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Cannot read config {path}: {e}"))?;

        // Guard: reject legacy format that would silently misparse
        if content.contains("[timing]") || content.contains("[entry]") || content.contains("[risk]")
        {
            return Err(format!(
                "Config {path} uses legacy format ([timing]/[entry]/[risk] sections). \
                 Use 02-pm5d.unified.toml format instead: [runtime]/[strategy]/[execution]."
            )
            .into());
        }

        Self::from_toml(&content).map_err(|e| format!("Invalid TOML in {path}: {e}").into())
    }

    /// Build RuntimeConfig from the parsed config.
    pub fn runtime_config(&self) -> RuntimeConfig {
        let mode = match self.runtime.mode.as_str() {
            "backtest" => RuntimeMode::Backtest,
            "replay" => RuntimeMode::Replay,
            "live" => RuntimeMode::Live,
            _ => RuntimeMode::DryRun,
        };
        RuntimeConfig {
            mode,
            throttle_hz: self.runtime.throttle_hz,
            max_updates: self.runtime.max_updates,
            skip_settlement_exits: self
                .runtime
                .skip_settlement_exits
                .unwrap_or(mode == RuntimeMode::Live),
        }
    }

    /// Parse backtest time range from config.
    /// Returns (from, to) if both are specified, otherwise None.
    pub fn backtest_time_range(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let from_str = self.runtime.from.as_ref()?;
        let to_str = self.runtime.to.as_ref()?;

        let from = DateTime::parse_from_rfc3339(from_str)
            .ok()?
            .with_timezone(&Utc);
        let to = DateTime::parse_from_rfc3339(to_str)
            .ok()?
            .with_timezone(&Utc);

        Some((from, to))
    }

    pub fn record_market_updates_path(&self) -> Option<&Path> {
        self.runtime.record_market_updates_to.as_deref()
    }

    pub fn record_market_updates_limits(&self) -> RecordingLimits {
        RecordingLimits {
            max_records: self.runtime.record_market_updates_max_records,
            max_bytes: self.runtime.record_market_updates_max_bytes,
        }
    }

    pub fn record_market_updates_policy(&self) -> RecordingPolicy {
        RecordingPolicy {
            limits: self.record_market_updates_limits(),
            include_kinds: self.runtime.record_market_updates_include_kinds.clone(),
            quote_sample_ms: self.runtime.record_market_updates_quote_sample_ms,
            quote_depth_levels: self.runtime.record_market_updates_quote_depth_levels,
            event_scoped_quotes: self.runtime.record_market_updates_event_scoped_quotes,
        }
    }

    pub fn replay_market_updates_path(&self) -> Option<&Path> {
        self.runtime.replay_market_updates_from.as_deref()
    }

    /// Build SimulatedExecutorConfig from the parsed config.
    pub fn sim_executor_config(&self) -> SimulatedExecutorConfig {
        let e = &self.execution;
        let default_schedule =
            default_fee_schedule(self.runtime.venue, self.runtime.prediction_family);
        let formula = match e.fee_formula {
            SimFeeFormula::VenueDefault => match default_schedule.formula {
                FeeFormula::ProbabilityPower { .. } => FeeFormula::ProbabilityPower {
                    exponent: e
                        .fee_exponent
                        .unwrap_or_else(|| match default_schedule.formula {
                            FeeFormula::ProbabilityPower { exponent } => exponent,
                            _ => 1,
                        }),
                },
                formula => formula,
            },
            SimFeeFormula::ProbabilityPower => FeeFormula::ProbabilityPower {
                exponent: e.fee_exponent.unwrap_or(1),
            },
            SimFeeFormula::Notional => FeeFormula::Notional,
            SimFeeFormula::PerContract => FeeFormula::PerContract,
        };
        let taker_rate_decimal = e
            .taker_fee_rate
            .and_then(Decimal::from_f64)
            .filter(|rate| *rate >= Decimal::ZERO);
        let maker_rate_decimal = e
            .maker_fee_rate
            .and_then(Decimal::from_f64)
            .filter(|rate| *rate >= Decimal::ZERO);
        let taker_rate_bps = e
            .taker_fee_rate_bps
            .map(|bps| Decimal::from(bps) / Decimal::from(10_000_u32));
        let maker_rate_bps = e
            .maker_fee_rate_bps
            .map(|bps| Decimal::from(bps) / Decimal::from(10_000_u32));
        let taker_rate_override = taker_rate_decimal.or(taker_rate_bps);
        let maker_rate_override = maker_rate_decimal.or(maker_rate_bps);
        let taker_rate = taker_rate_override.unwrap_or(default_schedule.taker_rate);
        let maker_rate = if self.runtime.venue == VenueKind::Polymarket
            && e.fee_taker_only == Some(true)
        {
            Decimal::ZERO
        } else {
            maker_rate_override.unwrap_or_else(|| {
                if self.runtime.venue == VenueKind::Polymarket && e.fee_taker_only == Some(false) {
                    taker_rate
                } else {
                    default_schedule.maker_rate
                }
            })
        };
        let rounding = e
            .fee_rounding_dp
            .map_or(default_schedule.rounding, |decimal_places| {
                FeeRounding::Ceiling { decimal_places }
            });
        let minimum_fee_override = e
            .minimum_fee
            .and_then(Decimal::from_f64)
            .filter(|fee| *fee >= Decimal::ZERO);
        let minimum_fee = minimum_fee_override.unwrap_or(default_schedule.minimum_fee);
        let override_values_valid = !(e.taker_fee_rate.is_some() && taker_rate_decimal.is_none())
            && !(e.maker_fee_rate.is_some() && maker_rate_decimal.is_none())
            && !(e.minimum_fee.is_some() && minimum_fee_override.is_none())
            && !(e.taker_fee_rate.is_some() && e.taker_fee_rate_bps.is_some())
            && !(e.maker_fee_rate.is_some() && e.maker_fee_rate_bps.is_some());
        let polymarket_fd_override_requested =
            matches!(self.runtime.prediction_family, PredictionFamily::Custom(_))
                || taker_rate_override.is_some()
                || maker_rate_override.is_some()
                || e.fee_exponent.is_some()
                || e.fee_taker_only.is_some()
                || e.fee_formula != SimFeeFormula::VenueDefault
                || e.fee_rounding_dp.is_some()
                || e.minimum_fee.is_some();
        let venue_metadata_complete = match self.runtime.venue {
            VenueKind::Polymarket => {
                if polymarket_fd_override_requested {
                    taker_rate_override.is_some()
                        && e.fee_exponent.is_some()
                        && e.fee_taker_only.is_some()
                        && maker_rate_override.is_none()
                        && matches!(
                            e.fee_formula,
                            SimFeeFormula::VenueDefault | SimFeeFormula::ProbabilityPower
                        )
                        && e.fee_rounding_dp.is_none()
                        && e.minimum_fee.is_none()
                } else {
                    true
                }
            }
            VenueKind::PredictFun => {
                taker_rate_override.is_some()
                    && matches!(
                        e.fee_formula,
                        SimFeeFormula::VenueDefault | SimFeeFormula::Notional
                    )
                    && e.fee_exponent.is_none()
                    && e.fee_taker_only.is_none()
                    && e.fee_rounding_dp.is_none()
                    && e.minimum_fee.is_none()
            }
            VenueKind::Kalshi => {
                let formula_valid = match e.fee_formula {
                    SimFeeFormula::ProbabilityPower => matches!(e.fee_exponent, None | Some(1)),
                    SimFeeFormula::PerContract => e.fee_exponent.is_none(),
                    SimFeeFormula::VenueDefault | SimFeeFormula::Notional => false,
                };
                taker_rate_override.is_some()
                    && formula_valid
                    && matches!(e.fee_balance_precision_dp, None | Some(2 | 4))
                    && matches!(e.fee_rounding_dp, None | Some(4))
                    && (e.minimum_fee.is_none() || minimum_fee_override == Some(Decimal::ZERO))
                    && e.fee_taker_only.is_none()
                    && matches!(e.fee_asset, None | Some(FeeAsset::Collateral))
            }
            VenueKind::Sportsbook => true,
        };
        let maker_metadata_complete = e.liquidity_role != LiquidityRole::Maker
            || match self.runtime.venue {
                VenueKind::Polymarket => {
                    !matches!(self.runtime.prediction_family, PredictionFamily::Custom(_))
                        || e.fee_taker_only.is_some()
                }
                VenueKind::PredictFun | VenueKind::Kalshi => maker_rate_override.is_some(),
                VenueKind::Sportsbook => true,
            };
        let metadata_complete =
            override_values_valid && venue_metadata_complete && maker_metadata_complete;
        let mut fee_schedule = if metadata_complete {
            FeeSchedule::new(formula, maker_rate, taker_rate, rounding, minimum_fee)
        } else {
            FeeSchedule::new(formula, maker_rate, taker_rate, rounding, minimum_fee)
                .require_market_metadata()
        };
        if self.runtime.venue == VenueKind::Kalshi {
            fee_schedule =
                fee_schedule.with_kalshi_balance_rounding(e.fee_balance_precision_dp.unwrap_or(2));
        }
        let fee_asset = e.fee_asset.or(match self.runtime.venue {
            VenueKind::PredictFun => None,
            VenueKind::Polymarket | VenueKind::Kalshi | VenueKind::Sportsbook => {
                Some(FeeAsset::Collateral)
            }
        });
        SimulatedExecutorConfig {
            use_spread: e.use_spread,
            spread_pct: Decimal::try_from(e.spread_pct).unwrap_or_default(),
            enable_partial_fills: e.enable_partial_fills,
            depth_multiple: Decimal::try_from(e.depth_multiple).unwrap_or_default(),
            min_fill_pct: Decimal::try_from(e.min_fill_pct).unwrap_or_default(),
            enable_market_impact: e.enable_market_impact,
            impact_coefficient: Decimal::try_from(e.impact_coefficient).unwrap_or_default(),
            default_depth_shares: e.default_depth_shares,
            require_lob_liquidity: e.require_lob_liquidity,
            visible_depth_haircut: Decimal::try_from(e.visible_depth_haircut)
                .unwrap_or(Decimal::ONE),
            max_sweep_levels: e.max_sweep_levels,
            fee_schedule,
            fee_asset,
            liquidity_role: e.liquidity_role,
        }
    }

    pub fn live_execution_policy(&self) -> crate::traits::ExecutionPolicy {
        crate::traits::ExecutionPolicy {
            max_slippage_bps: Decimal::from_u32(self.live_execution.max_slippage_bps)
                .unwrap_or_default(),
            max_attempts: self.live_execution.max_attempts.max(1),
            reconcile_cycles_before_retry: self.live_execution.reconcile_cycles_before_retry.max(1),
        }
    }
}

fn default_fee_schedule(venue: VenueKind, family: PredictionFamily) -> FeeSchedule {
    match venue {
        VenueKind::Polymarket => {
            let rate = match family {
                PredictionFamily::CryptoExpiry => Decimal::new(7, 2),
                PredictionFamily::Politics => Decimal::new(4, 2),
                PredictionFamily::SportsPregame | PredictionFamily::SportsLive => {
                    Decimal::new(5, 2)
                }
                PredictionFamily::Custom(_) => Decimal::ZERO,
            };
            let schedule = FeeSchedule::polymarket_v2(rate, 1, true);
            if matches!(family, PredictionFamily::Custom(_)) {
                schedule.require_market_metadata()
            } else {
                schedule
            }
        }
        VenueKind::PredictFun => FeeSchedule::new(
            FeeFormula::Notional,
            Decimal::ZERO,
            Decimal::ZERO,
            FeeRounding::Exact,
            Decimal::ZERO,
        )
        .require_market_metadata(),
        VenueKind::Kalshi => FeeSchedule::new(
            FeeFormula::ProbabilityPower { exponent: 1 },
            Decimal::ZERO,
            Decimal::ZERO,
            FeeRounding::Ceiling { decimal_places: 4 },
            Decimal::ZERO,
        )
        .with_kalshi_balance_rounding(2)
        .require_market_metadata(),
        VenueKind::Sportsbook => FeeSchedule::new(
            FeeFormula::Notional,
            Decimal::ZERO,
            Decimal::ZERO,
            FeeRounding::Exact,
            Decimal::ZERO,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    const SAMPLE_TOML: &str = r#"
[runtime]
mode = "backtest"
throttle_hz = 1
max_updates = 10000
record_market_updates_to = "tmp/sample.ndjson"
record_market_updates_max_records = 1000
record_market_updates_max_bytes = 1048576
record_market_updates_include_kinds = ["quote", "event_discovered", "event_expired", "reference_price"]
record_market_updates_quote_sample_ms = 500
record_market_updates_quote_depth_levels = 5
record_market_updates_event_scoped_quotes = true
market_data_source = "external_direct"

[strategy]
symbols = ["BTCUSDT", "ETHUSDT"]
vol_floor = 0.001
min_probability = 0.55
min_z_score = 0.35
min_entry_price = 0.15
max_entry_price = 0.85
no_trade_zone_min = 0.45
no_trade_zone_max = 0.55
min_edge = 0.02
min_time_remaining_secs = 60
max_time_remaining_secs = 300
cooldown_secs = 0
stake_usd = 25.0
max_positions = 1000
max_daily_trades = 1000

[reference_data]
pyth_symbols = ["AAPL", "XAUUSD"]
capture_sports_state = true

[backtest_data]
include_reference_prices = true
include_sports_state = true

[execution]
spread_pct = 0.02
enable_partial_fills = true
enable_market_impact = true
max_sweep_price_delta = 0.003
"#;

    #[test]
    fn parses_full_config() {
        let config = FullConfig::from_toml(SAMPLE_TOML).unwrap();
        assert_eq!(config.runtime.mode, "backtest");
        assert_eq!(config.runtime.throttle_hz, Some(1));
        assert_eq!(
            config.runtime.record_market_updates_to.as_deref(),
            Some(Path::new("tmp/sample.ndjson"))
        );
        assert_eq!(config.runtime.record_market_updates_max_records, Some(1000));
        assert_eq!(
            config.runtime.record_market_updates_max_bytes,
            Some(1_048_576)
        );
        assert_eq!(
            config.record_market_updates_limits(),
            RecordingLimits {
                max_records: Some(1000),
                max_bytes: Some(1_048_576)
            }
        );
        assert_eq!(
            config.record_market_updates_policy(),
            RecordingPolicy {
                limits: RecordingLimits {
                    max_records: Some(1000),
                    max_bytes: Some(1_048_576),
                },
                include_kinds: vec![
                    RecordingKind::Quote,
                    RecordingKind::EventDiscovered,
                    RecordingKind::EventExpired,
                    RecordingKind::ReferencePrice,
                ],
                quote_sample_ms: Some(500),
                quote_depth_levels: Some(5),
                event_scoped_quotes: true,
            }
        );
        assert_eq!(
            config.runtime.market_data_source,
            MarketDataSource::ExternalDirect
        );
        assert_eq!(config.strategy.symbols, vec!["BTCUSDT", "ETHUSDT"]);
        assert_eq!(config.reference_data.pyth_symbols, vec!["AAPL", "XAUUSD"]);
        assert!(config.reference_data.capture_sports_state);
        assert!(config.backtest_data.include_reference_prices);
        assert!(config.backtest_data.include_sports_state);
        assert_eq!(config.live_execution.max_slippage_bps, 150);
        assert!(!config.backtest_data.require_official_settlement);
        assert!((config.strategy.min_edge - 0.02).abs() < 1e-10);
        assert_eq!(config.strategy.stake_usd, Decimal::new(25, 0));
        assert_eq!(config.strategy.max_positions, 1000);
        assert_eq!(config.execution.max_sweep_price_delta, 0.003);
    }

    #[test]
    fn builds_runtime_config() {
        let config = FullConfig::from_toml(SAMPLE_TOML).unwrap();
        let rc = config.runtime_config();
        assert_eq!(rc.mode, RuntimeMode::Backtest);
        assert_eq!(rc.throttle_hz, Some(1));
        assert_eq!(rc.max_updates, Some(10000));
    }

    #[test]
    fn parses_replay_runtime_paths() {
        let replay_toml = r#"
[runtime]
mode = "replay"
replay_market_updates_from = "captures/dryrun.ndjson"

[strategy]
"#;

        let config = FullConfig::from_toml(replay_toml).unwrap();
        let runtime = config.runtime_config();

        assert_eq!(runtime.mode, RuntimeMode::Replay);
        assert_eq!(
            config.replay_market_updates_path(),
            Some(Path::new("captures/dryrun.ndjson"))
        );
    }

    #[test]
    fn replay_runtime_can_skip_settlement_exits_for_dryrun_parity() {
        let replay_toml = r#"
[runtime]
mode = "replay"
replay_market_updates_from = "captures/dryrun.ndjson"
skip_settlement_exits = true

[strategy]
"#;

        let config = FullConfig::from_toml(replay_toml).unwrap();
        let runtime = config.runtime_config();

        assert_eq!(runtime.mode, RuntimeMode::Replay);
        assert!(runtime.skip_settlement_exits);
    }

    #[test]
    fn replay_runtime_preserves_settlement_exits_by_default() {
        let replay_toml = r#"
[runtime]
mode = "replay"
replay_market_updates_from = "captures/dryrun.ndjson"

[strategy]
"#;

        let config = FullConfig::from_toml(replay_toml).unwrap();
        let runtime = config.runtime_config();

        assert_eq!(runtime.mode, RuntimeMode::Replay);
        assert!(!runtime.skip_settlement_exits);
    }

    #[test]
    fn builds_sim_executor_config() {
        let config = FullConfig::from_toml(SAMPLE_TOML).unwrap();
        let sec = config.sim_executor_config();
        assert!(!sec.use_spread);
        assert!(sec.enable_market_impact);
    }

    #[test]
    fn defaults_work_with_minimal_config() {
        let minimal = r#"
[runtime]
mode = "dryrun"

[strategy]
"#;
        let config = FullConfig::from_toml(minimal).unwrap();
        assert_eq!(config.runtime.mode, "dryrun");
        assert_eq!(
            config.strategy.symbols,
            vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]
        );
        assert!(config.reference_data.pyth_symbols.is_empty());
        assert!(!config.reference_data.capture_sports_state);
        assert_eq!(config.runtime.market_data_source, MarketDataSource::LocalDb);
        assert!(config.runtime.market_data_source.uses_local_db());
        assert!(!config.runtime.market_data_source.uses_external_direct());
        assert!(!config.backtest_data.include_reference_prices);
        assert!(!config.backtest_data.include_sports_state);
        assert!(!config.backtest_data.require_official_settlement);
        assert_eq!(config.live_execution.max_attempts, 2);
        assert_eq!(config.live_execution.reconcile_cycles_before_retry, 2);
        assert!((config.strategy.min_edge - 0.02).abs() < 1e-10);
        assert!(!config.execution.use_spread);
        assert_eq!(config.execution.max_sweep_price_delta, 0.0);
    }

    #[test]
    fn parses_live_execution_policy() {
        let config = FullConfig::from_toml(
            r#"
[runtime]
mode = "live"

[strategy]

[live_execution]
max_slippage_bps = 75
max_attempts = 3
reconcile_cycles_before_retry = 2
"#,
        )
        .unwrap();

        let policy = config.live_execution_policy();
        assert_eq!(policy.max_slippage_bps, Decimal::new(75, 0));
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.reconcile_cycles_before_retry, 2);
    }

    #[test]
    fn backtest_data_defaults_reference_symbols_to_reference_capture_list() {
        let config = FullConfig::from_toml(SAMPLE_TOML).unwrap();
        assert_eq!(
            config
                .backtest_data
                .reference_symbols(&config.reference_data),
            vec!["aapl".to_string(), "xauusd".to_string()]
        );
    }

    #[test]
    fn backtest_data_prefers_explicit_reference_symbols() {
        let toml = r#"
[runtime]
mode = "backtest"

[strategy]

[reference_data]
pyth_symbols = ["AAPL", "XAUUSD"]

[backtest_data]
include_reference_prices = true
reference_symbols = ["GLD", "SPY"]
"#;

        let config = FullConfig::from_toml(toml).unwrap();
        assert_eq!(
            config
                .backtest_data
                .reference_symbols(&config.reference_data),
            vec!["gld".to_string(), "spy".to_string()]
        );
    }

    #[test]
    fn legacy_quantity_alias_still_maps_to_stake_usd() {
        let legacy = r#"
[runtime]
mode = "backtest"

[strategy]
quantity = 25.0
"#;
        let config = FullConfig::from_toml(legacy).unwrap();
        assert_eq!(config.strategy.stake_usd, Decimal::new(25, 0));
    }

    #[test]
    fn canonical_strategy_variant_normalizes_roadmap_aliases() {
        for (raw, expected) in [
            ("directional", "directional"),
            ("v1", "directional"),
            ("v2", "directional"),
            ("v3", "directional"),
            ("directional_bayes", "directional_bayes"),
            ("v4", "mean_reversion"),
            ("pm5d_v4", "mean_reversion"),
            ("reversal", "reversal"),
            ("pm5d_reversal", "reversal"),
        ] {
            let config = FullConfig::from_toml(&format!(
                r#"
[runtime]
mode = "dryrun"
strategy_variant = "{raw}"

[strategy]
"#
            ))
            .unwrap();
            assert_eq!(config.runtime.canonical_strategy_variant(), expected);
        }
    }

    #[test]
    fn runtime_contract_metadata_defaults_to_crypto_polymarket() {
        let config = FullConfig::from_toml(
            r#"
[runtime]
mode = "dryrun"

[strategy]
"#,
        )
        .unwrap();

        assert_eq!(
            config.runtime.prediction_family,
            PredictionFamily::CryptoExpiry
        );
        assert_eq!(config.runtime.instrument_kind, InstrumentKind::UpDown);
        assert_eq!(config.runtime.venue, VenueKind::Polymarket);
    }

    #[test]
    fn runtime_contract_metadata_parses_snake_case() {
        let config = FullConfig::from_toml(
            r#"
[runtime]
mode = "dryrun"
prediction_family = "sports_pregame"
instrument_kind = "moneyline"
venue = "sportsbook"

[strategy]
"#,
        )
        .unwrap();

        assert_eq!(
            config.runtime.prediction_family,
            PredictionFamily::SportsPregame
        );
        assert_eq!(config.runtime.instrument_kind, InstrumentKind::Moneyline);
        assert_eq!(config.runtime.venue, VenueKind::Sportsbook);
    }

    #[test]
    fn venue_specific_market_metadata_is_required_when_rates_vary_by_market() {
        let predict = FullConfig::from_toml(
            r#"
[runtime]
mode = "backtest"
venue = "predict_fun"

[strategy]
"#,
        )
        .unwrap()
        .sim_executor_config();
        assert_eq!(
            predict.fee_schedule.formula,
            ploy_market_contracts::FeeFormula::Notional
        );
        assert!(!predict.fee_schedule.is_configured());
        assert_eq!(predict.fee_asset, None);

        let kalshi = FullConfig::from_toml(
            r#"
[runtime]
mode = "backtest"
venue = "kalshi"

[strategy]
"#,
        )
        .unwrap()
        .sim_executor_config();
        assert!(!kalshi.fee_schedule.is_configured());
        assert_eq!(
            kalshi.fee_asset,
            Some(ploy_market_contracts::FeeAsset::Collateral)
        );
    }

    #[test]
    fn predict_fun_market_fee_bps_and_asset_configure_simulation() {
        let predict = FullConfig::from_toml(
            r#"
[runtime]
mode = "backtest"
venue = "predict_fun"

[strategy]

[execution]
taker_fee_rate_bps = 200
maker_fee_rate_bps = 200
fee_asset = "collateral"
"#,
        )
        .unwrap()
        .sim_executor_config();

        assert!(predict.fee_schedule.is_configured());
        assert_eq!(
            predict.fee_schedule.calculate(
                dec!(10),
                dec!(0.50),
                ploy_market_contracts::LiquidityRole::Taker,
            ),
            dec!(0.10)
        );
        assert_eq!(
            predict.fee_asset,
            Some(ploy_market_contracts::FeeAsset::Collateral)
        );
    }

    #[test]
    fn maker_simulation_requires_maker_rate_metadata() {
        let missing = FullConfig::from_toml(
            r#"
[runtime]
mode = "backtest"
venue = "predict_fun"

[strategy]

[execution]
taker_fee_rate_bps = 200
fee_asset = "collateral"
liquidity_role = "maker"
"#,
        )
        .unwrap()
        .sim_executor_config();
        assert!(!missing.fee_schedule.is_configured());

        let complete = FullConfig::from_toml(
            r#"
[runtime]
mode = "backtest"
venue = "predict_fun"

[strategy]

[execution]
taker_fee_rate_bps = 200
maker_fee_rate_bps = 200
fee_asset = "collateral"
liquidity_role = "maker"
"#,
        )
        .unwrap()
        .sim_executor_config();
        assert!(complete.fee_schedule.is_configured());
    }

    #[test]
    fn custom_polymarket_requires_complete_fd_metadata() {
        let mut config = FullConfig::from_toml(
            r#"
[runtime]
mode = "backtest"

[strategy]
"#,
        )
        .unwrap();
        config.runtime.prediction_family = PredictionFamily::Custom(1);
        config.execution.taker_fee_rate = Some(0.04);
        assert!(!config.sim_executor_config().fee_schedule.is_configured());

        config.execution.fee_exponent = Some(1);
        config.execution.fee_taker_only = Some(true);
        let complete = config.sim_executor_config();
        assert!(complete.fee_schedule.is_configured());
        assert_eq!(complete.fee_schedule.taker_rate, dec!(0.04));
        assert_eq!(complete.fee_schedule.maker_rate, dec!(0));
    }

    #[test]
    fn polymarket_fd_override_is_atomic() {
        let mut config = FullConfig::from_toml(
            r#"
[runtime]
mode = "backtest"

[strategy]
"#,
        )
        .unwrap();
        assert!(config.sim_executor_config().fee_schedule.is_configured());

        config.execution.taker_fee_rate = Some(0.07);
        assert!(!config.sim_executor_config().fee_schedule.is_configured());

        config.execution.fee_exponent = Some(1);
        config.execution.fee_taker_only = Some(true);
        assert!(config.sim_executor_config().fee_schedule.is_configured());
    }

    #[test]
    fn invalid_or_ambiguous_fee_rates_fail_closed() {
        let mut config = FullConfig::from_toml(
            r#"
[runtime]
mode = "backtest"
venue = "predict_fun"

[strategy]

[execution]
fee_asset = "collateral"
"#,
        )
        .unwrap();
        config.execution.taker_fee_rate = Some(f64::NAN);
        assert!(!config.sim_executor_config().fee_schedule.is_configured());

        config.execution.taker_fee_rate = Some(-0.01);
        assert!(!config.sim_executor_config().fee_schedule.is_configured());

        config.execution.taker_fee_rate = Some(0.02);
        config.execution.taker_fee_rate_bps = Some(200);
        assert!(!config.sim_executor_config().fee_schedule.is_configured());
    }

    #[test]
    fn simulated_fee_overrides_capture_market_specific_schedule() {
        let config = FullConfig::from_toml(
            r#"
[runtime]
mode = "backtest"
venue = "kalshi"

[strategy]

[execution]
fee_formula = "per_contract"
taker_fee_rate = 0.03
maker_fee_rate = 0.01
fee_rounding_dp = 4
liquidity_role = "maker"
"#,
        )
        .unwrap()
        .sim_executor_config();

        assert_eq!(
            config.fee_schedule.formula,
            ploy_market_contracts::FeeFormula::PerContract
        );
        assert_eq!(
            config.liquidity_role,
            ploy_market_contracts::LiquidityRole::Maker
        );
        assert_eq!(
            config
                .fee_schedule
                .calculate(dec!(2), dec!(0.50), config.liquidity_role,),
            dec!(0.02)
        );
    }

    #[test]
    fn kalshi_rejects_non_venue_fee_semantics() {
        let mut config = FullConfig::from_toml(
            r#"
[runtime]
mode = "backtest"
venue = "kalshi"

[strategy]

[execution]
fee_formula = "probability_power"
taker_fee_rate = 0.07
"#,
        )
        .unwrap();
        assert!(config.sim_executor_config().fee_schedule.is_configured());

        config.execution.fee_exponent = Some(2);
        assert!(!config.sim_executor_config().fee_schedule.is_configured());
        config.execution.fee_exponent = Some(1);

        config.execution.fee_rounding_dp = Some(2);
        assert!(!config.sim_executor_config().fee_schedule.is_configured());
        config.execution.fee_rounding_dp = Some(4);

        config.execution.minimum_fee = Some(0.01);
        assert!(!config.sim_executor_config().fee_schedule.is_configured());
        config.execution.minimum_fee = Some(0.0);

        config.execution.fee_balance_precision_dp = Some(3);
        assert!(!config.sim_executor_config().fee_schedule.is_configured());
    }

    #[test]
    fn roadmap_config_family_parses() {
        let config_dir = strategy_config_dir();

        for file in [
            "02-pm5d-threelayer.unified.toml",
            "02-pm5d-threelayer.live.toml",
            "02-pm5d-threelayer.champion-dryrun.toml",
            "02-pm5d-threelayer.obi-soft-dryrun.toml",
            "02-pm5d-threelayer.obi-hard-dryrun.toml",
            "02-pm5d-threelayer.continuation-soft-dryrun.toml",
            "02-pm5d-threelayer.repricing-momentum-dryrun.toml",
            "02-pm5d.v1-dryrun.toml",
            "02-pm5d.v1-live.toml",
            "02-pm5d.v2-dryrun.toml",
            "02-pm5d.v2-live.toml",
            "02-pm5d.v3-dryrun.toml",
            "02-pm5d.v3-live.toml",
            "02-pm5d.v4-dryrun.toml",
            "02-pm5d.v4-live.toml",
            "05-reversal.dryrun.toml",
            "05-reversal.backtest.toml",
        ] {
            let path = config_dir.join(file);
            let config = FullConfig::from_file(path.to_str().unwrap()).unwrap();
            assert!(
                !config.strategy.symbols.is_empty(),
                "{file} should define at least one symbol"
            );
            assert!(
                !config.runtime.canonical_strategy_variant().is_empty(),
                "{file} should resolve to a runtime variant"
            );
        }
    }

    #[test]
    fn threelayer_live_config_matches_promoted_dryrun_with_bounded_risk_reductions() {
        let config_dir = strategy_config_dir();
        let dryrun_path =
            config_dir.join("02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml");
        let live_path = config_dir.join("02-pm5d-threelayer.live.toml");

        let dryrun_body = std::fs::read_to_string(&dryrun_path).unwrap();
        let live_body = std::fs::read_to_string(&live_path).unwrap();
        let mut dryrun: toml::Value = toml::from_str(&dryrun_body).unwrap();
        let live: toml::Value = toml::from_str(&live_body).unwrap();

        assert_eq!(
            dryrun
                .get("runtime")
                .and_then(|runtime| runtime.get("mode"))
                .and_then(toml::Value::as_str),
            Some("dryrun")
        );
        assert_eq!(
            live.get("runtime")
                .and_then(|runtime| runtime.get("mode"))
                .and_then(toml::Value::as_str),
            Some("live")
        );

        let dryrun_runtime = dryrun
            .get_mut("runtime")
            .and_then(toml::Value::as_table_mut)
            .unwrap();
        dryrun_runtime.insert("mode".to_string(), toml::Value::String("live".to_string()));
        for recording_key in [
            "record_market_updates_to",
            "record_market_updates_max_records",
            "record_market_updates_max_bytes",
        ] {
            dryrun_runtime.remove(recording_key);
        }

        let live_strategy = live
            .get("strategy")
            .and_then(toml::Value::as_table)
            .unwrap();
        let dryrun_strategy = dryrun
            .get_mut("strategy")
            .and_then(toml::Value::as_table_mut)
            .unwrap();
        assert!(
            dryrun_strategy["stake_usd"].as_float().unwrap()
                >= live_strategy["stake_usd"].as_float().unwrap()
        );
        assert!(
            dryrun_strategy["max_positions"].as_integer().unwrap()
                >= live_strategy["max_positions"].as_integer().unwrap()
        );
        assert_eq!(dryrun_strategy["max_daily_trades"].as_integer(), Some(0));
        assert!(live_strategy["max_daily_trades"].as_integer().unwrap() > 0);
        let dryrun_windows = dryrun_strategy["allowed_window_secs"].as_array().unwrap();
        assert!(live_strategy["allowed_window_secs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|window| dryrun_windows.contains(window)));
        for risk_key in [
            "stake_usd",
            "max_positions",
            "max_daily_trades",
            "allowed_window_secs",
        ] {
            dryrun_strategy.insert(risk_key.to_string(), live_strategy[risk_key].clone());
        }
        assert_eq!(dryrun, live);
    }

    #[test]
    fn threelayer_live_pair_uses_unthrottled_external_ticks() {
        let config_dir = strategy_config_dir();
        for file in [
            "02-pm5d-threelayer.live.toml",
            "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
        ] {
            let config = FullConfig::from_file(config_dir.join(file).to_str().unwrap()).unwrap();
            assert_eq!(
                config.runtime.market_data_source,
                MarketDataSource::Dual,
                "{file} must use direct ticks with DB-only fallback factors"
            );
            assert_eq!(
                config.runtime.throttle_hz, None,
                "{file} must evaluate every market tick"
            );
        }
    }

    #[test]
    fn threelayer_configs_require_official_settlement_for_backtests() {
        let config_dir = strategy_config_dir();

        for file in [
            "02-pm5d-threelayer.unified.toml",
            "02-pm5d-threelayer.live.toml",
            "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
        ] {
            let path = config_dir.join(file);
            let config = FullConfig::from_file(path.to_str().unwrap()).unwrap();
            assert!(
                config.backtest_data.require_official_settlement,
                "{file} must not optimize against spot-fallback settlement"
            );
        }
    }

    #[test]
    fn settlement_probability_config_carries_autofactor_handoff_score() {
        use crate::strategies::three_layer_model::{
            auto_settlement_formula_score, AutoSettlementFactorInputs,
        };

        let path = strategy_config_dir()
            .join("02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml");
        let config = FullConfig::from_file(path.to_str().unwrap()).unwrap();

        let runtime_score = config
            .strategy
            .three_layer_autofactor_runtime_score
            .as_deref()
            .expect("settlement probability dry-run config should carry an AutoFactor handoff");
        let formula_name = runtime_score
            .strip_prefix("autofactor_formula:")
            .unwrap_or(runtime_score);
        let mut normalized_name = formula_name;
        loop {
            if let Some(stripped) = normalized_name.strip_prefix("mut2_") {
                normalized_name = stripped;
            } else if let Some(stripped) = normalized_name.strip_prefix("mut_") {
                normalized_name = stripped;
            } else if let Some(stripped) = normalized_name.strip_prefix("mcts_") {
                normalized_name = stripped;
            } else {
                break;
            }
        }
        let supported_settlement_family = runtime_score.starts_with("autofactor_formula:")
            && (normalized_name.starts_with("auto_settlement_")
                || normalized_name.starts_with("amplitude_weighted_momentum_30s_sigma")
                || normalized_name.starts_with("poly_lag_pressure")
                || (normalized_name.starts_with("spread_adjusted_external_move")
                    && normalized_name != "spread_adjusted_external_move"));
        assert!(
            supported_settlement_family,
            "settlement probability dry-run config should use a supported settlement AutoFactor formula, got {runtime_score}"
        );
        let raw = auto_settlement_formula_score(
            runtime_score,
            AutoSettlementFactorInputs {
                settlement_edge: 0.06,
                entry_price: 0.30,
                distance_over_sigma: 0.20,
                direction_sign: 1.0,
                drift_30s: 0.004,
                sigma_horizon: 3.0,
                entry_capacity_ratio: 3.0,
                side_spread: 0.03,
                external_pressure: 1.0,
                pm_lag_secs: 6.0,
                iv_change_1m: 1.0,
            },
        )
        .expect("configured AutoFactor formula should be supported by the runtime scorer");
        assert!(
            raw.is_finite(),
            "configured AutoFactor formula should produce a finite runtime score"
        );
    }

    fn strategy_config_dir() -> PathBuf {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest_dir.join("../../config/strategies")
    }
}
