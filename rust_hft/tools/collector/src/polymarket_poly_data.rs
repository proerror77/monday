//! Bounded CSV adapter for a pinned external poly_data export. No acquisition,
//! order submission, historical availability inference, or tape certification.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use csv::{Reader, ReaderBuilder, StringRecord};
use data::polymarket_history::{
    history_sha256, validate_chain_hex, validate_hex, verify_history, HistoryFill,
    HistoryInputKind, HistoryManifest, HistoryMarket, HistoryQuality, HistoryQuarantine,
    HistoryRecord, HistoryRejectionReason as Reason, HistorySource, HistorySourceFile,
    MakerDirection, HISTORY_DATA_FILE, HISTORY_MANIFEST_FILE, HISTORY_MAX_BYTES, HISTORY_MAX_ROWS,
    HISTORY_ROW_MAX_BYTES, HISTORY_SCHEMA, HISTORY_SCOPE, HISTORY_SUCCESS_FILE, POLY_DATA_REVISION,
    POLY_DATA_SCHEMA, V2_EXCHANGE, V2_GENESIS_DATE_UNIX,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::polymarket_evidence_artifact::{
    publish_triplet, ArtifactBytes, ImmutablePublicationOutcome,
};
use crate::polymarket_upload::ensure_canonical_directory;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolyDataImportConfig {
    pub source_schema: String,
    pub source_revision: String,
    pub markets: PathBuf,
    pub markets_sha256: String,
    pub fills: PathBuf,
    pub fills_sha256: String,
    pub retrieved_at_unix: u64,
    pub imported_at_unix: u64,
    pub start_unix: u64,
    pub end_unix: u64,
    pub max_input_bytes: u64,
    pub max_input_rows: u64,
    pub output_root: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct PublishedPolyDataHistory {
    pub directory: PathBuf,
    pub manifest_sha256: String,
    pub content_sha256: String,
    pub publication: ImmutablePublicationOutcome,
    pub quality: HistoryQuality,
    pub evidence_scope: String,
    pub research_promotion_allowed: bool,
}

struct Snapshot {
    file: File,
    source: HistorySourceFile,
}

fn snapshot(path: &Path, expected: &str, max_bytes: u64) -> Result<Snapshot> {
    validate_hex(expected, 64)?;
    if !path.is_absolute() || fs::canonicalize(path)? != path {
        bail!("poly_data input must be an absolute canonical path");
    }
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let before = input.metadata()?;
    if !before.is_file() || before.len() == 0 || before.len() > max_bytes {
        bail!("poly_data input is not a bounded nonempty regular file");
    }
    let mut output = tempfile::tempfile()?;
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > max_bytes {
            bail!("poly_data input grew past its byte limit");
        }
        digest.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    let after = input.metadata()?;
    let current = fs::symlink_metadata(path)?;
    let identity = |m: &fs::Metadata| (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec());
    let sha256 = hex::encode(digest.finalize());
    if identity(&before) != identity(&after)
        || identity(&before) != identity(&current)
        || current.file_type().is_symlink()
        || sha256 != expected
    {
        bail!("poly_data input changed or does not match the supplied SHA-256");
    }
    output.seek(SeekFrom::Start(0))?;
    Ok(Snapshot {
        file: output,
        source: HistorySourceFile {
            path: path.to_string_lossy().into_owned(),
            sha256,
            bytes,
        },
    })
}

type Fields = BTreeMap<String, usize>;

#[derive(Clone, Copy)]
enum CsvState {
    FieldStart,
    Unquoted,
    Quoted,
    AfterQuote,
}

/// Bound logical CSV records before the CSV library can grow its record buffer.
/// This recognizes the strict quoting emitted by Python's csv.writer, including
/// escaped quotes and multiline fields. Actual field decoding stays in `csv`.
struct BoundedCsv<R> {
    input: R,
    state: CsvState,
    record_bytes: usize,
}

impl<R: Read> Read for BoundedCsv<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let count = self.input.read(buffer)?;
        if count == 0 && matches!(self.state, CsvState::Quoted) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unterminated CSV quoted field",
            ));
        }
        for byte in &buffer[..count] {
            self.record_bytes += 1;
            if self.record_bytes > HISTORY_ROW_MAX_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "CSV record exceeds byte limit",
                ));
            }
            self.state = match (self.state, byte) {
                (CsvState::FieldStart, b'"') => CsvState::Quoted,
                (CsvState::Quoted, b'"') => CsvState::AfterQuote,
                (CsvState::AfterQuote, b'"') => CsvState::Quoted,
                (CsvState::Quoted, _) => CsvState::Quoted,
                (_, b'\r' | b'\n') => {
                    self.record_bytes = 0;
                    CsvState::FieldStart
                }
                (_, b',') => CsvState::FieldStart,
                (CsvState::AfterQuote, _) | (CsvState::Unquoted, b'"') => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "malformed CSV quoting",
                    ));
                }
                _ => CsvState::Unquoted,
            };
        }
        Ok(count)
    }
}

