mod bayesian;
mod gp;
mod mcts;
mod offline_rl;

pub use bayesian::BayesianOptimizerEngine;
pub use gp::GeneticProgrammingEngine;
pub use mcts::{MctsEngine, MctsNodeSnapshot};
pub use offline_rl::{OfflineRlEngine, OfflineTrace};

#[derive(Debug, Clone)]
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
