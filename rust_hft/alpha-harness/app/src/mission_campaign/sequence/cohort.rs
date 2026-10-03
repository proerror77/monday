//! Bounded native assembly. Originals remain immutable; cohorts concatenate CAS
//! shards without renumbering recoveries, filling gaps or rewriting labels.
use super::*;
use crate::cli::PrepareSequenceCohortArgs;
use hft_research_manifest::sequence::{SequenceDatasetV1, SequenceInputSpecV1};
use inputs::{Artifact, DatasetLocation, SequenceSource, SequenceSourceIndex, INPUTS_SCHEMA};
use std::{fs, io::Write};

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
    training_window_days: u8,
    training_receipts: Vec<Artifact>,
    /// Validation is one contiguous materialized replay. No concatenation of
    /// independently reset execution tapes is admitted here.
    validation_receipt: Artifact,
}

pub(crate) fn prepare(args: PrepareSequenceCohortArgs) -> anyhow::Result<()> {
    let spec: CohortRequest = read_json(&args.request)?;
    if spec.schema_version != "monday.sol_sequence_cohort_request.v1"
        || spec.training_receipts.is_empty()
        || spec.training_receipts.len() > 512
    {
        bail!("invalid or oversized sequence cohort request");
    }
    // Identity is cheap. Do not stage or copy payloads for a request that
    // cannot be admitted.
    validate_cohort_identity(&spec)?;
    let training_receipts = spec.training_receipts.len();
    let inputs = publish_cohort(
        &args,
        |inputs: &SequenceCampaignInputs, root| inputs.verify_mount(root),
        |staged| {
            let mut train_plans = Vec::new();
            for receipt in &spec.training_receipts {
                train_plans.push(plan_source(
                    &spec,
                    &args.input_root,
                    receipt,
                    staged,
                    false,
                )?);
            }
            let validation_plan = plan_source(
                &spec,
                &args.input_root,
                &spec.validation_receipt,
                staged,
                true,
            )?;
            let train_bytes =
                fold_shard_bytes(train_plans.iter().flat_map(|plan| &plan.dataset.shards))?;
            let train_rows =
                fold_shard_rows(train_plans.iter().flat_map(|plan| &plan.dataset.shards))?;
            let validation_bytes = fold_shard_bytes(validation_plan.dataset.shards.iter())?;
            let validation_rows = fold_shard_rows(validation_plan.dataset.shards.iter())?;
            // Reject the combined payload before any shard or replay body is copied.
            payload_budget(
                train_bytes,
                train_rows,
                validation_bytes,
                validation_rows,
                validation_plan.replay_bytes,
            )?;
            for plan in train_plans.iter().chain(std::iter::once(&validation_plan)) {
                for copy in &plan.copies {
                    copy_verified(&copy.from, &copy.to, &copy.hash, copy.max_bytes)?;
                }
            }
            let train = assemble_dataset(
                staged,
                train_plans.iter().map(|plan| plan.source.clone()).collect(),
                train_plans
                    .into_iter()
                    .flat_map(|plan| plan.dataset.shards)
                    .collect(),
            )?;
            let validation = assemble_dataset(
                staged,
                vec![validation_plan.source],
                validation_plan.dataset.shards,
            )?;
            let inputs = SequenceCampaignInputs {
                schema_version: INPUTS_SCHEMA.into(),
                producer_source_revision: spec.producer_source_revision,
                producer_image: spec.producer_image,
                pvc_name: spec.pvc_name,
                pvc_uid: spec.pvc_uid,
                sub_path: spec.sub_path,
                fold_id: spec.fold_id,
                training_window_days: spec.training_window_days,
                train,
                validation,
                replay_artifact: validation_plan
                    .replay_artifact
                    .context("missing validation replay")?,
                replay_manifest: validation_plan
                    .replay_manifest
                    .context("missing validation replay manifest")?,
            };
            Ok(inputs)
        },
    )?;
    print_prepared(&inputs, &args.inputs_out, training_receipts)
}

