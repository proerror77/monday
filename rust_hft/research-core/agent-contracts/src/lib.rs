//! Pure declarations for a bounded researcher configuration comparison.
//! JSON, content hashes and signature envelopes do not establish authority.
//! Control owns trusted keys, original readback, ledger state and adoption.

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const CONTRACT_SCHEMA_V1: u32 = 1;
pub const META_EVALUATION_SIGNING_DOMAIN: &str = "monday.research_agent.meta_evaluation.v1";
pub const META_COST_SIGNING_DOMAIN: &str = "monday.research_agent.meta_cost.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperienceRankingOrder {
    TaskMatchThenRecency,
    RecencyThenTaskMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaTaskPhase {
    Development,
    Selection,
    Certification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaStudyArm {
    Incumbent,
    Challenger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaScoreDirectionV1 {
    HigherIsBetter,
    LowerIsBetter,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentReferenceV1 {
    pub id: String,
    pub content_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceCorpusReferenceV1 {
    pub content: ContentReferenceV1,
    pub feedback_phase: MetaTaskPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InformationPolicyV1 {
    pub schema: u32,
    pub proposal_feedback_phases: Vec<MetaTaskPhase>,
    pub allowed_data_view_sha256: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceRetrievalV1 {
    pub ranking: ExperienceRankingOrder,
    pub corpus: Vec<ExperienceCorpusReferenceV1>,
    pub query_template: String,
    pub top_k: u32,
    pub max_context_bytes: u64,
    pub information_policy: InformationPolicyV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearcherSnapshotV1 {
    pub prompt_text: String,
    pub search_policy: Value,
    pub retrieval: ExperienceRetrievalV1,
    pub source_commit: String,
    pub build_sha256: String,
    pub tools: Vec<ContentReferenceV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearcherChangeEvidenceV1 {
    pub content: ContentReferenceV1,
    pub phase: MetaTaskPhase,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearcherVersionV1 {
    pub schema: u32,
    pub parent_version_sha256: Option<String>,
    pub change_evidence: Option<ResearcherChangeEvidenceV1>,
    pub snapshot: ResearcherSnapshotV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankingChangeProposalV1 {
    pub schema: u32,
    pub incumbent_version_sha256: String,
    pub challenger: ResearcherVersionV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaResourcesV1 {
    pub cpu_millis: u32,
    pub memory_mib: u32,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaBudgetLimitsV1 {
    pub max_trials: u64,
    pub max_job_attempts: u32,
    pub max_llm_tokens: u64,
    pub max_cost_microusd: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaTaskV1 {
    pub task_id: String,
    pub phase: MetaTaskPhase,
    pub data_view_sha256: String,
    pub visibility: MetaTaskPhase,
    pub evaluator_code_sha256: String,
    pub scoring_rule: Value,
    pub score_direction: MetaScoreDirectionV1,
    pub seeds: Vec<u64>,
}

/// The Run identity is frozen before dispatch. Attempt/fence remain execution
/// facts and are added only to receipts, avoiding circular Study identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaRunBindingV1 {
    pub task_id: String,
    pub phase: MetaTaskPhase,
    pub arm: MetaStudyArm,
    pub version_sha256: String,
    pub run_sha256: String,
    pub seed: u64,
}

/// For each phase, compare matching (task, seed) pairs with equal weight and
/// average gains normalized by each task's frozen scoring direction. Both
/// fixed thresholds must pass.
/// Development feedback never contributes to this promotion comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaPromotionPolicyV1 {
    pub min_selection_score_gain: f64,
    pub min_certification_score_gain: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaStudyV1 {
    pub schema: u32,
    pub incumbent_version_sha256: String,
    pub challenger_version_sha256: String,
    pub tasks: Vec<MetaTaskV1>,
    pub runs: Vec<MetaRunBindingV1>,
    pub grant_sha256: String,
    pub budget_scope_sha256: String,
    pub resources: MetaResourcesV1,
    pub per_arm_budget: MetaBudgetLimitsV1,
    pub promotion_policy: MetaPromotionPolicyV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaExecutionBindingV1 {
    pub study_sha256: String,
    pub run: MetaRunBindingV1,
    pub task_sha256: String,
    pub attempt: u32,
    pub fence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaArtifactReferenceV1 {
    pub key: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaEvaluationOutcomeV1 {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaEvaluationV1 {
    pub schema: u32,
    pub binding: MetaExecutionBindingV1,
    pub data_view_sha256: String,
    pub visibility: MetaTaskPhase,
    pub evaluator_code_sha256: String,
    pub scoring_rule_sha256: String,
    pub grant_sha256: String,
    pub budget_scope_sha256: String,
    pub resources: MetaResourcesV1,
    pub outcome: MetaEvaluationOutcomeV1,
    pub score: Option<f64>,
    pub failure_code: Option<String>,
    pub result_artifact: MetaArtifactReferenceV1,
    pub result_readback_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaCostUsageV1 {
    pub trials: u64,
    pub job_attempts: u32,
    pub wall_ms: u64,
    pub llm_tokens: u64,
    pub cost_microusd: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaCostV1 {
    pub schema: u32,
    pub binding: MetaExecutionBindingV1,
    pub grant_sha256: String,
    pub budget_scope_sha256: String,
    pub resources: MetaResourcesV1,
    pub usage: MetaCostUsageV1,
    pub evaluated_result_sha256: String,
    pub cost_artifact: MetaArtifactReferenceV1,
    pub cost_readback_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedMetaEvaluationV1 {
    pub payload: MetaEvaluationV1,
    pub key_id: String,
    pub signature_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedMetaCostV1 {
    pub payload: MetaCostV1,
    pub key_id: String,
    pub signature_hex: String,
}

/// One bounded, deterministic encoding independent of serde_json map features.
pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(fields) => {
                let mut entries = fields.into_iter().collect::<Vec<_>>();
                entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                Value::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
            }
            Value::Array(values) => Value::Array(values.into_iter().map(sorted).collect()),
            scalar => scalar,
        }
    }
    let bytes = serde_json::to_vec(&sorted(serde_json::to_value(value)?))?;
    ensure!(
        bytes.len() <= 2 * 1024 * 1024,
        "agent contract exceeds bounded encoding"
    );
    Ok(bytes)
}

pub fn content_sha256<T: Serialize>(value: &T) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(canonical_bytes(value)?)))
}

impl SignedMetaEvaluationV1 {
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        self.payload.validate()?;
        valid_name(&self.key_id, "evaluation signing key id")?;
        canonical_bytes(&(META_EVALUATION_SIGNING_DOMAIN, &self.key_id, &self.payload))
    }

    pub fn validate(&self) -> Result<()> {
        self.signing_bytes()?;
        ensure!(
            lower_hex(&self.signature_hex, 128),
            "invalid evaluation signature encoding"
        );
        Ok(())
    }
}

impl SignedMetaCostV1 {
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        self.payload.validate()?;
        valid_name(&self.key_id, "cost signing key id")?;
        canonical_bytes(&(META_COST_SIGNING_DOMAIN, &self.key_id, &self.payload))
    }

    pub fn validate(&self) -> Result<()> {
        self.signing_bytes()?;
        ensure!(
            lower_hex(&self.signature_hex, 128),
            "invalid cost signature encoding"
        );
        Ok(())
    }
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn digest(value: &str, field: &str) -> Result<()> {
    ensure!(lower_hex(value, 64), "invalid {field} SHA-256");
    Ok(())
}

fn valid_name(value: &str, field: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control),
        "invalid {field}"
    );
    Ok(())
}

fn document(value: &Value, field: &str) -> Result<()> {
    ensure!(
        value.is_object(),
        "{field} must contain its actual object content"
    );
    let mut stack = vec![(value, 0usize)];
    let mut nodes = 0usize;
    while let Some((node, depth)) = stack.pop() {
        nodes += 1;
        ensure!(
            depth <= 32 && nodes <= 8192,
            "{field} exceeds structured content limits"
        );
        match node {
            Value::Object(fields) => stack.extend(fields.values().map(|v| (v, depth + 1))),
            Value::Array(values) => stack.extend(values.iter().map(|v| (v, depth + 1))),
            _ => {}
        }
    }
    ensure!(
        serde_json::to_vec(value)?.len() <= 65536,
        "{field} content exceeds byte limit"
    );
    Ok(())
}

impl ContentReferenceV1 {
    pub fn validate(&self) -> Result<()> {
        valid_name(&self.id, "content reference id")?;
        digest(&self.content_sha256, "content reference")
    }
}

impl InformationPolicyV1 {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == CONTRACT_SCHEMA_V1,
            "unsupported information policy schema"
        );
        ensure!(
            self.proposal_feedback_phases == [MetaTaskPhase::Development],
            "proposal feedback must exclude selection and certification"
        );
        ensure!(
            !self.allowed_data_view_sha256.is_empty() && self.allowed_data_view_sha256.len() <= 256,
            "unbounded or absent information scope"
        );
        let mut unique = BTreeSet::new();
        for view in &self.allowed_data_view_sha256 {
            digest(view, "information data view")?;
            ensure!(unique.insert(view), "duplicate information data view");
        }
        Ok(())
    }
}

impl ExperienceRetrievalV1 {
    pub fn validate(&self) -> Result<()> {
        self.information_policy.validate()?;
        ensure!(
            !self.corpus.is_empty() && self.corpus.len() <= 256,
            "unbounded or absent retrieval corpus"
        );
        ensure!(
            self.top_k > 0
                && self.top_k <= 256
                && self.max_context_bytes > 0
                && self.max_context_bytes <= 1024 * 1024,
            "invalid retrieval query/context limits"
        );
        ensure!(
            !self.query_template.trim().is_empty() && self.query_template.len() <= 16384,
            "invalid actual retrieval query template"
        );
        let mut unique = BTreeSet::new();
        for corpus in &self.corpus {
            corpus.content.validate()?;
            ensure!(
                corpus.feedback_phase == MetaTaskPhase::Development,
                "retrieval corpus cannot expose selection or certification feedback"
            );
            ensure!(
                unique.insert(&corpus.content.id),
                "duplicate retrieval corpus identity"
            );
        }
        Ok(())
    }
}

impl ResearcherSnapshotV1 {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.prompt_text.trim().is_empty() && self.prompt_text.len() <= 65536,
            "invalid actual researcher prompt"
        );
        document(&self.search_policy, "search policy")?;
        self.retrieval.validate()?;
        ensure!(
            lower_hex(&self.source_commit, 40),
            "researcher source must be a Git revision, not a data digest"
        );
        digest(&self.build_sha256, "researcher Build")?;
        ensure!(
            !self.tools.is_empty() && self.tools.len() <= 64,
            "unbounded or absent tool identities"
        );
        let mut unique = BTreeSet::new();
        for tool in &self.tools {
            tool.validate()?;
            ensure!(
                unique.insert(&tool.id),
                "duplicate researcher tool identity"
            );
        }
        Ok(())
    }
}

impl ResearcherVersionV1 {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == CONTRACT_SCHEMA_V1,
            "unsupported researcher version schema"
        );
        self.snapshot.validate()?;
        match (&self.parent_version_sha256, &self.change_evidence) {
            (None, None) => {}
            (Some(parent), Some(evidence)) => {
                digest(parent, "parent researcher version")?;
                evidence.content.validate()?;
                ensure!(
                    evidence.phase == MetaTaskPhase::Development,
                    "version changes cannot consume selection or certification feedback"
                );
            }
            _ => anyhow::bail!(
                "parent researcher version and development change evidence must be paired"
            ),
        }
        Ok(())
    }

    pub fn id(&self) -> Result<String> {
        self.validate()?;
        content_sha256(self)
    }
}

impl RankingChangeProposalV1 {
    /// A ranking configuration change reuses the same actual Build and tools.
    /// Code, prompt, corpus, policy or budget changes require another behavior.
    pub fn validate_against(&self, incumbent: &ResearcherVersionV1) -> Result<()> {
        ensure!(
            self.schema == CONTRACT_SCHEMA_V1,
            "unsupported ranking proposal schema"
        );
        let parent = incumbent.id()?;
        self.challenger.validate()?;
        ensure!(
            self.incumbent_version_sha256 == parent
                && self.challenger.parent_version_sha256.as_deref() == Some(parent.as_str()),
            "ranking proposal parent version drifted"
        );
        ensure!(
            self.challenger.snapshot.retrieval.ranking != incumbent.snapshot.retrieval.ranking,
            "ranking proposal makes no configuration change"
        );
        let mut compared = self.challenger.snapshot.clone();
        compared.retrieval.ranking = incumbent.snapshot.retrieval.ranking;
        ensure!(
            compared == incumbent.snapshot,
            "ranking proposal changes frozen content, information limits, source, tool or Build"
        );
        Ok(())
    }

    pub fn id(&self) -> Result<String> {
        ensure!(
            self.schema == CONTRACT_SCHEMA_V1,
            "unsupported ranking proposal schema"
        );
        digest(&self.incumbent_version_sha256, "incumbent version")?;
        self.challenger.validate()?;
        content_sha256(self)
    }
}

impl MetaResourcesV1 {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.cpu_millis > 0
                && self.cpu_millis <= 128000
                && self.memory_mib > 0
                && self.memory_mib <= 1048576
                && self.timeout_ms > 0
                && self.timeout_ms <= 7 * 24 * 60 * 60 * 1000,
            "invalid fixed meta-task resources"
        );
        Ok(())
    }
}

impl MetaBudgetLimitsV1 {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.max_trials > 0
                && self.max_trials <= 1000000
                && self.max_job_attempts > 0
                && self.max_job_attempts <= 65536,
            "invalid fixed meta-study budget"
        );
        Ok(())
    }
}

impl MetaPromotionPolicyV1 {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.min_selection_score_gain.is_finite()
                && self.min_selection_score_gain > 0.0
                && self.min_certification_score_gain.is_finite()
                && self.min_certification_score_gain > 0.0,
            "promotion thresholds must be fixed, positive and finite"
        );
        Ok(())
    }
}

