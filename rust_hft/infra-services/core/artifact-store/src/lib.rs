//! Typed ports for immutable research artifacts.

use hft_research_manifest::{ArtifactRef, ManifestId};
use serde::{Deserialize, Serialize};
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
}
