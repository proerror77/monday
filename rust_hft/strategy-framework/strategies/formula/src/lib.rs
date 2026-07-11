use hft_core::{OrderType, Price, Quantity, Side, Symbol, TimeInForce};
use hft_factor_dsl::{FactorAst, FactorDslError, FactorOperator, FactorTerminal};
use ports::{
    AccountView, AggregatedBar, ExecutionEvent, MarketEvent, MarketSnapshot, OrderIntent, Strategy,
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
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FormulaStrategyError {
    #[error("max_order_notional must be positive and finite")]
    InvalidMaxOrderNotional,
    #[error("signal_threshold must be nonnegative and finite")]
    InvalidSignalThreshold,
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
        let domain = validate_live_ast(&config.ast)?;
        Ok(Self {
            config,
            domain,
            signal_state: SignalState::Neutral,
        })
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

impl Strategy for FormulaStrategy {
    fn on_market_event(&mut self, event: &MarketEvent, _account: &AccountView) -> Vec<OrderIntent> {
        let Some((signal, source_venue)) = self.evaluate_event(event) else {
            return Vec::new();
        };
        let next_state = if signal > self.config.signal_threshold {
            SignalState::Buy
        } else if signal < -self.config.signal_threshold {
            SignalState::Sell
        } else {
            SignalState::Neutral
        };
        if next_state == SignalState::Neutral {
            self.signal_state = SignalState::Neutral;
            return Vec::new();
        }
        if next_state == self.signal_state {
            return Vec::new();
        }
        let Some(target_venue) = source_venue else {
            return Vec::new();
        };
        let side = match next_state {
            SignalState::Buy => Side::Buy,
            SignalState::Sell => Side::Sell,
            SignalState::Neutral => unreachable!(),
        };
        let Some(limit_price) = executable_price(event, side) else {
            return Vec::new();
        };
        let Some(quantity) = self
            .config
            .max_order_notional
            .checked_div(limit_price)
            .filter(|quantity| *quantity > Decimal::ZERO)
        else {
            return Vec::new();
        };
        self.signal_state = next_state;
        vec![OrderIntent::crypto_spot(
            self.config.symbol.clone(),
            side,
            Quantity(quantity),
            OrderType::Limit,
            Some(Price(limit_price)),
            TimeInForce::IOC,
            self.config.name.clone(),
            Some(target_venue),
        )]
    }

    fn on_execution_event(
        &mut self,
        _event: &ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
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

fn validate_live_ast(ast: &FactorAst) -> Result<EventDomain, FormulaStrategyError> {
    ast.validate()?;
    let mut domain = None;
    validate_live_node(ast, &mut domain)?;
    domain.ok_or(FormulaStrategyError::MissingEventDomain)
}

fn validate_live_node(
    ast: &FactorAst,
    domain: &mut Option<EventDomain>,
) -> Result<(), FormulaStrategyError> {
    match ast {
        FactorAst::Terminal(FactorTerminal::Constant(value)) => {
            let parsed = value
                .parse::<f64>()
                .map_err(|_| FormulaStrategyError::InvalidConstant(value.clone()))?;
            if !parsed.is_finite() {
                return Err(FormulaStrategyError::InvalidConstant(value.clone()));
            }
        }
        FactorAst::Terminal(FactorTerminal::Field(field)) => {
            let field_domain = match field.as_str() {
                "best_bid" | "best_ask" | "mid_price" | "spread" | "spread_bps" | "bid_size"
                | "ask_size" | "book_imbalance" => EventDomain::Snapshot,
                "open" | "high" | "low" | "close" | "volume" | "trade_count" | "bar_return" => {
                    EventDomain::Bar
                }
                _ => return Err(FormulaStrategyError::UnsupportedField(field.clone())),
            };
            match *domain {
                Some(existing) if existing != field_domain => {
                    return Err(FormulaStrategyError::MixedEventDomains)
                }
                None => *domain = Some(field_domain),
                _ => {}
            }
        }
        FactorAst::Call { operator, args } => {
            if !matches!(
                operator,
                FactorOperator::Add
                    | FactorOperator::Sub
                    | FactorOperator::Mul
                    | FactorOperator::Div
                    | FactorOperator::Abs
                    | FactorOperator::Log
                    | FactorOperator::GreaterThan
                    | FactorOperator::LessThan
                    | FactorOperator::IfElse
            ) {
                return Err(FormulaStrategyError::UnsupportedOperator(
                    operator.symbol().to_string(),
                ));
            }
            for arg in args {
                validate_live_node(arg, domain)?;
            }
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{
        AssetClass, OrderType, Price, ProductType, Quantity, Side, TimeInForce, VenueId,
    };
    use ports::{AggregatedBar, BookLevel, MarketSnapshot};

    fn config(ast: FactorAst) -> FormulaStrategyConfig {
        FormulaStrategyConfig {
            name: "formula-test".to_string(),
            symbol: Symbol::from("BTCUSDT"),
            ast,
            max_order_notional: Decimal::from(100),
            signal_threshold: 0.1,
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
        })
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
    fn evaluates_supported_operators_and_emits_sized_spot_ioc() {
        let condition = call(
            FactorOperator::GreaterThan,
            vec![
                call(
                    FactorOperator::Log,
                    vec![call(
                        FactorOperator::Abs,
                        vec![call(
                            FactorOperator::Sub,
                            vec![field("best_ask"), field("best_bid")],
                        )],
                    )],
                ),
                constant("0"),
            ],
        );
        let truthy = call(
            FactorOperator::Div,
            vec![
                call(
                    FactorOperator::Mul,
                    vec![
                        call(
                            FactorOperator::Add,
                            vec![field("bid_size"), field("ask_size")],
                        ),
                        constant("2"),
                    ],
                ),
                call(FactorOperator::Sub, vec![constant("5"), constant("1")]),
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
    fn invalid_arithmetic_and_empty_books_fail_closed() {
        let invalid_formulas = [
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
            call(
                FactorOperator::Log,
                vec![call(
                    FactorOperator::Sub,
                    vec![field("best_bid"), field("best_ask")],
                )],
            ),
            call(
                FactorOperator::Mul,
                vec![field("best_bid"), constant("1e308")],
            ),
        ];
        for ast in invalid_formulas {
            let mut strategy = FormulaStrategy::new(config(ast)).unwrap();
            assert!(intents(&mut strategy, &snapshot(3, 1)).is_empty());
        }

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
}
