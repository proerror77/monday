//! IPC command handler for SystemRuntime

#[cfg(feature = "infra-ipc")]
use std::sync::Arc;
#[cfg(feature = "infra-ipc")]
use tokio::sync::Mutex;

#[cfg(feature = "infra-ipc")]
use infra_ipc::{
    handlers::CommandHandler, AccountInfo, Command, Order, Position, Response, ResponseData,
    SystemHealth, SystemStatus, TradingMode,
};

#[cfg(feature = "infra-ipc")]
use async_trait::async_trait;

#[cfg(feature = "infra-ipc")]
use sysinfo::System;

#[cfg(feature = "infra-ipc")]
use crate::SystemRuntime;
#[cfg(feature = "infra-ipc")]
use hft_core::{AccountId, HftError, HftResult, OrderId, Price, Quantity, Symbol, VenueId};
#[cfg(feature = "infra-ipc")]
use std::collections::{HashMap, HashSet};
#[cfg(feature = "infra-ipc")]
use tokio::time::{Duration, Instant};
#[cfg(feature = "infra-ipc")]
use tracing::{error, info, warn};

#[cfg(feature = "infra-ipc")]
type IpcServerTask = tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>;

/// Command handler that integrates with SystemRuntime
#[cfg(feature = "infra-ipc")]
pub struct SystemCommandHandler {
    runtime: Arc<Mutex<SystemRuntime>>,
}

#[cfg(feature = "infra-ipc")]
impl SystemCommandHandler {
    pub fn new(runtime: Arc<Mutex<SystemRuntime>>) -> Self {
        Self { runtime }
    }
}

