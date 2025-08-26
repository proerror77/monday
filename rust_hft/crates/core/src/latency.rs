//! 延遲監控工具集
//! 
//! 提供高精度延遲測量和統計功能，專門針對 HFT 熱路徑優化
//! - 微秒級精度時間戳
//! - 零分配延遲統計
//! - 階段式延遲鏈追蹤

use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

/// 微秒級時間戳
pub type MicrosTimestamp = u64;

/// 獲取當前時間戳（微秒）
#[inline]
pub fn now_micros() -> MicrosTimestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

/// 延遲階段定義
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LatencyStage {
    /// 數據接入階段：從交易所接收到進入引擎
    Ingestion,
    /// 聚合階段：從原始數據到市場視圖
    Aggregation, 
    /// 策略階段：從市場視圖到交易意圖
    Strategy,
    /// 執行階段：從交易意圖到訂單發送
    Execution,
    /// 端到端：從接收到發送的完整鏈路
    EndToEnd,
}

impl LatencyStage {
    /// 獲取階段名稱（用於指標標籤）
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ingestion => "ingestion",
            Self::Aggregation => "aggregation", 
            Self::Strategy => "strategy",
            Self::Execution => "execution",
            Self::EndToEnd => "end_to_end",
        }
    }
}

/// 延遲測量點
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyMeasurement {
    pub stage: LatencyStage,
    pub start_time: MicrosTimestamp,
    pub end_time: MicrosTimestamp,
    pub duration_micros: u64,
}

impl LatencyMeasurement {
    /// 創建新的延遲測量
    pub fn new(stage: LatencyStage, start_time: MicrosTimestamp, end_time: MicrosTimestamp) -> Self {
        Self {
            stage,
            start_time,
            end_time,
            duration_micros: end_time.saturating_sub(start_time),
        }
    }

    /// 獲取延遲（微秒）
    #[inline]
    pub fn latency_micros(&self) -> u64 {
        self.duration_micros
    }

    /// 獲取延遲（毫秒）
    #[inline]
    pub fn latency_millis(&self) -> f64 {
        self.duration_micros as f64 / 1000.0
    }
}

/// 延遲追蹤器 - 用於追蹤多階段延遲
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyTracker {
    /// 起始時間戳
    pub origin_time: MicrosTimestamp,
    /// 各階段時間戳
    pub stage_times: Vec<(LatencyStage, MicrosTimestamp)>,
}

impl LatencyTracker {
    /// 創建新的延遲追蹤器
    pub fn new() -> Self {
        Self {
            origin_time: now_micros(),
            stage_times: Vec::new(),
        }
    }

    /// 從指定時間開始追蹤
    pub fn from_time(origin_time: MicrosTimestamp) -> Self {
        Self {
            origin_time,
            stage_times: Vec::new(),
        }
    }

    /// 記錄階段時間點
    pub fn record_stage(&mut self, stage: LatencyStage) {
        let now = now_micros();
        self.stage_times.push((stage, now));
    }

    /// 獲取指定階段的延遲測量
    pub fn get_measurement(&self, stage: LatencyStage) -> Option<LatencyMeasurement> {
        // 找到該階段的時間戳
        let stage_time = self.stage_times.iter()
            .find(|(s, _)| *s == stage)
            .map(|(_, t)| *t)?;

        // 找到前一個階段的時間戳，如果沒有則使用起始時間
        let prev_time = if let Some(stage_idx) = self.stage_times.iter().position(|(s, _)| *s == stage) {
            if stage_idx == 0 {
                self.origin_time
            } else {
                self.stage_times[stage_idx - 1].1
            }
        } else {
            return None;
        };

        Some(LatencyMeasurement::new(stage, prev_time, stage_time))
    }

    /// 獲取端到端延遲
    pub fn get_end_to_end(&self) -> Option<LatencyMeasurement> {
        let last_time = self.stage_times.last()?.1;
        Some(LatencyMeasurement::new(LatencyStage::EndToEnd, self.origin_time, last_time))
    }

    /// 獲取所有階段的延遲測量
    pub fn get_all_measurements(&self) -> Vec<LatencyMeasurement> {
        let mut measurements = Vec::new();
        
        // 各階段延遲
        for &stage in &[
            LatencyStage::Ingestion,
            LatencyStage::Aggregation, 
            LatencyStage::Strategy,
            LatencyStage::Execution,
        ] {
            if let Some(measurement) = self.get_measurement(stage) {
                measurements.push(measurement);
            }
        }
        
        // 端到端延遲
        if let Some(end_to_end) = self.get_end_to_end() {
            measurements.push(end_to_end);
        }
        
        measurements
    }
}

impl Default for LatencyTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// 延遲統計收集器
#[derive(Debug, Default)]
pub struct LatencyStats {
    /// 各階段延遲統計（微秒）
    pub ingestion_micros: Vec<u64>,
    pub aggregation_micros: Vec<u64>,
    pub strategy_micros: Vec<u64>,
    pub execution_micros: Vec<u64>,
    pub end_to_end_micros: Vec<u64>,
}

impl LatencyStats {
    /// 創建新的延遲統計收集器
    pub fn new() -> Self {
        Self::default()
    }

