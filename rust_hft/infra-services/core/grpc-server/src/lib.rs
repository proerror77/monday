//! HFT gRPC 控制服務
//!
//! 提供遠程控制交易引擎的 gRPC 接口

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use engine::{Engine, ExecutionControlHandle};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::{error, info, warn};

// 引入生成的 proto 代碼
pub mod proto {
    tonic::include_proto!("hft.control");
}

use proto::hft_control_server::{HftControl, HftControlServer};
use proto::*;

/// gRPC 控制服務實現
pub struct HftControlService {
    engine: Arc<Mutex<Engine>>,
    execution_control: Option<ExecutionControlHandle>,
}

impl HftControlService {
    /// 創建新的控制服務
    pub fn new(execution_control: ExecutionControlHandle) -> Self {
        Self::with_execution_control(execution_control)
    }

    pub fn with_execution_control(execution_control: ExecutionControlHandle) -> Self {
        Self {
            engine: execution_control.engine(),
            execution_control: Some(execution_control),
        }
    }

    /// 獲取當前時間戳（微秒）
    fn now_us() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as i64
    }
}

fn grpc_auth_token() -> anyhow::Result<String> {
    let token = std::env::var("HFT_GRPC_AUTH_TOKEN")
        .map_err(|_| anyhow::anyhow!("HFT_GRPC_AUTH_TOKEN is required"))?;
    validate_grpc_auth_token(&token)?;
    Ok(token)
}

fn validate_grpc_auth_token(token: &str) -> anyhow::Result<()> {
    if token.trim() != token || token.len() < 32 {
        anyhow::bail!("HFT_GRPC_AUTH_TOKEN must be at least 32 characters without edge whitespace");
    }
    Ok(())
}

#[allow(clippy::result_large_err)] // Tonic's Interceptor contract returns Status by value.
fn grpc_auth_interceptor(
    token: String,
) -> impl FnMut(Request<()>) -> Result<Request<()>, Status> + Clone {
    let expected_digest = Sha256::digest(format!("Bearer {token}").as_bytes()).to_vec();
    move |request: Request<()>| {
        let presented = request
            .metadata()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let presented_digest = Sha256::digest(presented.as_bytes());
        if presented_digest.as_slice() == expected_digest.as_slice() {
            Ok(request)
        } else {
            Err(Status::unauthenticated("invalid gRPC authorization token"))
        }
    }
}

#[tonic::async_trait]
impl HftControl for HftControlService {
    /// 加載新模型
    async fn load_model(
        &self,
        request: Request<LoadModelRequest>,
    ) -> Result<Response<Ack>, Status> {
        let req = request.into_inner();
        warn!(
            url = %req.url,
            version = %req.version,
            "rejected direct model load; only signed strategy bundles may replace runtime artifacts"
        );
        Err(Status::permission_denied(
            "direct model loading is disabled; deploy a signed StrategyBundle",
        ))
    }

    /// 暫停交易
    async fn pause_trading(&self, _request: Request<Empty>) -> Result<Response<Ack>, Status> {
        info!("收到暫停交易請求");

        self.execution_control
            .as_ref()
            .ok_or_else(|| Status::unavailable("execution worker control is not configured"))?
            .pause_trading()
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;

        Ok(Response::new(Ack {
            ok: true,
            message: "交易已暫停".to_string(),
            timestamp_us: Self::now_us(),
        }))
    }

    /// 恢復交易
    async fn resume_trading(&self, _request: Request<Empty>) -> Result<Response<Ack>, Status> {
        warn!("rejected direct resume; runtime activation requires a signed deployment");
        Err(Status::permission_denied(
            "direct resume is disabled; deploy a signed envelope and restart",
        ))
    }

    /// 進入降頻模式
    async fn enter_degrade_mode(&self, _request: Request<Empty>) -> Result<Response<Ack>, Status> {
        info!("收到進入降頻模式請求");

        let mut engine = self.engine.lock().await;
        engine.enter_degrade_mode();

        Ok(Response::new(Ack {
            ok: true,
            message: "已進入降頻模式".to_string(),
            timestamp_us: Self::now_us(),
        }))
    }

