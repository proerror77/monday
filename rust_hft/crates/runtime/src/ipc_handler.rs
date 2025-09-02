//! IPC command handler for SystemRuntime

use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(feature = "infra-ipc")]
use infra_ipc::{
    Command, Response, ResponseData, SystemStatus, AccountInfo, Position, Order,
    TradingMode, SystemHealth, handlers::CommandHandler
};

#[cfg(feature = "infra-ipc")]
use async_trait::async_trait;

use crate::SystemRuntime;
use hft_core::{Symbol, OrderId};
use tokio::time::{sleep, Duration, Instant};
use std::collections::{HashSet, HashMap};
use tracing::{info, warn, error};

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
                    }
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
                    }
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
                    }
                }
            }
            
            Command::LoadModel { model_path, model_version, sha256_hash } => {
                info!("IPC: Loading model {} version {}", model_path, model_version);
                // TODO: Implement model loading through strategy-dl integration
                Response::Error {
                    message: "Model loading not yet implemented".to_string(),
                    code: Some(501),
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

                if let Some(v) = global_position_limit { new_risk.global_position_limit = v; }
                if let Some(v) = global_notional_limit { new_risk.global_notional_limit = v; }
                if let Some(v) = max_daily_trades { new_risk.max_daily_trades = v; }
                if let Some(v) = max_orders_per_second { new_risk.max_orders_per_second = v; }
                if let Some(v) = staleness_threshold_us { new_risk.staleness_threshold_us = v; }

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
                    Err(e) => Response::Error { message: format!("Failed to update risk: {}", e), code: Some(500) }
                }
            }
            
            Command::GetStatus => {
                let runtime = self.runtime.lock().await;
                let account_view = runtime.get_account_view().await;
                let engine_guard = runtime.engine.lock().await;
                let engine_stats = engine_guard.get_statistics();
                let latency_stats = engine_guard.get_latency_stats();
                drop(engine_guard);

                // 從延遲統計提取關鍵健康指標（若可用）
                let ingestion_us = latency_stats.get(&hft_core::LatencyStage::Ingestion).map(|s| s.mean_micros as u64).unwrap_or(0);
                let execution_us = latency_stats.get(&hft_core::LatencyStage::Execution).map(|s| s.mean_micros as u64).unwrap_or(0);
                let staleness_us = latency_stats.get(&hft_core::LatencyStage::EndToEnd).map(|s| s.p95_micros).unwrap_or(0);

                #[cfg(feature = "infra-metrics")]
                let ring_util = infra_metrics::MetricsRegistry::global().queue_utilization_value();
                #[cfg(not(feature = "infra-metrics"))]
                let ring_util = 0.0f64;
                
                let status = SystemStatus {
                    uptime_seconds: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    trading_mode: if engine_stats.is_running { 
                        TradingMode::Live // TODO: Get actual trading mode from config
                    } else { 
                        TradingMode::Paused 
                    },
                    active_strategies: engine_stats.strategies_count as u32,
                    connected_venues: engine_stats.consumers_count as u32, // Approximation
                    orders_today: engine_stats.orders_submitted as u64,
                    trades_today: engine_stats.orders_filled as u64,
                    current_pnl: account_view.realized_pnl + account_view.unrealized_pnl,
                    max_drawdown: rust_decimal::Decimal::ZERO, // TODO: Track max drawdown
                    model_version: None, // TODO: Get from strategy-dl
                    health: SystemHealth {
                        ingestion_lag_us: ingestion_us,
                        execution_lag_us: execution_us,
                        data_staleness_us: staleness_us,
                        ring_utilization: ring_util,
                        memory_usage_bytes: 0, // TODO: 系統級記憶體監控
                        cpu_usage_pct: 0.0,   // TODO: 系統級 CPU 監控
                    },
                };
                
                Response::Data(ResponseData::Status(status))
            }
            
            Command::GetAccount => {
                let runtime = self.runtime.lock().await;
                let account_view = runtime.get_account_view().await;
                
                let account = AccountInfo {
                    cash_balance: account_view.cash_balance,
                    total_value: account_view.cash_balance + account_view.unrealized_pnl,
                    realized_pnl: account_view.realized_pnl,
                    unrealized_pnl: account_view.unrealized_pnl,
                    max_drawdown: rust_decimal::Decimal::ZERO, // TODO: Track max drawdown
                    open_positions: account_view.positions.len() as u32,
                    open_orders: 0, // TODO: Get from OMS
                };
                
                Response::Data(ResponseData::Account(account))
            }
            
            Command::GetPositions => {
                let runtime = self.runtime.lock().await;
                let account_view = runtime.get_account_view().await;
                
                let positions: Vec<Position> = account_view.positions.iter().map(|(symbol, pos)| {
                    Position {
                        symbol: Symbol(symbol.clone()),
                        quantity: pos.quantity,
                        average_price: pos.average_price,
                        market_value: pos.market_value,
                        unrealized_pnl: pos.unrealized_pnl,
                        realized_pnl: rust_decimal::Decimal::ZERO, // TODO: Track per-position realized PnL
                        last_update: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos() as u64,
                    }
                }).collect();
                
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
                    if matches!(rec.status,
                        oms_core::OrderStatus::New |
                        oms_core::OrderStatus::Acknowledged |
                        oms_core::OrderStatus::PartiallyFilled
                    ) {
                        // 狀態映射
                        let status = match rec.status {
                            oms_core::OrderStatus::New | oms_core::OrderStatus::Acknowledged => infra_ipc::messages::OrderStatus::Pending,
                            oms_core::OrderStatus::PartiallyFilled => infra_ipc::messages::OrderStatus::PartiallyFilled,
                            oms_core::OrderStatus::Filled => infra_ipc::messages::OrderStatus::Filled,
                            oms_core::OrderStatus::Canceled => infra_ipc::messages::OrderStatus::Cancelled,
                            oms_core::OrderStatus::Rejected => infra_ipc::messages::OrderStatus::Rejected,
                            _ => infra_ipc::messages::OrderStatus::Pending,
                        };

                        // 側/型別映射（型別暫不追蹤，先視為 Limit）
                        let side = match rec.side { hft_core::Side::Buy => infra_ipc::messages::OrderSide::Buy, hft_core::Side::Sell => infra_ipc::messages::OrderSide::Sell };
                        let order_type = infra_ipc::messages::OrderType::Limit;

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
                            strategy_id: None,
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
                        if matches!(rec.status, oms_core::OrderStatus::New | oms_core::OrderStatus::Acknowledged | oms_core::OrderStatus::PartiallyFilled) {
                            pairs.push((order_id, rec.symbol));
                        }
                    }
                    pairs
                };

                // 下發取消
                let _ = Self::cancel_all_orders_internal(&runtime).await;
                // 追蹤回覆
                let stats = Self::await_cancel_stats(&runtime, &pairs, Self::cancel_timeout_ms()).await;
                Response::Data(infra_ipc::messages::ResponseData::CancelResult(stats))
            }
            
            Command::CancelOrdersForSymbol { symbol } => {
                info!("IPC: Cancelling orders for symbol {}", symbol.0);
                let runtime = self.runtime.lock().await;
                // 導出未結訂單（該 symbol）
                let pairs = {
                    let eng = runtime.engine.lock().await;
                    let state = eng.export_oms_state();
                    let mut pairs = Vec::new();
                    for (order_id, rec) in state.into_iter() {
                        if rec.symbol.0 == symbol.0 {
                            if matches!(rec.status, oms_core::OrderStatus::New | oms_core::OrderStatus::Acknowledged | oms_core::OrderStatus::PartiallyFilled) {
                                pairs.push((order_id, rec.symbol));
                            }
                        }
                    }
                    pairs
                };
                // 發送控制
                for tx in &runtime.exec_control_txs {
                    let _ = tx.send(engine::execution_worker::ControlCommand::CancelOrders(pairs.clone()));
                }
                // 追蹤回覆
                let stats = Self::await_cancel_stats(&runtime, &pairs, Self::cancel_timeout_ms()).await;
                Response::Data(infra_ipc::messages::ResponseData::CancelResult(stats))
            }

            Command::CancelOrder { order_id, symbol } => {
                info!("IPC: Cancel order {} for symbol {}", order_id, symbol.0);
                let runtime = self.runtime.lock().await;
                let pair = (OrderId(order_id), symbol);
                for tx in &runtime.exec_control_txs {
                    let _ = tx.send(engine::execution_worker::ControlCommand::CancelOrders(vec![pair.clone()]));
                }
                let stats = Self::await_cancel_stats(&runtime, &vec![pair], Self::cancel_timeout_ms()).await;
                Response::Data(infra_ipc::messages::ResponseData::CancelResult(stats))
            }
            
            Command::SetTradingMode { mode } => {
                info!("IPC: Setting trading mode to {:?}", mode);
                // TODO: Implement trading mode changes
                Response::Error {
                    message: "Trading mode changes not yet implemented".to_string(),
                    code: Some(501),
                }
            }
            
            Command::SetStrategyEnabled { strategy_id, enabled } => {
                info!("IPC: Setting strategy {} enabled={}", strategy_id, enabled);
                // TODO: Implement strategy enable/disable
                Response::Error {
                    message: "Strategy enable/disable not yet implemented".to_string(),
                    code: Some(501),
                }
            }
            
            Command::SetSymbolLimits { symbol, max_position, max_notional } => {
                info!("IPC: Setting limits for symbol {}", symbol.0);
                // TODO: Implement symbol limit updates
                Response::Error {
                    message: "Symbol limit updates not yet implemented".to_string(),
                    code: Some(501),
                }
            }
        }
    }
}

