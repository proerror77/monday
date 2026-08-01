//! Live data feed backed by a tokio broadcast channel.
//!
//! Used for both dry-run and live trading. The feed blocks on
//! `recv()` until the next market update arrives from the WebSocket
//! adapters.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::warn;

use crate::traits::{Feed, MarketUpdate};

/// How the feed reacts when the broadcast receiver lags behind producers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LagPolicy {
    /// Close the feed on any lag so live/dry-run trading runtimes fail closed
    /// instead of evaluating against a market state with missing deltas.
    #[default]
    FailClosed,
    /// Skip the missed updates and keep consuming. Intended for pure data
    /// recorders, where a process restart loses more tape than a bounded gap.
    SkipAndContinue,
}

/// Live feed that consumes market updates from a broadcast channel.
///
/// Multiple strategies can subscribe to the same broadcast sender.
/// Lag handling is controlled by [`LagPolicy`]; the default closes the feed so
/// live/dry-run runtimes fail closed instead of evaluating against a market
/// state with missing deltas.
pub struct LiveFeed {
    rx: broadcast::Receiver<MarketUpdate>,
    lag_policy: LagPolicy,
}

impl LiveFeed {
    /// Create a live feed from a broadcast receiver.
    pub fn new(rx: broadcast::Receiver<MarketUpdate>) -> Self {
        Self::with_lag_policy(rx, LagPolicy::default())
    }

    /// Create a live feed with an explicit lag policy.
    pub fn with_lag_policy(rx: broadcast::Receiver<MarketUpdate>, lag_policy: LagPolicy) -> Self {
        Self { rx, lag_policy }
    }
}

#[async_trait]
impl Feed for LiveFeed {
    async fn next(&mut self) -> Option<MarketUpdate> {
        loop {
            match self.rx.recv().await {
                Ok(update) => return Some(update),
                Err(broadcast::error::RecvError::Lagged(n)) => match self.lag_policy {
                    LagPolicy::FailClosed => {
                        warn!(skipped = n, "LiveFeed lagged; closing feed fail-closed");
                        return None;
                    }
                    LagPolicy::SkipAndContinue => {
                        warn!(
                            skipped = n,
                            "LiveFeed lagged; skipping missed updates and continuing"
                        );
                    }
                },
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LagPolicy, LiveFeed};
    use crate::traits::{Feed, MarketUpdate};
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    fn update(price: Decimal) -> MarketUpdate {
        MarketUpdate::SpotPrice {
            symbol: Arc::from("BTCUSDT"),
            price,
            ts: Utc::now(),
        }
    }

    #[tokio::test]
    async fn lagged_live_feed_closes_fail_closed() {
        let (tx, rx) = broadcast::channel(1);
        let mut feed = LiveFeed::new(rx);

        tx.send(update(Decimal::ONE)).unwrap();
        tx.send(update(Decimal::from(2))).unwrap();

        assert!(feed.next().await.is_none());
    }

    #[tokio::test]
    async fn lagged_live_feed_skip_and_continue_survives() {
        let (tx, rx) = broadcast::channel(1);
        let mut feed = LiveFeed::with_lag_policy(rx, LagPolicy::SkipAndContinue);

        tx.send(update(Decimal::ONE)).unwrap();
        tx.send(update(Decimal::from(2))).unwrap();
        tx.send(update(Decimal::from(3))).unwrap();

        // The oldest updates were overwritten; the feed must deliver the newest
        // one instead of closing.
        let Some(MarketUpdate::SpotPrice { price, .. }) = feed.next().await else {
            panic!("skip-and-continue feed must keep delivering after lag");
        };
        assert_eq!(price, Decimal::from(3));

        tx.send(update(Decimal::from(4))).unwrap();
        assert!(feed.next().await.is_some());
    }

    #[test]
    fn lag_policy_defaults_to_fail_closed() {
        assert_eq!(LagPolicy::default(), LagPolicy::FailClosed);
    }

    #[test]
    fn lag_policy_deserializes_from_config_names() {
        assert_eq!(
            serde_json::from_str::<LagPolicy>("\"fail_closed\"").unwrap(),
            LagPolicy::FailClosed
        );
        assert_eq!(
            serde_json::from_str::<LagPolicy>("\"skip_and_continue\"").unwrap(),
            LagPolicy::SkipAndContinue
        );
    }
}
