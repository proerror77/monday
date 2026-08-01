use ploy_market_data::binance_collectors::spawn_binance_tick_feed;
use ploy_market_data::feeds::{
    spawn_chainlink_feed, spawn_db_aggtrade_feed, spawn_db_l2_feed, spawn_db_polymarket_feed,
    spawn_db_spot_feed, spawn_pyth_reference_feed,
};
use ploy_market_data::reference_prices::new_reference_price_registry;
use ploy_market_data::scanner::spawn_market_scanner;
use ploy_market_data::sports_feed::spawn_sports_feed;
use ploy_strategy_bundles::config::MarketDataSource;
use ploy_strategy_bundles::{
    Feed, FullConfig, LiveFeed, RecordingFeed, RuntimeMode, StrategyLogic,
};
#[cfg(feature = "live-execution")]
use ploy_trading::TradingRuntime;
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

fn uses_db_primary_ticks(source: MarketDataSource) -> bool {
    source.uses_local_db() && !source.uses_external_direct()
}

#[cfg(all(feature = "live", feature = "live-execution"))]
mod execution {
    use async_trait::async_trait;
    use ploy_control_client::ControlPlaneClient;
    use ploy_operator_contracts::{IntentPurpose, PaperIntentRequest};
    use ploy_strategy_bundles::{ExecutionPolicy, ExecutionReport};
    use ploy_trading::{FillRecord, TradeSide, TradingIntent};
    use rust_decimal::Decimal;
    use std::collections::{BTreeMap, BTreeSet};
    use tracing::{debug, error};

    pub(super) struct LiveExecutor {
        client: ControlPlaneClient,
        policy: ExecutionPolicy,
        deployment_id: String,
        seen_fill_ids: BTreeSet<String>,
        last_reconcile_attempted: bool,
        canonical_to_local_order: BTreeMap<String, String>,
    }

    impl LiveExecutor {
        pub(super) fn new(
            client: ControlPlaneClient,
            policy: ExecutionPolicy,
            deployment_id: impl Into<String>,
        ) -> Self {
            Self {
                client,
                policy,
                deployment_id: deployment_id.into(),
                seen_fill_ids: BTreeSet::new(),
                last_reconcile_attempted: false,
                canonical_to_local_order: BTreeMap::new(),
            }
        }

        fn request(intent: &TradingIntent, idempotency_key: &str) -> PaperIntentRequest {
            PaperIntentRequest {
                idempotency_key: Some(idempotency_key.to_string()),
                market_id: intent.market_id.clone(),
                token_id: intent.token_id.clone(),
                side: match intent.side {
                    TradeSide::Buy => "buy",
                    TradeSide::Sell => "sell",
                }
                .to_string(),
                quantity: intent.quantity,
                limit_price: intent.limit_price,
                purpose: match intent.purpose {
                    ploy_trading::IntentPurpose::Entry => IntentPurpose::Entry,
                    ploy_trading::IntentPurpose::Exit => IntentPurpose::Exit,
                    ploy_trading::IntentPurpose::Reduce => IntentPurpose::Reduce,
                    ploy_trading::IntentPurpose::Hedge => IntentPurpose::Hedge,
                    ploy_trading::IntentPurpose::Cancel => IntentPurpose::Cancel,
                },
            }
        }

        fn prepared_quantity_and_price(
            &self,
            intent: &TradingIntent,
        ) -> (Decimal, Option<Decimal>) {
            let normalized_quantity = intent.quantity.trunc_with_scale(2);
            let Some(limit_price) = intent.limit_price else {
                return (normalized_quantity, None);
            };

            let execution_price = self.slippage_bounded_price(limit_price, intent.side);
            if intent.side != TradeSide::Buy {
                return (normalized_quantity, Some(execution_price));
            }

            if limit_price <= Decimal::ZERO || normalized_quantity <= Decimal::ZERO {
                return (normalized_quantity, Some(execution_price));
            }

            let target_notional = (intent.quantity * limit_price).trunc_with_scale(6);
            if target_notional <= Decimal::ZERO {
                return (normalized_quantity, Some(execution_price));
            }

            let capped_quantity = (target_notional / execution_price).trunc_with_scale(2);
            let prepared_quantity = normalized_quantity.min(capped_quantity);
            if prepared_quantity < normalized_quantity {
                debug!(
                    intent_id = %intent.intent_id,
                    original_quantity = %intent.quantity,
                    prepared_quantity = %prepared_quantity,
                    limit_price = %limit_price,
                    execution_price = %execution_price,
                    target_notional = %target_notional,
                    "Capped live BUY quantity to strategy notional"
                );
            }

            (prepared_quantity, Some(execution_price))
        }

