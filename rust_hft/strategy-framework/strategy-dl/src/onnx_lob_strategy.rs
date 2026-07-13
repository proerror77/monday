use hft_core::{HftError, HftResult, OrderType, Price, Quantity, Side, Symbol, TimeInForce};
use hft_infer_onnx::{OnnxPredictor, MAX_ONNX_INPUT_ELEMENTS};
use ports::{
    AccountView, BookLevel, ExecutionEvent, L2BookView, MarketEvent, MarketSnapshot, OrderIntent,
    Strategy, StrategyContext,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnnxLobStrategyConfig {
    pub name: String,
    pub symbols: Vec<Symbol>,
    pub model_path: PathBuf,
    pub model_version: String,
    pub model_sha256: String,
    pub top_n: usize,
    pub window_size: usize,
    pub max_order_notional: Decimal,
    pub output_threshold: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnnxLobStrategyValidationError {
    EmptySymbols,
    EmptySymbol { index: usize },
    DuplicateSymbol(String),
    ZeroTopN,
    ZeroWindowSize,
    InputTooLarge,
    InvalidModelSha256,
    NonPositiveMaxOrderNotional,
    InvalidOutputThreshold,
}

impl fmt::Display for OnnxLobStrategyValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySymbols => write!(f, "symbols must not be empty"),
            Self::EmptySymbol { index } => write!(f, "symbol at index {index} must not be empty"),
            Self::DuplicateSymbol(symbol) => write!(f, "duplicate symbol: {symbol}"),
            Self::ZeroTopN => write!(f, "top_n must be greater than zero"),
            Self::ZeroWindowSize => write!(f, "window_size must be greater than zero"),
            Self::InputTooLarge => write!(
                f,
                "ONNX input exceeds the {MAX_ONNX_INPUT_ELEMENTS} element limit"
            ),
            Self::InvalidModelSha256 => {
                write!(f, "model_sha256 must be a lowercase SHA-256 value")
            }
            Self::NonPositiveMaxOrderNotional => {
                write!(f, "max_order_notional must be positive")
            }
            Self::InvalidOutputThreshold => {
                write!(f, "output_threshold must be finite and nonnegative")
            }
        }
    }
}

impl std::error::Error for OnnxLobStrategyValidationError {}

impl OnnxLobStrategyConfig {
    pub fn validate(&self) -> Result<(), OnnxLobStrategyValidationError> {
        if self.symbols.is_empty() {
            return Err(OnnxLobStrategyValidationError::EmptySymbols);
        }

        let mut symbols = HashSet::with_capacity(self.symbols.len());
        for (index, symbol) in self.symbols.iter().enumerate() {
            if symbol.as_str().trim().is_empty() {
                return Err(OnnxLobStrategyValidationError::EmptySymbol { index });
            }
            if !symbols.insert(symbol.as_str()) {
                return Err(OnnxLobStrategyValidationError::DuplicateSymbol(
                    symbol.as_str().to_string(),
                ));
            }
        }

        if self.top_n == 0 {
            return Err(OnnxLobStrategyValidationError::ZeroTopN);
        }
        if self.window_size == 0 {
            return Err(OnnxLobStrategyValidationError::ZeroWindowSize);
        }
        let input_elements = self
            .top_n
            .checked_mul(self.window_size)
            .and_then(|elements| elements.checked_mul(4))
            .ok_or(OnnxLobStrategyValidationError::InputTooLarge)?;
        if input_elements > MAX_ONNX_INPUT_ELEMENTS {
            return Err(OnnxLobStrategyValidationError::InputTooLarge);
        }
        if self.model_sha256.len() != 64
            || !self
                .model_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(OnnxLobStrategyValidationError::InvalidModelSha256);
        }
        if self.max_order_notional <= Decimal::ZERO {
            return Err(OnnxLobStrategyValidationError::NonPositiveMaxOrderNotional);
        }
        if !self.output_threshold.is_finite() || self.output_threshold < 0.0 {
            return Err(OnnxLobStrategyValidationError::InvalidOutputThreshold);
        }

        Ok(())
    }
}

#[derive(Debug)]
struct LobFrame {
    channels: [Vec<f32>; 4],
}

