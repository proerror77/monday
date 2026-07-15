//! Live data feed backed by a tokio broadcast channel.
//!
//! Used for both dry-run and live trading. The feed blocks on
//! `recv()` until the next market update arrives from the WebSocket
//! adapters.

use async_trait::async_trait;
use tokio::sync::broadcast;
use tracing::warn;

use crate::traits::{Feed, MarketUpdate};

/// Live feed that consumes market updates from a broadcast channel.
///
/// Multiple strategies can subscribe to the same broadcast sender.
/// Any lag closes the feed so live/dry-run runtimes fail closed instead of
/// evaluating against a market state with missing deltas.
pub struct LiveFeed {
    rx: broadcast::Receiver<MarketUpdate>,
}

impl LiveFeed {
    /// Create a live feed from a broadcast receiver.
    pub fn new(rx: broadcast::Receiver<MarketUpdate>) -> Self {
        Self { rx }
    }
}

#[async_trait]
impl Feed for LiveFeed {
    async fn next(&mut self) -> Option<MarketUpdate> {
        match self.rx.recv().await {
            Ok(update) => Some(update),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(skipped = n, "LiveFeed lagged; closing feed fail-closed");
                None
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LiveFeed;
    use crate::traits::{Feed, MarketUpdate};
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn lagged_live_feed_closes_fail_closed() {
        let (tx, rx) = broadcast::channel(1);
        let mut feed = LiveFeed::new(rx);
        let update = |price| MarketUpdate::SpotPrice {
            symbol: Arc::from("BTCUSDT"),
            price,
            ts: Utc::now(),
        };

        tx.send(update(Decimal::ONE)).unwrap();
        tx.send(update(Decimal::from(2))).unwrap();

        assert!(feed.next().await.is_none());
    }
}