impl MetaTaskV1 {
    pub fn validate(&self) -> Result<()> {
        valid_name(&self.task_id, "meta task id")?;
        digest(&self.data_view_sha256, "meta task DataView")?;
        digest(&self.evaluator_code_sha256, "meta evaluator code")?;
        ensure!(
            self.visibility == self.phase,
            "meta task role differs from declared DataView visibility"
        );
        document(&self.scoring_rule, "scoring rule")?;
        ensure!(
            !self.seeds.is_empty() && self.seeds.len() <= 64,
            "unbounded or absent seed schedule"
        );
        ensure!(
            self.seeds.iter().collect::<BTreeSet<_>>().len() == self.seeds.len(),
            "duplicate task seed"
        );
        Ok(())
    }

    pub fn id(&self) -> Result<String> {
        self.validate()?;
        content_sha256(self)
    }

    pub fn scoring_rule_sha256(&self) -> Result<String> {
        self.validate()?;
        content_sha256(&(self.score_direction, &self.scoring_rule))
    }
}

impl MetaRunBindingV1 {
    pub fn validate(&self) -> Result<()> {
        valid_name(&self.task_id, "Run task id")?;
        digest(&self.version_sha256, "Run researcher version")?;
        digest(&self.run_sha256, "Run")
    }
}

