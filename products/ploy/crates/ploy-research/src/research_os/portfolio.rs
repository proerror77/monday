use serde::{Deserialize, Serialize};

const MAX_CORR_EXISTING: f64 = 0.70;
const MIN_FULL_DEPTH_FILL_RATE: f64 = 0.30;
const MIN_PORTFOLIO_SIZE: usize = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorPortfolioInput {
    pub factors: Vec<FactorPortfolioCandidate>,
    #[serde(default)]
    pub pairwise_correlations: Vec<PairwiseCorrelation>,
    #[serde(default = "default_max_selected")]
    pub max_selected: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorPortfolioCandidate {
    pub factor_name: String,
    pub dsl_hash: String,
    pub reward: f64,
    #[serde(default)]
    pub ic: Option<f64>,
    #[serde(default)]
    pub icir: Option<f64>,
    #[serde(default)]
    pub test_pnl: Option<f64>,
    #[serde(default)]
    pub top_bucket_label: Option<f64>,
    #[serde(default)]
    pub turnover_proxy: f64,
    #[serde(default)]
    pub capacity_penalty: f64,
    #[serde(default)]
    pub top_bucket_full_depth_entry_fill_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairwiseCorrelation {
    pub left_hash: String,
    pub right_hash: String,
    pub corr: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorPortfolioOutput {
    pub selected_factors: Vec<SelectedFactor>,
    pub rejected_factors: Vec<RejectedFactor>,
    pub aggregate_expected_reward: f64,
    pub max_pairwise_correlation: f64,
    pub promotion_decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectedFactor {
    pub factor_name: String,
    pub dsl_hash: String,
    pub reward: f64,
    pub marginal_score: f64,
    pub max_corr_existing: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedFactor {
    pub factor_name: String,
    pub dsl_hash: String,
    pub reason: String,
    pub marginal_score: f64,
    pub max_corr_existing: f64,
}

pub fn build_factor_portfolio(input: &FactorPortfolioInput) -> FactorPortfolioOutput {
    let mut candidates = input.factors.clone();
    candidates.sort_by(|left, right| {
        right
            .reward
            .partial_cmp(&left.reward)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.dsl_hash.cmp(&right.dsl_hash))
    });

    let max_selected = input.max_selected.max(1);
    let mut selected = Vec::new();
    let mut rejected = Vec::new();

    for candidate in candidates {
        if selected.len() >= max_selected {
            rejected.push(reject(
                &candidate,
                "portfolio_size_limit",
                0.0,
                marginal_score(&candidate),
            ));
            continue;
        }

        let max_corr_existing = selected
            .iter()
            .map(|selected: &SelectedFactor| {
                correlation_between(
                    &candidate.dsl_hash,
                    &selected.dsl_hash,
                    &input.pairwise_correlations,
                )
            })
            .fold(0.0_f64, f64::max);
        let score = marginal_score(&candidate);

        if max_corr_existing >= MAX_CORR_EXISTING {
            rejected.push(reject(
                &candidate,
                "max_corr_existing_ge_0_70",
                max_corr_existing,
                score,
            ));
            continue;
        }
        if candidate.top_bucket_full_depth_entry_fill_rate < MIN_FULL_DEPTH_FILL_RATE {
            rejected.push(reject(
                &candidate,
                "top_bucket_full_depth_entry_fill_rate_lt_0_30",
                max_corr_existing,
                score,
            ));
            continue;
        }
        if score <= 0.0 {
            rejected.push(reject(
                &candidate,
                "non_positive_marginal_score",
                max_corr_existing,
                score,
            ));
            continue;
        }

        selected.push(SelectedFactor {
            factor_name: candidate.factor_name,
            dsl_hash: candidate.dsl_hash,
            reward: candidate.reward,
            marginal_score: round4(score),
            max_corr_existing: round4(max_corr_existing),
        });
    }

    let aggregate_expected_reward = selected
        .iter()
        .map(|factor| factor.marginal_score)
        .sum::<f64>();
    let max_pairwise_correlation = selected
        .iter()
        .enumerate()
        .flat_map(|(idx, left)| {
            selected.iter().skip(idx + 1).map(move |right| {
                correlation_between(
                    &left.dsl_hash,
                    &right.dsl_hash,
                    &input.pairwise_correlations,
                )
            })
        })
        .fold(0.0_f64, f64::max);
    let promotion_decision = if selected.len() >= MIN_PORTFOLIO_SIZE {
        "portfolio_candidate"
    } else if rejected
        .iter()
        .any(|factor| factor.reason == "max_corr_existing_ge_0_70")
    {
        "revise"
    } else {
        "continue"
    };

    FactorPortfolioOutput {
        selected_factors: selected,
        rejected_factors: rejected,
        aggregate_expected_reward: round4(aggregate_expected_reward),
        max_pairwise_correlation: round4(max_pairwise_correlation),
        promotion_decision: promotion_decision.to_string(),
    }
}

fn default_max_selected() -> usize {
    4
}

fn marginal_score(candidate: &FactorPortfolioCandidate) -> f64 {
    candidate.reward - candidate.turnover_proxy.abs() * 0.10 - candidate.capacity_penalty.abs()
}

fn correlation_between(left: &str, right: &str, correlations: &[PairwiseCorrelation]) -> f64 {
    correlations
        .iter()
        .find(|corr| {
            (corr.left_hash == left && corr.right_hash == right)
                || (corr.left_hash == right && corr.right_hash == left)
        })
        .map(|corr| corr.corr.abs())
        .unwrap_or(0.0)
}

fn reject(
    candidate: &FactorPortfolioCandidate,
    reason: &str,
    max_corr_existing: f64,
    marginal_score: f64,
) -> RejectedFactor {
    RejectedFactor {
        factor_name: candidate.factor_name.clone(),
        dsl_hash: candidate.dsl_hash.clone(),
        reason: reason.to_string(),
        marginal_score: round4(marginal_score),
        max_corr_existing: round4(max_corr_existing),
    }
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str, reward: f64, fill_rate: f64) -> FactorPortfolioCandidate {
        FactorPortfolioCandidate {
            factor_name: name.to_string(),
            dsl_hash: format!("hash-{name}"),
            reward,
            ic: Some(0.02),
            icir: Some(0.5),
            test_pnl: Some(reward * 10.0),
            top_bucket_label: Some(reward),
            turnover_proxy: 0.1,
            capacity_penalty: 0.0,
            top_bucket_full_depth_entry_fill_rate: fill_rate,
        }
    }

    #[test]
    fn selects_decorrelated_fillable_positive_factors() {
        let input = FactorPortfolioInput {
            factors: vec![candidate("a", 1.0, 0.8), candidate("b", 0.8, 0.7)],
            pairwise_correlations: vec![PairwiseCorrelation {
                left_hash: "hash-a".to_string(),
                right_hash: "hash-b".to_string(),
                corr: 0.25,
            }],
            max_selected: 4,
        };

        let output = build_factor_portfolio(&input);

        assert_eq!(output.selected_factors.len(), 2);
        assert_eq!(output.rejected_factors.len(), 0);
        assert_eq!(output.max_pairwise_correlation, 0.25);
        assert_eq!(output.promotion_decision, "portfolio_candidate");
    }

    #[test]
    fn rejects_highly_correlated_candidates() {
        let input = FactorPortfolioInput {
            factors: vec![candidate("a", 1.0, 0.8), candidate("b", 0.9, 0.8)],
            pairwise_correlations: vec![PairwiseCorrelation {
                left_hash: "hash-a".to_string(),
                right_hash: "hash-b".to_string(),
                corr: 0.71,
            }],
            max_selected: 4,
        };

        let output = build_factor_portfolio(&input);

        assert_eq!(output.selected_factors.len(), 1);
        assert_eq!(
            output.rejected_factors[0].reason,
            "max_corr_existing_ge_0_70"
        );
        assert_eq!(output.promotion_decision, "revise");
    }

    #[test]
    fn rejects_unfillable_and_non_positive_marginal_score() {
        let mut negative = candidate("negative", 0.05, 0.8);
        negative.turnover_proxy = 1.0;
        negative.capacity_penalty = 0.1;
        let input = FactorPortfolioInput {
            factors: vec![candidate("thin", 1.0, 0.29), negative],
            pairwise_correlations: vec![],
            max_selected: 4,
        };

        let output = build_factor_portfolio(&input);
        let reasons = output
            .rejected_factors
            .iter()
            .map(|factor| factor.reason.as_str())
            .collect::<Vec<_>>();

        assert!(reasons.contains(&"top_bucket_full_depth_entry_fill_rate_lt_0_30"));
        assert!(reasons.contains(&"non_positive_marginal_score"));
        assert_eq!(output.promotion_decision, "continue");
    }
}
