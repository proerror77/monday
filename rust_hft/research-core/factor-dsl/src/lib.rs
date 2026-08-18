//! Canonical factor DSL and program-factor AST contracts.

use hft_research_manifest::ManifestId;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

pub const MAX_FACTOR_AST_DEPTH: usize = 64;
pub const MAX_FACTOR_AST_NODES: usize = 10_000;
pub const MAX_LIVE_HISTORY_ROWS: usize = 10_001;
pub const MAX_LIVE_EVALUATION_STEPS: usize = 1_000_000;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FactorDslError {
    #[error("operator arity mismatch for {operator}: expected {expected}, got {actual}")]
    ArityMismatch {
        operator: String,
        expected: usize,
        actual: usize,
    },
    #[error("factor AST exceeds the maximum depth of {max_depth}")]
    AstTooDeep { max_depth: usize },
    #[error("factor AST exceeds the maximum size of {max_nodes} nodes")]
    AstTooLarge { max_nodes: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveEventDomain {
    Snapshot,
    Bar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveFormulaCapability {
    pub event_domain: LiveEventDomain,
    pub history_rows: usize,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LiveFormulaCapabilityError {
    #[error("invalid factor AST: {0}")]
    InvalidAst(#[from] FactorDslError),
    #[error("unsupported live operator: {0}")]
    UnsupportedOperator(String),
    #[error("unsupported live field: {0}")]
    UnsupportedField(String),
    #[error("formula mixes snapshot and bar fields")]
    MixedEventDomains,
    #[error("formula must reference a snapshot or bar field")]
    MissingEventDomain,
    #[error("formula constant is not a finite f64: {0}")]
    InvalidConstant(String),
    #[error("rolling window must be a positive integer no larger than 10000")]
    InvalidRollingWindow,
    #[error("live formula requires more than {max_rows} history rows")]
    HistoryTooLong { max_rows: usize },
    #[error("live formula exceeds {max_steps} bounded evaluation steps")]
    EvaluationTooExpensive { max_steps: usize },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LiveFormulaEvaluationError {
    #[error(transparent)]
    Capability(#[from] LiveFormulaCapabilityError),
    #[error("formula field is not registered: {0}")]
    MissingField(String),
    #[error("live formula produced a non-finite value")]
    NonFiniteValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorTerminal {
    Field(String),
    Constant(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorOperator {
    Add,
    Sub,
    Mul,
    Div,
    Abs,
    Log,
    Rank,
    ZScore,
    Delta,
    Mean,
    Std,
    GreaterThan,
    LessThan,
    IfElse,
}

impl FactorOperator {
    pub fn arity(&self) -> usize {
        match self {
            Self::Abs | Self::Log | Self::Rank => 1,
            Self::Add
            | Self::Sub
            | Self::Mul
            | Self::Div
            | Self::ZScore
            | Self::Delta
            | Self::Mean
            | Self::Std
            | Self::GreaterThan
            | Self::LessThan => 2,
            Self::IfElse => 3,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Abs => "abs",
            Self::Log => "log",
            Self::Rank => "rank",
            Self::ZScore => "zscore",
            Self::Delta => "delta",
            Self::Mean => "mean",
            Self::Std => "std",
            Self::GreaterThan => ">",
            Self::LessThan => "<",
            Self::IfElse => "if_else",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorAst {
    Terminal(FactorTerminal),
    Call {
        operator: FactorOperator,
        args: Vec<FactorAst>,
    },
}

impl FactorAst {
    pub fn call(operator: FactorOperator, args: Vec<FactorAst>) -> Result<Self, FactorDslError> {
        let expected = operator.arity();
        let actual = args.len();
        if expected != actual {
            return Err(FactorDslError::ArityMismatch {
                operator: operator.symbol().to_string(),
                expected,
                actual,
            });
        }
        Ok(Self::Call { operator, args })
    }

    pub fn validate(&self) -> Result<(), FactorDslError> {
        let mut stack = vec![(self, 1_usize)];
        let mut nodes = 0_usize;
        while let Some((node, depth)) = stack.pop() {
            if depth > MAX_FACTOR_AST_DEPTH {
                return Err(FactorDslError::AstTooDeep {
                    max_depth: MAX_FACTOR_AST_DEPTH,
                });
            }
            nodes = nodes.saturating_add(1);
            if nodes > MAX_FACTOR_AST_NODES {
                return Err(FactorDslError::AstTooLarge {
                    max_nodes: MAX_FACTOR_AST_NODES,
                });
            }
            if let Self::Call { operator, args } = node {
                let expected = operator.arity();
                let actual = args.len();
                if expected != actual {
                    return Err(FactorDslError::ArityMismatch {
                        operator: operator.symbol().to_string(),
                        expected,
                        actual,
                    });
                }
                stack.extend(args.iter().map(|arg| (arg, depth + 1)));
            }
        }
        Ok(())
    }
}

pub fn validate_live_formula(
    ast: &FactorAst,
) -> Result<LiveFormulaCapability, LiveFormulaCapabilityError> {
    ast.validate()?;
    let mut event_domain = None;
    let history_rows = validate_live_node(ast, &mut event_domain)?;
    if live_evaluation_steps(ast, history_rows)
        .is_none_or(|steps| steps > MAX_LIVE_EVALUATION_STEPS)
    {
        return Err(LiveFormulaCapabilityError::EvaluationTooExpensive {
            max_steps: MAX_LIVE_EVALUATION_STEPS,
        });
    }
    Ok(LiveFormulaCapability {
        event_domain: event_domain.ok_or(LiveFormulaCapabilityError::MissingEventDomain)?,
        history_rows,
    })
}

fn validate_live_node(
    ast: &FactorAst,
    event_domain: &mut Option<LiveEventDomain>,
) -> Result<usize, LiveFormulaCapabilityError> {
    match ast {
        FactorAst::Terminal(FactorTerminal::Constant(value)) => {
            let parsed = value
                .parse::<f64>()
                .map_err(|_| LiveFormulaCapabilityError::InvalidConstant(value.clone()))?;
            if !parsed.is_finite() {
                return Err(LiveFormulaCapabilityError::InvalidConstant(value.clone()));
            }
            Ok(1)
        }
        FactorAst::Terminal(FactorTerminal::Field(field)) => {
            let current = match field.as_str() {
                "best_bid" | "best_ask" | "mid_price" | "spread" | "spread_bps" | "bid_size"
                | "ask_size" | "book_imbalance" => LiveEventDomain::Snapshot,
                "open" | "high" | "low" | "close" | "volume" | "trade_count" | "bar_return" => {
                    LiveEventDomain::Bar
                }
                _ => return Err(LiveFormulaCapabilityError::UnsupportedField(field.clone())),
            };
            match *event_domain {
                Some(existing) if existing != current => {
                    return Err(LiveFormulaCapabilityError::MixedEventDomains)
                }
                None => *event_domain = Some(current),
                _ => {}
            }
            Ok(1)
        }
        FactorAst::Call { operator, args } => {
            let history_rows = match operator {
                FactorOperator::Delta | FactorOperator::ZScore => {
                    let value_rows = validate_live_node(&args[0], event_domain)?;
                    validate_live_node(&args[1], event_domain)?;
                    let FactorAst::Terminal(FactorTerminal::Constant(window)) = &args[1] else {
                        return Err(LiveFormulaCapabilityError::InvalidRollingWindow);
                    };
                    let window = window
                        .parse::<usize>()
                        .ok()
                        .filter(|window| (1..=10_000).contains(window))
                        .ok_or(LiveFormulaCapabilityError::InvalidRollingWindow)?;
                    value_rows
                        .checked_add(if *operator == FactorOperator::Delta {
                            window
                        } else {
                            window - 1
                        })
                        .ok_or(LiveFormulaCapabilityError::HistoryTooLong {
                            max_rows: MAX_LIVE_HISTORY_ROWS,
                        })?
                }
                FactorOperator::Add
                | FactorOperator::Sub
                | FactorOperator::Mul
                | FactorOperator::Abs
                | FactorOperator::GreaterThan
                | FactorOperator::LessThan
                | FactorOperator::IfElse => {
                    let mut history_rows = 1;
                    for arg in args {
                        history_rows = history_rows.max(validate_live_node(arg, event_domain)?);
                    }
                    history_rows
                }
                _ => {
                    return Err(LiveFormulaCapabilityError::UnsupportedOperator(
                        operator.symbol().to_string(),
                    ))
                }
            };
            if history_rows > MAX_LIVE_HISTORY_ROWS {
                return Err(LiveFormulaCapabilityError::HistoryTooLong {
                    max_rows: MAX_LIVE_HISTORY_ROWS,
                });
            }
            Ok(history_rows)
        }
    }
}

fn live_evaluation_steps(ast: &FactorAst, history_rows: usize) -> Option<usize> {
    match ast {
        FactorAst::Terminal(_) => Some(history_rows),
        FactorAst::Call { operator, args } => {
            let own_steps = if *operator == FactorOperator::ZScore {
                history_rows.checked_mul(rolling_window(&args[1])?)?
            } else {
                history_rows
            };
            let children = if matches!(operator, FactorOperator::Delta | FactorOperator::ZScore) {
                live_evaluation_steps(&args[0], history_rows)?
            } else {
                args.iter().try_fold(0_usize, |total, arg| {
                    total.checked_add(live_evaluation_steps(arg, history_rows)?)
                })?
            };
            own_steps.checked_add(children)
        }
    }
}

pub fn evaluate_live_formula_series(
    ast: &FactorAst,
    row_count: usize,
    field_value: impl Fn(usize, &str) -> Option<f64>,
) -> Result<Vec<f64>, LiveFormulaEvaluationError> {
    validate_live_formula(ast)?;
    let values = evaluate_live_series_node(ast, row_count, &field_value)?;
    if values.iter().any(|value| !value.is_finite()) {
        return Err(LiveFormulaEvaluationError::NonFiniteValue);
    }
    Ok(values)
}

fn evaluate_live_series_node(
    ast: &FactorAst,
    row_count: usize,
    field_value: &impl Fn(usize, &str) -> Option<f64>,
) -> Result<Vec<f64>, LiveFormulaEvaluationError> {
    match ast {
        FactorAst::Terminal(FactorTerminal::Field(field)) => (0..row_count)
            .map(|row| {
                field_value(row, field)
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| LiveFormulaEvaluationError::MissingField(field.clone()))
            })
            .collect(),
        FactorAst::Terminal(FactorTerminal::Constant(value)) => {
            Ok(vec![
                value.parse::<f64>().map_err(|_| {
                    LiveFormulaEvaluationError::NonFiniteValue
                })?;
                row_count
            ])
        }
        FactorAst::Call { operator, args } => match operator {
            FactorOperator::Abs => Ok(evaluate_live_series_node(&args[0], row_count, field_value)?
                .into_iter()
                .map(f64::abs)
                .collect()),
            FactorOperator::Delta => {
                let values = evaluate_live_series_node(&args[0], row_count, field_value)?;
                let window =
                    rolling_window(&args[1]).ok_or(LiveFormulaEvaluationError::NonFiniteValue)?;
                Ok((0..row_count)
                    .map(|index| values[index] - values[index.saturating_sub(window)])
                    .collect())
            }
            FactorOperator::ZScore => {
                let values = evaluate_live_series_node(&args[0], row_count, field_value)?;
                let window =
                    rolling_window(&args[1]).ok_or(LiveFormulaEvaluationError::NonFiniteValue)?;
                Ok((0..row_count)
                    .map(|index| {
                        let start = (index + 1).saturating_sub(window);
                        let history = &values[start..=index];
                        let mean = history.iter().sum::<f64>() / history.len() as f64;
                        let deviation = (history
                            .iter()
                            .map(|value| (value - mean).powi(2))
                            .sum::<f64>()
                            / history.len() as f64)
                            .sqrt();
                        if deviation <= f64::EPSILON {
                            0.0
                        } else {
                            (values[index] - mean) / deviation
                        }
                    })
                    .collect())
            }
            FactorOperator::IfElse => {
                let condition = evaluate_live_series_node(&args[0], row_count, field_value)?;
                let truthy = evaluate_live_series_node(&args[1], row_count, field_value)?;
                let falsy = evaluate_live_series_node(&args[2], row_count, field_value)?;
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
            FactorOperator::Add
            | FactorOperator::Sub
            | FactorOperator::Mul
            | FactorOperator::GreaterThan
            | FactorOperator::LessThan => {
                let left = evaluate_live_series_node(&args[0], row_count, field_value)?;
                let right = evaluate_live_series_node(&args[1], row_count, field_value)?;
                Ok(left
                    .into_iter()
                    .zip(right)
                    .map(|(left, right)| match operator {
                        FactorOperator::Add => left + right,
                        FactorOperator::Sub => left - right,
                        FactorOperator::Mul => left * right,
                        FactorOperator::GreaterThan => f64::from(left > right),
                        FactorOperator::LessThan => f64::from(left < right),
                        _ => unreachable!(),
                    })
                    .collect())
            }
            _ => Err(LiveFormulaCapabilityError::UnsupportedOperator(
                operator.symbol().to_string(),
            )
            .into()),
        },
    }
}

fn rolling_window(ast: &FactorAst) -> Option<usize> {
    let FactorAst::Terminal(FactorTerminal::Constant(value)) = ast else {
        return None;
    };
    value
        .parse::<usize>()
        .ok()
        .filter(|window| (1..=10_000).contains(window))
}

impl fmt::Display for FactorAst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FactorAst::Terminal(FactorTerminal::Field(name)) => write!(f, "{name}"),
            FactorAst::Terminal(FactorTerminal::Constant(value)) => write!(f, "{value}"),
            FactorAst::Call { operator, args }
                if args.len() == 2 && operator.symbol().len() == 1 =>
            {
                write!(f, "({} {} {})", args[0], operator.symbol(), args[1])
            }
            FactorAst::Call { operator, args } => {
                let rendered = args
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "{}({})", operator.symbol(), rendered)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactorProgram {
    pub id: String,
    pub ast: FactorAst,
    pub feature_manifest_id: ManifestId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_arity() {
        let arg = FactorAst::Terminal(FactorTerminal::Field("oi".to_string()));
        let err = FactorAst::call(FactorOperator::Add, vec![arg]).unwrap_err();
        assert_eq!(
            err,
            FactorDslError::ArityMismatch {
                operator: "+".to_string(),
                expected: 2,
                actual: 1
            }
        );
    }

    #[test]
    fn validate_rejects_deserialized_wrong_arity_recursively() {
        let bad_child = FactorAst::Call {
            operator: FactorOperator::Add,
            args: vec![FactorAst::Terminal(FactorTerminal::Field("oi".to_string()))],
        };
        let ast = FactorAst::Call {
            operator: FactorOperator::Abs,
            args: vec![bad_child],
        };

        assert_eq!(
            ast.validate().unwrap_err(),
            FactorDslError::ArityMismatch {
                operator: "+".to_string(),
                expected: 2,
                actual: 1
            }
        );
    }

    #[test]
    fn validate_rejects_excessive_depth_without_recursive_validation() {
        let mut ast = FactorAst::Terminal(FactorTerminal::Field("oi".to_string()));
        for _ in 0..MAX_FACTOR_AST_DEPTH {
            ast = FactorAst::Call {
                operator: FactorOperator::Abs,
                args: vec![ast],
            };
        }

        assert_eq!(
            ast.validate().unwrap_err(),
            FactorDslError::AstTooDeep {
                max_depth: MAX_FACTOR_AST_DEPTH
            }
        );
    }

    #[test]
    fn validate_rejects_excessive_node_count() {
        fn tree(depth: usize) -> FactorAst {
            if depth == 0 {
                return FactorAst::Terminal(FactorTerminal::Field("oi".to_string()));
            }
            FactorAst::Call {
                operator: FactorOperator::Add,
                args: vec![tree(depth - 1), tree(depth - 1)],
            }
        }

        assert_eq!(
            tree(14).validate().unwrap_err(),
            FactorDslError::AstTooLarge {
                max_nodes: MAX_FACTOR_AST_NODES
            }
        );
    }

    #[test]
    fn renders_binary_formula() {
        let left = FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string()));
        let right = FactorAst::Terminal(FactorTerminal::Field("cvd_slope_5m".to_string()));
        let ast = FactorAst::call(FactorOperator::Mul, vec![left, right]).unwrap();
        assert_eq!(ast.to_string(), "(oi_delta_5m * cvd_slope_5m)");
    }

    #[test]
    fn live_capability_accepts_only_runtime_fields_and_stateless_operators() {
        let ast = FactorAst::call(
            FactorOperator::Sub,
            vec![
                FactorAst::Terminal(FactorTerminal::Field("best_ask".to_string())),
                FactorAst::Terminal(FactorTerminal::Field("best_bid".to_string())),
            ],
        )
        .unwrap();

        assert_eq!(
            validate_live_formula(&ast).unwrap(),
            LiveFormulaCapability {
                event_domain: LiveEventDomain::Snapshot,
                history_rows: 1,
            }
        );
    }

    #[test]
    fn live_capability_rejects_research_only_operators_and_features() {
        for operator in [
            FactorOperator::Rank,
            FactorOperator::Mean,
            FactorOperator::Std,
            FactorOperator::Div,
            FactorOperator::Log,
        ] {
            let mut args = vec![FactorAst::Terminal(FactorTerminal::Field(
                "mid_price".to_string(),
            ))];
            if operator.arity() == 2 {
                args.push(FactorAst::Terminal(FactorTerminal::Constant(
                    "5".to_string(),
                )));
            }
            let ast = FactorAst::call(operator.clone(), args).unwrap();
            assert_eq!(
                validate_live_formula(&ast).unwrap_err(),
                LiveFormulaCapabilityError::UnsupportedOperator(operator.symbol().to_string())
            );
        }

        for field in ["book_imbalance_top5", "ofi_top5", "signal"] {
            let ast = FactorAst::Terminal(FactorTerminal::Field(field.to_string()));
            assert_eq!(
                validate_live_formula(&ast).unwrap_err(),
                LiveFormulaCapabilityError::UnsupportedField(field.to_string())
            );
        }
    }

    #[test]
    fn live_capability_accepts_bounded_causal_history() {
        let delta = FactorAst::call(
            FactorOperator::Delta,
            vec![
                FactorAst::Terminal(FactorTerminal::Field("book_imbalance".to_string())),
                FactorAst::Terminal(FactorTerminal::Constant("5".to_string())),
            ],
        )
        .unwrap();
        let ast = FactorAst::call(
            FactorOperator::ZScore,
            vec![
                delta,
                FactorAst::Terminal(FactorTerminal::Constant("20".to_string())),
            ],
        )
        .unwrap();

        assert_eq!(
            validate_live_formula(&ast).unwrap(),
            LiveFormulaCapability {
                event_domain: LiveEventDomain::Snapshot,
                history_rows: 25,
            }
        );
    }

    #[test]
    fn live_capability_rejects_unbounded_rolling_windows() {
        for window in ["0", "1.5", "10001"] {
            let ast = FactorAst::call(
                FactorOperator::ZScore,
                vec![
                    FactorAst::Terminal(FactorTerminal::Field("book_imbalance".to_string())),
                    FactorAst::Terminal(FactorTerminal::Constant(window.to_string())),
                ],
            )
            .unwrap();
            assert_eq!(
                validate_live_formula(&ast).unwrap_err(),
                LiveFormulaCapabilityError::InvalidRollingWindow
            );
        }
    }

    #[test]
    fn live_capability_rejects_expensive_nested_rolling_evaluation() {
        let mut ast = FactorAst::Terminal(FactorTerminal::Field("book_imbalance".to_string()));
        for _ in 0..3 {
            ast = FactorAst::call(
                FactorOperator::ZScore,
                vec![
                    ast,
                    FactorAst::Terminal(FactorTerminal::Constant("3000".to_string())),
                ],
            )
            .unwrap();
        }

        assert_eq!(
            validate_live_formula(&ast).unwrap_err(),
            LiveFormulaCapabilityError::EvaluationTooExpensive {
                max_steps: MAX_LIVE_EVALUATION_STEPS,
            }
        );
    }

    #[test]
    fn shared_live_series_evaluator_preserves_causal_delta_and_zscore() {
        let ast = FactorAst::call(
            FactorOperator::ZScore,
            vec![
                FactorAst::call(
                    FactorOperator::Delta,
                    vec![
                        FactorAst::Terminal(FactorTerminal::Field("book_imbalance".to_string())),
                        FactorAst::Terminal(FactorTerminal::Constant("1".to_string())),
                    ],
                )
                .unwrap(),
                FactorAst::Terminal(FactorTerminal::Constant("2".to_string())),
            ],
        )
        .unwrap();
        let values = [1.0, 2.0, 4.0];

        assert_eq!(
            evaluate_live_formula_series(&ast, values.len(), |row, field| {
                (field == "book_imbalance").then_some(values[row])
            })
            .unwrap(),
            vec![0.0, 1.0, 1.0]
        );
    }
}
