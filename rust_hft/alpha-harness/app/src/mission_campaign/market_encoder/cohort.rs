//! Native market cohort assembly preserves original row bytes and recovery
//! series. Only manifest metadata is rebound to the ordered source index.
use super::inputs::{
    self, MarketCampaignInputs, MarketDatasetLocation, MarketPreparedConversion, MarketSource,
    MarketSourceIndex,
};
use crate::cli::PrepareSequenceCohortArgs;
use crate::mission_campaign::sequence::{
    cohort::{
        bounded_file_len, copy_verified, dataset_budget, fold_shard_bytes, fold_shard_rows, item,
        publish_cohort, put_metadata, PlannedCopy, MAX_REPLAY_MANIFEST_BYTES,
        MAX_REPLAY_PARQUET_BYTES,
    },
    inputs::Artifact,
};
use alpha_domain::market_encoder_study::MarketDataViewV1;
use anyhow::{bail, Context};
use hft_cex_research_input::market_encoder::{
    verify_prepared_feature_equivalence, verify_prepared_target_equivalence,
};
use hft_research_manifest::{
    market_encoder::{
        MarketFeatureDatasetV1, MarketTargetDatasetV1, FEATURE_PARQUET_SCHEMA, FEATURE_SCHEMA,
        TARGET_PARQUET_SCHEMA, TARGET_SCHEMA, TASK_HORIZON_MS,
    },
    prepared_market::{validate_prepared_producer, PreparedMarketReadyReceiptV2},
    sequence::{SequenceInputSpecV1, SequenceShardV1, SequenceViewV1},
};
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read, path::Path};