type CsvInput<'a> = Reader<BoundedCsv<BufReader<&'a mut File>>>;

fn csv_reader<'a>(file: &'a mut File, required: &[&str]) -> Result<(CsvInput<'a>, Fields)> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = ReaderBuilder::new().flexible(true).from_reader(BoundedCsv {
        input: BufReader::new(file),
        state: CsvState::FieldStart,
        record_bytes: 0,
    });
    let headers = reader.headers()?;
    if headers.len() > 256 || headers.as_slice().len() > HISTORY_ROW_MAX_BYTES {
        bail!("CSV header exceeds the supported schema bound");
    }
    let mut fields = BTreeMap::new();
    for (index, name) in headers.iter().enumerate() {
        if name.is_empty() || fields.insert(name.to_owned(), index).is_some() {
            bail!("CSV contains empty or duplicate column names");
        }
    }
    if required.iter().any(|field| !fields.contains_key(*field)) {
        bail!("CSV does not match the pinned raw schema; processed/trades.csv is unsupported");
    }
    Ok((reader, fields))
}

fn value<'a>(row: &'a StringRecord, fields: &Fields, name: &str) -> Option<&'a str> {
    fields.get(name).and_then(|index| row.get(*index))
}

fn bounded_row(row: &StringRecord, fields: &Fields) -> bool {
    row.len() == fields.len() && row.as_slice().len() <= HISTORY_ROW_MAX_BYTES
}

fn fields_sha256(row: &StringRecord) -> String {
    // Length framing distinguishes ["ab", "c"] from ["a", "bc"].
    let mut digest = Sha256::new();
    for field in row {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field.as_bytes());
    }
    hex::encode(digest.finalize())
}

fn market(row: &StringRecord, fields: &Fields) -> Option<HistoryMarket> {
    if !bounded_row(row, fields) {
        return None;
    }
    let condition_id = value(row, fields, "id")?.to_owned();
    if let Some(explicit) = value(row, fields, "condition_id") {
        if explicit != condition_id {
            return None;
        }
    }
    let ids: Vec<String> = serde_json::from_str(value(row, fields, "clobTokenIds")?).ok()?;
    let tokens: Vec<serde_json::Value> =
        serde_json::from_str(value(row, fields, "tokens")?).ok()?;
    let mut outcomes = BTreeMap::new();
    for token in tokens {
        let id = token.get("token_id")?.as_str()?.to_owned();
        let label = token.get("outcome")?.as_str()?.to_owned();
        if outcomes.insert(id, label).is_some() {
            return None;
        }
    }
    if ids.len() != 2
        || ids.iter().collect::<BTreeSet<_>>().len() != 2
        || ids.iter().any(|id| !outcomes.contains_key(id))
    {
        return None;
    }
    let market = HistoryMarket {
        condition_id,
        slug: value(row, fields, "market_slug").unwrap_or("").to_owned(),
        outcomes,
    };
    market.validate().ok()?;
    Some(market)
}

struct Sink {
    file: BufWriter<File>,
    bytes: u64,
    quality: HistoryQuality,
}

impl Sink {
    fn new() -> Result<Self> {
        Ok(Self {
            file: BufWriter::new(tempfile::tempfile()?),
            bytes: 0,
            quality: HistoryQuality::default(),
        })
    }

    fn write(&mut self, record: HistoryRecord) -> Result<()> {
        let bytes = serde_json::to_vec(&record)?;
        if bytes.len() > HISTORY_ROW_MAX_BYTES
            || self.bytes + bytes.len() as u64 + 1 > HISTORY_MAX_BYTES
            || self.quality.records() >= HISTORY_MAX_ROWS
        {
            bail!("normalized history exceeds output row/byte limits; split the input export");
        }
        self.file.write_all(&bytes)?;
        self.file.write_all(b"\n")?;
        self.bytes += bytes.len() as u64 + 1;
        self.quality.observe(&record);
        Ok(())
    }

    fn quarantine(
        &mut self,
        input: HistoryInputKind,
        source_row: u64,
        row: &StringRecord,
        reason: Reason,
    ) -> Result<()> {
        self.write(HistoryRecord::Quarantine(HistoryQuarantine {
            input,
            source_row,
            fields_sha256: fields_sha256(row),
            reason,
        }))
    }
}

