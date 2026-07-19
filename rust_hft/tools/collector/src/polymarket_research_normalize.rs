use crate::polymarket_research_import::{ResearchSegmentValidationReport, TradeCompletionIdentity};
use crate::polymarket_research_select::{
    decimal_text, json_strings, required, timestamp, utc_text, visit,
    with_selected_research_contracts, ResearchSelectionConfig, SelectedContract, SYMBOLS,
    WINDOW_SECS,
};
use crate::polymarket_upload::{
    trade_record_ids_sha256, validate_canonical_trade, validate_market_settlement,
};
use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::str::FromStr;

const CHAINLINK_OPEN_LOOKBACK_SECS: i64 = 30;
const EVIDENCE_TRUST_BOUNDARY: &str = "typed collector staging evidence only; not an evaluator label snapshot or snapshot_contract_hash; validated staged triplets and adjacent local supersession markers; omitted remote-prefix markers are not proven absent";

pub type PolymarketEvidenceConfig = ResearchSelectionConfig;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolymarketEvidenceReport {
    pub schema: &'static str,
    /// Digest of the canonical evidence bytes; never a `snapshot_contract_hash`.
    pub content_sha256: String,
    pub content_bytes: u64,
    pub rows: u64,
    pub events: u64,
    pub surface_counts: BTreeMap<String, u64>,
    pub event_start_gte: String,
    pub event_start_lt: String,
    pub symbols: [&'static str; 2],
    pub window_secs: u64,
    pub event_selection: &'static str,
    pub trust_boundary: &'static str,
    pub validated_inputs: ResearchSegmentValidationReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedPolymarketEvidence {
    pub report: PolymarketEvidenceReport,
    pub ndjson: Vec<u8>,
}

#[derive(Clone)]
struct Pending {
    line: usize,
    sequence: u64,
    recorded_at: String,
    update: Map<String, Value>,
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
enum SurfaceOrder {
    MarketContract,
    OrderbookSnapshot,
    ChainlinkReference,
    PolymarketTrade,
    OfficialSettlementEvidence,
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct RowKey {
    surface: SurfaceOrder,
    event_start: String,
    symbol: String,
    market_id: String,
    clock: String,
    identity: String,
    sequence: u64,
}

struct Row {
    key: RowKey,
    value: Value,
}

fn reference_records(
    path: &Path,
    contracts: &BTreeMap<String, SelectedContract>,
) -> Result<(Vec<Pending>, Vec<Pending>)> {
    let mut trades = Vec::new();
    let mut settlements = Vec::new();
    visit(path, |line, sequence, recorded_at, update| {
        let Some(market_id) = update.get("market_id").and_then(Value::as_str) else {
            return Ok(());
        };
        if !contracts.contains_key(market_id) {
            return Ok(());
        }
        let pending = || Pending {
            line,
            sequence,
            recorded_at: recorded_at.to_owned(),
            update: update.clone(),
        };
        match update.get("kind").and_then(Value::as_str) {
            Some("polymarket_trade") => trades.push(pending()),
            Some("market_settlement") => settlements.push(pending()),
            _ => {}
        }
        Ok(())
    })?;
    Ok((trades, settlements))
}

fn base(contract: &SelectedContract, surface: &str) -> Value {
    let metadata = contract
        .metadata
        .as_ref()
        .expect("metadata completeness checked");
    json!({
        "schema": "monday.polymarket.evidence_row.v1",
        "surface": surface,
        "market_id": contract.market_id,
        "condition_id": metadata.condition_id,
        "symbol": contract.symbol,
        "event_start": utc_text(contract.event_start),
        "event_end": utc_text(contract.event_end),
        "window_secs": WINDOW_SECS
    })
}

fn insert(base: &mut Value, fields: Value) {
    base.as_object_mut()
        .expect("base row is an object")
        .extend(fields.as_object().expect("fields are an object").clone());
}

fn row(
    surface: SurfaceOrder,
    contract: &SelectedContract,
    clock: impl Into<String>,
    identity: impl Into<String>,
    sequence: u64,
    value: Value,
) -> Row {
    Row {
        key: RowKey {
            surface,
            event_start: utc_text(contract.event_start),
            symbol: contract.symbol.clone(),
            market_id: contract.market_id.clone(),
            clock: clock.into(),
            identity: identity.into(),
            sequence,
        },
        value,
    }
}

fn latest(left: &str, right: &str) -> Result<String> {
    Ok(
        if timestamp(left, "availability clock")? >= timestamp(right, "availability clock")? {
            left
        } else {
            right
        }
        .to_owned(),
    )
}

fn selected_reference(
    update: &Map<String, Value>,
    line: usize,
) -> Result<Option<(String, &'static str)>> {
    let source_symbol = required(update, "symbol")?.to_ascii_lowercase();
    let symbol = match source_symbol.as_str() {
        "btc/usd" => "BTCUSDT",
        "sol/usd" => "SOLUSDT",
        _ => return Ok(None),
    };
    if update.get("source").and_then(Value::as_str) != Some("chainlink")
        || update.get("asset_class").and_then(Value::as_str) != Some("crypto")
        || update
            .get("is_carried_forward")
            .and_then(Value::as_bool)
            .is_none()
    {
        bail!("line {line}: selected reference price is not typed Chainlink crypto");
    }
    Ok(Some((source_symbol, symbol)))
}

fn reference_belongs_to_contract(source_ts: DateTime<Utc>, contract: &SelectedContract) -> bool {
    source_ts >= contract.event_start - chrono::Duration::seconds(CHAINLINK_OPEN_LOOKBACK_SECS)
        && source_ts < contract.event_end
}

fn record_settlement_fingerprint(
    settled: &mut BTreeMap<String, String>,
    market_id: &str,
    fingerprint: &str,
    line: usize,
) -> Result<bool> {
    match settled.get(market_id) {
        Some(existing) if existing != fingerprint => {
            bail!("line {line}: settlement payload changed for market {market_id}")
        }
        Some(_) => Ok(false),
        None => {
            settled.insert(market_id.to_owned(), fingerprint.to_owned());
            Ok(true)
        }
    }
}

fn settlement_fingerprint(pending: &Pending) -> Result<String> {
    let canonical = json!({
        "sequence": pending.sequence,
        "recorded_at": pending.recorded_at,
        "update": pending.update
    });
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&canonical)?)))
}

