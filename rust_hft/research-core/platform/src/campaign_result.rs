//! Typed development evidence. Compute completion grants no scientific promotion.
use crate::{
    orchestrator::{Artifact, TaskKind, TaskSpec},
    valid_digest,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveEntry {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublishedNativeArtifact {
    pub artifact: Artifact,
    /// Receipt from the original native publisher's actual independent readback.
    pub native_publication_readback_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CampaignRoundEvidence {
    pub round_id: String,
    pub seed: u64,
    pub native_mission_id: String,
    pub native_mission_sha256: String,
    pub consumed_trials: u64,
    pub result_zip: PublishedNativeArtifact,
    /// Only entries actually present in the independently decoded archive.
    /// Missing fit, evaluation, or replay evidence stays missing.
    pub entries: Vec<ArchiveEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScientificStatus {
    InsufficientEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CexCampaignResultReceipt {
    pub schema: String,
    pub native_request_sha256: String,
    pub native_campaign_inputs_sha256: String,
    pub collection_sha256: String,
    pub evaluation_protocol_sha256: String,
    pub runner: hft_cex_research_input::campaign::SourceBuildRefV1,
    pub native_campaign_id: String,
    pub declared_trials: u64,
    pub actual_consumed_trials: u64,
    pub scientific_status: ScientificStatus,
    pub campaign_result: PublishedNativeArtifact,
    pub rounds: Vec<CampaignRoundEvidence>,
}
impl CexCampaignResultReceipt {
    pub fn validate(&self, spec: &TaskSpec, artifacts: &[Artifact]) -> Result<()> {
        ensure!(
            spec.kind == TaskKind::CexCampaign
                && self.schema == "monday.cex_campaign_result_receipt.v1"
                && self.collection_sha256 == spec.view_manifest_sha256
                && self.runner.image_identity == spec.image
                && [
                    &self.native_request_sha256,
                    &self.native_campaign_inputs_sha256,
                    &self.evaluation_protocol_sha256
                ]
                .into_iter()
                .all(|s| valid_digest(s)),
            "Campaign result changed fixed input identity"
        );
        let requests: Vec<_> = spec
            .command
            .windows(2)
            .filter(|p| p[0] == "--request-sha256")
            .map(|p| p[1].as_str())
            .collect();
        ensure!(
            requests == [self.native_request_sha256.as_str()],
            "Campaign result changed canonical request"
        );
        ensure!(
            !self.native_campaign_id.is_empty()
                && self.native_campaign_id.len() <= 128
                && self.declared_trials > 0
                && self.actual_consumed_trials > 0
                && self.actual_consumed_trials <= self.declared_trials
                && !self.rounds.is_empty()
                && self.rounds.len() <= 256,
            "Campaign result lacks bounded actual trial evidence"
        );
        let mut round_ids = std::collections::BTreeSet::new();
        let mut consumed = 0_u64;
        for round in &self.rounds {
            ensure!(
                !round.round_id.is_empty()
                    && round.round_id.len() <= 128
                    && round_ids.insert(&round.round_id)
                    && !round.native_mission_id.is_empty()
                    && round.native_mission_id.len() <= 128
                    && valid_digest(&round.native_mission_sha256)
                    && round.consumed_trials > 0
                    && !round.entries.is_empty()
                    && round.entries.len() <= 256,
                "Campaign round lacks actual native evidence"
            );
            consumed = consumed
                .checked_add(round.consumed_trials)
                .ok_or_else(|| anyhow::anyhow!("trial accounting overflow"))?;
            let mut paths = std::collections::BTreeSet::new();
            for entry in &round.entries {
                ensure!(
                    !entry.path.is_empty()
                        && entry.path.len() <= 256
                        && !entry.path.starts_with('/')
                        && !entry.path.contains("..")
                        && entry
                            .path
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
                        && valid_digest(&entry.sha256)
                        && entry.bytes > 0
                        && entry.bytes <= 512 * 1024 * 1024
                        && paths.insert(&entry.path),
                    "invalid or repeated native archive entry"
                );
            }
        }
        ensure!(
            consumed == self.actual_consumed_trials,
            "aggregate trial count differs from native rounds"
        );
        for published in
            std::iter::once(&self.campaign_result).chain(self.rounds.iter().map(|r| &r.result_zip))
        {
            ensure!(
                valid_digest(&published.native_publication_readback_sha256)
                    && artifacts.contains(&published.artifact),
                "native result lacks independently readable output"
            );
        }
        Ok(())
    }
}

/// Transport-level archive verification, independent of the worker's entry
/// claims. This does not evaluate a model or permit scientific promotion.
#[cfg(feature = "campaign-result-validation")]
pub fn verify_archive_entries(
    source: impl std::io::Read + std::io::Seek,
    expected: &[ArchiveEntry],
    decoded_budget: u64,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    ensure!(
        !expected.is_empty()
            && expected.len() <= 256
            && decoded_budget > 0
            && decoded_budget <= 512 * 1024 * 1024,
        "unbounded native archive verification"
    );
    let mut archive = zip::ZipArchive::new(source)?;
    ensure!(archive.len() <= 256, "unbounded native archive entry count");
    let mut seen = std::collections::BTreeSet::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let path = entry.name().to_owned();
        ensure!(
            seen.insert(path.clone())
                && !entry.is_dir()
                && !path.starts_with('/')
                && !path.contains("..")
                && path
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
                && entry
                    .unix_mode()
                    .is_none_or(|mode| mode & 0o170000 == 0 || mode & 0o170000 == 0o100000),
            "duplicate or unsafe native archive entry"
        );
        let reference = expected
            .iter()
            .find(|r| r.path == path)
            .ok_or_else(|| anyhow::anyhow!("archive entry omitted from native evidence"))?;
        ensure!(
            entry.size() == reference.bytes && crate::valid_digest(&reference.sha256),
            "native archive metadata changed"
        );
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| anyhow::anyhow!("native archive size overflow"))?;
        ensure!(
            total <= decoded_budget,
            "native archive exceeds decoded evidence budget"
        );
        let mut digest = Sha256::new();
        let mut count = 0_u64;
        let mut buffer = [0_u8; 8192];
        loop {
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            count += n as u64;
            ensure!(
                count <= reference.bytes,
                "native archive entry exceeds declared size"
            );
            digest.update(&buffer[..n]);
        }
        ensure!(
            count == reference.bytes && format!("{:x}", digest.finalize()) == reference.sha256,
            "native archive entry bytes changed"
        );
    }
    ensure!(
        seen.len() == expected.len() && expected.iter().all(|r| seen.contains(&r.path)),
        "native archive evidence missing entries"
    );
    Ok(())
}

#[cfg(all(test, feature = "campaign-result-validation"))]
mod tests {
    use super::*;
    #[test]
    fn native_zip_entry_claims_require_actual_bytes_and_bounded_coverage() -> Result<()> {
        use std::io::{Cursor, Write};
        let content = b"synthetic native verification receipt";
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive.start_file(
            "results/replay.json",
            zip::write::SimpleFileOptions::default(),
        )?;
        archive.write_all(content)?;
        let bytes = archive.finish()?.into_inner();
        let reference = ArchiveEntry {
            path: "results/replay.json".into(),
            sha256: crate::sha256(content),
            bytes: content.len() as u64,
        };
        verify_archive_entries(Cursor::new(&bytes), std::slice::from_ref(&reference), 1024)?;
        let mut forged = reference.clone();
        forged.sha256 = "a".repeat(64);
        assert!(verify_archive_entries(Cursor::new(&bytes), &[forged], 1024).is_err());
        assert!(
            verify_archive_entries(Cursor::new(&bytes), std::slice::from_ref(&reference), 1)
                .is_err()
        );
        let mut absent = reference.clone();
        absent.path = "missing.json".into();
        assert!(verify_archive_entries(Cursor::new(&bytes), &[absent], 1024).is_err());
        assert!(
            verify_archive_entries(Cursor::new(&bytes), &[reference.clone(), reference], 1024)
                .is_err()
        );
        Ok(())
    }
}