impl LobFrame {
    fn from_snapshot(snapshot: &MarketSnapshot, top_n: usize) -> Result<(Self, Decimal), String> {
        let best_bid = snapshot
            .bids
            .first()
            .ok_or_else(|| "empty bid book".to_string())?;
        let best_ask = snapshot
            .asks
            .first()
            .ok_or_else(|| "empty ask book".to_string())?;

        if best_bid.price.0 <= Decimal::ZERO || best_ask.price.0 <= Decimal::ZERO {
            return Err("best prices must be positive".to_string());
        }
        if best_ask.price.0 <= best_bid.price.0 {
            return Err("best ask must be greater than best bid".to_string());
        }

        let mid_decimal = best_bid
            .price
            .0
            .checked_add(best_ask.price.0)
            .and_then(|sum| sum.checked_div(Decimal::from(2)))
            .ok_or_else(|| "mid price cannot be represented".to_string())?;
        let mid = mid_decimal
            .to_f64()
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| "mid price is not a finite positive number".to_string())?;
        let bids = extract_levels(&snapshot.bids, top_n, true)?;
        let asks = extract_levels(&snapshot.asks, top_n, false)?;

        let channels: [Vec<f32>; 4] = [
            bids.iter()
                .map(|(price, _)| (*price - mid) as f32)
                .collect(),
            bids.iter().map(|(_, qty)| qty.ln_1p() as f32).collect(),
            asks.iter()
                .map(|(price, _)| (*price - mid) as f32)
                .collect(),
            asks.iter().map(|(_, qty)| qty.ln_1p() as f32).collect(),
        ];
        if channels.iter().flatten().any(|value| !value.is_finite()) {
            return Err("book features must be finite".to_string());
        }

        Ok((Self { channels }, mid_decimal))
    }

    fn from_book(book: L2BookView<'_>, top_n: usize) -> Result<(Self, Decimal), String> {
        let best_bid = book
            .bid_prices
            .first()
            .ok_or_else(|| "empty bid book".to_string())?;
        let best_ask = book
            .ask_prices
            .first()
            .ok_or_else(|| "empty ask book".to_string())?;
        if *best_bid <= hft_core::FixedPrice::ZERO || *best_ask <= *best_bid {
            return Err("best prices must be positive and uncrossed".to_string());
        }

        let bid_price = best_bid.to_f64();
        let ask_price = best_ask.to_f64();
        let mid = (bid_price + ask_price) * 0.5;
        let mid_decimal = Price::from(hft_core::FixedPrice::mid(*best_bid, *best_ask)).0;
        let bids = extract_fixed_levels(book.bid_prices, book.bid_quantities, top_n, true)?;
        let asks = extract_fixed_levels(book.ask_prices, book.ask_quantities, top_n, false)?;
        let channels: [Vec<f32>; 4] = [
            bids.iter()
                .map(|(price, _)| (*price - mid) as f32)
                .collect(),
            bids.iter().map(|(_, qty)| qty.ln_1p() as f32).collect(),
            asks.iter()
                .map(|(price, _)| (*price - mid) as f32)
                .collect(),
            asks.iter().map(|(_, qty)| qty.ln_1p() as f32).collect(),
        ];
        if channels.iter().flatten().any(|value| !value.is_finite()) {
            return Err("book features must be finite".to_string());
        }
        Ok((Self { channels }, mid_decimal))
    }
}

fn extract_levels(
    levels: &[BookLevel],
    top_n: usize,
    descending: bool,
) -> Result<Vec<(f64, f64)>, String> {
    let mut values = Vec::with_capacity(top_n);
    let mut previous_price = None;

    for level in levels.iter().take(top_n) {
        let price = level
            .price
            .to_f64()
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| "book prices must be finite and positive".to_string())?;
        let quantity = level
            .quantity
            .to_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or_else(|| "book quantities must be finite and nonnegative".to_string())?;

        if let Some(previous) = previous_price {
            let out_of_order = if descending {
                price > previous
            } else {
                price < previous
            };
            if out_of_order {
                return Err("book levels are not price sorted".to_string());
            }
        }
        previous_price = Some(price);
        values.push((price, quantity));
    }

    values.resize(top_n, (0.0, 0.0));
    Ok(values)
}

