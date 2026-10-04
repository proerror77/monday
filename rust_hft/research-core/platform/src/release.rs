//! Import an independently verified release without granting scientific authority.
//! Public keys come from operator configuration, never from the signed envelope.
use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::{
    build::{BuildArtifact, BuiltExecutable},
    identity,
    orchestrator::Artifact,
    valid_digest,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceArchive {
    pub schema: u32,
    pub code_commit: String,
    pub archive: Artifact,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseProducer {
    pub repository: String,
    pub workflow_path: String,
    pub source_sha: String,
    pub run_id: u64,
    pub run_attempt: u32,
    pub job_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuildReleaseReceipt {
    pub schema: u32,
    pub build_sha256: String,
    pub source: SourceArchive,
    pub image: String,
    pub target: String,
    pub executables: Vec<BuiltExecutable>,
    pub producer: ReleaseProducer,
    /// The release verifier checks current-head CI and OCI-contained bytes.
    /// Its signed receipt retains the independent publication readback identity.
    pub publication_readback_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedBuildRelease {
    pub schema: u32,
    pub key_id: String,
    pub receipt: BuildReleaseReceipt,
    pub signature_hex: String,
}

impl SignedBuildRelease {
    /// The signer runs outside the research service. This is only the message
    /// encoding; the platform never loads a signing key or issues a release.
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = b"monday.research-build-release.v1\0".to_vec();
        bytes.extend(serde_json::to_vec(&(
            self.schema,
            &self.key_id,
            &self.receipt,
        ))?);
        Ok(bytes)
    }

    pub fn validate_binding(&self, artifact: &BuildArtifact) -> Result<()> {
        artifact.id()?;
        let receipt = &self.receipt;
        ensure!(
            self.schema == 1
                && receipt.schema == 1
                && artifact.release_receipt_sha256 == identity(self)?
                && receipt.build_sha256 == artifact.build.id()?
                && receipt.image == artifact.image
                && receipt.target == artifact.build.target
                && receipt.executables == artifact.executables,
            "release/build/source/ABI binding mismatch"
        );
        let source = &receipt.source;
        ensure!(
            source.schema == 1
                && source.code_commit == artifact.build.code_commit
                && identity(source)? == artifact.build.source_manifest_sha256
                && source.archive.key
                    == format!("research/sources/{}/source.tar", source.code_commit)
                && valid_digest(&source.archive.sha256)
                && (1..=512 * 1024 * 1024).contains(&source.archive.bytes),
            "source archive identity mismatch"
        );
        ensure!(
            receipt.producer.source_sha == source.code_commit
                && receipt.producer.run_id > 0
                && receipt.producer.run_attempt > 0
                && receipt.producer.job_id > 0
                && valid_digest(&receipt.publication_readback_sha256),
            "incomplete native release provenance"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildReleaseTrust {
    pub schema: u32,
    pub repository: String,
    pub producer_workflow_path: String,
    /// Ed25519 public keys, encoded as 64 lowercase hexadecimal characters.
    pub keys: BTreeMap<String, String>,
}

fn hex_bytes<const N: usize>(value: &str) -> Result<[u8; N]> {
    ensure!(
        value.len() == N * 2
            && value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid public key/signature encoding"
    );
    let mut bytes = [0; N];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)?;
    }
    Ok(bytes)
}

/// Only signature verification can construct this value. It cannot be decoded
/// from caller JSON or edited after verification.
pub struct VerifiedBuildRelease {
    artifact: BuildArtifact,
    signed: SignedBuildRelease,
    trust_sha256: String,
}

impl VerifiedBuildRelease {
    pub fn artifact(&self) -> &BuildArtifact {
        &self.artifact
    }
    pub fn signed(&self) -> &SignedBuildRelease {
        &self.signed
    }
    pub fn trust_sha256(&self) -> &str {
        &self.trust_sha256
    }
}

impl BuildReleaseTrust {
    pub fn verify(
        &self,
        artifact: &BuildArtifact,
        signed: &SignedBuildRelease,
    ) -> Result<VerifiedBuildRelease> {
        ensure!(
            self.schema == 1
                && !self.repository.is_empty()
                && self.repository.len() <= 256
                && self
                    .producer_workflow_path
                    .starts_with(".github/workflows/")
                && self.producer_workflow_path.ends_with(".yml")
                && (1..=16).contains(&self.keys.len()),
            "invalid release trust configuration"
        );
        ensure!(
            signed.key_id.len() <= 128
                && !signed.key_id.is_empty()
                && signed.receipt.producer.repository == self.repository
                && signed.receipt.producer.workflow_path == self.producer_workflow_path,
            "untrusted release producer"
        );
        let key = self
            .keys
            .get(&signed.key_id)
            .context("untrusted release key")?;
        let key = VerifyingKey::from_bytes(&hex_bytes(key)?)?;
        let signature = Signature::from_bytes(&hex_bytes(&signed.signature_hex)?);
        key.verify_strict(&signed.signing_bytes()?, &signature)
            .context("invalid release signature")?;
        signed.validate_binding(artifact)?;
        Ok(VerifiedBuildRelease {
            artifact: artifact.clone(),
            signed: signed.clone(),
            trust_sha256: identity(self)?,
        })
    }
}
