use crate::{
    evaluation::{evaluate_sealed_holdout, EngineContext, PreparedDataset, ResearchRow},
    CandidateEvaluation, CandidateEvaluator, EngineProposal,
};
use alpha_domain::CandidateArtifact;
use hft_factor_dsl::{FactorAst, FactorOperator, FactorTerminal};

const BPS: f64 = 10_000.0;

#[derive(Debug, Clone, PartialEq)]
pub struct FormulaEvaluatorConfig {
    pub min_validation_rows: usize,
    pub min_fold_mean_return: f64,
    pub min_aggregate_score: f64,
    pub max_abs_signal: f64,
}

impl Default for FormulaEvaluatorConfig {
    fn default() -> Self {
        Self {
            min_validation_rows: 5,
            min_fold_mean_return: 0.0,
            min_aggregate_score: 0.0,
            max_abs_signal: 1.0e12,
        }
    }
}

pub struct FormulaEvaluator {
    config: FormulaEvaluatorConfig,
}

impl FormulaEvaluator {
    pub fn new(config: FormulaEvaluatorConfig) -> Result<Self, String> {
        if config.min_validation_rows < 2
            || !config.min_fold_mean_return.is_finite()
            || !config.min_aggregate_score.is_finite()
            || !config.max_abs_signal.is_finite()
            || config.max_abs_signal <= 0.0
        {
            return Err("invalid formula evaluator configuration".to_string());
        }
        Ok(Self { config })
    }

    pub fn evaluate_sealed(
        &self,
        proposal: &EngineProposal,
        dataset: &PreparedDataset,
    ) -> Result<CandidateEvaluation, String> {
        let ast = formula(proposal)?;
        evaluate_sealed_holdout(dataset, |rows| {
            let signals = evaluate_ast(ast, rows)?;
            self.evaluate_ranges(
                rows,
                &signals,
                std::iter::once(0..rows.len()),
                "sealed-holdout-v1",
            )
        })
    }

    fn evaluate_ranges(
        &self,
        rows: &[ResearchRow],
        signals: &[f64],
        ranges: impl IntoIterator<Item = std::ops::Range<usize>>,
        evaluator_version: &str,
    ) -> Result<CandidateEvaluation, String> {
        if signals.len() != rows.len() {
            return Err("formula output length does not match dataset".to_string());
        }
        if signals
            .iter()
            .any(|value| !value.is_finite() || value.abs() > self.config.max_abs_signal)
        {
            return Err("formula produced an invalid or unbounded signal".to_string());
        }

        let mut scores = Vec::new();
        let mut failures = Vec::new();
        for (fold_index, range) in ranges.into_iter().enumerate() {
            if range.len() < self.config.min_validation_rows || range.end > rows.len() {
                return Err("evaluation range is too short or out of bounds".to_string());
            }
            let returns = net_returns(rows, signals, range.clone());
            let mean = mean(&returns);
            let score = t_statistic(&returns, mean);
            if mean < self.config.min_fold_mean_return {
                failures.push(format!(
                    "fold {} mean net return {:.8} is below {:.8}",
                    fold_index + 1,
                    mean,
                    self.config.min_fold_mean_return
                ));
            }
            scores.push(score);
        }
        if scores.is_empty() {
            return Err("evaluation has no folds".to_string());
        }
        let aggregate = mean(&scores);
        if aggregate < self.config.min_aggregate_score {
            failures.push(format!(
                "aggregate score {:.8} is below {:.8}",
                aggregate, self.config.min_aggregate_score
            ));
        }
        Ok(CandidateEvaluation {
            passed: failures.is_empty(),
            score: aggregate,
            failure_reasons: failures,
            evaluator_version: evaluator_version.to_string(),
        })
    }
}

impl CandidateEvaluator for FormulaEvaluator {
    fn evaluate(
        &self,
        proposal: &EngineProposal,
        context: &EngineContext<'_>,
    ) -> Result<CandidateEvaluation, String> {
        let ast = formula(proposal)?;
        let signals = evaluate_ast(ast, context.rows())?;
        self.evaluate_ranges(
            context.rows(),
            &signals,
            context.folds().iter().map(|fold| fold.validation.clone()),
            "purged-walk-forward-v1",
        )
    }
}

fn formula(proposal: &EngineProposal) -> Result<&FactorAst, String> {
    match &proposal.artifact {
        CandidateArtifact::Formula(ast) => {
            ast.validate().map_err(|error| error.to_string())?;
            Ok(ast)
        }
        _ => Err("formula evaluator only accepts DSL formula artifacts".to_string()),
    }
}

