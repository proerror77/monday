//! Fixed inference inputs. This schema does not establish scientific validation.
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

pub const PROBABILITY_REVERSAL_SCHEMA: &str = "monday.prediction_probability_reversal.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryEpisodeV1 {
    pub episode_id: String,
    pub condition_id: String,
    pub underlying: String,
    pub venue: String,
    pub up_token: String,
    pub down_token: String,
    /// UTC epoch microseconds, matching the canonical runtime clock.
    pub start_us: u64,
    pub end_us: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbabilityReversalSpecV1 {
    pub schema: String,
    pub episodes: Vec<BinaryEpisodeV1>,
    pub prev_prob_low: f64,
    pub curr_prob_high: f64,
    pub prev_prob_high: f64,
    pub curr_prob_low: f64,
    pub take_profit_prob: f64,
    pub stop_loss_prob: f64,
    pub min_time_remaining_secs: u64,
    pub max_time_remaining_secs: u64,
    /// USD spent per entry; outcome quantity is measured in shares.
    pub stake_usd: Decimal,
    pub max_positions: usize,
    pub max_daily_trades: u32,
    pub quote_max_age_us: u64,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("invalid fixed probability reversal configuration: {0}")]
pub struct ProbabilitySpecError(pub &'static str);

fn token_id(value: &str) -> bool {
    const U256_MAX: &str =
        "115792089237316195423570985008687907853269984665640564039457584007913129639935";
    !value.is_empty()
        && !value.starts_with('0')
        && value.bytes().all(|b| b.is_ascii_digit())
        && (value.len() < U256_MAX.len() || (value.len() == U256_MAX.len() && value <= U256_MAX))
}
impl ProbabilityReversalSpecV1 {
    pub fn validate(&self) -> Result<(), ProbabilitySpecError> {
        if self.schema != PROBABILITY_REVERSAL_SCHEMA
            || self.episodes.is_empty()
            || self.episodes.len() > 1024
        {
            return Err(ProbabilitySpecError("schema or finite episodes"));
        }
        for probability in [
            self.prev_prob_low,
            self.curr_prob_high,
            self.prev_prob_high,
            self.curr_prob_low,
            self.take_profit_prob,
            self.stop_loss_prob,
        ] {
            if !probability.is_finite() || !(0.0..1.0).contains(&probability) || probability == 0.0
            {
                return Err(ProbabilitySpecError("probability threshold"));
            }
        }
        if self.prev_prob_low >= self.curr_prob_high
            || self.curr_prob_low >= self.prev_prob_high
            || self.stop_loss_prob >= self.take_profit_prob
            || self.min_time_remaining_secs == 0
            || self.min_time_remaining_secs > self.max_time_remaining_secs
            || self.max_time_remaining_secs > 86_400
            || self.stake_usd <= Decimal::ZERO
            || self.max_positions == 0
            || self.max_positions > 1024
            || self.max_daily_trades == 0
            || self.quote_max_age_us == 0
            || self.quote_max_age_us > 60_000_000
        {
            return Err(ProbabilitySpecError("threshold order, timing or sizing"));
        }
        let mut episodes = BTreeSet::new();
        let mut conditions = BTreeSet::new();
        let mut tokens = BTreeSet::new();
        for episode in &self.episodes {
            if episode.venue != "POLYMARKET"
                || episode.start_us == 0
                || episode.end_us <= episode.start_us
                || [
                    &episode.episode_id,
                    &episode.condition_id,
                    &episode.underlying,
                ]
                .into_iter()
                .any(|s| s.is_empty() || s.len() > 256 || s.trim() != s)
                || !episodes.insert(&episode.episode_id)
                || !conditions.insert(&episode.condition_id)
                || !token_id(&episode.up_token)
                || !token_id(&episode.down_token)
                || !tokens.insert(&episode.up_token)
                || !tokens.insert(&episode.down_token)
            {
                return Err(ProbabilitySpecError("episode identity, token or clock"));
            }
        }
        Ok(())
    }
}
