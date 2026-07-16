use crate::attribution::factor::factor_pnl;
use crate::attribution::regime::{regime_pnl, RegimePnl};
use crate::backtest::engine::SimulatedFill;
use crate::backtest::metrics::BacktestMetrics;
use ploy_market_contracts::Regime;
use std::collections::BTreeMap;

pub struct AttributionReport {
    pub overall: BacktestMetrics,
    pub by_regime: BTreeMap<Regime, RegimePnl>,
    pub by_factor: BTreeMap<String, f64>,
}

impl AttributionReport {
    pub fn build(fills: &[SimulatedFill], factor_fills: &[(f64, Vec<(String, f64)>)]) -> Self {
        let pnls: Vec<f64> = fills.iter().map(|f| f.pnl).collect();
        Self {
            overall: BacktestMetrics::from_pnls(&pnls),
            by_regime: regime_pnl(fills),
            by_factor: factor_pnl(factor_fills),
        }
    }

    pub fn print(&self) {
        eprintln!("\n=== Attribution Report ===");
        eprintln!("Overall: trades={} win_rate={:.1}% total_pnl={:.4} sharpe_per_trade={:.3} max_dd={:.4}",
            self.overall.trade_count,
            self.overall.win_rate * 100.0,
            self.overall.total_pnl,
            self.overall.sharpe_per_trade,
            self.overall.max_drawdown,
        );
        eprintln!("\n--- By Regime ---");
        for (regime, r) in &self.by_regime {
            eprintln!(
                "  {:8} trades={:4} win={:.1}% pnl={:.4}",
                regime.as_str(),
                r.trade_count,
                r.win_rate() * 100.0,
                r.total_pnl
            );
        }
        eprintln!("\n--- By Factor (P&L contribution, top 10) ---");
        let mut factor_vec: Vec<(&String, &f64)> = self.by_factor.iter().collect();
        factor_vec.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap());
        for (name, pnl) in factor_vec.iter().take(10) {
            eprintln!("  {:30} {:+.4}", name, pnl);
        }
        eprintln!("=========================\n");
    }
}