/// One create-once publication controller for native research cohorts. A
/// durable pending receipt makes the final rename recoverable without recopying.
pub(crate) fn publish_cohort<T>(
    args: &PrepareSequenceCohortArgs,
    verify: impl Fn(&T, &Path) -> anyhow::Result<()>,
    build: impl FnOnce(&Path) -> anyhow::Result<T>,
) -> anyhow::Result<T>
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq,
{
    let parent = args.output_root.parent().context("cohort output parent")?;
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let name = args.output_root.file_name().context("cohort output name")?;
    let output = parent.join(name);
    data_mission::ensure_output_path_is_not_symlink(&output, "cohort output")?;
    let mut lock_name = name.to_owned();
    lock_name.push(".cohort.lock");
    let lock_path = parent.join(lock_name);
    data_mission::ensure_output_path_is_not_symlink(&lock_path, "cohort publication lock")?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    lock.try_lock()
        .context("this cohort has an active publication controller")?;
    let inputs_parent = args.inputs_out.parent().context("cohort receipt parent")?;
    fs::create_dir_all(inputs_parent)?;
    let inputs_out = inputs_parent
        .canonicalize()?
        .join(args.inputs_out.file_name().context("cohort receipt name")?);
    if inputs_out.starts_with(&output) {
        bail!("cohort receipt must be outside the worker view");
    }
    let pending = pending_receipt(&inputs_out)?;
    data_mission::ensure_output_path_is_not_symlink(&inputs_out, "cohort receipt")?;
    data_mission::ensure_output_path_is_not_symlink(&pending, "cohort pending receipt")?;
    if output.is_dir() && pending.is_file() && !inputs_out.exists() {
        return finish_published_receipt(&output, &inputs_out, &pending, &verify);
    }
    if output.is_dir() && inputs_out.is_file() {
        let inputs: T = read_json(&inputs_out)?;
        verify(&inputs, &output)?;
        if pending.exists() {
            let previous: T = read_json(&pending)?;
            if previous != inputs {
                bail!("cohort pending receipt conflicts with the published receipt");
            }
            fs::remove_file(&pending)?;
        }
        return Ok(inputs);
    }
    if output.exists() || inputs_out.exists() {
        bail!(
            "sequence cohort publication is incomplete; a published view needs its pending receipt before the controller receipt can be finished"
        );
    }
    let temporary = tempfile::tempdir_in(&parent)?;
    let staged = temporary.path().join("view");
    fs::create_dir(&staged)?;
    let inputs = build(&staged)?;
    verify(&inputs, &staged)?;
    retain_receipt(&pending, &inputs)?;
    fs::rename(&staged, &output)?;
    File::open(&parent)?.sync_all()?;
    finish_published_receipt(&output, &inputs_out, &pending, &verify)
}

