//! Typed ports for immutable research artifacts.

use hft_research_manifest::{ArtifactRef, ManifestId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ArtifactStoreError {
    #[error("artifact uri cannot be empty")]
    EmptyUri,
    #[error("artifact not found")]
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub manifest_id: ManifestId,
    pub artifact: ArtifactRef,
}

impl ArtifactRecord {
    pub fn validate(&self) -> Result<(), ArtifactStoreError> {
        if self.artifact.uri.trim().is_empty() {
            return Err(ArtifactStoreError::EmptyUri);
        }
        Ok(())
    }
}

pub trait ArtifactStore {
    fn put_artifact(&mut self, record: ArtifactRecord) -> Result<(), ArtifactStoreError>;
    fn get_artifacts(
        &self,
        manifest_id: &ManifestId,
    ) -> Result<Vec<ArtifactRecord>, ArtifactStoreError>;
}

#[derive(Debug, Default)]
pub struct InMemoryArtifactStore {
    records: BTreeMap<ManifestId, Vec<ArtifactRecord>>,
}

impl ArtifactStore for InMemoryArtifactStore {
    fn put_artifact(&mut self, record: ArtifactRecord) -> Result<(), ArtifactStoreError> {
        record.validate()?;
        self.records
            .entry(record.manifest_id.clone())
            .or_default()
            .push(record);
        Ok(())
    }

    fn get_artifacts(
        &self,
        manifest_id: &ManifestId,
    ) -> Result<Vec<ArtifactRecord>, ArtifactStoreError> {
        self.records
            .get(manifest_id)
            .cloned()
            .ok_or(ArtifactStoreError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_artifact_uri() {
        let record = ArtifactRecord {
            manifest_id: ManifestId::new("manifest-1").unwrap(),
            artifact: ArtifactRef {
                uri: " ".to_string(),
                content_type: "application/json".to_string(),
                checksum: None,
            },
        };

        assert_eq!(record.validate().unwrap_err(), ArtifactStoreError::EmptyUri);
    }

    #[test]
    fn in_memory_store_round_trips_artifacts() {
        let manifest_id = ManifestId::new("manifest-1").unwrap();
        let record = ArtifactRecord {
            manifest_id: manifest_id.clone(),
            artifact: ArtifactRef {
                uri: "artifact://proposal-1.json".to_string(),
                content_type: "application/json".to_string(),
                checksum: None,
            },
        };

        let mut store = InMemoryArtifactStore::default();
        store.put_artifact(record.clone()).unwrap();

        assert_eq!(store.get_artifacts(&manifest_id).unwrap(), vec![record]);
    }
}