        fn slippage_bounded_price(&self, limit_price: Decimal, side: TradeSide) -> Decimal {
            let tolerance = self.policy.max_slippage_bps / Decimal::from(10_000_u32);
            let adjusted = match side {
                TradeSide::Buy => limit_price * (Decimal::ONE + tolerance),
                TradeSide::Sell => limit_price * (Decimal::ONE - tolerance),
            };
            adjusted
                .clamp(Decimal::new(1, 2), Decimal::new(99, 2))
                .round_dp(2)
        }
    }

    #[async_trait]
    impl ploy_strategy_bundles::Executor for LiveExecutor {
        fn execution_policy(&self) -> ExecutionPolicy {
            self.policy
        }

        fn last_reconcile_attempted(&self) -> bool {
            self.last_reconcile_attempted
        }

        fn owns_live_retries(&self) -> bool {
            false
        }

        fn prepare_intent(&self, intent: &TradingIntent) -> TradingIntent {
            let mut prepared = intent.clone();
            let (quantity, limit_price) = self.prepared_quantity_and_price(intent);
            prepared.quantity = quantity;
            prepared.limit_price = limit_price;
            prepared
        }

        async fn submit(&mut self, intent: &TradingIntent, order_id: &str) -> ExecutionReport {
            let client = self.client.clone();
            let deployment_id = intent.deployment_id.clone();
            let request = Self::request(intent, &intent.intent_id);
            match tokio::task::spawn_blocking(move || {
                client.submit_worker_intent(&deployment_id, &request)
            })
            .await
            {
                Ok(Ok(response)) if response.deployment_id != intent.deployment_id => {
                    ExecutionReport {
                        order_id: String::new(),
                        fill: None,
                        rejected: false,
                        rejection_reason: Some(format!(
                            "idempotent replay is owned by deployment `{}`",
                            response.deployment_id
                        )),
                        slippage: None,
                        market_impact: None,
                        price_basis: None,
                    }
                }
                Ok(Ok(response)) if response.state == "rejected" => ExecutionReport {
                    order_id: response.order_id,
                    fill: None,
                    rejected: true,
                    rejection_reason: response.rejection_reason.or(response.last_error),
                    slippage: None,
                    market_impact: None,
                    price_basis: None,
                },
                Ok(Ok(response)) if response.state == "unknown" => ExecutionReport {
                    order_id: String::new(),
                    fill: None,
                    rejected: false,
                    rejection_reason: response
                        .last_error
                        .or(Some("submission outcome unknown".to_string())),
                    slippage: None,
                    market_impact: None,
                    price_basis: None,
                },
                Ok(Ok(response))
                    if matches!(
                        response.state.as_str(),
                        "acknowledged" | "partially_filled" | "filled"
                    ) =>
                {
                    let Some(venue_order_id) = response.venue_order_id else {
                        return ExecutionReport {
                            order_id: String::new(),
                            fill: None,
                            rejected: false,
                            rejection_reason: Some(
                                "acknowledged response missing venue_order_id".to_string(),
                            ),
                            slippage: None,
                            market_impact: None,
                            price_basis: None,
                        };
                    };
                    self.canonical_to_local_order
                        .insert(response.order_id.clone(), order_id.to_string());
                    ExecutionReport {
                        order_id: venue_order_id,
                        fill: None,
                        rejected: false,
                        rejection_reason: None,
                        slippage: None,
                        market_impact: None,
                        price_basis: None,
                    }
                }
                Ok(Ok(response)) => ExecutionReport {
                    order_id: String::new(),
                    fill: None,
                    rejected: false,
                    rejection_reason: Some(format!(
                        "unsupported control-plane submit state `{}`",
                        response.state
                    )),
                    slippage: None,
                    market_impact: None,
                    price_basis: None,
                },
                Ok(Err(error)) => {
                    error!(%error, "Control-plane submission outcome is unknown");
                    ExecutionReport {
                        order_id: String::new(),
                        fill: None,
                        rejected: false,
                        rejection_reason: Some(error),
                        slippage: None,
                        market_impact: None,
                        price_basis: None,
                    }
                }
                Err(error) => ExecutionReport {
                    order_id: String::new(),
                    fill: None,
                    rejected: false,
                    rejection_reason: Some(format!("control-plane task failed: {error}")),
                    slippage: None,
                    market_impact: None,
                    price_basis: None,
                },
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            false
        }

        async fn reconcile_fills(
            &mut self,
            _orders: &ploy_trading::OrderLedger,
        ) -> Result<Vec<FillRecord>, String> {
            let client = self.client.clone();
            let deployment_id = self.deployment_id.clone();
            let snapshot =
                tokio::task::spawn_blocking(move || client.inspect_trading_state(&deployment_id))
                    .await
                    .map_err(|error| format!("control-plane reconcile task failed: {error}"))??;
            self.last_reconcile_attempted = true;
            snapshot
                .fills
                .into_iter()
                .filter(|fill| self.seen_fill_ids.insert(fill.fill_id.clone()))
                .map(|fill| {
                    Ok(FillRecord {
                        fill_id: fill.fill_id,
                        order_id: self
                            .canonical_to_local_order
                            .get(&fill.order_id)
                            .cloned()
                            .unwrap_or(fill.order_id),
                        token_id: fill.token_id,
                        side: match fill.side.as_str() {
                            "buy" => TradeSide::Buy,
                            "sell" => TradeSide::Sell,
                            side => return Err(format!("unsupported fill side `{side}`")),
                        },
                        quantity: fill.quantity,
                        price: fill.price,
                        fee: fill.fee,
                        timestamp: fill.timestamp,
                    })
                })
                .collect()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use chrono::Utc;
        use ploy_operator_contracts::{DeploymentRuntimeMode, FillSnapshot, TradingStateSnapshot};
        use ploy_strategy_bundles::Executor;
        use ploy_trading::IntentPurpose;
        use std::fs;
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn test_executor(max_slippage_bps: Decimal) -> LiveExecutor {
            LiveExecutor::new(
                ControlPlaneClient::default(),
                ExecutionPolicy {
                    max_slippage_bps,
                    max_attempts: 2,
                    reconcile_cycles_before_retry: 2,
                },
                "deployment",
            )
        }

        #[test]
        fn build_live_executor_scopes_worker_credentials() {
            let mut client = ControlPlaneClient::default();
            client.admin_token = Some("admin-token".to_string());
            client.operator_token = Some("operator-token".to_string());
            client.sidecar_token = Some("sidecar-token".to_string());

            let executor =
                super::super::build_live_executor(client, ExecutionPolicy::default(), "deployment");

            assert!(executor.client.admin_token.is_none());
            assert!(executor.client.operator_token.is_none());
            assert!(executor.client.sidecar_token.is_none());
        }

        fn buy_intent(quantity: Decimal, limit_price: Decimal) -> TradingIntent {
            TradingIntent {
                intent_id: "intent-buy".into(),
                deployment_id: "deployment".into(),
                market_id: "event".into(),
                token_id: "token".into(),
                side: TradeSide::Buy,
                quantity,
                limit_price: Some(limit_price),
                purpose: IntentPurpose::Entry,
                created_at: Utc::now(),
            }
        }

        async fn submit_response(
            deployment_id: &str,
            state: &str,
            venue_order_id: Option<&str>,
        ) -> (LiveExecutor, ExecutionReport) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let body = serde_json::json!({
                "deployment_id": deployment_id,
                "intent_id": "intent-buy",
                "order_id": "canonical-order-1",
                "state": state,
                "venue_order_id": venue_order_id,
                "rejection_reason": null,
                "last_error": null
            })
            .to_string();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            });
            let mut client = ControlPlaneClient::default();
            client.control_plane_addr = addr.to_string();
            let mut executor = LiveExecutor::new(client, ExecutionPolicy::default(), "deployment");
            let report = executor
                .submit(&buy_intent(Decimal::ONE, Decimal::new(40, 2)), "local-1")
                .await;
            (executor, report)
        }

