//! Conservative displayed-liquidity model. It makes no passive queue or hidden
//! liquidity claim. Unchanged depth cannot be spent again by another paper order.
use std::collections::BTreeMap;

use engine::aggregation::TopNSnapshot;
use hft_core::{OrderType, Price, Quantity, Side};
use ports::OpenOrder;
use rust_decimal::Decimal;

pub(super) const MAX_BOOK_AGE_US: u64 = 1_000_000;

#[derive(Clone, Default)]
pub(super) struct BookBudget {
    sequence: u64,
    received_at: u64,
    bids: BTreeMap<Decimal, Level>,
    asks: BTreeMap<Decimal, Level>,
}

#[derive(Clone)]
struct Level {
    observed: Decimal,
    remaining: Decimal,
}

pub(super) fn valid_book(book: &TopNSnapshot, now: u64) -> bool {
    let Some(received) = book.local_receive.map(|time| time.as_micros()) else {
        return false;
    };
    let valid_side =
        |prices: &[hft_core::FixedPrice], quantities: &[hft_core::FixedQuantity], bids: bool| {
            !prices.is_empty()
                && prices.len() == quantities.len()
                && prices
                    .iter()
                    .all(|price| Price::from(*price).0 > Decimal::ZERO)
                && quantities
                    .iter()
                    .all(|quantity| Quantity::from(*quantity).0 > Decimal::ZERO)
                && prices.windows(2).all(|pair| {
                    if bids {
                        pair[0] > pair[1]
                    } else {
                        pair[0] < pair[1]
                    }
                })
        };
    received > 0
        && received <= now
        && now - received <= MAX_BOOK_AGE_US
        && valid_side(&book.bid_prices, &book.bid_quantities, true)
        && valid_side(&book.ask_prices, &book.ask_quantities, false)
        && book.bid_prices[0] < book.ask_prices[0]
}

impl BookBudget {
    pub(super) fn observe(&mut self, book: &TopNSnapshot, now: u64) -> bool {
        if !valid_book(book, now) || book.sequence < self.sequence {
            return false;
        }
        let received = book.local_receive.unwrap().as_micros();
        if received < self.received_at {
            return false;
        }
        self.sequence = book.sequence;
        self.received_at = received;
        let refresh = |previous: &BTreeMap<Decimal, Level>,
                       prices: &[hft_core::FixedPrice],
                       quantities: &[hft_core::FixedQuantity]| {
            prices
                .iter()
                .zip(quantities)
                .map(|(price, quantity)| {
                    let price = Price::from(*price).0;
                    let observed = Quantity::from(*quantity).0;
                    let remaining = previous.get(&price).map_or(observed, |level| {
                        // Only an observed increase replenishes virtual liquidity.
                        // A decrease may be other participants; never undo our consumption.
                        level.remaining.min(observed)
                            + (observed - level.observed).max(Decimal::ZERO)
                    });
                    (
                        price,
                        Level {
                            observed,
                            remaining,
                        },
                    )
                })
                .collect()
        };
        self.bids = refresh(&self.bids, &book.bid_prices, &book.bid_quantities);
        self.asks = refresh(&self.asks, &book.ask_prices, &book.ask_quantities);
        true
    }

    pub(super) fn fills(&mut self, order: &OpenOrder) -> Vec<(Price, Quantity)> {
        let levels = match order.side {
            Side::Buy => &mut self.asks,
            Side::Sell => &mut self.bids,
        };
        let mut remaining = order.remaining_quantity.0;
        let mut fills = Vec::new();
        let mut take = |price: &Decimal, level: &mut Level| {
            let crosses = order.order_type == OrderType::Market
                || order.price.is_some_and(|limit| match order.side {
                    Side::Buy => *price <= limit.0,
                    Side::Sell => *price >= limit.0,
                });
            if crosses && remaining > Decimal::ZERO {
                let quantity = remaining.min(level.remaining);
                if quantity > Decimal::ZERO {
                    remaining -= quantity;
                    level.remaining -= quantity;
                    fills.push((Price(*price), Quantity(quantity)));
                }
            }
        };
        match order.side {
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
}
