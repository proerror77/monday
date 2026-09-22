//! External chain fills for descriptive research. This contract cannot represent
//! an executable quote, L2 book, settlement label, or complete market tape.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const HISTORY_SCHEMA: &str = "monday.polymarket.external_history.v1";
pub const POLY_DATA_SCHEMA: &str = "poly_data.order_filled.v2";
pub const POLY_DATA_REVISION: &str = "136e129735c73bce3a3d59f20354417b188e47fd";
pub const V2_EXCHANGE: &str = "0xe111180000d2663c0091e4f400237545b87b996b";
pub const V2_GENESIS_DATE_UNIX: u64 = 1_774_915_200;
pub const HISTORY_SCOPE: &str = "external_chain_fill_events_trade_only";
pub const HISTORY_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const HISTORY_MAX_ROWS: u64 = 1_000_000;
pub const HISTORY_ROW_MAX_BYTES: usize = 64 * 1024;
pub const HISTORY_DATA_FILE: &str = "history.ndjson";
pub const HISTORY_MANIFEST_FILE: &str = "manifest.json";
pub const HISTORY_SUCCESS_FILE: &str = "_SUCCESS";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistorySourceFile {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistorySource {
    pub schema: String,
    /// Declared provenance of externally supplied bytes, not an on-chain proof.
    pub revision: String,
    pub chain_id: u64,
    pub exchange_contract: String,
    pub markets: HistorySourceFile,
    pub fills: HistorySourceFile,
    pub retrieved_at_unix: u64,
    /// Frozen in the import specification so an exact retry has the same identity.
    pub imported_at_unix: u64,
    pub requested_start_unix: u64,
    pub requested_end_unix: u64,
    pub max_input_bytes: u64,
    pub max_input_rows: u64,
}

impl HistorySource {
    pub fn validate(&self) -> Result<()> {
        if self.schema != POLY_DATA_SCHEMA
            || self.revision != POLY_DATA_REVISION
            || self.chain_id != 137
            || self.exchange_contract != V2_EXCHANGE
            || self.requested_start_unix < V2_GENESIS_DATE_UNIX
            || self.requested_start_unix >= self.requested_end_unix
            || self.requested_end_unix > self.retrieved_at_unix
            || self.retrieved_at_unix > self.imported_at_unix
            || self.max_input_bytes == 0
            || self.max_input_bytes > 256 * 1024 * 1024
            || self.max_input_rows == 0
            || self.max_input_rows > HISTORY_MAX_ROWS
        {
            bail!("unsupported poly_data source profile, clocks, or resource bounds");
        }
        for input in [&self.markets, &self.fills] {
            validate_hex(&input.sha256, 64)?;
            if input.bytes == 0
                || input.bytes > self.max_input_bytes
                || !Path::new(&input.path).is_absolute()
            {
                bail!("history input must name a bounded absolute source file");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryMarket {
    pub condition_id: String,
    pub slug: String,
    /// Labels follow explicit token IDs from metadata; array position is not Up/Down.
    pub outcomes: BTreeMap<String, String>,
}

impl HistoryMarket {
    pub fn validate(&self) -> Result<()> {
        validate_chain_hex(&self.condition_id, 64)?;
        if self.slug.len() > 512 || self.outcomes.len() != 2 {
            bail!("history market must have exactly two distinct outcome tokens");
        }
        let mut labels = BTreeSet::new();
        for (token, outcome) in &self.outcomes {
            validate_token_id(token)?;
            if outcome.trim().is_empty()
                || outcome.len() > 256
                || !labels.insert(outcome.to_lowercase())
            {
                bail!("invalid or ambiguous market outcome label");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MakerDirection {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryFill {
    /// Source-file occurrence, not a claim of unique economic fill identity.
    pub source_row: u64,
    pub condition_id: String,
    pub token_id: String,
    pub outcome: String,
    pub block_time_unix: u64,
    /// Unknown for historical CSV exports, even when block_time is known.
    pub historical_available_at_unix: Option<u64>,
    pub transaction_hash: String,
    /// Optional explicitly enriched column. Stock upstream exports omit this.
    pub log_index: Option<u64>,
    pub maker_direction: MakerDirection,
    pub cash_amount: Decimal,
    pub token_amount: Decimal,
    /// Decimal approximation. cash_amount / token_amount is the authoritative ratio.
    pub price: Decimal,
}

impl HistoryFill {
    pub fn validate(&self, source: &HistorySource, market: &HistoryMarket) -> Result<()> {
        validate_chain_hex(&self.transaction_hash, 64)?;
        let max_amount = Decimal::from_parts(u32::MAX, u32::MAX, u32::MAX, false, 6);
        if self.source_row == 0
            || self.source_row > source.max_input_rows
            || self.condition_id != market.condition_id
            || market.outcomes.get(&self.token_id) != Some(&self.outcome)
            || self.historical_available_at_unix.is_some()
            || self.block_time_unix < source.requested_start_unix
            || self.block_time_unix >= source.requested_end_unix
            || self.cash_amount <= Decimal::ZERO
            || self.token_amount <= Decimal::ZERO
            || self.cash_amount > self.token_amount
            || self.token_amount > max_amount
            || self.cash_amount.scale() > 6
            || self.token_amount.scale() > 6
            || self.cash_amount.checked_div(self.token_amount) != Some(self.price)
            || self.price <= Decimal::ZERO
        {
            bail!("invalid historical fill mapping, clocks, amounts, or price");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRejectionReason {
    MalformedMarket,
    ConflictingMarket,
    DuplicateMarket,
    MalformedFill,
    UnmappedToken,
    InvalidAmount,
    InvalidTimestamp,
    OutsideRequestedWindow,
    IntermediaryMintBurn,
    ConflictingLogIdentity,
    DuplicateLogIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryInputKind {
    Markets,
    Fills,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuarantine {
    pub input: HistoryInputKind,
    pub source_row: u64,
    /// Hash of decoded CSV fields in their original order. The source hash binds
    /// the actual CSV bytes; neither wallet addresses nor raw CSV are logged here.
    pub fields_sha256: String,
    pub reason: HistoryRejectionReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "record",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum HistoryRecord {
    Market(HistoryMarket),
    Fill(HistoryFill),
    Quarantine(HistoryQuarantine),
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuality {
    pub markets: u64,
    pub accepted_fill_rows: u64,
    pub quarantined_rows: u64,
    pub ambiguous_identity_rows: u64,
    pub min_block_time_unix: Option<u64>,
    pub max_block_time_unix: Option<u64>,
    pub reasons: BTreeMap<HistoryRejectionReason, u64>,
}

impl HistoryQuality {
    pub fn observe(&mut self, row: &HistoryRecord) {
        match row {
            HistoryRecord::Market(_) => self.markets += 1,
            HistoryRecord::Fill(fill) => {
                self.accepted_fill_rows += 1;
                self.ambiguous_identity_rows += u64::from(fill.log_index.is_none());
                self.min_block_time_unix = Some(
                    self.min_block_time_unix
                        .map_or(fill.block_time_unix, |time| time.min(fill.block_time_unix)),
                );
                self.max_block_time_unix = Some(
                    self.max_block_time_unix
                        .map_or(fill.block_time_unix, |time| time.max(fill.block_time_unix)),
                );
            }
            HistoryRecord::Quarantine(row) => {
                self.quarantined_rows += 1;
                *self.reasons.entry(row.reason).or_default() += 1;
            }
        }
    }

    pub fn records(&self) -> u64 {
        self.markets
            .saturating_add(self.accepted_fill_rows)
            .saturating_add(self.quarantined_rows)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryManifest {
    pub schema: String,
    pub evidence_scope: String,
    pub source: HistorySource,
    pub content_sha256: String,
    pub content_bytes: u64,
    pub quality: HistoryQuality,
    pub l2_available: bool,
    pub executable_quotes_available: bool,
    pub settlement_available: bool,
    pub trade_completion_certified: bool,
    pub coverage_certified: bool,
    pub research_promotion_allowed: bool,
}

impl HistoryManifest {
    pub fn validate(&self) -> Result<()> {
        self.source.validate()?;
        validate_hex(&self.content_sha256, 64)?;
        if self.schema != HISTORY_SCHEMA
            || self.evidence_scope != HISTORY_SCOPE
            || self.content_bytes > HISTORY_MAX_BYTES
            || self.content_bytes == 0
            || self.quality.records() > HISTORY_MAX_ROWS
            || self.l2_available
            || self.executable_quotes_available
            || self.settlement_available
            || self.trade_completion_certified
            || self.coverage_certified
            || self.research_promotion_allowed
        {
            bail!("history is bounded trade-only evidence and cannot certify replay or promotion");
        }
        Ok(())
    }
}

/// The fields are private: a research consumer can obtain this only by anchored
/// readback and semantic validation, never by deserializing a producer claim.
#[derive(Debug)]
pub struct VerifiedHistory {
    manifest: HistoryManifest,
    manifest_sha256: String,
    records: Vec<HistoryRecord>,
}

impl VerifiedHistory {
    pub fn manifest(&self) -> &HistoryManifest {
        &self.manifest
    }

    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    pub fn records(&self) -> impl Iterator<Item = &HistoryRecord> {
        self.records.iter()
    }
}

pub fn history_sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn validate_hex(value: &str, length: usize) -> Result<()> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("expected {length} lowercase hexadecimal characters");
    }
    Ok(())
}

pub fn validate_chain_hex(value: &str, length: usize) -> Result<()> {
    validate_hex(
        value.strip_prefix("0x").context("missing 0x prefix")?,
        length,
    )
}

pub fn validate_token_id(value: &str) -> Result<()> {
    // Decimal string avoids f64/JSON-number loss on uint256 token IDs.
    const UINT256_MAX: &str =
        "115792089237316195423570985008687907853269984665640564039457584007913129639935";
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || value.len() > UINT256_MAX.len()
        || (value.len() == UINT256_MAX.len() && value > UINT256_MAX)
    {
        bail!("invalid canonical uint256 outcome token ID");
    }
    Ok(())
}

fn read_history_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    if !path.is_absolute() || fs::canonicalize(path)? != path {
        bail!("history artifacts require absolute canonical paths");
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > max_bytes {
        bail!("history artifact is not a bounded regular file");
    }
    let mut bytes = Vec::new();
    file.by_ref().take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        bail!("history artifact exceeded byte bound");
    }
    Ok(bytes)
}

pub fn verify_history(directory: &Path, expected_manifest_sha256: &str) -> Result<VerifiedHistory> {
    validate_hex(expected_manifest_sha256, 64)?;
    let manifest_bytes = read_history_file(&directory.join(HISTORY_MANIFEST_FILE), 1024 * 1024)?;
    if history_sha256(&manifest_bytes) != expected_manifest_sha256 {
        bail!("history manifest differs from the supplied trust anchor");
    }
    let manifest: HistoryManifest = serde_json::from_slice(&manifest_bytes)?;
    manifest.validate()?;
    let content = read_history_file(&directory.join(HISTORY_DATA_FILE), HISTORY_MAX_BYTES)?;
    let success = read_history_file(&directory.join(HISTORY_SUCCESS_FILE), 65)?;
    if content.len() as u64 != manifest.content_bytes
        || history_sha256(&content) != manifest.content_sha256
        || success != format!("{}\n", manifest.content_sha256).as_bytes()
        || !content.ends_with(b"\n")
    {
        bail!("history content or _SUCCESS identity mismatch");
    }
    let mut quality = HistoryQuality::default();
    let mut records = Vec::new();
    let mut markets = BTreeMap::new();
    let mut tokens = BTreeSet::new();
    let mut source_rows = BTreeSet::new();
    let mut log_ids = BTreeSet::new();
    let mut quarantine_rows = BTreeSet::new();
    for line in content[..content.len() - 1].split(|byte| *byte == b'\n') {
        if line.len() > HISTORY_ROW_MAX_BYTES || records.len() as u64 >= HISTORY_MAX_ROWS {
            bail!("history row or record count exceeds bound");
        }
        let row: HistoryRecord = serde_json::from_slice(line)?;
        match &row {
            HistoryRecord::Market(market) => {
                market.validate()?;
                if markets
                    .insert(market.condition_id.clone(), market.clone())
                    .is_some()
                    || market
                        .outcomes
                        .keys()
                        .any(|token| !tokens.insert(token.clone()))
                {
                    bail!("history contains conflicting market/token mapping");
                }
            }
            HistoryRecord::Fill(fill) => {
                let market = markets
                    .get(&fill.condition_id)
                    .context("unmapped history fill")?;
                fill.validate(&manifest.source, market)?;
                if !source_rows.insert(fill.source_row) {
                    bail!("history repeats a source row");
                }
                if let Some(index) = fill.log_index {
                    if !log_ids.insert((fill.transaction_hash.clone(), index)) {
                        bail!("history repeats a chain log identity");
                    }
                }
            }
            HistoryRecord::Quarantine(row) => {
                validate_hex(&row.fields_sha256, 64)?;
                if row.source_row == 0 || row.source_row > manifest.source.max_input_rows {
                    bail!("invalid quarantined source row");
                }
                let input = match row.input {
                    HistoryInputKind::Markets => "markets",
                    HistoryInputKind::Fills => "fills",
                };
                if !quarantine_rows.insert((input, row.source_row)) {
                    bail!("history repeats a quarantined source row");
                }
            }
        }
        quality.observe(&row);
        records.push(row);
    }
    if quality != manifest.quality
        || quarantine_rows
            .iter()
            .any(|(input, row)| *input == "fills" && source_rows.contains(row))
    {
        bail!("history quality report differs from independently observed records");
    }
    Ok(VerifiedHistory {
        manifest,
        manifest_sha256: expected_manifest_sha256.to_owned(),
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_token_ids_preserve_uint256_precision_and_reject_aliases() {
        assert!(validate_token_id(
            "115792089237316195423570985008687907853269984665640564039457584007913129639935"
        )
        .is_ok());
        for value in [
            "0",
            "01",
            "1.0",
            "1e20",
            "-1",
            "115792089237316195423570985008687907853269984665640564039457584007913129639936",
        ] {
            assert!(validate_token_id(value).is_err(), "{value}");
        }
    }
}
