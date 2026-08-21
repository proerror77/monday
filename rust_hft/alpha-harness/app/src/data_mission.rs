use alpha_domain::{EvaluationCostsV1, EvaluationLabelSpecV1};
use alpha_engine::evaluation::ResearchRow;
use alpha_store::{AlphaStore, RegistryRevision};
use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use hft_collector::{
    acquire_dataset, import_feature_dataset, lob_archiver::source_revision, read_feature_rows,
    DataAcquisitionMission, DataModality, DatasetManifest, FeatureDatasetManifest, OhlcvTraceRow,
};
use hft_research_manifest::{
    CexReplayDatasetManifestV1, CexReplayDatasetManifestV2, CexReplayDatasetManifestV3,
    CexReplayDatasetManifestV4, CexReplayDatasetManifestV5, CexReplaySeriesV1, CexReplaySnapshotV1,
    CexReplaySnapshotV2, CexReplaySnapshotV3, CexReplaySnapshotV4, CexReplaySnapshotV5,
    CEX_REPLAY_DATASET_KIND, CEX_REPLAY_DATASET_SCHEMA_V1, CEX_REPLAY_DATASET_SCHEMA_V2,
    CEX_REPLAY_DATASET_SCHEMA_V3, CEX_REPLAY_DATASET_SCHEMA_V4, CEX_REPLAY_DATASET_SCHEMA_V5,
    CEX_REPLAY_SNAPSHOT_SCHEMA_V3, CEX_REPLAY_SNAPSHOT_SCHEMA_V4,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{BufRead, Write},
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FeatureDecisionClock {
    pub(crate) series_id: u64,
    pub(crate) feature_available_time: DateTime<Utc>,
    pub(crate) series_close_time: DateTime<Utc>,
}

struct BoundedWriter<W> {
    inner: W,
    remaining: u64,
    max_bytes: u64,
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let allowed = buffer
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        if allowed == 0 && !buffer.is_empty() {
            return Err(std::io::Error::other(format!(
                "serialized JSON exceeds maximum {} bytes",
                self.max_bytes
            )));
        }
        let written = self.inner.write(&buffer[..allowed])?;
        self.remaining = self
            .remaining
            .saturating_sub(u64::try_from(written).unwrap_or(u64::MAX));
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub async fn acquire_and_register(
    store: &mut AlphaStore,
    mission: &DataAcquisitionMission,
) -> anyhow::Result<DatasetManifest> {
    let manifest = acquire_dataset(mission).await.map_err(anyhow::Error::msg)?;
    store.put_registry_revision(&RegistryRevision {
        revision_id: manifest.manifest_id.clone(),
        registry_kind: "dataset".to_string(),
        asset_id: manifest.symbol.clone(),
        parent_revision_id: None,
        payload: serde_json::to_value(&manifest)?,
        created_at: manifest.created_at,
    })?;
    Ok(manifest)
}

pub fn import_and_register_features(
    store: &mut AlphaStore,
    mission_id: &str,
    input: &Path,
    artifact_dir: &Path,
) -> anyhow::Result<FeatureDatasetManifest> {
    let manifest =
        import_feature_dataset(mission_id, input, artifact_dir).map_err(anyhow::Error::msg)?;
    store.put_registry_revision(&RegistryRevision {
        revision_id: manifest.manifest_id.clone(),
        registry_kind: "dataset".to_string(),
        asset_id: manifest.symbol.clone(),
        parent_revision_id: None,
        payload: serde_json::to_value(&manifest)?,
        created_at: manifest.created_at,
    })?;
    Ok(manifest)
}

pub fn admit_cex_replay_dataset(
    store: &mut AlphaStore,
    features: &FeatureDatasetManifest,
    snapshot: &CexReplaySnapshotV5,
) -> anyhow::Result<CexReplayDatasetManifestV5> {
    validate_cex_replay_features(snapshot, features)?;
    let manifest = CexReplayDatasetManifestV5::new(features.manifest_id.clone(), snapshot.clone())?;
    store.put_registry_revision(&RegistryRevision {
        revision_id: manifest.manifest_id.clone(),
        registry_kind: "dataset".to_string(),
        asset_id: snapshot.symbol.clone(),
        parent_revision_id: Some(features.manifest_id.clone()),
        payload: serde_json::to_value(&manifest)?,
        created_at: features.created_at,
    })?;
    Ok(manifest)
}

const FORBIDDEN_CURRENT_CEX_FEATURES: [&str; 3] =
    ["funding_cost_bps", "funding_rate", "open_interest"];

pub(crate) fn validate_cex_replay_features(
    snapshot: &CexReplaySnapshotV5,
    features: &FeatureDatasetManifest,
) -> anyhow::Result<()> {
    snapshot.validate()?;
    if features.symbol != snapshot.symbol
        || features.artifact_sha256 != snapshot.feature_artifact_sha256
        || features.label_spec.horizon_buckets != snapshot.label_horizon_buckets
        || features.label_spec.observation_frequency_millis != snapshot.bucket_ms
    {
        bail!("feature lineage or label facts do not match the CEX replay snapshot");
    }
    if features.time_bounds.first_event_time != snapshot.first_event_time
        || features.time_bounds.last_event_time != snapshot.last_event_time
    {
        bail!("feature time bounds do not match the CEX replay snapshot");
    }
    let expected_modalities = BTreeSet::from([DataModality::Lob, DataModality::TradeTick]);
    if features.modalities != expected_modalities {
        bail!("feature modalities do not match the CEX replay snapshot");
    }
    let last_label_available_ns = u64::try_from(
        features
            .time_bounds
            .last_label_available_time
            .timestamp_nanos_opt()
            .context("feature label availability is out of range")?,
    )
    .context("feature label availability is out of range")?;
    if last_label_available_ns
        > snapshot
            .source_segments
            .last()
            .expect("validated snapshot has source segments")
            .end_received_at_ns
    {
        bail!("feature label availability is outside the CEX replay snapshot");
    }
    let source_key = format!("binance-{}-lob", snapshot.instrument_type);
    let expected_source_revision = source_revision(
        snapshot
            .source_segments
            .iter()
            .map(|segment| segment.content_sha256.as_str()),
    );
    if features.source_revisions.len() != 1
        || features.source_revisions.get(&source_key) != Some(&expected_source_revision)
    {
        bail!("feature source revision does not match the CEX replay snapshot");
    }
    if features.series_count != snapshot.series.len() {
        bail!("feature series count does not match the CEX replay snapshot");
    }
    let rows = read_feature_rows(features).map_err(anyhow::Error::msg)?;
    let label_horizon = u64::try_from(snapshot.label_horizon_buckets)
        .ok()
        .and_then(|horizon| snapshot.bucket_ms.checked_mul(horizon))
        .and_then(|offset| i64::try_from(offset).ok())
        .and_then(chrono::TimeDelta::try_milliseconds)
        .context("feature label horizon is out of range")?;
    let mut actual_series = std::collections::BTreeMap::<u64, ActualSeriesBounds>::new();
    for row in rows {
        if row.feature_available_time != row.event_time {
            bail!("feature availability does not match the CEX replay decision clock");
        }
        if let Some(field) = FORBIDDEN_CURRENT_CEX_FEATURES
            .into_iter()
            .find(|field| row.features.contains_key(*field))
        {
            bail!("current L2-only CEX replay cannot include {field}");
        }
        actual_series
            .entry(row.series_id)
            .and_modify(|series| {
                if row.event_time < series.first_event_time {
                    series.first_event_time = row.event_time;
                }
                if row.event_time >= series.last_event_time {
                    series.last_event_time = row.event_time;
                    series.last_label_available_time = row.label_available_time;
                }
            })
            .or_insert(ActualSeriesBounds {
                first_event_time: row.event_time,
                last_event_time: row.event_time,
                last_label_available_time: row.label_available_time,
            });
    }
    if actual_series.len() != snapshot.series.len() {
        bail!("feature row series do not match the CEX replay snapshot");
    }
    for expected in &snapshot.series {
        let actual = actual_series
            .remove(&u64::from(expected.series_id))
            .with_context(|| format!("feature row series {} is missing", expected.series_id))?;
        validate_series_matches_rows(expected, &actual, label_horizon)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActualSeriesBounds {
    first_event_time: DateTime<Utc>,
    last_event_time: DateTime<Utc>,
    last_label_available_time: DateTime<Utc>,
}

fn validate_series_matches_rows(
    expected: &CexReplaySeriesV1,
    actual: &ActualSeriesBounds,
    label_horizon: chrono::TimeDelta,
) -> anyhow::Result<()> {
    if expected.first_event_time != actual.first_event_time
        || expected.last_event_time != actual.last_event_time
    {
        bail!("feature row series bounds do not match the CEX replay snapshot");
    }
    let expected_last_label_available_time = expected
        .last_event_time
        .checked_add_signed(label_horizon)
        .context("feature label horizon is out of range")?;
    if actual.last_label_available_time != expected_last_label_available_time {
        bail!(
            "feature row series {} last label availability {} does not match snapshot close {}",
            expected.series_id,
            actual.last_label_available_time.to_rfc3339(),
            expected_last_label_available_time.to_rfc3339(),
        );
    }
    Ok(())
}

fn validate_cex_replay_features_v4(
    snapshot: &CexReplaySnapshotV4,
    features: &FeatureDatasetManifest,
) -> anyhow::Result<()> {
    snapshot.validate()?;
    if features.symbol != snapshot.symbol
        || features.artifact_sha256 != snapshot.feature_artifact_sha256
        || features.label_spec.horizon_buckets != snapshot.label_horizon_buckets
        || features.label_spec.observation_frequency_millis != snapshot.bucket_ms
    {
        bail!("feature lineage or label facts do not match the historical CEX replay snapshot");
    }
    if features.time_bounds.first_event_time != snapshot.first_event_time
        || features.time_bounds.last_event_time != snapshot.last_event_time
    {
        bail!("feature time bounds do not match the historical CEX replay snapshot");
    }
    let expected_modalities = BTreeSet::from([
        DataModality::Lob,
        DataModality::TradeTick,
        DataModality::Funding,
        DataModality::OpenInterest,
    ]);
    if features.modalities != expected_modalities {
        bail!("feature modalities do not match the historical CEX replay snapshot");
    }
    let last_label_available_ns = u64::try_from(
        features
            .time_bounds
            .last_label_available_time
            .timestamp_nanos_opt()
            .context("feature label availability is out of range")?,
    )
    .context("feature label availability is out of range")?;
    if last_label_available_ns
        > snapshot
            .source_segments
            .last()
            .expect("validated snapshot has source segments")
            .end_received_at_ns
    {
        bail!("feature label availability is outside the historical CEX replay snapshot");
    }
    let source_key = format!("binance-{}-lob", snapshot.instrument_type);
    let expected_source_revision = source_revision(
        snapshot
            .source_segments
            .iter()
            .map(|segment| segment.content_sha256.as_str()),
    );
    if features.source_revisions.len() != 1
        || features.source_revisions.get(&source_key) != Some(&expected_source_revision)
    {
        bail!("feature source revision does not match the historical CEX replay snapshot");
    }
    let rows = read_feature_rows(features).map_err(anyhow::Error::msg)?;
    if rows
        .iter()
        .any(|row| row.feature_available_time != row.event_time)
    {
        bail!("feature availability does not match the CEX replay decision clock");
    }
    Ok(())
}

fn validate_cex_replay_features_v2(
    snapshot: &CexReplaySnapshotV2,
    features: &FeatureDatasetManifest,
) -> anyhow::Result<()> {
    snapshot.validate()?;
    validate_cex_replay_features_v3(
        &CexReplaySnapshotV3 {
            schema_version: CEX_REPLAY_SNAPSHOT_SCHEMA_V3.to_string(),
            venue: snapshot.venue.clone(),
            instrument_type: snapshot.instrument_type.clone(),
            symbol: snapshot.symbol.clone(),
            replay_clock: snapshot.replay_clock.clone(),
            required_modalities: snapshot.required_modalities.clone(),
            source_segments: snapshot.source_segments.clone(),
            first_event_time: snapshot.first_event_time,
            last_event_time: snapshot.last_event_time,
            feature_artifact_sha256: snapshot.feature_artifact_sha256.clone(),
            feature_availability_policy: snapshot.feature_availability_policy.clone(),
            bucket_ms: snapshot.bucket_ms,
            label_horizon_buckets: snapshot.label_horizon_buckets,
            top_depth: snapshot.top_depth,
            instrument_rules: snapshot.instrument_rules.clone(),
            fee_schedule: snapshot.fee_schedule.clone(),
            derivatives_reference: snapshot.derivatives_reference.clone(),
        },
        features,
    )
}

fn validate_cex_replay_features_v3(
    snapshot: &CexReplaySnapshotV3,
    features: &FeatureDatasetManifest,
) -> anyhow::Result<()> {
    snapshot.validate()?;
    if snapshot.instrument_type == "spot" {
        if features.modalities != BTreeSet::from([DataModality::Lob, DataModality::TradeTick]) {
            bail!("feature modalities do not match the historical CEX replay snapshot");
        }
        return validate_cex_replay_features_v1(
            &CexReplaySnapshotV1 {
                schema_version: hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V1.to_string(),
                venue: snapshot.venue.clone(),
                instrument_type: snapshot.instrument_type.clone(),
                symbol: snapshot.symbol.clone(),
                replay_clock: snapshot.replay_clock.clone(),
                required_modalities: snapshot.required_modalities.clone(),
                source_segments: snapshot.source_segments.clone(),
                first_event_time: snapshot.first_event_time,
                last_event_time: snapshot.last_event_time,
                feature_artifact_sha256: snapshot.feature_artifact_sha256.clone(),
                feature_availability_policy: snapshot.feature_availability_policy.clone(),
                bucket_ms: snapshot.bucket_ms,
                label_horizon_buckets: snapshot.label_horizon_buckets,
                top_depth: snapshot.top_depth,
            },
            features,
        );
    }
    validate_cex_replay_features_v4(
        &CexReplaySnapshotV4 {
            schema_version: CEX_REPLAY_SNAPSHOT_SCHEMA_V4.to_string(),
            venue: snapshot.venue.clone(),
            instrument_type: snapshot.instrument_type.clone(),
            symbol: snapshot.symbol.clone(),
            replay_clock: snapshot.replay_clock.clone(),
            required_modalities: snapshot.required_modalities.clone(),
            source_segments: snapshot.source_segments.clone(),
            first_event_time: snapshot.first_event_time,
            last_event_time: snapshot.last_event_time,
            feature_artifact_sha256: snapshot.feature_artifact_sha256.clone(),
            feature_availability_policy: snapshot.feature_availability_policy.clone(),
            bucket_ms: snapshot.bucket_ms,
            label_horizon_buckets: snapshot.label_horizon_buckets,
            top_depth: snapshot.top_depth,
            instrument_rules: snapshot.instrument_rules.clone(),
            derivatives_reference: snapshot.derivatives_reference.clone(),
        },
        features,
    )
}

fn validate_cex_replay_features_v1(
    snapshot: &CexReplaySnapshotV1,
    features: &FeatureDatasetManifest,
) -> anyhow::Result<()> {
    snapshot.validate()?;
    if features.symbol != snapshot.symbol
        || features.artifact_sha256 != snapshot.feature_artifact_sha256
        || features.label_spec.horizon_buckets != snapshot.label_horizon_buckets
        || features.label_spec.observation_frequency_millis != snapshot.bucket_ms
        || features.time_bounds.first_event_time != snapshot.first_event_time
        || features.time_bounds.last_event_time != snapshot.last_event_time
    {
        bail!("feature lineage does not match the historical CEX replay snapshot");
    }
    let last_label_available_ns = u64::try_from(
        features
            .time_bounds
            .last_label_available_time
            .timestamp_nanos_opt()
            .context("feature label availability is out of range")?,
    )
    .context("feature label availability is out of range")?;
    if last_label_available_ns
        > snapshot
            .source_segments
            .last()
            .expect("validated snapshot has source segments")
            .end_received_at_ns
    {
        bail!("feature label availability is outside the historical CEX replay snapshot");
    }
    let source_key = format!("binance-{}-lob", snapshot.instrument_type);
    let expected_source_revision = source_revision(
        snapshot
            .source_segments
            .iter()
            .map(|segment| segment.content_sha256.as_str()),
    );
    if features.source_revisions.len() != 1
        || features.source_revisions.get(&source_key) != Some(&expected_source_revision)
    {
        bail!("feature source revision does not match the historical CEX replay snapshot");
    }
    let rows = read_feature_rows(features).map_err(anyhow::Error::msg)?;
    if rows
        .iter()
        .any(|row| row.feature_available_time != row.event_time)
    {
        bail!("feature availability does not match the CEX replay decision clock");
    }
    Ok(())
}

pub enum RegisteredResearchDataset {
    Ohlcv(DatasetManifest),
    FeatureMatrix(FeatureDatasetManifest),
    CexReplay {
        admission: CexReplayAdmission,
        features: FeatureDatasetManifest,
    },
}

pub enum CexReplayAdmission {
    V1(Box<CexReplayDatasetManifestV1>),
    V2(Box<CexReplayDatasetManifestV2>),
    V3(Box<CexReplayDatasetManifestV3>),
    V4(Box<CexReplayDatasetManifestV4>),
    V5(Box<CexReplayDatasetManifestV5>),
}

impl CexReplayAdmission {
    fn manifest_id(&self) -> &str {
        match self {
            Self::V1(manifest) => &manifest.manifest_id,
            Self::V2(manifest) => &manifest.manifest_id,
            Self::V3(manifest) => &manifest.manifest_id,
            Self::V4(manifest) => &manifest.manifest_id,
            Self::V5(manifest) => &manifest.manifest_id,
        }
    }

    fn symbol(&self) -> &str {
        match self {
            Self::V1(manifest) => &manifest.snapshot.symbol,
            Self::V2(manifest) => &manifest.snapshot.symbol,
            Self::V3(manifest) => &manifest.snapshot.symbol,
            Self::V4(manifest) => &manifest.snapshot.symbol,
            Self::V5(manifest) => &manifest.snapshot.symbol,
        }
    }
}

impl RegisteredResearchDataset {
    pub fn manifest_id(&self) -> &str {
        match self {
            Self::Ohlcv(manifest) => &manifest.manifest_id,
            Self::FeatureMatrix(manifest) => &manifest.manifest_id,
            Self::CexReplay { admission, .. } => admission.manifest_id(),
        }
    }

    pub fn load_rows(&self, costs: &EvaluationCostsV1) -> anyhow::Result<Vec<ResearchRow>> {
        match self {
            Self::Ohlcv(manifest) => load_research_rows(
                manifest,
                costs.fee_bps,
                costs.funding_bps,
                costs.latency_bps,
            ),
            Self::FeatureMatrix(manifest) => load_feature_research_rows(
                manifest,
                costs.fee_bps,
                costs.funding_bps,
                costs.latency_bps,
                false,
            ),
            Self::CexReplay {
                admission:
                    CexReplayAdmission::V1(_)
                    | CexReplayAdmission::V2(_)
                    | CexReplayAdmission::V3(_)
                    | CexReplayAdmission::V4(_),
                ..
            } => bail!("historical CEX replay evidence is read-only and cannot execute"),
            Self::CexReplay {
                admission: CexReplayAdmission::V5(manifest),
                features,
            } => {
                let funding_bps = cex_snapshot_funding_bps(&manifest.snapshot)?;
                if costs.funding_bps.to_bits() != funding_bps.to_bits() {
                    bail!(
                        "evaluation funding cost does not match the verified CEX replay snapshot"
                    );
                }
                load_feature_research_rows(
                    features,
                    costs.fee_bps,
                    funding_bps,
                    costs.latency_bps,
                    false,
                )
            }
        }
    }

    pub fn evaluation_label_spec(&self) -> anyhow::Result<EvaluationLabelSpecV1> {
        match self {
            Self::Ohlcv(manifest) => Ok(EvaluationLabelSpecV1 {
                horizon_buckets: 1,
                observation_frequency_millis: u64::try_from(manifest.interval.milliseconds())
                    .context("OHLC interval is not a positive millisecond frequency")?,
            }),
            Self::FeatureMatrix(manifest) => {
                // Re-read and validate the content-addressed PIT rows before trusting label facts.
                read_feature_rows(manifest).map_err(anyhow::Error::msg)?;
                Ok(EvaluationLabelSpecV1 {
                    horizon_buckets: manifest.label_spec.horizon_buckets,
                    observation_frequency_millis: manifest.label_spec.observation_frequency_millis,
                })
            }
            Self::CexReplay { features, .. } => {
                read_feature_rows(features).map_err(anyhow::Error::msg)?;
                Ok(EvaluationLabelSpecV1 {
                    horizon_buckets: features.label_spec.horizon_buckets,
                    observation_frequency_millis: features.label_spec.observation_frequency_millis,
                })
            }
        }
    }
}

pub fn cex_snapshot_funding_bps(snapshot: &CexReplaySnapshotV5) -> anyhow::Result<f64> {
    snapshot.validate()?;
    Ok(0.0)
}

#[cfg(test)]
pub fn read_manifest(path: &Path) -> anyhow::Result<DatasetManifest> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read dataset manifest {}", path.display()))?;
    serde_json::from_slice(&bytes).context("dataset manifest is invalid JSON")
}

#[cfg(test)]
pub fn read_registered_manifest(
    store: &AlphaStore,
    path: &Path,
) -> anyhow::Result<DatasetManifest> {
    let manifest = read_manifest(path)?;
    let registered = store
        .get_registry_revision(&manifest.manifest_id)
        .context("dataset manifest is not registered in the control-plane store")?;
    if registered.registry_kind != "dataset"
        || registered.asset_id != manifest.symbol
        || registered.created_at != manifest.created_at
        || registered.payload != serde_json::to_value(&manifest)?
    {
        bail!("dataset manifest does not match its registered immutable revision");
    }
    Ok(manifest)
}

pub fn read_registered_research_dataset(
    store: &AlphaStore,
    path: &Path,
) -> anyhow::Result<RegisteredResearchDataset> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read dataset manifest {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).context("dataset manifest is invalid JSON")?;
    if value
        .get("dataset_kind")
        .and_then(serde_json::Value::as_str)
        == Some(CEX_REPLAY_DATASET_KIND)
    {
        let schema = value
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            .context("CEX replay dataset schema is missing")?;
        let (admission, manifest_id, feature_manifest_id, symbol) = match schema {
            CEX_REPLAY_DATASET_SCHEMA_V1 => {
                let manifest: CexReplayDatasetManifestV1 = serde_json::from_value(value.clone())?;
                manifest.validate()?;
                let fields = (
                    manifest.manifest_id.clone(),
                    manifest.feature_manifest_id.clone(),
                    manifest.snapshot.symbol.clone(),
                );
                (
                    CexReplayAdmission::V1(Box::new(manifest)),
                    fields.0,
                    fields.1,
                    fields.2,
                )
            }
            CEX_REPLAY_DATASET_SCHEMA_V2 => {
                let manifest: CexReplayDatasetManifestV2 = serde_json::from_value(value.clone())?;
                manifest.validate()?;
                let fields = (
                    manifest.manifest_id.clone(),
                    manifest.feature_manifest_id.clone(),
                    manifest.snapshot.symbol.clone(),
                );
                (
                    CexReplayAdmission::V2(Box::new(manifest)),
                    fields.0,
                    fields.1,
                    fields.2,
                )
            }
            CEX_REPLAY_DATASET_SCHEMA_V3 => {
                let manifest: CexReplayDatasetManifestV3 = serde_json::from_value(value.clone())?;
                manifest.validate()?;
                let fields = (
                    manifest.manifest_id.clone(),
                    manifest.feature_manifest_id.clone(),
                    manifest.snapshot.symbol.clone(),
                );
                (
                    CexReplayAdmission::V3(Box::new(manifest)),
                    fields.0,
                    fields.1,
                    fields.2,
                )
            }
            CEX_REPLAY_DATASET_SCHEMA_V4 => {
                let manifest: CexReplayDatasetManifestV4 = serde_json::from_value(value.clone())?;
                manifest.validate()?;
                let fields = (
                    manifest.manifest_id.clone(),
                    manifest.feature_manifest_id.clone(),
                    manifest.snapshot.symbol.clone(),
                );
                (
                    CexReplayAdmission::V4(Box::new(manifest)),
                    fields.0,
                    fields.1,
                    fields.2,
                )
            }
            CEX_REPLAY_DATASET_SCHEMA_V5 => {
                let manifest: CexReplayDatasetManifestV5 = serde_json::from_value(value.clone())?;
                manifest.validate()?;
                let fields = (
                    manifest.manifest_id.clone(),
                    manifest.feature_manifest_id.clone(),
                    manifest.snapshot.symbol.clone(),
                );
                (
                    CexReplayAdmission::V5(Box::new(manifest)),
                    fields.0,
                    fields.1,
                    fields.2,
                )
            }
            _ => bail!("CEX replay dataset schema is unsupported"),
        };
        let feature_revision = store
            .get_registry_revision(&feature_manifest_id)
            .context("CEX replay feature manifest is not registered")?;
        let features: FeatureDatasetManifest =
            serde_json::from_value(feature_revision.payload.clone())?;
        if feature_revision.registry_kind != "dataset"
            || feature_revision.asset_id != features.symbol
            || feature_revision.created_at != features.created_at
            || features.manifest_id != feature_manifest_id
        {
            bail!("CEX replay feature manifest does not match its registered revision");
        }
        match &admission {
            CexReplayAdmission::V1(manifest) => {
                validate_cex_replay_features_v1(&manifest.snapshot, &features)?
            }
            CexReplayAdmission::V2(manifest) => {
                validate_cex_replay_features_v2(&manifest.snapshot, &features)?
            }
            CexReplayAdmission::V3(manifest) => {
                validate_cex_replay_features_v3(&manifest.snapshot, &features)?
            }
            CexReplayAdmission::V4(manifest) => {
                validate_cex_replay_features_v4(&manifest.snapshot, &features)?
            }
            CexReplayAdmission::V5(manifest) => {
                validate_cex_replay_features(&manifest.snapshot, &features)?
            }
        }
        let registered = store
            .get_registry_revision(&manifest_id)
            .context("CEX replay dataset manifest is not registered")?;
        if registered.registry_kind != "dataset"
            || registered.asset_id != symbol
            || registered.parent_revision_id.as_deref() != Some(feature_manifest_id.as_str())
            || registered.payload != value
        {
            bail!("CEX replay dataset manifest does not match its registered revision");
        }
        return Ok(RegisteredResearchDataset::CexReplay {
            admission,
            features,
        });
    }
    let dataset = if value
        .get("dataset_kind")
        .and_then(serde_json::Value::as_str)
        == Some("point_in_time_feature_matrix")
    {
        RegisteredResearchDataset::FeatureMatrix(serde_json::from_value(value.clone())?)
    } else {
        RegisteredResearchDataset::Ohlcv(serde_json::from_value(value.clone())?)
    };
    let (manifest_id, symbol, created_at) = match &dataset {
        RegisteredResearchDataset::Ohlcv(manifest) => (
            manifest.manifest_id.as_str(),
            manifest.symbol.as_str(),
            manifest.created_at,
        ),
        RegisteredResearchDataset::FeatureMatrix(manifest) => (
            manifest.manifest_id.as_str(),
            manifest.symbol.as_str(),
            manifest.created_at,
        ),
        RegisteredResearchDataset::CexReplay {
            admission,
            features,
        } => (
            admission.manifest_id(),
            admission.symbol(),
            features.created_at,
        ),
    };
    let registered = store
        .get_registry_revision(manifest_id)
        .context("dataset manifest is not registered in the control-plane store")?;
    if registered.registry_kind != "dataset"
        || registered.asset_id != symbol
        || registered.created_at != created_at
        || registered.payload != value
    {
        bail!("dataset manifest does not match its registered immutable revision");
    }
    Ok(dataset)
}

pub fn require_promotable_research_dataset(
    store: &AlphaStore,
    manifest_id: &str,
) -> anyhow::Result<()> {
    let revision = store
        .get_registry_revision(manifest_id)
        .context("promotion dataset manifest is not registered")?;
    if revision.registry_kind != "dataset" || revision.revision_id != manifest_id {
        bail!("promotion dataset manifest does not match its registered revision");
    }
    if revision
        .payload
        .get("dataset_kind")
        .and_then(serde_json::Value::as_str)
        == Some(CEX_REPLAY_DATASET_KIND)
        && revision
            .payload
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            != Some(CEX_REPLAY_DATASET_SCHEMA_V5)
    {
        bail!("historical CEX replay evidence is read-only and cannot be promoted");
    }
    Ok(())
}

pub fn load_research_rows(
    manifest: &DatasetManifest,
    fee_bps: f64,
    funding_bps: f64,
    latency_bps: f64,
) -> anyhow::Result<Vec<ResearchRow>> {
    if [fee_bps, funding_bps, latency_bps]
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        bail!("research costs must be finite and non-negative");
    }
    let bytes = std::fs::read(&manifest.artifact_path).with_context(|| {
        format!(
            "failed to read dataset artifact {}",
            manifest.artifact_path.display()
        )
    })?;
    let actual_hash = hex::encode(Sha256::digest(&bytes));
    if actual_hash != manifest.artifact_sha256
        || manifest.manifest_id != format!("dataset-{actual_hash}")
        || manifest
            .artifact_path
            .file_stem()
            .and_then(|name| name.to_str())
            != Some(actual_hash.as_str())
    {
        bail!("dataset artifact does not match its content-addressed manifest");
    }

    let mut trace = Vec::new();
    for (line_number, line) in std::io::BufReader::new(bytes.as_slice())
        .lines()
        .enumerate()
    {
        let line =
            line.with_context(|| format!("failed to read trace line {}", line_number + 1))?;
        let row: OhlcvTraceRow = serde_json::from_str(&line)
            .with_context(|| format!("invalid trace row at line {}", line_number + 1))?;
        trace.push(row);
    }
    if trace.len() != manifest.quality.rows || trace.len() < 3 {
        bail!("trace row count does not match manifest or is too short");
    }
    manifest
        .validate_trace(&trace)
        .map_err(anyhow::Error::msg)?;

    let mut rows = Vec::with_capacity(trace.len() - 2);
    for index in 1..trace.len() - 1 {
        let previous = trace[index - 1].close;
        let current = trace[index].close;
        let next = trace[index + 1].close;
        let signal = current / previous - 1.0;
        let label = next / current - 1.0;
        let bar_return = trace[index].close / trace[index].open - 1.0;
        if !signal.is_finite() || !label.is_finite() || !bar_return.is_finite() {
            bail!("derived research return is not finite");
        }
        rows.push(ResearchRow {
            series_id: 1,
            // The forward label is only observable when the next bar is available.
            available_time: trace[index + 1].available_time,
            signal,
            features: std::collections::BTreeMap::from([
                ("open".to_string(), trace[index].open),
                ("high".to_string(), trace[index].high),
                ("low".to_string(), trace[index].low),
                ("close".to_string(), trace[index].close),
                ("volume".to_string(), trace[index].volume),
                ("bar_return".to_string(), bar_return),
                ("return_1".to_string(), signal),
            ]),
            label,
            fee_bps,
            funding_bps,
            pit_funding: false,
            latency_bps,
        });
    }
    Ok(rows)
}

fn load_feature_research_rows(
    manifest: &FeatureDatasetManifest,
    fee_bps: f64,
    funding_bps: f64,
    latency_bps: f64,
    use_pit_funding: bool,
) -> anyhow::Result<Vec<ResearchRow>> {
    if [fee_bps, funding_bps, latency_bps]
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        bail!("research costs must be finite and non-negative");
    }
    read_feature_rows(manifest)
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .map(|row| {
            let row_funding_bps = use_pit_funding
                .then(|| row.features.get("funding_cost_bps").copied())
                .flatten()
                .unwrap_or(funding_bps);
            if !row_funding_bps.is_finite()
                || row_funding_bps < 0.0
                || row_funding_bps > funding_bps
            {
                bail!("PIT row funding cost exceeds the verified evaluation bound");
            }
            Ok(ResearchRow {
                series_id: row.series_id,
                // Training labels cannot enter the research row before their availability time.
                available_time: row.label_available_time,
                signal: 0.0,
                features: row.features,
                label: row.label,
                fee_bps,
                funding_bps: row_funding_bps,
                pit_funding: use_pit_funding,
                latency_bps,
            })
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn feature_available_times(
    manifest: &FeatureDatasetManifest,
) -> anyhow::Result<Vec<DateTime<Utc>>> {
    Ok(feature_decision_clocks(manifest)?
        .into_iter()
        .map(|row| row.feature_available_time)
        .collect())
}

pub(crate) fn feature_decision_clocks(
    manifest: &FeatureDatasetManifest,
) -> anyhow::Result<Vec<FeatureDecisionClock>> {
    Ok(read_feature_rows(manifest)
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .map(|row| FeatureDecisionClock {
            series_id: row.series_id,
            feature_available_time: row.feature_available_time,
            series_close_time: row.label_available_time,
        })
        .collect())
}

pub(crate) fn ensure_real_directory(path: &Path, label: &str) -> anyhow::Result<()> {
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolve current directory for output safety")?
            .join(path)
    };
    ensure_real_directory_at(&normalize_platform_root_alias(&absolute_path)?, label)
}

fn normalize_platform_root_alias(path: &Path) -> anyhow::Result<PathBuf> {
    let mut normalized = PathBuf::new();
    let mut resolved_root_component = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => normalized.push(".."),
            Component::Normal(component) if !resolved_root_component => {
                let root_component = normalized.join(component);
                normalized = if is_platform_root_alias(component) {
                    match std::fs::symlink_metadata(&root_component) {
                        Ok(metadata) if metadata.file_type().is_symlink() => {
                            std::fs::canonicalize(&root_component).with_context(|| {
                                format!(
                                    "resolve platform root alias for output safety: {}",
                                    root_component.display()
                                )
                            })?
                        }
                        Ok(_) | Err(_) => root_component,
                    }
                } else {
                    root_component
                };
                resolved_root_component = true;
            }
            Component::Normal(component) => {
                normalized.push(component);
                resolved_root_component = true;
            }
        }
    }
    Ok(normalized)
}

