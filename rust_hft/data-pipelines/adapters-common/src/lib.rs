//! Common utilities for market data adapters.
//!
//! This crate intentionally keeps only reusable, exchange-agnostic helpers
//! (parsing, subscriptions, light helpers). It does not implement any
//! exchange-specific logic.

pub mod errors;
pub mod converter;
pub mod subscriptions;
pub mod ws_helpers;
pub mod rest_helpers;

pub use errors::{AdapterError, AdapterResult};
pub use converter::{parse_json, parse_price, parse_quantity};
pub use subscriptions::{Subscription, SubscriptionKind, SubscriptionBuilder};

