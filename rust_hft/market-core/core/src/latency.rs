//! 延遲監控工具集
//!
//! 提供高精度延遲測量和統計功能，專門針對 HFT 熱路徑優化
//! - 微秒級精度時間戳
//! - 零分配延遲統計
//! - 階段式延遲鏈追蹤

#[cfg(feature = "histogram")]
use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

static MONOTONIC_ORIGIN: OnceLock<Instant> = OnceLock::new();

#[inline]
pub fn monotonic_micros() -> MicrosTimestamp {
    let origin = MONOTONIC_ORIGIN.get_or_init(Instant::now);
    origin.elapsed().as_micros() as u64
}

#[inline]
fn monotonic_now() -> Instant {
    Instant::now()
}

/// 延遲階段定義
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LatencyStage {
    /// WebSocket frame 接收（epoll 喚醒 → 緩衝前）
    WsReceive,
    /// JSON 解析完成
    Parsing,
    /// 數據接入階段：從交易所接收到進入引擎
    Ingestion,
    /// 聚合階段：從原始數據到市場視圖
    Aggregation,
    /// 策略階段：從市場視圖到交易意圖
    Strategy,
    /// 風控階段：策略意圖的風控檢查
    Risk,
    /// 執行階段：從交易意圖到訂單排隊
    Execution,
    /// 訂單送出至交易所
    Submission,
    /// 端到端：從接收到發送的完整鏈路（兼容指標）
    EndToEnd,
}

impl LatencyStage {
    /// 核心延遲階段（不含 EndToEnd 聚合）
    pub const fn core_stages() -> [LatencyStage; 8] {
        [
            LatencyStage::WsReceive,
            LatencyStage::Parsing,
            LatencyStage::Ingestion,
            LatencyStage::Aggregation,
            LatencyStage::Strategy,
            LatencyStage::Risk,
            LatencyStage::Execution,
            LatencyStage::Submission,
        ]
    }

    /// 核心階段加上端到端聚合
    pub const fn all_stages() -> [LatencyStage; 9] {
        [
            LatencyStage::WsReceive,
            LatencyStage::Parsing,
            LatencyStage::Ingestion,
            LatencyStage::Aggregation,
            LatencyStage::Strategy,
            LatencyStage::Risk,
            LatencyStage::Execution,
            LatencyStage::Submission,
            LatencyStage::EndToEnd,
        ]
    }

    /// 獲取階段名稱（用於指標標籤）
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WsReceive => "ws_receive",
            Self::Parsing => "parsing",
            Self::Ingestion => "ingestion",
            Self::Aggregation => "aggregation",
            Self::Strategy => "strategy",
            Self::Risk => "risk",
            Self::Execution => "execution",
            Self::Submission => "submission",
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
    pub fn new(
        stage: LatencyStage,
        start_time: MicrosTimestamp,
        end_time: MicrosTimestamp,
    ) -> Self {
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
    #[serde(skip, default = "monotonic_now")]
    origin_instant: Instant,
    /// 各階段時間偏移（相對於起點，微秒）
    pub stage_offsets: Vec<(LatencyStage, u64)>,
}

impl LatencyTracker {
    /// 創建新的延遲追蹤器
    pub fn new() -> Self {
        Self {
            origin_time: now_micros(),
            origin_instant: Instant::now(),
            stage_offsets: Vec::new(),
        }
    }

    /// 從指定時間開始追蹤
    pub fn from_time(origin_time: MicrosTimestamp) -> Self {
        Self {
            origin_time,
            origin_instant: Instant::now(),
            stage_offsets: Vec::new(),
        }
    }

    /// 從 monotonic 時刻開始追蹤（例如 WsFrameMetrics）
    pub fn from_monotonic(origin_micros: MicrosTimestamp) -> Self {
        let now_mono = monotonic_micros();
        let delta = now_mono.saturating_sub(origin_micros);
        let origin_instant = Instant::now() - Duration::from_micros(delta);
        let origin_wall = now_micros().saturating_sub(delta);
        Self {
            origin_time: origin_wall,
            origin_instant,
            stage_offsets: Vec::new(),
        }
    }

    /// 記錄階段時間點
    pub fn record_stage(&mut self, stage: LatencyStage) {
        let now = Instant::now();
        let offset = now
            .saturating_duration_since(self.origin_instant)
            .as_micros() as u64;
        self.stage_offsets.push((stage, offset));
    }

    /// 以指定偏移（微秒）記錄階段
    pub fn record_stage_with_offset(&mut self, stage: LatencyStage, offset_micros: u64) {
        self.stage_offsets.push((stage, offset_micros));
    }

    /// 獲取指定階段的延遲測量
    pub fn get_measurement(&self, stage: LatencyStage) -> Option<LatencyMeasurement> {
        let stage_idx = self.stage_offsets.iter().position(|(s, _)| *s == stage)?;

        let stage_offset = self.stage_offsets[stage_idx].1;
        let prev_offset = if stage_idx == 0 {
            0
        } else {
            self.stage_offsets[stage_idx - 1].1
        };

        let start_time = self.origin_time.saturating_add(prev_offset);
        let end_time = self.origin_time.saturating_add(stage_offset);

        Some(LatencyMeasurement::new(stage, start_time, end_time))
    }

