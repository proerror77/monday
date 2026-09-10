pub mod config;
pub mod events;
pub mod http;
pub mod reports;
pub mod runtime;

#[cfg(test)]
pub(crate) mod test_support {
    use async_trait::async_trait;
    use hft_core::{HftError, OrderId};
    use portfolio_core::prediction::FillRecord;
    use ports::{
        AccountBalance, AccountFill, BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent,
        OpenOrder, OrderIntent, Position,
    };

    #[derive(Debug, Clone)]
    pub(crate) struct StaticExecutionGateway {
        submit_result: Result<String, HftError>,
        cancel_result: Result<(), HftError>,
        replace_result: Result<(), HftError>,
        fills: Vec<AccountFill>,
    }

    impl StaticExecutionGateway {
        pub(crate) fn acknowledged(venue_order_id: impl Into<String>) -> Self {
            let venue_order_id = venue_order_id.into();
            Self {
                submit_result: Ok(venue_order_id),
                cancel_result: Ok(()),
                replace_result: Ok(()),
                fills: Vec::new(),
            }
        }

        pub(crate) fn rejected(reason: impl Into<String>) -> Self {
            let reason = reason.into();
            Self {
                submit_result: Err(HftError::SubmissionNotAttempted(reason.clone())),
                cancel_result: Ok(()),
                replace_result: Err(HftError::SubmissionNotAttempted(reason)),
                fills: Vec::new(),
            }
        }

        pub(crate) fn failed(error: HftError) -> Self {
            Self {
                submit_result: Err(error.clone()),
                cancel_result: Err(error.clone()),
                replace_result: Err(error),
                fills: Vec::new(),
            }
        }

        pub(crate) fn with_probe_result(self, _result: Result<(), HftError>) -> Self {
            self
        }

        pub(crate) fn with_cancel_result(mut self, result: Result<(), HftError>) -> Self {
            self.cancel_result = result;
            self
        }

        pub(crate) fn with_replace_result(mut self, result: Result<(), HftError>) -> Self {
            self.replace_result = result;
            self
        }

        pub(crate) fn with_reconciled_fills(mut self, fills: Vec<FillRecord>) -> Self {
            self.fills = fills
                .into_iter()
                .map(|fill| AccountFill {
                    fill_id: fill.fill_id,
                    order_id: OrderId(fill.order_id),
                    symbol: hft_core::Symbol::new(fill.token_id),
                    side: match fill.side {
                        portfolio_core::prediction::TradeSide::Buy => hft_core::Side::Buy,
                        portfolio_core::prediction::TradeSide::Sell => hft_core::Side::Sell,
                    },
                    price: hft_core::Price(fill.price),
                    quantity: hft_core::Quantity(fill.quantity),
                    fee: Some(fill.fee),
                    timestamp: fill.timestamp.timestamp_micros().max(0) as u64,
                })
                .collect();
            self
        }
    }

    #[async_trait]
    impl ExecutionClient for StaticExecutionGateway {
        async fn place_order(&mut self, _intent: OrderIntent) -> Result<OrderId, HftError> {
            self.submit_result.clone().map(OrderId)
        }
        async fn cancel_order(&mut self, _order_id: &OrderId) -> Result<(), HftError> {
            self.cancel_result.clone()
        }
        async fn modify_order(
            &mut self,
            _order_id: &OrderId,
            _new_quantity: Option<hft_core::Quantity>,
            _new_price: Option<hft_core::Price>,
        ) -> Result<(), HftError> {
            self.replace_result.clone()
        }
        async fn execution_stream(&self) -> Result<BoxStream<ExecutionEvent>, HftError> {
            Err(HftError::Config(
                "test execution stream unavailable".to_string(),
            ))
        }
        async fn list_open_orders(&self) -> Result<Vec<OpenOrder>, HftError> {
            Ok(Vec::new())
        }
        async fn list_recent_fills(&self) -> Result<Vec<AccountFill>, HftError> {
            Ok(self.fills.clone())
        }
        async fn get_balance(&self) -> Result<Vec<AccountBalance>, HftError> {
            Ok(Vec::new())
        }
        async fn get_positions(&self) -> Result<Vec<Position>, HftError> {
            Ok(Vec::new())
        }
        async fn connect(&mut self) -> Result<(), HftError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), HftError> {
            Ok(())
        }
        async fn health(&self) -> ConnectionHealth {
            ConnectionHealth {
                connected: true,
                latency_ms: Some(0.0),
                last_heartbeat: 0,
            }
        }
    }
}
