use crate::polymarket_research_import::{
    with_event_local_validated_research_segments, with_validated_research_segments,
    ResearchSegmentValidationConfig, ResearchSegmentValidationReport,
};
use crate::polymarket_upload::validate_market_metadata;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;
use url::Url;

pub(crate) const SYMBOLS: [&str; 2] = ["BTCUSDT", "SOLUSDT"];
pub(crate) const WINDOW_SECS: i64 = 300;
const MAX_SELECTED_QUOTE_GAP_SECS: i64 = 30;

#[derive(Debug, Clone)]
pub struct ResearchSelectionConfig {
    pub segments: ResearchSegmentValidationConfig,
    pub event_start_gte: String,
    pub event_start_lt: String,
    pub market_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelectedResearchContract {
    pub market_id: String,
    pub condition_id: String,
    pub symbol: String,
    pub event_start: String,
    pub event_end: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub price_to_beat: String,
    pub resolution_source: String,
    pub discovery_recorded_at: String,
    pub metadata_retrieved_at: String,
    pub metadata_recorded_at: String,
    pub discovery_source_sequence: u64,
    pub metadata_source_sequence: u64,
    pub raw_market: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResearchSelectionReport {
    pub schema: &'static str,
    pub event_start_gte: String,
    pub event_start_lt: String,
    pub market_ids: Vec<String>,
    pub symbols: Vec<String>,
    pub window_secs: u64,
    pub contracts: Vec<SelectedResearchContract>,
    pub validated_inputs: ResearchSegmentValidationReport,
}

#[derive(Clone)]
pub(crate) struct SelectedMetadata {
    pub condition_id: String,
    pub resolution_source: String,
    pub retrieved_at: String,
    pub recorded_at: String,
    pub sequence: u64,
    pub tokens: [String; 2],
    pub outcomes: [String; 2],
    pub raw_market: Value,
}

#[derive(Clone)]
pub(crate) struct SelectedContract {
    pub market_id: String,
    pub symbol: String,
    pub event_start: DateTime<Utc>,
    pub event_end: DateTime<Utc>,
    pub up_token: String,
    pub down_token: String,
    pub price_to_beat: String,
    pub discovery_recorded_at: String,
    pub discovery_sequence: u64,
    pub metadata: Option<SelectedMetadata>,
}

#[derive(Default)]
struct SelectedTokenQuoteCoverage {
    first_recorded_at: Option<DateTime<Utc>>,
    last_recorded_at: Option<DateTime<Utc>>,
    last_source_at: Option<DateTime<Utc>>,
    pending_failure_at: Option<DateTime<Utc>>,
}

fn validate_selected_market_quote_coverage(
    inputs: &ResearchSegmentValidationReport,
    path: &Path,
    contracts: &BTreeMap<String, SelectedContract>,
) -> Result<()> {
    let bound = Duration::seconds(MAX_SELECTED_QUOTE_GAP_SECS);
    let segment_end = timestamp(&inputs.market.end_recorded_at, "market segment end")?;
    let token_contracts = contracts
        .values()
        .flat_map(|contract| {
            [
                (contract.up_token.clone(), contract),
                (contract.down_token.clone(), contract),
            ]
        })
        .collect::<BTreeMap<_, _>>();
    let mut coverage = token_contracts
        .keys()
        .cloned()
        .map(|token| (token, SelectedTokenQuoteCoverage::default()))
        .collect::<BTreeMap<_, _>>();

    visit(path, |line, _, recorded_at, update| {
        let Some(token) = update.get("token_id").and_then(Value::as_str) else {
            return Ok(());
        };
        let Some(contract) = token_contracts.get(token) else {
            return Ok(());
        };
        let recorded_at = timestamp(recorded_at, "quote recorded_at")?;
        if recorded_at < timestamp(&contract.discovery_recorded_at, "discovery recorded_at")?
            || recorded_at >= contract.event_end
        {
            return Ok(());
        }
        let state = coverage
            .get_mut(token)
            .expect("selected token was initialized");
        match update.get("kind").and_then(Value::as_str) {
            Some("quote_collection_failure") => {
                state.pending_failure_at.get_or_insert(recorded_at);
            }
            Some("quote") => {
                let source_at = timestamp(required(update, "ts")?, "quote ts")?;
                if source_at < contract.event_start || source_at >= contract.event_end {
                    return Ok(());
                }
                if recorded_at - source_at > bound {
                    bail!(
                        "line {line}: selected token quote exceeds the 30-second freshness bound"
                    );
                }
                if let Some(previous) = state.last_recorded_at {
                    if recorded_at - previous > bound {
                        bail!("line {line}: selected token has a quote availability gap over 30 seconds");
                    }
                }
                if let Some(previous) = state.last_source_at {
                    if source_at - previous > bound {
                        bail!("line {line}: selected token has a quote source gap over 30 seconds");
                    }
                }
                if let Some(failure) = state.pending_failure_at.take() {
                    if recorded_at - failure > bound {
                        bail!("line {line}: selected token quote recovery exceeds 30 seconds");
                    }
                }
                state.first_recorded_at.get_or_insert(recorded_at);
                state.last_recorded_at = Some(recorded_at);
                state.last_source_at = Some(source_at);
            }
            _ => {}
        }
        Ok(())
    })?;

    for (token, contract) in token_contracts {
        let state = &coverage[&token];
        let discovery = timestamp(&contract.discovery_recorded_at, "discovery recorded_at")?;
        let Some(first) = state.first_recorded_at else {
            bail!("selected token {token} has no causally available quote");
        };
        let last = state
            .last_recorded_at
            .expect("first quote also sets last quote");
        if first - discovery > bound {
            bail!("selected token {token} first quote is more than 30 seconds after discovery");
        }
        if state.pending_failure_at.is_some() {
            bail!("selected token {token} has an unresolved quote collection failure");
        }
        let coverage_end = segment_end.min(contract.event_end);
        if coverage_end - last > bound {
            bail!("selected token {token} ends with a quote availability gap over 30 seconds");
        }
    }
    Ok(())
}

pub(crate) fn timestamp(value: &str, field: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .with_context(|| format!("invalid {field}: {value}"))
}

pub(crate) fn utc_text(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn required<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("{field} must be a non-empty string"))
}

pub(crate) fn decimal_text(value: Option<&Value>, field: &str) -> Result<String> {
    let text = value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            value
                .filter(|value| value.is_number())
                .map(Value::to_string)
        })
        .ok_or_else(|| anyhow!("{field} must be numeric"))?;
    Decimal::from_str(&text).with_context(|| format!("invalid decimal {field}: {text}"))?;
    Ok(text)
}

