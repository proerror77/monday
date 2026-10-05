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
