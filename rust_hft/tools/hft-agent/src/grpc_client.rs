//! gRPC client wrapper for HFT Control Service

use crate::config::GrpcConfig;
use anyhow::{Context, Result};
use std::time::Duration;
use tonic::transport::Channel;
use tracing::{debug, info, warn};

// Include generated protobuf code
pub mod proto {
    tonic::include_proto!("hft.control");
}

use proto::hft_control_client::HftControlClient;
use proto::{
    CancelAllOrdersRequest, CancelAllOrdersResponse, Empty, EmergencyStopRequest, HealthResponse,
    LoadModelRequest, PortfolioStatus, RiskConfigUpdate, SystemStatus,
};

/// Wrapper around gRPC client for HFT Control Service
pub struct HftClient {
    client: HftControlClient<Channel>,
}

impl HftClient {
    /// Connect to the gRPC server
    pub async fn connect(config: &GrpcConfig) -> Result<Self> {
        info!(address = %config.address, "Connecting to HFT Control Service");

        let channel = Channel::from_shared(config.address.clone())
            .context("Invalid gRPC address")?
            .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
            .timeout(Duration::from_secs(config.request_timeout_secs))
            .connect()
            .await
            .context("Failed to connect to gRPC server")?;

        info!("Connected to HFT Control Service");
        Ok(Self {
            client: HftControlClient::new(channel),
        })
    }

    /// Get system status
    pub async fn get_status(&mut self) -> Result<SystemStatus> {
        debug!("Getting system status");
        let response = self
            .client
            .get_status(Empty {})
            .await
            .context("Failed to get system status")?;
        Ok(response.into_inner())
    }

    /// Get portfolio status
    pub async fn get_portfolio_status(&mut self) -> Result<PortfolioStatus> {
        debug!("Getting portfolio status");
        let response = self
            .client
            .get_portfolio_status(Empty {})
            .await
            .context("Failed to get portfolio status")?;
        Ok(response.into_inner())
    }

    /// Health check
    pub async fn health_check(&mut self) -> Result<HealthResponse> {
        debug!("Performing health check");
        let response = self
            .client
            .health_check(Empty {})
            .await
            .context("Failed to perform health check")?;
        Ok(response.into_inner())
    }

    /// Pause trading
    pub async fn pause_trading(&mut self) -> Result<String> {
        warn!("Pausing trading");
        let response = self
            .client
            .pause_trading(Empty {})
            .await
            .context("Failed to pause trading")?;
        let ack = response.into_inner();
        if ack.ok {
            Ok(ack.message)
        } else {
            anyhow::bail!("Pause trading failed: {}", ack.message)
        }
    }

    /// Resume trading
    pub async fn resume_trading(&mut self) -> Result<String> {
        info!("Resuming trading");
        let response = self
            .client
            .resume_trading(Empty {})
            .await
            .context("Failed to resume trading")?;
        let ack = response.into_inner();
        if ack.ok {
            Ok(ack.message)
        } else {
            anyhow::bail!("Resume trading failed: {}", ack.message)
        }
    }

    /// Enter degrade mode
    pub async fn enter_degrade_mode(&mut self) -> Result<String> {
        warn!("Entering degrade mode");
        let response = self
            .client
            .enter_degrade_mode(Empty {})
            .await
            .context("Failed to enter degrade mode")?;
        let ack = response.into_inner();
        if ack.ok {
            Ok(ack.message)
        } else {
            anyhow::bail!("Enter degrade mode failed: {}", ack.message)
        }
    }

    /// Emergency stop
    pub async fn emergency_stop(
        &mut self,
        reason: String,
        cancel_orders: bool,
        close_positions: bool,
    ) -> Result<String> {
        warn!(
            reason = %reason,
            cancel_orders = cancel_orders,
            close_positions = close_positions,
            "EMERGENCY STOP"
        );
        let response = self
            .client
            .emergency_stop(EmergencyStopRequest {
                reason,
                cancel_orders,
                close_positions,
            })
            .await
            .context("Failed to execute emergency stop")?;
        let ack = response.into_inner();
        if ack.ok {
            Ok(ack.message)
        } else {
            anyhow::bail!("Emergency stop failed: {}", ack.message)
        }
    }

