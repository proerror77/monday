//! Atomic publication and strict readback for Binance USD-M reference batches.

use crate::binance_usdm_reference_collector::OFFICIAL_USDM_SOURCE_ORIGIN;
use crate::polymarket_upload::ensure_canonical_directory;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use data::binance_market_tape::{MAX_SOURCE_DELAY_MS, MAX_SOURCE_LEAD_MS};
use data::binance_usdm_reference::{
    ActivePerpetualContract, CompleteReferenceBatch, MarkIndexFundingObservation,
    OpenInterestObservation, ReferenceCoverage, EXCHANGE_INFO_ENDPOINT, OPEN_INTEREST_ENDPOINT,
    PREMIUM_INDEX_ENDPOINT, REFERENCE_SCHEMA, SERVER_TIME_ENDPOINT,
};
use hft_research_manifest::CEX_DERIVATIVES_MAX_GAP_NS;
use rand::random;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MANIFEST_SCHEMA_V1: &str = "binance.usdm_reference_manifest.v1";
const MANIFEST_SCHEMA_V2: &str = "binance.usdm_reference_manifest.v2";
const VENUE: &str = "binance_usdm";
const DATASET: &str = "reference";
const DATA_NAME: &str = "reference.ndjson";
const MAX_DATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SUCCESS_BYTES: u64 = 65;
const HISTORICAL_REFERENCE_SCHEMA_V2: &str = "binance.usdm_reference.v2";
#[derive(Debug, Clone)]
pub struct ReferenceArtifactConfig {
    pub output_root: PathBuf,
    pub observed_at_ns: u64,
    pub max_staleness_ms: u64,
}
#[derive(Debug, Clone)]
pub struct PublishedReferenceArtifact {
    pub data_path: PathBuf,
    pub manifest_path: PathBuf,
    pub success_path: PathBuf,
    pub data_sha256: String,
    pub manifest_sha256: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedReferenceCounts {
    pub metadata: usize,
    pub mark_index_funding: usize,
    pub open_interest: usize,
    pub historical_read_only: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ArtifactCoverage {
    active_contracts: u64,
    metadata_observations: u64,
    mark_index_funding_observations: u64,
    open_interest_observations: u64,
    stale_metadata: u64,
    stale_mark_index_funding: u64,
    stale_open_interest: u64,
    api_error_count: u64,
}

impl From<ReferenceCoverage> for ArtifactCoverage {
    fn from(value: ReferenceCoverage) -> Self {
        Self {
            active_contracts: value.active_contracts,
            metadata_observations: value.metadata_observations,
            mark_index_funding_observations: value.mark_index_funding_observations,
            open_interest_observations: value.open_interest_observations,
            stale_metadata: value.stale_metadata,
            stale_mark_index_funding: value.stale_mark_index_funding,
            stale_open_interest: value.stale_open_interest,
            api_error_count: 0,
        }
    }
}

impl ArtifactCoverage {
    fn is_complete(&self) -> bool {
        // stale_open_interest is deliberately evidence-only: the exchange's
        // openInterest `time` is a per-instrument last-change timestamp that
        // legitimately lags for quiet instruments, so completeness is proven
        // by full per-contract coverage, real-time clocks (metadata/mark), and
        // zero API errors. The count stays published for readback.
        self.active_contracts > 0
            && self.active_contracts == self.metadata_observations
            && self.active_contracts == self.mark_index_funding_observations
            && self.active_contracts == self.open_interest_observations
            && self.stale_metadata == 0
            && self.stale_mark_index_funding == 0
            && self.api_error_count == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ArtifactTimeBounds {
    min_source_time_ms: u64,
    max_source_time_ms: u64,
    min_received_at_ns: u64,
    max_received_at_ns: u64,
}

/// Point-in-time clock evidence for one reference modality. Availability times
/// are row-level `received_at_ns`; event times are exchange `source_time_ms`.
/// Funding/mark-index and open interest are published as separate clocks so a
/// consumer never merges their independent release cadences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ModalityPitClock {
    observations: u64,
    first_available_at_ns: u64,
    last_available_at_ns: u64,
    max_gap_ns: u64,
    first_event_time_ms: u64,
    last_event_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManifestIdentity {
    schema: String,
    venue: String,
    dataset: String,
    data_schema: String,
    format: String,
    source_origin: String,
    source_endpoints: Vec<String>,
    file: String,
    bytes: u64,
    sha256: String,
    rows: u64,
    observed_at_ns: u64,
    max_staleness_ms: u64,
    coverage: ArtifactCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReferenceManifest {
    #[serde(flatten)]
    identity: ManifestIdentity,
    mark_index_funding: ModalityPitClock,
    open_interest: ModalityPitClock,
}

/// Pre-V2 manifest with one merged cross-modality `time_bounds`. Decoded
/// read-only for artifacts published before per-modality PIT clocks; never
/// written by the current publisher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HistoricalManifestV1 {
    #[serde(flatten)]
    identity: ManifestIdentity,
    time_bounds: ArtifactTimeBounds,
}

#[derive(Debug, Deserialize)]
struct ManifestSchemaPeek {
    schema: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "observation", rename_all = "snake_case")]
enum ReferenceRecord {
    Metadata(ActivePerpetualContract),
    MarkIndexFunding(MarkIndexFundingObservation),
    OpenInterest(OpenInterestObservation),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalContractV2 {
    schema: String,
    symbol: String,
    pair: String,
    base_asset: String,
    quote_asset: String,
    margin_asset: String,
    contract_type: String,
    status: String,
    #[serde(rename = "onboard_date_ms")]
    _onboard_date_ms: u64,
    #[serde(rename = "delivery_date_ms")]
    _delivery_date_ms: u64,
    source_time_ms: u64,
    source_clock_received_at_ns: u64,
    received_at_ns: u64,
    source_endpoint: String,
    source_clock_endpoint: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalMarkV2 {
    schema: String,
    symbol: String,
    mark_price: Decimal,
    index_price: Decimal,
    basis: Decimal,
    basis_rate: Decimal,
    #[serde(rename = "last_funding_rate")]
    _last_funding_rate: Decimal,
    #[serde(rename = "interest_rate")]
    _interest_rate: Decimal,
    next_funding_time_ms: u64,
    source_time_ms: u64,
    received_at_ns: u64,
    source_endpoint: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalOpenInterestV2 {
    schema: String,
    symbol: String,
    open_interest: Decimal,
    source_time_ms: u64,
    received_at_ns: u64,
    source_endpoint: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", content = "observation", rename_all = "snake_case")]
enum HistoricalReferenceRecordV2 {
    Metadata(HistoricalContractV2),
    MarkIndexFunding(HistoricalMarkV2),
    OpenInterest(HistoricalOpenInterestV2),
}

#[derive(Debug)]
struct HistoricalBatchV2 {
    contracts: Vec<HistoricalContractV2>,
    marks: Vec<HistoricalMarkV2>,
    open_interest: Vec<HistoricalOpenInterestV2>,
}

impl HistoricalBatchV2 {
    fn new(
        contracts: Vec<HistoricalContractV2>,
        marks: Vec<HistoricalMarkV2>,
        open_interest: Vec<HistoricalOpenInterestV2>,
    ) -> Result<Self> {
        for row in &contracts {
            validate_historical_contract(row)?;
        }
        for row in &marks {
            validate_historical_mark(row)?;
        }
        for row in &open_interest {
            validate_historical_open_interest(row)?;
        }
        let expected = historical_symbols(contracts.iter().map(|row| row.symbol.as_str()))?;
        if expected.is_empty()
            || historical_symbols(marks.iter().map(|row| row.symbol.as_str()))? != expected
            || historical_symbols(open_interest.iter().map(|row| row.symbol.as_str()))? != expected
        {
            bail!("historical reference artifact has incomplete symbol coverage");
        }
        Ok(Self {
            contracts,
            marks,
            open_interest,
        })
    }

    fn coverage(&self, observed_at_ns: u64, max_staleness_ms: u64) -> Result<ArtifactCoverage> {
        Ok(ArtifactCoverage {
            active_contracts: self.contracts.len() as u64,
            metadata_observations: self.contracts.len() as u64,
            mark_index_funding_observations: self.marks.len() as u64,
            open_interest_observations: self.open_interest.len() as u64,
            stale_metadata: historical_stale_count(
                self.contracts.iter().map(|row| row.source_time_ms),
                observed_at_ns,
                max_staleness_ms,
            )?,
            stale_mark_index_funding: historical_stale_count(
                self.marks.iter().map(|row| row.source_time_ms),
                observed_at_ns,
                max_staleness_ms,
            )?,
            stale_open_interest: historical_stale_count(
                self.open_interest.iter().map(|row| row.source_time_ms),
                observed_at_ns,
                max_staleness_ms,
            )?,
            api_error_count: 0,
        })
    }

    fn time_bounds(&self) -> Result<ArtifactTimeBounds> {
        artifact_time_bounds(
            self.contracts
                .iter()
                .map(|row| row.source_time_ms)
                .chain(self.marks.iter().map(|row| row.source_time_ms))
                .chain(self.open_interest.iter().map(|row| row.source_time_ms)),
            self.contracts
                .iter()
                .flat_map(|row| [row.source_clock_received_at_ns, row.received_at_ns])
                .chain(self.marks.iter().map(|row| row.received_at_ns))
                .chain(self.open_interest.iter().map(|row| row.received_at_ns)),
        )
    }

    fn row_count(&self) -> u64 {
        (self.contracts.len() + self.marks.len() + self.open_interest.len()) as u64
    }
}

fn validate_historical_contract(row: &HistoricalContractV2) -> Result<()> {
    validate_historical_symbol(&row.symbol)?;
    validate_historical_receive_clock(row.source_time_ms, row.source_clock_received_at_ns)?;
    validate_historical_receive_clock(row.source_time_ms, row.received_at_ns)?;
    if row.source_clock_received_at_ns > row.received_at_ns {
        bail!("historical reference metadata precedes its source-clock receipt");
    }
    if row.schema != HISTORICAL_REFERENCE_SCHEMA_V2
        || row.source_endpoint != EXCHANGE_INFO_ENDPOINT
        || row.source_clock_endpoint != SERVER_TIME_ENDPOINT
        || row.contract_type != "PERPETUAL"
        || row.status != "TRADING"
        || row.pair.is_empty()
        || row.base_asset.is_empty()
        || row.quote_asset.is_empty()
        || row.margin_asset.is_empty()
    {
        bail!("historical reference metadata identity is invalid");
    }
    Ok(())
}

fn validate_historical_mark(row: &HistoricalMarkV2) -> Result<()> {
    validate_historical_symbol(&row.symbol)?;
    validate_historical_receive_clock(row.source_time_ms, row.received_at_ns)?;
    if row.schema != HISTORICAL_REFERENCE_SCHEMA_V2
        || row.source_endpoint != PREMIUM_INDEX_ENDPOINT
        || row.mark_price <= Decimal::ZERO
        || row.index_price <= Decimal::ZERO
        || row.next_funding_time_ms < row.source_time_ms
    {
        bail!("historical mark/index/funding observation is invalid");
    }
    let basis = row
        .mark_price
        .checked_sub(row.index_price)
        .context("basis overflow")?;
    let basis_rate = basis
        .checked_div(row.index_price)
        .context("basis-rate overflow")?;
    if row.basis != basis || row.basis_rate != basis_rate {
        bail!("historical mark/index/funding derived basis is inconsistent");
    }
    Ok(())
}

fn validate_historical_open_interest(row: &HistoricalOpenInterestV2) -> Result<()> {
    validate_historical_symbol(&row.symbol)?;
    validate_historical_source_not_future(row.source_time_ms, row.received_at_ns)?;
    if row.schema != HISTORICAL_REFERENCE_SCHEMA_V2
        || row.source_endpoint != OPEN_INTEREST_ENDPOINT
        || row.open_interest < Decimal::ZERO
    {
        bail!("historical open-interest observation is invalid");
    }
    Ok(())
}

fn historical_symbols<'a>(symbols: impl Iterator<Item = &'a str>) -> Result<BTreeSet<String>> {
    let mut unique = BTreeSet::new();
    for symbol in symbols {
        if !unique.insert(symbol.to_owned()) {
            bail!("historical reference artifact has duplicate rows");
        }
    }
    Ok(unique)
}

fn historical_stale_count(
    source_times: impl Iterator<Item = u64>,
    observed_at_ns: u64,
    max_staleness_ms: u64,
) -> Result<u64> {
    let observed_at_ms = observed_at_ns / 1_000_000;
    let mut stale = 0;
    for source_time_ms in source_times {
        if source_time_ms > observed_at_ms.saturating_add(MAX_SOURCE_LEAD_MS) {
            bail!("historical reference source clock leads coverage clock");
        }
        if observed_at_ms.saturating_sub(source_time_ms) > max_staleness_ms {
            stale += 1;
        }
    }
    Ok(stale)
}

fn validate_historical_receive_clock(source_time_ms: u64, received_at_ns: u64) -> Result<()> {
    let received_at_ms = received_at_ns / 1_000_000;
    if source_time_ms > received_at_ms.saturating_add(MAX_SOURCE_LEAD_MS)
        || received_at_ms > source_time_ms.saturating_add(MAX_SOURCE_DELAY_MS)
    {
        bail!("historical reference source clock is invalid at receipt");
    }
    Ok(())
}

fn validate_historical_source_not_future(source_time_ms: u64, received_at_ns: u64) -> Result<()> {
    if source_time_ms > (received_at_ns / 1_000_000).saturating_add(MAX_SOURCE_LEAD_MS) {
        bail!("historical reference source clock leads received clock");
    }
    Ok(())
}

fn validate_historical_symbol(symbol: &str) -> Result<()> {
    if symbol.is_empty()
        || symbol.chars().count() > 32
        || !symbol.chars().all(|ch| {
            ch.is_ascii_uppercase()
                || ch.is_ascii_digit()
                || ch == '_'
                || ('\u{3400}'..='\u{4DBF}').contains(&ch)
                || ('\u{4E00}'..='\u{9FFF}').contains(&ch)
        })
    {
        bail!("historical reference symbol identity is invalid");
    }
    Ok(())
}

pub fn publish_reference_batch(
    config: &ReferenceArtifactConfig,
    source_origin: &str,
    batch: &CompleteReferenceBatch,
) -> Result<PublishedReferenceArtifact> {
    publish_reference_batch_inner(config, source_origin, batch, false)
}

pub fn verify_reference_artifact(
    published: &PublishedReferenceArtifact,
    expected_data_sha256: &str,
    expected_manifest_sha256: &str,
) -> Result<CompleteReferenceBatch> {
    let (data, manifest_bytes) =
        read_artifact_trust_anchor(published, expected_data_sha256, expected_manifest_sha256)?;
    match peek_manifest_schema(&manifest_bytes)?.as_str() {
        MANIFEST_SCHEMA_V2 => {
            verify_current_manifest_v2(published, &data, &manifest_bytes, expected_data_sha256)
        }
        MANIFEST_SCHEMA_V1 => bail!(
            "historical v1 reference manifests are read-only evidence and cannot pass the writable verifier"
        ),
        _ => bail!("reference manifest schema is unsupported"),
    }
}

pub fn verify_reference_artifact_read_only_current_batch(
    published: &PublishedReferenceArtifact,
    expected_data_sha256: &str,
    expected_manifest_sha256: &str,
) -> Result<CompleteReferenceBatch> {
    let (data, manifest_bytes) =
        read_artifact_trust_anchor(published, expected_data_sha256, expected_manifest_sha256)?;
    match peek_manifest_schema(&manifest_bytes)?.as_str() {
        MANIFEST_SCHEMA_V2 => {
            verify_current_manifest_v2(published, &data, &manifest_bytes, expected_data_sha256)
        }
        MANIFEST_SCHEMA_V1 => {
            let manifest: HistoricalManifestV1 = serde_json::from_slice(&manifest_bytes)
                .context("parse historical v1 reference manifest")?;
            if manifest.identity.data_schema != REFERENCE_SCHEMA {
                bail!(
                    "historical reference rows without current instrument rules are read-only evidence and cannot seed PIT materialization"
                );
            }
            verify_current_manifest_v1(published, &data, &manifest_bytes, expected_data_sha256)
        }
        _ => bail!("reference manifest schema is unsupported"),
    }
}

fn read_artifact_trust_anchor(
    published: &PublishedReferenceArtifact,
    expected_data_sha256: &str,
    expected_manifest_sha256: &str,
) -> Result<(Vec<u8>, Vec<u8>)> {
    validate_digest(expected_data_sha256, "expected data")?;
    validate_digest(expected_manifest_sha256, "expected manifest")?;
    validate_artifact_paths(published)?;
    let data = read_bound_file(&published.data_path, MAX_DATA_BYTES)?;
    let manifest_bytes = read_bound_file(&published.manifest_path, MAX_MANIFEST_BYTES)?;
    let success = read_bound_file(&published.success_path, MAX_SUCCESS_BYTES)?;
    if digest(&data) != expected_data_sha256
        || digest(&manifest_bytes) != expected_manifest_sha256
        || success != format!("{expected_data_sha256}\n").as_bytes()
    {
        bail!("reference artifact digest trust anchor does not match");
    }
    Ok((data, manifest_bytes))
}

fn peek_manifest_schema(manifest_bytes: &[u8]) -> Result<String> {
    Ok(serde_json::from_slice::<ManifestSchemaPeek>(manifest_bytes)
        .context("parse reference manifest schema")?
        .schema)
}

fn parse_current_batch(data: &[u8]) -> Result<CompleteReferenceBatch> {
    let mut contracts = Vec::new();
    let mut marks = Vec::new();
    let mut open_interest = Vec::new();
    for (index, line) in data.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<ReferenceRecord>(line)
            .with_context(|| format!("parse reference row {}", index + 1))?
        {
            ReferenceRecord::Metadata(row) => contracts.push(row),
            ReferenceRecord::MarkIndexFunding(row) => marks.push(row),
            ReferenceRecord::OpenInterest(row) => open_interest.push(row),
        }
    }
    CompleteReferenceBatch::new(contracts, marks, open_interest)
}

fn verify_current_manifest_v2(
    published: &PublishedReferenceArtifact,
    data: &[u8],
    manifest_bytes: &[u8],
    expected_data_sha256: &str,
) -> Result<CompleteReferenceBatch> {
    let manifest: ReferenceManifest =
        serde_json::from_slice(manifest_bytes).context("parse reference manifest")?;
    validate_manifest_identity(
        &manifest.identity,
        MANIFEST_SCHEMA_V2,
        REFERENCE_SCHEMA,
        expected_data_sha256,
        data.len() as u64,
    )?;
    validate_partition_path(&published.data_path, manifest.identity.observed_at_ns)?;
    let batch = parse_current_batch(data)?;
    let coverage = ArtifactCoverage::from(batch.coverage(
        manifest.identity.observed_at_ns,
        manifest.identity.max_staleness_ms,
    )?);
    let (mark_index_funding, open_interest) = batch_modality_clocks(&batch)?;
    if coverage != manifest.identity.coverage
        || !coverage.is_complete()
        || mark_index_funding != manifest.mark_index_funding
        || open_interest != manifest.open_interest
        || manifest.identity.rows != row_count(&batch)
    {
        bail!("reference manifest does not match complete artifact contents");
    }
    Ok(batch)
}

fn verify_current_manifest_v1(
    published: &PublishedReferenceArtifact,
    data: &[u8],
    manifest_bytes: &[u8],
    expected_data_sha256: &str,
) -> Result<CompleteReferenceBatch> {
    let manifest: HistoricalManifestV1 =
        serde_json::from_slice(manifest_bytes).context("parse historical v1 reference manifest")?;
    validate_manifest_identity(
        &manifest.identity,
        MANIFEST_SCHEMA_V1,
        REFERENCE_SCHEMA,
        expected_data_sha256,
        data.len() as u64,
    )?;
    validate_partition_path(&published.data_path, manifest.identity.observed_at_ns)?;
    let batch = parse_current_batch(data)?;
    let coverage = ArtifactCoverage::from(batch.coverage(
        manifest.identity.observed_at_ns,
        manifest.identity.max_staleness_ms,
    )?);
    if coverage != manifest.identity.coverage
        || !coverage.is_complete()
        || time_bounds(&batch)? != manifest.time_bounds
        || manifest.identity.rows != row_count(&batch)
    {
        bail!("reference manifest does not match complete artifact contents");
    }
    Ok(batch)
}

pub fn verify_reference_artifact_read_only(
    published: &PublishedReferenceArtifact,
    expected_data_sha256: &str,
    expected_manifest_sha256: &str,
) -> Result<VerifiedReferenceCounts> {
    let (data, manifest_bytes) =
        read_artifact_trust_anchor(published, expected_data_sha256, expected_manifest_sha256)?;
    let schema = peek_manifest_schema(&manifest_bytes)?;
    if schema == MANIFEST_SCHEMA_V2 {
        let batch =
            verify_reference_artifact(published, expected_data_sha256, expected_manifest_sha256)?;
        return Ok(VerifiedReferenceCounts {
            metadata: batch.contracts().len(),
            mark_index_funding: batch.mark_index_funding().len(),
            open_interest: batch.open_interest().len(),
            historical_read_only: false,
        });
    }
    if schema != MANIFEST_SCHEMA_V1 {
        bail!("reference manifest schema is unsupported");
    }
    let manifest: HistoricalManifestV1 = serde_json::from_slice(&manifest_bytes)
        .context("parse historical v1 reference manifest")?;
    if manifest.identity.data_schema == REFERENCE_SCHEMA {
        let batch =
            verify_current_manifest_v1(published, &data, &manifest_bytes, expected_data_sha256)?;
        return Ok(VerifiedReferenceCounts {
            metadata: batch.contracts().len(),
            mark_index_funding: batch.mark_index_funding().len(),
            open_interest: batch.open_interest().len(),
            historical_read_only: false,
        });
    }
    validate_manifest_identity(
        &manifest.identity,
        MANIFEST_SCHEMA_V1,
        HISTORICAL_REFERENCE_SCHEMA_V2,
        expected_data_sha256,
        data.len() as u64,
    )?;
    validate_partition_path(&published.data_path, manifest.identity.observed_at_ns)?;
    let mut contracts = Vec::new();
    let mut marks = Vec::new();
    let mut open_interest = Vec::new();
    for (index, line) in data.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<HistoricalReferenceRecordV2>(line)
            .with_context(|| format!("parse historical reference row {}", index + 1))?
        {
            HistoricalReferenceRecordV2::Metadata(row) => contracts.push(row),
            HistoricalReferenceRecordV2::MarkIndexFunding(row) => marks.push(row),
            HistoricalReferenceRecordV2::OpenInterest(row) => open_interest.push(row),
        }
    }
    let batch = HistoricalBatchV2::new(contracts, marks, open_interest)?;
    let coverage = batch.coverage(
        manifest.identity.observed_at_ns,
        manifest.identity.max_staleness_ms,
    )?;
    if coverage != manifest.identity.coverage
        || !coverage.is_complete()
        || batch.time_bounds()? != manifest.time_bounds
        || manifest.identity.rows != batch.row_count()
    {
        bail!("historical reference manifest does not match complete artifact contents");
    }
    Ok(VerifiedReferenceCounts {
        metadata: batch.contracts.len(),
        mark_index_funding: batch.marks.len(),
        open_interest: batch.open_interest.len(),
        historical_read_only: true,
    })
}

fn publish_reference_batch_inner(
    config: &ReferenceArtifactConfig,
    source_origin: &str,
    batch: &CompleteReferenceBatch,
    fail_after_data: bool,
) -> Result<PublishedReferenceArtifact> {
    if source_origin != OFFICIAL_USDM_SOURCE_ORIGIN {
        bail!("reference artifact source origin is not official Binance");
    }
    let coverage =
        ArtifactCoverage::from(batch.coverage(config.observed_at_ns, config.max_staleness_ms)?);
    if !coverage.is_complete() {
        bail!("reference artifact batch is incomplete or stale");
    }
    let (mark_index_funding, open_interest) = batch_modality_clocks(batch)?;
    let records = batch
        .contracts()
        .iter()
        .cloned()
        .map(ReferenceRecord::Metadata)
        .chain(
            batch
                .mark_index_funding()
                .iter()
                .cloned()
                .map(ReferenceRecord::MarkIndexFunding),
        )
        .chain(
            batch
                .open_interest()
                .iter()
                .cloned()
                .map(ReferenceRecord::OpenInterest),
        );
    let mut data = Vec::new();
    for record in records {
        serde_json::to_writer(&mut data, &record)?;
        data.push(b'\n');
    }
    let data_sha256 = digest(&data);
    let partition = utc_partition(config.observed_at_ns)?;
    let hour_dir = config
        .output_root
        .join("lake/raw")
        .join(format!("venue={VENUE}"))
        .join(format!("dataset={DATASET}"))
        .join(format!("date={}", partition.0))
        .join(format!("hour={}", partition.1));
    ensure_canonical_directory(&hour_dir)?;
    let final_dir = hour_dir.join(format!("batch={}", config.observed_at_ns));
    if fs::symlink_metadata(&final_dir).is_ok() {
        bail!("reference artifact batch already exists");
    }
    let mut staging = StagingDir::create(&hour_dir)?;
    let manifest = ReferenceManifest {
        identity: ManifestIdentity {
            schema: MANIFEST_SCHEMA_V2.to_owned(),
            venue: VENUE.to_owned(),
            dataset: DATASET.to_owned(),
            data_schema: REFERENCE_SCHEMA.to_owned(),
            format: "ndjson".to_owned(),
            source_origin: source_origin.to_owned(),
            source_endpoints: source_endpoints(),
            file: DATA_NAME.to_owned(),
            bytes: data.len() as u64,
            sha256: data_sha256.clone(),
            rows: row_count(batch),
            observed_at_ns: config.observed_at_ns,
            max_staleness_ms: config.max_staleness_ms,
            coverage,
        },
        mark_index_funding,
        open_interest,
    };
    let mut manifest_bytes = serde_json::to_vec(&manifest)?;
    manifest_bytes.push(b'\n');
    let manifest_sha256 = digest(&manifest_bytes);
    write_new(&staging.path.join(DATA_NAME), &data)?;
    if fail_after_data {
        bail!("injected reference publication failure after data");
    }
    write_new(
        &staging.path.join(format!("{DATA_NAME}.manifest.json")),
        &manifest_bytes,
    )?;
    write_new(
        &staging.path.join(format!("{DATA_NAME}._SUCCESS")),
        format!("{data_sha256}\n").as_bytes(),
    )?;
    File::open(&staging.path)?.sync_all()?;
    rename_noreplace(&staging.path, &final_dir)?;
    staging.published = true;
    File::open(&hour_dir)?.sync_all()?;
    Ok(PublishedReferenceArtifact {
        data_path: final_dir.join(DATA_NAME),
        manifest_path: final_dir.join(format!("{DATA_NAME}.manifest.json")),
        success_path: final_dir.join(format!("{DATA_NAME}._SUCCESS")),
        data_sha256,
        manifest_sha256,
    })
}

struct StagingDir {
    path: PathBuf,
    published: bool,
}

impl StagingDir {
    fn create(parent: &Path) -> Result<Self> {
        for _ in 0..32 {
            let path = parent.join(format!(".reference-staging.{:016x}", random::<u64>()));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        published: false,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        bail!("could not allocate reference artifact staging directory")
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn path_c_string(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| anyhow!("path contains a NUL byte: {}", path.display()))
}

#[cfg(target_os = "linux")]
fn rename_noreplace(source: &Path, target: &Path) -> Result<()> {
    let source = path_c_string(source)?;
    let target = path_c_string(target)?;
    // SAFETY: both C strings are NUL-terminated and live for the call.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("no-clobber rename failed");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn rename_noreplace(source: &Path, target: &Path) -> Result<()> {
    let source = path_c_string(source)?;
    let target = path_c_string(target)?;
    // SAFETY: both C strings are NUL-terminated and live for the call.
    if unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("no-clobber rename failed");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_noreplace(_source: &Path, _target: &Path) -> Result<()> {
    bail!("atomic no-clobber rename is unsupported on this platform")
}

fn read_bound_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    if !path.is_absolute() {
        bail!("reference artifact path must be absolute");
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("open reference artifact {}", path.display()))?;
    let opened = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    if !opened.is_file()
        || !named.is_file()
        || named.file_type().is_symlink()
        || opened.dev() != named.dev()
        || opened.ino() != named.ino()
        || opened.len() > max_bytes
        || fs::canonicalize(path)? != path
    {
        bail!("reference artifact must be a bounded canonical regular file");
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.read_to_end(&mut bytes)?;
    if bytes.len() as u64 != opened.len() {
        bail!("reference artifact changed during readback");
    }
    Ok(bytes)
}

fn validate_artifact_paths(published: &PublishedReferenceArtifact) -> Result<()> {
    let parent = published
        .data_path
        .parent()
        .ok_or_else(|| anyhow!("reference artifact has no batch directory"))?;
    if published.manifest_path.parent() != Some(parent)
        || published.success_path.parent() != Some(parent)
        || published
            .data_path
            .file_name()
            .and_then(|name| name.to_str())
            != Some(DATA_NAME)
        || published
            .manifest_path
            .file_name()
            .and_then(|name| name.to_str())
            != Some(&format!("{DATA_NAME}.manifest.json"))
        || published
            .success_path
            .file_name()
            .and_then(|name| name.to_str())
            != Some(&format!("{DATA_NAME}._SUCCESS"))
        || fs::canonicalize(parent)? != parent
    {
        bail!("reference artifact paths are not one canonical sibling triplet");
    }
    Ok(())
}

fn validate_manifest_identity(
    identity: &ManifestIdentity,
    expected_manifest_schema: &str,
    expected_data_schema: &str,
    expected_data_sha256: &str,
    data_bytes: u64,
) -> Result<()> {
    if identity.schema != expected_manifest_schema
        || identity.venue != VENUE
        || identity.dataset != DATASET
        || identity.data_schema != expected_data_schema
        || identity.format != "ndjson"
        || identity.source_origin != OFFICIAL_USDM_SOURCE_ORIGIN
        || identity.source_endpoints != source_endpoints()
        || identity.file != DATA_NAME
        || identity.bytes != data_bytes
        || identity.sha256 != expected_data_sha256
    {
        bail!("reference manifest identity is invalid");
    }
    Ok(())
}

fn validate_partition_path(data_path: &Path, observed_at_ns: u64) -> Result<()> {
    let batch = data_path.parent().context("missing batch partition")?;
    let hour = batch.parent().context("missing hour partition")?;
    let date = hour.parent().context("missing date partition")?;
    let dataset = date.parent().context("missing dataset partition")?;
    let venue = dataset.parent().context("missing venue partition")?;
    let raw = venue.parent().context("missing raw partition")?;
    let lake = raw.parent().context("missing lake partition")?;
    let expected = utc_partition(observed_at_ns)?;
    if batch.file_name().and_then(|value| value.to_str())
        != Some(&format!("batch={observed_at_ns}"))
        || hour.file_name().and_then(|value| value.to_str())
            != Some(&format!("hour={}", expected.1))
        || date.file_name().and_then(|value| value.to_str())
            != Some(&format!("date={}", expected.0))
        || dataset.file_name().and_then(|value| value.to_str())
            != Some(&format!("dataset={DATASET}"))
        || venue.file_name().and_then(|value| value.to_str()) != Some(&format!("venue={VENUE}"))
        || raw.file_name().and_then(|value| value.to_str()) != Some("raw")
        || lake.file_name().and_then(|value| value.to_str()) != Some("lake")
    {
        bail!("reference artifact partition identity is invalid");
    }
    Ok(())
}

fn source_endpoints() -> Vec<String> {
    [
        SERVER_TIME_ENDPOINT,
        EXCHANGE_INFO_ENDPOINT,
        PREMIUM_INDEX_ENDPOINT,
        OPEN_INTEREST_ENDPOINT,
    ]
    .into_iter()
    .map(|endpoint| format!("{OFFICIAL_USDM_SOURCE_ORIGIN}{endpoint}"))
    .collect()
}

// Merged cross-modality bounds are only computed to read back historical V1
// manifests; the current publisher emits per-modality PIT clocks instead.
fn time_bounds(batch: &CompleteReferenceBatch) -> Result<ArtifactTimeBounds> {
    artifact_time_bounds(
        batch
            .contracts()
            .iter()
            .map(|row| row.source_time_ms)
            .chain(
                batch
                    .mark_index_funding()
                    .iter()
                    .map(|row| row.source_time_ms),
            )
            .chain(batch.open_interest().iter().map(|row| row.source_time_ms)),
        batch
            .contracts()
            .iter()
            .flat_map(|row| [row.source_clock_received_at_ns, row.received_at_ns])
            .chain(
                batch
                    .mark_index_funding()
                    .iter()
                    .map(|row| row.received_at_ns),
            )
            .chain(batch.open_interest().iter().map(|row| row.received_at_ns)),
    )
}

fn artifact_time_bounds(
    source_times: impl Iterator<Item = u64>,
    received_times: impl Iterator<Item = u64>,
) -> Result<ArtifactTimeBounds> {
    let sources = source_times.collect::<Vec<_>>();
    let received = received_times.collect::<Vec<_>>();
    Ok(ArtifactTimeBounds {
        min_source_time_ms: *sources
            .iter()
            .min()
            .context("reference batch has no source time")?,
        max_source_time_ms: *sources
            .iter()
            .max()
            .context("reference batch has no source time")?,
        min_received_at_ns: *received
            .iter()
            .min()
            .context("reference batch has no receive time")?,
        max_received_at_ns: *received
            .iter()
            .max()
            .context("reference batch has no receive time")?,
    })
}

fn batch_modality_clocks(
    batch: &CompleteReferenceBatch,
) -> Result<(ModalityPitClock, ModalityPitClock)> {
    let mark_index_funding = modality_pit_clock(
        "mark/index/funding",
        batch
            .mark_index_funding()
            .iter()
            .map(|row| (row.source_time_ms, row.received_at_ns)),
    )?;
    let open_interest = modality_pit_clock(
        "open-interest",
        batch
            .open_interest()
            .iter()
            .map(|row| (row.source_time_ms, row.received_at_ns)),
    )?;
    Ok((mark_index_funding, open_interest))
}

fn modality_pit_clock(
    modality: &str,
    rows: impl Iterator<Item = (u64, u64)>,
) -> Result<ModalityPitClock> {
    let mut first_event_time_ms = u64::MAX;
    let mut last_event_time_ms = 0_u64;
    let mut available_at_ns = Vec::new();
    for (source_time_ms, received_at_ns) in rows {
        first_event_time_ms = first_event_time_ms.min(source_time_ms);
        last_event_time_ms = last_event_time_ms.max(source_time_ms);
        available_at_ns.push(received_at_ns);
    }
    if available_at_ns.is_empty() {
        bail!("reference {modality} modality has no observations");
    }
    available_at_ns.sort_unstable();
    let max_gap_ns = available_at_ns
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .max()
        .unwrap_or(0);
    if max_gap_ns > CEX_DERIVATIVES_MAX_GAP_NS {
        bail!("reference {modality} modality exceeds the PIT availability gap bound");
    }
    Ok(ModalityPitClock {
        observations: available_at_ns.len() as u64,
        first_available_at_ns: available_at_ns[0],
        last_available_at_ns: available_at_ns[available_at_ns.len() - 1],
        max_gap_ns,
        first_event_time_ms,
        last_event_time_ms,
    })
}

fn row_count(batch: &CompleteReferenceBatch) -> u64 {
    (batch.contracts().len() + batch.mark_index_funding().len() + batch.open_interest().len())
        as u64
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn validate_digest(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} SHA-256 must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

fn utc_partition(timestamp_ns: u64) -> Result<(String, String)> {
    let seconds = i64::try_from(timestamp_ns / 1_000_000_000)?;
    let nanos = u32::try_from(timestamp_ns % 1_000_000_000)?;
    let observed = DateTime::<Utc>::from_timestamp(seconds, nanos)
        .context("reference observed time is outside UTC range")?;
    Ok((
        observed.format("%Y-%m-%d").to_string(),
        observed.format("%H").to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binance_usdm_reference_collector::OFFICIAL_USDM_SOURCE_ORIGIN;
    use data::binance_usdm_reference::{
        ActivePerpetualContract, MarkIndexFundingObservation, OpenInterestObservation,
        EXCHANGE_INFO_ENDPOINT, OPEN_INTEREST_ENDPOINT, PREMIUM_INDEX_ENDPOINT, REFERENCE_SCHEMA,
        SERVER_TIME_ENDPOINT,
    };
    use rust_decimal::Decimal;
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const RECEIVED_NS: u64 = 1_700_000_000_500_000_000;

    fn sample_batch() -> CompleteReferenceBatch {
        CompleteReferenceBatch::new(
            vec![ActivePerpetualContract {
                schema: REFERENCE_SCHEMA.to_owned(),
                symbol: "BTCUSDT".to_owned(),
                pair: "BTCUSDT".to_owned(),
                base_asset: "BTC".to_owned(),
                quote_asset: "USDT".to_owned(),
                margin_asset: "USDT".to_owned(),
                tick_size: Decimal::new(1, 1),
                step_size: Decimal::new(1, 3),
                min_notional: Decimal::new(5, 0),
                contract_type: "PERPETUAL".to_owned(),
                status: "TRADING".to_owned(),
                onboard_date_ms: 1,
                delivery_date_ms: 4_133_404_800_000,
                source_time_ms: SOURCE_MS,
                source_clock_received_at_ns: RECEIVED_NS - 100,
                received_at_ns: RECEIVED_NS - 50,
                source_endpoint: EXCHANGE_INFO_ENDPOINT.to_owned(),
                source_clock_endpoint: SERVER_TIME_ENDPOINT.to_owned(),
            }],
            vec![MarkIndexFundingObservation {
                schema: REFERENCE_SCHEMA.to_owned(),
                symbol: "BTCUSDT".to_owned(),
                mark_price: Decimal::new(101, 0),
                index_price: Decimal::new(100, 0),
                basis: Decimal::ONE,
                basis_rate: Decimal::new(1, 2),
                last_funding_rate: Decimal::new(1, 4),
                interest_rate: Decimal::new(1, 4),
                next_funding_time_ms: SOURCE_MS + 28_800_000,
                source_time_ms: SOURCE_MS,
                received_at_ns: RECEIVED_NS,
                source_endpoint: PREMIUM_INDEX_ENDPOINT.to_owned(),
            }],
            vec![OpenInterestObservation {
                schema: REFERENCE_SCHEMA.to_owned(),
                symbol: "BTCUSDT".to_owned(),
                open_interest: Decimal::new(12345, 3),
                source_time_ms: SOURCE_MS,
                received_at_ns: RECEIVED_NS + 50,
                source_endpoint: OPEN_INTEREST_ENDPOINT.to_owned(),
            }],
        )
        .unwrap()
    }

    fn config(root: PathBuf) -> ReferenceArtifactConfig {
        ReferenceArtifactConfig {
            output_root: root,
            observed_at_ns: RECEIVED_NS + 100,
            max_staleness_ms: 1_000,
        }
    }

    fn publish_fixture() -> (
        tempfile::TempDir,
        ReferenceArtifactConfig,
        PublishedReferenceArtifact,
    ) {
        let temp = tempdir().unwrap();
        let config = config(fs::canonicalize(temp.path()).unwrap());
        let published =
            publish_reference_batch(&config, OFFICIAL_USDM_SOURCE_ORIGIN, &sample_batch()).unwrap();
        (temp, config, published)
    }

    #[test]
    fn publishes_and_reads_back_one_complete_canonical_batch() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let published =
            publish_reference_batch(&config(root), OFFICIAL_USDM_SOURCE_ORIGIN, &sample_batch())
                .unwrap();

        let verified = verify_reference_artifact(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )
        .unwrap();
        assert_eq!(verified.contracts().len(), 1);
        assert_eq!(verified.mark_index_funding().len(), 1);
        assert_eq!(verified.open_interest().len(), 1);

        let manifest: ReferenceManifest =
            serde_json::from_slice(&fs::read(&published.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.identity.source_endpoints, source_endpoints());
        assert_eq!(manifest.identity.coverage.api_error_count, 0);
        assert_eq!(manifest.identity.coverage.stale_metadata, 0);
        assert_eq!(
            manifest.mark_index_funding.first_available_at_ns,
            RECEIVED_NS
        );
        assert_eq!(
            manifest.mark_index_funding.last_available_at_ns,
            RECEIVED_NS
        );
        assert_eq!(
            manifest.open_interest.first_available_at_ns,
            RECEIVED_NS + 50
        );
        assert_eq!(
            manifest.open_interest.last_available_at_ns,
            RECEIVED_NS + 50
        );
    }

    #[test]
    fn publication_failure_leaks_no_batch_and_retry_succeeds() {
        let temp = tempdir().unwrap();
        let config = config(fs::canonicalize(temp.path()).unwrap());
        let error = publish_reference_batch_inner(
            &config,
            OFFICIAL_USDM_SOURCE_ORIGIN,
            &sample_batch(),
            true,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("injected reference publication failure"));
        let partition = utc_partition(config.observed_at_ns).unwrap();
        let hour = config
            .output_root
            .join("lake/raw")
            .join(format!("venue={VENUE}"))
            .join(format!("dataset={DATASET}"))
            .join(format!("date={}", partition.0))
            .join(format!("hour={}", partition.1));
        assert_eq!(fs::read_dir(&hour).unwrap().count(), 0);

        let published =
            publish_reference_batch(&config, OFFICIAL_USDM_SOURCE_ORIGIN, &sample_batch()).unwrap();
        assert!(published.success_path.is_file());
    }

    #[test]
    fn stale_open_interest_is_published_as_evidence_not_rejected() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let batch = CompleteReferenceBatch::new(
            sample_batch().contracts().to_vec(),
            sample_batch().mark_index_funding().to_vec(),
            vec![OpenInterestObservation {
                schema: REFERENCE_SCHEMA.to_owned(),
                symbol: "BTCUSDT".to_owned(),
                open_interest: Decimal::new(12345, 3),
                source_time_ms: SOURCE_MS - 3_600_000,
                received_at_ns: RECEIVED_NS + 50,
                source_endpoint: OPEN_INTEREST_ENDPOINT.to_owned(),
            }],
        )
        .unwrap();
        let published =
            publish_reference_batch(&config(root), OFFICIAL_USDM_SOURCE_ORIGIN, &batch).unwrap();
        let manifest: ReferenceManifest =
            serde_json::from_slice(&fs::read(&published.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.identity.coverage.open_interest_observations, 1);
        assert_eq!(manifest.identity.coverage.stale_open_interest, 1);
        assert_eq!(manifest.identity.coverage.stale_metadata, 0);
        assert_eq!(manifest.identity.coverage.stale_mark_index_funding, 0);
        verify_reference_artifact(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )
        .unwrap();
    }

    #[test]
    fn stale_batches_and_non_official_sources_fail_closed() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let mut stale = config(root.clone());
        stale.observed_at_ns = (SOURCE_MS + 2_000) * 1_000_000;
        assert!(
            publish_reference_batch(&stale, OFFICIAL_USDM_SOURCE_ORIGIN, &sample_batch(),)
                .unwrap_err()
                .to_string()
                .contains("incomplete or stale")
        );
        assert!(
            publish_reference_batch(&config(root), "https://example.com", &sample_batch(),)
                .unwrap_err()
                .to_string()
                .contains("not official Binance")
        );
    }

    #[test]
    fn external_digest_anchors_and_file_contents_are_both_required() {
        let (_temp, _config, published) = publish_fixture();
        let wrong = "0".repeat(64);
        assert!(
            verify_reference_artifact(&published, &wrong, &published.manifest_sha256,).is_err()
        );

        let mut tampered = fs::read(&published.data_path).unwrap();
        tampered.extend_from_slice(b"{}\n");
        fs::write(&published.data_path, tampered).unwrap();
        assert!(verify_reference_artifact(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )
        .is_err());
    }

    #[test]
    fn symlinked_artifact_members_are_rejected() {
        let (_temp, _config, published) = publish_fixture();
        fs::remove_file(&published.success_path).unwrap();
        symlink(&published.data_path, &published.success_path).unwrap();
        assert!(verify_reference_artifact(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )
        .is_err());
    }

    fn contract_row(symbol: &str, source_ms: u64, received_ns: u64) -> ActivePerpetualContract {
        ActivePerpetualContract {
            schema: REFERENCE_SCHEMA.to_owned(),
            symbol: symbol.to_owned(),
            pair: symbol.to_owned(),
            base_asset: "BTC".to_owned(),
            quote_asset: "USDT".to_owned(),
            margin_asset: "USDT".to_owned(),
            tick_size: Decimal::new(1, 1),
            step_size: Decimal::new(1, 3),
            min_notional: Decimal::new(5, 0),
            contract_type: "PERPETUAL".to_owned(),
            status: "TRADING".to_owned(),
            onboard_date_ms: 1,
            delivery_date_ms: 4_133_404_800_000,
            source_time_ms: source_ms,
            source_clock_received_at_ns: received_ns - 100,
            received_at_ns: received_ns,
            source_endpoint: EXCHANGE_INFO_ENDPOINT.to_owned(),
            source_clock_endpoint: SERVER_TIME_ENDPOINT.to_owned(),
        }
    }

    fn mark_row(symbol: &str, source_ms: u64, received_ns: u64) -> MarkIndexFundingObservation {
        MarkIndexFundingObservation {
            schema: REFERENCE_SCHEMA.to_owned(),
            symbol: symbol.to_owned(),
            mark_price: Decimal::new(101, 0),
            index_price: Decimal::new(100, 0),
            basis: Decimal::ONE,
            basis_rate: Decimal::new(1, 2),
            last_funding_rate: Decimal::new(1, 4),
            interest_rate: Decimal::new(1, 4),
            next_funding_time_ms: source_ms + 28_800_000,
            source_time_ms: source_ms,
            received_at_ns: received_ns,
            source_endpoint: PREMIUM_INDEX_ENDPOINT.to_owned(),
        }
    }

    fn open_interest_row(
        symbol: &str,
        source_ms: u64,
        received_ns: u64,
    ) -> OpenInterestObservation {
        OpenInterestObservation {
            schema: REFERENCE_SCHEMA.to_owned(),
            symbol: symbol.to_owned(),
            open_interest: Decimal::new(12345, 3),
            source_time_ms: source_ms,
            received_at_ns: received_ns,
            source_endpoint: OPEN_INTEREST_ENDPOINT.to_owned(),
        }
    }

    #[test]
    fn modality_pit_clocks_stay_independent_under_interleaved_arrival() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let batch = CompleteReferenceBatch::new(
            vec![
                contract_row("AAAUSDT", SOURCE_MS, RECEIVED_NS - 50),
                contract_row("BBBUSDT", SOURCE_MS, RECEIVED_NS - 40),
            ],
            vec![
                mark_row("AAAUSDT", SOURCE_MS, RECEIVED_NS),
                mark_row("BBBUSDT", SOURCE_MS, RECEIVED_NS + 200_000_000),
            ],
            vec![
                open_interest_row("AAAUSDT", SOURCE_MS - 3_600_000, RECEIVED_NS + 100_000_000),
                open_interest_row("BBBUSDT", SOURCE_MS - 3_600_000, RECEIVED_NS + 300_000_000),
            ],
        )
        .unwrap();
        let config = ReferenceArtifactConfig {
            output_root: root,
            observed_at_ns: RECEIVED_NS + 400_000_000,
            max_staleness_ms: 1_000,
        };
        let published =
            publish_reference_batch(&config, OFFICIAL_USDM_SOURCE_ORIGIN, &batch).unwrap();
        let manifest: ReferenceManifest =
            serde_json::from_slice(&fs::read(&published.manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest.mark_index_funding,
            ModalityPitClock {
                observations: 2,
                first_available_at_ns: RECEIVED_NS,
                last_available_at_ns: RECEIVED_NS + 200_000_000,
                max_gap_ns: 200_000_000,
                first_event_time_ms: SOURCE_MS,
                last_event_time_ms: SOURCE_MS,
            }
        );
        assert_eq!(
            manifest.open_interest,
            ModalityPitClock {
                observations: 2,
                first_available_at_ns: RECEIVED_NS + 100_000_000,
                last_available_at_ns: RECEIVED_NS + 300_000_000,
                max_gap_ns: 200_000_000,
                first_event_time_ms: SOURCE_MS - 3_600_000,
                last_event_time_ms: SOURCE_MS - 3_600_000,
            }
        );
        verify_reference_artifact(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )
        .unwrap();
    }

    #[test]
    fn modality_availability_gap_above_bound_fails_closed() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let late_ms = SOURCE_MS + 100_000;
        let late_ns = RECEIVED_NS + 100_000_000_000;
        let batch = CompleteReferenceBatch::new(
            vec![
                contract_row("AAAUSDT", late_ms, late_ns - 50),
                contract_row("BBBUSDT", late_ms, late_ns - 40),
            ],
            vec![
                mark_row("AAAUSDT", SOURCE_MS, RECEIVED_NS),
                mark_row("BBBUSDT", late_ms, late_ns),
            ],
            vec![
                open_interest_row("AAAUSDT", late_ms, late_ns - 30),
                open_interest_row("BBBUSDT", late_ms, late_ns - 20),
            ],
        )
        .unwrap();
        let config = ReferenceArtifactConfig {
            output_root: root.clone(),
            observed_at_ns: late_ns + 100,
            max_staleness_ms: 120_000,
        };
        let error = publish_reference_batch(&config, OFFICIAL_USDM_SOURCE_ORIGIN, &batch)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("exceeds the PIT availability gap bound"),
            "{error}"
        );
        let partition = utc_partition(config.observed_at_ns).unwrap();
        let hour = root
            .join("lake/raw")
            .join(format!("venue={VENUE}"))
            .join(format!("dataset={DATASET}"))
            .join(format!("date={}", partition.0))
            .join(format!("hour={}", partition.1));
        assert!(!hour.exists() || fs::read_dir(&hour).unwrap().count() == 0);
    }

    #[test]
    fn missing_modality_clock_fails_closed() {
        assert!(modality_pit_clock("open-interest", std::iter::empty()).is_err());

        let (_temp, _config, published) = publish_fixture();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&published.manifest_path).unwrap()).unwrap();
        manifest.as_object_mut().unwrap().remove("open_interest");
        let mut bytes = serde_json::to_vec(&manifest).unwrap();
        bytes.push(b'\n');
        fs::write(&published.manifest_path, &bytes).unwrap();
        let manifest_sha256 = digest(&bytes);
        assert!(
            verify_reference_artifact(&published, &published.data_sha256, &manifest_sha256,)
                .is_err()
        );
    }

    #[test]
    fn manifest_publishes_per_modality_pit_clocks_without_merged_time_bounds() {
        let (_temp, _config, published) = publish_fixture();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&published.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["schema"], MANIFEST_SCHEMA_V2);
        assert!(manifest.get("time_bounds").is_none());
        for modality in ["mark_index_funding", "open_interest"] {
            let clock = manifest
                .get(modality)
                .unwrap_or_else(|| panic!("manifest is missing {modality}"));
            let mut keys: Vec<&str> = clock
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                [
                    "first_available_at_ns",
                    "first_event_time_ms",
                    "last_available_at_ns",
                    "last_event_time_ms",
                    "max_gap_ns",
                    "observations",
                ]
            );
        }
    }

    #[test]
    fn historical_v1_manifest_remains_readable_read_only() {
        let (_temp, config, published) = publish_fixture();
        let data = fs::read(&published.data_path).unwrap();
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA_V1,
            "venue": VENUE,
            "dataset": DATASET,
            "data_schema": REFERENCE_SCHEMA,
            "format": "ndjson",
            "source_origin": OFFICIAL_USDM_SOURCE_ORIGIN,
            "source_endpoints": source_endpoints(),
            "file": DATA_NAME,
            "bytes": data.len(),
            "sha256": published.data_sha256,
            "rows": 3,
            "observed_at_ns": config.observed_at_ns,
            "max_staleness_ms": config.max_staleness_ms,
            "coverage": {
                "active_contracts": 1,
                "metadata_observations": 1,
                "mark_index_funding_observations": 1,
                "open_interest_observations": 1,
                "stale_metadata": 0,
                "stale_mark_index_funding": 0,
                "stale_open_interest": 0,
                "api_error_count": 0,
            },
            "time_bounds": {
                "min_source_time_ms": SOURCE_MS,
                "max_source_time_ms": SOURCE_MS,
                "min_received_at_ns": RECEIVED_NS - 100,
                "max_received_at_ns": RECEIVED_NS + 50,
            },
        });
        let mut bytes = serde_json::to_vec(&manifest).unwrap();
        bytes.push(b'\n');
        fs::write(&published.manifest_path, &bytes).unwrap();
        let manifest_sha256 = digest(&bytes);
        let error = verify_reference_artifact(&published, &published.data_sha256, &manifest_sha256)
            .unwrap_err()
            .to_string();
        assert!(error.contains("read-only evidence"), "{error}");
        let counts = verify_reference_artifact_read_only(
            &published,
            &published.data_sha256,
            &manifest_sha256,
        )
        .unwrap();
        assert_eq!(
            counts,
            VerifiedReferenceCounts {
                metadata: 1,
                mark_index_funding: 1,
                open_interest: 1,
                historical_read_only: false,
            }
        );
    }

    #[test]
    fn historical_v1_current_rows_seed_the_read_only_current_batch_helper() {
        let (_temp, config, published) = publish_fixture();
        let data = fs::read(&published.data_path).unwrap();
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA_V1,
            "venue": VENUE,
            "dataset": DATASET,
            "data_schema": REFERENCE_SCHEMA,
            "format": "ndjson",
            "source_origin": OFFICIAL_USDM_SOURCE_ORIGIN,
            "source_endpoints": source_endpoints(),
            "file": DATA_NAME,
            "bytes": data.len(),
            "sha256": published.data_sha256,
            "rows": 3,
            "observed_at_ns": config.observed_at_ns,
            "max_staleness_ms": config.max_staleness_ms,
            "coverage": {
                "active_contracts": 1,
                "metadata_observations": 1,
                "mark_index_funding_observations": 1,
                "open_interest_observations": 1,
                "stale_metadata": 0,
                "stale_mark_index_funding": 0,
                "stale_open_interest": 0,
                "api_error_count": 0,
            },
            "time_bounds": {
                "min_source_time_ms": SOURCE_MS,
                "max_source_time_ms": SOURCE_MS,
                "min_received_at_ns": RECEIVED_NS - 100,
                "max_received_at_ns": RECEIVED_NS + 50,
            },
        });
        let mut bytes = serde_json::to_vec(&manifest).unwrap();
        bytes.push(b'\n');
        fs::write(&published.manifest_path, &bytes).unwrap();
        let manifest_sha256 = digest(&bytes);

