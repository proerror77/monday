//! IPC command handler for SystemRuntime

use std::sync::Arc;
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

use crate::SystemRuntime;
#[cfg(feature = "infra-ipc")]
use hft_core::OrderId;
#[cfg(feature = "infra-ipc")]
use std::collections::{HashMap, HashSet};
#[cfg(feature = "infra-ipc")]
use tokio::time::{Duration, Instant};
#[cfg(not(feature = "infra-ipc"))]
use tracing::warn;
#[cfg(feature = "infra-ipc")]
use tracing::{error, info, warn};

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
                    trading_mode: if !engine_stats.is_running {
                        TradingMode::Paused
                    } else if runtime.config.quotes_only {
                        // 若 quotes_only = true，系統僅接收行情，不執行交易
                        TradingMode::Replay
                    } else if runtime
                        .config
                        .venues
                        .iter()
                        .any(|v| v.execution_mode.as_deref() == Some("Live"))
                    {
                        // 若任一交易所配置為 Live 模式
                        TradingMode::Live
                    } else {
                        // 預設為 Paper 模式
                        TradingMode::Paper
                    },
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

            Command::SetTradingMode { mode } => {
                info!("IPC: Setting trading mode to {:?}", mode);
                let runtime = self.runtime.lock().await;

                match mode {
                    TradingMode::Live | TradingMode::Paper => Response::Error {
                        message:
                            "direct resume is disabled; deploy a signed envelope and restart"
                                .to_string(),
                        code: Some(403),
                    },
                    TradingMode::Replay => match runtime
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
pub fn start_ipc_server(
    runtime: Arc<Mutex<SystemRuntime>>,
    socket_path: Option<String>,
) -> tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>> {
    let socket_path = socket_path.unwrap_or_else(|| infra_ipc::DEFAULT_SOCKET_PATH.to_string());
    let handler = SystemCommandHandler::new(runtime);

    // Check for authentication token from environment
    let auth_token = std::env::var("HFT_IPC_AUTH_TOKEN").ok();
    if auth_token.is_some() {
        info!("IPC server will require token authentication");
    }

    let server = infra_ipc::IPCServer::new_with_auth(&socket_path, handler, auth_token);

    info!("Starting IPC control server at {}", socket_path);

    // Start server in background task
    tokio::spawn(async move {
        match server.start().await {
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

// Stub implementation when IPC feature is disabled
#[cfg(not(feature = "infra-ipc"))]
pub fn start_ipc_server(
    _runtime: Arc<Mutex<SystemRuntime>>,
    _socket_path: Option<String>,
) -> tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>> {
    warn!("IPC server disabled - compile with 'infra-ipc' feature to enable");
    tokio::spawn(async move { Ok(()) })
}
