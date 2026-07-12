use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::BufRead,
    path::{Path, PathBuf},
};

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
    pub rows: usize,
    pub time_bounds: FeatureDatasetTimeBounds,
    pub artifact_path: PathBuf,
    pub artifact_sha256: String,
    pub created_at: DateTime<Utc>,
}

impl FeatureDatasetManifest {
    pub fn validate_trace(&self, rows: &[PointInTimeFeatureRow]) -> Result<(), String> {
        if self.dataset_kind != "point_in_time_feature_matrix"
            || self.schema_version != "pit-feature-matrix-v1"
            || self.mission_id.trim().is_empty()
            || self.symbol.trim().is_empty()
            || self.rows != rows.len()
            || self.rows < 3
        {
            return Err("feature dataset manifest identity is invalid".to_string());
        }
        let facts = validate_rows(rows, self.created_at)?;
        if self.symbol != facts.symbol
            || self.source_revisions != facts.source_revisions
            || self.modalities != facts.modalities
            || self.feature_names != facts.feature_names
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
    let rows = parse_feature_rows(&input_bytes)?;
    let created_at = Utc::now();
    let facts = validate_rows(&rows, created_at)?;

    let mut bytes = Vec::new();
    for row in &rows {
        serde_json::to_writer(&mut bytes, row)
            .map_err(|error| format!("failed to serialize feature row: {error}"))?;
        bytes.push(b'\n');
    }
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
        schema_version: "pit-feature-matrix-v1".to_string(),
        source_revisions: facts.source_revisions,
        modalities: facts.modalities,
        feature_names: facts.feature_names,
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
    let rows = parse_feature_rows(&bytes)?;
    manifest.validate_trace(&rows)?;
    Ok(rows)
}

fn parse_feature_rows(bytes: &[u8]) -> Result<Vec<PointInTimeFeatureRow>, String> {
    std::io::BufReader::new(bytes)
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let line =
                line.map_err(|error| format!("feature row {} read failed: {error}", index + 1))?;
            serde_json::from_str(&line)
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
    if rows.windows(2).any(|window| {
        window[0].event_time >= window[1].event_time
            || window[0].feature_available_time >= window[1].feature_available_time
            || window[0].label_available_time >= window[1].label_available_time
            || window[0].ingestion_time > window[1].ingestion_time
    }) {
        return Err("feature matrix times must be strictly ordered".to_string());
    }
    Ok(TraceFacts {
        symbol: first.symbol.clone(),
        source_revisions: first.source_revisions.clone(),
        modalities: first.modalities.clone(),
        feature_names,
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
        assert!(manifest.modalities.contains(&DataModality::Lob));
        assert!(manifest.modalities.contains(&DataModality::OnChain));
        assert_eq!(
            manifest.feature_names,
            vec!["lob_imbalance", "onchain_flow"]
        );
        std::fs::remove_file(input).unwrap();
        std::fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn rejects_future_information_and_schema_drift() {
        let mut leaked = rows();
        leaked[1].feature_available_time = leaked[1].label_available_time + Duration::seconds(1);
        assert!(validate_rows(&leaked, Utc::now()).is_err());

        let mut drifted = rows();
        drifted[1].features.remove("onchain_flow");
        assert!(validate_rows(&drifted, Utc::now()).is_err());
    }
}
