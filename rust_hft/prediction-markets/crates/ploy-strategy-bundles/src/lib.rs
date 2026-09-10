pub mod bundle;
pub mod config;
pub mod engine;
pub mod executor;
pub mod feed;
pub mod recorder;
pub mod runtime;
pub mod signals;
pub mod strategies;
pub mod traits;

pub use bundle::StrategyBundle;
pub use config::FullConfig;
pub use engine::{RuntimeConfig, RuntimeMode, RuntimeResult, StrategyRuntime};
pub use executor::{CallbackExecutor, SimulatedExecutor, SimulatedExecutorConfig};
#[cfg(feature = "parquet-feed")]
pub use feed::StreamingParquetFeed;
pub use feed::{
    HistoricalFeed, LiveFeed, RecordedFeed, RecordedFeedError, RecordedMarketUpdate, RecordingFeed,
    RecordingLimits,
};
pub use ploy_market_contracts::{Feed, InstrumentKind, MarketUpdate, PredictionFamily, VenueKind};
pub use recorder::BufferedRecorder;
pub use runtime::emit_intents;
pub use signals::{MarketSignal, SignalConfig};
pub use strategies::registry::{build_strategy, canonical_strategy_variant};
pub use strategies::BayesianDirectionalStrategy;
pub use strategies::DiffEnhancedStrategy;
pub use strategies::DiffRegularStrategy;
pub use strategies::DirectionalStrategy;
pub use strategies::MeanReversionStrategy;
pub use strategies::ProbChaseStrategy;
pub use strategies::ProbReversalStrategy;
pub use strategies::ReversalStrategy;
pub use strategies::SweepStrategy;
pub use strategies::ThreeLayerProfile;
pub use strategies::ThreeLayerStrategy;
pub use traits::{
    ExecutionPolicy, ExecutionReport, Executor, NullRecorder, Recorder, SignalRecord,
    StrategyDecision, StrategyLogic, SubmitOutcome,
};

pub const CRATE_MARKER: &str = "ploy-strategy-bundles";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}

#[cfg(test)]
pub(crate) mod canonical_test_support {
    use chrono::Utc;
    use portfolio_core::prediction::{
        FillRecord, IntentPurpose, OrderLedger, OrderState, PositionLedger, TradeSide,
        TradingIntent, TradingRuntime,
    };
    use rust_decimal::Decimal;

    pub(crate) fn order_projection(
        intent: TradingIntent,
        order_id: &str,
        state: OrderState,
        venue_order_id: &str,
        fill: Option<FillRecord>,
    ) -> OrderLedger {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(intent.clone(), order_id.to_string(), None)
            .expect("canonical test intent");
        if !matches!(state, OrderState::Pending) {
            runtime.acknowledge_order(order_id, venue_order_id);
        }
        if let Some(fill) = fill {
            runtime.record_fill(fill);
        }
        match state {
            OrderState::Unknown => {
                runtime.mark_order_unknown(order_id, "test transport ambiguity");
            }
            OrderState::Canceled => {
                runtime.cancel_order(order_id);
            }
            _ => {}
        }
        OrderLedger::restore(runtime.snapshot(&Default::default()).orders)
    }

    pub(crate) fn position_projection(
        intent: TradingIntent,
        order_id: &str,
        venue_order_id: &str,
        fills: impl IntoIterator<Item = FillRecord>,
    ) -> PositionLedger {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(intent.clone(), order_id.to_string(), None)
            .expect("canonical test intent");
        runtime.acknowledge_order(order_id, venue_order_id);
        for fill in fills {
            if fill.order_id != order_id {
                let side = fill.side;
                let purpose = if side == TradeSide::Sell {
                    IntentPurpose::Exit
                } else {
                    IntentPurpose::Entry
                };
                runtime
                    .submit_intent(
                        TradingIntent {
                            intent_id: format!("test-{}", fill.order_id),
                            deployment_id: intent.deployment_id.clone(),
                            market_id: intent.market_id.clone(),
                            token_id: fill.token_id.clone(),
                            side,
                            quantity: fill.quantity,
                            limit_price: Some(fill.price),
                            purpose,
                            created_at: fill.timestamp,
                        },
                        fill.order_id.clone(),
                        None,
                    )
                    .expect("canonical test exit intent");
                runtime.acknowledge_order(&fill.order_id, format!("venue-{}", fill.order_id));
            }
            runtime.record_fill(fill);
        }
        PositionLedger::restore(runtime.snapshot(&Default::default()).positions)
    }

    pub(crate) fn entry_intent(token_id: &str, quantity: Decimal) -> TradingIntent {
        TradingIntent {
            intent_id: format!("test-entry-{token_id}"),
            deployment_id: "test.deployment".to_string(),
            market_id: "test-market".to_string(),
            token_id: token_id.to_string(),
            side: TradeSide::Buy,
            quantity,
            limit_price: Some(Decimal::new(99, 2)),
            purpose: IntentPurpose::Entry,
            created_at: Utc::now(),
        }
    }
}
