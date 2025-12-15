//! Common utilities for market data adapters.
//!
//! This crate intentionally keeps only reusable, exchange-agnostic helpers
//! (parsing, subscriptions, light helpers). It does not implement any
//! exchange-specific logic.

pub mod converter;
pub mod errors;
pub mod rest_helpers;
pub mod subscriptions;
pub mod ws_helpers;

pub use converter::{parse_bytes, parse_json, parse_owned_value, parse_price, parse_quantity};
pub use errors::{AdapterError, AdapterResult};
pub use subscriptions::{Subscription, SubscriptionBuilder, SubscriptionKind};
pub use ws_helpers::{calculate_exponential_backoff, ReconnectConfig};
