//! Monday-native execution-client seam for the transitional prediction runtime.
//!
//! The prediction module does not own a venue client.  It consumes the shared
//! `hft-ports::ExecutionClient` contract and production construction remains
//! disabled until the reviewed Monday runtime supplies an admitted client.

use async_trait::async_trait;
use hft_core::{HftError, HftResult, OrderId};
pub use ports::ExecutionClient;
use ports::{
    AccountBalance, AccountFill, BoxStream, ConnectionHealth, ExecutionEvent, OpenOrder,
    OrderIntent, Position,
};
use std::sync::Arc;
use tokio::sync::{Mutex, MutexGuard};

pub const MONDAY_EXECUTION_DISABLED: &str =
    "prediction-market canonical execution is disabled; Monday runtime is the only production execution authority";

pub type SharedExecutionClient = Arc<Mutex<Box<dyn ExecutionClient>>>;

#[derive(Debug, Clone, Copy, Default)]
pub struct DisabledExecutionClient;

fn disabled_error() -> HftError {
    HftError::Config(MONDAY_EXECUTION_DISABLED.to_string())
}

#[async_trait]
impl ExecutionClient for DisabledExecutionClient {
    async fn place_order(&mut self, _intent: OrderIntent) -> HftResult<OrderId> {
        Err(disabled_error())
    }

    async fn cancel_order(&mut self, _order_id: &OrderId) -> HftResult<()> {
        Err(disabled_error())
    }

    async fn modify_order(
        &mut self,
        _order_id: &OrderId,
        _new_quantity: Option<hft_core::Quantity>,
        _new_price: Option<hft_core::Price>,
    ) -> HftResult<()> {
        Err(disabled_error())
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        Err(disabled_error())
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        Err(disabled_error())
    }

    async fn get_balance(&self) -> HftResult<Vec<AccountBalance>> {
        Err(disabled_error())
    }

    async fn get_positions(&self) -> HftResult<Vec<Position>> {
        Err(disabled_error())
    }

    async fn list_recent_fills(&self) -> HftResult<Vec<AccountFill>> {
        Err(disabled_error())
    }

    async fn connect(&mut self) -> HftResult<()> {
        Err(disabled_error())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        Ok(())
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: false,
            latency_ms: None,
            last_heartbeat: 0,
        }
    }
}

#[must_use]
pub fn disabled_execution_client() -> SharedExecutionClient {
    Arc::new(Mutex::new(Box::new(DisabledExecutionClient)))
}

pub async fn lock_execution_client(
    client: &SharedExecutionClient,
) -> std::io::Result<MutexGuard<'_, Box<dyn ExecutionClient>>> {
    Ok(client.lock().await)
}

pub fn execution_io_error(error: HftError) -> std::io::Error {
    let kind = match error {
        HftError::Config(_) | HftError::InvalidOrder(_) | HftError::SubmissionNotAttempted(_) => {
            std::io::ErrorKind::InvalidInput
        }
        HftError::Network(_)
        | HftError::Timeout(_)
        | HftError::Io { .. }
        | HftError::Exchange(_) => std::io::ErrorKind::ConnectionAborted,
        _ => std::io::ErrorKind::Other,
    };
    std::io::Error::new(kind, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderType, Price, Quantity, Side, Symbol, TimeInForce, VenueId};
    use ports::{OrderIntentEnvelope, OrderIntentLifecycle};
    use rust_decimal::Decimal;

    fn assert_disabled<T>(result: HftResult<T>) {
        match result {
            Err(HftError::Config(message)) if message == MONDAY_EXECUTION_DISABLED => {}
            Err(error) => panic!("expected the disabled execution error, got {error}"),
            Ok(_) => panic!("disabled execution unexpectedly succeeded"),
        }
    }

    #[tokio::test]
    async fn production_client_rejects_orders_and_authoritative_account_reads() {
        let shared = disabled_execution_client();
        let mut client = lock_execution_client(&shared).await.unwrap();
        let intent = OrderIntent::prediction_market(
            Symbol::new("test-yes-token"),
            Side::Buy,
            Quantity(Decimal::ONE),
            OrderType::Limit,
            Some(Price(Decimal::new(5, 1))),
            TimeInForce::GTC,
            "disabled-client-regression".to_string(),
            VenueId::POLYMARKET,
        );
        let now = hft_core::now_micros();
        let envelope = OrderIntentEnvelope::new(
            intent.clone(),
            OrderIntentLifecycle::new(now, now + 60_000_000),
        );
        envelope.validate_cex_pre_execution(now, None).unwrap();
        let order_id = OrderId("existing-order".to_string());

        assert_disabled(client.connect().await);
        assert_disabled(client.place_order(intent.clone()).await);
        assert_disabled(client.place_order_with_spec(intent, None).await);
        assert_disabled(client.place_order_envelope(&envelope).await);
        let attempt = client.place_order_envelope_traced(&envelope).await;
        assert_disabled(attempt.outcome);
        assert!(attempt.userspace_write_started_mono_us.is_none());
        assert!(attempt.userspace_write_returned_mono_us.is_none());
        assert!(attempt.response_received_mono_us.is_none());
        assert_disabled(client.cancel_order(&order_id).await);
        assert_disabled(client.modify_order(&order_id, None, None).await);
        assert_disabled(client.execution_stream().await);
        assert_disabled(client.list_open_orders().await);
        assert_disabled(client.get_balance().await);
        assert_disabled(client.get_positions().await);
        assert_disabled(client.list_recent_fills().await);
        assert!(!client.health().await.connected);
        assert!(!client.is_simulated_execution());
        assert!(!client.execution_stream_may_complete());
        assert!(!client.supports_position_snapshot());
        assert!(!client.supports_recent_fills_snapshot());
        client.disconnect().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execution_client_lock_is_async() {
        let started = std::time::Instant::now();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        assert!(started.elapsed() >= std::time::Duration::from_millis(2));
    }
}
