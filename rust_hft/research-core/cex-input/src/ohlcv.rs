//! Frozen OHLCV trace contracts and deterministic point-in-time validation.
//! Acquisition and venue HTTP parsing remain owned by the collector.
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf};

pub const BINANCE_SOURCE_ID: &str = "binance-public";
pub const BINANCE_KLINE_SCHEMA_VERSION: &str = "binance-kline-v2";
const MAX_STALENESS_INTERVALS: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandleInterval {
    #[serde(rename = "1m")]
    OneMinute,
    #[serde(rename = "3m")]
    ThreeMinutes,
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "15m")]
    FifteenMinutes,
    #[serde(rename = "30m")]
    ThirtyMinutes,
    #[serde(rename = "1h")]
    OneHour,
    #[serde(rename = "4h")]
    FourHours,
    #[serde(rename = "1d")]
    OneDay,
}

impl CandleInterval {
    pub const fn milliseconds(self) -> i64 {
        match self {
            Self::OneMinute => 60_000,
            Self::ThreeMinutes => 3 * 60_000,
            Self::FiveMinutes => 5 * 60_000,
            Self::FifteenMinutes => 15 * 60_000,
            Self::ThirtyMinutes => 30 * 60_000,
            Self::OneHour => 60 * 60_000,
            Self::FourHours => 4 * 60 * 60_000,
            Self::OneDay => 24 * 60 * 60_000,
        }
    }
}

impl TryFrom<&str> for CandleInterval {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "1m" => Ok(Self::OneMinute),
            "3m" => Ok(Self::ThreeMinutes),
            "5m" => Ok(Self::FiveMinutes),
            "15m" => Ok(Self::FifteenMinutes),
            "30m" => Ok(Self::ThirtyMinutes),
            "1h" => Ok(Self::OneHour),
            "4h" => Ok(Self::FourHours),
            "1d" => Ok(Self::OneDay),
            _ => Err("interval is not allowed".to_string()),
        }
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
    pub interval: CandleInterval,
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
    pub duplicate_timestamps: usize,
    pub interval_gaps: usize,
    pub open_or_partial_candles: usize,
    pub point_in_time_violations: usize,
    pub invalid_ohlc_rows: usize,
    pub non_positive_price_rows: usize,
    pub negative_volume_rows: usize,
    pub latest_candle_age_millis: i64,
    pub max_staleness_millis: i64,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetTimeBounds {
    pub first_event_time: DateTime<Utc>,
    pub last_event_time: DateTime<Utc>,
    pub last_exchange_time: DateTime<Utc>,
    pub first_receive_time: DateTime<Utc>,
    pub last_receive_time: DateTime<Utc>,
    pub first_available_time: DateTime<Utc>,
    pub last_available_time: DateTime<Utc>,
    pub first_ingestion_time: DateTime<Utc>,
    pub last_ingestion_time: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub manifest_id: String,
    pub mission_id: String,
    pub source_id: String,
    pub symbol: String,
    pub schema_version: String,
    pub interval: CandleInterval,
    pub time_bounds: DatasetTimeBounds,
    pub artifact_path: PathBuf,
    pub artifact_sha256: String,
    pub quality: QualityReport,
    pub created_at: DateTime<Utc>,
}

impl DatasetManifest {
    pub fn validate_trace(&self, rows: &[OhlcvTraceRow]) -> Result<(), String> {
        if self.source_id != BINANCE_SOURCE_ID {
            return Err("dataset manifest source is not governed".to_string());
        }
        if self.schema_version != BINANCE_KLINE_SCHEMA_VERSION {
            return Err("dataset manifest schema is not governed".to_string());
        }
        validate_symbol(&self.symbol)?;
        if rows.iter().any(|row| {
            row.source != self.source_id
                || row.schema_version != self.schema_version
                || row.symbol != self.symbol
                || row.interval != self.interval
        }) {
            return Err("trace row identity does not match dataset manifest".to_string());
        }

        let (quality, time_bounds) = trace_facts(rows, self.interval, self.created_at)?;
        reject_invalid_quality(&quality)?;
        if quality != self.quality || time_bounds != self.time_bounds {
            return Err("dataset manifest quality or time bounds do not match trace".to_string());
        }
        Ok(())
    }
}

