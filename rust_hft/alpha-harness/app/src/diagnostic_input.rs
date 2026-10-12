//! Diagnostic input conversion reuses native replay validation and grants no authority.
use alpha_domain::EvaluationProtocolV1;
use alpha_engine::evaluation::ResearchRow;
use anyhow::{bail, ensure, Context, Result};
use data::binance_usdm_reference::{
    ActivePerpetualContract, CompleteReferenceBatch, MarkIndexFundingObservation,
    OpenInterestObservation,
};
use hft_research_manifest::CEX_DERIVATIVES_MAX_GAP_NS;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize)]
pub struct DiagnosticPitReceipt {
    pub schema: &'static str,
    pub materialization_sha256: String,
    pub native_rows_sha256: String,
    pub reference_data_sha256: Vec<String>,
    pub funding_method: &'static str,
    pub compute_authority_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_series: Option<DiagnosticSeriesSelection>,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticSeriesSelection {
    pub original_series_id: u64,
    pub original_input_rows: usize,
    pub selected_rows: usize,
    pub original_row_range: std::ops::Range<usize>,
    pub original_rows_sha256: String,
}

/// Convert the byte-bound PIT artifact, retaining its original clocks and labels.
/// USD-M support is limited to holdings that cross no observed funding settlement.
pub fn load_pit_rows(
    feature: &Path,
    feature_sha256: &str,
    materialization: &Path,
    materialization_sha256: &str,
    references: &[PathBuf],
    protocol: &EvaluationProtocolV1,
    selected_series_id: Option<u64>,
) -> Result<(Vec<ResearchRow>, DiagnosticPitReceipt)> {
    let feature_bytes = read_bounded(feature, 32 * 1024 * 1024)?;
    let materialization_bytes = read_bounded(materialization, 1024 * 1024)?;
    ensure!(
        digest(&feature_bytes) == feature_sha256,
        "PIT input SHA256 mismatch"
    );
    ensure!(
        digest(&materialization_bytes) == materialization_sha256,
        "materialization SHA256 mismatch"
    );
    // Native import consumes the same private bytes whose identities were checked.
    let frozen = tempfile::tempdir()?;
    let frozen_feature = frozen.path().join("features.jsonl");
    let frozen_materialization = frozen.path().join("materialization.json");
    std::fs::write(&frozen_feature, &feature_bytes)?;
    std::fs::write(&frozen_materialization, &materialization_bytes)?;
    let prepared = crate::mission_render::PreparedCexInputs::load(
        &frozen_feature,
        &frozen_materialization,
        true,
    )?;
    ensure!(
        prepared.feature_sha256() == feature_sha256,
        "PIT input SHA256 mismatch"
    );
    ensure!(
        prepared.materialization_sha256() == materialization_sha256,
        "materialization SHA256 mismatch"
    );
    let m = prepared.materialization();
    let rows = prepared.native_source_rows(protocol)?;
    ensure!(
        m.snapshot_sha256 == m.snapshot.sha256()
            && m.artifact_sha256 == feature_sha256
            && m.symbol == m.snapshot.symbol
            && m.market == m.snapshot.instrument_type
            && m.rows == rows.len(),
        "materialization metadata differs from its native snapshot"
    );
    ensure!(
        protocol.labels.horizon_buckets == m.label_horizon_buckets
            && protocol.labels.observation_frequency_millis == m.bucket_ms,
        "diagnostic label protocol differs from native PIT materialization"
    );
    ensure!(
        protocol.costs.funding_bps == 0.0,
        "PIT diagnostic currently supports verified no-settlement intervals only"
    );
    let (rows, selected_series) = select_series(rows, selected_series_id)?;
    let mut reference_hashes = Vec::new();
    let funding_method = match m.market.as_str() {
        "spot" => {
            ensure!(
                references.is_empty(),
                "Spot diagnostic does not consume USD-M references"
            );
            "spot_not_applicable"
        }
        "usdm" => {
            let expected = &m.snapshot.instrument_rules.evidence;
            ensure!(
                !references.is_empty()
                    && references.len() == expected.len()
                    && references.len() <= 64,
                "exact bound USD-M reference set is required"
            );
            let mut seen = BTreeSet::new();
            let mut marks = Vec::new();
            let mut total = 0_u64;
            for path in references {
                let bytes = read_bounded(path, 32 * 1024 * 1024)?;
                total += bytes.len() as u64;
                ensure!(total <= 32 * 1024 * 1024, "reference byte budget exceeded");
                let hash = digest(&bytes);
                let anchor = expected
                    .iter()
                    .find(|e| e.data_sha256 == hash)
                    .context("reference is not bound by the verified materialization")?;
                ensure!(seen.insert(hash.clone()), "duplicate reference artifact");
                let manifest = read_bounded(&sibling(path, ".manifest.json"), 1024 * 1024)?;
                let success = read_bounded(&sibling(path, "._SUCCESS"), 65)?;
                ensure!(
                    digest(&manifest) == anchor.manifest_sha256
                        && anchor.success_sha256 == hash
                        && success == format!("{hash}\n").as_bytes(),
                    "reference triplet differs from materialization evidence"
                );
                let mut contracts = Vec::new();
                let mut funding = Vec::new();
                let mut interest = Vec::new();
                for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
                    match serde_json::from_slice::<ReferenceRecord>(line)? {
                        ReferenceRecord::Metadata(row) => contracts.push(row),
                        ReferenceRecord::MarkIndexFunding(row) => funding.push(row),
                        ReferenceRecord::OpenInterest(row) => interest.push(row),
                    }
                }
                let batch = CompleteReferenceBatch::new(contracts, funding, interest)?;
                marks.push(
                    batch
                        .mark_index_funding()
                        .iter()
                        .find(|r| r.symbol == m.symbol)
                        .context("reference has no funding observation for the selected symbol")?
                        .clone(),
                );
                reference_hashes.push(hash);
            }
            verify_no_settlement(&rows, &marks)?;
            "bound_pit_next_funding_after_each_holding_end"
        }
        _ => bail!("unsupported PIT market"),
    };
    reference_hashes.sort();
    let receipt = DiagnosticPitReceipt {
        schema: "monday.diagnostic_pit_input.v1",
        materialization_sha256: materialization_sha256.into(),
        native_rows_sha256: digest(&serde_json::to_vec(&rows)?),
        reference_data_sha256: reference_hashes,
        funding_method,
        compute_authority_verified: false,
        selected_series,
    };
    Ok((rows, receipt))
}

