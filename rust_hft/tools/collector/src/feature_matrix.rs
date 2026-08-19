use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::BufRead,
    path::{Path, PathBuf},
};

const PIT_FEATURE_MATRIX_SCHEMA_V2: &str = "pit-feature-matrix-v2";
const PIT_FEATURE_MATRIX_SCHEMA_V3: &str = "pit-feature-matrix-v3";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataModality {
    Ohlcv,
    TradeTick,
    Lob,
    Funding,
    OpenInterest,
    OnChain,
    Alternative,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointInTimeFeatureRow {
    pub series_id: u64,
    pub event_time: DateTime<Utc>,
    pub feature_available_time: DateTime<Utc>,
    pub label_available_time: DateTime<Utc>,
    pub ingestion_time: DateTime<Utc>,
    pub symbol: String,
    pub source_revisions: BTreeMap<String, String>,
    pub modalities: BTreeSet<DataModality>,
    pub features: BTreeMap<String, f64>,
    pub label: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureDatasetTimeBounds {
    pub first_event_time: DateTime<Utc>,
    pub last_event_time: DateTime<Utc>,
    pub first_feature_available_time: DateTime<Utc>,
    pub last_feature_available_time: DateTime<Utc>,
    pub first_label_available_time: DateTime<Utc>,
    pub last_label_available_time: DateTime<Utc>,
    pub first_ingestion_time: DateTime<Utc>,
    pub last_ingestion_time: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureLabelSpec {
    pub horizon_buckets: usize,
    pub observation_frequency_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureDatasetManifest {
    pub dataset_kind: String,
    pub manifest_id: String,
    pub mission_id: String,
    pub symbol: String,
    pub schema_version: String,
    pub source_revisions: BTreeMap<String, String>,
    pub modalities: BTreeSet<DataModality>,
    pub feature_names: Vec<String>,
    pub label_spec: FeatureLabelSpec,
    #[serde(default = "default_series_count")]
    pub series_count: usize,
    pub rows: usize,
    pub time_bounds: FeatureDatasetTimeBounds,
    pub artifact_path: PathBuf,
    pub artifact_sha256: String,
    pub created_at: DateTime<Utc>,
}

impl FeatureDatasetManifest {
    pub fn validate_trace(&self, rows: &[PointInTimeFeatureRow]) -> Result<(), String> {
        if self.dataset_kind != "point_in_time_feature_matrix"
            || !matches!(
                self.schema_version.as_str(),
                PIT_FEATURE_MATRIX_SCHEMA_V2 | PIT_FEATURE_MATRIX_SCHEMA_V3
            )
            || self.mission_id.trim().is_empty()
            || self.symbol.trim().is_empty()
            || self.rows != rows.len()
            || self.rows < 3
            || self.series_count == 0
            || (self.schema_version == PIT_FEATURE_MATRIX_SCHEMA_V2 && self.series_count != 1)
        {
            return Err("feature dataset manifest identity is invalid".to_string());
        }
        let facts = validate_rows(rows, self.created_at)?;
        if self.symbol != facts.symbol
            || self.source_revisions != facts.source_revisions
            || self.modalities != facts.modalities
            || self.feature_names != facts.feature_names
            || self.label_spec != facts.label_spec
            || self.series_count != facts.series_count
            || self.time_bounds != facts.time_bounds
        {
            return Err("feature dataset manifest does not match trace facts".to_string());
        }
        Ok(())
    }
}

struct TraceFacts {
    symbol: String,
    source_revisions: BTreeMap<String, String>,
    modalities: BTreeSet<DataModality>,
    feature_names: Vec<String>,
    label_spec: FeatureLabelSpec,
    series_count: usize,
    time_bounds: FeatureDatasetTimeBounds,
}

pub fn import_feature_dataset(
    mission_id: impl Into<String>,
    input: &Path,
    artifact_dir: &Path,
) -> Result<FeatureDatasetManifest, String> {
    let mission_id = mission_id.into();
    if mission_id.trim().is_empty() || artifact_dir.as_os_str().is_empty() {
        return Err("feature data mission and artifact directory are required".to_string());
    }
    let input_bytes =
        std::fs::read(input).map_err(|error| format!("failed to read feature matrix: {error}"))?;
    let rows = parse_feature_rows(&input_bytes, SeriesFieldRequirement::Required)?;
    let created_at = Utc::now();
    let facts = validate_rows(&rows, created_at)?;

    let bytes = input_bytes;
    let hash = hex::encode(Sha256::digest(&bytes));
    std::fs::create_dir_all(artifact_dir)
        .map_err(|error| format!("failed to create feature artifact directory: {error}"))?;
    let artifact_path = artifact_dir.join(format!("{hash}.jsonl"));
    let temporary = artifact_dir.join(format!(".{hash}.tmp"));
    std::fs::write(&temporary, &bytes)
        .map_err(|error| format!("failed to write feature artifact: {error}"))?;
    std::fs::rename(&temporary, &artifact_path)
        .map_err(|error| format!("failed to publish feature artifact: {error}"))?;

    Ok(FeatureDatasetManifest {
        dataset_kind: "point_in_time_feature_matrix".to_string(),
        manifest_id: format!("dataset-{hash}"),
        mission_id,
        symbol: facts.symbol,
        schema_version: PIT_FEATURE_MATRIX_SCHEMA_V3.to_string(),
        source_revisions: facts.source_revisions,
        modalities: facts.modalities,
        feature_names: facts.feature_names,
        label_spec: facts.label_spec,
        series_count: facts.series_count,
        rows: rows.len(),
        time_bounds: facts.time_bounds,
        artifact_path,
        artifact_sha256: hash,
        created_at,
    })
}

pub fn read_feature_rows(
    manifest: &FeatureDatasetManifest,
) -> Result<Vec<PointInTimeFeatureRow>, String> {
    let bytes = std::fs::read(&manifest.artifact_path)
        .map_err(|error| format!("failed to read feature artifact: {error}"))?;
    let hash = hex::encode(Sha256::digest(&bytes));
    if hash != manifest.artifact_sha256
        || manifest.manifest_id != format!("dataset-{hash}")
        || manifest
            .artifact_path
            .file_stem()
            .and_then(|name| name.to_str())
            != Some(hash.as_str())
    {
        return Err("feature artifact does not match its content-addressed manifest".to_string());
    }
    let rows = parse_feature_rows(
        &bytes,
        match manifest.schema_version.as_str() {
            PIT_FEATURE_MATRIX_SCHEMA_V2 => SeriesFieldRequirement::AllowLegacySingleSeries,
            PIT_FEATURE_MATRIX_SCHEMA_V3 => SeriesFieldRequirement::Required,
            _ => return Err("feature dataset manifest identity is invalid".to_string()),
        },
    )?;
    manifest.validate_trace(&rows)?;
    Ok(rows)
}

enum SeriesFieldRequirement {
    Required,
    AllowLegacySingleSeries,
}

fn parse_feature_rows(
    bytes: &[u8],
    series_field_requirement: SeriesFieldRequirement,
) -> Result<Vec<PointInTimeFeatureRow>, String> {
    std::io::BufReader::new(bytes)
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let line =
                line.map_err(|error| format!("feature row {} read failed: {error}", index + 1))?;
            let mut value: serde_json::Value = serde_json::from_str(&line)
                .map_err(|error| format!("feature row {} is invalid: {error}", index + 1))?;
            let has_series_id = value.get("series_id").is_some();
            match series_field_requirement {
                SeriesFieldRequirement::Required if !has_series_id => {
                    return Err(format!("feature row {} is missing series_id", index + 1));
                }
                SeriesFieldRequirement::AllowLegacySingleSeries if !has_series_id => {
                    value
                        .as_object_mut()
                        .expect("feature rows decode as JSON objects")
                        .insert("series_id".to_string(), serde_json::json!(1_u64));
                }
                SeriesFieldRequirement::Required
                | SeriesFieldRequirement::AllowLegacySingleSeries => {}
            }
            serde_json::from_value(value)
                .map_err(|error| format!("feature row {} is invalid: {error}", index + 1))
        })
        .collect()
}

fn validate_rows(
    rows: &[PointInTimeFeatureRow],
    created_at: DateTime<Utc>,
) -> Result<TraceFacts, String> {
    let first = rows
        .first()
        .ok_or_else(|| "feature matrix is empty".to_string())?;
    let last = rows.last().expect("non-empty rows have a last row");
    if rows.len() < 3
        || first.series_id != 1
        || first.symbol.trim().is_empty()
        || first.source_revisions.is_empty()
        || first.modalities.is_empty()
        || first.features.is_empty()
    {
        return Err("feature matrix identity and schema must be non-empty".to_string());
    }
    let feature_names = first.features.keys().cloned().collect::<Vec<_>>();
    if feature_names
        .iter()
        .any(|name| name.trim().is_empty() || name == "signal")
        || first
            .source_revisions
            .iter()
            .any(|(source, revision)| source.trim().is_empty() || revision.trim().is_empty())
    {
        return Err("feature matrix field or source revision is invalid".to_string());
    }
    for row in rows {
        if row.symbol != first.symbol
            || row.series_id == 0
            || row.source_revisions != first.source_revisions
            || row.modalities != first.modalities
            || row.features.keys().cloned().collect::<Vec<_>>() != feature_names
            || row.features.values().any(|value| !value.is_finite())
            || !row.label.is_finite()
            || row.event_time > row.feature_available_time
            || row.feature_available_time > row.label_available_time
            || row.label_available_time > row.ingestion_time
            || row.ingestion_time > created_at
        {
            return Err(
                "feature matrix contains schema drift, non-finite data, or PIT leakage".to_string(),
            );
        }
    }
    let observation_frequency_millis = rows
        .windows(2)
        .find(|window| window[0].series_id == window[1].series_id)
        .map(|window| {
            window[1]
                .event_time
                .signed_duration_since(window[0].event_time)
                .num_milliseconds()
        })
        .filter(|frequency| *frequency > 0)
        .and_then(|frequency| u64::try_from(frequency).ok())
        .ok_or_else(|| "feature matrix observation frequency is invalid".to_string())?;
    for window in rows.windows(2) {
        if window[0].event_time >= window[1].event_time
            || window[0].feature_available_time >= window[1].feature_available_time
            || window[0].label_available_time >= window[1].label_available_time
            || window[0].ingestion_time > window[1].ingestion_time
        {
            return Err("feature matrix times must be strictly ordered".to_string());
        }
        match window[1].series_id {
            id if id == window[0].series_id => {
                if window[1]
                    .event_time
                    .signed_duration_since(window[0].event_time)
                    .num_milliseconds()
                    != observation_frequency_millis as i64
                {
                    return Err("feature matrix observation frequency is not uniform".to_string());
                }
            }
            id if id == window[0].series_id + 1 => {
                if window[0].label_available_time >= window[1].event_time {
                    return Err("feature matrix labels cross a series boundary".to_string());
                }
            }
            _ => {
                return Err("feature matrix series ids must be contiguous".to_string());
            }
        }
    }
    let horizon_millis = first
        .label_available_time
        .signed_duration_since(first.event_time)
        .num_milliseconds();
    if horizon_millis <= 0
        || horizon_millis % observation_frequency_millis as i64 != 0
        || rows.iter().any(|row| {
            row.label_available_time
                .signed_duration_since(row.event_time)
                .num_milliseconds()
                != horizon_millis
        })
    {
        return Err("feature matrix label horizon is invalid or inconsistent".to_string());
    }
    let horizon_buckets = usize::try_from(horizon_millis / observation_frequency_millis as i64)
        .ok()
        .filter(|horizon| *horizon > 0)
        .ok_or_else(|| "feature matrix label horizon is invalid".to_string())?;
    Ok(TraceFacts {
        symbol: first.symbol.clone(),
        source_revisions: first.source_revisions.clone(),
        modalities: first.modalities.clone(),
        feature_names,
        label_spec: FeatureLabelSpec {
            horizon_buckets,
            observation_frequency_millis,
        },
        series_count: usize::try_from(last.series_id)
            .map_err(|_| "feature matrix series count is out of range".to_string())?,
        time_bounds: FeatureDatasetTimeBounds {
            first_event_time: first.event_time,
            last_event_time: last.event_time,
            first_feature_available_time: first.feature_available_time,
            last_feature_available_time: last.feature_available_time,
            first_label_available_time: first.label_available_time,
            last_label_available_time: last.label_available_time,
            first_ingestion_time: first.ingestion_time,
            last_ingestion_time: last.ingestion_time,
        },
    })
}

const fn default_series_count() -> usize {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn rows() -> Vec<PointInTimeFeatureRow> {
        let ingestion = Utc::now() - Duration::seconds(1);
        (0..4)
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
                        ("ethereum".to_string(), "erc20-transfer-v1".to_string()),
                    ]),
                    modalities: BTreeSet::from([
                        DataModality::TradeTick,
                        DataModality::Lob,
                        DataModality::OnChain,
                    ]),
                    features: BTreeMap::from([
                        ("lob_imbalance".to_string(), index as f64),
                        ("onchain_flow".to_string(), -(index as f64)),
                    ]),
                    label: index as f64 * 0.001,
                }
            })
            .collect()
    }

    fn write_input(rows: &[PointInTimeFeatureRow]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pit-feature-input-{}-{}.jsonl",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn imports_content_addressed_multimodal_point_in_time_features() {
        let input = write_input(&rows());
        let output = input.with_extension("artifacts");
        let manifest = import_feature_dataset("data-1", &input, &output).unwrap();
        let loaded = read_feature_rows(&manifest).unwrap();

        assert_eq!(loaded.len(), 4);
        assert_eq!(
            std::fs::read(&input).unwrap(),
            std::fs::read(&manifest.artifact_path).unwrap()
        );
        assert!(manifest.modalities.contains(&DataModality::Lob));
        assert!(manifest.modalities.contains(&DataModality::OnChain));
        assert_eq!(
            manifest.feature_names,
            vec!["lob_imbalance", "onchain_flow"]
        );
        assert_eq!(manifest.schema_version, PIT_FEATURE_MATRIX_SCHEMA_V3);
        assert_eq!(manifest.series_count, 1);
        assert_eq!(manifest.label_spec.observation_frequency_millis, 60_000);
        assert_eq!(manifest.label_spec.horizon_buckets, 1);
        let mut legacy_manifest = serde_json::to_value(&manifest).unwrap();
        legacy_manifest
            .as_object_mut()
            .unwrap()
            .remove("label_spec");
        assert!(serde_json::from_value::<FeatureDatasetManifest>(legacy_manifest).is_err());
        std::fs::remove_file(input).unwrap();
        std::fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn rejects_future_information_and_schema_drift() {
        let mut leaked = rows();
        leaked[1].feature_available_time = leaked[1].label_available_time + Duration::seconds(1);
        assert!(validate_rows(&leaked, Utc::now()).is_err());

        let mut invalid_series = rows();
        invalid_series[0].series_id = 2;
        assert!(validate_rows(&invalid_series, Utc::now()).is_err());

        let mut drifted = rows();
        drifted[1].features.remove("onchain_flow");
        assert!(validate_rows(&drifted, Utc::now()).is_err());

        let mut inconsistent_label_horizon = rows();
        inconsistent_label_horizon[1].label_available_time += Duration::minutes(1);
        assert!(validate_rows(&inconsistent_label_horizon, Utc::now()).is_err());
    }

    #[test]
    fn imports_multiseries_rows_and_rejects_missing_v3_ids() {
        let mut input_rows = rows();
        let series_start = input_rows[2].event_time + Duration::minutes(5);
        input_rows[2].series_id = 2;
        input_rows[2].event_time = series_start;
        input_rows[2].feature_available_time = series_start + Duration::seconds(1);
        input_rows[2].label_available_time = series_start + Duration::minutes(1);
        input_rows[3].series_id = 2;
        input_rows[3].event_time = series_start + Duration::minutes(1);
        input_rows[3].feature_available_time = input_rows[3].event_time + Duration::seconds(1);
        input_rows[3].label_available_time = input_rows[3].event_time + Duration::minutes(1);
        let input = write_input(&input_rows);
        let output = input.with_extension("artifacts");

        let manifest = import_feature_dataset("data-2", &input, &output).unwrap();

        assert_eq!(manifest.series_count, 2);
        assert_eq!(read_feature_rows(&manifest).unwrap(), input_rows);

        let legacy_bytes = std::fs::read_to_string(&input)
            .unwrap()
            .lines()
            .map(|line| {
                let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
                value.as_object_mut().unwrap().remove("series_id");
                serde_json::to_string(&value).unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&input, legacy_bytes).unwrap();
        let error = import_feature_dataset("data-3", &input, &output).unwrap_err();
        assert!(error.contains("missing series_id"));

        std::fs::remove_file(input).unwrap();
        std::fs::remove_dir_all(output).unwrap();
    }
}