        let batch = verify_reference_artifact_read_only_current_batch(
            &published,
            &published.data_sha256,
            &manifest_sha256,
        )
        .unwrap();

        assert_eq!(batch.contracts().len(), 1);
        assert_eq!(batch.mark_index_funding().len(), 1);
        assert_eq!(batch.open_interest().len(), 1);
    }

    #[test]
    fn historical_v2_rows_are_rejected_by_the_current_batch_helper() {
        let (_temp, config, published) = publish_fixture();
        let data = fs::read(&published.data_path).unwrap();
        let mut historical = Vec::new();
        for line in data.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let mut row: serde_json::Value = serde_json::from_slice(line).unwrap();
            row["observation"]["schema"] = serde_json::json!(HISTORICAL_REFERENCE_SCHEMA_V2);
            if row["kind"] == "metadata" {
                let observation = row["observation"].as_object_mut().unwrap();
                observation.remove("tick_size");
                observation.remove("step_size");
                observation.remove("min_notional");
            }
            serde_json::to_writer(&mut historical, &row).unwrap();
            historical.push(b'\n');
        }
        fs::write(&published.data_path, &historical).unwrap();
        let data_sha256 = digest(&historical);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA_V1,
            "venue": VENUE,
            "dataset": DATASET,
            "data_schema": HISTORICAL_REFERENCE_SCHEMA_V2,
            "format": "ndjson",
            "source_origin": OFFICIAL_USDM_SOURCE_ORIGIN,
            "source_endpoints": source_endpoints(),
            "file": DATA_NAME,
            "bytes": historical.len(),
            "sha256": data_sha256,
            "rows": 3,
            "observed_at_ns": config.observed_at_ns,
            "max_staleness_ms": config.max_staleness_ms,
            "coverage": {
                "active_contracts": 1,
                "metadata_observations": 1,
                "mark_index_funding_observations": 1,
                "open_interest_observations": 1,
                "stale_metadata": 0,
                "stale_mark_index_funding": 0,
                "stale_open_interest": 0,
                "api_error_count": 0,
            },
            "time_bounds": {
                "min_source_time_ms": SOURCE_MS,
                "max_source_time_ms": SOURCE_MS,
                "min_received_at_ns": RECEIVED_NS,
                "max_received_at_ns": RECEIVED_NS + 50,
            },
        });
        let mut manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        manifest_bytes.push(b'\n');
        fs::write(&published.manifest_path, &manifest_bytes).unwrap();
        let manifest_sha256 = digest(&manifest_bytes);
        fs::write(&published.success_path, format!("{data_sha256}\n")).unwrap();

        let error = verify_reference_artifact_read_only_current_batch(
            &published,
            &data_sha256,
            &manifest_sha256,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("read-only evidence"), "{error}");
    }
}