impl MetaStudyV1 {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == CONTRACT_SCHEMA_V1,
            "unsupported meta-study schema"
        );
        digest(&self.incumbent_version_sha256, "incumbent version")?;
        digest(&self.challenger_version_sha256, "challenger version")?;
        ensure!(
            self.incumbent_version_sha256 != self.challenger_version_sha256,
            "meta-study requires two distinct frozen versions"
        );
        digest(&self.grant_sha256, "meta-study grant")?;
        digest(&self.budget_scope_sha256, "meta-study budget scope")?;
        self.resources.validate()?;
        self.per_arm_budget.validate()?;
        self.promotion_policy.validate()?;
        ensure!(
            self.tasks.len() >= 3 && self.tasks.len() <= 128,
            "meta-study requires bounded development, selection and certification tasks"
        );
        let mut task_ids = BTreeMap::new();
        let mut view_phases = BTreeMap::new();
        let mut phases = BTreeSet::new();
        let mut expected = BTreeSet::new();
        for task in &self.tasks {
            task.validate()?;
            ensure!(
                task_ids.insert(task.task_id.as_str(), task).is_none(),
                "task identity overlaps phases or repeats"
            );
            if let Some(phase) = view_phases.insert(&task.data_view_sha256, task.phase) {
                ensure!(
                    phase == task.phase,
                    "one DataView overlaps development, selection or certification"
                );
            }
            phases.insert(task.phase);
            for seed in &task.seeds {
                expected.insert((
                    task.task_id.as_str(),
                    task.phase,
                    MetaStudyArm::Incumbent,
                    self.incumbent_version_sha256.as_str(),
                    *seed,
                ));
                expected.insert((
                    task.task_id.as_str(),
                    task.phase,
                    MetaStudyArm::Challenger,
                    self.challenger_version_sha256.as_str(),
                    *seed,
                ));
            }
        }
        ensure!(phases.len() == 3, "one meta-study phase is absent");
        ensure!(
            self.runs.len() == expected.len(),
            "frozen runs do not cover every task, seed and arm"
        );
        let mut seen = BTreeSet::new();
        let mut run_ids = BTreeSet::new();
        for run in &self.runs {
            run.validate()?;
            let key = (
                run.task_id.as_str(),
                run.phase,
                run.arm,
                run.version_sha256.as_str(),
                run.seed,
            );
            ensure!(
                expected.contains(&key) && seen.insert(key),
                "Run role/version/seed is foreign or duplicate"
            );
            ensure!(
                run_ids.insert(&run.run_sha256),
                "one Run identity is reused across meta tasks or arms"
            );
        }
        ensure!(
            self.runs.len() as u64 / 2 <= self.per_arm_budget.max_trials
                && self.runs.len() / 2 <= self.per_arm_budget.max_job_attempts as usize,
            "declared per-arm budget cannot cover the frozen matrix"
        );
        Ok(())
    }

    pub fn id(&self) -> Result<String> {
        self.validate()?;
        content_sha256(self)
    }
}

