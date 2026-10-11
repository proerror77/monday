use chrono::{DateTime, Duration, TimeZone, Utc};
use hft_cex_research_input::ohlcv::{
    reject_invalid_quality, trace_facts, validate_symbol, CandleInterval, DatasetManifest,
    OhlcvTraceRow, BINANCE_KLINE_SCHEMA_VERSION, BINANCE_SOURCE_ID,
};
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
    #[serde(alias = "capabilities")]
    pub advertised_capabilities: Vec<SourceCapability>,
    #[serde(default)]
    pub governed_acquisition_capabilities: Vec<SourceCapability>,
}

pub fn source_catalog() -> Vec<SourceDescriptor> {
    vec![
        SourceDescriptor {
            source_id: "binance-public".to_string(),
            venue: "binance".to_string(),
            advertised_capabilities: vec![
                #[cfg(feature = "collector-binance")]
                SourceCapability::Lob,
                #[cfg(feature = "collector-binance")]
                SourceCapability::Trades,
                #[cfg(feature = "collector-binance")]
                SourceCapability::Bbo,
                SourceCapability::Ohlcv,
            ],
            governed_acquisition_capabilities: vec![SourceCapability::Ohlcv],
        },
        #[cfg(feature = "collector-binance-futures")]
        SourceDescriptor {
            source_id: "binance-futures-public".to_string(),
            venue: "binance-futures".to_string(),
            advertised_capabilities: vec![
                SourceCapability::Lob,
                SourceCapability::Trades,
                SourceCapability::Bbo,
                SourceCapability::Funding,
                SourceCapability::OpenInterest,
            ],
            governed_acquisition_capabilities: vec![],
        },
        #[cfg(feature = "collector-bitget")]
        SourceDescriptor {
            source_id: "bitget-public".to_string(),
            venue: "bitget".to_string(),
            advertised_capabilities: vec![
                SourceCapability::Lob,
                SourceCapability::Trades,
                SourceCapability::Bbo,
            ],
            governed_acquisition_capabilities: vec![],
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
        if self.source_id != BINANCE_SOURCE_ID {
            return Err("source is not registered for one-shot acquisition".to_string());
        }
        validate_symbol(&self.symbol)?;
        CandleInterval::try_from(self.interval.as_str())?;
        if !(1..=1_000).contains(&self.limit) {
            return Err("limit must be between 1 and 1000".to_string());
        }
        if self.artifact_dir.as_os_str().is_empty() {
            return Err("artifact directory cannot be empty".to_string());
        }
        if self.quality_requirements.max_parse_failures != 0
            || self.quality_requirements.max_non_monotonic_events != 0
            || self.quality_requirements.max_non_finite_values != 0
        {
            return Err("governed acquisition requires zero-error quality thresholds".to_string());
        }
        Ok(())
    }

    fn candle_interval(&self) -> Result<CandleInterval, String> {
        CandleInterval::try_from(self.interval.as_str())
    }
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
    let interval = mission.candle_interval()?;
    let limit = mission.limit.to_string();
    let request_time = Utc::now();
    let end_time = (request_time
        .timestamp_millis()
        .div_euclid(interval.milliseconds())
        * interval.milliseconds()
        - 1)
    .to_string();
    let response = reqwest::Client::new()
        .get(BINANCE_KLINES_URL)
        .query(&[
            ("symbol", mission.symbol.as_str()),
            ("interval", mission.interval.as_str()),
            ("limit", limit.as_str()),
            ("endTime", end_time.as_str()),
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
    let rows = parse_binance_klines(&payload, &mission.symbol, interval, received_at)?;
    persist_trace(mission, &rows, received_at)
}

fn parse_binance_klines(
    payload: &serde_json::Value,
    symbol: &str,
    interval: CandleInterval,
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
        if close_time
            != open_time + Duration::milliseconds(interval.milliseconds())
                - Duration::milliseconds(1)
            || close_time >= received_at
        {
            return Err("Binance OHLCV row is open or partial".to_string());
        }
        rows.push(OhlcvTraceRow {
            event_time: open_time,
            exchange_time: close_time,
            receive_time: received_at,
            available_time: close_time + Duration::milliseconds(1),
            ingestion_time: received_at,
            source: BINANCE_SOURCE_ID.to_string(),
            schema_version: BINANCE_KLINE_SCHEMA_VERSION.to_string(),
            quality_flags: vec![],
            symbol: symbol.to_string(),
            interval,
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
    let interval = mission.candle_interval()?;
    if rows.iter().any(|row| {
        row.source != mission.source_id
            || row.schema_version != BINANCE_KLINE_SCHEMA_VERSION
            || row.symbol != mission.symbol
            || row.interval != interval
    }) {
        return Err("trace row identity does not match acquisition mission".to_string());
    }
    let (quality, time_bounds) = trace_facts(rows, interval, created_at)?;
    reject_invalid_quality(&quality)?;
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
        schema_version: BINANCE_KLINE_SCHEMA_VERSION.to_string(),
        interval,
        time_bounds,
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

    fn mission(artifact_dir: PathBuf) -> DataAcquisitionMission {
        DataAcquisitionMission {
            mission_id: "data-1".to_string(),
            source_id: "binance-public".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            limit: 10,
            artifact_dir,
            quality_requirements: QualityRequirements {
                max_parse_failures: 0,
                max_non_monotonic_events: 0,
                max_non_finite_values: 0,
            },
        }
    }

    fn trace_row(event_time: DateTime<Utc>, received_at: DateTime<Utc>) -> OhlcvTraceRow {
        OhlcvTraceRow {
            event_time,
            exchange_time: event_time + chrono::Duration::seconds(60)
                - chrono::Duration::milliseconds(1),
            receive_time: received_at,
            available_time: event_time + chrono::Duration::seconds(60),
            ingestion_time: received_at,
            source: "binance-public".to_string(),
            schema_version: "binance-kline-v2".to_string(),
            quality_flags: vec![],
            symbol: "BTCUSDT".to_string(),
            interval: CandleInterval::OneMinute,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 1.0,
        }
    }

    #[test]
    fn catalog_serialization_separates_advertised_from_governed_capabilities() {
        let descriptors = serde_json::to_value(source_catalog()).unwrap();
        let descriptors = descriptors.as_array().unwrap();
        assert!(!descriptors.is_empty());
        let binance = descriptors
            .iter()
            .find(|descriptor| descriptor["source_id"] == "binance-public")
            .expect("governed Binance OHLCV acquisition must always be cataloged");
        assert!(binance["advertised_capabilities"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("Ohlcv")));

        for descriptor in descriptors {
            assert!(descriptor.get("advertised_capabilities").is_some());
            assert!(descriptor.get("capabilities").is_none());
            let governed = descriptor
                .get("governed_acquisition_capabilities")
                .and_then(serde_json::Value::as_array)
                .expect("catalog must state governed acquisition support separately");
            if descriptor["source_id"] == "binance-public" {
                assert_eq!(governed, &[serde_json::json!("Ohlcv")]);
            } else {
                assert!(governed.is_empty());
            }
        }

        let legacy: SourceDescriptor = serde_json::from_value(serde_json::json!({
            "source_id": "legacy",
            "venue": "legacy",
            "capabilities": ["Ohlcv"]
        }))
        .unwrap();
        assert_eq!(
            legacy.advertised_capabilities,
            vec![SourceCapability::Ohlcv]
        );
    }

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
        mission.symbol = "BTCUSDT".to_string();
        mission.quality_requirements.max_non_finite_values = 1;
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
        let rows =
            parse_binance_klines(&payload, "BTCUSDT", CandleInterval::OneMinute, received_at)
                .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].available_time,
            rows[0].exchange_time + Duration::milliseconds(1)
        );
        assert!(rows[0].available_time <= rows[0].receive_time);
        assert_eq!(rows[0].receive_time, received_at);
        assert_eq!(rows[0].ingestion_time, received_at);
        assert_eq!(rows[0].close, 1.5);
    }

    #[test]
    fn rejects_an_open_binance_candle() {
        let received_at = Utc.timestamp_millis_opt(1_700_000_030_000).unwrap();
        let payload = serde_json::json!([[
            1_700_000_000_000_i64,
            "1",
            "2",
            "0.5",
            "1.5",
            "10",
            1_700_000_059_999_i64
        ]]);

        assert!(
            parse_binance_klines(&payload, "BTCUSDT", CandleInterval::OneMinute, received_at)
                .is_err()
        );
    }

    #[test]
    fn rejects_duplicate_gapped_or_stale_traces() {
        let received_at = Utc::now();
        let start = received_at - chrono::Duration::minutes(3);
        let directory = std::env::temp_dir().join(format!(
            "collector-data-truth-{}-{}",
            std::process::id(),
            received_at.timestamp_nanos_opt().unwrap()
        ));
        let mission = mission(directory.clone());

        let duplicate = [trace_row(start, received_at), trace_row(start, received_at)];
        assert!(persist_trace(&mission, &duplicate, received_at).is_err());

        let gap = [
            trace_row(start, received_at),
            trace_row(start + chrono::Duration::minutes(2), received_at),
        ];
        assert!(persist_trace(&mission, &gap, received_at).is_err());

        let stale_start = received_at - chrono::Duration::hours(1);
        let stale = [
            trace_row(stale_start, received_at),
            trace_row(stale_start + chrono::Duration::minutes(1), received_at),
        ];
        assert!(persist_trace(&mission, &stale, received_at).is_err());

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn rejects_invalid_ohlc_prices_volume_and_non_finite_values() {
        let received_at = Utc::now();
        let start = received_at - chrono::Duration::minutes(2);
        let directory = std::env::temp_dir().join(format!(
            "collector-price-truth-{}-{}",
            std::process::id(),
            received_at.timestamp_nanos_opt().unwrap()
        ));
        let mission = mission(directory.clone());
        let valid_second = trace_row(start + chrono::Duration::minutes(1), received_at);

        for invalid in [
            OhlcvTraceRow {
                high: 99.0,
                ..trace_row(start, received_at)
            },
            OhlcvTraceRow {
                open: 0.0,
                ..trace_row(start, received_at)
            },
            OhlcvTraceRow {
                volume: -1.0,
                ..trace_row(start, received_at)
            },
            OhlcvTraceRow {
                close: f64::NAN,
                ..trace_row(start, received_at)
            },
        ] {
            assert!(
                persist_trace(&mission, &[invalid, valid_second.clone()], received_at).is_err()
            );
        }

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn persisted_manifest_carries_interval_bounds_and_typed_quality_facts() {
        let received_at = Utc::now();
        let start = received_at - chrono::Duration::minutes(2);
        let directory = std::env::temp_dir().join(format!(
            "collector-manifest-truth-{}-{}",
            std::process::id(),
            received_at.timestamp_nanos_opt().unwrap()
        ));
        let mission = mission(directory.clone());
        let rows = [
            trace_row(start, received_at),
            trace_row(start + chrono::Duration::minutes(1), received_at),
        ];

        let manifest = persist_trace(&mission, &rows, received_at).unwrap();
        let serialized = serde_json::to_value(&manifest).unwrap();
        assert_eq!(serialized["interval"], "1m");
        assert!(serialized["time_bounds"].is_object());
        assert_eq!(serialized["quality"]["duplicate_timestamps"], 0);
        assert_eq!(serialized["quality"]["interval_gaps"], 0);
        assert_eq!(serialized["quality"]["open_or_partial_candles"], 0);
        assert_eq!(serialized["quality"]["invalid_ohlc_rows"], 0);
        assert_eq!(serialized["quality"]["negative_volume_rows"], 0);
        assert_eq!(serialized["quality"]["stale"], false);
        assert!(serialized["time_bounds"]["first_receive_time"].is_string());
        assert!(serialized["time_bounds"]["last_receive_time"].is_string());
        assert!(serialized["time_bounds"]["first_ingestion_time"].is_string());
        assert!(serialized["time_bounds"]["last_ingestion_time"].is_string());
        assert!(manifest.manifest_id.ends_with(&manifest.artifact_sha256));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quality_reports_non_adjacent_duplicate_timestamps() {
        let received_at = Utc::now();
        let start = received_at - chrono::Duration::minutes(3);
        let rows = [
            trace_row(start, received_at),
            trace_row(start + chrono::Duration::minutes(1), received_at),
            trace_row(start, received_at),
        ];

        let (quality, _) = trace_facts(&rows, CandleInterval::OneMinute, received_at).unwrap();
        assert_eq!(quality.duplicate_timestamps, 1);
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