pub(crate) fn json_strings(value: Option<&Value>, field: &str) -> Result<[String; 2]> {
    let values: Vec<String> = serde_json::from_str(
        value
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{field} must be a JSON string array"))?,
    )?;
    values
        .try_into()
        .map_err(|_| anyhow!("{field} must contain exactly two strings"))
}

pub(crate) fn semantic_index(outcomes: &[String; 2], up: bool) -> Result<usize> {
    let expected = if up { ["up", "yes"] } else { ["down", "no"] };
    let matches = outcomes
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            expected
                .contains(&value.to_ascii_lowercase().as_str())
                .then_some(index)
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        bail!(
            "outcomes do not contain one semantic {} side",
            if up { "up" } else { "down" }
        );
    }
    Ok(matches[0])
}

pub(crate) fn visit(
    path: &Path,
    mut consume: impl FnMut(usize, u64, &str, &Map<String, Value>) -> Result<()>,
) -> Result<()> {
    for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line_number = index + 1;
        let value: Value = serde_json::from_str(&line?)
            .with_context(|| format!("parse {} line {line_number}", path.display()))?;
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("line {line_number}: envelope must be an object"))?;
        let sequence = object
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("line {line_number}: sequence must be an integer"))?;
        let recorded_at = required(object, "recorded_at")?;
        let update = object
            .get("update")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("line {line_number}: update must be an object"))?;
        consume(line_number, sequence, recorded_at, update)?;
    }
    Ok(())
}

fn selection(
    config: &ResearchSelectionConfig,
) -> Result<(DateTime<Utc>, DateTime<Utc>, BTreeSet<String>)> {
    let start = timestamp(&config.event_start_gte, "event_start_gte")?;
    let end = timestamp(&config.event_start_lt, "event_start_lt")?;
    if start.timestamp_subsec_nanos() != 0
        || end.timestamp_subsec_nanos() != 0
        || start.timestamp().rem_euclid(WINDOW_SECS) != 0
        || end.timestamp().rem_euclid(WINDOW_SECS) != 0
        || end <= start
    {
        bail!("event selection must be a positive UTC interval aligned to five minutes");
    }
    let mut market_ids = BTreeSet::new();
    for market_id in &config.market_ids {
        if market_id.is_empty()
            || market_id.chars().any(char::is_whitespace)
            || !market_ids.insert(market_id.clone())
        {
            bail!("market IDs must be unique, non-empty, whitespace-free identifiers");
        }
    }
    if market_ids.is_empty() {
        bail!("at least one market ID is required");
    }
    Ok((start, end, market_ids))
}

