use crate::{evaluation::ProposalContext, EngineProposal, ProposalEngine, RemainingBudget};
use alpha_domain::{CandidateArtifact, EngineKind};
use hft_factor_dsl::{FactorAst, FactorOperator, FactorTerminal};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfflineTrace {
    pub state: String,
    pub action: String,
    pub reward: f64,
    pub next_state: String,
    pub terminal: bool,
}

pub struct OfflineRlEngine {
    field: String,
    policy_version: String,
    q_values: BTreeMap<(String, String), f64>,
    actions: BTreeSet<String>,
}

impl OfflineRlEngine {
    pub fn train(
        field: impl Into<String>,
        policy_version: impl Into<String>,
        traces: &[OfflineTrace],
        min_trace_count: usize,
        learning_rate: f64,
        discount: f64,
        epochs: usize,
    ) -> Result<Self, String> {
        let field = field.into();
        let policy_version = policy_version.into();
        if field.trim().is_empty()
            || policy_version.trim().is_empty()
            || traces.len() < min_trace_count
            || min_trace_count == 0
            || epochs == 0
            || !learning_rate.is_finite()
            || !(0.0..=1.0).contains(&learning_rate)
            || !discount.is_finite()
            || !(0.0..=1.0).contains(&discount)
            || traces.iter().any(|trace| {
                trace.state.trim().is_empty()
                    || trace.action.trim().is_empty()
                    || !matches!(trace.action.as_str(), "rank" | "delta" | "mean" | "zscore")
                    || trace.next_state.trim().is_empty()
                    || !trace.reward.is_finite()
            })
        {
            return Err("offline RL training data or configuration is invalid".to_string());
        }
        let actions = traces
            .iter()
            .map(|trace| trace.action.clone())
            .collect::<BTreeSet<_>>();
        let mut q_values = BTreeMap::new();
        for _ in 0..epochs {
            for trace in traces {
                let key = (trace.state.clone(), trace.action.clone());
                let current = *q_values.get(&key).unwrap_or(&0.0);
                let next_best = if trace.terminal {
                    0.0
                } else {
                    actions
                        .iter()
                        .map(|action| {
                            *q_values
                                .get(&(trace.next_state.clone(), action.clone()))
                                .unwrap_or(&0.0)
                        })
                        .max_by(f64::total_cmp)
                        .unwrap_or(0.0)
                };
                let target = trace.reward + discount * next_best;
                q_values.insert(key, current + learning_rate * (target - current));
            }
        }
        Ok(Self {
            field,
            policy_version,
            q_values,
            actions,
        })
    }

    pub fn q_value(&self, state: &str, action: &str) -> f64 {
        *self
            .q_values
            .get(&(state.to_string(), action.to_string()))
            .unwrap_or(&0.0)
    }

    fn action_for_state(&self, state: &str) -> Result<&str, String> {
        self.actions
            .iter()
            .max_by(|left, right| {
                self.q_value(state, left)
                    .total_cmp(&self.q_value(state, right))
                    .then_with(|| right.cmp(left))
            })
            .map(String::as_str)
            .ok_or_else(|| "offline RL policy has no actions".to_string())
    }

    fn ast_for_action(&self, action: &str) -> Result<FactorAst, String> {
        let field = FactorAst::Terminal(FactorTerminal::Field(self.field.clone()));
        let window = |value: &str| FactorAst::Terminal(FactorTerminal::Constant(value.to_string()));
        match action {
            "rank" => FactorAst::call(FactorOperator::Rank, vec![field]),
            "delta" => FactorAst::call(FactorOperator::Delta, vec![field, window("5")]),
            "mean" => FactorAst::call(FactorOperator::Mean, vec![field, window("20")]),
            "zscore" => FactorAst::call(FactorOperator::ZScore, vec![field, window("60")]),
            other => {
                return Err(format!(
                    "offline RL action is not in the factor grammar: {other}"
                ))
            }
        }
        .map_err(|error| error.to_string())
    }
}

impl ProposalEngine for OfflineRlEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::OfflineReinforcementLearning
    }

    fn propose(
        &mut self,
        mission_id: &str,
        iteration_index: usize,
        context: &ProposalContext<'_>,
        _remaining: &RemainingBudget,
    ) -> Result<EngineProposal, String> {
        let signal = context
            .latest_signal()
            .ok_or_else(|| "offline RL requires research rows".to_string())?;
        let state = if signal > 0.0 {
            "positive"
        } else if signal < 0.0 {
            "negative"
        } else {
            "flat"
        };
        let action = self.action_for_state(state)?;
        Ok(EngineProposal {
            candidate_id: format!("{mission_id}-offline-rl-{iteration_index}"),
            hypothesis: format!(
                "lab-only offline Q-learning search policy {} selected {} for {} state; no allocator or runtime authority",
                self.policy_version, action, state
            ),
            artifact: CandidateArtifact::Formula(self.ast_for_action(action)?),
            expansions: 0,
            tokens: 0,
            elapsed_ms: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn traces() -> Vec<OfflineTrace> {
        vec![
            OfflineTrace {
                state: "positive".to_string(),
                action: "rank".to_string(),
                reward: 1.0,
                next_state: "positive".to_string(),
                terminal: false,
            },
            OfflineTrace {
                state: "positive".to_string(),
                action: "mean".to_string(),
                reward: -1.0,
                next_state: "flat".to_string(),
                terminal: true,
            },
            OfflineTrace {
                state: "flat".to_string(),
                action: "rank".to_string(),
                reward: 0.2,
                next_state: "positive".to_string(),
                terminal: false,
            },
        ]
    }

    #[test]
    fn refuses_insufficient_trace_history() {
        assert!(OfflineRlEngine::train("oi", "policy-1", &traces(), 4, 0.2, 0.9, 10).is_err());
    }

    #[test]
    fn offline_q_learning_prefers_higher_reward_action() {
        let engine = OfflineRlEngine::train("oi", "policy-1", &traces(), 3, 0.2, 0.9, 20).unwrap();
        assert!(engine.q_value("positive", "rank") > engine.q_value("positive", "mean"));
    }

    #[test]
    fn proposals_are_explicitly_labeled_lab_search_only() {
        let mut engine =
            OfflineRlEngine::train("signal", "policy-1", &traces(), 3, 0.2, 0.9, 20).unwrap();
        let proposal = engine
            .propose(
                "mission",
                0,
                &super::super::test_dataset().proposal_context(),
                &RemainingBudget {
                    candidates: 1,
                    expansions: 0,
                    tokens: 0,
                    milliseconds: 0,
                },
            )
            .unwrap();

        assert!(proposal.hypothesis.contains("lab-only"));
        assert!(proposal
            .hypothesis
            .contains("no allocator or runtime authority"));
    }
}
