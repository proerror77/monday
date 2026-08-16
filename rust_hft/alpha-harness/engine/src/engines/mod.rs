mod bayesian;
mod factor_bank_mcts;
mod gp;
mod mcts;
mod offline_rl;

use hft_search_kernel::DeterministicRng;

pub(crate) use bayesian::solve;
pub use bayesian::BayesianOptimizerEngine;
pub use factor_bank_mcts::{
    CexCombinationResearchArtifactV1, CexFactorBankMcts, CexFactorBankMctsCheckpointV1,
    CexFactorBankMctsResultV1, CexFactorBankMctsStopReasonV1, CexFactorBankMctsTraceStepV1,
    CexFactorIdentityV1, CexFactorSubsetActionV1, CexFactorSubsetV1,
    CEX_FACTOR_BANK_MCTS_IMPLEMENTATION_VERSION,
};
pub use gp::GeneticProgrammingEngine;
pub use mcts::{MctsEngine, MctsNodeSnapshot, MCTS_CHECKPOINT_VERSION};
pub use offline_rl::{OfflineRlEngine, OfflineTrace};

#[cfg(test)]
fn test_dataset() -> crate::evaluation::PreparedDataset {
    use crate::evaluation::{prepare_dataset, ResearchRow};
    use alpha_domain::{
        EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationProtocolV1, EvaluationWalkForwardV1,
    };
    use chrono::{Duration, Utc};

    let start = Utc::now();
    let rows = (0..4)
        .map(|index| ResearchRow {
            available_time: start + Duration::seconds(index),
            signal: index as f64,
            features: std::collections::BTreeMap::new(),
            label: index as f64,
            fee_bps: 0.0,
            funding_bps: 0.0,
            pit_funding: false,
            latency_bps: 0.0,
        })
        .collect();
    prepare_dataset(
        rows,
        &EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 1,
                validation_rows: 1,
                fold_count: 1,
                purge_rows: 1,
                embargo_rows: 0,
                sealed_holdout_rows: 1,
            },
            EvaluationCostsV1 {
                fee_bps: 0.0,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.0,
                slippage_bps: 0.0,
                cross_spread: false,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            },
            EvaluationLabelSpecV1 {
                horizon_buckets: 1,
                observation_frequency_millis: 1_000,
            },
        )
        .unwrap(),
    )
    .unwrap()
}
