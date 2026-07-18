use std::collections::HashMap;

use chrono::Duration;

use crate::factors::FactorObservation;

const ACTIVE_TIME_BINS: usize = 4;
const TIME_BINS: usize = ACTIVE_TIME_BINS + 1;
const FLOW_BINS: usize = 3;
const REGIME_STATES: usize = 5 * FLOW_BINS;
const ALPHA: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DistanceBin {
    StrongDown,
    WeakDown,
    Neutral,
    WeakUp,
    StrongUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeBin {
    T181To300s,
    T91To180s,
    T31To90s,
    T1To30s,
    Expiry,
}

impl TimeBin {
    pub fn from_seconds(seconds: i64) -> Self {
        match seconds {
            i64::MIN..=0 => Self::Expiry,
            181.. => Self::T181To300s,
            91..=180 => Self::T91To180s,
            31..=90 => Self::T31To90s,
            _ => Self::T1To30s,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlowBin {
    SellPressure,
    Neutral,
    BuyPressure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MarketState {
    pub distance: DistanceBin,
    pub time: TimeBin,
    pub flow: FlowBin,
}

impl MarketState {
    pub fn encode(distance_over_sigma: f64, seconds_to_expiry: i64, flow: f64) -> Option<Self> {
        if !distance_over_sigma.is_finite() || !flow.is_finite() {
            return None;
        }
        Some(Self {
            distance: match distance_over_sigma {
                z if z <= -1.5 => DistanceBin::StrongDown,
                z if z < -0.25 => DistanceBin::WeakDown,
                z if z <= 0.25 => DistanceBin::Neutral,
                z if z < 1.5 => DistanceBin::WeakUp,
                _ => DistanceBin::StrongUp,
            },
            time: TimeBin::from_seconds(seconds_to_expiry),
            flow: match flow {
                value if value <= -0.2 => FlowBin::SellPressure,
                value if value >= 0.2 => FlowBin::BuyPressure,
                _ => FlowBin::Neutral,
            },
        })
    }

    fn regime_index(self) -> usize {
        self.distance as usize * FLOW_BINS + self.flow as usize
    }
}

pub fn state_from_observation(observation: &FactorObservation) -> Option<MarketState> {
    MarketState::encode(
        observation.distance_over_sigma,
        observation.time_remaining_secs,
        observation.cum_trade_imbalance_5m,
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkovEstimate {
    pub probability_up: f64,
    pub standard_error: f64,
    pub stability: f64,
    pub transition_entropy: f64,
    pub effective_samples: usize,
}

#[derive(Debug, Clone)]
struct MarkovTrainingRow {
    event_id: String,
    seconds_to_expiry: i64,
    state: MarketState,
    settlement_up: bool,
}

#[derive(Debug, Clone)]
pub struct MarkovModel {
    transitions: Vec<Vec<Vec<f64>>>,
    transition_samples: Vec<Vec<usize>>,
    training_events: usize,
}

impl MarkovModel {
    pub fn fit_factor_observations(observations: &[FactorObservation]) -> Result<Self, String> {
        if observations.is_empty() {
            return Err("Markov training requires factor observations".to_string());
        }
        let mut event_labels = HashMap::new();
        let mut event_resolution_clocks = HashMap::new();
        let mut rows = Vec::with_capacity(observations.len());
        for observation in observations {
            if observation.event_window_secs != 300 {
                return Err(format!(
                    "Markov event {} has {}s horizon; expected 300s",
                    observation.event_id, observation.event_window_secs
                ));
            }
            if !(0..=300).contains(&observation.time_remaining_secs) {
                return Err(format!(
                    "Markov event {} has time outside its 300s window",
                    observation.event_id
                ));
            }
            if !matches!(observation.settlement_up, 0.0 | 1.0) {
                return Err(format!(
                    "Markov event {} lacks an official binary settlement label",
                    observation.event_id
                ));
            }
            let resolution_observed_at =
                observation.official_resolution_observed_at.ok_or_else(|| {
                    format!(
                        "Markov event {} lacks an official resolution observation clock",
                        observation.event_id
                    )
                })?;
            let settlement_at =
                observation.tick_ts + Duration::seconds(observation.time_remaining_secs);
            if resolution_observed_at < settlement_at {
                return Err(format!(
                    "Markov event {} resolution clock precedes settlement",
                    observation.event_id
                ));
            }
            if event_resolution_clocks
                .insert(observation.event_id.as_str(), resolution_observed_at)
                .is_some_and(|previous| previous != resolution_observed_at)
            {
                return Err(format!(
                    "Markov event {} has inconsistent resolution clocks",
                    observation.event_id
                ));
            }
            if !(observation.chainlink_reference_fresh
                && observation.binance_spot_fresh
                && observation.binance_lob_fresh
                && observation.binance_agg_trade_fresh)
            {
                return Err(format!(
                    "Markov event {} lacks fresh Chainlink/Binance inputs",
                    observation.event_id
                ));
            }
            let state = state_from_observation(observation).ok_or_else(|| {
                format!(
                    "Markov event {} has a non-finite distance or flow feature",
                    observation.event_id
                )
            })?;
            let settlement_up = observation.settlement_up == 1.0;
            if event_labels
                .insert(observation.event_id.as_str(), settlement_up)
                .is_some_and(|previous| previous != settlement_up)
            {
                return Err(format!(
                    "Markov event {} has inconsistent settlement labels",
                    observation.event_id
                ));
            }
            rows.push(MarkovTrainingRow {
                event_id: observation.event_id.clone(),
                seconds_to_expiry: observation.time_remaining_secs,
                state,
                settlement_up,
            });
        }

        let model = Self::fit_rows(&rows);
        if model.training_events == 0 {
            return Err(
                "Markov training found no event with all four decision-time buckets".to_string(),
            );
        }
        Ok(model)
    }

    fn fit_rows(rows: &[MarkovTrainingRow]) -> Self {
        let mut counts = vec![vec![vec![ALPHA; REGIME_STATES]; REGIME_STATES]; ACTIVE_TIME_BINS];
        let mut transition_samples = vec![vec![0; REGIME_STATES]; ACTIVE_TIME_BINS];
        let mut by_event: HashMap<&str, Vec<&MarkovTrainingRow>> = HashMap::new();
        for row in rows {
            by_event.entry(&row.event_id).or_default().push(row);
        }

        let mut training_events = 0;
        for event in by_event.values_mut() {
            event.sort_by_key(|row| std::cmp::Reverse(row.seconds_to_expiry));
            let mut representatives = [None; ACTIVE_TIME_BINS];
            for row in event.iter().filter(|row| row.state.time != TimeBin::Expiry) {
                representatives[row.state.time as usize] = Some(*row);
            }
            let [Some(early), Some(middle), Some(late), Some(final_30s)] = representatives else {
                continue;
            };
            let states = [early.state, middle.state, late.state, final_30s.state];
            for bucket in 0..ACTIVE_TIME_BINS - 1 {
                let from = states[bucket].regime_index();
                let to = states[bucket + 1].regime_index();
                counts[bucket][from][to] += 1.0;
                transition_samples[bucket][from] += 1;
            }
            let from = final_30s.state.regime_index();
            let terminal = MarketState {
                distance: if final_30s.settlement_up {
                    DistanceBin::StrongUp
                } else {
                    DistanceBin::StrongDown
                },
                time: TimeBin::Expiry,
                flow: FlowBin::Neutral,
            };
            counts[ACTIVE_TIME_BINS - 1][from][terminal.regime_index()] += 1.0;
            transition_samples[ACTIVE_TIME_BINS - 1][from] += 1;
            training_events += 1;
        }

        let transitions = counts
            .into_iter()
            .map(|matrix| {
                matrix
                    .into_iter()
                    .map(|row| {
                        let total: f64 = row.iter().sum();
                        row.into_iter().map(|count| count / total).collect()
                    })
                    .collect()
            })
            .collect();
        Self {
            transitions,
            transition_samples,
            training_events,
        }
    }

    pub fn training_events(&self) -> usize {
        self.training_events
    }

    pub fn estimate(&self, state: MarketState) -> MarkovEstimate {
        let time = state.time as usize;
        let regime = state.regime_index();
        let mut values = vec![terminal_values(); TIME_BINS];
        for bucket in (0..ACTIVE_TIME_BINS).rev() {
            values[bucket] = self.transitions[bucket]
                .iter()
                .map(|row| {
                    row.iter()
                        .zip(&values[bucket + 1])
                        .map(|(probability, value)| probability * value)
                        .sum()
                })
                .collect();
        }
        let probability_up = values[time][regime];
        let effective_samples = if time == ACTIVE_TIME_BINS {
            0
        } else {
            self.transition_samples[time][regime]
        };
        let standard_error = if time == ACTIVE_TIME_BINS {
            0.0
        } else if effective_samples == 0 {
            0.5
        } else {
            (probability_up * (1.0 - probability_up) / effective_samples as f64).sqrt()
        };
        let row = (time < ACTIVE_TIME_BINS).then(|| &self.transitions[time][regime]);
        let direction = direction_family(state.distance);
        let stability = row.map_or(1.0, |row| {
            row.iter()
                .enumerate()
                .filter(|(index, _)| direction_family(distance_from_regime(*index)) == direction)
                .map(|(_, probability)| probability)
                .sum()
        });
        let transition_entropy = row.map_or(0.0, |row| {
            row.iter()
                .map(|probability| -probability * probability.ln())
                .sum()
        });

        MarkovEstimate {
            probability_up,
            standard_error,
            stability,
            transition_entropy,
            effective_samples,
        }
    }
}

fn terminal_values() -> Vec<f64> {
    (0..REGIME_STATES)
        .map(|index| match distance_from_regime(index) {
            DistanceBin::StrongDown | DistanceBin::WeakDown => 0.0,
            DistanceBin::Neutral => 0.5,
            DistanceBin::WeakUp | DistanceBin::StrongUp => 1.0,
        })
        .collect()
}

fn distance_from_regime(index: usize) -> DistanceBin {
    match index / FLOW_BINS {
        0 => DistanceBin::StrongDown,
        1 => DistanceBin::WeakDown,
        2 => DistanceBin::Neutral,
        3 => DistanceBin::WeakUp,
        _ => DistanceBin::StrongUp,
    }
}

fn direction_family(distance: DistanceBin) -> i8 {
    match distance {
        DistanceBin::StrongDown | DistanceBin::WeakDown => -1,
        DistanceBin::Neutral => 0,
        DistanceBin::WeakUp | DistanceBin::StrongUp => 1,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MispricingBin {
    StrongUnderpriced,
    Underpriced,
    Fair,
    Overpriced,
    StrongOverpriced,
}

impl MispricingBin {
    fn from_error(error: f64) -> Option<Self> {
        if !error.is_finite() {
            return None;
        }
        Some(match error {
            value if value <= -0.1 => Self::StrongOverpriced,
            value if value < -0.03 => Self::Overpriced,
            value if value <= 0.03 => Self::Fair,
            value if value < 0.1 => Self::Underpriced,
            _ => Self::StrongUnderpriced,
        })
    }

    fn distance_from_fair(self) -> usize {
        match self {
            Self::Fair => 0,
            Self::Underpriced | Self::Overpriced => 1,
            Self::StrongUnderpriced | Self::StrongOverpriced => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MispricingTrainingRow {
    pub event_id: String,
    pub seconds_to_expiry: i64,
    pub model_probability: f64,
    pub market_probability: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MispricingEstimate {
    pub convergence_probability: f64,
    pub persistence_probability: f64,
    pub expansion_probability: f64,
    pub effective_samples: usize,
}

#[derive(Debug, Clone)]
pub struct MispricingMarkov {
    transitions: [[f64; 5]; 5],
    samples: [usize; 5],
}

impl MispricingMarkov {
    pub fn fit(rows: &[MispricingTrainingRow]) -> Result<Self, String> {
        let mut counts = [[ALPHA; 5]; 5];
        let mut samples = [0; 5];
        let mut by_event: HashMap<&str, Vec<&MispricingTrainingRow>> = HashMap::new();
        for row in rows {
            if !(row.model_probability.is_finite()
                && row.market_probability.is_finite()
                && (0.0..=1.0).contains(&row.model_probability)
                && (0.0..=1.0).contains(&row.market_probability))
            {
                return Err(format!(
                    "mispricing event {} has an invalid probability",
                    row.event_id
                ));
            }
            by_event.entry(&row.event_id).or_default().push(row);
        }
        for event in by_event.values_mut() {
            event.sort_by_key(|row| std::cmp::Reverse(row.seconds_to_expiry));
            for pair in event.windows(2) {
                let from = MispricingBin::from_error(
                    pair[0].model_probability - pair[0].market_probability,
                )
                .expect("validated probability") as usize;
                let to = MispricingBin::from_error(
                    pair[1].model_probability - pair[1].market_probability,
                )
                .expect("validated probability") as usize;
                counts[from][to] += 1.0;
                samples[from] += 1;
            }
        }
        if samples.iter().sum::<usize>() == 0 {
            return Err("mispricing training requires an event transition".to_string());
        }
        let mut transitions = [[0.0; 5]; 5];
        for (from, row) in counts.into_iter().enumerate() {
            let total: f64 = row.iter().sum();
            for (to, count) in row.into_iter().enumerate() {
                transitions[from][to] = count / total;
            }
        }
        Ok(Self {
            transitions,
            samples,
        })
    }

    pub fn estimate(
        &self,
        model_probability: f64,
        market_probability: f64,
    ) -> Result<MispricingEstimate, String> {
        if !(0.0..=1.0).contains(&model_probability) || !(0.0..=1.0).contains(&market_probability) {
            return Err("mispricing estimate requires finite probabilities in [0, 1]".to_string());
        }
        let from = MispricingBin::from_error(model_probability - market_probability)
            .expect("validated probability");
        let from_distance = from.distance_from_fair();
        let mut convergence_probability = 0.0;
        let mut persistence_probability = 0.0;
        let mut expansion_probability = 0.0;
        for (to, probability) in self.transitions[from as usize].iter().enumerate() {
            let to_distance = match to {
                0 | 4 => 2,
                1 | 3 => 1,
                _ => 0,
            };
            match to_distance.cmp(&from_distance) {
                std::cmp::Ordering::Less => convergence_probability += probability,
                std::cmp::Ordering::Equal => persistence_probability += probability,
                std::cmp::Ordering::Greater => expansion_probability += probability,
            }
        }
        Ok(MispricingEstimate {
            convergence_probability,
            persistence_probability,
            expansion_probability,
            effective_samples: self.samples[from as usize],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(event_id: &str, seconds: i64, distance: f64, settlement_up: bool) -> MarkovTrainingRow {
        MarkovTrainingRow {
            event_id: event_id.into(),
            seconds_to_expiry: seconds,
            state: MarketState::encode(distance, seconds, 0.8).unwrap(),
            settlement_up,
        }
    }

    #[test]
    fn time_conditioned_model_recurses_to_up_settlement() {
        let mut rows = Vec::new();
        for event in 0..300 {
            let id = format!("event-{event}");
            rows.extend([
                row(&id, 300, 2.0, true),
                row(&id, 180, 2.0, true),
                row(&id, 90, 2.0, true),
                row(&id, 30, 2.0, true),
            ]);
        }
        let model = MarkovModel::fit_rows(&rows);
        let estimate = model.estimate(MarketState::encode(2.0, 300, 0.8).unwrap());
        assert!(estimate.probability_up > 0.9);
        assert!(estimate.stability > 0.9);
        assert_eq!(estimate.effective_samples, 300);
        assert_eq!(model.training_events(), 300);
    }

    #[test]
    fn incomplete_event_is_not_training_evidence() {
        let model = MarkovModel::fit_rows(&[
            row("event-1", 300, 2.0, true),
            row("event-1", 180, 2.0, true),
            row("event-1", 30, 2.0, true),
        ]);
        let estimate = model.estimate(MarketState::encode(2.0, 300, 0.8).unwrap());
        assert_eq!(model.training_events(), 0);
        assert_eq!(estimate.effective_samples, 0);
        assert_eq!(estimate.standard_error, 0.5);
    }

    #[test]
    fn non_finite_state_is_rejected() {
        assert!(MarketState::encode(f64::NAN, 120, 0.0).is_none());
        assert!(MarketState::encode(0.0, 120, f64::INFINITY).is_none());
    }

    #[test]
    fn mispricing_model_separates_convergence_persistence_and_expansion() {
        let rows = [
            MispricingTrainingRow {
                event_id: "event-1".into(),
                seconds_to_expiry: 300,
                model_probability: 0.70,
                market_probability: 0.50,
            },
            MispricingTrainingRow {
                event_id: "event-1".into(),
                seconds_to_expiry: 290,
                model_probability: 0.58,
                market_probability: 0.50,
            },
            MispricingTrainingRow {
                event_id: "event-1".into(),
                seconds_to_expiry: 280,
                model_probability: 0.52,
                market_probability: 0.50,
            },
        ];
        let model = MispricingMarkov::fit(&rows).unwrap();
        let estimate = model.estimate(0.70, 0.50).unwrap();
        assert!(estimate.convergence_probability > 0.5);
        assert!(estimate.convergence_probability > estimate.expansion_probability);
        assert_eq!(estimate.effective_samples, 1);
    }
}
