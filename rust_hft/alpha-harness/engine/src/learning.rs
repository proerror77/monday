use alpha_domain::{IterationVerdict, LearningDirective, MissionStatus, ResearchMission};
use alpha_store::{AlphaStore, MemoryRecord, StoreError};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LearningError {
    #[error("control-plane store failed: {0}")]
    Store(#[from] StoreError),
    #[error("learning requires a terminal research mission")]
    MissionNotTerminal,
    #[error("learning configuration is invalid")]
    InvalidConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearningConfig {
    pub repeated_failure_threshold: usize,
    pub max_critic_tokens: u64,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            repeated_failure_threshold: 3,
            max_critic_tokens: 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureContext {
    pub mission_id: String,
    pub failure_class: String,
    pub iteration_ids: Vec<String>,
    pub failure_explanations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureCritique {
    pub payload: serde_json::Value,
    pub tokens: u64,
}

pub trait FailureCritic {
    fn explain(&self, context: &FailureContext, max_tokens: u64)
        -> Result<FailureCritique, String>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearningOutcome {
    pub follow_up_mission_ids: Vec<String>,
    pub directive_ids: Vec<String>,
    pub critic_failure_count: usize,
}

pub fn close_learning_loop(
    store: &mut AlphaStore,
    mission_id: &str,
    config: &LearningConfig,
    critic: Option<&dyn FailureCritic>,
) -> Result<LearningOutcome, LearningError> {
    if config.repeated_failure_threshold == 0 || config.max_critic_tokens == 0 {
        return Err(LearningError::InvalidConfiguration);
    }
    let lineage = store.mission_lineage(mission_id)?;
    if !matches!(
        lineage.mission.status,
        MissionStatus::Completed | MissionStatus::BudgetExhausted | MissionStatus::Failed
    ) {
        return Err(LearningError::MissionNotTerminal);
    }
    let mut failures = BTreeMap::<String, Vec<_>>::new();
    for iteration in &lineage.iterations {
        let failure_class = match iteration.verdict {
            IterationVerdict::Keep => continue,
            IterationVerdict::Discard => "validation_failure".to_string(),
            IterationVerdict::Crash => iteration
                .failure_class
                .clone()
                .unwrap_or_else(|| "unclassified_crash".to_string()),
        };
        failures.entry(failure_class).or_default().push(iteration);
    }

    let mut outcome = LearningOutcome {
        follow_up_mission_ids: vec![],
        directive_ids: vec![],
        critic_failure_count: 0,
    };
    for (failure_class, iterations) in failures {
        if iterations.len() < config.repeated_failure_threshold {
            continue;
        }
        let suffix = hex::encode(Sha256::digest(failure_class.as_bytes()));
        let suffix = &suffix[..12];
        let follow_up_id = format!("{mission_id}-followup-{suffix}");
        let directive_id = format!("learning:{mission_id}:{suffix}");
        let now = iterations
            .last()
            .map(|iteration| iteration.created_at)
            .unwrap_or_else(Utc::now);
        let follow_up = ResearchMission {
            mission_id: follow_up_id.clone(),
            objective: format!(
                "Explain and reduce repeated {failure_class} failures from mission {mission_id}"
            ),
            hypothesis_scope: format!(
                "{}; failure class: {failure_class}",
                lineage.mission.hypothesis_scope
            ),
            mutable_scope: lineage.mission.mutable_scope.clone(),
            dataset_manifest_id: lineage.mission.dataset_manifest_id.clone(),
            baseline_artifact_id: lineage.mission.baseline_artifact_id.clone(),
            validation_mode: lineage.mission.validation_mode.clone(),
            validator_spec: lineage.mission.validator_spec.clone(),
            search_budget: lineage.mission.search_budget.clone(),
            prompt_snapshot_id: lineage.mission.prompt_snapshot_id.clone(),
            search_policy_snapshot_id: lineage.mission.search_policy_snapshot_id.clone(),
            status: MissionStatus::Pending,
            created_at: now,
            updated_at: now,
        };
        match store.get_mission(&follow_up_id) {
            Ok(_) => {}
            Err(StoreError::NotFound) => store.create_mission(&follow_up)?,
            Err(error) => return Err(error.into()),
        }

        let context = FailureContext {
            mission_id: mission_id.to_string(),
            failure_class: failure_class.clone(),
            iteration_ids: iterations
                .iter()
                .map(|iteration| iteration.iteration_id.clone())
                .collect(),
            failure_explanations: iterations
                .iter()
                .filter_map(|iteration| iteration.failure_explanation.clone())
                .collect(),
        };
        let critique_id = format!("critic:{directive_id}");
        if let Some(critic) = critic {
            let critique_missing = match store.get_memory(&critique_id) {
                Ok(_) => false,
                Err(StoreError::NotFound) => true,
                Err(error) => return Err(error.into()),
            };
            if critique_missing {
                match critic.explain(&context, config.max_critic_tokens) {
                    Ok(critique) if critique.tokens <= config.max_critic_tokens => {
                        store.append_memory_idempotent(&MemoryRecord {
                            event_id: critique_id,
                            mission_id: Some(mission_id.to_string()),
                            payload: serde_json::json!({
                                "kind": "failure_critique",
                                "context": context.clone(),
                                "critique": critique,
                            }),
                            created_at: now,
                        })?;
                    }
                    Ok(_) | Err(_) => outcome.critic_failure_count += 1,
                }
            }
        }

        let directive = LearningDirective {
            directive_id: directive_id.clone(),
            mission_id: mission_id.to_string(),
            failure_class,
            evidence_iteration_ids: context.iteration_ids,
            follow_up_mission_id: follow_up_id.clone(),
            search_policy_revision_id: lineage.mission.search_policy_snapshot_id.clone(),
            created_at: now,
        };
        store.append_learning_directive(&directive)?;
        outcome.follow_up_mission_ids.push(follow_up_id);
        outcome.directive_ids.push(directive_id);
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::{
        EngineKind, ResearchIteration, SearchBudget, SearchBudgetUsage, ValidatorMode,
    };
    use chrono::Utc;
    use hft_research_manifest::ManifestId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingCritic(AtomicUsize);

    impl FailureCritic for CountingCritic {
        fn explain(
            &self,
            context: &FailureContext,
            max_tokens: u64,
        ) -> Result<FailureCritique, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(FailureCritique {
                payload: serde_json::json!({
                    "failure_class": context.failure_class,
                    "next_experiment": "reduce turnover",
                }),
                tokens: max_tokens,
            })
        }
    }

    fn mission() -> ResearchMission {
        let now = Utc::now();
        ResearchMission {
            mission_id: "mission-learning".to_string(),
            objective: "find stable factor".to_string(),
            hypothesis_scope: "causal signal".to_string(),
            mutable_scope: vec!["factor_ast".to_string()],
            dataset_manifest_id: ManifestId::new("dataset-1").unwrap(),
            baseline_artifact_id: None,
            validation_mode: ValidatorMode::MissionValidator,
            validator_spec: serde_json::json!({"score": "net"}),
            search_budget: SearchBudget {
                max_candidates: 3,
                max_expansions: 10,
                max_tokens: 0,
                max_seconds: 30,
            },
            prompt_snapshot_id: None,
            search_policy_snapshot_id: "policy-1".to_string(),
            status: MissionStatus::Pending,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn repeated_failures_create_one_idempotent_follow_up_mission() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        store
            .transition_mission("mission-learning", MissionStatus::Running, Utc::now())
            .unwrap();
        for index in 1..=3 {
            store
                .append_iteration(
                    &ResearchIteration {
                        iteration_id: format!("iteration-{index}"),
                        mission_id: "mission-learning".to_string(),
                        parent_candidate_ids: vec![],
                        engine: EngineKind::GeneticProgramming,
                        hypothesis: "failed hypothesis".to_string(),
                        candidate_artifact_id: None,
                        evaluation_artifact_id: None,
                        budget_usage: SearchBudgetUsage {
                            candidates: index,
                            expansions: index as u64,
                            tokens: 0,
                            elapsed_ms: index as u64,
                        },
                        verdict: IterationVerdict::Discard,
                        failure_class: None,
                        failure_explanation: Some("negative net return".to_string()),
                        created_at: Utc::now(),
                    },
                    None,
                    None,
                )
                .unwrap();
        }
        store
            .transition_mission(
                "mission-learning",
                MissionStatus::BudgetExhausted,
                Utc::now(),
            )
            .unwrap();
        let critic = CountingCritic(AtomicUsize::new(0));
        let first = close_learning_loop(
            &mut store,
            "mission-learning",
            &LearningConfig::default(),
            Some(&critic),
        )
        .unwrap();
        let second = close_learning_loop(
            &mut store,
            "mission-learning",
            &LearningConfig::default(),
            Some(&critic),
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.follow_up_mission_ids.len(), 1);
        assert!(store.get_mission(&first.follow_up_mission_ids[0]).is_ok());
        assert!(store.get_memory(&first.directive_ids[0]).is_ok());
        assert!(store
            .get_memory(&format!("critic:{}", first.directive_ids[0]))
            .is_ok());
        assert_eq!(critic.0.load(Ordering::SeqCst), 1);
    }
}
