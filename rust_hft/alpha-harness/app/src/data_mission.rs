use alpha_engine::evaluation::ResearchRow;
use alpha_store::{AlphaStore, RegistryRevision};
use anyhow::{bail, Context};
use hft_collector::{acquire_dataset, DataAcquisitionMission, DatasetManifest, OhlcvTraceRow};
use sha2::{Digest, Sha256};
use std::{fs::File, io::BufRead, path::Path};

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
    if manifest.quality.parse_failures > 0
        || manifest.quality.non_monotonic_events > 0
        || manifest.quality.non_finite_values > 0
    {
        bail!("dataset manifest does not satisfy zero-error research quality policy");
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
    {
        bail!("dataset artifact does not match its content-addressed manifest");
    }

    let file = File::open(&manifest.artifact_path)?;
    let mut trace = Vec::new();
    for (line_number, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line =
            line.with_context(|| format!("failed to read trace line {}", line_number + 1))?;
        let row: OhlcvTraceRow = serde_json::from_str(&line)
            .with_context(|| format!("invalid trace row at line {}", line_number + 1))?;
        if row.schema_version != manifest.schema_version || row.symbol != manifest.symbol {
            bail!("trace row does not match dataset manifest");
        }
        trace.push(row);
    }
    if trace.len() != manifest.quality.rows || trace.len() < 3 {
        bail!("trace row count does not match manifest or is too short");
    }
    if trace.windows(2).any(|pair| {
        pair[0].event_time >= pair[1].event_time || pair[0].available_time > pair[1].available_time
    }) {
        bail!("trace event or availability time is not strictly ordered");
    }

    let mut rows = Vec::with_capacity(trace.len() - 2);
    for index in 1..trace.len() - 1 {
        let previous = trace[index - 1].close;
        let current = trace[index].close;
        let next = trace[index + 1].close;
        if previous <= 0.0 || current <= 0.0 || next <= 0.0 {
            bail!("OHLCV close must be positive");
        }
        rows.push(ResearchRow {
            // The forward label is only observable when the next bar is available.
            available_time: trace[index + 1].available_time,
            signal: current / previous - 1.0,
            label: next / current - 1.0,
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
    use chrono::{Duration, Utc};
    use hft_collector::QualityReport;

    fn trace_fixture() -> (std::path::PathBuf, DatasetManifest, Vec<OhlcvTraceRow>) {
        let directory = std::env::temp_dir().join(format!(
            "alpha-harness-data-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let start = Utc::now();
        let rows = (0..5)
            .map(|index| OhlcvTraceRow {
                event_time: start + Duration::minutes(index),
                exchange_time: start + Duration::minutes(index),
                receive_time: start + Duration::minutes(index),
                available_time: start + Duration::minutes(index),
                ingestion_time: start + Duration::minutes(index),
                source: "binance-public".to_string(),
                schema_version: "binance-kline-v1".to_string(),
                quality_flags: vec![],
                symbol: "BTCUSDT".to_string(),
                open: 100.0 + index as f64,
                high: 101.0 + index as f64,
                low: 99.0 + index as f64,
                close: 100.0 + index as f64,
                volume: 1.0,
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
            schema_version: "binance-kline-v1".to_string(),
            artifact_path,
            artifact_sha256: hash,
            quality: QualityReport {
                rows: rows.len(),
                parse_failures: 0,
                non_monotonic_events: 0,
                non_finite_values: 0,
            },
            created_at: start,
        };
        (directory, manifest, rows)
    }

    #[test]
    fn labels_use_next_bar_availability_and_manifest_hash_is_enforced() {
        let (directory, manifest, trace) = trace_fixture();
        let rows = load_research_rows(&manifest, 1.0, 0.0, 0.5).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].available_time, trace[2].available_time);
        assert!((rows[0].signal - 0.01).abs() < 1e-12);

        std::fs::write(&manifest.artifact_path, b"tampered").unwrap();
        assert!(load_research_rows(&manifest, 1.0, 0.0, 0.5).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