pub fn trace_facts(
    rows: &[OhlcvTraceRow],
    interval: CandleInterval,
    created_at: DateTime<Utc>,
) -> Result<(QualityReport, DatasetTimeBounds), String> {
    let first = rows
        .first()
        .ok_or_else(|| "acquisition returned no rows".to_string())?;
    let last = rows.last().expect("non-empty rows have a last element");
    let interval_millis = interval.milliseconds();
    let expected_span = Duration::milliseconds(interval_millis) - Duration::milliseconds(1);
    let non_monotonic_events = rows
        .windows(2)
        .filter(|window| window[0].event_time > window[1].event_time)
        .count();
    let mut event_times = HashSet::with_capacity(rows.len());
    let duplicate_timestamps = rows
        .iter()
        .filter(|row| !event_times.insert(row.event_time))
        .count();
    let interval_gaps = rows
        .windows(2)
        .filter(|window| {
            window[1].event_time > window[0].event_time
                && window[1].event_time - window[0].event_time
                    != Duration::milliseconds(interval_millis)
        })
        .count();
    let non_finite_values = rows
        .iter()
        .filter(|row| {
            [row.open, row.high, row.low, row.close, row.volume]
                .iter()
                .any(|value| !value.is_finite())
        })
        .count();
    let open_or_partial_candles = rows
        .iter()
        .filter(|row| {
            row.exchange_time != row.event_time + expected_span
                || row.exchange_time >= row.receive_time
                || row.exchange_time >= created_at
        })
        .count();
    let point_in_time_violations = rows
        .iter()
        .filter(|row| {
            row.exchange_time >= row.available_time
                || row.available_time > row.receive_time
                || row.receive_time > row.ingestion_time
                || row.ingestion_time > created_at
        })
        .count()
        + rows
            .windows(2)
            .filter(|window| {
                window[0].receive_time > window[1].receive_time
                    || window[0].available_time >= window[1].available_time
                    || window[0].ingestion_time > window[1].ingestion_time
            })
            .count()
        + usize::from(last.ingestion_time != created_at);
    let invalid_ohlc_rows = rows
        .iter()
        .filter(|row| {
            row.high < row.low
                || row.high < row.open
                || row.high < row.close
                || row.low > row.open
                || row.low > row.close
        })
        .count();
    let non_positive_price_rows = rows
        .iter()
        .filter(|row| {
            [row.open, row.high, row.low, row.close]
                .iter()
                .any(|price| *price <= 0.0)
        })
        .count();
    let negative_volume_rows = rows.iter().filter(|row| row.volume < 0.0).count();
    let latest_candle_age_millis = (created_at - last.exchange_time).num_milliseconds();
    let max_staleness_millis = interval_millis * MAX_STALENESS_INTERVALS;
    let quality = QualityReport {
        rows: rows.len(),
        parse_failures: 0,
        non_monotonic_events,
        non_finite_values,
        duplicate_timestamps,
        interval_gaps,
        open_or_partial_candles,
        point_in_time_violations,
        invalid_ohlc_rows,
        non_positive_price_rows,
        negative_volume_rows,
        latest_candle_age_millis,
        max_staleness_millis,
        stale: latest_candle_age_millis > max_staleness_millis,
    };
    let time_bounds = DatasetTimeBounds {
        first_event_time: first.event_time,
        last_event_time: last.event_time,
        last_exchange_time: last.exchange_time,
        first_receive_time: first.receive_time,
        last_receive_time: last.receive_time,
        first_available_time: first.available_time,
        last_available_time: last.available_time,
        first_ingestion_time: first.ingestion_time,
        last_ingestion_time: last.ingestion_time,
    };
    Ok((quality, time_bounds))
}

