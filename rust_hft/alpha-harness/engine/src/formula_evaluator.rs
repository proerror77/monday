use crate::{
    evaluation::{evaluate_sealed_holdout, EngineContext, PreparedDataset, ResearchRow},
    CandidateEvaluation, CandidateEvaluator, EngineProposal, EvaluationMetrics,
    FoldEvaluationMetrics,
};
use alpha_domain::{CandidateArtifact, SEALED_HOLDOUT_EVALUATOR_VERSION};
pub use alpha_domain::{
    FormulaEvaluatorConfig, MultipleTestingAdjustment, WALK_FORWARD_EVALUATOR_VERSION,
};
use hft_factor_dsl::{FactorAst, FactorOperator, FactorTerminal};

const BPS: f64 = 10_000.0;

pub struct FormulaEvaluator {
    config: FormulaEvaluatorConfig,
}

impl FormulaEvaluator {
    pub fn for_trials(multiple_testing_trials: usize) -> Result<Self, String> {
        Self::new(
            FormulaEvaluatorConfig::for_trials(multiple_testing_trials)
                .map_err(|error| error.to_string())?,
        )
    }

    pub fn for_mission(mission: &alpha_domain::ResearchMission) -> Result<Self, String> {
        Self::new(FormulaEvaluatorConfig::for_mission(mission).map_err(|error| error.to_string())?)
    }

    pub fn new(config: FormulaEvaluatorConfig) -> Result<Self, String> {
        config.validate().map_err(|error| error.to_string())?;
        Ok(Self { config })
    }

    pub fn config_evidence(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(&self.config)
            .map_err(|error| format!("failed to serialize evaluator config: {error}"))
    }

    pub fn evaluate_onnx_signals(
        &self,
        rows: &[ResearchRow],
        signals: &[f64],
        ranges: impl IntoIterator<Item = std::ops::Range<usize>>,
        sealed: bool,
    ) -> Result<CandidateEvaluation, String> {
        self.evaluate_ranges(
            rows,
            signals,
            ranges,
            if sealed {
                alpha_domain::ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION
            } else {
                alpha_domain::ONNX_WALK_FORWARD_EVALUATOR_VERSION
            },
        )
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
                SEALED_HOLDOUT_EVALUATOR_VERSION,
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

        let mut fold_metrics = Vec::new();
        let mut all_returns = Vec::new();
        let mut failures = Vec::new();
        for (fold_index, range) in ranges.into_iter().enumerate() {
            if range.len() < self.config.min_validation_rows || range.end > rows.len() {
                return Err("evaluation range is too short or out of bounds".to_string());
            }
            let (returns, trade_count) = net_returns(rows, signals, range.clone());
            let mean = mean(&returns);
            let raw_score = t_statistic(&returns, mean);
            let max_drawdown = max_drawdown(&returns);
            if trade_count < self.config.min_trades {
                failures.push(format!(
                    "fold {} trades {} are below {}",
                    fold_index + 1,
                    trade_count,
                    self.config.min_trades
                ));
            }
            if mean <= self.config.min_fold_mean_return {
                failures.push(format!(
                    "fold {} does not establish positive net edge: mean {:.8} must exceed {:.8}",
                    fold_index + 1,
                    mean,
                    self.config.min_fold_mean_return
                ));
            }
            if max_drawdown > self.config.max_drawdown {
                failures.push(format!(
                    "fold {} drawdown {:.8} exceeds {:.8}",
                    fold_index + 1,
                    max_drawdown,
                    self.config.max_drawdown
                ));
            }
            fold_metrics.push(FoldEvaluationMetrics {
                fold_index: fold_index + 1,
                row_count: returns.len(),
                trade_count,
                mean_net_return: mean,
                cumulative_net_return: returns.iter().sum(),
                max_drawdown,
                raw_score,
            });
            all_returns.extend(returns);
        }
        if fold_metrics.is_empty() {
            return Err("evaluation has no folds".to_string());
        }
        let raw_score = mean(
            &fold_metrics
                .iter()
                .map(|fold| fold.raw_score)
                .collect::<Vec<_>>(),
        );
        let adjusted_score = self
            .config
            .adjusted_score(raw_score)
            .map_err(|error| error.to_string())?;
        if adjusted_score < self.config.min_aggregate_score {
            failures.push(format!(
                "multiple-testing-adjusted score {:.8} is below {:.8}",
                adjusted_score, self.config.min_aggregate_score
            ));
        }
        let metrics = EvaluationMetrics {
            row_count: all_returns.len(),
            trade_count: fold_metrics.iter().map(|fold| fold.trade_count).sum(),
            mean_net_return: mean(&all_returns),
            cumulative_net_return: all_returns.iter().sum(),
            max_drawdown: fold_metrics
                .iter()
                .map(|fold| fold.max_drawdown)
                .fold(0.0, f64::max),
            raw_score,
            adjusted_score,
            folds: fold_metrics,
        };
        let evaluation = CandidateEvaluation {
            passed: failures.is_empty(),
            score: adjusted_score,
            failure_reasons: failures,
            evaluator_version: evaluator_version.to_string(),
            evaluator_config: self.config_evidence()?,
            metrics,
        };
        evaluation.validate().map_err(|error| error.to_string())?;
        Ok(evaluation)
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
            WALK_FORWARD_EVALUATOR_VERSION,
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
        FactorAst::Terminal(FactorTerminal::Field(field)) => rows
            .iter()
            .map(|row| {
                row.features
                    .get(field)
                    .copied()
                    .ok_or_else(|| format!("formula field is not registered: {field}"))
            })
            .collect(),
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

fn net_returns(
    rows: &[ResearchRow],
    signals: &[f64],
    range: std::ops::Range<usize>,
) -> (Vec<f64>, usize) {
    let mut previous_position = 0.0;
    let mut trade_count = 0;
    let returns = range
        .map(|index| {
            let position = signal_position(signals[index]);
            let turnover = (position - previous_position).abs();
            if turnover > f64::EPSILON {
                trade_count += 1;
            }
            previous_position = position;
            let row = &rows[index];
            let transaction_cost =
                (row.fee_bps.max(0.0) + row.latency_bps.max(0.0)) * turnover / BPS;
            let funding_cost = row.funding_bps.max(0.0) * position.abs() / BPS;
            position * row.label - transaction_cost - funding_cost
        })
        .collect();
    (returns, trade_count)
}

fn signal_position(signal: f64) -> f64 {
    if signal.abs() <= f64::EPSILON {
        0.0
    } else {
        signal.signum()
    }
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
        if average.abs() <= f64::EPSILON {
            0.0
        } else {
            // Keep deterministic returns finite instead of persisting an infinite t-statistic.
            average.signum() * (values.len() as f64).sqrt()
        }
    } else {
        average / (deviation / (values.len() as f64).sqrt())
    }
}

fn max_drawdown(returns: &[f64]) -> f64 {
    let mut equity = 1.0_f64;
    let mut peak = 1.0_f64;
    let mut maximum = 0.0_f64;
    for value in returns {
        equity += value;
        peak = peak.max(equity);
        if peak > f64::EPSILON {
            maximum = maximum.max((peak - equity) / peak);
        }
    }
    maximum
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
        (0..500)
            .map(|index| ResearchRow {
                available_time: start + Duration::minutes(index as i64),
                signal: if index % 2 == 0 { 1.0 } else { -1.0 },
                features: std::collections::BTreeMap::new(),
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
                initial_train_rows: 200,
                validation_rows: 64,
                fold_count: 3,
                purge_rows: 1,
                embargo_rows: 1,
                sealed_holdout_rows: 64,
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
    fn registered_point_in_time_feature_can_drive_a_formula() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "lob_imbalance".to_string(),
        )));
        let mut rows = rows(0.0);
        for row in &mut rows {
            row.features.insert("lob_imbalance".to_string(), row.signal);
        }
        let dataset = prepare_dataset(
            rows,
            &WalkForwardConfig {
                initial_train_rows: 200,
                validation_rows: 64,
                fold_count: 3,
                purge_rows: 1,
                embargo_rows: 1,
                sealed_holdout_rows: 64,
            },
            "sealed-1",
        )
        .unwrap();

