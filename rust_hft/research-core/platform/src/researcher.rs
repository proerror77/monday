//! Pure researcher receipt verification and head transitions. The caller supplies
//! controlled frozen context and public role keys; this module neither establishes
//! a live MetaStudy grant nor writes a persistent authority or starts a Run.
use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use ed25519_dalek::{Signature, VerifyingKey};
use hft_research_agent_contracts::{
    content_sha256, MetaArtifactReferenceV1, MetaCostV1, MetaEvaluationOutcomeV1, MetaEvaluationV1,
    MetaExecutionBindingV1, MetaScoreDirectionV1, MetaStudyArm, MetaStudyV1, MetaTaskPhase,
    RankingChangeProposalV1, ResearcherVersionV1, SignedMetaCostV1, SignedMetaEvaluationV1,
};

use crate::{
    execution::Backend, orchestrator::TaskSpec, release::VerifiedBuildRelease, research::Run,
    sha256, valid_digest,
};

/// Operator-controlled role configuration, never taken from a receipt's JSON.
/// Separate domains alone do not justify reusing one actor's key for both roles.
pub struct MetaVerificationTrust {
    evaluator_keys: BTreeMap<String, VerifyingKey>,
    cost_keys: BTreeMap<String, VerifyingKey>,
}
impl MetaVerificationTrust {
    fn id(&self) -> Result<String> {
        let role = |keys: &BTreeMap<String, VerifyingKey>| {
            keys.iter()
                .map(|(id, key)| (id.clone(), key.to_bytes()))
                .collect::<Vec<_>>()
        };
        content_sha256(&(role(&self.evaluator_keys), role(&self.cost_keys)))
    }

    pub fn from_role_keys(
        evaluator_keys: BTreeMap<String, [u8; 32]>,
        cost_keys: BTreeMap<String, [u8; 32]>,
    ) -> Result<Self> {
        fn keys(values: BTreeMap<String, [u8; 32]>) -> Result<BTreeMap<String, VerifyingKey>> {
            ensure!(
                (1..=16).contains(&values.len()),
                "bounded nonempty role keys required"
            );
            values
                .into_iter()
                .map(|(id, bytes)| {
                    ensure!(
                        !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control),
                        "invalid role key id"
                    );
                    Ok((id, VerifyingKey::from_bytes(&bytes)?))
                })
                .collect()
        }
        ensure!(
            evaluator_keys.keys().all(|id| !cost_keys.contains_key(id)),
            "evaluator and cost key identities overlap"
        );
        ensure!(
            evaluator_keys
                .values()
                .all(|key| !cost_keys.values().any(|cost| cost == key)),
            "evaluator and cost roles share a public key"
        );
        Ok(Self {
            evaluator_keys: keys(evaluator_keys)?,
            cost_keys: keys(cost_keys)?,
        })
    }
}

/// These are controlled execution facts in the first software slice. A later
/// ledger adapter must derive them from the actual immutable Task/Attempt record.
pub struct ExpectedMetaExecution<'a> {
    pub binding: &'a MetaExecutionBindingV1,
    pub run: &'a Run,
    pub task: &'a TaskSpec,
    pub released_build: &'a VerifiedBuildRelease,
}