#[cfg(feature = "infra-ipc")]
#[async_trait]
impl CommandHandler for SystemCommandHandler {
    async fn handle_command(&self, command: Command) -> Response {
        match command {
            Command::Start => Response::Error {
                message: "runtime lifecycle is process-owned; restart the supervised process"
                    .to_string(),
                code: Some(409),
            },

            Command::Stop => Response::Error {
                message: "use EmergencyStop for trading safety or stop the supervised process"
                    .to_string(),
                code: Some(409),
            },

            Command::EmergencyStop => {
                warn!("IPC: Emergency stop requested");
                let runtime = self.runtime.lock().await;
                match runtime
                    .execution_control_handle()
                    .emergency_stop(true)
                    .await
                {
                    Ok(report) if report.is_complete() => Response::Ok,
                    Ok(report) => Response::Error {
                        message: format!(
                            "Emergency cancellation incomplete: {} of {} submitted",
                            report.submitted.len(),
                            report.requested
                        ),
                        code: Some(503),
                    },
                    Err(error) => Response::Error {
                        message: format!("Emergency stop failed: {error}"),
                        code: Some(503),
                    },
                }
            }

            Command::LoadModel { .. } => Response::Error {
                message: "direct model loading is disabled; deploy a signed StrategyBundle"
                    .to_string(),
                code: Some(403),
            },

            Command::UpdateRisk { .. } => Response::Error {
                message: "direct risk mutation is disabled; deploy a signed envelope and restart"
                    .to_string(),
                code: Some(403),
            },

            Command::GetStatus => {
                let runtime = self.runtime.lock().await;
                let account_view = runtime.get_account_view().await;
                let model_version = runtime.config.strategies.iter().find_map(|strategy| {
                    if let crate::StrategyParams::Onnx { model_version, .. } = &strategy.params {
                        Some(model_version.clone())
                    } else {
                        None
                    }
                });
                let engine_guard = runtime.engine.lock().await;
                let engine_stats = engine_guard.get_statistics();
                let latency_stats = engine_guard.get_latency_stats();
                let risk_metrics = engine_guard.get_risk_metrics();
                let engine_trading_mode = engine_guard.trading_mode();
                drop(engine_guard);

                // 從延遲統計提取關鍵健康指標（若可用）
                let ingestion_us = latency_stats
                    .get(&hft_core::LatencyStage::Ingestion)
                    .map(|s| s.mean_micros as u64)
                    .unwrap_or(0);
                let execution_us = latency_stats
                    .get(&hft_core::LatencyStage::Execution)
                    .map(|s| s.mean_micros as u64)
                    .unwrap_or(0);
                let staleness_us = latency_stats
                    .get(&hft_core::LatencyStage::EndToEnd)
                    .map(|s| s.p95_micros)
                    .unwrap_or(0);

                #[cfg(feature = "metrics")]
                let ring_util = infra_metrics::MetricsRegistry::global().queue_utilization_value();
                #[cfg(not(feature = "metrics"))]
                let ring_util = 0.0f64;

                // 系統級監控
                #[cfg(feature = "infra-ipc")]
                let (memory_bytes, cpu_pct) = {
                    let mut sys = System::new_all();
                    sys.refresh_all();
                    (sys.used_memory(), sys.global_cpu_info().cpu_usage() as f64)
                };
                #[cfg(not(feature = "infra-ipc"))]
                let (memory_bytes, cpu_pct) = (0u64, 0.0f64);

                let status = SystemStatus {
                    uptime_seconds: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    trading_mode: reported_trading_mode(
                        engine_trading_mode,
                        engine_stats.is_running,
                        runtime.config.quotes_only,
                        !runtime.config.strategies.is_empty(),
                        runtime
                            .config
                            .venues
                            .iter()
                            .any(|venue| venue.execution_mode.as_deref() == Some("Live")),
                    ),
                    active_strategies: engine_stats.strategies_count as u32,
                    connected_venues: engine_stats.consumers_count as u32, // Approximation
                    orders_today: engine_stats.orders_submitted as u64,
                    trades_today: engine_stats.orders_filled as u64,
                    current_pnl: account_view.realized_pnl + account_view.unrealized_pnl,
                    max_drawdown: risk_metrics
                        .as_ref()
                        .map(|metrics| metrics.max_drawdown)
                        .unwrap_or(rust_decimal::Decimal::ZERO),
                    model_version,
                    health: SystemHealth {
                        ingestion_lag_us: ingestion_us,
                        execution_lag_us: execution_us,
                        data_staleness_us: staleness_us,
                        ring_utilization: ring_util,
                        memory_usage_bytes: memory_bytes,
                        cpu_usage_pct: cpu_pct,
                    },
                };

                Response::Data(ResponseData::Status(status))
            }

            Command::GetAccount => {
                let runtime = self.runtime.lock().await;
                let account_view = runtime.get_account_view().await;

                // 從 OMS 獲取未結訂單數量和風控指標
                let engine_guard = runtime.engine.lock().await;
                let oms_state = engine_guard.export_oms_state();
                let risk_metrics = engine_guard.get_risk_metrics();
                drop(engine_guard);

                let open_orders_count = oms_state
                    .values()
                    .filter(|rec| {
                        matches!(
                            rec.status,
                            ports::OrderStatus::New
                                | ports::OrderStatus::Acknowledged
                                | ports::OrderStatus::Accepted
                                | ports::OrderStatus::PartiallyFilled
                        )
                    })
                    .count() as u32;

                let account = AccountInfo {
                    cash_balance: account_view.cash_balance,
                    total_value: account_view.equity(),
                    realized_pnl: account_view.realized_pnl,
                    unrealized_pnl: account_view.unrealized_pnl,
                    max_drawdown: risk_metrics
                        .as_ref()
                        .map(|metrics| metrics.max_drawdown)
                        .unwrap_or(rust_decimal::Decimal::ZERO),
                    open_positions: account_view.positions.len() as u32,
                    open_orders: open_orders_count,
                };

                Response::Data(ResponseData::Account(account))
            }

            Command::GetPositions => {
                let runtime = self.runtime.lock().await;
                let account_view = runtime.get_account_view().await;

                let positions: Vec<Position> = account_view
                    .positions
                    .iter()
                    .map(|(symbol, pos)| {
                        Position {
                            symbol: symbol.clone(),
                            quantity: pos.quantity.0,
                            average_price: pos.avg_price.0,
                            market_value: pos.avg_price.0 * pos.quantity.0 + pos.unrealized_pnl,
                            unrealized_pnl: pos.unrealized_pnl,
                            realized_pnl: rust_decimal::Decimal::ZERO, // TODO: 逐部位已实现损益追踪需要扩展 ports::Position 结构
                            // 当前系统仅在 AccountView 层面追踪总已实现损益
                            // 未来改进：在 Position 中添加 realized_pnl 字段，并在 PortfolioCore 中追踪每个仓位的平仓损益
                            last_update: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_nanos() as u64,
                        }
                    })
                    .collect();

                Response::Data(ResponseData::Positions(positions))
            }

            Command::GetOpenOrders => {
                info!("IPC: Getting open orders");
                let runtime = self.runtime.lock().await;
                let oms_state = runtime.engine.lock().await.export_oms_state();

                // 轉換為 IPC Order 結構
                let mut orders: Vec<Order> = Vec::new();
                for (order_id, rec) in oms_state.into_iter() {
                    // 僅返回未完成/未取消/未拒絕的訂單
                    if matches!(
                        rec.status,
                        ports::OrderStatus::New
                            | ports::OrderStatus::Acknowledged
                            | ports::OrderStatus::Accepted
                            | ports::OrderStatus::PartiallyFilled
                    ) {
                        // 狀態映射
                        let status = match rec.status {
                            ports::OrderStatus::New
                            | ports::OrderStatus::Acknowledged
                            | ports::OrderStatus::Accepted => {
                                infra_ipc::messages::OrderStatus::Pending
                            }
                            ports::OrderStatus::PartiallyFilled => {
                                infra_ipc::messages::OrderStatus::PartiallyFilled
                            }
                            ports::OrderStatus::Filled => infra_ipc::messages::OrderStatus::Filled,
                            ports::OrderStatus::Canceled => {
                                infra_ipc::messages::OrderStatus::Cancelled
                            }
                            ports::OrderStatus::Rejected => {
                                infra_ipc::messages::OrderStatus::Rejected
                            }
                            _ => infra_ipc::messages::OrderStatus::Pending,
                        };

                        // 側/型別映射（型別暫不追蹤，先視為 Limit）
                        let side = match rec.side {
                            hft_core::Side::Buy => infra_ipc::messages::OrderSide::Buy,
                            hft_core::Side::Sell => infra_ipc::messages::OrderSide::Sell,
                        };
                        let order_type = infra_ipc::messages::OrderType::Limit;

                        // 派生帳戶 ID（Phase 1：基於策略映射）
                        let account_id = {
                            let eng = self.runtime.lock().await;
                            let eng = eng.engine.lock().await;
                            eng.get_account_for_order(&order_id).map(|a| a.0)
                        };

                        orders.push(Order {
                            order_id: order_id.0,
                            symbol: rec.symbol.clone(),
                            side,
                            order_type,
                            quantity: rec.qty.0,
                            price: rec.avg_price.map(|p| p.0),
                            filled_quantity: rec.cum_qty.0,
                            status,
                            timestamp: 0,
                            strategy_id: rec.strategy_id.clone(),
                            account_id,
                        });
                    }
                }

                Response::Data(ResponseData::OpenOrders(orders))
            }

            Command::InspectExecutionAccounts => {
                info!("IPC: Inspecting authoritative execution accounts");
                let control = self.runtime.lock().await.execution_control_handle();
                match control.inspect_account().await {
                    Ok(snapshot) => Response::Data(ResponseData::ExecutionAccounts(
                        execution_account_inspection(snapshot),
                    )),
                    Err(error) => control_error_response(error),
                }
            }

            Command::CancelAllOrders => {
                info!("IPC: Cancelling all orders");
                let runtime = self.runtime.lock().await;
                let cancel_events = runtime.engine.lock().await.subscribe_execution_events();
                let report = match Self::cancel_all_orders_internal(&runtime).await {
                    Ok(report) => report,
                    Err(error) => {
                        return Response::Error {
                            message: format!("Failed to dispatch cancellation: {error}"),
                            code: Some(503),
                        };
                    }
                };
                // 追蹤回覆
                let stats = Self::await_cancel_stats(
                    &runtime,
                    &report.submitted,
                    Self::cancel_timeout_ms(),
                    cancel_events,
                )
                .await;
                Response::Data(infra_ipc::messages::ResponseData::CancelResult(stats))
            }

            Command::CancelOrdersFiltered { symbol, venue } => {
                let venue = match venue.as_deref() {
                    Some(value) => match VenueId::from_str(value) {
                        Some(venue) => Some(venue),
                        None => {
                            return Response::Error {
                                message: format!("Unknown venue filter: {value}"),
                                code: Some(400),
                            };
                        }
                    },
                    None => None,
                };
                info!(
                    "IPC: Cancelling orders with symbol={:?} venue={:?}",
                    symbol, venue
                );
                let runtime = self.runtime.lock().await;
                let cancel_events = runtime.engine.lock().await.subscribe_execution_events();
                let report = match runtime
                    .execution_control_handle()
                    .cancel_orders_filtered(symbol, venue)
                    .await
                {
                    Ok(report) if report.is_complete() => report,
                    Ok(report) => {
                        return Response::Error {
                            message: incomplete_cancel_message(&report),
                            code: Some(503),
                        };
                    }
                    Err(error) => return control_error_response(error),
                };
                let stats = Self::await_cancel_stats(
                    &runtime,
                    &report.submitted,
                    Self::cancel_timeout_ms(),
                    cancel_events,
                )
                .await;
                Response::Data(ResponseData::CancelResult(stats))
            }

            Command::CancelOrdersForSymbol { symbol } => {
                info!("IPC: Cancelling orders for symbol {}", symbol.as_str());
                let runtime = self.runtime.lock().await;
                let cancel_events = runtime.engine.lock().await.subscribe_execution_events();
                let report = match runtime
                    .execution_control_handle()
                    .cancel_orders_filtered(Some(symbol.clone()), None)
                    .await
                {
                    Ok(report) if report.is_complete() => report,
                    Ok(report) => {
                        return Response::Error {
                            message: incomplete_cancel_message(&report),
                            code: Some(503),
                        };
                    }
                    Err(error) => {
                        return Response::Error {
                            message: format!("Failed to dispatch cancellation: {error}"),
                            code: Some(503),
                        };
                    }
                };
                // 追蹤回覆
                let stats = Self::await_cancel_stats(
                    &runtime,
                    &report.submitted,
                    Self::cancel_timeout_ms(),
                    cancel_events,
                )
                .await;
                Response::Data(infra_ipc::messages::ResponseData::CancelResult(stats))
            }

            Command::CancelOrdersForStrategy { strategy_id } => {
                info!("IPC: Cancelling orders for strategy {}", strategy_id);
                let runtime = self.runtime.lock().await;
                let cancel_events = runtime.engine.lock().await.subscribe_execution_events();
                let report = match runtime.cancel_orders_for_strategy(&strategy_id).await {
                    Ok(report) if report.is_complete() => report,
                    Ok(report) => {
                        return Response::Error {
                            message: incomplete_cancel_message(&report),
                            code: Some(503),
                        };
                    }
                    Err(error) => {
                        return Response::Error {
                            message: format!("Failed to dispatch cancellation: {error}"),
                            code: Some(503),
                        };
                    }
                };
                // 等待撤單統計
                let stats = Self::await_cancel_stats(
                    &runtime,
                    &report.submitted,
                    Self::cancel_timeout_ms(),
                    cancel_events,
                )
                .await;
                Response::Data(infra_ipc::messages::ResponseData::CancelResult(stats))
            }

            Command::CancelOrder { order_id, symbol } => {
                info!(
                    "IPC: Cancel order {} for symbol {}",
                    order_id,
                    symbol.as_str()
                );
                let runtime = self.runtime.lock().await;
                let cancel_events = runtime.engine.lock().await.subscribe_execution_events();
                let target_id = OrderId(order_id);
                let report = match runtime
                    .execution_control_handle()
                    .cancel_order(target_id, symbol)
                    .await
                {
                    Ok(report) if report.is_complete() => report,
                    Ok(report) => {
                        return Response::Error {
                            message: incomplete_cancel_message(&report),
                            code: Some(503),
                        };
                    }
                    Err(error) => {
                        return Response::Error {
                            message: format!("Failed to dispatch cancellation: {error}"),
                            code: Some(503),
                        };
                    }
                };
                let stats = Self::await_cancel_stats(
                    &runtime,
                    &report.submitted,
                    Self::cancel_timeout_ms(),
                    cancel_events,
                )
                .await;
                Response::Data(infra_ipc::messages::ResponseData::CancelResult(stats))
            }

            Command::CancelOrderById { order_id } => {
                info!("IPC: Cancelling order {order_id}");
                let runtime = self.runtime.lock().await;
                let target_id = OrderId(order_id);
                let (local_symbol, cancel_events) = {
                    let engine = runtime.engine.lock().await;
                    let state = engine.export_oms_state();
                    let symbol = match state.get(&target_id) {
                        Some(order)
                            if matches!(
                                order.status,
                                ports::OrderStatus::New
                                    | ports::OrderStatus::Acknowledged
                                    | ports::OrderStatus::Accepted
                                    | ports::OrderStatus::PartiallyFilled
                            ) => Some(order.symbol.clone()),
                        Some(_) => {
                            return Response::Error {
                                message: format!("OMS order is not open: {}", target_id.0),
                                code: Some(409),
                            };
                        }
                        None => None,
                    };
                    (symbol, engine.subscribe_execution_events())
                };
                let (symbol, authoritative_route) = match local_symbol {
                    Some(symbol) => (symbol, None),
                    None => {
                        let snapshot = match runtime.execution_control_handle().reconcile(false).await
                        {
                            Ok(report) => report.worker_snapshot,
                            Err(error) => return control_error_response(error),
                        };
                        match authoritative_open_order_symbol(&snapshot, &target_id) {
                            Ok((symbol, venue, account_id)) => {
                                (symbol, Some((venue, account_id)))
                            }
                            Err(error) => return control_error_response(error),
                        }
                    }
                };
                let control = runtime.execution_control_handle();
                let cancellation = match authoritative_route {
                    Some((venue, account_id)) => {
                        control
                            .cancel_authoritative_order(
                                target_id,
                                symbol,
                                venue,
                                account_id,
                            )
                            .await
                    }
                    None => control.cancel_order(target_id, symbol).await,
                };
                let report = match cancellation {
                    Ok(report) if report.is_complete() => report,
                    Ok(report) => {
                        return Response::Error {
                            message: incomplete_cancel_message(&report),
                            code: Some(503),
                        };
                    }
                    Err(error) => return control_error_response(error),
                };
                let stats = Self::await_cancel_stats(
                    &runtime,
                    &report.submitted,
                    Self::cancel_timeout_ms(),
                    cancel_events,
                )
                .await;
                Response::Data(ResponseData::CancelResult(stats))
            }

            Command::ReplaceOrder {
                order_id,
                symbol,
                new_quantity,
                new_price,
            } => {
                info!(
                    "IPC: Replacing OMS-tracked order {} for {}",
                    order_id,
                    symbol.as_str()
                );
                if new_quantity.is_none() && new_price.is_none() {
                    return Response::Error {
                        message: "Replacement requires new_quantity or new_price".to_string(),
                        code: Some(400),
                    };
                }
                let control = self.runtime.lock().await.execution_control_handle();
                match control
                    .replace_order(
                        OrderId(order_id),
                        symbol,
                        new_quantity.map(Quantity),
                        new_price.map(Price),
                    )
                    .await
                {
                    Ok(()) => Response::Ok,
                    Err(error) => control_error_response(error),
                }
            }

            Command::SetTradingMode { mode } => {
                info!("IPC: Setting trading mode to {:?}", mode);
                let runtime = self.runtime.lock().await;

                match mode {
                    TradingMode::Live
                    | TradingMode::Paper
                    | TradingMode::Shadow
                    | TradingMode::Degraded => Response::Error {
                        message:
                            "direct resume is disabled; deploy a signed envelope and restart"
                                .to_string(),
                        code: Some(403),
                    },
                    TradingMode::Replay | TradingMode::Emergency => match runtime
                        .execution_control_handle()
                        .emergency_stop(true)
                        .await
                    {
                        Ok(report) if report.is_complete() => {
                            info!("已进入安全 Replay 边界；重启后方可恢复执行");
                            Response::Ok
                        }
                        Ok(report) => Response::Error {
                            message: format!(
                                "Replay cancellation incomplete: {} of {} submitted",
                                report.submitted.len(),
                                report.requested
                            ),
                            code: Some(503),
                        },
                        Err(error) => Response::Error {
                            message: error.to_string(),
                            code: Some(503),
                        },
                    },
                    TradingMode::Paused => {
                        match runtime.execution_control_handle().pause_trading().await {
                            Ok(()) => {
                                info!("系统已暂停");
                                Response::Ok
                            }
                            Err(error) => Response::Error {
                                message: error.to_string(),
                                code: Some(503),
                            },
                        }
                    }
                }
            }

            Command::SetStrategyEnabled {
                strategy_id,
                enabled,
            } => {
                info!("IPC: Setting strategy {} enabled={}", strategy_id, enabled);
                if enabled {
                    return Response::Error {
                        message: "direct strategy enable is disabled; deploy a signed StrategyBundle and restart"
                            .to_string(),
                        code: Some(403),
                    };
                }
                let runtime = self.runtime.lock().await;

                // 通过引擎设置策略启用/禁用状态
                let mut engine = runtime.engine.lock().await;
                match engine.set_strategy_enabled(&strategy_id, enabled) {
                    Ok(_) => {
                        info!(
                            "策略 {} 已{}启用",
                            strategy_id,
                            if enabled { "" } else { "禁" }
                        );

                        // 如果禁用策略，取消该策略的所有未结订单
                        if !enabled {
                            drop(engine);
                            let cancel_events =
                                runtime.engine.lock().await.subscribe_execution_events();
                            let report = match runtime
                                .cancel_orders_for_strategy(&strategy_id)
                                .await
                            {
                                Ok(report) if report.is_complete() => report,
                                Ok(report) => {
                                    return Response::Error {
                                        message: format!(
                                            "strategy disabled but cancellation failed: {}",
                                            incomplete_cancel_message(&report)
                                        ),
                                        code: Some(503),
                                    };
                                }
                                Err(error) => {
                                    return Response::Error {
                                        message: format!(
                                            "strategy disabled but cancellation dispatch failed: {error}"
                                        ),
                                        code: Some(503),
                                    };
                                }
                            };
                            let stats = Self::await_cancel_stats(
                                &runtime,
                                &report.submitted,
                                Self::cancel_timeout_ms(),
                                cancel_events,
                            )
                            .await;
                            if stats.failed > 0 {
                                return Response::Error {
                                    message: format!(
                                        "strategy disabled but {} of {} cancellations were not confirmed",
                                        stats.failed, stats.requested
                                    ),
                                    code: Some(503),
                                };
                            }
                            info!(
                                "已取消策略 {} 的 {} 个未结订单",
                                strategy_id, stats.succeeded
                            );
                        }

                        Response::Ok
                    }
                    Err(e) => Response::Error {
                        message: format!("设置策略状态失败: {}", e),
                        code: Some(500),
                    },
                }
            }

            Command::SetSymbolLimits { .. } => Response::Error {
                message: "direct symbol-limit mutation is disabled; deploy a signed envelope and restart"
                    .to_string(),
                code: Some(403),
            },

            Command::UpdateStrategyParams { .. } => Response::Error {
                message: "direct strategy replacement is disabled; deploy a signed StrategyBundle and restart"
                    .to_string(),
                code: Some(403),
            },
        }
    }
}