const MARKET_FIELDS: &[&str] = &["id", "clobTokenIds", "tokens"];
const FILL_FIELDS: &[&str] = &[
    "timestamp",
    "maker",
    "makerAssetId",
    "makerAmountFilled",
    "taker",
    "takerAssetId",
    "takerAmountFilled",
    "transactionHash",
];

fn check_row_limit(index: usize, limit: u64) -> Result<u64> {
    let row = index as u64 + 1;
    if row > limit {
        bail!("CSV exceeds the declared input row limit");
    }
    Ok(row)
}

fn load_markets(
    input: &mut File,
    limit: u64,
    sink: &mut Sink,
) -> Result<BTreeMap<String, HistoryMarket>> {
    let mut candidates: BTreeMap<String, Option<HistoryMarket>> = BTreeMap::new();
    let (mut reader, fields) = csv_reader(input, MARKET_FIELDS)?;
    for (index, row) in reader.records().enumerate() {
        check_row_limit(index, limit)?;
        let row = row.context("malformed CSV encoding in markets")?;
        if let Some(id) = value(&row, &fields, "id") {
            let parsed = market(&row, &fields);
            candidates
                .entry(id.to_owned())
                .and_modify(|existing| {
                    if *existing != parsed {
                        *existing = None;
                    }
                })
                .or_insert(parsed);
        }
    }
    let mut token_owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for market in candidates.values().flatten() {
        for token in market.outcomes.keys() {
            token_owners
                .entry(token.clone())
                .or_default()
                .insert(market.condition_id.clone());
        }
    }
    for owners in token_owners.values().filter(|owners| owners.len() > 1) {
        for id in owners {
            candidates.insert(id.clone(), None);
        }
    }
    let (mut reader, fields) = csv_reader(input, MARKET_FIELDS)?;
    let mut emitted = BTreeSet::new();
    for (index, row) in reader.records().enumerate() {
        let source_row = check_row_limit(index, limit)?;
        let row = row?;
        match market(&row, &fields) {
            None => sink.quarantine(
                HistoryInputKind::Markets,
                source_row,
                &row,
                Reason::MalformedMarket,
            )?,
            Some(market)
                if candidates
                    .get(&market.condition_id)
                    .is_none_or(Option::is_none) =>
            {
                sink.quarantine(
                    HistoryInputKind::Markets,
                    source_row,
                    &row,
                    Reason::ConflictingMarket,
                )?;
            }
            Some(market) if !emitted.insert(market.condition_id.clone()) => {
                sink.quarantine(
                    HistoryInputKind::Markets,
                    source_row,
                    &row,
                    Reason::DuplicateMarket,
                )?;
            }
            Some(market) => sink.write(HistoryRecord::Market(market))?,
        }
    }
    Ok(candidates
        .into_iter()
        .filter_map(|(id, market)| market.map(|market| (id, market)))
        .collect())
}

fn log_identity(row: &StringRecord, fields: &Fields) -> Option<(String, u64)> {
    let transaction = value(row, fields, "transactionHash")?;
    validate_chain_hex(transaction, 64).ok()?;
    let raw = value(row, fields, "logIndex")?;
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((transaction.to_owned(), raw.parse().ok()?))
}

fn amount(raw: &str) -> std::result::Result<Decimal, Reason> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Reason::InvalidAmount);
    }
    let mut value = Decimal::from_str_exact(raw).map_err(|_| Reason::InvalidAmount)?;
    if value <= Decimal::ZERO {
        return Err(Reason::InvalidAmount);
    }
    value.set_scale(6).map_err(|_| Reason::InvalidAmount)?;
    Ok(value)
}

