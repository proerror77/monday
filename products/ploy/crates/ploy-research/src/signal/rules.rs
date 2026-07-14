use crate::factors::FactorObservation;
use crate::signal::traits::{Signal, SignalSource};

pub struct ThresholdRule {
    pub extractor: fn(&FactorObservation) -> f64,
    pub buy_above: Option<f64>,
    pub sell_below: Option<f64>,
}

impl SignalSource for ThresholdRule {
    fn signal(&self, obs: &FactorObservation) -> Signal {
        let v = (self.extractor)(obs);
        if self.buy_above.map_or(false, |t| v > t) {
            return Signal::Buy;
        }
        if self.sell_below.map_or(false, |t| v < t) {
            return Signal::Sell;
        }
        Signal::Hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factors::FactorObservation;
    use crate::signal::traits::{Signal, SignalSource};
    use chrono::Utc;

    fn obs_with(distance_over_sigma: f64) -> FactorObservation {
        FactorObservation {
            distance_over_sigma,
            time_remaining_secs: 250,
            settlement_up: 0.0,
            event_id: "e".into(),
            symbol: "BTC".into(),
            tick_ts: Utc::now(),
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
    fn threshold_rule_buy_above_threshold() {
        let rule = ThresholdRule {
            extractor: |o: &FactorObservation| o.distance_over_sigma,
            buy_above: Some(0.5),
            sell_below: Some(-0.5),
        };
        assert_eq!(rule.signal(&obs_with(0.8)), Signal::Buy);
        assert_eq!(rule.signal(&obs_with(-0.8)), Signal::Sell);
        assert_eq!(rule.signal(&obs_with(0.1)), Signal::Hold);
    }
}