    /// 添加測量結果
    pub fn add_measurement(&mut self, measurement: &LatencyMeasurement) {
        let micros = measurement.latency_micros();
        match measurement.stage {
            LatencyStage::Ingestion => self.ingestion_micros.push(micros),
            LatencyStage::Aggregation => self.aggregation_micros.push(micros),
            LatencyStage::Strategy => self.strategy_micros.push(micros),
            LatencyStage::Execution => self.execution_micros.push(micros),
            LatencyStage::EndToEnd => self.end_to_end_micros.push(micros),
        }
    }

    /// 添加追蹤器的所有測量結果
    pub fn add_tracker(&mut self, tracker: &LatencyTracker) {
        for measurement in tracker.get_all_measurements() {
            self.add_measurement(&measurement);
        }
    }

    /// 獲取指定階段的統計信息
    pub fn get_stage_stats(&self, stage: LatencyStage) -> LatencyStageStats {
        let samples = match stage {
            LatencyStage::Ingestion => &self.ingestion_micros,
            LatencyStage::Aggregation => &self.aggregation_micros,
            LatencyStage::Strategy => &self.strategy_micros,
            LatencyStage::Execution => &self.execution_micros,
            LatencyStage::EndToEnd => &self.end_to_end_micros,
        };

        LatencyStageStats::from_samples(stage, samples)
    }
}

/// 階段延遲統計
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyStageStats {
    pub stage: LatencyStage,
    pub count: u64,
    pub min_micros: u64,
    pub max_micros: u64,
    pub mean_micros: f64,
    pub p50_micros: u64,
    pub p95_micros: u64,
    pub p99_micros: u64,
}

impl LatencyStageStats {
    /// 從樣本創建統計信息
    pub fn from_samples(stage: LatencyStage, samples: &[u64]) -> Self {
        if samples.is_empty() {
            return Self {
                stage,
                count: 0,
                min_micros: 0,
                max_micros: 0,
                mean_micros: 0.0,
                p50_micros: 0,
                p95_micros: 0,
                p99_micros: 0,
            };
        }

        let mut sorted_samples = samples.to_vec();
        sorted_samples.sort_unstable();

        let count = samples.len() as u64;
        let min_micros = sorted_samples[0];
        let max_micros = sorted_samples[samples.len() - 1];
        let mean_micros = samples.iter().sum::<u64>() as f64 / samples.len() as f64;

        let p50_micros = percentile(&sorted_samples, 0.5);
        let p95_micros = percentile(&sorted_samples, 0.95);
        let p99_micros = percentile(&sorted_samples, 0.99);

        Self {
            stage,
            count,
            min_micros,
            max_micros,
            mean_micros,
            p50_micros,
            p95_micros,
            p99_micros,
        }
    }
}

/// 計算百分位數
fn percentile(sorted_data: &[u64], percentile: f64) -> u64 {
    if sorted_data.is_empty() {
        return 0;
    }
    
    let index = (percentile * (sorted_data.len() - 1) as f64).round() as usize;
    sorted_data[index.min(sorted_data.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_latency_tracker() {
        let mut tracker = LatencyTracker::new();
        let start_time = tracker.origin_time;
        
        // 模擬各階段處理
        thread::sleep(Duration::from_millis(1));
        tracker.record_stage(LatencyStage::Ingestion);
        
        thread::sleep(Duration::from_millis(1)); 
        tracker.record_stage(LatencyStage::Aggregation);
        
        thread::sleep(Duration::from_millis(1));
        tracker.record_stage(LatencyStage::Strategy);
        
        thread::sleep(Duration::from_millis(1));
        tracker.record_stage(LatencyStage::Execution);
        
        // 檢查測量結果
        let ingestion = tracker.get_measurement(LatencyStage::Ingestion).unwrap();
        assert!(ingestion.latency_micros() > 500); // 至少 0.5ms
        assert!(ingestion.latency_micros() < 5000); // 小於 5ms
        
        let end_to_end = tracker.get_end_to_end().unwrap();
        assert!(end_to_end.latency_micros() > 2000); // 至少 2ms
        assert!(end_to_end.latency_micros() < 20000); // 小於 20ms
        
        // 檢查所有測量結果
        let all_measurements = tracker.get_all_measurements();
        assert_eq!(all_measurements.len(), 5); // 4個階段 + 端到端
    }

    #[test]
    fn test_latency_stats() {
        let mut stats = LatencyStats::new();
        
        // 添加一些測量樣本
        for i in 1..=100 {
            let measurement = LatencyMeasurement {
                stage: LatencyStage::Ingestion,
                start_time: 0,
                end_time: i * 10,
                duration_micros: i * 10,
            };
            stats.add_measurement(&measurement);
        }
        
        let stage_stats = stats.get_stage_stats(LatencyStage::Ingestion);
        assert_eq!(stage_stats.count, 100);
        assert_eq!(stage_stats.min_micros, 10);
        assert_eq!(stage_stats.max_micros, 1000);
        assert!((stage_stats.mean_micros - 505.0).abs() < 1.0);
        assert!(stage_stats.p50_micros >= 500 && stage_stats.p50_micros <= 510);
        assert!(stage_stats.p95_micros >= 940 && stage_stats.p95_micros <= 960);
        assert!(stage_stats.p99_micros >= 980 && stage_stats.p99_micros <= 1000);
    }
}