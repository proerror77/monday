use super::DeterministicRng;
use crate::{
    evaluation::EngineContext, CandidateEvaluation, EngineProposal, HistoricalObservation,
    ProposalEngine, RemainingBudget,
};
use alpha_domain::{CandidateArtifact, EngineKind};
use hft_factor_dsl::{FactorAst, FactorOperator, FactorTerminal};
use std::collections::BTreeSet;

pub struct GeneticProgrammingEngine {
    rng: DeterministicRng,
    fields: Vec<String>,
    population_limit: usize,
    max_depth: usize,
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
            population_limit,
            max_depth,
            population: Vec::new(),
            seen: BTreeSet::new(),
        })
    }

    fn build_candidate(&mut self) -> Result<FactorAst, String> {
        let base = if self.population.is_empty() {
            self.field()
        } else {
            let tournament = (0..self.population.len().min(3))
                .map(|_| self.rng.index(self.population.len()))
                .max_by(|left, right| {
                    self.population[*left]
                        .1
                        .total_cmp(&self.population[*right].1)
                })
                .unwrap_or(0);
            let selected = self.population[tournament].0.clone();
            if ast_depth(&selected) >= self.max_depth {
                self.field()
            } else {
                selected
            }
        };
        let candidate = match self.rng.index(6) {
            0 => FactorAst::call(FactorOperator::Rank, vec![base]),
            1 => FactorAst::call(FactorOperator::Delta, vec![base, self.window_constant()]),
            2 => FactorAst::call(FactorOperator::Mean, vec![base, self.window_constant()]),
            3 => FactorAst::call(FactorOperator::Add, vec![base, self.field()]),
            4 => FactorAst::call(FactorOperator::Sub, vec![base, self.field()]),
            _ => FactorAst::call(FactorOperator::Mul, vec![base, self.field()]),
        }
        .map_err(|error| error.to_string())?;
        candidate.validate().map_err(|error| error.to_string())?;
        Ok(candidate)
    }

    fn field(&mut self) -> FactorAst {
        FactorAst::Terminal(FactorTerminal::Field(
            self.fields[self.rng.index(self.fields.len())].clone(),
        ))
    }

    fn window_constant(&mut self) -> FactorAst {
        const WINDOWS: [&str; 5] = ["5", "10", "20", "40", "60"];
        FactorAst::Terminal(FactorTerminal::Constant(
            WINDOWS[self.rng.index(WINDOWS.len())].to_string(),
        ))
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
        _context: &EngineContext<'_>,
        remaining: &RemainingBudget,
    ) -> Result<EngineProposal, String> {
        if remaining.expansions == 0 {
            return Err("GP expansion budget is exhausted".to_string());
        }
        let max_attempts = remaining.expansions.min(32) as usize;
        for attempt in 1..=max_attempts {
            let ast = self.build_candidate()?;
            if self.seen.insert(ast.to_string()) {
                return Ok(EngineProposal {
                    candidate_id: format!("{mission_id}-gp-{iteration_index}"),
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
        self.population.push((ast.clone(), evaluation.score));
        self.population
            .sort_by(|left, right| right.1.total_cmp(&left.1));
        self.population.truncate(self.population_limit);
    }

    fn restore(&mut self, observations: &[HistoricalObservation]) -> Result<(), String> {
        for observation in observations {
            let CandidateArtifact::Formula(ast) = &observation.proposal.artifact else {
                return Err("GP history contains a non-formula artifact".to_string());
            };
            self.seen.insert(ast.to_string());
            self.population
                .push((ast.clone(), observation.evaluation.score));
        }
        self.population
            .sort_by(|left, right| right.1.total_cmp(&left.1));
        self.population.truncate(self.population_limit);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