/// Create-once durable receipts: an interrupted earlier publication may be
/// resumed only if it names exactly the reconstructed immutable cohort.
fn retain_receipt<T>(path: &Path, value: &T) -> anyhow::Result<()>
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq,
{
    data_mission::ensure_output_path_is_not_symlink(path, "cohort immutable receipt")?;
    if path.exists() {
        let existing: T = read_json(path)?;
        if existing != *value {
            bail!("cohort immutable receipt conflicts with this request");
        }
        return Ok(());
    }
    let parent = path.parent().context("cohort receipt parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn pending_receipt(inputs_out: &Path) -> anyhow::Result<PathBuf> {
    let name = inputs_out
        .file_name()
        .context("cohort receipt name")?
        .to_owned();
    let mut pending = name;
    pending.push(".pending");
    Ok(inputs_out.with_file_name(pending))
}

fn validate_cohort_identity(spec: &CohortRequest) -> anyhow::Result<()> {
    if !matches!(spec.fold_id, 1 | 2)
        || !matches!(spec.training_window_days, 7 | 14)
        || !crate::mission_runner::valid_git_revision(&spec.producer_source_revision)
        || !inputs::safe_relative_path(&spec.sub_path)
        || !spec.sub_path.starts_with("sol-sequence/")
        || spec.pvc_uid.is_empty()
        || spec.pvc_uid.len() > 128
        || !spec
            .pvc_uid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || !spec.producer_image.contains("@sha256:")
        || !hft_research_manifest::sequence::valid_sha256(
            spec.producer_image
                .rsplit_once("@sha256:")
                .context("sequence producer image is not pinned")?
                .1,
        )
    {
        bail!("invalid SOL sequence cohort identity");
    }
    crate::prediction_dispatch::validate_dns_label("sequence input PVC", &spec.pvc_name)
}

fn finish_published_receipt<T>(
    output: &Path,
    inputs_out: &Path,
    pending: &Path,
    verify: &impl Fn(&T, &Path) -> anyhow::Result<()>,
) -> anyhow::Result<T>
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq,
{
    let inputs: T = read_json(pending)?;
    verify(&inputs, output)?;
    if inputs_out.is_file() {
        let existing: T = read_json(inputs_out)?;
        if existing != inputs {
            bail!("sequence cohort receipt differs from the published view");
        }
    } else {
        retain_receipt(inputs_out, &inputs)?;
    }
    if pending.exists() {
        fs::remove_file(pending)?;
    }
    Ok(inputs)
}

fn print_prepared(
    inputs: &SequenceCampaignInputs,
    inputs_out: &Path,
    training_receipts: usize,
) -> anyhow::Result<()> {
    print_json(
        &serde_json::json!({"schema_version":"monday.sol_sequence_cohort_prepared.v1",
        "inputs_sha256":canonical_json_hash(inputs)?,"inputs_out":inputs_out,
        "train_dataset_sha256":inputs.train.manifest.sha256,"validation_dataset_sha256":inputs.validation.manifest.sha256,
        "replay_manifest_sha256":inputs.replay_manifest.sha256,"source_receipts":training_receipts+1,
        "training_performed":false,"sealed_holdout_opened":false}),
    )
}

pub(crate) const MAX_DATASET_SHARD_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub(crate) const MAX_DATASET_ROWS: u64 = 14 * 86_400;
pub(crate) const MAX_REPLAY_PARQUET_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub(crate) const MAX_REPLAY_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) struct PlannedCopy {
    pub(crate) from: std::path::PathBuf,
    pub(crate) to: std::path::PathBuf,
    pub(crate) hash: String,
    pub(crate) max_bytes: u64,
}

struct PlannedSource {
    source: SequenceSource,
    dataset: SequenceDatasetV1,
    replay_artifact: Option<Artifact>,
    replay_manifest: Option<Artifact>,
    copies: Vec<PlannedCopy>,
    replay_bytes: u64,
}

pub(crate) fn dataset_budget(bytes: u64, rows: u64) -> anyhow::Result<()> {
    if bytes > MAX_DATASET_SHARD_BYTES || rows > MAX_DATASET_ROWS {
        bail!("sequence cohort exceeds its 14-day row or byte limit");
    }
    Ok(())
}

pub(crate) fn payload_budget(
    train_bytes: u64,
    train_rows: u64,
    validation_bytes: u64,
    validation_rows: u64,
    replay_bytes: u64,
) -> anyhow::Result<()> {
    dataset_budget(train_bytes, train_rows)?;
    dataset_budget(validation_bytes, validation_rows)?;
    if replay_bytes > MAX_REPLAY_PARQUET_BYTES {
        bail!("cohort replay exceeds byte bound");
    }
    let total = train_bytes
        .checked_add(validation_bytes)
        .and_then(|sum| sum.checked_add(replay_bytes))
        .context("sequence cohort copy budget overflow")?;
    if total > MAX_DATASET_SHARD_BYTES * 2 + MAX_REPLAY_PARQUET_BYTES {
        bail!("sequence cohort copy exceeds the pre-copy byte budget");
    }
    Ok(())
}

pub(crate) fn fold_shard_bytes<'a>(
    mut shards: impl Iterator<Item = &'a hft_research_manifest::sequence::SequenceShardV1>,
) -> anyhow::Result<u64> {
    shards
        .try_fold(0_u64, |sum, shard| sum.checked_add(shard.bytes))
        .context("sequence shard bytes overflow")
}