    /// 緊急停止
    async fn emergency_stop(
        &self,
        request: Request<EmergencyStopRequest>,
    ) -> Result<Response<Ack>, Status> {
        let req = request.into_inner();
        error!(
            "收到緊急停止請求: reason={}, cancel_orders={}, close_positions={}",
            req.reason, req.cancel_orders, req.close_positions
        );

        if req.cancel_orders || req.close_positions {
            let control = self
                .execution_control
                .as_ref()
                .ok_or_else(|| Status::unavailable("execution worker control is not configured"))?;
            let report = control
                .emergency_stop(true)
                .await
                .map_err(|error| Status::unavailable(error.to_string()))?;
            if !report.is_complete() {
                return Ok(Response::new(Ack {
                    ok: false,
                    message: format!(
                        "emergency cancellation incomplete: {} of {} submitted",
                        report.submitted.len(),
                        report.requested
                    ),
                    timestamp_us: Self::now_us(),
                }));
            }
            if req.close_positions {
                return Ok(Response::new(Ack {
                    ok: false,
                    message: "orders canceled and intake disabled; position flattening is not implemented"
                        .to_string(),
                    timestamp_us: Self::now_us(),
                }));
            }
        } else {
            self.execution_control
                .as_ref()
                .ok_or_else(|| Status::unavailable("execution worker control is not configured"))?
                .pause_trading()
                .await
                .map_err(|error| Status::unavailable(error.to_string()))?;
        }

        Ok(Response::new(Ack {
            ok: true,
            message: format!("緊急停止執行完成: {}", req.reason),
            timestamp_us: Self::now_us(),
        }))
    }

    /// 獲取系統狀態
    async fn get_status(&self, _request: Request<Empty>) -> Result<Response<SystemStatus>, Status> {
        let engine = self.engine.lock().await;
        let stats = engine.get_statistics();
        let sentinel_stats = engine.get_sentinel_stats();

        let trading_mode = match engine.trading_mode() {
            engine::TradingMode::Normal => "Normal",
            engine::TradingMode::Degraded => "Degraded",
            engine::TradingMode::Paused => "Paused",
            engine::TradingMode::Emergency => "Emergency",
        };

        let active_orders = stats
            .orders_submitted
            .saturating_sub(stats.orders_filled)
            .saturating_sub(stats.orders_canceled)
            .saturating_sub(stats.orders_rejected);

        Ok(Response::new(SystemStatus {
            is_running: stats.is_running,
            trading_mode: trading_mode.to_string(),
            latency_p50_us: sentinel_stats.latency_p50_us as i64,
            latency_p99_us: sentinel_stats.latency_p99_us as i64,
            latency_max_us: 0,
            orders_submitted: stats.orders_submitted as i64,
            orders_filled: stats.orders_filled as i64,
            orders_canceled: stats.orders_canceled as i64,
            orders_rejected: stats.orders_rejected as i64,
            active_orders: active_orders as i64,
            events_received: stats.cycle_count as i64,
            events_processed: stats.execution_events_processed as i64,
            events_dropped: 0,
            ws_connections_active: 0,
            ws_reconnect_count: 0,
            uptime_seconds: stats.uptime_seconds as i64,
            timestamp_us: Self::now_us(),
        }))
    }