fn is_platform_root_alias(component: &std::ffi::OsStr) -> bool {
    // On macOS these are symlinks into /private. They are the only root-level
    // aliases we normalize; every other symlink must fail the directory walk.
    #[cfg(target_os = "macos")]
    {
        component == std::ffi::OsStr::new("tmp") || component == std::ffi::OsStr::new("var")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = component;
        false
    }
}

fn ensure_real_directory_at(path: &Path, label: &str) -> anyhow::Result<()> {
    // Root-level platform aliases (for example macOS /var) are normalized
    // above. Every remaining component is application-controlled and must not
    // resolve through a symlink.
    if let Some(parent) = path.parent().filter(|parent| *parent != path) {
        ensure_real_directory_at(parent, label)?;
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("create {label} directory {}", path.display()))
                }
            }
            std::fs::symlink_metadata(path)
                .with_context(|| format!("inspect {label} directory {}", path.display()))?
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect {label} directory {}", path.display()))
        }
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "{label} directory cannot be a symbolic link: {}",
            path.display()
        );
    }
    if !metadata.is_dir() {
        bail!("{label} path must be a directory: {}", path.display());
    }
    Ok(())
}

pub(crate) fn temporary_output_file(
    path: &Path,
    prefix: &str,
) -> anyhow::Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_real_directory(parent, "temporary output parent")?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempfile_in(parent)
        .with_context(|| format!("create private temporary output in {}", parent.display()))
}

