use std::fmt::Write as _;
use std::sync::Arc;

use engine::Engine;
use prometheus::Encoder;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::info;

pub fn spawn_metrics_server(
    engine_arc: Arc<Mutex<Engine>>,
    port: u16,
) -> JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>> {
    info!("啟動 Axum Metrics HTTP 服務器於端口 {}", port);
    tokio::spawn(run_axum_metrics_server(engine_arc, port))
}

#[allow(dead_code)]
async fn run_axum_metrics_server(
    engine_arc: Arc<Mutex<Engine>>,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use infra_metrics::http_server::{MetricsServer, MetricsServerConfig};

    let config = MetricsServerConfig {
        bind_address: "0.0.0.0".to_string(),
        port,
        verbose_logging: false,
        readiness_max_idle_secs: 5,
        readiness_max_utilization: 0.9,
    };

    let server = MetricsServer::new(config);

    info!("Metrics 服務器監聽於 http://0.0.0.0:{}/metrics", port);

    let sync_engine_arc = engine_arc.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;

            if let Ok(engine) = sync_engine_arc.try_lock() {
                let latency_stats = engine.get_latency_stats();
                if !latency_stats.is_empty() {
                    infra_metrics::MetricsRegistry::global()
                        .update_from_latency_monitor(&latency_stats);
                }
            }
        }
    });

    server.start().await
}

#[allow(dead_code)]
async fn generate_basic_metrics(engine_arc: Arc<Mutex<Engine>>) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let (account_view, stats) = {
        let engine = engine_arc.lock().await;
        #[cfg(feature = "metrics")]
        {
            engine.sync_latency_metrics_to_prometheus();
        }
        (engine.get_account_view(), engine.get_statistics())
    };

    let mut position_lines = String::new();
    for (sym, pos) in &account_view.positions {
        let _ = writeln!(
            position_lines,
            "hft_position_quantity{{symbol=\"{}\"}} {}",
            sym.as_str(),
            pos.quantity.0
        );
        let _ = writeln!(
            position_lines,
            "hft_position_avg_price{{symbol=\"{}\"}} {}",
            sym.as_str(),
            pos.avg_price.0
        );
    }

    let total_pnl = account_view.realized_pnl + account_view.unrealized_pnl;
    let equity = account_view.cash_balance + total_pnl;

    let custom = format!(
        r#"# HELP hft_system_uptime_seconds HFT system uptime in seconds
# TYPE hft_system_uptime_seconds counter
hft_system_uptime_seconds {}

# HELP hft_build_info Build information
# TYPE hft_build_info gauge
hft_build_info{{version="{}"}} 1

# HELP hft_account_cash_balance Account cash balance
# TYPE hft_account_cash_balance gauge
hft_account_cash_balance {}

# HELP hft_account_unrealized_pnl Account unrealized PnL
# TYPE hft_account_unrealized_pnl gauge
hft_account_unrealized_pnl {}

# HELP hft_account_realized_pnl Account realized PnL
# TYPE hft_account_realized_pnl gauge
hft_account_realized_pnl {}

# HELP hft_account_total_pnl Account total PnL (realized + unrealized)
# TYPE hft_account_total_pnl gauge
hft_account_total_pnl {}

# HELP hft_account_equity Account equity (cash + total_pnl)
# TYPE hft_account_equity gauge
hft_account_equity {}

# HELP hft_account_positions Number of open positions
# TYPE hft_account_positions gauge
hft_account_positions {}

# HELP hft_engine_cycles_total Engine tick cycles total
# TYPE hft_engine_cycles_total counter
hft_engine_cycles_total {}

# HELP hft_engine_exec_events_total Execution events processed total
# TYPE hft_engine_exec_events_total counter
hft_engine_exec_events_total {}

# HELP hft_orders_submitted_total Orders submitted (OrderNew)
# TYPE hft_orders_submitted_total counter
hft_orders_submitted_total {}

# HELP hft_orders_ack_total Orders acknowledged
# TYPE hft_orders_ack_total counter
hft_orders_ack_total {}

# HELP hft_orders_filled_total Orders filled
# TYPE hft_orders_filled_total counter
hft_orders_filled_total {}

# HELP hft_orders_rejected_total Orders rejected
# TYPE hft_orders_rejected_total counter
hft_orders_rejected_total {}

# HELP hft_orders_canceled_total Orders canceled
# TYPE hft_orders_canceled_total counter
hft_orders_canceled_total {}

# HELP hft_position_quantity Per-symbol position quantity
# TYPE hft_position_quantity gauge
# HELP hft_position_avg_price Per-symbol average price
# TYPE hft_position_avg_price gauge
{}
"#,
        now / 1000,
        env!("CARGO_PKG_VERSION"),
        account_view.cash_balance,
        account_view.unrealized_pnl,
        account_view.realized_pnl,
        total_pnl,
        equity,
        account_view.positions.len(),
        stats.cycle_count,
        stats.execution_events_processed,
        stats.orders_submitted,
        stats.orders_ack,
        stats.orders_filled,
        stats.orders_rejected,
        stats.orders_canceled,
        position_lines,
    );

    let mut combined = custom;
    {
        let mf = infra_metrics::MetricsRegistry::global().registry().gather();
        let mut buf = Vec::new();
        let encoder = prometheus::TextEncoder::new();
        if encoder.encode(&mf, &mut buf).is_ok() {
            let text = String::from_utf8_lossy(&buf);
            combined.push('\n');
            combined.push_str(&text);
        }
    }

    combined
}