fn contract_rows(
    contracts: &BTreeMap<String, SelectedContract>,
    rows: &mut Vec<Row>,
) -> Result<()> {
    for contract in contracts.values() {
        let metadata = contract
            .metadata
            .as_ref()
            .expect("metadata completeness checked");
        let metadata_available = latest(&metadata.retrieved_at, &metadata.recorded_at)?;
        let available_at = latest(&contract.discovery_recorded_at, &metadata_available)?;
        let mut value = base(contract, "market_contract");
        insert(
            &mut value,
            json!({
                "source_token_ids": metadata.tokens,
                "source_outcomes": metadata.outcomes,
                "price_to_beat": contract.price_to_beat,
                "resolution_source": metadata.resolution_source,
                "metadata_retrieved_at": metadata.retrieved_at,
                "discovery_recorded_at": contract.discovery_recorded_at,
                "metadata_recorded_at": metadata.recorded_at,
                "available_at": available_at,
                "discovery_source_sequence": contract.discovery_sequence,
                "metadata_source_sequence": metadata.sequence,
                "source_datasets": ["crypto_expiry", "crypto_expiry_reference"]
            }),
        );
        rows.push(row(
            SurfaceOrder::MarketContract,
            contract,
            "",
            "",
            0,
            value,
        ));
    }
    Ok(())
}

