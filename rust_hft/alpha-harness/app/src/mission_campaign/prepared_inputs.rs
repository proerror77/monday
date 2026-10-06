//! Trusted native preparation and the exact finalized binding consumed by the worker.
use super::*;
use alpha_domain::EvaluationProtocolV1;
use alpha_engine::evaluation::{prepare_dataset, ResearchRow};
use hft_cex_research_input::{
    campaign::*,
    data::{DataViewSpec, FeatureFrame, ReplayEvent, ReplayPayload, Split, TypedBlock, Window},
    identity,
    prepared::AcquiredBlocks,
};
use std::collections::BTreeMap;

const MAX_COLLECTION_BYTES: u64 = 128 * 1024 * 1024;
const MAX_NATIVE_DECODED_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativePreparedCampaignRefV1 {
    pub collection_sha256: String,
    pub collection_url: String,
    pub expected_native: ExpectedNativeCampaignInputsV1,
    pub block_urls: BTreeMap<String, String>,
    pub render_metadata: crate::mission_render::PreparedCexInputMetadata,
    /// Safe partition metadata frozen in DataReady before Root signing. Older
    /// audit records can omit it; planning never derives it from a fetched body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning_metadata: Option<NativeDatasetMetadataV1>,
}

pub(crate) struct PreparedCampaignArtifacts {
    pub manifest: CampaignPreparedInputsV1,
    pub id: String,
    pub blocks: BTreeMap<String, Vec<u8>>,
}

pub(crate) struct VerifiedNativeCampaignPreparedInputs {
    finalized_request: CampaignRequest,
    request_sha256: String,
    campaign_inputs_sha256: String,
    source_revision: String,
    runner_image_identity: String,
    declared_trials: usize,
    prepared: VerifiedCampaignPreparedInputsV1,
    render_inputs: PreparedCexInputs,
}
impl VerifiedNativeCampaignPreparedInputs {
    pub(crate) fn finalized_request(&self) -> &CampaignRequest {
        &self.finalized_request
    }
    pub(crate) fn request_sha256(&self) -> &str {
        &self.request_sha256
    }
    pub(crate) fn campaign_inputs_sha256(&self) -> &str {
        &self.campaign_inputs_sha256
    }
    pub(crate) fn evaluation_protocol_sha256(&self) -> &str {
        self.prepared.native_protocol_sha256()
    }
    pub(crate) fn source_revision(&self) -> &str {
        &self.source_revision
    }
    pub(crate) fn runner_image_identity(&self) -> &str {
        &self.runner_image_identity
    }
    pub(crate) fn declared_trials(&self) -> usize {
        self.declared_trials
    }
    pub(crate) fn prepared(&self) -> &VerifiedCampaignPreparedInputsV1 {
        &self.prepared
    }
    pub(crate) fn collection_id(&self) -> &str {
        self.prepared.id()
    }
    pub(crate) fn view_ids(&self) -> [&str; 3] {
        self.prepared.view_ids()
    }
}