fn discover(
    path: &Path,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    market_ids: &BTreeSet<String>,
) -> Result<BTreeMap<String, SelectedContract>> {
    let mut contracts = BTreeMap::new();
    visit(path, |line, sequence, recorded_at, update| {
        if update.get("kind").and_then(Value::as_str) != Some("event_discovered")
            || update.get("window_secs").and_then(Value::as_u64) != Some(WINDOW_SECS as u64)
        {
            return Ok(());
        }
        let market_id = required(update, "event_id")?.to_owned();
        if !market_ids.contains(&market_id) {
            return Ok(());
        }
        let symbol = required(update, "symbol")?;
        if !SYMBOLS.contains(&symbol) {
            return Ok(());
        }
        let event_end = timestamp(required(update, "end_time")?, "end_time")?;
        let event_start = event_end - Duration::seconds(WINDOW_SECS);
        if event_start < start || event_start >= end {
            return Ok(());
        }
        let up_token = required(update, "up_token")?.to_owned();
        let down_token = required(update, "down_token")?.to_owned();
        if up_token == down_token {
            bail!("line {line}: discovery has duplicate outcome tokens");
        }
        let price_to_beat = decimal_text(update.get("price_to_beat"), "price_to_beat")?;
        if Decimal::from_str(&price_to_beat)? <= Decimal::ZERO {
            bail!("line {line}: price_to_beat must be positive");
        }
        let contract = SelectedContract {
            market_id: market_id.clone(),
            symbol: symbol.to_owned(),
            event_start,
            event_end,
            up_token,
            down_token,
            price_to_beat,
            discovery_recorded_at: recorded_at.to_owned(),
            discovery_sequence: sequence,
            metadata: None,
        };
        if contracts.insert(market_id.clone(), contract).is_some() {
            bail!("line {line}: duplicate selected discovery for market {market_id}");
        }
        Ok(())
    })?;
    if contracts.len() != market_ids.len()
        || market_ids
            .iter()
            .any(|market_id| !contracts.contains_key(market_id))
    {
        bail!("one or more requested market IDs were not discovered in the bounded market tape");
    }
    Ok(contracts)
}

fn raw_market_start(market: &Map<String, Value>) -> Result<DateTime<Utc>> {
    let value = market
        .get("eventStartTime")
        .and_then(Value::as_str)
        .or_else(|| {
            market
                .get("events")
                .and_then(Value::as_array)
                .and_then(|events| events.first())
                .and_then(|event| event.get("startTime"))
                .and_then(Value::as_str)
        })
        .or_else(|| market.get("startDate").and_then(Value::as_str))
        .or_else(|| {
            market
                .get("events")
                .and_then(Value::as_array)
                .and_then(|events| events.first())
                .and_then(|event| event.get("startDate"))
                .and_then(Value::as_str)
        })
        .ok_or_else(|| anyhow!("raw market has no event start"))?;
    timestamp(value, "raw market event start")
}

fn validate_resolution_source(value: &str, symbol: &str, line: usize) -> Result<()> {
    let url =
        Url::parse(value).with_context(|| format!("line {line}: invalid resolutionSource"))?;
    let expected_path = match symbol {
        "BTCUSDT" => "/streams/btc-usd",
        "SOLUSDT" => "/streams/sol-usd",
        _ => unreachable!("discovery restricts symbols"),
    };
    if url.scheme() != "https"
        || url.host_str() != Some("data.chain.link")
        || url.path() != expected_path
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        bail!("line {line}: selected market is not resolved by the expected Chainlink stream");
    }
    Ok(())
}