fn extract_fixed_levels(
    prices: &[hft_core::FixedPrice],
    quantities: &[hft_core::FixedQuantity],
    top_n: usize,
    descending: bool,
) -> Result<Vec<(f64, f64)>, String> {
    if prices.len() != quantities.len() {
        return Err("book price and quantity lengths differ".to_string());
    }
    let mut values = Vec::with_capacity(top_n);
    let mut previous_price = None;
    for (price, quantity) in prices.iter().zip(quantities).take(top_n) {
        let price = price.to_f64();
        let quantity = quantity.to_f64();
        if !price.is_finite() || price <= 0.0 {
            return Err("book prices must be finite and positive".to_string());
        }
        if !quantity.is_finite() || quantity < 0.0 {
            return Err("book quantities must be finite and nonnegative".to_string());
        }
        if let Some(previous) = previous_price {
            let out_of_order = if descending {
                price > previous
            } else {
                price < previous
            };
            if out_of_order {
                return Err("book levels are not price sorted".to_string());
            }
        }
        previous_price = Some(price);
        values.push((price, quantity));
    }
    values.resize(top_n, (0.0, 0.0));
    Ok(values)
}

#[derive(Debug, Clone, Copy)]
struct LobExecutionContext {
    symbol_venue: hft_core::VenueId,
    best_bid: Decimal,
    best_ask: Decimal,
}

impl LobExecutionContext {
    fn from_snapshot(snapshot: &MarketSnapshot) -> Result<Self, String> {
        Ok(Self {
            symbol_venue: snapshot
                .source_venue
                .ok_or_else(|| "snapshot source_venue is required".to_string())?,
            best_bid: snapshot
                .bids
                .first()
                .map(|level| level.price.0)
                .filter(|price| *price > Decimal::ZERO)
                .ok_or_else(|| "best bid is required".to_string())?,
            best_ask: snapshot
                .asks
                .first()
                .map(|level| level.price.0)
                .filter(|price| *price > Decimal::ZERO)
                .ok_or_else(|| "best ask is required".to_string())?,
        })
    }

    fn from_book(book: L2BookView<'_>) -> Result<Self, String> {
        Ok(Self {
            symbol_venue: book.venue,
            best_bid: Price::from(
                *book
                    .bid_prices
                    .first()
                    .ok_or_else(|| "best bid is required".to_string())?,
            )
            .0,
            best_ask: Price::from(
                *book
                    .ask_prices
                    .first()
                    .ok_or_else(|| "best ask is required".to_string())?,
            )
            .0,
        })
    }
}

#[derive(Debug)]
struct LobWindow {
    frames: VecDeque<LobFrame>,
    window_size: usize,
    top_n: usize,
}

impl LobWindow {
    fn new(window_size: usize, top_n: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(window_size),
            window_size,
            top_n,
        }
    }

    fn push(&mut self, frame: LobFrame) {
        debug_assert!(frame
            .channels
            .iter()
            .all(|channel| channel.len() == self.top_n));
        if self.frames.len() == self.window_size {
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
    }

    fn clear(&mut self) {
        self.frames.clear();
    }

    fn full_input(&self) -> Option<Vec<f32>> {
        if self.frames.len() != self.window_size {
            return None;
        }

        let mut input = Vec::with_capacity(4 * self.window_size * self.top_n);
        for channel in 0..4 {
            for frame in &self.frames {
                input.extend_from_slice(&frame.channels[channel]);
            }
        }
        Some(input)
    }
}

#[derive(Debug, Default)]
struct FailureState {
    total: u64,
    consecutive: u64,
    last_error: Option<String>,
}

impl FailureState {
    fn record(&mut self, error: String) {
        self.total = self.total.saturating_add(1);
        self.consecutive = self.consecutive.saturating_add(1);
        self.last_error = Some(error);
    }

    fn clear(&mut self) {
        self.consecutive = 0;
        self.last_error = None;
    }
}

