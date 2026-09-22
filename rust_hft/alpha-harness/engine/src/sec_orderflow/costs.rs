use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CashflowFill {
    pub sign: i8,
    pub qty: f64,
    pub entry: f64,
    pub exit: f64,
    pub taker_bps_per_side: f64,
}

pub fn round_trip_cost_bp(taker_per_side: f64, spread_bp: f64) -> f64 {
    taker_per_side.mul_add(2.0, spread_bp)
}

pub fn net_usdt(fill: &CashflowFill) -> f64 {
    let gross = f64::from(fill.sign) * fill.qty * (fill.exit - fill.entry);
    let entry_notional = fill.qty * fill.entry;
    let exit_notional = fill.qty * fill.exit;
    let fee = (fill.taker_bps_per_side / 10_000.0) * (entry_notional + exit_notional);
    gross - fee
}

pub fn net_bp(fill: &CashflowFill) -> f64 {
    let entry_notional = fill.qty * fill.entry;
    if entry_notional <= 0.0 || !entry_notional.is_finite() {
        return f64::NAN;
    }
    10_000.0 * net_usdt(fill) / entry_notional
}

pub fn adverse_first_exit(sign: i8, up: f64, down: f64) -> f64 {
    if sign >= 0 {
        down
    } else {
        up
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_and_short_pay_spread_and_fees() {
        let long = CashflowFill {
            sign: 1,
            qty: 1.0,
            entry: 100.10,
            exit: 100.00,
            taker_bps_per_side: 11.0,
        };
        let short = CashflowFill {
            sign: -1,
            qty: 1.0,
            entry: 99.90,
            exit: 100.00,
            taker_bps_per_side: 11.0,
        };
        assert!(net_bp(&long) < 0.0);
        assert!(net_bp(&short) < 0.0);
        assert_eq!(adverse_first_exit(1, 101.0, 99.0), 99.0);
        assert_eq!(adverse_first_exit(-1, 101.0, 99.0), 101.0);
    }
}
