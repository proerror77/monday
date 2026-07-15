use crate::factors::FactorObservation;
use crate::signal::traits::{Signal, SignalSource};
use ploy_operator_contracts::Regime;
use std::collections::HashMap;

pub struct RegimeRouter {
    default: Box<dyn SignalSource>,
    routes: HashMap<Regime, Box<dyn SignalSource>>,
}

impl RegimeRouter {
    pub fn new(default: Box<dyn SignalSource>) -> Self {
        Self {
            default,
            routes: HashMap::new(),
        }
    }

    pub fn set(&mut self, regime: Regime, source: Box<dyn SignalSource>) {
        self.routes.insert(regime, source);
    }
}

impl SignalSource for RegimeRouter {
    fn signal(&self, obs: &FactorObservation) -> Signal {
        let regime = Regime::from_secs(obs.time_remaining_secs);
        self.routes
            .get(&regime)
            .map(|s| s.signal(obs))
            .unwrap_or_else(|| self.default.signal(obs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factors::FactorObservation;
    use crate::signal::traits::{Signal, SignalSource};
    use chrono::Utc;
    use ploy_operator_contracts::Regime;

    struct FixedSignal(Signal);
    impl SignalSource for FixedSignal {
        fn signal(&self, _: &FactorObservation) -> Signal {
            self.0
        }
    }

    fn obs_at(time_remaining_secs: i64) -> FactorObservation {
        FactorObservation {
            time_remaining_secs,
            settlement_up: 0.0,
            event_id: "e".into(),
            symbol: "BTC".into(),
            tick_ts: Utc::now(),
            source_availability: Default::default(),
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
            pm_up_ask: 0.0,
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
    fn router_dispatches_to_correct_regime_source() {
        let mut router = RegimeRouter::new(Box::new(FixedSignal(Signal::Hold)));
        router.set(Regime::Early, Box::new(FixedSignal(Signal::Buy)));
        assert_eq!(router.signal(&obs_at(220)), Signal::Buy); // early (181-300s)
        assert_eq!(router.signal(&obs_at(30)), Signal::Hold); // expiry -> falls back to default
    }
}