#[derive(Debug)]
struct SymbolState {
    window: LobWindow,
    last_side: Option<Side>,
    failures: FailureState,
}

impl SymbolState {
    fn new(window_size: usize, top_n: usize) -> Self {
        Self {
            window: LobWindow::new(window_size, top_n),
            last_side: None,
            failures: FailureState::default(),
        }
    }
}

pub struct OnnxLobStrategy {
    config: OnnxLobStrategyConfig,
    predictor: OnnxPredictor,
    states: HashMap<Symbol, SymbolState>,
}

impl OnnxLobStrategy {
    pub fn new(config: OnnxLobStrategyConfig) -> HftResult<Self> {
        config
            .validate()
            .map_err(|error| HftError::Config(error.to_string()))?;
        let predictor = OnnxPredictor::load_verified(
            &config.model_path,
            &config.model_sha256,
            (1, 4, config.window_size, config.top_n),
        )
        .map_err(|error| {
            HftError::Config(format!(
                "failed to load ONNX model {} ({}): {error}",
                config.model_path.display(),
                config.model_version
            ))
        })?;
        let states = config
            .symbols
            .iter()
            .cloned()
            .map(|symbol| (symbol, SymbolState::new(config.window_size, config.top_n)))
            .collect();

        Ok(Self {
            config,
            predictor,
            states,
        })
    }

    pub fn model_version(&self) -> &str {
        &self.config.model_version
    }

    pub fn total_failures(&self, symbol: &Symbol) -> Option<u64> {
        self.states.get(symbol).map(|state| state.failures.total)
    }

    pub fn consecutive_failures(&self, symbol: &Symbol) -> Option<u64> {
        self.states
            .get(symbol)
            .map(|state| state.failures.consecutive)
    }

    pub fn last_failure(&self, symbol: &Symbol) -> Option<&str> {
        self.states
            .get(symbol)
            .and_then(|state| state.failures.last_error.as_deref())
    }

    fn record_failure(&mut self, symbol: &Symbol, error: String, clear_window: bool) {
        if let Some(state) = self.states.get_mut(symbol) {
            if clear_window {
                state.window.clear();
            }
            state.failures.record(error);
        }
    }

    fn handle_snapshot(&mut self, snapshot: &MarketSnapshot) -> Vec<OrderIntent> {
        if !self.states.contains_key(&snapshot.symbol) {
            return Vec::new();
        }

        let (frame, _) = match LobFrame::from_snapshot(snapshot, self.config.top_n) {
            Ok(features) => features,
            Err(error) => {
                self.record_failure(&snapshot.symbol, error, true);
                return Vec::new();
            }
        };
        let execution_context = LobExecutionContext::from_snapshot(snapshot);
        self.handle_frame(&snapshot.symbol, frame, execution_context)
    }

    fn handle_book(&mut self, book: L2BookView<'_>) -> Vec<OrderIntent> {
        if !self.states.contains_key(book.symbol) {
            return Vec::new();
        }
        let (frame, _) = match LobFrame::from_book(book, self.config.top_n) {
            Ok(features) => features,
            Err(error) => {
                self.record_failure(book.symbol, error, true);
                return Vec::new();
            }
        };
        let execution_context = LobExecutionContext::from_book(book);
        self.handle_frame(book.symbol, frame, execution_context)
    }

    fn handle_frame(
        &mut self,
        symbol: &Symbol,
        frame: LobFrame,
        execution_context: Result<LobExecutionContext, String>,
    ) -> Vec<OrderIntent> {
        let input = {
            let state = self
                .states
                .get_mut(symbol)
                .expect("configured symbol state must exist");
            state.window.push(frame);
            match state.window.full_input() {
                Some(input) => input,
                None => return Vec::new(),
            }
        };

        let output = match self.predictor.infer(&input) {
            Ok(output) => output,
            Err(error) => {
                self.record_failure(symbol, error.to_string(), false);
                return Vec::new();
            }
        };
        let signal = match output.first().copied() {
            Some(signal) if signal.is_finite() => signal,
            Some(_) => {
                self.record_failure(
                    symbol,
                    "first model output is not finite".to_string(),
                    false,
                );
                return Vec::new();
            }
            None => {
                self.record_failure(symbol, "model returned no outputs".to_string(), false);
                return Vec::new();
            }
        };

        let previous_side = self
            .states
            .get(symbol)
            .expect("configured symbol state must exist")
            .last_side;
        let (emit_side, current_side) =
            signal_crossing(signal, self.config.output_threshold, previous_side);
        let order = match emit_side {
            Some(side) => match execution_context
                .and_then(|context| build_order(&self.config, symbol, context, side))
            {
                Ok(order) => Some(order),
                Err(error) => {
                    self.record_failure(symbol, error, false);
                    return Vec::new();
                }
            },
            None => None,
        };

        let state = self
            .states
            .get_mut(symbol)
            .expect("configured symbol state must exist");
        state.failures.clear();
        state.last_side = current_side;
        order.into_iter().collect()
    }
}