        #[tokio::test]
        async fn cross_deployment_replay_does_not_create_local_order_mapping() {
            let (executor, report) =
                submit_response("other.live", "acknowledged", Some("venue-1")).await;
            assert_eq!(
                report.submit_outcome(),
                ploy_strategy_bundles::SubmitOutcome::Unknown
            );
            assert!(executor.canonical_to_local_order.is_empty());
        }

        #[tokio::test]
        async fn unsupported_submit_states_fail_closed_without_venue_mapping() {
            for state in ["pending", "acknowleged", "future_state"] {
                let (executor, report) =
                    submit_response("deployment", state, Some("venue-1")).await;
                assert_eq!(
                    report.submit_outcome(),
                    ploy_strategy_bundles::SubmitOutcome::Unknown
                );
                assert!(report.order_id.is_empty());
                assert!(executor.canonical_to_local_order.is_empty());
            }
        }

        #[tokio::test]
        async fn control_plane_reconcile_propagates_only_incremental_fills() {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("ploy-live-fills-{unique}"));
            fs::create_dir_all(&root).expect("runtime root");
            let snapshot_path = root.join("trading-state.json");
            let write_fills = |fills: Vec<FillSnapshot>| {
                fs::write(
                    &snapshot_path,
                    serde_json::to_vec(&vec![TradingStateSnapshot {
                        deployment_id: "deployment".to_string(),
                        runtime_mode: DeploymentRuntimeMode::Live,
                        fills,
                        ..TradingStateSnapshot::default()
                    }])
                    .expect("snapshot json"),
                )
                .expect("write snapshot");
            };
            let fill = |id: &str| FillSnapshot {
                fill_id: id.to_string(),
                order_id: "order-1".to_string(),
                token_id: "token-1".to_string(),
                side: "buy".to_string(),
                quantity: Decimal::ONE,
                price: Decimal::new(40, 2),
                fee: Decimal::ZERO,
                timestamp: Utc::now(),
            };
            write_fills(vec![fill("fill-1")]);
            let mut client = ControlPlaneClient::from_runtime_root(root);
            client.control_plane_addr = "127.0.0.1:9".to_string();
            let mut executor = LiveExecutor::new(client, ExecutionPolicy::default(), "deployment");
            let orders = ploy_trading::OrderLedger::default();