/// Immutable borrowing prevents configuration edits after validation. A caller
/// JSON cannot supply an opaque release or self-assert a trusted Build boolean.
pub struct MetaVerificationContext<'a> {
    study: &'a MetaStudyV1,
    trust: &'a MetaVerificationTrust,
    context_sha256: String,
    executions: BTreeMap<String, ExpectedMetaExecution<'a>>,
}
impl<'a> MetaVerificationContext<'a> {
    pub fn new(
        study: &'a MetaStudyV1,
        trust: &'a MetaVerificationTrust,
        incumbent: &'a ResearcherVersionV1,
        challenger: &'a ResearcherVersionV1,
        executions: Vec<ExpectedMetaExecution<'a>>,
    ) -> Result<Self> {
        study.validate()?;
        RankingChangeProposalV1 {
            schema: 1,
            incumbent_version_sha256: incumbent.id()?,
            challenger: challenger.clone(),
        }
        .validate_against(incumbent)?;
        ensure!(
            study.incumbent_version_sha256 == incumbent.id()?
                && study.challenger_version_sha256 == challenger.id()?,
            "study version/parent changed"
        );
        let proposal_views = &incumbent
            .snapshot
            .retrieval
            .information_policy
            .allowed_data_view_sha256;
        ensure!(
            study.tasks.iter().all(|task| match task.phase {
                MetaTaskPhase::Development => proposal_views.contains(&task.data_view_sha256),
                MetaTaskPhase::Selection | MetaTaskPhase::Certification =>
                    !proposal_views.contains(&task.data_view_sha256),
            }),
            "development view is outside allowed scope or hidden views are exposed"
        );
        ensure!(
            executions.len() == study.runs.len(),
            "expected execution matrix is incomplete"
        );
        let mut by_run = BTreeMap::new();
        for execution in executions {
            execution.binding.validate_for(study)?;
            let frozen = &execution.binding.run;
            let task = study
                .tasks
                .iter()
                .find(|task| task.task_id == frozen.task_id)
                .context("expected task absent")?;
            let version = match frozen.arm {
                MetaStudyArm::Incumbent => incumbent,
                MetaStudyArm::Challenger => challenger,
            };
            let artifact = execution.released_build.artifact();
            execution.task.validate()?;
            execution.task.profile.validate()?;
            ensure!(
                execution
                    .task
                    .output_prefix
                    .split('/')
                    .all(|part| !part.is_empty()),
                "task output prefix contains an empty segment"
            );
            execution.run.admit_build(artifact)?;
            execution.run.admit(execution.task)?;
            let architecture = match artifact.build.target.as_str() {
                "x86_64-unknown-linux-gnu" => "amd64",
                "aarch64-unknown-linux-gnu" => "arm64",
                _ => anyhow::bail!("unsupported verified Build target"),
            };
            ensure!(
                execution.task.profile.architecture == architecture
                    && execution.task.profile.worker_secret.is_none()
                    && execution.task.worker_configuration.is_none(),
                "Build architecture or unverified worker configuration changed"
            );
            ensure!(
                execution.run.id()? == frozen.run_sha256
                    && execution.run.configuration_sha256 == frozen.version_sha256
                    && execution.run.seed == frozen.seed
                    && execution.run.data_manifest_sha256 == task.data_view_sha256
                    && execution.run.evaluator_sha256 == task.evaluator_code_sha256
                    && execution.run.evaluation_protocol_sha256 == task.scoring_rule_sha256()?
                    && artifact.id()? == version.snapshot.build_sha256
                    && artifact.build.code_commit == version.snapshot.source_commit,
                "expected Run changed the version, Build, source, input, seed or evaluator"
            );
            let selected_program = artifact
                .executables
                .iter()
                .find(|program| {
                    execution.run.command.first()
                        == Some(&format!("/usr/local/bin/{}", program.name))
                })
                .context("Run does not select a verified released program")?;
            ensure!(
                selected_program.blob.sha256 == task.evaluator_code_sha256,
                "invoked evaluator program differs from the frozen evaluator code"
            );
            ensure!(
                execution.task.profile.backend == Backend::KubernetesJob
                    && execution.task.profile.gpu == 0
                    && execution.task.profile.cpu_millis == study.resources.cpu_millis
                    && execution.task.profile.memory_mib == study.resources.memory_mib
                    && u64::try_from(execution.task.timeout_ms)? == study.resources.timeout_ms
                    && execution.task.max_attempts == 1,
                "expected execution changes fixed resources or permits unaccounted retries"
            );
            ensure!(
                by_run
                    .insert(frozen.run_sha256.clone(), execution)
                    .is_none(),
                "duplicate expected Run"
            );
        }
        ensure!(
            study
                .runs
                .iter()
                .all(|run| by_run.contains_key(&run.run_sha256)),
            "expected execution omits a frozen Run"
        );
        for task in &study.tasks {
            for seed in &task.seeds {
                let paired = |arm| {
                    study
                        .runs
                        .iter()
                        .find(|run| {
                            run.task_id == task.task_id && run.seed == *seed && run.arm == arm
                        })
                        .and_then(|run| by_run.get(&run.run_sha256))
                        .context("paired expected execution absent")
                };
                let incumbent = paired(MetaStudyArm::Incumbent)?;
                let challenger = paired(MetaStudyArm::Challenger)?;
                let mut normalized_run = challenger.run.clone();
                normalized_run
                    .configuration_sha256
                    .clone_from(&incumbent.run.configuration_sha256);
                ensure!(
                    normalized_run == *incumbent.run,
                    "paired Run changes command, kind, experiment, fit, source or evaluator"
                );
                let mut normalized_task = challenger.task.clone();
                normalized_task
                    .run_manifest_sha256
                    .clone_from(&incumbent.task.run_manifest_sha256);
                ensure!(normalized_task == *incumbent.task, "paired task changes profile, scratch, target, acceptance or other execution policy");
            }
        }
        let execution_identities = by_run
            .values()
            .map(|execution| {
                Ok((
                    execution.binding,
                    execution.run.id()?,
                    execution.task.id()?,
                    execution.released_build.artifact().id()?,
                    execution.released_build.trust_sha256(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let context_sha256 = content_sha256(&(
            "monday.meta_verification_context.v1",
            study.id()?,
            trust.id()?,
            execution_identities,
        ))?;
        Ok(Self {
            study,
            trust,
            context_sha256,
            executions: by_run,
        })
    }

    pub fn verify_evaluation(
        &self,
        signed_evaluation_bytes: &[u8],
        result_bytes: &[u8],
        signed_cost_bytes: &[u8],
        cost_bytes: &[u8],
    ) -> Result<VerifiedMetaEvaluation> {
        ensure!(
            (1..=2 * 1024 * 1024).contains(&signed_evaluation_bytes.len())
                && (1..=2 * 1024 * 1024).contains(&signed_cost_bytes.len()),
            "signed receipt exceeds wire bounds"
        );
        let evaluated: SignedMetaEvaluationV1 = serde_json::from_slice(signed_evaluation_bytes)?;
        let cost: SignedMetaCostV1 = serde_json::from_slice(signed_cost_bytes)?;
        evaluated.validate()?;
        cost.validate()?;
        verify_signature(
            &self.trust.evaluator_keys,
            &evaluated.key_id,
            &evaluated.signing_bytes()?,
            &evaluated.signature_hex,
        )?;
        verify_signature(
            &self.trust.cost_keys,
            &cost.key_id,
            &cost.signing_bytes()?,
            &cost.signature_hex,
        )?;
        evaluated.payload.validate_for(self.study)?;
        cost.payload.validate_for(self.study)?;
        let expected = self
            .executions
            .get(&evaluated.payload.binding.run.run_sha256)
            .context("receipt has no controlled expected Run")?;
        ensure!(
            evaluated.payload.binding == *expected.binding
                && cost.payload.binding == evaluated.payload.binding
                && cost.payload.evaluated_result_sha256 == evaluated.payload.result_artifact.sha256,
            "evaluation/cost changed the original Run, Attempt, fence or result"
        );
        verify_artifact(
            &evaluated.payload.result_artifact,
            result_bytes,
            expected.task,
            expected.binding.attempt,
        )?;
        verify_artifact(
            &cost.payload.cost_artifact,
            cost_bytes,
            expected.task,
            expected.binding.attempt,
        )?;
        Ok(VerifiedMetaEvaluation {
            context_sha256: self.context_sha256.clone(),
            evaluation: evaluated.payload,
            cost: cost.payload,
            evidence_sha256: content_sha256(&(
                sha256(signed_evaluation_bytes),
                sha256(signed_cost_bytes),
            ))?,
        })
    }

    pub fn verify_promotion(
        &self,
        expected_head: &ResearcherHead,
        evaluations: &[VerifiedMetaEvaluation],
    ) -> Result<VerifiedPromotionDecision> {
        expected_head.validate()?;
        ensure!(
            expected_head.version_sha256 == self.study.incumbent_version_sha256,
            "promotion parent is not the expected incumbent"
        );
        ensure!(
            evaluations.len() == self.executions.len(),
            "evaluation/cost matrix is incomplete"
        );
        let mut by_run = BTreeMap::new();
        let mut costs = BTreeMap::new();
        let mut evidence = Vec::new();
        let mut failed = false;
        for evaluated in evaluations {
            ensure!(
                evaluated.context_sha256 == self.context_sha256,
                "verified receipt belongs to another trust or execution context"
            );
            evaluated.evaluation.validate_for(self.study)?;
            evaluated.cost.validate_for(self.study)?;
            let binding = &evaluated.evaluation.binding;
            ensure!(
                self.executions
                    .get(&binding.run.run_sha256)
                    .is_some_and(|expected| expected.binding == binding)
                    && by_run
                        .insert(binding.run.run_sha256.clone(), evaluated)
                        .is_none(),
                "foreign or duplicate verified execution"
            );
            failed |= evaluated.evaluation.outcome != MetaEvaluationOutcomeV1::Succeeded;
            let usage = costs.entry(binding.run.arm).or_insert([0_u64; 4]);
            for (sum, value) in usage.iter_mut().zip([
                evaluated.cost.usage.trials,
                u64::from(evaluated.cost.usage.job_attempts),
                evaluated.cost.usage.llm_tokens,
                evaluated.cost.usage.cost_microusd,
            ]) {
                *sum = sum.checked_add(value).context("meta cost sum overflow")?;
            }
            evidence.push(evaluated.evidence_sha256.clone());
        }
        ensure!(
            self.study
                .runs
                .iter()
                .all(|run| by_run.contains_key(&run.run_sha256)),
            "verified coverage omits a frozen Run"
        );
        let cap = &self.study.per_arm_budget;
        let overrun = costs.values().any(|usage| {
            usage[0] > cap.max_trials
                || usage[1] > u64::from(cap.max_job_attempts)
                || usage[2] > cap.max_llm_tokens
                || usage[3] > cap.max_cost_microusd
        });
        let outcome = if overrun {
            PromotionOutcome::Rejected(PromotionRejection::BudgetOverrun)
        } else if failed {
            PromotionOutcome::Rejected(PromotionRejection::FailedEvaluation)
        } else {
            let mut gains = BTreeMap::new();
            for task in &self.study.tasks {
                if task.phase == MetaTaskPhase::Development {
                    continue;
                }
                for seed in &task.seeds {
                    let score = |arm| -> Result<f64> {
                        let frozen = self
                            .study
                            .runs
                            .iter()
                            .find(|run| {
                                run.task_id == task.task_id && run.seed == *seed && run.arm == arm
                            })
                            .context("paired Run missing")?;
                        by_run
                            .get(&frozen.run_sha256)
                            .context("paired result missing")?
                            .evaluation
                            .score
                            .context("successful result lacks score")
                    };
                    let gain = match task.score_direction {
                        MetaScoreDirectionV1::HigherIsBetter => {
                            score(MetaStudyArm::Challenger)? - score(MetaStudyArm::Incumbent)?
                        }
                        MetaScoreDirectionV1::LowerIsBetter => {
                            score(MetaStudyArm::Incumbent)? - score(MetaStudyArm::Challenger)?
                        }
                    };
                    ensure!(gain.is_finite(), "paired score difference is not finite");
                    let (sum, count) = gains.entry(task.phase).or_insert((0_f64, 0_usize));
                    *sum += gain;
                    ensure!(sum.is_finite(), "phase score sum is not finite");
                    *count += 1;
                }
            }
            let mean = |phase| -> Result<f64> {
                let (sum, count) = gains
                    .get(&phase)
                    .context("promotion phase has no paired evidence")?;
                ensure!(*count > 0, "promotion phase lacks coverage");
                let gain = sum / *count as f64;
                ensure!(gain.is_finite(), "phase score mean is not finite");
                Ok(gain)
            };
            if mean(MetaTaskPhase::Selection)?
                >= self.study.promotion_policy.min_selection_score_gain
                && mean(MetaTaskPhase::Certification)?
                    >= self.study.promotion_policy.min_certification_score_gain
            {
                PromotionOutcome::Adopt
            } else {
                PromotionOutcome::Rejected(PromotionRejection::InsufficientGain)
            }
        };
        evidence.sort();
        let study_sha256 = self.study.id()?;
        let decision_sha256 = content_sha256(&(
            &self.context_sha256,
            &study_sha256,
            &expected_head.version_sha256,
            expected_head.revision,
            &expected_head.last_decision_sha256,
            &self.study.challenger_version_sha256,
            &evidence,
            format!("{outcome:?}"),
        ))?;
        Ok(VerifiedPromotionDecision {
            context_sha256: self.context_sha256.clone(),
            expected_head: expected_head.clone(),
            challenger_version_sha256: self.study.challenger_version_sha256.clone(),
            decision_sha256,
            outcome,
        })
    }
}

fn verify_signature(
    keys: &BTreeMap<String, VerifyingKey>,
    key_id: &str,
    bytes: &[u8],
    encoded: &str,
) -> Result<()> {
    let key = keys
        .get(key_id)
        .context("unknown key for this receipt role")?;
    ensure!(
        encoded.len() == 128
            && encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid signature encoding"
    );
    let mut signature = [0_u8; 64];
    for (index, byte) in signature.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16)?;
    }
    key.verify_strict(bytes, &Signature::from_bytes(&signature))
        .context("invalid role signature")
}
fn verify_artifact(
    reference: &MetaArtifactReferenceV1,
    bytes: &[u8],
    task: &TaskSpec,
    attempt: u32,
) -> Result<()> {
    let prefix = format!("{}/{}/{attempt}/", task.output_prefix, task.id()?);
    ensure!(
        reference.key.starts_with(&prefix)
            && reference.bytes == bytes.len() as u64
            && reference.sha256 == sha256(bytes),
        "artifact readback bytes, size or original task scope changed"
    );
    Ok(())
}

/// Authentication only: failures and their signed costs remain readable, but
/// the separate promotion check never adopts a failed evaluation.
pub struct VerifiedMetaEvaluation {
    context_sha256: String,
    evaluation: MetaEvaluationV1,
    cost: MetaCostV1,
    evidence_sha256: String,
}
impl VerifiedMetaEvaluation {
    pub fn evaluation(&self) -> &MetaEvaluationV1 {
        &self.evaluation
    }
    pub fn cost(&self) -> &MetaCostV1 {
        &self.cost
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearcherHead {
    version_sha256: String,
    revision: u64,
    last_decision_sha256: Option<String>,
}
impl ResearcherHead {
    /// A software head model. Persistent state provenance belongs to its future
    /// active-authority adapter, not this constructor or a serialized receipt.
    pub fn new(version_sha256: String, revision: u64) -> Result<Self> {
        let head = Self {
            version_sha256,
            revision,
            last_decision_sha256: None,
        };
        head.validate()?;
        Ok(head)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            valid_digest(&self.version_sha256)
                && self
                    .last_decision_sha256
                    .as_ref()
                    .is_none_or(|id| valid_digest(id)),
            "invalid researcher head identity"
        );
        Ok(())
    }
    pub fn version_sha256(&self) -> &str {
        &self.version_sha256
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionRejection {
    FailedEvaluation,
    BudgetOverrun,
    InsufficientGain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionOutcome {
    Adopt,
    Rejected(PromotionRejection),
}
pub struct VerifiedPromotionDecision {
    context_sha256: String,
    expected_head: ResearcherHead,
    challenger_version_sha256: String,
    decision_sha256: String,
    outcome: PromotionOutcome,
}
impl VerifiedPromotionDecision {
    pub fn outcome(&self) -> PromotionOutcome {
        self.outcome
    }
    pub fn id(&self) -> &str {
        &self.decision_sha256
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadTransition {
    Adopted,
    Reused,
    Rejected(PromotionRejection),
}

/// Caller holds the actual head's lock through this check and update. This is
/// neither a PG write nor a replacement authority; no arbitrary rollback API.
pub fn apply_expected_head(
    context: &MetaVerificationContext<'_>,
    head: &mut ResearcherHead,
    decision: &VerifiedPromotionDecision,
) -> Result<HeadTransition> {
    ensure!(
        decision.context_sha256 == context.context_sha256,
        "promotion decision belongs to another controlled context"
    );
    head.validate()?;
    if decision.outcome == PromotionOutcome::Adopt
        && head.version_sha256 == decision.challenger_version_sha256
        && head.last_decision_sha256.as_deref() == Some(decision.decision_sha256.as_str())
        && decision.expected_head.revision.checked_add(1) == Some(head.revision)
    {
        return Ok(HeadTransition::Reused);
    }
    ensure!(
        *head == decision.expected_head,
        "stale promotion parent or head revision"
    );
    match decision.outcome {
        PromotionOutcome::Rejected(reason) => Ok(HeadTransition::Rejected(reason)),
        PromotionOutcome::Adopt => {
            let revision = head
                .revision
                .checked_add(1)
                .context("head revision overflow")?;
            head.version_sha256
                .clone_from(&decision.challenger_version_sha256);
            head.revision = revision;
            head.last_decision_sha256 = Some(decision.decision_sha256.clone());
            Ok(HeadTransition::Adopted)
        }
    }
}

/// Actual content for the next configuration. References in the adopted version
/// must match these complete bodies, not caller-declared corpus hashes.
pub struct PrepareResearcherConfigurationRequest<'a> {
    pub candidate: &'a ResearcherVersionV1,
    pub released_build: &'a VerifiedBuildRelease,
    pub query: hft_research_agent_contracts::consumption::ExperienceQueryV1,
    pub corpora: Vec<hft_research_agent_contracts::consumption::FrozenExperienceCorpusV1>,
}

/// Constructed only from an applied, verified adoption in this exact context.
/// Exported bytes are configuration data, not a grant or a serialized proof.
pub struct PreparedResearcherConfiguration {
    context_sha256: String,
    applied_head: ResearcherHead,
    decision_sha256: String,
    configuration: hft_research_agent_contracts::consumption::ResearcherConsumptionConfigV1,
    canonical_bytes: Vec<u8>,
    configuration_sha256: String,
    build: crate::build::BuildArtifact,
}
impl PreparedResearcherConfiguration {
    pub fn bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
    pub fn id(&self) -> &str {
        &self.configuration_sha256
    }
    pub fn version_sha256(&self) -> &str {
        self.applied_head.version_sha256()
    }
}

impl MetaVerificationContext<'_> {
    pub fn prepare_configuration(
        &self,
        decision: &VerifiedPromotionDecision,
        applied_head: &ResearcherHead,
        request: PrepareResearcherConfigurationRequest<'_>,
    ) -> Result<PreparedResearcherConfiguration> {
        applied_head.validate()?;
        ensure!(
            decision.context_sha256 == self.context_sha256
                && decision.outcome == PromotionOutcome::Adopt,
            "configuration requires an Adopt decision from this controlled context"
        );
        ensure!(
            decision.expected_head.revision.checked_add(1) == Some(applied_head.revision)
                && applied_head.version_sha256 == decision.challenger_version_sha256
                && applied_head.last_decision_sha256.as_deref() == Some(decision.id()),
            "adoption is unapplied or the actual head changed"
        );
        let candidate_sha256 = request.candidate.id()?;
        ensure!(
            candidate_sha256 == applied_head.version_sha256
                && candidate_sha256 == self.study.challenger_version_sha256,
            "configuration candidate differs from the adopted version"
        );
        let artifact = request.released_build.artifact();
        ensure!(
            artifact.id()? == request.candidate.snapshot.build_sha256
                && artifact.build.code_commit == request.candidate.snapshot.source_commit,
            "configuration uses a different verified Build or source"
        );
        ensure!(
            self.executions
                .values()
                .all(|expected| expected.released_build.artifact() == artifact
                    && expected.released_build.trust_sha256()
                        == request.released_build.trust_sha256()),
            "configuration Build differs from the original controlled release"
        );
        let configuration =
            hft_research_agent_contracts::consumption::ResearcherConsumptionConfigV1 {
                schema: 1,
                version: request.candidate.clone(),
                query: request.query,
                corpora: request.corpora,
            };
        configuration.validate()?;
        let canonical_bytes = configuration.canonical_bytes()?;
        let configuration_sha256 = configuration.id()?;
        ensure!(
            sha256(&canonical_bytes) == configuration_sha256,
            "configuration identity differs from its actual canonical bytes"
        );
        Ok(PreparedResearcherConfiguration {
            context_sha256: self.context_sha256.clone(),
            applied_head: applied_head.clone(),
            decision_sha256: decision.id().into(),
            configuration,
            canonical_bytes,
            configuration_sha256,
            build: artifact.clone(),
        })
    }

    /// A metadata binding preflight. Success neither admits, reserves, registers,
    /// stages nor submits this Run; the original scientific gates remain required.
    pub fn bind_next_run(
        &self,
        actual_head: &ResearcherHead,
        prepared: &PreparedResearcherConfiguration,
        run: &Run,
        task: &TaskSpec,
    ) -> Result<()> {
        actual_head.validate()?;
        ensure!(
            prepared.context_sha256 == self.context_sha256
                && prepared.applied_head == *actual_head
                && actual_head.last_decision_sha256.as_deref()
                    == Some(prepared.decision_sha256.as_str()),
            "prepared configuration belongs to a foreign, stale or unapplied head"
        );
        ensure!(
            prepared.configuration.version.id()? == actual_head.version_sha256
                && prepared.configuration.id()? == prepared.configuration_sha256
                && prepared.configuration.canonical_bytes()? == prepared.canonical_bytes,
            "prepared configuration content or adopted version changed"
        );
        ensure!(
            run.configuration_sha256 == prepared.configuration_sha256,
            "next Run does not bind the complete consumed configuration"
        );
        task.validate()?;
        task.profile.validate()?;
        run.admit_build(&prepared.build)?;
        run.admit(task)?;
        ensure!(run.data_manifest_sha256 == prepared.configuration.query.data_view_sha256, "next Run input differs from the consumed query view");
        ensure!(
            prepared
                .configuration
                .version
                .snapshot
                .retrieval
                .information_policy
                .allowed_data_view_sha256
                .contains(&run.data_manifest_sha256),
            "next Run input is outside the adopted information scope"
        );
        let program = prepared
            .build
            .executables
            .iter()
            .find(|program| {
                run.command.first() == Some(&format!("/usr/local/bin/{}", program.name))
            })
            .context("next Run program is not a verified executable")?;
        ensure!(
            prepared
                .configuration
                .version
                .snapshot
                .tools
                .iter()
                .any(|tool| tool.content_sha256 == program.blob.sha256),
            "next Run selects a program absent from the adopted tool snapshot"
        );
        let architecture = match prepared.build.build.target.as_str() {
            "x86_64-unknown-linux-gnu" => "amd64",
            "aarch64-unknown-linux-gnu" => "arm64",
            _ => anyhow::bail!("unsupported next Run Build target"),
        };
        ensure!(
            task.profile.architecture == architecture
                && task.profile.worker_secret.is_none()
                && task.worker_configuration.is_none(),
            "next Run architecture or unverified worker configuration changed"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        build::{BuildArtifact, BuildSpec, BuiltExecutable},
        execution::Profile,
        identity,
        orchestrator::{Artifact, TaskKind},
        release::{
            BuildReleaseReceipt, BuildReleaseTrust, ReleaseProducer, SignedBuildRelease,
            SourceArchive,
        },
    };
    use ed25519_dalek::{Signer, SigningKey};
    use hft_research_agent_contracts::{
        ContentReferenceV1, ExperienceCorpusReferenceV1, ExperienceRankingOrder,
        ExperienceRetrievalV1, InformationPolicyV1, MetaBudgetLimitsV1, MetaCostUsageV1,
        MetaPromotionPolicyV1, MetaResourcesV1, MetaRunBindingV1, MetaTaskV1,
        ResearcherChangeEvidenceV1, ResearcherSnapshotV1,
    };
    use serde_json::json;
    use std::sync::{Arc, Barrier, Mutex};

    fn h(c: char) -> String {
        c.to_string().repeat(64)
    }
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
    fn release(two_programs: bool) -> VerifiedBuildRelease {
        let source = SourceArchive {
            schema: 1,
            code_commit: "a".repeat(40),
            archive: Artifact {
                key: format!("research/sources/{}/source.tar", "a".repeat(40)),
                sha256: sha256(b"fixture source"),
                bytes: 14,
            },
        };
        let binaries = if two_programs {
            vec!["meta-evaluator".into(), "other-evaluator".into()]
        } else {
            vec!["meta-evaluator".into()]
        };
        let build = BuildSpec {
            schema: 2,
            code_commit: source.code_commit.clone(),
            workspace_manifest: "research-core/Cargo.toml".into(),
            source_manifest_sha256: identity(&source).unwrap(),
            cargo_lock_sha256: h('a'),
            toolchain_manifest_sha256: h('b'),
            target: "x86_64-unknown-linux-gnu".into(),
            packages: vec!["meta-fixture".into()],
            binaries,
            features: vec![],
            default_features: false,
            profile: "research".into(),
            profile_manifest_sha256: h('c'),
            rustflags_sha256: h('d'),
            native_environment_sha256: h('e'),
            builder_image: format!("builder@sha256:{}", h('f')),
        };
        let build_id = build.id().unwrap();
        let executables = build
            .binaries
            .iter()
            .enumerate()
            .map(|(index, name)| BuiltExecutable {
                name: name.clone(),
                blob: Artifact {
                    key: format!("research/builds/{build_id}/{name}"),
                    sha256: sha256(format!("fixture program {index}").as_bytes()),
                    bytes: 17,
                },
            })
            .collect::<Vec<_>>();
        let mut artifact = BuildArtifact {
            schema: 1,
            build,
            image: format!("worker@sha256:{}", h('a')),
            executables,
            release_receipt_sha256: h('b'),
        };
        let key = SigningKey::from_bytes(&[17; 32]);
        let mut signed = SignedBuildRelease {
            schema: 1,
            key_id: "release".into(),
            signature_hex: String::new(),
            receipt: BuildReleaseReceipt {
                schema: 1,
                build_sha256: artifact.build.id().unwrap(),
                source,
                image: artifact.image.clone(),
                target: artifact.build.target.clone(),
                executables: artifact.executables.clone(),
                producer: ReleaseProducer {
                    repository: "fixture/monday".into(),
                    workflow_path: ".github/workflows/fixture.yml".into(),
                    source_sha: artifact.build.code_commit.clone(),
                    run_id: 1,
                    run_attempt: 1,
                    job_id: 1,
                },
                publication_readback_sha256: h('c'),
            },
        };
        signed.signature_hex = hex(&key.sign(&signed.signing_bytes().unwrap()).to_bytes());
        artifact.release_receipt_sha256 = identity(&signed).unwrap();
        BuildReleaseTrust {
            schema: 1,
            repository: "fixture/monday".into(),
            producer_workflow_path: ".github/workflows/fixture.yml".into(),
            keys: BTreeMap::from([("release".into(), hex(key.verifying_key().as_bytes()))]),
        }
        .verify(&artifact, &signed)
        .unwrap()
    }

    struct Execution {
        binding: MetaExecutionBindingV1,
        run: Run,
        task: TaskSpec,
    }
    struct Fixture {
        released: VerifiedBuildRelease,
        incumbent: ResearcherVersionV1,
        challenger: ResearcherVersionV1,
        study: MetaStudyV1,
        executions: Vec<Execution>,
        trust: MetaVerificationTrust,
        evaluator_key: SigningKey,
        cost_key: SigningKey,
    }
    impl Fixture {
        fn new(two_programs: bool) -> Self {
            let released = release(two_programs);
            let code = released.artifact().executables[0].blob.sha256.clone();
            let incumbent = ResearcherVersionV1 {
                schema: 1,
                parent_version_sha256: None,
                change_evidence: None,
                snapshot: ResearcherSnapshotV1 {
                    prompt_text: "Return honest bounded research outcomes.".into(),
                    search_policy: json!({"strategy":"fixed"}),
                    retrieval: ExperienceRetrievalV1 {
                        ranking: ExperienceRankingOrder::TaskMatchThenRecency,
                        corpus: vec![ExperienceCorpusReferenceV1 {
                            content: ContentReferenceV1 {
                                id: "development-experience".into(),
                                content_sha256: h('a'),
                            },
                            feedback_phase: MetaTaskPhase::Development,
                        }],
                        query_template: "match the declared task".into(),
                        top_k: 2,
                        max_context_bytes: 4096,
                        information_policy: InformationPolicyV1 {
                            schema: 1,
                            proposal_feedback_phases: vec![MetaTaskPhase::Development],
                            allowed_data_view_sha256: vec![h('1')],
                        },
                    },
                    source_commit: released.artifact().build.code_commit.clone(),
                    build_sha256: released.artifact().id().unwrap(),
                    tools: vec![ContentReferenceV1 {
                        id: "meta-evaluator".into(),
                        content_sha256: code.clone(),
                    }],
                },
            };
            let mut challenger = incumbent.clone();
            challenger.parent_version_sha256 = Some(incumbent.id().unwrap());
            challenger.change_evidence = Some(ResearcherChangeEvidenceV1 {
                content: ContentReferenceV1 {
                    id: "development-change".into(),
                    content_sha256: h('d'),
                },
                phase: MetaTaskPhase::Development,
            });
            challenger.snapshot.retrieval.ranking = ExperienceRankingOrder::RecencyThenTaskMatch;
            let tasks = [
                ("development", MetaTaskPhase::Development, '1'),
                ("selection", MetaTaskPhase::Selection, '2'),
                ("certification", MetaTaskPhase::Certification, '3'),
            ]
            .into_iter()
            .map(|(id, phase, view)| MetaTaskV1 {
                task_id: id.into(),
                phase,
                visibility: phase,
                data_view_sha256: h(view),
                evaluator_code_sha256: code.clone(),
                scoring_rule: json!({"metric":"correct-bounded-outcomes"}),
                score_direction: MetaScoreDirectionV1::HigherIsBetter,
                seeds: vec![7, 11],
            })
            .collect::<Vec<_>>();
            let mut study = MetaStudyV1 {
                schema: 1,
                incumbent_version_sha256: incumbent.id().unwrap(),
                challenger_version_sha256: challenger.id().unwrap(),
                tasks,
                runs: vec![],
                grant_sha256: h('a'),
                budget_scope_sha256: h('b'),
                resources: MetaResourcesV1 {
                    cpu_millis: 1000,
                    memory_mib: 256,
                    timeout_ms: 1000,
                },
                per_arm_budget: MetaBudgetLimitsV1 {
                    max_trials: 12,
                    max_job_attempts: 6,
                    max_llm_tokens: 100,
                    max_cost_microusd: 1000,
                },
                promotion_policy: MetaPromotionPolicyV1 {
                    min_selection_score_gain: 0.1,
                    min_certification_score_gain: 0.1,
                },
            };
            let mut executions = Vec::new();
            for meta_task in &study.tasks {
                for seed in &meta_task.seeds {
                    for (arm, version) in [
                        (MetaStudyArm::Incumbent, &incumbent),
                        (MetaStudyArm::Challenger, &challenger),
                    ] {
                        let run = Run {
                            schema: 1,
                            experiment_sha256: h('a'),
                            kind: TaskKind::Backtest,
                            build_artifact_sha256: released.artifact().id().unwrap(),
                            configuration_sha256: version.id().unwrap(),
                            command: vec![
                                "/usr/local/bin/meta-evaluator".into(),
                                "--config".into(),
                                "/work/researcher.json".into(),
                            ],
                            code_commit: released.artifact().build.code_commit.clone(),
                            source_manifest_sha256: released
                                .artifact()
                                .build
                                .source_manifest_sha256
                                .clone(),
                            image: released.artifact().image.clone(),
                            data_manifest_sha256: meta_task.data_view_sha256.clone(),
                            seed: *seed,
                            evaluator_sha256: code.clone(),
                            evaluation_protocol_sha256: meta_task.scoring_rule_sha256().unwrap(),
                            fit_identity_sha256: None,
                        };
                        let frozen = MetaRunBindingV1 {
                            task_id: meta_task.task_id.clone(),
                            phase: meta_task.phase,
                            arm,
                            version_sha256: version.id().unwrap(),
                            run_sha256: run.id().unwrap(),
                            seed: *seed,
                        };
                        let task = TaskSpec {
                            schema: 1,
                            kind: run.kind,
                            run_manifest_sha256: run.id().unwrap(),
                            view_manifest_sha256: run.data_manifest_sha256.clone(),
                            source_sha256: run.source_manifest_sha256.clone(),
                            image: run.image.clone(),
                            command: run.command.clone(),
                            profile: Profile {
                                backend: Backend::KubernetesJob,
                                cluster: "fixture".into(),
                                namespace: "monday-research".into(),
                                service_account: "worker".into(),
                                architecture: "amd64".into(),
                                cpu_millis: 1000,
                                memory_mib: 256,
                                scratch_mib: 64,
                                gpu: 0,
                                acceptance_sha256: h('f'),
                                prepared_pvc: None,
                                worker_secret: None,
                            },
                            timeout_ms: 1000,
                            max_attempts: 1,
                            output_prefix: "research/meta-results".into(),
                            fit_identity_sha256: None,
                            worker_configuration: None,
                        };
                        study.runs.push(frozen.clone());
                        executions.push(Execution {
                            binding: MetaExecutionBindingV1 {
                                study_sha256: h('a'),
                                run: frozen,
                                task_sha256: meta_task.id().unwrap(),
                                attempt: 1,
                                fence: executions.len() as u64 + 10,
                            },
                            run,
                            task,
                        });
                    }
                }
            }
            let evaluator_key = SigningKey::from_bytes(&[19; 32]);
            let cost_key = SigningKey::from_bytes(&[29; 32]);
            let trust = Self::trust(&evaluator_key, &cost_key);
            let mut fixture = Self {
                released,
                incumbent,
                challenger,
                study,
                executions,
                trust,
                evaluator_key,
                cost_key,
            };
            fixture.rebind();
            fixture
        }
        fn trust(evaluator: &SigningKey, cost: &SigningKey) -> MetaVerificationTrust {
            MetaVerificationTrust::from_role_keys(
                BTreeMap::from([("evaluator".into(), evaluator.verifying_key().to_bytes())]),
                BTreeMap::from([("cost".into(), cost.verifying_key().to_bytes())]),
            )
            .unwrap()
        }
        // Keep changed fixture identities internally valid so rejection exercises
        // the actual frozen-context rule rather than a stale hand-authored SHA.
        fn rebind(&mut self) {
            self.challenger.parent_version_sha256 = Some(self.incumbent.id().unwrap());
            self.study.incumbent_version_sha256 = self.incumbent.id().unwrap();
            self.study.challenger_version_sha256 = self.challenger.id().unwrap();
            for (index, execution) in self.executions.iter_mut().enumerate() {
                let version = match execution.binding.run.arm {
                    MetaStudyArm::Incumbent => &self.incumbent,
                    MetaStudyArm::Challenger => &self.challenger,
                };
                execution.run.configuration_sha256 = version.id().unwrap();
                let meta_task = self
                    .study
                    .tasks
                    .iter()
                    .find(|task| task.task_id == execution.binding.run.task_id)
                    .unwrap();
                execution.run.evaluation_protocol_sha256 = meta_task.scoring_rule_sha256().unwrap();
                execution.binding.task_sha256 = meta_task.id().unwrap();
                execution.binding.run.version_sha256 = version.id().unwrap();
                execution.binding.run.run_sha256 = execution.run.id().unwrap();
                execution.task.run_manifest_sha256 = execution.run.id().unwrap();
                execution.task.command.clone_from(&execution.run.command);
                execution.task.kind = execution.run.kind;
                execution
                    .task
                    .fit_identity_sha256
                    .clone_from(&execution.run.fit_identity_sha256);
                self.study.runs[index] = execution.binding.run.clone();
            }
            let study_id = self.study.id().unwrap();
            for execution in &mut self.executions {
                execution.binding.study_sha256.clone_from(&study_id);
            }
        }
        fn context_with<'a>(
            &'a self,
            trust: &'a MetaVerificationTrust,
        ) -> Result<MetaVerificationContext<'a>> {
            MetaVerificationContext::new(
                &self.study,
                trust,
                &self.incumbent,
                &self.challenger,
                self.executions
                    .iter()
                    .map(|execution| ExpectedMetaExecution {
                        binding: &execution.binding,
                        run: &execution.run,
                        task: &execution.task,
                        released_build: &self.released,
                    })
                    .collect(),
            )
        }
        fn context(&self) -> MetaVerificationContext<'_> {
            self.context_with(&self.trust).unwrap()
        }
        fn payloads(
            &self,
            index: usize,
        ) -> (SignedMetaEvaluationV1, Vec<u8>, SignedMetaCostV1, Vec<u8>) {
            let execution = &self.executions[index];
            let task = self
                .study
                .tasks
                .iter()
                .find(|task| task.task_id == execution.binding.run.task_id)
                .unwrap();
            let score = if execution.binding.run.arm == MetaStudyArm::Incumbent {
                0.2
            } else {
                0.5
            };
            let result =
                serde_json::to_vec(&json!({"score":score,"fixture":"actual readback bytes"}))
                    .unwrap();
            let usage = MetaCostUsageV1 {
                trials: 1,
                job_attempts: 1,
                wall_ms: 10,
                llm_tokens: 0,
                cost_microusd: 10,
            };
            let cost_bytes = serde_json::to_vec(&usage).unwrap();
            let prefix = format!(
                "{}/{}/1",
                execution.task.output_prefix,
                execution.task.id().unwrap()
            );
            let evaluated = SignedMetaEvaluationV1 {
                key_id: "evaluator".into(),
                signature_hex: String::new(),
                payload: MetaEvaluationV1 {
                    schema: 1,
                    binding: execution.binding.clone(),
                    data_view_sha256: task.data_view_sha256.clone(),
                    visibility: task.visibility,
                    evaluator_code_sha256: task.evaluator_code_sha256.clone(),
                    scoring_rule_sha256: task.scoring_rule_sha256().unwrap(),
                    grant_sha256: self.study.grant_sha256.clone(),
                    budget_scope_sha256: self.study.budget_scope_sha256.clone(),
                    resources: self.study.resources.clone(),
                    outcome: MetaEvaluationOutcomeV1::Succeeded,
                    score: Some(score),
                    failure_code: None,
                    result_artifact: MetaArtifactReferenceV1 {
                        key: format!("{prefix}/evaluation.json"),
                        sha256: sha256(&result),
                        bytes: result.len() as u64,
                    },
                    result_readback_sha256: sha256(&result),
                },
            };
            let cost = SignedMetaCostV1 {
                key_id: "cost".into(),
                signature_hex: String::new(),
                payload: MetaCostV1 {
                    schema: 1,
                    binding: execution.binding.clone(),
                    grant_sha256: self.study.grant_sha256.clone(),
                    budget_scope_sha256: self.study.budget_scope_sha256.clone(),
                    resources: self.study.resources.clone(),
                    usage,
                    evaluated_result_sha256: sha256(&result),
                    cost_artifact: MetaArtifactReferenceV1 {
                        key: format!("{prefix}/cost.json"),
                        sha256: sha256(&cost_bytes),
                        bytes: cost_bytes.len() as u64,
                    },
                    cost_readback_sha256: sha256(&cost_bytes),
                },
            };
            (evaluated, result, cost, cost_bytes)
        }
        fn signed(
            &self,
            mut evaluated: SignedMetaEvaluationV1,
            mut cost: SignedMetaCostV1,
            evaluator_key: &SigningKey,
            cost_key: &SigningKey,
        ) -> (Vec<u8>, Vec<u8>) {
            evaluated.signature_hex = hex(&evaluator_key
                .sign(&evaluated.signing_bytes().unwrap())
                .to_bytes());
            cost.signature_hex = hex(&cost_key.sign(&cost.signing_bytes().unwrap()).to_bytes());
            (
                serde_json::to_vec(&evaluated).unwrap(),
                serde_json::to_vec(&cost).unwrap(),
            )
        }
        fn all(&self, context: &MetaVerificationContext<'_>) -> Vec<VerifiedMetaEvaluation> {
            (0..self.executions.len())
                .map(|index| {
                    let (evaluated, result, cost, cost_bytes) = self.payloads(index);
                    let (eval_wire, cost_wire) =
                        self.signed(evaluated, cost, &self.evaluator_key, &self.cost_key);
                    context
                        .verify_evaluation(&eval_wire, &result, &cost_wire, &cost_bytes)
                        .unwrap()
                })
                .collect()
        }
        fn head(&self) -> ResearcherHead {
            ResearcherHead::new(self.incumbent.id().unwrap(), 4).unwrap()
        }
    }

    #[test]
    fn signed_complete_matrix_adopts_once_and_rejection_preserves_parent() {
        let fixture = Fixture::new(false);
        let context = fixture.context();
        let evidence = fixture.all(&context);
        let mut head = fixture.head();
        let decision = context.verify_promotion(&head, &evidence).unwrap();
        assert_eq!(decision.outcome(), PromotionOutcome::Adopt);
        assert_eq!(
            apply_expected_head(&context, &mut head, &decision).unwrap(),
            HeadTransition::Adopted
        );
        assert_eq!(head.version_sha256(), fixture.challenger.id().unwrap());
        assert_eq!(head.revision(), 5);
        assert_eq!(
            apply_expected_head(&context, &mut head, &decision).unwrap(),
            HeadTransition::Reused
        );
        let before = head.clone();
        head.revision += 1;
        assert!(apply_expected_head(&context, &mut head, &decision).is_err());
        assert_eq!(head.version_sha256, before.version_sha256);

        let mut failed = fixture.all(&context);
        let (mut evaluated, result, cost, cost_bytes) = fixture.payloads(0);
        evaluated.payload.outcome = MetaEvaluationOutcomeV1::Failed;
        evaluated.payload.score = None;
        evaluated.payload.failure_code = Some("honest-negative".into());
        let (wire, cost_wire) =
            fixture.signed(evaluated, cost, &fixture.evaluator_key, &fixture.cost_key);
        failed[0] = context
            .verify_evaluation(&wire, &result, &cost_wire, &cost_bytes)
            .unwrap();
        assert_eq!(failed[0].cost().usage.cost_microusd, 10);
        let mut parent = fixture.head();
        let before = parent.clone();
        let rejected = context.verify_promotion(&parent, &failed).unwrap();
        assert_eq!(
            apply_expected_head(&context, &mut parent, &rejected).unwrap(),
            HeadTransition::Rejected(PromotionRejection::FailedEvaluation)
        );
        assert_eq!(parent, before);
    }

    #[test]
    fn raw_json_unknown_role_keys_and_changed_actual_bytes_cannot_mint_evidence() {
        let fixture = Fixture::new(false);
        let context = fixture.context();
        let (evaluated, result, cost, cost_bytes) = fixture.payloads(0);
        let (wire, cost_wire) = fixture.signed(
            evaluated.clone(),
            cost.clone(),
            &fixture.evaluator_key,
            &fixture.cost_key,
        );
        assert!(context
            .verify_evaluation(
                &serde_json::to_vec(&evaluated.payload).unwrap(),
                &result,
                &cost_wire,
                &cost_bytes
            )
            .is_err());
        assert!(context
            .verify_evaluation(&wire, b"different actual bytes", &cost_wire, &cost_bytes)
            .is_err());
        assert!(context
            .verify_evaluation(&wire, &result, &cost_wire, b"different cost bytes")
            .is_err());
        let forged_key = SigningKey::from_bytes(&[39; 32]);
        let (forged, forged_cost) =
            fixture.signed(evaluated.clone(), cost.clone(), &forged_key, &forged_key);
        assert!(context
            .verify_evaluation(&forged, &result, &forged_cost, &cost_bytes)
            .is_err());
        let mut self_key: serde_json::Value = serde_json::from_slice(&forged).unwrap();
        self_key["public_key"] = json!(hex(forged_key.verifying_key().as_bytes()));
        assert!(context
            .verify_evaluation(
                &serde_json::to_vec(&self_key).unwrap(),
                &result,
                &cost_wire,
                &cost_bytes
            )
            .is_err());
        assert!(MetaVerificationTrust::from_role_keys(
            BTreeMap::from([("evaluator".into(), forged_key.verifying_key().to_bytes())]),
            BTreeMap::from([("cost".into(), forged_key.verifying_key().to_bytes())])
        )
        .is_err());
        let mut swapped_cost = cost;
        swapped_cost.key_id = "evaluator".into();
        let (wire, swapped) = fixture.signed(
            evaluated,
            swapped_cost,
            &fixture.evaluator_key,
            &fixture.evaluator_key,
        );
        assert!(context
            .verify_evaluation(&wire, &result, &swapped, &cost_bytes)
            .is_err());
    }

    #[test]
    fn foreign_trust_cannot_cross_mint_into_good_promotion_or_head() {
        let fixture = Fixture::new(false);
        let good = fixture.context();
        let evaluator = SigningKey::from_bytes(&[40; 32]);
        let cost_key = SigningKey::from_bytes(&[41; 32]);
        let foreign_trust = Fixture::trust(&evaluator, &cost_key);
        let foreign = fixture.context_with(&foreign_trust).unwrap();
        let evidence = (0..fixture.executions.len())
            .map(|index| {
                let (eval, result, cost, cost_bytes) = fixture.payloads(index);
                let (wire, cost_wire) = fixture.signed(eval, cost, &evaluator, &cost_key);
                foreign
                    .verify_evaluation(&wire, &result, &cost_wire, &cost_bytes)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let mut head = fixture.head();
        assert!(good.verify_promotion(&head, &evidence).is_err());
        let counterfeit = foreign.verify_promotion(&head, &evidence).unwrap();
        let before = head.clone();
        assert!(apply_expected_head(&good, &mut head, &counterfeit).is_err());
        assert_eq!(head, before);
    }

    #[test]
    fn phase_attempt_fence_source_budget_and_matrix_drift_are_rejected() {
        let fixture = Fixture::new(false);
        let context = fixture.context();
        let (original, result, original_cost, cost_bytes) = fixture.payloads(0);
        for change in [
            "attempt",
            "fence",
            "phase",
            "source",
            "grant",
            "view",
            "budget",
            "missing-cost",
        ] {
            let mut evaluated = original.clone();
            let mut cost = original_cost.clone();
            match change {
                "attempt" => {
                    evaluated.payload.binding.attempt = 2;
                    cost.payload.binding.attempt = 2;
                }
                "fence" => {
                    evaluated.payload.binding.fence += 1;
                    cost.payload.binding.fence += 1;
                }
                "phase" => {
                    evaluated.payload.binding.run.phase = MetaTaskPhase::Certification;
                    evaluated.payload.visibility = MetaTaskPhase::Certification;
                    cost.payload.binding.run.phase = MetaTaskPhase::Certification;
                }
                "source" => evaluated.payload.evaluator_code_sha256 = h('f'),
                "grant" => evaluated.payload.grant_sha256 = h('f'),
                "view" => evaluated.payload.data_view_sha256 = h('f'),
                "budget" => {
                    cost.payload.usage.cost_microusd =
                        fixture.study.per_arm_budget.max_cost_microusd + 1
                }
                _ => {}
            }
            if change == "attempt" {
                // Invalid shape is rejected before a signer can emit a retry receipt.
                assert!(evaluated.signing_bytes().is_err());
                continue;
            }
            let (wire, mut cost_wire) =
                fixture.signed(evaluated, cost, &fixture.evaluator_key, &fixture.cost_key);
            if change == "missing-cost" {
                cost_wire.clear();
            }
            assert!(
                context
                    .verify_evaluation(&wire, &result, &cost_wire, &cost_bytes)
                    .is_err(),
                "{change}"
            );
        }
        let mut matrix = fixture.all(&context);
        matrix.pop();
        assert!(context.verify_promotion(&fixture.head(), &matrix).is_err());
        let duplicate = fixture.all(&context).remove(0);
        matrix.push(duplicate);
        assert!(context.verify_promotion(&fixture.head(), &matrix).is_err());
    }

    #[test]
    fn only_ranking_changes_and_matching_run_profiles_are_eligible() {
        for change in [
            "prompt",
            "policy",
            "corpus",
            "context",
            "tool",
            "command",
            "kind",
            "fit",
            "scratch",
            "architecture",
            "service-account",
            "worker-secret",
        ] {
            let mut fixture = Fixture::new(false);
            match change {
                "prompt" => fixture.challenger.snapshot.prompt_text.push_str(" changed"),
                "policy" => {
                    fixture.challenger.snapshot.search_policy = json!({"strategy":"changed"})
                }
                "corpus" => {
                    fixture.challenger.snapshot.retrieval.corpus[0]
                        .content
                        .content_sha256 = h('f')
                }
                "context" => fixture.challenger.snapshot.retrieval.max_context_bytes += 1,
                "tool" => fixture.challenger.snapshot.tools[0].content_sha256 = h('f'),
                "command" => fixture.executions[1]
                    .run
                    .command
                    .push("--changed-policy".into()),
                "kind" => fixture.executions[1].run.kind = TaskKind::Train,
                "fit" => fixture.executions[1].run.fit_identity_sha256 = Some(h('f')),
                "scratch" => fixture.executions[1].task.profile.scratch_mib += 1,
                "architecture" => fixture.executions[1].task.profile.architecture = "arm64".into(),
                "service-account" => {
                    fixture.executions[1].task.profile.service_account = "different".into()
                }
                "worker-secret" => {
                    fixture.executions[1].task.profile.worker_secret = Some("not-verified".into())
                }
                _ => unreachable!(),
            }
            fixture.rebind();
            assert!(fixture.context_with(&fixture.trust).is_err(), "{change}");
        }
    }

    #[test]
    fn hidden_view_relabeling_and_wrong_invoked_published_program_are_rejected() {
        for view in [h('2'), h('3')] {
            let mut fixture = Fixture::new(false);
            fixture
                .incumbent
                .snapshot
                .retrieval
                .information_policy
                .allowed_data_view_sha256
                .push(view.clone());
            fixture
                .challenger
                .snapshot
                .retrieval
                .information_policy
                .allowed_data_view_sha256
                .push(view);
            fixture.rebind();
            assert!(fixture.context_with(&fixture.trust).is_err());
        }
        let mut fixture = Fixture::new(true);
        assert!(fixture.context_with(&fixture.trust).is_ok());
        for execution in &mut fixture.executions {
            execution.run.command[0] = "/usr/local/bin/other-evaluator".into();
        }
        fixture.rebind();
        // Both binaries are genuinely release-verified; the invoked one is B,
        // while the frozen evaluator SHA still refers to published program A.
        assert!(fixture.context_with(&fixture.trust).is_err());
    }

    #[test]
    fn fixed_threshold_cost_totals_and_finite_pairing_precede_adoption() {
        let mut fixture = Fixture::new(false);
        let original_id = fixture.study.id().unwrap();
        fixture.study.promotion_policy.min_certification_score_gain = 0.4;
        fixture.rebind();
        assert_ne!(original_id, fixture.study.id().unwrap());
        let context = fixture.context();
        let mut head = fixture.head();
        let before = head.clone();
        let decision = context
            .verify_promotion(&head, &fixture.all(&context))
            .unwrap();
        assert_eq!(
            apply_expected_head(&context, &mut head, &decision).unwrap(),
            HeadTransition::Rejected(PromotionRejection::InsufficientGain)
        );
        assert_eq!(head, before);

        let fixture = Fixture::new(false);
        let context = fixture.context();
        let mut evidence = fixture.all(&context);
        for (index, slot) in evidence.iter_mut().enumerate() {
            let (evaluated, result, mut cost, _) = fixture.payloads(index);
            cost.payload.usage.cost_microusd = 200;
            let actual_cost = serde_json::to_vec(&cost.payload.usage).unwrap();
            cost.payload.cost_artifact.sha256 = sha256(&actual_cost);
            cost.payload.cost_artifact.bytes = actual_cost.len() as u64;
            cost.payload.cost_readback_sha256 = sha256(&actual_cost);
            let (wire, cost_wire) =
                fixture.signed(evaluated, cost, &fixture.evaluator_key, &fixture.cost_key);
            *slot = context
                .verify_evaluation(&wire, &result, &cost_wire, &actual_cost)
                .unwrap();
        }
        let decision = context
            .verify_promotion(&fixture.head(), &evidence)
            .unwrap();
        assert_eq!(
            decision.outcome(),
            PromotionOutcome::Rejected(PromotionRejection::BudgetOverrun)
        );
        for (index, slot) in evidence.iter_mut().enumerate() {
            let (mut evaluated, result, cost, cost_bytes) = fixture.payloads(index);
            evaluated.payload.score = Some(
                if evaluated.payload.binding.run.arm == MetaStudyArm::Incumbent {
                    -f64::MAX
                } else {
                    f64::MAX
                },
            );
            let (wire, cost_wire) =
                fixture.signed(evaluated, cost, &fixture.evaluator_key, &fixture.cost_key);
            *slot = context
                .verify_evaluation(&wire, &result, &cost_wire, &cost_bytes)
                .unwrap();
        }
        assert!(context
            .verify_promotion(&fixture.head(), &evidence)
            .is_err());
    }

    #[test]
    fn development_scope_and_usable_output_prefix_are_required() {
        let mut fixture = Fixture::new(false);
        assert!(fixture.context_with(&fixture.trust).is_ok());
        fixture
            .incumbent
            .snapshot
            .retrieval
            .information_policy
            .allowed_data_view_sha256 = vec![h('4')];
        fixture
            .challenger
            .snapshot
            .retrieval
            .information_policy
            .allowed_data_view_sha256 = vec![h('4')];
        fixture.rebind();
        assert!(fixture
            .context_with(&fixture.trust)
            .err()
            .unwrap()
            .to_string()
            .contains("development view"));
        for prefix in ["research/meta-results/", "research//meta-results"] {
            let mut fixture = Fixture::new(false);
            for execution in &mut fixture.executions {
                execution.task.output_prefix = prefix.into();
            }
            fixture.rebind();
            assert!(fixture
                .context_with(&fixture.trust)
                .err()
                .unwrap()
                .to_string()
                .contains("empty segment"));
        }
    }

    #[test]
    fn lower_error_is_an_improvement_and_direction_is_frozen() {
        let mut fixture = Fixture::new(false);
        let old_protocol = fixture.study.tasks[0].scoring_rule_sha256().unwrap();
        for task in &mut fixture.study.tasks {
            task.score_direction = MetaScoreDirectionV1::LowerIsBetter;
        }
        assert_ne!(
            old_protocol,
            fixture.study.tasks[0].scoring_rule_sha256().unwrap()
        );
        fixture.rebind();
        let context = fixture.context();
        let worse = context
            .verify_promotion(&fixture.head(), &fixture.all(&context))
            .unwrap();
        assert_eq!(
            worse.outcome(),
            PromotionOutcome::Rejected(PromotionRejection::InsufficientGain)
        );
        let evidence = (0..fixture.executions.len())
            .map(|index| {
                let (mut evaluated, _, mut cost, cost_bytes) = fixture.payloads(index);
                let score = if evaluated.payload.binding.run.arm == MetaStudyArm::Incumbent {
                    0.2
                } else {
                    0.0
                };
                evaluated.payload.score = Some(score);
                let result = serde_json::to_vec(&json!({"error":score})).unwrap();
                evaluated.payload.result_artifact.sha256 = sha256(&result);
                evaluated.payload.result_artifact.bytes = result.len() as u64;
                evaluated.payload.result_readback_sha256 = sha256(&result);
                cost.payload.evaluated_result_sha256 = sha256(&result);
                let (wire, cost_wire) =
                    fixture.signed(evaluated, cost, &fixture.evaluator_key, &fixture.cost_key);
                context
                    .verify_evaluation(&wire, &result, &cost_wire, &cost_bytes)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            context
                .verify_promotion(&fixture.head(), &evidence)
                .unwrap()
                .outcome(),
            PromotionOutcome::Adopt
        );
    }

    #[test]
    fn distinct_valid_prior_decisions_produce_distinct_successor_decisions() {
        let first = Fixture::new(false);
        let mut other = Fixture::new(false);
        other.study.grant_sha256 = h('f');
        other.rebind();
        let first_context = first.context();
        let other_context = other.context();
        let mut first_head = first.head();
        let mut other_head = other.head();
        let first_decision = first_context
            .verify_promotion(&first_head, &first.all(&first_context))
            .unwrap();
        let other_decision = other_context
            .verify_promotion(&other_head, &other.all(&other_context))
            .unwrap();
        apply_expected_head(&first_context, &mut first_head, &first_decision).unwrap();
        apply_expected_head(&other_context, &mut other_head, &other_decision).unwrap();
        assert_eq!(first_head.version_sha256(), other_head.version_sha256());
        assert_eq!(first_head.revision(), other_head.revision());
        assert_ne!(
            first_head.last_decision_sha256,
            other_head.last_decision_sha256
        );
        let mut next = Fixture::new(false);
        next.incumbent = first.challenger.clone();
        next.challenger = next.incumbent.clone();
        next.challenger.snapshot.retrieval.ranking = ExperienceRankingOrder::TaskMatchThenRecency;
        next.rebind();
        let next_context = next.context();
        let evidence = next.all(&next_context);
        let decision_a = next_context
            .verify_promotion(&first_head, &evidence)
            .unwrap();
        let decision_b = next_context
            .verify_promotion(&other_head, &evidence)
            .unwrap();
        assert_ne!(decision_a.id(), decision_b.id());
        let before = other_head.clone();
        assert!(apply_expected_head(&next_context, &mut other_head, &decision_a).is_err());
        assert_eq!(other_head, before);
        assert_eq!(
            apply_expected_head(&next_context, &mut other_head, &decision_b).unwrap(),
            HeadTransition::Adopted
        );
    }

    #[test]
    fn arithmetic_overflow_never_changes_the_incumbent() {
        let fixture = Fixture::new(false);
        let context = fixture.context();
        let mut head = ResearcherHead::new(fixture.incumbent.id().unwrap(), u64::MAX).unwrap();
        let before = head.clone();
        let decision = context
            .verify_promotion(&head, &fixture.all(&context))
            .unwrap();
        assert_eq!(decision.outcome(), PromotionOutcome::Adopt);
        let error = apply_expected_head(&context, &mut head, &decision).unwrap_err();
        assert!(error.to_string().contains("head revision overflow"));
        assert_eq!(head, before);

        let mut fixture = Fixture::new(false);
        fixture.study.per_arm_budget.max_cost_microusd = u64::MAX;
        fixture.rebind();
        let context = fixture.context();
        let evidence = (0..fixture.executions.len())
            .map(|index| {
                let (evaluated, result, mut cost, _) = fixture.payloads(index);
                cost.payload.usage.cost_microusd = u64::MAX;
                let cost_bytes = serde_json::to_vec(&cost.payload.usage).unwrap();
                cost.payload.cost_artifact.sha256 = sha256(&cost_bytes);
                cost.payload.cost_artifact.bytes = cost_bytes.len() as u64;
                cost.payload.cost_readback_sha256 = sha256(&cost_bytes);
                let (wire, cost_wire) =
                    fixture.signed(evaluated, cost, &fixture.evaluator_key, &fixture.cost_key);
                context
                    .verify_evaluation(&wire, &result, &cost_wire, &cost_bytes)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let head = fixture.head();
        let error = context.verify_promotion(&head, &evidence).err().unwrap();
        assert!(error.to_string().contains("meta cost sum overflow"));

        let fixture = Fixture::new(false);
        let context = fixture.context();
        let evidence = (0..fixture.executions.len())
            .map(|index| {
                let (mut evaluated, _, mut cost, cost_bytes) = fixture.payloads(index);
                let score = if evaluated.payload.binding.run.arm == MetaStudyArm::Incumbent {
                    0.0
                } else {
                    f64::MAX * 0.75
                };
                assert!(score.is_finite());
                evaluated.payload.score = Some(score);
                let result = serde_json::to_vec(&json!({"score":score})).unwrap();
                evaluated.payload.result_artifact.sha256 = sha256(&result);
                evaluated.payload.result_artifact.bytes = result.len() as u64;
                evaluated.payload.result_readback_sha256 = sha256(&result);
                cost.payload.evaluated_result_sha256 = sha256(&result);
                let (wire, cost_wire) =
                    fixture.signed(evaluated, cost, &fixture.evaluator_key, &fixture.cost_key);
                context
                    .verify_evaluation(&wire, &result, &cost_wire, &cost_bytes)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let error = context
            .verify_promotion(&fixture.head(), &evidence)
            .err()
            .unwrap();
        assert!(error.to_string().contains("phase score sum is not finite"));
    }

    #[test]
    fn competing_verified_decisions_have_one_atomic_winner_and_no_rollback() {
        let first = Fixture::new(false);
        let mut second = Fixture::new(false);
        second
            .challenger
            .change_evidence
            .as_mut()
            .unwrap()
            .content
            .content_sha256 = h('f');
        second.rebind();
        let first_context = first.context();
        let second_context = second.context();
        let first_decision = first_context
            .verify_promotion(&first.head(), &first.all(&first_context))
            .unwrap();
        let second_decision = second_context
            .verify_promotion(&second.head(), &second.all(&second_context))
            .unwrap();
        let head = Arc::new(Mutex::new(first.head()));
        let barrier = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            let results = [
                (&first_context, &first_decision),
                (&second_context, &second_decision),
            ]
            .into_iter()
            .map(|(context, decision)| {
                let head = Arc::clone(&head);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    apply_expected_head(context, &mut head.lock().unwrap(), decision)
                })
            })
            .collect::<Vec<_>>();
            let results = results
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                results
                    .iter()
                    .filter(|result| matches!(result, Ok(HeadTransition::Adopted)))
                    .count(),
                1
            );
            assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        });
        let after = head.lock().unwrap().clone();
        assert_eq!(after.revision(), 5);
        let loser = if after.version_sha256() == first.challenger.id().unwrap() {
            (&second_context, &second_decision)
        } else {
            (&first_context, &first_decision)
        };
        assert!(apply_expected_head(loser.0, &mut head.lock().unwrap(), loser.1).is_err());
        assert_eq!(*head.lock().unwrap(), after);
    }

    fn consumption_inputs() -> (
        Fixture,
        Vec<hft_research_agent_contracts::consumption::FrozenExperienceCorpusV1>,
        hft_research_agent_contracts::consumption::ExperienceQueryV1,
    ) {
        use hft_research_agent_contracts::consumption::{
            ExperienceEvidenceV1, ExperienceQueryV1, FrozenExperienceCorpusV1, ResearchExperienceV1,
        };
        let query = ExperienceQueryV1 {
            task_context_sha256: h('d'),
            data_view_sha256: h('1'),
            as_of_ns: 100,
            query_text: "retrieve the frozen development experience".into(),
        };
        let entries = [("older-match", h('d'), 10), ("newer-other", h('e'), 20)]
            .into_iter()
            .map(|(id, task, time)| {
                let source = ExperienceEvidenceV1 {
                    content: ContentReferenceV1 {
                        id: format!("source-{id}"),
                        content_sha256: h('c'),
                    },
                    phase: MetaTaskPhase::Development,
                    data_view_sha256: h('1'),
                    available_ns: time,
                };
                ResearchExperienceV1 {
                    id: id.into(),
                    task_context_sha256: task,
                    observed_ns: time,
                    available_ns: time,
                    text: format!("bounded development experience {id}"),
                    source: source.clone(),
                    evidence: vec![source],
                }
            })
            .collect();
        let corpus = FrozenExperienceCorpusV1 {
            id: "development-experience".into(),
            entries,
        };
        let mut fixture = Fixture::new(false);
        for version in [&mut fixture.incumbent, &mut fixture.challenger] {
            version.snapshot.retrieval.corpus[0].content = corpus.content_reference().unwrap();
            version.snapshot.retrieval.top_k = 1;
        }
        fixture.rebind();
        (fixture, vec![corpus], query)
    }

    #[test]
    fn applied_configuration_actual_bytes_drive_leaf_and_bind_next_run() {
        use hft_research_agent_contracts::consumption::ResearcherConsumptionConfigV1;
        let (mut fixture, corpora, query) = consumption_inputs();
        for version in [&mut fixture.incumbent, &mut fixture.challenger] {
            version.snapshot.retrieval.information_policy.allowed_data_view_sha256.push(h('4'));
        }
        fixture.rebind();
        let context = fixture.context();
        let mut head = fixture.head();
        let decision = context
            .verify_promotion(&head, &fixture.all(&context))
            .unwrap();
        assert!(context
            .prepare_configuration(
                &decision,
                &head,
                PrepareResearcherConfigurationRequest {
                    candidate: &fixture.challenger,
                    released_build: &fixture.released,
                    query: query.clone(),
                    corpora: corpora.clone(),
                }
            )
            .is_err());
        apply_expected_head(&context, &mut head, &decision).unwrap();
        let prepared = context
            .prepare_configuration(
                &decision,
                &head,
                PrepareResearcherConfigurationRequest {
                    candidate: &fixture.challenger,
                    released_build: &fixture.released,
                    query: query.clone(),
                    corpora: corpora.clone(),
                },
            )
            .unwrap();
        assert_eq!(sha256(prepared.bytes()), prepared.id());
        assert_eq!(prepared.version_sha256(), fixture.challenger.id().unwrap());
        assert_ne!(prepared.id(), prepared.version_sha256());
        let mut run = fixture.executions[1].run.clone();
        run.configuration_sha256 = prepared.id().into();
        let mut task = fixture.executions[1].task.clone();
        task.run_manifest_sha256 = run.id().unwrap();
        context
            .bind_next_run(&head, &prepared, &run, &task)
            .unwrap();
        let observed = hft_research_agent_improvement::consume_config(
            prepared.bytes(),
            &run.configuration_sha256,
        )
        .unwrap();
        assert_eq!(observed.receipt.configuration_sha256, prepared.id());
        assert_eq!(
            observed.receipt.researcher_version_sha256,
            head.version_sha256()
        );
        assert_eq!(observed.receipt.selected_experience_ids, ["newer-other"]);
        assert_eq!(
            observed.context.prompt_text,
            fixture.challenger.snapshot.prompt_text
        );
        let baseline = ResearcherConsumptionConfigV1 {
            schema: 1,
            version: fixture.incumbent.clone(),
            query,
            corpora,
        };
        let old_usage = hft_research_agent_improvement::consume_config(
            &baseline.canonical_bytes().unwrap(),
            &baseline.id().unwrap(),
        )
        .unwrap();
        assert_eq!(old_usage.receipt.selected_experience_ids, ["older-match"]);
        assert_ne!(
            observed.receipt.context_sha256,
            old_usage.receipt.context_sha256
        );
        // This is actual local configuration consumption, not a submitted Run,
        // budget admission, persisted incumbent or scientific result.
    }

    #[test]
    fn configuration_preparation_rejects_false_adoption_stale_parent_trust_and_build() {
        let (fixture, corpora, query) = consumption_inputs();
        let context = fixture.context();
        let mut head = fixture.head();
        let decision = context
            .verify_promotion(&head, &fixture.all(&context))
            .unwrap();
        let fake_adopted =
            ResearcherHead::new(fixture.challenger.id().unwrap(), head.revision() + 1).unwrap();
        assert!(context
            .prepare_configuration(
                &decision,
                &fake_adopted,
                PrepareResearcherConfigurationRequest {
                    candidate: &fixture.challenger,
                    released_build: &fixture.released,
                    query: query.clone(),
                    corpora: corpora.clone(),
                }
            )
            .is_err());
        apply_expected_head(&context, &mut head, &decision).unwrap();
        let before = head.clone();
        for (candidate, stale) in [
            (&fixture.incumbent, head.clone()),
            (
                &fixture.challenger,
                ResearcherHead::new(head.version_sha256().into(), head.revision() + 1).unwrap(),
            ),
        ] {
            assert!(context
                .prepare_configuration(
                    &decision,
                    &stale,
                    PrepareResearcherConfigurationRequest {
                        candidate,
                        released_build: &fixture.released,
                        query: query.clone(),
                        corpora: corpora.clone(),
                    }
                )
                .is_err());
        }
        let wrong_build = release(true);
        assert!(context
            .prepare_configuration(
                &decision,
                &head,
                PrepareResearcherConfigurationRequest {
                    candidate: &fixture.challenger,
                    released_build: &wrong_build,
                    query: query.clone(),
                    corpora: corpora.clone(),
                }
            )
            .is_err());
        let evaluator = SigningKey::from_bytes(&[40; 32]);
        let cost = SigningKey::from_bytes(&[41; 32]);
        let other_trust = Fixture::trust(&evaluator, &cost);
        let foreign = fixture.context_with(&other_trust).unwrap();
        assert!(foreign
            .prepare_configuration(
                &decision,
                &head,
                PrepareResearcherConfigurationRequest {
                    candidate: &fixture.challenger,
                    released_build: &fixture.released,
                    query: query.clone(),
                    corpora: corpora.clone(),
                }
            )
            .is_err());
        let mut altered_corpus = corpora;
        altered_corpus[0].entries[0].text.push_str(" drift");
        assert!(context
            .prepare_configuration(
                &decision,
                &head,
                PrepareResearcherConfigurationRequest {
                    candidate: &fixture.challenger,
                    released_build: &fixture.released,
                    query,
                    corpora: altered_corpus,
                }
            )
            .is_err());
        assert_eq!(head, before);

        let (mut rejected_fixture, corpora, query) = consumption_inputs();
        rejected_fixture
            .study
            .promotion_policy
            .min_certification_score_gain = 0.4;
        rejected_fixture.rebind();
        let rejected_context = rejected_fixture.context();
        let parent = rejected_fixture.head();
        let rejected = rejected_context
            .verify_promotion(&parent, &rejected_fixture.all(&rejected_context))
            .unwrap();
        assert!(rejected_context
            .prepare_configuration(
                &rejected,
                &parent,
                PrepareResearcherConfigurationRequest {
                    candidate: &rejected_fixture.challenger,
                    released_build: &rejected_fixture.released,
                    query,
                    corpora,
                }
            )
            .is_err());
    }

    #[test]
    fn next_run_rejects_version_only_config_wrong_source_input_or_stale_head() {
        let (mut fixture, corpora, query) = consumption_inputs();
        for version in [&mut fixture.incumbent, &mut fixture.challenger] {
            version.snapshot.retrieval.information_policy.allowed_data_view_sha256.push(h('4'));
        }
        fixture.rebind();
        let context = fixture.context();
        let mut head = fixture.head();
        let decision = context
            .verify_promotion(&head, &fixture.all(&context))
            .unwrap();
        apply_expected_head(&context, &mut head, &decision).unwrap();
        let prepared = context
            .prepare_configuration(
                &decision,
                &head,
                PrepareResearcherConfigurationRequest {
                    candidate: &fixture.challenger,
                    released_build: &fixture.released,
                    query,
                    corpora,
                },
            )
            .unwrap();
        for change in ["version-only", "source", "input", "allowed-wrong-view", "build", "stale", "invalid-profile"] {
            let mut run = fixture.executions[1].run.clone();
            run.configuration_sha256 = prepared.id().into();
            match change {
                "version-only" => run.configuration_sha256 = prepared.version_sha256().into(),
                "source" => run.code_commit = "b".repeat(40),
                "input" => run.data_manifest_sha256 = h('f'),
                "allowed-wrong-view" => run.data_manifest_sha256 = h('4'),
                "build" => run.build_artifact_sha256 = h('f'),
                _ => {}
            }
            let mut task = fixture.executions[1].task.clone();
            task.run_manifest_sha256 = run.id().unwrap();
            task.view_manifest_sha256
                .clone_from(&run.data_manifest_sha256);
            if change == "invalid-profile" { task.profile.namespace = "Invalid_Namespace".into(); }
            let mut current = head.clone();
            if change == "stale" {
                current.revision += 1;
            }
            assert!(
                context
                    .bind_next_run(&current, &prepared, &run, &task)
                    .is_err(),
                "{change}"
            );
        }
        let decoded: hft_research_agent_contracts::consumption::ResearcherConsumptionConfigV1 =
            serde_json::from_slice(prepared.bytes()).unwrap();
        assert_eq!(decoded.id().unwrap(), prepared.id());
        // An ordinary decoded DTO has no constructor for the private prepared
        // handle consumed by bind_next_run.
    }
}
