use crate::{
    evaluation::{
        contiguous_series_ranges, evaluate_sealed_holdout, EngineContext, PreparedDataset,
        ResearchRow,
    },
    CandidateEvaluation, CandidateEvaluator, EngineProposal, EvaluationMetrics,
    FoldEvaluationMetrics, FoldPredictiveMetrics, PredictiveMetrics,
};
use alpha_domain::{
    CandidateArtifact, EvaluationCostsV1, EvaluationProtocolV1,
    CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION, ONNX_WALK_FORWARD_EVALUATOR_VERSION,
    SEALED_HOLDOUT_EVALUATOR_VERSION,
};
pub use alpha_domain::{
    FormulaEvaluatorConfig, MultipleTestingAdjustment, WALK_FORWARD_EVALUATOR_VERSION,
};
use hft_factor_dsl::{
    evaluate_live_formula_series, validate_live_formula, FactorAst, FactorOperator, FactorTerminal,
};

const BPS: f64 = 10_000.0;

pub struct FormulaEvaluator {
    config: FormulaEvaluatorConfig,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PositionEvaluationPoint {
    pub row_index: usize,
    pub series_id: u64,
    pub available_time: chrono::DateTime<chrono::Utc>,
    pub fold_index: usize,
    pub prediction: f64,
    pub target_position: f64,
    pub gross_return: f64,
    pub transaction_cost: f64,
    pub funding_cost: f64,
    pub net_return: f64,
    pub equity: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PositionEvaluationReport {
    pub evaluation: CandidateEvaluation,
    pub ledger: Vec<PositionEvaluationPoint>,
}

struct PredictiveGateResult {
    predictive: PredictiveMetrics,
    failures: Vec<String>,
}

struct PositionReturnPoint {
    row_index: usize,
    series_id: u64,
    available_time: chrono::DateTime<chrono::Utc>,
    target_position: f64,
    gross_return: f64,
    transaction_cost: f64,
    funding_cost: f64,
    net_return: f64,
}

impl PredictiveGateResult {
    fn target_positions_to_net_returns(
        &self,
        rows: &[ResearchRow],
        target_positions: &[f64],
        range: std::ops::Range<usize>,
        costs: &EvaluationCostsV1,
    ) -> (Vec<PositionReturnPoint>, usize, f64, Option<f64>) {
        let mut previous_position = 0.0;
        let mut trade_count = 0;
        let mut total_turnover = 0.0;
        let capacity_features = costs.capacity_enabled().then(|| {
            (
                format!("bid_depth_top{}", costs.capacity_depth_levels),
                format!("ask_depth_top{}", costs.capacity_depth_levels),
            )
        });
        let mut max_book_depth_fraction = capacity_features.as_ref().map(|_| 0.0_f64);
        let range_start = range.start;
        let range_end = range.end;
        let points = (range_start..range_end)
            .map(|index| {
                if index == range_start || rows[index - 1].series_id != rows[index].series_id {
                    previous_position = 0.0;
                }
                let position = target_positions[index];
                let position_change = position - previous_position;
                let turnover = position_change.abs();
                total_turnover += turnover;
                if turnover > f64::EPSILON {
                    trade_count += 1;
                }
                let row = &rows[index];
                let gross_return = position * row.label;
                let mut transaction_cost_value = transaction_cost(
                    row,
                    turnover,
                    position_change,
                    costs,
                    &capacity_features,
                    &mut max_book_depth_fraction,
                );
                let funding_cost = row.funding_bps.max(0.0) * position.abs() / BPS;
                let series_end =
                    index + 1 == range_end || rows[index + 1].series_id != row.series_id;
                previous_position = position;
                if series_end && position.abs() > f64::EPSILON {
                    total_turnover += position.abs();
                    trade_count += 1;
                    transaction_cost_value += transaction_cost(
                        row,
                        position.abs(),
                        -position,
                        costs,
                        &capacity_features,
                        &mut max_book_depth_fraction,
                    );
                    previous_position = 0.0;
                }
                let net_return = gross_return - transaction_cost_value - funding_cost;
                PositionReturnPoint {
                    row_index: index,
                    series_id: row.series_id,
                    available_time: row.available_time,
                    target_position: position,
                    gross_return,
                    transaction_cost: transaction_cost_value,
                    funding_cost,
                    net_return,
                }
            })
            .collect();
        (points, trade_count, total_turnover, max_book_depth_fraction)
    }
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

    fn predictive_gates(
        &self,
        rows: &[ResearchRow],
        signals: &[f64],
        ranges: &[std::ops::Range<usize>],
        evaluator_version: &str,
    ) -> PredictiveGateResult {
        let predictive = predictive_metrics(rows, signals, ranges);
        let mut failures = Vec::new();
        let require_icir = matches!(
            evaluator_version,
            WALK_FORWARD_EVALUATOR_VERSION
                | CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION
                | ONNX_WALK_FORWARD_EVALUATOR_VERSION
        );
        if predictive
            .time_series_ic
            .is_none_or(|ic| ic < self.config.min_time_series_ic)
        {
            failures.push(format!(
                "time-series IC does not meet {:.8}",
                self.config.min_time_series_ic
            ));
        }
        if predictive
            .time_series_rank_ic
            .is_none_or(|ic| ic < self.config.min_time_series_rank_ic)
        {
            failures.push(format!(
                "time-series RankIC does not meet {:.8}",
                self.config.min_time_series_rank_ic
            ));
        }
        if require_icir
            && predictive
                .time_series_icir
                .is_none_or(|icir| icir < self.config.min_time_series_icir)
        {
            failures.push(format!(
                "time-series ICIR does not meet {:.8}",
                self.config.min_time_series_icir
            ));
        }
        if require_icir
            && predictive
                .time_series_rank_icir
                .is_none_or(|icir| icir < self.config.min_time_series_rank_icir)
        {
            failures.push(format!(
                "time-series RankICIR does not meet {:.8}",
                self.config.min_time_series_rank_icir
            ));
        }
        if predictive.positive_ic_ratio < self.config.min_positive_ic_ratio {
            failures.push(format!(
                "positive IC ratio {:.8} is below {:.8}",
                predictive.positive_ic_ratio, self.config.min_positive_ic_ratio
            ));
        }
        PredictiveGateResult {
            predictive,
            failures,
        }
    }

    pub fn evaluate_onnx_signals(
        &self,
        rows: &[ResearchRow],
        signals: &[f64],
        ranges: impl IntoIterator<Item = std::ops::Range<usize>>,
        sealed: bool,
        protocol: &EvaluationProtocolV1,
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
            protocol,
        )
    }

    pub(crate) fn evaluate_signals(
        &self,
        rows: &[ResearchRow],
        signals: &[f64],
        ranges: impl IntoIterator<Item = std::ops::Range<usize>>,
        evaluator_version: &str,
        protocol: &EvaluationProtocolV1,
    ) -> Result<CandidateEvaluation, String> {
        self.evaluate_ranges(rows, signals, ranges, evaluator_version, protocol)
    }

    pub(crate) fn evaluate_predictions_and_positions(
        &self,
        rows: &[ResearchRow],
        predictions: &[f64],
        target_positions: &[f64],
        ranges: impl IntoIterator<Item = std::ops::Range<usize>>,
        evaluator_version: &str,
        protocol: &EvaluationProtocolV1,
    ) -> Result<PositionEvaluationReport, String> {
        self.evaluate_prediction_position_ranges(
            rows,
            predictions,
            target_positions,
            ranges,
            evaluator_version,
            protocol,
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
                dataset.protocol(),
            )
        })
    }

    fn evaluate_ranges(
        &self,
        rows: &[ResearchRow],
        signals: &[f64],
        ranges: impl IntoIterator<Item = std::ops::Range<usize>>,
        evaluator_version: &str,
        protocol: &EvaluationProtocolV1,
    ) -> Result<CandidateEvaluation, String> {
        let target_positions = signals
            .iter()
            .copied()
            .map(signal_position)
            .collect::<Vec<_>>();
        self.evaluate_prediction_position_ranges(
            rows,
            signals,
            &target_positions,
            ranges,
            evaluator_version,
            protocol,
        )
        .map(|report| report.evaluation)
    }

    fn evaluate_prediction_position_ranges(
        &self,
        rows: &[ResearchRow],
        predictions: &[f64],
        target_positions: &[f64],
        ranges: impl IntoIterator<Item = std::ops::Range<usize>>,
        evaluator_version: &str,
        protocol: &EvaluationProtocolV1,
    ) -> Result<PositionEvaluationReport, String> {
        protocol.validate().map_err(|error| error.to_string())?;
        if predictions.len() != rows.len() || target_positions.len() != rows.len() {
            return Err("prediction or target-position length does not match dataset".to_string());
        }
        if rows.iter().any(|row| {
            row.fee_bps.to_bits() != protocol.costs.fee_bps.to_bits()
                || row.funding_bps < 0.0
                || row.funding_bps > protocol.costs.funding_bps
                || row.latency_bps.to_bits() != protocol.costs.latency_bps.to_bits()
        }) {
            return Err("dataset costs do not match the evaluation protocol".to_string());
        }
        if protocol.costs.cross_spread
            && rows.iter().any(|row| {
                row.features
                    .get("spread_bps")
                    .is_none_or(|spread| !spread.is_finite() || *spread < 0.0)
            })
        {
            return Err(
                "taker spread crossing requires a finite non-negative spread_bps feature"
                    .to_string(),
            );
        }
        if protocol.costs.capacity_enabled() {
            let bid_depth = format!("bid_depth_top{}", protocol.costs.capacity_depth_levels);
            let ask_depth = format!("ask_depth_top{}", protocol.costs.capacity_depth_levels);
            if rows.iter().any(|row| {
                ["mid_price", bid_depth.as_str(), ask_depth.as_str()]
                    .iter()
                    .any(|feature| {
                        row.features
                            .get(*feature)
                            .is_none_or(|value| !value.is_finite() || *value <= 0.0)
                    })
            }) {
                return Err(
                    "capacity checks require positive mid_price and matching top-N bid/ask depth features"
                        .to_string(),
                );
            }
        }
        if predictions
            .iter()
            .any(|value| !value.is_finite() || value.abs() > self.config.max_abs_signal)
        {
            return Err("model produced an invalid or unbounded prediction".to_string());
        }
        if target_positions
            .iter()
            .any(|value| !value.is_finite() || value.abs() > 1.0)
        {
            return Err("decision policy produced an invalid target position".to_string());
        }

        let ranges = ranges.into_iter().collect::<Vec<_>>();
        if ranges.is_empty() {
            return Err("evaluation has no folds".to_string());
        }
        let sealed = matches!(
            evaluator_version,
            SEALED_HOLDOUT_EVALUATOR_VERSION | alpha_domain::ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION
        );
        if (sealed
            && (ranges.len() != 1 || ranges[0].len() != protocol.walk_forward.sealed_holdout_rows))
            || (!sealed
                && (ranges.len() != protocol.walk_forward.fold_count
                    || ranges
                        .iter()
                        .any(|range| range.len() != protocol.walk_forward.validation_rows)))
        {
            return Err("evaluation ranges do not match the evaluation protocol".to_string());
        }
        for range in &ranges {
            if range.len() < self.config.min_validation_rows || range.end > rows.len() {
                return Err("evaluation range is too short or out of bounds".to_string());
            }
        }

        // Position mapping requires this token, so predictive gates cannot be skipped.
        let mut predictive_stage =
            self.predictive_gates(rows, predictions, &ranges, evaluator_version);

        let mut fold_metrics = Vec::with_capacity(ranges.len());
        let mut ledger = Vec::new();
        let mut equity = 1.0_f64;
        for (fold_index, range) in ranges.into_iter().enumerate() {
            let (points, trade_count, total_turnover, max_book_depth_fraction) = predictive_stage
                .target_positions_to_net_returns(rows, target_positions, range, &protocol.costs);
            let returns = points
                .iter()
                .map(|point| point.net_return)
                .collect::<Vec<_>>();
            for point in points {
                equity += point.net_return;
                ledger.push(PositionEvaluationPoint {
                    row_index: point.row_index,
                    series_id: point.series_id,
                    available_time: point.available_time,
                    fold_index: fold_index + 1,
                    prediction: predictions[point.row_index],
                    target_position: point.target_position,
                    gross_return: point.gross_return,
                    transaction_cost: point.transaction_cost,
                    funding_cost: point.funding_cost,
                    net_return: point.net_return,
                    equity,
                });
            }
            let mean = mean(&returns);
            let net_sharpe = sharpe_ratio(&returns, mean);
            let raw_score = t_statistic(&returns, mean);
            let max_drawdown = max_drawdown(&returns);
            if trade_count < self.config.min_trades {
                predictive_stage.failures.push(format!(
                    "fold {} trades {} are below {}",
                    fold_index + 1,
                    trade_count,
                    self.config.min_trades
                ));
            }
            if mean <= self.config.min_fold_mean_return {
                predictive_stage.failures.push(format!(
                    "fold {} does not establish positive net edge: mean {:.8} must exceed {:.8}",
                    fold_index + 1,
                    mean,
                    self.config.min_fold_mean_return
                ));
            }
            if max_drawdown > self.config.max_drawdown {
                predictive_stage.failures.push(format!(
                    "fold {} drawdown {:.8} exceeds {:.8}",
                    fold_index + 1,
                    max_drawdown,
                    self.config.max_drawdown
                ));
            }
            if max_book_depth_fraction
                .is_some_and(|fraction| fraction > protocol.costs.max_book_depth_fraction)
            {
                predictive_stage.failures.push(format!(
                    "fold {} top-{} book depth fraction {:.8} exceeds {:.8}",
                    fold_index + 1,
                    protocol.costs.capacity_depth_levels,
                    max_book_depth_fraction.expect("capacity is enabled"),
                    protocol.costs.max_book_depth_fraction,
                ));
            }
            fold_metrics.push(FoldEvaluationMetrics {
                fold_index: fold_index + 1,
                row_count: returns.len(),
                trade_count,
                total_turnover,
                mean_net_return: mean,
                cumulative_net_return: returns.iter().sum(),
                max_drawdown,
                net_sharpe,
                raw_score,
                max_book_depth_fraction,
            });
        }
        let raw_score = fold_metrics.iter().map(|fold| fold.raw_score).sum::<f64>()
            / fold_metrics.len().max(1) as f64;
        let adjusted_score = self
            .config
            .adjusted_score(raw_score)
            .map_err(|error| error.to_string())?;
        if adjusted_score < self.config.min_aggregate_score {
            predictive_stage.failures.push(format!(
                "multiple-testing-adjusted score {:.8} is below {:.8}",
                adjusted_score, self.config.min_aggregate_score
            ));
        }
        let PredictiveGateResult {
            predictive,
            failures,
        } = predictive_stage;
        let row_count = fold_metrics.iter().map(|fold| fold.row_count).sum();
        let metrics = EvaluationMetrics {
            predictive,
            row_count,
            trade_count: fold_metrics.iter().map(|fold| fold.trade_count).sum(),
            total_turnover: fold_metrics.iter().map(|fold| fold.total_turnover).sum(),
            mean_net_return: fold_metrics
                .iter()
                .map(|fold| fold.mean_net_return * fold.row_count as f64)
                .sum::<f64>()
                / row_count.max(1) as f64,
            cumulative_net_return: fold_metrics
                .iter()
                .map(|fold| fold.cumulative_net_return)
                .sum(),
            max_drawdown: fold_metrics
                .iter()
                .map(|fold| fold.max_drawdown)
                .fold(0.0, f64::max),
            net_sharpe: fold_metrics.iter().map(|fold| fold.net_sharpe).sum::<f64>()
                / fold_metrics.len().max(1) as f64,
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
            evaluation_protocol: Some(protocol.clone()),
            evaluation_protocol_hash: Some(
                protocol.content_hash().map_err(|error| error.to_string())?,
            ),
            metrics,
        };
        evaluation
            .validate_reason()
            .map_err(|reason| format!("evaluation evidence is inconsistent: {reason}"))?;
        Ok(PositionEvaluationReport { evaluation, ledger })
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
            context.protocol(),
        )
    }
}

