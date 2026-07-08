//! Typed ports for search and evaluation experiment indexes.

use chrono::{DateTime, Utc};
use hft_research_manifest::ManifestId;
use hft_search_protocol::{ProposalArtifact, SearchProtocolError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

#[derive(Debug, Default)]
pub struct InMemoryExperimentStore {
    runs: BTreeMap<String, ExperimentRun>,
}

impl ExperimentStore for InMemoryExperimentStore {
    fn put_run(&mut self, run: ExperimentRun) -> Result<(), ExperimentStoreError> {
        run.validate()?;
        self.runs.insert(run.experiment_id.clone(), run);
        Ok(())
    }

    fn get_run(&self, experiment_id: &str) -> Result<ExperimentRun, ExperimentStoreError> {
        self.runs
            .get(experiment_id)
            .cloned()
            .ok_or(ExperimentStoreError::NotFound)
    }
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

    #[test]
    fn in_memory_store_round_trips_runs() {
        let run = ExperimentRun {
            experiment_id: "exp-1".to_string(),
            search_manifest_id: ManifestId::new("search-1").unwrap(),
            proposals: vec![],
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
        };

        let mut store = InMemoryExperimentStore::default();
        store.put_run(run.clone()).unwrap();

        assert_eq!(store.get_run("exp-1").unwrap(), run);
    }
}