fn fill(
    row: &StringRecord,
    fields: &Fields,
    source_row: u64,
    source: &HistorySource,
    tokens: &BTreeMap<String, &HistoryMarket>,
) -> std::result::Result<HistoryFill, Reason> {
    if !bounded_row(row, fields) {
        return Err(Reason::MalformedFill);
    }
    let get = |name| value(row, fields, name).ok_or(Reason::MalformedFill);
    let maker = get("maker")?;
    let taker = get("taker")?;
    validate_chain_hex(maker, 40).map_err(|_| Reason::MalformedFill)?;
    validate_chain_hex(taker, 40).map_err(|_| Reason::MalformedFill)?;
    validate_chain_hex(get("transactionHash")?, 64).map_err(|_| Reason::MalformedFill)?;
    if maker == V2_EXCHANGE || taker == V2_EXCHANGE {
        return Err(Reason::IntermediaryMintBurn);
    }
    let timestamp = get("timestamp")?;
    if !timestamp.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Reason::InvalidTimestamp);
    }
    let block_time_unix: u64 = timestamp.parse().map_err(|_| Reason::InvalidTimestamp)?;
    if block_time_unix > source.retrieved_at_unix || block_time_unix < V2_GENESIS_DATE_UNIX {
        return Err(Reason::InvalidTimestamp);
    }
    if block_time_unix < source.requested_start_unix || block_time_unix >= source.requested_end_unix
    {
        return Err(Reason::OutsideRequestedWindow);
    }
    let maker_asset = get("makerAssetId")?;
    let taker_asset = get("takerAssetId")?;
    let (token_id, cash, quantity, maker_direction) = match (maker_asset == "0", taker_asset == "0")
    {
        (true, false) => (
            taker_asset,
            get("makerAmountFilled")?,
            get("takerAmountFilled")?,
            MakerDirection::Buy,
        ),
        (false, true) => (
            maker_asset,
            get("takerAmountFilled")?,
            get("makerAmountFilled")?,
            MakerDirection::Sell,
        ),
        _ => return Err(Reason::MalformedFill),
    };
    let market = tokens.get(token_id).ok_or(Reason::UnmappedToken)?;
    let log_index = match value(row, fields, "logIndex") {
        None => None,
        Some("") => None,
        Some(_) => Some(log_identity(row, fields).ok_or(Reason::MalformedFill)?.1),
    };
    let cash_amount = amount(cash)?;
    let token_amount = amount(quantity)?;
    let fill = HistoryFill {
        source_row,
        condition_id: market.condition_id.clone(),
        token_id: token_id.to_owned(),
        outcome: market.outcomes[token_id].clone(),
        block_time_unix,
        historical_available_at_unix: None,
        transaction_hash: get("transactionHash")?.to_owned(),
        log_index,
        maker_direction,
        cash_amount,
        token_amount,
        price: cash_amount
            .checked_div(token_amount)
            .ok_or(Reason::InvalidAmount)?,
    };
    fill.validate(source, market)
        .map_err(|_| Reason::InvalidAmount)?;
    Ok(fill)
}

fn load_fills(
    input: &mut File,
    source: &HistorySource,
    markets: &BTreeMap<String, HistoryMarket>,
    sink: &mut Sink,
) -> Result<()> {
    let tokens = markets
        .values()
        .flat_map(|market| {
            market
                .outcomes
                .keys()
                .map(move |token| (token.clone(), market))
        })
        .collect();
    let (mut reader, fields) = csv_reader(input, FILL_FIELDS)?;
    let mut log_digests: BTreeMap<(String, u64), Option<String>> = BTreeMap::new();
    for (index, row) in reader.records().enumerate() {
        check_row_limit(index, source.max_input_rows)?;
        let row = row.context("malformed CSV encoding in fills")?;
        if let Some(identity) = log_identity(&row, &fields) {
            let digest = Some(fields_sha256(&row));
            log_digests
                .entry(identity)
                .and_modify(|existing| {
                    if *existing != digest {
                        *existing = None;
                    }
                })
                .or_insert(digest);
        }
    }
    let (mut reader, fields) = csv_reader(input, FILL_FIELDS)?;
    let mut seen = BTreeSet::new();
    for (index, row) in reader.records().enumerate() {
        let source_row = check_row_limit(index, source.max_input_rows)?;
        let row = row?;
        let identity = log_identity(&row, &fields);
        let result = if identity
            .as_ref()
            .is_some_and(|id| log_digests.get(id) == Some(&None))
        {
            Err(Reason::ConflictingLogIdentity)
        } else {
            fill(&row, &fields, source_row, source, &tokens)
        };
        match result {
            Ok(_) if identity.is_some_and(|identity| !seen.insert(identity)) => {
                sink.quarantine(
                    HistoryInputKind::Fills,
                    source_row,
                    &row,
                    Reason::DuplicateLogIdentity,
                )?;
            }
            Ok(fill) => sink.write(HistoryRecord::Fill(fill))?,
            Err(reason) => sink.quarantine(HistoryInputKind::Fills, source_row, &row, reason)?,
        }
    }
    Ok(())
}

struct PreparedHistory {
    content: Vec<u8>,
    manifest: HistoryManifest,
}

