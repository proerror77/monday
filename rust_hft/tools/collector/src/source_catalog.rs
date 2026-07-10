use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const BINANCE_KLINES_URL: &str = "https://api.binance.com/api/v3/klines";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceCapability {
    Lob,
    Trades,
    Bbo,
    Ohlcv,
    Funding,
    OpenInterest,
    Listings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDescriptor {
    pub source_id: String,
    pub venue: String,
    pub capabilities: Vec<SourceCapability>,
}

pub fn source_catalog() -> Vec<SourceDescriptor> {
    vec![
        #[cfg(feature = "collector-binance")]
        SourceDescriptor {
            source_id: "binance-public".to_string(),
            venue: "binance".to_string(),
            capabilities: vec![
                SourceCapability::Lob,
                SourceCapability::Trades,
                SourceCapability::Bbo,
                SourceCapability::Ohlcv,
            ],
        },
        #[cfg(feature = "collector-binance-futures")]
        SourceDescriptor {
            source_id: "binance-futures-public".to_string(),
            venue: "binance-futures".to_string(),
            capabilities: vec![
                SourceCapability::Lob,
                SourceCapability::Trades,
                SourceCapability::Bbo,
                SourceCapability::Funding,
                SourceCapability::OpenInterest,
            ],
        },
        #[cfg(feature = "collector-bitget")]
        SourceDescriptor {
            source_id: "bitget-public".to_string(),
            venue: "bitget".to_string(),
            capabilities: vec![
                SourceCapability::Lob,
                SourceCapability::Trades,
                SourceCapability::Bbo,
            ],
        },
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataAcquisitionMission {
    pub mission_id: String,
    pub source_id: String,
    pub symbol: String,
    pub interval: String,
    pub limit: usize,
    pub artifact_dir: PathBuf,
    pub quality_requirements: QualityRequirements,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityRequirements {
    pub max_parse_failures: usize,
    pub max_non_monotonic_events: usize,
    pub max_non_finite_values: usize,
}

impl DataAcquisitionMission {
    pub fn validate(&self) -> Result<(), String> {
        if self.mission_id.trim().is_empty() {
            return Err("data mission id cannot be empty".to_string());
        }
        if self.source_id != "binance-public" {
            return Err("source is not registered for one-shot acquisition".to_string());
        }
        if self.symbol.is_empty()
            || self.symbol.len() > 30
            || !self
                .symbol
                .chars()
                .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
        {
            return Err("symbol must be uppercase ASCII alphanumeric".to_string());
        }
        if !matches!(
            self.interval.as_str(),
            "1m" | "3m" | "5m" | "15m" | "30m" | "1h" | "4h" | "1d"
        ) {
            return Err("interval is not allowed".to_string());
        }
        if !(1..=1_000).contains(&self.limit) {
            return Err("limit must be between 1 and 1000".to_string());
        }
        if self.artifact_dir.as_os_str().is_empty() {
            return Err("artifact directory cannot be empty".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OhlcvTraceRow {
    pub event_time: DateTime<Utc>,
    pub exchange_time: DateTime<Utc>,
    pub receive_time: DateTime<Utc>,
    pub available_time: DateTime<Utc>,
    pub ingestion_time: DateTime<Utc>,
    pub source: String,
    pub schema_version: String,
    pub quality_flags: Vec<String>,
    pub symbol: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityReport {
    pub rows: usize,
    pub parse_failures: usize,
    pub non_monotonic_events: usize,
    pub non_finite_values: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub manifest_id: String,
    pub mission_id: String,
    pub source_id: String,
    pub symbol: String,
    pub schema_version: String,
    pub artifact_path: PathBuf,
    pub artifact_sha256: String,
    pub quality: QualityReport,
    pub created_at: DateTime<Utc>,
}

pub async fn acquire_dataset(mission: &DataAcquisitionMission) -> Result<DatasetManifest, String> {
    mission.validate()?;
    match acquire_dataset_inner(mission).await {
        Ok(manifest) => Ok(manifest),
        Err(error) => {
            let failure_path = persist_failure(mission, &error)?;
            Err(format!(
                "{error}; failure artifact: {}",
                failure_path.display()
            ))
        }
    }
}

async fn acquire_dataset_inner(
    mission: &DataAcquisitionMission,
) -> Result<DatasetManifest, String> {
    let limit = mission.limit.to_string();
    let response = reqwest::Client::new()
        .get(BINANCE_KLINES_URL)
        .query(&[
            ("symbol", mission.symbol.as_str()),
            ("interval", mission.interval.as_str()),
            ("limit", limit.as_str()),
        ])
        .send()
        .await
        .map_err(|error| format!("Binance OHLCV request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Binance OHLCV request returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let received_at = Utc::now();
    let payload: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("Binance OHLCV response is invalid JSON: {error}"))?;
    let rows = parse_binance_klines(&payload, &mission.symbol, received_at)?;
    persist_trace(mission, &rows, received_at)
}

fn parse_binance_klines(
    payload: &serde_json::Value,
    symbol: &str,
    received_at: DateTime<Utc>,
) -> Result<Vec<OhlcvTraceRow>, String> {
    let values = payload
        .as_array()
        .ok_or_else(|| "Binance OHLCV payload must be an array".to_string())?;
    let mut rows = Vec::with_capacity(values.len());
    for value in values {
        let fields = value
            .as_array()
            .ok_or_else(|| "Binance OHLCV row must be an array".to_string())?;
        if fields.len() < 7 {
            return Err("Binance OHLCV row is truncated".to_string());
        }
        let open_time = timestamp(fields.first())?;
        let close_time = timestamp(fields.get(6))?;
        rows.push(OhlcvTraceRow {
            event_time: open_time,
            exchange_time: close_time,
            receive_time: received_at,
            available_time: received_at,
            ingestion_time: received_at,
            source: "binance-public".to_string(),
            schema_version: "binance-kline-v1".to_string(),
            quality_flags: vec![],
            symbol: symbol.to_string(),
            open: number(fields.get(1))?,
            high: number(fields.get(2))?,
            low: number(fields.get(3))?,
            close: number(fields.get(4))?,
            volume: number(fields.get(5))?,
        });
    }
    Ok(rows)
}

fn persist_trace(
    mission: &DataAcquisitionMission,
    rows: &[OhlcvTraceRow],
    created_at: DateTime<Utc>,
) -> Result<DatasetManifest, String> {
    if rows.is_empty() {
        return Err("acquisition returned no rows".to_string());
    }
    let non_monotonic_events = rows
        .windows(2)
        .filter(|window| window[0].event_time > window[1].event_time)
        .count();
    let non_finite_values = rows
        .iter()
        .filter(|row| {
            [row.open, row.high, row.low, row.close, row.volume]
                .iter()
                .any(|value| !value.is_finite())
        })
        .count();
    let quality = QualityReport {
        rows: rows.len(),
        parse_failures: 0,
        non_monotonic_events,
        non_finite_values,
    };
    if quality.parse_failures > mission.quality_requirements.max_parse_failures
        || quality.non_monotonic_events > mission.quality_requirements.max_non_monotonic_events
        || quality.non_finite_values > mission.quality_requirements.max_non_finite_values
    {
        return Err("acquired trace failed quality validation".to_string());
    }
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row)
            .map_err(|error| format!("failed to serialize trace row: {error}"))?;
        bytes.push(b'\n');
    }
    let hash = hex::encode(Sha256::digest(&bytes));
    std::fs::create_dir_all(&mission.artifact_dir)
        .map_err(|error| format!("failed to create artifact directory: {error}"))?;
    let artifact_path = mission.artifact_dir.join(format!("{hash}.jsonl"));
    let temporary = mission.artifact_dir.join(format!(".{hash}.tmp"));
    std::fs::write(&temporary, &bytes)
        .map_err(|error| format!("failed to write trace artifact: {error}"))?;
    std::fs::rename(&temporary, &artifact_path)
        .map_err(|error| format!("failed to publish trace artifact: {error}"))?;
    Ok(DatasetManifest {
        manifest_id: format!("dataset-{hash}"),
        mission_id: mission.mission_id.clone(),
        source_id: mission.source_id.clone(),
        symbol: mission.symbol.clone(),
        schema_version: "binance-kline-v1".to_string(),
        artifact_path,
        artifact_sha256: hash,
        quality,
        created_at,
    })
}

fn persist_failure(mission: &DataAcquisitionMission, error: &str) -> Result<PathBuf, String> {
    #[derive(Serialize)]
    struct FailureArtifact<'a> {
        mission_id: &'a str,
        source_id: &'a str,
        symbol: &'a str,
        interval: &'a str,
        error: &'a str,
        failed_at: DateTime<Utc>,
    }

    let artifact = FailureArtifact {
        mission_id: &mission.mission_id,
        source_id: &mission.source_id,
        symbol: &mission.symbol,
        interval: &mission.interval,
        error,
        failed_at: Utc::now(),
    };
    let bytes = serde_json::to_vec(&artifact).map_err(|serialization| {
        format!("failed to serialize failure artifact: {serialization}")
    })?;
    let hash = hex::encode(Sha256::digest(&bytes));
    let directory = mission.artifact_dir.join("failures");
    std::fs::create_dir_all(&directory)
        .map_err(|io| format!("failed to create failure artifact directory: {io}"))?;
    let path = directory.join(format!("{hash}.json"));
    let temporary = directory.join(format!(".{hash}.tmp"));
    std::fs::write(&temporary, bytes)
        .map_err(|io| format!("failed to write failure artifact: {io}"))?;
    std::fs::rename(&temporary, &path)
        .map_err(|io| format!("failed to publish failure artifact: {io}"))?;
    Ok(path)
}

fn timestamp(value: Option<&serde_json::Value>) -> Result<DateTime<Utc>, String> {
    let milliseconds = value
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| "OHLCV timestamp is missing".to_string())?;
    Utc.timestamp_millis_opt(milliseconds)
        .single()
        .ok_or_else(|| "OHLCV timestamp is out of range".to_string())
}

fn number(value: Option<&serde_json::Value>) -> Result<f64, String> {
    let parsed = value
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "OHLCV numeric field is missing".to_string())?
        .parse::<f64>()
        .map_err(|error| format!("OHLCV numeric field is invalid: {error}"))?;
    parsed
        .is_finite()
        .then_some(parsed)
        .ok_or_else(|| "OHLCV numeric field is not finite".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unregistered_or_unsafe_request_values() {
        let mut mission = DataAcquisitionMission {
            mission_id: "data-1".to_string(),
            source_id: "binance-public".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            limit: 10,
            artifact_dir: std::env::temp_dir().join("collector-validation-test"),
            quality_requirements: QualityRequirements {
                max_parse_failures: 0,
                max_non_monotonic_events: 0,
                max_non_finite_values: 0,
            },
        };
        assert!(mission.validate().is_ok());
        mission.symbol = "btc/usdt".to_string();
        assert!(mission.validate().is_err());
    }

    #[test]
    fn parses_point_in_time_kline_rows() {
        let received_at = Utc::now();
        let payload = serde_json::json!([[
            1_700_000_000_000_i64,
            "1",
            "2",
            "0.5",
            "1.5",
            "10",
            1_700_000_059_999_i64
        ]]);
        let rows = parse_binance_klines(&payload, "BTCUSDT", received_at).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].available_time, received_at);
        assert_eq!(rows[0].close, 1.5);
    }

    #[test]
    fn failed_acquisition_writes_a_content_addressed_failure_artifact() {
        let directory = std::env::temp_dir().join(format!(
            "collector-failure-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let mission = DataAcquisitionMission {
            mission_id: "data-failure".to_string(),
            source_id: "binance-public".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            limit: 5,
            artifact_dir: directory.clone(),
            quality_requirements: QualityRequirements {
                max_parse_failures: 0,
                max_non_monotonic_events: 0,
                max_non_finite_values: 0,
            },
        };
        let path = persist_failure(&mission, "bounded test failure").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let hash = hex::encode(Sha256::digest(bytes));
        assert_eq!(path.file_stem().unwrap().to_string_lossy(), hash);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
