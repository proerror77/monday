//! Typed ports for search and evaluation experiment indexes.

use chrono::{DateTime, Utc};
use hft_research_manifest::ManifestId;
use hft_search_protocol::{ProposalArtifact, SearchProtocolError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExperimentStoreError {
    #[error("experiment id cannot be empty")]
    EmptyExperimentId,
    #[error("invalid proposal artifact: {0}")]
    InvalidProposal(#[from] SearchProtocolError),
    #[error("experiment not found")]
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentRun {
    pub experiment_id: String,
    pub search_manifest_id: ManifestId,
    pub proposals: Vec<ProposalArtifact>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl ExperimentRun {
    pub fn validate(&self) -> Result<(), ExperimentStoreError> {
        if self.experiment_id.trim().is_empty() {
            return Err(ExperimentStoreError::EmptyExperimentId);
        }
        for proposal in &self.proposals {
            proposal.validate()?;
        }
        Ok(())
    }
}

pub trait ExperimentStore {
    fn put_run(&mut self, run: ExperimentRun) -> Result<(), ExperimentStoreError>;
    fn get_run(&self, experiment_id: &str) -> Result<ExperimentRun, ExperimentStoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_experiment_id() {
        let run = ExperimentRun {
            experiment_id: " ".to_string(),
            search_manifest_id: ManifestId::new("search-1").unwrap(),
            proposals: vec![],
            started_at: Utc::now(),
            completed_at: None,
        };

        assert_eq!(
            run.validate().unwrap_err(),
            ExperimentStoreError::EmptyExperimentId
        );
    }
}