#[cfg(feature = "infra-ipc")]
fn execution_account_inspection(
    snapshot: engine::execution_worker::WorkerReconcileSnapshot,
) -> infra_ipc::ExecutionAccountInspection {
    infra_ipc::ExecutionAccountInspection {
        clients: snapshot
            .clients
            .into_iter()
            .map(|client| infra_ipc::ExecutionAccountSnapshot {
                client_index: u32::try_from(client.client_index).unwrap_or(u32::MAX),
                venue: client.venue.map(|venue| venue.to_string()),
                account_id: client.account_id.map(|account| account.0),
                open_orders: map_required_snapshot(client.open_orders, |order| {
                    infra_ipc::ExecutionOpenOrder {
                        order_id: order.order_id.0,
                        client_order_id: order.client_order_id,
                        symbol: order.symbol,
                        side: ipc_side(order.side),
                        order_type: format!("{:?}", order.order_type),
                        original_quantity: order.original_quantity.0,
                        remaining_quantity: order.remaining_quantity.0,
                        filled_quantity: order.filled_quantity.0,
                        price: order.price.map(|price| price.0),
                        status: format!("{:?}", order.status),
                        created_at: order.created_at,
                        updated_at: order.updated_at,
                    }
                }),
                balances: map_optional_snapshot(client.balances, |balance| {
                    infra_ipc::ExecutionBalance {
                        asset: balance.asset,
                        available: balance.available,
                        frozen: balance.frozen,
                        total: balance.total,
                        usd_value: balance.usd_value,
                    }
                }),
                positions: map_optional_snapshot(client.positions, |position| {
                    infra_ipc::ExecutionPosition {
                        symbol: position.symbol,
                        quantity: position.quantity.0,
                        average_price: position.avg_price.0,
                        unrealized_pnl: position.unrealized_pnl,
                    }
                }),
                recent_fills: map_optional_snapshot(client.recent_fills, |fill| {
                    infra_ipc::ExecutionFill {
                        fill_id: fill.fill_id,
                        order_id: fill.order_id.0,
                        symbol: fill.symbol,
                        side: ipc_side(fill.side),
                        price: fill.price.0,
                        quantity: fill.quantity.0,
                        fee: fill.fee,
                        timestamp: fill.timestamp,
                    }
                }),
            })
            .collect(),
    }
}