fn select_series(
    mut rows: Vec<ResearchRow>,
    selected: Option<u64>,
) -> Result<(Vec<ResearchRow>, Option<DiagnosticSeriesSelection>)> {
    let Some(id) = selected else {
        return Ok((rows, None));
    };
    ensure!(id > 0, "PIT series id must be positive");
    let first = rows
        .iter()
        .position(|row| row.series_id == id)
        .context("requested PIT series is absent")?;
    let end = rows.iter().rposition(|row| row.series_id == id).unwrap() + 1;
    ensure!(
        rows[first..end].iter().all(|row| row.series_id == id),
        "PIT series selection cannot bridge another series"
    );
    let receipt = DiagnosticSeriesSelection {
        original_series_id: id,
        original_input_rows: rows.len(),
        selected_rows: end - first,
        original_row_range: first..end,
        original_rows_sha256: digest(&serde_json::to_vec(&rows[first..end])?),
    };
    rows = rows[first..end].to_vec();
    // A new single-series view has local id1. Preserve original identity in its receipt.
    for row in &mut rows {
        row.series_id = 1;
    }
    Ok((rows, Some(receipt)))
}

#[derive(Deserialize)]
#[serde(tag = "kind", content = "observation", rename_all = "snake_case")]
enum ReferenceRecord {
    Metadata(ActivePerpetualContract),
    MarkIndexFunding(MarkIndexFundingObservation),
    OpenInterest(OpenInterestObservation),
}