pub(crate) fn ensure_output_path_is_not_symlink(path: &Path, label: &str) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("{label} path cannot be a symbolic link: {}", path.display());
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("inspect {label} path {}", path.display()))
        }
    }
}

pub(crate) fn persist_output_file(
    file: tempfile::NamedTempFile,
    path: &Path,
    label: &str,
) -> anyhow::Result<()> {
    ensure_output_path_is_not_symlink(path, label)?;
    file.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("atomically publish {label} to {}", path.display()))?;
    Ok(())
}

pub fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    write_json_atomic_bounded(path, value, u64::MAX)
}

pub fn write_json_atomic_bounded(
    path: &Path,
    value: &impl serde::Serialize,
    max_bytes: u64,
) -> anyhow::Result<()> {
    let mut temporary = temporary_output_file(path, ".monday-json-")?;
    serde_json::to_writer_pretty(
        BoundedWriter {
            inner: temporary.as_file_mut(),
            remaining: max_bytes,
            max_bytes,
        },
        value,
    )?;
    temporary.as_file().sync_all()?;
    persist_output_file(temporary, path, "JSON evidence")
}

pub fn default_manifest_path(manifest: &DatasetManifest) -> std::path::PathBuf {
    manifest
        .artifact_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{}.manifest.json", manifest.manifest_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};
    use hft_collector::{CandleInterval, DatasetTimeBounds, QualityReport};
    use hft_collector::{DataModality, PointInTimeFeatureRow};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn historical_cex_replay_dataset_cannot_be_promoted() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store
            .put_registry_revision(&RegistryRevision {
                revision_id: "cex-v1".to_string(),
                registry_kind: "dataset".to_string(),
                asset_id: "BTCUSDT".to_string(),
                parent_revision_id: None,
                payload: serde_json::json!({
                    "dataset_kind": CEX_REPLAY_DATASET_KIND,
                    "schema_version": CEX_REPLAY_DATASET_SCHEMA_V1,
                }),
                created_at: Utc::now(),
            })
            .unwrap();

        assert!(require_promotable_research_dataset(&store, "cex-v1").is_err());
    }

    #[test]
    fn historical_spot_cex_replay_v2_and_v3_are_readback_only() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.jsonl");
        let ingestion = Utc::now();
        let source_content_sha256 = "a".repeat(64);
        let source_revision = source_revision([source_content_sha256.as_str()]);
        let rows = (0..3)
            .map(|index| {
                let event_time = ingestion - Duration::seconds(10 - index);
                PointInTimeFeatureRow {
                    series_id: 1,
                    event_time,
                    feature_available_time: event_time,
                    label_available_time: event_time + Duration::seconds(1),
                    ingestion_time: ingestion,
                    symbol: "BTCUSDT".to_string(),
                    source_revisions: BTreeMap::from([(
                        "binance-spot-lob".to_string(),
                        source_revision.clone(),
                    )]),
                    modalities: BTreeSet::from([DataModality::Lob, DataModality::TradeTick]),
                    features: BTreeMap::from([("book_imbalance".to_string(), index as f64)]),
                    label: index as f64 * 0.001,
                }
            })
            .collect::<Vec<_>>();
        let mut bytes = Vec::new();
        for row in &rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(&input, bytes).unwrap();

        let mut store = AlphaStore::open_in_memory().unwrap();
        let features = import_and_register_features(
            &mut store,
            "historical-spot-readback",
            &input,
            &directory.path().join("artifacts"),
        )
        .unwrap();
        let triplet = |data: char, manifest: char| hft_research_manifest::CexArtifactTripletV2 {
            data_sha256: data.to_string().repeat(64),
            manifest_sha256: manifest.to_string().repeat(64),
            success_sha256: data.to_string().repeat(64),
        };
        let first_event_time = rows.first().unwrap().event_time;
        let last_event_time = rows.last().unwrap().event_time;
        let last_label_available_time = rows.last().unwrap().label_available_time;
        let snapshot_v3 = CexReplaySnapshotV3 {
            schema_version: hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V3.to_string(),
            venue: "binance".to_string(),
            instrument_type: "spot".to_string(),
            symbol: "BTCUSDT".to_string(),
            replay_clock: hft_research_manifest::CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
            required_modalities: BTreeSet::from([
                hft_research_manifest::CEX_MODALITY_LOB.to_string(),
                hft_research_manifest::CEX_MODALITY_AGGREGATE_TRADE.to_string(),
            ]),
            source_segments: vec![hft_research_manifest::CexReplaySegmentIdentity {
                content_sha256: source_content_sha256,
                manifest_sha256: "1".repeat(64),
                start_received_at_ns: u64::try_from(
                    (first_event_time - Duration::seconds(1))
                        .timestamp_nanos_opt()
                        .unwrap(),
                )
                .unwrap(),
                end_received_at_ns: u64::try_from(
                    last_label_available_time.timestamp_nanos_opt().unwrap(),
                )
                .unwrap(),
                events: rows.len() as u64,
            }],
            first_event_time,
            last_event_time,
            feature_artifact_sha256: features.artifact_sha256.clone(),
            feature_availability_policy: hft_research_manifest::CEX_FEATURE_AVAILABILITY_POLICY
                .to_string(),
            bucket_ms: 1_000,
            label_horizon_buckets: 1,
            top_depth: 5,
            instrument_rules: hft_research_manifest::CexInstrumentRulesV2 {
                tick_size: "0.1".to_string(),
                step_size: "0.001".to_string(),
                min_notional: "5".to_string(),
                available_at: first_event_time - Duration::seconds(1),
                valid_through: last_label_available_time,
                evidence: vec![triplet('2', '3')],
            },
            fee_schedule: hft_research_manifest::CexFeeScheduleV2 {
                runtime_account_id: "historical/spot".to_string(),
                account_fingerprint: "4".repeat(64),
                maker_buy_fee_bps: "2".to_string(),
                maker_sell_fee_bps: "2".to_string(),
                taker_buy_fee_bps: "5".to_string(),
                taker_sell_fee_bps: "5".to_string(),
                available_at: first_event_time - Duration::seconds(1),
                valid_through: last_label_available_time,
                evidence: vec![triplet('5', '6')],
            },
            derivatives_reference: None,
        };
        let mut snapshot_v2_value = serde_json::to_value(&snapshot_v3).unwrap();
        snapshot_v2_value["schema_version"] =
            serde_json::json!(hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V2);
        snapshot_v2_value["latency_cost"] = serde_json::json!({
            "method": "verified_order_lifecycle_realized_slippage",
            "venue": "binance",
            "symbol": "BTCUSDT",
            "runtime_account_id": "historical/spot",
            "account_fingerprint": "4".repeat(64),
            "evidence": triplet('7', '8'),
            "first_observed_at": first_event_time - Duration::seconds(2),
            "last_observed_at": first_event_time - Duration::seconds(1),
            "available_at": first_event_time - Duration::seconds(1),
            "observations": 2,
            "p50_ns": 1_000_000,
            "p95_ns": 2_000_000,
            "p99_ns": 3_000_000,
            "p50_cost_bps": "0.1",
            "p95_cost_bps": "0.2",
            "p99_cost_bps": "0.3"
        });
        let snapshot_v2: CexReplaySnapshotV2 = serde_json::from_value(snapshot_v2_value).unwrap();
        let snapshot_v2_sha256 = snapshot_v2.sha256();
        let snapshot_v3_sha256 = snapshot_v3.sha256();
        let manifests = [
            serde_json::to_value(CexReplayDatasetManifestV2 {
                dataset_kind: CEX_REPLAY_DATASET_KIND.to_string(),
                schema_version: CEX_REPLAY_DATASET_SCHEMA_V2.to_string(),
                manifest_id: format!("dataset-cex-replay-{snapshot_v2_sha256}"),
                feature_manifest_id: features.manifest_id.clone(),
                snapshot: snapshot_v2,
                snapshot_sha256: snapshot_v2_sha256,
            })
            .unwrap(),
            serde_json::to_value(CexReplayDatasetManifestV3 {
                dataset_kind: CEX_REPLAY_DATASET_KIND.to_string(),
                schema_version: CEX_REPLAY_DATASET_SCHEMA_V3.to_string(),
                manifest_id: format!("dataset-cex-replay-{snapshot_v3_sha256}"),
                feature_manifest_id: features.manifest_id.clone(),
                snapshot: snapshot_v3,
                snapshot_sha256: snapshot_v3_sha256,
            })
            .unwrap(),
        ];

        for (index, manifest) in manifests.into_iter().enumerate() {
            let manifest_id = manifest["manifest_id"].as_str().unwrap().to_string();
            store
                .put_registry_revision(&RegistryRevision {
                    revision_id: manifest_id.clone(),
                    registry_kind: "dataset".to_string(),
                    asset_id: "BTCUSDT".to_string(),
                    parent_revision_id: Some(features.manifest_id.clone()),
                    payload: manifest.clone(),
                    created_at: features.created_at,
                })
                .unwrap();
            let manifest_path = directory.path().join(format!("historical-{index}.json"));
            write_json_atomic(&manifest_path, &manifest).unwrap();

            let registered = read_registered_research_dataset(&store, &manifest_path).unwrap();
            assert_eq!(registered.manifest_id(), manifest_id);
            assert!(registered
                .load_rows(&EvaluationCostsV1 {
                    fee_bps: 2.0,
                    rebate_bps: 0.0,
                    funding_bps: 0.0,
                    latency_bps: 0.0,
                    slippage_bps: 0.0,
                    cross_spread: false,
                    position_notional_usd: 0.0,
                    capacity_depth_levels: 0,
                    max_book_depth_fraction: 0.0,
                })
                .unwrap_err()
                .to_string()
                .contains("read-only and cannot execute"));
            assert!(require_promotable_research_dataset(&store, &manifest_id)
                .unwrap_err()
                .to_string()
                .contains("read-only and cannot be promoted"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_json_atomic_rejects_a_stale_output_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("create JSON output test root");
        let path = root.path().join("evidence.json");
        let protected_target = root.path().join("protected-target");
        std::fs::write(&protected_target, "preserve\n").unwrap();
        symlink(&protected_target, &path).unwrap();

        let error = write_json_atomic(&path, &serde_json::json!({"status": "fresh"}))
            .expect_err("a symlinked JSON output path must be rejected");

        assert!(error.to_string().contains("symbolic link"));
        assert_eq!(
            std::fs::read_to_string(protected_target).unwrap(),
            "preserve\n"
        );
        assert!(std::fs::symlink_metadata(path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn write_json_atomic_bounded_rejects_oversized_output() {
        let root = tempfile::tempdir().expect("create bounded JSON output test root");
        let path = root.path().join("evidence.json");

        let error =
            write_json_atomic_bounded(&path, &serde_json::json!({"status": "too large"}), 8)
                .expect_err("oversized JSON evidence must fail before publication");

        assert!(error.to_string().contains("maximum 8 bytes"));
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn temporary_output_file_rejects_a_symlinked_ancestor_directory() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("create temporary output test root");
        let protected_directory = root.path().join("protected-directory");
        let protected_work_directory = protected_directory.join("work");
        std::fs::create_dir_all(&protected_work_directory).unwrap();
        let linked_parent = root.path().join("linked-parent");
        symlink(&protected_directory, &linked_parent).unwrap();
        let artifact_directory = linked_parent.join("work/artifacts");

        let error = temporary_output_file(
            &artifact_directory.join("execution-evidence.json"),
            ".monday-json-",
        )
        .expect_err("a symlinked output ancestor must be rejected");

        assert!(error.to_string().contains("symbolic link"));
        assert!(std::fs::read_dir(protected_work_directory)
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn platform_root_aliases_are_explicitly_whitelisted() {
        assert!(!is_platform_root_alias(std::ffi::OsStr::new("evil")));

        #[cfg(target_os = "macos")]
        {
            assert!(is_platform_root_alias(std::ffi::OsStr::new("tmp")));
            assert!(is_platform_root_alias(std::ffi::OsStr::new("var")));
        }
    }

    #[test]
    fn registered_multimodal_feature_matrix_loads_without_losing_pit_availability() {
        let directory = std::env::temp_dir().join(format!(
            "alpha-feature-data-{}-{}",
            std::process::id(),
            NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let input = directory.join("input.jsonl");
        let ingestion = Utc::now() - Duration::seconds(1);
        let rows = (0..4)
            .map(|index| {
                let event_time = ingestion - Duration::minutes(10 - index);
                PointInTimeFeatureRow {
                    series_id: 1,
                    event_time,
                    feature_available_time: event_time + Duration::seconds(1),
                    label_available_time: event_time + Duration::minutes(1),
                    ingestion_time: ingestion,
                    symbol: "BTCUSDT".to_string(),
                    source_revisions: BTreeMap::from([
                        ("binance-lob".to_string(), "depth-v1".to_string()),
                        ("ethereum".to_string(), "transfer-v1".to_string()),
                    ]),
                    modalities: BTreeSet::from([DataModality::Lob, DataModality::OnChain]),
                    features: BTreeMap::from([
                        ("funding_cost_bps".to_string(), 0.0),
                        ("lob_imbalance".to_string(), index as f64),
                        ("onchain_flow".to_string(), index as f64 * 2.0),
                    ]),
                    label: index as f64 * 0.001,
                }
            })
            .collect::<Vec<_>>();
        let mut bytes = Vec::new();
        for row in &rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(&input, bytes).unwrap();
        let mut store = AlphaStore::open_in_memory().unwrap();
        let manifest = import_and_register_features(
            &mut store,
            "data-feature-1",
            &input,
            &directory.join("artifacts"),
        )
        .unwrap();
        let manifest_path = directory.join("manifest.json");
        write_json_atomic(&manifest_path, &manifest).unwrap();

        let registered = read_registered_research_dataset(&store, &manifest_path).unwrap();
        assert_eq!(
            registered.evaluation_label_spec().unwrap(),
            EvaluationLabelSpecV1 {
                horizon_buckets: 1,
                observation_frequency_millis: 60_000,
            }
        );
        let loaded = registered
            .load_rows(&EvaluationCostsV1 {
                fee_bps: 1.0,
                rebate_bps: 0.0,
                funding_bps: 2.0,
                latency_bps: 0.5,
                slippage_bps: 0.0,
                cross_spread: false,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            })
            .unwrap();

        assert_eq!(loaded[0].available_time, rows[0].label_available_time);
        assert_eq!(loaded[0].series_id, 1);
        assert_eq!(
            feature_available_times(&manifest).unwrap(),
            rows.iter()
                .map(|row| row.feature_available_time)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            feature_decision_clocks(&manifest).unwrap(),
            rows.iter()
                .map(|row| FeatureDecisionClock {
                    series_id: row.series_id,
                    feature_available_time: row.feature_available_time,
                    series_close_time: row.label_available_time,
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(loaded[0].features["lob_imbalance"], 0.0);
        assert_eq!(loaded[0].features["onchain_flow"], 0.0);
        assert_eq!(loaded[0].funding_bps, 2.0);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn registered_current_cex_replay_v5_loads_as_l2_only_without_pit_funding() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.jsonl");
        let manifest_path = directory.path().join("cex-replay-dataset.json");
        let artifacts = directory.path().join("artifacts");
        let ingestion = Utc::now() - Duration::seconds(1);
        let source_content_sha256 = "a".repeat(64);
        let source_revision = source_revision([source_content_sha256.as_str()]);
        let rows = (0..3)
            .map(|index| {
                let event_time = ingestion - Duration::seconds(3 - index as i64);
                PointInTimeFeatureRow {
                    series_id: 1,
                    event_time,
                    feature_available_time: event_time,
                    label_available_time: event_time + Duration::seconds(1),
                    ingestion_time: ingestion,
                    symbol: "BTCUSDT".to_string(),
                    source_revisions: BTreeMap::from([(
                        "binance-usdm-lob".to_string(),
                        source_revision.clone(),
                    )]),
                    modalities: BTreeSet::from([DataModality::Lob, DataModality::TradeTick]),
                    features: BTreeMap::from([
                        ("ask_depth_top5".to_string(), 10.0 + index as f64),
                        ("bid_depth_top5".to_string(), 9.0 + index as f64),
                        ("book_imbalance".to_string(), index as f64 / 10.0),
                        ("book_imbalance_top5".to_string(), index as f64 / 20.0),
                        ("mid_price".to_string(), 60_000.0 + index as f64),
                        ("near_depth_concentration_skew_top5".to_string(), 0.1),
                        ("spread_bps".to_string(), 1.0),
                        ("vwap_center_deviation_top5_bps".to_string(), 0.5),
                        ("weighted_book_imbalance_top5".to_string(), 0.2),
                    ]),
                    label: index as f64 * 0.001,
                }
            })
            .collect::<Vec<_>>();
        let mut bytes = Vec::new();
        for row in &rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(&input, bytes).unwrap();
        let mut store = AlphaStore::open_in_memory().unwrap();
        let features =
            import_and_register_features(&mut store, "data-cex-v5", &input, &artifacts).unwrap();
        let instrument_rules_evidence = (0..2)
            .map(|index| hft_research_manifest::CexArtifactTripletV2 {
                data_sha256: hex::encode(Sha256::digest(format!(
                    "data-mission-v5-rules-data-{index}"
                ))),
                manifest_sha256: hex::encode(Sha256::digest(format!(
                    "data-mission-v5-rules-manifest-{index}"
                ))),
                success_sha256: hex::encode(Sha256::digest(format!(
                    "data-mission-v5-rules-data-{index}"
                ))),
            })
            .collect::<Vec<_>>();
        let snapshot = CexReplaySnapshotV5 {
            schema_version: hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V5.to_string(),
            venue: "binance".to_string(),
            instrument_type: "usdm".to_string(),
            symbol: "BTCUSDT".to_string(),
            replay_clock: hft_research_manifest::CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
            required_modalities: BTreeSet::from([
                hft_research_manifest::CEX_MODALITY_LOB.to_string(),
                hft_research_manifest::CEX_MODALITY_AGGREGATE_TRADE.to_string(),
            ]),
            source_segments: vec![hft_research_manifest::CexReplaySegmentIdentity {
                content_sha256: source_content_sha256,
                manifest_sha256: "b".repeat(64),
                start_received_at_ns: u64::try_from(
                    (rows.first().unwrap().event_time - Duration::seconds(1))
                        .timestamp_nanos_opt()
                        .unwrap(),
                )
                .unwrap(),
                end_received_at_ns: u64::try_from(
                    rows.last()
                        .unwrap()
                        .label_available_time
                        .timestamp_nanos_opt()
                        .unwrap(),
                )
                .unwrap(),
                events: rows.len() as u64,
            }],
            first_event_time: rows.first().unwrap().event_time,
            last_event_time: rows.last().unwrap().event_time,
            feature_artifact_sha256: features.artifact_sha256.clone(),
            feature_availability_policy: hft_research_manifest::CEX_FEATURE_AVAILABILITY_POLICY
                .to_string(),
            bucket_ms: 1_000,
            label_horizon_buckets: 1,
            top_depth: 5,
            instrument_rules: hft_research_manifest::CexInstrumentRulesV2 {
                tick_size: "0.1".to_string(),
                step_size: "0.001".to_string(),
                min_notional: "5".to_string(),
                available_at: rows.first().unwrap().event_time - Duration::seconds(1),
                valid_through: rows.last().unwrap().label_available_time,
                evidence: instrument_rules_evidence.clone(),
            },
            series: vec![hft_research_manifest::CexReplaySeriesV1 {
                series_id: 1,
                first_event_time: rows.first().unwrap().event_time,
                last_event_time: rows.last().unwrap().event_time,
                instrument_rules_coverage: hft_research_manifest::CexPitSeriesEvidenceV2 {
                    evidence: instrument_rules_evidence,
                    first_available_at: rows.first().unwrap().event_time - Duration::seconds(1),
                    last_available_at: rows.last().unwrap().label_available_time,
                    observations: 2,
                    max_gap_ns: 8_000_000_000,
                },
            }],
        };
        let manifest = admit_cex_replay_dataset(&mut store, &features, &snapshot).unwrap();
        write_json_atomic(&manifest_path, &manifest).unwrap();

        let registered = read_registered_research_dataset(&store, &manifest_path).unwrap();
        let loaded = registered
            .load_rows(&EvaluationCostsV1 {
                fee_bps: 1.0,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.5,
                slippage_bps: 0.0,
                cross_spread: false,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            })
            .unwrap();

        assert_eq!(loaded.len(), rows.len());
        assert_eq!(loaded[0].funding_bps, 0.0);
        assert!(!loaded[0].pit_funding);
        assert!(!loaded[0].features.contains_key("funding_cost_bps"));
    }

    #[test]
    fn current_cex_replay_v5_rejects_series_label_availability_that_stretches_into_gap() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.jsonl");
        let artifacts = directory.path().join("artifacts");
        let ingestion = Utc::now() - Duration::seconds(1);
        let source_content_sha256 = "1".repeat(64);
        let source_revision = source_revision([source_content_sha256.as_str()]);
        let series_one_start = ingestion - Duration::minutes(10);
        let series_two_start = ingestion - Duration::minutes(4);
        let rows = vec![
            PointInTimeFeatureRow {
                series_id: 1,
                event_time: series_one_start,
                feature_available_time: series_one_start,
                label_available_time: series_one_start + Duration::minutes(1),
                ingestion_time: ingestion,
                symbol: "BTCUSDT".to_string(),
                source_revisions: BTreeMap::from([(
                    "binance-usdm-lob".to_string(),
                    source_revision.clone(),
                )]),
                modalities: BTreeSet::from([DataModality::Lob, DataModality::TradeTick]),
                features: BTreeMap::from([
                    ("ask_depth_top5".to_string(), 10.0),
                    ("bid_depth_top5".to_string(), 9.0),
                    ("book_imbalance".to_string(), 0.1),
                    ("book_imbalance_top5".to_string(), 0.05),
                    ("mid_price".to_string(), 60_000.0),
                    ("near_depth_concentration_skew_top5".to_string(), 0.1),
                    ("spread_bps".to_string(), 1.0),
                    ("vwap_center_deviation_top5_bps".to_string(), 0.5),
                    ("weighted_book_imbalance_top5".to_string(), 0.2),
                ]),
                label: 0.001,
            },
            PointInTimeFeatureRow {
                series_id: 1,
                event_time: series_one_start + Duration::minutes(1),
                feature_available_time: series_one_start + Duration::minutes(1),
                label_available_time: series_one_start + Duration::minutes(3),
                ingestion_time: ingestion,
                symbol: "BTCUSDT".to_string(),
                source_revisions: BTreeMap::from([(
                    "binance-usdm-lob".to_string(),
                    source_revision.clone(),
                )]),
                modalities: BTreeSet::from([DataModality::Lob, DataModality::TradeTick]),
                features: BTreeMap::from([
                    ("ask_depth_top5".to_string(), 11.0),
                    ("bid_depth_top5".to_string(), 10.0),
                    ("book_imbalance".to_string(), 0.2),
                    ("book_imbalance_top5".to_string(), 0.1),
                    ("mid_price".to_string(), 60_001.0),
                    ("near_depth_concentration_skew_top5".to_string(), 0.1),
                    ("spread_bps".to_string(), 1.0),
                    ("vwap_center_deviation_top5_bps".to_string(), 0.5),
                    ("weighted_book_imbalance_top5".to_string(), 0.2),
                ]),
                label: 0.002,
            },
            PointInTimeFeatureRow {
                series_id: 2,
                event_time: series_two_start,
                feature_available_time: series_two_start,
                label_available_time: series_two_start + Duration::minutes(1),
                ingestion_time: ingestion,
                symbol: "BTCUSDT".to_string(),
                source_revisions: BTreeMap::from([(
                    "binance-usdm-lob".to_string(),
                    source_revision.clone(),
                )]),
                modalities: BTreeSet::from([DataModality::Lob, DataModality::TradeTick]),
                features: BTreeMap::from([
                    ("ask_depth_top5".to_string(), 12.0),
                    ("bid_depth_top5".to_string(), 11.0),
                    ("book_imbalance".to_string(), 0.3),
                    ("book_imbalance_top5".to_string(), 0.15),
                    ("mid_price".to_string(), 60_002.0),
                    ("near_depth_concentration_skew_top5".to_string(), 0.1),
                    ("spread_bps".to_string(), 1.0),
                    ("vwap_center_deviation_top5_bps".to_string(), 0.5),
                    ("weighted_book_imbalance_top5".to_string(), 0.2),
                ]),
                label: 0.003,
            },
            PointInTimeFeatureRow {
                series_id: 2,
                event_time: series_two_start + Duration::minutes(1),
                feature_available_time: series_two_start + Duration::minutes(1),
                label_available_time: series_two_start + Duration::minutes(2),
                ingestion_time: ingestion,
                symbol: "BTCUSDT".to_string(),
                source_revisions: BTreeMap::from([(
                    "binance-usdm-lob".to_string(),
                    source_revision.clone(),
                )]),
                modalities: BTreeSet::from([DataModality::Lob, DataModality::TradeTick]),
                features: BTreeMap::from([
                    ("ask_depth_top5".to_string(), 13.0),
                    ("bid_depth_top5".to_string(), 12.0),
                    ("book_imbalance".to_string(), 0.4),
                    ("book_imbalance_top5".to_string(), 0.2),
                    ("mid_price".to_string(), 60_003.0),
                    ("near_depth_concentration_skew_top5".to_string(), 0.1),
                    ("spread_bps".to_string(), 1.0),
                    ("vwap_center_deviation_top5_bps".to_string(), 0.5),
                    ("weighted_book_imbalance_top5".to_string(), 0.2),
                ]),
                label: 0.004,
            },
        ];
        let mut bytes = Vec::new();
        for row in &rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(&input, bytes).unwrap();
        let mut store = AlphaStore::open_in_memory().unwrap();
        let error = import_and_register_features(&mut store, "data-cex-v5-gap", &input, &artifacts)
            .unwrap_err();

        assert!(format!("{error:#}").contains("label horizon"));
    }

    fn trace_fixture() -> (std::path::PathBuf, DatasetManifest, Vec<OhlcvTraceRow>) {
        let directory = std::env::temp_dir().join(format!(
            "alpha-harness-data-{}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let created_at = Utc::now();
        let start = created_at - Duration::minutes(5);
        let rows = (0..5)
            .map(|index| {
                let event_time = start + Duration::minutes(index);
                let available_time = event_time + Duration::minutes(1);
                OhlcvTraceRow {
                    event_time,
                    exchange_time: available_time - Duration::milliseconds(1),
                    receive_time: created_at,
                    available_time,
                    ingestion_time: created_at,
                    source: "binance-public".to_string(),
                    schema_version: "binance-kline-v2".to_string(),
                    quality_flags: vec![],
                    symbol: "BTCUSDT".to_string(),
                    interval: CandleInterval::OneMinute,
                    open: 100.0 + index as f64,
                    high: 101.0 + index as f64,
                    low: 99.0 + index as f64,
                    close: 100.0 + index as f64,
                    volume: 1.0,
                }
            })
            .collect::<Vec<_>>();
        let mut bytes = Vec::new();
        for row in &rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        let hash = hex::encode(Sha256::digest(&bytes));
        let artifact_path = directory.join(format!("{hash}.jsonl"));
        std::fs::write(&artifact_path, bytes).unwrap();
        let manifest = DatasetManifest {
            manifest_id: format!("dataset-{hash}"),
            mission_id: "data-1".to_string(),
            source_id: "binance-public".to_string(),
            symbol: "BTCUSDT".to_string(),
            schema_version: "binance-kline-v2".to_string(),
            interval: CandleInterval::OneMinute,
            time_bounds: DatasetTimeBounds {
                first_event_time: rows.first().unwrap().event_time,
                last_event_time: rows.last().unwrap().event_time,
                last_exchange_time: rows.last().unwrap().exchange_time,
                first_receive_time: rows.first().unwrap().receive_time,
                last_receive_time: rows.last().unwrap().receive_time,
                first_available_time: rows.first().unwrap().available_time,
                last_available_time: rows.last().unwrap().available_time,
                first_ingestion_time: rows.first().unwrap().ingestion_time,
                last_ingestion_time: rows.last().unwrap().ingestion_time,
            },
            artifact_path,
            artifact_sha256: hash,
            quality: QualityReport {
                rows: rows.len(),
                parse_failures: 0,
                non_monotonic_events: 0,
                non_finite_values: 0,
                duplicate_timestamps: 0,
                interval_gaps: 0,
                open_or_partial_candles: 0,
                point_in_time_violations: 0,
                invalid_ohlc_rows: 0,
                non_positive_price_rows: 0,
                negative_volume_rows: 0,
                latest_candle_age_millis: 1,
                max_staleness_millis: 120_000,
                stale: false,
            },
            created_at,
        };
        (directory, manifest, rows)
    }

    fn rewrite_trace(
        manifest: &mut DatasetManifest,
        mutate: impl FnOnce(&mut [serde_json::Value]),
    ) {
        let bytes = std::fs::read(&manifest.artifact_path).unwrap();
        let mut rows = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect::<Vec<_>>();
        mutate(&mut rows);

        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, &row).unwrap();
            bytes.push(b'\n');
        }
        let hash = hex::encode(Sha256::digest(&bytes));
        let artifact_path = manifest
            .artifact_path
            .parent()
            .unwrap()
            .join(format!("{hash}.jsonl"));
        std::fs::write(&artifact_path, bytes).unwrap();
        manifest.artifact_path = artifact_path;
        manifest.artifact_sha256 = hash.clone();
        manifest.manifest_id = format!("dataset-{hash}");
    }

    #[test]
    fn labels_use_next_bar_availability_and_manifest_hash_is_enforced() {
        let (directory, manifest, trace) = trace_fixture();
        let rows = load_research_rows(&manifest, 1.0, 0.0, 0.5).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].available_time, trace[2].available_time);
        assert!(rows
            .windows(2)
            .all(|window| window[0].available_time < window[1].available_time));
        assert!(trace.iter().all(|row| {
            row.exchange_time < row.available_time
                && row.available_time <= row.receive_time
                && row.receive_time <= row.ingestion_time
                && row.ingestion_time <= manifest.created_at
        }));
        assert!(trace.first().unwrap().available_time < trace.first().unwrap().receive_time);
        assert_eq!(trace.first().unwrap().receive_time, manifest.created_at);
        assert_eq!(trace.first().unwrap().ingestion_time, manifest.created_at);
        assert!((rows[0].signal - 0.01).abs() < 1e-12);
        assert_eq!(rows[0].features["open"], trace[1].open);
        assert_eq!(rows[0].features["high"], trace[1].high);
        assert_eq!(rows[0].features["low"], trace[1].low);
        assert_eq!(rows[0].features["close"], trace[1].close);
        assert_eq!(rows[0].features["volume"], trace[1].volume);
        assert_eq!(rows[0].features["bar_return"], 0.0);
        assert!((rows[0].features["return_1"] - 0.01).abs() < 1e-12);

        std::fs::write(&manifest.artifact_path, b"tampered").unwrap();
        assert!(load_research_rows(&manifest, 1.0, 0.0, 0.5).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn registered_manifest_rejects_metadata_rebinding() {
        let (directory, manifest, _) = trace_fixture();
        let mut store = AlphaStore::open_in_memory().unwrap();
        store
            .put_registry_revision(&RegistryRevision {
                revision_id: manifest.manifest_id.clone(),
                registry_kind: "dataset".to_string(),
                asset_id: manifest.symbol.clone(),
                parent_revision_id: None,
                payload: serde_json::to_value(&manifest).unwrap(),
                created_at: manifest.created_at,
            })
            .unwrap();
        let path = directory.join("manifest.json");
        let mut rebound = manifest;
        rebound.mission_id = "different-data-mission".to_string();
        write_json_atomic(&path, &rebound).unwrap();

        assert!(read_registered_manifest(&store, &path).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn content_address_requires_the_hashed_artifact_path() {
        let (directory, mut manifest, _) = trace_fixture();
        let renamed = directory.join("renamed.jsonl");
        std::fs::copy(&manifest.artifact_path, &renamed).unwrap();
        manifest.artifact_path = renamed;

        assert!(load_research_rows(&manifest, 0.0, 0.0, 0.0).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn manifest_creation_time_is_bound_to_trace_ingestion_time() {
        let (directory, mut manifest, _) = trace_fixture();
        manifest.created_at += Duration::seconds(1);
        manifest.quality.latest_candle_age_millis += 1_000;

        assert!(load_research_rows(&manifest, 0.0, 0.0, 0.0).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_manifest_or_row_source_mismatches() {
        let (directory, mut manifest, _) = trace_fixture();
        manifest.source_id = "different-source".to_string();
        assert!(load_research_rows(&manifest, 0.0, 0.0, 0.0).is_err());
        std::fs::remove_dir_all(&directory).unwrap();

        let (directory, mut manifest, _) = trace_fixture();
        rewrite_trace(&mut manifest, |rows| {
            rows[0]["source"] = serde_json::json!("different-source");
        });
        assert!(load_research_rows(&manifest, 0.0, 0.0, 0.0).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_manifest_or_row_schema_and_symbol_mismatches() {
        for field in ["schema_version", "symbol"] {
            let (directory, mut manifest, _) = trace_fixture();
            match field {
                "schema_version" => manifest.schema_version = "different-schema".to_string(),
                "symbol" => manifest.symbol = "ETHUSDT".to_string(),
                _ => unreachable!(),
            }
            assert!(load_research_rows(&manifest, 0.0, 0.0, 0.0).is_err());
            std::fs::remove_dir_all(directory).unwrap();

            let (directory, mut manifest, _) = trace_fixture();
            rewrite_trace(&mut manifest, |rows| {
                rows[0][field] = serde_json::json!("different-row-value");
            });
            assert!(load_research_rows(&manifest, 0.0, 0.0, 0.0).is_err());
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn rejects_manifest_or_row_interval_mismatches() {
        let (directory, mut manifest, trace) = trace_fixture();
        rewrite_trace(&mut manifest, |rows| {
            for row in rows {
                row["interval"] = serde_json::json!("1m");
            }
        });

        let mut serialized = serde_json::to_value(&manifest).unwrap();
        serialized["interval"] = serde_json::json!("5m");
        serialized["time_bounds"] = serde_json::json!({
            "first_event_time": trace.first().unwrap().event_time,
            "last_event_time": trace.last().unwrap().event_time,
            "last_exchange_time": trace.last().unwrap().exchange_time,
            "first_receive_time": trace.first().unwrap().receive_time,
            "last_receive_time": trace.last().unwrap().receive_time,
            "first_available_time": trace.first().unwrap().available_time,
            "last_available_time": trace.last().unwrap().available_time,
            "first_ingestion_time": trace.first().unwrap().ingestion_time,
            "last_ingestion_time": trace.last().unwrap().ingestion_time,
        });
        let mismatched: DatasetManifest = serde_json::from_value(serialized).unwrap();
        assert!(load_research_rows(&mismatched, 0.0, 0.0, 0.0).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_manifest_time_bounds_that_do_not_match_rows() {
        let (directory, manifest, trace) = trace_fixture();
        let mut serialized = serde_json::to_value(&manifest).unwrap();
        serialized["interval"] = serde_json::json!("1m");
        serialized["time_bounds"] = serde_json::json!({
            "first_event_time": trace.first().unwrap().event_time,
            "last_event_time": trace.first().unwrap().event_time,
            "last_exchange_time": trace.last().unwrap().exchange_time,
            "first_receive_time": trace.first().unwrap().receive_time,
            "last_receive_time": trace.last().unwrap().receive_time,
            "first_available_time": trace.first().unwrap().available_time,
            "last_available_time": trace.last().unwrap().available_time,
            "first_ingestion_time": trace.first().unwrap().ingestion_time,
            "last_ingestion_time": trace.last().unwrap().ingestion_time,
        });
        let mismatched: DatasetManifest = serde_json::from_value(serialized).unwrap();

        assert!(load_research_rows(&mismatched, 0.0, 0.0, 0.0).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_stale_or_point_in_time_invalid_rows() {
        let (directory, mut manifest, _) = trace_fixture();
        rewrite_trace(&mut manifest, |rows| {
            for row in rows {
                for field in ["event_time", "exchange_time"] {
                    let timestamp = row[field].as_str().unwrap();
                    let timestamp = DateTime::parse_from_rfc3339(timestamp)
                        .unwrap()
                        .with_timezone(&Utc)
                        - Duration::hours(1);
                    row[field] = serde_json::json!(timestamp);
                }
            }
        });
        assert!(load_research_rows(&manifest, 0.0, 0.0, 0.0).is_err());
        std::fs::remove_dir_all(directory).unwrap();

        let (directory, mut manifest, _) = trace_fixture();
        rewrite_trace(&mut manifest, |rows| {
            for row in rows {
                row["available_time"] = row["event_time"].clone();
            }
        });
        assert!(load_research_rows(&manifest, 0.0, 0.0, 0.0).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_invalid_market_data_at_load_time() {
        for mutation in [
            "duplicate",
            "gap",
            "partial",
            "ohlc",
            "price",
            "volume",
            "nonfinite",
        ] {
            let (directory, mut manifest, _) = trace_fixture();
            rewrite_trace(&mut manifest, |rows| match mutation {
                "duplicate" => {
                    let event_time = rows[1]["event_time"].clone();
                    let exchange_time = rows[1]["exchange_time"].clone();
                    rows[2]["event_time"] = event_time;
                    rows[2]["exchange_time"] = exchange_time;
                }
                "gap" => {
                    let event_time = rows[2]["event_time"].as_str().unwrap();
                    let event_time = DateTime::parse_from_rfc3339(event_time)
                        .unwrap()
                        .with_timezone(&Utc)
                        + Duration::minutes(1);
                    rows[2]["event_time"] = serde_json::json!(event_time);
                }
                "partial" => rows[1]["exchange_time"] = rows[1]["event_time"].clone(),
                "ohlc" => rows[1]["high"] = serde_json::json!(1.0),
                "price" => rows[1]["open"] = serde_json::json!(0.0),
                "volume" => rows[1]["volume"] = serde_json::json!(-1.0),
                "nonfinite" => rows[1]["open"] = serde_json::json!("NaN"),
                _ => unreachable!(),
            });
            assert!(
                load_research_rows(&manifest, 0.0, 0.0, 0.0).is_err(),
                "{mutation} mutation was accepted"
            );
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn rejects_finite_prices_that_overflow_derived_returns() {
        let (directory, mut manifest, _) = trace_fixture();
        rewrite_trace(&mut manifest, |rows| {
            for field in ["open", "high", "low", "close"] {
                rows[0][field] = serde_json::json!(1.0e-300);
                rows[1][field] = serde_json::json!(1.0e300);
            }
        });

        assert!(load_research_rows(&manifest, 0.0, 0.0, 0.0).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