#[cfg(feature = "infra-ipc")]
fn map_required_snapshot<T, U>(
    result: Result<Vec<T>, HftError>,
    map: impl FnMut(T) -> U,
) -> infra_ipc::AuthoritativeSnapshot<Vec<U>> {
    match result {
        Ok(values) => infra_ipc::AuthoritativeSnapshot::Data(values.into_iter().map(map).collect()),
        Err(error) => infra_ipc::AuthoritativeSnapshot::Error(error.to_string()),
    }
}

#[cfg(feature = "infra-ipc")]
fn map_optional_snapshot<T, U>(
    result: Option<Result<Vec<T>, HftError>>,
    map: impl FnMut(T) -> U,
) -> infra_ipc::AuthoritativeSnapshot<Vec<U>> {
    match result {
        None => infra_ipc::AuthoritativeSnapshot::Unsupported,
        Some(result) => map_required_snapshot(result, map),
    }
}

#[cfg(feature = "infra-ipc")]
fn ipc_side(side: hft_core::Side) -> infra_ipc::OrderSide {
    match side {
        hft_core::Side::Buy => infra_ipc::OrderSide::Buy,
        hft_core::Side::Sell => infra_ipc::OrderSide::Sell,
    }
}

#[cfg(feature = "infra-ipc")]
fn control_error_response(error: HftError) -> Response {
    let code = match &error {
        HftError::OrderNotFound(_) => 404,
        HftError::InvalidOrder(_) | HftError::Risk(_) => 409,
        HftError::Authentication(_) => 401,
        _ => 503,
    };
    Response::Error {
        message: error.to_string(),
        code: Some(code),
    }
}

