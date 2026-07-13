#[derive(Debug, Clone)]
pub struct BacktestMetrics {
    pub trade_count: usize,
    pub win_count: usize,
    pub win_rate: f64,
    pub total_pnl: f64,
    pub sharpe_per_trade: f64,
    pub max_drawdown: f64,
}

impl BacktestMetrics {
    pub fn from_pnls(pnls: &[f64]) -> Self {
        let n = pnls.len();
        if n == 0 {
            return Self {
                trade_count: 0,
                win_count: 0,
                win_rate: 0.0,
                total_pnl: 0.0,
                sharpe_per_trade: 0.0,
                max_drawdown: 0.0,
            };
        }
        let win_count = pnls.iter().filter(|&&p| p > 0.0).count();
        let total_pnl: f64 = pnls.iter().sum();
        let mean = total_pnl / n as f64;
        let variance = if n > 1 {
            pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (n - 1) as f64
        } else {
            0.0
        };
        // Per-trade Sharpe: mean_pnl / std_pnl. Not annualized.
        // To annualize: multiply by sqrt(trades_per_year).
        let sharpe_per_trade = if variance > 0.0 {
            mean / variance.sqrt()
        } else if mean > 0.0 {
            f64::INFINITY
        } else if mean < 0.0 {
            f64::NEG_INFINITY
        } else {
            0.0
        };

        let mut peak = 0.0_f64;
        let mut cumulative = 0.0_f64;
        let mut max_drawdown = 0.0_f64;
        for &p in pnls {
            cumulative += p;
            if cumulative > peak {
                peak = cumulative;
            }
            let dd = peak - cumulative;
            if dd > max_drawdown {
                max_drawdown = dd;
            }
        }

        Self {
            trade_count: n,
            win_count,
            win_rate: win_count as f64 / n as f64,
            total_pnl,
            sharpe_per_trade,
            max_drawdown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_from_pnl_stream() {
        let pnls = vec![0.3, -0.5, 0.4, 0.2, -0.1];
        let m = BacktestMetrics::from_pnls(&pnls);
        assert_eq!(m.trade_count, 5);
        assert_eq!(m.win_count, 3);
        assert!((m.win_rate - 0.6).abs() < 1e-9);
        assert!((m.total_pnl - 0.3).abs() < 1e-9);
    }

    #[test]
    fn max_drawdown_is_correct() {
        // cumulative: 0.3, then -0.2 → peak=0.3, trough=-0.2, drawdown=0.5
        let pnls = vec![0.3, -0.5];
        let m = BacktestMetrics::from_pnls(&pnls);
        assert!((m.max_drawdown - 0.5).abs() < 1e-9);
    }
}
