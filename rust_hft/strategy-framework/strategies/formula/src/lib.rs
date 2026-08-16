use hft_core::{OrderType, Price, Quantity, Side, Symbol, TimeInForce};
use hft_factor_dsl::{
    validate_live_formula, FactorAst, FactorDslError, FactorOperator, FactorTerminal,
    LiveEventDomain, LiveFormulaCapabilityError,
};
use ports::{
    AccountView, AggregatedBar, ExecutionEvent, L2BookView, MarketEvent, MarketSnapshot,
    OrderIntent, Strategy, StrategyContext,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct FormulaStrategyConfig {
    pub name: String,
    pub symbol: Symbol,
    pub ast: FactorAst,
    pub max_order_notional: Decimal,
    pub signal_threshold: f64,
    pub target_position: bool,
    pub evaluation_interval_millis: Option<u64>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FormulaStrategyError {
    #[error("max_order_notional must be positive and finite")]
    InvalidMaxOrderNotional,
    #[error("signal_threshold must be nonnegative and finite")]
    InvalidSignalThreshold,
    #[error("target-position formulas require a positive evaluation interval")]
    InvalidEvaluationInterval,
    #[error("invalid factor AST: {0}")]
    InvalidAst(#[from] FactorDslError),
    #[error("unsupported live operator: {0}")]
    UnsupportedOperator(String),
    #[error("unsupported live field: {0}")]
    UnsupportedField(String),
    #[error("formula mixes snapshot and bar fields")]
    MixedEventDomains,
    #[error("formula must reference a snapshot or bar field")]
    MissingEventDomain,
    #[error("formula constant is not a finite f64: {0}")]
    InvalidConstant(String),
}

#[derive(Debug)]
pub struct FormulaStrategy {
    config: FormulaStrategyConfig,
    domain: EventDomain,
    signal_state: SignalState,
    signal_initialized: bool,
    evaluation_interval_micros: Option<u64>,
    bucket_sample: Option<BucketSample>,
    pending_target: Option<PendingTarget>,
}

impl FormulaStrategy {
    pub fn new(config: FormulaStrategyConfig) -> Result<Self, FormulaStrategyError> {
        if config.max_order_notional <= Decimal::ZERO
            || !config
                .max_order_notional
                .to_f64()
                .is_some_and(f64::is_finite)
        {
            return Err(FormulaStrategyError::InvalidMaxOrderNotional);
        }
        if !config.signal_threshold.is_finite() || config.signal_threshold < 0.0 {
            return Err(FormulaStrategyError::InvalidSignalThreshold);
        }
        let evaluation_interval_micros = match config.evaluation_interval_millis {
            Some(interval) => Some(
                interval
                    .checked_mul(1_000)
                    .filter(|value| *value > 0)
                    .ok_or(FormulaStrategyError::InvalidEvaluationInterval)?,
            ),
            None => None,
        };
        if config.target_position && evaluation_interval_micros.is_none() {
            return Err(FormulaStrategyError::InvalidEvaluationInterval);
        }
        let domain = validate_live_ast(&config.ast)?;
        Ok(Self {
            config,
            domain,
            signal_state: SignalState::Neutral,
            signal_initialized: false,
            evaluation_interval_micros,
            bucket_sample: None,
            pending_target: None,
        })
    }

    fn bucketed_decision(
        &mut self,
        timestamp: u64,
        signal: f64,
        venue: hft_core::VenueId,
        best_bid: Decimal,
        best_ask: Decimal,
    ) -> Option<BucketSample> {
        let interval = self.evaluation_interval_micros?;
        let current = BucketSample {
            bucket: timestamp / interval,
            signal,
            venue,
            best_bid,
            best_ask,
        };
        let Some(previous) = self.bucket_sample else {
            self.bucket_sample = Some(current);
            return timestamp.is_multiple_of(interval).then_some(current);
        };
        if current.bucket < previous.bucket {
            return None;
        }
        self.bucket_sample = Some(current);
        if current.bucket == previous.bucket {
            return None;
        }
        let mut decision = if timestamp.is_multiple_of(interval) {
            current
        } else {
            previous
        };
        decision.bucket = current.bucket;
        Some(decision)
    }

    fn evaluate_event(&self, event: &MarketEvent) -> Option<(f64, Option<hft_core::VenueId>)> {
        match (self.domain, event) {
            (EventDomain::Snapshot, MarketEvent::Snapshot(snapshot))
                if snapshot.symbol == self.config.symbol =>
            {
                let best_bid = snapshot.bids.first()?.price.0;
                let best_ask = snapshot.asks.first()?.price.0;
                if best_bid <= Decimal::ZERO
                    || best_ask <= best_bid
                    || snapshot.bids.first()?.quantity.0 <= Decimal::ZERO
                    || snapshot.asks.first()?.quantity.0 <= Decimal::ZERO
                {
                    return None;
                }
                let signal =
                    evaluate_ast(&self.config.ast, &|field| snapshot_value(snapshot, field))?;
                Some((signal, snapshot.source_venue))
            }
            (EventDomain::Bar, MarketEvent::Bar(bar)) if bar.symbol == self.config.symbol => {
                let reference_price = bar.close.0;
                if reference_price <= Decimal::ZERO {
                    return None;
                }
                let signal = evaluate_ast(&self.config.ast, &|field| bar_value(bar, field))?;
                Some((signal, bar.source_venue))
            }
            _ => None,
        }
    }

    fn emit_signal(
        &mut self,
        signal: f64,
        target_venue: hft_core::VenueId,
        account: &AccountView,
        executable_price: impl Fn(Side) -> Option<Decimal>,
        decision_bucket: Option<u64>,
    ) -> Vec<OrderIntent> {
        let next_state = if signal > self.config.signal_threshold {
            SignalState::Buy
        } else if signal < -self.config.signal_threshold {
            SignalState::Sell
        } else {
            SignalState::Neutral
        };
        if !self.config.target_position && next_state == SignalState::Neutral {
            self.signal_state = SignalState::Neutral;
            self.signal_initialized = true;
            return Vec::new();
        }
        if !self.config.target_position
            && self.signal_initialized
            && next_state == self.signal_state
        {
            return Vec::new();
        }
        let mut target_position_before = None;
        let (side, raw_quantity) = if self.config.target_position {
            let Some(decision_bucket) = decision_bucket else {
                return Vec::new();
            };
            let current = account
                .positions
                .get(&self.config.symbol)
                .map(|position| position.quantity.0)
                .unwrap_or(Decimal::ZERO);
            if let Some(pending) = self.pending_target {
                // ponytail: signed Paper/Shadow fills in 50 ms; wait one full research bucket
                // before retrying an unchanged position. Track order IDs before external execution.
                if next_state == self.signal_state
                    && current == pending.position_before
                    && decision_bucket <= pending.emitted_bucket.saturating_add(1)
                {
                    return Vec::new();
                }
                self.pending_target = None;
            }
            target_position_before = Some(current);
            let target = match next_state {
                SignalState::Neutral => Decimal::ZERO,
                SignalState::Buy | SignalState::Sell => {
                    let target_side = match next_state {
                        SignalState::Buy => Side::Buy,
                        SignalState::Sell => Side::Sell,
                        SignalState::Neutral => unreachable!(),
                    };
                    let Some(price) =
                        executable_price(target_side).filter(|price| *price > Decimal::ZERO)
                    else {
                        return Vec::new();
                    };
                    let Some(quantity) =
                        quantity_within_notional(self.config.max_order_notional, price)
                    else {
                        return Vec::new();
                    };
                    if next_state == SignalState::Buy {
                        quantity
                    } else {
                        -quantity
                    }
                }
            };
            let delta = target - current;
            if delta == Decimal::ZERO {
                self.signal_state = next_state;
                self.signal_initialized = true;
                self.pending_target = None;
                return Vec::new();
            }
            if delta > Decimal::ZERO {
                (Side::Buy, delta)
            } else {
                (Side::Sell, -delta)
            }
        } else {
            let side = match next_state {
                SignalState::Buy => Side::Buy,
                SignalState::Sell => Side::Sell,
                SignalState::Neutral => unreachable!(),
            };
            let Some(price) = executable_price(side).filter(|price| *price > Decimal::ZERO) else {
                return Vec::new();
            };
            let Some(quantity) = self
                .config
                .max_order_notional
                .checked_div(price)
                .filter(|quantity| *quantity > Decimal::ZERO)
            else {
                return Vec::new();
            };
            (side, quantity)
        };
        let Some(limit_price) = executable_price(side).filter(|price| *price > Decimal::ZERO)
        else {
            return Vec::new();
        };
        let raw_quantity = if self.config.target_position {
            let Some(max_quantity) =
                quantity_within_notional(self.config.max_order_notional, limit_price)
            else {
                return Vec::new();
            };
            raw_quantity.min(max_quantity)
        } else {
            raw_quantity
        };
        // Polymarket CLOB share sizes are signed with two decimal places. Round down before the
        // intent enters risk review so the adapter does not reject ordinary non-divisible prices
        // and the signed notional ceiling can never be increased by normalization.
        let quantity = if target_venue == hft_core::VenueId::POLYMARKET {
            raw_quantity.trunc_with_scale(2)
        } else {
            raw_quantity
        };
        if quantity <= Decimal::ZERO {
            return Vec::new();
        }

        self.signal_state = next_state;
        self.signal_initialized = true;
        if let (Some(position_before), Some(emitted_bucket)) =
            (target_position_before, decision_bucket)
        {
            self.pending_target = Some(PendingTarget {
                position_before,
                emitted_bucket,
            });
        }
        let mut intent = if target_venue == hft_core::VenueId::POLYMARKET {
            OrderIntent::prediction_market(
                self.config.symbol.clone(),
                side,
                Quantity(quantity),
                OrderType::Limit,
                Some(Price(limit_price)),
                TimeInForce::IOC,
                self.config.name.clone(),
                target_venue,
            )
        } else {
            OrderIntent::crypto_spot(
                self.config.symbol.clone(),
                side,
                Quantity(quantity),
                OrderType::Limit,
                Some(Price(limit_price)),
                TimeInForce::IOC,
                self.config.name.clone(),
                Some(target_venue),
            )
        };
        if self.config.target_position {
            intent.product_type = hft_core::ProductType::Perp;
        }
        vec![intent]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventDomain {
    Snapshot,
    Bar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalState {
    Neutral,
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy)]
struct BucketSample {
    bucket: u64,
    signal: f64,
    venue: hft_core::VenueId,
    best_bid: Decimal,
    best_ask: Decimal,
}

#[derive(Debug, Clone, Copy)]
struct PendingTarget {
    position_before: Decimal,
    emitted_bucket: u64,
}

impl Strategy for FormulaStrategy {
    fn on_market_event(&mut self, event: &MarketEvent, account: &AccountView) -> Vec<OrderIntent> {
        let Some((signal, source_venue)) = self.evaluate_event(event) else {
            return Vec::new();
        };
        let Some(target_venue) = source_venue else {
            return Vec::new();
        };
        if self.config.target_position {
            let (Some(best_bid), Some(best_ask)) = (
                executable_price(event, Side::Sell),
                executable_price(event, Side::Buy),
            ) else {
                return Vec::new();
            };
            let timestamp = event
                .timestamps()
                .and_then(|timestamps| timestamps.local_receive)
                .map(|timestamp| timestamp.as_micros())
                .unwrap_or_else(|| event_timestamp(event));
            let Some(decision) =
                self.bucketed_decision(timestamp, signal, target_venue, best_bid, best_ask)
            else {
                return Vec::new();
            };
            return self.emit_signal(
                decision.signal,
                decision.venue,
                account,
                |side| match side {
                    Side::Buy => Some(decision.best_ask),
                    Side::Sell => Some(decision.best_bid),
                },
                Some(decision.bucket),
            );
        }
        self.emit_signal(
            signal,
            target_venue,
            account,
            |side| executable_price(event, side),
            None,
        )
    }

    fn on_market_event_with_context(
        &mut self,
        event: &MarketEvent,
        context: &StrategyContext<'_>,
    ) -> Vec<OrderIntent> {
        if self.domain != EventDomain::Snapshot {
            return self.on_market_event(event, context.account);
        }
        let event_symbol = match event {
            MarketEvent::Snapshot(snapshot) => &snapshot.symbol,
            MarketEvent::Update(update) => &update.symbol,
            MarketEvent::Quote(quote) => &quote.symbol,
            _ => return Vec::new(),
        };
        if event_symbol != &self.config.symbol {
            return Vec::new();
        }
        let Some(book) = context
            .book
            .filter(|book| book.symbol == &self.config.symbol)
        else {
            return Vec::new();
        };
        let Some(signal) = evaluate_book_formula(&self.config.ast, book) else {
            return Vec::new();
        };
        let (Some(best_bid), Some(best_ask)) = (book.bid_prices.first(), book.ask_prices.first())
        else {
            return Vec::new();
        };
        let best_bid = Price::from(*best_bid).0;
        let best_ask = Price::from(*best_ask).0;
        if self.config.target_position {
            let timestamp = event
                .timestamps()
                .and_then(|timestamps| timestamps.local_receive)
                .map(|timestamp| timestamp.as_micros())
                .unwrap_or(book.timestamp);
            let Some(decision) =
                self.bucketed_decision(timestamp, signal, book.venue, best_bid, best_ask)
            else {
                return Vec::new();
            };
            return self.emit_signal(
                decision.signal,
                decision.venue,
                context.account,
                |side| match side {
                    Side::Buy => Some(decision.best_ask),
                    Side::Sell => Some(decision.best_bid),
                },
                Some(decision.bucket),
            );
        }
        self.emit_signal(
            signal,
            book.venue,
            context.account,
            |side| match side {
                Side::Buy => Some(best_ask),
                Side::Sell => Some(best_bid),
            },
            None,
        )
    }

    fn on_execution_event(
        &mut self,
        _event: &ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn supported_asset_classes(&self) -> &'static [hft_core::AssetClass] {
        &[
            hft_core::AssetClass::Crypto,
            hft_core::AssetClass::PredictionMarket,
        ]
    }

    fn name(&self) -> &str {
        &self.config.name
    }
}

fn executable_price(event: &MarketEvent, side: Side) -> Option<Decimal> {
    let price = match (event, side) {
        (MarketEvent::Snapshot(snapshot), Side::Buy) => snapshot.asks.first()?.price.0,
        (MarketEvent::Snapshot(snapshot), Side::Sell) => snapshot.bids.first()?.price.0,
        (MarketEvent::Bar(bar), _) => bar.close.0,
        _ => return None,
    };
    (price > Decimal::ZERO).then_some(price)
}

fn event_timestamp(event: &MarketEvent) -> u64 {
    match event {
        MarketEvent::Snapshot(snapshot) => snapshot.timestamp,
        MarketEvent::Bar(bar) => bar.close_time,
        _ => 0,
    }
}

fn validate_live_ast(ast: &FactorAst) -> Result<EventDomain, FormulaStrategyError> {
    let capability = validate_live_formula(ast).map_err(|error| match error {
        LiveFormulaCapabilityError::InvalidAst(error) => FormulaStrategyError::InvalidAst(error),
        LiveFormulaCapabilityError::UnsupportedOperator(operator) => {
            FormulaStrategyError::UnsupportedOperator(operator)
        }
        LiveFormulaCapabilityError::UnsupportedField(field) => {
            FormulaStrategyError::UnsupportedField(field)
        }
        LiveFormulaCapabilityError::MixedEventDomains => FormulaStrategyError::MixedEventDomains,
        LiveFormulaCapabilityError::MissingEventDomain => FormulaStrategyError::MissingEventDomain,
        LiveFormulaCapabilityError::InvalidConstant(value) => {
            FormulaStrategyError::InvalidConstant(value)
        }
    })?;
    Ok(match capability.event_domain {
        LiveEventDomain::Snapshot => EventDomain::Snapshot,
        LiveEventDomain::Bar => EventDomain::Bar,
    })
}

fn evaluate_ast(ast: &FactorAst, field_value: &impl Fn(&str) -> Option<f64>) -> Option<f64> {
    let value = match ast {
        FactorAst::Terminal(FactorTerminal::Field(field)) => field_value(field)?,
        FactorAst::Terminal(FactorTerminal::Constant(value)) => value.parse::<f64>().ok()?,
        FactorAst::Call { operator, args } => match operator {
            FactorOperator::Abs if args.len() == 1 => evaluate_ast(&args[0], field_value)?.abs(),
            FactorOperator::Log if args.len() == 1 => {
                let value = evaluate_ast(&args[0], field_value)?;
                if value <= 0.0 {
                    return None;
                }
                value.ln()
            }
            FactorOperator::IfElse if args.len() == 3 => {
                let condition = evaluate_ast(&args[0], field_value)?;
                let truthy = evaluate_ast(&args[1], field_value)?;
                let falsy = evaluate_ast(&args[2], field_value)?;
                if condition > 0.0 {
                    truthy
                } else {
                    falsy
                }
            }
            FactorOperator::Add
            | FactorOperator::Sub
            | FactorOperator::Mul
            | FactorOperator::Div
            | FactorOperator::GreaterThan
            | FactorOperator::LessThan
                if args.len() == 2 =>
            {
                let left = evaluate_ast(&args[0], field_value)?;
                let right = evaluate_ast(&args[1], field_value)?;
                match operator {
                    FactorOperator::Add => left + right,
                    FactorOperator::Sub => left - right,
                    FactorOperator::Mul => left * right,
                    FactorOperator::Div => {
                        if right == 0.0 {
                            return None;
                        }
                        left / right
                    }
                    FactorOperator::GreaterThan => f64::from(left > right),
                    FactorOperator::LessThan => f64::from(left < right),
                    _ => unreachable!(),
                }
            }
            _ => return None,
        },
    };
    value.is_finite().then_some(value)
}

fn snapshot_value(snapshot: &MarketSnapshot, field: &str) -> Option<f64> {
    let best_bid = snapshot.bids.first()?;
    let best_ask = snapshot.asks.first()?;
    let bid_price = decimal_to_f64(best_bid.price.0)?;
    let ask_price = decimal_to_f64(best_ask.price.0)?;
    let bid_size = decimal_to_f64(best_bid.quantity.0)?;
    let ask_size = decimal_to_f64(best_ask.quantity.0)?;
    let mid_price = (bid_price + ask_price) / 2.0;
    let spread = ask_price - bid_price;
    match field {
        "best_bid" => Some(bid_price),
        "best_ask" => Some(ask_price),
        "mid_price" => finite(mid_price),
        "spread" => finite(spread),
        "spread_bps" if mid_price != 0.0 => finite(spread / mid_price * 10_000.0),
        "bid_size" => Some(bid_size),
        "ask_size" => Some(ask_size),
        "book_imbalance" if bid_size + ask_size != 0.0 => {
            finite((bid_size - ask_size) / (bid_size + ask_size))
        }
        _ => None,
    }
}

fn evaluate_book_formula(ast: &FactorAst, book: L2BookView<'_>) -> Option<f64> {
    let best_bid = book.bid_prices.first()?.to_f64();
    let best_ask = book.ask_prices.first()?.to_f64();
    let bid_size = book.bid_quantities.first()?.to_f64();
    let ask_size = book.ask_quantities.first()?.to_f64();
    if best_bid <= 0.0
        || best_ask <= best_bid
        || bid_size <= 0.0
        || ask_size <= 0.0
        || ![best_bid, best_ask, bid_size, ask_size]
            .into_iter()
            .all(f64::is_finite)
    {
        return None;
    }
    let mid_price = (best_bid + best_ask) * 0.5;
    let spread = best_ask - best_bid;
    evaluate_ast(ast, &|field| match field {
        "best_bid" => Some(best_bid),
        "best_ask" => Some(best_ask),
        "mid_price" => finite(mid_price),
        "spread" => finite(spread),
        "spread_bps" if mid_price != 0.0 => finite(spread / mid_price * 10_000.0),
        "bid_size" => Some(bid_size),
        "ask_size" => Some(ask_size),
        "book_imbalance" if bid_size + ask_size != 0.0 => {
            finite((bid_size - ask_size) / (bid_size + ask_size))
        }
        _ => None,
    })
}

fn bar_value(bar: &AggregatedBar, field: &str) -> Option<f64> {
    match field {
        "open" => decimal_to_f64(bar.open.0),
        "high" => decimal_to_f64(bar.high.0),
        "low" => decimal_to_f64(bar.low.0),
        "close" => decimal_to_f64(bar.close.0),
        "volume" => decimal_to_f64(bar.volume.0),
        "trade_count" => Some(f64::from(bar.trade_count)),
        "bar_return" => {
            let open = decimal_to_f64(bar.open.0)?;
            if open == 0.0 {
                return None;
            }
            finite((decimal_to_f64(bar.close.0)? - open) / open)
        }
        _ => None,
    }
}

fn decimal_to_f64(value: Decimal) -> Option<f64> {
    finite(value.to_f64()?)
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn quantity_within_notional(notional: Decimal, price: Decimal) -> Option<Decimal> {
    let quantity = notional.checked_div(price)?;
    if quantity.checked_mul(price)? <= notional {
        return (quantity > Decimal::ZERO).then_some(quantity);
    }
    quantity
        .checked_sub(Decimal::new(1, quantity.scale()))
        .filter(|quantity| *quantity > Decimal::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{
        AssetClass, FixedPrice, FixedQuantity, OrderType, Price, ProductType, Quantity, Side,
        TimeInForce, VenueId,
    };
    use ports::{AggregatedBar, BookLevel, L2BookView, MarketSnapshot, StrategyContext};

    fn config(ast: FactorAst) -> FormulaStrategyConfig {
        FormulaStrategyConfig {
            name: "formula-test".to_string(),
            symbol: Symbol::from("BTCUSDT"),
            ast,
            max_order_notional: Decimal::from(100),
            signal_threshold: 0.1,
            target_position: false,
            evaluation_interval_millis: None,
        }
    }

    fn field(name: &str) -> FactorAst {
        FactorAst::Terminal(FactorTerminal::Field(name.to_string()))
    }

    fn constant(value: &str) -> FactorAst {
        FactorAst::Terminal(FactorTerminal::Constant(value.to_string()))
    }

    fn call(operator: FactorOperator, args: Vec<FactorAst>) -> FactorAst {
        FactorAst::call(operator, args).unwrap()
    }

    fn snapshot(bid_size: i64, ask_size: i64) -> MarketEvent {
        MarketEvent::Snapshot(MarketSnapshot {
            symbol: Symbol::from("BTCUSDT"),
            timestamp: 1,
            bids: vec![BookLevel {
                price: Price(Decimal::from(99)),
                quantity: Quantity(Decimal::from(bid_size)),
            }],
            asks: vec![BookLevel {
                price: Price(Decimal::from(101)),
                quantity: Quantity(Decimal::from(ask_size)),
            }],
            sequence: 1,
            source_venue: Some(VenueId::BITGET),
            timestamps: Default::default(),
        })
    }

    fn snapshot_at(bid_size: i64, ask_size: i64, timestamp: u64) -> MarketEvent {
        let mut event = snapshot(bid_size, ask_size);
        let MarketEvent::Snapshot(snapshot) = &mut event else {
            unreachable!()
        };
        snapshot.timestamp = timestamp;
        event
    }

    fn bar(open: i64, close: i64) -> MarketEvent {
        MarketEvent::Bar(AggregatedBar {
            symbol: Symbol::from("BTCUSDT"),
            interval_ms: 60_000,
            open_time: 1,
            close_time: 2,
            open: Price(Decimal::from(open)),
            high: Price(Decimal::from(103)),
            low: Price(Decimal::from(97)),
            close: Price(Decimal::from(close)),
            volume: Quantity(Decimal::from(5)),
            trade_count: 10,
            source_venue: Some(VenueId::BINANCE_SPOT),
            timestamps: Default::default(),
        })
    }

    fn intents(strategy: &mut FormulaStrategy, event: &MarketEvent) -> Vec<OrderIntent> {
        strategy.on_market_event(event, &AccountView::default())
    }

    #[test]
    fn rejects_nonpositive_order_notional() {
        for value in [Decimal::ZERO, Decimal::NEGATIVE_ONE] {
            let mut config = config(field("best_bid"));
            config.max_order_notional = value;

            assert_eq!(
                FormulaStrategy::new(config).unwrap_err(),
                FormulaStrategyError::InvalidMaxOrderNotional
            );
        }
    }

    #[test]
    fn rejects_invalid_signal_threshold() {
        for value in [-0.1, f64::NAN, f64::INFINITY] {
            let mut config = config(field("best_bid"));
            config.signal_threshold = value;

            assert_eq!(
                FormulaStrategy::new(config).unwrap_err(),
                FormulaStrategyError::InvalidSignalThreshold
            );
        }
    }

    #[test]
    fn target_position_requires_a_positive_bucket_interval() {
        let mut target = config(field("book_imbalance"));
        target.target_position = true;

        assert_eq!(
            FormulaStrategy::new(target).unwrap_err(),
            FormulaStrategyError::InvalidEvaluationInterval
        );
    }

    #[test]
    fn rejects_wrong_arity() {
        let ast = FactorAst::Call {
            operator: FactorOperator::Add,
            args: vec![field("best_bid")],
        };

        assert!(matches!(
            FormulaStrategy::new(config(ast)),
            Err(FormulaStrategyError::InvalidAst(
                FactorDslError::ArityMismatch {
                    expected: 2,
                    actual: 1,
                    ..
                }
            ))
        ));
    }

    #[test]
    fn rejects_stateful_operators() {
        for operator in [
            FactorOperator::Rank,
            FactorOperator::ZScore,
            FactorOperator::Delta,
            FactorOperator::Mean,
            FactorOperator::Std,
        ] {
            let name = operator.symbol().to_string();
            let args = (0..operator.arity()).map(|_| field("best_bid")).collect();
            let ast = FactorAst::Call { operator, args };

            assert_eq!(
                FormulaStrategy::new(config(ast)).unwrap_err(),
                FormulaStrategyError::UnsupportedOperator(name)
            );
        }
    }

    #[test]
    fn rejects_unknown_and_mixed_fields() {
        assert_eq!(
            FormulaStrategy::new(config(field("last_price"))).unwrap_err(),
            FormulaStrategyError::UnsupportedField("last_price".to_string())
        );

        let mixed = FactorAst::Call {
            operator: FactorOperator::Add,
            args: vec![field("best_bid"), field("close")],
        };
        assert_eq!(
            FormulaStrategy::new(config(mixed)).unwrap_err(),
            FormulaStrategyError::MixedEventDomains
        );
    }

    #[test]
    fn rejects_bad_constants_and_fieldless_formulas() {
        for value in ["not-a-number", "NaN", "inf"] {
            let ast = FactorAst::Call {
                operator: FactorOperator::Add,
                args: vec![field("best_bid"), constant(value)],
            };
            assert_eq!(
                FormulaStrategy::new(config(ast)).unwrap_err(),
                FormulaStrategyError::InvalidConstant(value.to_string())
            );
        }

        assert_eq!(
            FormulaStrategy::new(config(constant("1"))).unwrap_err(),
            FormulaStrategyError::MissingEventDomain
        );
    }

    #[test]
    fn evaluates_every_supported_field_on_its_event_domain() {
        for name in [
            "best_bid",
            "best_ask",
            "mid_price",
            "spread",
            "spread_bps",
            "bid_size",
            "ask_size",
            "book_imbalance",
        ] {
            let mut config = config(field(name));
            config.signal_threshold = 0.0;
            let mut strategy = FormulaStrategy::new(config).unwrap();
            assert_eq!(intents(&mut strategy, &snapshot(3, 1)).len(), 1, "{name}");
        }

        for name in [
            "open",
            "high",
            "low",
            "close",
            "volume",
            "trade_count",
            "bar_return",
        ] {
            let mut config = config(field(name));
            config.signal_threshold = 0.0;
            let mut strategy = FormulaStrategy::new(config).unwrap();
            assert_eq!(intents(&mut strategy, &bar(100, 102)).len(), 1, "{name}");
        }
    }

    #[test]
    fn evaluates_live_supported_operators_and_emits_sized_spot_ioc() {
        let condition = call(
            FactorOperator::GreaterThan,
            vec![
                call(
                    FactorOperator::Abs,
                    vec![call(
                        FactorOperator::Sub,
                        vec![field("best_ask"), field("best_bid")],
                    )],
                ),
                constant("0"),
            ],
        );
        let truthy = call(
            FactorOperator::Mul,
            vec![
                call(
                    FactorOperator::Add,
                    vec![field("bid_size"), field("ask_size")],
                ),
                constant("0.5"),
            ],
        );
        let ast = call(
            FactorOperator::IfElse,
            vec![
                condition,
                truthy,
                call(
                    FactorOperator::LessThan,
                    vec![field("best_bid"), field("best_ask")],
                ),
            ],
        );
        let mut config = config(ast);
        config.signal_threshold = 1.0;
        let mut strategy = FormulaStrategy::new(config).unwrap();

        let orders = intents(&mut strategy, &snapshot(3, 1));
        assert_eq!(orders.len(), 1);
        let order = &orders[0];
        assert_eq!(order.symbol, Symbol::from("BTCUSDT"));
        assert_eq!(order.asset_class, AssetClass::Crypto);
        assert_eq!(order.product_type, ProductType::Spot);
        assert_eq!(order.side, Side::Buy);
        assert_eq!(
            order.quantity,
            Quantity(Decimal::from(100).checked_div(Decimal::from(101)).unwrap())
        );
        assert_eq!(order.order_type, OrderType::Limit);
        assert_eq!(order.price, Some(Price(Decimal::from(101))));
        assert_eq!(order.time_in_force, TimeInForce::IOC);
        assert_eq!(order.strategy_id, "formula-test");
        assert_eq!(order.target_venue, Some(VenueId::BITGET));
        assert_eq!(strategy.name(), "formula-test");
        assert_eq!(strategy.id(), "formula-test");
    }

    #[test]
    fn polymarket_book_emits_prediction_market_intent() {
        let mut config = config(field("book_imbalance"));
        config.symbol = Symbol::from("123456789");
        config.signal_threshold = 0.0;
        let mut strategy = FormulaStrategy::new(config).unwrap();
        let mut event = snapshot(3, 1);
        let MarketEvent::Snapshot(snapshot) = &mut event else {
            unreachable!();
        };
        snapshot.symbol = Symbol::from("123456789");
        snapshot.bids[0].price = Price(Decimal::new(60, 2));
        snapshot.asks[0].price = Price(Decimal::new(61, 2));
        snapshot.source_venue = Some(VenueId::POLYMARKET);

        let orders = intents(&mut strategy, &event);

        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].asset_class, AssetClass::PredictionMarket);
        assert_eq!(orders[0].product_type, ProductType::PredictionMarket);
        assert_eq!(orders[0].target_venue, Some(VenueId::POLYMARKET));
        assert_eq!(orders[0].price, Some(Price(Decimal::new(61, 2))));
        assert_eq!(orders[0].quantity, Quantity(Decimal::new(16_393, 2)));
        assert_eq!(orders[0].quantity.0.scale(), 2);
        assert!(orders[0].quantity.0 * orders[0].price.unwrap().0 <= Decimal::from(100));
        assert!(strategy
            .supported_asset_classes()
            .contains(&AssetClass::PredictionMarket));
    }

    #[test]
    fn ignores_nonmatching_events_and_sizes_bars_at_close() {
        let mut config = config(field("bar_return"));
        config.signal_threshold = 0.01;
        let mut strategy = FormulaStrategy::new(config).unwrap();
        assert!(intents(&mut strategy, &snapshot(3, 1)).is_empty());

        let orders = intents(&mut strategy, &bar(100, 102));
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].side, Side::Buy);
        assert_eq!(
            orders[0].quantity,
            Quantity(Decimal::from(100).checked_div(Decimal::from(102)).unwrap())
        );
        assert_eq!(orders[0].order_type, OrderType::Limit);
        assert_eq!(orders[0].price, Some(Price(Decimal::from(102))));
        assert_eq!(orders[0].target_venue, Some(VenueId::BINANCE_SPOT));
    }

    #[test]
    fn requires_matching_symbol_and_source_venue() {
        let mut config = config(field("best_bid"));
        config.signal_threshold = 0.0;
        let mut strategy = FormulaStrategy::new(config).unwrap();
        let MarketEvent::Snapshot(mut event) = snapshot(3, 1) else {
            unreachable!()
        };

        event.symbol = Symbol::from("ETHUSDT");
        assert!(intents(&mut strategy, &MarketEvent::Snapshot(event.clone())).is_empty());
        event.symbol = Symbol::from("BTCUSDT");
        event.source_venue = None;
        assert!(intents(&mut strategy, &MarketEvent::Snapshot(event.clone())).is_empty());
        event.source_venue = Some(VenueId::BITGET);
        assert_eq!(
            intents(&mut strategy, &MarketEvent::Snapshot(event)).len(),
            1
        );
    }

    #[test]
    fn suppresses_same_side_until_a_neutral_signal() {
        let mut config = config(field("book_imbalance"));
        config.signal_threshold = 0.2;
        let mut strategy = FormulaStrategy::new(config).unwrap();
        let buy = snapshot(3, 1);

        assert_eq!(intents(&mut strategy, &buy).len(), 1);
        assert!(intents(&mut strategy, &buy).is_empty());
        assert!(intents(&mut strategy, &snapshot(1, 1)).is_empty());
        assert_eq!(intents(&mut strategy, &buy).len(), 1);
        assert_eq!(intents(&mut strategy, &snapshot(1, 3))[0].side, Side::Sell);
        assert!(intents(&mut strategy, &snapshot(1, 3)).is_empty());
    }

    #[test]
    fn target_position_mode_reverses_and_closes_with_position_deltas() {
        let long_quantity =
            quantity_within_notional(Decimal::from(50), Decimal::from(101)).unwrap();
        let mut account = AccountView::default();
        account.positions.insert(
            Symbol::from("BTCUSDT"),
            ports::Position {
                symbol: Symbol::from("BTCUSDT"),
                quantity: Quantity(long_quantity),
                avg_price: Price(Decimal::from(101)),
                unrealized_pnl: Decimal::ZERO,
                realized_pnl: Decimal::ZERO,
            },
        );
        let mut target = config(field("book_imbalance"));
        target.max_order_notional = Decimal::from(50);
        target.signal_threshold = 0.2;
        target.target_position = true;
        target.evaluation_interval_millis = Some(1_000);
        let mut strategy = FormulaStrategy::new(target.clone()).unwrap();

        let reversal = strategy.on_market_event(&snapshot_at(1, 3, 1_000_000), &account);
        assert_eq!(reversal.len(), 1);
        assert_eq!(reversal[0].side, Side::Sell);
        assert_eq!(reversal[0].product_type, ProductType::Perp);
        assert_eq!(
            reversal[0].quantity.0,
            quantity_within_notional(Decimal::from(50), Decimal::from(99)).unwrap()
        );
        assert!(reversal[0].quantity.0 * reversal[0].price.unwrap().0 <= Decimal::from(50));

        let mut progressed = account.clone();
        progressed
            .positions
            .get_mut(&Symbol::from("BTCUSDT"))
            .unwrap()
            .quantity = Quantity(long_quantity - reversal[0].quantity.0);
        let remainder = strategy.on_market_event(&snapshot_at(1, 3, 2_000_000), &progressed);
        assert_eq!(remainder.len(), 1);
        assert_eq!(remainder[0].side, Side::Sell);
        assert_eq!(remainder[0].quantity.0, long_quantity);
        assert!(remainder[0].quantity.0 * remainder[0].price.unwrap().0 <= Decimal::from(50));

        let mut rejected = FormulaStrategy::new(target.clone()).unwrap();
        assert_eq!(
            rejected
                .on_market_event(&snapshot_at(1, 3, 1_000_000), &account)
                .len(),
            1
        );
        assert!(strategy
            .on_market_event(&snapshot_at(1, 3, 3_000_000), &progressed)
            .is_empty());
        assert!(rejected
            .on_market_event(&snapshot_at(1, 3, 2_000_000), &account)
            .is_empty());
        assert_eq!(
            rejected
                .on_market_event(&snapshot_at(1, 3, 3_000_000), &account)
                .len(),
            1
        );

        let mut flipped = FormulaStrategy::new(target.clone()).unwrap();
        assert_eq!(
            flipped
                .on_market_event(&snapshot_at(3, 1, 1_000_000), &AccountView::default())
                .len(),
            1
        );
        assert_eq!(
            flipped.on_market_event(&snapshot_at(1, 3, 2_000_000), &AccountView::default())[0].side,
            Side::Sell
        );

        let mut strategy = FormulaStrategy::new(target).unwrap();
        let close = strategy.on_market_event(&snapshot_at(1, 1, 1_000_000), &account);
        assert_eq!(close.len(), 1);
        assert_eq!(close[0].side, Side::Sell);
        assert_eq!(close[0].quantity.0, long_quantity);
    }

    #[test]
    fn target_position_uses_the_last_point_in_time_book_before_each_bucket() {
        let mut target = config(field("book_imbalance"));
        target.max_order_notional = Decimal::from(50);
        target.signal_threshold = f64::EPSILON;
        target.target_position = true;
        target.evaluation_interval_millis = Some(1_000);
        let mut strategy = FormulaStrategy::new(target).unwrap();
        let account = AccountView::default();

        assert!(strategy
            .on_market_event(&snapshot_at(3, 1, 100_000), &account)
            .is_empty());
        assert!(strategy
            .on_market_event(&snapshot_at(1, 3, 900_000), &account)
            .is_empty());
        let boundary = strategy.on_market_event(&snapshot_at(3, 1, 1_100_000), &account);
        assert_eq!(boundary.len(), 1);
        assert_eq!(boundary[0].side, Side::Sell);
    }

    #[test]
    fn target_position_retries_from_the_emission_bucket_after_empty_buckets() {
        let mut target = config(field("book_imbalance"));
        target.max_order_notional = Decimal::from(50);
        target.signal_threshold = f64::EPSILON;
        target.target_position = true;
        target.evaluation_interval_millis = Some(1_000);
        let mut strategy = FormulaStrategy::new(target).unwrap();
        let account = AccountView::default();

        assert!(strategy
            .on_market_event(&snapshot_at(3, 1, 100_000), &account)
            .is_empty());
        assert_eq!(
            strategy
                .on_market_event(&snapshot_at(3, 1, 3_100_000), &account)
                .len(),
            1
        );
        assert!(strategy
            .on_market_event(&snapshot_at(3, 1, 4_100_000), &account)
            .is_empty());
        assert_eq!(
            strategy
                .on_market_event(&snapshot_at(3, 1, 5_100_000), &account)
                .len(),
            1
        );
    }

    #[test]
    fn unsupported_or_invalid_arithmetic_and_empty_books_fail_closed() {
        for (ast, operator) in [
            (
                call(
                    FactorOperator::Div,
                    vec![
                        field("best_bid"),
                        call(
                            FactorOperator::Sub,
                            vec![field("ask_size"), field("ask_size")],
                        ),
                    ],
                ),
                "/",
            ),
            (
                call(
                    FactorOperator::Log,
                    vec![call(
                        FactorOperator::Sub,
                        vec![field("best_bid"), field("best_ask")],
                    )],
                ),
                "log",
            ),
        ] {
            assert_eq!(
                FormulaStrategy::new(config(ast)).unwrap_err(),
                FormulaStrategyError::UnsupportedOperator(operator.to_string())
            );
        }

        let mut strategy = FormulaStrategy::new(config(call(
            FactorOperator::Mul,
            vec![field("best_bid"), constant("1e308")],
        )))
        .unwrap();
        assert!(intents(&mut strategy, &snapshot(3, 1)).is_empty());

        let mut strategy = FormulaStrategy::new(config(field("best_bid"))).unwrap();
        let MarketEvent::Snapshot(mut empty) = snapshot(3, 1) else {
            unreachable!()
        };
        empty.bids.clear();
        assert!(intents(&mut strategy, &MarketEvent::Snapshot(empty.clone())).is_empty());
        empty.bids.push(BookLevel {
            price: Price(Decimal::from(99)),
            quantity: Quantity(Decimal::from(3)),
        });
        empty.asks.clear();
        assert!(intents(&mut strategy, &MarketEvent::Snapshot(empty)).is_empty());

        let MarketEvent::Snapshot(mut crossed) = snapshot(3, 1) else {
            unreachable!()
        };
        crossed.bids[0].price = Price(Decimal::from(102));
        assert!(intents(&mut strategy, &MarketEvent::Snapshot(crossed)).is_empty());
    }

    #[test]
    fn canonical_lob_delta_evaluates_snapshot_domain_formula() {
        let mut strategy =
            FormulaStrategy::new(config(field("book_imbalance"))).expect("valid strategy");
        let symbol = Symbol::from("BTCUSDT");
        let bid_prices = [FixedPrice::from_f64(99.0)];
        let bid_quantities = [FixedQuantity::from_f64(3.0)];
        let ask_prices = [FixedPrice::from_f64(101.0)];
        let ask_quantities = [FixedQuantity::from_f64(1.0)];
        let account = AccountView::default();
        let context = StrategyContext {
            account: &account,
            book: Some(L2BookView {
                symbol: &symbol,
                venue: VenueId::BITGET,
                timestamp: 1_000_000,
                sequence: 2,
                bid_prices: &bid_prices,
                bid_quantities: &bid_quantities,
                ask_prices: &ask_prices,
                ask_quantities: &ask_quantities,
            }),
        };
        let event = MarketEvent::Update(ports::BookUpdate {
            symbol: symbol.clone(),
            timestamp: 1_000_000,
            bids: vec![BookLevel {
                price: Price(Decimal::from(99)),
                quantity: Quantity(Decimal::from(3)),
            }],
            asks: Vec::new(),
            first_sequence: Some(2),
            sequence: 2,
            is_snapshot: false,
            source_venue: Some(VenueId::BITGET),
            timestamps: Default::default(),
        });

        let orders = strategy.on_market_event_with_context(&event, &context);

        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].side, Side::Buy);
        assert_eq!(orders[0].price, Some(Price(Decimal::from(101))));
        assert_eq!(orders[0].target_venue, Some(VenueId::BITGET));

        let mut target = config(field("book_imbalance"));
        target.target_position = true;
        target.evaluation_interval_millis = Some(1_000);
        let mut target = FormulaStrategy::new(target).unwrap();
        let orders = target.on_market_event_with_context(&event, &context);
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].product_type, ProductType::Perp);
    }
}
