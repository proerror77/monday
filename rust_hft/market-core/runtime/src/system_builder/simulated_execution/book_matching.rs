//! Runtime boundary for the shared displayed-liquidity matcher.
//!
//! The matching state and consumption rules live in `hft-core`; this module
//! only converts the engine-owned Top-N snapshot into the core representation.

use engine::aggregation::TopNSnapshot;
use hft_core::{BookBudget, DisplayedBookLevel, DisplayedBookSnapshot, Price, Quantity};

pub(super) fn displayed_snapshot(book: &TopNSnapshot) -> Option<DisplayedBookSnapshot> {
    if book.bid_prices.len() != book.bid_quantities.len()
        || book.ask_prices.len() != book.ask_quantities.len()
    {
        return None;
    }
    Some(DisplayedBookSnapshot::new(
        book.sequence,
        book.generation,
        book.local_receive
            .map(|timestamp| timestamp.as_micros())
            .unwrap_or_default(),
        book.bid_prices
            .iter()
            .zip(&book.bid_quantities)
            .map(|(price, quantity)| {
                DisplayedBookLevel::new(Price::from(*price), Quantity::from(*quantity))
            })
            .collect(),
        book.ask_prices
            .iter()
            .zip(&book.ask_quantities)
            .map(|(price, quantity)| {
                DisplayedBookLevel::new(Price::from(*price), Quantity::from(*quantity))
            })
            .collect(),
    ))
}

pub(super) fn valid_book(book: &TopNSnapshot, now_us: u64) -> bool {
    displayed_snapshot(book).is_some_and(|snapshot| BookBudget::valid_snapshot(&snapshot, now_us))
}