/// Inspect the original DataReady bytes anchored by the already signed Root.
/// This returns metadata only, not raw-data qualification or a restored view grant.
pub(crate) fn inspect_planning_ready_metadata(
    bytes: &[u8],
    expected_receipt_sha256: &str,
    request: &CampaignRequest,
) -> anyhow::Result<PreparedCexInputs> {
    if bytes.is_empty()
        || bytes.len() > MAX_COLLECTION_BYTES as usize
        || hft_cex_research_input::sha256(bytes) != expected_receipt_sha256
        || request.campaign_inputs_sha256 != expected_receipt_sha256
    {
        bail!("planning DataReady bytes differ from the signed root input identity");
    }
    validate_request_for_execute(request)?;
    let receipt: CampaignInputsReceipt = serde_json::from_slice(bytes)?;
    let reference = request
        .prepared_inputs
        .as_ref()
        .context("planning requires the original development-only prepared reference")?;
    let frozen = receipt
        .prepared_inputs
        .as_ref()
        .context("planning DataReady has no prepared source")?;
    let mut canonical_reference = reference.clone();
    if canonical_reference.collection_url != frozen.collection_url {
        canonical_reference.collection_url = canonical_tokyo_oss_internal_object(
            "planning collection",
            &canonical_reference.collection_url,
        )?;
    }
    for (sha, url) in &mut canonical_reference.block_urls {
        if frozen.block_urls.get(sha) != Some(&*url) {
            *url = canonical_tokyo_oss_internal_object("planning block", url)?;
        }
    }
    if receipt.schema_version != CAMPAIGN_INPUTS_SCHEMA_V1
        || frozen != &canonical_reference
        || receipt.source_revision != request.producer_source_revision
        || mission_dispatch::image_digest(&receipt.image_ref)? != request.producer_image_identity
        || receipt.feature.sha256 != request.feature_sha256
        || receipt.materialization.sha256 != request.materialization_sha256
        || receipt.replay_artifact.sha256 != request.replay_artifact_sha256
        || receipt.replay_manifest.sha256 != request.replay_manifest_sha256
        || receipt.run_id != reference.expected_native.source.preparation_run_id
    {
        bail!("planning DataReady changed its original producer or prepared sources");
    }
    let mut original = receipt.clone();
    original.prepared_inputs = None;
    if hft_cex_research_input::sha256(&serde_json::to_vec_pretty(&original)?)
        != reference.expected_native.source.preparation_receipt_sha256
    {
        bail!("planning DataReady does not retain the original preparation receipt");
    }
    let inputs = PreparedCexInputs::restore_metadata(
        reference.render_metadata.clone(),
        &request.feature_sha256,
        &request.materialization_sha256,
    )?;
    let materialization = inputs.materialization();
    if receipt.market != materialization.market
        || receipt.symbol != materialization.symbol
        || receipt.mission_id != materialization.mission_id
        || !inputs
            .feature_manifest()
            .artifact_path
            .as_os_str()
            .is_empty()
    {
        bail!("planning metadata changed the actual instrument or exposes whole-source inputs");
    }
    validate_signed_planning_metadata(request)?;
    Ok(inputs)
}