impl MetaExecutionBindingV1 {
    pub fn validate(&self) -> Result<()> {
        digest(&self.study_sha256, "execution Study")?;
        digest(&self.task_sha256, "execution task")?;
        self.run.validate()?;
        ensure!(self.attempt == 1 && self.fence > 0, "initial meta contract requires first Attempt and actual fence; retry needs cumulative cost authority");
        Ok(())
    }

    pub fn validate_for(&self, study: &MetaStudyV1) -> Result<()> {
        self.validate()?;
        ensure!(
            self.study_sha256 == study.id()?,
            "execution Study identity differs from frozen study"
        );
        ensure!(
            study.runs.contains(&self.run),
            "execution Run binding differs from frozen matrix"
        );
        let task = study
            .tasks
            .iter()
            .find(|task| task.task_id == self.run.task_id)
            .ok_or_else(|| anyhow::anyhow!("execution task is absent"))?;
        ensure!(
            task.id()? == self.task_sha256,
            "execution task content differs from frozen task"
        );
        ensure!(
            self.attempt <= study.per_arm_budget.max_job_attempts,
            "execution Attempt exceeds declared arm limit"
        );
        Ok(())
    }
}

impl MetaArtifactReferenceV1 {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.key.starts_with("research/")
                && self.key.len() <= 1024
                && self
                    .key
                    .split('/')
                    .all(|v| !v.is_empty() && v != "." && v != "..")
                && self
                    .key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/-_.=".contains(&b)),
            "invalid immutable meta artifact key"
        );
        digest(&self.sha256, "meta artifact")?;
        ensure!(
            self.bytes > 0 && self.bytes <= 512 * 1024 * 1024,
            "unbounded or empty meta artifact"
        );
        Ok(())
    }
}

