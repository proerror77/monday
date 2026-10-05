//! Immutable market cohorts bind native PIT sources, separate targets and a
//! common, independently recomputed training anchor set.
use crate::mission_campaign::sequence::inputs::{require_exact_files, verify_source_replay};
pub(crate) use crate::mission_campaign::sequence::inputs::{safe_relative_path, Artifact};
use crate::mission_render::approved_validation;
use crate::mission_runner::{decode_materialization, validate_materialization};
use alpha_domain::market_encoder_study::MarketDataViewV1;
use anyhow::{bail, Context};
use hft_cex_research_input::market_encoder::{
    derive_market_training_anchors, verify_prepared_feature_equivalence,
    verify_prepared_target_equivalence, MarketFeatureReader, MarketTaskReader,
};
use hft_research_manifest::{
    market_encoder::{
        MarketDataReadRequestV1, MarketFeatureDatasetV1, MarketTargetDatasetV1,
        MarketTrainingAnchorSetV1, FEATURE_PARQUET_SCHEMA, TARGET_PARQUET_SCHEMA, TASK_HORIZON_MS,
    },
    prepared_market::{validate_prepared_producer, PreparedMarketReadyReceiptV2},
    sequence::{valid_sha256, SequenceInputSpecV1, SequenceViewV1},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub const INPUTS_SCHEMA: &str = "monday.sol_market_encoder_inputs.v1";
pub const PREPARED_INPUTS_SCHEMA: &str = "monday.sol_market_encoder_inputs.v2";
pub const SOURCES_SCHEMA: &str = "monday.sol_market_encoder_sources.v1";
pub const PREPARED_SOURCES_SCHEMA: &str = "monday.sol_market_encoder_sources.v2";
pub(crate) const PREPARED_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketDatasetLocation {
    pub features: Artifact,
    pub targets: Artifact,
    pub sources: Artifact,
    pub qualified_anchors: Option<Artifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketCampaignInputs {
    pub schema_version: String,
    pub producer_source_revision: String,
    pub producer_image: String,
    pub pvc_name: String,
    pub pvc_uid: String,
    pub sub_path: String,
    pub fold_id: u8,
    pub train: MarketDatasetLocation,
    pub validation: MarketDatasetLocation,
    pub replay_artifact: Artifact,
    pub replay_manifest: Artifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketSourceIndex {
    pub schema_version: String,
    pub sources: Vec<MarketSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared: Option<Artifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MarketPreparedConversion {
    pub schema_version: String,
    pub producer_source_revision: String,
    pub producer_image: String,
    pub ready_receipt: Artifact,
    pub ready_features: Artifact,
    pub ready_targets: Artifact,
    pub feature_decoded_sha256: String,
    pub target_decoded_sha256: String,
    pub feature_rows: u64,
    pub target_rows: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketSource {
    pub receipt: Artifact,
    pub materialization: Artifact,
    pub feature_sources: Artifact,
    pub features: Artifact,
    pub targets: Artifact,
}

impl MarketCampaignInputs {
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_identity(
            &self.producer_source_revision,
            &self.producer_image,
            &self.pvc_name,
            &self.pvc_uid,
            &self.sub_path,
            self.fold_id,
        )?;
        if !matches!(
            self.schema_version.as_str(),
            INPUTS_SCHEMA | PREPARED_INPUTS_SCHEMA
        ) || self.train.qualified_anchors.is_none()
            || self.validation.qualified_anchors.is_some()
            || self.train == self.validation
        {
            bail!("invalid market cohort schema or training/evaluation anchor isolation");
        }
        for location in [&self.train, &self.validation] {
            for item in [&location.features, &location.targets, &location.sources]
                .into_iter()
                .chain(location.qualified_anchors.iter())
            {
                item.validate()?;
            }
            if let Some(anchors) = &location.qualified_anchors {
                let parent = Path::new(&location.features.file)
                    .parent()
                    .context("feature parent")?;
                if Path::new(&anchors.file)
                    != parent.join(format!("{}.market-anchors.json", anchors.sha256))
                {
                    bail!("market anchor reference is not beside its feature manifest");
                }
            }
        }
        self.replay_artifact.validate()?;
        self.replay_manifest.validate()?;
        Ok(())
    }

    pub fn verify_dataset(
        &self,
        root: &Path,
        location: &MarketDatasetLocation,
    ) -> anyhow::Result<(MarketFeatureDatasetV1, MarketTargetDatasetV1)> {
        self.validate()?;
        if location != &self.train && location != &self.validation {
            bail!("market dataset is not a declared cohort location");
        }
        let (features, targets) = read_datasets(root, location)?;
        if features.source_manifest_sha256 != location.sources.sha256 {
            bail!("market cohort feature provenance differs from its source index");
        }
        let index = read_source_index(root, location)?;
        if (self.schema_version == INPUTS_SCHEMA && index.prepared.is_some())
            || (self.schema_version == PREPARED_INPUTS_SCHEMA
                && (index.prepared.is_some() != (location == &self.train)))
        {
            bail!("market cohort schema does not admit this prepared training location");
        }
        if location == &self.validation && index.sources.len() != 1 {
            bail!("market validation must use one original contiguous replay");
        }
        let mut expected_features = Vec::new();
        let mut expected_targets = Vec::new();
        let mut receipts = BTreeSet::new();
        let mut original_features = Vec::new();
        let mut original_targets = Vec::new();
        for source in &index.sources {
            if !receipts.insert(&source.receipt.sha256) {
                bail!("duplicate market source receipt");
            }
            let (source_features, source_targets) = verify_original_source(
                root,
                source,
                &self.producer_source_revision,
                &self.producer_image,
            )?;
            expected_features.extend(source_features.shards.clone());
            expected_targets.extend(source_targets.shards.clone());
            original_features.push((dataset_root(root, &source.features)?, source_features));
            original_targets.push((dataset_root(root, &source.targets)?, source_targets));
        }
        if let Some(conversion) = &index.prepared {
            verify_prepared_conversion(
                root,
                location,
                conversion,
                &features,
                &targets,
                &original_features,
                &original_targets,
            )?;
        } else if features.shards != expected_features || targets.shards != expected_targets {
            bail!("market cohort rewrites, reorders or omits original feature or target shards");
        }
        Ok((features, targets))
    }

    pub fn verify_mount(&self, root: &Path) -> anyhow::Result<()> {
        let mut admitted = BTreeSet::new();
        for artifact in [&self.replay_artifact, &self.replay_manifest] {
            admitted.insert(artifact.path(root)?);
        }
        for location in [&self.train, &self.validation] {
            let (features, targets) = self.verify_dataset(root, location)?;
            for artifact in [&location.features, &location.targets, &location.sources]
                .into_iter()
                .chain(location.qualified_anchors.iter())
            {
                admitted.insert(artifact.path(root)?);
            }
            for source in read_source_index(root, location)?.sources {
                for artifact in [
                    &source.receipt,
                    &source.materialization,
                    &source.feature_sources,
                    &source.features,
                    &source.targets,
                ] {
                    admitted.insert(artifact.path(root)?);
                }
                for original in [&source.features, &source.targets] {
                    let source_dataset: serde_json::Value =
                        serde_json::from_slice(&original.read(root, MANIFEST_BYTES)?)?;
                    let shards: Vec<hft_research_manifest::sequence::SequenceShardV1> =
                        serde_json::from_value(source_dataset["shards"].clone())?;
                    let parent = dataset_root(root, original)?;
                    for shard in shards {
                        admitted.insert(
                            Artifact {
                                file: shard.file,
                                sha256: shard.sha256,
                            }
                            .path(&parent)?,
                        );
                    }
                }
            }
            if let Some(reference) = read_source_index(root, location)?.prepared {
                admitted.insert(reference.path(root)?);
                let conversion: MarketPreparedConversion =
                    serde_json::from_slice(&reference.read(root, MANIFEST_BYTES)?)?;
                for artifact in [
                    &conversion.ready_receipt,
                    &conversion.ready_features,
                    &conversion.ready_targets,
                ] {
                    admitted.insert(artifact.path(root)?);
                }
            }
            for (manifest, shards) in [
                (&location.features, features.shards),
                (&location.targets, targets.shards),
            ] {
                let parent = dataset_root(root, manifest)?;
                for shard in shards {
                    let reference = Artifact {
                        file: shard.file,
                        sha256: shard.sha256,
                    };
                    admitted.insert(reference.path(&parent)?);
                }
            }
        }
        require_exact_files(&root.canonicalize()?, &admitted)
    }

    pub fn verify_view(
        &self,
        root: &Path,
        location: &MarketDatasetLocation,
        view: &MarketDataViewV1,
    ) -> anyhow::Result<(MarketFeatureDatasetV1, MarketTargetDatasetV1)> {
        let (features, targets) = self.verify_dataset(root, location)?;
        if location.features.sha256 != view.features_sha256
            || location.targets.sha256 != view.targets_sha256
            || location.qualified_anchors.as_ref().map(|a| &a.sha256)
                != view.qualified_anchors_sha256.as_ref()
        {
            bail!("market view changed its exact feature, target or anchor identity");
        }
        let request = read_request(location, view.view);
        if let Some(reference) = read_source_index(root, location)?.prepared {
            let conversion: MarketPreparedConversion =
                serde_json::from_slice(&reference.read(root, MANIFEST_BYTES)?)?;
            let ready: PreparedMarketReadyReceiptV2 =
                serde_json::from_slice(&conversion.ready_receipt.read(root, 16 * 1024 * 1024)?)?;
            if ready.request.view != view.view
                || ready.request.anchor_end_ms != request.anchor_end_ms
            {
                bail!("prepared training view escaped the admitted Campaign clocks");
            }
        }
        request.validate().map_err(anyhow::Error::msg)?;
        // The source report can include label-only lookahead beyond a feature
        // partition, but that lookahead must remain inside the admitted view.
        for source in read_source_index(root, location)?.sources {
            verify_source_view(root, &source, view.view)?;
        }
        if let Some(artifact) = &location.qualified_anchors {
            let declared: MarketTrainingAnchorSetV1 =
                serde_json::from_slice(&artifact.read(root, MANIFEST_BYTES)?)?;
            let actual = derive_anchors(root, location, view.view)?;
            if actual != declared || actual.digest().map_err(anyhow::Error::msg)? != artifact.sha256
            {
                bail!(
                    "market training anchor index differs from independently derived eligibility"
                );
            }
        }
        // Full task iteration checks every eligible common anchor. Evaluation
        // uses its unfiltered grid, so missing labels cannot disappear silently.
        let mut reader = open_task_reader(root, location, &request)?;
        let mut examples = 0_u64;
        loop {
            let batch = reader.next_batch(256).map_err(anyhow::Error::msg)?;
            if batch.is_empty() {
                break;
            }
            if location.qualified_anchors.is_none() {
                for row in &batch {
                    let expected = request
                        .view
                        .decision_start_ms
                        .checked_add(
                            i64::try_from(examples)?
                                .checked_mul(request.view.decision_stride_ms)
                                .context("market evaluation grid overflow")?,
                        )
                        .context("market evaluation clock overflow")?;
                    if row.features.observed_at_ms != expected {
                        bail!("market evaluation omitted a decision from its original grid");
                    }
                    examples += 1;
                }
            } else {
                examples += batch.len() as u64;
            }
        }
        reader.finish_pass().map_err(anyhow::Error::msg)?;
        if examples == 0 {
            bail!("market view has no eligible examples");
        }
        if location.qualified_anchors.is_none() {
            let expected = (request.anchor_end_ms - request.view.decision_start_ms - 1)
                / request.view.decision_stride_ms
                + 1;
            if examples != expected as u64 {
                bail!("market evaluation lacks its full decision grid");
            }
        }
        Ok((features, targets))
    }

    pub fn verify_replay(&self, root: &Path, view: SequenceViewV1) -> anyhow::Result<()> {
        self.verify_dataset(root, &self.validation)?;
        let index = read_source_index(root, &self.validation)?;
        let receipt: serde_json::Value =
            serde_json::from_slice(&index.sources[0].receipt.read(root, 1024 * 1024)?)?;
        if receipt["replay_artifact"]["sha256"] != self.replay_artifact.sha256
            || receipt["replay_manifest"]["sha256"] != self.replay_manifest.sha256
        {
            bail!("market replay was replaced after native preparation");
        }
        verify_source_replay(
            root,
            &self.replay_artifact,
            &self.replay_manifest,
            view,
            index.sources.iter().map(|s| &s.materialization),
        )
    }
}

pub(crate) fn validate_identity(
    revision: &str,
    image: &str,
    pvc_name: &str,
    pvc_uid: &str,
    sub_path: &str,
    fold_id: u8,
) -> anyhow::Result<()> {
    if !matches!(fold_id, 1 | 2)
        || !crate::mission_runner::valid_git_revision(revision)
        || !safe_relative_path(sub_path)
        || !sub_path.starts_with("sol-market-encoder/")
        || pvc_uid.is_empty()
        || pvc_uid.len() > 128
        || !pvc_uid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || image
            .rsplit_once("@sha256:")
            .is_none_or(|(name, hash)| name.is_empty() || !valid_sha256(hash))
    {
        bail!("invalid SOL market encoder cohort identity");
    }
    crate::prediction_dispatch::validate_dns_label("market encoder input PVC", pvc_name)
}

pub(crate) fn read_source_index(
    root: &Path,
    location: &MarketDatasetLocation,
) -> anyhow::Result<MarketSourceIndex> {
    let index: MarketSourceIndex =
        serde_json::from_slice(&location.sources.read(root, MANIFEST_BYTES)?)?;
    if !matches!(
        index.schema_version.as_str(),
        SOURCES_SCHEMA | PREPARED_SOURCES_SCHEMA
    ) || (index.schema_version == PREPARED_SOURCES_SCHEMA) != index.prepared.is_some()
        || index.sources.is_empty()
        || index.sources.len() > 512
    {
        bail!("invalid market source index");
    }
    Ok(index)
}

pub(crate) fn verify_prepared_conversion(
    root: &Path,
    location: &MarketDatasetLocation,
    reference: &Artifact,
    features: &MarketFeatureDatasetV1,
    targets: &MarketTargetDatasetV1,
    originals: &[(PathBuf, MarketFeatureDatasetV1)],
    original_targets: &[(PathBuf, MarketTargetDatasetV1)],
) -> anyhow::Result<()> {
    let conversion: MarketPreparedConversion =
        serde_json::from_slice(&reference.read(root, MANIFEST_BYTES)?)?;
    validate_prepared_producer(
        &conversion.producer_source_revision,
        &conversion.producer_image,
    )
    .map_err(anyhow::Error::msg)?;
    let ready: PreparedMarketReadyReceiptV2 =
        serde_json::from_slice(&conversion.ready_receipt.read(root, 16 * 1024 * 1024)?)?;
    ready.validate().map_err(anyhow::Error::msg)?;
    let ready_features: MarketFeatureDatasetV1 =
        serde_json::from_slice(&conversion.ready_features.read(root, MANIFEST_BYTES)?)?;
    let ready_targets: MarketTargetDatasetV1 =
        serde_json::from_slice(&conversion.ready_targets.read(root, MANIFEST_BYTES)?)?;
    ready
        .prepared_view
        .validate_datasets(&ready_features, Some(&ready_targets))
        .map_err(anyhow::Error::msg)?;
    if conversion.schema_version != "monday.market_prepared_conversion.v1"
        || !valid_sha256(&conversion.feature_decoded_sha256)
        || !valid_sha256(&conversion.target_decoded_sha256)
        || conversion.feature_rows == 0
        || conversion.target_rows == 0
        || ready.producer_source_revision != conversion.producer_source_revision
        || ready.producer_image != conversion.producer_image
        || ready.feature_manifest.sha256 != conversion.ready_features.sha256
        || ready.target_manifest.as_ref().map(|r| &r.sha256)
            != Some(&conversion.ready_targets.sha256)
        || ready.request.purpose != "pre_holdout_supervised"
        || ready.request.qualified_anchors
        || ready.request.input != features.input
        || features.schema_version != FEATURE_PARQUET_SCHEMA
        || targets.schema_version != TARGET_PARQUET_SCHEMA
        || features.input != ready_features.input
        || ready.request.sources.len() != originals.len()
        || originals.len() != original_targets.len()
    {
        bail!("prepared Campaign conversion does not bind the converter or original sources");
    }
    let prepared_bytes = features
        .shards
        .iter()
        .chain(&targets.shards)
        .try_fold(0_u64, |sum, shard| sum.checked_add(shard.bytes))
        .context("prepared byte count overflow")?;
    if prepared_bytes > PREPARED_CACHE_BYTES {
        bail!("prepared numerical cache exceeds the admitted SOL cohort byte budget");
    }
    for ((source, (_, feature)), (_, target)) in ready
        .prepared_view
        .sources
        .iter()
        .zip(originals)
        .zip(original_targets)
    {
        if source.feature_dataset_sha256 != feature.digest().map_err(anyhow::Error::msg)?
            || source.target_dataset_sha256 != Some(target.digest().map_err(anyhow::Error::msg)?)
            || source.source_manifest_sha256 != feature.source_manifest_sha256
        {
            bail!("prepared conversion replaced or reordered native source identities");
        }
    }
    let rebind = |shards: &[hft_research_manifest::sequence::SequenceShardV1]| {
        shards
            .iter()
            .cloned()
            .map(|mut s| {
                s.file = format!("{}.parquet", s.sha256);
                s
            })
            .collect::<Vec<_>>()
    };
    if features.shards != rebind(&ready_features.shards)
        || targets.shards != rebind(&ready_targets.shards)
    {
        bail!("prepared conversion rewrote its immutable shard mapping");
    }
    let feature_proof = verify_prepared_feature_equivalence(
        originals,
        &dataset_root(root, &location.features)?,
        features,
    )
    .map_err(anyhow::Error::msg)?;
    let target_proof = verify_prepared_target_equivalence(
        original_targets,
        &dataset_root(root, &location.targets)?,
        targets,
    )
    .map_err(anyhow::Error::msg)?;
    if feature_proof.decoded_sha256 != conversion.feature_decoded_sha256
        || target_proof.decoded_sha256 != conversion.target_decoded_sha256
        || feature_proof.rows != conversion.feature_rows
        || target_proof.rows != conversion.target_rows
    {
        bail!("prepared conversion differs from the complete native numerical proof");
    }
    Ok(())
}

fn read_datasets(
    root: &Path,
    location: &MarketDatasetLocation,
) -> anyhow::Result<(MarketFeatureDatasetV1, MarketTargetDatasetV1)> {
    let features: MarketFeatureDatasetV1 =
        serde_json::from_slice(&location.features.read(root, MANIFEST_BYTES)?)?;
    let targets: MarketTargetDatasetV1 =
        serde_json::from_slice(&location.targets.read(root, MANIFEST_BYTES)?)?;
    if features.digest().map_err(anyhow::Error::msg)? != location.features.sha256
        || targets.digest().map_err(anyhow::Error::msg)? != location.targets.sha256
        || features.input != SequenceInputSpecV1::sol_lob()
        || targets.feature_dataset_sha256 != location.features.sha256
    {
        bail!("market features or targets changed their canonical identity or binding");
    }
    Ok((features, targets))
}

pub(crate) fn verify_original_source(
    root: &Path,
    source: &MarketSource,
    revision: &str,
    image: &str,
) -> anyhow::Result<(MarketFeatureDatasetV1, MarketTargetDatasetV1)> {
    let receipt: serde_json::Value =
        serde_json::from_slice(&source.receipt.read(root, 1024 * 1024)?)?;
    if receipt["schema_version"] != "monday.cex_campaign_inputs.v1"
        || receipt["source_revision"] != revision
        || receipt["image_ref"] != image
        || receipt["symbol"] != "SOLUSDT"
        || receipt["market"] != "usdm"
        || receipt["materialization"]["sha256"] != source.materialization.sha256
    {
        bail!("market source is not the declared native SOL preparation receipt");
    }
    let report_bytes = source.materialization.read(root, 16 * 1024 * 1024)?;
    let report: serde_json::Value = serde_json::from_slice(&report_bytes)?;
    let materialization = decode_materialization(&report_bytes)?;
    validate_materialization(
        &materialization,
        &materialization.artifact_sha256,
        &approved_validation(&materialization)?,
    )?;
    let (features, targets) = read_datasets(
        root,
        &MarketDatasetLocation {
            features: source.features.clone(),
            targets: source.targets.clone(),
            sources: source.materialization.clone(),
            qualified_anchors: None,
        },
    )?;
    let exported = &report["market_encoder"];
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
        || exported["schema_version"] != "monday.market_encoder_export.v1"
        || exported["features"]["sha256"] != source.features.sha256
        || exported["targets"]["sha256"] != source.targets.sha256
        || exported["features"]["file"]
            != format!("{}.market-features.json", source.features.sha256)
        || exported["targets"]["file"] != format!("{}.market-targets.json", source.targets.sha256)
    {
        bail!("market source differs from verified PIT output or original target binding");
    }
    verify_feature_sources(
        &report,
        &source.feature_sources.read(root, 16 * 1024 * 1024)?,
        &source.feature_sources.sha256,
        &features,
    )?;
    if !exported["feature_end_received_at_ns"].is_null() {
        let end = exported["feature_end_received_at_ns"]
            .as_u64()
            .context("invalid market partition end")?;
        if end % 1_000_000_000 != 0
            || features
                .shards
                .iter()
                .any(|s| s.last_observed_at_ms as u128 * 1_000_000 >= u128::from(end))
        {
            bail!("market source escaped its feature partition");
        }
    }
    Ok((features, targets))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeatureSources {
    schema_version: String,
    market: String,
    symbol: String,
    replay_clock: String,
    source_revision: String,
    source_segments: Vec<serde_json::Value>,
    feature_start_received_at_ns: Option<u64>,
    feature_end_received_at_ns: Option<u64>,
    first_feature_observed_at_ms: i64,
    last_feature_observed_at_ms: i64,
    first_dependency_received_at_ns: u64,
    last_dependency_received_at_ns: u64,
}

/// Shared pure provenance check for native preparation, cohort admission and
/// independent readback. The label-bound PIT snapshot is checked separately;
/// the full feature source identity covers warmup and unlabeled tail frames.
pub(crate) fn verify_feature_sources(
    report: &serde_json::Value,
    source_bytes: &[u8],
    expected_sha256: &str,
    features: &MarketFeatureDatasetV1,
) -> anyhow::Result<()> {
    if source_bytes.len() > 16 * 1024 * 1024 {
        bail!("market feature source exceeds byte bound");
    }
    features.validate().map_err(anyhow::Error::msg)?;
    let sources: FeatureSources = serde_json::from_slice(source_bytes)?;
    let exported = &report["market_encoder"];
    let optional_clock = |value: &serde_json::Value| -> anyhow::Result<Option<u64>> {
        if value.is_null() {
            Ok(None)
        } else {
            Ok(Some(
                value
                    .as_u64()
                    .context("invalid feature source window clock")?,
            ))
        }
    };
    let raw_start = optional_clock(&report["output_start_received_at_ns"])?;
    let feature_start = optional_clock(&exported["feature_start_received_at_ns"])?;
    let expected_start = feature_start.or(raw_start);
    let label_end = optional_clock(&report["output_end_received_at_ns"])?;
    let expected_end = optional_clock(&exported["feature_end_received_at_ns"])?.or(label_end);
    if let Some(start) = feature_start {
        let raw_start =
            raw_start.context("explicit feature start has no admitted raw/PIT window")?;
        let end = expected_end.context("explicit feature start has no admitted end")?;
        if start % 1_000_000_000 != 0
            || start < raw_start
            || start - raw_start > 60_000_000_000
            || start >= end
        {
            bail!("market feature start escaped its 60-second raw/PIT warmup");
        }
    }
    let first = features
        .shards
        .first()
        .context("feature source has no shards")?
        .first_observed_at_ms;
    let last = features
        .shards
        .last()
        .context("feature source has no shards")?
        .last_observed_at_ms;
    let raw = report["source_segments"]
        .as_array()
        .context("missing feature source segments")?;
    let hashes = sources
        .source_segments
        .iter()
        .map(|s| s["sha256"].as_str().context("feature source has no digest"))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let revision = hft_collector::lob_archiver::source_revision(hashes);
    let first_source = sources
        .source_segments
        .iter()
        .map(|s| {
            s["start_received_at_ns"]
                .as_u64()
                .context("missing source start")
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .min()
        .context("feature source has no segments")?;
    let last_source = sources
        .source_segments
        .iter()
        .map(|s| {
            s["end_received_at_ns"]
                .as_u64()
                .context("missing source end")
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .max()
        .context("feature source has no segments")?;
    let first_ns = u64::try_from(first)?
        .checked_mul(1_000_000)
        .context("feature source clock overflow")?;
    let last_ns = u64::try_from(last)?
        .checked_mul(1_000_000)
        .context("feature source clock overflow")?;
    if hft_research_manifest::market_encoder::bytes_digest(source_bytes) != expected_sha256
        || exported["schema_version"] != "monday.market_encoder_export.v1"
        || exported["features"]["sha256"] != features.digest().map_err(anyhow::Error::msg)?
        || features.source_manifest_sha256 != expected_sha256
        || exported["feature_sources"]["sha256"] != expected_sha256
        || exported["feature_sources"]["file"]
            != format!("{expected_sha256}.market-feature-sources.json")
        || sources.schema_version != "monday.market_feature_sources.v1"
        || sources.market != "usdm"
        || sources.symbol != "SOLUSDT"
        || sources.replay_clock != hft_research_manifest::CEX_REPLAY_CLOCK_RECEIVED_AT_NS
        || report["source_revision"] != sources.source_revision
        || sources.source_revision != revision
        || sources.source_segments != *raw
        || sources.feature_start_received_at_ns != expected_start
        || sources.feature_end_received_at_ns != expected_end
        || sources.first_feature_observed_at_ms != first
        || sources.last_feature_observed_at_ms != last
        || first_ns
            .checked_sub(1_000_000_000)
            .is_none_or(|t| sources.first_dependency_received_at_ns > t)
        || sources.first_dependency_received_at_ns < first_source
        || sources.last_dependency_received_at_ns != last_ns
        || last_ns > last_source
        || sources
            .feature_start_received_at_ns
            .is_some_and(|t| t > first_ns)
        || sources
            .feature_end_received_at_ns
            .is_some_and(|t| t <= last_ns)
    {
        bail!("market feature source manifest differs from original raw sources or full feature bounds");
    }
    Ok(())
}

fn verify_source_view(
    root: &Path,
    source: &MarketSource,
    view: SequenceViewV1,
) -> anyhow::Result<()> {
    let bytes = source.materialization.read(root, 16 * 1024 * 1024)?;
    let report: serde_json::Value = serde_json::from_slice(&bytes)?;
    let materialization = decode_materialization(&bytes)?;
    let snapshot = materialization.snapshot;
    let features: MarketFeatureDatasetV1 =
        serde_json::from_slice(&source.features.read(root, MANIFEST_BYTES)?)?;
    let source_bytes = source.feature_sources.read(root, 16 * 1024 * 1024)?;
    let sources: FeatureSources = serde_json::from_slice(&source_bytes)?;
    let first_shard = features
        .shards
        .first()
        .context("market source has no feature shard")?;
    let reference = Artifact {
        file: first_shard.file.clone(),
        sha256: first_shard.sha256.clone(),
    };
    let path = reference.path(&dataset_root(root, &source.features)?)?;
    let mut first_line = Vec::new();
    use std::io::{BufRead, Read};
    std::io::BufReader::new(std::fs::File::open(path)?)
        .take(32 * 1024 + 1)
        .read_until(b'\n', &mut first_line)?;
    if first_line.len() > 32 * 1024 || first_line.last() != Some(&b'\n') {
        bail!("invalid bounded source feature frame");
    }
    let first_frame: hft_research_manifest::market_encoder::MarketFeatureFrameV1 =
        serde_json::from_slice(&first_line)?;
    first_frame
        .validate(&features.input)
        .map_err(anyhow::Error::msg)?;
    if first_frame.observed_at_ms != sources.first_feature_observed_at_ms
        || first_frame.series_id != sources.first_dependency_received_at_ns
    {
        bail!("market feature source changed its original recovery dependency");
    }
    // Only the feature/target rows are mounted, not the native PIT row body.
    // An explicitly bound preparation may retain up to 60s of prior PIT
    // context while all feature and target shards remain inside this view.
    let feature_start = report["market_encoder"]["feature_start_received_at_ns"].as_u64();
    let pit_start_ms = if let Some(feature_start) = feature_start {
        let raw_start = report["output_start_received_at_ns"]
            .as_u64()
            .context("missing admitted raw/PIT start")?;
        if feature_start < raw_start
            || feature_start - raw_start > 60_000_000_000
            || u128::from(feature_start) < view.history_start_ms as u128 * 1_000_000
        {
            bail!("market source warmup or feature start escaped its admitted view");
        }
        i64::try_from(raw_start / 1_000_000).context("market raw/PIT start overflow")?
    } else {
        view.history_start_ms
    };
    if snapshot.first_event_time.timestamp_millis() < pit_start_ms
        || report["output_end_received_at_ns"]
            .as_u64()
            .is_some_and(|end| u128::from(end) > view.end_ms as u128 * 1_000_000)
        || snapshot
            .last_event_time
            .timestamp_millis()
            .checked_add(TASK_HORIZON_MS)
            .is_none_or(|end| end >= view.end_ms)
        || report["market_encoder"]["feature_end_received_at_ns"]
            .as_u64()
            .is_some_and(|end| u128::from(end) > view.end_ms as u128 * 1_000_000)
    {
        bail!("market source PIT or label-only lookahead escaped its admitted view");
    }
    Ok(())
}

fn dataset_root(root: &Path, artifact: &Artifact) -> anyhow::Result<PathBuf> {
    Ok(artifact
        .path(root)?
        .parent()
        .context("market dataset parent")?
        .to_owned())
}

pub(crate) fn read_request(
    location: &MarketDatasetLocation,
    view: SequenceViewV1,
) -> MarketDataReadRequestV1 {
    MarketDataReadRequestV1 {
        feature_dataset_sha256: location.features.sha256.clone(),
        qualified_anchors_sha256: location
            .qualified_anchors
            .as_ref()
            .map(|a| a.sha256.clone()),
        input: SequenceInputSpecV1::sol_lob(),
        view,
        anchor_end_ms: view.end_ms - TASK_HORIZON_MS,
    }
}

pub(crate) fn derive_anchors(
    root: &Path,
    location: &MarketDatasetLocation,
    view: SequenceViewV1,
) -> anyhow::Result<MarketTrainingAnchorSetV1> {
    let (features, targets) = read_datasets(root, location)?;
    let mut request = read_request(location, view);
    request.qualified_anchors_sha256 = None;
    let mut reader =
        MarketFeatureReader::open(&dataset_root(root, &location.features)?, features, &request)
            .map_err(anyhow::Error::msg)?;
    derive_market_training_anchors(
        &mut reader,
        &dataset_root(root, &location.targets)?,
        targets,
        &location.targets.sha256,
    )
    .map_err(anyhow::Error::msg)
}

pub fn open_feature_reader(
    root: &Path,
    location: &MarketDatasetLocation,
    request: &MarketDataReadRequestV1,
) -> anyhow::Result<MarketFeatureReader> {
    if request.feature_dataset_sha256 != location.features.sha256
        || request.qualified_anchors_sha256.as_ref()
            != location.qualified_anchors.as_ref().map(|a| &a.sha256)
    {
        bail!("market reader differs from its admitted feature or anchor identity");
    }
    // Deliberately do not open target metadata or rows on a pretraining read.
    let features: MarketFeatureDatasetV1 =
        serde_json::from_slice(&location.features.read(root, MANIFEST_BYTES)?)?;
    MarketFeatureReader::open(&dataset_root(root, &location.features)?, features, request)
        .map_err(anyhow::Error::msg)
}

pub fn open_task_reader(
    root: &Path,
    location: &MarketDatasetLocation,
    request: &MarketDataReadRequestV1,
) -> anyhow::Result<MarketTaskReader> {
    let features = open_feature_reader(root, location, request)?;
    let targets: MarketTargetDatasetV1 =
        serde_json::from_slice(&location.targets.read(root, MANIFEST_BYTES)?)?;
    MarketTaskReader::open(
        features,
        &dataset_root(root, &location.targets)?,
        targets,
        &location.targets.sha256,
    )
    .map_err(anyhow::Error::msg)
}

#[cfg(all(test, feature = "scientific"))]
pub(super) mod tests {
    use super::*;
    use crate::mission_campaign::sequence::cohort::put_metadata;
    use hft_research_manifest::market_encoder::{
        MarketFeatureFrameV1, MarketTargetFrameV1, FEATURE_SCHEMA, TARGET_SCHEMA,
    };
    use hft_research_manifest::sequence::SequenceShardV1;

    fn save<T: Serialize>(root: &Path, value: &T, suffix: &str) -> Artifact {
        put_metadata(root, &serde_json::to_vec(value).unwrap(), suffix).unwrap()
    }

    fn shard<T: Serialize>(
        root: &Path,
        rows: &[T],
        suffix: &str,
        first: i64,
        last: i64,
    ) -> SequenceShardV1 {
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        let artifact = put_metadata(root, &bytes, suffix).unwrap();
        SequenceShardV1 {
            file: artifact.file,
            sha256: artifact.sha256,
            bytes: bytes.len() as u64,
            rows: rows.len() as u64,
            first_observed_at_ms: first,
            last_observed_at_ms: last,
        }
    }

    pub(crate) fn source(root: &Path, start: i64, missing_target: bool) -> MarketSource {
        source_with_warmup(root, start, missing_target, 0)
    }

    fn source_with_warmup(
        root: &Path,
        start: i64,
        missing_target: bool,
        warmup_ms: i64,
    ) -> MarketSource {
        let pit_start = start - warmup_ms;
        // Reuse the validated native PIT fixture, adapting its typed snapshot
        // to this controlled SOL source; no real data is loaded by these tests.
        let fixture = crate::mission_runner::tests::finalizing_fixture("market-cohort");
        let mut report = fixture.materialization.clone();
        std::fs::remove_dir_all(fixture.root).unwrap();
        let dt = |ms| chrono::DateTime::from_timestamp_millis(ms).unwrap();
        let mut snapshot: hft_research_manifest::CexReplaySnapshotV5 =
            serde_json::from_value(report["snapshot"].clone()).unwrap();
        snapshot.symbol = "SOLUSDT".into();
        snapshot.label_horizon_buckets = 30;
        snapshot.first_event_time = dt(pit_start);
        snapshot.last_event_time = dt(start + 149_000);
        snapshot.instrument_rules.available_at = dt(pit_start - 1000);
        snapshot.instrument_rules.valid_through = dt(start + 180_000);
        snapshot.series[0].first_event_time = snapshot.first_event_time;
        snapshot.series[0].last_event_time = snapshot.last_event_time;
        snapshot.series[0]
            .instrument_rules_coverage
            .first_available_at = dt(pit_start - 1000);
        snapshot.series[0]
            .instrument_rules_coverage
            .last_available_at = dt(start + 180_000);
        snapshot.source_segments[0].start_received_at_ns = (pit_start as u64 - 1000) * 1_000_000;
        snapshot.source_segments[0].end_received_at_ns = (start as u64 + 180_000) * 1_000_000;
        snapshot.source_segments[0].events = 181;
        snapshot.validate().unwrap();
        report["symbol"] = "SOLUSDT".into();
        report["label_horizon_buckets"] = 30.into();
        report["first_event_time"] = serde_json::to_value(snapshot.first_event_time).unwrap();
        report["last_event_time"] = serde_json::to_value(snapshot.last_event_time).unwrap();
        report["source_segments"][0]["start_received_at_ns"] =
            snapshot.source_segments[0].start_received_at_ns.into();
        report["source_segments"][0]["end_received_at_ns"] =
            snapshot.source_segments[0].end_received_at_ns.into();
        report["source_segments"][0]["events"] = 181.into();
        report["snapshot_sha256"] = snapshot.sha256().into();
        report["snapshot"] = serde_json::to_value(&snapshot).unwrap();
        if warmup_ms != 0 {
            report["output_start_received_at_ns"] = (pit_start as u64 * 1_000_000).into();
            report["output_end_received_at_ns"] = ((start as u64 + 180_000) * 1_000_000).into();
        }
        let feature_start = (warmup_ms != 0).then_some(start as u64 * 1_000_000);
        let feature_rows = (0..180)
            .map(|i| MarketFeatureFrameV1 {
                series_id: (pit_start as u64 - 1000) * 1_000_000,
                observed_at_ms: start + i * 1000,
                feature_max_available_at_ms: start + i * 1000,
                channels: vec![i as f32 / 180.; 24],
            })
            .collect::<Vec<_>>();
        let feature_shard = shard(
            root,
            &feature_rows,
            "market-features.jsonl",
            start,
            start + 179_000,
        );
        let feature_sources = save(
            root,
            &serde_json::json!({
                "schema_version":"monday.market_feature_sources.v1","market":"usdm","symbol":"SOLUSDT",
                "replay_clock":hft_research_manifest::CEX_REPLAY_CLOCK_RECEIVED_AT_NS,
                "source_revision":report["source_revision"],"source_segments":report["source_segments"],
                "feature_start_received_at_ns":feature_start,"feature_end_received_at_ns":(start as u64+180_000)*1_000_000,
                "first_feature_observed_at_ms":start,"last_feature_observed_at_ms":start+179_000,
                "first_dependency_received_at_ns":(pit_start as u64-1000)*1_000_000,
                "last_dependency_received_at_ns":(start as u64+179_000)*1_000_000,
            }),
            "market-feature-sources.json",
        );
        let features = save(
            root,
            &MarketFeatureDatasetV1 {
                schema_version: FEATURE_SCHEMA.into(),
                venue: "binance-usdm".into(),
                symbol: "SOLUSDT".into(),
                source_manifest_sha256: feature_sources.sha256.clone(),
                input: SequenceInputSpecV1::sol_lob(),
                shards: vec![feature_shard],
            },
            "market-features.json",
        );
        let target_rows = (0..150)
            .filter(|i| !missing_target || *i != 100)
            .map(|i| MarketTargetFrameV1 {
                series_id: (pit_start as u64 - 1000) * 1_000_000,
                observed_at_ms: start + i * 1000,
                available_at_ms: start + (i + 30) * 1000,
                simple_return: i as f32 / 10000.,
                spread_bps: 1.,
            })
            .collect::<Vec<_>>();
        let target_shard = shard(
            root,
            &target_rows,
            "market-targets.jsonl",
            start,
            start + 149_000,
        );
        let targets = save(
            root,
            &MarketTargetDatasetV1 {
                schema_version: TARGET_SCHEMA.into(),
                feature_dataset_sha256: features.sha256.clone(),
                horizon_ms: TASK_HORIZON_MS,
                shards: vec![target_shard],
            },
            "market-targets.json",
        );
        report["market_encoder"] = serde_json::json!({"schema_version":"monday.market_encoder_export.v1",
            "feature_start_received_at_ns":feature_start,"feature_end_received_at_ns":(start as u64+180_000)*1_000_000,"feature_sources":feature_sources,"features":features,"targets":targets});
        let materialization = save(root, &report, "materialization.json");
        let (replay_artifact, replay_manifest) = replay(root, &report, start);
        let receipt = save(
            root,
            &serde_json::json!({"schema_version":"monday.cex_campaign_inputs.v1",
            "source_revision":"a".repeat(40),"image_ref":format!("registry/runner@sha256:{}","b".repeat(64)),
            "symbol":"SOLUSDT","market":"usdm","feature":{"sha256":snapshot.feature_artifact_sha256},
            "materialization":{"sha256":materialization.sha256},
                "replay_artifact":replay_artifact,"replay_manifest":replay_manifest}),
            "receipt.json",
        );
        MarketSource {
            receipt,
            materialization,
            feature_sources,
            features,
            targets,
        }
    }

    fn replay(root: &Path, report: &serde_json::Value, start: i64) -> (Artifact, Artifact) {
        use parquet::{
            data_type::{ByteArray, ByteArrayType, Int64Type},
            file::{
                properties::WriterProperties,
                writer::{SerializedFileWriter, SerializedRowGroupWriter},
            },
            schema::parser::parse_message_type,
        };
        use std::sync::Arc;
        fn ints(group: &mut SerializedRowGroupWriter<'_, std::fs::File>, values: &[i64]) {
            let mut column = group.next_column().unwrap().unwrap();
            column
                .typed::<Int64Type>()
                .write_batch(values, None, None)
                .unwrap();
            column.close().unwrap();
        }
        fn strings(group: &mut SerializedRowGroupWriter<'_, std::fs::File>, values: &[String]) {
            let values = values
                .iter()
                .map(|s| ByteArray::from(s.as_str()))
                .collect::<Vec<_>>();
            let mut column = group.next_column().unwrap().unwrap();
            column
                .typed::<ByteArrayType>()
                .write_batch(&values, None, None)
                .unwrap();
            column.close().unwrap();
        }
        let path = root.join("temporary-replay.parquet");
        let schema = Arc::new(parse_message_type("message binance_replay { REQUIRED INT64 timestamp_us; REQUIRED INT64 sequence; REQUIRED BINARY event (UTF8); REQUIRED BINARY payload_json (UTF8); }").unwrap());
        let mut writer = SerializedFileWriter::new(
            std::fs::File::create(&path).unwrap(),
            schema,
            Arc::new(WriterProperties::builder().build()),
        )
        .unwrap();
        let times = (0..181)
            .map(|i| (start - 1000 + i * 1000) * 1000)
            .collect::<Vec<_>>();
        let sequences = (1..=181).collect::<Vec<i64>>();
        let events = (0..181)
            .map(|i| if i == 0 { "snapshot" } else { "l2_update" }.to_owned())
            .collect::<Vec<_>>();
        let levels = serde_json::json!({"bids":[["99","10"],["98","10"],["97","10"],["96","10"],["95","10"]],
            "asks":[["101","10"],["102","10"],["103","10"],["104","10"],["105","10"]]});
        let payloads = vec![serde_json::to_string(&levels).unwrap(); 181];
        let mut group = writer.next_row_group().unwrap();
        ints(&mut group, &times);
        ints(&mut group, &sequences);
        strings(&mut group, &events);
        strings(&mut group, &payloads);
        group.close().unwrap();
        writer.close().unwrap();
        let artifact = put_metadata(root, &std::fs::read(&path).unwrap(), "parquet").unwrap();
        std::fs::remove_file(path).unwrap();
        let mut source = report["source_segments"][0].clone();
        let object = source.as_object_mut().unwrap();
        object.remove("path");
        object.remove("collector_manifest_path");
        object.remove("success_marker_path");
        object.insert("file".into(), "segment.jsonl.zst".into());
        let manifest = save(
            root,
            &serde_json::json!({"dataset_kind":"backtest_canonical_replay_parquet",
            "schema_version":"binance-replay-parquet-v1","format":"parquet",
            "parquet_schema":"timestamp_us:int64,sequence:int64,event:utf8,payload_json:utf8",
            "mission_id":"data-1","market":"usdm","symbol":"SOLUSDT","dataset":"binance_usdm_lob","modalities":["lob"],
            "source_revision":report["source_revision"],"source_segments":[source],"rows":181,
            "first_event_time_us":times[0],"last_event_time_us":times[180],"sequence_start":1,"sequence_end":181,
            "artifact_path":artifact.file,"artifact_sha256":artifact.sha256,"point_in_time":true}),
            "replay.json",
        );
        (artifact, manifest)
    }

    fn location(root: &Path, source: MarketSource) -> MarketDatasetLocation {
        multi_source_location(root, vec![source])
    }

    pub(crate) fn multi_source_location(
        root: &Path,
        originals: Vec<MarketSource>,
    ) -> MarketDatasetLocation {
        let mut feature_shards = Vec::new();
        let mut target_shards = Vec::new();
        for source in &originals {
            let original_features: MarketFeatureDatasetV1 =
                serde_json::from_slice(&source.features.read(root, MANIFEST_BYTES).unwrap())
                    .unwrap();
            let original_targets: MarketTargetDatasetV1 =
                serde_json::from_slice(&source.targets.read(root, MANIFEST_BYTES).unwrap())
                    .unwrap();
            feature_shards.extend(original_features.shards);
            target_shards.extend(original_targets.shards);
        }
        let sources = save(
            root,
            &MarketSourceIndex {
                schema_version: SOURCES_SCHEMA.into(),
                sources: originals,
                prepared: None,
            },
            "market-sources.json",
        );
        let features = save(
            root,
            &MarketFeatureDatasetV1 {
                schema_version: FEATURE_SCHEMA.into(),
                venue: "binance-usdm".into(),
                symbol: "SOLUSDT".into(),
                source_manifest_sha256: sources.sha256.clone(),
                input: SequenceInputSpecV1::sol_lob(),
                shards: feature_shards,
            },
            "market-features.json",
        );
        let targets = save(
            root,
            &MarketTargetDatasetV1 {
                schema_version: TARGET_SCHEMA.into(),
                feature_dataset_sha256: features.sha256.clone(),
                horizon_ms: TASK_HORIZON_MS,
                shards: target_shards,
            },
            "market-targets.json",
        );
        MarketDatasetLocation {
            features,
            targets,
            sources,
            qualified_anchors: None,
        }
    }

    pub(crate) fn fixture() -> (
        tempfile::TempDir,
        MarketCampaignInputs,
        MarketDataViewV1,
        MarketDataViewV1,
    ) {
        let root = tempfile::tempdir().unwrap();
        let start = 1_700_000_000_000;
        let view = |start| SequenceViewV1 {
            history_start_ms: start,
            decision_start_ms: start + 59_000,
            end_ms: start + 180_000,
            decision_stride_ms: 1000,
        };
        let training_source = source(root.path(), start, true);
        let receipt: serde_json::Value = serde_json::from_slice(
            &training_source
                .receipt
                .read(root.path(), MANIFEST_BYTES)
                .unwrap(),
        )
        .unwrap();
        for field in ["replay_artifact", "replay_manifest"] {
            std::fs::remove_file(root.path().join(receipt[field]["file"].as_str().unwrap()))
                .unwrap();
        }
        let mut train = location(root.path(), training_source);
        let anchors = derive_anchors(root.path(), &train, view(start)).unwrap();
        train.qualified_anchors = Some(save(root.path(), &anchors, "market-anchors.json"));
        let validation = location(root.path(), source(root.path(), start + 300_000, false));
        let data_view = |location: &MarketDatasetLocation, view| MarketDataViewV1 {
            features_sha256: location.features.sha256.clone(),
            targets_sha256: location.targets.sha256.clone(),
            qualified_anchors_sha256: location
                .qualified_anchors
                .as_ref()
                .map(|a| a.sha256.clone()),
            view,
        };
        let train_view = data_view(&train, view(start));
        let validation_view = data_view(&validation, view(start + 300_000));
        let validation_source = read_source_index(root.path(), &validation)
            .unwrap()
            .sources
            .remove(0);
        let receipt: serde_json::Value = serde_json::from_slice(
            &validation_source
                .receipt
                .read(root.path(), MANIFEST_BYTES)
                .unwrap(),
        )
        .unwrap();
        let replay_artifact = serde_json::from_value(receipt["replay_artifact"].clone()).unwrap();
        let replay_manifest = serde_json::from_value(receipt["replay_manifest"].clone()).unwrap();
        let inputs = MarketCampaignInputs {
            schema_version: INPUTS_SCHEMA.into(),
            producer_source_revision: "a".repeat(40),
            producer_image: format!("registry/runner@sha256:{}", "b".repeat(64)),
            pvc_name: "sol-inputs".into(),
            pvc_uid: "pvc-uid".into(),
            sub_path: "sol-market-encoder/fold-1".into(),
            fold_id: 1,
            train,
            validation,
            replay_artifact,
            replay_manifest,
        };
        (root, inputs, train_view, validation_view)
    }

    #[test]
    fn market_cohort_provenance_views_and_exact_mount_are_verified() {
        let (root, inputs, train, validation) = fixture();
        inputs
            .verify_view(root.path(), &inputs.train, &train)
            .unwrap();
        inputs
            .verify_view(root.path(), &inputs.validation, &validation)
            .unwrap();
        inputs.verify_mount(root.path()).unwrap();
        inputs.verify_replay(root.path(), validation.view).unwrap();
        let mut foreign = train.clone();
        foreign.features_sha256 = "f".repeat(64);
        assert!(inputs
            .verify_view(root.path(), &inputs.train, &foreign)
            .is_err());
        let mut wrong_clock = train.clone();
        wrong_clock.view.history_start_ms += 1000;
        assert!(inputs
            .verify_view(root.path(), &inputs.train, &wrong_clock)
            .is_err());
        let mut producer = inputs.clone();
        producer.producer_source_revision = "c".repeat(40);
        assert!(producer
            .verify_dataset(root.path(), &producer.train)
            .is_err());
        std::fs::write(root.path().join("sealed.json"), b"withheld").unwrap();
        assert!(inputs.verify_mount(root.path()).is_err());
        std::fs::remove_file(root.path().join("sealed.json")).unwrap();
        std::os::unix::fs::symlink(
            root.path().join(&inputs.train.features.file),
            root.path().join("hidden-link"),
        )
        .unwrap();
        assert!(inputs.verify_mount(root.path()).is_err());
    }

    #[test]
    fn market_cohort_recomputes_anchors_and_pretraining_reads_no_targets() {
        let (root, mut inputs, mut train, _) = fixture();
        let anchor_ref = inputs.train.qualified_anchors.as_ref().unwrap();
        let mut anchors: MarketTrainingAnchorSetV1 =
            serde_json::from_slice(&anchor_ref.read(root.path(), MANIFEST_BYTES).unwrap()).unwrap();
        assert_eq!(anchors.anchors.len(), 90);
        assert!(!anchors
            .anchors
            .iter()
            .any(|a| a.observed_at_ms == train.view.history_start_ms + 100_000));
        // A smaller, structurally valid list must fail independent admission.
        anchors.anchors.remove(0);
        let altered = save(root.path(), &anchors, "market-anchors.json");
        inputs.train.qualified_anchors = Some(altered.clone());
        train.qualified_anchors_sha256 = Some(altered.sha256);
        let error = inputs
            .verify_view(root.path(), &inputs.train, &train)
            .unwrap_err();
        assert!(format!("{error:#}").contains("independently derived"));
        // The P reader's interface never needs even the target manifest.
        std::fs::remove_file(root.path().join(&inputs.train.targets.file)).unwrap();
        let request = read_request(&inputs.train, train.view);
        let mut reader = open_feature_reader(root.path(), &inputs.train, &request).unwrap();
        reader.finish_pass().unwrap();
        assert!(open_task_reader(root.path(), &inputs.train, &request).is_err());
    }

    #[test]
    fn market_cohort_rejects_rebound_original_targets_and_changed_shards() {
        let (root, inputs, _, _) = fixture();
        let source = read_source_index(root.path(), &inputs.train)
            .unwrap()
            .sources
            .remove(0);
        let mut wrong = source.clone();
        let mut target: MarketTargetDatasetV1 =
            serde_json::from_slice(&source.targets.read(root.path(), MANIFEST_BYTES).unwrap())
                .unwrap();
        target.feature_dataset_sha256 = inputs.train.features.sha256.clone();
        wrong.targets = save(root.path(), &target, "market-targets.json");
        assert!(verify_original_source(
            root.path(),
            &wrong,
            &inputs.producer_source_revision,
            &inputs.producer_image
        )
        .is_err());
        let features: MarketFeatureDatasetV1 = serde_json::from_slice(
            &inputs
                .train
                .features
                .read(root.path(), MANIFEST_BYTES)
                .unwrap(),
        )
        .unwrap();
        let shard = root.path().join(&features.shards[0].file);
        let mut bytes = std::fs::read(&shard).unwrap();
        bytes[0] ^= 1;
        std::fs::write(&shard, bytes).unwrap();
        assert!(open_feature_reader(
            root.path(),
            &inputs.train,
            &read_request(
                &inputs.train,
                SequenceViewV1 {
                    history_start_ms: 1_700_000_000_000,
                    decision_start_ms: 1_700_000_059_000,
                    end_ms: 1_700_000_180_000,
                    decision_stride_ms: 1000
                }
            )
        )
        .is_err());
    }

    #[test]
    fn market_cohort_rejects_receipt_and_report_tampering() {
        let (root, inputs, _, _) = fixture();
        let source = read_source_index(root.path(), &inputs.train)
            .unwrap()
            .sources
            .remove(0);
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&source.receipt.read(root.path(), MANIFEST_BYTES).unwrap())
                .unwrap();
        receipt["feature"]["sha256"] = "f".repeat(64).into();
        let altered = MarketSource {
            receipt: save(root.path(), &receipt, "receipt.json"),
            ..source.clone()
        };
        assert!(verify_original_source(
            root.path(),
            &altered,
            &inputs.producer_source_revision,
            &inputs.producer_image
        )
        .is_err());
        let mut report: serde_json::Value = serde_json::from_slice(
            &source
                .materialization
                .read(root.path(), 16 * 1024 * 1024)
                .unwrap(),
        )
        .unwrap();
        report["source_segments"][0]["events"] = 999.into();
        let altered = MarketSource {
            materialization: save(root.path(), &report, "materialization.json"),
            ..source
        };
        receipt["materialization"]["sha256"] = altered.materialization.sha256.clone().into();
        let altered = MarketSource {
            receipt: save(root.path(), &receipt, "receipt.json"),
            ..altered
        };
        assert!(verify_original_source(
            root.path(),
            &altered,
            &inputs.producer_source_revision,
            &inputs.producer_image
        )
        .is_err());
    }
    #[test]
    fn market_cohort_anchor_membership_ignores_label_values_and_resets_at_gaps() {
        let (root, inputs, train, _) = fixture();
        let original = derive_anchors(root.path(), &inputs.train, train.view).unwrap();
        let mut location = inputs.train.clone();
        let mut targets: MarketTargetDatasetV1 =
            serde_json::from_slice(&location.targets.read(root.path(), MANIFEST_BYTES).unwrap())
                .unwrap();
        let mut rows: Vec<MarketTargetFrameV1> =
            std::fs::read_to_string(root.path().join(&targets.shards[0].file))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        for row in &mut rows {
            row.simple_return = -100. * row.simple_return + 2.;
        }
        targets.shards = vec![shard(
            root.path(),
            &rows,
            "market-targets.jsonl",
            rows[0].observed_at_ms,
            rows.last().unwrap().observed_at_ms,
        )];
        location.targets = save(root.path(), &targets, "market-targets.json");
        assert_eq!(
            original,
            derive_anchors(root.path(), &location, train.view).unwrap()
        );
        let mut features: MarketFeatureDatasetV1 =
            serde_json::from_slice(&location.features.read(root.path(), MANIFEST_BYTES).unwrap())
                .unwrap();
        let feature_rows: Vec<MarketFeatureFrameV1> =
            std::fs::read_to_string(root.path().join(&features.shards[0].file))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<MarketFeatureFrameV1>(line).unwrap())
                .filter(|row| row.observed_at_ms != train.view.history_start_ms + 90_000)
                .collect();
        features.shards = vec![shard(
            root.path(),
            &feature_rows,
            "market-features.jsonl",
            feature_rows[0].observed_at_ms,
            feature_rows.last().unwrap().observed_at_ms,
        )];
        location.features = save(root.path(), &features, "market-features.json");
        targets.feature_dataset_sha256 = location.features.sha256.clone();
        location.targets = save(root.path(), &targets, "market-targets.json");
        let gapped = derive_anchors(root.path(), &location, train.view).unwrap();
        assert_eq!(gapped.anchors.len(), 31);
        assert!(gapped.anchors.iter().all(|row| row.observed_at_ms
            < train.view.history_start_ms + 90_000
            || row.observed_at_ms >= train.view.history_start_ms + 150_000));
        rows.iter_mut()
            .find(|row| row.observed_at_ms == train.view.decision_start_ms)
            .unwrap()
            .series_id = 2;
        targets.shards = vec![shard(
            root.path(),
            &rows,
            "market-targets.jsonl",
            rows[0].observed_at_ms,
            rows.last().unwrap().observed_at_ms,
        )];
        location.targets = save(root.path(), &targets, "market-targets.json");
        assert!(derive_anchors(root.path(), &location, train.view).is_err());
    }

    #[test]
    fn market_cohort_evaluation_cannot_filter_missing_labels_or_extend_lookahead() {
        let (root, mut inputs, mut train, _) = fixture();
        inputs.validation = inputs.train.clone();
        inputs.validation.qualified_anchors = None;
        train.qualified_anchors_sha256 = None;
        let error = inputs
            .verify_view(root.path(), &inputs.validation, &train)
            .unwrap_err();
        assert!(format!("{error:#}").contains("missing"));
        let source = read_source_index(root.path(), &inputs.train)
            .unwrap()
            .sources
            .remove(0);
        let mut view = train.view;
        view.end_ms -= 1000;
        assert!(verify_source_view(root.path(), &source, view).is_err());
        view = train.view;
        view.history_start_ms += 1000;
        assert!(verify_source_view(root.path(), &source, view).is_err());
    }
    #[test]
    fn market_cohort_feature_sources_bind_warmup_tail_and_original_raw_segments() {
        let (root, inputs, _, _) = fixture();
        let source = read_source_index(root.path(), &inputs.train)
            .unwrap()
            .sources
            .remove(0);
        let report: serde_json::Value = serde_json::from_slice(
            &source
                .materialization
                .read(root.path(), 16 * 1024 * 1024)
                .unwrap(),
        )
        .unwrap();
        let features: MarketFeatureDatasetV1 =
            serde_json::from_slice(&source.features.read(root.path(), MANIFEST_BYTES).unwrap())
                .unwrap();
        let bytes = source
            .feature_sources
            .read(root.path(), 16 * 1024 * 1024)
            .unwrap();
        verify_feature_sources(&report, &bytes, &source.feature_sources.sha256, &features).unwrap();
        let metadata: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            metadata["first_dependency_received_at_ns"],
            (features.shards[0].first_observed_at_ms as u64 - 1000) * 1_000_000
        );
        assert_eq!(
            metadata["last_feature_observed_at_ms"],
            features.shards[0].last_observed_at_ms
        );
        let pit = decode_materialization(
            &source
                .materialization
                .read(root.path(), 16 * 1024 * 1024)
                .unwrap(),
        )
        .unwrap();
        assert!(
            features.shards[0].last_observed_at_ms
                > pit.snapshot.last_event_time.timestamp_millis()
        );
        for field in [
            "last_feature_observed_at_ms",
            "first_dependency_received_at_ns",
            "source_segments",
        ] {
            let mut altered = metadata.clone();
            match field {
                "last_feature_observed_at_ms" => {
                    altered[field] = (features.shards[0].last_observed_at_ms - 30_000).into()
                }
                "first_dependency_received_at_ns" => {
                    altered[field] =
                        (features.shards[0].first_observed_at_ms as u64 * 1_000_000).into()
                }
                _ => altered[field][0]["events"] = 999.into(),
            }
            let bytes = serde_json::to_vec(&altered).unwrap();
            let hash = hft_research_manifest::market_encoder::bytes_digest(&bytes);
            let mut features = features.clone();
            features.source_manifest_sha256 = hash.clone();
            let mut report = report.clone();
            report["market_encoder"]["feature_sources"] = serde_json::json!({"sha256":hash,"file":format!("{hash}.market-feature-sources.json")});
            report["market_encoder"]["features"]["sha256"] = features.digest().unwrap().into();
            assert!(
                verify_feature_sources(&report, &bytes, &hash, &features).is_err(),
                "{field}"
            );
        }
    }
    #[test]
    fn market_cohort_explicit_feature_start_admits_only_bounded_pit_warmup() {
        let (root, mut inputs, train, _) = fixture();
        let source = source_with_warmup(root.path(), train.view.history_start_ms, true, 60_000);
        let mut location = multi_source_location(root.path(), vec![source.clone()]);
        let anchors = derive_anchors(root.path(), &location, train.view).unwrap();
        location.qualified_anchors = Some(save(root.path(), &anchors, "market-anchors.json"));
        inputs.train = location;
        let data = MarketDataViewV1 {
            features_sha256: inputs.train.features.sha256.clone(),
            targets_sha256: inputs.train.targets.sha256.clone(),
            qualified_anchors_sha256: inputs
                .train
                .qualified_anchors
                .as_ref()
                .map(|a| a.sha256.clone()),
            view: train.view,
        };
        inputs
            .verify_view(root.path(), &inputs.train, &data)
            .unwrap();
        let report: serde_json::Value = serde_json::from_slice(
            &source
                .materialization
                .read(root.path(), 16 * 1024 * 1024)
                .unwrap(),
        )
        .unwrap();
        let features: MarketFeatureDatasetV1 =
            serde_json::from_slice(&source.features.read(root.path(), MANIFEST_BYTES).unwrap())
                .unwrap();
        let bytes = source
            .feature_sources
            .read(root.path(), 16 * 1024 * 1024)
            .unwrap();
        assert_eq!(
            report["output_start_received_at_ns"],
            (train.view.history_start_ms as u64 - 60000) * 1_000_000
        );
        assert_eq!(
            report["market_encoder"]["feature_start_received_at_ns"],
            train.view.history_start_ms as u64 * 1_000_000
        );
        let mut drift = report.clone();
        drift["market_encoder"]["feature_start_received_at_ns"] =
            (train.view.history_start_ms as u64 * 1_000_000 + 1_000_000_000).into();
        assert!(
            verify_feature_sources(&drift, &bytes, &source.feature_sources.sha256, &features)
                .is_err()
        );
        let too_early = source_with_warmup(root.path(), train.view.history_start_ms, true, 61_000);
        assert!(verify_original_source(
            root.path(),
            &too_early,
            &inputs.producer_source_revision,
            &inputs.producer_image
        )
        .is_err());
        let mut earlier_view = train.view;
        earlier_view.history_start_ms += 1000;
        assert!(verify_source_view(root.path(), &source, earlier_view).is_err());
    }
}