    /// Cancel all orders
    pub async fn cancel_all_orders(
        &mut self,
        symbol: Option<String>,
        venue: Option<String>,
    ) -> Result<CancelAllOrdersResponse> {
        warn!(symbol = ?symbol, venue = ?venue, "Canceling all orders");
        let response = self
            .client
            .cancel_all_orders(CancelAllOrdersRequest { symbol, venue })
            .await
            .context("Failed to cancel all orders")?;
        Ok(response.into_inner())
    }

    /// Update risk config
    pub async fn update_risk_config(
        &mut self,
        max_drawdown_pct: Option<f64>,
        max_position_usd: Option<f64>,
        max_order_size_usd: Option<f64>,
        latency_threshold_us: Option<i64>,
        max_orders_per_second: Option<i32>,
    ) -> Result<String> {
        info!(
            max_drawdown_pct = ?max_drawdown_pct,
            max_position_usd = ?max_position_usd,
            "Updating risk config"
        );
        let response = self
            .client
            .update_risk_config(RiskConfigUpdate {
                max_drawdown_pct,
                max_position_usd,
                max_order_size_usd,
                latency_threshold_us,
                max_orders_per_second,
            })
            .await
            .context("Failed to update risk config")?;
        let ack = response.into_inner();
        if ack.ok {
            Ok(ack.message)
        } else {
            anyhow::bail!("Update risk config failed: {}", ack.message)
        }
    }

    /// Load new model
    pub async fn load_model(
        &mut self,
        url: String,
        sha256: String,
        version: String,
        model_type: String,
    ) -> Result<String> {
        info!(url = %url, version = %version, "Loading new model");
        let response = self
            .client
            .load_model(LoadModelRequest {
                url,
                sha256,
                version,
                model_type,
            })
            .await
            .context("Failed to load model")?;
        let ack = response.into_inner();
        if ack.ok {
            Ok(ack.message)
        } else {
            anyhow::bail!("Load model failed: {}", ack.message)
        }
    }
}

/// Format system status as a readable string
pub fn format_system_status(status: &SystemStatus) -> String {
    format!(
        "System Status:\n\
         - Running: {}\n\
         - Trading Mode: {}\n\
         - Latency: p50={}us, p99={}us, max={}us\n\
         - Orders: submitted={}, filled={}, canceled={}, rejected={}, active={}\n\
         - Events: received={}, processed={}, dropped={}\n\
         - WebSocket: active={}, reconnects={}\n\
         - Uptime: {}s",
        status.is_running,
        status.trading_mode,
        status.latency_p50_us,
        status.latency_p99_us,
        status.latency_max_us,
        status.orders_submitted,
        status.orders_filled,
        status.orders_canceled,
        status.orders_rejected,
        status.active_orders,
        status.events_received,
        status.events_processed,
        status.events_dropped,
        status.ws_connections_active,
        status.ws_reconnect_count,
        status.uptime_seconds
    )
}

/// Format portfolio status as a readable string
pub fn format_portfolio_status(status: &PortfolioStatus) -> String {
    let mut result = format!(
        "Portfolio Status:\n\
         - Cash Balance: ${:.2}\n\
         - Total Equity: ${:.2}\n\
         - Unrealized PnL: ${:.2}\n\
         - Realized PnL: ${:.2}\n\
         - High Water Mark: ${:.2}\n\
         - Current Drawdown: {:.2}%\n\
         - Max Drawdown: {:.2}%\n\
         - Positions ({}):",
        status.cash_balance,
        status.total_equity,
        status.unrealized_pnl,
        status.realized_pnl,
        status.high_water_mark,
        status.drawdown_pct,
        status.max_drawdown_pct,
        status.positions.len()
    );

    for pos in &status.positions {
        result.push_str(&format!(
            "\n  - {} @ {}: qty={:.4}, avg_price=${:.4}, unrealized_pnl=${:.2}",
            pos.symbol, pos.venue, pos.quantity, pos.avg_price, pos.unrealized_pnl
        ));
    }

    result
}

/// Format health response as a readable string
pub fn format_health_response(health: &HealthResponse) -> String {
    let mut result = format!(
        "Health Check:\n\
         - Overall: {} ({})",
        if health.healthy { "HEALTHY" } else { "UNHEALTHY" },
        health.status
    );

    for (name, component) in &health.components {
        result.push_str(&format!(
            "\n  - {}: {} - {}",
            name,
            if component.healthy { "OK" } else { "FAIL" },
            component.message
        ));
    }

    result
}