fn evaluate_ast(ast: &FactorAst, rows: &[ResearchRow]) -> Result<Vec<f64>, String> {
    match ast {
        FactorAst::Terminal(FactorTerminal::Field(field)) if field == "signal" => {
            Ok(rows.iter().map(|row| row.signal).collect())
        }
        FactorAst::Terminal(FactorTerminal::Field(field)) => {
            Err(format!("formula field is not registered: {field}"))
        }
        FactorAst::Terminal(FactorTerminal::Constant(value)) => {
            let value = value
                .parse::<f64>()
                .map_err(|_| "formula constant is not numeric".to_string())?;
            if !value.is_finite() {
                return Err("formula constant is not finite".to_string());
            }
            Ok(vec![value; rows.len()])
        }
        FactorAst::Call { operator, args } => {
            operator
                .arity()
                .eq(&args.len())
                .then_some(())
                .ok_or_else(|| format!("operator {} has invalid arity", operator.symbol()))?;
            match operator {
                FactorOperator::Abs => unary(&args[0], rows, f64::abs),
                FactorOperator::Log => {
                    unary(&args[0], rows, |value| value.signum() * value.abs().ln_1p())
                }
                FactorOperator::Rank => expanding_rank(&evaluate_ast(&args[0], rows)?),
                FactorOperator::Delta => {
                    rolling_binary(&args[0], &args[1], rows, RollingOperation::Delta)
                }
                FactorOperator::Mean => {
                    rolling_binary(&args[0], &args[1], rows, RollingOperation::Mean)
                }
                FactorOperator::Std => {
                    rolling_binary(&args[0], &args[1], rows, RollingOperation::Std)
                }
                FactorOperator::ZScore => {
                    rolling_binary(&args[0], &args[1], rows, RollingOperation::ZScore)
                }
                FactorOperator::IfElse => {
                    let condition = evaluate_ast(&args[0], rows)?;
                    let truthy = evaluate_ast(&args[1], rows)?;
                    let falsy = evaluate_ast(&args[2], rows)?;
                    Ok(condition
                        .into_iter()
                        .zip(truthy)
                        .zip(falsy)
                        .map(
                            |((condition, truthy), falsy)| {
                                if condition > 0.0 {
                                    truthy
                                } else {
                                    falsy
                                }
                            },
                        )
                        .collect())
                }
                binary_operator => {
                    let left = evaluate_ast(&args[0], rows)?;
                    let right = evaluate_ast(&args[1], rows)?;
                    Ok(left
                        .into_iter()
                        .zip(right)
                        .map(|(left, right)| match binary_operator {
                            FactorOperator::Add => left + right,
                            FactorOperator::Sub => left - right,
                            FactorOperator::Mul => left * right,
                            FactorOperator::Div => {
                                if right.abs() <= f64::EPSILON {
                                    0.0
                                } else {
                                    left / right
                                }
                            }
                            FactorOperator::GreaterThan => (left > right) as u8 as f64,
                            FactorOperator::LessThan => (left < right) as u8 as f64,
                            _ => unreachable!("remaining operators are binary scalar operations"),
                        })
                        .collect())
                }
            }
        }
    }
}

fn unary(
    ast: &FactorAst,
    rows: &[ResearchRow],
    operation: impl Fn(f64) -> f64,
) -> Result<Vec<f64>, String> {
    Ok(evaluate_ast(ast, rows)?
        .into_iter()
        .map(operation)
        .collect())
}

enum RollingOperation {
    Delta,
    Mean,
    Std,
    ZScore,
}

fn rolling_binary(
    values: &FactorAst,
    window: &FactorAst,
    rows: &[ResearchRow],
    operation: RollingOperation,
) -> Result<Vec<f64>, String> {
    let values = evaluate_ast(values, rows)?;
    let window = parse_window(window)?;
    let mut output = Vec::with_capacity(values.len());
    for index in 0..values.len() {
        let start = (index + 1).saturating_sub(window);
        let history = &values[start..=index];
        let value = match operation {
            RollingOperation::Delta => values[index] - values[index.saturating_sub(window)],
            RollingOperation::Mean => mean(history),
            RollingOperation::Std => standard_deviation(history, mean(history)),
            RollingOperation::ZScore => {
                let average = mean(history);
                let deviation = standard_deviation(history, average);
                if deviation <= f64::EPSILON {
                    0.0
                } else {
                    (values[index] - average) / deviation
                }
            }
        };
        output.push(value);
    }
    Ok(output)
}

fn parse_window(ast: &FactorAst) -> Result<usize, String> {
    let FactorAst::Terminal(FactorTerminal::Constant(value)) = ast else {
        return Err("rolling window must be a constant".to_string());
    };
    let parsed = value
        .parse::<usize>()
        .map_err(|_| "rolling window must be a positive integer".to_string())?;
    (1..=10_000)
        .contains(&parsed)
        .then_some(parsed)
        .ok_or_else(|| "rolling window is out of bounds".to_string())
}