impl MetaEvaluationV1 {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == CONTRACT_SCHEMA_V1,
            "unsupported meta evaluation schema"
        );
        self.binding.validate()?;
        self.resources.validate()?;
        self.result_artifact.validate()?;
        for (value, name) in [
            (&self.data_view_sha256, "evaluation DataView"),
            (&self.evaluator_code_sha256, "evaluation code"),
            (&self.scoring_rule_sha256, "evaluation scoring rule"),
            (&self.grant_sha256, "evaluation grant"),
            (&self.budget_scope_sha256, "evaluation budget scope"),
        ] {
            digest(value, name)?;
        }
        ensure!(
            self.visibility == self.binding.run.phase,
            "evaluation claims a different task phase"
        );
        ensure!(
            self.result_readback_sha256 == self.result_artifact.sha256,
            "evaluation result readback differs from artifact"
        );
        match self.outcome {
            MetaEvaluationOutcomeV1::Succeeded => ensure!(
                self.score.is_some_and(f64::is_finite) && self.failure_code.is_none(),
                "successful evaluation requires finite score and no failure"
            ),
            MetaEvaluationOutcomeV1::Failed | MetaEvaluationOutcomeV1::Cancelled => {
                ensure!(
                    self.score.is_none(),
                    "failed evaluation cannot claim a scientific score"
                );
                valid_name(
                    self.failure_code.as_deref().unwrap_or_default(),
                    "evaluation failure code",
                )?;
            }
        }
        Ok(())
    }

    pub fn validate_for(&self, study: &MetaStudyV1) -> Result<()> {
        self.validate()?;
        self.binding.validate_for(study)?;
        let task = study
            .tasks
            .iter()
            .find(|task| task.task_id == self.binding.run.task_id)
            .ok_or_else(|| anyhow::anyhow!("evaluation task is absent"))?;
        ensure!(
            self.data_view_sha256 == task.data_view_sha256
                && self.visibility == task.visibility
                && self.evaluator_code_sha256 == task.evaluator_code_sha256
                && self.scoring_rule_sha256 == task.scoring_rule_sha256()?,
            "evaluation input/visibility/evaluator/scoring differs from frozen task"
        );
        ensure!(
            self.grant_sha256 == study.grant_sha256
                && self.budget_scope_sha256 == study.budget_scope_sha256
                && self.resources == study.resources,
            "evaluation grant/budget/resources differ between frozen arms"
        );
        Ok(())
    }
}