pub(crate) const COHORT_REQUEST_SCHEMA: &str = "monday.sol_market_encoder_cohort_request.v1";
pub(crate) const PREPARED_COHORT_REQUEST_SCHEMA: &str =
    "monday.sol_market_encoder_cohort_request.v2";
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedSelection {
    ready_receipt: Artifact,
    producer_source_revision: String,
    producer_image: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CohortRequest {
    schema_version: String,
    producer_source_revision: String,
    producer_image: String,
    pvc_name: String,
    pvc_uid: String,
    sub_path: String,
    fold_id: u8,
    training_view: SequenceViewV1,
    validation_view: SequenceViewV1,
    training_receipts: Vec<Artifact>,
    /// One contiguous original execution tape. Never stitch separate replays.
    validation_receipt: Artifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prepared_training: Option<PreparedSelection>,
}

impl CohortRequest {
    fn validate(&self) -> anyhow::Result<()> {
        inputs::validate_identity(
            &self.producer_source_revision,
            &self.producer_image,
            &self.pvc_name,
            &self.pvc_uid,
            &self.sub_path,
            self.fold_id,
        )?;
        self.training_view.validate().map_err(anyhow::Error::msg)?;
        self.validation_view
            .validate()
            .map_err(anyhow::Error::msg)?;
        if !matches!(
            self.schema_version.as_str(),
            COHORT_REQUEST_SCHEMA | PREPARED_COHORT_REQUEST_SCHEMA
        ) || (self.schema_version == PREPARED_COHORT_REQUEST_SCHEMA)
            != self.prepared_training.is_some()
            || self.training_receipts.is_empty()
            || self.training_receipts.len() > 512
            || self.training_view.end_ms - self.training_view.history_start_ms != 14 * 86_400_000
            || self.validation_view.history_start_ms - self.training_view.end_ms < TASK_HORIZON_MS
            || self.validation_view.decision_stride_ms != 1000
        {
            bail!("invalid market cohort request or chronological views");
        }
        if let Some(prepared) = &self.prepared_training {
            prepared.ready_receipt.validate()?;
            validate_prepared_producer(
                &prepared.producer_source_revision,
                &prepared.producer_image,
            )
            .map_err(anyhow::Error::msg)?;
        }
        let mut receipts = std::collections::BTreeSet::new();
        for receipt in self
            .training_receipts
            .iter()
            .chain([&self.validation_receipt])
        {
            receipt.validate()?;
            if !receipts.insert(&receipt.sha256) {
                bail!("duplicate market cohort source receipt");
            }
        }
        for view in [self.training_view, self.validation_view] {
            if view.decision_start_ms - view.history_start_ms < 59_000
                || view.end_ms - view.decision_start_ms <= TASK_HORIZON_MS
            {
                bail!("market cohort view lacks context or label maturity");
            }
        }
        Ok(())
    }

    fn verify_published(&self, inputs: &MarketCampaignInputs, root: &Path) -> anyhow::Result<()> {
        if inputs.producer_source_revision != self.producer_source_revision
            || inputs.schema_version
                != if self.prepared_training.is_some() {
                    inputs::PREPARED_INPUTS_SCHEMA
                } else {
                    inputs::INPUTS_SCHEMA
                }
            || inputs.producer_image != self.producer_image
            || inputs.pvc_name != self.pvc_name
            || inputs.pvc_uid != self.pvc_uid
            || inputs.sub_path != self.sub_path
            || inputs.fold_id != self.fold_id
        {
            bail!("published market cohort belongs to another request identity");
        }
        if let Some(selection) = &self.prepared_training {
            let index = inputs::read_source_index(root, &inputs.train)?;
            let reference = index
                .prepared
                .context("prepared Campaign lost its conversion receipt")?;
            let conversion: MarketPreparedConversion =
                serde_json::from_slice(&reference.read(root, 4 * 1024 * 1024)?)?;
            if conversion.ready_receipt.sha256 != selection.ready_receipt.sha256
                || conversion.producer_source_revision != selection.producer_source_revision
                || conversion.producer_image != selection.producer_image
            {
                bail!("published conversion differs from the admitted prepared request");
            }
        }
        for (location, receipts, view) in [
            (
                &inputs.train,
                self.training_receipts.as_slice(),
                self.training_view,
            ),
            (
                &inputs.validation,
                std::slice::from_ref(&self.validation_receipt),
                self.validation_view,
            ),
        ] {
            let index = inputs::read_source_index(root, location)?;
            if index
                .sources
                .iter()
                .map(|s| &s.receipt.sha256)
                .collect::<Vec<_>>()
                != receipts.iter().map(|r| &r.sha256).collect::<Vec<_>>()
            {
                bail!("published market cohort changed original receipt membership or ordering");
            }
            inputs.verify_view(
                root,
                location,
                &MarketDataViewV1 {
                    features_sha256: location.features.sha256.clone(),
                    targets_sha256: location.targets.sha256.clone(),
                    qualified_anchors_sha256: location
                        .qualified_anchors
                        .as_ref()
                        .map(|a| a.sha256.clone()),
                    view,
                },
            )?;
        }
        inputs.verify_mount(root)?;
        inputs.verify_replay(root, self.validation_view)
    }
}

pub(crate) fn prepare(args: PrepareSequenceCohortArgs) -> anyhow::Result<()> {
    let file = File::open(&args.request)?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        bail!("market cohort request exceeds byte bound");
    }
    let spec: CohortRequest = serde_json::from_slice(&bytes)?;
    spec.validate()?;
    let inputs = publish_cohort(
        &args,
        |inputs, root| spec.verify_published(inputs, root),
        |staged| {
            let mut training = Vec::new();
            for receipt in &spec.training_receipts {
                training.push(plan_source(
                    &spec,
                    &args.input_root,
                    receipt,
                    staged,
                    false,
                )?);
            }
            let validation = plan_source(
                &spec,
                &args.input_root,
                &spec.validation_receipt,
                staged,
                true,
            )?;
            // Both feature and target byte budgets are charged before any row or
            // replay body is copied. Metadata staging never publishes a view.
            validate_payload_budget(&training, &validation)?;
            for plan in training.iter().chain([&validation]) {
                for copy in &plan.copies {
                    copy_verified(&copy.from, &copy.to, &copy.hash, copy.max_bytes)?;
                }
            }
            let mut train = if let Some(prepared) = &spec.prepared_training {
                assemble_prepared_dataset(
                    staged,
                    &args.input_root,
                    &training,
                    prepared,
                    spec.training_view,
                )?
            } else {
                assemble_dataset(staged, &training)?
            };
            let anchors = inputs::derive_anchors(staged, &train, spec.training_view)?;
            train.qualified_anchors = Some(put_metadata(
                staged,
                &serde_json::to_vec(&anchors)?,
                "market-anchors.json",
            )?);
            let validation_location = assemble_dataset(staged, std::slice::from_ref(&validation))?;
            Ok(MarketCampaignInputs {
                schema_version: if spec.prepared_training.is_some() {
                    inputs::PREPARED_INPUTS_SCHEMA
                } else {
                    inputs::INPUTS_SCHEMA
                }
                .into(),
                producer_source_revision: spec.producer_source_revision.clone(),
                producer_image: spec.producer_image.clone(),
                pvc_name: spec.pvc_name.clone(),
                pvc_uid: spec.pvc_uid.clone(),
                sub_path: spec.sub_path.clone(),
                fold_id: spec.fold_id,
                train,
                validation: validation_location,
                replay_artifact: validation
                    .replay_artifact
                    .context("missing market validation replay")?,
                replay_manifest: validation
                    .replay_manifest
                    .context("missing market validation replay manifest")?,
            })
        },
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version":if spec.prepared_training.is_some() { "monday.sol_market_encoder_cohort_prepared.v2" } else { "monday.sol_market_encoder_cohort_prepared.v1" },
            "inputs_sha256":alpha_domain::canonical_json_hash(&inputs)?, "inputs_out":args.inputs_out,
            "train_features_sha256":inputs.train.features.sha256, "train_targets_sha256":inputs.train.targets.sha256,
            "qualified_anchors_sha256":inputs.train.qualified_anchors.as_ref().map(|a| &a.sha256),
            "validation_features_sha256":inputs.validation.features.sha256,
            "validation_targets_sha256":inputs.validation.targets.sha256,
            "replay_manifest_sha256":inputs.replay_manifest.sha256, "source_receipts":spec.training_receipts.len()+1,
            "training_performed":false,"sealed_holdout_opened":false,
        }))?
    );
    Ok(())
}

