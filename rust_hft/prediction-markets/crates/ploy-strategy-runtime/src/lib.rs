#[cfg(feature = "backtest-db")]
mod backtest;
#[cfg(feature = "live")]
mod live;
#[cfg(feature = "db-recorder")]
mod recording;
#[cfg(feature = "replay")]
mod replay;

#[cfg(feature = "backtest-db")]
use backtest::run_backtest_entry;
#[cfg(feature = "live")]
use live::run_live_or_dry_run_entry;
#[cfg(any(
    not(feature = "backtest-db"),
    not(feature = "live"),
    not(feature = "replay")
))]
use ploy_strategy_bundles::StrategyLogic;
use ploy_strategy_bundles::{FullConfig, RuntimeMode};
#[cfg(feature = "replay")]
use replay::run_replay_entry;
use rust_decimal::Decimal;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use tracing::info;

pub use ploy_strategy_bundles::RuntimeMode as StrategyRuntimeMode;

pub async fn run_strategy(config: FullConfig, config_path: &str, force_dry_run: bool) {
    run_strategy_with_deployment_id(config, config_path, force_dry_run, None).await;
}

pub async fn run_strategy_with_deployment_id(
    config: FullConfig,
    config_path: &str,
    force_dry_run: bool,
    deployment_id: Option<String>,
) {
    run_strategy_with_deployment_id_and_output(
        config,
        config_path,
        force_dry_run,
        deployment_id,
        None,
    )
    .await;
}

pub async fn run_strategy_with_deployment_id_and_output(
    config: FullConfig,
    config_path: &str,
    force_dry_run: bool,
    deployment_id: Option<String>,
    output_json: Option<&Path>,
) {
    let mut runtime_config = config.runtime_config();
    if force_dry_run {
        runtime_config.mode = RuntimeMode::DryRun;
    }
    let deployment_id = resolve_deployment_id(deployment_id);
    let deployment_label = deployment_id.clone();
    if matches!(runtime_config.mode, RuntimeMode::Live | RuntimeMode::DryRun)
        && deployment_id.is_none()
    {
        eprintln!(
            "Live and dry-run strategy runtime requires --deployment-id or PLOY_DEPLOYMENT_ID"
        );
        std::process::exit(1);
    }

    info!(
        mode = ?runtime_config.mode,
        deployment_id = deployment_id.as_deref().unwrap_or(""),
        config = %config_path,
        symbols = ?config.strategy.symbols,
        "ploy-runner starting",
    );

    let symbols = prepare_feed_symbols(runtime_config.mode, &config.strategy.symbols);

    let (result, snapshot) = match runtime_config.mode {
        RuntimeMode::Backtest => {
            run_backtest_entry(
                &config,
                &symbols,
                ploy_strategy_bundles::build_strategy(&config),
                runtime_config.clone(),
            )
            .await
        }
        RuntimeMode::Replay => {
            run_replay_entry(
                &config,
                ploy_strategy_bundles::build_strategy(&config),
                runtime_config.clone(),
            )
            .await
        }
        RuntimeMode::Live | RuntimeMode::DryRun => {
            let strategy = if config.is_market_update_recorder_profile(runtime_config.mode) {
                None
            } else {
                Some(ploy_strategy_bundles::build_strategy(&config))
            };
            run_live_or_dry_run_entry(
                &config,
                &symbols,
                strategy,
                runtime_config.clone(),
                deployment_id.expect("deployment_id checked for live/dry-run"),
            )
            .await
        }
    };

    info!(
        updates = result.updates_processed,
        intents = result.intents_submitted,
        fills = result.fills_recorded,
        net_pnl = %result.pnl.net_pnl(),
        elapsed = format!("{:.1}s", result.elapsed_secs),
        "ploy-runner finished",
    );

    if matches!(
        runtime_config.mode,
        RuntimeMode::Backtest | RuntimeMode::Replay
    ) {
        let cashflow = snapshot.fill_cashflow_summary();
        let roi_on_deployed_capital = cashflow
            .roi_on_deployed_capital()
            .map(|roi| format!("{}%", (roi * Decimal::from(100)).round_dp(2)))
            .unwrap_or_else(|| "n/a".to_string());

        info!(
            buy_shares = %cashflow.buy_shares,
            sell_shares = %cashflow.sell_shares,
            deployed_capital = %cashflow.deployed_capital(),
            gross_sell_proceeds = %cashflow.gross_sell_proceeds,
            fees = %cashflow.total_fees,
            roi_on_deployed_capital = %roi_on_deployed_capital,
            "Replay/backtest cashflow summary",
        );
    }

    if let Some(output_path) = output_json {
        write_strategy_evaluation(
            output_path,
            config_path,
            deployment_label.as_deref(),
            &result,
            &snapshot,
        );
    }
}

