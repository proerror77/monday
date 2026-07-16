use crate::factors::FactorObservation;
use crate::signal::traits::{Signal, SignalSource};
use ploy_market_contracts::Regime;

#[derive(Debug, Clone)]
pub struct SimulatedFill {
    pub event_id: String,
    pub regime: Regime,
    pub signal: Signal,
    pub entry_price: f64,
    pub settled_up: bool,
    pub pnl: f64,
}

/// Simulate binary option trades. Each observation is one entry attempt.
/// Hold signals produce no fill.
/// Buy P&L:  (1.0 - ask - fee) if settled_up, else (-ask - fee)
/// Sell P&L: (bid - fee) if settled_down, else (bid - 1.0 - fee)
pub fn run_binary_backtest(
    obs: &[FactorObservation],
    source: &dyn SignalSource,
    fee: f64,
) -> Vec<SimulatedFill> {
    obs.iter()
        .filter_map(|o| {
            let signal = source.signal(o);
            if signal == Signal::Hold {
                return None;
            }
            let settled_up = o.settlement_up > 0.5;
            let (entry_price, pnl) = match signal {
                Signal::Buy => {
                    let ask = o.pm_up_ask;
                    let p = if settled_up {
                        1.0 - ask - fee
                    } else {
                        -ask - fee
                    };
                    (ask, p)
                }
                Signal::Sell => {
                    let bid = o.pm_up_bid;
                    let p = if !settled_up {
                        bid - fee
                    } else {
                        bid - 1.0 - fee
                    };
                    (bid, p)
                }
                Signal::Hold => unreachable!(),
            };
            Some(SimulatedFill {
                event_id: o.event_id.clone(),
                regime: Regime::from_secs(o.time_remaining_secs),
                signal,
                entry_price,
                settled_up,
                pnl,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factors::FactorObservation;
    use crate::signal::traits::{Signal, SignalSource};
    use chrono::Utc;

    struct AlwaysBuy;
    impl SignalSource for AlwaysBuy {
        fn signal(&self, _: &FactorObservation) -> Signal {
            Signal::Buy
        }
    }

    fn obs(settlement_up: f64, pm_up_ask: f64) -> FactorObservation {
        FactorObservation {
            settlement_up,
            official_resolution_observed_at: None,
            pm_up_ask,
            time_remaining_secs: 120,
            event_id: "e1".into(),
            symbol: "BTC".into(),
            tick_ts: Utc::now(),
            up_token_id: String::new(),
            down_token_id: String::new(),
            chainlink_reference_fresh: false,
            binance_spot_fresh: false,
            binance_lob_fresh: false,
            binance_agg_trade_fresh: false,
            event_window_secs: 300,
            distance_over_sigma: 0.0,
            signed_distance_to_beat: 0.0,
            abs_distance_to_beat: 0.0,
            drift_10s: 0.0,
            drift_30s: 0.0,
            flip_age_secs: 0.0,
            post_flip_drift: 0.0,
            sigma_horizon: 0.0,
            fair_prob_up: 0.0,
            fair_prob_up_clean: 0.0,
            prob_disagreement: 0.0,
            implied_sigma_horizon: 0.0,
            vol_gap: 0.0,
            model_prob_up: 0.0,
            chainlink_prob_up: f64::NAN,
            model_edge_up: 0.0,
            reward_risk_up: 0.0,
            reward_risk_down: 0.0,
            obi: 0.0,
            spread_bps: 0.0,
            microprice_offset_bps: 0.0,
            bid_depth_near: 0.0,
            ask_depth_near: 0.0,
            depth_ratio: 0.0,
            depth_imbalance: 0.0,
            depth_far_ratio: 0.0,
            depth_acceleration: 0.0,
            obi_10: 0.0,
            pm_up_bid: 0.0,
            pm_down_bid: 0.0,
            pm_down_ask: 0.0,
            pm_up_bid_size: 0.0,
            pm_up_ask_size: 0.0,
            pm_down_bid_size: 0.0,
            pm_down_ask_size: 0.0,
            pm_lag_secs: 0.0,
            future_up_ask_change_30s: None,
            future_up_ask_change_60s: None,
            cum_obi_delta_5m: 0.0,
            cum_depth_delta_5m: 0.0,
            cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0,
            cex_bar_return_30s: 0.0,
            cex_bar_return_60s: 0.0,
            cex_bar_volume_ratio_30s: 0.0,
            cex_bar_volume_trend_3: 0.0,
            cex_signed_volume_ratio_30s: 0.0,
            cex_consecutive_up_bars: 0.0,
            cex_consecutive_down_bars: 0.0,
            cex_breakout_volume_score: 0.0,
        }
    }

    #[test]
    fn buy_wins_when_settled_up() {
        // Buy at ask=0.6, settled up → pnl = 1.0 - 0.6 - 0.02 = 0.38
        let fills = run_binary_backtest(&[obs(1.0, 0.6)], &AlwaysBuy, 0.02);
        assert_eq!(fills.len(), 1);
        assert!((fills[0].pnl - 0.38).abs() < 1e-9);
    }

    #[test]
    fn buy_loses_when_settled_down() {
        // Buy at ask=0.6, settled down → pnl = -0.6 - 0.02 = -0.62
        let fills = run_binary_backtest(&[obs(0.0, 0.6)], &AlwaysBuy, 0.02);
        assert!((fills[0].pnl - (-0.62)).abs() < 1e-9);
    }
}
