use alpha_domain::{
    AttributionKind, AttributionOutcome, IterationVerdict, LearningDirective, MissionStatus,
    ResearchMission,
};
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
    pub runtime_event_ids: Vec<String>,
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
    pub pinned_policy_revision_ids: Vec<String>,
    pub critic_failure_count: usize,
}

#[derive(Default)]
struct FailureEvidence {
    iteration_ids: Vec<String>,
    runtime_event_ids: Vec<String>,
    failure_explanations: Vec<String>,
    observed_at: Option<chrono::DateTime<Utc>>,
}

impl FailureEvidence {
    fn count(&self) -> usize {
        self.iteration_ids.len() + self.runtime_event_ids.len()
    }

    fn all_ids(&self) -> Vec<String> {
        self.iteration_ids
            .iter()
            .chain(&self.runtime_event_ids)
            .cloned()
            .collect()
    }
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
    let mut failures = BTreeMap::<String, FailureEvidence>::new();
    for iteration in &lineage.iterations {
        let failure_class = match iteration.verdict {
            IterationVerdict::Keep => continue,
            IterationVerdict::Discard => "validation_failure".to_string(),
            IterationVerdict::Crash => iteration
                .failure_class
                .clone()
                .unwrap_or_else(|| "unclassified_crash".to_string()),
        };
        let evidence = failures.entry(failure_class).or_default();
        evidence.iteration_ids.push(iteration.iteration_id.clone());
        if let Some(explanation) = &iteration.failure_explanation {
            evidence.failure_explanations.push(explanation.clone());
        }
        evidence.observed_at = Some(
            evidence
                .observed_at
                .map_or(iteration.created_at, |at| at.max(iteration.created_at)),
        );
    }
    for event in store.runtime_attributions_for_mission(mission_id)? {
        let Some(failure_class) = runtime_failure_class(&event) else {
            continue;
        };
        let evidence = failures.entry(failure_class).or_default();
        evidence.runtime_event_ids.push(event.event_id);
        if let Some(reason) = event.reason {
            evidence.failure_explanations.push(reason);
        }
        evidence.observed_at = Some(
            evidence
                .observed_at
                .map_or(event.observed_at, |at| at.max(event.observed_at)),
        );
    }

    let mut outcome = LearningOutcome {
        follow_up_mission_ids: vec![],
        directive_ids: vec![],
        pinned_policy_revision_ids: vec![],
        critic_failure_count: 0,
    };
    for (failure_class, evidence) in failures {
        if evidence.count() < config.repeated_failure_threshold {
            continue;
        }
        let suffix = hex::encode(Sha256::digest(failure_class.as_bytes()));
        let suffix = &suffix[..12];
        let follow_up_id = format!("{mission_id}-followup-{suffix}");
        let directive_id = format!("learning:{mission_id}:{suffix}");
        let now = evidence.observed_at.unwrap_or_else(Utc::now);
        let all_evidence_ids = evidence.all_ids();
        let pinned_policy = store.find_adopted_search_policy_child(
            &lineage.mission.search_policy_snapshot_id,
            &all_evidence_ids,
        )?;
        let pinned_policy_id = pinned_policy
            .as_ref()
            .map(|revision| revision.revision_id.clone())
            .unwrap_or_else(|| lineage.mission.search_policy_snapshot_id.clone());
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
            completion_policy: lineage.mission.completion_policy.clone(),
            prompt_snapshot_id: lineage.mission.prompt_snapshot_id.clone(),
            search_policy_snapshot_id: pinned_policy_id.clone(),
            status: MissionStatus::Pending,
            terminal_reason: None,
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
            iteration_ids: evidence.iteration_ids.clone(),
            runtime_event_ids: evidence.runtime_event_ids.clone(),
            failure_explanations: evidence.failure_explanations,
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
            runtime_evidence_event_ids: context.runtime_event_ids,
            follow_up_mission_id: follow_up_id.clone(),
            search_policy_revision_id: pinned_policy_id.clone(),
            created_at: now,
        };
        store.append_learning_directive(&directive)?;
        outcome.follow_up_mission_ids.push(follow_up_id);
        outcome.directive_ids.push(directive_id);
        if pinned_policy.is_some() {
            outcome.pinned_policy_revision_ids.push(pinned_policy_id);
        }
    }
    outcome.pinned_policy_revision_ids.sort();
    outcome.pinned_policy_revision_ids.dedup();
    Ok(outcome)
}