            assert_eq!(executor.reconcile_fills(&orders).await.unwrap().len(), 1);
            assert!(executor.reconcile_fills(&orders).await.unwrap().is_empty());
            write_fills(vec![fill("fill-1"), fill("fill-2")]);
            let incremental = executor.reconcile_fills(&orders).await.unwrap();
            assert_eq!(incremental.len(), 1);
            assert_eq!(incremental[0].fill_id, "fill-2");
            assert!(executor.last_reconcile_attempted());
        }

        #[test]
        fn prepare_intent_caps_buy_quantity_to_slippage_bounded_notional() {
            let executor = test_executor(Decimal::new(150, 0));
            let intent = buy_intent(Decimal::new(142_857_143, 6), Decimal::new(105, 3));

            let prepared = executor.prepare_intent(&intent);
            let execution_price = prepared.limit_price.expect("bounded live price");

            assert_eq!(prepared.limit_price, Some(Decimal::new(11, 2)));
            assert_eq!(execution_price, Decimal::new(11, 2));
            assert_eq!(prepared.quantity, Decimal::new(13_636, 2));
            assert!(prepared.quantity * execution_price <= Decimal::new(1_500, 2));
        }

        #[test]
        fn prepare_intent_keeps_buy_quantity_when_slippage_price_does_not_raise_notional() {
            let executor = test_executor(Decimal::new(150, 0));
            let intent = buy_intent(Decimal::new(150, 0), Decimal::new(10, 2));

            let prepared = executor.prepare_intent(&intent);

            assert_eq!(prepared.limit_price, Some(Decimal::new(10, 2)));
            assert_eq!(prepared.quantity, Decimal::new(15_000, 2));
        }
    }
}

use crate::recording::build_signal_recorder;
use crate::{database_unavailable_is_fatal, RuntimeModeConfig};

pub(crate) async fn run_live_or_dry_run_entry(
    config: &FullConfig,
    symbols: &[String],
    strategy: Box<dyn StrategyLogic>,
    runtime_config: RuntimeModeConfig,
    deployment_id: String,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    run_live_or_dry_run(config, symbols, strategy, runtime_config, deployment_id).await
}