#[cfg(feature = "infra-ipc")]
impl SystemCommandHandler {
    /// Internal helper to cancel all orders
    async fn cancel_all_orders_internal(runtime: &SystemRuntime) -> Result<(), Box<dyn std::error::Error>> {
        // 1) 從引擎導出 OMS 狀態，收集未結訂單（New/Ack/PartiallyFilled）
        use oms_core::OrderStatus as OmsStatus;
        let (pairs, txs) = {
            let eng = runtime.engine.lock().await;
            let state = eng.export_oms_state();
            let mut pairs = Vec::new();
            for (order_id, rec) in state.into_iter() {
                if matches!(rec.status, OmsStatus::New | OmsStatus::Acknowledged | OmsStatus::PartiallyFilled) {
                    pairs.push((order_id, rec.symbol));
                }
            }
            (pairs, runtime.exec_control_txs.clone())
        };

        if pairs.is_empty() { return Ok(()); }

        // 2) 通知執行 worker 撤單
        for tx in txs {
            let _ = tx.send(engine::execution_worker::ControlCommand::CancelOrders(pairs.clone()));
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
        if requested == 0 { return infra_ipc::messages::CancelStats { requested, succeeded: 0, failed: 0, details: vec![] }; }

        let mut pending: HashSet<OrderId> = targets.iter().map(|(id, _)| id.clone()).collect();
        let mut details: HashMap<OrderId, (bool, Option<String>)> = targets.iter().map(|(id, _)| (id.clone(), (false, None))).collect();

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
                            if let Some(entry) = details.get_mut(&order_id) { *entry = (true, None); }
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
            let state = { let eng = runtime.engine.lock().await; eng.export_oms_state() };
            for oid in pending.clone() {
                if let Some(rec) = state.get(&oid) {
                    if matches!(rec.status, oms_core::OrderStatus::Canceled) {
                        pending.remove(&oid);
                        if let Some(entry) = details.get_mut(&oid) { *entry = (true, None); }
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
            detail_vec.push(infra_ipc::messages::CancelDetail { order_id: oid.0, success: ok, reason });
        }
        infra_ipc::messages::CancelStats { requested, succeeded, failed, details: detail_vec }
    }

    #[inline]
    fn cancel_timeout_ms() -> u64 {
        std::env::var("HFT_CANCEL_TIMEOUT_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(2_000)
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
    socket_path: Option<String>
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
    _socket_path: Option<String>
) -> tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>> {
    warn!("IPC server disabled - compile with 'infra-ipc' feature to enable");
    tokio::spawn(async move {
        Ok(())
    })
}