struct PlannedSource {
    source: MarketSource,
    features: MarketFeatureDatasetV1,
    targets: MarketTargetDatasetV1,
    copies: Vec<PlannedCopy>,
    replay_artifact: Option<Artifact>,
    replay_manifest: Option<Artifact>,
    replay_bytes: u64,
}

fn validate_payload_budget(
    training: &[PlannedSource],
    validation: &PlannedSource,
) -> anyhow::Result<()> {
    let combined = |plans: &[PlannedSource]| -> anyhow::Result<u64> {
        let feature_bytes = fold_shard_bytes(plans.iter().flat_map(|p| &p.features.shards))?;
        let target_bytes = fold_shard_bytes(plans.iter().flat_map(|p| &p.targets.shards))?;
        dataset_budget(
            feature_bytes,
            fold_shard_rows(plans.iter().flat_map(|p| &p.features.shards))?,
        )?;
        dataset_budget(
            target_bytes,
            fold_shard_rows(plans.iter().flat_map(|p| &p.targets.shards))?,
        )?;
        feature_bytes
            .checked_add(target_bytes)
            .context("market payload overflow")
    };
    let total = combined(training)?
        .checked_add(combined(std::slice::from_ref(validation))?)
        .and_then(|sum| sum.checked_add(validation.replay_bytes))
        .context("market payload overflow")?;
    if validation.replay_bytes > MAX_REPLAY_PARQUET_BYTES || total > 48 * 1024 * 1024 * 1024 {
        bail!("market cohort exceeds combined feature, target and replay byte budget");
    }
    Ok(())
}

fn plan_source(
    spec: &CohortRequest,
    root: &Path,
    receipt: &Artifact,
    output: &Path,
    include_replay: bool,
) -> anyhow::Result<PlannedSource> {
    let receipt_path = receipt.path(root)?;
    let bytes = receipt.read(root, 1024 * 1024)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let receipts_dir = receipt_path.parent().context("market receipt parent")?;
    if receipts_dir.file_name().and_then(|s| s.to_str()) != Some("receipts") {
        bail!("market source receipt is not in its native prepared run");
    }
    let run_root = receipts_dir.parent().context("market source run root")?;
    let materialization = item(&value, "materialization")?;
    let report_bytes = materialization.read(run_root, 16 * 1024 * 1024)?;
    let report: serde_json::Value = serde_json::from_slice(&report_bytes)?;
    let parent = Path::new(&materialization.file)
        .parent()
        .context("market report parent")?;
    let original = |name: &str| -> anyhow::Result<Artifact> {
        let mut reference: Artifact =
            serde_json::from_value(report["market_encoder"][name].clone())?;
        reference.validate()?;
        reference.file = parent
            .join(&reference.file)
            .to_str()
            .context("non UTF-8 market manifest")?
            .into();
        Ok(reference)
    };
    let feature_sources = original("feature_sources")?;
    let feature_source_bytes = feature_sources.read(run_root, 16 * 1024 * 1024)?;
    let features = original("features")?;
    let targets = original("targets")?;
    let feature_bytes = features.read(run_root, 4 * 1024 * 1024)?;
    let target_bytes = targets.read(run_root, 4 * 1024 * 1024)?;
    let source = MarketSource {
        receipt: put_metadata(output, &bytes, "receipt.json")?,
        materialization: put_metadata(output, &report_bytes, "materialization.json")?,
        feature_sources: put_metadata(
            output,
            &feature_source_bytes,
            "market-feature-sources.json",
        )?,
        features: put_metadata(output, &feature_bytes, "market-features.json")?,
        targets: put_metadata(output, &target_bytes, "market-targets.json")?,
    };
    let (feature_dataset, target_dataset) = inputs::verify_original_source(
        output,
        &source,
        &spec.producer_source_revision,
        &spec.producer_image,
    )?;
    let mut copies = Vec::new();
    for (reference, shards) in [
        (&features, &feature_dataset.shards),
        (&targets, &target_dataset.shards),
    ] {
        let source_parent = Path::new(&reference.file)
            .parent()
            .context("market shard parent")?;
        for shard in shards {
            let reference = Artifact {
                file: source_parent
                    .join(&shard.file)
                    .to_str()
                    .context("non UTF-8 market shard")?
                    .into(),
                sha256: shard.sha256.clone(),
            };
            let from = reference.path(run_root)?;
            if bounded_file_len(&from, shard.bytes)? != shard.bytes {
                bail!("market source shard size changed");
            }
            copies.push(PlannedCopy {
                from,
                to: output.join(&shard.file),
                hash: shard.sha256.clone(),
                max_bytes: shard.bytes,
            });
        }
    }
    let plan_replay =
        |field: &str, suffix: &str, max: u64| -> anyhow::Result<(Artifact, PlannedCopy)> {
            let reference = item(&value, field)?;
            let from = reference.path(run_root)?;
            let length = bounded_file_len(&from, max)?;
            let target = Artifact {
                file: format!("{}.{}", reference.sha256, suffix),
                sha256: reference.sha256,
            };
            Ok((
                target.clone(),
                PlannedCopy {
                    from,
                    to: output.join(&target.file),
                    hash: target.sha256,
                    max_bytes: length,
                },
            ))
        };
    let (replay_artifact, replay_manifest, replay_bytes) = if include_replay {
        let (artifact, artifact_copy) =
            plan_replay("replay_artifact", "parquet", MAX_REPLAY_PARQUET_BYTES)?;
        let (manifest, manifest_copy) =
            plan_replay("replay_manifest", "replay.json", MAX_REPLAY_MANIFEST_BYTES)?;
        let length = artifact_copy.max_bytes;
        copies.extend([artifact_copy, manifest_copy]);
        (Some(artifact), Some(manifest), length)
    } else {
        (None, None, 0)
    };
    Ok(PlannedSource {
        source,
        features: feature_dataset,
        targets: target_dataset,
        copies,
        replay_artifact,
        replay_manifest,
        replay_bytes,
    })
}

