//! Canonical factor DSL and program-factor AST contracts.

use hft_research_manifest::ManifestId;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

pub const MAX_FACTOR_AST_DEPTH: usize = 64;
pub const MAX_FACTOR_AST_NODES: usize = 10_000;

#[derive(Debug, Error, PartialEq, Eq)]
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
}