fn prepare(config: &PolyDataImportConfig) -> Result<PreparedHistory> {
    if config.source_schema != POLY_DATA_SCHEMA
        || config.source_revision != POLY_DATA_REVISION
        || config.max_input_bytes == 0
        || config.max_input_bytes > 256 * 1024 * 1024
        || config.max_input_rows == 0
        || config.max_input_rows > HISTORY_MAX_ROWS
    {
        bail!("unsupported poly_data source revision/schema or input bounds");
    }
    let mut markets = snapshot(
        &config.markets,
        &config.markets_sha256,
        config.max_input_bytes,
    )?;
    let mut fills = snapshot(&config.fills, &config.fills_sha256, config.max_input_bytes)?;
    let source = HistorySource {
        schema: config.source_schema.clone(),
        revision: config.source_revision.clone(),
        chain_id: 137,
        exchange_contract: V2_EXCHANGE.to_owned(),
        markets: markets.source,
        fills: fills.source,
        retrieved_at_unix: config.retrieved_at_unix,
        imported_at_unix: config.imported_at_unix,
        requested_start_unix: config.start_unix,
        requested_end_unix: config.end_unix,
        max_input_bytes: config.max_input_bytes,
        max_input_rows: config.max_input_rows,
    };
    source.validate()?;
    let mut sink = Sink::new()?;
    let mappings = load_markets(&mut markets.file, config.max_input_rows, &mut sink)?;
    load_fills(&mut fills.file, &source, &mappings, &mut sink)?;
    sink.file.flush()?;
    sink.file.seek(SeekFrom::Start(0))?;
    // Input and normalization stream through private files. Only the final,
    // independently capped 64 MiB artifact is buffered for the existing publisher.
    let mut content = Vec::new();
    sink.file
        .get_mut()
        .take(HISTORY_MAX_BYTES + 1)
        .read_to_end(&mut content)?;
    if content.len() as u64 > HISTORY_MAX_BYTES {
        bail!("history output exceeded byte bound");
    }
    let manifest = HistoryManifest {
        schema: HISTORY_SCHEMA.to_owned(),
        evidence_scope: HISTORY_SCOPE.to_owned(),
        source,
        content_sha256: history_sha256(&content),
        content_bytes: content.len() as u64,
        quality: sink.quality,
        l2_available: false,
        executable_quotes_available: false,
        settlement_available: false,
        trade_completion_certified: false,
        coverage_certified: false,
        research_promotion_allowed: false,
    };
    manifest.validate()?;
    Ok(PreparedHistory { content, manifest })
}

