//! Sentinel - 自動化風控哨兵
//!
//! 取代 Python control_ws 的功能，在 Rust 內部實現：
//! - 延遲監控 (Latency Guard)
//! - 回撤監控 (Drawdown Guard)
//! - 自動降頻/停止交易
//! - Kill Switch 機制
//!
//! 設計原則：
//! - 微秒級響應（不依賴 Python/gRPC）
//! - 配置驅動（可熱重載）
//! - 自動恢復機制

use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// Sentinel 配置
#[derive(Debug, Clone)]
pub struct SentinelConfig {
    // === 延遲監控 ===
    /// 延遲警告閾值 (microseconds)
    pub latency_warn_us: u64,
    /// 延遲降頻閾值 (microseconds)
    pub latency_degrade_us: u64,
    /// 延遲停止閾值 (microseconds)
    pub latency_stop_us: u64,

    // === 回撤監控 ===
    /// 回撤警告閾值 (百分比)
    pub drawdown_warn_pct: f64,
    /// 回撤降頻閾值 (百分比)
    pub drawdown_degrade_pct: f64,
    /// 回撤停止閾值 (百分比)
    pub drawdown_stop_pct: f64,
    /// 緊急平倉閾值 (百分比)
    pub drawdown_emergency_pct: f64,

    // === 降頻策略 ===
    /// 降頻時減倉比例
    pub degrade_reduce_position_pct: f64,
    /// 降頻時最大下單頻率 (orders/second)
    pub degrade_max_orders_per_second: f64,

    // === 恢復條件 ===
    /// 延遲恢復閾值 (microseconds)
    pub recovery_latency_below_us: u64,
    /// 恢復冷卻時間 (seconds)
    pub recovery_cooldown_secs: u64,

    // === 連續異常計數 ===
    /// 連續異常次數觸發動作
    pub consecutive_anomaly_threshold: u32,
}

impl Default for SentinelConfig {
    fn default() -> Self {
        Self {
            // 延遲閾值
            latency_warn_us: 15_000,      // 15ms 警告
            latency_degrade_us: 25_000,   // 25ms 降頻
            latency_stop_us: 50_000,      // 50ms 停止

            // 回撤閾值
            drawdown_warn_pct: 2.0,       // 2% 警告
            drawdown_degrade_pct: 3.0,    // 3% 降頻
            drawdown_stop_pct: 5.0,       // 5% 停止
            drawdown_emergency_pct: 7.0,  // 7% 緊急平倉

            // 降頻策略
            degrade_reduce_position_pct: 50.0,
            degrade_max_orders_per_second: 1.0,

            // 恢復條件
            recovery_latency_below_us: 10_000,
            recovery_cooldown_secs: 300,

            // 連續異常
            consecutive_anomaly_threshold: 3,
        }
    }
}

/// Sentinel 動作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentinelAction {
    /// 正常運行
    Continue,
    /// 發出警告
    Warn,
    /// 進入降頻模式
    Degrade,
    /// 停止交易
    Stop,
    /// 緊急平倉
    EmergencyExit,
}

impl SentinelAction {
    /// 獲取動作優先級（數字越大越嚴重）
    pub fn priority(&self) -> u8 {
        match self {
            Self::Continue => 0,
            Self::Warn => 1,
            Self::Degrade => 2,
            Self::Stop => 3,
            Self::EmergencyExit => 4,
        }
    }

    /// 合併兩個動作，取更嚴重的
    pub fn merge(self, other: Self) -> Self {
        if self.priority() >= other.priority() {
            self
        } else {
            other
        }
    }
}

/// Sentinel 狀態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentinelState {
    /// 正常運行
    Normal,
    /// 降頻模式
    Degraded,
    /// 已停止
    Stopped,
    /// 緊急狀態
    Emergency,
    /// 恢復中
    Recovering,
}

/// 系統統計數據（每個 tick 更新）
#[derive(Debug, Clone, Default)]
pub struct SystemStats {
    /// 當前延遲 p99 (microseconds)
    pub latency_p99_us: u64,
    /// 當前延遲 p50 (microseconds)
    pub latency_p50_us: u64,
    /// 當前回撤 (百分比)
    pub drawdown_pct: f64,
    /// 當前 PnL
    pub pnl: f64,
    /// 高水位 PnL
    pub high_water_mark: f64,
    /// 當前持倉數量
    pub position_count: i64,
    /// 當前名義價值
    pub notional_value: f64,
    /// 訂單提交率 (orders/second)
    pub order_rate: f64,
    /// WebSocket 重連次數
    pub ws_reconnect_count: u32,
    /// 數據間隙次數
    pub data_gap_count: u32,
}

/// Sentinel 哨兵 - 自動化風控核心
pub struct Sentinel {
    config: SentinelConfig,
    state: SentinelState,

    // 延遲追蹤
    consecutive_latency_violations: u32,

    // 回撤追蹤
    consecutive_drawdown_violations: u32,

    // 恢復追蹤
    last_violation_time: Option<Instant>,
    degraded_since: Option<Instant>,