fn assemble_dataset(root: &Path, plans: &[PlannedSource]) -> anyhow::Result<MarketDatasetLocation> {
    assemble_shards(
        root,
        plans.iter().map(|p| p.source.clone()).collect(),
        plans
            .iter()
            .flat_map(|p| p.features.shards.clone())
            .collect(),
        plans
            .iter()
            .flat_map(|p| p.targets.shards.clone())
            .collect(),
    )
}

fn assemble_prepared_dataset(
    output: &Path,
    input_root: &Path,
    plans: &[PlannedSource],
    selection: &PreparedSelection,
    view: SequenceViewV1,
) -> anyhow::Result<MarketDatasetLocation> {
    let ready_bytes = selection.ready_receipt.read(input_root, 16 * 1024 * 1024)?;
    let ready: PreparedMarketReadyReceiptV2 = serde_json::from_slice(&ready_bytes)?;
    ready.validate().map_err(anyhow::Error::msg)?;
    if ready.producer_source_revision != selection.producer_source_revision
        || ready.producer_image != selection.producer_image
        || ready.request.view != view
        || ready.request.anchor_end_ms != view.end_ms - TASK_HORIZON_MS
        || ready.request.purpose != "pre_holdout_supervised"
        || ready.request.qualified_anchors
        || ready.request.input != SequenceInputSpecV1::sol_lob()
        || ready.request.sources.len() != plans.len()
    {
        bail!("prepared service view differs from the admitted native training request; anchors are independently derived by Campaign");
    }
    let ready_path = selection.ready_receipt.path(input_root)?;
    let ready_root = ready_path.parent().context("prepared ready root")?;
    let ready_feature_ref = Artifact {
        file: ready.feature_manifest.file.clone(),
        sha256: ready.feature_manifest.sha256.clone(),
    };
    let ready_target_ref = ready
        .target_manifest
        .as_ref()
        .context("prepared training has no targets")?;
    let ready_target_ref = Artifact {
        file: ready_target_ref.file.clone(),
        sha256: ready_target_ref.sha256.clone(),
    };
    let feature_bytes = ready_feature_ref.read(ready_root, 4 * 1024 * 1024)?;
    let target_bytes = ready_target_ref.read(ready_root, 4 * 1024 * 1024)?;
    let ready_features: MarketFeatureDatasetV1 = serde_json::from_slice(&feature_bytes)?;
    let ready_targets: MarketTargetDatasetV1 = serde_json::from_slice(&target_bytes)?;
    ready
        .prepared_view
        .validate_datasets(&ready_features, Some(&ready_targets))
        .map_err(anyhow::Error::msg)?;
    for (prepared, plan) in ready.prepared_view.sources.iter().zip(plans) {
        if prepared.feature_dataset_sha256 != plan.source.features.sha256
            || prepared.target_dataset_sha256.as_ref() != Some(&plan.source.targets.sha256)
            || prepared.source_manifest_sha256 != plan.features.source_manifest_sha256
        {
            bail!("prepared service source union changed native receipt membership or order");
        }
    }
    let prepared_bytes =
        fold_shard_bytes(ready_features.shards.iter().chain(&ready_targets.shards))?;
    if prepared_bytes > inputs::PREPARED_CACHE_BYTES {
        bail!("prepared numerical cache exceeds the 512 MiB SOL cohort budget");
    }
    let originals = plans
        .iter()
        .map(|p| (output.to_path_buf(), p.features.clone()))
        .collect::<Vec<_>>();
    let original_targets = plans
        .iter()
        .map(|p| (output.to_path_buf(), p.targets.clone()))
        .collect::<Vec<_>>();
    let feature_root = ready_feature_ref
        .path(ready_root)?
        .parent()
        .context("prepared feature parent")?
        .to_owned();
    let target_root = ready_target_ref
        .path(ready_root)?
        .parent()
        .context("prepared target parent")?
        .to_owned();
    let feature_proof =
        verify_prepared_feature_equivalence(&originals, &feature_root, &ready_features)
            .map_err(anyhow::Error::msg)?;
    let target_proof =
        verify_prepared_target_equivalence(&original_targets, &target_root, &ready_targets)
            .map_err(anyhow::Error::msg)?;
    let conversion = MarketPreparedConversion {
        schema_version: "monday.market_prepared_conversion.v1".into(),
        producer_source_revision: selection.producer_source_revision.clone(),
        producer_image: selection.producer_image.clone(),
        ready_receipt: put_metadata(output, &ready_bytes, "prepared-ready.json")?,
        ready_features: put_metadata(output, &feature_bytes, "prepared-features.json")?,
        ready_targets: put_metadata(output, &target_bytes, "prepared-targets.json")?,
        feature_decoded_sha256: feature_proof.decoded_sha256,
        target_decoded_sha256: target_proof.decoded_sha256,
        feature_rows: feature_proof.rows,
        target_rows: target_proof.rows,
    };
    let conversion_ref = put_metadata(
        output,
        &serde_json::to_vec(&conversion)?,
        "prepared-conversion.json",
    )?;
    let index = MarketSourceIndex {
        schema_version: inputs::PREPARED_SOURCES_SCHEMA.into(),
        sources: plans.iter().map(|p| p.source.clone()).collect(),
        prepared: Some(conversion_ref),
    };
    let sources = put_metadata(output, &serde_json::to_vec(&index)?, "market-sources.json")?;
    let copy_shards = |original_root: &Path,
                       original: &[SequenceShardV1]|
     -> anyhow::Result<Vec<SequenceShardV1>> {
        let mut result = Vec::with_capacity(original.len());
        for shard in original {
            let from = Artifact {
                file: shard.file.clone(),
                sha256: shard.sha256.clone(),
            }
            .path(original_root)?;
            let mut renamed = shard.clone();
            renamed.file = format!("{}.parquet", shard.sha256);
            copy_verified(
                &from,
                &output.join(&renamed.file),
                &shard.sha256,
                shard.bytes,
            )?;
            result.push(renamed);
        }
        Ok(result)
    };
    let features = MarketFeatureDatasetV1 {
        schema_version: FEATURE_PARQUET_SCHEMA.into(),
        venue: ready_features.venue,
        symbol: ready_features.symbol,
        input: ready_features.input,
        source_manifest_sha256: sources.sha256.clone(),
        shards: copy_shards(&feature_root, &ready_features.shards)?,
    };
    features.validate().map_err(anyhow::Error::msg)?;
    let features = put_metadata(
        output,
        &serde_json::to_vec(&features)?,
        "market-features.json",
    )?;
    let targets = MarketTargetDatasetV1 {
        schema_version: TARGET_PARQUET_SCHEMA.into(),
        feature_dataset_sha256: features.sha256.clone(),
        horizon_ms: TASK_HORIZON_MS,
        shards: copy_shards(&target_root, &ready_targets.shards)?,
    };
    targets.validate().map_err(anyhow::Error::msg)?;
    let targets = put_metadata(
        output,
        &serde_json::to_vec(&targets)?,
        "market-targets.json",
    )?;
    Ok(MarketDatasetLocation {
        features,
        targets,
        sources,
        qualified_anchors: None,
    })
}