fn write_strategy_evaluation(
    output_path: &Path,
    config_path: &str,
    deployment_id: Option<&str>,
    result: &ploy_strategy_bundles::RuntimeResult,
    snapshot: &ploy_trading::TradingRuntimeSnapshot,
) {
    let cashflow = snapshot.fill_cashflow_summary();
    let strategy_diagnostics: BTreeMap<&str, u64> = result
        .strategy_diagnostics
        .iter()
        .map(|(key, value)| (key.as_str(), *value))
        .collect();
    let artifact = json!({
        "schema_version": 1,
        "artifact_type": "strategy_runtime_evaluation",
        "producer": "new-ploy-runner",
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "config_path": config_path,
        "deployment_id": deployment_id,
        "mode": format!("{:?}", result.mode),
        "result": {
            "updates_processed": result.updates_processed,
            "intents_submitted": result.intents_submitted,
            "fills_recorded": result.fills_recorded,
            "realized_pnl": result.pnl.realized_pnl,
            "unrealized_pnl": result.pnl.unrealized_pnl,
            "total_fees": result.pnl.total_fees,
            "net_pnl": result.pnl.net_pnl(),
            "risk": &result.risk,
            "elapsed_secs": result.elapsed_secs,
            "strategy_diagnostics": strategy_diagnostics,
        },
        "strategy_diagnostics": strategy_diagnostics,
        "cashflow": {
            "buy_shares": cashflow.buy_shares,
            "sell_shares": cashflow.sell_shares,
            "gross_buy_cost": cashflow.gross_buy_cost,
            "gross_sell_proceeds": cashflow.gross_sell_proceeds,
            "total_fees": cashflow.total_fees,
            "deployed_capital": cashflow.deployed_capital(),
            "net_pnl": cashflow.net_pnl(),
            "roi_on_deployed_capital": cashflow.roi_on_deployed_capital(),
        },
        "snapshot_counts": {
            "intents": snapshot.intents.len(),
            "orders": snapshot.orders.len(),
            "fills": snapshot.fills.len(),
            "positions": snapshot.positions.len(),
        },
        "runtime_evidence": normalized_runtime_evidence(snapshot, deployment_id),
    });

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "Failed to create output JSON directory {}: {error}",
                    parent.display()
                );
                std::process::exit(1);
            }
        }
    }

    match serde_json::to_vec_pretty(&artifact)
        .map(|mut bytes| {
            bytes.push(b'\n');
            bytes
        })
        .and_then(|bytes| std::fs::write(output_path, bytes).map_err(serde_json::Error::io))
    {
        Ok(()) => info!(path = %output_path.display(), "Wrote strategy runtime evaluation JSON"),
        Err(error) => {
            eprintln!(
                "Failed to write strategy runtime evaluation JSON {}: {error}",
                output_path.display()
            );
            std::process::exit(1);
        }
    }
}

