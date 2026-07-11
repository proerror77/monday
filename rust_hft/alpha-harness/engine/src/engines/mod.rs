mod bayesian;
mod gp;
mod mcts;
mod offline_rl;

use serde::{Deserialize, Serialize};

pub use bayesian::BayesianOptimizerEngine;
pub use gp::GeneticProgrammingEngine;
pub use mcts::{MctsEngine, MctsNodeSnapshot};
pub use offline_rl::{OfflineRlEngine, OfflineTrace};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeterministicRng(u64);

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    fn index(&mut self, len: usize) -> usize {
        (self.next_u64() as usize) % len
    }
}

#[cfg(test)]
fn test_dataset() -> crate::evaluation::PreparedDataset {
    use crate::evaluation::{prepare_dataset, ResearchRow, WalkForwardConfig};
    use chrono::{Duration, Utc};

    let start = Utc::now();
    let rows = (0..4)
        .map(|index| ResearchRow {
            available_time: start + Duration::seconds(index),
            signal: index as f64,
            label: index as f64,
            fee_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
        })
        .collect();
    prepare_dataset(
        rows,
        &WalkForwardConfig {
            initial_train_rows: 1,
            validation_rows: 1,
            fold_count: 1,
            purge_rows: 0,
            embargo_rows: 0,
            sealed_holdout_rows: 1,
        },
        "engine-checkpoint-test",
    )
    .unwrap()
}
