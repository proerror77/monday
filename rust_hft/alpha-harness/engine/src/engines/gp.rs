use super::DeterministicRng;
use crate::{
    evaluation::ProposalContext, CandidateEvaluation, EngineProposal, HistoricalObservation,
    ProposalEngine, RemainingBudget,
};
use alpha_domain::{CandidateArtifact, CexGpPolicyV1, EngineKind};
use hft_factor_dsl::{FactorAst, FactorOperator, FactorTerminal};
use std::collections::BTreeSet;

pub struct GeneticProgrammingEngine {
    rng: DeterministicRng,
    fields: Vec<String>,
    operators: Vec<FactorOperator>,
    windows: Vec<String>,
    population_limit: usize,
    max_depth: usize,
    candidate_namespace: Option<String>,
    governed_policy: Option<CexGpPolicyV1>,
    population: Vec<(FactorAst, f64)>,
    seen: BTreeSet<String>,
}

impl GeneticProgrammingEngine {
    pub fn new(
        seed: u64,
        fields: Vec<String>,
        population_limit: usize,
        max_depth: usize,
    ) -> Result<Self, String> {
        if fields.is_empty() || fields.iter().any(|field| field.trim().is_empty()) {
            return Err("GP requires non-empty fields".to_string());
        }
        if population_limit == 0 || max_depth == 0 {
            return Err("GP population and depth limits must be positive".to_string());
        }
        Ok(Self {
            rng: DeterministicRng::new(seed),
            fields,
            operators: vec![
                FactorOperator::Rank,
                FactorOperator::Delta,
                FactorOperator::Mean,
                FactorOperator::Add,
                FactorOperator::Sub,
                FactorOperator::Mul,
            ],
            windows: ["5", "10", "20", "40", "60"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            population_limit,
            max_depth,
            candidate_namespace: None,
            governed_policy: None,
            population: Vec::new(),
            seen: BTreeSet::new(),
        })
    }

    pub fn new_governed(
        policy: CexGpPolicyV1,
        candidate_namespace: impl Into<String>,
    ) -> Result<Self, String> {
        policy.validate().map_err(|error| error.to_string())?;
        let candidate_namespace = candidate_namespace.into();
        if candidate_namespace.trim().is_empty() {
            return Err("governed GP candidate namespace cannot be empty".to_string());
        }
        Ok(Self {
            rng: DeterministicRng::new(policy.seed),
            fields: policy.admitted_fields.clone(),
            operators: policy.operators.clone(),
            windows: policy.windows.iter().map(ToString::to_string).collect(),
            population_limit: policy.population_limit,
            max_depth: policy.max_ast_depth,
            candidate_namespace: Some(candidate_namespace),
            governed_policy: Some(policy),
            population: Vec::new(),
            seen: BTreeSet::new(),
        })
    }

    fn build_candidate(&mut self) -> Result<FactorAst, String> {
        let base = if self.population.is_empty() {
            self.field()
        } else {
            let selected = if self.governed_policy.is_some() {
                self.population[self.rng.index(self.population.len())]
                    .0
                    .clone()
            } else {
                let tournament = (0..self.population.len().min(3))
                    .map(|_| self.rng.index(self.population.len()))
                    .max_by(|left, right| {
                        self.population[*left]
                            .1
                            .total_cmp(&self.population[*right].1)
                    })
                    .unwrap_or(0);
                self.population[tournament].0.clone()
            };
            if ast_depth(&selected) >= self.max_depth {
                self.field()
            } else {
                selected
            }
        };
        let operator = self.operators[self.rng.index(self.operators.len())].clone();
        let candidate = match operator {
            FactorOperator::Rank => FactorAst::call(operator, vec![base]),
            FactorOperator::Delta | FactorOperator::Mean => {
                FactorAst::call(operator, vec![base, self.window_constant()?])
            }
            FactorOperator::Add | FactorOperator::Sub | FactorOperator::Mul => {
                FactorAst::call(operator, vec![base, self.field()])
            }
            _ => return Err("GP operator is not implemented by this engine".to_string()),
        }
        .map_err(|error| error.to_string())?;
        candidate.validate().map_err(|error| error.to_string())?;
        if let Some(policy) = &self.governed_policy {
            policy
                .validate_candidate(&candidate)
                .map_err(|error| error.to_string())?;
        }
        Ok(candidate)
    }

    fn field(&mut self) -> FactorAst {
        FactorAst::Terminal(FactorTerminal::Field(
            self.fields[self.rng.index(self.fields.len())].clone(),
        ))
    }

    fn window_constant(&mut self) -> Result<FactorAst, String> {
        if self.windows.is_empty() {
            return Err("GP rolling operator requires an admitted window".to_string());
        }
        Ok(FactorAst::Terminal(FactorTerminal::Constant(
            self.windows[self.rng.index(self.windows.len())].clone(),
        )))
    }

    fn remember(&mut self, ast: FactorAst, score: f64) {
        self.population.push((ast, score));
        if self.governed_policy.is_some() {
            if self.population.len() > self.population_limit {
                self.population.remove(0);
            }
        } else {
            self.population
                .sort_by(|left, right| right.1.total_cmp(&left.1));
            self.population.truncate(self.population_limit);
        }
    }
}

fn ast_depth(ast: &FactorAst) -> usize {
    match ast {
        FactorAst::Terminal(_) => 1,
        FactorAst::Call { args, .. } => 1 + args.iter().map(ast_depth).max().unwrap_or(0),
    }
}

impl ProposalEngine for GeneticProgrammingEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::GeneticProgramming
    }

    fn propose(
        &mut self,
        mission_id: &str,
        iteration_index: usize,
        _context: &ProposalContext<'_>,
        remaining: &RemainingBudget,
    ) -> Result<EngineProposal, String> {
        if remaining.expansions == 0 {
            return Err("GP expansion budget is exhausted".to_string());
        }
        let governed = self.governed_policy.is_some();
        if let Some(policy) = &self.governed_policy {
            let iteration = u64::try_from(iteration_index)
                .map_err(|_| "GP iteration index exceeds the deterministic seed range")?;
            self.rng = DeterministicRng::new(policy.seed.wrapping_add(iteration));
        }
        let max_attempts = if governed {
            1
        } else {
            remaining.expansions.min(32) as usize
        };
        for attempt in 1..=max_attempts {
            let ast = self.build_candidate()?;
            if self.seen.insert(ast.to_string()) || governed {
                return Ok(EngineProposal {
                    candidate_id: format!(
                        "{}-gp-{iteration_index}",
                        self.candidate_namespace.as_deref().unwrap_or(mission_id)
                    ),
                    hypothesis: "genetic programming mutation over registered factor fields"
                        .to_string(),
                    artifact: CandidateArtifact::Formula(ast),
                    expansions: attempt as u64,
                    tokens: 0,
                    elapsed_ms: 0,
                });
            }
        }
        Err("GP could not produce a novel candidate within budget".to_string())
    }

    fn observe(&mut self, proposal: &EngineProposal, evaluation: &CandidateEvaluation) {
        let CandidateArtifact::Formula(ast) = &proposal.artifact else {
            return;
        };
        let score = if self.governed_policy.is_some() {
            0.0
        } else {
            evaluation.score
        };
        self.remember(ast.clone(), score);
    }

    fn restore(&mut self, observations: &[HistoricalObservation]) -> Result<(), String> {
        for observation in observations {
            let CandidateArtifact::Formula(ast) = &observation.proposal.artifact else {
                return Err("GP history contains a non-formula artifact".to_string());
            };
            self.seen.insert(ast.to_string());
            let score = if self.governed_policy.is_some() {
                0.0
            } else {
                observation.evaluation.score
            };
            self.remember(ast.clone(), score);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluation(score: f64) -> CandidateEvaluation {
        CandidateEvaluation {
            passed: true,
            score,
            failure_reasons: vec![],
            evaluator_version: "test".to_string(),
            evaluator_config: serde_json::json!({"fixture": true}),
            evaluation_protocol: None,
            evaluation_protocol_hash: None,
            metrics: crate::EvaluationMetrics {
                predictive: crate::PredictiveMetrics::from_folds(vec![]),
                row_count: 1,
                trade_count: 1,
                total_turnover: 1.0,
                mean_net_return: score,
                cumulative_net_return: score,
                max_drawdown: 0.0,
                net_sharpe: score,
                raw_score: score,
                adjusted_score: score,
                folds: vec![],
            },
        }
    }

    fn scores_change_candidates(
        mut left: GeneticProgrammingEngine,
        mut right: GeneticProgrammingEngine,
    ) -> bool {
        let dataset = super::super::test_dataset();
        let remaining = RemainingBudget {
            candidates: 8,
            expansions: 256,
            tokens: 0,
            milliseconds: 0,
        };
        for iteration in 1..=8 {
            let context = dataset.proposal_context();
            let left_proposal = left
                .propose("mission", iteration, &context, &remaining)
                .unwrap();
            let right_proposal = right
                .propose("mission", iteration, &context, &remaining)
                .unwrap();
            if left_proposal.artifact != right_proposal.artifact {
                return true;
            }
            left.observe(&left_proposal, &evaluation(iteration as f64));
            right.observe(&right_proposal, &evaluation(-(iteration as f64)));
        }
        false
    }

    #[test]
    fn seeded_gp_is_reproducible() {
        let fields = vec!["oi".to_string(), "imbalance".to_string()];
        let mut left = GeneticProgrammingEngine::new(7, fields.clone(), 4, 4).unwrap();
        let mut right = GeneticProgrammingEngine::new(7, fields, 4, 4).unwrap();
        for _ in 0..5 {
            assert_eq!(
                left.build_candidate().unwrap(),
                right.build_candidate().unwrap()
            );
        }
    }

    #[test]
    fn gp_respects_ast_depth_limit() {
        let mut engine = GeneticProgrammingEngine::new(7, vec!["oi".to_string()], 4, 3).unwrap();
        for _ in 0..20 {
            let ast = engine.build_candidate().unwrap();
            assert!(ast_depth(&ast) <= 3);
            engine.population.push((ast, 1.0));
        }
    }

    #[test]
    fn generic_gp_remains_fitness_guided() {
        let fields = vec!["book_imbalance".to_string(), "spread_bps".to_string()];
        let left = GeneticProgrammingEngine::new(7, fields.clone(), 8, 5).unwrap();
        let right = GeneticProgrammingEngine::new(7, fields, 8, 5).unwrap();
        assert!(scores_change_candidates(left, right));
    }

    #[test]
    fn governed_gp_does_not_observe_evaluation_scores() {
        let fields = vec!["book_imbalance".to_string(), "spread_bps".to_string()];
        let budget = alpha_domain::SearchBudget {
            max_candidates: 8,
            max_expansions: 256,
            max_tokens: 0,
            max_seconds: 0,
        };
        let policy = CexGpPolicyV1::controlled_v1("policy", fields, 7, &budget).unwrap();
        let left = GeneticProgrammingEngine::new_governed(policy.clone(), "mission").unwrap();
        let right = GeneticProgrammingEngine::new_governed(policy, "mission").unwrap();
        assert!(!scores_change_candidates(left, right));
    }

    #[test]
    fn governed_gp_resume_matches_the_uninterrupted_sequence() {
        let budget = alpha_domain::SearchBudget {
            max_candidates: 8,
            max_expansions: 256,
            max_tokens: 0,
            max_seconds: 0,
        };
        let policy = CexGpPolicyV1::controlled_v1(
            "policy",
            vec!["book_imbalance".to_string(), "spread_bps".to_string()],
            7,
            &budget,
        )
        .unwrap();
        let dataset = super::super::test_dataset();
        let remaining = RemainingBudget {
            candidates: 8,
            expansions: 256,
            tokens: 0,
            milliseconds: 0,
        };
        let mut uninterrupted =
            GeneticProgrammingEngine::new_governed(policy.clone(), "mission").unwrap();
        let mut history = Vec::new();
        let mut expected = Vec::new();
        for iteration in 1..=4 {
            let proposal = uninterrupted
                .propose(
                    "mission",
                    iteration,
                    &dataset.proposal_context(),
                    &remaining,
                )
                .unwrap();
            let evaluation = evaluation(1.0);
            uninterrupted.observe(&proposal, &evaluation);
            if iteration <= 2 {
                history.push(HistoricalObservation {
                    proposal,
                    evaluation,
                });
            } else {
                expected.push(proposal.artifact);
            }
        }

        let mut resumed = GeneticProgrammingEngine::new_governed(policy, "mission").unwrap();
        resumed.restore(&history).unwrap();
        let actual = (3..=4)
            .map(|iteration| {
                let proposal = resumed
                    .propose(
                        "mission",
                        iteration,
                        &dataset.proposal_context(),
                        &remaining,
                    )
                    .unwrap();
                resumed.observe(&proposal, &evaluation(1.0));
                proposal.artifact
            })
            .collect::<Vec<_>>();

        assert_eq!(actual, expected);
    }
}
