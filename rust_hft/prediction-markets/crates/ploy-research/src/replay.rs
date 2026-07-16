use ploy_trading::{FillRecord, PnlSnapshot, PositionLedger};
use rust_decimal::Decimal;
use std::collections::BTreeMap;

pub fn replay_fills(fills: &[FillRecord]) -> PnlSnapshot {
    let mut ledger = PositionLedger::default();
    for fill in fills {
        ledger.apply_fill(fill);
    }

    ledger.pnl_snapshot(&BTreeMap::<String, Decimal>::new())
}

#[cfg(test)]
mod tests {
    use super::replay_fills;
    use crate::backtesting::run_backtest;
    use chrono::Utc;
    use ploy_trading::{FillRecord, TradeSide};
    use rust_decimal_macros::dec;

    fn sample_fill(
        fill_id: &str,
        side: TradeSide,
        quantity: rust_decimal::Decimal,
        price: rust_decimal::Decimal,
    ) -> FillRecord {
        FillRecord {
            fill_id: fill_id.to_string(),
            order_id: format!("order-{fill_id}"),
            token_id: "yes-token".to_string(),
            side,
            quantity,
            price,
            fee: dec!(0.05),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn research_replays_trading_models() {
        let fills = vec![
            sample_fill("1", TradeSide::Buy, dec!(3), dec!(0.40)),
            sample_fill("2", TradeSide::Sell, dec!(1), dec!(0.55)),
        ];

        let pnl = replay_fills(&fills);
        assert!(pnl.realized_pnl > dec!(0));
    }

    #[test]
    fn backtest_report_wraps_replay() {
        let fills = vec![sample_fill("1", TradeSide::Buy, dec!(2), dec!(0.40))];
        let report = run_backtest(&fills);
        assert_eq!(report.fill_count, 1);
    }
}
