use chrono::{DateTime, Utc};
use hft_core::{OrderId, Price, Quantity, Side, Symbol};
use hft_portfolio_core::Portfolio;
use hft_ports::ExecutionEvent;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Research replay input. It is deliberately a value object owned by the
/// research crate; execution/runtime ledgers are not part of the research
/// dependency graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchFill {
    pub fill_id: String,
    pub order_id: String,
    pub token_id: String,
    pub side: Side,
    pub quantity: Decimal,
    pub price: Decimal,
    pub fee: Decimal,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResearchTradeSide {
    Buy,
    Sell,
}

impl From<ResearchTradeSide> for Side {
    fn from(side: ResearchTradeSide) -> Self {
        match side {
            ResearchTradeSide::Buy => Side::Buy,
            ResearchTradeSide::Sell => Side::Sell,
        }
    }
}

/// PnL projection emitted by the canonical portfolio ledger for research.
/// Fees are already included in `realized_pnl`; `total_fees` is a separate
/// disclosure field and must not be subtracted again by callers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PnlSnapshot {
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_fees: Decimal,
}

pub fn replay_fills(fills: &[ResearchFill]) -> PnlSnapshot {
    let mut portfolio = Portfolio::new();
    for fill in fills {
        let order_id = OrderId(fill.order_id.clone());
        let symbol = Symbol::new(&fill.token_id);
        portfolio.register_order(order_id.clone(), symbol, fill.side);
        portfolio.on_execution_event(&ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price(fill.price),
            quantity: Quantity(fill.quantity),
            timestamp: fill.timestamp.timestamp_micros().max(0) as u64,
            fill_id: fill.fill_id.clone(),
        });
        if fill.fee > Decimal::ZERO {
            portfolio.on_execution_event(&ExecutionEvent::FeeCharged {
                order_id,
                amount: fill.fee,
                timestamp: fill.timestamp.timestamp_micros().max(0) as u64,
                fill_id: fill.fill_id.clone(),
            });
        }
    }
    let view = portfolio.reader().load();
    let total_fees = portfolio.export_state().total_fees;
    PnlSnapshot {
        realized_pnl: view.realized_pnl,
        unrealized_pnl: view.unrealized_pnl,
        total_fees,
    }
}

#[cfg(test)]
mod tests {
    use super::{replay_fills, PnlSnapshot, ResearchFill, ResearchTradeSide};
    use crate::backtesting::run_backtest;
    use chrono::Utc;
    use rust_decimal_macros::dec;

    fn sample_fill(
        fill_id: &str,
        side: ResearchTradeSide,
        quantity: rust_decimal::Decimal,
        price: rust_decimal::Decimal,
    ) -> ResearchFill {
        ResearchFill {
            fill_id: fill_id.to_string(),
            order_id: format!("order-{fill_id}"),
            token_id: "yes-token".to_string(),
            side: side.into(),
            quantity,
            price,
            fee: dec!(0.05),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn research_replays_through_canonical_portfolio() {
        let fills = vec![
            sample_fill("1", ResearchTradeSide::Buy, dec!(3), dec!(0.40)),
            sample_fill("2", ResearchTradeSide::Sell, dec!(1), dec!(0.55)),
        ];

        let pnl = replay_fills(&fills);
        assert!(pnl.realized_pnl > dec!(0));
        assert_eq!(pnl.total_fees, dec!(0.10));
    }

    #[test]
    fn duplicate_fill_id_uses_canonical_fee_and_pnl_once() {
        let first = ResearchFill {
            fill_id: "same-fill".to_string(),
            order_id: "order-1".to_string(),
            token_id: "yes-token".to_string(),
            side: ResearchTradeSide::Buy.into(),
            quantity: dec!(1),
            price: dec!(0.40),
            fee: dec!(0.05),
            timestamp: Utc::now(),
        };

        let pnl = replay_fills(&[first.clone(), first]);

        assert_eq!(pnl.total_fees, dec!(0.05));
        assert_eq!(pnl.realized_pnl, dec!(-0.05));
    }

    #[test]
    fn negative_fee_is_not_counted_or_applied() {
        let mut fill = sample_fill("negative-fee", ResearchTradeSide::Buy, dec!(1), dec!(0.40));
        fill.fee = dec!(-0.05);

        let pnl = replay_fills(&[fill]);

        assert_eq!(pnl.total_fees, dec!(0));
        assert_eq!(pnl.realized_pnl, dec!(0));
    }

    #[test]
    fn backtest_report_wraps_canonical_replay() {
        let fills = vec![sample_fill(
            "1",
            ResearchTradeSide::Buy,
            dec!(2),
            dec!(0.40),
        )];
        let report = run_backtest(&fills);
        assert_eq!(report.fill_count, 1);
        assert_ne!(report.pnl, PnlSnapshot::default());
    }
}
