//! Admission of an immutable sequence cohort and its original PIT receipts.
use crate::mission_render::approved_validation;
use crate::mission_runner::{decode_materialization, validate_materialization};
use anyhow::{bail, Context};
use hft_research_manifest::sequence::{
    valid_sha256, SequenceDatasetV1, SequenceInputSpecV1, SequenceViewV1,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

pub const INPUTS_SCHEMA: &str = "monday.sol_sequence_inputs.v1";

pub(crate) fn require_exact_files(
    root: &Path,
    admitted: &std::collections::BTreeSet<PathBuf>,
) -> anyhow::Result<()> {
    if admitted.iter().any(|path| !path.starts_with(root)) {
        bail!("sequence shard escaped mount");
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0_usize;
    while let Some(directory) = pending.pop() {
        for item in std::fs::read_dir(directory)? {
            entries += 1;
            if entries > 8192 {
                bail!("sequence mount exceeds inventory bound");
            }
            let item = item?;
            let kind = item.file_type()?;
            if kind.is_dir() {
                pending.push(item.path());
            } else if kind.is_file() && admitted.contains(&item.path()) {
                seen.insert(item.path());
            } else {
                bail!("sequence mount contains an unadmitted file or symlink");
            }
        }
    }
    if seen != *admitted {
        bail!("sequence mount omits admitted artifacts");
    }
    Ok(())
}

/// Observations and the 30-second label must stay inside the half-open view.
fn dataset_span_within_view(first: i64, last: i64, view: SequenceViewV1) -> anyhow::Result<()> {
    let maturity = last
        .checked_add(30_000)
        .context("sequence label clock overflow")?;
    if first < view.history_start_ms || maturity >= view.end_ms {
        bail!("sequence dataset exposes observations or targets beyond its authorized view");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub file: String,
    pub sha256: String,
}

impl Artifact {
    pub fn validate(&self) -> anyhow::Result<()> {
        if !valid_sha256(&self.sha256) || !safe_relative_path(&self.file) {
            bail!("invalid sequence artifact reference");
        }
        Ok(())
    }
    pub fn path(&self, root: &Path) -> anyhow::Result<PathBuf> {
        self.validate()?;
        if !std::fs::symlink_metadata(root)?.file_type().is_dir() {
            bail!("sequence artifact root is not a regular directory");
        }
        let root = root.canonicalize()?;
        let mut path = root.clone();
        for part in Path::new(&self.file).components() {
            path.push(part);
            if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                bail!("sequence artifact contains a symlink");
            }
        }
        let path = path.canonicalize()?;
        if !path.starts_with(&root) || !path.is_file() {
            bail!("sequence artifact escapes its admitted input root");
        }
        Ok(path)
    }
    pub fn read(&self, root: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
        let path = self.path(root)?;
        let file = File::open(path)?;
        if file.metadata()?.len() > limit {
            bail!("sequence metadata exceeds byte bound");
        }
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit || format!("{:x}", Sha256::digest(&bytes)) != self.sha256 {
            bail!("sequence artifact checksum or size mismatch");
        }
        Ok(bytes)
    }
}

pub fn safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.starts_with('/')
        && value.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.=".contains(&b))
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetLocation {
    pub manifest: Artifact,
    pub sources: Artifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceCampaignInputs {
    pub schema_version: String,
    pub producer_source_revision: String,
    pub producer_image: String,
    pub pvc_name: String,
    pub pvc_uid: String,
    pub sub_path: String,
    pub fold_id: u8,
    pub training_window_days: u8,
    pub train: DatasetLocation,
    pub validation: DatasetLocation,
    pub replay_artifact: Artifact,
    pub replay_manifest: Artifact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceSourceIndex {
    pub schema_version: String,
    pub sources: Vec<SequenceSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceSource {
    pub receipt: Artifact,
    pub materialization: Artifact,
    pub sequence: Artifact,
}

impl SequenceCampaignInputs {
    /// A view is not a secrecy barrier. Reject every file outside the declared
    /// development inputs so a broad PVC subPath cannot expose a sealed shard.
    pub fn verify_mount(&self, root: &Path) -> anyhow::Result<()> {
        let mut admitted = std::collections::BTreeSet::new();
        for artifact in [&self.replay_artifact, &self.replay_manifest] {
            admitted.insert(artifact.path(root)?);
        }
        for location in [&self.train, &self.validation] {
            let dataset = self.verify_dataset(root, location)?;
            let manifest = location.manifest.path(root)?;
            admitted.insert(manifest.clone());
            admitted.insert(location.sources.path(root)?);
            let index: SequenceSourceIndex =
                serde_json::from_slice(&location.sources.read(root, 4 * 1024 * 1024)?)?;
            for source in index.sources {
                for artifact in [&source.receipt, &source.materialization, &source.sequence] {
                    admitted.insert(artifact.path(root)?);
                }
            }
            for shard in dataset.shards {
                admitted.insert(
                    manifest
                        .parent()
                        .context("dataset parent")?
                        .join(shard.file)
                        .canonicalize()?,
                );
            }
        }
        require_exact_files(&root.canonicalize()?, &admitted)
    }

    pub fn verify_replay(
        &self,
        root: &Path,
        view: hft_research_manifest::sequence::SequenceViewV1,
    ) -> anyhow::Result<()> {
        let index: SequenceSourceIndex =
            serde_json::from_slice(&self.validation.sources.read(root, 4 * 1024 * 1024)?)?;
        verify_source_replay(
            root,
            &self.replay_artifact,
            &self.replay_manifest,
            view,
            index.sources.iter().map(|source| &source.materialization),
        )
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != INPUTS_SCHEMA
            || !matches!(self.fold_id, 1 | 2)
            || !matches!(self.training_window_days, 7 | 14)
            || !crate::mission_runner::valid_git_revision(&self.producer_source_revision)
            || !safe_relative_path(&self.sub_path)
            || !self.sub_path.starts_with("sol-sequence/")
            || self.pvc_uid.is_empty()
            || self.pvc_uid.len() > 128
            || !self
                .pvc_uid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            bail!("invalid SOL sequence input identity");
        }
        hft_research_dispatch_io::validate_dns_label("sequence input PVC", &self.pvc_name)?;
        if !self.producer_image.contains("@sha256:")
            || !valid_sha256(
                self.producer_image
                    .rsplit_once("@sha256:")
                    .context("sequence producer image is not pinned")?
                    .1,
            )
        {
            bail!("sequence producer image must be digest pinned");
        }
        for item in [
            &self.train.manifest,
            &self.train.sources,
            &self.validation.manifest,
            &self.validation.sources,
            &self.replay_artifact,
            &self.replay_manifest,
        ] {
            item.validate()?;
        }
        Ok(())
    }

    pub fn verify_view(
        &self,
        root: &Path,
        location: &DatasetLocation,
        view: SequenceViewV1,
    ) -> anyhow::Result<SequenceDatasetV1> {
        view.validate().map_err(anyhow::Error::msg)?;
        let dataset = self.verify_dataset(root, location)?;
        for shard in &dataset.shards {
            dataset_span_within_view(shard.first_observed_at_ms, shard.last_observed_at_ms, view)?;
        }
        Ok(dataset)
    }

    pub fn verify_dataset(
        &self,
        root: &Path,
        location: &DatasetLocation,
    ) -> anyhow::Result<SequenceDatasetV1> {
        self.validate()?;
        let dataset: SequenceDatasetV1 =
            serde_json::from_slice(&location.manifest.read(root, 4 * 1024 * 1024)?)?;
        dataset.validate().map_err(anyhow::Error::msg)?;
        if dataset.input != SequenceInputSpecV1::sol_lob()
            || dataset.digest().map_err(anyhow::Error::msg)? != location.manifest.sha256
            || dataset.source_manifest_sha256 != location.sources.sha256
        {
            bail!("sequence cohort differs from its source index or canonical channels");
        }
        let index: SequenceSourceIndex =
            serde_json::from_slice(&location.sources.read(root, 4 * 1024 * 1024)?)?;
        if index.schema_version != "monday.sol_sequence_sources.v1"
            || index.sources.is_empty()
            || index.sources.len() > 512
        {
            bail!("invalid sequence source index");
        }
        let mut expected_shards = Vec::new();
        for source in index.sources {
            let receipt: serde_json::Value =
                serde_json::from_slice(&source.receipt.read(root, 1024 * 1024)?)?;
            if receipt["schema_version"] != "monday.cex_campaign_inputs.v1"
                || receipt["source_revision"] != self.producer_source_revision
                || receipt["image_ref"] != self.producer_image
                || receipt["symbol"] != "SOLUSDT"
                || receipt["market"] != "usdm"
                || receipt["materialization"]["sha256"] != source.materialization.sha256
            {
                bail!("sequence source is not the declared native SOL preparation receipt");
            }
            let bytes = source.materialization.read(root, 16 * 1024 * 1024)?;
            let report: serde_json::Value = serde_json::from_slice(&bytes)?;
            let materialization = decode_materialization(&bytes)?;
            validate_materialization(
                &materialization,
                &materialization.artifact_sha256,
                &approved_validation(&materialization)?,
            )?;
            let original: SequenceDatasetV1 =
                serde_json::from_slice(&source.sequence.read(root, 4 * 1024 * 1024)?)?;
            original.validate().map_err(anyhow::Error::msg)?;
            if materialization.symbol != "SOLUSDT"
                || materialization.market != "usdm"
                || materialization.bucket_ms != 1000
                || materialization.top_depth != 5
                || materialization.label_horizon_buckets != 30
                || !materialization
                    .snapshot
                    .required_modalities
                    .contains(hft_research_manifest::CEX_MODALITY_AGGREGATE_TRADE)
                || receipt["feature"]["sha256"] != materialization.artifact_sha256
                || report["sequence_manifest_sha256"] != source.sequence.sha256
                || original.digest().map_err(anyhow::Error::msg)? != source.sequence.sha256
                || original.source_manifest_sha256 != materialization.snapshot.sha256()
                || original.input != dataset.input
                || original.shards.iter().map(|shard| shard.rows).sum::<u64>()
                    != materialization.rows as u64
            {
                bail!("sequence source provenance or row count differs from verified PIT output");
            }
            expected_shards.extend(original.shards);
        }
        if expected_shards != dataset.shards {
            bail!("sequence cohort rewrites, reorders or omits source shards");
        }
        // Shard bytes are checked by SequenceReader against this admitted identity.
        Ok(dataset)
    }
}

/// Verify the complete original canonical replay provenance shared by both
/// sequence and market-encoder cohorts. PIT snapshot hashes are not replay hashes.
pub(crate) fn verify_source_replay<'a>(
    root: &Path,
    replay_artifact: &Artifact,
    replay_manifest: &Artifact,
    view: SequenceViewV1,
    materializations: impl IntoIterator<Item = &'a Artifact>,
) -> anyhow::Result<()> {
    view.validate().map_err(anyhow::Error::msg)?;
    let evidence = hft_backtest::config::verify_canonical_replay_artifact_streaming(
        &replay_artifact.path(root)?,
        &replay_manifest.path(root)?,
        Some(&replay_artifact.sha256),
        &replay_manifest.sha256,
        None,
        Some(
            view.end_ms
                .checked_mul(1000)
                .context("replay end overflow")?,
        ),
    )?;
    let mut expected = std::collections::BTreeMap::new();
    for source in materializations {
        let report: serde_json::Value =
            serde_json::from_slice(&source.read(root, 16 * 1024 * 1024)?)?;
        if report["source_revision"] != evidence.source_revision {
            bail!("sequence replay source revision differs from its PIT sources");
        }
        for source in report["source_segments"]
            .as_array()
            .context("missing PIT sources")?
        {
            let mut source = source.clone();
            let path = source["path"].as_str().context("missing PIT source path")?;
            let file = Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .context("invalid PIT source name")?
                .to_owned();
            let object = source.as_object_mut().context("invalid PIT source")?;
            object.remove("path");
            object.insert("file".into(), file.into());
            // Compare the complete canonical replay provenance fields.
            let source: hft_backtest::config::CanonicalSourceSegmentEvidence =
                serde_json::from_value(source)?;
            if expected
                .insert(source.sha256.clone(), source.clone())
                .is_some_and(|old| old != source)
            {
                bail!("conflicting sequence replay source provenance");
            }
        }
    }
    let actual = evidence
        .source_segments
        .iter()
        .map(|s| (s.sha256.clone(), s.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    if evidence.symbol != "SOLUSDT"
        || evidence.market != "usdm"
        || evidence.dataset != "binance_usdm_lob"
        || evidence.modalities != ["lob"]
        || actual.len() != evidence.source_segments.len()
        || actual != expected
        || evidence.first_event_time_us
            > view
                .decision_start_ms
                .checked_mul(1000)
                .context("replay start overflow")?
        || evidence.last_event_time_us
            < (view.end_ms - 1000)
                .checked_mul(1000)
                .context("replay end overflow")?
    {
        bail!("sequence replay differs from validation PIT sources or clock coverage");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> SequenceViewV1 {
        SequenceViewV1 {
            history_start_ms: 60_000,
            decision_start_ms: 120_000,
            end_ms: 180_000,
            decision_stride_ms: 1_000,
        }
    }

    #[test]
    fn sequence_view_rejects_early_observations_and_immature_labels() {
        let view = view();
        assert!(dataset_span_within_view(60_000, 149_000, view).is_ok());
        assert!(dataset_span_within_view(59_000, 149_000, view).is_err());
        assert!(dataset_span_within_view(60_000, 150_000, view).is_err());
    }

    #[test]
    fn sequence_mount_rejects_sealed_files_symlinks_and_omissions() {
        let root = tempfile::tempdir().unwrap();
        let admitted_path = root.path().join("train.json");
        std::fs::write(&admitted_path, b"train").unwrap();
        let admitted = std::collections::BTreeSet::from([admitted_path.canonicalize().unwrap()]);
        require_exact_files(&root.path().canonicalize().unwrap(), &admitted).unwrap();
        let sealed = root.path().join("sealed.json");
        std::fs::write(&sealed, b"holdout").unwrap();
        assert!(require_exact_files(&root.path().canonicalize().unwrap(), &admitted).is_err());
        std::fs::remove_file(&sealed).unwrap();
        std::os::unix::fs::symlink(&admitted_path, root.path().join("link.json")).unwrap();
        assert!(require_exact_files(&root.path().canonicalize().unwrap(), &admitted).is_err());
        std::fs::remove_file(root.path().join("link.json")).unwrap();
        std::fs::remove_file(&admitted_path).unwrap();
        assert!(require_exact_files(&root.path().canonicalize().unwrap(), &admitted).is_err());
    }
}
