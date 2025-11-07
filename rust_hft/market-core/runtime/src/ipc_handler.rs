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
use sysinfo::{System, SystemExt, CpuExt};

use crate::SystemRuntime;
#[cfg(feature = "infra-ipc")]
use hft_core::{OrderId, Symbol};
#[cfg(feature = "infra-ipc")]
use shared_config::StrategyParams as SharedStrategyParams;
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
            Command::Start => {
                info!("IPC: Starting trading system");
                let mut runtime = self.runtime.lock().await;
                match runtime.start().await {
                    Ok(_) => Response::Ok,
                    Err(e) => Response::Error {
                        message: format!("Failed to start system: {}", e),
                        code: Some(500),
                    },
                }
            }

            Command::Stop => {
                info!("IPC: Stopping trading system");
                let mut runtime = self.runtime.lock().await;
                match runtime.stop().await {
                    Ok(_) => Response::Ok,
                    Err(e) => Response::Error {
                        message: format!("Failed to stop system: {}", e),
                        code: Some(500),
                    },
                }
            }

            Command::EmergencyStop => {
                warn!("IPC: Emergency stop requested");
                let mut runtime = self.runtime.lock().await;

                // First cancel all orders
                match Self::cancel_all_orders_internal(&runtime).await {
                    Ok(_) => info!("All orders cancelled as part of emergency stop"),
                    Err(e) => error!("Failed to cancel orders during emergency stop: {}", e),
                }

                // Then stop the system
                match runtime.stop().await {
                    Ok(_) => Response::Ok,
                    Err(e) => Response::Error {
                        message: format!("Emergency stop failed: {}", e),
                        code: Some(500),
                    },
                }
            }

            Command::LoadModel {
                model_path,
                model_version,
                sha256_hash,
            } => {
                info!(
                    "IPC: Loading model {} version {}",
                    model_path, model_version
                );

                #[cfg(feature = "strategy-dl")]
                {
                    use std::path::PathBuf;

                    // 验证 SHA256（如果提供）
                    if let Some(expected_hash) = &sha256_hash {
                        let path = PathBuf::from(&model_path);
                        if path.exists() {
                            // 计算文件哈希
                            match Self::calculate_sha256(&path) {
                                Ok(actual_hash) => {
                                    if &actual_hash != expected_hash {
                                        return Response::Error {
                                            message: format!(
                                                "SHA256 校验失败: 期望 {}, 实际 {}",
                                                expected_hash, actual_hash
                                            ),
                                            code: Some(400),
                                        };
                                    }
                                }
                                Err(e) => {
                                    return Response::Error {
                                        message: format!("无法计算文件哈希: {}", e),
                                        code: Some(500),
                                    };
                                }
                            }
                        } else {
                            return Response::Error {
                                message: format!("模型文件不存在: {}", model_path),
                                code: Some(404),
                            };
                        }
                    }

                    // 查找 DL 策略实例并重载模型
                    let mut runtime = self.runtime.lock().await;
                    let mut engine = runtime.engine.lock().await;

                    let mut loaded_count = 0;
                    let strategy_ids = engine.strategy_instance_ids();

                    for strategy_id in strategy_ids {
                        // 通过策略 ID 获取策略实例的可变引用
                        if let Some(strategy) = engine.get_strategy_mut_by_id(&strategy_id) {
                            // 尝试将策略向下转型为 DlStrategy
                            if let Some(dl_strategy) = strategy.as_any_mut().downcast_mut::<hft_strategy_dl::DlStrategy>() {
                                match dl_strategy.load_model(PathBuf::from(&model_path), model_version.clone()).await {
                                    Ok(_) => {
                                        info!("策略 {} 成功加载模型: {}", strategy_id, model_path);
                                        loaded_count += 1;
                                    }
                                    Err(e) => {
                                        error!("策略 {} 模型加载失败: {}", strategy_id, e);
                                        return Response::Error {
                                            message: format!("策略 {} 模型加载失败: {}", strategy_id, e),
                                            code: Some(500),
                                        };
                                    }
                                }
                            }
                        }
                    }

                    if loaded_count == 0 {
                        Response::Error {
                            message: "未找到 DL 策略实例".to_string(),
                            code: Some(404),
                        }
                    } else {
                        info!("成功为 {} 个 DL 策略加载模型", loaded_count);
                        Response::Ok
                    }
                }

                #[cfg(not(feature = "strategy-dl"))]
                {
                    Response::Error {
                        message: "DL 策略功能未启用，请使用 --features strategy-dl 编译".to_string(),
                        code: Some(501),
                    }
                }
            }

            Command::UpdateRisk {
                global_position_limit,
                global_notional_limit,
                max_daily_trades,
                max_orders_per_second,
                staleness_threshold_us,
                strategy_overrides,
            } => {
                info!("IPC: Updating risk parameters");
                let mut runtime = self.runtime.lock().await;
                // 基於當前配置建立新的風控配置（僅覆蓋提供的欄位）
                let mut new_risk = runtime.config.risk.clone();

                if let Some(v) = global_position_limit {
                    new_risk.global_position_limit = v;
                }
                if let Some(v) = global_notional_limit {
                    new_risk.global_notional_limit = v;
                }
                if let Some(v) = max_daily_trades {
                    new_risk.max_daily_trades = v;
                }
                if let Some(v) = max_orders_per_second {
                    new_risk.max_orders_per_second = v;
                }
                if let Some(v) = staleness_threshold_us {
                    new_risk.staleness_threshold_us = v;
                }

                // 轉換策略覆蓋（若有）
                if !strategy_overrides.is_empty() {
                    let mut merged = new_risk.strategy_overrides.clone();
                    for (k, ov) in strategy_overrides.into_iter() {
                        let converted = crate::ipc_handler::convert_strategy_override(ov);
                        merged.insert(k, converted);
                    }
                    new_risk.strategy_overrides = merged;
                }

                // 應用更新
                match runtime.update_risk_config(new_risk).await {
                    Ok(_) => Response::Ok,
                    Err(e) => Response::Error {
                        message: format!("Failed to update risk: {}", e),
                        code: Some(500),
                    },
                }
            }

            Command::GetStatus => {
                let runtime = self.runtime.lock().await;
                let account_view = runtime.get_account_view().await;
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
                    } else if runtime.config.venues.iter().any(|v| {
                        v.execution_mode.as_deref() == Some("Live")
                    }) {
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
                        .and_then(|m| rust_decimal::Decimal::from_f64_retain(m.max_drawdown))
                        .unwrap_or(rust_decimal::Decimal::ZERO),
                    model_version: {
                        // 从 DL 策略获取模型版本
                        #[cfg(feature = "strategy-dl")]
                        {
                            let strategy_ids = engine_guard.strategy_instance_ids();
                            let mut version = None;
                            for strategy_id in strategy_ids {
                                if let Some(strategy) = engine_guard.get_strategy_by_id(&strategy_id) {
                                    if let Some(dl_strategy) = strategy.as_any().downcast_ref::<hft_strategy_dl::DlStrategy>() {
                                        version = dl_strategy.get_model_version();
                                        break; // 使用第一个找到的 DL 策略的模型版本
                                    }
                                }
                            }
                            version
                        }
                        #[cfg(not(feature = "strategy-dl"))]
                        None
                    },
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
                            oms_core::OrderStatus::New
                                | oms_core::OrderStatus::Acknowledged
                                | oms_core::OrderStatus::PartiallyFilled
                        )
                    })
                    .count() as u32;

                let account = AccountInfo {
                    cash_balance: account_view.cash_balance,
                    total_value: account_view.cash_balance + account_view.unrealized_pnl,
                    realized_pnl: account_view.realized_pnl,
                    unrealized_pnl: account_view.unrealized_pnl,
                    max_drawdown: risk_metrics
                        .as_ref()
                        .and_then(|m| rust_decimal::Decimal::from_f64_retain(m.max_drawdown))
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
                            symbol: Symbol::from(symbol.clone()),
                            quantity: pos.quantity,
                            average_price: pos.average_price,
                            market_value: pos.market_value,
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
                        oms_core::OrderStatus::New
                            | oms_core::OrderStatus::Acknowledged
                            | oms_core::OrderStatus::PartiallyFilled
                    ) {
                        // 狀態映射
                        let status = match rec.status {
                            oms_core::OrderStatus::New | oms_core::OrderStatus::Acknowledged => {
                                infra_ipc::messages::OrderStatus::Pending
                            }
                            oms_core::OrderStatus::PartiallyFilled => {
                                infra_ipc::messages::OrderStatus::PartiallyFilled
                            }
                            oms_core::OrderStatus::Filled => {
                                infra_ipc::messages::OrderStatus::Filled
                            }
                            oms_core::OrderStatus::Canceled => {
                                infra_ipc::messages::OrderStatus::Cancelled
                            }
                            oms_core::OrderStatus::Rejected => {
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
                // 收集目標
                let pairs = {
                    let eng = runtime.engine.lock().await;
                    let state = eng.export_oms_state();
                    let mut pairs = Vec::new();
                    for (order_id, rec) in state.into_iter() {
                        if matches!(
                            rec.status,
                            oms_core::OrderStatus::New
                                | oms_core::OrderStatus::Acknowledged
                                | oms_core::OrderStatus::PartiallyFilled
                        ) {
                            pairs.push((order_id, rec.symbol));
                        }
                    }
                    pairs
                };

                // 下發取消
                let _ = Self::cancel_all_orders_internal(&runtime).await;
                // 追蹤回覆
                let stats =
                    Self::await_cancel_stats(&runtime, &pairs, Self::cancel_timeout_ms()).await;
                Response::Data(infra_ipc::messages::ResponseData::CancelResult(stats))
            }

            Command::CancelOrdersForSymbol { symbol } => {
                info!("IPC: Cancelling orders for symbol {}", symbol.as_str());
                let runtime = self.runtime.lock().await;
                // 導出未結訂單（該 symbol）
                let pairs = {
                    let eng = runtime.engine.lock().await;
                    let state = eng.export_oms_state();
                    let mut pairs = Vec::new();
                    for (order_id, rec) in state.into_iter() {
                        if rec.symbol.as_str() == symbol.as_str() {
                            if matches!(
                                rec.status,
                                oms_core::OrderStatus::New
                                    | oms_core::OrderStatus::Acknowledged
                                    | oms_core::OrderStatus::PartiallyFilled
                            ) {
                                pairs.push((order_id, rec.symbol));
                            }
                        }
                    }
                    pairs
                };
                // 發送控制
                for tx in &runtime.exec_control_txs {
                    let _ = tx.send(engine::execution_worker::ControlCommand::CancelOrders(
                        pairs.clone(),
                    ));
                }
                // 追蹤回覆
                let stats =
                    Self::await_cancel_stats(&runtime, &pairs, Self::cancel_timeout_ms()).await;
                Response::Data(infra_ipc::messages::ResponseData::CancelResult(stats))
            }

            Command::CancelOrdersForStrategy { strategy_id } => {
                info!("IPC: Cancelling orders for strategy {}", strategy_id);
                let runtime = self.runtime.lock().await;
                // 收集該策略未結訂單 (order_id, symbol)
                let pairs = {
                    let eng = runtime.engine.lock().await;
                    eng.open_order_pairs_by_strategy(&strategy_id)
                };
                // 發送控制
                for tx in &runtime.exec_control_txs {
                    let _ = tx.send(engine::execution_worker::ControlCommand::CancelOrders(
                        pairs.clone(),
                    ));
                }
                // 等待撤單統計
                let stats =
                    Self::await_cancel_stats(&runtime, &pairs, Self::cancel_timeout_ms()).await;
                Response::Data(infra_ipc::messages::ResponseData::CancelResult(stats))
            }

            Command::CancelOrder { order_id, symbol } => {
                info!(
                    "IPC: Cancel order {} for symbol {}",
                    order_id,
                    symbol.as_str()
                );
                let runtime = self.runtime.lock().await;
                let pair = (OrderId(order_id), symbol);
                for tx in &runtime.exec_control_txs {
                    let _ = tx.send(engine::execution_worker::ControlCommand::CancelOrders(
                        vec![pair.clone()],
                    ));
                }
                let stats =
                    Self::await_cancel_stats(&runtime, &vec![pair], Self::cancel_timeout_ms())
                        .await;
                Response::Data(infra_ipc::messages::ResponseData::CancelResult(stats))
            }

            Command::SetTradingMode { mode } => {
                info!("IPC: Setting trading mode to {:?}", mode);
                let mut runtime = self.runtime.lock().await;

                // 更新配置中的 quotes_only 标志
                match mode {
                    TradingMode::Live | TradingMode::Paper => {
                        runtime.config.quotes_only = false;
                        // 确保引擎处于运行状态
                        let mut engine = runtime.engine.lock().await;
                        if !engine.get_statistics().is_running {
                            // 如果引擎已停止，需要重新启动
                            drop(engine);
                            match runtime.start().await {
                                Ok(_) => {
                                    info!("交易模式已切换至 {:?}", mode);
                                    Response::Ok
                                }
                                Err(e) => Response::Error {
                                    message: format!("启动交易引擎失败: {}", e),
                                    code: Some(500),
                                }
                            }
                        } else {
                            drop(engine);
                            info!("交易模式已切换至 {:?}", mode);
                            Response::Ok
                        }
                    }
                    TradingMode::Replay => {
                        // Replay 模式：仅接收行情，不执行交易
                        runtime.config.quotes_only = true;

                        // 取消所有未结订单
                        match Self::cancel_all_orders_internal(&runtime).await {
                            Ok(_) => {
                                info!("已取消所有订单，切换至 Replay 模式");
                                Response::Ok
                            }
                            Err(e) => Response::Error {
                                message: format!("取消订单失败: {}", e),
                                code: Some(500),
                            }
                        }
                    }
                    TradingMode::Paused => {
                        // 暂停模式：停止引擎
                        runtime.config.quotes_only = true;
                        match runtime.stop().await {
                            Ok(_) => {
                                info!("系统已暂停");
                                Response::Ok
                            }
                            Err(e) => Response::Error {
                                message: format!("暂停系统失败: {}", e),
                                code: Some(500),
                            }
                        }
                    }
                }
            }

            Command::SetStrategyEnabled {
                strategy_id,
                enabled,
            } => {
                info!("IPC: Setting strategy {} enabled={}", strategy_id, enabled);
                let runtime = self.runtime.lock().await;

                // 通过引擎设置策略启用/禁用状态
                let mut engine = runtime.engine.lock().await;
                match engine.set_strategy_enabled(&strategy_id, enabled) {
                    Ok(_) => {
                        info!("策略 {} 已{}启用", strategy_id, if enabled { "" } else { "禁" });

                        // 如果禁用策略，取消该策略的所有未结订单
                        if !enabled {
                            drop(engine);
                            let cancelled_count = runtime.cancel_orders_for_strategy(&strategy_id).await;
                            info!("已取消策略 {} 的 {} 个未结订单", strategy_id, cancelled_count);
                        }

                        Response::Ok
                    }
                    Err(e) => Response::Error {
                        message: format!("设置策略状态失败: {}", e),
                        code: Some(500),
                    }
                }
            }

            Command::SetSymbolLimits {
                symbol,
                max_position,
                max_notional,
            } => {
                info!("IPC: Setting limits for symbol {}", symbol.as_str());
                let mut runtime = self.runtime.lock().await;

                // 更新风控配置中的商品限制
                // 这需要通过策略覆盖来实现，因为风控系统是以策略为维度的
                // 我们需要为所有涉及该商品的策略应用限制

                // 查找所有使用该商品的策略
                let affected_strategies: Vec<String> = runtime
                    .config
                    .strategies
                    .iter()
                    .filter(|s| s.symbols.iter().any(|sym| sym.as_str() == symbol.as_str()))
                    .map(|s| s.name.clone())
                    .collect();

                if affected_strategies.is_empty() {
                    return Response::Error {
                        message: format!("未找到使用商品 {} 的策略", symbol.as_str()),
                        code: Some(404),
                    };
                }

                // 更新每个受影响策略的风控覆盖
                for strategy_name in &affected_strategies {
                    let override_entry = runtime
                        .config
                        .risk
                        .strategy_overrides
                        .entry(strategy_name.clone())
                        .or_insert_with(|| crate::StrategyRiskOverride {
                            max_position: None,
                            max_notional: None,
                            max_orders_per_second: None,
                            order_cooldown_ms: None,
                            staleness_threshold_us: None,
                            max_daily_loss: None,
                            aggressive_mode: None,
                            enhanced_overrides: None,
                        });

                    // 更新限制（仅覆盖提供的值）
                    if let Some(pos_limit) = max_position {
                        override_entry.max_position = Some(pos_limit);
                    }
                    if let Some(notional_limit) = max_notional {
                        override_entry.max_notional = Some(notional_limit);
                    }
                }

                // 重新创建风控管理器并应用到引擎
                let new_manager = crate::RiskManagerFactory::create_strategy_aware_risk_manager(
                    &runtime.config.risk,
                );

                let mut engine = runtime.engine.lock().await;
                engine.register_risk_manager_boxed(new_manager);
                drop(engine);

                info!(
                    "已为商品 {} 更新限制（影响策略: {:?}）",
                    symbol.as_str(),
                    affected_strategies
                );
                Response::Ok
            }

            Command::UpdateStrategyParams {
                strategy_id,
                params,
            } => {
                info!("IPC: Updating strategy params: {}", strategy_id);
                let mut runtime = self.runtime.lock().await;
                match runtime.update_strategy_params(&strategy_id, params).await {
                    Ok(_) => Response::Ok,
                    Err(e) => Response::Error {
                        message: format!("Failed to update strategy params: {}", e),
                        code: Some(500),
                    },
                }
            }
        }
    }
}

