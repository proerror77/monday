//! Simulated order executor for backtesting and dry-run.
//!
//! Models realistic execution with:
//! - Bid-ask spread impact
//! - Partial fills based on market depth
//! - Market impact on large orders
//! - Fill delay simulation
//!
//! Used by both backtest (`HistoricalFeed + SimulatedExecutor`) and
//! dry-run (`LiveFeed + SimulatedExecutor`) modes.

use std::collections::HashMap;

use async_trait::async_trait;
use ploy_market_contracts::{BookLevel, FeeAccumulator, FeeAsset, FeeSchedule, LiquidityRole};
use ploy_trading::{FillRecord, IntentPurpose, TradeSide, TradingIntent};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::traits::{ExecutionReport, Executor, MarketUpdate};
const MIN_BINARY_PRICE: Decimal = dec!(0.01);
const MAX_BINARY_PRICE: Decimal = dec!(0.99);

/// Execution simulation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedExecutorConfig {
    /// Apply bid-ask spread to fill price.
    pub use_spread: bool,
    /// Spread width as fraction of mid (e.g. 0.02 = 2%).
    pub spread_pct: Decimal,
    /// Enable partial fills based on market depth.
    pub enable_partial_fills: bool,
    /// Available depth as multiple of typical order (e.g. 5.0).
    pub depth_multiple: Decimal,
    /// Minimum fill percentage when partial (e.g. 0.5 = 50%).
    pub min_fill_pct: Decimal,
    /// Enable market impact modelling.
    pub enable_market_impact: bool,
    /// Price impact coefficient per depth ratio.
    pub impact_coefficient: Decimal,
    /// Default market depth in shares when unknown.
    pub default_depth_shares: u64,
    /// Require a fresh observed PM quote with executable size before filling.
    ///
    /// When enabled, BUY fills consume `ask_size` at `ask`, and SELL fills
    /// consume `bid_size` at `bid`. This mirrors FAK-style top-of-book
    /// execution more closely than synthetic fixed-depth fills.
    pub require_lob_liquidity: bool,
    /// Haircut applied to observed full-depth book levels before sweep.
    pub visible_depth_haircut: Decimal,
    /// Maximum number of full-depth levels to sweep. Zero means unlimited.
    pub max_sweep_levels: usize,
    /// Venue-specific fee schedule used by backtest and dry-run fills.
    pub fee_schedule: FeeSchedule,
    /// Asset in which the venue charges the simulated fee.
    pub fee_asset: Option<FeeAsset>,
    /// Assumed maker/taker role for simulated fills.
    pub liquidity_role: LiquidityRole,
}

