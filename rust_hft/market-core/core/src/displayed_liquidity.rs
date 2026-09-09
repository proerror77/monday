//! Deterministic matching against observed displayed depth.
//!
//! This module intentionally owns only the counterfactual book budget.  It has
//! no runtime, venue, or execution-port dependency, so Paper and historical
//! replay use the same consumption rules.  It does not model passive queue
//! position, hidden liquidity, or market impact.

use std::collections::BTreeMap;

use rust_decimal::Decimal;

use crate::{OrderType, Price, Quantity, Side};

/// Maximum age accepted for an observed book by callers that use the standard
/// userspace-receive clock.
pub const MAX_DISPLAYED_BOOK_AGE_US: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayedBookLevel {
    pub price: Price,
    pub quantity: Quantity,
}

impl DisplayedBookLevel {
    pub const fn new(price: Price, quantity: Quantity) -> Self {
        Self { price, quantity }
    }
}

/// A validated point-in-time book supplied by a market-data adapter or replay.
/// `generation` identifies a fresh snapshot stream; `sequence` and
/// `received_at_us` must be monotonic inside a generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayedBookSnapshot {
    pub sequence: u64,
    pub generation: u64,
    pub received_at_us: u64,
    pub bids: Vec<DisplayedBookLevel>,
    pub asks: Vec<DisplayedBookLevel>,
}