#[cfg(feature = "infra-ipc")]
impl SystemCommandHandler {
    /// 计算文件的 SHA256 哈希
    #[cfg(feature = "strategy-dl")]
    fn calculate_sha256(path: &std::path::Path) -> Result<String, std::io::Error> {
        use sha2::{Sha256, Digest};
        use std::io::Read;

        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192];

        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }

        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Internal helper to cancel all orders
    async fn cancel_all_orders_internal(
        runtime: &SystemRuntime,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 1) 從引擎導出 OMS 狀態，收集未結訂單（New/Ack/PartiallyFilled）
        use oms_core::OrderStatus as OmsStatus;
        let (pairs, txs) = {
            let eng = runtime.engine.lock().await;
            let state = eng.export_oms_state();
            let mut pairs = Vec::new();
            for (order_id, rec) in state.into_iter() {
                if matches!(
                    rec.status,
                    OmsStatus::New | OmsStatus::Acknowledged | OmsStatus::PartiallyFilled
                ) {
                    pairs.push((order_id, rec.symbol));
                }
            }
            (pairs, runtime.exec_control_txs.clone())
        };

        if pairs.is_empty() {
            return Ok(());
        }

        // 2) 通知執行 worker 撤單
        for tx in txs {
            let _ = tx.send(engine::execution_worker::ControlCommand::CancelOrders(
                pairs.clone(),
            ));
        }
        Ok(())
    }

    /// 追蹤撤單回覆：輪詢 OMS 狀態，直到所有訂單為 Canceled 或超時
    async fn await_cancel_stats(
        runtime: &SystemRuntime,
        targets: &[(OrderId, Symbol)],
        timeout_ms: u64,
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

        let mut pending: HashSet<OrderId> = targets.iter().map(|(id, _)| id.clone()).collect();
        let mut details: HashMap<OrderId, (bool, Option<String>)> = targets
            .iter()
            .map(|(id, _)| (id.clone(), (false, None)))
            .collect();

        // 事件驅動：訂閱執行事件流
        let mut rx = {
            let eng = runtime.engine.lock().await;
            eng.subscribe_execution_events()
        };

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline && !pending.is_empty() {
            // 等待下一個事件（最長 200ms）
            match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(ev)) => {
                    if let ports::ExecutionEvent::OrderCanceled { order_id, .. } = ev {
                        if pending.remove(&order_id) {
                            if let Some(entry) = details.get_mut(&order_id) {
                                *entry = (true, None);
                            }
                        }
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
                    if matches!(rec.status, oms_core::OrderStatus::Canceled) {
                        pending.remove(&oid);
                        if let Some(entry) = details.get_mut(&oid) {
                            *entry = (true, None);
                        }
                    }
                }
            }
        }

        // 標記超時者原因
        for (oid, entry) in details.iter_mut() {
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

#[cfg(feature = "infra-ipc")]
fn convert_strategy_override(ov: hft_ipc::StrategyRiskConfig) -> crate::StrategyRiskOverride {
    crate::StrategyRiskOverride {
        max_position: ov.max_position,
        max_notional: ov.max_notional,
        max_orders_per_second: ov.max_orders_per_second,
        order_cooldown_ms: None,
        staleness_threshold_us: None,
        max_daily_loss: None,
        aggressive_mode: None,
        enhanced_overrides: None,
    }
}

/// Helper function to start IPC server if enabled
#[cfg(feature = "infra-ipc")]
pub fn start_ipc_server(
    runtime: Arc<Mutex<SystemRuntime>>,
    socket_path: Option<String>,
) -> tokio::task::JoinHandle<Result<(), infra_ipc::IPCError>> {
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
                Err(e)
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
