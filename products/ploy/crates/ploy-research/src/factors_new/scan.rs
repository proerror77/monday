use crate::factors::{spearman_ic, FactorObservation};
use crate::factors_new::registry::{FactorMeta, FactorRegistry};
use ploy_operator_contracts::Regime;

const MIN_OBS: usize = 10;

const FACTOR_EXTRACTORS: &[(&str, fn(&FactorObservation) -> f64)] = &[
    ("distance_over_sigma", |o| o.distance_over_sigma),
    ("model_prob_up", |o| o.model_prob_up),
    ("drift_30s", |o| o.drift_30s),
    ("drift_10s", |o| o.drift_10s),
    ("obi_10", |o| o.obi_10),
    ("depth_imbalance", |o| o.depth_imbalance),
    ("cum_mprice_drift_5m", |o| o.cum_mprice_drift_5m),
    ("sigma_horizon", |o| o.sigma_horizon),
    ("vol_gap", |o| o.vol_gap),
    ("fair_prob_up_clean", |o| o.fair_prob_up_clean),
    ("pm_lag_secs", |o| o.pm_lag_secs),
    ("spread_bps", |o| o.spread_bps),
    ("microprice_offset_bps", |o| o.microprice_offset_bps),
    ("depth_far_ratio", |o| o.depth_far_ratio),
    ("cum_obi_delta_5m", |o| o.cum_obi_delta_5m),
    ("cum_trade_imbalance_5m", |o| o.cum_trade_imbalance_5m),
    ("cex_bar_return_30s", |o| o.cex_bar_return_30s),
    ("cex_bar_return_60s", |o| o.cex_bar_return_60s),
    ("cex_bar_volume_ratio_30s", |o| o.cex_bar_volume_ratio_30s),
    ("cex_signed_volume_ratio_30s", |o| {
        o.cex_signed_volume_ratio_30s
    }),
    ("cex_breakout_volume_score", |o| o.cex_breakout_volume_score),
];

fn event_bucket_id(event_id: &str) -> i64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    event_id.hash(&mut h);
    h.finish() as i64
}

const LABELS: &[(&str, fn(&FactorObservation) -> Option<f64>)] = &[
    ("settlement_up", |o| Some(o.settlement_up)),
    ("future_up_ask_change_30s", |o| o.future_up_ask_change_30s),
];

pub fn scan_into_registry(obs: &[FactorObservation], registry: &mut FactorRegistry) {
    for regime in [Regime::Early, Regime::Middle, Regime::Late, Regime::Expiry] {
        let regime_obs: Vec<&FactorObservation> = obs
            .iter()
            .filter(|o| Regime::from_secs(o.time_remaining_secs) == regime)
            .collect();
        if regime_obs.len() < MIN_OBS {
            continue;
        }

        for (label_name, label_fn) in LABELS {
            for (factor_name, factor_fn) in FACTOR_EXTRACTORS {
                let triples: Vec<(i64, f64, f64)> = regime_obs
                    .iter()
                    .filter_map(|o| {
                        label_fn(o).map(|y| (event_bucket_id(&o.event_id), factor_fn(o), y))
                    })
                    .collect();
                if triples.len() < MIN_OBS {
                    continue;
                }
                let xs: Vec<f64> = triples.iter().map(|t| t.1).collect();
                let ys: Vec<f64> = triples.iter().map(|t| t.2).collect();
                let ic = spearman_ic(&xs, &ys);
                if ic.is_nan() {
                    continue;
                }
                let icir = crate::factors::bucket_icir(&triples, 3).unwrap_or(0.0);
                registry.insert(FactorMeta {
                    name: factor_name.to_string(),
                    regime,
                    label: label_name.to_string(),
                    ic,
                    direction: if ic >= 0.0 { 1 } else { -1 },
                    stability: icir,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factors::FactorObservation;
    use crate::factors_new::registry::FactorRegistry;
    use chrono::Utc;
    use ploy_operator_contracts::Regime;

    fn obs(
        time_remaining_secs: i64,
        distance_over_sigma: f64,
        settlement_up: f64,
    ) -> FactorObservation {
        FactorObservation {
            event_id: "e1".into(),
            symbol: "BTC".into(),
            tick_ts: Utc::now(),
            source_availability: Default::default(),
            event_window_secs: 300,
            time_remaining_secs,
            distance_over_sigma,
            settlement_up,
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
    fn scan_populates_registry_for_early_regime() {
        // 20 observations across 5 events in early regime (220s = Early 181-300s)
        let observations: Vec<FactorObservation> = (0..20)
            .map(|i| {
                let mut o = obs(220, i as f64 * 0.1, if i % 2 == 0 { 1.0 } else { 0.0 });
                o.event_id = format!("event-{}", i / 4); // 5 events, 4 obs each
                o
            })
            .collect();
        let mut reg = FactorRegistry::new();
        scan_into_registry(&observations, &mut reg);
        let top = reg.top_n(Regime::Early, "settlement_up", 1);
        assert!(
            !top.is_empty(),
            "registry should have at least one early factor"
        );
    }
}
