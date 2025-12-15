//! HFT gRPC 控制服務
//!
//! 提供遠程控制交易引擎的 gRPC 接口

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use engine::Engine;
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
}

impl HftControlService {
    /// 創建新的控制服務
    pub fn new(engine: Arc<Mutex<Engine>>) -> Self {
        Self { engine }
    }

    /// 創建 gRPC 服務器
    pub fn into_server(self) -> HftControlServer<Self> {
        HftControlServer::new(self)
    }

    /// 獲取當前時間戳（微秒）
    fn now_us() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as i64
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
        info!(
            "收到模型加載請求: url={}, version={}, type={}",
            req.url, req.version, req.model_type
        );

        // TODO: 實現模型加載邏輯
        // 1. 下載模型文件
        // 2. 驗證 SHA256
        // 3. 加載到推理引擎

        Ok(Response::new(Ack {
            ok: true,
            message: format!("模型 {} 加載成功", req.version),
            timestamp_us: Self::now_us(),
        }))
    }

    /// 暫停交易
    async fn pause_trading(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Ack>, Status> {
        info!("收到暫停交易請求");

        let mut engine = self.engine.lock().await;
        engine.pause_trading();

        Ok(Response::new(Ack {
            ok: true,
            message: "交易已暫停".to_string(),
            timestamp_us: Self::now_us(),
        }))
    }

    /// 恢復交易
    async fn resume_trading(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Ack>, Status> {
        info!("收到恢復交易請求");

        let mut engine = self.engine.lock().await;
        engine.resume_trading();

        Ok(Response::new(Ack {
            ok: true,
            message: "交易已恢復".to_string(),
            timestamp_us: Self::now_us(),
        }))
    }

    /// 進入降頻模式
    async fn enter_degrade_mode(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Ack>, Status> {
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

        let mut engine = self.engine.lock().await;

        if req.cancel_orders || req.close_positions {
            let orders_to_cancel = engine.emergency_exit();
            error!("緊急停止: 需要取消 {} 個訂單", orders_to_cancel.len());
        } else {
            engine.pause_trading();
        }

        Ok(Response::new(Ack {
            ok: true,
            message: format!("緊急停止執行完成: {}", req.reason),
            timestamp_us: Self::now_us(),
        }))
    }

    /// 獲取系統狀態
    async fn get_status(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<SystemStatus>, Status> {
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

        let cash: f64 = state.account_view.cash_balance.to_string().parse().unwrap_or(0.0);
        let unrealized: f64 = state.account_view.unrealized_pnl.to_string().parse().unwrap_or(0.0);
        let realized: f64 = state.account_view.realized_pnl.to_string().parse().unwrap_or(0.0);
        let high_water: f64 = state.account_view.high_water_mark.to_string().parse().unwrap_or(0.0);
        let total_equity = cash + unrealized;

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
        request: Request<RiskConfigUpdate>,
    ) -> Result<Response<Ack>, Status> {
        let req = request.into_inner();
        info!("收到風控參數更新請求: {:?}", req);

        // 轉換 proto RiskConfigUpdate 到 ports RiskConfigUpdate
        let update = ports::RiskConfigUpdate {
            max_drawdown_pct: req.max_drawdown_pct,
            max_position_usd: req.max_position_usd,
            max_order_size_usd: req.max_order_size_usd,
            latency_threshold_us: req.latency_threshold_us,
            max_orders_per_second: req.max_orders_per_second,
        };

        let mut engine = self.engine.lock().await;
        match engine.update_risk_config(update) {
            Ok(()) => {
                info!("風控參數更新成功");
                Ok(Response::new(Ack {
                    ok: true,
                    message: "風控參數已更新".to_string(),
                    timestamp_us: Self::now_us(),
                }))
            }
            Err(e) => {
                error!("風控參數更新失敗: {}", e);
                Ok(Response::new(Ack {
                    ok: false,
                    message: format!("風控參數更新失敗: {}", e),
                    timestamp_us: Self::now_us(),
                }))
            }
        }
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

        let mut engine = self.engine.lock().await;

        // 使用 emergency_exit 取消所有訂單
        let orders_to_cancel = engine.emergency_exit();
        let count = orders_to_cancel.len() as i32;

        Ok(Response::new(CancelAllOrdersResponse {
            ok: true,
            orders_canceled: count,
            orders_failed: 0,
            failed_order_ids: vec![],
        }))
    }

    /// 健康檢查
    async fn health_check(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<HealthResponse>, Status> {
        let engine = self.engine.lock().await;
        let stats = engine.get_statistics();

        let healthy = stats.is_running;
        let status = if healthy { "healthy" } else { "unhealthy" };

        let mut components = std::collections::HashMap::new();
        components.insert(
            "engine".to_string(),
            ComponentHealth {
                healthy: stats.is_running,
                message: format!("cycles: {}", stats.cycle_count),
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
    engine: Arc<Mutex<Engine>>,
    addr: std::net::SocketAddr,
) -> anyhow::Result<()> {
    let service = HftControlService::new(engine);

    info!("啟動 gRPC 控制服務: {}", addr);

    tonic::transport::Server::builder()
        .add_service(service.into_server())
        .serve(addr)
        .await?;

    Ok(())
}

/// 在後台啟動 gRPC 服務器
pub fn spawn_grpc_server(
    engine: Arc<Mutex<Engine>>,
    port: u16,
) -> tokio::task::JoinHandle<()> {
    let addr = format!("0.0.0.0:{}", port).parse().unwrap();

    tokio::spawn(async move {
        if let Err(e) = start_grpc_server(engine, addr).await {
            error!("gRPC 服務器錯誤: {}", e);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_now_us() {
        let ts = HftControlService::now_us();
        assert!(ts > 0);
    }
}
