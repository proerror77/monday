use super::{
    wire::{
        parse_row, RawBook, RawBookLevel, RawContract, RawReference, RawRow, RawSettlement,
        RawTrade, RowContext, ROW_SCHEMA,
    },
    SealedPolymarketEvidenceTriplet,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet};

const WINDOW_SECS: i64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BinaryOutcomeSide {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceTradeSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolymarketEvidenceIdentity {
    pub content_sha256: String,
    pub manifest_sha256: String,
    pub rows: u64,
    pub events: u64,
    pub event_start_gte: DateTime<Utc>,
    pub event_start_lt: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolymarketEvidenceContract {
    pub market_id: String,
    pub condition_id: String,
    pub symbol: String,
    pub event_start: DateTime<Utc>,
    pub event_end: DateTime<Utc>,
    pub up_token_id: String,
    pub down_token_id: String,
    pub price_to_beat: Decimal,
    pub resolution_source: String,
    pub available_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolymarketBookLevel {
    pub price: Decimal,
    pub size: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolymarketEvidenceBook {
    pub market_id: String,
    pub token_id: String,
    pub side: BinaryOutcomeSide,
    pub source_time: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub bid: Option<Decimal>,
    pub ask: Option<Decimal>,
    pub bid_size: Option<Decimal>,
    pub ask_size: Option<Decimal>,
    pub bid_levels: Option<Vec<PolymarketBookLevel>>,
    pub ask_levels: Option<Vec<PolymarketBookLevel>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolymarketEvidenceReference {
    pub market_id: String,
    pub source_time: DateTime<Utc>,
    pub price: Decimal,
    pub is_carried_forward: bool,
    pub available_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolymarketEvidenceTrade {
    pub market_id: String,
    pub token_id: String,
    pub side: BinaryOutcomeSide,
    pub trade_side: EvidenceTradeSide,
    pub trade_time: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub size: Decimal,
    pub price: Decimal,
    pub record_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolymarketEvidenceSettlement {
    pub market_id: String,
    pub winning_side: BinaryOutcomeSide,
    pub winning_token_id: String,
    pub up_price: Decimal,
    pub down_price: Decimal,
    pub retrieved_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
}

/// Trust-bearing handle; its projection structs are non-authoritative data carriers.
#[derive(Debug)]
pub struct VerifiedPolymarketEvidence {
    identity: PolymarketEvidenceIdentity,
    contracts: Vec<PolymarketEvidenceContract>,
    books: Vec<PolymarketEvidenceBook>,
    references: Vec<PolymarketEvidenceReference>,
    trades: Vec<PolymarketEvidenceTrade>,
    settlements: Vec<PolymarketEvidenceSettlement>,
}

impl VerifiedPolymarketEvidence {
    pub fn identity(&self) -> &PolymarketEvidenceIdentity {
        &self.identity
    }

    pub fn contracts(&self) -> &[PolymarketEvidenceContract] {
        &self.contracts
    }

    pub fn books(&self) -> &[PolymarketEvidenceBook] {
        &self.books
    }

    pub fn references(&self) -> &[PolymarketEvidenceReference] {
        &self.references
    }

    pub fn trades(&self) -> &[PolymarketEvidenceTrade] {
        &self.trades
    }

    pub fn settlements(&self) -> &[PolymarketEvidenceSettlement] {
        &self.settlements
    }
}

#[derive(Debug)]
struct Contract {
    context: RowContext,
    token_ids: [String; 2],
    outcomes: [String; 2],
    sides: [BinaryOutcomeSide; 2],
    projection: PolymarketEvidenceContract,
}

impl Contract {
    fn from_raw(raw: RawContract, lower: DateTime<Utc>, upper: DateTime<Utc>) -> Result<Self> {
        validate_context(&raw.context, lower, upper)?;
        nonempty(&raw.source_token_ids[0], "contract token")?;
        nonempty(&raw.source_token_ids[1], "contract token")?;
        if raw.source_token_ids[0] == raw.source_token_ids[1] {
            bail!("contract token ids must be unique");
        }
        let sides = [
            semantic_side(&raw.source_outcomes[0])?,
            semantic_side(&raw.source_outcomes[1])?,
        ];
        let outcome_names = raw
            .source_outcomes
            .iter()
            .map(|outcome| outcome.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        if sides[0] == sides[1]
            || !matches!(
                outcome_names,
                names if names == BTreeSet::from(["up".to_owned(), "down".to_owned()])
                    || names == BTreeSet::from(["yes".to_owned(), "no".to_owned()])
            )
            || raw.price_to_beat <= Decimal::ZERO
        {
            bail!("contract must define one positive binary UP/DOWN market");
        }
        if raw.available_at
            != raw
                .metadata_retrieved_at
                .max(raw.discovery_recorded_at)
                .max(raw.metadata_recorded_at)
            || raw.discovery_source_sequence == 0
            || raw.metadata_source_sequence == 0
            || raw.source_datasets != ["crypto_expiry", "crypto_expiry_reference"]
        {
            bail!("contract provenance or availability clock is invalid");
        }
        nonempty(&raw.resolution_source, "resolution_source")?;
        let up = sides
            .iter()
            .position(|side| *side == BinaryOutcomeSide::Up)
            .unwrap();
        let down = 1 - up;
        let projection = PolymarketEvidenceContract {
            market_id: raw.context.market_id.clone(),
            condition_id: raw.context.condition_id.clone(),
            symbol: raw.context.symbol.clone(),
            event_start: raw.context.event_start,
            event_end: raw.context.event_end,
            up_token_id: raw.source_token_ids[up].clone(),
            down_token_id: raw.source_token_ids[down].clone(),
            price_to_beat: raw.price_to_beat,
            resolution_source: raw.resolution_source,
            available_at: raw.available_at,
        };
        Ok(Self {
            context: raw.context,
            token_ids: raw.source_token_ids,
            outcomes: raw.source_outcomes,
            sides,
            projection,
        })
    }

    fn side_for_token(&self, token: &str) -> Result<BinaryOutcomeSide> {
        self.token_ids
            .iter()
            .position(|candidate| candidate == token)
            .map(|index| self.sides[index])
            .ok_or_else(|| anyhow!("row token does not belong to its contract"))
    }
}

#[derive(Default)]
struct Coverage {
    books: BTreeSet<BinaryOutcomeSide>,
    references: u64,
    trades: u64,
    settlement: bool,
}

pub fn verify_polymarket_evidence(
    sealed: SealedPolymarketEvidenceTriplet,
) -> Result<VerifiedPolymarketEvidence> {
    let (lower, upper) = sealed.selection_bounds()?;
    let identity = PolymarketEvidenceIdentity {
        content_sha256: sealed.content_sha256().to_owned(),
        manifest_sha256: sealed.manifest_sha256().to_owned(),
        rows: sealed.rows(),
        events: sealed.events(),
        event_start_gte: lower,
        event_start_lt: upper,
    };
    let mut contracts = BTreeMap::new();
    let mut deferred = Vec::with_capacity(usize::try_from(sealed.rows())?);
    for (index, frame) in sealed.framed_rows().enumerate() {
        let row = parse_row(frame).with_context(|| format!("verify evidence row {}", index + 1))?;
        match row {
            RawRow::Contract(raw) => {
                let market_id = raw.context.market_id.clone();
                let contract = Contract::from_raw(raw, lower, upper)?;
                if contracts.insert(market_id, contract).is_some() {
                    bail!("duplicate market contract");
                }
            }
            row => deferred.push(row),
        }
    }
    if u64::try_from(contracts.len())? != identity.events {
        bail!("market contract count does not match sealed event count");
    }
    validate_unique_contract_identities(&contracts)?;

    let mut books = Vec::new();
    let mut references = Vec::new();
    let mut trades = Vec::new();
    let mut settlements = Vec::new();
    let mut coverage: BTreeMap<String, Coverage> = BTreeMap::new();
    let mut trade_ids = BTreeSet::new();
    for row in deferred {
        match row {
            RawRow::Book(raw) => {
                let contract = contract_for(&contracts, &raw.context)?;
                let side = contract.side_for_token(&raw.token_id)?;
                validate_book(&raw, contract)?;
                coverage
                    .entry(raw.context.market_id.clone())
                    .or_default()
                    .books
                    .insert(side);
                books.push(PolymarketEvidenceBook {
                    market_id: raw.context.market_id,
                    token_id: raw.token_id,
                    side,
                    source_time: raw.ts,
                    available_at: raw.available_at,
                    bid: raw.bid,
                    ask: raw.ask,
                    bid_size: raw.bid_size,
                    ask_size: raw.ask_size,
                    bid_levels: project_book_levels(raw.bid_levels),
                    ask_levels: project_book_levels(raw.ask_levels),
                });
            }
            RawRow::Reference(raw) => {
                let contract = contract_for(&contracts, &raw.context)?;
                validate_reference(&raw, contract)?;
                coverage
                    .entry(raw.context.market_id.clone())
                    .or_default()
                    .references += 1;
                references.push(PolymarketEvidenceReference {
                    market_id: raw.context.market_id,
                    source_time: raw.ts,
                    price: raw.price,
                    is_carried_forward: raw.is_carried_forward,
                    available_at: raw.available_at,
                });
            }
            RawRow::Trade(raw) => {
                let contract = contract_for(&contracts, &raw.context)?;
                let (side, trade_side) = validate_trade(&raw, contract)?;
                if !trade_ids.insert(raw.record_id.clone()) {
                    bail!("duplicate Polymarket trade record_id");
                }
                coverage
                    .entry(raw.context.market_id.clone())
                    .or_default()
                    .trades += 1;
                trades.push(PolymarketEvidenceTrade {
                    market_id: raw.context.market_id,
                    token_id: raw.token_id,
                    side,
                    trade_side,
                    trade_time: raw.trade_ts,
                    available_at: raw.available_at,
                    size: raw.size,
                    price: raw.price,
                    record_id: raw.record_id,
                });
            }
            RawRow::Settlement(raw) => {
                let contract = contract_for(&contracts, &raw.context)?;
                let settlement = validate_settlement(&raw, contract)?;
                let event = coverage.entry(raw.context.market_id).or_default();
                if std::mem::replace(&mut event.settlement, true) {
                    bail!("duplicate official settlement evidence");
                }
                settlements.push(settlement);
            }
            RawRow::Contract(_) => unreachable!("contracts were separated in the first pass"),
        }
    }
    let expected_books = BTreeSet::from([BinaryOutcomeSide::Up, BinaryOutcomeSide::Down]);
    for market_id in contracts.keys() {
        let event = coverage
            .get(market_id)
            .ok_or_else(|| anyhow!("market has no evidence"))?;
        if event.books != expected_books
            || event.references == 0
            || event.trades == 0
            || !event.settlement
        {
            bail!("market {market_id} is missing one or more required evidence surfaces");
        }
    }

    Ok(VerifiedPolymarketEvidence {
        identity,
        contracts: contracts
            .into_values()
            .map(|contract| contract.projection)
            .collect(),
        books,
        references,
        trades,
        settlements,
    })
}

fn validate_context(
    context: &RowContext,
    lower: DateTime<Utc>,
    upper: DateTime<Utc>,
) -> Result<()> {
    nonempty(&context.market_id, "market_id")?;
    nonempty(&context.condition_id, "condition_id")?;
    if context.schema != ROW_SCHEMA
        || !matches!(context.symbol.as_str(), "BTCUSDT" | "SOLUSDT")
        || context.window_secs != WINDOW_SECS as u64
        || (context.event_end - context.event_start) != Duration::seconds(WINDOW_SECS)
        || context.event_start < lower
        || context.event_start >= upper
    {
        bail!("evidence row context is outside the sealed event contract");
    }
    Ok(())
}

fn validate_unique_contract_identities(contracts: &BTreeMap<String, Contract>) -> Result<()> {
    let mut conditions = BTreeSet::new();
    let mut tokens = BTreeSet::new();
    for contract in contracts.values() {
        if !conditions.insert(&contract.context.condition_id)
            || contract.token_ids.iter().any(|token| !tokens.insert(token))
        {
            bail!("contract condition and token identities must be globally unique");
        }
    }
    Ok(())
}

fn contract_for<'a>(
    contracts: &'a BTreeMap<String, Contract>,
    context: &RowContext,
) -> Result<&'a Contract> {
    let contract = contracts
        .get(&context.market_id)
        .ok_or_else(|| anyhow!("row references an unknown market"))?;
    if context != &contract.context {
        bail!("row context contradicts its market contract");
    }
    Ok(contract)
}

fn validate_book(raw: &RawBook, contract: &Contract) -> Result<()> {
    if raw.ts < contract.context.event_start
        || raw.ts >= contract.context.event_end
        || raw.recorded_at < raw.ts
        || raw.available_at != raw.recorded_at
        || raw.source_sequence == 0
        || raw.source_dataset != "crypto_expiry"
        || raw.bid.is_some() != raw.bid_size.is_some()
        || raw.ask.is_some() != raw.ask_size.is_some()
    {
        bail!("orderbook row violates its clock or source contract");
    }
    for price in [raw.bid, raw.ask].into_iter().flatten() {
        probability(price, "orderbook price")?;
    }
    for size in [raw.bid_size, raw.ask_size].into_iter().flatten() {
        positive(size, "orderbook size")?;
    }
    if raw.bid.zip(raw.ask).is_some_and(|(bid, ask)| bid > ask) {
        bail!("orderbook top of book is crossed");
    }
    for level in raw
        .bid_levels
        .iter()
        .flatten()
        .chain(raw.ask_levels.iter().flatten())
    {
        probability(level.price, "orderbook level price")?;
        positive(level.size, "orderbook level size")?;
    }
    if !top_matches_full_depth(raw.bid, raw.bid_size, &raw.bid_levels, true)
        || !top_matches_full_depth(raw.ask, raw.ask_size, &raw.ask_levels, false)
    {
        bail!("orderbook top of book disagrees with full depth");
    }
    Ok(())
}

fn top_matches_full_depth(
    price: Option<Decimal>,
    size: Option<Decimal>,
    levels: &Option<Vec<RawBookLevel>>,
    is_bid: bool,
) -> bool {
    match (price.zip(size), levels.as_deref()) {
        (None, Some([])) => true,
        (Some((price, size)), Some(levels)) if !levels.is_empty() => {
            let best_price = if is_bid {
                levels.iter().map(|level| level.price).max()
            } else {
                levels.iter().map(|level| level.price).min()
            }
            .expect("non-empty depth has a best price");
            let mut best_levels = levels.iter().filter(|level| level.price == best_price);
            best_levels
                .next()
                .is_some_and(|level| level.price == price && level.size == size)
                && best_levels.next().is_none()
        }
        _ => false,
    }
}

fn project_book_levels(levels: Option<Vec<RawBookLevel>>) -> Option<Vec<PolymarketBookLevel>> {
    levels.map(|levels| {
        levels
            .into_iter()
            .map(|level| PolymarketBookLevel {
                price: level.price,
                size: level.size,
            })
            .collect()
    })
}

fn validate_reference(raw: &RawReference, contract: &Contract) -> Result<()> {
    let expected_symbol = match contract.context.symbol.as_str() {
        "BTCUSDT" => "btc/usd",
        "SOLUSDT" => "sol/usd",
        _ => unreachable!("context validation fixes symbols"),
    };
    let earliest = contract
        .context
        .event_start
        .checked_sub_signed(Duration::seconds(30))
        .ok_or_else(|| anyhow!("reference lookback underflows"))?;
    let received_at = raw.received_at.unwrap_or(raw.recorded_at);
    if raw.source != "chainlink"
        || raw.asset_class != "crypto"
        || !raw.source_symbol.eq_ignore_ascii_case(expected_symbol)
        || raw.ts < earliest
        || raw.ts >= contract.context.event_end
        || received_at < raw.ts
        || raw.recorded_at < raw.ts
        || raw.available_at != raw.recorded_at.max(received_at)
        || raw.source_sequence == 0
        || raw.source_dataset != "crypto_expiry"
    {
        bail!("Chainlink reference row violates its source or clock contract");
    }
    positive(raw.price, "Chainlink reference price")
}

fn validate_trade(
    raw: &RawTrade,
    contract: &Contract,
) -> Result<(BinaryOutcomeSide, EvidenceTradeSide)> {
    let index = usize::from(raw.outcome_index);
    if index >= 2
        || raw.token_id != contract.token_ids[index]
        || raw.source_outcome != contract.outcomes[index]
    {
        bail!("trade token, outcome, and source index disagree");
    }
    let trade_side = match raw.side.as_str() {
        "BUY" => EvidenceTradeSide::Buy,
        "SELL" => EvidenceTradeSide::Sell,
        _ => bail!("trade side must be BUY or SELL"),
    };
    if raw.record_id_version != "v2"
        || raw.source != "polymarket_data_api"
        || raw.source_dataset != "crypto_expiry_reference"
        || raw.source_sequence == 0
        || raw.received_at < raw.trade_ts
        || raw.recorded_at < raw.trade_ts
        || raw.available_at != raw.received_at.max(raw.recorded_at)
        || raw.trade_ts_unix != raw.trade_ts.timestamp()
    {
        bail!("Polymarket trade violates its source or clock contract");
    }
    for (value, label) in [
        (&raw.record_id, "record_id"),
        (&raw.transaction_hash, "transaction_hash"),
        (&raw.proxy_wallet, "proxy_wallet"),
    ] {
        nonempty(value, label)?;
    }
    positive(raw.size, "trade size")?;
    probability(raw.price, "trade price")?;
    Ok((contract.sides[index], trade_side))
}

fn validate_settlement(
    raw: &RawSettlement,
    contract: &Contract,
) -> Result<PolymarketEvidenceSettlement> {
    if raw.source_token_ids != contract.token_ids
        || raw.source_outcomes != contract.outcomes
        || raw.resolution_source != "gamma_api_closed_market"
        || raw.retrieved_at < contract.context.event_end
        || raw.available_at != raw.retrieved_at.max(raw.recorded_at)
        || raw.source_sequence == 0
        || raw.source_dataset != "crypto_expiry_reference"
    {
        bail!("settlement row contradicts its contract or source clocks");
    }
    let [left, right] = raw.source_outcome_prices;
    if !((left == Decimal::ZERO && right == Decimal::ONE)
        || (left == Decimal::ONE && right == Decimal::ZERO))
    {
        bail!("settlement must contain an exact complementary 0/1 pair");
    }
    let winner = if left == Decimal::ONE { 0 } else { 1 };
    if raw.winning_token_id != contract.token_ids[winner]
        || raw.winning_outcome != contract.outcomes[winner]
    {
        bail!("settlement winner does not match its 1.0 leg");
    }
    let up = contract
        .sides
        .iter()
        .position(|side| *side == BinaryOutcomeSide::Up)
        .unwrap();
    Ok(PolymarketEvidenceSettlement {
        market_id: raw.context.market_id.clone(),
        winning_side: contract.sides[winner],
        winning_token_id: raw.winning_token_id.clone(),
        up_price: raw.source_outcome_prices[up],
        down_price: raw.source_outcome_prices[1 - up],
        retrieved_at: raw.retrieved_at,
        observed_at: raw.available_at,
    })
}

fn semantic_side(outcome: &str) -> Result<BinaryOutcomeSide> {
    match outcome.to_ascii_lowercase().as_str() {
        "up" | "yes" => Ok(BinaryOutcomeSide::Up),
        "down" | "no" => Ok(BinaryOutcomeSide::Down),
        _ => bail!("contract outcomes must be an UP/DOWN or YES/NO pair"),
    }
}

fn probability(value: Decimal, label: &str) -> Result<()> {
    ensure!(
        value >= Decimal::ZERO && value <= Decimal::ONE,
        "{label} outside [0,1]"
    );
    Ok(())
}

fn positive(value: Decimal, label: &str) -> Result<()> {
    ensure!(value > Decimal::ZERO, "{label} must be positive");
    Ok(())
}

fn nonempty(value: &str, label: &str) -> Result<()> {
    ensure!(!value.is_empty(), "{label} must be non-empty");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polymarket_evidence::{artifact::tests, seal_polymarket_evidence_triplet};
    use serde_json::{json, Value};

    #[rustfmt::skip]
    fn valid_rows() -> Vec<Value> {
        let context = json!({"schema":ROW_SCHEMA,"market_id":"market-1","condition_id":"condition-1","symbol":"BTCUSDT","event_start":"2026-07-17T05:30:00Z","event_end":"2026-07-17T05:35:00Z","window_secs":300});
        let row = |surface: &str, fields: Value| { let mut value=context.clone(); value["surface"]=json!(surface); value.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone()); value };
        vec![
            row("market_contract", json!({"source_token_ids":["down-token","up-token"],"source_outcomes":["Down","Up"],"price_to_beat":"63000","resolution_source":"https://data.chain.link/streams/btc-usd","metadata_retrieved_at":"2026-07-17T05:29:58Z","discovery_recorded_at":"2026-07-17T05:29:57Z","metadata_recorded_at":"2026-07-17T05:29:59Z","available_at":"2026-07-17T05:29:59Z","discovery_source_sequence":1,"metadata_source_sequence":2,"source_datasets":["crypto_expiry","crypto_expiry_reference"]})),
            row("orderbook_snapshot", json!({"token_id":"down-token","ts":"2026-07-17T05:30:01Z","recorded_at":"2026-07-17T05:30:02Z","available_at":"2026-07-17T05:30:02Z","source_sequence":3,"source_dataset":"crypto_expiry","bid":"0.4","ask":"0.5","bid_size":"10","ask_size":"11","bid_levels":[{"price":"0.4","size":"10"}],"ask_levels":[{"price":"0.5","size":"11"}]})),
            row("orderbook_snapshot", json!({"token_id":"up-token","ts":"2026-07-17T05:30:01Z","recorded_at":"2026-07-17T05:30:02Z","available_at":"2026-07-17T05:30:02Z","source_sequence":4,"source_dataset":"crypto_expiry","bid":"0.5","ask":"0.6","bid_size":"11","ask_size":"10","bid_levels":[{"price":"0.5","size":"11"}],"ask_levels":[{"price":"0.6","size":"10"}]})),
            row("chainlink_reference", json!({"source":"chainlink","asset_class":"crypto","source_symbol":"btc/usd","price":"63000","full_accuracy_value":null,"is_carried_forward":false,"ts":"2026-07-17T05:29:55Z","received_at":"2026-07-17T05:29:56Z","available_at":"2026-07-17T05:29:57Z","recorded_at":"2026-07-17T05:29:57Z","source_sequence":5,"source_dataset":"crypto_expiry"})),
            row("polymarket_trade", json!({"record_id":"trade-v2-1","record_id_version":"v2","token_id":"up-token","source_outcome":"Up","outcome_index":1,"side":"BUY","size":"2","price":"0.6","trade_ts":"2026-07-17T05:30:03Z","trade_ts_unix":1784266203_i64,"transaction_hash":"0xabc","proxy_wallet":"0xdef","source":"polymarket_data_api","received_at":"2026-07-17T05:30:04Z","available_at":"2026-07-17T05:30:05Z","recorded_at":"2026-07-17T05:30:05Z","source_sequence":6,"source_dataset":"crypto_expiry_reference"})),
            row("official_settlement_evidence", json!({"source_token_ids":["down-token","up-token"],"source_outcomes":["Down","Up"],"source_outcome_prices":["0","1"],"winning_token_id":"up-token","winning_outcome":"Up","resolution_source":"gamma_api_closed_market","retrieved_at":"2026-07-17T05:35:01Z","available_at":"2026-07-17T05:35:02Z","recorded_at":"2026-07-17T05:35:02Z","source_sequence":7,"source_dataset":"crypto_expiry_reference"})),
        ]
    }

    fn valid_two_event_rows() -> Vec<Value> {
        let mut rows = valid_rows();
        let mut second = valid_rows();
        for row in &mut second {
            row["market_id"] = json!("market-2");
            row["condition_id"] = json!("condition-2");
            row["symbol"] = json!("SOLUSDT");
        }
        let contract = row(&mut second, "market_contract", 0);
        contract["source_token_ids"] = json!(["down-token-2", "up-token-2"]);
        contract["price_to_beat"] = json!("150");
        contract["resolution_source"] = json!("https://data.chain.link/streams/sol-usd");
        row(&mut second, "orderbook_snapshot", 0)["token_id"] = json!("down-token-2");
        row(&mut second, "orderbook_snapshot", 1)["token_id"] = json!("up-token-2");
        row(&mut second, "chainlink_reference", 0)["source_symbol"] = json!("sol/usd");
        let trade = row(&mut second, "polymarket_trade", 0);
        trade["record_id"] = json!("trade-v2-2");
        trade["token_id"] = json!("up-token-2");
        trade["transaction_hash"] = json!("0x123");
        let settlement = row(&mut second, "official_settlement_evidence", 0);
        settlement["source_token_ids"] = json!(["down-token-2", "up-token-2"]);
        settlement["winning_token_id"] = json!("up-token-2");
        rows.extend(second);
        rows
    }

    fn verify_rows(rows: &[Value]) -> Result<VerifiedPolymarketEvidence> {
        let temp = tempfile::tempdir().unwrap();
        let triplet = tests::write_triplet_rows(&temp, rows);
        let sealed = seal_polymarket_evidence_triplet(&triplet, &tests::trust(&triplet))?;
        verify_polymarket_evidence(sealed)
    }

    fn row<'a>(rows: &'a mut [Value], surface: &str, offset: usize) -> &'a mut Value {
        rows.iter_mut()
            .filter(|row| row["surface"] == surface)
            .nth(offset)
            .unwrap()
    }

    fn assert_rejected(rows: &[Value], message: &str) {
        let error = format!("{:#}", verify_rows(rows).unwrap_err());
        assert!(error.contains(message), "{error}");
    }

    #[test]
    fn verifies_reversed_binary_arrays_without_index_assumption() {
        let verified = verify_rows(&valid_rows()).unwrap();
        assert_eq!(verified.identity().manifest_sha256.len(), 64);
        assert!(verified
            .identity()
            .manifest_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        assert_eq!(verified.contracts()[0].up_token_id, "up-token");
        assert_eq!(verified.books()[0].side, BinaryOutcomeSide::Down);
        assert_eq!(verified.trades()[0].side, BinaryOutcomeSide::Up);
        assert_eq!(
            verified.settlements()[0].winning_side,
            BinaryOutcomeSide::Up
        );
    }

    #[test]
    fn rejects_trade_index_or_numeric_contradictions() {
        let mut rows = valid_rows();
        row(&mut rows, "polymarket_trade", 0)["outcome_index"] = json!(0);
        assert_rejected(&rows, "disagree");
        let mut rows = valid_rows();
        row(&mut rows, "polymarket_trade", 0)["price"] = json!("1.01");
        assert_rejected(&rows, "[0,1]");
        let mut rows = valid_rows();
        let trade = row(&mut rows, "polymarket_trade", 0);
        trade["recorded_at"] = json!("2026-07-17T05:30:02Z");
        trade["available_at"] = json!("2026-07-17T05:30:04Z");
        assert_rejected(&rows, "clock");
    }

    #[test]
    fn enforces_reference_preopen_window() {
        let mut rows = valid_rows();
        row(&mut rows, "chainlink_reference", 0)["ts"] = json!("2026-07-17T05:29:29Z");
        assert_rejected(&rows, "Chainlink");
    }

    #[test]
    fn rejects_invalid_book_or_missing_complementary_book() {
        let mut rows = valid_rows();
        row(&mut rows, "orderbook_snapshot", 0)["bid_levels"][0]["size"] = json!("0");
        assert_rejected(&rows, "positive");
        let mut rows = valid_rows();
        rows.remove(2);
        assert_rejected(&rows, "missing");
        let mut rows = valid_rows();
        let book = row(&mut rows, "orderbook_snapshot", 0);
        book["recorded_at"] = json!("2026-07-17T05:30:00Z");
        book["available_at"] = json!("2026-07-17T05:30:00Z");
        assert_rejected(&rows, "clock");
    }

    #[test]
    fn rejects_top_of_book_that_disagrees_with_full_depth() {
        let mut rows = valid_rows();
        row(&mut rows, "orderbook_snapshot", 0)["bid"] = json!("0.3");
        assert_rejected(&rows, "full depth");

        let mut rows = valid_rows();
        row(&mut rows, "orderbook_snapshot", 0)["ask_size"] = json!("12");
        assert_rejected(&rows, "full depth");

        let mut rows = valid_rows();
        row(&mut rows, "orderbook_snapshot", 0)["bid_levels"] = json!([
            {"price":"0.4","size":"10"},
            {"price":"0.4","size":"10"}
        ]);
        assert_rejected(&rows, "full depth");

        let mut rows = valid_rows();
        let book = row(&mut rows, "orderbook_snapshot", 0);
        book["bid"] = Value::Null;
        book["bid_size"] = Value::Null;
        assert_rejected(&rows, "full depth");

        let mut rows = valid_rows();
        row(&mut rows, "orderbook_snapshot", 0)["bid_levels"] = json!([]);
        assert_rejected(&rows, "full depth");
    }

    #[test]
    fn accepts_unique_best_levels_independent_of_depth_order() {
        let mut rows = valid_rows();
        let book = row(&mut rows, "orderbook_snapshot", 0);
        book["bid_levels"] = json!([
            {"price":"0.3","size":"7"},
            {"price":"0.4","size":"10"},
            {"price":"0.35","size":"8"}
        ]);
        book["ask_levels"] = json!([
            {"price":"0.7","size":"3"},
            {"price":"0.5","size":"11"},
            {"price":"0.6","size":"4"}
        ]);
        verify_rows(&rows).unwrap();

        let mut rows = valid_rows();
        let book = row(&mut rows, "orderbook_snapshot", 0);
        book["bid"] = Value::Null;
        book["bid_size"] = Value::Null;
        book["bid_levels"] = json!([]);
        verify_rows(&rows).unwrap();

        let mut rows = valid_rows();
        let book = row(&mut rows, "orderbook_snapshot", 0);
        book["bid"] = Value::Null;
        book["bid_size"] = Value::Null;
        book["bid_levels"] = Value::Null;
        assert_rejected(&rows, "full depth");
    }

    #[test]
    fn rejects_settlement_complement_or_winner_mismatch() {
        let mut rows = valid_rows();
        row(&mut rows, "official_settlement_evidence", 0)["source_outcome_prices"] =
            json!(["0.5", "0.5"]);
        assert_rejected(&rows, "0/1");
        let mut rows = valid_rows();
        row(&mut rows, "official_settlement_evidence", 0)["winning_token_id"] = json!("down-token");
        assert_rejected(&rows, "winner");
        let mut rows = valid_rows();
        let settlement = row(&mut rows, "official_settlement_evidence", 0);
        settlement["retrieved_at"] = json!("2026-07-17T05:34:58Z");
        settlement["recorded_at"] = json!("2026-07-17T05:34:59Z");
        settlement["available_at"] = json!("2026-07-17T05:34:59Z");
        assert!(verify_rows(&rows).is_err());
    }

    #[test]
    fn rejects_old_or_derived_wire_fields() {
        let mut rows = valid_rows();
        row(&mut rows, "market_contract", 0)["up_token_id"] = json!("up-token");
        assert_rejected(&rows, "source-neutral");
        let mut rows = valid_rows();
        row(&mut rows, "market_contract", 0)["source_outcomes"] = json!(["Up", "No"]);
        assert!(verify_rows(&rows).is_err());
    }

    #[test]
    fn rejects_global_condition_or_token_identity_reuse() {
        let mut rows = valid_two_event_rows();
        for row in rows.iter_mut().filter(|row| row["market_id"] == "market-2") {
            row["condition_id"] = json!("condition-1");
        }
        assert_rejected(&rows, "globally unique");

        let mut rows = valid_two_event_rows();
        row(&mut rows, "market_contract", 1)["source_token_ids"][0] = json!("down-token");
        assert_rejected(&rows, "globally unique");
    }

    #[test]
    fn rejects_duplicate_trade_or_event_settlement() {
        let mut rows = valid_rows();
        let duplicate = row(&mut rows, "polymarket_trade", 0).clone();
        rows.push(duplicate);
        assert_rejected(&rows, "duplicate Polymarket trade");

        let mut rows = valid_two_event_rows();
        let duplicate = row(&mut rows, "official_settlement_evidence", 0).clone();
        rows.retain(|row| {
            row["market_id"] != "market-2" || row["surface"] != "official_settlement_evidence"
        });
        rows.push(duplicate);
        assert_rejected(&rows, "duplicate official settlement");
    }

    #[test]
    fn rejects_context_mismatch_or_event_local_missing_surface() {
        let mut rows = valid_rows();
        row(&mut rows, "orderbook_snapshot", 0)["condition_id"] = json!("wrong-condition");
        assert_rejected(&rows, "contradicts");

        for surface in ["chainlink_reference", "polymarket_trade"] {
            let mut rows = valid_two_event_rows();
            rows.retain(|row| row["market_id"] != "market-1" || row["surface"] != surface);
            assert_rejected(&rows, "missing");
        }
        let mut rows = valid_rows();
        rows.retain(|row| row["surface"] != "official_settlement_evidence");
        assert_rejected(&rows, "manifest identity is inconsistent");
    }
}