fn trade_rows(
    pending: Vec<Pending>,
    contracts: &BTreeMap<String, SelectedContract>,
    rows: &mut Vec<Row>,
    record_ids: &mut BTreeMap<String, BTreeSet<String>>,
) -> Result<()> {
    for pending in pending {
        let record_id = validate_canonical_trade(&pending.update, pending.line)?;
        let contract = &contracts[required(&pending.update, "market_id")?];
        let metadata = contract
            .metadata
            .as_ref()
            .expect("metadata completeness checked");
        let index = pending
            .update
            .get("outcome_index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value < 2)
            .ok_or_else(|| anyhow!("line {}: invalid outcome_index", pending.line))?;
        let token = required(&pending.update, "token_id")?;
        let outcome = required(&pending.update, "outcome")?;
        if required(&pending.update, "condition_id")? != metadata.condition_id
            || required(&pending.update, "symbol")? != contract.symbol
            || required(&pending.update, "source")? != "polymarket_data_api"
            || pending
                .update
                .get("market_window_secs")
                .and_then(Value::as_u64)
                != Some(WINDOW_SECS as u64)
            || token != metadata.tokens[index]
            || outcome != metadata.outcomes[index]
        {
            bail!(
                "line {}: trade contradicts selected market context",
                pending.line
            );
        }
        let trade_ts = required(&pending.update, "trade_ts")?;
        let received_at = required(&pending.update, "received_at")?;
        let source_clock = timestamp(trade_ts, "trade_ts")?;
        let received_clock = timestamp(received_at, "received_at")?;
        let recorded_clock = timestamp(&pending.recorded_at, "recorded_at")?;
        if received_clock < source_clock || recorded_clock < source_clock {
            continue;
        }
        let trade_ts = trade_ts.to_owned();
        let mut value = base(contract, "polymarket_trade");
        insert(
            &mut value,
            json!({
                "record_id": record_id,
                "record_id_version": "v2",
                "token_id": token,
                "source_outcome": outcome,
                "outcome_index": index,
                "side": required(&pending.update, "side")?,
                "size": decimal_text(pending.update.get("size"), "trade size")?,
                "price": decimal_text(pending.update.get("price"), "trade price")?,
                "trade_ts": trade_ts,
                "trade_ts_unix": pending.update["trade_ts_unix"],
                "transaction_hash": required(&pending.update, "transaction_hash")?,
                "proxy_wallet": required(&pending.update, "proxy_wallet")?,
                "source": required(&pending.update, "source")?,
                "received_at": received_at,
                "available_at": latest(received_at, &pending.recorded_at)?,
                "recorded_at": pending.recorded_at,
                "source_sequence": pending.sequence,
                "source_dataset": "crypto_expiry_reference"
            }),
        );
        record_ids
            .entry(contract.market_id.clone())
            .or_default()
            .insert(record_id.clone());
        rows.push(row(
            SurfaceOrder::PolymarketTrade,
            contract,
            trade_ts,
            record_id,
            pending.sequence,
            value,
        ));
    }
    Ok(())
}

fn settlement_rows(
    pending: Vec<Pending>,
    contracts: &BTreeMap<String, SelectedContract>,
    rows: &mut Vec<Row>,
    settled: &mut BTreeMap<String, String>,
) -> Result<()> {
    for pending in pending {
        validate_market_settlement(&pending.update, pending.line)?;
        let market_id = required(&pending.update, "market_id")?;
        let contract = &contracts[market_id];
        let metadata = contract
            .metadata
            .as_ref()
            .expect("metadata completeness checked");
        let market = pending.update["market"]
            .as_object()
            .expect("settlement validator checked market");
        let tokens = json_strings(market.get("clobTokenIds"), "clobTokenIds")?;
        let outcomes = json_strings(market.get("outcomes"), "outcomes")?;
        if required(&pending.update, "condition_id")? != metadata.condition_id
            || tokens != metadata.tokens
            || outcomes != metadata.outcomes
        {
            bail!(
                "line {}: settlement contradicts selected metadata",
                pending.line
            );
        }
        if required(&pending.update, "resolution_source")? != "gamma_api_closed_market" {
            bail!(
                "line {}: settlement is not from the official closed-market source",
                pending.line
            );
        }
        let winner = required(&pending.update, "winning_token_id")?.to_owned();
        let fingerprint = settlement_fingerprint(&pending)?;
        if record_settlement_fingerprint(settled, market_id, &fingerprint, pending.line)? {
            let prices = json_strings(market.get("outcomePrices"), "outcomePrices")?;
            let mut value = base(contract, "official_settlement_evidence");
            insert(
                &mut value,
                json!({
                    "source_token_ids": tokens,
                    "source_outcomes": outcomes,
                    "source_outcome_prices": prices,
                    "winning_token_id": winner,
                    "winning_outcome": required(&pending.update, "winning_outcome")?,
                    "resolution_source": required(&pending.update, "resolution_source")?,
                    "retrieved_at": required(&pending.update, "retrieved_at")?,
                    "available_at": latest(required(&pending.update, "retrieved_at")?, &pending.recorded_at)?,
                    "recorded_at": pending.recorded_at,
                    "source_sequence": pending.sequence,
                    "source_dataset": "crypto_expiry_reference"
                }),
            );
            rows.push(row(
                SurfaceOrder::OfficialSettlementEvidence,
                contract,
                utc_text(contract.event_end),
                "",
                pending.sequence,
                value,
            ));
        }
    }
    Ok(())
}

fn paired_quote_fields(
    update: &Map<String, Value>,
    price_field: &str,
    size_field: &str,
) -> (Value, Value) {
    match (update.get(price_field), update.get(size_field)) {
        (Some(price), Some(size)) if !price.is_null() && !size.is_null() => {
            (price.clone(), size.clone())
        }
        _ => (Value::Null, Value::Null),
    }
}

fn market_rows(
    path: &Path,
    contracts: &BTreeMap<String, SelectedContract>,
    rows: &mut Vec<Row>,
    quote_tokens: &mut BTreeMap<String, BTreeSet<String>>,
    references: &mut BTreeMap<String, u64>,
) -> Result<()> {
    let token_market = contracts
        .values()
        .flat_map(|contract| {
            [
                (contract.up_token.clone(), contract.market_id.clone()),
                (contract.down_token.clone(), contract.market_id.clone()),
            ]
        })
        .collect::<BTreeMap<_, _>>();
    visit(path, |line, sequence, recorded_at, update| {
        match update.get("kind").and_then(Value::as_str) {
            Some("quote") => {
                let token = required(update, "token_id")?;
                let Some(market_id) = token_market.get(token) else {
                    return Ok(());
                };
                let contract = &contracts[market_id];
                let source_ts = timestamp(required(update, "ts")?, "quote ts")?;
                if source_ts < contract.event_start || source_ts >= contract.event_end {
                    return Ok(());
                }
                let (bid, bid_size) = paired_quote_fields(update, "bid", "bid_size");
                let (ask, ask_size) = paired_quote_fields(update, "ask", "ask_size");
                let mut value = base(contract, "orderbook_snapshot");
                insert(
                    &mut value,
                    json!({
                        "token_id": token,
                        "ts": required(update, "ts")?,
                        "recorded_at": recorded_at,
                        "available_at": recorded_at,
                        "source_sequence": sequence,
                        "source_dataset": "crypto_expiry",
                        "bid": bid,
                        "ask": ask,
                        "bid_size": bid_size,
                        "ask_size": ask_size,
                        "bid_levels": update.get("bid_levels").cloned().unwrap_or(Value::Null),
                        "ask_levels": update.get("ask_levels").cloned().unwrap_or(Value::Null)
                    }),
                );
                rows.push(row(
                    SurfaceOrder::OrderbookSnapshot,
                    contract,
                    required(update, "ts")?,
                    token,
                    sequence,
                    value,
                ));
                quote_tokens
                    .entry(contract.market_id.clone())
                    .or_default()
                    .insert(token.to_owned());
            }
            Some("reference_price") => {
                let Some((source_symbol, symbol)) = selected_reference(update, line)? else {
                    return Ok(());
                };
                let source_ts = timestamp(required(update, "ts")?, "reference ts")?;
                let price = decimal_text(update.get("price"), "reference price")?;
                if Decimal::from_str(&price)? <= Decimal::ZERO {
                    bail!("line {line}: reference price must be positive")
                }
                for contract in contracts.values().filter(|contract| {
                    contract.symbol == symbol && reference_belongs_to_contract(source_ts, contract)
                }) {
                    let received_at = update.get("received_at").and_then(Value::as_str);
                    let available_at = match received_at {
                        Some(received_at) => latest(received_at, recorded_at)?,
                        None => recorded_at.to_owned(),
                    };
                    let mut value = base(contract, "chainlink_reference");
                    insert(
                        &mut value,
                        json!({
                            "source": "chainlink",
                            "asset_class": "crypto",
                            "source_symbol": source_symbol,
                            "price": price,
                            "full_accuracy_value": update.get("full_accuracy_value").cloned().unwrap_or(Value::Null),
                            "is_carried_forward": update["is_carried_forward"],
                            "ts": required(update, "ts")?,
                            "received_at": received_at,
                            "available_at": available_at,
                            "recorded_at": recorded_at,
                            "source_sequence": sequence,
                            "source_dataset": "crypto_expiry"
                        }),
                    );
                    rows.push(row(
                        SurfaceOrder::ChainlinkReference,
                        contract,
                        required(update, "ts")?,
                        "",
                        sequence,
                        value,
                    ));
                    *references.entry(contract.market_id.clone()).or_default() += 1;
                }
            }
            _ => {}
        }
        Ok(())
    })
}

const FORBIDDEN_DERIVED_FIELDS: [&str; 3] = ["outcome", "up_token_id", "down_token_id"];

fn validate_source_neutral_row(value: &Value) -> Result<()> {
    for field in FORBIDDEN_DERIVED_FIELDS {
        if value.get(field).is_some() {
            bail!("evidence row must not include {field}");
        }
    }
    Ok(())
}

fn encode_rows(mut rows: Vec<Row>) -> Result<(Vec<u8>, BTreeMap<String, u64>)> {
    rows.sort_by(|left, right| left.key.cmp(&right.key));
    let mut ndjson = Vec::new();
    let mut surface_counts = BTreeMap::new();
    for row in rows {
        let surface = row.value["surface"]
            .as_str()
            .expect("all rows have a surface");
        validate_source_neutral_row(&row.value)?;
        *surface_counts.entry(surface.to_owned()).or_default() += 1;
        serde_json::to_writer(&mut ndjson, &row.value)?;
        ndjson.push(b'\n');
    }
    Ok((ndjson, surface_counts))
}

fn has_all_surfaces(
    contract: &SelectedContract,
    quote_tokens: &BTreeMap<String, BTreeSet<String>>,
    references: &BTreeMap<String, u64>,
    trades: &BTreeMap<String, BTreeSet<String>>,
    settlements: &BTreeMap<String, String>,
) -> bool {
    let expected = BTreeSet::from([contract.up_token.clone(), contract.down_token.clone()]);
    quote_tokens.get(&contract.market_id) == Some(&expected)
        && references
            .get(&contract.market_id)
            .copied()
            .unwrap_or_default()
            > 0
        && trades
            .get(&contract.market_id)
            .is_some_and(|record_ids| !record_ids.is_empty())
        && settlements.contains_key(&contract.market_id)
}

fn aggregated_trade_completions(
    inputs: &ResearchSegmentValidationReport,
) -> Result<BTreeMap<String, TradeCompletionIdentity>> {
    let mut completions = BTreeMap::new();
    for reference in &inputs.references {
        for (market_id, completion) in &reference.trade_completions {
            if completions
                .insert(market_id.clone(), completion.clone())
                .is_some()
            {
                bail!("selected evidence received duplicate event-local trade completion proofs");
            }
        }
    }
    Ok(completions)
}

fn validate_event_local_trade_completions(
    completions: &BTreeMap<String, TradeCompletionIdentity>,
    contracts: &BTreeMap<String, SelectedContract>,
    trade_record_ids: &BTreeMap<String, BTreeSet<String>>,
) -> Result<()> {
    for contract in contracts.values() {
        let completion = completions.get(&contract.market_id).ok_or_else(|| {
            anyhow!(
                "selected market {} has no event-local trade completion proof",
                contract.market_id
            )
        })?;
        let metadata = contract
            .metadata
            .as_ref()
            .expect("metadata completeness checked");
        let observed = trade_record_ids
            .get(&contract.market_id)
            .cloned()
            .unwrap_or_default();
        let observed_count = u64::try_from(observed.len())?;
        let observed_digest = trade_record_ids_sha256(observed.iter().map(String::as_str));
        if completion.condition_id != metadata.condition_id
            || completion.symbol != contract.symbol
            || completion.market_window_secs != WINDOW_SECS as u64
            || completion.completeness_basis != crate::polymarket_upload::TRADE_COMPLETION_BASIS
            || completion.trade_count != observed_count
            || completion.trade_record_ids_sha256 != observed_digest
        {
            bail!(
                "selected market {} does not match event-local trade completion proof",
                contract.market_id
            );
        }
    }
    Ok(())
}

fn normalize_raw(
    inputs: &ResearchSegmentValidationReport,
    market_path: &Path,
    reference_path: &Path,
    contracts: &BTreeMap<String, SelectedContract>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<NormalizedPolymarketEvidence> {
    let (trades, settlements) = reference_records(reference_path, contracts)?;
    let completions = aggregated_trade_completions(inputs)?;
    let mut rows = Vec::new();
    let mut trade_counts = BTreeMap::new();
    let mut settled = BTreeMap::new();
    let mut quote_tokens = BTreeMap::new();
    let mut references = BTreeMap::new();
    contract_rows(contracts, &mut rows)?;
    trade_rows(trades, contracts, &mut rows, &mut trade_counts)?;
    validate_event_local_trade_completions(&completions, contracts, &trade_counts)?;
    settlement_rows(settlements, contracts, &mut rows, &mut settled)?;
    market_rows(
        market_path,
        contracts,
        &mut rows,
        &mut quote_tokens,
        &mut references,
    )?;
    for contract in contracts.values() {
        if !has_all_surfaces(
            contract,
            &quote_tokens,
            &references,
            &trade_counts,
            &settled,
        ) {
            bail!(
                "selected market {} is missing one or more research surfaces",
                contract.market_id
            );
        }
    }
    let (ndjson, surface_counts) = encode_rows(rows)?;
    let sha256 = hex::encode(Sha256::digest(&ndjson));
    Ok(NormalizedPolymarketEvidence {
        report: PolymarketEvidenceReport {
            schema: "monday.polymarket.normalized_evidence.v1",
            content_sha256: sha256,
            content_bytes: u64::try_from(ndjson.len())?,
            rows: surface_counts.values().sum(),
            events: u64::try_from(contracts.len())?,
            surface_counts,
            event_start_gte: utc_text(start),
            event_start_lt: utc_text(end),
            symbols: SYMBOLS,
            window_secs: WINDOW_SECS as u64,
            event_selection: "event_start in [event_start_gte,event_start_lt)",
            trust_boundary: EVIDENCE_TRUST_BOUNDARY,
            validated_inputs: inputs.clone(),
        },
        ndjson,
    })
}

pub fn normalize_polymarket_evidence(
    config: &PolymarketEvidenceConfig,
) -> Result<NormalizedPolymarketEvidence> {
    with_selected_research_contracts(
        config,
        |inputs, market_path, reference_path, contracts, start, end| {
            normalize_raw(inputs, market_path, reference_path, contracts, start, end)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polymarket_research_select::SelectedMetadata;
    use crate::polymarket_upload::derived_trade_record_id;
    use std::fs;

    fn selected_contract() -> SelectedContract {
        let start = timestamp("2026-07-17T05:30:00Z", "start").unwrap();
        SelectedContract {
            market_id: "market".to_owned(),
            symbol: "BTCUSDT".to_owned(),
            event_start: start,
            event_end: start + chrono::Duration::minutes(5),
            up_token: "up".to_owned(),
            down_token: "down".to_owned(),
            price_to_beat: "100".to_owned(),
            discovery_recorded_at: utc_text(start),
            discovery_sequence: 0,
            metadata: Some(SelectedMetadata {
                condition_id: "condition".to_owned(),
                resolution_source: "https://data.chain.link/streams/btc-usd".to_owned(),
                retrieved_at: utc_text(start),
                recorded_at: utc_text(start),
                sequence: 0,
                tokens: ["up".to_owned(), "down".to_owned()],
                outcomes: ["Up".to_owned(), "Down".to_owned()],
                raw_market: json!({}),
            }),
        }
    }

    fn reference_rows(source_ts: &str) -> (SelectedContract, Vec<Row>, BTreeMap<String, u64>) {
        let contract = selected_contract();
        let contracts = BTreeMap::from([(contract.market_id.clone(), contract.clone())]);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("market.ndjson");
        let envelope = json!({
            "sequence": 1,
            "recorded_at": "2026-07-17T05:30:10Z",
            "update": {
                "kind": "reference_price",
                "source": "chainlink",
                "asset_class": "crypto",
                "symbol": "btc/usd",
                "is_carried_forward": false,
                "ts": source_ts,
                "received_at": "2026-07-17T05:30:05Z",
                "price": "100",
                "full_accuracy_value": "100"
            }
        });
        let mut bytes = serde_json::to_vec(&envelope).unwrap();
        bytes.push(b'\n');
        fs::write(&path, bytes).unwrap();
        let mut rows = Vec::new();
        let mut books = BTreeMap::new();
        let mut references = BTreeMap::new();
        market_rows(&path, &contracts, &mut rows, &mut books, &mut references).unwrap();
        (contract, rows, references)
    }

    fn quote_rows(update: Value) -> Vec<Row> {
        let contract = selected_contract();
        let contracts = BTreeMap::from([(contract.market_id.clone(), contract)]);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("market.ndjson");
        let envelope = json!({
            "sequence": 1,
            "recorded_at": "2026-07-17T05:30:02Z",
            "update": update,
        });
        let mut bytes = serde_json::to_vec(&envelope).unwrap();
        bytes.push(b'\n');
        fs::write(&path, bytes).unwrap();
        let mut rows = Vec::new();
        let mut books = BTreeMap::new();
        let mut references = BTreeMap::new();
        market_rows(&path, &contracts, &mut rows, &mut books, &mut references).unwrap();
        rows
    }

    fn trade_pending(
        sequence: u64,
        trade_ts: &str,
        trade_ts_unix: i64,
        received_at: &str,
        recorded_at: &str,
    ) -> Pending {
        let trade = json!({
            "transactionHash": format!("0xtx-{sequence}"),
            "conditionId": "condition",
            "asset": "up",
            "side": "BUY",
            "timestamp": trade_ts_unix,
            "proxyWallet": "0xwallet",
            "size": "10",
            "price": "0.5",
            "outcome": "Up",
            "outcomeIndex": 0,
        });
        let record_id = derived_trade_record_id(trade.as_object().unwrap());
        Pending {
            line: usize::try_from(sequence).unwrap(),
            sequence,
            recorded_at: recorded_at.to_owned(),
            update: json!({
                "kind": "polymarket_trade",
                "record_id": record_id,
                "record_id_version": "v2",
                "market_id": "market",
                "condition_id": "condition",
                "token_id": "up",
                "symbol": "BTCUSDT",
                "market_window_secs": 300,
                "side": "BUY",
                "size": "10",
                "price": "0.5",
                "trade_ts": trade_ts,
                "trade_ts_unix": trade_ts_unix,
                "transaction_hash": format!("0xtx-{sequence}"),
                "proxy_wallet": "0xwallet",
                "outcome": "Up",
                "outcome_index": 0,
                "source": "polymarket_data_api",
                "received_at": received_at,
                "trade": trade,
            })
            .as_object()
            .unwrap()
            .clone(),
        }
    }

    #[test]
    fn selected_reference_ignores_unrelated_assets_but_rejects_wrong_btc_source() {
        let unrelated = json!({"symbol": "hype/usd"});
        assert!(selected_reference(unrelated.as_object().unwrap(), 1)
            .unwrap()
            .is_none());

        let wrong_source = json!({
            "symbol": "btc/usd",
            "source": "other",
            "asset_class": "crypto",
            "is_carried_forward": false
        });
        assert!(selected_reference(wrong_source.as_object().unwrap(), 2).is_err());

        let valid = json!({
            "symbol": "btc/usd",
            "source": "chainlink",
            "asset_class": "crypto",
            "is_carried_forward": true
        });
        assert_eq!(
            selected_reference(valid.as_object().unwrap(), 3)
                .unwrap()
                .unwrap()
                .1,
            "BTCUSDT"
        );
    }

    #[test]
    fn official_settlement_evidence_rejects_any_changed_payload() {
        assert!(EVIDENCE_TRUST_BOUNDARY.contains("not an evaluator label snapshot"));
        assert!(EVIDENCE_TRUST_BOUNDARY.contains("snapshot_contract_hash"));
        let mut settled = BTreeMap::new();
        assert!(record_settlement_fingerprint(&mut settled, "market", "a", 1).unwrap());
        assert!(!record_settlement_fingerprint(&mut settled, "market", "a", 2).unwrap());
        assert!(record_settlement_fingerprint(&mut settled, "market", "b", 3).is_err());
    }

    #[test]
    fn canonical_rows_are_independent_of_encounter_order() {
        let make = |surface, id: &str| Row {
            key: RowKey {
                surface,
                event_start: String::new(),
                symbol: String::new(),
                market_id: String::new(),
                clock: String::new(),
                identity: id.to_owned(),
                sequence: 0,
            },
            value: json!({"surface": "test", "id": id}),
        };
        let (left, left_counts) = encode_rows(vec![
            make(SurfaceOrder::OrderbookSnapshot, "b"),
            make(SurfaceOrder::MarketContract, "a"),
        ])
        .unwrap();
        let (right, right_counts) = encode_rows(vec![
            make(SurfaceOrder::MarketContract, "a"),
            make(SurfaceOrder::OrderbookSnapshot, "b"),
        ])
        .unwrap();
        assert_eq!(left, right);
        assert_eq!(left_counts, right_counts);
        assert_eq!(left_counts["test"], 2);
    }

    #[test]
    fn encode_rows_preserves_source_arrays_and_rejects_derived_fields() {
        let row = |id: &str, value| Row {
            key: RowKey {
                surface: SurfaceOrder::MarketContract,
                event_start: String::new(),
                symbol: String::new(),
                market_id: String::new(),
                clock: String::new(),
                identity: id.to_owned(),
                sequence: 0,
            },
            value,
        };
        let value = json!({"surface":"test","source_token_ids":["down","up"],"source_outcomes":["Higher","Lower"]});
        let line =
            String::from_utf8(encode_rows(vec![row("a", value.clone())]).unwrap().0).unwrap();
        assert_eq!(serde_json::from_str::<Value>(line.trim()).unwrap(), value);
        for field in FORBIDDEN_DERIVED_FIELDS {
            let mut value = json!({"surface":"test"});
            value
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), Value::Bool(true));
            assert!(encode_rows(vec![row(field, value)]).is_err());
        }
    }

    #[test]
    fn availability_uses_the_later_valid_arrival_clock() {
        assert_eq!(
            latest("2026-07-17T05:30:00Z", "2026-07-17T05:30:01Z").unwrap(),
            "2026-07-17T05:30:01Z"
        );
        assert!(latest("not-a-time", "2026-07-17T05:30:01Z").is_err());
    }

    #[test]
    fn reference_window_is_half_open_with_a_thirty_second_pre_open_bound() {
        let contract = selected_contract();
        assert!(reference_belongs_to_contract(
            contract.event_start - chrono::Duration::seconds(30),
            &contract
        ));
        assert!(!reference_belongs_to_contract(
            contract.event_start - chrono::Duration::seconds(31),
            &contract
        ));
        assert!(!reference_belongs_to_contract(
            contract.event_end,
            &contract
        ));
    }

    #[test]
    fn pre_open_reference_is_retained_without_backdating_availability() {
        let (_, rows, references) = reference_rows("2026-07-17T05:29:30Z");
        assert_eq!(references.get("market"), Some(&1));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value["ts"], "2026-07-17T05:29:30Z");
        assert_eq!(rows[0].value["received_at"], "2026-07-17T05:30:05Z");
        assert_eq!(rows[0].value["recorded_at"], "2026-07-17T05:30:10Z");
        assert_eq!(rows[0].value["available_at"], "2026-07-17T05:30:10Z");
    }

    #[test]
    fn boundary_ask_price_without_size_is_not_published_as_top_of_book() {
        let rows = quote_rows(json!({
            "kind": "quote",
            "token_id": "up",
            "ts": "2026-07-17T05:30:01Z",
            "bid": "0.99",
            "bid_size": "10",
            "ask": "1",
            "ask_levels": [],
            "bid_levels": [{"price": "0.99", "size": "10"}],
        }));

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value["bid"], "0.99");
        assert_eq!(rows[0].value["bid_size"], "10");
        assert_eq!(rows[0].value["ask"], Value::Null);
        assert_eq!(rows[0].value["ask_size"], Value::Null);
    }

    #[test]
    fn boundary_bid_price_without_size_is_not_published_as_top_of_book() {
        let rows = quote_rows(json!({
            "kind": "quote",
            "token_id": "up",
            "ts": "2026-07-17T05:30:01Z",
            "bid": "0",
            "ask": "0.01",
            "ask_size": "10",
            "ask_levels": [{"price": "0.01", "size": "10"}],
            "bid_levels": [],
        }));

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value["bid"], Value::Null);
        assert_eq!(rows[0].value["bid_size"], Value::Null);
        assert_eq!(rows[0].value["ask"], "0.01");
        assert_eq!(rows[0].value["ask_size"], "10");
    }

    #[test]
    fn noncausal_trade_clock_is_not_published_as_evidence() {
        let contract = selected_contract();
        let contracts = BTreeMap::from([(contract.market_id.clone(), contract)]);
        let mut rows = Vec::new();
        let mut counts = BTreeMap::new();
        trade_rows(
            vec![
                trade_pending(
                    1,
                    "2026-07-17T05:30:03Z",
                    1_784_266_203,
                    "2026-07-17T05:30:02Z",
                    "2026-07-17T05:30:04Z",
                ),
                trade_pending(
                    2,
                    "2026-07-17T05:30:03Z",
                    1_784_266_203,
                    "2026-07-17T05:30:04Z",
                    "2026-07-17T05:30:02Z",
                ),
                trade_pending(
                    3,
                    "2026-07-17T05:30:03Z",
                    1_784_266_203,
                    "2026-07-17T05:30:04Z",
                    "2026-07-17T05:30:05Z",
                ),
            ],
            &contracts,
            &mut rows,
            &mut counts,
        )
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value["source_sequence"], 3);
        assert_eq!(counts.get("market").map(BTreeSet::len), Some(1));
    }

    #[test]
    fn noncausal_trades_do_not_satisfy_event_completeness() {
        let contract = selected_contract();
        let contracts = BTreeMap::from([(contract.market_id.clone(), contract.clone())]);
        let mut rows = Vec::new();
        let mut trades = BTreeMap::new();
        trade_rows(
            vec![trade_pending(
                1,
                "2026-07-17T05:30:03Z",
                1_784_266_203,
                "2026-07-17T05:30:02Z",
                "2026-07-17T05:30:04Z",
            )],
            &contracts,
            &mut rows,
            &mut trades,
        )
        .unwrap();

        let books = BTreeMap::from([(
            "market".to_owned(),
            BTreeSet::from(["up".to_owned(), "down".to_owned()]),
        )]);
        let references = BTreeMap::from([("market".to_owned(), 1)]);
        let settlements = BTreeMap::from([("market".to_owned(), "digest".to_owned())]);
        assert!(rows.is_empty());
        assert!(trades.is_empty());
        assert!(!has_all_surfaces(
            &contract,
            &books,
            &references,
            &trades,
            &settlements
        ));
    }

    #[test]
    fn rejects_selected_event_missing_a_trade_bound_by_completion_digest() {
        let contract = selected_contract();
        let contracts = BTreeMap::from([(contract.market_id.clone(), contract)]);
        let complete_ids = BTreeSet::from(["trade-a".to_owned(), "trade-b".to_owned()]);
        let completions = BTreeMap::from([(
            "market".to_owned(),
            TradeCompletionIdentity {
                condition_id: "condition".to_owned(),
                symbol: "BTCUSDT".to_owned(),
                market_window_secs: 300,
                trade_count: 2,
                trade_record_ids_sha256: trade_record_ids_sha256(
                    complete_ids.iter().map(String::as_str),
                ),
                completion_sequence: 10,
                retrieved_at: "2026-07-17T05:31:00Z".to_owned(),
                completeness_basis: crate::polymarket_upload::TRADE_COMPLETION_BASIS.to_owned(),
                finalization_lag_secs: 60,
                stable_polls_required: 2,
            },
        )]);
        let observed =
            BTreeMap::from([("market".to_owned(), BTreeSet::from(["trade-a".to_owned()]))]);

        let error = validate_event_local_trade_completions(&completions, &contracts, &observed)
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("does not match event-local trade completion proof"));
    }

    #[test]
    fn rejects_duplicate_trade_completion_proofs_across_reference_segments() {
        let completion = TradeCompletionIdentity {
            condition_id: "condition".to_owned(),
            symbol: "BTCUSDT".to_owned(),
            market_window_secs: 300,
            trade_count: 1,
            trade_record_ids_sha256: "1".repeat(64),
            completion_sequence: 10,
            retrieved_at: "2026-07-17T05:31:00Z".to_owned(),
            completeness_basis: crate::polymarket_upload::TRADE_COMPLETION_BASIS.to_owned(),
            finalization_lag_secs: 60,
            stable_polls_required: 2,
        };
        let inputs = ResearchSegmentValidationReport {
            schema: "monday.polymarket.research_segment_validation.v2",
            market: crate::polymarket_research_import::SegmentIdentity {
                schema: "monday.polymarket.raw.v1".to_owned(),
                venue: "polymarket".to_owned(),
                dataset: "crypto_expiry".to_owned(),
                date: "2026-07-17".to_owned(),
                hour: "05".to_owned(),
                file: "market.ndjson.zst".to_owned(),
                bytes: 1,
                sha256: "0".repeat(64),
                events: 1,
                start_sequence: 1,
                end_sequence: 1,
                sequence_gaps: 0,
                start_recorded_at: "2026-07-17T05:00:00Z".to_owned(),
                end_recorded_at: "2026-07-17T05:00:01Z".to_owned(),
                source_file: "market.ndjson".to_owned(),
                replay_scope: "fixture".to_owned(),
                recording_policy: json!({}),
                record_id_versions: json!([]),
                event_types: BTreeMap::new(),
                trade_completions: BTreeMap::new(),
            },
            references: vec![
                crate::polymarket_research_import::SegmentIdentity {
                    schema: "monday.polymarket.raw.v1".to_owned(),
                    venue: "polymarket".to_owned(),
                    dataset: "crypto_expiry_reference".to_owned(),
                    date: "2026-07-17".to_owned(),
                    hour: "05".to_owned(),
                    file: "reference-05.ndjson.zst".to_owned(),
                    bytes: 1,
                    sha256: "0".repeat(64),
                    events: 1,
                    start_sequence: 1,
                    end_sequence: 1,
                    sequence_gaps: 0,
                    start_recorded_at: "2026-07-17T05:00:00Z".to_owned(),
                    end_recorded_at: "2026-07-17T05:00:01Z".to_owned(),
                    source_file: "reference-05.ndjson".to_owned(),
                    replay_scope: "fixture".to_owned(),
                    recording_policy: json!({}),
                    record_id_versions: json!(["v2"]),
                    event_types: BTreeMap::new(),
                    trade_completions: BTreeMap::from([("market".to_owned(), completion.clone())]),
                },
                crate::polymarket_research_import::SegmentIdentity {
                    schema: "monday.polymarket.raw.v1".to_owned(),
                    venue: "polymarket".to_owned(),
                    dataset: "crypto_expiry_reference".to_owned(),
                    date: "2026-07-17".to_owned(),
                    hour: "06".to_owned(),
                    file: "reference-06.ndjson.zst".to_owned(),
                    bytes: 1,
                    sha256: "0".repeat(64),
                    events: 1,
                    start_sequence: 2,
                    end_sequence: 2,
                    sequence_gaps: 0,
                    start_recorded_at: "2026-07-17T06:00:00Z".to_owned(),
                    end_recorded_at: "2026-07-17T06:00:01Z".to_owned(),
                    source_file: "reference-06.ndjson".to_owned(),
                    replay_scope: "fixture".to_owned(),
                    recording_policy: json!({}),
                    record_id_versions: json!([]),
                    event_types: BTreeMap::new(),
                    trade_completions: BTreeMap::from([("market".to_owned(), completion)]),
                },
            ],
        };

        let error = aggregated_trade_completions(&inputs).unwrap_err();

        assert!(error.to_string().contains("duplicate"), "{error:#}");
    }

    #[test]
    fn malformed_recorded_at_fails_even_when_received_at_is_noncausal() {
        let contract = selected_contract();
        let contracts = BTreeMap::from([(contract.market_id.clone(), contract)]);
        let error = trade_rows(
            vec![trade_pending(
                1,
                "2026-07-17T05:30:03Z",
                1_784_266_203,
                "2026-07-17T05:30:02Z",
                "not-a-timestamp",
            )],
            &contracts,
            &mut Vec::new(),
            &mut BTreeMap::new(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("recorded_at"), "{error:#}");
    }

    #[test]
    fn only_pre_open_reference_satisfies_surface_completeness() {
        let (contract, _, fresh) = reference_rows("2026-07-17T05:29:30Z");
        let (_, stale_rows, stale) = reference_rows("2026-07-17T05:29:29Z");
        let (_, end_rows, end) = reference_rows("2026-07-17T05:35:00Z");
        let books = BTreeMap::from([(
            "market".to_owned(),
            BTreeSet::from(["up".to_owned(), "down".to_owned()]),
        )]);
        let trades = BTreeMap::from([("market".to_owned(), BTreeSet::from(["trade".to_owned()]))]);
        let settlements = BTreeMap::from([("market".to_owned(), "digest".to_owned())]);
        assert!(has_all_surfaces(
            &contract,
            &books,
            &fresh,
            &trades,
            &settlements
        ));
        assert!(stale_rows.is_empty());
        assert!(end_rows.is_empty());
        assert!(end.is_empty());
        assert!(!has_all_surfaces(
            &contract,
            &books,
            &stale,
            &trades,
            &settlements
        ));
    }

    #[test]
    fn completeness_requires_both_books_reference_trade_and_settlement() {
        let start = timestamp("2026-07-17T05:30:00Z", "start").unwrap();
        let contract = SelectedContract {
            market_id: "market".to_owned(),
            symbol: "BTCUSDT".to_owned(),
            event_start: start,
            event_end: start,
            up_token: "up".to_owned(),
            down_token: "down".to_owned(),
            price_to_beat: "1".to_owned(),
            discovery_recorded_at: utc_text(start),
            discovery_sequence: 0,
            metadata: None,
        };
        let books = BTreeMap::from([(
            "market".to_owned(),
            BTreeSet::from(["up".to_owned(), "down".to_owned()]),
        )]);
        let mut references = BTreeMap::from([("market".to_owned(), 1)]);
        let trades = BTreeMap::from([("market".to_owned(), BTreeSet::from(["trade".to_owned()]))]);
        let settlements = BTreeMap::from([("market".to_owned(), "digest".to_owned())]);
        assert!(has_all_surfaces(
            &contract,
            &books,
            &references,
            &trades,
            &settlements
        ));
        references.clear();
        assert!(!has_all_surfaces(
            &contract,
            &books,
            &references,
            &trades,
            &settlements
        ));
    }
}