fn normalized_runtime_evidence(
    snapshot: &ploy_trading::TradingRuntimeSnapshot,
    fallback_deployment_id: Option<&str>,
) -> serde_json::Value {
    let intents_by_id: BTreeMap<&str, &ploy_trading::TradingIntent> = snapshot
        .intents
        .iter()
        .map(|intent| (intent.intent_id.as_str(), intent))
        .collect();
    let orders_by_id: BTreeMap<&str, &ploy_trading::OrderRecord> = snapshot
        .orders
        .iter()
        .map(|order| (order.order_id.as_str(), order))
        .collect();

    let mut fill_quantity_by_order: BTreeMap<&str, Decimal> = BTreeMap::new();
    let mut fill_notional_by_order: BTreeMap<&str, Decimal> = BTreeMap::new();
    let mut fill_pnl_by_order: BTreeMap<&str, Decimal> = BTreeMap::new();
    for fill in &snapshot.fills {
        *fill_quantity_by_order
            .entry(fill.order_id.as_str())
            .or_default() += fill.quantity;
        *fill_notional_by_order
            .entry(fill.order_id.as_str())
            .or_default() += fill.quantity * fill.price;
        let signed_notional = match fill.side {
            ploy_trading::TradeSide::Buy => -(fill.quantity * fill.price),
            ploy_trading::TradeSide::Sell => fill.quantity * fill.price,
        };
        *fill_pnl_by_order.entry(fill.order_id.as_str()).or_default() += signed_notional - fill.fee;
    }

    let intents: Vec<_> = snapshot
        .intents
        .iter()
        .map(|intent| {
            let deployment_id =
                evidence_deployment_id(intent.deployment_id.as_str(), fallback_deployment_id);
            json!({
                "deployment_id": deployment_id,
                "intent_id": intent.intent_id.as_str(),
                "event_id": intent.market_id.as_str(),
                "market_id": intent.market_id.as_str(),
                "token_id": intent.token_id.as_str(),
                "side": trade_side_label(intent.side),
                "order_side": trade_side_label(intent.side),
                "purpose": intent_purpose_label(intent.purpose),
                "quantity": intent.quantity,
                "requested_qty": intent.quantity,
                "limit_price": intent.limit_price,
                "created_at": intent.created_at,
            })
        })
        .collect();

    let events: Vec<_> = snapshot
        .orders
        .iter()
        .map(|order| {
            let intent = intents_by_id.get(order.intent_id.as_str()).copied();
            let deployment_id =
                evidence_deployment_id(order.deployment_id.as_str(), fallback_deployment_id)
                    .or_else(|| {
                        intent.and_then(|intent| {
                            evidence_deployment_id(
                                intent.deployment_id.as_str(),
                                fallback_deployment_id,
                            )
                        })
                    });
            let fill_quantity = fill_quantity_by_order
                .get(order.order_id.as_str())
                .copied()
                .unwrap_or(order.filled_qty);
            let avg_fill_price = if fill_quantity.is_zero() {
                None
            } else {
                fill_notional_by_order
                    .get(order.order_id.as_str())
                    .map(|notional| *notional / fill_quantity)
            };
            let purpose = intent
                .map(|intent| intent_purpose_label(intent.purpose))
                .unwrap_or("ENTRY");

            json!({
                "deployment_id": deployment_id,
                "intent_id": order.intent_id.as_str(),
                "order_id": order.order_id.as_str(),
                "event_id": intent.map(|intent| intent.market_id.as_str()),
                "market_id": intent.map(|intent| intent.market_id.as_str()),
                "token_id": order.token_id.as_str(),
                "decision_ts": intent.map(|intent| intent.created_at),
                "quote": order.limit_price,
                "signal_inputs": {
                    "purpose": purpose,
                    "requested_qty": order.requested_qty,
                    "limit_price": order.limit_price,
                },
                "side": intent
                    .map(|intent| trade_side_label(intent.side))
                    .unwrap_or("UNKNOWN"),
                "entry_price": avg_fill_price.or(order.limit_price),
                "fill_status": order_state_label(order.state),
                "settlement": "open",
                "pnl": fill_pnl_by_order
                    .get(order.order_id.as_str())
                    .copied()
                    .unwrap_or(Decimal::ZERO),
            })
        })
        .collect();

    let orders: Vec<_> = snapshot
        .orders
        .iter()
        .map(|order| {
            let intent = intents_by_id.get(order.intent_id.as_str()).copied();
            let deployment_id =
                evidence_deployment_id(order.deployment_id.as_str(), fallback_deployment_id)
                    .or_else(|| {
                        intent.and_then(|intent| {
                            evidence_deployment_id(
                                intent.deployment_id.as_str(),
                                fallback_deployment_id,
                            )
                        })
                    });
            let fill_quantity = fill_quantity_by_order
                .get(order.order_id.as_str())
                .copied()
                .unwrap_or(order.filled_qty);
            let avg_fill_price = if fill_quantity.is_zero() {
                None
            } else {
                fill_notional_by_order
                    .get(order.order_id.as_str())
                    .map(|notional| *notional / fill_quantity)
            };

            json!({
                "deployment_id": deployment_id,
                "intent_id": order.intent_id.as_str(),
                "order_id": order.order_id.as_str(),
                "venue_order_id": order.venue_order_id.as_deref(),
                "event_id": intent.map(|intent| intent.market_id.as_str()),
                "market_id": intent.map(|intent| intent.market_id.as_str()),
                "token_id": order.token_id.as_str(),
                "market_side": None::<String>,
                "order_side": intent
                    .map(|intent| trade_side_label(intent.side))
                    .unwrap_or("UNKNOWN"),
                "purpose": intent.map(|intent| intent_purpose_label(intent.purpose)),
                "quantity": order.requested_qty,
                "requested_qty": order.requested_qty,
                "limit_price": order.limit_price,
                "filled_quantity": fill_quantity,
                "avg_fill_price": avg_fill_price,
                "status": order_state_label(order.state),
                "rejection_reason": order.rejection_reason.as_deref(),
                "last_error": order.last_error.as_deref(),
                "created_at": intent.map(|intent| intent.created_at),
            })
        })
        .collect();

    let fills: Vec<_> = snapshot
        .fills
        .iter()
        .map(|fill| {
            let order = orders_by_id.get(fill.order_id.as_str()).copied();
            let intent =
                order.and_then(|order| intents_by_id.get(order.intent_id.as_str()).copied());
            let deployment_id = order
                .and_then(|order| {
                    evidence_deployment_id(order.deployment_id.as_str(), fallback_deployment_id)
                })
                .or_else(|| {
                    intent.and_then(|intent| {
                        evidence_deployment_id(
                            intent.deployment_id.as_str(),
                            fallback_deployment_id,
                        )
                    })
                });
            json!({
                "deployment_id": deployment_id,
                "intent_id": order.map(|order| order.intent_id.as_str()),
                "order_id": fill.order_id.as_str(),
                "fill_id": fill.fill_id.as_str(),
                "event_id": intent.map(|intent| intent.market_id.as_str()),
                "market_id": intent.map(|intent| intent.market_id.as_str()),
                "token_id": fill.token_id.as_str(),
                "market_side": None::<String>,
                "fill_side": trade_side_label(fill.side),
                "purpose": intent.map(|intent| intent_purpose_label(intent.purpose)),
                "quantity": fill.quantity,
                "price": fill.price,
                "fee": fill.fee,
                "fill_timestamp": fill.timestamp,
            })
        })
        .collect();

    json!({
        "schema_version": 1,
        "basis": "trading_runtime_snapshot",
        "comparison_contract": "Compare these normalized rows against strategy_runtime_orders and strategy_runtime_fills exported from Tango for the same deployment/config/feed window.",
        "intents": intents,
        "events": events,
        "orders": orders,
        "fills": fills,
    })
}

