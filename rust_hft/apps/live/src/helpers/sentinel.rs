//! Sentinel Worker - 自動化風控監控
//!
//! 定期檢查系統狀態，根據延遲和回撤自動調整交易行為

use std::time::Duration;

use engine::ExecutionControlHandle;
use risk::{Sentinel, SentinelAction, SentinelConfig, SentinelState, SystemStats};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Sentinel 配置參數
pub struct SentinelWorkerConfig {
    /// 檢查間隔（毫秒）
    pub check_interval_ms: u64,
    /// 延遲警告閾值（微秒）
    pub latency_warn_us: u64,
    /// 延遲降頻閾值（微秒）
    pub latency_degrade_us: u64,
    /// 回撤警告閾值（百分比）
    pub drawdown_warn_pct: f64,
    /// 回撤停止閾值（百分比）
    pub drawdown_stop_pct: f64,
}

impl Default for SentinelWorkerConfig {
    fn default() -> Self {
        Self {
            check_interval_ms: 100, // 每 100ms 檢查一次
            latency_warn_us: 15_000,
            latency_degrade_us: 25_000,
            drawdown_warn_pct: 2.0,
            drawdown_stop_pct: 5.0,
        }
    }
}

impl From<SentinelWorkerConfig> for SentinelConfig {
    fn from(config: SentinelWorkerConfig) -> Self {
        SentinelConfig {
            latency_warn_us: config.latency_warn_us,
            latency_degrade_us: config.latency_degrade_us,
            drawdown_warn_pct: config.drawdown_warn_pct,
            drawdown_stop_pct: config.drawdown_stop_pct,
            ..Default::default()
        }
    }
}

/// 啟動 Sentinel 監控 Worker
pub fn spawn_sentinel_worker(
    control: ExecutionControlHandle,
    config: SentinelWorkerConfig,
) -> JoinHandle<()> {
    info!(
        "啟動 Sentinel 監控: check_interval={}ms, latency_warn={}us, drawdown_stop={}%",
        config.check_interval_ms, config.latency_warn_us, config.drawdown_stop_pct
    );

    let check_interval = config.check_interval_ms;
    let sentinel_config: SentinelConfig = config.into();

    tokio::spawn(async move {
        run_sentinel_loop(control, sentinel_config, check_interval).await;
    })
}

/// Sentinel 主循環
async fn run_sentinel_loop(
    control: ExecutionControlHandle,
    config: SentinelConfig,
    check_interval_ms: u64,
) {
    let engine_arc = control.engine();
    let mut sentinel = Sentinel::new(config);
    let mut interval = tokio::time::interval(Duration::from_millis(check_interval_ms));

    let mut last_state = SentinelState::Normal;
    let mut last_orders_submitted = 0_u64;

    loop {
        interval.tick().await;

        // 獲取引擎統計
        let (stats, is_running) = {
            let engine = engine_arc.lock().await;
            let engine_stats = engine.get_statistics();

            // 從引擎獲取真實 PnL、延遲和回撤統計 (drawdown 現在由 Portfolio 計算)
            let sentinel_stats = engine.get_sentinel_stats();

            let stats = SystemStats {
                latency_p99_us: sentinel_stats.latency_p99_us,
                latency_p50_us: sentinel_stats.latency_p50_us,
                drawdown_pct: sentinel_stats.drawdown_pct,
                pnl: sentinel_stats.pnl,
                high_water_mark: sentinel_stats.high_water_mark,
                position_count: sentinel_stats.position_count,
                notional_value: sentinel_stats.notional_value,
                order_rate: engine_stats
                    .orders_submitted
                    .saturating_sub(last_orders_submitted) as f64
                    / (check_interval_ms.max(1) as f64 / 1_000.0),
                // ponytail: engine does not expose venue reconnects; wire adapter metrics here
                // if Sentinel policy begins to act on reconnect frequency.
                ws_reconnect_count: 0,
                data_gap_count: u32::try_from(
                    engine_stats
                        .market_events_dropped
                        .saturating_add(engine_stats.snapshot_publish_failed)
                        .saturating_add(engine_stats.data_integrity_gaps),
                )
                .unwrap_or(u32::MAX),
            };
            last_orders_submitted = engine_stats.orders_submitted;

            (stats, engine_stats.is_running)
        };

        if !is_running {
            info!("引擎已停止，Sentinel 退出");
            break;
        }

        // 檢查並獲取動作
        let action = sentinel.check(&stats);

        // 狀態變化時記錄
        let current_state = sentinel.state();
        if current_state != last_state {
            match current_state {
                SentinelState::Normal => info!("Sentinel 狀態: 正常"),
                SentinelState::Degraded => warn!("Sentinel 狀態: 降頻模式"),
                SentinelState::Stopped => error!("Sentinel 狀態: 已停止"),
                SentinelState::Emergency => error!("Sentinel 狀態: 緊急狀態"),
                SentinelState::Recovering => info!("Sentinel 狀態: 恢復中"),
            }
            last_state = current_state;
        }

        // 根據動作執行操作
        match action {
            SentinelAction::Continue => {}
            SentinelAction::Warn => {
                // 警告已在 Sentinel 內部記錄
            }
            SentinelAction::Degrade => {
                // 降頻模式 - 減少交易頻率
                warn!(
                    "進入降頻模式: latency_p99={}us, drawdown={:.2}%",
                    stats.latency_p99_us, stats.drawdown_pct
                );
                let mut engine = engine_arc.lock().await;
                engine.enter_degrade_mode();
            }
            SentinelAction::Stop => {
                // 停止交易
                error!(
                    "停止交易: latency_p99={}us, drawdown={:.2}%",
                    stats.latency_p99_us, stats.drawdown_pct
                );
                if let Err(error) = control.pause_trading().await {
                    error!(%error, "Sentinel failed to disable execution intake");
                }
            }
            SentinelAction::EmergencyExit => {
                error!("緊急平倉觸發: drawdown={:.2}%", stats.drawdown_pct);
                match control.emergency_stop(true).await {
                    Ok(report) if report.is_complete() => {
                        error!(submitted = report.submitted.len(), "緊急撤單已提交")
                    }
                    Ok(report) => {
                        error!(
                            requested = report.requested,
                            submitted = report.submitted.len(),
                            failures = report.failures.len(),
                            "緊急撤單未完整提交"
                        )
                    }
                    Err(error) => {
                        error!(%error, "緊急控制失敗；系統保持 Emergency 模式")
                    }
                }
            }
        }
    }
}

/// 獲取 Sentinel 當前狀態（用於外部查詢）
/// (保留供未來 gRPC 控制接口使用)
#[allow(dead_code)]
pub struct SentinelHandle {
    _handle: JoinHandle<()>,
}

#[allow(dead_code)]
impl SentinelHandle {
    pub fn new(handle: JoinHandle<()>) -> Self {
        Self { _handle: handle }
    }
}