    /// 獲取端到端延遲
    pub fn get_end_to_end(&self) -> Option<LatencyMeasurement> {
        let last_offset = self.stage_offsets.last()?.1;
        Some(LatencyMeasurement::new(
            LatencyStage::EndToEnd,
            self.origin_time,
            self.origin_time.saturating_add(last_offset),
        ))
    }

    /// 獲取所有階段的延遲測量
    pub fn get_all_measurements(&self) -> Vec<LatencyMeasurement> {
        let mut measurements = Vec::with_capacity(self.stage_offsets.len() + 1);

        for (stage, _) in &self.stage_offsets {
            if let Some(measurement) = self.get_measurement(*stage) {
                measurements.push(measurement);
            }
        }

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
    pub ws_receive_micros: Vec<u64>,
    pub parsing_micros: Vec<u64>,
    pub ingestion_micros: Vec<u64>,
    pub aggregation_micros: Vec<u64>,
    pub strategy_micros: Vec<u64>,
    pub risk_micros: Vec<u64>,
    pub execution_micros: Vec<u64>,
    pub submission_micros: Vec<u64>,
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
            LatencyStage::WsReceive => self.ws_receive_micros.push(micros),
            LatencyStage::Parsing => self.parsing_micros.push(micros),
            LatencyStage::Ingestion => self.ingestion_micros.push(micros),
            LatencyStage::Aggregation => self.aggregation_micros.push(micros),
            LatencyStage::Strategy => self.strategy_micros.push(micros),
            LatencyStage::Risk => self.risk_micros.push(micros),
            LatencyStage::Execution => self.execution_micros.push(micros),
            LatencyStage::Submission => self.submission_micros.push(micros),
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
            LatencyStage::WsReceive => &self.ws_receive_micros,
            LatencyStage::Parsing => &self.parsing_micros,
            LatencyStage::Ingestion => &self.ingestion_micros,
            LatencyStage::Aggregation => &self.aggregation_micros,
            LatencyStage::Strategy => &self.strategy_micros,
            LatencyStage::Risk => &self.risk_micros,
            LatencyStage::Execution => &self.execution_micros,
            LatencyStage::Submission => &self.submission_micros,
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

    /// 從 HDR Histogram 對象產生統計信息
    ///
    /// 注意：此方法需要啟用 `histogram` feature
    #[cfg(feature = "histogram")]
    pub fn from_histogram(stage: LatencyStage, histogram: &Histogram<u64>) -> Self {
        if histogram.is_empty() {
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

        Self {
            stage,
            count: histogram.len(),
            min_micros: histogram.min(),
            max_micros: histogram.max(),
            mean_micros: histogram.mean(),
            p50_micros: histogram.value_at_quantile(0.5),
            p95_micros: histogram.value_at_quantile(0.95),
            p99_micros: histogram.value_at_quantile(0.99),
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

        // 模擬各階段處理
        tracker.record_stage(LatencyStage::WsReceive);
        thread::sleep(Duration::from_millis(1));
        tracker.record_stage(LatencyStage::Parsing);
        thread::sleep(Duration::from_millis(1));
        tracker.record_stage(LatencyStage::Ingestion);

        thread::sleep(Duration::from_millis(1));
        tracker.record_stage(LatencyStage::Aggregation);

        thread::sleep(Duration::from_millis(1));
        tracker.record_stage(LatencyStage::Strategy);

        thread::sleep(Duration::from_millis(1));
        tracker.record_stage(LatencyStage::Risk);

        thread::sleep(Duration::from_millis(1));
        tracker.record_stage(LatencyStage::Execution);

        thread::sleep(Duration::from_millis(1));
        tracker.record_stage(LatencyStage::Submission);

        // 檢查測量結果
        let ingestion = tracker.get_measurement(LatencyStage::Ingestion).unwrap();
        assert!(ingestion.latency_micros() > 500); // 至少 0.5ms
        assert!(ingestion.latency_micros() < 5000); // 小於 5ms

        let end_to_end = tracker.get_end_to_end().unwrap();
        assert!(end_to_end.latency_micros() > 4000); // 至少 4ms
        assert!(end_to_end.latency_micros() < 40000); // 小於 40ms

        // 檢查所有測量結果
        let all_measurements = tracker.get_all_measurements();
        assert_eq!(all_measurements.len(), 9); // 8 個階段 + 端到端
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

    #[test]
    fn test_manual_stage_offsets() {
        let mut tracker = LatencyTracker::from_monotonic(10_000);
        tracker.record_stage_with_offset(LatencyStage::WsReceive, 0);
        tracker.record_stage_with_offset(LatencyStage::Parsing, 150);

        // 後續階段使用實時計算
        tracker.record_stage(LatencyStage::Ingestion);

        let parsing = tracker
            .get_measurement(LatencyStage::Parsing)
            .expect("parsing measurement");
        assert_eq!(parsing.duration_micros, 150);
    }
}