impl DisplayedBookSnapshot {
    pub fn new(
        sequence: u64,
        generation: u64,
        received_at_us: u64,
        bids: Vec<DisplayedBookLevel>,
        asks: Vec<DisplayedBookLevel>,
    ) -> Self {
        Self {
            sequence,
            generation,
            received_at_us,
            bids,
            asks,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayedFill {
    pub price: Price,
    pub quantity: Quantity,
}

#[derive(Clone, Default)]
pub struct BookBudget {
    sequence: u64,
    generation: u64,
    received_at_us: u64,
    bids: BTreeMap<Decimal, BudgetLevel>,
    asks: BTreeMap<Decimal, BudgetLevel>,
}

#[derive(Clone)]
struct BudgetLevel {
    observed: Decimal,
    remaining: Decimal,
}

impl BookBudget {
    /// Validate a supplied book against the standard displayed-depth rules.
    pub fn valid_snapshot(book: &DisplayedBookSnapshot, now_us: u64) -> bool {
        if book.generation == 0
            || book.received_at_us == 0
            || book.received_at_us > now_us
            || now_us - book.received_at_us > MAX_DISPLAYED_BOOK_AGE_US
            || book.bids.is_empty()
            || book.asks.is_empty()
        {
            return false;
        }
        let valid_side = |levels: &[DisplayedBookLevel], bids: bool| {
            levels
                .iter()
                .all(|level| level.price.0 > Decimal::ZERO && level.quantity.0 > Decimal::ZERO)
                && levels.windows(2).all(|pair| {
                    if bids {
                        pair[0].price > pair[1].price
                    } else {
                        pair[0].price < pair[1].price
                    }
                })
        };
        valid_side(&book.bids, true)
            && valid_side(&book.asks, false)
            && book.bids[0].price < book.asks[0].price
    }

    /// Observe a new book and reconcile external additions/removals with our
    /// own virtual consumption.  A rejected observation leaves this budget
    /// untouched and returns `false`.
    pub fn observe(&mut self, book: &DisplayedBookSnapshot, now_us: u64) -> bool {
        if !Self::valid_snapshot(book, now_us)
            || book.generation < self.generation
            || (book.generation == self.generation && book.sequence < self.sequence)
            || (book.generation == self.generation && book.received_at_us < self.received_at_us)
        {
            return false;
        }
        if book.generation > self.generation {
            *self = Self {
                generation: book.generation,
                ..Self::default()
            };
        }

        self.sequence = book.sequence;
        self.received_at_us = book.received_at_us;
        self.bids = reconcile_levels(&self.bids, &book.bids);
        self.asks = reconcile_levels(&self.asks, &book.asks);
        true
    }

    /// Remove all levels while preserving the stream identity.  Callers use
    /// this when a symbol disappears from the current market view; a later
    /// reappearance must establish fresh displayed liquidity.
    pub fn clear_levels(&mut self) {
        self.bids.clear();
        self.asks.clear();
    }

    /// Return whether the currently observed displayed book is still usable at
    /// `now_us`.  This deliberately uses the same receive-clock age bound as
    /// snapshot validation; a caller cannot make an old book fresh by
    /// observing an unrelated event such as a trade.
    pub fn is_fresh(&self, now_us: u64) -> bool {
        self.received_at_us > 0
            && self.received_at_us <= now_us
            && now_us - self.received_at_us <= MAX_DISPLAYED_BOOK_AGE_US
            && self.mid_price().is_some()
    }

    /// Match a taker order against the budget in price-time order.  The
    /// caller owns TIF semantics; cloning the budget before this call gives a
    /// transactional FOK attempt.
    pub fn fills(
        &mut self,
        side: Side,
        order_type: OrderType,
        limit_price: Option<Price>,
        quantity: Quantity,
    ) -> Vec<DisplayedFill> {
        self.fills_with_price_bound(side, order_type, limit_price, quantity, None)
    }

    /// Match a taker order while applying an additional caller-owned price
    /// bound (for example, a frozen slippage ceiling).  The bound is combined
    /// with a limit order and levels outside it are left unconsumed.
    pub fn fills_with_price_bound(
        &mut self,
        side: Side,
        order_type: OrderType,
        limit_price: Option<Price>,
        quantity: Quantity,
        price_bound: Option<Price>,
    ) -> Vec<DisplayedFill> {
        self.fills_with_price_bound_and_participation(
            side,
            order_type,
            limit_price,
            quantity,
            price_bound,
            Decimal::ONE,
        )
    }

    /// Variant used by bounded historical/diagnostic fills where the policy
    /// caps participation at each displayed level.
    pub fn fills_with_price_bound_and_participation(
        &mut self,
        side: Side,
        order_type: OrderType,
        limit_price: Option<Price>,
        quantity: Quantity,
        price_bound: Option<Price>,
        participation: Decimal,
    ) -> Vec<DisplayedFill> {
        if quantity.0 <= Decimal::ZERO {
            return Vec::new();
        }
        if participation <= Decimal::ZERO {
            return Vec::new();
        }
        let participation = participation.min(Decimal::ONE);
        let limit_price = match order_type {
            // A caller-provided order price is meaningful only for a limit
            // order.  Market orders remain unconditional takers unless an
            // explicit caller-owned bound is supplied.
            OrderType::Market => price_bound,
            OrderType::Limit => match (limit_price, price_bound, side) {
                (Some(limit), Some(bound), Side::Buy) => Some(Price(limit.0.min(bound.0))),
                (Some(limit), Some(bound), Side::Sell) => Some(Price(limit.0.max(bound.0))),
                (Some(limit), None, _) | (None, Some(limit), _) => Some(limit),
                (None, None, _) => None,
            },
        };
        let levels = match side {
            Side::Buy => &mut self.asks,
            Side::Sell => &mut self.bids,
        };
        let mut remaining = quantity.0;
        let mut fills = Vec::new();
        let mut take = |price: &Decimal, level: &mut BudgetLevel| {
            let crosses = match limit_price {
                Some(limit) => match side {
                    Side::Buy => *price <= limit.0,
                    Side::Sell => *price >= limit.0,
                },
                None => order_type == OrderType::Market,
            };
            if crosses && remaining > Decimal::ZERO {
                let quantity = remaining.min(level.remaining * participation);
                if quantity > Decimal::ZERO {
                    remaining -= quantity;
                    level.remaining -= quantity;
                    fills.push(DisplayedFill {
                        price: Price(*price),
                        quantity: Quantity(quantity),
                    });
                }
            }
        };
        match side {
            Side::Buy => levels
                .iter_mut()
                .for_each(|(price, level)| take(price, level)),
            Side::Sell => levels
                .iter_mut()
                .rev()
                .for_each(|(price, level)| take(price, level)),
        }
        fills
    }

    pub fn level_counts(&self) -> (usize, usize) {
        (self.bids.len(), self.asks.len())
    }

    pub fn same_side_depth(&self, side: Side, levels: usize) -> Option<Quantity> {
        if levels == 0 {
            return None;
        }
        let mut depth = Decimal::ZERO;
        let mut found = false;
        match side {
            Side::Buy => {
                for level in self.asks.values().take(levels) {
                    found = true;
                    depth += level.remaining;
                }
            }
            Side::Sell => {
                for level in self.bids.values().rev().take(levels) {
                    found = true;
                    depth += level.remaining;
                }
            }
        }
        found.then_some(Quantity(depth))
    }

    pub fn best_bid(&self) -> Option<(Price, Quantity)> {
        self.bids
            .iter()
            .next_back()
            .map(|(price, level)| (Price(*price), Quantity(level.remaining)))
    }

    pub fn best_ask(&self) -> Option<(Price, Quantity)> {
        self.asks
            .iter()
            .next()
            .map(|(price, level)| (Price(*price), Quantity(level.remaining)))
    }

    pub fn mid_price(&self) -> Option<Price> {
        let (bid, _) = self.best_bid()?;
        let (ask, _) = self.best_ask()?;
        (bid.0 < ask.0).then_some(Price((bid.0 + ask.0) / Decimal::from(2)))
    }

    pub fn spread_bps(&self) -> Option<Decimal> {
        let (bid, _) = self.best_bid()?;
        let (ask, _) = self.best_ask()?;
        let mid = (bid.0 + ask.0) / Decimal::from(2);
        (mid > Decimal::ZERO).then_some((ask.0 - bid.0) / mid * Decimal::from(10_000))
    }
}

fn reconcile_levels(
    previous: &BTreeMap<Decimal, BudgetLevel>,
    levels: &[DisplayedBookLevel],
) -> BTreeMap<Decimal, BudgetLevel> {
    levels
        .iter()
        .map(|level| {
            let observed = level.quantity.0;
            let remaining = previous.get(&level.price.0).map_or(observed, |previous| {
                // External changes adjust the counterfactual remainder while
                // retaining our virtual consumption at this price.
                (previous.remaining + (observed - previous.observed)).clamp(Decimal::ZERO, observed)
            });
            (
                level.price.0,
                BudgetLevel {
                    observed,
                    remaining,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(value: i64) -> Price {
        Price(Decimal::from(value))
    }

    fn quantity(value: i64) -> Quantity {
        Quantity(Decimal::from(value))
    }

    fn book(sequence: u64, asks: &[i64]) -> DisplayedBookSnapshot {
        DisplayedBookSnapshot::new(
            sequence,
            1,
            1_000,
            vec![DisplayedBookLevel::new(price(99), quantity(10))],
            asks.iter()
                .map(|value| DisplayedBookLevel::new(price(*value), quantity(2)))
                .collect(),
        )
    }

    #[test]
    fn vwap_walks_levels_and_partial_fill_is_explicit() {
        let mut budget = BookBudget::default();
        assert!(budget.observe(&book(1, &[100, 101]), 1_000));
        let fills = budget.fills(Side::Buy, OrderType::Market, None, quantity(5));
        assert_eq!(
            fills,
            vec![
                DisplayedFill {
                    price: price(100),
                    quantity: quantity(2),
                },
                DisplayedFill {
                    price: price(101),
                    quantity: quantity(2),
                },
            ]
        );
        assert_eq!(budget.same_side_depth(Side::Buy, 2), Some(quantity(0)));
    }

    #[test]
    fn market_order_price_is_ignored_but_an_explicit_bound_is_honored() {
        let mut unrestricted = BookBudget::default();
        assert!(unrestricted.observe(&book(1, &[100, 101]), 1_000));
        assert_eq!(
            unrestricted.fills(Side::Buy, OrderType::Market, Some(price(99)), quantity(1)),
            vec![DisplayedFill {
                price: price(100),
                quantity: quantity(1),
            }]
        );

        let mut bounded = BookBudget::default();
        assert!(bounded.observe(&book(1, &[100, 101]), 1_000));
        assert!(bounded
            .fills_with_price_bound(
                Side::Buy,
                OrderType::Market,
                Some(price(999)),
                quantity(1),
                Some(price(99)),
            )
            .is_empty());
    }

    #[test]
    fn unchanged_depth_cannot_replenish_virtual_consumption() {
        let mut budget = BookBudget::default();
        assert!(budget.observe(&book(1, &[100]), 1_000));
        assert_eq!(
            budget
                .fills(Side::Buy, OrderType::Market, None, quantity(2))
                .len(),
            1
        );
        assert!(budget.observe(&book(2, &[100]), 1_000));
        assert!(budget
            .fills(Side::Buy, OrderType::Market, None, quantity(2))
            .is_empty());
    }

    #[test]
    fn external_addition_restores_only_the_observed_increment() {
        let mut budget = BookBudget::default();
        assert!(budget.observe(&book(1, &[100]), 1_000));
        assert_eq!(
            budget
                .fills(Side::Buy, OrderType::Market, None, quantity(1))
                .len(),
            1
        );
        let mut replenished = book(2, &[100]);
        replenished.asks[0].quantity = quantity(3);
        assert!(budget.observe(&replenished, 1_000));
        assert_eq!(
            budget.fills(Side::Buy, OrderType::Market, None, quantity(3))[0].quantity,
            quantity(2)
        );
    }

    #[test]
    fn invalid_or_stale_snapshot_is_rejected_without_mutation() {
        let mut budget = BookBudget::default();
        assert!(budget.observe(&book(1, &[100]), 1_000));
        let invalid = DisplayedBookSnapshot::new(
            2,
            1,
            1,
            vec![DisplayedBookLevel::new(price(99), quantity(10))],
            vec![DisplayedBookLevel::new(price(98), quantity(10))],
        );
        assert!(!budget.observe(&invalid, 1_000));
        assert_eq!(budget.best_ask(), Some((price(100), quantity(2))));
        let regressed = book(0, &[100]);
        assert!(!budget.observe(&regressed, 1_000));
    }
}
