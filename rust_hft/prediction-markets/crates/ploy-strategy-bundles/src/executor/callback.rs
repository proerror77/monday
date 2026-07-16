//! Callback-based real exchange executor.
//!
//! Wraps a user-provided async function for order submission.
//! This keeps `ploy-strategy-bundles` free of exchange-specific
//! dependencies — the caller wires in their own exchange client.
//!
//! ```rust,ignore
//! let executor = CallbackExecutor::new(|intent| async move {
//!     let response = polymarket_client.place_order(&intent).await?;
//!     Ok(ExecutionReport { ... })
//! });
//! ```

use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;
use ploy_trading::TradingIntent;

use crate::traits::{ExecutionReport, Executor};

/// Type alias for the async submit callback.
pub type SubmitFn = Box<
    dyn Fn(TradingIntent) -> Pin<Box<dyn Future<Output = ExecutionReport> + Send>> + Send + Sync,
>;

/// Executor backed by a user-supplied async callback.
///
/// Use this for live trading by providing a closure that calls
/// the exchange API and returns an `ExecutionReport`.
pub struct CallbackExecutor {
    submit_fn: SubmitFn,
}

impl CallbackExecutor {
    /// Create a new callback executor.
    ///
    /// The `submit_fn` receives a `TradingIntent` and must return
    /// an `ExecutionReport` with fill details or rejection info.
    pub fn new(submit_fn: SubmitFn) -> Self {
        Self { submit_fn }
    }
}

#[async_trait]
impl Executor for CallbackExecutor {
    async fn submit(&mut self, intent: &TradingIntent, _order_id: &str) -> ExecutionReport {
        (self.submit_fn)(intent.clone()).await
    }

    async fn cancel(&mut self, _order_id: &str) -> bool {
        // TODO: add cancel callback when needed
        false
    }
}
