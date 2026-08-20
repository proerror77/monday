use super::DeterministicRng;
use crate::{
    evaluation::ProposalContext, CandidateEvaluation, EngineProposal, HistoricalObservation,
    ProposalEngine, RemainingBudget,
};
use alpha_domain::{
    CandidateArtifact, CexGpPolicyV1, EngineKind, CEX_GP_POLICY_SCHEMA_V2, CEX_GP_POLICY_SCHEMA_V3,
};
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
            operators: policy
                .operators
                .iter()
                .filter(|operator| {
                    matches!(
                        operator,
                        FactorOperator::ZScore
                            | FactorOperator::Delta
                            | FactorOperator::Add
                            | FactorOperator::Sub
                            | FactorOperator::Mul
                    )
                })
                .cloned()
                .collect(),
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
            FactorOperator::Delta | FactorOperator::Mean | FactorOperator::ZScore => {
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

    fn governed_template(&self, iteration_index: usize) -> Result<Option<FactorAst>, String> {
        let Some(policy) = &self.governed_policy else {
            return Ok(None);
        };
        if iteration_index == 0 {
            return Ok(None);
        }
        let template_index = iteration_index - 1;
        let candidate = match policy.schema_version.as_str() {
            CEX_GP_POLICY_SCHEMA_V2 => self.atomic_dynamic_template(template_index)?,
            CEX_GP_POLICY_SCHEMA_V3 => self.dynamic_v3_template(template_index)?,
            _ => None,
        };
        if let Some(candidate) = candidate {
            policy
                .validate_candidate(&candidate)
                .map_err(|error| error.to_string())?;
            return Ok(Some(candidate));
        }
        Ok(None)
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
        if let Some(ast) = self.governed_template(iteration_index)? {
            self.seen.insert(ast.to_string());
            return Ok(EngineProposal {
                candidate_id: format!(
                    "{}-gp-{iteration_index}",
                    self.candidate_namespace.as_deref().unwrap_or(mission_id)
                ),
                hypothesis: "fixed causal normalization and entry threshold over one registered factor field"
                    .to_string(),
                artifact: CandidateArtifact::Formula(ast),
                expansions: 1,
                tokens: 0,
                elapsed_ms: 0,
            });
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

impl GeneticProgrammingEngine {
    fn atomic_dynamic_template(&self, template_index: usize) -> Result<Option<FactorAst>, String> {
        let Some(field) = self.fields.get(template_index / 2) else {
            return Ok(None);
        };
        let value = if template_index.is_multiple_of(2) {
            standardized_field(field)?
        } else {
            standardized_delta(field)?
        };
        Ok(Some(thresholded_standard_signal(value)?))
    }

    fn dynamic_v3_template(&self, template_index: usize) -> Result<Option<FactorAst>, String> {
        let atomic_templates = self.fields.len() * 2;
        if template_index < atomic_templates {
            return self.atomic_dynamic_template(template_index);
        }
        let named_index = template_index - atomic_templates;
        let candidate = match named_index {
            0 => thresholded_standard_signal(negate(standardized_field("spread_bps")?)?)?,
            1 => thresholded_standard_signal(negate(standardized_delta("spread_bps")?)?)?,
            2 => directional_consensus_signal(
                standardized_field("book_imbalance")?,
                standardized_field("weighted_book_imbalance_top5")?,
            )?,
            3 => thresholded_signal(
                standardized_field("weighted_book_imbalance_top5")?,
                add_one_plus_one()?,
            )?,
            _ => return Ok(None),
        };
        Ok(Some(candidate))
    }
}

fn constant(value: &str) -> FactorAst {
    FactorAst::Terminal(FactorTerminal::Constant(value.to_string()))
}

fn field_terminal(field: &str) -> FactorAst {
    FactorAst::Terminal(FactorTerminal::Field(field.to_string()))
}

fn standardized_field(field: &str) -> Result<FactorAst, String> {
    FactorAst::call(
        FactorOperator::ZScore,
        vec![field_terminal(field), constant("20")],
    )
    .map_err(|error| error.to_string())
}

fn standardized_delta(field: &str) -> Result<FactorAst, String> {
    FactorAst::call(
        FactorOperator::ZScore,
        vec![
            FactorAst::call(
                FactorOperator::Delta,
                vec![field_terminal(field), constant("5")],
            )
            .map_err(|error| error.to_string())?,
            constant("20"),
        ],
    )
    .map_err(|error| error.to_string())
}

fn negate(value: FactorAst) -> Result<FactorAst, String> {
    FactorAst::call(FactorOperator::Mul, vec![constant("-1"), value])
        .map_err(|error| error.to_string())
}

fn add_one_plus_one() -> Result<FactorAst, String> {
    FactorAst::call(FactorOperator::Add, vec![constant("1"), constant("1")])
        .map_err(|error| error.to_string())
}

fn thresholded_standard_signal(value: FactorAst) -> Result<FactorAst, String> {
    FactorAst::call(
        FactorOperator::IfElse,
        vec![
            FactorAst::call(
                FactorOperator::GreaterThan,
                vec![value.clone(), constant("1")],
            )
            .map_err(|error| error.to_string())?,
            constant("1"),
            FactorAst::call(
                FactorOperator::IfElse,
                vec![
                    FactorAst::call(FactorOperator::LessThan, vec![value, constant("-1")])
                        .map_err(|error| error.to_string())?,
                    constant("-1"),
                    constant("0"),
                ],
            )
            .map_err(|error| error.to_string())?,
        ],
    )
    .map_err(|error| error.to_string())
}

fn thresholded_signal(value: FactorAst, threshold: FactorAst) -> Result<FactorAst, String> {
    let negative_threshold =
        FactorAst::call(FactorOperator::Sub, vec![constant("0"), threshold.clone()])
            .map_err(|error| error.to_string())?;
    FactorAst::call(
        FactorOperator::IfElse,
        vec![
            FactorAst::call(FactorOperator::GreaterThan, vec![value.clone(), threshold])
                .map_err(|error| error.to_string())?,
            constant("1"),
            FactorAst::call(
                FactorOperator::IfElse,
                vec![
                    FactorAst::call(FactorOperator::LessThan, vec![value, negative_threshold])
                        .map_err(|error| error.to_string())?,
                    constant("-1"),
                    constant("0"),
                ],
            )
            .map_err(|error| error.to_string())?,
        ],
    )
    .map_err(|error| error.to_string())
}

fn directional_consensus_signal(left: FactorAst, right: FactorAst) -> Result<FactorAst, String> {
    let right_positive = right.clone();
    FactorAst::call(
        FactorOperator::IfElse,
        vec![
            FactorAst::call(
                FactorOperator::GreaterThan,
                vec![left.clone(), constant("1")],
            )
            .map_err(|error| error.to_string())?,
            FactorAst::call(
                FactorOperator::IfElse,
                vec![
                    FactorAst::call(
                        FactorOperator::GreaterThan,
                        vec![right_positive, constant("1")],
                    )
                    .map_err(|error| error.to_string())?,
                    constant("1"),
                    constant("0"),
                ],
            )
            .map_err(|error| error.to_string())?,
            FactorAst::call(
                FactorOperator::IfElse,
                vec![
                    FactorAst::call(FactorOperator::LessThan, vec![left, constant("-1")])
                        .map_err(|error| error.to_string())?,
                    FactorAst::call(
                        FactorOperator::IfElse,
                        vec![
                            FactorAst::call(FactorOperator::LessThan, vec![right, constant("-1")])
                                .map_err(|error| error.to_string())?,
                            constant("-1"),
                            constant("0"),
                        ],
                    )
                    .map_err(|error| error.to_string())?,
                    constant("0"),
                ],
            )
            .map_err(|error| error.to_string())?,
        ],
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn governed_budget(max_candidates: usize) -> alpha_domain::SearchBudget {
        alpha_domain::SearchBudget {
            max_candidates,
            max_expansions: 256,
            max_tokens: 0,
            max_seconds: 0,
        }
    }

    fn governed_remaining(candidates: usize) -> RemainingBudget {
        RemainingBudget {
            candidates,
            expansions: 256,
            tokens: 0,
            milliseconds: 0,
        }
    }

    fn named_template_fields() -> Vec<String> {
        [
            "ask_depth_top5",
            "bid_depth_top5",
            "book_imbalance",
            "book_imbalance_top5",
            "near_depth_concentration_skew_top5",
            "spread_bps",
            "vwap_center_deviation_top5_bps",
            "weighted_book_imbalance_top5",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

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

    fn governed_templates(policy: CexGpPolicyV1, count: usize) -> Vec<CandidateArtifact> {
        let mut engine = GeneticProgrammingEngine::new_governed(policy, "mission").unwrap();
        let dataset = super::super::test_dataset();
        let remaining = governed_remaining(count);
        (1..=count)
            .map(|iteration| {
                engine
                    .propose(
                        "mission",
                        iteration,
                        &dataset.proposal_context(),
                        &remaining,
                    )
                    .unwrap()
                    .artifact
            })
            .collect()
    }

    fn scores_change_candidates(
        mut left: GeneticProgrammingEngine,
        mut right: GeneticProgrammingEngine,
    ) -> bool {
        let dataset = super::super::test_dataset();
        let remaining = governed_remaining(8);
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
        let budget = governed_budget(8);
        let policy = CexGpPolicyV1::controlled_v1("policy", fields, 7, &budget).unwrap();
        let left = GeneticProgrammingEngine::new_governed(policy.clone(), "mission").unwrap();
        let right = GeneticProgrammingEngine::new_governed(policy, "mission").unwrap();
        assert!(!scores_change_candidates(left, right));
    }

    #[test]
    fn governed_gp_resume_matches_the_uninterrupted_sequence() {
        let budget = governed_budget(8);
        let policy = CexGpPolicyV1::controlled_v1(
            "policy",
            vec!["book_imbalance".to_string(), "spread_bps".to_string()],
            7,
            &budget,
        )
        .unwrap();
        let dataset = super::super::test_dataset();
        let remaining = governed_remaining(8);
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

    #[test]
    fn governed_gp_emits_atomic_dynamic_templates_before_composition() {
        let budget = governed_budget(8);
        let policy = CexGpPolicyV1::controlled_dynamic_v2(
            "policy",
            vec!["spread_bps".to_string(), "book_imbalance".to_string()],
            7,
            &budget,
        )
        .unwrap();
        let mut engine = GeneticProgrammingEngine::new_governed(policy, "mission").unwrap();
        let dataset = super::super::test_dataset();
        let remaining = governed_remaining(8);
        let templates = (1..=4)
            .map(|iteration| {
                engine
                    .propose(
                        "mission",
                        iteration,
                        &dataset.proposal_context(),
                        &remaining,
                    )
                    .unwrap()
                    .artifact
            })
            .collect::<Vec<_>>();

        let rendered = templates
            .iter()
            .map(|artifact| match artifact {
                CandidateArtifact::Formula(ast) => ast.to_string(),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        assert!(rendered[0].contains("zscore(book_imbalance, 20)"));
        assert!(rendered[1].contains("zscore(delta(book_imbalance, 5), 20)"));
        assert!(rendered[2].contains("zscore(spread_bps, 20)"));
        assert!(rendered[3].contains("zscore(delta(spread_bps, 5), 20)"));
        assert!(rendered.iter().all(|formula| {
            formula.contains(" > 1") && formula.contains(" < -1") && formula.contains("if_else")
        }));
    }

    #[test]
    fn governed_gp_v2_keeps_the_preexisting_seventeenth_candidate() {
        let budget = governed_budget(20);
        let policy =
            CexGpPolicyV1::controlled_dynamic_v2("policy", named_template_fields(), 7, &budget)
                .unwrap();
        let mut engine = GeneticProgrammingEngine::new_governed(policy, "mission").unwrap();
        let dataset = super::super::test_dataset();
        let remaining = governed_remaining(20);
        for iteration in 1..=16 {
            let proposal = engine
                .propose(
                    "mission",
                    iteration,
                    &dataset.proposal_context(),
                    &remaining,
                )
                .unwrap();
            engine.observe(&proposal, &evaluation(1.0));
        }
        let seventeenth = engine
            .propose("mission", 17, &dataset.proposal_context(), &remaining)
            .unwrap();
        let expected = FactorAst::call(
            FactorOperator::Mul,
            vec![
                thresholded_standard_signal(standardized_delta("book_imbalance_top5").unwrap())
                    .unwrap(),
                field_terminal("bid_depth_top5"),
            ],
        )
        .unwrap();

        assert_eq!(seventeenth.artifact, CandidateArtifact::Formula(expected));
    }

    #[test]
    fn governed_gp_v3_emits_sixteen_atomic_then_four_named_templates() {
        let budget = governed_budget(20);
        let fields = named_template_fields();
        let v2_templates = governed_templates(
            CexGpPolicyV1::controlled_dynamic_v2("policy-v2", fields.clone(), 7, &budget).unwrap(),
            16,
        );
        assert_eq!(
            alpha_domain::canonical_json_hash(&v2_templates).unwrap(),
            "3fb55524158416564acd9c01dd780b2b3b9934c990fb764ae93afef38a1504d4"
        );
        let templates = governed_templates(
            CexGpPolicyV1::controlled_dynamic_v3("policy", fields, 7, &budget).unwrap(),
            20,
        );
        assert_eq!(&templates[..16], v2_templates.as_slice());

        let expected_named = vec![
            CandidateArtifact::Formula(
                thresholded_standard_signal(
                    negate(standardized_field("spread_bps").unwrap()).unwrap(),
                )
                .unwrap(),
            ),
            CandidateArtifact::Formula(
                thresholded_standard_signal(
                    negate(standardized_delta("spread_bps").unwrap()).unwrap(),
                )
                .unwrap(),
            ),
            CandidateArtifact::Formula(
                directional_consensus_signal(
                    standardized_field("book_imbalance").unwrap(),
                    standardized_field("weighted_book_imbalance_top5").unwrap(),
                )
                .unwrap(),
            ),
            CandidateArtifact::Formula(
                thresholded_signal(
                    standardized_field("weighted_book_imbalance_top5").unwrap(),
                    add_one_plus_one().unwrap(),
                )
                .unwrap(),
            ),
        ];

        assert_eq!(&templates[16..], expected_named.as_slice());
    }
}