fn signal_crossing(
    output: f32,
    threshold: f64,
    previous_side: Option<Side>,
) -> (Option<Side>, Option<Side>) {
    let output = f64::from(output);
    let current_side = if output > threshold {
        Some(Side::Buy)
    } else if output < -threshold {
        Some(Side::Sell)
    } else {
        None
    };
    let emit_side = current_side.filter(|side| Some(*side) != previous_side);
    (emit_side, current_side)
}

fn build_order(
    config: &OnnxLobStrategyConfig,
    symbol: &Symbol,
    context: LobExecutionContext,
    side: Side,
) -> Result<OrderIntent, String> {
    let limit_price = match side {
        Side::Buy => context.best_ask,
        Side::Sell => context.best_bid,
    };
    if limit_price <= Decimal::ZERO {
        return Err("executable limit price is required".to_string());
    }
    let quantity = config
        .max_order_notional
        .checked_div(limit_price)
        .filter(|quantity| *quantity > Decimal::ZERO)
        .ok_or_else(|| "order quantity cannot be represented".to_string())?;

    Ok(OrderIntent::crypto_spot(
        symbol.clone(),
        side,
        Quantity(quantity),
        OrderType::Limit,
        Some(Price(limit_price)),
        TimeInForce::IOC,
        config.name.clone(),
        Some(context.symbol_venue),
    ))
}

impl Strategy for OnnxLobStrategy {
    fn on_market_event(&mut self, event: &MarketEvent, _account: &AccountView) -> Vec<OrderIntent> {
        match event {
            MarketEvent::Snapshot(snapshot) => self.handle_snapshot(snapshot),
            _ => Vec::new(),
        }
    }

    fn on_market_event_with_context(
        &mut self,
        event: &MarketEvent,
        context: &StrategyContext<'_>,
    ) -> Vec<OrderIntent> {
        match event {
            MarketEvent::Snapshot(snapshot) if self.states.contains_key(&snapshot.symbol) => {
                context
                    .book
                    .map_or_else(Vec::new, |book| self.handle_book(book))
            }
            MarketEvent::Update(update) if self.states.contains_key(&update.symbol) => context
                .book
                .map_or_else(Vec::new, |book| self.handle_book(book)),
            MarketEvent::Quote(quote) if self.states.contains_key(&quote.symbol) => context
                .book
                .map_or_else(Vec::new, |book| self.handle_book(book)),
            _ => Vec::new(),
        }
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{AssetClass, FixedPrice, FixedQuantity, ProductType, Symbol, VenueId};
    use ports::{BookLevel, L2BookView, MarketSnapshot};
    use rust_decimal::Decimal;
    use sha2::Digest as _;
    use std::path::PathBuf;

    fn config() -> OnnxLobStrategyConfig {
        OnnxLobStrategyConfig {
            name: "onnx-lob-v1".to_string(),
            symbols: vec![Symbol::new("BTCUSDT")],
            model_path: PathBuf::from("model.onnx"),
            model_version: "sha256:test".to_string(),
            model_sha256: "a".repeat(64),
            top_n: 2,
            window_size: 2,
            max_order_notional: Decimal::from(1_000),
            output_threshold: 0.5,
        }
    }

    fn snapshot() -> MarketSnapshot {
        MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 1,
            bids: vec![
                BookLevel::new_unchecked(100.0, 3.0),
                BookLevel::new_unchecked(99.0, 8.0),
            ],
            asks: vec![
                BookLevel::new_unchecked(101.0, 15.0),
                BookLevel::new_unchecked(102.0, 24.0),
            ],
            sequence: 1,
            source_venue: Some(VenueId::BINANCE_SPOT),
        }
    }