fn formula(proposal: &EngineProposal) -> Result<&FactorAst, String> {
    match &proposal.artifact {
        CandidateArtifact::Formula(ast) => {
            ast.validate().map_err(|error| error.to_string())?;
            validate_live_formula(ast).map_err(|error| error.to_string())?;
            Ok(ast)
        }
        CandidateArtifact::CexFourStage(strategy) => {
            strategy.validate().map_err(|error| error.to_string())?;
            Ok(&strategy.executable_formula)
        }
        _ => Err("formula evaluator only accepts DSL formula artifacts".to_string()),
    }
}

pub(crate) fn evaluate_ast(ast: &FactorAst, rows: &[ResearchRow]) -> Result<Vec<f64>, String> {
    if validate_live_formula(ast).is_ok() {
        let mut output = Vec::with_capacity(rows.len());
        for range in contiguous_series_ranges(rows) {
            let series = &rows[range];
            output.extend(
                evaluate_live_formula_series(ast, series.len(), |row, field| {
                    series.get(row)?.features.get(field).copied()
                })
                .map_err(|error| error.to_string())?,
            );
        }
        return Ok(output);
    }
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
                FactorOperator::Rank => expanding_rank(&evaluate_ast(&args[0], rows)?, rows),
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
    let mut output = vec![0.0; values.len()];
    for range in contiguous_series_ranges(rows) {
        let series = &values[range.clone()];
        for offset in 0..series.len() {
            let start = (offset + 1).saturating_sub(window);
            let history = &series[start..=offset];
            output[range.start + offset] = match operation {
                RollingOperation::Delta => series[offset] - series[offset.saturating_sub(window)],
                RollingOperation::Mean => mean(history),
                RollingOperation::Std => standard_deviation(history, mean(history)),
                RollingOperation::ZScore => {
                    let average = mean(history);
                    let deviation = standard_deviation(history, average);
                    if deviation <= f64::EPSILON {
                        0.0
                    } else {
                        (series[offset] - average) / deviation
                    }
                }
            };
        }
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

fn expanding_rank(values: &[f64], rows: &[ResearchRow]) -> Result<Vec<f64>, String> {
    if values.iter().any(|value| !value.is_finite()) {
        return Err("rank input is not finite".to_string());
    }
    let mut output = vec![0.0; values.len()];
    for range in contiguous_series_ranges(rows) {
        for offset in 0..range.len() {
            let index = range.start + offset;
            let value = values[index];
            let less_or_equal = values[range.start..=index]
                .iter()
                .filter(|historical| **historical <= value)
                .count();
            output[index] = less_or_equal as f64 / (offset + 1) as f64;
        }
    }
    Ok(output)
}

fn transaction_cost(
    row: &ResearchRow,
    turnover: f64,
    position_change: f64,
    costs: &EvaluationCostsV1,
    capacity_features: &Option<(String, String)>,
    max_book_depth_fraction: &mut Option<f64>,
) -> f64 {
    let spread_crossing_bps = if costs.cross_spread {
        row.features.get("spread_bps").copied().unwrap_or(0.0) / 2.0
    } else {
        0.0
    };
    if let (Some((bid_depth, ask_depth)), Some(max_fraction)) =
        (capacity_features.as_ref(), max_book_depth_fraction.as_mut())
    {
        let depth_feature = if position_change > 0.0 {
            ask_depth.as_str()
        } else {
            bid_depth.as_str()
        };
        let depth_notional = row.features.get(depth_feature).copied().unwrap_or(0.0)
            * row.features.get("mid_price").copied().unwrap_or(0.0);
        if turnover > f64::EPSILON {
            let fraction = if depth_notional.is_finite() && depth_notional > 0.0 {
                costs.position_notional_usd * turnover / depth_notional
            } else {
                f64::MAX
            };
            *max_fraction = (*max_fraction).max(fraction);
        }
    }
    (row.fee_bps.max(0.0) - costs.rebate_bps
        + row.latency_bps.max(0.0)
        + costs.slippage_bps
        + spread_crossing_bps)
        * turnover
        / BPS
}

fn predictive_metrics(
    rows: &[ResearchRow],
    signals: &[f64],
    ranges: &[std::ops::Range<usize>],
) -> PredictiveMetrics {
    PredictiveMetrics::from_folds(
        ranges
            .iter()
            .enumerate()
            .map(|(fold_index, range)| {
                let factor_values = &signals[range.clone()];
                let labels = range
                    .clone()
                    .map(|index| rows[index].label)
                    .collect::<Vec<_>>();
                FoldPredictiveMetrics {
                    fold_index: fold_index + 1,
                    row_count: range.len(),
                    time_series_ic: pearson_correlation(factor_values, &labels),
                    time_series_rank_ic: spearman_correlation(factor_values, &labels),
                }
            })
            .collect(),
    )
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

fn pearson_correlation(left: &[f64], right: &[f64]) -> Option<f64> {
    if left.len() != right.len() || left.len() < 2 {
        return None;
    }
    let left_mean = mean(left);
    let right_mean = mean(right);
    let mut covariance = 0.0;
    let mut left_variance = 0.0;
    let mut right_variance = 0.0;
    for (left, right) in left.iter().zip(right) {
        let left_centered = left - left_mean;
        let right_centered = right - right_mean;
        covariance += left_centered * right_centered;
        left_variance += left_centered.powi(2);
        right_variance += right_centered.powi(2);
    }
    let denominator = (left_variance * right_variance).sqrt();
    (denominator > 0.0)
        .then(|| (covariance / denominator).clamp(-1.0, 1.0))
        .filter(|value| value.is_finite())
}

fn spearman_correlation(left: &[f64], right: &[f64]) -> Option<f64> {
    pearson_correlation(&average_ranks(left)?, &average_ranks(right)?)
}

fn average_ranks(values: &[f64]) -> Option<Vec<f64>> {
    if values.len() < 2 || values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let mut indices = (0..values.len()).collect::<Vec<_>>();
    indices.sort_by(|left, right| values[*left].total_cmp(&values[*right]));
    let mut ranks = vec![0.0; values.len()];
    let mut start = 0;
    while start < indices.len() {
        let mut end = start + 1;
        while end < indices.len() && values[indices[end]] == values[indices[start]] {
            end += 1;
        }
        let average_rank = (start + end + 1) as f64 / 2.0;
        for index in &indices[start..end] {
            ranks[*index] = average_rank;
        }
        start = end;
    }
    Some(ranks)
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

fn sharpe_ratio(values: &[f64], average: f64) -> f64 {
    let deviation = standard_deviation(values, average);
    // ponytail: this is a per-observation Sharpe; annualize only after dataset frequency is explicit.
    average / deviation.max(f64::EPSILON)
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
    use crate::evaluation::{prepare_dataset, ResearchRow};
    use alpha_domain::{EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationWalkForwardV1};
    use chrono::{Duration, Utc};

    fn map_signal_returns(
        gate: &PredictiveGateResult,
        rows: &[ResearchRow],
        signals: &[f64],
        range: std::ops::Range<usize>,
        costs: &EvaluationCostsV1,
    ) -> (Vec<f64>, usize, f64, Option<f64>) {
        let positions = signals
            .iter()
            .copied()
            .map(signal_position)
            .collect::<Vec<_>>();
        let (points, trades, turnover, capacity) =
            gate.target_positions_to_net_returns(rows, &positions, range, costs);
        (
            points.into_iter().map(|point| point.net_return).collect(),
            trades,
            turnover,
            capacity,
        )
    }

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
                series_id: 1,
                available_time: start + Duration::minutes(index as i64),
                signal: if index % 2 == 0 { 1.0 } else { -1.0 },
                features: std::collections::BTreeMap::from([(
                    "book_imbalance".to_string(),
                    if index % 2 == 0 { 1.0 } else { -1.0 },
                )]),
                label: if index % 2 == 0 { 0.01 } else { -0.01 },
                fee_bps,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.0,
            })
            .collect()
    }

    fn protocol(fee_bps: f64, fold_count: usize) -> EvaluationProtocolV1 {
        EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 200,
                validation_rows: 64,
                fold_count,
                purge_rows: 1,
                embargo_rows: 1,
                sealed_holdout_rows: 64,
            },
            EvaluationCostsV1 {
                fee_bps,
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
                observation_frequency_millis: 60_000,
            },
        )
        .unwrap()
    }

    fn dataset(fee_bps: f64) -> PreparedDataset {
        prepare_dataset(rows(fee_bps), &protocol(fee_bps, 3)).unwrap()
    }

    #[test]
    fn causal_signal_formula_passes_walk_forward_and_holdout() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "book_imbalance".to_string(),
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
    fn causal_signal_records_predictive_metrics_before_trading_mapping() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "book_imbalance".to_string(),
        )));

        let result = evaluator
            .evaluate(&proposal, &dataset(0.0).engine_context())
            .unwrap();

        assert_eq!(result.metrics.predictive.time_series_ic, Some(1.0));
        assert_eq!(result.metrics.predictive.time_series_rank_ic, Some(1.0));
        assert!(result.metrics.predictive.time_series_icir.unwrap() > 1.0);
        assert!(result.metrics.predictive.time_series_rank_icir.unwrap() > 1.0);
        assert_eq!(result.metrics.predictive.positive_ic_ratio, 1.0);
        assert_eq!(result.metrics.predictive.folds.len(), 3);
        assert!(result.metrics.net_sharpe > 0.0);
    }

    #[test]
    fn model_predictions_keep_magnitude_and_emit_a_reconciled_equity_ledger() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let prepared = dataset(0.0);
        let context = prepared.engine_context();
        let predictions = context
            .rows()
            .iter()
            .map(|row| row.label)
            .collect::<Vec<_>>();
        let positions = predictions
            .iter()
            .map(|prediction| prediction.signum() * 0.25)
            .collect::<Vec<_>>();
        let report = evaluator
            .evaluate_predictions_and_positions(
                context.rows(),
                &predictions,
                &positions,
                context.folds().iter().map(|fold| fold.validation.clone()),
                CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
                context.protocol(),
            )
            .unwrap();

        assert_eq!(
            report.evaluation.metrics.predictive.time_series_ic,
            Some(1.0)
        );
        assert!(report
            .ledger
            .iter()
            .all(|point| point.target_position.abs() == 0.25));
        let cumulative = report
            .ledger
            .iter()
            .map(|point| point.net_return)
            .sum::<f64>();
        assert!((cumulative - report.evaluation.metrics.cumulative_net_return).abs() < 1.0e-12);
        assert!((report.ledger.last().unwrap().equity - (1.0 + cumulative)).abs() < 1.0e-12);
    }

    #[test]
    fn predictive_gate_stage_is_required_before_trade_mapping() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let dataset = dataset(f64::MAX);
        let context = dataset.engine_context();
        let signals = context
            .rows()
            .iter()
            .map(|row| row.signal)
            .collect::<Vec<_>>();
        let ranges = context
            .folds()
            .iter()
            .map(|fold| fold.validation.clone())
            .collect::<Vec<_>>();

        let predictive_stage = evaluator.predictive_gates(
            context.rows(),
            &signals,
            &ranges,
            WALK_FORWARD_EVALUATOR_VERSION,
        );

        assert_eq!(predictive_stage.predictive.time_series_ic, Some(1.0));
        assert!(map_signal_returns(
            &predictive_stage,
            context.rows(),
            &signals,
            ranges[0].clone(),
            &context.protocol().costs,
        )
        .0
        .iter()
        .any(|value| !value.is_finite()));
    }

    #[test]
    fn taker_costs_charge_fee_latency_slippage_and_half_spread_per_turnover() {
        let mut input = rows(2.0);
        for row in &mut input {
            row.latency_bps = 0.5;
            row.features.insert("spread_bps".to_string(), 0.2);
        }
        let signals = input.iter().map(|row| row.signal).collect::<Vec<_>>();
        let costs = EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            slippage_bps: 0.75,
            cross_spread: true,
            position_notional_usd: 0.0,
            capacity_depth_levels: 0,
            max_book_depth_fraction: 0.0,
        };
        let gate = FormulaEvaluator::new(FormulaEvaluatorConfig::default())
            .unwrap()
            .predictive_gates(
                &input,
                &signals,
                std::slice::from_ref(&(0..3)),
                WALK_FORWARD_EVALUATOR_VERSION,
            );

        let (net_returns, trade_count, _, max_book_depth_fraction) =
            map_signal_returns(&gate, &input, &signals, 0..3, &costs);

        assert_eq!(trade_count, 4);
        assert_eq!(max_book_depth_fraction, None);
        assert!((net_returns.iter().sum::<f64>() - 0.02799).abs() < 1.0e-12);
    }

    #[test]
    fn walk_forward_evidence_reports_rebate_adjusted_return_and_turnover() {
        let mut evaluation_protocol = protocol(2.0, 1);
        evaluation_protocol.costs.rebate_bps = 1.0;
        let dataset = prepare_dataset(rows(2.0), &evaluation_protocol).unwrap();
        let evaluation = FormulaEvaluator::new(FormulaEvaluatorConfig::default())
            .unwrap()
            .evaluate(
                &proposal(FactorAst::Terminal(FactorTerminal::Field(
                    "book_imbalance".to_string(),
                ))),
                &dataset.engine_context(),
            )
            .unwrap();

        assert_eq!(evaluation.metrics.total_turnover, 128.0);
        assert_eq!(evaluation.metrics.folds[0].total_turnover, 128.0);
        assert!((evaluation.metrics.cumulative_net_return - 0.6272).abs() < 1.0e-12);
    }

    #[test]
    fn walk_forward_evidence_ignores_holdout_mutations() {
        let evaluation_protocol = protocol(0.0, 3);
        let original = dataset(0.0);
        let mut mutated_rows = rows(0.0);
        for row in &mut mutated_rows[436..] {
            row.features.insert("book_imbalance".to_string(), 0.0);
            row.label = -row.label;
        }
        let mutated = prepare_dataset(mutated_rows, &evaluation_protocol).unwrap();
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let candidate = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "book_imbalance".to_string(),
        )));

        assert_eq!(
            evaluator
                .evaluate(&candidate, &original.engine_context())
                .unwrap(),
            evaluator
                .evaluate(&candidate, &mutated.engine_context())
                .unwrap()
        );
        assert_ne!(
            evaluator.evaluate_sealed(&candidate, &original).unwrap(),
            evaluator.evaluate_sealed(&candidate, &mutated).unwrap()
        );
    }

    #[test]
    fn full_side_flip_charges_two_units_of_baseline_turnover() {
        let mut input = rows(2.0);
        for row in &mut input[..3] {
            row.label = 0.0;
            row.latency_bps = 0.5;
        }
        let signals = vec![-1.0, 1.0, 1.0];
        let costs = EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            slippage_bps: 0.0,
            cross_spread: false,
            position_notional_usd: 0.0,
            capacity_depth_levels: 0,
            max_book_depth_fraction: 0.0,
        };
        let gate = FormulaEvaluator::new(FormulaEvaluatorConfig::default())
            .unwrap()
            .predictive_gates(
                &input,
                &signals,
                std::slice::from_ref(&(0..3)),
                WALK_FORWARD_EVALUATOR_VERSION,
            );

        let (net_returns, trade_count, _, _) =
            map_signal_returns(&gate, &input, &signals, 0..3, &costs);

        assert_eq!(trade_count, 3);
        assert!((net_returns[0] + 0.00025).abs() < 1.0e-12);
        assert!((net_returns[1] + 0.0005).abs() < 1.0e-12);
        assert!((net_returns[2] + 0.00025).abs() < 1.0e-12);
    }

    #[test]
    fn rolling_state_resets_on_second_series() {
        let start = Utc::now();
        let rows = vec![
            ResearchRow {
                series_id: 1,
                available_time: start,
                signal: 0.0,
                features: std::collections::BTreeMap::from([("book_imbalance".to_string(), 1.0)]),
                label: 0.0,
                fee_bps: 0.0,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.0,
            },
            ResearchRow {
                series_id: 1,
                available_time: start + Duration::minutes(1),
                signal: 0.0,
                features: std::collections::BTreeMap::from([("book_imbalance".to_string(), 3.0)]),
                label: 0.0,
                fee_bps: 0.0,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.0,
            },
            ResearchRow {
                series_id: 2,
                available_time: start + Duration::minutes(10),
                signal: 0.0,
                features: std::collections::BTreeMap::from([("book_imbalance".to_string(), 10.0)]),
                label: 0.0,
                fee_bps: 0.0,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.0,
            },
            ResearchRow {
                series_id: 2,
                available_time: start + Duration::minutes(11),
                signal: 0.0,
                features: std::collections::BTreeMap::from([("book_imbalance".to_string(), 13.0)]),
                label: 0.0,
                fee_bps: 0.0,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.0,
            },
        ];
        let delta = evaluate_ast(
            &FactorAst::call(
                FactorOperator::Delta,
                vec![
                    FactorAst::Terminal(FactorTerminal::Field("book_imbalance".to_string())),
                    FactorAst::Terminal(FactorTerminal::Constant("1".to_string())),
                ],
            )
            .unwrap(),
            &rows,
        )
        .unwrap();
        let zscore = evaluate_ast(
            &FactorAst::call(
                FactorOperator::ZScore,
                vec![
                    FactorAst::Terminal(FactorTerminal::Field("book_imbalance".to_string())),
                    FactorAst::Terminal(FactorTerminal::Constant("2".to_string())),
                ],
            )
            .unwrap(),
            &rows,
        )
        .unwrap();

        assert_eq!(delta, vec![0.0, 2.0, 0.0, 3.0]);
        assert_eq!(zscore[2], 0.0);
        assert!(zscore[3].is_finite());
    }

    #[test]
    fn series_end_forces_close_and_range_end_flattens() {
        let start = Utc::now();
        let input = vec![
            ResearchRow {
                series_id: 1,
                available_time: start,
                signal: 1.0,
                features: std::collections::BTreeMap::from([("spread_bps".to_string(), 0.0)]),
                label: 0.0,
                fee_bps: 2.0,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.0,
            },
            ResearchRow {
                series_id: 1,
                available_time: start + Duration::minutes(1),
                signal: 1.0,
                features: std::collections::BTreeMap::from([("spread_bps".to_string(), 0.0)]),
                label: 0.0,
                fee_bps: 2.0,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.0,
            },
            ResearchRow {
                series_id: 2,
                available_time: start + Duration::minutes(10),
                signal: 1.0,
                features: std::collections::BTreeMap::from([("spread_bps".to_string(), 0.0)]),
                label: 0.0,
                fee_bps: 2.0,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.0,
            },
        ];
        let signals = vec![1.0, 1.0, 1.0];
        let costs = EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            slippage_bps: 0.0,
            cross_spread: false,
            position_notional_usd: 0.0,
            capacity_depth_levels: 0,
            max_book_depth_fraction: 0.0,
        };
        let gate = FormulaEvaluator::new(FormulaEvaluatorConfig::default())
            .unwrap()
            .predictive_gates(
                &input,
                &signals,
                std::slice::from_ref(&(0..3)),
                WALK_FORWARD_EVALUATOR_VERSION,
            );

        let (net_returns, trade_count, total_turnover, _) =
            map_signal_returns(&gate, &input, &signals, 0..3, &costs);

        assert_eq!(trade_count, 4);
        assert_eq!(total_turnover, 4.0);
        assert!((net_returns.iter().sum::<f64>() + 0.0008).abs() < 1.0e-12);
    }

    #[test]
    fn capacity_uses_trade_notional_and_same_side_top_n_depth() {
        let mut input = rows(0.0);
        for row in &mut input {
            row.features.insert("mid_price".to_string(), 1_000.0);
            row.features.insert("bid_depth_top5".to_string(), 200.0);
            row.features.insert("ask_depth_top5".to_string(), 50.0);
        }
        let signals = input.iter().map(|row| row.signal).collect::<Vec<_>>();
        let costs = EvaluationCostsV1 {
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            slippage_bps: 0.0,
            cross_spread: false,
            position_notional_usd: 10_000.0,
            capacity_depth_levels: 5,
            max_book_depth_fraction: 0.1,
        };
        let gate = FormulaEvaluator::new(FormulaEvaluatorConfig::default())
            .unwrap()
            .predictive_gates(
                &input,
                &signals,
                std::slice::from_ref(&(0..3)),
                WALK_FORWARD_EVALUATOR_VERSION,
            );

        let (_, trade_count, _, max_fraction) =
            map_signal_returns(&gate, &input, &signals, 0..3, &costs);

        assert_eq!(trade_count, 4);
        assert_eq!(max_fraction, Some(0.4));
    }

    #[test]
    fn capacity_breach_is_a_bound_evaluation_failure() {
        let mut input = rows(0.0);
        for row in &mut input {
            row.features.insert("mid_price".to_string(), 1_000.0);
            row.features.insert("bid_depth_top5".to_string(), 100.0);
            row.features.insert("ask_depth_top5".to_string(), 100.0);
        }
        let mut protocol = protocol(0.0, 3);
        protocol.costs.position_notional_usd = 10_000.0;
        protocol.costs.capacity_depth_levels = 5;
        protocol.costs.max_book_depth_fraction = 0.1;
        let dataset = prepare_dataset(input, &protocol).unwrap();
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();

        let result = evaluator
            .evaluate(
                &proposal(FactorAst::Terminal(FactorTerminal::Field(
                    "book_imbalance".to_string(),
                ))),
                &dataset.engine_context(),
            )
            .unwrap();

        assert!(!result.passed);
        assert!(result
            .failure_reasons
            .iter()
            .any(|reason| reason.contains("book depth fraction")));
        assert!(result
            .metrics
            .folds
            .iter()
            .all(|fold| fold.max_book_depth_fraction == Some(0.2)));
    }

    #[test]
    fn pearson_ic_is_scale_invariant_for_small_finite_values() {
        assert_eq!(
            pearson_correlation(&[1.0e-12, 2.0e-12, 3.0e-12], &[2.0e-12, 4.0e-12, 6.0e-12],),
            Some(1.0)
        );
    }

    #[test]
    fn single_fold_walk_forward_records_missing_icir_as_a_failed_evaluation() {
        let input = prepare_dataset(rows(0.0), &protocol(0.0, 1)).unwrap();
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let result = evaluator
            .evaluate(
                &proposal(FactorAst::Terminal(FactorTerminal::Field(
                    "book_imbalance".to_string(),
                ))),
                &input.engine_context(),
            )
            .unwrap();

        assert!(!result.passed);
        assert_eq!(result.metrics.predictive.time_series_icir, None);
        assert_eq!(result.metrics.predictive.time_series_rank_icir, None);
        assert!(result
            .failure_reasons
            .iter()
            .any(|reason| reason.starts_with("time-series ICIR")));
        assert!(result
            .failure_reasons
            .iter()
            .any(|reason| reason.starts_with("time-series RankICIR")));
        result.validate().unwrap();
    }

    #[test]
    fn cex_baseline_requires_icir_and_rank_icir_when_they_are_missing() {
        let input = prepare_dataset(rows(0.0), &protocol(0.0, 1)).unwrap();
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let context = input.engine_context();
        let signals = context
            .rows()
            .iter()
            .map(|row| row.signal)
            .collect::<Vec<_>>();
        let ranges = context
            .folds()
            .iter()
            .map(|fold| fold.validation.clone())
            .collect::<Vec<_>>();

        let result = evaluator
            .evaluate_signals(
                context.rows(),
                &signals,
                ranges,
                CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
                context.protocol(),
            )
            .unwrap();

        assert!(!result.passed);
        assert_eq!(result.metrics.predictive.time_series_icir, None);
        assert_eq!(result.metrics.predictive.time_series_rank_icir, None);
        assert!(result
            .failure_reasons
            .iter()
            .any(|reason| reason.starts_with("time-series ICIR")));
        assert!(result
            .failure_reasons
            .iter()
            .any(|reason| reason.starts_with("time-series RankICIR")));
        result.validate().unwrap();
    }

    #[test]
    fn walk_forward_rejects_a_formula_that_live_cannot_construct() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(
            FactorAst::call(
                FactorOperator::Rank,
                vec![FactorAst::Terminal(FactorTerminal::Field(
                    "book_imbalance".to_string(),
                ))],
            )
            .unwrap(),
        );

        assert_eq!(
            evaluator
                .evaluate(&proposal, &dataset(0.0).engine_context())
                .unwrap_err(),
            "unsupported live operator: rank"
        );
    }

    #[test]
    fn sealed_evaluation_rejects_a_formula_that_live_cannot_construct() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(
            FactorAst::call(
                FactorOperator::Rank,
                vec![FactorAst::Terminal(FactorTerminal::Field(
                    "mid_price".to_string(),
                ))],
            )
            .unwrap(),
        );

        assert_eq!(
            evaluator
                .evaluate_sealed(&proposal, &dataset(0.0))
                .unwrap_err(),
            "unsupported live operator: rank"
        );
    }

    #[test]
    fn registered_research_only_feature_is_rejected_before_evidence() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "lob_imbalance".to_string(),
        )));
        let mut rows = rows(0.0);
        for row in &mut rows {
            row.features.insert("lob_imbalance".to_string(), row.signal);
        }
        let dataset = prepare_dataset(rows, &protocol(0.0, 3)).unwrap();

        assert_eq!(
            evaluator
                .evaluate(&proposal, &dataset.engine_context())
                .unwrap_err(),
            "unsupported live field: lob_imbalance"
        );
    }

    #[test]
    fn transaction_cost_can_reject_a_high_turnover_formula() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "book_imbalance".to_string(),
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
            "book_imbalance".to_string(),
        )));

        assert!(evaluator
            .evaluate(&proposal, &dataset(f64::MAX).engine_context())
            .is_err());
    }

    #[test]
    fn constant_formula_is_rejected_without_evidence() {
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Constant(
            "0".to_string(),
        )));
        let input = dataset(0.0);
        assert_eq!(
            evaluator
                .evaluate(&proposal, &input.engine_context())
                .unwrap_err(),
            "formula must reference a snapshot or bar field"
        );
        assert_eq!(
            evaluator.evaluate_sealed(&proposal, &input).unwrap_err(),
            "formula must reference a snapshot or bar field"
        );
    }

    #[test]
    fn sealed_evidence_persists_metrics_config_and_multiple_testing_haircut() {
        let config = FormulaEvaluatorConfig {
            multiple_testing_trials: 100,
            ..FormulaEvaluatorConfig::default()
        };
        let evaluator = FormulaEvaluator::new(config.clone()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "book_imbalance".to_string(),
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
        let dataset = prepare_dataset(input, &protocol(0.0, 3)).unwrap();
        let evaluator = FormulaEvaluator::new(FormulaEvaluatorConfig::default()).unwrap();
        let proposal = proposal(FactorAst::Terminal(FactorTerminal::Field(
            "book_imbalance".to_string(),
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