/// `independently_expected_request_sha256` comes from the authenticated reservation/witness.
/// Verifying data cannot replace that admission, transfer its budget, or launch a Run.
pub(crate) fn inspect_finalized_campaign_prepared_inputs(
    request: &CampaignRequest,
    independently_expected_request_sha256: &str,
    collection: CampaignPreparedInputsV1,
    source: &mut impl hft_cex_research_input::data::BlockSource,
    max_decoded_bytes: u64,
) -> anyhow::Result<VerifiedNativeCampaignPreparedInputs> {
    validate_request_for_execute(request)?;
    let request_sha256 = hft_cex_research_input::sha256(&serialize_request(request)?);
    if request_sha256
        != normalized_sha256(
            "independently admitted native request",
            independently_expected_request_sha256,
        )?
    {
        bail!("finalized native request differs from durable admission/witness");
    }
    let reference = request
        .prepared_inputs
        .as_ref()
        .context("native Campaign has no actual prepared collection")?;
    if !request.feature_url.is_empty()
        || !request.replay_artifact_url.is_empty()
        || !request.replay_manifest_url.is_empty()
        || !request.materialization_url.is_empty()
    {
        bail!("native prepared request still exposes whole-source/withheld input transports");
    }
    if reference.expected_native.source.feature_sha256 != request.feature_sha256
        || reference.expected_native.source.materialization_sha256 != request.materialization_sha256
        || reference.expected_native.source.replay_artifact_sha256 != request.replay_artifact_sha256
        || reference.expected_native.source.replay_manifest_sha256 != request.replay_manifest_sha256
        || reference.expected_native.source.build.source_revision
            != request.producer_source_revision
        || mission_dispatch::image_digest(&reference.expected_native.source.build.image_identity)?
            != request.producer_image_identity
    {
        bail!("prepared collection changed its native original source Build/input identity");
    }
    let prepared = collection.verify(
        &reference.collection_sha256,
        &reference.expected_native,
        source,
        max_decoded_bytes,
    )?;
    let declared = prepared
        .manifest()
        .features
        .manifest
        .blocks
        .iter()
        .chain(&prepared.manifest().future_marks.manifest.blocks)
        .chain(&prepared.manifest().replay.manifest.blocks)
        .map(|b| b.sha256.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if reference
        .block_urls
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        != declared
    {
        bail!("native prepared transport set differs from its actual views");
    }
    let render_inputs = verify_native_render_binding(request, &prepared)?;
    Ok(VerifiedNativeCampaignPreparedInputs {
        finalized_request: request.clone(),
        request_sha256,
        campaign_inputs_sha256: request.campaign_inputs_sha256.clone(),
        source_revision: request.build_source_revision.clone(),
        runner_image_identity: request.image_identity.clone(),
        declared_trials: request.declared_total_trials,
        prepared,
        render_inputs,
    })
}

impl VerifiedNativeCampaignPreparedInputs {
    pub(crate) fn render_inputs(&self) -> &PreparedCexInputs {
        &self.render_inputs
    }
}
fn verify_native_render_binding(
    request: &CampaignRequest,
    prepared: &VerifiedCampaignPreparedInputsV1,
) -> anyhow::Result<PreparedCexInputs> {
    let reference = request
        .prepared_inputs
        .as_ref()
        .context("missing prepared reference")?;
    let render = PreparedCexInputs::restore_metadata(
        reference.render_metadata.clone(),
        &request.feature_sha256,
        &request.materialization_sha256,
    )?;
    let protocol = crate::mission_render::approved_evaluation_protocol_for_plan(
        render.materialization(),
        &request.research_plan,
    )?;
    if !render
        .feature_manifest()
        .artifact_path
        .as_os_str()
        .is_empty()
    {
        bail!("native render metadata exposes the original whole-source staging path");
    }
    if serde_json::from_str::<EvaluationProtocolV1>(&prepared.original_metadata().protocol_json)?
        != protocol
        || render.materialization().rows != prepared.original_metadata().total_rows
        || render.feature_manifest().rows != prepared.original_metadata().total_rows
        || render.materialization().series_count != 1
    {
        bail!("native prepared source metadata/protocol differs from the original Mission");
    }
    alpha_engine::evaluation::prepare_native_campaign_dataset(prepared, &protocol)?;
    Ok(render)
}

pub(super) fn freeze_native_prepared_reference(
    inputs: &ValidatedCampaignInputSet,
    plan: &CexCampaignResearchPlanV1,
) -> anyhow::Result<NativePreparedCampaignRefV1> {
    if inputs.receipt.prepared_inputs.is_some() {
        bail!("native exporter requires the original source receipt, before collection freeze");
    }
    inputs.render_inputs.verify_development_precheck(plan)?;
    let protocol = crate::mission_render::approved_evaluation_protocol_for_plan(
        inputs.render_inputs.materialization(),
        plan,
    )?;
    let rows = inputs.render_inputs.native_source_rows(&protocol)?;
    let partitions = protocol.row_partitions(rows.len())?;
    let first_withheld = partitions
        .selection
        .as_ref()
        .map_or(partitions.sealed_holdout.start, |r| r.start);
    let end = rows[first_withheld]
        .available_time
        .timestamp_nanos_opt()
        .context("native context boundary overflow")?;
    let canonical = hft_backtest::config::verify_canonical_replay_artifact(
        &inputs.replay_artifact_path,
        &inputs.replay_manifest_path,
        Some(&inputs.replay_artifact_sha256),
        &inputs.replay_manifest_sha256,
        None,
        Some(
            end.checked_sub(1)
                .context("native boundary underflow")?
                .div_euclid(1000),
        ),
    )?;
    let source = NativeSourceBindingV1 {
        build: SourceBuildRefV1 {
            source_revision: inputs.receipt.source_revision.clone(),
            image_identity: inputs.receipt.image_ref.clone(),
        },
        preparation_run_id: inputs.receipt.run_id.clone(),
        preparation_receipt_sha256: inputs.campaign_inputs_sha256.clone(),
        feature_sha256: inputs.feature_sha256.clone(),
        materialization_sha256: inputs.materialization_sha256.clone(),
        replay_artifact_sha256: inputs.replay_artifact_sha256.clone(),
        replay_manifest_sha256: inputs.replay_manifest_sha256.clone(),
    };
    let artifacts = export_trusted_source(source, rows, &protocol, canonical)?;
    let root = campaign_inputs_output_root(&inputs.receipt)?;
    let mut block_urls = BTreeMap::new();
    for (sha, bytes) in &artifacts.blocks {
        let relative = PathBuf::from(format!("native-prepared/{}.mondaybin", sha));
        let local = inputs.input_root.join(&relative);
        persist_native_bytes(&local, bytes)?;
        block_urls.insert(
            sha.clone(),
            format!("{root}/{}", relative.to_string_lossy()),
        );
    }
    let relative = format!("native-prepared/{}.json", artifacts.id);
    let bytes = serde_json::to_vec(&artifacts.manifest)?;
    if bytes.len() as u64 > MAX_COLLECTION_BYTES {
        bail!("native prepared metadata exceeds its immutable artifact bound");
    }
    persist_native_bytes(&inputs.input_root.join(&relative), &bytes)?;
    Ok(NativePreparedCampaignRefV1 {
        collection_sha256: artifacts.id,
        collection_url: format!("{root}/{relative}"),
        expected_native: artifacts.manifest.expected_native()?,
        block_urls,
        render_metadata: inputs.render_inputs.native_metadata()?,
        planning_metadata: Some(artifacts.manifest.original.clone()),
    })
}

fn persist_native_bytes(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.try_exists()? {
        if std::fs::read(path)? != bytes {
            bail!("native immutable preparation artifact changed");
        }
        return Ok(());
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn acquire_native_prepared(
    request: &CampaignRequest,
    expected_request_sha256: &str,
    client: &Client,
    directory: &Path,
) -> anyhow::Result<VerifiedNativeCampaignPreparedInputs> {
    acquire_prepared_with_guard(
        request,
        expected_request_sha256,
        client,
        directory,
        false,
        || Ok(()),
    )
}

/// Planning reads only the original search projection, with current authority
/// checked before each transport read and again after original verification.
pub(crate) fn acquire_planning_prepared(
    request: &CampaignRequest,
    expected_request_sha256: &str,
    client: &Client,
    directory: &Path,
    guard: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<VerifiedNativeCampaignPreparedInputs> {
    acquire_prepared_with_guard(
        request,
        expected_request_sha256,
        client,
        directory,
        true,
        guard,
    )
}

fn acquire_prepared_with_guard(
    request: &CampaignRequest,
    expected_request_sha256: &str,
    client: &Client,
    directory: &Path,
    planning: bool,
    mut guard: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<VerifiedNativeCampaignPreparedInputs> {
    guard()?;
    let reference = request
        .prepared_inputs
        .as_ref()
        .context("native Campaign requires a frozen prepared collection")?;
    let metadata_path = directory.join("native-prepared-inputs.json");
    let read_collection = || {
        fetch_verified(
            client,
            "native prepared collection",
            &reference.collection_url,
            &metadata_path,
            &reference.collection_sha256,
            MAX_COLLECTION_BYTES,
        )?;
        Ok(serde_json::from_slice::<CampaignPreparedInputsV1>(
            &std::fs::read(&metadata_path)?,
        )?)
    };
    let collection = if planning {
        with_signed_planning_metadata(request, read_collection)?
    } else {
        read_collection()?
    };
    collection.validate_metadata()?;
    if collection.id()? != reference.collection_sha256
        || collection.expected_native()? != reference.expected_native
    {
        bail!("native collection differs from the finalized source binding");
    }
    let declared = collection
        .features
        .manifest
        .blocks
        .iter()
        .chain(&collection.future_marks.manifest.blocks)
        .chain(&collection.replay.manifest.blocks)
        .map(|block| block.sha256.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if declared != reference.block_urls.keys().cloned().collect() {
        bail!("native collection transports do not match the actual allowed views");
    }
    let mut read = |collection| {
        let mut files = BTreeMap::new();
        for (sha, url) in &reference.block_urls {
            guard()?;
            let path = directory.join(format!("{sha}.mondaybin"));
            fetch_verified(
                client,
                "native prepared block",
                url,
                &path,
                sha,
                16 * 1024 * 1024,
            )?;
            files.insert(sha.clone(), path);
        }
        inspect_finalized_campaign_prepared_inputs(
            request,
            expected_request_sha256,
            collection,
            &mut ReadbackBlocks { files },
            MAX_NATIVE_DECODED_BYTES,
        )
    };
    let verified = if planning {
        with_planning_projection(collection, request, read)?
    } else {
        read(collection)?
    };
    guard()?;
    Ok(verified)
}

fn with_planning_projection<T>(
    collection: CampaignPreparedInputsV1,
    request: &CampaignRequest,
    read: impl FnOnce(CampaignPreparedInputsV1) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    validate_planning_projection_metadata(&collection, request)?;
    read(collection)
}

fn with_signed_planning_metadata<T>(
    request: &CampaignRequest,
    read: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    validate_signed_planning_metadata(request)?;
    read()
}

pub(crate) fn validate_signed_planning_metadata(request: &CampaignRequest) -> anyhow::Result<()> {
    let reference = request
        .prepared_inputs
        .as_ref()
        .context("planning source reference missing")?;
    let original = reference.planning_metadata.as_ref().context(
        "planning needs safe original partition metadata before reading collection rows",
    )?;
    if identity(original)? != reference.expected_native.original_metadata_sha256 {
        bail!("planning safe header differs from the signed DataReady metadata identity");
    }
    validate_planning_metadata(original, request)
}

fn validate_planning_metadata(
    original: &NativeDatasetMetadataV1,
    request: &CampaignRequest,
) -> anyhow::Result<()> {
    let reference = request
        .prepared_inputs
        .as_ref()
        .context("planning request has no prepared metadata")?;
    let render = PreparedCexInputs::restore_metadata(
        reference.render_metadata.clone(),
        &request.feature_sha256,
        &request.materialization_sha256,
    )?;
    let materialization = render.materialization();
    let protocol = crate::mission_render::approved_evaluation_protocol_for_plan(
        materialization,
        &request.research_plan,
    )?;
    let partitions = protocol.row_partitions(materialization.rows)?;
    if original.total_rows != materialization.rows
        || original.protocol_sha256 != protocol.content_hash()?
        || serde_json::from_str::<EvaluationProtocolV1>(&original.protocol_json)? != protocol
        || original.search_rows != partitions.search
        || original.visible_rows != partitions.search
        || original.selection.as_ref().map(|view| &view.original_rows)
            != partitions.selection.as_ref()
        || original.holdout.original_rows != partitions.sealed_holdout
    {
        bail!("planning source is not the original search-only development projection");
    }
    let first = materialization
        .snapshot
        .first_event_time
        .timestamp_nanos_opt()
        .context("planning first feature clock overflow")?;
    let cadence = i64::try_from(materialization.bucket_ms)?
        .checked_mul(1_000_000)
        .context("planning cadence overflow")?;
    let clock = |row: usize| -> anyhow::Result<i64> {
        first
            .checked_add(
                i64::try_from(row)?
                    .checked_mul(cadence)
                    .context("planning row clock overflow")?,
            )
            .context("planning clock overflow")
    };
    let search_window = Window {
        start_ns: clock(partitions.search.start)?,
        end_ns: clock(partitions.search.end)?,
    };
    let withheld_start = partitions
        .selection
        .as_ref()
        .map_or(partitions.sealed_holdout.start, |view| view.start);
    let context_end = clock(withheld_start)?;
    if original.development_window != search_window
        || original.authorized_context_end_ns != context_end
        || original.holdout.window.start_ns != clock(partitions.sealed_holdout.start)?
    {
        bail!("planning source clocks extend outside the signed original search/label context");
    }
    if let Some(selection) = &original.selection {
        if selection.window.start_ns != clock(selection.original_rows.start)?
            || selection.window.end_ns != clock(selection.original_rows.end)?
        {
            bail!("planning original selection clocks differ from the actual protocol");
        }
    }
    Ok(())
}

pub(crate) fn validate_planning_projection_metadata(
    collection: &CampaignPreparedInputsV1,
    request: &CampaignRequest,
) -> anyhow::Result<()> {
    validate_signed_planning_metadata(request)?;
    let header = request
        .prepared_inputs
        .as_ref()
        .unwrap()
        .planning_metadata
        .as_ref()
        .unwrap();
    if &collection.original != header
        || collection.features.manifest.spec.split != Split::Validation
        || collection.features.manifest.spec.window != header.development_window
        || collection.future_marks.manifest.spec.window.end_ns > header.authorized_context_end_ns
        || collection.replay.manifest.spec.window.end_ns > header.authorized_context_end_ns
    {
        bail!("fetched planning collection differs from its original signed safe scope");
    }
    Ok(())
}

struct ReadbackBlocks {
    files: BTreeMap<String, PathBuf>,
}

#[cfg(all(test, feature = "scientific"))]
mod planning_projection_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn safe_scope_is_verified_before_any_collection_or_block_read() {
        let fixture = crate::mission_campaign::tests::native_prepared_fixture_for_tests();
        let reads = Cell::new(0);
        with_signed_planning_metadata(&fixture.request, || {
            reads.set(reads.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(reads.get(), 1);
        for change in ["absent", "range", "protocol", "rows", "context"] {
            let mut request = fixture.request.clone();
            let reference = request.prepared_inputs.as_mut().unwrap();
            if change == "absent" {
                reference.planning_metadata = None;
                assert!(serde_json::to_value(&reference)
                    .unwrap()
                    .get("planning_metadata")
                    .is_none());
            } else {
                let header = reference.planning_metadata.as_mut().unwrap();
                match change {
                    "range" => {
                        header.search_rows.end += 1;
                        header.visible_rows.end += 1;
                    }
                    "protocol" => header.protocol_sha256 = "0".repeat(64),
                    "rows" => header.total_rows += 1,
                    "context" => header.authorized_context_end_ns += 1,
                    _ => unreachable!(),
                }
                // Even an internally rehashed declaration cannot replace actual protocol bounds.
                reference.expected_native.original_metadata_sha256 = identity(header).unwrap();
            }
            assert!(
                with_signed_planning_metadata(&request, || {
                    reads.set(reads.get() + 1);
                    Ok(())
                })
                .is_err(),
                "{change}"
            );
            assert_eq!(reads.get(), 1, "{change} read protected collection anchors");
        }
        let mut collection = fixture.inputs.prepared().manifest().clone();
        collection.original.search_rows.end += 1;
        collection.original.visible_rows.end += 1;
        assert!(with_planning_projection(collection, &fixture.request, |_| {
            reads.set(reads.get() + 1);
            Ok(())
        })
        .is_err());
        assert_eq!(
            reads.get(),
            1,
            "changed collection fetched protected blocks"
        );
    }
}
impl hft_cex_research_input::data::BlockSource for ReadbackBlocks {
    fn read(&mut self, block: &hft_cex_research_input::data::BlockRef) -> anyhow::Result<Vec<u8>> {
        let path = self
            .files
            .get(&block.sha256)
            .context("native declared block readback absent")?;
        let mut file = std::fs::File::open(path)?;
        if file.metadata()?.len() != block.bytes {
            bail!("native block readback byte count changed");
        }
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(
                block
                    .bytes
                    .checked_add(1)
                    .context("native block byte limit overflow")?,
            )
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

/// The caller holds source receipt/file verification. This exporter reads only actual observations.
pub(super) fn export_trusted_source(
    source: NativeSourceBindingV1,
    full_rows: Vec<ResearchRow>,
    protocol: &EvaluationProtocolV1,
    canonical: hft_backtest::config::VerifiedCanonicalReplay,
) -> anyhow::Result<PreparedCampaignArtifacts> {
    prepare_dataset(full_rows.clone(), protocol)?;
    let partitions = protocol.row_partitions(full_rows.len())?;
    let visible_end = protocol
        .calendar
        .as_ref()
        .map_or(partitions.search.end, |c| c.develop_end_row);
    if visible_end == 0
        || visible_end > partitions.sealed_holdout.start
        || full_rows.iter().any(|row| row.series_id != 1)
    {
        bail!("native prepared replay requires an explicitly bounded single continuous series");
    }
    let ns = |time: chrono::DateTime<chrono::Utc>| {
        time.timestamp_nanos_opt()
            .context("native source clock overflow")
    };
    let first_withheld = partitions
        .selection
        .as_ref()
        .map_or(partitions.sealed_holdout.start, |r| r.start);
    let context_end = ns(full_rows[first_withheld].available_time)?;
    let development_start = ns(full_rows[0].available_time)?;
    let development_end = ns(full_rows[visible_end].available_time)?;
    if development_end > context_end {
        bail!("native development rows include a withheld partition");
    }
    let actual_prices = full_rows
        .iter()
        .map(|row| {
            Ok((
                (row.series_id, ns(row.available_time)?),
                *row.features
                    .get("mid_price")
                    .context("native source lacks actual mid_price")?,
            ))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let names = full_rows[0].features.keys().cloned().collect::<Vec<_>>();
    let mut frames = Vec::with_capacity(visible_end);
    let mut anchors = Vec::with_capacity(visible_end);
    let mut marks = BTreeMap::new();
    for (ordinal, row) in full_rows[..visible_end].iter().enumerate() {
        let clock = ns(row.available_time)?;
        let future_at = ns(row.label_available_time)?;
        if future_at >= context_end {
            bail!("native label requires a price/maturity in withheld selection or holdout");
        }
        let future = *actual_prices
            .get(&(row.series_id, future_at))
            .context("original native future mark has no actual source observation")?;
        let current = *row
            .features
            .get("mid_price")
            .context("native source lacks actual current price")?;
        if (future / current - 1.0).to_bits() != row.label.to_bits() {
            bail!("native source label differs from its actual future/current price recipe");
        }
        let segment = format!("native-series-{}", row.series_id);
        frames.push(FeatureFrame {
            segment: segment.clone(),
            ordinal: ordinal as u64,
            event_ns: clock,
            available_ns: clock,
            values: names.iter().map(|name| row.features[name]).collect(),
        });
        anchors.push(NativeLabelAnchorV1 {
            ordinal: ordinal as u64,
            series_id: row.series_id,
            segment: segment.clone(),
            observed_at_ns: clock,
            future_at_ns: future_at,
            mature_at_ns: future_at,
            signal: row.signal,
            fee_bps: row.fee_bps,
            funding_bps: row.funding_bps,
            pit_funding: row.pit_funding,
            latency_bps: row.latency_bps,
        });
        marks.insert((future_at, segment), future);
    }
    let future_frames = marks
        .into_iter()
        .enumerate()
        .map(|(ordinal, ((clock, segment), price))| FeatureFrame {
            segment,
            ordinal: ordinal as u64,
            event_ns: clock,
            available_ns: clock,
            values: vec![price],
        })
        .collect::<Vec<_>>();
    let mut replay_rows = Vec::new();
    for line in canonical
        .bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let event: hft_backtest::event::EventEnvelope = serde_json::from_slice(line)?;
        let clock = event
            .ts
            .checked_mul(1000)
            .context("native replay clock overflow")?;
        // The authorised projection is explicit. Never pass a withheld event to the worker.
        if clock >= context_end {
            break;
        }
        let levels = |rows: Vec<hft_backtest::event::Level>| {
            rows.into_iter().map(|r| [r.price, r.quantity]).collect()
        };
        let payload = match event.payload {
            hft_backtest::event::EventPayload::Snapshot { bids, asks } => ReplayPayload::Snapshot {
                bids: levels(bids),
                asks: levels(asks),
            },
            hft_backtest::event::EventPayload::L2Update { bids, asks } => ReplayPayload::Delta {
                bids: levels(bids),
                asks: levels(asks),
            },
            hft_backtest::event::EventPayload::Trade {
                side,
                price,
                quantity,
            } => ReplayPayload::Trade {
                price,
                quantity,
                buyer_initiated: side == hft_backtest::event::TradeSide::Buy,
            },
        };
        replay_rows.push(ReplayEvent {
            segment: "native-series-1".into(),
            ordinal: event
                .sequence
                .context("native replay lacks original sequence")?,
            event_ns: clock,
            available_ns: clock,
            payload,
        });
    }
    let replay_start = replay_rows
        .first()
        .context("native authorized replay is empty")?
        .available_ns;
    let original_end = canonical
        .evidence
        .last_event_time_us
        .checked_mul(1000)
        .and_then(|t| t.checked_add(1))
        .context("native replay end overflow")?;
    let protocol_json = serde_json::to_string(protocol)?;
    let opaque = |range: std::ops::Range<usize>| -> anyhow::Result<OpaqueWithheldPartitionV1> {
        Ok(OpaqueWithheldPartitionV1 {
            window: Window {
                start_ns: ns(full_rows[range.start].available_time)?,
                end_ns: if range.end < full_rows.len() {
                    ns(full_rows[range.end].available_time)?
                } else {
                    original_end
                },
            },
            source_content_sha256: identity(&&full_rows[range.clone()])?,
            original_rows: range,
        })
    };
    let metadata = NativeDatasetMetadataV1 {
        total_rows: full_rows.len(),
        original_window: Window {
            start_ns: replay_start,
            end_ns: original_end,
        },
        original_rows_sha256: identity(&full_rows)?,
        protocol_sha256: alpha_domain::canonical_json_hash(protocol)?,
        protocol_json,
        search_rows: partitions.search.clone(),
        visible_rows: 0..visible_end,
        development_window: Window {
            start_ns: development_start,
            end_ns: development_end,
        },
        authorized_context_end_ns: context_end,
        selection: partitions.selection.clone().map(opaque).transpose()?,
        holdout: opaque(partitions.sealed_holdout.clone())?,
    };
    let sources = {
        let mut values = vec![
            source.feature_sha256.clone(),
            source.materialization_sha256.clone(),
            source.replay_artifact_sha256.clone(),
            source.replay_manifest_sha256.clone(),
        ];
        values.sort();
        values.dedup();
        values
    };
    let horizon_ns = i64::try_from(protocol.labels.horizon_buckets)
        .ok()
        .and_then(|count| {
            i64::try_from(protocol.labels.observation_frequency_millis)
                .ok()
                .and_then(|frequency| count.checked_mul(frequency))
        })
        .and_then(|millis| millis.checked_mul(1_000_000))
        .context("native label horizon overflow")?;
    let replay_payload_depth = replay_rows
        .iter()
        .filter_map(|row| match &row.payload {
            ReplayPayload::Snapshot { bids, asks } | ReplayPayload::Delta { bids, asks } => {
                Some(bids.len().max(asks.len()))
            }
            ReplayPayload::Trade { .. } => None,
        })
        .max()
        .context("native replay has no actual book payload")?;
    let replay_payload_depth =
        u16::try_from(replay_payload_depth).context("native replay payload depth overflow")?;
    let feature_sql_sha256 = identity(&(NATIVE_LABEL_RECIPE, &names))?;
    let spec = |window: Window, feature_names: Vec<String>| DataViewSpec {
        schema: 1,
        venue: "binance".into(),
        instrument: canonical.evidence.symbol.clone(),
        market: canonical.evidence.market.clone(),
        depth: replay_payload_depth,
        sources: sources.clone(),
        normalizer_sha256: source.materialization_sha256.clone(),
        feature_sql_sha256: feature_sql_sha256.clone(),
        feature_names,
        window,
        lookback_ns: 0,
        horizons_ns: vec![horizon_ns],
        label_tolerance_ns: 0,
        fit_cutoff_ns: context_end,
        split: Split::Validation,
    };
    let mut blocks = BTreeMap::new();
    let chunks = |rows: Vec<FeatureFrame>| {
        rows.chunks(4096)
            .map(|r| TypedBlock::Features(r.to_vec()))
            .collect()
    };
    let (features, bytes) = encode_prepared_view(
        spec(metadata.development_window.clone(), names),
        chunks(frames),
        &source,
    )?;
    blocks.extend(bytes);
    let (future_marks, bytes) = encode_prepared_view(
        spec(
            Window {
                start_ns: development_start,
                end_ns: context_end,
            },
            vec!["mid_price".into()],
        ),
        chunks(future_frames),
        &source,
    )?;
    blocks.extend(bytes);
    let replay_hash = identity(&replay_rows)?;
    let replay_blocks = replay_rows
        .chunks(4096)
        .map(|rows| TypedBlock::Replay(rows.to_vec()))
        .collect();
    let (replay, bytes) = encode_prepared_view(
        spec(
            Window {
                start_ns: replay_start,
                end_ns: context_end,
            },
            vec!["mid_price".into()],
        ),
        replay_blocks,
        &source,
    )?;
    blocks.extend(bytes);
    let manifest = CampaignPreparedInputsV1 {
        schema_version: CAMPAIGN_PREPARED_INPUTS_SCHEMA.into(),
        source,
        original: metadata,
        label_recipe: NATIVE_LABEL_RECIPE.into(),
        anchors,
        development_rows_sha256: identity(&&full_rows[..visible_end])?,
        replay_rows_sha256: replay_hash,
        features,
        future_marks,
        replay,
    };
    let id = manifest.id()?;
    let expected = manifest.expected_native()?;
    let mut acquired = AcquiredBlocks {
        bytes: blocks.clone(),
    };
    let verified =
        manifest
            .clone()
            .verify(&id, &expected, &mut acquired, MAX_NATIVE_DECODED_BYTES)?;
    if verified.rows() != &full_rows[..visible_end]
        || verified.original_metadata().search_rows != partitions.search
    {
        bail!("native export does not reproduce the verified source rows/schedule");
    }
    Ok(PreparedCampaignArtifacts {
        manifest,
        id,
        blocks,
    })
}