    #[test]
    fn validates_onnx_lob_config() {
        let mut cfg = config();
        assert_eq!(cfg.validate(), Ok(()));

        cfg.symbols.clear();
        assert_eq!(
            cfg.validate(),
            Err(OnnxLobStrategyValidationError::EmptySymbols)
        );

        cfg.symbols = vec![Symbol::new("")];
        assert_eq!(
            cfg.validate(),
            Err(OnnxLobStrategyValidationError::EmptySymbol { index: 0 })
        );

        cfg.symbols = vec![Symbol::new("BTCUSDT"), Symbol::new("BTCUSDT")];
        assert_eq!(
            cfg.validate(),
            Err(OnnxLobStrategyValidationError::DuplicateSymbol(
                "BTCUSDT".to_string()
            ))
        );

        cfg = config();
        cfg.top_n = 0;
        assert_eq!(
            cfg.validate(),
            Err(OnnxLobStrategyValidationError::ZeroTopN)
        );

        cfg = config();
        cfg.window_size = 0;
        assert_eq!(
            cfg.validate(),
            Err(OnnxLobStrategyValidationError::ZeroWindowSize)
        );

        cfg = config();
        cfg.model_sha256 = "ABC".to_string();
        assert_eq!(
            cfg.validate(),
            Err(OnnxLobStrategyValidationError::InvalidModelSha256)
        );

        cfg = config();
        cfg.top_n = MAX_ONNX_INPUT_ELEMENTS;
        assert_eq!(
            cfg.validate(),
            Err(OnnxLobStrategyValidationError::InputTooLarge)
        );

        for notional in [Decimal::ZERO, -Decimal::ONE] {
            cfg = config();
            cfg.max_order_notional = notional;
            assert_eq!(
                cfg.validate(),
                Err(OnnxLobStrategyValidationError::NonPositiveMaxOrderNotional)
            );
        }

        for threshold in [-1.0, f64::NAN, f64::INFINITY] {
            cfg = config();
            cfg.output_threshold = threshold;
            assert_eq!(
                cfg.validate(),
                Err(OnnxLobStrategyValidationError::InvalidOutputThreshold)
            );
        }
    }

    #[test]
    fn extracts_relative_price_and_log_quantity_channels() {
        let (frame, mid) = LobFrame::from_snapshot(&snapshot(), 2).unwrap();

        assert_eq!(mid, Decimal::new(1005, 1));
        assert_eq!(frame.channels[0], vec![-0.5, -1.5]);
        assert_eq!(frame.channels[2], vec![0.5, 1.5]);
        assert!((frame.channels[1][0] - 4.0_f32.ln()).abs() < 1e-6);
        assert!((frame.channels[1][1] - 9.0_f32.ln()).abs() < 1e-6);
        assert!((frame.channels[3][0] - 16.0_f32.ln()).abs() < 1e-6);
        assert!((frame.channels[3][1] - 25.0_f32.ln()).abs() < 1e-6);
    }

    #[test]
    fn extracts_onnx_frame_from_canonical_lob_view() {
        let symbol = Symbol::new("BTCUSDT");
        let bid_prices = [FixedPrice::from_f64(100.0), FixedPrice::from_f64(99.0)];
        let bid_quantities = [FixedQuantity::from_f64(3.0), FixedQuantity::from_f64(8.0)];
        let ask_prices = [FixedPrice::from_f64(101.0), FixedPrice::from_f64(102.0)];
        let ask_quantities = [FixedQuantity::from_f64(15.0), FixedQuantity::from_f64(24.0)];
        let book = L2BookView {
            symbol: &symbol,
            venue: VenueId::BINANCE_SPOT,
            timestamp: 2,
            sequence: 2,
            bid_prices: &bid_prices,
            bid_quantities: &bid_quantities,
            ask_prices: &ask_prices,
            ask_quantities: &ask_quantities,
        };

        let (frame, mid) = LobFrame::from_book(book, 2).unwrap();

        assert_eq!(mid, Decimal::new(1005, 1));
        assert_eq!(frame.channels[0], vec![-0.5, -1.5]);
        assert_eq!(frame.channels[2], vec![0.5, 1.5]);
    }