impl MetaCostV1 {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == CONTRACT_SCHEMA_V1,
            "unsupported meta cost schema"
        );
        self.binding.validate()?;
        self.resources.validate()?;
        self.cost_artifact.validate()?;
        digest(&self.grant_sha256, "cost grant")?;
        digest(&self.budget_scope_sha256, "cost budget scope")?;
        digest(&self.evaluated_result_sha256, "cost evaluated result")?;
        ensure!(
            self.cost_readback_sha256 == self.cost_artifact.sha256,
            "cost readback differs from artifact"
        );
        ensure!(
            self.usage.job_attempts == 1 && self.usage.wall_ms <= self.resources.timeout_ms,
            "cost receipt must cover its one bounded Attempt"
        );
        Ok(())
    }

    pub fn validate_for(&self, study: &MetaStudyV1) -> Result<()> {
        self.validate()?;
        self.binding.validate_for(study)?;
        ensure!(
            self.grant_sha256 == study.grant_sha256
                && self.budget_scope_sha256 == study.budget_scope_sha256
                && self.resources == study.resources,
            "cost grant/budget/resources differ from frozen study"
        );
        ensure!(
            self.usage.trials <= study.per_arm_budget.max_trials
                && self.usage.llm_tokens <= study.per_arm_budget.max_llm_tokens
                && self.usage.cost_microusd <= study.per_arm_budget.max_cost_microusd,
            "individual cost receipt exceeds frozen arm limits"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn versions() -> (ResearcherVersionV1, ResearcherVersionV1) {
        let incumbent = ResearcherVersionV1 {
            schema: 1,
            parent_version_sha256: None,
            change_evidence: None,
            snapshot: ResearcherSnapshotV1 {
                prompt_text: "Compare allowed research methods using development evidence.".into(),
                search_policy: serde_json::json!({"max_candidates":4,"method":"bounded"}),
                retrieval: ExperienceRetrievalV1 {
                    ranking: ExperienceRankingOrder::TaskMatchThenRecency,
                    corpus: vec![ExperienceCorpusReferenceV1 {
                        content: ContentReferenceV1 {
                            id: "development-corpus".into(),
                            content_sha256: hash('a'),
                        },
                        feedback_phase: MetaTaskPhase::Development,
                    }],
                    query_template: "task context and permitted evidence".into(),
                    top_k: 4,
                    max_context_bytes: 8192,
                    information_policy: InformationPolicyV1 {
                        schema: 1,
                        proposal_feedback_phases: vec![MetaTaskPhase::Development],
                        allowed_data_view_sha256: vec![hash('d')],
                    },
                },
                source_commit: "a".repeat(40),
                build_sha256: hash('b'),
                tools: vec![ContentReferenceV1 {
                    id: "registered-tool".into(),
                    content_sha256: hash('c'),
                }],
            },
        };
        let mut challenger = incumbent.clone();
        challenger.parent_version_sha256 = Some(incumbent.id().unwrap());
        challenger.change_evidence = Some(ResearcherChangeEvidenceV1 {
            content: ContentReferenceV1 {
                id: "development-observation".into(),
                content_sha256: hash('e'),
            },
            phase: MetaTaskPhase::Development,
        });
        challenger.snapshot.retrieval.ranking = ExperienceRankingOrder::RecencyThenTaskMatch;
        (incumbent, challenger)
    }

    fn study() -> MetaStudyV1 {
        let (incumbent, challenger) = versions();
        let mut study = MetaStudyV1 {
            schema: 1,
            incumbent_version_sha256: incumbent.id().unwrap(),
            challenger_version_sha256: challenger.id().unwrap(),
            tasks: vec![],
            runs: vec![],
            grant_sha256: hash('a'),
            budget_scope_sha256: hash('b'),
            resources: MetaResourcesV1 {
                cpu_millis: 1000,
                memory_mib: 512,
                timeout_ms: 10000,
            },
            per_arm_budget: MetaBudgetLimitsV1 {
                max_trials: 8,
                max_job_attempts: 8,
                max_llm_tokens: 1000,
                max_cost_microusd: 10000,
            },
            promotion_policy: MetaPromotionPolicyV1 {
                min_selection_score_gain: 0.1,
                min_certification_score_gain: 0.1,
            },
        };
        for (phase, name, view) in [
            (MetaTaskPhase::Development, "development", 'd'),
            (MetaTaskPhase::Selection, "selection", 'e'),
            (MetaTaskPhase::Certification, "certification", 'f'),
        ] {
            study.tasks.push(MetaTaskV1 {
                task_id: name.into(),
                phase,
                data_view_sha256: hash(view),
                visibility: phase,
                evaluator_code_sha256: hash('a'),
                scoring_rule: serde_json::json!({"metric":"fixed_normalized_score"}),
                score_direction: MetaScoreDirectionV1::HigherIsBetter,
                seeds: vec![7, 19],
            });
        }
        for task in &study.tasks {
            for &seed in &task.seeds {
                for (arm, version) in [
                    (MetaStudyArm::Incumbent, &study.incumbent_version_sha256),
                    (MetaStudyArm::Challenger, &study.challenger_version_sha256),
                ] {
                    study.runs.push(MetaRunBindingV1 {
                        task_id: task.task_id.clone(),
                        phase: task.phase,
                        arm,
                        version_sha256: version.clone(),
                        run_sha256: content_sha256(&(&task.task_id, seed, arm, version)).unwrap(),
                        seed,
                    });
                }
            }
        }
        study.validate().unwrap();
        study
    }

    fn receipts(study: &MetaStudyV1) -> (MetaEvaluationV1, MetaCostV1) {
        let run = study.runs[0].clone();
        let task = study
            .tasks
            .iter()
            .find(|task| task.task_id == run.task_id)
            .unwrap();
        let binding = MetaExecutionBindingV1 {
            study_sha256: study.id().unwrap(),
            run,
            task_sha256: task.id().unwrap(),
            attempt: 1,
            fence: 11,
        };
        let evaluation = MetaEvaluationV1 {
            schema: 1,
            binding: binding.clone(),
            data_view_sha256: task.data_view_sha256.clone(),
            visibility: task.visibility,
            evaluator_code_sha256: task.evaluator_code_sha256.clone(),
            scoring_rule_sha256: task.scoring_rule_sha256().unwrap(),
            grant_sha256: study.grant_sha256.clone(),
            budget_scope_sha256: study.budget_scope_sha256.clone(),
            resources: study.resources.clone(),
            outcome: MetaEvaluationOutcomeV1::Succeeded,
            score: Some(1.25),
            failure_code: None,
            result_artifact: MetaArtifactReferenceV1 {
                key: "research/meta/evaluation.json".into(),
                sha256: hash('c'),
                bytes: 128,
            },
            result_readback_sha256: hash('c'),
        };
        let cost = MetaCostV1 {
            schema: 1,
            binding,
            grant_sha256: study.grant_sha256.clone(),
            budget_scope_sha256: study.budget_scope_sha256.clone(),
            resources: study.resources.clone(),
            usage: MetaCostUsageV1 {
                trials: 1,
                job_attempts: 1,
                wall_ms: 100,
                llm_tokens: 30,
                cost_microusd: 100,
            },
            evaluated_result_sha256: hash('c'),
            cost_artifact: MetaArtifactReferenceV1 {
                key: "research/meta/cost.json".into(),
                sha256: hash('d'),
                bytes: 128,
            },
            cost_readback_sha256: hash('d'),
        };
        evaluation.validate_for(study).unwrap();
        cost.validate_for(study).unwrap();
        (evaluation, cost)
    }

    #[test]
    fn ranking_change_freezes_actual_configuration_and_reuses_build() {
        let (incumbent, challenger) = versions();
        let proposal = RankingChangeProposalV1 {
            schema: 1,
            incumbent_version_sha256: incumbent.id().unwrap(),
            challenger,
        };
        proposal.validate_against(&incumbent).unwrap();
        assert_eq!(
            proposal.challenger.snapshot.build_sha256,
            incumbent.snapshot.build_sha256
        );
        assert_ne!(proposal.challenger.id().unwrap(), incumbent.id().unwrap());
        let restored: RankingChangeProposalV1 =
            serde_json::from_slice(&canonical_bytes(&proposal).unwrap()).unwrap();
        assert_eq!(proposal.id().unwrap(), restored.id().unwrap());
        for change in [
            "prompt", "policy", "corpus", "query", "top_k", "context", "scope", "source", "build",
            "tool", "parent", "noop",
        ] {
            let mut changed = proposal.clone();
            match change {
                "prompt" => changed.challenger.snapshot.prompt_text.push('!'),
                "policy" => {
                    changed.challenger.snapshot.search_policy["max_candidates"] =
                        serde_json::json!(5)
                }
                "corpus" => {
                    changed.challenger.snapshot.retrieval.corpus[0]
                        .content
                        .content_sha256 = hash('f')
                }
                "query" => changed
                    .challenger
                    .snapshot
                    .retrieval
                    .query_template
                    .push('!'),
                "top_k" => changed.challenger.snapshot.retrieval.top_k += 1,
                "context" => changed.challenger.snapshot.retrieval.max_context_bytes += 1,
                "scope" => changed
                    .challenger
                    .snapshot
                    .retrieval
                    .information_policy
                    .allowed_data_view_sha256
                    .push(hash('f')),
                "source" => changed.challenger.snapshot.source_commit = "b".repeat(40),
                "build" => changed.challenger.snapshot.build_sha256 = hash('f'),
                "tool" => changed.challenger.snapshot.tools[0].content_sha256 = hash('f'),
                "parent" => changed.challenger.parent_version_sha256 = Some(hash('f')),
                "noop" => {
                    changed.challenger.snapshot.retrieval.ranking =
                        incumbent.snapshot.retrieval.ranking
                }
                _ => unreachable!(),
            }
            assert!(changed.validate_against(&incumbent).is_err(), "{change}");
        }
    }

    #[test]
    fn certification_and_selection_cannot_become_proposal_feedback() {
        let (incumbent, challenger) = versions();
        challenger.validate().unwrap();
        for phase in [MetaTaskPhase::Selection, MetaTaskPhase::Certification] {
            let mut changed = challenger.clone();
            changed.change_evidence.as_mut().unwrap().phase = phase;
            assert!(changed.validate().is_err());
            changed = incumbent.clone();
            changed.snapshot.retrieval.corpus[0].feedback_phase = phase;
            assert!(changed.validate().is_err());
            changed = incumbent.clone();
            changed
                .snapshot
                .retrieval
                .information_policy
                .proposal_feedback_phases
                .push(phase);
            assert!(changed.validate().is_err());
        }
    }

    #[test]
    fn frozen_study_requires_separate_complete_task_seed_arm_matrix() {
        let original = study();
        for change in [
            "missing_run",
            "duplicate_run",
            "foreign_seed",
            "wrong_arm",
            "wrong_version",
            "shared_view",
            "role",
            "missing_phase",
            "low_budget",
            "duplicate_task",
        ] {
            let mut changed = original.clone();
            match change {
                "missing_run" => {
                    changed.runs.pop();
                }
                "duplicate_run" => changed.runs[1] = changed.runs[0].clone(),
                "foreign_seed" => changed.runs[0].seed = 999,
                "wrong_arm" => changed.runs[0].arm = MetaStudyArm::Challenger,
                "wrong_version" => changed.runs[0].version_sha256 = hash('f'),
                "shared_view" => {
                    changed.tasks[1].data_view_sha256 = changed.tasks[0].data_view_sha256.clone()
                }
                "role" => changed.tasks[1].visibility = MetaTaskPhase::Development,
                "missing_phase" => changed.tasks[2].phase = MetaTaskPhase::Selection,
                "low_budget" => changed.per_arm_budget.max_trials = 1,
                "duplicate_task" => changed.tasks[1].task_id = changed.tasks[0].task_id.clone(),
                _ => unreachable!(),
            }
            assert!(changed.validate().is_err(), "{change}");
        }
    }

    #[test]
    fn thresholds_are_frozen_positive_finite_and_change_study_identity() {
        let original = study();
        let id = original.id().unwrap();
        let mut changed = original.clone();
        changed.promotion_policy.min_selection_score_gain = 0.2;
        assert_ne!(changed.id().unwrap(), id);
        for value in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            changed = original.clone();
            changed.promotion_policy.min_selection_score_gain = value;
            assert!(changed.validate().is_err());
            changed = original.clone();
            changed.promotion_policy.min_certification_score_gain = value;
            assert!(changed.validate().is_err());
        }
    }

    #[test]
    fn receipts_reject_changed_source_budget_role_and_retry_without_cost_authority() {
        let study = study();
        let (evaluation, cost) = receipts(&study);
        for change in [
            "study",
            "run",
            "version",
            "task",
            "attempt2",
            "zero_fence",
            "view",
            "code",
            "rule",
            "grant",
            "budget",
            "resource",
            "readback",
            "nan_score",
        ] {
            let mut changed = evaluation.clone();
            match change {
                "study" => changed.binding.study_sha256 = hash('f'),
                "run" => changed.binding.run.run_sha256 = hash('f'),
                "version" => changed.binding.run.version_sha256 = hash('f'),
                "task" => changed.binding.task_sha256 = hash('f'),
                "attempt2" => changed.binding.attempt = 2,
                "zero_fence" => changed.binding.fence = 0,
                "view" => changed.data_view_sha256 = hash('f'),
                "code" => changed.evaluator_code_sha256 = hash('f'),
                "rule" => changed.scoring_rule_sha256 = hash('f'),
                "grant" => changed.grant_sha256 = hash('f'),
                "budget" => changed.budget_scope_sha256 = hash('f'),
                "resource" => changed.resources.cpu_millis += 1,
                "readback" => changed.result_readback_sha256 = hash('f'),
                "nan_score" => changed.score = Some(f64::NAN),
                _ => unreachable!(),
            }
            assert!(changed.validate_for(&study).is_err(), "{change}");
        }
        let mut changed = cost.clone();
        changed.binding.attempt = 2;
        assert!(changed.validate_for(&study).is_err());
        changed = cost.clone();
        changed.usage.job_attempts = 2;
        assert!(changed.validate_for(&study).is_err());
        changed = cost.clone();
        changed.usage.cost_microusd = study.per_arm_budget.max_cost_microusd + 1;
        assert!(changed.validate_for(&study).is_err());
        changed = cost;
        changed.cost_readback_sha256 = hash('f');
        assert!(changed.validate_for(&study).is_err());
        // A nonzero but wrong fence is a valid declaration. Control must bind it
        // to the actual task/ledger; pure shape validation cannot authenticate it.
        let mut declared = evaluation;
        declared.binding.fence += 1;
        declared.validate_for(&study).unwrap();
    }

    #[test]
    fn failed_or_cancelled_evaluation_never_claims_a_success_score() {
        let study = study();
        let (original, _) = receipts(&study);
        for outcome in [
            MetaEvaluationOutcomeV1::Failed,
            MetaEvaluationOutcomeV1::Cancelled,
        ] {
            let mut evaluation = original.clone();
            evaluation.outcome = outcome;
            evaluation.failure_code = Some("worker_failed".into());
            assert!(evaluation.validate().is_err());
            evaluation.score = None;
            evaluation.validate_for(&study).unwrap();
            evaluation.failure_code = None;
            assert!(evaluation.validate().is_err());
        }
    }

    #[test]
    fn signing_payload_is_domain_key_payload_and_json_cannot_supply_trust() {
        let study = study();
        let (evaluation, cost) = receipts(&study);
        let signed = SignedMetaEvaluationV1 {
            payload: evaluation,
            key_id: "evaluation-key".into(),
            signature_hex: "0".repeat(128),
        };
        let cost = SignedMetaCostV1 {
            payload: cost,
            key_id: "cost-key".into(),
            signature_hex: "0".repeat(128),
        };
        // Encoding checks do not verify this deliberately fake signature.
        signed.validate().unwrap();
        cost.validate().unwrap();
        assert_eq!(
            signed.signing_bytes().unwrap(),
            canonical_bytes(&(
                META_EVALUATION_SIGNING_DOMAIN,
                &signed.key_id,
                &signed.payload
            ))
            .unwrap()
        );
        assert_ne!(
            signed.signing_bytes().unwrap(),
            cost.signing_bytes().unwrap()
        );
        let mut altered = signed.clone();
        altered.key_id = "different-key".into();
        assert_ne!(
            signed.signing_bytes().unwrap(),
            altered.signing_bytes().unwrap()
        );
        altered = signed.clone();
        altered.payload.binding.fence += 1;
        assert_ne!(
            signed.signing_bytes().unwrap(),
            altered.signing_bytes().unwrap()
        );
        for forged in ["verified", "trusted_public_key", "opaque_proof"] {
            let mut value = serde_json::to_value(&signed).unwrap();
            value[forged] = serde_json::json!(true);
            assert!(
                serde_json::from_value::<SignedMetaEvaluationV1>(value).is_err(),
                "{forged}"
            );
        }
    }

    #[test]
    fn canonical_content_binds_nested_values_and_real_git_build_identities() {
        let a = serde_json::json!({"z":{"b":2,"a":1},"a":0});
        let b = serde_json::json!({"a":0,"z":{"a":1,"b":2}});
        assert_eq!(content_sha256(&a).unwrap(), content_sha256(&b).unwrap());
        let (mut version, _) = versions();
        version.snapshot.source_commit = hash('a');
        assert!(version.validate().is_err());
        version.snapshot.source_commit = "unknown".into();
        assert!(version.validate().is_err());
        version = versions().0;
        let old = version.id().unwrap();
        version.snapshot.prompt_text.push('!');
        assert_ne!(old, version.id().unwrap());
    }
}