fn assemble_shards(
    root: &Path,
    originals: Vec<MarketSource>,
    feature_shards: Vec<SequenceShardV1>,
    target_shards: Vec<SequenceShardV1>,
) -> anyhow::Result<MarketDatasetLocation> {
    let index = MarketSourceIndex {
        schema_version: inputs::SOURCES_SCHEMA.into(),
        sources: originals,
        prepared: None,
    };
    let sources = put_metadata(root, &serde_json::to_vec(&index)?, "market-sources.json")?;
    let features = MarketFeatureDatasetV1 {
        schema_version: FEATURE_SCHEMA.into(),
        venue: "binance-usdm".into(),
        symbol: "SOLUSDT".into(),
        source_manifest_sha256: sources.sha256.clone(),
        input: SequenceInputSpecV1::sol_lob(),
        shards: feature_shards,
    };
    features.validate().map_err(anyhow::Error::msg)?;
    let features = put_metadata(
        root,
        &serde_json::to_vec(&features)?,
        "market-features.json",
    )?;
    let targets = MarketTargetDatasetV1 {
        schema_version: TARGET_SCHEMA.into(),
        feature_dataset_sha256: features.sha256.clone(),
        horizon_ms: TASK_HORIZON_MS,
        shards: target_shards,
    };
    targets.validate().map_err(anyhow::Error::msg)?;
    let targets = put_metadata(root, &serde_json::to_vec(&targets)?, "market-targets.json")?;
    Ok(MarketDatasetLocation {
        features,
        targets,
        sources,
        qualified_anchors: None,
    })
}