pub fn import_poly_data(config: &PolyDataImportConfig) -> Result<PublishedPolyDataHistory> {
    let prepared = prepare(config)?;
    let manifest_bytes = serde_json::to_vec_pretty(&prepared.manifest)?;
    let manifest_sha256 = history_sha256(&manifest_bytes);
    ensure_canonical_directory(&config.output_root)?;
    // Provenance, window and clocks join the content in the immutable identity.
    let directory = config.output_root.join(format!("sha256={manifest_sha256}"));
    ensure_canonical_directory(&directory)?;
    let success = format!("{}\n", prepared.manifest.content_sha256);
    let publication = publish_triplet(
        &directory.join(HISTORY_DATA_FILE),
        &directory.join(HISTORY_MANIFEST_FILE),
        &directory.join(HISTORY_SUCCESS_FILE),
        &ArtifactBytes {
            data: &prepared.content,
            manifest: &manifest_bytes,
            success: success.as_bytes(),
        },
    )?;
    let verified = verify_history(&directory, &manifest_sha256)?;
    Ok(PublishedPolyDataHistory {
        directory,
        manifest_sha256,
        content_sha256: verified.manifest().content_sha256.clone(),
        publication,
        quality: verified.manifest().quality.clone(),
        evidence_scope: HISTORY_SCOPE.to_owned(),
        research_promotion_allowed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::str::FromStr;

    // Synthetic fixtures exercise the pinned CSV shape. They are never research evidence.
    const START: u64 = 1_789_000_000;
    const TOKEN_UP: &str =
        "115792089237316195423570985008687907853269984665640564039457584007913129639935";
    const TOKEN_DOWN: &str = "900719925474099300000000000000000000001";

    fn condition() -> String {
        format!("0x{}", "a".repeat(64))
    }
    fn maker() -> String {
        format!("0x{}", "1".repeat(40))
    }
    fn taker() -> String {
        format!("0x{}", "2".repeat(40))
    }
    fn transaction() -> String {
        format!("0x{}", "b".repeat(64))
    }

    fn market_fields() -> Vec<String> {
        vec![
            condition(),
            json!([TOKEN_DOWN, TOKEN_UP]).to_string(),
            json!([
                {"token_id": TOKEN_UP, "outcome": "Up", "winner": true},
                {"token_id": TOKEN_DOWN, "outcome": "Down", "winner": false}
            ])
            .to_string(),
            "btc-updown-5m-synthetic,with\nquoted title".into(),
        ]
    }

    fn buy() -> Vec<String> {
        vec![
            START.to_string(),
            maker(),
            "0".into(),
            "4000000".into(),
            taker(),
            TOKEN_UP.into(),
            "10000000".into(),
            transaction(),
        ]
    }

    fn write_csv(path: &Path, headers: &[&str], rows: &[Vec<String>]) {
        let mut writer = csv::Writer::from_path(path).unwrap();
        writer.write_record(headers).unwrap();
        for row in rows {
            writer.write_record(row).unwrap();
        }
        writer.flush().unwrap();
    }

    fn fixture(rows: &[Vec<String>], indexed: bool) -> (tempfile::TempDir, PolyDataImportConfig) {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let markets = root.join("markets.csv");
        let fills = root.join("orderFilled.csv");
        write_csv(
            &markets,
            &["id", "clobTokenIds", "tokens", "market_slug"],
            &[market_fields()],
        );
        let mut headers = FILL_FIELDS.to_vec();
        if indexed {
            headers.push("logIndex");
        }
        write_csv(&fills, &headers, rows);
        let config = PolyDataImportConfig {
            source_schema: POLY_DATA_SCHEMA.into(),
            source_revision: POLY_DATA_REVISION.into(),
            markets_sha256: history_sha256(&fs::read(&markets).unwrap()),
            fills_sha256: history_sha256(&fs::read(&fills).unwrap()),
            markets,
            fills,
            retrieved_at_unix: START + 3600,
            imported_at_unix: START + 7200,
            start_unix: START,
            end_unix: START + 300,
            max_input_bytes: 1024 * 1024,
            max_input_rows: 100,
            output_root: root.join("output"),
        };
        (temp, config)
    }

    fn fills(prepared: &PreparedHistory) -> Vec<HistoryFill> {
        prepared
            .content
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .filter_map(
                |line| match serde_json::from_slice::<HistoryRecord>(line).unwrap() {
                    HistoryRecord::Fill(fill) => Some(fill),
                    _ => None,
                },
            )
            .collect()
    }

    fn stage_for_readback(config: &PolyDataImportConfig, prepared: &PreparedHistory) -> String {
        fs::create_dir_all(&config.output_root).unwrap();
        let manifest = serde_json::to_vec_pretty(&prepared.manifest).unwrap();
        fs::write(config.output_root.join(HISTORY_MANIFEST_FILE), &manifest).unwrap();
        fs::write(
            config.output_root.join(HISTORY_DATA_FILE),
            &prepared.content,
        )
        .unwrap();
        fs::write(
            config.output_root.join(HISTORY_SUCCESS_FILE),
            format!("{}\n", prepared.manifest.content_sha256),
        )
        .unwrap();
        history_sha256(&manifest)
    }

    #[test]
    fn poly_data_maps_maker_direction_and_exact_amounts_by_token_identity() {
        let mut sell = buy();
        sell[2] = TOKEN_DOWN.into();
        sell[3] = "1234567".into();
        sell[5] = "0".into();
        sell[6] = "617283".into();
        let (_temp, config) = fixture(&[buy(), sell], false);
        let prepared = prepare(&config).unwrap();
        let rows = fills(&prepared);
        assert_eq!(rows[0].maker_direction, MakerDirection::Buy);
        assert_eq!(rows[0].token_id, TOKEN_UP);
        assert_eq!(rows[0].outcome, "Up");
        assert_eq!(rows[0].price, Decimal::from_str("0.4").unwrap());
        assert_eq!(rows[1].maker_direction, MakerDirection::Sell);
        assert_eq!(rows[1].outcome, "Down");
        assert_eq!(rows[1].token_amount, Decimal::from_str("1.234567").unwrap());
        assert_eq!(rows[1].cash_amount, Decimal::from_str("0.617283").unwrap());
        assert!(rows
            .iter()
            .all(|row| row.historical_available_at_unix.is_none()));
        // A winner field in current metadata must never become a settlement label.
        assert!(!prepared.manifest.settlement_available);
    }

    #[test]
    fn poly_data_preserves_same_transaction_and_identical_unindexed_occurrences() {
        let (_temp, config) = fixture(&[buy(), buy(), buy()], false);
        let first = prepare(&config).unwrap();
        let second = prepare(&config).unwrap();
        assert_eq!(fills(&first).len(), 3);
        assert_eq!(first.manifest.quality.ambiguous_identity_rows, 3);
        assert_eq!(first.content, second.content);
        assert_eq!(first.manifest, second.manifest);
        assert_eq!(
            fills(&first)
                .iter()
                .map(|row| row.source_row)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn poly_data_deduplicates_only_exact_chain_logs_and_quarantines_all_conflicts() {
        let mut first = buy();
        first.push("5".into());
        let mut distinct = first.clone();
        distinct[8] = "6".into();
        let mut conflict = first.clone();
        conflict[8] = "7".into();
        let mut changed = conflict.clone();
        changed[3] = "5000000".into();
        let (_temp, config) = fixture(&[first.clone(), first, distinct, conflict, changed], true);
        let prepared = prepare(&config).unwrap();
        assert_eq!(fills(&prepared).len(), 2);
        assert_eq!(
            prepared.manifest.quality.reasons[&Reason::DuplicateLogIdentity],
            1
        );
        assert_eq!(
            prepared.manifest.quality.reasons[&Reason::ConflictingLogIdentity],
            2
        );
        assert_eq!(prepared.manifest.quality.ambiguous_identity_rows, 0);
    }

    #[test]
    fn poly_data_quarantines_invalid_amount_time_mapping_and_intermediary() {
        let cases = [
            (3, "0", Reason::InvalidAmount),
            (3, "10000001", Reason::InvalidAmount),
            (3, "NaN", Reason::InvalidAmount),
            (3, "1e6", Reason::InvalidAmount),
            (3, "79228162514264337593543950336", Reason::InvalidAmount),
            (0, "1789000000.5", Reason::InvalidTimestamp),
            (0, "1789000000000", Reason::InvalidTimestamp),
            (0, "1789000300", Reason::OutsideRequestedWindow),
            (5, "777", Reason::UnmappedToken),
            (4, V2_EXCHANGE, Reason::IntermediaryMintBurn),
            (2, "22", Reason::MalformedFill),
        ];
        for (column, value, reason) in cases {
            let mut row = buy();
            row[column] = value.into();
            let (_temp, config) = fixture(&[row], false);
            let prepared = prepare(&config).unwrap();
            assert!(fills(&prepared).is_empty(), "{value}");
            assert_eq!(
                prepared.manifest.quality.reasons.get(&reason),
                Some(&1),
                "{value}"
            );
        }
    }

    #[test]
    fn poly_data_rejects_unknown_schema_revision_and_bounded_input_failures() {
        let (_temp, mut config) = fixture(&[buy()], false);
        config.max_input_rows = 0;
        assert!(prepare(&config).is_err());
        config.max_input_rows = 1;
        config.max_input_bytes = 5;
        assert!(prepare(&config).is_err());
        config.max_input_bytes = 1024 * 1024;
        config.source_revision = "0".repeat(40);
        assert!(prepare(&config).is_err());
        config.source_revision = POLY_DATA_REVISION.into();
        config.source_schema = "poly_data.processed_trades.v2".into();
        assert!(prepare(&config).is_err());
        config.source_schema = POLY_DATA_SCHEMA.into();
        config.fills_sha256 = "0".repeat(64);
        assert!(prepare(&config).is_err());
    }

    #[test]
    fn poly_data_row_limit_is_fail_closed_without_partial_publication() {
        let (_temp, mut config) = fixture(&[buy(), buy()], false);
        config.max_input_rows = 1;
        assert!(import_poly_data(&config)
            .unwrap_err()
            .to_string()
            .contains("row limit"));
        assert!(!config.output_root.exists());
    }

    #[test]
    fn poly_data_rejects_ambiguous_market_and_token_mappings() {
        let (_temp, mut config) = fixture(&[buy()], false);
        let first = market_fields();
        let mut conflicting = first.clone();
        conflicting[2] = json!([
            {"token_id": TOKEN_UP, "outcome": "Down"},
            {"token_id": TOKEN_DOWN, "outcome": "Up"}
        ])
        .to_string();
        write_csv(
            &config.markets,
            &["id", "clobTokenIds", "tokens", "market_slug"],
            &[first, conflicting],
        );
        config.markets_sha256 = history_sha256(&fs::read(&config.markets).unwrap());
        let prepared = prepare(&config).unwrap();
        assert_eq!(prepared.manifest.quality.markets, 0);
        assert_eq!(
            prepared.manifest.quality.reasons[&Reason::ConflictingMarket],
            2
        );
        assert_eq!(prepared.manifest.quality.reasons[&Reason::UnmappedToken], 1);
    }

    #[test]
    fn poly_data_rejects_missing_and_duplicate_csv_headers() {
        for bytes in [
            "timestamp,price\n1789000000,0.5\n",
            "timestamp,timestamp\n1,2\n",
        ] {
            let (_temp, mut config) = fixture(&[buy()], false);
            fs::write(&config.fills, bytes).unwrap();
            config.fills_sha256 = history_sha256(bytes.as_bytes());
            assert!(prepare(&config).is_err());
        }
    }

    #[test]
    fn poly_data_rejects_unterminated_quotes_and_oversized_logical_records() {
        for tail in [
            "\"unterminated".to_owned(),
            format!("\"{}\"", "x".repeat(HISTORY_ROW_MAX_BYTES)),
        ] {
            let (_temp, mut config) = fixture(&[buy()], false);
            let bytes = format!("{}\n{tail}", FILL_FIELDS.join(","));
            fs::write(&config.fills, &bytes).unwrap();
            config.fills_sha256 = history_sha256(bytes.as_bytes());
            assert!(prepare(&config).is_err());
            assert!(!config.output_root.exists());
        }
    }

    #[test]
    fn poly_data_readback_rejects_tampering_quality_claims_and_false_l2_promotion() {
        let (_temp, config) = fixture(&[buy()], false);
        let mut prepared = prepare(&config).unwrap();
        let anchor = stage_for_readback(&config, &prepared);
        let verified = verify_history(&config.output_root, &anchor).unwrap();
        assert_eq!(verified.manifest().quality.accepted_fill_rows, 1);
        use crate::polymarket_research_import::{
            validate_research_segments, ArtifactTriplet, ResearchSegmentValidationConfig,
        };
        assert!(
            validate_research_segments(&ResearchSegmentValidationConfig {
                market: ArtifactTriplet {
                    data: config.output_root.join(HISTORY_DATA_FILE),
                    manifest: config.output_root.join(HISTORY_MANIFEST_FILE),
                    success: config.output_root.join(HISTORY_SUCCESS_FILE),
                },
                references: Vec::new(),
            })
            .is_err()
        );
        assert!(verify_history(&config.output_root, &"0".repeat(64)).is_err());
        fs::write(config.output_root.join(HISTORY_DATA_FILE), b"{}\n").unwrap();
        assert!(verify_history(&config.output_root, &anchor).is_err());
        prepared.manifest.quality.accepted_fill_rows += 1;
        let anchor = stage_for_readback(&config, &prepared);
        assert!(verify_history(&config.output_root, &anchor).is_err());
        prepared.manifest.quality.accepted_fill_rows -= 1;
        prepared.manifest.l2_available = true;
        let anchor = stage_for_readback(&config, &prepared);
        assert!(verify_history(&config.output_root, &anchor).is_err());
        prepared.manifest.l2_available = false;
        prepared.manifest.trade_completion_certified = true;
        let anchor = stage_for_readback(&config, &prepared);
        assert!(verify_history(&config.output_root, &anchor).is_err());
        prepared.manifest.trade_completion_certified = false;
        prepared.manifest.research_promotion_allowed = true;
        let anchor = stage_for_readback(&config, &prepared);
        assert!(verify_history(&config.output_root, &anchor).is_err());
    }

    #[test]
    fn poly_data_rejects_source_and_artifact_symlinks() {
        let (_temp, mut config) = fixture(&[buy()], false);
        let prepared = prepare(&config).unwrap();
        let anchor = stage_for_readback(&config, &prepared);
        let link = config.fills.with_file_name("linked.csv");
        std::os::unix::fs::symlink(&config.fills, &link).unwrap();
        config.fills = link;
        assert!(prepare(&config).is_err());
        let data = config.output_root.join(HISTORY_DATA_FILE);
        fs::remove_file(&data).unwrap();
        std::os::unix::fs::symlink(&config.markets, &data).unwrap();
        assert!(verify_history(&config.output_root, &anchor).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn poly_data_publication_restart_is_unchanged_and_resumes_without_success_marker() {
        let (_temp, config) = fixture(&[buy(), buy()], false);
        let first = import_poly_data(&config).unwrap();
        let second = import_poly_data(&config).unwrap();
        assert_eq!(first.publication, ImmutablePublicationOutcome::Published);
        assert_eq!(second.publication, ImmutablePublicationOutcome::Unchanged);
        assert_eq!(first.manifest_sha256, second.manifest_sha256);
        fs::remove_file(first.directory.join(HISTORY_SUCCESS_FILE)).unwrap();
        let resumed = import_poly_data(&config).unwrap();
        assert_eq!(resumed.publication, ImmutablePublicationOutcome::Published);
        assert_eq!(first.manifest_sha256, resumed.manifest_sha256);
        fs::set_permissions(
            first.directory.join(HISTORY_DATA_FILE),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        fs::write(first.directory.join(HISTORY_DATA_FILE), b"corrupt\n").unwrap();
        assert!(import_poly_data(&config).is_err());
    }
}
