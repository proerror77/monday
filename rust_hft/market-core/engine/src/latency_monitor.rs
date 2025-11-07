//! 統一延遲監控服務
//!
//! 收集並統計各階段的延遲數據，提供實時監控和告警功能

use hdrhistogram::Histogram;
use hft_core::latency::LatencyStageStats;
use hft_core::{now_micros, LatencyStage};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// 延遲監控配置
#[derive(Debug, Clone)]
pub struct LatencyMonitorConfig {
    /// 各階段延遲告警閾值（微秒）
    pub alert_thresholds: HashMap<LatencyStage, u64>,
    /// 統計窗口大小（樣本數）
    pub window_size: usize,
    /// 統計報告間隔（毫秒）
    pub report_interval_ms: u64,
}

impl Default for LatencyMonitorConfig {
    fn default() -> Self {
        let mut alert_thresholds = HashMap::new();
        // 延遲閾值設定（公網環境建議將 Ingestion 放寬到 30ms）
        alert_thresholds.insert(LatencyStage::WsReceive, 100); // 100μs 內完成
        alert_thresholds.insert(LatencyStage::Parsing, 500); // 0.5ms
        alert_thresholds.insert(LatencyStage::Ingestion, 30_000); // 30ms
        alert_thresholds.insert(LatencyStage::Aggregation, 500); // 0.5ms
        alert_thresholds.insert(LatencyStage::Strategy, 2000); // 2ms
        alert_thresholds.insert(LatencyStage::Risk, 1500); // 1.5ms
        alert_thresholds.insert(LatencyStage::Execution, 5000); // 5ms
        alert_thresholds.insert(LatencyStage::Submission, 10_000); // 10ms
        alert_thresholds.insert(LatencyStage::EndToEnd, 25000); // 25ms (目標)

        Self {
            alert_thresholds,
            window_size: 200,
            report_interval_ms: 1000, // 每 1 秒報告一次
        }
    }
}

const LATENCY_HISTOGRAM_MAX_US: u64 = 120_000_000; // 120 秒，可覆蓋極端情況
const LATENCY_HISTOGRAM_SIGFIGS: u8 = 3;

/// 延遲監控服務
#[derive(Debug)]
pub struct LatencyMonitor {
    config: LatencyMonitorConfig,
    /// 各階段最近樣本（僅保留 window_size 筆）
    samples: Arc<Mutex<HashMap<LatencyStage, VecDeque<u64>>>>,
    /// 最後報告時間（微秒）
    last_report_time: AtomicU64,
}

impl LatencyMonitor {
    /// 創建新的延遲監控器
    pub fn new(config: LatencyMonitorConfig) -> Self {
        Self {
            config,
            samples: Arc::new(Mutex::new(HashMap::new())),
            last_report_time: AtomicU64::new(now_micros()),
        }
    }

    /// 記錄延遲測量
    pub fn record_latency(&self, stage: LatencyStage, latency_micros: u64) {
        // 檢查是否超過告警閾值
        if let Some(&threshold) = self.config.alert_thresholds.get(&stage) {
            if latency_micros > threshold {
                warn!(
                    "延遲告警: {} 階段延遲 {}μs 超過閾值 {}μs",
                    stage.as_str(),
                    latency_micros,
                    threshold
                );
            }
        }

        // 保存樣本（僅保留最近 window_size 筆）
        let mut samples = self.samples.lock();
        let dq = samples
            .entry(stage)
            .or_insert_with(|| VecDeque::with_capacity(self.config.window_size));
        if dq.len() >= self.config.window_size {
            dq.pop_front();
        }
        dq.push_back(latency_micros);

        // 檢查是否需要生成報告
        self.maybe_generate_report();
    }

    /// 獲取指定階段的統計信息
    pub fn get_stage_stats(&self, stage: LatencyStage) -> Option<LatencyStageStats> {
        let samples = self.samples.lock();
        if let Some(dq) = samples.get(&stage) {
            if !dq.is_empty() {
                // 重建 HDR 直方圖用于統計
                let mut histogram = Histogram::new_with_bounds(
                    1,
                    LATENCY_HISTOGRAM_MAX_US,
                    LATENCY_HISTOGRAM_SIGFIGS,
                )
                .expect("latency histogram bounds");
                for &v in dq.iter() {
                    if let Err(err) = histogram.record(v) {
                        warn!("延遲統計記錄失敗: {}", err);
                    }
                }
                return Some(LatencyStageStats::from_histogram(stage, &histogram));
            }
        }
        None
    }

    /// 獲取所有階段的統計信息
    pub fn get_all_stats(&self) -> HashMap<LatencyStage, LatencyStageStats> {
        let mut all_stats = HashMap::new();

        let samples = self.samples.lock();
        for (&stage, dq) in samples.iter() {
            if !dq.is_empty() {
                let mut histogram = Histogram::new_with_bounds(
                    1,
                    LATENCY_HISTOGRAM_MAX_US,
                    LATENCY_HISTOGRAM_SIGFIGS,
                )
                .expect("latency histogram bounds");
                for &v in dq.iter() {
                    let _ = histogram.record(v);
                }
                let stats = LatencyStageStats::from_histogram(stage, &histogram);
                all_stats.insert(stage, stats);
            }
        }

        all_stats
    }