fn evidence_deployment_id<'a>(
    deployment_id: &'a str,
    fallback_deployment_id: Option<&'a str>,
) -> Option<&'a str> {
    if deployment_id.is_empty() {
        fallback_deployment_id
    } else {
        Some(deployment_id)
    }
}

fn trade_side_label(side: ploy_trading::TradeSide) -> &'static str {
    match side {
        ploy_trading::TradeSide::Buy => "BUY",
        ploy_trading::TradeSide::Sell => "SELL",
    }
}

fn intent_purpose_label(purpose: ploy_trading::IntentPurpose) -> &'static str {
    match purpose {
        ploy_trading::IntentPurpose::Entry => "ENTRY",
        ploy_trading::IntentPurpose::Exit => "EXIT",
        ploy_trading::IntentPurpose::Reduce => "REDUCE",
        ploy_trading::IntentPurpose::Hedge => "HEDGE",
        ploy_trading::IntentPurpose::Cancel => "CANCEL",
    }
}

fn order_state_label(state: ploy_trading::OrderState) -> &'static str {
    match state {
        ploy_trading::OrderState::Pending => "PENDING",
        ploy_trading::OrderState::Unknown => "UNKNOWN",
        ploy_trading::OrderState::Acknowledged => "ACKNOWLEDGED",
        ploy_trading::OrderState::PartiallyFilled => "PARTIALLY_FILLED",
        ploy_trading::OrderState::Filled => "FILLED",
        ploy_trading::OrderState::Canceled => "CANCELED",
        ploy_trading::OrderState::Rejected => "REJECTED",
    }
}