    /// 獲取 Portfolio 狀態
    async fn get_portfolio_status(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<PortfolioStatus>, Status> {
        let engine = self.engine.lock().await;
        let state = engine.export_portfolio_state();

        // 從 account_view.positions HashMap 轉換為 Vec<Position>
        let positions: Vec<Position> = state
            .account_view
            .positions
            .iter()
            .map(|(sym, pos)| {
                let qty: f64 = pos.quantity.to_string().parse().unwrap_or(0.0);
                let avg_price: f64 = pos.avg_price.to_string().parse().unwrap_or(0.0);
                let unrealized: f64 = pos.unrealized_pnl.to_string().parse().unwrap_or(0.0);
                Position {
                    symbol: sym.as_str().to_string(),
                    venue: String::new(),
                    quantity: qty,
                    avg_price,
                    unrealized_pnl: unrealized,
                    notional_value: qty.abs() * avg_price,
                }
            })
            .collect();

        let cash: f64 = state
            .account_view
            .cash_balance
            .to_string()
            .parse()
            .unwrap_or(0.0);
        let unrealized: f64 = state
            .account_view
            .unrealized_pnl
            .to_string()
            .parse()
            .unwrap_or(0.0);
        let realized: f64 = state
            .account_view
            .realized_pnl
            .to_string()
            .parse()
            .unwrap_or(0.0);
        let high_water: f64 = state
            .account_view
            .high_water_mark
            .to_string()
            .parse()
            .unwrap_or(0.0);
        let total_equity = state
            .account_view
            .equity()
            .to_string()
            .parse()
            .unwrap_or(0.0);

        Ok(Response::new(PortfolioStatus {
            cash_balance: cash,
            total_equity,
            unrealized_pnl: unrealized,
            realized_pnl: realized,
            high_water_mark: high_water,
            drawdown_pct: state.account_view.drawdown_pct,
            max_drawdown_pct: state.account_view.max_drawdown_pct,
            positions,
            timestamp_us: Self::now_us(),
        }))
    }

    /// 更新風控參數
    async fn update_risk_config(
        &self,
        _request: Request<RiskConfigUpdate>,
    ) -> Result<Response<Ack>, Status> {
        warn!("rejected direct risk mutation; runtime limits are deployment-bound");
        Err(Status::permission_denied(
            "direct risk mutation is disabled; deploy a signed envelope and restart",
        ))
    }

    /// 取消所有訂單
    async fn cancel_all_orders(
        &self,
        request: Request<CancelAllOrdersRequest>,
    ) -> Result<Response<CancelAllOrdersResponse>, Status> {
        let req = request.into_inner();
        warn!(
            "收到取消所有訂單請求: symbol={:?}, venue={:?}",
            req.symbol, req.venue
        );

        let venue = match req.venue.as_deref() {
            Some(value) => Some(
                hft_core::VenueId::from_str(value)
                    .ok_or_else(|| Status::invalid_argument(format!("unknown venue: {value}")))?,
            ),
            None => None,
        };
        let symbol = req.symbol.map(hft_core::Symbol::new);
        let control = self
            .execution_control
            .as_ref()
            .ok_or_else(|| Status::unavailable("execution worker control is not configured"))?;
        let report = control
            .cancel_orders_filtered(symbol, venue)
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;
        let failed_order_ids = report
            .failures
            .iter()
            .map(|failure| failure.order_id.0.clone())
            .collect::<Vec<_>>();

        Ok(Response::new(CancelAllOrdersResponse {
            ok: report.is_complete(),
            orders_canceled: report.submitted.len() as i32,
            orders_failed: report.failures.len() as i32,
            failed_order_ids,
        }))
    }

    /// 健康檢查
    async fn health_check(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<HealthResponse>, Status> {
        let engine = self.engine.lock().await;
        let stats = engine.get_statistics();

        let mode = engine.trading_mode();
        let healthy = stats.is_running
            && matches!(
                mode,
                engine::TradingMode::Normal | engine::TradingMode::Degraded
            );
        let status = if healthy { "healthy" } else { "unhealthy" };

        let mut components = std::collections::HashMap::new();
        components.insert(
            "engine".to_string(),
            ComponentHealth {
                healthy,
                message: format!("mode: {:?}, cycles: {}", mode, stats.cycle_count),
                last_check_us: Self::now_us(),
            },
        );

        Ok(Response::new(HealthResponse {
            healthy,
            status: status.to_string(),
            components,
        }))
    }
}

/// 啟動 gRPC 服務器
pub async fn start_grpc_server(
    execution_control: ExecutionControlHandle,
    addr: std::net::SocketAddr,
) -> anyhow::Result<()> {
    let service = HftControlService::with_execution_control(execution_control);
    let auth_token = grpc_auth_token()?;
    let service = HftControlServer::with_interceptor(service, grpc_auth_interceptor(auth_token));

    info!("啟動 gRPC 控制服務: {}", addr);

    tonic::transport::Server::builder()
        .add_service(service)
        .serve(addr)
        .await?;

    Ok(())
}

/// 在後台啟動 gRPC 服務器
pub fn spawn_grpc_server(
    execution_control: ExecutionControlHandle,
    port: u16,
) -> tokio::task::JoinHandle<()> {
    let addr = format!("0.0.0.0:{}", port).parse().unwrap();

    tokio::spawn(async move {
        if let Err(e) = start_grpc_server(execution_control, addr).await {
            error!("gRPC 服務器錯誤: {}", e);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::execution_worker::{CancelDispatchReport, ControlCommand};

    #[test]
    fn test_now_us() {
        let ts = HftControlService::now_us();
        assert!(ts > 0);
    }

    #[test]
    fn grpc_auth_requires_a_long_exact_bearer_token() {
        assert!(validate_grpc_auth_token("short").is_err());
        assert!(validate_grpc_auth_token(&"a".repeat(32)).is_ok());

        let token = "a".repeat(32);
        let mut interceptor = grpc_auth_interceptor(token.clone());
        assert!(interceptor(Request::new(())).is_err());

        let mut request = Request::new(());
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {token}").parse().expect("metadata value"),
        );
        assert!(interceptor(request).is_ok());
    }

    #[tokio::test]
    async fn direct_model_loading_is_explicitly_disabled() {
        let engine = Arc::new(Mutex::new(Engine::new(engine::EngineConfig::default())));
        let control = ExecutionControlHandle::new(engine, None, false);
        let service = HftControlService::with_execution_control(control);

        let error = service
            .load_model(Request::new(LoadModelRequest {
                url: "file:///tmp/model.onnx".to_string(),
                sha256: "a".repeat(64),
                version: "v1".to_string(),
                model_type: "onnx".to_string(),
            }))
            .await
            .expect_err("direct model loading must be denied");

        assert_eq!(error.code(), tonic::Code::PermissionDenied);
        assert!(error.message().contains("StrategyBundle"));
    }

    #[tokio::test]
    async fn direct_resume_and_risk_mutation_are_explicitly_disabled() {
        let engine = Arc::new(Mutex::new(Engine::new(engine::EngineConfig::default())));
        let control = ExecutionControlHandle::new(engine, None, false);
        let service = HftControlService::with_execution_control(control);

        let resume_error = service
            .resume_trading(Request::new(Empty {}))
            .await
            .expect_err("direct resume must be denied");
        assert_eq!(resume_error.code(), tonic::Code::PermissionDenied);

        let risk_error = service
            .update_risk_config(Request::new(RiskConfigUpdate::default()))
            .await
            .expect_err("direct risk mutation must be denied");
        assert_eq!(risk_error.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn close_positions_is_explicitly_unsupported_after_emergency_cancel() {
        let engine = Arc::new(Mutex::new(Engine::new(engine::EngineConfig::default())));
        let (worker_tx, mut worker_rx) = tokio::sync::mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine.clone(), Some(worker_tx), true);
        let service = HftControlService::with_execution_control(control);
        let worker = tokio::spawn(async move {
            match worker_rx.recv().await.expect("control command") {
                ControlCommand::EnterEmergency { reply, .. } => reply
                    .send(CancelDispatchReport::default())
                    .expect("send cancellation report"),
                _ => panic!("unexpected control command"),
            }
        });

        let response = service
            .emergency_stop(Request::new(EmergencyStopRequest {
                reason: "test".to_string(),
                cancel_orders: true,
                close_positions: true,
            }))
            .await
            .expect("grpc response")
            .into_inner();

        assert!(!response.ok);
        assert!(response.message.contains("not implemented"));
        assert_eq!(
            engine.lock().await.trading_mode(),
            engine::TradingMode::Emergency
        );
        worker.await.expect("worker task");
    }

    #[tokio::test]
    async fn paused_engine_is_not_reported_as_healthy() {
        let mut engine = Engine::new(engine::EngineConfig::default());
        engine.pause_trading();
        let control = ExecutionControlHandle::new(Arc::new(Mutex::new(engine)), None, false);
        let service = HftControlService::new(control);

        let response = service
            .health_check(Request::new(Empty {}))
            .await
            .expect("health response")
            .into_inner();

        assert!(!response.healthy);
        assert_eq!(response.status, "unhealthy");
        assert!(response.components["engine"].message.contains("Paused"));
    }
}
