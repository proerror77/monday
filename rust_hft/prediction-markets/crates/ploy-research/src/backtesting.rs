use crate::replay::{replay_fills, PnlSnapshot, ResearchFill};

#[derive(Debug, Clone)]
pub struct BacktestReport {
    pub pnl: PnlSnapshot,
    pub fill_count: usize,
}

pub fn run_backtest(fills: &[ResearchFill]) -> BacktestReport {
    BacktestReport {
        pnl: replay_fills(fills),
        fill_count: fills.len(),
    }
}
