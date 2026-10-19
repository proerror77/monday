//! Live data feed backed by a tokio broadcast channel.
//!
//! Used for both dry-run and live trading. The feed blocks on
//! `recv()` until the next market update arrives from the WebSocket
//! adapters.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tracing::warn;

use crate::traits::{Feed, MarketUpdate};

tokio::task_local! {
    // RecordingFeed scopes this counter to one inner poll. It also works through
    // Box<dyn Feed> without changing the shared market-data Feed contract.
    static RECORDING_SKIPPED_UPDATES: Arc<AtomicU64>;
}

pub(super) async fn capture_recording_lag<F: Future>(
    skipped_updates: Arc<AtomicU64>,
    future: F,
) -> F::Output {
    RECORDING_SKIPPED_UPDATES
        .scope(skipped_updates, future)
        .await
}

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
    skipped_total: u64,
    lag_events_total: u64,
    pending_skipped: u64,
    last_lag_report: Option<Instant>,
}

impl LiveFeed {
    /// Create a live feed from a broadcast receiver.
    pub fn new(rx: broadcast::Receiver<MarketUpdate>) -> Self {
        Self::with_lag_policy(rx, LagPolicy::default())
    }

    /// Create a live feed with an explicit lag policy.
    pub fn with_lag_policy(rx: broadcast::Receiver<MarketUpdate>, lag_policy: LagPolicy) -> Self {
        Self {
            rx,
            lag_policy,
            skipped_total: 0,
            lag_events_total: 0,
            pending_skipped: 0,
            last_lag_report: None,
        }
    }

    fn report_lag(&mut self, force: bool) {
        if self.pending_skipped == 0
            || (!force
                && self
                    .last_lag_report
                    .is_some_and(|last| last.elapsed() < Duration::from_secs(1)))
        {
            return;
        }
        warn!(
            skipped = self.pending_skipped,
            skipped_total = self.skipped_total,
            lag_events_total = self.lag_events_total,
            "LiveFeed lagged; skipping missed updates and continuing"
        );
        self.pending_skipped = 0;
        self.last_lag_report = Some(Instant::now());
    }
}

#[async_trait]
impl Feed for LiveFeed {
    async fn next(&mut self) -> Option<MarketUpdate> {
        loop {
            match self.rx.recv().await {
                Ok(update) => {
                    self.report_lag(false);
                    return Some(update);
                }
                Err(broadcast::error::RecvError::Lagged(n)) => match self.lag_policy {
                    LagPolicy::FailClosed => {
                        warn!(skipped = n, "LiveFeed lagged; closing feed fail-closed");
                        return None;
                    }
                    LagPolicy::SkipAndContinue => {
                        let _ = RECORDING_SKIPPED_UPDATES.try_with(|skipped| {
                            let _ = skipped.fetch_update(
                                Ordering::Relaxed,
                                Ordering::Relaxed,
                                |total| Some(total.saturating_add(n)),
                            );
                        });
                        self.skipped_total = self.skipped_total.saturating_add(n);
                        self.lag_events_total = self.lag_events_total.saturating_add(1);
                        self.pending_skipped = self.pending_skipped.saturating_add(n);
                        self.report_lag(false);
                    }
                },
                Err(broadcast::error::RecvError::Closed) => {
                    self.report_lag(true);
                    return None;
                }
            }
        }
    }
}

impl Drop for LiveFeed {
    fn drop(&mut self) {
        self.report_lag(true);
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
        assert_eq!(feed.skipped_total, 2);
        assert_eq!(feed.lag_events_total, 1);

        tx.send(update(Decimal::from(4))).unwrap();
        assert!(feed.next().await.is_some());
    }

    #[tokio::test]
    async fn burst_lag_accounting_keeps_all_drops_when_reports_are_coalesced() {
        let (tx, rx) = broadcast::channel(1);
        let mut feed = LiveFeed::with_lag_policy(rx, LagPolicy::SkipAndContinue);
        tx.send(update(Decimal::ONE)).unwrap();
        tx.send(update(Decimal::from(2))).unwrap();
        assert!(feed.next().await.is_some());
        for n in 3..=5 {
            tx.send(update(Decimal::from(n))).unwrap();
        }
        assert!(feed.next().await.is_some());
        assert_eq!(feed.skipped_total + 2, 5);
        assert_eq!(feed.lag_events_total, 2);
        assert_eq!(feed.pending_skipped, 2);
        drop(tx);
        assert!(feed.next().await.is_none());
        assert_eq!(feed.pending_skipped, 0);
        assert_eq!(feed.skipped_total, 3);
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
