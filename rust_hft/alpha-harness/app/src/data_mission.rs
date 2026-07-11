use alpha_engine::evaluation::ResearchRow;
use alpha_store::{AlphaStore, RegistryRevision};
use anyhow::{bail, Context};
use hft_collector::{acquire_dataset, DataAcquisitionMission, DatasetManifest, OhlcvTraceRow};
use sha2::{Digest, Sha256};
use std::{io::BufRead, path::Path};

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

pub fn read_manifest(path: &Path) -> anyhow::Result<DatasetManifest> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read dataset manifest {}", path.display()))?;
    serde_json::from_slice(&bytes).context("dataset manifest is invalid JSON")
}

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
        if !signal.is_finite() || !label.is_finite() {
            bail!("derived research return is not finite");
        }
        rows.push(ResearchRow {
            // The forward label is only observable when the next bar is available.
            available_time: trace[index + 1].available_time,
            signal,
            label,
            fee_bps,
            funding_bps,
            latency_bps,
        });
    }
    Ok(rows)
}

pub fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
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
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

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
