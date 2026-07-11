use crate::{
    evaluation::EngineContext, CandidateEvaluation, EngineProposal, HistoricalObservation,
    ProposalEngine, ProposalEngineCheckpoint, RemainingBudget,
};
use alpha_domain::{CandidateArtifact, EngineKind};
use hft_factor_dsl::{FactorAst, FactorOperator, FactorTerminal};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const BAYESIAN_CHECKPOINT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BayesianConfigV1 {
    field: String,
    grid: Vec<f64>,
    noise: f64,
    length_scale: f64,
    exploration: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BayesianCheckpointV1 {
    config: BayesianConfigV1,
    observations: Vec<(f64, f64)>,
    pending: BTreeMap<String, f64>,
}

pub struct BayesianOptimizerEngine {
    field: String,
    grid: Vec<f64>,
    noise: f64,
    length_scale: f64,
    exploration: f64,
    observations: Vec<(f64, f64)>,
    pending: BTreeMap<String, f64>,
}

impl BayesianOptimizerEngine {
    pub fn new(
        field: impl Into<String>,
        min: f64,
        max: f64,
        points: usize,
        noise: f64,
        length_scale: f64,
        exploration: f64,
    ) -> Result<Self, String> {
        let field = field.into();
        if field.trim().is_empty()
            || !min.is_finite()
            || !max.is_finite()
            || min >= max
            || points < 3
            || !noise.is_finite()
            || noise <= 0.0
            || !length_scale.is_finite()
            || length_scale <= 0.0
            || !exploration.is_finite()
            || exploration < 0.0
        {
            return Err("invalid Bayesian optimizer configuration".to_string());
        }
        let step = (max - min) / (points - 1) as f64;
        let grid = (0..points)
            .map(|index| (min + index as f64 * step).round() as u64)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|window| window.max(1) as f64)
            .collect::<Vec<_>>();
        if grid.len() < 3 {
            return Err("Bayesian window grid has fewer than three unique points".to_string());
        }
        Ok(Self {
            field,
            grid,
            noise,
            length_scale,
            exploration,
            observations: vec![],
            pending: BTreeMap::new(),
        })
    }

    fn config(&self) -> BayesianConfigV1 {
        BayesianConfigV1 {
            field: self.field.clone(),
            grid: self.grid.clone(),
            noise: self.noise,
            length_scale: self.length_scale,
            exploration: self.exploration,
        }
    }

    fn choose_point(&self) -> Result<f64, String> {
        let observed = self
            .observations
            .iter()
            .map(|(x, _)| x.to_bits())
            .collect::<BTreeSet<_>>();
        let pending = self
            .pending
            .values()
            .map(|x| x.to_bits())
            .collect::<BTreeSet<_>>();
        let available = self
            .grid
            .iter()
            .copied()
            .filter(|x| !observed.contains(&x.to_bits()) && !pending.contains(&x.to_bits()))
            .collect::<Vec<_>>();
        if available.is_empty() {
            return Err("Bayesian parameter grid is exhausted".to_string());
        }
        if self.observations.len() < 3 {
            let seeds = [
                self.grid[0],
                self.grid[self.grid.len() - 1],
                self.grid[self.grid.len() / 2],
            ];
            return seeds
                .into_iter()
                .find(|x| available.iter().any(|candidate| candidate == x))
                .or_else(|| available.first().copied())
                .ok_or_else(|| "Bayesian parameter grid is exhausted".to_string());
        }
        let best = self
            .observations
            .iter()
            .map(|(_, score)| *score)
            .max_by(f64::total_cmp)
            .unwrap_or(0.0);
        available
            .into_iter()
            .map(|x| {
                self.expected_improvement(x, best)
                    .map(|acquisition| (x, acquisition))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(x, _)| x)
            .ok_or_else(|| "Bayesian acquisition failed".to_string())
    }

    fn expected_improvement(&self, x: f64, best: f64) -> Result<f64, String> {
        let size = self.observations.len();
        let mut matrix = vec![vec![0.0; size]; size];
        let targets = self
            .observations
            .iter()
            .map(|(_, score)| *score)
            .collect::<Vec<_>>();
        for (row, values) in matrix.iter_mut().enumerate() {
            for (column, value) in values.iter_mut().enumerate() {
                *value = self.kernel(self.observations[row].0, self.observations[column].0);
            }
            values[row] += self.noise;
        }
        let k = self
            .observations
            .iter()
            .map(|(observed_x, _)| self.kernel(x, *observed_x))
            .collect::<Vec<_>>();
        let alpha = solve(matrix.clone(), targets)?;
        let inverse_k = solve(matrix, k.clone())?;
        let mean = dot(&k, &alpha);
        let variance = (self.kernel(x, x) - dot(&k, &inverse_k)).max(1e-12);
        let sigma = variance.sqrt();
        let improvement = mean - best - self.exploration;
        let z = improvement / sigma;
        Ok((improvement * normal_cdf(z) + sigma * normal_pdf(z)).max(0.0))
    }

    fn kernel(&self, left: f64, right: f64) -> f64 {
        let distance = (left - right) / self.length_scale;
        (-0.5 * distance * distance).exp()
    }
}

impl ProposalEngine for BayesianOptimizerEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::BayesianOptimizer
    }

    fn propose(
        &mut self,
        mission_id: &str,
        iteration_index: usize,
        _context: &EngineContext<'_>,
        _remaining: &RemainingBudget,
    ) -> Result<EngineProposal, String> {
        let point = self.choose_point()?;
        let window = point.round().max(1.0) as u64;
        let ast = FactorAst::call(
            FactorOperator::Mean,
            vec![
                FactorAst::Terminal(FactorTerminal::Field(self.field.clone())),
                FactorAst::Terminal(FactorTerminal::Constant(window.to_string())),
            ],
        )
        .map_err(|error| error.to_string())?;
        let candidate_id = format!("{mission_id}-bayes-{iteration_index}");
        self.pending.insert(candidate_id.clone(), point);
        Ok(EngineProposal {
            candidate_id,
            hypothesis: format!("Bayesian expected improvement selected window {window}"),
            artifact: CandidateArtifact::Formula(ast),
            expansions: 1,
            tokens: 0,
            elapsed_ms: 0,
        })
    }

    fn observe(&mut self, proposal: &EngineProposal, evaluation: &CandidateEvaluation) {
        if let Some(point) = self.pending.remove(&proposal.candidate_id) {
            if evaluation.score.is_finite() {
                self.observations.push((point, evaluation.score));
            }
        }
    }

    fn abandon(&mut self, proposal: &EngineProposal) {
        self.pending.remove(&proposal.candidate_id);
    }

    fn restore(&mut self, observations: &[HistoricalObservation]) -> Result<(), String> {
        for observation in observations {
            let CandidateArtifact::Formula(FactorAst::Call {
                operator: FactorOperator::Mean,
                args,
            }) = &observation.proposal.artifact
            else {
                return Err("Bayesian history contains an unexpected artifact".to_string());
            };
            let Some(FactorAst::Terminal(FactorTerminal::Constant(window))) = args.get(1) else {
                return Err("Bayesian history has no window parameter".to_string());
            };
            let point = window
                .parse::<f64>()
                .map_err(|_| "Bayesian history window is invalid".to_string())?;
            if !observation.evaluation.score.is_finite() {
                return Err("Bayesian history contains a non-finite score".to_string());
            }
            self.observations
                .push((point, observation.evaluation.score));
        }
        Ok(())
    }

    fn checkpoint(&self) -> Result<ProposalEngineCheckpoint, String> {
        let state = BayesianCheckpointV1 {
            config: self.config(),
            observations: self.observations.clone(),
            pending: self.pending.clone(),
        };
        state.validate()?;
        Ok(ProposalEngineCheckpoint {
            kind: EngineKind::BayesianOptimizer,
            version: BAYESIAN_CHECKPOINT_VERSION,
            state: serde_json::to_value(state)
                .map_err(|error| format!("failed to encode Bayesian checkpoint: {error}"))?,
        })
    }

    fn restore_checkpoint(
        &mut self,
        checkpoint: &ProposalEngineCheckpoint,
        _observations: &[HistoricalObservation],
    ) -> Result<(), String> {
        if checkpoint.kind != EngineKind::BayesianOptimizer
            || checkpoint.version != BAYESIAN_CHECKPOINT_VERSION
        {
            return Err("Bayesian checkpoint kind or version mismatch".to_string());
        }
        let state: BayesianCheckpointV1 = serde_json::from_value(checkpoint.state.clone())
            .map_err(|error| format!("invalid Bayesian checkpoint state: {error}"))?;
        state.validate()?;
        if state.config != self.config() {
            return Err("Bayesian checkpoint configuration mismatch".to_string());
        }

        let BayesianCheckpointV1 {
            config,
            observations,
            pending,
        } = state;
        self.field = config.field;
        self.grid = config.grid;
        self.noise = config.noise;
        self.length_scale = config.length_scale;
        self.exploration = config.exploration;
        self.observations = observations;
        self.pending = pending;
        Ok(())
    }
}