#[cfg(not(feature = "backtest-db"))]
async fn run_backtest_entry(
    _config: &FullConfig,
    _symbols: &[String],
    _strategy: Box<dyn StrategyLogic>,
    _runtime_config: RuntimeModeConfig,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    eprintln!("Backtest mode requires the `backtest-db` feature");
    std::process::exit(1);
}

#[cfg(not(feature = "replay"))]
async fn run_replay_entry(
    _config: &FullConfig,
    _strategy: Box<dyn StrategyLogic>,
    _runtime_config: RuntimeModeConfig,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    eprintln!("Replay mode requires the `replay` feature");
    std::process::exit(1);
}

#[cfg(not(feature = "live"))]
async fn run_live_or_dry_run_entry(
    _config: &FullConfig,
    _symbols: &[String],
    _strategy: Option<Box<dyn StrategyLogic>>,
    _runtime_config: RuntimeModeConfig,
    _deployment_id: String,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    eprintln!("Live and dry-run modes require the `live` feature");
    std::process::exit(1);
}

type RuntimeModeConfig = ploy_strategy_bundles::RuntimeConfig;

fn resolve_deployment_id(cli_value: Option<String>) -> Option<String> {
    cli_value
        .or_else(|| std::env::var("PLOY_DEPLOYMENT_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn prepare_feed_symbols(mode: RuntimeMode, strategy_symbols: &[String]) -> Vec<String> {
    match mode {
        RuntimeMode::Backtest | RuntimeMode::Replay => strategy_symbols.to_vec(),
        RuntimeMode::Live | RuntimeMode::DryRun => strategy_symbols.to_vec(),
    }
}

#[cfg(any(feature = "live", test))]
fn database_unavailable_is_fatal(mode: RuntimeMode, database_url_present: bool) -> bool {
    mode == RuntimeMode::Live || (database_url_present && mode == RuntimeMode::DryRun)
}

#[cfg(test)]
mod tests {
    use super::{database_unavailable_is_fatal, prepare_feed_symbols, resolve_deployment_id};
    use ploy_strategy_bundles::RuntimeMode;

    #[test]
    fn keeps_strategy_symbols_canonical_for_live_feeds() {
        let symbols = vec!["BTCUSDT".to_string(), "ethusdt".to_string()];
        let prepared = prepare_feed_symbols(RuntimeMode::DryRun, &symbols);
        assert_eq!(prepared, vec!["BTCUSDT".to_string(), "ethusdt".to_string()]);
    }

    #[test]
    fn treats_live_and_dry_run_db_connection_failures_as_fatal_when_configured() {
        assert!(database_unavailable_is_fatal(RuntimeMode::Live, true));
        assert!(database_unavailable_is_fatal(RuntimeMode::Live, false));
        assert!(database_unavailable_is_fatal(RuntimeMode::DryRun, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::Backtest, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::Replay, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::DryRun, false));
    }

    #[test]
    fn trims_empty_deployment_ids() {
        assert_eq!(
            resolve_deployment_id(Some(" dep-1 ".to_string())).as_deref(),
            Some("dep-1")
        );
        assert!(resolve_deployment_id(Some(" ".to_string())).is_none());
    }
}
