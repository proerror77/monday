//! 統一延遲監控服務
//! 
//! 收集並統計各階段的延遲數據，提供實時監控和告警功能

use hft_core::{LatencyStage, MicrosTimestamp, now_micros};
use hft_core::latency::LatencyStageStats;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{warn, info, debug};

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
        // HFT 標準延遲閾值設定
        alert_thresholds.insert(LatencyStage::Ingestion, 1000);    // 1ms
        alert_thresholds.insert(LatencyStage::Aggregation, 500);   // 0.5ms  
        alert_thresholds.insert(LatencyStage::Strategy, 2000);     // 2ms
        alert_thresholds.insert(LatencyStage::Execution, 5000);    // 5ms
        alert_thresholds.insert(LatencyStage::EndToEnd, 25000);    // 25ms (目標)
        
        Self {
            alert_thresholds,
            window_size: 1000,
            report_interval_ms: 5000, // 每 5 秒報告一次
        }
    }
}

/// 延遲監控服務
#[derive(Debug)]
pub struct LatencyMonitor {
    config: LatencyMonitorConfig,
    /// 各階段的滾動統計窗口
    windows: Arc<Mutex<HashMap<LatencyStage, Vec<u64>>>>,
    /// 最後報告時間
    last_report_time: Arc<Mutex<MicrosTimestamp>>,
}

impl LatencyMonitor {
    /// 創建新的延遲監控器
    pub fn new(config: LatencyMonitorConfig) -> Self {
        Self {
            config,
            windows: Arc::new(Mutex::new(HashMap::new())),
            last_report_time: Arc::new(Mutex::new(now_micros())),
        }
    }

    /// 記錄延遲測量
    pub fn record_latency(&self, stage: LatencyStage, latency_micros: u64) {
        // 檢查是否超過告警閾值
        if let Some(&threshold) = self.config.alert_thresholds.get(&stage) {
            if latency_micros > threshold {
                warn!("延遲告警: {} 階段延遲 {}μs 超過閾值 {}μs", 
                      stage.as_str(), latency_micros, threshold);
            }
        }

        // 添加到滾動窗口
        if let Ok(mut windows) = self.windows.lock() {
            let window = windows.entry(stage).or_insert_with(Vec::new);
            window.push(latency_micros);
            
            // 保持窗口大小
            if window.len() > self.config.window_size {
                window.remove(0);
            }
        }

        // 檢查是否需要生成報告
        self.maybe_generate_report();
    }

    /// 獲取指定階段的統計信息
    pub fn get_stage_stats(&self, stage: LatencyStage) -> Option<LatencyStageStats> {
        if let Ok(windows) = self.windows.lock() {
            if let Some(samples) = windows.get(&stage) {
                if !samples.is_empty() {
                    return Some(LatencyStageStats::from_samples(stage, samples));
                }
            }
        }
        None
    }

    /// 獲取所有階段的統計信息
    pub fn get_all_stats(&self) -> HashMap<LatencyStage, LatencyStageStats> {
        let mut all_stats = HashMap::new();
        
        if let Ok(windows) = self.windows.lock() {
            for (&stage, samples) in windows.iter() {
                if !samples.is_empty() {
                    let stats = LatencyStageStats::from_samples(stage, samples);
                    all_stats.insert(stage, stats);
                }
            }
        }
        
        all_stats
    }

    /// 重置統計數據
    pub fn reset_stats(&self) {
        if let Ok(mut windows) = self.windows.lock() {
            windows.clear();
        }
        
        if let Ok(mut last_report) = self.last_report_time.lock() {
            *last_report = now_micros();
        }
    }

    /// 檢查並生成定期報告
    fn maybe_generate_report(&self) {
        let now = now_micros();
        let should_report = {
            if let Ok(mut last_report) = self.last_report_time.lock() {
                let elapsed_ms = (now - *last_report) / 1000;
                if elapsed_ms >= self.config.report_interval_ms {
                    *last_report = now;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };

        if should_report {
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
        
        // 按階段順序輸出
        let stages = [
            LatencyStage::Ingestion,
            LatencyStage::Aggregation,
            LatencyStage::Strategy, 
            LatencyStage::Execution,
            LatencyStage::EndToEnd,
        ];

        for stage in &stages {
            if let Some(stats) = all_stats.get(stage) {
                info!("{}: 樣本={}, 平均={:.1}μs, p50={}μs, p95={}μs, p99={}μs", 
                      stats.stage.as_str(),
                      stats.count,
                      stats.mean_micros,
                      stats.p50_micros,
                      stats.p95_micros, 
                      stats.p99_micros);
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
    use std::thread;
    use std::time::Duration;

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
        let config = LatencyMonitorConfig::default();
        let monitor = LatencyMonitor::new(config);
        
        // 添加一些正常的延遲樣本
        for _ in 0..100 {
            monitor.record_latency(LatencyStage::Ingestion, 300);
        }
        
        let health = monitor.check_latency_health(LatencyStage::Ingestion);
        assert_eq!(health, LatencyHealth::Healthy);
        
        // 添加一些高延遲樣本
        for _ in 0..10 {
            monitor.record_latency(LatencyStage::Ingestion, 2000);
        }
        
        let health = monitor.check_latency_health(LatencyStage::Ingestion);
        assert!(matches!(health, LatencyHealth::Critical { .. }));
    }

    #[test] 
    fn test_rolling_window() {
        let mut config = LatencyMonitorConfig::default();
        config.window_size = 5;
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