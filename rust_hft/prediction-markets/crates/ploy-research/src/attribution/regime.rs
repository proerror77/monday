use crate::backtest::engine::SimulatedFill;
use ploy_market_contracts::Regime;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct RegimePnl {
    pub trade_count: usize,
    pub win_count: usize,
    pub total_pnl: f64,
}

impl RegimePnl {
    pub fn win_rate(&self) -> f64 {
        if self.trade_count == 0 {
            0.0
        } else {
            self.win_count as f64 / self.trade_count as f64
        }
    }
}

pub fn regime_pnl(fills: &[SimulatedFill]) -> BTreeMap<Regime, RegimePnl> {
    let mut map: BTreeMap<Regime, RegimePnl> = BTreeMap::new();
    for f in fills {
        let e = map.entry(f.regime).or_default();
        e.trade_count += 1;
        e.total_pnl += f.pnl;
        if f.pnl > 0.0 {
            e.win_count += 1;
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::engine::SimulatedFill;
    use crate::signal::traits::Signal;
    use ploy_market_contracts::Regime;

    fn fill(regime: Regime, pnl: f64) -> SimulatedFill {
        SimulatedFill {
            event_id: "e".into(),
            regime,
            signal: Signal::Buy,
            entry_price: 0.5,
            settled_up: pnl > 0.0,
            pnl,
        }
    }

    #[test]
    fn regime_pnl_groups_correctly() {
        let fills = vec![
            fill(Regime::Early, 0.3),
            fill(Regime::Early, -0.1),
            fill(Regime::Middle, 0.2),
        ];
        let by_regime = regime_pnl(&fills);
        assert!((by_regime[&Regime::Early].total_pnl - 0.2).abs() < 1e-9);
        assert_eq!(by_regime[&Regime::Early].trade_count, 2);
        assert_eq!(by_regime[&Regime::Middle].trade_count, 1);
    }
}