impl BayesianCheckpointV1 {
    fn validate(&self) -> Result<(), String> {
        if self.config.field.trim().is_empty()
            || self.config.grid.len() < 3
            || self
                .config
                .grid
                .iter()
                .any(|point| !point.is_finite() || *point <= 0.0)
            || !self.config.noise.is_finite()
            || self.config.noise <= 0.0
            || !self.config.length_scale.is_finite()
            || self.config.length_scale <= 0.0
            || !self.config.exploration.is_finite()
            || self.config.exploration < 0.0
        {
            return Err("invalid Bayesian checkpoint configuration".to_string());
        }

        let grid = self
            .config
            .grid
            .iter()
            .map(|point| point.to_bits())
            .collect::<BTreeSet<_>>();
        for (point, score) in &self.observations {
            if !point.is_finite() || !score.is_finite() || !grid.contains(&point.to_bits()) {
                return Err("invalid Bayesian checkpoint observations".to_string());
            }
        }
        for (candidate_id, point) in &self.pending {
            if candidate_id.is_empty() || !point.is_finite() || !grid.contains(&point.to_bits()) {
                return Err("invalid Bayesian checkpoint pending candidates".to_string());
            }
        }
        Ok(())
    }
}

fn solve(mut matrix: Vec<Vec<f64>>, mut values: Vec<f64>) -> Result<Vec<f64>, String> {
    let size = values.len();
    for pivot in 0..size {
        let best = (pivot..size)
            .max_by(|left, right| {
                matrix[*left][pivot]
                    .abs()
                    .total_cmp(&matrix[*right][pivot].abs())
            })
            .ok_or_else(|| "empty Gaussian process matrix".to_string())?;
        matrix.swap(pivot, best);
        values.swap(pivot, best);
        let divisor = matrix[pivot][pivot];
        if divisor.abs() < 1e-12 {
            return Err("singular Gaussian process matrix".to_string());
        }
        for value in matrix[pivot].iter_mut().skip(pivot) {
            *value /= divisor;
        }
        values[pivot] /= divisor;
        let pivot_values = matrix[pivot].clone();
        for (row, row_values) in matrix.iter_mut().enumerate() {
            if row == pivot {
                continue;
            }
            let factor = row_values[pivot];
            for (column, value) in row_values.iter_mut().enumerate().skip(pivot) {
                *value -= factor * pivot_values[column];
            }
            values[row] -= factor * values[pivot];
        }
    }
    Ok(values)
}

