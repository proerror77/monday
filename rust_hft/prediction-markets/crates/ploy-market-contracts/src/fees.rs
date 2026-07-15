use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiquidityRole {
    Maker,
    Taker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeAsset {
    Collateral,
    Shares,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeFormula {
    ProbabilityPower { exponent: u32 },
    Notional,
    PerContract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeRounding {
    Exact,
    Truncate { decimal_places: u32 },
    Ceiling { decimal_places: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeSettlement {
    TradeFeeOnly,
    KalshiBalance {
        balance_decimal_places: u32,
        rebate_decimal_places: u32,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct FeeAccumulator {
    pub rounding_overpayment: Decimal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct FeeCharge {
    pub trade_fee: Decimal,
    pub rounding_fee: Decimal,
    pub rebate: Decimal,
    pub net_fee: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FeeSchedule {
    pub formula: FeeFormula,
    pub maker_rate: Decimal,
    pub taker_rate: Decimal,
    pub rounding: FeeRounding,
    pub minimum_fee: Decimal,
    pub settlement: FeeSettlement,
    #[serde(default)]
    configured: bool,
}

impl FeeSchedule {
    #[must_use]
    pub const fn new(
        formula: FeeFormula,
        maker_rate: Decimal,
        taker_rate: Decimal,
        rounding: FeeRounding,
        minimum_fee: Decimal,
    ) -> Self {
        Self {
            formula,
            maker_rate,
            taker_rate,
            rounding,
            minimum_fee,
            settlement: FeeSettlement::TradeFeeOnly,
            configured: true,
        }
    }

    #[must_use]
    pub const fn unconfigured(formula: FeeFormula) -> Self {
        Self {
            formula,
            maker_rate: Decimal::ZERO,
            taker_rate: Decimal::ZERO,
            rounding: FeeRounding::Exact,
            minimum_fee: Decimal::ZERO,
            settlement: FeeSettlement::TradeFeeOnly,
            configured: false,
        }
    }

    #[must_use]
    pub const fn is_configured(self) -> bool {
        self.configured
    }

    #[must_use]
    pub const fn require_market_metadata(mut self) -> Self {
        self.configured = false;
        self
    }

    #[must_use]
    pub const fn rate_for(self, liquidity_role: LiquidityRole) -> Decimal {
        match liquidity_role {
            LiquidityRole::Maker => self.maker_rate,
            LiquidityRole::Taker => self.taker_rate,
        }
    }

    #[must_use]
    pub const fn with_kalshi_balance_rounding(mut self, balance_decimal_places: u32) -> Self {
        self.settlement = FeeSettlement::KalshiBalance {
            balance_decimal_places,
            rebate_decimal_places: 2,
        };
        self
    }

    #[must_use]
    pub fn polymarket_v2(rate: Decimal, exponent: u32, taker_only: bool) -> Self {
        Self::new(
            FeeFormula::ProbabilityPower { exponent },
            if taker_only { Decimal::ZERO } else { rate },
            rate,
            FeeRounding::Truncate { decimal_places: 5 },
            Decimal::ZERO,
        )
    }

    #[must_use]
    pub fn calculate(
        self,
        quantity: Decimal,
        price: Decimal,
        liquidity_role: LiquidityRole,
    ) -> Decimal {
        if !self.configured {
            return Decimal::ZERO;
        }
        if quantity <= Decimal::ZERO || price < Decimal::ZERO || price > Decimal::ONE {
            return Decimal::ZERO;
        }

        let rate = self.rate_for(liquidity_role);
        if rate <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let raw = match self.formula {
            FeeFormula::ProbabilityPower { exponent } => {
                let mut factor = Decimal::ONE;
                let base = price * (Decimal::ONE - price);
                for _ in 0..exponent {
                    factor *= base;
                }
                quantity * rate * factor
            }
            FeeFormula::Notional => quantity * price * rate,
            FeeFormula::PerContract => quantity * rate,
        };

        let rounded = match self.rounding {
            FeeRounding::Exact => raw,
            FeeRounding::Truncate { decimal_places } => {
                raw.round_dp_with_strategy(decimal_places, RoundingStrategy::ToZero)
            }
            FeeRounding::Ceiling { decimal_places } => {
                raw.round_dp_with_strategy(decimal_places, RoundingStrategy::ToPositiveInfinity)
            }
        };

        if rounded > Decimal::ZERO {
            rounded.max(self.minimum_fee)
        } else {
            Decimal::ZERO
        }
    }

    #[must_use]
    pub fn charge(
        self,
        quantity: Decimal,
        price: Decimal,
        liquidity_role: LiquidityRole,
        signed_revenue: Decimal,
        accumulator: &mut FeeAccumulator,
    ) -> Option<FeeCharge> {
        if !self.configured {
            return None;
        }

        let trade_fee = self.calculate(quantity, price, liquidity_role);
        let mut charge = FeeCharge {
            trade_fee,
            net_fee: trade_fee,
            ..FeeCharge::default()
        };

        let FeeSettlement::KalshiBalance {
            balance_decimal_places,
            rebate_decimal_places,
        } = self.settlement
        else {
            return Some(charge);
        };

        let balance_change = signed_revenue - trade_fee;
        let rounded_balance = balance_change
            .round_dp_with_strategy(balance_decimal_places, RoundingStrategy::ToNegativeInfinity);
        charge.rounding_fee = balance_change - rounded_balance;
        accumulator.rounding_overpayment += charge.rounding_fee;

        let rebate_unit = Decimal::new(1, rebate_decimal_places);
        if rebate_unit > Decimal::ZERO && accumulator.rounding_overpayment >= rebate_unit {
            let rebate_units = (accumulator.rounding_overpayment / rebate_unit).floor();
            charge.rebate = rebate_units * rebate_unit;
            accumulator.rounding_overpayment -= charge.rebate;
        }
        charge.net_fee = (trade_fee + charge.rounding_fee - charge.rebate).max(Decimal::ZERO);
        Some(charge)
    }
}

impl Default for FeeSchedule {
    fn default() -> Self {
        Self::polymarket_v2(Decimal::new(7, 2), 1, true)
    }
}

#[must_use]
pub fn polymarket_crypto_taker_fee_per_share(price: f64) -> f64 {
    if price.is_finite() && (0.0..=1.0).contains(&price) {
        0.07 * price * (1.0 - price)
    } else {
        0.0
    }
}

#[must_use]
pub fn polymarket_crypto_taker_fee_cost(quantity: f64, price: f64) -> f64 {
    let Some(quantity) = Decimal::from_f64(quantity).filter(|quantity| *quantity > Decimal::ZERO)
    else {
        return 0.0;
    };
    let Some(price) =
        Decimal::from_f64(price).filter(|price| *price >= Decimal::ZERO && *price <= Decimal::ONE)
    else {
        return 0.0;
    };
    FeeSchedule::polymarket_v2(Decimal::new(7, 2), 1, true)
        .calculate(quantity, price, LiquidityRole::Taker)
        .to_f64()
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::{FeeAccumulator, FeeFormula, FeeRounding, FeeSchedule, LiquidityRole};

    #[test]
    fn polymarket_v2_uses_market_rate_exponent_and_taker_only_flag() {
        let fees = FeeSchedule::polymarket_v2(dec!(0.07), 1, true);

        assert_eq!(
            fees.calculate(dec!(10), dec!(0.50), LiquidityRole::Taker),
            dec!(0.17500)
        );
        assert_eq!(
            fees.calculate(dec!(10), dec!(0.50), LiquidityRole::Maker),
            dec!(0)
        );
    }

    #[test]
    fn polymarket_v2_truncates_to_five_decimals_and_drops_sub_tick_fees() {
        let fees = FeeSchedule::polymarket_v2(dec!(0.07), 1, true);

        assert_eq!(
            fees.calculate(dec!(1), dec!(0.01), LiquidityRole::Taker),
            dec!(0.00069)
        );
        assert_eq!(
            fees.calculate(dec!(1), dec!(0.0001), LiquidityRole::Taker),
            dec!(0)
        );
    }

    #[test]
    fn predict_fun_can_model_market_fee_bps_without_polymarket_curve() {
        let fees = FeeSchedule::new(
            FeeFormula::Notional,
            dec!(0.02),
            dec!(0.02),
            FeeRounding::Exact,
            dec!(0),
        );

        assert_eq!(
            fees.calculate(dec!(10), dec!(0.50), LiquidityRole::Taker),
            dec!(0.10)
        );
    }

    #[test]
    fn kalshi_style_fees_support_role_specific_rates_and_ceiling() {
        let fees = FeeSchedule::new(
            FeeFormula::ProbabilityPower { exponent: 1 },
            dec!(0.0175),
            dec!(0.07),
            FeeRounding::Ceiling { decimal_places: 4 },
            dec!(0),
        );

        assert_eq!(
            fees.calculate(dec!(1), dec!(0.33), LiquidityRole::Taker),
            dec!(0.0155)
        );
        assert_eq!(
            fees.calculate(dec!(1), dec!(0.33), LiquidityRole::Maker),
            dec!(0.0039)
        );
    }

    #[test]
    fn kalshi_balance_rounding_accumulates_and_rebates_per_order() {
        let fees = FeeSchedule::new(
            FeeFormula::ProbabilityPower { exponent: 1 },
            dec!(0),
            dec!(0.07),
            FeeRounding::Ceiling { decimal_places: 4 },
            dec!(0),
        )
        .with_kalshi_balance_rounding(2);
        let mut accumulator = FeeAccumulator::default();

        let first = fees
            .charge(
                dec!(0.30),
                dec!(0.50),
                LiquidityRole::Taker,
                dec!(-0.15),
                &mut accumulator,
            )
            .expect("configured fee");
        assert_eq!(first.trade_fee, dec!(0.0053));
        assert_eq!(first.rounding_fee, dec!(0.0047));
        assert_eq!(first.rebate, dec!(0));
        assert_eq!(first.net_fee, dec!(0.0100));

        let second = fees
            .charge(
                dec!(0.30),
                dec!(0.50),
                LiquidityRole::Taker,
                dec!(-0.15),
                &mut accumulator,
            )
            .expect("configured fee");
        assert_eq!(second.net_fee, dec!(0.0100));

        let third = fees
            .charge(
                dec!(0.30),
                dec!(0.50),
                LiquidityRole::Taker,
                dec!(-0.15),
                &mut accumulator,
            )
            .expect("configured fee");
        assert_eq!(third.rebate, dec!(0.01));
        assert_eq!(third.net_fee, dec!(0));
        assert_eq!(accumulator.rounding_overpayment, dec!(0.0041));
    }

    #[test]
    fn kalshi_direct_member_balance_precision_avoids_cent_rounding_fee() {
        let fees = FeeSchedule::new(
            FeeFormula::ProbabilityPower { exponent: 1 },
            dec!(0),
            dec!(0.07),
            FeeRounding::Ceiling { decimal_places: 4 },
            dec!(0),
        )
        .with_kalshi_balance_rounding(4);
        let charge = fees
            .charge(
                dec!(0.30),
                dec!(0.50),
                LiquidityRole::Taker,
                dec!(-0.15),
                &mut FeeAccumulator::default(),
            )
            .expect("configured fee");

        assert_eq!(charge.trade_fee, dec!(0.0053));
        assert_eq!(charge.rounding_fee, dec!(0));
        assert_eq!(charge.net_fee, dec!(0.0053));
    }

    #[test]
    fn missing_market_fee_metadata_fails_closed() {
        let fees = FeeSchedule::unconfigured(FeeFormula::Notional);

        assert!(!fees.is_configured());
        assert!(fees
            .charge(
                dec!(10),
                dec!(0.50),
                LiquidityRole::Taker,
                dec!(-5),
                &mut FeeAccumulator::default(),
            )
            .is_none());
    }

    #[test]
    fn current_polymarket_crypto_helper_uses_seven_percent_curve() {
        assert!((super::polymarket_crypto_taker_fee_per_share(0.50) - 0.0175).abs() < 1e-12);
        assert_eq!(super::polymarket_crypto_taker_fee_cost(1.0, 0.0001), 0.0);
        assert_eq!(super::polymarket_crypto_taker_fee_cost(10.0, 0.50), 0.175);
    }
}