fn metadata(
    update: &Map<String, Value>,
    contract: &SelectedContract,
    line: usize,
) -> Result<SelectedMetadata> {
    validate_market_metadata(update, line)?;
    let market = update["market"]
        .as_object()
        .expect("metadata validator checked market");
    let tokens = json_strings(market.get("clobTokenIds"), "clobTokenIds")?;
    let outcomes = json_strings(market.get("outcomes"), "outcomes")?;
    let up = semantic_index(&outcomes, true)?;
    let down = semantic_index(&outcomes, false)?;
    let event_start = raw_market_start(market)?;
    let event_end = timestamp(
        market
            .get("endDate")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("raw market endDate is missing"))?,
        "raw market endDate",
    )?;
    if required(update, "market_id")? != contract.market_id
        || required(update, "symbol")? != contract.symbol
        || event_start != contract.event_start
        || event_end != contract.event_end
        || tokens[up] != contract.up_token
        || tokens[down] != contract.down_token
    {
        bail!("line {line}: metadata contradicts selected discovery");
    }
    let resolution_source = market
        .get("resolutionSource")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("line {line}: resolutionSource is missing"))?;
    validate_resolution_source(resolution_source, &contract.symbol, line)?;
    let retrieved_at = required(update, "retrieved_at")?;
    timestamp(retrieved_at, "retrieved_at")?;
    Ok(SelectedMetadata {
        condition_id: required(update, "condition_id")?.to_owned(),
        resolution_source: resolution_source.to_owned(),
        retrieved_at: retrieved_at.to_owned(),
        recorded_at: String::new(),
        sequence: 0,
        tokens,
        outcomes,
        raw_market: Value::Object(market.clone()),
    })
}

fn enrich_metadata(path: &Path, contracts: &mut BTreeMap<String, SelectedContract>) -> Result<()> {
    visit(path, |line, sequence, recorded_at, update| {
        if update.get("kind").and_then(Value::as_str) != Some("market_metadata") {
            return Ok(());
        }
        let Some(market_id) = update.get("market_id").and_then(Value::as_str) else {
            return Ok(());
        };
        let Some(contract) = contracts.get_mut(market_id) else {
            return Ok(());
        };
        let mut candidate = metadata(update, contract, line)?;
        candidate.recorded_at = recorded_at.to_owned();
        candidate.sequence = sequence;
        if let Some(existing) = &contract.metadata {
            if candidate.condition_id != existing.condition_id
                || candidate.tokens != existing.tokens
                || candidate.outcomes != existing.outcomes
                || candidate.resolution_source != existing.resolution_source
            {
                bail!("line {line}: metadata identity changed for market {market_id}");
            }
        } else {
            contract.metadata = Some(candidate);
        }
        Ok(())
    })?;
    if let Some(contract) = contracts
        .values()
        .find(|contract| contract.metadata.is_none())
    {
        bail!(
            "missing metadata for selected market {}",
            contract.market_id
        );
    }
    Ok(())
}

fn with_selected_research_contracts_policy<T>(
    config: &ResearchSelectionConfig,
    event_local: bool,
    consume: impl FnOnce(
        &ResearchSegmentValidationReport,
        &Path,
        &Path,
        &BTreeMap<String, SelectedContract>,
        DateTime<Utc>,
        DateTime<Utc>,
    ) -> Result<T>,
) -> Result<T> {
    let (start, end, market_ids) = selection(config)?;
    let selected =
        |inputs: &ResearchSegmentValidationReport, market_path: &Path, reference_path: &Path| {
            let mut contracts = discover(market_path, start, end, &market_ids)?;
            if event_local {
                validate_selected_market_quote_coverage(inputs, market_path, &contracts)?;
            }
            enrich_metadata(reference_path, &mut contracts)?;
            consume(inputs, market_path, reference_path, &contracts, start, end)
        };
    if event_local {
        with_event_local_validated_research_segments(&config.segments, selected)
    } else {
        with_validated_research_segments(&config.segments, selected)
    }
}

pub(crate) fn with_event_local_selected_research_contracts<T>(
    config: &ResearchSelectionConfig,
    consume: impl FnOnce(
        &ResearchSegmentValidationReport,
        &Path,
        &Path,
        &BTreeMap<String, SelectedContract>,
        DateTime<Utc>,
        DateTime<Utc>,
    ) -> Result<T>,
) -> Result<T> {
    with_selected_research_contracts_policy(config, true, consume)
}