    // 統計
    total_checks: u64,
    total_warnings: u64,
    total_degrades: u64,
    total_stops: u64,
}

impl Sentinel {
    /// 創建新的 Sentinel
    pub fn new(config: SentinelConfig) -> Self {
        Self {
            config,
            state: SentinelState::Normal,
            consecutive_latency_violations: 0,
            consecutive_drawdown_violations: 0,
            last_violation_time: None,
            degraded_since: None,
            total_checks: 0,
            total_warnings: 0,
            total_degrades: 0,
            total_stops: 0,
        }
    }

    /// 使用默認配置創建
    pub fn with_defaults() -> Self {
        Self::new(SentinelConfig::default())
    }

    /// 獲取當前狀態
    pub fn state(&self) -> SentinelState {
        self.state
    }

    /// 獲取配置
    pub fn config(&self) -> &SentinelConfig {
        &self.config
    }

    /// 更新配置（支持熱重載）
    pub fn update_config(&mut self, config: SentinelConfig) {
        info!("Sentinel config updated");
        self.config = config;
    }

    /// 檢查系統狀態並返回建議動作
    ///
    /// 這個函數應該在每個 engine tick 中調用
    pub fn check(&mut self, stats: &SystemStats) -> SentinelAction {
        self.total_checks += 1;

        // 檢查延遲
        let latency_action = self.check_latency(stats);

        // 檢查回撤
        let drawdown_action = self.check_drawdown(stats);

        // 合併動作（取更嚴重的）
        let action = latency_action.merge(drawdown_action);

        // 更新狀態
        self.update_state(action);

        // 檢查恢復條件
        if (self.state == SentinelState::Degraded || self.state == SentinelState::Recovering)
            && self.should_recover(stats) {
                self.recover();
                return SentinelAction::Continue;
            }

        action
    }

    /// 檢查延遲
    fn check_latency(&mut self, stats: &SystemStats) -> SentinelAction {
        let latency = stats.latency_p99_us;

        if latency >= self.config.latency_stop_us {
            self.consecutive_latency_violations += 1;
            self.last_violation_time = Some(Instant::now());

            if self.consecutive_latency_violations >= self.config.consecutive_anomaly_threshold {
                error!(
                    "Latency critical: {}us >= {}us (consecutive: {})",
                    latency, self.config.latency_stop_us, self.consecutive_latency_violations
                );
                self.total_stops += 1;
                return SentinelAction::Stop;
            }
        } else if latency >= self.config.latency_degrade_us {
            self.consecutive_latency_violations += 1;
            self.last_violation_time = Some(Instant::now());

            if self.consecutive_latency_violations >= self.config.consecutive_anomaly_threshold {
                warn!(
                    "Latency high: {}us >= {}us - entering degrade mode",
                    latency, self.config.latency_degrade_us
                );
                self.total_degrades += 1;
                return SentinelAction::Degrade;
            }
        } else if latency >= self.config.latency_warn_us {
            warn!("Latency warning: {}us >= {}us", latency, self.config.latency_warn_us);
            self.total_warnings += 1;
            return SentinelAction::Warn;
        } else {
            // 延遲正常，重置計數
            self.consecutive_latency_violations = 0;
        }

        SentinelAction::Continue
    }

    /// 檢查回撤
    fn check_drawdown(&mut self, stats: &SystemStats) -> SentinelAction {
        let dd = stats.drawdown_pct;

        if dd >= self.config.drawdown_emergency_pct {
            error!(
                "EMERGENCY: Drawdown {:.2}% >= {:.2}% - triggering emergency exit!",
                dd, self.config.drawdown_emergency_pct
            );
            return SentinelAction::EmergencyExit;
        }

        if dd >= self.config.drawdown_stop_pct {
            self.consecutive_drawdown_violations += 1;
            self.last_violation_time = Some(Instant::now());

            if self.consecutive_drawdown_violations >= self.config.consecutive_anomaly_threshold {
                error!(
                    "Drawdown critical: {:.2}% >= {:.2}% - stopping trading",
                    dd, self.config.drawdown_stop_pct
                );
                self.total_stops += 1;
                return SentinelAction::Stop;
            }
        } else if dd >= self.config.drawdown_degrade_pct {
            self.consecutive_drawdown_violations += 1;
            self.last_violation_time = Some(Instant::now());

            if self.consecutive_drawdown_violations >= self.config.consecutive_anomaly_threshold {
                warn!(
                    "Drawdown high: {:.2}% >= {:.2}% - entering degrade mode",
                    dd, self.config.drawdown_degrade_pct
                );
                self.total_degrades += 1;
                return SentinelAction::Degrade;
            }
        } else if dd >= self.config.drawdown_warn_pct {
            warn!(
                "Drawdown warning: {:.2}% >= {:.2}%",
                dd, self.config.drawdown_warn_pct
            );
            self.total_warnings += 1;
            return SentinelAction::Warn;
        } else {
            // 回撤正常，重置計數
            self.consecutive_drawdown_violations = 0;
        }

        SentinelAction::Continue
    }