pub(crate) fn fold_shard_rows<'a>(
    mut shards: impl Iterator<Item = &'a hft_research_manifest::sequence::SequenceShardV1>,
) -> anyhow::Result<u64> {
    shards
        .try_fold(0_u64, |sum, shard| sum.checked_add(shard.rows))
        .context("sequence shard rows overflow")
}

pub(crate) fn bounded_file_len(path: &Path, max: u64) -> anyhow::Result<u64> {
    let length = File::open(path)?.metadata()?.len();
    if length == 0 || length > max {
        bail!("cohort source exceeds byte bound");
    }
    Ok(length)
}

fn plan_source(
    spec: &CohortRequest,
    root: &Path,
    receipt: &Artifact,
    output: &Path,
    include_replay: bool,
) -> anyhow::Result<PlannedSource> {
    let receipt_path = receipt.path(root)?;
    let bytes = receipt.read(root, MAX_REQUEST_BYTES)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    if value["schema_version"] != "monday.cex_campaign_inputs.v1"
        || value["source_revision"] != spec.producer_source_revision
        || value["image_ref"] != spec.producer_image
        || value["symbol"] != "SOLUSDT"
        || value["market"] != "usdm"
    {
        bail!("sequence cohort source is not the declared native SOL receipt");
    }
    let receipts_dir = receipt_path.parent().context("receipt parent")?;
    if receipts_dir.file_name().and_then(|s| s.to_str()) != Some("receipts") {
        bail!("source receipt is not in its prepared run");
    }
    let run_root = receipts_dir.parent().context("source run root")?;
    let materialization = item(&value, "materialization")?;
    let report_bytes = materialization.read(run_root, 16 * 1024 * 1024)?;
    let report: serde_json::Value = serde_json::from_slice(&report_bytes)?;
    let sequence_hash = report["sequence_manifest_sha256"]
        .as_str()
        .context("source was prepared without sequence output")?;
    if !hft_research_manifest::sequence::valid_sha256(sequence_hash) {
        bail!("invalid source sequence digest");
    }
    let materialization_parent = Path::new(&materialization.file)
        .parent()
        .context("PIT artifact parent")?;
    let sequence = Artifact {
        file: materialization_parent
            .join(format!("{sequence_hash}.sequence.json"))
            .to_str()
            .context("non UTF-8 path")?
            .into(),
        sha256: sequence_hash.into(),
    };
    let sequence_bytes = sequence.read(run_root, 4 * 1024 * 1024)?;
    let dataset: SequenceDatasetV1 = serde_json::from_slice(&sequence_bytes)?;
    dataset.validate().map_err(anyhow::Error::msg)?;
    if dataset.digest().map_err(anyhow::Error::msg)? != sequence_hash
        || dataset.input != SequenceInputSpecV1::sol_lob()
    {
        bail!("source sequence manifest changed");
    }
    let source = SequenceSource {
        receipt: put_metadata(output, &bytes, "receipt.json")?,
        materialization: put_metadata(output, &report_bytes, "materialization.json")?,
        sequence: put_metadata(output, &sequence_bytes, "sequence.json")?,
    };
    let mut copies = Vec::new();
    for shard in &dataset.shards {
        let reference = Artifact {
            file: materialization_parent
                .join(&shard.file)
                .to_str()
                .context("non UTF-8 shard")?
                .into(),
            sha256: shard.sha256.clone(),
        };
        let from = reference.path(run_root)?;
        bounded_file_len(&from, shard.bytes)?;
        copies.push(PlannedCopy {
            from,
            to: output.join(&shard.file),
            hash: shard.sha256.clone(),
            max_bytes: shard.bytes,
        });
    }
    let plan_replay =
        |field: &str, extension: &str, max: u64| -> anyhow::Result<(Artifact, PlannedCopy)> {
            let reference = item(&value, field)?;
            let from = reference.path(run_root)?;
            let length = bounded_file_len(&from, max)?;
            let target = Artifact {
                file: format!("{}.{}", reference.sha256, extension),
                sha256: reference.sha256.clone(),
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
        let replay_bytes = artifact_copy.max_bytes;
        copies.push(artifact_copy);
        copies.push(manifest_copy);
        (Some(artifact), Some(manifest), replay_bytes)
    } else {
        (None, None, 0)
    };
    Ok(PlannedSource {
        source,
        dataset,
        replay_artifact,
        replay_manifest,
        copies,
        replay_bytes,
    })
}

pub(crate) fn item(value: &serde_json::Value, name: &str) -> anyhow::Result<Artifact> {
    let result = Artifact {
        file: value[name]["relative_path"]
            .as_str()
            .context("missing receipt artifact path")?
            .into(),
        sha256: value[name]["sha256"]
            .as_str()
            .context("missing receipt artifact hash")?
            .into(),
    };
    result.validate()?;
    Ok(result)
}

pub(crate) fn put_metadata(root: &Path, bytes: &[u8], suffix: &str) -> anyhow::Result<Artifact> {
    let hash = format!("{:x}", Sha256::digest(bytes));
    let artifact = Artifact {
        file: format!("{hash}.{suffix}"),
        sha256: hash,
    };
    let path = root.join(&artifact.file);
    if path.try_exists()? {
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file()
            || metadata.len() != bytes.len() as u64
            || crate::mission_runner::sha256_file(&path)? != artifact.sha256
        {
            bail!("existing cohort metadata changed");
        }
    } else {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    Ok(artifact)
}

pub(crate) fn copy_verified(
    source: &Path,
    destination: &Path,
    hash: &str,
    max: u64,
) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.file_type().is_file() || metadata.len() > max {
        bail!("cohort source is not a regular bounded file");
    }
    let input = File::open(source)?;
    if input.metadata()?.len() > max {
        bail!("cohort source exceeds byte bound");
    }
    if destination.try_exists()? {
        let metadata = fs::symlink_metadata(destination)?;
        if !metadata.file_type().is_file()
            || metadata.len() > max
            || crate::mission_runner::sha256_file(destination)? != hash
        {
            bail!("cohort artifact conflict");
        }
        return Ok(());
    }
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    let count = std::io::copy(&mut input.take(max + 1), &mut output)?;
    output.sync_all()?;
    if count > max || crate::mission_runner::sha256_file(destination)? != hash {
        bail!("cohort copy hash or byte bound differs");
    }
    Ok(())
}

fn assemble_dataset(
    root: &Path,
    sources: Vec<SequenceSource>,
    shards: Vec<hft_research_manifest::sequence::SequenceShardV1>,
) -> anyhow::Result<DatasetLocation> {
    dataset_budget(
        fold_shard_bytes(shards.iter())?,
        fold_shard_rows(shards.iter())?,
    )?;
    let index = SequenceSourceIndex {
        schema_version: "monday.sol_sequence_sources.v1".into(),
        sources,
    };
    let sources = put_metadata(root, &serde_json::to_vec(&index)?, "sources.json")?;
    let dataset = SequenceDatasetV1 {
        schema_version: hft_research_manifest::sequence::SEQUENCE_DATASET_SCHEMA.into(),
        venue: "binance-usdm".into(),
        symbol: "SOLUSDT".into(),
        source_manifest_sha256: sources.sha256.clone(),
        input: SequenceInputSpecV1::sol_lob(),
        shards,
    };
    dataset.validate().map_err(anyhow::Error::msg)?;
    let manifest = put_metadata(root, &serde_json::to_vec(&dataset)?, "cohort.json")?;
    Ok(DatasetLocation { manifest, sources })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequence_cohort_preserves_gaps_and_rejects_reordered_or_duplicate_shards() {
        use hft_research_manifest::sequence::SequenceShardV1;
        let root = tempfile::tempdir().unwrap();
        let shard = |first, hash: &str| SequenceShardV1 {
            file: format!("{}.sequence.jsonl", hash.repeat(64)),
            sha256: hash.repeat(64),
            bytes: 32,
            rows: 2,
            first_observed_at_ms: first,
            last_observed_at_ms: first + 1000,
        };
        let first = shard(0, "a");
        let later = shard(10_000, "b");
        let location =
            assemble_dataset(root.path(), vec![], vec![first.clone(), later.clone()]).unwrap();
        let dataset: SequenceDatasetV1 =
            serde_json::from_slice(&location.manifest.read(root.path(), 4096).unwrap()).unwrap();
        assert_eq!(dataset.shards, vec![first.clone(), later.clone()]);
        assert_eq!(dataset.source_manifest_sha256, location.sources.sha256);
        assert!(assemble_dataset(root.path(), vec![], vec![later, first.clone()]).is_err());
        assert!(assemble_dataset(root.path(), vec![], vec![first.clone(), first]).is_err());
    }
    #[test]
    fn sequence_cohort_copy_enforces_hash_and_size() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input");
        fs::write(&input, b"bounded").unwrap();
        let hash = format!("{:x}", Sha256::digest(b"bounded"));
        let output = root.path().join("output");
        copy_verified(&input, &output, &hash, 7).unwrap();
        copy_verified(&input, &output, &hash, 7).unwrap();
        assert!(copy_verified(&input, &root.path().join("tiny"), &hash, 2).is_err());
        assert!(copy_verified(&input, &output, &"a".repeat(64), 7).is_err());
    }
    #[test]
    fn sequence_cohort_rejects_oversized_payload_before_any_copy() {
        let limit = 8 * 1024 * 1024 * 1024;
        assert!(payload_budget(limit, 14 * 86_400, 1, 1, 0).is_ok());
        assert!(payload_budget(limit + 1, 1, 1, 1, 0).is_err());
        assert!(payload_budget(1, 14 * 86_400 + 1, 1, 1, 0).is_err());
        assert!(payload_budget(1, 1, 1, 1, 16 * 1024 * 1024 * 1024 + 1).is_err());
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("blob");
        fs::write(&source, vec![7_u8; 32]).unwrap();
        let destination = root.path().join("copied");
        let hash = format!("{:x}", Sha256::digest(vec![7_u8; 32].as_slice()));
        assert!(payload_budget(limit + 1, 1, 0, 0, 0).is_err());
        assert!(!destination.exists());
        assert!(copy_verified(&source, &destination, &hash, 32).is_ok());
        assert!(destination.exists());
    }

    fn identity_request(sub_path: &str) -> CohortRequest {
        CohortRequest {
            schema_version: "monday.sol_sequence_cohort_request.v1".into(),
            producer_source_revision: "a".repeat(40),
            producer_image: format!("registry/runner@sha256:{}", "e".repeat(64)),
            pvc_name: "sol-inputs".into(),
            pvc_uid: "pvc-uid".into(),
            sub_path: sub_path.into(),
            fold_id: 1,
            training_window_days: 7,
            training_receipts: vec![Artifact {
                file: "receipt.json".into(),
                sha256: "a".repeat(64),
            }],
            validation_receipt: Artifact {
                file: "validation.json".into(),
                sha256: "b".repeat(64),
            },
        }
    }

    #[test]
    fn sequence_cohort_identity_is_rejected_before_any_copy() {
        assert!(validate_cohort_identity(&identity_request("sol-sequence/fold-1-7")).is_ok());
        assert!(validate_cohort_identity(&identity_request("../sealed")).is_err());
        let mut image = identity_request("sol-sequence/fold-1-7");
        image.producer_image = "registry/runner:latest".into();
        assert!(validate_cohort_identity(&image).is_err());
        let root = tempfile::tempdir().unwrap();
        let request = root.path().join("request.json");
        fs::write(
            &request,
            serde_json::to_vec(&identity_request("../sealed")).unwrap(),
        )
        .unwrap();
        let output = root.path().join("view");
        let error = prepare(PrepareSequenceCohortArgs {
            request,
            input_root: root.path().join("missing"),
            output_root: output.clone(),
            inputs_out: root.path().join("inputs.json"),
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("identity"));
        assert!(!output.exists());
    }

    #[test]
    fn sequence_cohort_resume_finishes_receipt_without_recopying() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("view");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("keep"), b"published").unwrap();
        let inputs_out = root.path().join("inputs.json");
        let pending = pending_receipt(&inputs_out).unwrap();
        let inputs = crate::mission_campaign::sequence::tests::request().inputs;
        fs::write(&pending, serde_json::to_vec(&inputs).unwrap()).unwrap();
        let request = root.path().join("request.json");
        fs::write(
            &request,
            serde_json::to_vec(&identity_request("sol-sequence/fold-1-7")).unwrap(),
        )
        .unwrap();
        let error = prepare(PrepareSequenceCohortArgs {
            request,
            input_root: root.path().join("unused"),
            output_root: output.clone(),
            inputs_out,
        })
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(!message.contains("must be fresh"), "{message}");
        assert!(
            !message.contains("invalid SOL sequence cohort identity"),
            "{message}"
        );
        assert_eq!(fs::read(output.join("keep")).unwrap(), b"published");
        assert!(pending.is_file());
    }
    #[test]
    fn sequence_cohort_publication_resumes_pending_receipt_and_rejects_drift() {
        let root = tempfile::tempdir().unwrap();
        let args = PrepareSequenceCohortArgs {
            request: root.path().join("request"),
            input_root: root.path().join("unused"),
            output_root: root.path().join("view"),
            inputs_out: root.path().join("inputs.json"),
        };
        let verify = |value: &u32, output: &Path| -> anyhow::Result<()> {
            if *value != 7 || std::fs::read(output.join("rows"))? != b"immutable" {
                bail!("changed cohort request or rows");
            }
            Ok(())
        };
        let first = publish_cohort(&args, verify, |output| {
            std::fs::write(output.join("rows"), b"immutable")?;
            Ok(7_u32)
        })
        .unwrap();
        assert_eq!(first, 7);
        let pending = pending_receipt(&args.inputs_out).unwrap();
        std::fs::rename(&args.inputs_out, &pending).unwrap();
        let restored = publish_cohort(&args, verify, |_| -> anyhow::Result<u32> {
            panic!("must not recopy an already published view")
        })
        .unwrap();
        assert_eq!(restored, 7);
        assert!(args.inputs_out.is_file());
        assert!(!pending.exists());
        retain_receipt(&pending, &8_u32).unwrap();
        assert!(publish_cohort(&args, verify, |_| Ok(7_u32)).is_err());
        assert_eq!(
            std::fs::read(args.output_root.join("rows")).unwrap(),
            b"immutable"
        );
    }

    #[test]
    fn sequence_cohort_unpublished_pending_receipt_cannot_be_rebound() {
        let root = tempfile::tempdir().unwrap();
        let args = PrepareSequenceCohortArgs {
            request: root.path().join("request"),
            input_root: root.path().join("unused"),
            output_root: root.path().join("view"),
            inputs_out: root.path().join("inputs.json"),
        };
        let pending = pending_receipt(&args.inputs_out).unwrap();
        retain_receipt(&pending, &7_u32).unwrap();
        assert!(publish_cohort(&args, |_: &u32, _| Ok(()), |_| Ok(8_u32)).is_err());
        assert!(!args.output_root.exists());
        assert_eq!(read_json::<u32>(&pending).unwrap(), 7);
        assert_eq!(
            publish_cohort(&args, |_: &u32, _| Ok(()), |_| Ok(7_u32)).unwrap(),
            7
        );
    }

    #[test]
    fn sequence_cohort_metadata_and_copies_reject_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"immutable";
        let hash = format!("{:x}", Sha256::digest(bytes));
        let original = root.path().join("original");
        fs::write(&original, bytes).unwrap();
        let alias = root.path().join(format!("{hash}.json"));
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        assert!(put_metadata(root.path(), bytes, "json").is_err());
        assert!(copy_verified(&alias, &root.path().join("copied"), &hash, 100).is_err());
        assert!(copy_verified(&original, &alias, &hash, 100).is_err());
    }
}