pub fn reject_invalid_quality(quality: &QualityReport) -> Result<(), String> {
    if quality.parse_failures > 0
        || quality.non_monotonic_events > 0
        || quality.non_finite_values > 0
        || quality.duplicate_timestamps > 0
        || quality.interval_gaps > 0
        || quality.open_or_partial_candles > 0
        || quality.point_in_time_violations > 0
        || quality.invalid_ohlc_rows > 0
        || quality.non_positive_price_rows > 0
        || quality.negative_volume_rows > 0
        || quality.stale
    {
        return Err("acquired trace failed governed quality validation".to_string());
    }
    Ok(())
}

pub fn validate_symbol(symbol: &str) -> Result<(), String> {
    if symbol.is_empty()
        || symbol.len() > 30
        || !symbol
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
    {
        return Err("symbol must be uppercase ASCII alphanumeric".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn frozen_trace() -> (DatasetManifest, Vec<OhlcvTraceRow>) {
        let received_at = Utc.timestamp_millis_opt(1_700_000_180_000).unwrap();
        let rows = (0..3)
            .map(|index| {
                let event_time = received_at - Duration::minutes(3 - index);
                OhlcvTraceRow {
                    event_time,
                    exchange_time: event_time + Duration::minutes(1) - Duration::milliseconds(1),
                    receive_time: received_at,
                    available_time: event_time + Duration::minutes(1),
                    ingestion_time: received_at,
                    source: BINANCE_SOURCE_ID.into(),
                    schema_version: BINANCE_KLINE_SCHEMA_VERSION.into(),
                    quality_flags: vec![],
                    symbol: "BTCUSDT".into(),
                    interval: CandleInterval::OneMinute,
                    open: 100.0,
                    high: 101.0,
                    low: 99.0,
                    close: 100.5,
                    volume: 1.0,
                }
            })
            .collect::<Vec<_>>();
        let (quality, time_bounds) =
            trace_facts(&rows, CandleInterval::OneMinute, received_at).unwrap();
        let manifest = DatasetManifest {
            manifest_id: "fixture".into(),
            mission_id: "fixture".into(),
            source_id: BINANCE_SOURCE_ID.into(),
            symbol: "BTCUSDT".into(),
            schema_version: BINANCE_KLINE_SCHEMA_VERSION.into(),
            interval: CandleInterval::OneMinute,
            time_bounds,
            artifact_path: "fixture.jsonl".into(),
            artifact_sha256: "a".repeat(64),
            quality,
            created_at: received_at,
        };
        (manifest, rows)
    }

    #[test]
    fn frozen_manifest_revalidation_rejects_time_gap_identity_and_price_drift() {
        let (manifest, rows) = frozen_trace();
        let frozen: DatasetManifest =
            serde_json::from_slice(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(frozen, manifest);
        frozen.validate_trace(&rows).unwrap();

        let mut duplicate = rows.clone();
        duplicate[1] = duplicate[0].clone();
        let mut gap = rows.clone();
        gap.remove(1);
        let mut future = rows.clone();
        future[0].available_time = future[0].exchange_time;
        let mut identity = rows.clone();
        identity[0].symbol = "ETHUSDT".into();
        let mut price = rows.clone();
        price[0].high = price[0].low - 1.0;
        for invalid in [duplicate, gap, future, identity, price] {
            assert!(frozen.validate_trace(&invalid).is_err());
        }
        let mut false_bounds = frozen.clone();
        false_bounds.time_bounds.last_available_time += Duration::milliseconds(1);
        assert!(false_bounds.validate_trace(&rows).is_err());
        let mut false_quality = frozen;
        false_quality.quality.rows += 1;
        assert!(false_quality.validate_trace(&rows).is_err());
    }
}