pub fn select_research_contracts(
    config: &ResearchSelectionConfig,
) -> Result<ResearchSelectionReport> {
    with_selected_research_contracts_policy(config, false, |inputs, _, _, contracts, start, end| {
        let market_ids = contracts.keys().cloned().collect();
        let symbols = contracts
            .values()
            .map(|contract| contract.symbol.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let contracts = contracts
            .values()
            .map(|contract| {
                let metadata = contract.metadata.as_ref().expect("metadata was enriched");
                SelectedResearchContract {
                    market_id: contract.market_id.clone(),
                    condition_id: metadata.condition_id.clone(),
                    symbol: contract.symbol.clone(),
                    event_start: utc_text(contract.event_start),
                    event_end: utc_text(contract.event_end),
                    up_token_id: contract.up_token.clone(),
                    down_token_id: contract.down_token.clone(),
                    price_to_beat: contract.price_to_beat.clone(),
                    resolution_source: metadata.resolution_source.clone(),
                    discovery_recorded_at: contract.discovery_recorded_at.clone(),
                    metadata_retrieved_at: metadata.retrieved_at.clone(),
                    metadata_recorded_at: metadata.recorded_at.clone(),
                    discovery_source_sequence: contract.discovery_sequence,
                    metadata_source_sequence: metadata.sequence,
                    raw_market: metadata.raw_market.clone(),
                }
            })
            .collect();
        Ok(ResearchSelectionReport {
            schema: "monday.polymarket.research_selection.v2",
            event_start_gte: utc_text(start),
            event_start_lt: utc_text(end),
            market_ids,
            symbols,
            window_secs: WINDOW_SECS as u64,
            contracts,
            validated_inputs: inputs.clone(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polymarket_research_import::ArtifactTriplet;
    use serde_json::json;
    use std::path::PathBuf;

    fn config(start: &str, end: &str) -> ResearchSelectionConfig {
        let triplet = ArtifactTriplet {
            data: PathBuf::new(),
            manifest: PathBuf::new(),
            success: PathBuf::new(),
        };
        ResearchSelectionConfig {
            segments: ResearchSegmentValidationConfig {
                market: triplet.clone(),
                references: vec![triplet],
            },
            event_start_gte: start.to_owned(),
            event_start_lt: end.to_owned(),
            market_ids: vec!["market-1".to_owned()],
        }
    }

    fn contract(market_id: &str, symbol: &str, event_start: DateTime<Utc>) -> SelectedContract {
        SelectedContract {
            market_id: market_id.to_owned(),
            symbol: symbol.to_owned(),
            event_start,
            event_end: event_start + Duration::seconds(WINDOW_SECS),
            up_token: format!("{market_id}-up"),
            down_token: format!("{market_id}-down"),
            price_to_beat: "1".to_owned(),
            discovery_recorded_at: utc_text(event_start),
            discovery_sequence: 0,
            metadata: None,
        }
    }

    #[test]
    fn selection_rejects_non_five_minute_boundary() {
        let error = selection(&config("2026-07-17T05:30:01Z", "2026-07-17T05:40:00Z")).unwrap_err();
        assert!(error.to_string().contains("aligned to five minutes"));
    }

    #[test]
    fn selection_rejects_duplicate_market_ids() {
        let mut config = config("2026-07-17T05:30:00Z", "2026-07-17T05:35:00Z");
        config.market_ids.push("market-1".to_owned());
        let error = selection(&config).unwrap_err();
        assert!(error.to_string().contains("market IDs must be unique"));
    }

    #[test]
    fn selection_rejects_market_ids_with_internal_whitespace() {
        let mut config = config("2026-07-17T05:30:00Z", "2026-07-17T05:35:00Z");
        config.market_ids = vec!["market 1".to_owned()];
        let error = selection(&config).unwrap_err();
        assert!(error.to_string().contains("whitespace-free"));
    }

    #[test]
    fn semantic_outcomes_support_reversed_arrays_and_reject_duplicates() {
        let reversed = ["Down".to_owned(), "Up".to_owned()];
        assert_eq!(semantic_index(&reversed, true).unwrap(), 1);
        assert_eq!(semantic_index(&reversed, false).unwrap(), 0);
        assert!(semantic_index(&["Up".to_owned(), "Yes".to_owned()], true).is_err());
    }

    #[test]
    fn raw_market_start_prefers_event_start_over_listing_date() {
        let market = json!({
            "eventStartTime": "2026-07-17T05:30:00Z",
            "startDate": "2026-07-15T05:30:00Z",
            "events": [{"startTime": "2026-07-16T05:30:00Z"}]
        });
        assert_eq!(
            utc_text(raw_market_start(market.as_object().unwrap()).unwrap()),
            "2026-07-17T05:30:00Z"
        );
    }

    #[test]
    fn chainlink_resolution_requires_exact_https_host_and_stream() {
        assert!(validate_resolution_source(
            "https://data.chain.link/streams/btc-usd",
            "BTCUSDT",
            1
        )
        .is_ok());
        assert!(validate_resolution_source(
            "https://data.chain.link.attacker.example/streams/btc-usd",
            "BTCUSDT",
            1
        )
        .is_err());
        assert!(validate_resolution_source(
            "https://data.chain.link/streams/sol-usd",
            "BTCUSDT",
            1
        )
        .is_err());
    }

    #[test]
    fn discovery_rejects_non_positive_price_to_beat() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("market.ndjson");
        let rows = [
            json!({
                "sequence": 0,
                "recorded_at": "2026-07-17T05:29:00Z",
                "update": {
                    "kind": "event_discovered",
                    "window_secs": 300,
                    "symbol": "BTCUSDT",
                    "end_time": "2026-07-17T05:35:00Z",
                    "event_id": "btc-market",
                    "up_token": "btc-up",
                    "down_token": "btc-down",
                    "price_to_beat": "0"
                }
            }),
            json!({
                "sequence": 1,
                "recorded_at": "2026-07-17T05:29:01Z",
                "update": {
                    "kind": "event_discovered",
                    "window_secs": 300,
                    "symbol": "SOLUSDT",
                    "end_time": "2026-07-17T05:35:00Z",
                    "event_id": "sol-market",
                    "up_token": "sol-up",
                    "down_token": "sol-down",
                    "price_to_beat": "100"
                }
            }),
        ];
        let tape = rows
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            + "\n";
        std::fs::write(&path, tape).unwrap();
        let start = timestamp("2026-07-17T05:30:00Z", "start").unwrap();
        let error = match discover(
            &path,
            start,
            start + Duration::seconds(WINDOW_SECS),
            &BTreeSet::from(["btc-market".to_owned(), "sol-market".to_owned()]),
        ) {
            Ok(_) => panic!("non-positive price_to_beat should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("price_to_beat must be positive"));
    }

    #[test]
    fn discovery_selects_only_the_requested_market_id() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("market.ndjson");
        let rows = [
            json!({
                "sequence": 0,
                "recorded_at": "2026-07-17T05:29:00Z",
                "update": {
                    "kind": "event_discovered",
                    "window_secs": 300,
                    "symbol": "BTCUSDT",
                    "end_time": "2026-07-17T05:35:00Z",
                    "event_id": "btc-market",
                    "up_token": "btc-up",
                    "down_token": "btc-down",
                    "price_to_beat": "100"
                }
            }),
            json!({
                "sequence": 1,
                "recorded_at": "2026-07-17T05:29:01Z",
                "update": {
                    "kind": "event_discovered",
                    "window_secs": 300,
                    "symbol": "SOLUSDT",
                    "end_time": "2026-07-17T05:35:00Z",
                    "event_id": "sol-market",
                    "up_token": "sol-up",
                    "down_token": "sol-down",
                    "price_to_beat": "100"
                }
            }),
        ];
        let tape = rows
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            + "\n";
        std::fs::write(&path, tape).unwrap();

        let start = timestamp("2026-07-17T05:30:00Z", "start").unwrap();
        let contracts = discover(
            &path,
            start,
            start + Duration::seconds(WINDOW_SECS),
            &BTreeSet::from(["btc-market".to_owned()]),
        )
        .expect("the requested BTC episode must not require the unrelated SOL episode");

        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts["btc-market"].symbol, "BTCUSDT");
        assert!(!contracts.contains_key("sol-market"));
    }

    #[test]
    fn metadata_rejects_invalid_retrieved_at() {
        let event_start = timestamp("2026-07-17T05:30:00Z", "start").unwrap();
        let contract = contract("market-1", "BTCUSDT", event_start);
        let mut update = json!({
            "market_id": "market-1",
            "condition_id": "0xcondition",
            "symbol": "BTCUSDT",
            "market_window_secs": 300,
            "retrieved_at": "not-a-timestamp",
            "market": {
                "id": "market-1",
                "conditionId": "0xcondition",
                "question": "Bitcoin Up or Down - 5 minutes",
                "slug": "btc-updown-5m-test",
                "startDate": "2026-07-17T05:30:00Z",
                "endDate": "2026-07-17T05:35:00Z",
                "clobTokenIds": "[\"market-1-up\",\"market-1-down\"]",
                "outcomes": "[\"Up\",\"Down\"]",
                "resolutionSource": "https://data.chain.link/streams/btc-usd",
                "makerBaseFee": 1000,
                "takerBaseFee": 1000
            }
        });
        let error = match metadata(update.as_object_mut().unwrap(), &contract, 1) {
            Ok(_) => panic!("invalid retrieved_at should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("invalid retrieved_at"));
    }
}