    /// 重置統計數據
    pub fn reset_stats(&self) {
        self.samples.lock().clear();
        self.last_report_time.store(now_micros(), Ordering::Relaxed);
    }

    /// 檢查並生成定期報告
    fn maybe_generate_report(&self) {
        let now = now_micros();
        let last = self.last_report_time.load(Ordering::Relaxed);
        let elapsed_ms = (now - last) / 1000;

        if elapsed_ms >= self.config.report_interval_ms
            && self
                .last_report_time
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.generate_report();
        }
    }

    /// 生成延遲統計報告
    fn generate_report(&self) {
        let all_stats = self.get_all_stats();

        if all_stats.is_empty() {
            debug!("延遲監控報告: 暫無數據");
            return;
        }

        info!("=== 延遲監控報告 ===");

        // 按階段順序輸出（含 EndToEnd）
        for stage in LatencyStage::all_stages().iter() {
            if let Some(stats) = all_stats.get(stage) {
                info!(
                    "{}: 樣本={}, 平均={:.1}μs, p50={}μs, p95={}μs, p99={}μs",
                    stats.stage.as_str(),
                    stats.count,
                    stats.mean_micros,
                    stats.p50_micros,
                    stats.p95_micros,
                    stats.p99_micros
                );
            }
        }

        // 檢查端到端延遲是否達標
        if let Some(e2e_stats) = all_stats.get(&LatencyStage::EndToEnd) {
            if e2e_stats.p99_micros <= 25000 {
                info!("🎯 端到端延遲達標: p99={}μs ≤ 25ms", e2e_stats.p99_micros);
            } else {
                warn!("⚠️  端到端延遲超標: p99={}μs > 25ms", e2e_stats.p99_micros);
            }
        }
    }

    /// 檢查特定階段是否存在延遲問題
    pub fn check_latency_health(&self, stage: LatencyStage) -> LatencyHealth {
        if let Some(stats) = self.get_stage_stats(stage) {
            if let Some(&threshold) = self.config.alert_thresholds.get(&stage) {
                if stats.p99_micros > threshold {
                    return LatencyHealth::Critical {
                        current_p99: stats.p99_micros,
                        threshold,
                    };
                } else if stats.p95_micros > threshold / 2 {
                    return LatencyHealth::Warning {
                        current_p95: stats.p95_micros,
                        threshold: threshold / 2,
                    };
                }
            }
            LatencyHealth::Healthy
        } else {
            LatencyHealth::NoData
        }
    }
}

/// 延遲健康狀態
#[derive(Debug, Clone, PartialEq)]
pub enum LatencyHealth {
    /// 健康狀態
    Healthy,
    /// 警告狀態（p95 超過閾值的一半）
    Warning { current_p95: u64, threshold: u64 },
    /// 危險狀態（p99 超過閾值）
    Critical { current_p99: u64, threshold: u64 },
    /// 無數據
    NoData,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_latency_monitor_basic() {
        let config = LatencyMonitorConfig::default();
        let monitor = LatencyMonitor::new(config);

        // 記錄一些延遲樣本
        monitor.record_latency(LatencyStage::Ingestion, 500);
        monitor.record_latency(LatencyStage::Ingestion, 800);
        monitor.record_latency(LatencyStage::Ingestion, 1200); // 超過閾值

        let stats = monitor.get_stage_stats(LatencyStage::Ingestion).unwrap();
        assert_eq!(stats.count, 3);
        assert_eq!(stats.min_micros, 500);
        assert_eq!(stats.max_micros, 1200);
    }

    #[test]
    fn test_latency_health_check() {
        let mut config = LatencyMonitorConfig::default();
        // 測試使用更嚴格的 Ingestion 閾值，便於快速觸發 Critical
        config.alert_thresholds.insert(LatencyStage::Ingestion, 1_000);
        let monitor = LatencyMonitor::new(config);

        // 添加一些正常的延遲樣本
        for _ in 0..100 {
            monitor.record_latency(LatencyStage::Ingestion, 300);
        }

        let health = monitor.check_latency_health(LatencyStage::Ingestion);
        assert_eq!(health, LatencyHealth::Healthy);

        // 添加一些高延遲樣本（超過 1ms 的閾值）
        for _ in 0..10 {
            monitor.record_latency(LatencyStage::Ingestion, 2000);
        }

        let health = monitor.check_latency_health(LatencyStage::Ingestion);
        assert!(matches!(health, LatencyHealth::Critical { .. }));
    }

    #[test]
    fn test_rolling_window() {
        let config = LatencyMonitorConfig {
            window_size: 5,
            ..Default::default()
        };
        let monitor = LatencyMonitor::new(config);

        // 添加超過窗口大小的樣本
        for i in 1..=10 {
            monitor.record_latency(LatencyStage::Ingestion, i * 100);
        }

        let stats = monitor.get_stage_stats(LatencyStage::Ingestion).unwrap();
        assert_eq!(stats.count, 5); // 應該只保留最後 5 個樣本
        assert_eq!(stats.min_micros, 600); // 最小值應該是第 6 個樣本
        assert_eq!(stats.max_micros, 1000); // 最大值應該是第 10 個樣本
    }
}