        assert!(
            evaluator
                .evaluate(&proposal, &dataset.engine_context())
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
    fn derived_return_overflow_is_rejected_before_evidence_is_emitted() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "signal".to_string(),
        )));

        assert!(evaluator
            .evaluate(&proposal, &dataset(f64::MAX).engine_context())
            .is_err());
    }

    #[test]
    fn zero_signal_cannot_pass_without_trades_or_positive_edge() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Constant(
            "0".to_string(),
        )));
        let result = evaluator.evaluate_sealed(&proposal, &dataset(0.0)).unwrap();

        assert!(!result.passed);
        assert_eq!(result.metrics.trade_count, 0);
        assert_eq!(result.metrics.mean_net_return, 0.0);
        assert!(result
            .failure_reasons
            .iter()
            .any(|reason| reason.contains("trades")));
        assert!(result
            .failure_reasons
            .iter()
            .any(|reason| reason.contains("positive net edge")));
    }

    #[test]
    fn sealed_evidence_persists_metrics_config_and_multiple_testing_haircut() {
        let config = FormulaEvaluatorConfig {
            multiple_testing_trials: 100,
            ..FormulaEvaluatorConfig::default()
        };
        let evaluator = FormulaEvaluator::new(config.clone()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "signal".to_string(),
        )));
        let result = evaluator.evaluate_sealed(&proposal, &dataset(0.0)).unwrap();

        result.validate().unwrap();
        assert!(result.metrics.row_count >= config.min_validation_rows);
        assert!(result.metrics.trade_count >= config.min_trades);
        assert!(result.metrics.raw_score > result.metrics.adjusted_score);
        assert_eq!(
            result.evaluator_config["multiple_testing_trials"],
            serde_json::json!(100)
        );

        let mut tampered = result;
        tampered.evaluator_config = serde_json::Value::Null;
        assert!(tampered.validate().is_err());
    }

    #[test]
    fn drawdown_above_the_governed_bound_cannot_pass() {
        let mut input = rows(0.0);
        let holdout_loss = input.len() - 32;
        input[holdout_loss].label = -0.5 * input[holdout_loss].signal.signum();
        let dataset = prepare_dataset(
            input,
            &WalkForwardConfig {
                initial_train_rows: 200,
                validation_rows: 64,
                fold_count: 3,
                purge_rows: 1,
                embargo_rows: 1,
                sealed_holdout_rows: 64,
            },
            "sealed-drawdown",
        )
        .unwrap();
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "signal".to_string(),
        )));
        let result = evaluator.evaluate_sealed(&proposal, &dataset).unwrap();

        assert!(!result.passed);
        assert!(result.metrics.max_drawdown > 0.2);
        assert!(result
            .failure_reasons
            .iter()
            .any(|reason| reason.contains("drawdown")));
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