#[cfg(feature = "infra-ipc")]
fn authoritative_open_order_symbol(
    snapshot: &engine::execution_worker::WorkerReconcileSnapshot,
    order_id: &OrderId,
) -> HftResult<(Symbol, Option<VenueId>, Option<AccountId>)> {
    if !snapshot.is_complete() {
        return Err(HftError::Execution(
            "authoritative open-order snapshot is incomplete".to_string(),
        ));
    }
    let matches = snapshot
        .clients
        .iter()
        .flat_map(|client| {
            client
                .open_orders
                .as_ref()
                .ok()
                .into_iter()
                .flatten()
                .filter(|order| order.order_id == *order_id)
                .map(|order| {
                    (
                        order.symbol.clone(),
                        client.venue,
                        client.account_id.clone(),
                    )
                })
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [(symbol, venue, account_id)] if venue.is_some() || account_id.is_some() => {
            Ok((symbol.clone(), *venue, account_id.clone()))
        }
        [_] => Err(HftError::Execution(format!(
            "authoritative order {} has no venue/account routing identity",
            order_id.0
        ))),
        [] => Err(HftError::OrderNotFound(format!(
            "authoritative open order not found: {}",
            order_id.0
        ))),
        _ => Err(HftError::Execution(format!(
            "order ID {} is ambiguous across execution accounts",
            order_id.0
        ))),
    }
}

#[cfg(feature = "infra-ipc")]
fn reported_trading_mode(
    engine_mode: engine::TradingMode,
    is_running: bool,
    quotes_only: bool,
    has_strategies: bool,
    has_live_venue: bool,
) -> TradingMode {
    match engine_mode {
        engine::TradingMode::Emergency => TradingMode::Emergency,
        engine::TradingMode::Paused => TradingMode::Paused,
        engine::TradingMode::Degraded => TradingMode::Degraded,
        engine::TradingMode::Normal if !is_running => TradingMode::Paused,
        engine::TradingMode::Normal if quotes_only && has_strategies => TradingMode::Shadow,
        engine::TradingMode::Normal if quotes_only => TradingMode::Replay,
        engine::TradingMode::Normal if has_live_venue => TradingMode::Live,
        engine::TradingMode::Normal => TradingMode::Paper,
    }
}

#[cfg(all(test, feature = "infra-ipc"))]
mod status_tests {
    use super::*;

    #[test]
    fn ipc_status_preserves_engine_safety_modes() {
        for (engine_mode, expected) in [
            (engine::TradingMode::Degraded, TradingMode::Degraded),
            (engine::TradingMode::Paused, TradingMode::Paused),
            (engine::TradingMode::Emergency, TradingMode::Emergency),
        ] {
            assert_eq!(
                reported_trading_mode(engine_mode, true, false, true, true),
                expected
            );
        }
    }

    #[test]
    fn ipc_status_distinguishes_shadow_replay_paper_and_live() {
        assert_eq!(
            reported_trading_mode(engine::TradingMode::Normal, true, true, true, false),
            TradingMode::Shadow
        );
        assert_eq!(
            reported_trading_mode(engine::TradingMode::Normal, true, true, false, false),
            TradingMode::Replay
        );
        assert_eq!(
            reported_trading_mode(engine::TradingMode::Normal, true, false, true, false),
            TradingMode::Paper
        );
        assert_eq!(
            reported_trading_mode(engine::TradingMode::Normal, true, false, true, true),
            TradingMode::Live
        );
    }
}

#[cfg(feature = "infra-ipc")]
fn incomplete_cancel_message(report: &engine::execution_worker::CancelDispatchReport) -> String {
    let reasons = report
        .failures
        .iter()
        .map(|failure| format!("{}: {}", failure.order_id.0, failure.reason))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "cancellation dispatch incomplete: {} of {} submitted{}{}",
        report.submitted.len(),
        report.requested,
        if reasons.is_empty() { "" } else { "; " },
        reasons
    )
}

