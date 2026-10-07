//! Immutable borrowed validation indexes. No signature, key, authority or state.
use crate::{
    MetaCostV1, MetaEvaluationV1, MetaExecutionBindingV1, MetaRunBindingV1, MetaStudyArm,
    MetaStudyV1, MetaTaskV1,
};
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Precomputed identities of one frozen task; this is not a source witness.
pub struct ValidatedMetaTaskV1<'a> {
    definition: &'a MetaTaskV1,
    id: String,
    scoring_rule_sha256: String,
}
impl<'a> ValidatedMetaTaskV1<'a> {
    pub fn definition(&self) -> &'a MetaTaskV1 {
        self.definition
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn scoring_rule_sha256(&self) -> &str {
        &self.scoring_rule_sha256
    }
}

/// Borrowing prevents edits to the whole study while its indexes are reused.
/// Private fields and no Serde prevent restoring this process-local cache.
pub struct ValidatedMetaStudyV1<'a> {
    study: &'a MetaStudyV1,
    id: String,
    tasks: BTreeMap<&'a str, ValidatedMetaTaskV1<'a>>,
    runs: BTreeMap<&'a str, &'a MetaRunBindingV1>,
    pairs: BTreeMap<(&'a str, u64, MetaStudyArm), &'a MetaRunBindingV1>,
}
impl<'a> ValidatedMetaStudyV1<'a> {
    pub fn new(study: &'a MetaStudyV1) -> Result<Self> {
        let bytes = study.validated_encoding()?;
        let id = format!("{:x}", Sha256::digest(&bytes));
        let mut tasks = BTreeMap::new();
        for task in &study.tasks {
            tasks.insert(
                task.task_id.as_str(),
                ValidatedMetaTaskV1 {
                    definition: task,
                    id: crate::content_sha256(task)?,
                    scoring_rule_sha256: crate::content_sha256(&(
                        task.score_direction,
                        &task.scoring_rule,
                    ))?,
                },
            );
        }
        let runs = study
            .runs
            .iter()
            .map(|run| (run.run_sha256.as_str(), run))
            .collect();
        let pairs = study
            .runs
            .iter()
            .map(|run| ((run.task_id.as_str(), run.seed, run.arm), run))
            .collect();
        Ok(Self {
            study,
            id,
            tasks,
            runs,
            pairs,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn study(&self) -> &'a MetaStudyV1 {
        self.study
    }
    pub fn task(&self, id: &str) -> Option<&ValidatedMetaTaskV1<'a>> {
        self.tasks.get(id)
    }
    pub fn run_for(
        &self,
        task_id: &str,
        seed: u64,
        arm: MetaStudyArm,
    ) -> Option<&'a MetaRunBindingV1> {
        self.pairs.get(&(task_id, seed, arm)).copied()
    }
    pub fn validate_execution(&self, binding: &MetaExecutionBindingV1) -> Result<()> {
        binding.validate()?;
        ensure!(
            binding.study_sha256 == self.id,
            "execution Study identity differs from frozen study"
        );
        let run = self
            .runs
            .get(binding.run.run_sha256.as_str())
            .context("execution Run is absent from frozen matrix")?;
        ensure!(
            **run == binding.run,
            "execution Run binding differs from frozen matrix"
        );
        let task = self
            .task(&binding.run.task_id)
            .context("execution task is absent")?;
        ensure!(
            task.id() == binding.task_sha256,
            "execution task content differs from frozen task"
        );
        ensure!(
            binding.attempt <= self.study.per_arm_budget.max_job_attempts,
            "execution Attempt exceeds declared arm limit"
        );
        Ok(())
    }
    pub fn validate_evaluation(&self, payload: &MetaEvaluationV1) -> Result<()> {
        payload.validate()?;
        self.validate_execution(&payload.binding)?;
        let cached = self
            .task(&payload.binding.run.task_id)
            .context("evaluation task is absent")?;
        let task = cached.definition();
        ensure!(
            payload.data_view_sha256 == task.data_view_sha256
                && payload.visibility == task.visibility
                && payload.evaluator_code_sha256 == task.evaluator_code_sha256
                && payload.scoring_rule_sha256 == cached.scoring_rule_sha256(),
            "evaluation input/visibility/evaluator/scoring differs from frozen task"
        );
        ensure!(
            payload.grant_sha256 == self.study.grant_sha256
                && payload.budget_scope_sha256 == self.study.budget_scope_sha256
                && payload.resources == self.study.resources,
            "evaluation grant/budget/resources differ between frozen arms"
        );
        Ok(())
    }
    pub fn validate_cost(&self, payload: &MetaCostV1) -> Result<()> {
        payload.validate()?;
        self.validate_execution(&payload.binding)?;
        ensure!(
            payload.grant_sha256 == self.study.grant_sha256
                && payload.budget_scope_sha256 == self.study.budget_scope_sha256
                && payload.resources == self.study.resources,
            "cost grant/budget/resources differ from frozen study"
        );
        ensure!(
            payload.usage.trials <= self.study.per_arm_budget.max_trials
                && payload.usage.llm_tokens <= self.study.per_arm_budget.max_llm_tokens
                && payload.usage.cost_microusd <= self.study.per_arm_budget.max_cost_microusd,
            "individual cost receipt exceeds frozen arm limits"
        );
        Ok(())
    }
}