#[cfg(feature = "live-execution")]
fn restore_live_trading_runtime(
    client: &ploy_control_client::ControlPlaneClient,
    deployment_id: &str,
) -> Result<TradingRuntime, String> {
    let snapshot = client.inspect_trading_state(deployment_id)?;
    if snapshot.deployment_id != deployment_id {
        return Err(format!(
            "control plane returned deployment `{}` while restoring `{deployment_id}`",
            snapshot.deployment_id
        ));
    }
    ploy_platform_runtime::restore_trading_runtime(snapshot)
        .map_err(|error| format!("restore trading state: {error}"))
}

async fn run_live_or_dry_run(
    config: &FullConfig,
    symbols: &[String],
    strategy: Box<dyn StrategyLogic>,
    runtime_config: RuntimeModeConfig,
    deployment_id: String,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    let db_url = env::var("DATABASE_URL").ok();
    let db_pool: Option<sqlx::PgPool> = match db_url.as_deref() {
        Some(url) => match PgPoolOptions::new().max_connections(5).connect(url).await {
            Ok(pool) => {
                info!("DB connected — market metadata and quotes will be persisted");
                Some(pool)
            }
            Err(error) => {
                if database_unavailable_is_fatal(runtime_config.mode, true) {
                    error!(
                        error = %error,
                        "DB connection failed for configured runtime; refusing to start without persistence"
                    );
                    std::process::exit(1);
                }

                warn!(error = %error, "DB connection failed; running without persistence");
                None
            }
        },
        None => {
            if database_unavailable_is_fatal(runtime_config.mode, false) {
                error!(
                    "DATABASE_URL not set for live runtime; refusing to start without persistence"
                );
                std::process::exit(1);
            }
            info!("DATABASE_URL not set — running without DB persistence");
            None
        }
    };

    let (tx, rx) = broadcast::channel(config.feed_broadcast_capacity());
    let tx = Arc::new(tx);
    let reference_prices = new_reference_price_registry();
    let market_data_source = config.runtime.market_data_source;

    if market_data_source.uses_local_db() && db_pool.is_none() {
        error!(
            source = ?market_data_source,
            "Local market-data source requires DATABASE_URL; refusing to open direct public feeds"
        );
        std::process::exit(1);
    }

    let mut feed_handles = Vec::new();
    if market_data_source.uses_local_db() {
        if let Some(ref db) = db_pool {
            if uses_db_primary_ticks(market_data_source) {
                feed_handles.push(spawn_db_spot_feed(tx.clone(), symbols.to_vec(), db.clone()));
                feed_handles.push(spawn_db_polymarket_feed(
                    tx.clone(),
                    symbols.to_vec(),
                    db.clone(),
                ));
                feed_handles.push(spawn_db_aggtrade_feed(
                    tx.clone(),
                    symbols.to_vec(),
                    db.clone(),
                ));
                feed_handles.push(spawn_db_l2_feed(tx.clone(), symbols.to_vec(), db.clone()));
            }
        }
    }

    if market_data_source.uses_external_direct() {
        feed_handles.push(spawn_binance_tick_feed(
            tx.clone(),
            reference_prices.clone(),
            symbols.to_vec(),
            20,
        ));
        feed_handles.push(spawn_chainlink_feed(
            tx.clone(),
            reference_prices.clone(),
            symbols.to_vec(),
            db_pool.clone(),
        ));
        feed_handles.push(spawn_pyth_reference_feed(
            tx.clone(),
            reference_prices.clone(),
            config.reference_data.pyth_symbols.clone(),
            db_pool.clone(),
        ));
        feed_handles.push(spawn_market_scanner(
            tx.clone(),
            reference_prices.clone(),
            symbols.to_vec(),
            db_pool.clone(),
            config.reference_data.capture_sports_state,
        ));

        if config.reference_data.capture_sports_state {
            feed_handles.push(spawn_sports_feed(tx.clone(), db_pool.clone()));
        }
    } else if config.reference_data.capture_sports_state {
        warn!(
            "capture_sports_state requires market_data_source = external_direct or dual; local_db runtime will not open the sports WebSocket"
        );
    }

    let feed: Box<dyn Feed> = if let Some(record_path) = config.record_market_updates_path() {
        Box::new(
            RecordingFeed::with_policy(
                LiveFeed::with_lag_policy(rx, config.feed_lag_policy()),
                record_path,
                config.record_market_updates_policy(),
            )
            .unwrap_or_else(|error| {
                eprintln!(
                    "Failed to open market-update log {}: {error}",
                    record_path.display()
                );
                std::process::exit(1);
            }),
        )
    } else {
        Box::new(LiveFeed::with_lag_policy(rx, config.feed_lag_policy()))
    };

    let recorder = build_signal_recorder(db_pool.clone(), runtime_config.mode);
    let result = if runtime_config.mode == RuntimeMode::Live {
        #[cfg(not(feature = "live-execution"))]
        {
            eprintln!("Live execution requires the `live-execution` feature");
            std::process::exit(1);
        }

        #[cfg(feature = "live-execution")]
        {
            let client = ploy_control_client::ControlPlaneClient::default();
            let trading =
                restore_live_trading_runtime(&client, &deployment_id).unwrap_or_else(|error| {
                    error!(%error, %deployment_id, "Live restore failed; refusing to start empty");
                    std::process::exit(1);
                });
            let executor =
                build_live_executor(client, config.live_execution_policy(), &deployment_id);
            let mut runtime = ploy_strategy_bundles::StrategyRuntime::new_with_trading(
                strategy,
                feed,
                executor,
                recorder,
                runtime_config,
                trading,
            )
            .with_deployment_id(deployment_id.clone());
            let result = runtime.run().await;
            let snapshot = runtime
                .trading()
                .snapshot(&std::collections::BTreeMap::new());
            (result, snapshot)
        }
    } else {
        let executor = ploy_strategy_bundles::SimulatedExecutor::new(config.sim_executor_config());
        let mut runtime = ploy_strategy_bundles::StrategyRuntime::new(
            strategy,
            feed,
            executor,
            recorder,
            runtime_config,
        )
        .with_deployment_id(deployment_id);
        let result = runtime.run().await;
        let snapshot = runtime
            .trading()
            .snapshot(&std::collections::BTreeMap::new());
        (result, snapshot)
    };

    for handle in feed_handles {
        handle.abort();
    }

    result
}