#[cfg(all(test, feature = "scientific"))]
mod tests {
    use super::*;

    #[test]
    fn prepared_campaign_cohort_preserves_native_proofs_anchors_and_converter_identity() {
        use hft_prepared_market_io::{write_feature_parquet_shard, write_target_parquet_shard};
        use hft_research_manifest::{
            market_encoder::{MarketFeatureFrameV1, MarketTargetFrameV1},
            prepared_market::{
                PreparedMarketArtifactV1, PreparedMarketDataRequestV1,
                PreparedMarketRequestSourceV1, PreparedMarketSeriesV1, PreparedMarketSourceV1,
                PreparedMarketViewV1, PREPARED_MARKET_VIEW_SCHEMA,
            },
        };
        let (root, mut campaign, native_train, _) = inputs::tests::fixture();
        let source = inputs::read_source_index(root.path(), &campaign.train)
            .unwrap()
            .sources
            .remove(0);
        let features: MarketFeatureDatasetV1 =
            serde_json::from_slice(&source.features.read(root.path(), 4 * 1024 * 1024).unwrap())
                .unwrap();
        let targets: MarketTargetDatasetV1 =
            serde_json::from_slice(&source.targets.read(root.path(), 4 * 1024 * 1024).unwrap())
                .unwrap();
        let read_rows = |file: &str| std::fs::read_to_string(root.path().join(file)).unwrap();
        let feature_rows = features
            .shards
            .iter()
            .flat_map(|s| {
                read_rows(&s.file)
                    .lines()
                    .map(|l| serde_json::from_str::<MarketFeatureFrameV1>(l).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let target_rows = targets
            .shards
            .iter()
            .flat_map(|s| {
                read_rows(&s.file)
                    .lines()
                    .map(|l| serde_json::from_str::<MarketTargetFrameV1>(l).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let ready_root = root.path().join("ready");
        std::fs::create_dir(&ready_root).unwrap();
        let mut prepared_features = features.clone();
        prepared_features.schema_version = FEATURE_PARQUET_SCHEMA.into();
        prepared_features.shards = vec![write_feature_parquet_shard(
            &ready_root,
            "features.parquet",
            &feature_rows,
            &features.input,
        )
        .unwrap()];
        let mut prepared_targets = targets.clone();
        prepared_targets.schema_version = TARGET_PARQUET_SCHEMA.into();
        prepared_targets.feature_dataset_sha256 = prepared_features.digest().unwrap();
        prepared_targets.shards =
            vec![write_target_parquet_shard(&ready_root, "targets.parquet", &target_rows).unwrap()];
        let feature_ref = put_metadata(
            &ready_root,
            &serde_json::to_vec(&prepared_features).unwrap(),
            "features.json",
        )
        .unwrap();
        let target_ref = put_metadata(
            &ready_root,
            &serde_json::to_vec(&prepared_targets).unwrap(),
            "targets.json",
        )
        .unwrap();
        let request = PreparedMarketDataRequestV1 {
            schema_version: "monday.market_data_request.v1".into(),
            sources: vec![PreparedMarketRequestSourceV1 {
                feature_dataset_sha256: features.digest().unwrap(),
                target_dataset_sha256: Some(targets.digest().unwrap()),
            }],
            transform_sha256: "e".repeat(64),
            input: features.input.clone(),
            view: native_train.view,
            anchor_end_ms: native_train.view.end_ms - TASK_HORIZON_MS,
            purpose: "pre_holdout_supervised".into(),
            qualified_anchors: false,
        };
        let prepared_view = PreparedMarketViewV1 {
            schema_version: PREPARED_MARKET_VIEW_SCHEMA.into(),
            sources: vec![PreparedMarketSourceV1 {
                feature_dataset_sha256: features.digest().unwrap(),
                target_dataset_sha256: Some(targets.digest().unwrap()),
                source_manifest_sha256: features.source_manifest_sha256.clone(),
                transform_sha256: request.transform_sha256.clone(),
            }],
            source_feature_dataset_sha256: features.digest().unwrap(),
            source_target_dataset_sha256: Some(targets.digest().unwrap()),
            source_manifest_sha256: features.source_manifest_sha256.clone(),
            transform_sha256: request.transform_sha256.clone(),
            data_watermark_ms: native_train.view.end_ms,
            view: native_train.view,
            feature_dataset_sha256: prepared_features.digest().unwrap(),
            target_dataset_sha256: Some(prepared_targets.digest().unwrap()),
            qualified_anchors_sha256: None,
            series: vec![PreparedMarketSeriesV1 {
                series_id: feature_rows[0].series_id,
                first_observed_at_ms: feature_rows[0].observed_at_ms,
                last_observed_at_ms: feature_rows.last().unwrap().observed_at_ms,
                rows: feature_rows.len() as u64,
            }],
            gaps: vec![],
        };
        let ready = PreparedMarketReadyReceiptV2 {
            schema_version: "monday.market_ready_receipt.v2".into(),
            producer_source_revision: "c".repeat(40),
            producer_image: format!("registry/data@sha256:{}", "d".repeat(64)),
            request_sha256: request.digest().unwrap(),
            request,
            prepared_view_sha256: prepared_view.digest().unwrap(),
            prepared_view,
            feature_manifest: PreparedMarketArtifactV1 {
                file: feature_ref.file,
                sha256: feature_ref.sha256,
            },
            target_manifest: Some(PreparedMarketArtifactV1 {
                file: target_ref.file,
                sha256: target_ref.sha256,
            }),
            qualified_anchors: None,
        };
        let ready_ref = put_metadata(
            &ready_root,
            &serde_json::to_vec(&ready).unwrap(),
            "ready.json",
        )
        .unwrap();
        let mut selection = PreparedSelection {
            ready_receipt: Artifact {
                file: format!("ready/{}", ready_ref.file),
                sha256: ready_ref.sha256,
            },
            producer_source_revision: ready.producer_source_revision.clone(),
            producer_image: ready.producer_image.clone(),
        };
        let plans = vec![PlannedSource {
            source,
            features,
            targets,
            copies: vec![],
            replay_artifact: None,
            replay_manifest: None,
            replay_bytes: 0,
        }];
        let mut location = assemble_prepared_dataset(
            root.path(),
            root.path(),
            &plans,
            &selection,
            native_train.view,
        )
        .unwrap();
        let anchors = inputs::derive_anchors(root.path(), &location, native_train.view).unwrap();
        let original_anchors: hft_research_manifest::market_encoder::MarketTrainingAnchorSetV1 =
            serde_json::from_slice(
                &campaign
                    .train
                    .qualified_anchors
                    .as_ref()
                    .unwrap()
                    .read(root.path(), 4 * 1024 * 1024)
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(anchors.anchors, original_anchors.anchors);
        location.qualified_anchors = Some(
            put_metadata(
                root.path(),
                &serde_json::to_vec(&anchors).unwrap(),
                "market-anchors.json",
            )
            .unwrap(),
        );
        campaign.schema_version = inputs::PREPARED_INPUTS_SCHEMA.into();
        campaign.train = location;
        let view = MarketDataViewV1 {
            features_sha256: campaign.train.features.sha256.clone(),
            targets_sha256: campaign.train.targets.sha256.clone(),
            qualified_anchors_sha256: campaign
                .train
                .qualified_anchors
                .as_ref()
                .map(|a| a.sha256.clone()),
            view: native_train.view,
        };
        let (actual, _) = campaign
            .verify_view(root.path(), &campaign.train, &view)
            .unwrap();
        assert_eq!(actual.schema_version, FEATURE_PARQUET_SCHEMA);
        campaign.schema_version = inputs::INPUTS_SCHEMA.into();
        assert!(campaign
            .verify_dataset(root.path(), &campaign.train)
            .is_err());
        selection.producer_image = format!("registry/data@sha256:{}", "f".repeat(64));
        assert!(assemble_prepared_dataset(
            root.path(),
            root.path(),
            &plans,
            &selection,
            native_train.view
        )
        .is_err());
    }

    fn shard(first: i64, tag: &str) -> SequenceShardV1 {
        SequenceShardV1 {
            file: format!("{}.jsonl", tag.repeat(64)),
            sha256: tag.repeat(64),
            bytes: 20,
            rows: 2,
            first_observed_at_ms: first,
            last_observed_at_ms: first + 1000,
        }
    }

    #[test]
    fn market_cohort_assembly_preserves_original_order_gaps_and_target_binding() {
        let root = tempfile::tempdir().unwrap();
        let feature_shards = vec![shard(0, "a"), shard(10000, "b")];
        let target_shards = vec![shard(0, "c"), shard(10000, "d")];
        let first = assemble_shards(
            root.path(),
            vec![],
            feature_shards.clone(),
            target_shards.clone(),
        )
        .unwrap();
        let repeated = assemble_shards(
            root.path(),
            vec![],
            feature_shards.clone(),
            target_shards.clone(),
        )
        .unwrap();
        assert_eq!(first, repeated);
        let features: MarketFeatureDatasetV1 =
            serde_json::from_slice(&first.features.read(root.path(), 4096).unwrap()).unwrap();
        let targets: MarketTargetDatasetV1 =
            serde_json::from_slice(&first.targets.read(root.path(), 4096).unwrap()).unwrap();
        assert_eq!(features.shards, feature_shards);
        assert_eq!(targets.shards, target_shards);
        assert_eq!(features.source_manifest_sha256, first.sources.sha256);
        assert_eq!(targets.feature_dataset_sha256, first.features.sha256);
        assert!(assemble_shards(
            root.path(),
            vec![],
            vec![feature_shards[1].clone(), feature_shards[0].clone()],
            target_shards.clone()
        )
        .is_err());
        assert!(assemble_shards(
            root.path(),
            vec![],
            feature_shards.clone(),
            vec![target_shards[1].clone(), target_shards[0].clone()]
        )
        .is_err());
        assert!(assemble_shards(
            root.path(),
            vec![],
            vec![feature_shards[0].clone(), feature_shards[0].clone()],
            target_shards
        )
        .is_err());
    }

    #[test]
    fn market_cohort_recovery_rejects_request_or_source_drift() {
        let (root, inputs, train, validation) = inputs::tests::fixture();
        let mut request = CohortRequest {
            schema_version: COHORT_REQUEST_SCHEMA.into(),
            producer_source_revision: inputs.producer_source_revision.clone(),
            producer_image: inputs.producer_image.clone(),
            pvc_name: inputs.pvc_name.clone(),
            pvc_uid: inputs.pvc_uid.clone(),
            sub_path: inputs.sub_path.clone(),
            fold_id: inputs.fold_id,
            training_view: train.view,
            validation_view: validation.view,
            training_receipts: inputs::read_source_index(root.path(), &inputs.train)
                .unwrap()
                .sources
                .into_iter()
                .map(|s| s.receipt)
                .collect(),
            validation_receipt: inputs::read_source_index(root.path(), &inputs.validation)
                .unwrap()
                .sources
                .remove(0)
                .receipt,
            prepared_training: None,
        };
        request.pvc_uid.push_str("-other");
        assert!(format!(
            "{:#}",
            request.verify_published(&inputs, root.path()).unwrap_err()
        )
        .contains("request identity"));
        request.pvc_uid = inputs.pvc_uid.clone();
        request.training_receipts[0].sha256 = "f".repeat(64);
        assert!(format!(
            "{:#}",
            request.verify_published(&inputs, root.path()).unwrap_err()
        )
        .contains("receipt membership"));
        request.training_receipts = inputs::read_source_index(root.path(), &inputs.train)
            .unwrap()
            .sources
            .into_iter()
            .map(|s| s.receipt)
            .collect();
        request.training_view.history_start_ms += 1000;
        assert!(request.verify_published(&inputs, root.path()).is_err());
    }

    #[test]
    fn market_cohort_combined_budget_counts_feature_and_target_payloads() {
        let (root, inputs, _, _) = inputs::tests::fixture();
        let source = inputs::read_source_index(root.path(), &inputs.train)
            .unwrap()
            .sources
            .remove(0);
        let (features, targets) = inputs.verify_dataset(root.path(), &inputs.train).unwrap();
        let mut plan = PlannedSource {
            source,
            features,
            targets,
            copies: vec![],
            replay_artifact: None,
            replay_manifest: None,
            replay_bytes: 0,
        };
        assert!(validate_payload_budget(&[], &plan).is_ok());
        plan.targets.shards[0].bytes = 8 * 1024 * 1024 * 1024 + 1;
        assert!(validate_payload_budget(&[], &plan).is_err());
        plan.targets.shards[0].bytes = 20;
        plan.replay_bytes = MAX_REPLAY_PARQUET_BYTES + 1;
        assert!(validate_payload_budget(&[], &plan).is_err());
    }
}
