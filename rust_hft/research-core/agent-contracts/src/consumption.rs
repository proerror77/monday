//! Fixed configuration declarations and observed usage, not adoption authority.
//! All clocks are Unix UTC nanoseconds. Source/phase/view fields remain claims;
//! the controlled producer must authorize and read back their original evidence.

use crate::{
    content_sha256, digest, valid_name, ContentReferenceV1, MetaTaskPhase, ResearcherVersionV1,
    CONTRACT_SCHEMA_V1,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceQueryV1 {
    pub task_context_sha256: String,
    pub data_view_sha256: String,
    pub as_of_ns: i64,
    pub query_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceEvidenceV1 {
    pub content: ContentReferenceV1,
    pub phase: MetaTaskPhase,
    pub data_view_sha256: String,
    pub available_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchExperienceV1 {
    pub id: String,
    pub task_context_sha256: String,
    pub observed_ns: i64,
    pub available_ns: i64,
    pub text: String,
    pub source: ExperienceEvidenceV1,
    pub evidence: Vec<ExperienceEvidenceV1>,
}

/// The reference hashes this complete body, including its ID and every entry.
/// It does not hash a caller-supplied reference back into itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenExperienceCorpusV1 {
    pub id: String,
    pub entries: Vec<ResearchExperienceV1>,
}

/// Independent configuration identity: no Run, Task, Attempt or adoption flag.
/// Control binds its canonical body hash to the existing Run configuration hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearcherConsumptionConfigV1 {
    pub schema: u32,
    pub version: ResearcherVersionV1,
    pub query: ExperienceQueryV1,
    pub corpora: Vec<FrozenExperienceCorpusV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearcherTaskContextV1 {
    pub prompt_text: String,
    pub search_policy: Value,
    pub query: ExperienceQueryV1,
    pub query_template: String,
    pub ordered_experiences: Vec<ResearchExperienceV1>,
}

/// Only observed immutable bytes and selected context. This receipt grants no
/// permission, proves no adopted head, and contains no scientific score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationConsumptionReceiptV1 {
    pub schema: u32,
    pub configuration_sha256: String,
    pub researcher_version_sha256: String,
    pub query_sha256: String,
    pub corpus: Vec<ContentReferenceV1>,
    pub selected_experience_ids: Vec<String>,
    pub context_sha256: String,
    pub context_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumedResearcherConfigurationV1 {
    pub context: ResearcherTaskContextV1,
    pub receipt: ConfigurationConsumptionReceiptV1,
}

impl ExperienceQueryV1 {
    pub fn validate(&self) -> Result<()> {
        digest(&self.task_context_sha256, "retrieval task context")?;
        digest(&self.data_view_sha256, "retrieval query DataView")?;
        ensure!(
            self.as_of_ns >= 0,
            "query clock must be Unix UTC nanoseconds"
        );
        ensure!(
            !self.query_text.trim().is_empty() && self.query_text.len() <= 16384,
            "query text is absent or unbounded"
        );
        Ok(())
    }

    pub fn id(&self) -> Result<String> {
        self.validate()?;
        content_sha256(self)
    }
}

impl ExperienceEvidenceV1 {
    fn validate_for(&self, version: &ResearcherVersionV1, as_of_ns: i64) -> Result<()> {
        self.content.validate()?;
        digest(&self.data_view_sha256, "experience evidence DataView")?;
        ensure!(
            self.phase == MetaTaskPhase::Development,
            "experience source exposes hidden selection/certification phase"
        );
        ensure!(
            version
                .snapshot
                .retrieval
                .information_policy
                .allowed_data_view_sha256
                .contains(&self.data_view_sha256),
            "experience source DataView is outside declared information scope"
        );
        ensure!(
            self.available_ns >= 0 && self.available_ns <= as_of_ns,
            "experience evidence was not available at query time"
        );
        Ok(())
    }
}

impl ResearchExperienceV1 {
    fn validate_for(&self, version: &ResearcherVersionV1, as_of_ns: i64) -> Result<()> {
        valid_name(&self.id, "experience id")?;
        digest(&self.task_context_sha256, "experience task context")?;
        ensure!(
            !self.text.trim().is_empty() && self.text.len() <= 65536,
            "experience text is absent or unbounded"
        );
        ensure!(
            self.observed_ns >= 0
                && self.observed_ns <= self.available_ns
                && self.available_ns <= as_of_ns,
            "experience clocks violate observed/available/query ordering"
        );
        self.source.validate_for(version, as_of_ns)?;
        ensure!(
            self.source.available_ns <= self.available_ns,
            "experience predates its source availability"
        );
        ensure!(
            !self.evidence.is_empty() && self.evidence.len() <= 64,
            "experience must bind bounded source evidence"
        );
        let mut ids = BTreeSet::new();
        for evidence in &self.evidence {
            evidence.validate_for(version, as_of_ns)?;
            ensure!(
                evidence.available_ns <= self.available_ns,
                "experience predates its evidence availability"
            );
            ensure!(
                ids.insert(&evidence.content.id),
                "experience repeats an evidence id"
            );
        }
        Ok(())
    }
}

impl FrozenExperienceCorpusV1 {
    pub fn content_reference(&self) -> Result<ContentReferenceV1> {
        valid_name(&self.id, "frozen corpus id")?;
        ensure!(
            !self.entries.is_empty() && self.entries.len() <= 1024,
            "frozen corpus must contain bounded actual entries"
        );
        Ok(ContentReferenceV1 {
            id: self.id.clone(),
            content_sha256: content_sha256(self)?,
        })
    }
}

impl ResearcherConsumptionConfigV1 {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == CONTRACT_SCHEMA_V1,
            "unsupported researcher consumption schema"
        );
        self.version.validate()?;
        self.query.validate()?;
        let retrieval = &self.version.snapshot.retrieval;
        ensure!(
            retrieval
                .information_policy
                .allowed_data_view_sha256
                .contains(&self.query.data_view_sha256),
            "query DataView is outside declared version scope"
        );
        ensure!(
            self.corpora.len() == retrieval.corpus.len() && !self.corpora.is_empty(),
            "actual corpus coverage differs from frozen version"
        );
        let mut corpora = BTreeSet::new();
        let mut entries = BTreeSet::new();
        for corpus in &self.corpora {
            let reference = corpus.content_reference()?;
            ensure!(corpora.insert(&corpus.id), "duplicate actual corpus id");
            let frozen = retrieval
                .corpus
                .iter()
                .find(|frozen| frozen.content.id == corpus.id)
                .ok_or_else(|| anyhow::anyhow!("actual corpus was not declared by version"))?;
            ensure!(
                frozen.feedback_phase == MetaTaskPhase::Development && frozen.content == reference,
                "actual corpus content or phase differs from frozen reference"
            );
            for entry in &corpus.entries {
                entry.validate_for(&self.version, self.query.as_of_ns)?;
                ensure!(
                    entries.insert(&entry.id),
                    "experience id repeats across frozen corpus"
                );
                ensure!(
                    entries.len() <= 4096,
                    "combined experience corpus exceeds entry limit"
                );
            }
        }
        Ok(())
    }

    pub fn id(&self) -> Result<String> {
        self.validate()?;
        content_sha256(self)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        crate::canonical_bytes(self)
    }
}
