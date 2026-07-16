#![allow(dead_code)]
use crate::factors::FactorObservation;

pub trait Environment {
    fn reset(&mut self) -> Vec<f64>;
    /// Returns (next_state, reward, done)
    fn step(&mut self, action: u8) -> (Vec<f64>, f64, bool);
}

/// Wraps a slice of FactorObservations as an RL environment.
/// Each step advances to the next observation; reward = simulated P&L.
pub struct BinaryEventEnv<'a> {
    obs: &'a [FactorObservation],
    cursor: usize,
    fee: f64,
}

impl<'a> BinaryEventEnv<'a> {
    pub fn new(obs: &'a [FactorObservation], fee: f64) -> Self {
        assert!(
            !obs.is_empty(),
            "BinaryEventEnv requires at least one observation"
        );
        Self {
            obs,
            cursor: 0,
            fee,
        }
    }
}

impl<'a> Environment for BinaryEventEnv<'a> {
    fn reset(&mut self) -> Vec<f64> {
        self.cursor = 0;
        obs_to_state(&self.obs[0])
    }

    fn step(&mut self, action: u8) -> (Vec<f64>, f64, bool) {
        assert!(
            self.cursor < self.obs.len(),
            "step() called after done=true"
        );
        let o = &self.obs[self.cursor];
        let settled_up = o.settlement_up > 0.5;
        let reward = match action {
            1 => {
                if settled_up {
                    1.0 - o.pm_up_ask - self.fee
                } else {
                    -o.pm_up_ask - self.fee
                }
            }
            2 => {
                if !settled_up {
                    o.pm_up_bid - self.fee
                } else {
                    o.pm_up_bid - 1.0 - self.fee
                }
            }
            _ => 0.0,
        };
        self.cursor += 1;
        let done = self.cursor >= self.obs.len();
        let next = if done {
            vec![0.0; 16]
        } else {
            obs_to_state(&self.obs[self.cursor])
        };
        (next, reward, done)
    }
}

fn obs_to_state(o: &FactorObservation) -> Vec<f64> {
    vec![
        o.time_remaining_secs as f64 / 300.0,
        o.distance_over_sigma,
        o.model_prob_up,
        o.drift_30s,
        o.obi_10,
        o.depth_imbalance,
        o.cum_mprice_drift_5m,
        o.sigma_horizon,
        o.vol_gap,
        o.fair_prob_up_clean,
        o.pm_lag_secs / 60.0,
        o.spread_bps / 100.0,
        o.microprice_offset_bps / 100.0,
        o.depth_far_ratio,
        o.cum_obi_delta_5m,
        o.cum_trade_imbalance_5m,
    ]
}