fn verify_no_settlement(rows: &[ResearchRow], marks: &[MarkIndexFundingObservation]) -> Result<()> {
    let mut marks = marks.iter().collect::<Vec<_>>();
    marks.sort_by_key(|m| m.received_at_ns);
    ensure!(
        marks
            .windows(2)
            .all(|w| w[0].received_at_ns < w[1].received_at_ns),
        "funding observations have duplicate availability clocks"
    );
    for row in rows {
        let now = u64::try_from(
            row.available_time
                .timestamp_nanos_opt()
                .context("PIT clock overflow")?,
        )?;
        let end = u64::try_from(
            row.label_available_time
                .timestamp_nanos_opt()
                .context("label clock overflow")?,
        )?;
        let index = marks.partition_point(|m| m.received_at_ns <= now);
        let mark = index
            .checked_sub(1)
            .map(|i| marks[i])
            .context("funding observation was unavailable at the decision clock")?;
        ensure!(
            now - mark.received_at_ns <= CEX_DERIVATIVES_MAX_GAP_NS,
            "funding applicability observation is stale"
        );
        let source_clock = mark
            .source_time_ms
            .checked_mul(1_000_000)
            .context("funding source clock overflow")?;
        ensure!(
            source_clock <= now && now - source_clock <= CEX_DERIVATIVES_MAX_GAP_NS,
            "funding source observation is future or stale"
        );
        let settlement = mark
            .next_funding_time_ms
            .checked_mul(1_000_000)
            .context("funding settlement clock overflow")?;
        ensure!(
            settlement > end,
            "holding crosses funding settlement; this diagnostic must stop rather than assign zero"
        );
    }
    Ok(())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= maximum,
        "reference input exceeds byte bound"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission_render::tests::{read_feature_rows, Fixture};
    use alpha_domain::{EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationWalkForwardV1};
    use chrono::TimeDelta;

    fn protocol() -> EvaluationProtocolV1 {
        EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 120,
                validation_rows: 20,
                fold_count: 2,
                purge_rows: 5,
                embargo_rows: 5,
                sealed_holdout_rows: 30,
            },
            EvaluationCostsV1 {
                fee_bps: 5.0,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.5,
                slippage_bps: 2.0,
                cross_spread: true,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            },
            EvaluationLabelSpecV1 {
                horizon_buckets: 5,
                observation_frequency_millis: 1000,
            },
        )
        .unwrap()
    }

    #[test]
    fn pit_conversion_preserves_native_clocks_labels_features_and_byte_binding() {
        let fixture = Fixture::spot(300);
        let expected = read_feature_rows(&fixture.feature_path);
        let input_hash = digest(&std::fs::read(&fixture.feature_path).unwrap());
        let materialization_hash = digest(&std::fs::read(&fixture.materialization_path).unwrap());
        let load = |input_hash: &str, materialization_hash: &str| {
            load_pit_rows(
                &fixture.feature_path,
                input_hash,
                &fixture.materialization_path,
                materialization_hash,
                &[],
                &protocol(),
                None,
            )
        };
        let (rows, receipt) = load(&input_hash, &materialization_hash).unwrap();
        assert_eq!(rows.len(), expected.len());
        for (actual, source) in rows.iter().zip(&expected) {
            assert_eq!(actual.features, source.features);
            assert_eq!(actual.available_time, source.feature_available_time);
            assert_eq!(actual.label_available_time, source.label_available_time);
            assert_eq!(actual.label, source.label);
            assert_eq!(actual.fee_bps, 5.0);
            assert_eq!(actual.funding_bps, 0.0);
            assert_eq!(actual.latency_bps, 0.5);
        }
        assert!(!receipt.compute_authority_verified);
        assert_eq!(receipt.funding_method, "spot_not_applicable");
        assert!(load(&"0".repeat(64), &materialization_hash).is_err());
        assert!(load(&input_hash, &"0".repeat(64)).is_err());
    }

    #[test]
    fn usdm_never_defaults_missing_reference_evidence_to_zero() {
        let fixture = Fixture::new(300);
        let result = load_pit_rows(
            &fixture.feature_path,
            &digest(&std::fs::read(&fixture.feature_path).unwrap()),
            &fixture.materialization_path,
            &digest(&std::fs::read(&fixture.materialization_path).unwrap()),
            &[],
            &protocol(),
            None,
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("exact bound USD-M reference set"));
    }

    #[test]
    fn funding_gate_rejects_unavailable_stale_and_crossed_settlements() {
        let fixture = Fixture::spot(300);
        let (rows, _) = load_pit_rows(
            &fixture.feature_path,
            &digest(&std::fs::read(&fixture.feature_path).unwrap()),
            &fixture.materialization_path,
            &digest(&std::fs::read(&fixture.materialization_path).unwrap()),
            &[],
            &protocol(),
            None,
        )
        .unwrap();
        let row = rows[0].clone();
        let now = row.available_time.timestamp_nanos_opt().unwrap() as u64;
        let end_ms =
            (row.label_available_time.timestamp_nanos_opt().unwrap() as u64).div_ceil(1_000_000);
        let mark: MarkIndexFundingObservation = serde_json::from_value(serde_json::json!({
            "schema":"binance.usdm_reference.v3", "symbol":"BTCUSDT", "mark_price":"100",
            "index_price":"100", "basis":"0", "basis_rate":"0", "last_funding_rate":"0.0001",
            "interest_rate":"0.0001", "next_funding_time_ms":end_ms + 1000,
            "source_time_ms":now / 1_000_000, "received_at_ns":now,
            "source_endpoint":"/fapi/v1/premiumIndex",
        }))
        .unwrap();
        assert!(
            verify_no_settlement(std::slice::from_ref(&row), std::slice::from_ref(&mark)).is_ok()
        );
        let mut future = mark.clone();
        future.received_at_ns += 1;
        assert!(verify_no_settlement(std::slice::from_ref(&row), &[future]).is_err());
        let mut stale = mark.clone();
        stale.received_at_ns -= CEX_DERIVATIVES_MAX_GAP_NS + 1;
        assert!(verify_no_settlement(std::slice::from_ref(&row), &[stale]).is_err());
        let mut crossed = mark.clone();
        crossed.next_funding_time_ms = now / 1_000_000 + 1000;
        assert!(verify_no_settlement(std::slice::from_ref(&row), &[crossed]).is_err());
        let mut stale_source = mark.clone();
        stale_source.source_time_ms -= CEX_DERIVATIVES_MAX_GAP_NS / 1_000_000 + 1;
        assert!(verify_no_settlement(std::slice::from_ref(&row), &[stale_source]).is_err());
        assert!(verify_no_settlement(std::slice::from_ref(&row), &[]).is_err());
        let mut later = row;
        later.available_time += TimeDelta::seconds(2);
        assert!(verify_no_settlement(&[later], &[mark]).is_ok());
    }

    #[test]
    fn explicit_series_selection_preserves_original_identity_and_never_joins_segments() {
        let fixture = Fixture::spot(300);
        let (first, _) = load_pit_rows(
            &fixture.feature_path,
            &digest(&std::fs::read(&fixture.feature_path).unwrap()),
            &fixture.materialization_path,
            &digest(&std::fs::read(&fixture.materialization_path).unwrap()),
            &[],
            &protocol(),
            None,
        )
        .unwrap();
        let mut second = first.clone();
        for row in &mut second {
            row.series_id = 2;
            row.available_time += TimeDelta::seconds(400);
            row.label_available_time += TimeDelta::seconds(400);
        }
        let mut original = first;
        original.extend(second.clone());
        let (selected, receipt) = select_series(original.clone(), Some(2)).unwrap();
        let receipt = receipt.unwrap();
        assert_eq!(receipt.original_series_id, 2);
        assert_eq!(receipt.original_input_rows, 600);
        assert_eq!(receipt.original_row_range, 300..600);
        assert_eq!(
            receipt.original_rows_sha256,
            digest(&serde_json::to_vec(&second).unwrap())
        );
        for (actual, expected) in selected.iter().zip(second) {
            assert_eq!(actual.series_id, 1);
            assert_eq!(actual.available_time, expected.available_time);
            assert_eq!(actual.label_available_time, expected.label_available_time);
            assert_eq!(actual.features, expected.features);
            assert_eq!(actual.label, expected.label);
        }
        assert!(select_series(original.clone(), Some(3)).is_err());
        original[350].series_id = 1;
        assert!(select_series(original, Some(2)).is_err());
    }
}