#[cfg(all(test, feature = "infra-ipc"))]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn incomplete_cancel_message_preserves_dispatch_failure_evidence() {
        let report = engine::execution_worker::CancelDispatchReport {
            requested: 2,
            submitted: vec![OrderId("submitted".to_string())],
            failures: vec![engine::execution_worker::CancelFailure {
                order_id: OrderId("failed".to_string()),
                reason: "venue rejected cancellation".to_string(),
            }],
        };

        let message = incomplete_cancel_message(&report);
        assert!(message.contains("1 of 2"));
        assert!(message.contains("failed: venue rejected cancellation"));
    }

    #[test]
    fn cancel_by_id_can_resolve_an_exchange_only_order_from_authoritative_state() {
        let order_id = OrderId("venue-order-1".to_string());
        let symbol = Symbol::new("123");
        let snapshot = engine::execution_worker::WorkerReconcileSnapshot {
            clients: vec![engine::execution_worker::ClientReconcileSnapshot {
                client_index: 0,
                venue: Some(VenueId::POLYMARKET),
                account_id: Some(hft_core::AccountId("poly-main".to_string())),
                open_orders: Ok(vec![ports::OpenOrder {
                    order_id: order_id.clone(),
                    client_order_id: None,
                    symbol: symbol.clone(),
                    side: hft_core::Side::Buy,
                    order_type: hft_core::OrderType::Limit,
                    original_quantity: Quantity(Decimal::from(2)),
                    remaining_quantity: Quantity(Decimal::from(2)),
                    filled_quantity: Quantity(Decimal::ZERO),
                    price: Some(Price(Decimal::new(51, 2))),
                    status: ports::OrderStatus::Accepted,
                    created_at: 1,
                    updated_at: 1,
                }]),
                balances: None,
                positions: None,
                recent_fills: None,
            }],
        };

        assert_eq!(
            authoritative_open_order_symbol(&snapshot, &order_id)
                .expect("authoritative order identity"),
            (
                symbol,
                Some(VenueId::POLYMARKET),
                Some(AccountId("poly-main".to_string()))
            )
        );
        assert!(matches!(
            authoritative_open_order_symbol(&snapshot, &OrderId("missing".to_string())),
            Err(HftError::OrderNotFound(_))
        ));
    }

    #[test]
    fn authoritative_account_mapping_preserves_data_unsupported_and_errors() {
        let snapshot = engine::execution_worker::WorkerReconcileSnapshot {
            clients: vec![
                engine::execution_worker::ClientReconcileSnapshot {
                    client_index: 0,
                    venue: Some(VenueId::POLYMARKET),
                    account_id: Some(hft_core::AccountId("poly-main".to_string())),
                    open_orders: Ok(Vec::new()),
                    balances: Some(Ok(vec![ports::AccountBalance {
                        asset: "USDC".to_string(),
                        available: Decimal::from(10),
                        frozen: Decimal::ONE,
                        total: Decimal::from(11),
                        usd_value: Some(Decimal::from(11)),
                    }])),
                    positions: Some(Ok(vec![ports::Position {
                        symbol: hft_core::Symbol::new("123"),
                        quantity: Quantity(Decimal::from(2)),
                        avg_price: Price(Decimal::new(51, 2)),
                        unrealized_pnl: Decimal::new(5, 2),
                    }])),
                    recent_fills: Some(Ok(vec![ports::AccountFill {
                        fill_id: "fill-1".to_string(),
                        order_id: OrderId("order-1".to_string()),
                        symbol: hft_core::Symbol::new("123"),
                        side: hft_core::Side::Buy,
                        price: Price(Decimal::new(51, 2)),
                        quantity: Quantity(Decimal::ONE),
                        fee: Some(Decimal::new(1, 3)),
                        timestamp: 123_000,
                    }])),
                },
                engine::execution_worker::ClientReconcileSnapshot {
                    client_index: 1,
                    venue: Some(VenueId::BINANCE),
                    account_id: None,
                    open_orders: Ok(Vec::new()),
                    balances: None,
                    positions: None,
                    recent_fills: Some(Err(HftError::Network("history unavailable".to_string()))),
                },
            ],
        };

        let inspection = execution_account_inspection(snapshot);
        assert_eq!(inspection.clients.len(), 2);
        let polymarket = &inspection.clients[0];
        assert_eq!(polymarket.venue.as_deref(), Some("POLYMARKET"));
        assert_eq!(polymarket.account_id.as_deref(), Some("poly-main"));
        let infra_ipc::AuthoritativeSnapshot::Data(balances) = &polymarket.balances else {
            panic!("expected authoritative balances")
        };
        assert_eq!(balances[0].total, Decimal::from(11));
        let infra_ipc::AuthoritativeSnapshot::Data(positions) = &polymarket.positions else {
            panic!("expected authoritative positions")
        };
        assert_eq!(positions[0].symbol.as_str(), "123");
        let infra_ipc::AuthoritativeSnapshot::Data(fills) = &polymarket.recent_fills else {
            panic!("expected authoritative recent fills")
        };
        assert_eq!(fills[0].fill_id, "fill-1");
        assert!(matches!(
            &inspection.clients[1].positions,
            infra_ipc::AuthoritativeSnapshot::Unsupported
        ));
        assert!(matches!(
            &inspection.clients[1].recent_fills,
            infra_ipc::AuthoritativeSnapshot::Error(_)
        ));
    }

    #[tokio::test]
    async fn ipc_denies_every_direct_risk_increasing_mutation() {
        let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/dev/binance_quotes_only.yaml");
        let runtime = crate::SystemBuilder::from_yaml(config_path.to_str().unwrap())
            .unwrap()
            .build();
        let handler = SystemCommandHandler::new(Arc::new(Mutex::new(runtime)));
        let commands = vec![
            Command::LoadModel {
                model_path: "/tmp/model.onnx".to_string(),
                model_version: "v1".to_string(),
                sha256_hash: None,
            },
            Command::UpdateRisk {
                global_position_limit: None,
                global_notional_limit: None,
                max_daily_trades: None,
                max_orders_per_second: None,
                staleness_threshold_us: None,
                strategy_overrides: HashMap::new(),
            },
            Command::SetTradingMode {
                mode: TradingMode::Paper,
            },
            Command::SetStrategyEnabled {
                strategy_id: "strategy-1".to_string(),
                enabled: true,
            },
            Command::SetSymbolLimits {
                symbol: hft_core::Symbol::new("BTCUSDT"),
                max_position: None,
                max_notional: None,
            },
            Command::UpdateStrategyParams {
                strategy_id: "strategy-1".to_string(),
                params: shared_config::StrategyParams::None,
            },
        ];

        for command in commands {
            assert!(matches!(
                handler.handle_command(command).await,
                Response::Error {
                    code: Some(403),
                    ..
                }
            ));
        }
    }

    #[tokio::test]
    async fn ipc_operator_commands_fail_closed_without_execution_worker_or_tracked_order() {
        let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/dev/binance_quotes_only.yaml");
        let runtime = crate::SystemBuilder::from_yaml(config_path.to_str().unwrap())
            .unwrap()
            .build();
        let handler = SystemCommandHandler::new(Arc::new(Mutex::new(runtime)));

        assert!(matches!(
            handler
                .handle_command(Command::InspectExecutionAccounts)
                .await,
            Response::Error {
                code: Some(503),
                ..
            }
        ));
        assert!(matches!(
            handler
                .handle_command(Command::CancelOrderById {
                    order_id: "unknown".to_string(),
                })
                .await,
            Response::Error {
                code: Some(503),
                ..
            }
        ));
        assert!(matches!(
            handler
                .handle_command(Command::ReplaceOrder {
                    order_id: "unknown".to_string(),
                    symbol: hft_core::Symbol::new("123"),
                    new_quantity: Some(Decimal::ONE),
                    new_price: None,
                })
                .await,
            Response::Error {
                code: Some(404),
                ..
            }
        ));
        assert!(matches!(
            handler
                .handle_command(Command::CancelOrdersFiltered {
                    symbol: None,
                    venue: Some("NOT_A_VENUE".to_string()),
                })
                .await,
            Response::Error {
                code: Some(400),
                ..
            }
        ));
        let response = handler
            .handle_command(Command::CancelOrdersFiltered {
                symbol: Some(hft_core::Symbol::new("123")),
                venue: Some("POLYMARKET".to_string()),
            })
            .await;
        let Response::Data(ResponseData::CancelResult(stats)) = response else {
            panic!("empty safe filter should return cancellation stats")
        };
        assert_eq!(stats.requested, 0);
        assert_eq!(stats.failed, 0);
    }

    #[tokio::test]
    async fn unix_ipc_client_reaches_runtime_operator_commands() {
        let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/dev/binance_quotes_only.yaml");
        let runtime = crate::SystemBuilder::from_yaml(config_path.to_str().unwrap())
            .unwrap()
            .build();
        let handler = SystemCommandHandler::new(Arc::new(Mutex::new(runtime)));
        let directory = tempfile::tempdir().expect("temporary IPC directory");
        let socket_path = directory.path().join("operator.sock");
        let server = infra_ipc::IPCServer::new(&socket_path, handler);
        let prepared = server.prepare().expect("IPC socket is prepared");
        let server_task = tokio::spawn(async move { server.serve(prepared).await });
        assert!(
            socket_path.exists(),
            "prepared IPC server did not bind its Unix socket"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&socket_path)
                    .expect("IPC socket metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let client = infra_ipc::IPCClient::new(&socket_path);
        let response = client
            .cancel_orders_filtered(
                Some(hft_core::Symbol::new("123")),
                Some("POLYMARKET".to_string()),
            )
            .await
            .expect("operator cancellation request");
        let Response::Data(ResponseData::CancelResult(stats)) = response else {
            panic!("expected cancellation result over Unix IPC")
        };
        assert_eq!(stats.requested, 0);
        assert!(matches!(
            client
                .inspect_execution_accounts()
                .await
                .expect("operator account inspection response"),
            Response::Error {
                code: Some(503),
                ..
            }
        ));

        server_task.abort();
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn ipc_prepare_reports_socket_failure_synchronously() {
        let directory = tempfile::tempdir().expect("temporary IPC directory");
        let parent_file = directory.path().join("not-a-directory");
        std::fs::write(&parent_file, b"occupied").expect("create blocking path component");
        let socket_path = parent_file.join("operator.sock");

        let result = prepare_ipc_server(Some(socket_path.to_string_lossy().into_owned()));
        match result {
            Ok(_) => panic!("IPC preparation must fail before returning a background task"),
            Err(error) => assert!(
                !error.to_string().is_empty(),
                "socket preparation error should preserve diagnostic context"
            ),
        }
    }
}

#[cfg(feature = "infra-ipc")]
impl SystemCommandHandler {
    /// Internal helper to cancel all orders
    async fn cancel_all_orders_internal(
        runtime: &SystemRuntime,
    ) -> Result<engine::execution_worker::CancelDispatchReport, hft_core::HftError> {
        let report = runtime
            .execution_control_handle()
            .cancel_all_orders()
            .await?;
        if !report.is_complete() {
            return Err(hft_core::HftError::Execution(format!(
                "cancellation dispatch incomplete: {} of {} submitted",
                report.submitted.len(),
                report.requested
            )));
        }
        Ok(report)
    }

    /// 追蹤撤單回覆：輪詢 OMS 狀態，直到所有訂單為 Canceled 或超時
    async fn await_cancel_stats(
        runtime: &SystemRuntime,
        targets: &[OrderId],
        timeout_ms: u64,
        mut rx: tokio::sync::broadcast::Receiver<ports::ExecutionEvent>,
    ) -> infra_ipc::messages::CancelStats {
        let requested = targets.len() as u32;
        if requested == 0 {
            return infra_ipc::messages::CancelStats {
                requested,
                succeeded: 0,
                failed: 0,
                details: vec![],
            };
        }

        let mut pending: HashSet<OrderId> = targets.iter().cloned().collect();
        let mut details: HashMap<OrderId, (bool, Option<String>)> = targets
            .iter()
            .map(|id| (id.clone(), (false, None)))
            .collect();

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline && !pending.is_empty() {
            // 等待下一個事件（最長 200ms）
            match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(ports::ExecutionEvent::OrderCanceled { order_id, .. }))
                    if pending.remove(&order_id) =>
                {
                    if let Some(entry) = details.get_mut(&order_id) {
                        *entry = (true, None);
                    }
                }
                _ => {
                    // 無新事件，短暫等待
                }
            }
        }

        // 事件仍未覆蓋者：以 OMS 輔助確認（避免遺漏）
        if !pending.is_empty() {
            let state = {
                let eng = runtime.engine.lock().await;
                eng.export_oms_state()
            };
            for oid in pending.clone() {
                if let Some(rec) = state.get(&oid) {
                    if matches!(rec.status, ports::OrderStatus::Canceled) {
                        pending.remove(&oid);
                        if let Some(entry) = details.get_mut(&oid) {
                            *entry = (true, None);
                        }
                    }
                }
            }
        }

        // 標記超時者原因
        for (_oid, entry) in details.iter_mut() {
            if !entry.0 {
                entry.1 = Some("timeout".to_string());
            }
        }
        let succeeded = details.values().filter(|(ok, _)| *ok).count() as u32;
        let failed = requested.saturating_sub(succeeded);
        let mut detail_vec = Vec::new();
        for (oid, (ok, reason)) in details.into_iter() {
            detail_vec.push(infra_ipc::messages::CancelDetail {
                order_id: oid.0,
                success: ok,
                reason,
            });
        }
        infra_ipc::messages::CancelStats {
            requested,
            succeeded,
            failed,
            details: detail_vec,
        }
    }

    #[inline]
    fn cancel_timeout_ms() -> u64 {
        std::env::var("HFT_CANCEL_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2_000)
    }
}

