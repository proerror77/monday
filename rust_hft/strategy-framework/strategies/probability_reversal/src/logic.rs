//! The existing probability-reversal inference, shared by both input adapters.
use rust_decimal::{Decimal, RoundingStrategy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub prev_prob_low: f64,
    pub curr_prob_high: f64,
    pub prev_prob_high: f64,
    pub curr_prob_low: f64,
    pub take_profit_prob: f64,
    pub stop_loss_prob: f64,
}
impl Thresholds {
    pub fn entry(&self, previous_up: f64, current_up: f64) -> Option<Outcome> {
        if !previous_up.is_finite()
            || !current_up.is_finite()
            || !(0.0..1.0).contains(&previous_up)
            || !(0.0..1.0).contains(&current_up)
        {
            return None;
        }
        if previous_up < self.prev_prob_low && current_up > self.curr_prob_high {
            Some(Outcome::Up)
        } else if previous_up > self.prev_prob_high && current_up < self.curr_prob_low {
            Some(Outcome::Down)
        } else {
            None
        }
    }
    pub fn exit(&self, held_probability: f64) -> bool {
        held_probability.is_finite()
            && held_probability > 0.0
            && held_probability < 1.0
            && (held_probability >= self.take_profit_prob
                || held_probability <= self.stop_loss_prob)
    }
}
/// Quote-currency stake divided by probability price gives outcome shares.
pub fn entry_quantity(stake_usd: Decimal, ask: Decimal) -> Decimal {
    if stake_usd <= Decimal::ZERO || ask <= Decimal::ZERO || ask >= Decimal::ONE {
        return Decimal::ZERO;
    }
    stake_usd
        .checked_div(ask)
        .map(|qty| qty.round_dp_with_strategy(6, RoundingStrategy::ToZero))
        .unwrap_or(Decimal::ZERO)
}
