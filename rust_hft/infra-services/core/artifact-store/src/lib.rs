//! Typed ports for immutable research artifacts.

use hft_research_manifest::{ArtifactRef, ManifestId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ArtifactStoreError {
    #[error("artifact uri cannot be empty")]
    EmptyUri,
    #[error("artifact not found")]
    NotFound,
    #[error("artifact store io error: {0}")]
    Io(String),
    #[error("artifact store serialization error: {0}")]
    Serde(String),
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

#[derive(Debug, Clone)]
pub struct FileArtifactStore {
    root: PathBuf,
}

impl FileArtifactStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, manifest_id: &ManifestId) -> PathBuf {
        self.root
            .join(format!("{}.json", sanitized_manifest_id(manifest_id)))
    }

    fn read_records(
        &self,
        manifest_id: &ManifestId,
    ) -> Result<Vec<ArtifactRecord>, ArtifactStoreError> {
        let path = self.path_for(manifest_id);
        if !path.exists() {
            return Err(ArtifactStoreError::NotFound);
        }
        let data = std::fs::read_to_string(path).map_err(io_error)?;
        serde_json::from_str(&data).map_err(serde_error)
    }
}

impl ArtifactStore for FileArtifactStore {
    fn put_artifact(&mut self, record: ArtifactRecord) -> Result<(), ArtifactStoreError> {
        record.validate()?;
        std::fs::create_dir_all(&self.root).map_err(io_error)?;
        let mut records = match self.read_records(&record.manifest_id) {
            Ok(records) => records,
            Err(ArtifactStoreError::NotFound) => Vec::new(),
            Err(error) => return Err(error),
        };
        let path = self.path_for(&record.manifest_id);
        records.push(record);
        let data = serde_json::to_string_pretty(&records).map_err(serde_error)?;
        std::fs::write(path, data).map_err(io_error)
    }

    fn get_artifacts(
        &self,
        manifest_id: &ManifestId,
    ) -> Result<Vec<ArtifactRecord>, ArtifactStoreError> {
        self.read_records(manifest_id)
    }
}

fn sanitized_manifest_id(manifest_id: &ManifestId) -> String {
    manifest_id
        .as_str()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn io_error(error: std::io::Error) -> ArtifactStoreError {
    ArtifactStoreError::Io(error.to_string())
}

fn serde_error(error: serde_json::Error) -> ArtifactStoreError {
    ArtifactStoreError::Serde(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

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

    #[test]
    fn file_store_round_trips_artifacts() {
        let root = std::env::temp_dir().join(format!(
            "hft-artifact-store-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let manifest_id = ManifestId::new("manifest/1").unwrap();
        let record = ArtifactRecord {
            manifest_id: manifest_id.clone(),
            artifact: ArtifactRef {
                uri: "target/alpha-harness/audit-demo.json".to_string(),
                content_type: "application/json".to_string(),
                checksum: None,
            },
        };

        let mut store = FileArtifactStore::new(&root);
        store.put_artifact(record.clone()).unwrap();

        assert_eq!(store.get_artifacts(&manifest_id).unwrap(), vec![record]);
        assert!(root.join("manifest_1.json").exists());

        let _ = std::fs::remove_dir_all(root);
    }
}
