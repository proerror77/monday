use crate::replay::replay_fills;
use ploy_trading::{FillRecord, PnlSnapshot};

#[derive(Debug, Clone)]
pub struct BacktestReport {
    pub pnl: PnlSnapshot,
    pub fill_count: usize,
}

pub fn run_backtest(fills: &[FillRecord]) -> BacktestReport {
    BacktestReport {
        pnl: replay_fills(fills),
        fill_count: fills.len(),
    }
}