#[cfg(test)]
mod feed_source_tests {
    use super::{uses_db_primary_ticks, MarketDataSource};

    #[test]
    fn dual_market_data_keeps_primary_ticks_direct() {
        assert!(uses_db_primary_ticks(MarketDataSource::LocalDb));
        assert!(!uses_db_primary_ticks(MarketDataSource::Dual));
        assert!(!uses_db_primary_ticks(MarketDataSource::ExternalDirect));
    }
}

#[cfg(all(feature = "live", feature = "live-execution"))]
fn build_live_executor(
    client: ploy_control_client::ControlPlaneClient,
    policy: ploy_strategy_bundles::ExecutionPolicy,
    deployment_id: &str,
) -> execution::LiveExecutor {
    execution::LiveExecutor::new(client.worker_scoped(), policy, deployment_id)
}

#[cfg(all(test, feature = "live-execution"))]
mod restore_tests {
    use super::restore_live_trading_runtime;
    use ploy_control_client::ControlPlaneClient;
    use ploy_operator_contracts::{DeploymentRuntimeMode, TradingStateSnapshot};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn client_with_snapshots(snapshots: Vec<TradingStateSnapshot>) -> ControlPlaneClient {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ploy-live-restore-{unique}"));
        fs::create_dir_all(&root).expect("create runtime root");
        fs::write(
            root.join("trading-state.json"),
            serde_json::to_vec(&snapshots).expect("serialize snapshots"),
        )
        .expect("write snapshots");
        let mut client = ControlPlaneClient::from_runtime_root(root);
        client.control_plane_addr = "127.0.0.1:9".to_string();
        client
    }

    #[test]
    fn restore_is_scoped_to_deployment() {
        let client = client_with_snapshots(vec![
            TradingStateSnapshot {
                deployment_id: "other.live".to_string(),
                runtime_mode: DeploymentRuntimeMode::Live,
                ..TradingStateSnapshot::default()
            },
            TradingStateSnapshot {
                deployment_id: "target.live".to_string(),
                runtime_mode: DeploymentRuntimeMode::Live,
                ..TradingStateSnapshot::default()
            },
        ]);

        let runtime = restore_live_trading_runtime(&client, "target.live").expect("restore target");
        assert!(runtime.snapshot(&Default::default()).intents.is_empty());
    }

    #[test]
    fn restore_failure_does_not_start_empty_live_runtime() {
        let client = client_with_snapshots(Vec::new());
        assert!(restore_live_trading_runtime(&client, "missing.live").is_err());
    }
}