fn dot(left: &[f64], right: &[f64]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn normal_pdf(value: f64) -> f64 {
    (-0.5 * value * value).exp() / (2.0 * std::f64::consts::PI).sqrt()
}

fn normal_cdf(value: f64) -> f64 {
    0.5 * (1.0 + erf(value / 2.0_f64.sqrt()))
}

fn erf(value: f64) -> f64 {
    let sign = value.signum();
    let x = value.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let polynomial =
        (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t;
    sign * (1.0 - polynomial * (-x * x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> RemainingBudget {
        RemainingBudget {
            candidates: 16,
            expansions: 16,
            tokens: 0,
            milliseconds: 0,
        }
    }

    fn evaluation(score: f64) -> CandidateEvaluation {
        CandidateEvaluation {
            passed: true,
            score,
            failure_reasons: vec![],
            evaluator_version: "test".to_string(),
            evaluator_config: serde_json::json!({"fixture": true}),
            metrics: crate::EvaluationMetrics {
                row_count: 1,
                trade_count: 1,
                mean_net_return: score,
                cumulative_net_return: score,
                max_drawdown: 0.0,
                raw_score: score,
                adjusted_score: score,
                folds: vec![],
            },
        }
    }

    fn advance(
        engine: &mut BayesianOptimizerEngine,
        iteration: usize,
        score: f64,
    ) -> EngineProposal {
        let dataset = super::super::test_dataset();
        let proposal = engine
            .propose("mission", iteration, &dataset.engine_context(), &budget())
            .unwrap();
        engine.observe(&proposal, &evaluation(score));
        proposal
    }

    #[test]
    fn gaussian_process_acquisition_is_finite() {
        let mut engine =
            BayesianOptimizerEngine::new("oi", 5.0, 60.0, 12, 1e-6, 10.0, 0.01).unwrap();
        engine.observations = vec![(5.0, 0.1), (60.0, 0.2), (30.0, 0.8)];
        let point = engine.choose_point().unwrap();
        assert!(point.is_finite());
        assert!(![5.0, 60.0, 30.0].contains(&point));
    }

    #[test]
    fn checkpoint_round_trip_restores_complete_search_state() {
        let mut engine =
            BayesianOptimizerEngine::new("oi", 5.0, 60.0, 12, 1e-6, 10.0, 0.01).unwrap();
        advance(&mut engine, 0, 0.2);
        advance(&mut engine, 1, 0.8);
        let dataset = super::super::test_dataset();
        let pending = engine
            .propose("mission", 2, &dataset.engine_context(), &budget())
            .unwrap();
        let checkpoint = engine.checkpoint().unwrap();

        let mut restored =
            BayesianOptimizerEngine::new("oi", 5.0, 60.0, 12, 1e-6, 10.0, 0.01).unwrap();
        restored.restore_checkpoint(&checkpoint, &[]).unwrap();

        assert_eq!(restored.checkpoint().unwrap(), checkpoint);
        assert_eq!(restored.field, engine.field);
        assert_eq!(restored.grid, engine.grid);
        assert_eq!(restored.noise, engine.noise);
        assert_eq!(restored.length_scale, engine.length_scale);
        assert_eq!(restored.exploration, engine.exploration);
        assert_eq!(restored.observations, engine.observations);
        assert_eq!(restored.pending, engine.pending);
        assert!(restored.pending.contains_key(&pending.candidate_id));
    }

    #[test]
    fn restored_search_continues_like_uninterrupted_search() {
        let mut uninterrupted =
            BayesianOptimizerEngine::new("oi", 5.0, 60.0, 12, 1e-6, 10.0, 0.01).unwrap();
        for (iteration, score) in [0.2, 0.8, 0.4, 0.6].into_iter().enumerate() {
            advance(&mut uninterrupted, iteration, score);
        }
        let checkpoint = uninterrupted.checkpoint().unwrap();
        let mut restored =
            BayesianOptimizerEngine::new("oi", 5.0, 60.0, 12, 1e-6, 10.0, 0.01).unwrap();
        restored.restore_checkpoint(&checkpoint, &[]).unwrap();
        let dataset = super::super::test_dataset();

        let expected = uninterrupted
            .propose("mission", 4, &dataset.engine_context(), &budget())
            .unwrap();
        let actual = restored
            .propose("mission", 4, &dataset.engine_context(), &budget())
            .unwrap();

        assert_eq!(actual, expected);
        assert_eq!(
            restored.checkpoint().unwrap(),
            uninterrupted.checkpoint().unwrap()
        );
    }

    #[test]
    fn restore_checkpoint_fails_closed_on_config_or_state_mismatch() {
        let engine = BayesianOptimizerEngine::new("oi", 5.0, 60.0, 12, 1e-6, 10.0, 0.01).unwrap();
        let checkpoint = engine.checkpoint().unwrap();
        let mut wrong_kind = checkpoint.clone();
        wrong_kind.kind = EngineKind::Mcts;
        let mut restored =
            BayesianOptimizerEngine::new("oi", 5.0, 60.0, 12, 1e-6, 10.0, 0.01).unwrap();
        assert!(restored.restore_checkpoint(&wrong_kind, &[]).is_err());

        let mut wrong_version = checkpoint.clone();
        wrong_version.version += 1;
        assert!(restored.restore_checkpoint(&wrong_version, &[]).is_err());

        let mut different_config =
            BayesianOptimizerEngine::new("price", 5.0, 60.0, 12, 1e-6, 10.0, 0.01).unwrap();
        assert!(different_config
            .restore_checkpoint(&checkpoint, &[])
            .is_err());

        let mut malformed = checkpoint;
        malformed.state = serde_json::json!({"unexpected": true});
        assert!(restored.restore_checkpoint(&malformed, &[]).is_err());
    }

    #[test]
    fn abandon_removes_pending_candidate() {
        let mut engine =
            BayesianOptimizerEngine::new("oi", 5.0, 60.0, 12, 1e-6, 10.0, 0.01).unwrap();
        let dataset = super::super::test_dataset();
        let proposal = engine
            .propose("mission", 0, &dataset.engine_context(), &budget())
            .unwrap();
        assert!(engine.pending.contains_key(&proposal.candidate_id));

        engine.abandon(&proposal);

        assert!(!engine.pending.contains_key(&proposal.candidate_id));
    }
}
