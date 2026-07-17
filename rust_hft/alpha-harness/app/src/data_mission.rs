use alpha_domain::EvaluationLabelSpecV1;
use alpha_engine::evaluation::ResearchRow;
use alpha_store::{AlphaStore, RegistryRevision};
use anyhow::{bail, Context};
use hft_collector::{
    acquire_dataset, import_feature_dataset, read_feature_rows, DataAcquisitionMission,
    DatasetManifest, FeatureDatasetManifest, OhlcvTraceRow,
};
use sha2::{Digest, Sha256};
use std::{
    io::BufRead,
    path::{Component, Path, PathBuf},
};

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

pub enum RegisteredResearchDataset {
    Ohlcv(DatasetManifest),
    FeatureMatrix(FeatureDatasetManifest),
}

impl RegisteredResearchDataset {
    pub fn manifest_id(&self) -> &str {
        match self {
            Self::Ohlcv(manifest) => &manifest.manifest_id,
            Self::FeatureMatrix(manifest) => &manifest.manifest_id,
        }
    }

    pub fn load_rows(
        &self,
        fee_bps: f64,
        funding_bps: f64,
        latency_bps: f64,
    ) -> anyhow::Result<Vec<ResearchRow>> {
        match self {
            Self::Ohlcv(manifest) => {
                load_research_rows(manifest, fee_bps, funding_bps, latency_bps)
            }
            Self::FeatureMatrix(manifest) => {
                load_feature_research_rows(manifest, fee_bps, funding_bps, latency_bps)
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
        }
    }
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
            Ok(ResearchRow {
                // Training labels cannot enter the research row before their availability time.
                available_time: row.label_available_time,
                signal: 0.0,
                features: row.features,
                label: row.label,
                fee_bps,
                funding_bps,
                latency_bps,
            })
        })
        .collect()
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
    let mut temporary = temporary_output_file(path, ".monday-json-")?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)?;
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
        let loaded = registered.load_rows(1.0, 0.0, 0.5).unwrap();

        assert_eq!(loaded[0].available_time, rows[0].label_available_time);
        assert_eq!(loaded[0].features["lob_imbalance"], 0.0);
        assert_eq!(loaded[0].features["onchain_flow"], 0.0);
        std::fs::remove_dir_all(directory).unwrap();
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