impl Default for SimulatedExecutorConfig {
    fn default() -> Self {
        Self {
            use_spread: false,
            spread_pct: dec!(0.02),
            enable_partial_fills: false,
            depth_multiple: dec!(5.0),
            min_fill_pct: dec!(0.5),
            enable_market_impact: false,
            impact_coefficient: dec!(0.1),
            default_depth_shares: 500,
            require_lob_liquidity: false,
            visible_depth_haircut: Decimal::ONE,
            max_sweep_levels: 0,
            fee_schedule: FeeSchedule::default(),
            fee_asset: Some(FeeAsset::Collateral),
            liquidity_role: LiquidityRole::Taker,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct QuoteLiquidity {
    bid: Option<Decimal>,
    ask: Option<Decimal>,
    bid_size: Option<Decimal>,
    ask_size: Option<Decimal>,
    bid_levels: Vec<BookLevel>,
    ask_levels: Vec<BookLevel>,
}

/// Simulated executor that models realistic fills.
pub struct SimulatedExecutor {
    config: SimulatedExecutorConfig,
    quotes: HashMap<String, QuoteLiquidity>,
}

impl SimulatedExecutor {
    pub fn new(config: SimulatedExecutorConfig) -> Self {
        Self {
            config,
            quotes: HashMap::new(),
        }
    }

    fn clamp_price(price: Decimal) -> Decimal {
        price.max(MIN_BINARY_PRICE).min(MAX_BINARY_PRICE)
    }

    /// Simulate a buy fill from a quoted executable price.
    fn simulate_buy(
        &self,
        signal_price: Decimal,
        quantity: Decimal,
        synthetic_mid: bool,
    ) -> (Decimal, Decimal, Decimal, Decimal) {
        let signal_price = Self::clamp_price(signal_price);
        let shares = quantity.max(Decimal::ZERO);
        let depth = Decimal::from(self.config.default_depth_shares);

        // Only synthesize spread when we do not already have an executable quote.
        let half_spread = if self.config.use_spread && synthetic_mid {
            signal_price * self.config.spread_pct / dec!(2)
        } else {
            Decimal::ZERO
        };
        let ask = Self::clamp_price(signal_price + half_spread);

        // Partial fill
        let (filled_qty, _is_partial) = self.fill_quantity(shares, depth);

        // Market impact
        let impact = if self.config.enable_market_impact && depth > Decimal::ZERO {
            let ratio = filled_qty / depth;
            ask * self.config.impact_coefficient * ratio
        } else {
            Decimal::ZERO
        };

        let fill_price = Self::clamp_price(ask + impact);
        let slippage = fill_price - signal_price;

        (fill_price, filled_qty, slippage, impact)
    }

    /// Simulate a sell fill from a quoted executable price.
    fn simulate_sell(
        &self,
        signal_price: Decimal,
        quantity: Decimal,
        synthetic_mid: bool,
    ) -> (Decimal, Decimal, Decimal, Decimal) {
        let signal_price = Self::clamp_price(signal_price);
        let shares = quantity.max(Decimal::ZERO);
        let depth = Decimal::from(self.config.default_depth_shares);

        let half_spread = if self.config.use_spread && synthetic_mid {
            signal_price * self.config.spread_pct / dec!(2)
        } else {
            Decimal::ZERO
        };
        let bid = Self::clamp_price(signal_price - half_spread);

        let (filled_qty, _is_partial) = self.fill_quantity(shares, depth);

        let impact = if self.config.enable_market_impact && depth > Decimal::ZERO {
            let ratio = filled_qty / depth;
            bid * self.config.impact_coefficient * ratio
        } else {
            Decimal::ZERO
        };

        let fill_price = Self::clamp_price(bid - impact);
        let slippage = signal_price - fill_price;

        (fill_price, filled_qty, slippage, impact)
    }

    fn sweep_levels(
        levels: &[BookLevel],
        shares: Decimal,
        side: TradeSide,
        limit: Decimal,
        visible_depth_haircut: Decimal,
        max_sweep_levels: usize,
        allow_partial: bool,
    ) -> Result<(Decimal, Decimal, Vec<(Decimal, Decimal)>), String> {
        let mut remaining = shares.max(Decimal::ZERO);
        let mut filled = Decimal::ZERO;
        let mut notional = Decimal::ZERO;
        let mut fee_legs = Vec::new();
        let level_limit = if max_sweep_levels == 0 {
            usize::MAX
        } else {
            max_sweep_levels
        };

        for level in levels.iter().take(level_limit) {
            if remaining <= Decimal::ZERO {
                break;
            }
            let price = Self::clamp_price(level.price);
            match side {
                TradeSide::Buy if price > limit => break,
                TradeSide::Sell if price < limit => break,
                _ => {}
            }

            let usable_size = level.size * visible_depth_haircut;
            if usable_size <= Decimal::ZERO {
                continue;
            }
            let take = remaining.min(usable_size);
            remaining -= take;
            filled += take;
            notional += take * price;
            fee_legs.push((take, price));
        }

        if filled <= Decimal::ZERO {
            return Err("No full-depth liquidity".into());
        }
        if !allow_partial && filled < shares {
            return Err(format!(
                "Insufficient full-depth liquidity for full fill: requested {shares}, available {filled}"
            ));
        }

        Ok((notional / filled, filled, fee_legs))
    }

    fn simulate_lob_fill(
        &self,
        intent: &TradingIntent,
        signal_price: Decimal,
    ) -> Result<
        (
            Decimal,
            Decimal,
            Decimal,
            Decimal,
            &'static str,
            Vec<(Decimal, Decimal)>,
        ),
        String,
    > {
        let quote = self
            .quotes
            .get(&intent.token_id)
            .ok_or_else(|| "No observed LOB quote".to_string())?;
        let requested = intent.quantity.max(Decimal::ZERO);

        match intent.side {
            TradeSide::Buy => {
                let limit = intent
                    .limit_price
                    .map(Self::clamp_price)
                    .unwrap_or_else(|| quote.ask.map(Self::clamp_price).unwrap_or(dec!(0.99)));
                if !quote.ask_levels.is_empty() {
                    let (avg_price, filled_qty, fee_legs) = Self::sweep_levels(
                        &quote.ask_levels,
                        requested,
                        TradeSide::Buy,
                        limit,
                        self.config.visible_depth_haircut,
                        self.config.max_sweep_levels,
                        self.config.enable_partial_fills,
                    )?;
                    let reference = Self::clamp_price(signal_price);
                    return Ok((
                        avg_price,
                        filled_qty,
                        avg_price - reference,
                        Decimal::ZERO,
                        "full_depth_sweep",
                        fee_legs,
                    ));
                }

                let ask = quote
                    .ask
                    .map(Self::clamp_price)
                    .ok_or_else(|| "No executable ask quote".to_string())?;
                let ask_size = quote
                    .ask_size
                    .ok_or_else(|| "No executable ask liquidity".to_string())?;
                if ask_size <= Decimal::ZERO {
                    return Err("No executable ask liquidity".into());
                }

                let limit = intent.limit_price.map(Self::clamp_price).unwrap_or(ask);
                if ask > limit {
                    return Err(format!("Best ask {ask} above limit {limit}"));
                }

                let filled_qty = requested.min(ask_size);
                if filled_qty <= Decimal::ZERO {
                    return Err("No liquidity".into());
                }
                if !self.config.enable_partial_fills && filled_qty < requested {
                    return Err(format!(
                        "Insufficient ask liquidity for full fill: requested {requested}, available {filled_qty}"
                    ));
                }
                let reference = Self::clamp_price(signal_price);
                Ok((
                    ask,
                    filled_qty,
                    ask - reference,
                    Decimal::ZERO,
                    "top_book_quote",
                    vec![(filled_qty, ask)],
                ))
            }
            TradeSide::Sell => {
                let limit = intent
                    .limit_price
                    .map(Self::clamp_price)
                    .unwrap_or_else(|| quote.bid.map(Self::clamp_price).unwrap_or(dec!(0.01)));
                if !quote.bid_levels.is_empty() {
                    let (avg_price, filled_qty, fee_legs) = Self::sweep_levels(
                        &quote.bid_levels,
                        requested,
                        TradeSide::Sell,
                        limit,
                        self.config.visible_depth_haircut,
                        self.config.max_sweep_levels,
                        self.config.enable_partial_fills,
                    )?;
                    let reference = Self::clamp_price(signal_price);
                    return Ok((
                        avg_price,
                        filled_qty,
                        reference - avg_price,
                        Decimal::ZERO,
                        "full_depth_sweep",
                        fee_legs,
                    ));
                }

                let bid = quote
                    .bid
                    .map(Self::clamp_price)
                    .ok_or_else(|| "No executable bid quote".to_string())?;
                let bid_size = quote
                    .bid_size
                    .ok_or_else(|| "No executable bid liquidity".to_string())?;
                if bid_size <= Decimal::ZERO {
                    return Err("No executable bid liquidity".into());
                }

                let limit = intent.limit_price.map(Self::clamp_price).unwrap_or(bid);
                if bid < limit {
                    return Err(format!("Best bid {bid} below limit {limit}"));
                }

                let filled_qty = requested.min(bid_size);
                if filled_qty <= Decimal::ZERO {
                    return Err("No liquidity".into());
                }
                if !self.config.enable_partial_fills && filled_qty < requested {
                    return Err(format!(
                        "Insufficient bid liquidity for full fill: requested {requested}, available {filled_qty}"
                    ));
                }
                let reference = Self::clamp_price(signal_price);
                Ok((
                    bid,
                    filled_qty,
                    reference - bid,
                    Decimal::ZERO,
                    "top_book_quote",
                    vec![(filled_qty, bid)],
                ))
            }
        }
    }

    /// Determine fill quantity given requested shares and market depth.
    fn fill_quantity(&self, requested: Decimal, depth: Decimal) -> (Decimal, bool) {
        if !self.config.enable_partial_fills || depth <= Decimal::ZERO {
            return (requested, false);
        }

        let depth_multiple = self.config.depth_multiple.max(dec!(0.000001));
        let typical = (depth / depth_multiple).max(dec!(100));

        if requested <= typical {
            (requested, false)
        } else if requested <= depth {
            let ratio = (depth / requested).min(Decimal::ONE);
            let min = requested * self.config.min_fill_pct;
            let filled = (requested * ratio).max(min).min(requested);
            (filled, filled < requested)
        } else {
            let min = requested * self.config.min_fill_pct;
            let filled = depth.max(min).min(requested);
            (filled, true)
        }
    }
}

#[async_trait]
impl Executor for SimulatedExecutor {
    fn observe_market_update(&mut self, update: &MarketUpdate) {
        if let MarketUpdate::Quote {
            token_id,
            bid,
            ask,
            bid_size,
            ask_size,
            bid_levels,
            ask_levels,
            ..
        } = update
        {
            let clears_book = bid.is_none() && ask.is_none();
            let previous = self
                .quotes
                .get(token_id.as_ref())
                .cloned()
                .unwrap_or_default();
            self.quotes.insert(
                token_id.to_string(),
                QuoteLiquidity {
                    bid: *bid,
                    ask: *ask,
                    bid_size: if clears_book {
                        None
                    } else {
                        bid_size.or(previous.bid_size)
                    },
                    ask_size: if clears_book {
                        None
                    } else {
                        ask_size.or(previous.ask_size)
                    },
                    bid_levels: if clears_book {
                        Vec::new()
                    } else if bid_levels.is_empty() {
                        previous.bid_levels
                    } else {
                        bid_levels.clone()
                    },
                    ask_levels: if clears_book {
                        Vec::new()
                    } else if ask_levels.is_empty() {
                        previous.ask_levels
                    } else {
                        ask_levels.clone()
                    },
                },
            );
        }
    }

    async fn submit(&mut self, intent: &TradingIntent, order_id: &str) -> ExecutionReport {
        let signal_price = intent.limit_price.unwrap_or(dec!(0.50));
        let synthetic_mid = intent.limit_price.is_none();

        // Settlement exits bypass spread/impact simulation
        let is_settlement = intent.purpose == IntentPurpose::Exit
            && (signal_price == Decimal::ZERO || signal_price == Decimal::ONE);

        if !is_settlement && !self.config.fee_schedule.is_configured() {
            return ExecutionReport {
                order_id: order_id.to_string(),
                fill: None,
                rejected: true,
                rejection_reason: Some(
                    "Market fee metadata is required for simulated execution".into(),
                ),
                slippage: None,
                market_impact: None,
                price_basis: None,
            };
        }
        let charges_fee = self
            .config
            .fee_schedule
            .rate_for(self.config.liquidity_role)
            > Decimal::ZERO;
        if !is_settlement && charges_fee && self.config.fee_asset.is_none() {
            return ExecutionReport {
                order_id: order_id.to_string(),
                fill: None,
                rejected: true,
                rejection_reason: Some(
                    "Market fee asset metadata is required for simulated execution".into(),
                ),
                slippage: None,
                market_impact: None,
                price_basis: None,
            };
        }
        if !is_settlement && charges_fee && self.config.fee_asset == Some(FeeAsset::Shares) {
            return ExecutionReport {
                order_id: order_id.to_string(),
                fill: None,
                rejected: true,
                rejection_reason: Some(
                    "Share-denominated fees require net-position accounting".into(),
                ),
                slippage: None,
                market_impact: None,
                price_basis: None,
            };
        }

        let simulated = if is_settlement {
            Ok((
                signal_price,
                intent.quantity,
                Decimal::ZERO,
                Decimal::ZERO,
                "settlement",
                Vec::new(),
            ))
        } else if self.config.require_lob_liquidity {
            self.simulate_lob_fill(intent, signal_price)
        } else {
            let (fill_price, filled_qty, slippage, impact) = match intent.side {
                TradeSide::Buy => self.simulate_buy(signal_price, intent.quantity, synthetic_mid),
                TradeSide::Sell => self.simulate_sell(signal_price, intent.quantity, synthetic_mid),
            };
            Ok((
                fill_price,
                filled_qty,
                slippage,
                impact,
                if synthetic_mid {
                    "synthetic_mid"
                } else {
                    "signal_limit"
                },
                vec![(filled_qty, fill_price)],
            ))
        };

        let (fill_price, filled_qty, slippage, impact, price_basis, fee_legs) = match simulated {
            Ok(fill) => fill,
            Err(reason) => {
                return ExecutionReport {
                    order_id: order_id.to_string(),
                    fill: None,
                    rejected: true,
                    rejection_reason: Some(reason),
                    slippage: None,
                    market_impact: None,
                    price_basis: None,
                };
            }
        };

        if filled_qty <= Decimal::ZERO {
            return ExecutionReport {
                order_id: order_id.to_string(),
                fill: None,
                rejected: true,
                rejection_reason: Some("No liquidity".into()),
                slippage: None,
                market_impact: None,
                price_basis: None,
            };
        }

        let fee = if is_settlement {
            Decimal::ZERO
        } else {
            let revenue_sign = match intent.side {
                TradeSide::Buy => -Decimal::ONE,
                TradeSide::Sell => Decimal::ONE,
            };
            let mut accumulator = FeeAccumulator::default();
            let fee = fee_legs
                .iter()
                .map(|(leg_quantity, leg_price)| {
                    self.config
                        .fee_schedule
                        .charge(
                            *leg_quantity,
                            *leg_price,
                            self.config.liquidity_role,
                            *leg_quantity * *leg_price * revenue_sign,
                            &mut accumulator,
                        )
                        .expect("fee schedule was checked above")
                        .net_fee
                })
                .sum();
            fee
        };

        let fill = FillRecord {
            fill_id: Uuid::new_v4().to_string(),
            order_id: order_id.to_string(),
            token_id: intent.token_id.clone(),
            side: intent.side.clone(),
            quantity: filled_qty,
            price: fill_price,
            fee,
            timestamp: intent.created_at, // Use intent time for backtest consistency
        };

        ExecutionReport {
            order_id: order_id.to_string(),
            fill: Some(fill),
            rejected: false,
            rejection_reason: None,
            slippage: Some(slippage),
            market_impact: Some(impact),
            price_basis: Some(price_basis),
        }
    }

    async fn cancel(&mut self, _order_id: &str) -> bool {
        true // Simulated cancel always succeeds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ploy_trading::IntentPurpose;

    fn test_intent(side: TradeSide, price: Decimal, qty: Decimal) -> TradingIntent {
        TradingIntent {
            intent_id: "test-intent".into(),
            deployment_id: "test".into(),
            market_id: "market-1".into(),
            token_id: "token-1".into(),
            side,
            quantity: qty,
            limit_price: Some(price),
            purpose: IntentPurpose::Entry,
            created_at: Utc::now(),
        }
    }

    fn quote_update(
        bid: Option<Decimal>,
        ask: Option<Decimal>,
        bid_size: Option<Decimal>,
        ask_size: Option<Decimal>,
    ) -> MarketUpdate {
        MarketUpdate::Quote {
            token_id: "token-1".into(),
            bid,
            ask,
            bid_size,
            ask_size,
            bid_levels: Vec::new(),
            ask_levels: Vec::new(),
            ts: Utc::now(),
        }
    }

    fn quote_update_with_levels(
        bid_levels: Vec<BookLevel>,
        ask_levels: Vec<BookLevel>,
    ) -> MarketUpdate {
        let bid = bid_levels.first().map(|level| level.price);
        let ask = ask_levels.first().map(|level| level.price);
        let bid_size = bid_levels.first().map(|level| level.size);
        let ask_size = ask_levels.first().map(|level| level.size);
        MarketUpdate::Quote {
            token_id: "token-1".into(),
            bid,
            ask,
            bid_size,
            ask_size,
            bid_levels,
            ask_levels,
            ts: Utc::now(),
        }
    }

    fn level(price: Decimal, size: Decimal) -> BookLevel {
        BookLevel { price, size }
    }

    #[tokio::test]
    async fn buy_applies_quote_price_and_impact() {
        let config = SimulatedExecutorConfig {
            enable_market_impact: true,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(25));

        let report = exec.submit(&intent, "test-order-1").await;
        assert!(!report.rejected);
        let fill = report.fill.unwrap();
        // Fill price should be above the quoted ask because of impact.
        assert!(
            fill.price > dec!(0.50),
            "fill={} should be > 0.50",
            fill.price
        );
        assert!(report.slippage.unwrap() > Decimal::ZERO);
        assert_eq!(fill.price.round_dp(4), dec!(0.5025));
    }

    #[tokio::test]
    async fn sell_applies_quote_price_and_impact() {
        let config = SimulatedExecutorConfig {
            enable_market_impact: true,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Sell, dec!(0.50), dec!(25));

        let report = exec.submit(&intent, "test-order-2").await;
        assert!(!report.rejected);
        let fill = report.fill.unwrap();
        // Fill price should be below the quoted bid only because of impact.
        assert!(
            fill.price < dec!(0.50),
            "fill={} should be < 0.50",
            fill.price
        );
        assert_eq!(fill.price.round_dp(4), dec!(0.4975));
    }

    #[tokio::test]
    async fn no_spread_means_signal_price() {
        let config = SimulatedExecutorConfig {
            use_spread: false,
            enable_market_impact: false,
            enable_partial_fills: false,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Buy, dec!(0.60), dec!(10));

        let report = exec.submit(&intent, "test-order-3").await;
        let fill = report.fill.unwrap();
        assert_eq!(fill.price, dec!(0.60));
        assert_eq!(report.slippage.unwrap(), Decimal::ZERO);
    }

    #[tokio::test]
    async fn lob_buy_consumes_only_executable_ask_size() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            enable_partial_fills: true,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update(
            Some(dec!(0.49)),
            Some(dec!(0.50)),
            Some(dec!(50)),
            Some(dec!(7.5)),
        ));
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(25));

        let report = exec.submit(&intent, "test-order-lob-buy").await;
        assert!(!report.rejected);
        let fill = report.fill.expect("partial top-of-book fill");
        assert_eq!(fill.price, dec!(0.50));
        assert_eq!(fill.quantity, dec!(7.5));
    }

    #[tokio::test]
    async fn lob_buy_rejects_when_ask_is_not_crossable() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update(
            Some(dec!(0.49)),
            Some(dec!(0.52)),
            Some(dec!(50)),
            Some(dec!(50)),
        ));
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(25));

        let report = exec.submit(&intent, "test-order-lob-buy-reject").await;
        assert!(report.rejected);
        assert!(report.fill.is_none());
        assert_eq!(
            report.rejection_reason.as_deref(),
            Some("Best ask 0.52 above limit 0.50")
        );
    }

    #[tokio::test]
    async fn lob_buy_rejects_when_size_is_missing() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update(
            Some(dec!(0.49)),
            Some(dec!(0.50)),
            Some(dec!(50)),
            None,
        ));
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(25));

        let report = exec.submit(&intent, "test-order-lob-no-size").await;
        assert!(report.rejected);
        assert_eq!(
            report.rejection_reason.as_deref(),
            Some("No executable ask liquidity")
        );
    }

    #[tokio::test]
    async fn full_depth_buy_sweeps_multiple_ask_levels() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update_with_levels(
            vec![level(dec!(0.49), dec!(20))],
            vec![level(dec!(0.50), dec!(5)), level(dec!(0.52), dec!(10))],
        ));
        let intent = test_intent(TradeSide::Buy, dec!(0.53), dec!(12));

        let report = exec.submit(&intent, "test-order-full-depth-buy").await;
        assert!(!report.rejected);
        assert_eq!(report.price_basis, Some("full_depth_sweep"));
        let fill = report.fill.expect("full-depth fill");
        assert_eq!(fill.quantity, dec!(12));
        assert_eq!(fill.price.round_dp(6), dec!(0.511667));
        assert_eq!(report.slippage.unwrap().round_dp(6), dec!(-0.018333));
    }

    #[tokio::test]
    async fn full_depth_buy_respects_haircut_and_max_levels() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            enable_partial_fills: true,
            visible_depth_haircut: dec!(0.5),
            max_sweep_levels: 1,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update_with_levels(
            Vec::new(),
            vec![level(dec!(0.50), dec!(10)), level(dec!(0.51), dec!(10))],
        ));
        let intent = test_intent(TradeSide::Buy, dec!(0.55), dec!(12));

        let report = exec.submit(&intent, "test-order-full-depth-haircut").await;
        assert!(!report.rejected);
        assert_eq!(report.price_basis, Some("full_depth_sweep"));
        let fill = report.fill.expect("haircut-limited fill");
        assert_eq!(fill.quantity, dec!(5.0));
        assert_eq!(fill.price, dec!(0.50));
    }

    #[tokio::test]
    async fn price_only_quote_does_not_erase_last_observed_liquidity() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            enable_partial_fills: true,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update(
            Some(dec!(0.49)),
            Some(dec!(0.50)),
            Some(dec!(50)),
            Some(dec!(8)),
        ));
        exec.observe_market_update(&quote_update(
            Some(dec!(0.48)),
            Some(dec!(0.50)),
            None,
            None,
        ));
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(25));

        let report = exec.submit(&intent, "test-order-lob-price-only").await;
        assert!(!report.rejected);
        let fill = report.fill.expect("last known top-of-book size");
        assert_eq!(fill.price, dec!(0.50));
        assert_eq!(fill.quantity, dec!(8));
    }

    #[tokio::test]
    async fn empty_quote_clears_last_observed_liquidity() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            enable_partial_fills: true,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update(
            Some(dec!(0.49)),
            Some(dec!(0.50)),
            Some(dec!(50)),
            Some(dec!(8)),
        ));
        exec.observe_market_update(&quote_update(None, None, None, None));

        let report = exec
            .submit(
                &test_intent(TradeSide::Buy, dec!(0.50), dec!(25)),
                "test-order-empty-book",
            )
            .await;

        assert!(report.rejected);
        assert!(report.fill.is_none());
    }

    #[tokio::test]
    async fn lob_sell_consumes_only_executable_bid_size() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            enable_partial_fills: true,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update(
            Some(dec!(0.54)),
            Some(dec!(0.55)),
            Some(dec!(6)),
            Some(dec!(50)),
        ));
        let intent = test_intent(TradeSide::Sell, dec!(0.54), dec!(20));

        let report = exec.submit(&intent, "test-order-lob-sell").await;
        assert!(!report.rejected);
        let fill = report.fill.expect("partial bid fill");
        assert_eq!(fill.price, dec!(0.54));
        assert_eq!(fill.quantity, dec!(6));
    }

    #[tokio::test]
    async fn lob_buy_rejects_partial_when_partial_fills_disabled() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            enable_partial_fills: false,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update(
            Some(dec!(0.49)),
            Some(dec!(0.50)),
            Some(dec!(50)),
            Some(dec!(7.5)),
        ));
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(25));

        let report = exec.submit(&intent, "test-order-lob-buy-no-partial").await;
        assert!(report.rejected);
        assert!(report.fill.is_none());
        assert_eq!(
            report.rejection_reason.as_deref(),
            Some("Insufficient ask liquidity for full fill: requested 25, available 7.5")
        );
    }

    #[tokio::test]
    async fn full_depth_buy_rejects_partial_when_partial_fills_disabled() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            enable_partial_fills: false,
            visible_depth_haircut: dec!(0.5),
            max_sweep_levels: 1,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update_with_levels(
            Vec::new(),
            vec![level(dec!(0.50), dec!(10)), level(dec!(0.51), dec!(10))],
        ));
        let intent = test_intent(TradeSide::Buy, dec!(0.55), dec!(12));

        let report = exec
            .submit(&intent, "test-order-full-depth-no-partial")
            .await;
        assert!(report.rejected);
        assert!(report.fill.is_none());
        assert_eq!(
            report.rejection_reason.as_deref(),
            Some("Insufficient full-depth liquidity for full fill: requested 12, available 5.0")
        );
    }

    #[tokio::test]
    async fn settlement_exit_has_no_fee() {
        let mut exec = SimulatedExecutor::new(SimulatedExecutorConfig::default());
        let mut intent = test_intent(TradeSide::Sell, dec!(1.00), dec!(10));
        intent.purpose = IntentPurpose::Exit;

        let report = exec.submit(&intent, "test-order-4").await;
        let fill = report.fill.expect("settlement fill");
        assert_eq!(fill.price, dec!(1.00));
        assert_eq!(fill.fee, Decimal::ZERO);
    }

    #[tokio::test]
    async fn entry_fee_uses_configured_venue_schedule() {
        let config = SimulatedExecutorConfig {
            use_spread: false,
            enable_market_impact: false,
            enable_partial_fills: false,
            fee_schedule: ploy_market_contracts::FeeSchedule::polymarket_v2(dec!(0.07), 1, true),
            liquidity_role: ploy_market_contracts::LiquidityRole::Taker,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(10));

        let report = exec.submit(&intent, "test-order-5").await;
        let fill = report.fill.expect("fill");
        assert_eq!(fill.fee, dec!(0.175));
    }

    #[tokio::test]
    async fn maker_fill_uses_maker_rate() {
        let config = SimulatedExecutorConfig {
            fee_schedule: ploy_market_contracts::FeeSchedule::new(
                ploy_market_contracts::FeeFormula::Notional,
                dec!(0.005),
                dec!(0.02),
                ploy_market_contracts::FeeRounding::Exact,
                Decimal::ZERO,
            ),
            liquidity_role: ploy_market_contracts::LiquidityRole::Maker,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(10));

        let fill = exec
            .submit(&intent, "test-order-maker-fee")
            .await
            .fill
            .expect("fill");

        assert_eq!(fill.fee, dec!(0.025));
    }

    #[tokio::test]
    async fn missing_market_fee_metadata_rejects_fill() {
        let config = SimulatedExecutorConfig {
            fee_schedule: ploy_market_contracts::FeeSchedule::unconfigured(
                ploy_market_contracts::FeeFormula::Notional,
            ),
            fee_asset: Some(ploy_market_contracts::FeeAsset::Collateral),
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(10));

        let report = exec.submit(&intent, "missing-market-fee").await;

        assert!(report.rejected);
        assert_eq!(
            report.rejection_reason.as_deref(),
            Some("Market fee metadata is required for simulated execution")
        );
    }

    #[tokio::test]
    async fn share_denomination_fails_closed_until_fill_contract_tracks_fee_asset() {
        let config = SimulatedExecutorConfig {
            fee_asset: Some(ploy_market_contracts::FeeAsset::Shares),
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(10));

        let report = exec.submit(&intent, "share-fee").await;

        assert!(report.rejected);
        assert_eq!(
            report.rejection_reason.as_deref(),
            Some("Share-denominated fees require net-position accounting")
        );
    }

    #[tokio::test]
    async fn explicitly_fee_free_market_does_not_require_fee_asset() {
        let config = SimulatedExecutorConfig {
            fee_schedule: ploy_market_contracts::FeeSchedule::new(
                ploy_market_contracts::FeeFormula::Notional,
                dec!(0),
                dec!(0),
                ploy_market_contracts::FeeRounding::Exact,
                dec!(0),
            ),
            fee_asset: None,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(10));

        let fill = exec
            .submit(&intent, "fee-free")
            .await
            .fill
            .expect("fee-free fill");

        assert_eq!(fill.fee, dec!(0));
    }

    #[tokio::test]
    async fn kalshi_match_legs_share_one_order_rounding_accumulator() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            fee_schedule: ploy_market_contracts::FeeSchedule::new(
                ploy_market_contracts::FeeFormula::ProbabilityPower { exponent: 1 },
                dec!(0),
                dec!(0.07),
                ploy_market_contracts::FeeRounding::Ceiling { decimal_places: 4 },
                dec!(0),
            )
            .with_kalshi_balance_rounding(2),
            fee_asset: Some(ploy_market_contracts::FeeAsset::Collateral),
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update_with_levels(
            Vec::new(),
            vec![
                level(dec!(0.50), dec!(0.30)),
                level(dec!(0.50), dec!(0.30)),
                level(dec!(0.50), dec!(0.30)),
            ],
        ));
        let intent = test_intent(TradeSide::Buy, dec!(0.50), dec!(0.90));

        let fill = exec
            .submit(&intent, "kalshi-partials")
            .await
            .fill
            .expect("fill");

        assert_eq!(fill.fee, dec!(0.0200));
    }

    #[tokio::test]
    async fn full_depth_probability_fees_are_rounded_per_match_level() {
        let config = SimulatedExecutorConfig {
            require_lob_liquidity: true,
            fee_schedule: ploy_market_contracts::FeeSchedule::polymarket_v2(dec!(0.07), 1, true),
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        exec.observe_market_update(&quote_update_with_levels(
            Vec::new(),
            vec![level(dec!(0.20), dec!(1)), level(dec!(0.80), dec!(1))],
        ));
        let intent = test_intent(TradeSide::Buy, dec!(0.90), dec!(2));

        let fill = exec
            .submit(&intent, "multi-level-fee")
            .await
            .fill
            .expect("fill");

        assert_eq!(fill.price, dec!(0.50));
        assert_eq!(fill.fee, dec!(0.02240));
    }

    #[tokio::test]
    async fn preserves_fractional_share_quantity() {
        let config = SimulatedExecutorConfig {
            use_spread: false,
            enable_market_impact: false,
            enable_partial_fills: false,
            ..Default::default()
        };
        let mut exec = SimulatedExecutor::new(config);
        let intent = test_intent(TradeSide::Buy, dec!(0.60), dec!(41.666667));

        let report = exec.submit(&intent, "test-order-6").await;
        let fill = report.fill.expect("fill");
        assert_eq!(fill.quantity, dec!(41.666667));
        assert_eq!(fill.price, dec!(0.60));
    }
}
