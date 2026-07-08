//! Typed ports for search and evaluation experiment indexes.

use chrono::{DateTime, Utc};
use hft_research_manifest::ManifestId;
use hft_search_protocol::{ProposalArtifact, SearchProtocolError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExperimentStoreError {
    #[error("experiment id cannot be empty")]
    EmptyExperimentId,
    #[error("invalid proposal artifact: {0}")]
    InvalidProposal(#[from] SearchProtocolError),
    #[error("experiment not found")]
    NotFound,
    #[error("experiment store io error: {0}")]
    Io(String),
    #[error("experiment store serialization error: {0}")]
    Serde(String),
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

#[derive(Debug, Clone)]
pub struct FileExperimentStore {
    path: PathBuf,
}

impl FileExperimentStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_runs(&self) -> Result<BTreeMap<String, ExperimentRun>, ExperimentStoreError> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        let data = std::fs::read_to_string(&self.path).map_err(io_error)?;
        serde_json::from_str(&data).map_err(serde_error)
    }

    fn write_runs(
        &self,
        runs: &BTreeMap<String, ExperimentRun>,
    ) -> Result<(), ExperimentStoreError> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        let data = serde_json::to_string_pretty(runs).map_err(serde_error)?;
        std::fs::write(&self.path, data).map_err(io_error)
    }
}

impl ExperimentStore for FileExperimentStore {
    fn put_run(&mut self, run: ExperimentRun) -> Result<(), ExperimentStoreError> {
        run.validate()?;
        let mut runs = self.read_runs()?;
        runs.insert(run.experiment_id.clone(), run);
        self.write_runs(&runs)
    }

    fn get_run(&self, experiment_id: &str) -> Result<ExperimentRun, ExperimentStoreError> {
        self.read_runs()?
            .remove(experiment_id)
            .ok_or(ExperimentStoreError::NotFound)
    }
}

fn io_error(error: std::io::Error) -> ExperimentStoreError {
    ExperimentStoreError::Io(error.to_string())
}

fn serde_error(error: serde_json::Error) -> ExperimentStoreError {
    ExperimentStoreError::Serde(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

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

    #[test]
    fn file_store_round_trips_runs() {
        let root = std::env::temp_dir().join(format!(
            "hft-experiment-store-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("experiments.json");
        let run = ExperimentRun {
            experiment_id: "exp-1".to_string(),
            search_manifest_id: ManifestId::new("search-1").unwrap(),
            proposals: vec![],
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
        };

        let mut store = FileExperimentStore::new(&path);
        store.put_run(run.clone()).unwrap();

        assert_eq!(store.get_run("exp-1").unwrap(), run);
        assert!(store.path().exists());

        let _ = std::fs::remove_dir_all(root);
    }
}