    #[test]
    fn rejects_empty_and_crossed_books() {
        let mut malformed = snapshot();
        malformed.bids.clear();
        assert!(LobFrame::from_snapshot(&malformed, 2).is_err());

        malformed = snapshot();
        malformed.bids[0] = BookLevel::new_unchecked(102.0, 3.0);
        assert!(LobFrame::from_snapshot(&malformed, 2).is_err());
    }

    #[test]
    fn rolling_window_flattens_in_channel_time_level_order() {
        let mut window = LobWindow::new(2, 2);
        window.push(LobFrame {
            channels: [
                vec![1.0, 2.0],
                vec![3.0, 4.0],
                vec![5.0, 6.0],
                vec![7.0, 8.0],
            ],
        });
        assert!(window.full_input().is_none());

        window.push(LobFrame {
            channels: [
                vec![9.0, 10.0],
                vec![11.0, 12.0],
                vec![13.0, 14.0],
                vec![15.0, 16.0],
            ],
        });

        assert_eq!(
            window.full_input().unwrap(),
            vec![
                1.0, 2.0, 9.0, 10.0, 3.0, 4.0, 11.0, 12.0, 5.0, 6.0, 13.0, 14.0, 7.0, 8.0, 15.0,
                16.0,
            ]
        );
    }

    #[test]
    fn signal_crossings_suppress_repeats_and_reset_at_neutral() {
        let threshold = 0.5;
        let mut previous = None;

        let (emit, current) = signal_crossing(0.6, threshold, previous);
        assert_eq!(emit, Some(Side::Buy));
        previous = current;

        let (emit, current) = signal_crossing(0.8, threshold, previous);
        assert_eq!(emit, None);
        previous = current;

        let (emit, current) = signal_crossing(0.1, threshold, previous);
        assert_eq!(emit, None);
        previous = current;

        let (emit, current) = signal_crossing(0.7, threshold, previous);
        assert_eq!(emit, Some(Side::Buy));
        previous = current;

        let (emit, _) = signal_crossing(-0.7, threshold, previous);
        assert_eq!(emit, Some(Side::Sell));
    }

    #[test]
    fn builds_bounded_crypto_spot_limit_ioc_with_source_venue() {
        let cfg = config();
        let snapshot = snapshot();
        let context = LobExecutionContext::from_snapshot(&snapshot).unwrap();
        let order = build_order(&cfg, &snapshot.symbol, context, Side::Buy).unwrap();

        assert_eq!(order.asset_class, AssetClass::Crypto);
        assert_eq!(order.product_type, ProductType::Spot);
        assert_eq!(
            order.quantity.0,
            Decimal::from(1_000)
                .checked_div(Decimal::from(101))
                .unwrap()
        );
        assert_eq!(order.order_type, OrderType::Limit);
        assert_eq!(order.price, Some(Price(Decimal::from(101))));
        assert_eq!(order.time_in_force, TimeInForce::IOC);
        assert_eq!(order.strategy_id, cfg.name);
        assert_eq!(order.target_venue, snapshot.source_venue);

        let mut no_venue = snapshot;
        no_venue.source_venue = None;
        assert!(LobExecutionContext::from_snapshot(&no_venue).is_err());
    }

    #[test]
    fn constructor_rejects_malformed_onnx_model() {
        let path = std::env::temp_dir().join(format!(
            "hft-strategy-dl-malformed-{}-{}.onnx",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"not an onnx model").unwrap();
        let mut cfg = config();
        cfg.model_path = path.clone();
        cfg.model_sha256 = hex::encode(sha2::Sha256::digest(b"not an onnx model"));

        let result = OnnxLobStrategy::new(cfg);
        let _ = std::fs::remove_file(path);

        assert!(result.is_err());
    }
}