fn runtime_failure_class(event: &alpha_domain::RuntimeAttributionEvent) -> Option<String> {
    match event.kind {
        AttributionKind::Reject => Some("runtime_reject".to_string()),
        AttributionKind::StreamGap => Some("runtime_stream_gap".to_string()),
        _ => match event.outcome {
            AttributionOutcome::Decayed => Some("runtime_decay".to_string()),
            AttributionOutcome::RolledBack => Some("runtime_rollback".to_string()),
            AttributionOutcome::Failed => Some("runtime_failure".to_string()),
            AttributionOutcome::Activated | AttributionOutcome::Healthy => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::{
        AttributionKind, AttributionMode, AttributionOutcome, EngineKind, MissionCompletionPolicy,
        MissionTerminalReason, ResearchIteration, RuntimeAttributionEvent, SearchBudget,
        SearchBudgetLimit, SearchBudgetUsage, SearchPolicyRevision, ValidatorMode,
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
            completion_policy: MissionCompletionPolicy::default(),
            prompt_snapshot_id: None,
            search_policy_snapshot_id: "policy-1".to_string(),
            status: MissionStatus::Pending,
            terminal_reason: None,
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
            .finish_mission(
                "mission-learning",
                MissionTerminalReason::SearchBudgetExhausted {
                    exhausted_limits: vec![SearchBudgetLimit::Candidates],
                },
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

    #[test]
    fn runtime_failures_pin_an_adopted_validator_gated_policy_into_follow_up() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        store
            .put_search_policy_revision(SearchPolicyRevision {
                revision_id: "policy-1".to_string(),
                parent_revision_id: None,
                policy: serde_json::json!({"engine": "gp"}),
                evidence_event_ids: vec![],
                validator_score: 1.0,
                adopted: false,
                rollback_reason: None,
                created_at: Utc::now(),
            })
            .unwrap();
        let event_ids = (1..=3)
            .map(|index| format!("runtime-reject-{index}"))
            .collect::<Vec<_>>();
        let adopted = store
            .put_search_policy_revision(SearchPolicyRevision {
                revision_id: "policy-2".to_string(),
                parent_revision_id: Some("policy-1".to_string()),
                policy: serde_json::json!({"engine": "mcts", "avoid": "runtime_reject"}),
                evidence_event_ids: event_ids.clone(),
                validator_score: 1.1,
                adopted: false,
                rollback_reason: None,
                created_at: Utc::now(),
            })
            .unwrap();
        assert!(adopted.adopted);
        for event_id in &event_ids {
            let event = RuntimeAttributionEvent {
                event_id: event_id.clone(),
                deployment_id: "deployment-1".to_string(),
                asset_revision_id: "candidate-1".to_string(),
                mission_id: Some("mission-learning".to_string()),
                mode: AttributionMode::Paper,
                outcome: AttributionOutcome::Failed,
                kind: AttributionKind::Reject,
                strategy_id: Some("strategy-1".to_string()),
                order_id: Some(format!("order-{event_id}")),
                account_id: Some("account-1".to_string()),
                venue: Some("binance".to_string()),
                symbol: Some("BTCUSDT".to_string()),
                metrics: BTreeMap::new(),
                reason: Some("exchange reject".to_string()),
                observed_at: Utc::now(),
            };
            store
                .append_memory(&MemoryRecord {
                    event_id: event_id.clone(),
                    mission_id: Some("mission-learning".to_string()),
                    payload: serde_json::json!({
                        "kind": "runtime_attribution",
                        "event": event,
                    }),
                    created_at: Utc::now(),
                })
                .unwrap();
        }
        store
            .transition_mission("mission-learning", MissionStatus::Running, Utc::now())
            .unwrap();
        store
            .finish_mission(
                "mission-learning",
                MissionTerminalReason::SearchBudgetExhausted {
                    exhausted_limits: vec![SearchBudgetLimit::Candidates],
                },
                Utc::now(),
            )
            .unwrap();

        let outcome = close_learning_loop(
            &mut store,
            "mission-learning",
            &LearningConfig::default(),
            None,
        )
        .unwrap();
        assert_eq!(outcome.pinned_policy_revision_ids, vec!["policy-2"]);
        let follow_up = store
            .get_mission(&outcome.follow_up_mission_ids[0])
            .unwrap();
        assert_eq!(follow_up.search_policy_snapshot_id, "policy-2");
        let directive = store.get_memory(&outcome.directive_ids[0]).unwrap();
        let directive: LearningDirective =
            serde_json::from_value(directive.payload["directive"].clone()).unwrap();
        assert_eq!(directive.runtime_evidence_event_ids, event_ids);
    }
}