    /// 更新內部狀態
    fn update_state(&mut self, action: SentinelAction) {
        self.state = match action {
            SentinelAction::Continue | SentinelAction::Warn => {
                if self.state == SentinelState::Recovering {
                    SentinelState::Recovering
                } else {
                    SentinelState::Normal
                }
            }
            SentinelAction::Degrade => {
                if self.degraded_since.is_none() {
                    self.degraded_since = Some(Instant::now());
                }
                SentinelState::Degraded
            }
            SentinelAction::Stop => SentinelState::Stopped,
            SentinelAction::EmergencyExit => SentinelState::Emergency,
        };
    }

    /// 檢查是否應該恢復
    fn should_recover(&self, stats: &SystemStats) -> bool {
        // 檢查延遲是否恢復
        if stats.latency_p99_us > self.config.recovery_latency_below_us {
            return false;
        }

        // 檢查回撤是否恢復
        if stats.drawdown_pct > self.config.drawdown_warn_pct {
            return false;
        }

        // 檢查冷卻時間
        if let Some(violation_time) = self.last_violation_time {
            let elapsed = violation_time.elapsed();
            if elapsed < Duration::from_secs(self.config.recovery_cooldown_secs) {
                return false;
            }
        }

        true
    }

    /// 執行恢復
    fn recover(&mut self) {
        info!("Sentinel recovering to normal state");
        self.state = SentinelState::Normal;
        self.degraded_since = None;
        self.consecutive_latency_violations = 0;
        self.consecutive_drawdown_violations = 0;
    }

    /// 強制停止
    pub fn force_stop(&mut self) {
        warn!("Sentinel force stop triggered");
        self.state = SentinelState::Stopped;
        self.total_stops += 1;
    }

    /// 強制恢復（需要人工確認）
    pub fn force_recover(&mut self) {
        info!("Sentinel force recover (manual override)");
        self.recover();
    }

    /// 獲取統計信息
    pub fn stats(&self) -> SentinelStats {
        SentinelStats {
            state: self.state,
            total_checks: self.total_checks,
            total_warnings: self.total_warnings,
            total_degrades: self.total_degrades,
            total_stops: self.total_stops,
            consecutive_latency_violations: self.consecutive_latency_violations,
            consecutive_drawdown_violations: self.consecutive_drawdown_violations,
            degraded_duration: self.degraded_since.map(|t| t.elapsed()),
        }
    }
}

/// Sentinel 統計
#[derive(Debug, Clone)]
pub struct SentinelStats {
    pub state: SentinelState,
    pub total_checks: u64,
    pub total_warnings: u64,
    pub total_degrades: u64,
    pub total_stops: u64,
    pub consecutive_latency_violations: u32,
    pub consecutive_drawdown_violations: u32,
    pub degraded_duration: Option<Duration>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sentinel_normal() {
        let mut sentinel = Sentinel::with_defaults();
        let stats = SystemStats {
            latency_p99_us: 5_000,
            drawdown_pct: 1.0,
            ..Default::default()
        };

        let action = sentinel.check(&stats);
        assert_eq!(action, SentinelAction::Continue);
        assert_eq!(sentinel.state(), SentinelState::Normal);
    }

    #[test]
    fn test_sentinel_latency_warning() {
        let mut sentinel = Sentinel::with_defaults();
        let stats = SystemStats {
            latency_p99_us: 20_000, // > 15ms warn threshold
            drawdown_pct: 0.5,
            ..Default::default()
        };

        let action = sentinel.check(&stats);
        assert_eq!(action, SentinelAction::Warn);
    }

    #[test]
    fn test_sentinel_drawdown_stop() {
        let config = SentinelConfig {
            consecutive_anomaly_threshold: 1, // 立即觸發
            ..Default::default()
        };
        let mut sentinel = Sentinel::new(config);

        let stats = SystemStats {
            latency_p99_us: 5_000,
            drawdown_pct: 6.0, // > 5% stop threshold
            ..Default::default()
        };

        let action = sentinel.check(&stats);
        assert_eq!(action, SentinelAction::Stop);
        assert_eq!(sentinel.state(), SentinelState::Stopped);
    }

    #[test]
    fn test_sentinel_emergency() {
        let mut sentinel = Sentinel::with_defaults();
        let stats = SystemStats {
            latency_p99_us: 5_000,
            drawdown_pct: 8.0, // > 7% emergency threshold
            ..Default::default()
        };

        let action = sentinel.check(&stats);
        assert_eq!(action, SentinelAction::EmergencyExit);
        assert_eq!(sentinel.state(), SentinelState::Emergency);
    }

    #[test]
    fn test_action_merge() {
        assert_eq!(
            SentinelAction::Warn.merge(SentinelAction::Continue),
            SentinelAction::Warn
        );
        assert_eq!(
            SentinelAction::Continue.merge(SentinelAction::Stop),
            SentinelAction::Stop
        );
        assert_eq!(
            SentinelAction::Degrade.merge(SentinelAction::EmergencyExit),
            SentinelAction::EmergencyExit
        );
    }
}