fn expanding_rank(values: &[f64]) -> Result<Vec<f64>, String> {
    if values.iter().any(|value| !value.is_finite()) {
        return Err("rank input is not finite".to_string());
    }
    Ok(values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let less_or_equal = values[..=index]
                .iter()
                .filter(|historical| *historical <= value)
                .count();
            less_or_equal as f64 / (index + 1) as f64
        })
        .collect())
}

fn net_returns(rows: &[ResearchRow], signals: &[f64], range: std::ops::Range<usize>) -> Vec<f64> {
    let mut previous_position = if range.start == 0 {
        0.0
    } else {
        signals[range.start - 1].signum()
    };
    range
        .map(|index| {
            let position = signals[index].signum();
            let turnover = (position - previous_position).abs();
            previous_position = position;
            let row = &rows[index];
            let transaction_cost =
                (row.fee_bps.max(0.0) + row.latency_bps.max(0.0)) * turnover / BPS;
            let funding_cost = row.funding_bps.max(0.0) * position.abs() / BPS;
            position * row.label - transaction_cost - funding_cost
        })
        .collect()
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn standard_deviation(values: &[f64], average: f64) -> f64 {
    (values
        .iter()
        .map(|value| (value - average).powi(2))
        .sum::<f64>()
        / values.len() as f64)
        .sqrt()
}

fn t_statistic(values: &[f64], average: f64) -> f64 {
    let deviation = standard_deviation(values, average);
    if deviation <= f64::EPSILON {
        0.0
    } else {
        average / (deviation / (values.len() as f64).sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::{prepare_dataset, ResearchRow, WalkForwardConfig};
    use chrono::{Duration, Utc};

    fn proposal(ast: FactorAst) -> EngineProposal {
        EngineProposal {
            candidate_id: "candidate-1".to_string(),
            hypothesis: "test".to_string(),
            artifact: CandidateArtifact::Formula(ast),
            expansions: 1,
            tokens: 0,
            elapsed_ms: 0,
        }
    }

    fn rows(fee_bps: f64) -> Vec<ResearchRow> {
        let start = Utc::now();
        (0..30)
            .map(|index| ResearchRow {
                available_time: start + Duration::minutes(index as i64),
                signal: if index % 2 == 0 { 1.0 } else { -1.0 },
                label: if index % 2 == 0 { 0.01 } else { -0.01 },
                fee_bps,
                funding_bps: 0.0,
                latency_bps: 0.0,
            })
            .collect()
    }

    fn dataset(fee_bps: f64) -> PreparedDataset {
        prepare_dataset(
            rows(fee_bps),
            &WalkForwardConfig {
                initial_train_rows: 8,
                validation_rows: 5,
                fold_count: 2,
                purge_rows: 1,
                embargo_rows: 1,
                sealed_holdout_rows: 8,
            },
            "sealed-1",
        )
        .unwrap()
    }

    #[test]
    fn causal_signal_formula_passes_walk_forward_and_holdout() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "signal".to_string(),
        )));
        let dataset = dataset(0.0);
        assert!(
            evaluator
                .evaluate(&proposal, &dataset.engine_context())
                .unwrap()
                .passed
        );
        assert!(
            evaluator
                .evaluate_sealed(&proposal, &dataset)
                .unwrap()
                .passed
        );
    }

    #[test]
    fn transaction_cost_can_reject_a_high_turnover_formula() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "signal".to_string(),
        )));
        let result = evaluator
            .evaluate(&proposal, &dataset(100.0).engine_context())
            .unwrap();
        assert!(!result.passed);
        assert!(!result.failure_reasons.is_empty());
    }

    #[test]
    fn rolling_window_must_be_a_bounded_integer_constant() {
        let ast = FactorAst::call(
            FactorOperator::Mean,
            vec![
                FactorAst::Terminal(FactorTerminal::Field("signal".to_string())),
                FactorAst::Terminal(FactorTerminal::Constant("0".to_string())),
            ],
        )
        .unwrap();
        assert!(evaluate_ast(&ast, &rows(0.0)).is_err());
    }

    #[test]
    fn evaluator_rejects_non_formula_artifacts() {
        let mut proposal = proposal(FactorAst::Terminal(FactorTerminal::Constant(
            "1".to_string(),
        )));
        proposal.artifact = CandidateArtifact::ModelConfig(serde_json::json!({}));
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        assert!(evaluator
            .evaluate(&proposal, &dataset(0.0).engine_context())
            .is_err());
    }
}
