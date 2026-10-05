//! Deterministic probability reversal. Execution and account truth remain shared.
pub mod logic;
use hft_core::{
    AssetClass, OrderId, OrderType, Price, Quantity, Side, Symbol, TimeInForce, Timestamp, VenueId,
};
use hft_research_manifest::prediction_probability::{BinaryEpisodeV1, ProbabilityReversalSpecV1};
use logic::{Outcome, Thresholds};
use ports::{AccountView, ExecutionEvent, MarketEvent, OrderIntent, Strategy, StrategyContext};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProbabilityStrategyError {
    #[error("invalid fixed probability specification: {0}")]
    Spec(#[from] hft_research_manifest::prediction_probability::ProbabilitySpecError),
    #[error("strategy name and positive per-order limits are required")]
    Limits,
}
#[derive(Debug, Clone)]
pub struct ProbabilityStrategyConfig {
    pub name: String,
    pub spec: ProbabilityReversalSpecV1,
    pub max_order_notional: Decimal,
    pub max_order_quantity: Decimal,
}
#[derive(Debug, Clone, Copy)]
struct Quote {
    ask: Decimal,
    observed: Timestamp,
    sequence: u64,
}
#[derive(Debug, Clone)]
struct Pending {
    symbol: Symbol,
    side: Side,
    filled_counted: bool,
}
struct PendingIntent {
    side: Side,
    queued_client_id: Option<String>,
}
pub struct ProbabilityReversalStrategy {
    config: ProbabilityStrategyConfig,
    thresholds: Thresholds,
    tokens: HashMap<Symbol, (usize, Outcome)>,
    quotes: HashMap<Symbol, Quote>,
    bound_books: HashSet<Symbol>,
    previous: HashMap<usize, f64>,
    pending: HashMap<Symbol, PendingIntent>,
    orders: HashMap<OrderId, Pending>,
    retired: HashSet<usize>,
    day: Option<u64>,
    daily_entries: u32,
}
impl ProbabilityReversalStrategy {
    pub fn new(config: ProbabilityStrategyConfig) -> Result<Self, ProbabilityStrategyError> {
        config.spec.validate()?;
        if config.name.trim().is_empty()
            || config.max_order_notional <= Decimal::ZERO
            || config.max_order_quantity <= Decimal::ZERO
        {
            return Err(ProbabilityStrategyError::Limits);
        }
        let mut tokens = HashMap::new();
        for (index, episode) in config.spec.episodes.iter().enumerate() {
            tokens.insert(Symbol::new(&episode.up_token), (index, Outcome::Up));
            tokens.insert(Symbol::new(&episode.down_token), (index, Outcome::Down));
        }
        let p = &config.spec;
        let thresholds = Thresholds {
            prev_prob_low: p.prev_prob_low,
            curr_prob_high: p.curr_prob_high,
            prev_prob_high: p.prev_prob_high,
            curr_prob_low: p.curr_prob_low,
            take_profit_prob: p.take_profit_prob,
            stop_loss_prob: p.stop_loss_prob,
        };
        Ok(Self {
            config,
            thresholds,
            tokens,
            quotes: HashMap::new(),
            bound_books: HashSet::new(),
            previous: HashMap::new(),
            pending: HashMap::new(),
            orders: HashMap::new(),
            retired: HashSet::new(),
            day: None,
            daily_entries: 0,
        })
    }
    fn quantity(account: &AccountView, token: &Symbol) -> Decimal {
        account
            .positions
            .get(token)
            .map_or(Decimal::ZERO, |p| p.quantity.0)
    }
    fn intent(
        &mut self,
        index: usize,
        token: Symbol,
        side: Side,
        price: Decimal,
        quantity: Decimal,
    ) -> Vec<OrderIntent> {
        if quantity <= Decimal::ZERO || self.pending.contains_key(&token) {
            return Vec::new();
        }
        let cap_by_price = self
            .config
            .max_order_notional
            .checked_div(price)
            .unwrap_or(self.config.max_order_quantity);
        let quantity = quantity
            .min(cap_by_price)
            .min(self.config.max_order_quantity)
            .round_dp_with_strategy(6, rust_decimal::RoundingStrategy::ToZero);
        if quantity <= Decimal::ZERO {
            return Vec::new();
        }
        self.pending.insert(
            token.clone(),
            PendingIntent {
                side,
                queued_client_id: None,
            },
        );
        debug_assert!(index < self.config.spec.episodes.len());
        vec![OrderIntent::prediction_market(
            token,
            side,
            Quantity(quantity),
            OrderType::Limit,
            Some(Price(price)),
            TimeInForce::IOC,
            self.config.name.clone(),
            VenueId::POLYMARKET,
        )]
    }
    fn quote(
        &mut self,
        token: &Symbol,
        bid: Decimal,
        ask: Decimal,
        observed: Timestamp,
        sequence: u64,
        account: &AccountView,
    ) -> Vec<OrderIntent> {
        let Some(&(index, outcome)) = self.tokens.get(token) else {
            return Vec::new();
        };
        if self.retired.contains(&index)
            || self
                .quotes
                .get(token)
                .is_some_and(|q| observed <= q.observed || sequence <= q.sequence)
        {
            return Vec::new();
        }
        let episode = &self.config.spec.episodes[index];
        if observed < episode.start_us
            || observed >= episode.end_us
            || bid <= Decimal::ZERO
            || bid >= Decimal::ONE
            || ask <= Decimal::ZERO
            || ask >= Decimal::ONE
            || bid > ask
        {
            self.quotes.remove(token);
            self.previous.remove(&index);
            return Vec::new();
        }
        self.quotes.insert(
            token.clone(),
            Quote {
                ask,
                observed,
                sequence,
            },
        );
        let day = observed / 86_400_000_000;
        if self.day != Some(day) {
            self.day = Some(day);
            self.daily_entries = 0;
        }
        let probability = ask.to_f64().unwrap_or(f64::NAN);
        let held = Self::quantity(account, token);
        if held > Decimal::ZERO && self.thresholds.exit(probability) {
            if outcome == Outcome::Up {
                self.previous.insert(index, probability);
            }
            return self.intent(index, token.clone(), Side::Sell, bid, held);
        }
        if outcome == Outcome::Down {
            return Vec::new();
        }
        let previous = self.previous.insert(index, probability);
        let remaining = (episode.end_us - observed) / 1_000_000;
        if remaining < self.config.spec.min_time_remaining_secs
            || remaining > self.config.spec.max_time_remaining_secs
            || self.daily_entries >= self.config.spec.max_daily_trades
            || account
                .positions
                .values()
                .filter(|p| p.quantity.0 > Decimal::ZERO)
                .count()
                >= self.config.spec.max_positions
        {
            return Vec::new();
        }
        let Some(direction) = previous.and_then(|p| self.thresholds.entry(p, probability)) else {
            return Vec::new();
        };
        let selected = Symbol::new(match direction {
            Outcome::Up => &episode.up_token,
            Outcome::Down => &episode.down_token,
        });
        if Self::quantity(account, &selected) > Decimal::ZERO
            || self.pending.contains_key(&selected)
        {
            return Vec::new();
        }
        let Some(selected_quote) = self.quotes.get(&selected).copied() else {
            return Vec::new();
        };
        if selected_quote.observed > observed
            || observed - selected_quote.observed > self.config.spec.quote_max_age_us
        {
            return Vec::new();
        }
        self.intent(
            index,
            selected,
            Side::Buy,
            selected_quote.ask,
            logic::entry_quantity(self.config.spec.stake_usd, selected_quote.ask),
        )
    }
    fn reset_quotes(&mut self) {
        self.quotes.clear();
        self.bound_books.clear();
        self.previous.clear();
    }
    pub fn episodes(&self) -> &[BinaryEpisodeV1] {
        &self.config.spec.episodes
    }
}
impl Strategy for ProbabilityReversalStrategy {
    fn on_market_event(&mut self, event: &MarketEvent, account: &AccountView) -> Vec<OrderIntent> {
        match event {
            MarketEvent::Snapshot(s) if s.source_venue == Some(VenueId::POLYMARKET) => {
                let Some(&(index, _)) = self.tokens.get(&s.symbol) else {
                    return Vec::new();
                };
                if s.provider_identity
                    .as_ref()
                    .is_none_or(|id| id.market != self.config.spec.episodes[index].condition_id)
                {
                    self.bound_books.remove(&s.symbol);
                    self.quotes.remove(&s.symbol);
                    self.previous.remove(&index);
                    return Vec::new();
                }
                let Some(received) = s.timestamps.local_receive else {
                    return Vec::new();
                };
                match (s.bids.first(), s.asks.first()) {
                    (Some(b), Some(a))
                        if b.quantity.0 > Decimal::ZERO && a.quantity.0 > Decimal::ZERO =>
                    {
                        self.bound_books.insert(s.symbol.clone());
                        self.quote(
                            &s.symbol,
                            b.price.0,
                            a.price.0,
                            received.as_micros(),
                            s.sequence,
                            account,
                        )
                    }
                    _ => {
                        self.bound_books.remove(&s.symbol);
                        self.quotes.remove(&s.symbol);
                        self.previous.remove(&index);
                        Vec::new()
                    }
                }
            }
            MarketEvent::Quote(q) if q.source_venue == Some(VenueId::POLYMARKET) => {
                if !self.bound_books.contains(&q.symbol)
                    || q.bid.quantity.0 <= Decimal::ZERO
                    || q.ask.quantity.0 <= Decimal::ZERO
                {
                    return Vec::new();
                }
                let Some(received) = q.timestamps.local_receive else {
                    return Vec::new();
                };
                self.quote(
                    &q.symbol,
                    q.bid.price.0,
                    q.ask.price.0,
                    received.as_micros(),
                    q.sequence,
                    account,
                )
            }
            MarketEvent::Disconnect { source_venue, .. }
                if source_venue.is_none() || *source_venue == Some(VenueId::POLYMARKET) =>
            {
                self.reset_quotes();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
    fn on_market_event_with_context(
        &mut self,
        event: &MarketEvent,
        context: &StrategyContext<'_>,
    ) -> Vec<OrderIntent> {
        if let MarketEvent::Update(update) = event {
            if !self.bound_books.contains(&update.symbol) {
                return Vec::new();
            }
            let Some(book) = context
                .book
                .filter(|b| b.venue == VenueId::POLYMARKET && b.symbol == &update.symbol)
            else {
                return Vec::new();
            };
            let Some(received) = update.timestamps.local_receive else {
                return Vec::new();
            };
            let (Some(bid), Some(ask)) = (book.bid_prices.first(), book.ask_prices.first()) else {
                return Vec::new();
            };
            if book
                .bid_quantities
                .first()
                .is_none_or(|q| *q <= hft_core::FixedQuantity::ZERO)
                || book
                    .ask_quantities
                    .first()
                    .is_none_or(|q| *q <= hft_core::FixedQuantity::ZERO)
            {
                return Vec::new();
            }
            return self.quote(
                &update.symbol,
                Decimal::new(bid.raw(), 6),
                Decimal::new(ask.raw(), 6),
                received.as_micros(),
                book.sequence,
                context.account,
            );
        }
        self.on_market_event(event, context.account)
    }
    fn on_execution_event(
        &mut self,
        event: &ExecutionEvent,
        account: &AccountView,
    ) -> Vec<OrderIntent> {
        match event {
            ExecutionEvent::OrderNew {
                order_id,
                client_order_id,
                symbol,
                side,
                strategy_id,
                ..
            } if strategy_id == &self.config.name
                && self.pending.get(symbol).is_some_and(|pending| {
                    pending.side == *side
                        && pending
                            .queued_client_id
                            .as_ref()
                            .is_none_or(|id| client_order_id.as_ref() == Some(id))
                }) =>
            {
                self.orders.entry(order_id.clone()).or_insert(Pending {
                    symbol: symbol.clone(),
                    side: *side,
                    filled_counted: false,
                });
            }
            ExecutionEvent::Fill { order_id, .. } => {
                if let Some(order) = self.orders.get_mut(order_id) {
                    if order.side == Side::Buy && !order.filled_counted {
                        self.daily_entries = self.daily_entries.saturating_add(1);
                        order.filled_counted = true;
                    }
                    if order.side == Side::Sell
                        && Self::quantity(account, &order.symbol) <= Decimal::ZERO
                    {
                        if let Some((index, _)) = self.tokens.get(&order.symbol) {
                            self.retired.insert(*index);
                        }
                    }
                }
            }
            ExecutionEvent::OrderReject { order_id, .. }
            | ExecutionEvent::OrderCanceled { order_id, .. }
            | ExecutionEvent::OrderCompleted { order_id, .. } => {
                if let Some(order) = self.orders.remove(order_id) {
                    // Canonical OMS may report completion before the original
                    // Fill notification. Count the accepted entry exactly once.
                    if matches!(event, ExecutionEvent::OrderCompleted { total_filled, .. } if total_filled.0 > Decimal::ZERO)
                        && order.side == Side::Buy
                        && !order.filled_counted
                    {
                        self.daily_entries = self.daily_entries.saturating_add(1);
                    }
                    if matches!(event, ExecutionEvent::OrderCompleted { .. })
                        && order.side == Side::Sell
                        && Self::quantity(account, &order.symbol) <= Decimal::ZERO
                    {
                        if let Some((index, _)) = self.tokens.get(&order.symbol) {
                            self.retired.insert(*index);
                        }
                    }
                    self.pending.remove(&order.symbol);
                } else if matches!(event, ExecutionEvent::OrderReject { .. }) {
                    // Existing worker pre-submission rejects use the original
                    // envelope client ID as OrderId, without an OrderNew.
                    self.pending.retain(|_, pending| {
                        pending.queued_client_id.as_deref() != Some(order_id.0.as_str())
                    });
                }
            }
            _ => {}
        }
        Vec::new()
    }
    fn observe_execution_state(&mut self, event: &ExecutionEvent, account: &AccountView) {
        self.on_execution_event(event, account);
    }
    fn observe_intent_submission(
        &mut self,
        intent: &OrderIntent,
        result: ports::IntentSubmissionResult<'_>,
    ) {
        if intent.strategy_id != self.config.name {
            return;
        }
        let Some(pending) = self
            .pending
            .get_mut(&intent.symbol)
            .filter(|p| p.side == intent.side)
        else {
            return;
        };
        match result {
            ports::IntentSubmissionResult::Enqueued { client_order_id } => {
                if pending.queued_client_id.is_none() {
                    pending.queued_client_id = Some(client_order_id.into());
                }
            }
            ports::IntentSubmissionResult::NotSubmitted if pending.queued_client_id.is_none() => {
                self.pending.remove(&intent.symbol);
            }
            ports::IntentSubmissionResult::NotSubmitted => {}
        }
    }
    fn name(&self) -> &str {
        &self.config.name
    }
    fn supported_asset_classes(&self) -> &'static [AssetClass] {
        &[AssetClass::PredictionMarket]
    }
    fn intent_semantic_deadline(&self, intent: &OrderIntent) -> Option<Timestamp> {
        self.tokens
            .get(&intent.symbol)
            .map(|(index, _)| self.config.spec.episodes[*index].end_us)
    }
}