/// Helper function to start IPC server if enabled
#[cfg(feature = "infra-ipc")]
pub(crate) struct PreparedRuntimeIpc {
    socket_path: String,
    listener: infra_ipc::PreparedIPCListener,
    auth_token: Option<String>,
}

/// Bind and secure the operator socket before any strategy-processing loop can start.
#[cfg(feature = "infra-ipc")]
pub(crate) fn prepare_ipc_server(
    socket_path: Option<String>,
) -> Result<PreparedRuntimeIpc, Box<dyn std::error::Error + Send + Sync>> {
    let socket_path = socket_path.unwrap_or_else(|| infra_ipc::DEFAULT_SOCKET_PATH.to_string());
    let auth_token = std::env::var("HFT_IPC_AUTH_TOKEN").ok();
    if auth_token.is_some() {
        info!("IPC server will require token authentication");
    }
    let listener = infra_ipc::PreparedIPCListener::bind(&socket_path)?;
    Ok(PreparedRuntimeIpc {
        socket_path,
        listener,
        auth_token,
    })
}

/// Attach a prepared socket to the fully wired runtime and start accepting commands.
#[cfg(feature = "infra-ipc")]
pub(crate) fn start_prepared_ipc_server(
    runtime: Arc<Mutex<SystemRuntime>>,
    prepared: PreparedRuntimeIpc,
) -> IpcServerTask {
    let handler = SystemCommandHandler::new(runtime);
    let server =
        infra_ipc::IPCServer::new_with_auth(&prepared.socket_path, handler, prepared.auth_token);

    info!("Starting IPC control server at {}", prepared.socket_path);

    tokio::spawn(async move {
        match server.serve(prepared.listener).await {
            Ok(_) => {
                info!("IPC server stopped normally");
                Ok(())
            }
            Err(e) => {
                error!("IPC server error: {}", e);
                Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            }
        }
    })
}
