//! 事件攝取與有界匯流
//!
//! 實現 Adapter → Engine 的有界匯流管線：
//! - SPSC ring buffer 避免無界隊列堆積
//! - Last-wins 策略處理滿載情況
//! - 時間戳控制 staleness
//! - flip 策略控制快照發佈頻率

use super::ring_buffer::{spsc_ring_buffer, SpscConsumer, SpscProducer};
use hdrhistogram::Histogram;
use hft_core::{now_micros, HftError, LatencyStage, LatencyTracker, Symbol};
use ports::{MarketEvent, TrackedMarketEvent};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Notify;
use tracing::{debug, error, warn};

const STALE_WARN_INTERVAL_US: u64 = 500_000; // 0.5 秒
const LATENCY_HISTOGRAM_MAX_US: u64 = 60_000_000; // 60 秒上限，覆蓋慢速環境
const LATENCY_HISTOGRAM_SIGFIGS: u8 = 3;

/// 事件攝取配置
#[derive(Debug, Clone)]
pub struct IngestionConfig {
    /// Ring buffer 容量 (power of 2)
    pub queue_capacity: usize,
    /// 事件陳舊度閾值 (微秒)
    pub stale_threshold_us: u64,
    /// 快照發佈策略
    pub flip_policy: FlipPolicy,
    /// 丟棄策略：滿載時如何處理
    pub backpressure_policy: BackpressurePolicy,
}

impl Default for IngestionConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 32768,    // 32K events
            stale_threshold_us: 3000, // 3ms
            flip_policy: FlipPolicy::OnUpdate,
            // 使用自適應背壓策略作為安全默認值
            backpressure_policy: BackpressurePolicy::Adaptive {
                low_threshold: 0.6,   // 60% 以下使用 LastWins
                high_threshold: 0.85, // 85% 以上使用 DropNew
            },
        }
    }
}

impl IngestionConfig {
    /// 創建保守的生產環境配置
    pub fn production_safe() -> Self {
        Self {
            queue_capacity: 65536,    // 64K events - 更大緩衝區
            stale_threshold_us: 2000, // 2ms - 更嚴格的新鮮度要求
            flip_policy: FlipPolicy::OnUpdate,
            backpressure_policy: BackpressurePolicy::DropNew, // 最保守策略
        }
    }

    /// 創建高性能配置（適用於低延遲場景）
    pub fn high_performance() -> Self {
        Self {
            queue_capacity: 16384,    // 16K events - 較小緩衝區
            stale_threshold_us: 1000, // 1ms - 非常嚴格
            flip_policy: FlipPolicy::OnUpdate,
            backpressure_policy: BackpressurePolicy::Adaptive {
                low_threshold: 0.5,  // 50% 以下使用 LastWins
                high_threshold: 0.8, // 80% 以上使用 DropNew
            },
        }
    }
}

/// 快照發佈策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FlipPolicy {
    /// 每次更新都發佈
    OnUpdate,
    /// 定時發佈 (微秒間隔)
    OnTimer(u64),
    /// 利用率閾值觸發 (0.0-1.0)
    OnUtilization(f64),
}

/// 背壓策略
#[derive(Debug, Clone)]
pub enum BackpressurePolicy {
    /// 丟棄新事件，保留已入隊事件（最安全）
    DropNew,
    /// 丟棄舊事件，保留最新事件 (last-wins)
    LastWins,
    /// 阻塞直到有空間 (可能導致死鎖)
    Block,
    /// 自適應策略：根據利用率自動選擇 (推薦用於生產環境)
    Adaptive {
        /// 低利用率閾值，小於此值使用 LastWins
        low_threshold: f64,
        /// 高利用率閾值，大於此值使用 DropNew
        high_threshold: f64,
    },
}

/// 事件攝取統計
#[derive(Debug)]
pub struct IngestionMetrics {
    pub events_received: u64,
    pub events_dropped: u64,
    pub events_stale: u64,
    pub ring_utilization_max: f64,
    pub flip_count: u64,
    /// 攝取階段延遲統計（微秒） - HDR 直方圖
    latency_histogram: Histogram<u64>,
    /// 最近的延遲測量（用於實時監控）
    pub recent_latency_micros: Option<u64>,
    last_stale_warn_ts: Option<u64>,
    suppressed_stale_warnings: u64,
}

impl IngestionMetrics {
    pub fn new() -> Self {
        let latency_histogram =
            Histogram::new_with_bounds(1, LATENCY_HISTOGRAM_MAX_US, LATENCY_HISTOGRAM_SIGFIGS)
                .expect("latency histogram bounds");
        Self {
            events_received: 0,
            events_dropped: 0,
            events_stale: 0,
            ring_utilization_max: 0.0,
            flip_count: 0,
            latency_histogram,
            recent_latency_micros: None,
            last_stale_warn_ts: None,
            suppressed_stale_warnings: 0,
        }
    }

    pub fn record_latency(&mut self, latency: u64) {
        if let Err(err) = self.latency_histogram.record(latency) {
            warn!("记录摄取延迟失败: {}", err);
        }
        self.recent_latency_micros = Some(latency);
    }

    pub fn record_stale_warn(&mut self, now_us: u64, interval_us: u64) -> Option<u64> {
        match self.last_stale_warn_ts {
            Some(last) if now_us.saturating_sub(last) < interval_us => {
                self.suppressed_stale_warnings = self.suppressed_stale_warnings.saturating_add(1);
                None
            }
            _ => {
                let suppressed = self.suppressed_stale_warnings;
                self.suppressed_stale_warnings = 0;
                self.last_stale_warn_ts = Some(now_us);
                Some(suppressed)
            }
        }
    }

    pub fn latency_histogram(&self) -> &Histogram<u64> {
        &self.latency_histogram
    }

    pub fn average_latency(&self) -> Option<f64> {
        if self.latency_histogram.len() == 0 {
            return None;
        }
        Some(self.latency_histogram.mean())
    }

    pub fn latency_percentile(&self, percentile: f64) -> Option<u64> {
        if self.latency_histogram.len() == 0 {
            return None;
        }
        let pct = percentile.clamp(0.0, 100.0);
        Some(self.latency_histogram.value_at_quantile(pct / 100.0))
    }
}

impl Default for IngestionMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// 背壓狀態報告
#[derive(Debug, Clone)]
pub struct BackpressureStatus {
    /// 當前隊列利用率 (0.0-1.0)
    pub utilization: f64,
    /// 是否處於背壓狀態
    pub is_under_pressure: bool,
    /// 累計丟棄的事件數量
    pub events_dropped_total: u64,
    /// 建議操作
    pub recommended_action: String,
    /// 隊列容量
    pub queue_capacity: usize,
}

/// 單向事件攝取器 (Producer 端)
pub struct EventIngester {
    producer: SpscProducer<TrackedMarketEvent>,
    config: IngestionConfig,
    metrics: IngestionMetrics,
    #[allow(dead_code)]
    last_flip_time: u64,
    /// 引擎喚醒通知器（可選）：成功入隊時喚醒引擎
    engine_notify: Option<Arc<Notify>>,
}

impl EventIngester {
    pub fn new(config: IngestionConfig) -> (Self, EventConsumer) {
        let (producer, consumer) = spsc_ring_buffer(config.queue_capacity);

        let ingester = Self {
            producer,
            config: config.clone(),
            metrics: IngestionMetrics::default(),
            last_flip_time: current_timestamp_us(),
            engine_notify: None,
        };

        let event_consumer = EventConsumer::new(consumer, config);

        (ingester, event_consumer)
    }

    /// 設置引擎喚醒通知器（成功入隊時喚醒）
    pub fn set_engine_notify(&mut self, notify: Arc<Notify>) {
        self.engine_notify = Some(notify);
    }

    /// 攝取單個事件，應用背壓與陳舊度策略
    pub fn ingest(&mut self, event: MarketEvent) -> Result<(), HftError> {
        self.ingest_with_tracker(event, None)
    }

    pub fn ingest_with_tracker(
        &mut self,
        event: MarketEvent,
        provided_tracker: Option<LatencyTracker>,
    ) -> Result<(), HftError> {
        let _ingestion_start = now_micros();
        self.metrics.events_received += 1;

        // 先檢查陳舊度與記錄延遲，再構造帶追蹤事件（避免多餘拷貝）
        let event_ts_opt = self.extract_timestamp(&event);
        if let Some(event_ts) = event_ts_opt {
            let now = current_timestamp_us();
            let delay = now.saturating_sub(event_ts);

            // 記錄攝取延遲統計
            self.metrics.record_latency(delay);
            #[cfg(feature = "metrics")]
            infra_metrics::MetricsRegistry::global().record_ingestion_latency(delay as f64);

            // Prometheus 直方圖打點（可選）

            // 記錄每個事件的延遲情況
            debug!(
                "EventIngester 攝取延遲: {}μs, 閾值: {}μs",
                delay, self.config.stale_threshold_us
            );

            if delay > self.config.stale_threshold_us {
                self.metrics.events_stale += 1;
                #[cfg(feature = "metrics")]
                {
                    infra_metrics::MetricsRegistry::global().inc_events_stale();
                    infra_metrics::MetricsRegistry::global()
                        .record_staleness(delay as f64 / 1000.0);
                }
                if let Some(suppressed) =
                    self.metrics.record_stale_warn(now, STALE_WARN_INTERVAL_US)
                {
                    if suppressed > 0 {
                        warn!(
                            suppressed,
                            "丟棄陳舊事件: {}μs 延遲 > {}μs 閾值（過去 {} 筆已合併）",
                            delay,
                            self.config.stale_threshold_us,
                            suppressed
                        );
                    } else {
                        warn!(
                            "丟棄陳舊事件: {}μs 延遲 > {}μs 閾值",
                            delay, self.config.stale_threshold_us
                        );
                    }
                }
                return Ok(()); // 靜默丟棄陳舊事件
            }
        }

        // 創建延遲追蹤器（若事件有時間戳，使用其作為起點）
        let mut tracker = if let Some(tracker) = provided_tracker {
            tracker
        } else if let Some(event_ts) = event_ts_opt {
            let mut tracker = LatencyTracker::from_time(event_ts);
            tracker.record_stage_with_offset(LatencyStage::WsReceive, 0);
            tracker.record_stage_with_offset(LatencyStage::Parsing, 0);
            tracker
        } else {
            let mut tracker = LatencyTracker::new();
            tracker.record_stage_with_offset(LatencyStage::WsReceive, 0);
            tracker.record_stage_with_offset(LatencyStage::Parsing, 0);
            tracker
        };
        tracker.record_stage(LatencyStage::Ingestion);

        // 創建帶追蹤的事件（零拷貝：移動事件所有權）
        let tracked_event = TrackedMarketEvent { event, tracker };

        // 嘗試發送，應用背壓策略
        match self.producer.send(tracked_event) {
            Ok(()) => {
                debug!("EventIngester 事件成功入隊到 ring buffer");

                // 更新利用率統計
                let utilization = self.producer.utilization();
                if utilization > self.metrics.ring_utilization_max {
                    self.metrics.ring_utilization_max = utilization;
                }

                // 成功入隊，喚醒引擎處理
                if let Some(notify) = &self.engine_notify {
                    notify.notify_one();
                }

                Ok(())
            }
            Err(tracked_event) => {
                // Ring buffer 滿載，應用背壓策略
                let current_utilization = self.producer.utilization();

                let effective_policy = match &self.config.backpressure_policy {
                    BackpressurePolicy::Adaptive {
                        low_threshold,
                        high_threshold,
                    } => {
                        if current_utilization <= *low_threshold {
                            BackpressurePolicy::LastWins
                        } else if current_utilization >= *high_threshold {
                            BackpressurePolicy::DropNew
                        } else {
                            // 中間區域：根據利用率線性插值決定
                            let ratio = (current_utilization - low_threshold)
                                / (high_threshold - low_threshold);
                            if ratio < 0.5 {
                                BackpressurePolicy::LastWins
                            } else {
                                BackpressurePolicy::DropNew
                            }
                        }
                    }
                    policy => policy.clone(),
                };

                match effective_policy {
                    BackpressurePolicy::DropNew => {
                        self.metrics.events_dropped += 1;
                        #[cfg(feature = "metrics")]
                        infra_metrics::MetricsRegistry::global().inc_events_dropped();
                        warn!(
                            "Ring buffer 滿載 ({:.1}% 利用率)，丟棄新事件",
                            current_utilization * 100.0
                        );
                        Ok(())
                    }
                    BackpressurePolicy::LastWins => {
                        // 使用 force_send 實現 Last-Wins：強制插入新事件，丟棄最舊事件
                        if self.producer.force_send(tracked_event) {
                            self.metrics.events_dropped += 1; // 有一個舊事件被丟棄
                            #[cfg(feature = "metrics")]
                            infra_metrics::MetricsRegistry::global().inc_events_dropped();
                            warn!(
                                "Last-wins ({:.1}% 利用率): 丟棄最舊事件，插入新事件",
                                current_utilization * 100.0
                            );
                            // 成功入隊（丟棄舊事件），喚醒引擎處理
                            if let Some(notify) = &self.engine_notify {
                                notify.notify_one();
                            }
                            Ok(())
                        } else {
                            self.metrics.events_dropped += 1;
                            #[cfg(feature = "metrics")]
                            infra_metrics::MetricsRegistry::global().inc_events_dropped();
                            error!("Last-wins force_send 失敗");
                            Ok(())
                        }
                    }
                    BackpressurePolicy::Block => {
                        error!("Ring buffer 滿載，阻塞模式暫不支援");
                        Err(HftError::Generic {
                            message: "Ring buffer full, blocking not implemented".to_string(),
                        })
                    }
                    BackpressurePolicy::Adaptive { .. } => {
                        unreachable!("Adaptive policy should have been resolved above")
                    }
                }
            }
        }
    }

    /// 提取事件時間戳
    fn extract_timestamp(&self, event: &MarketEvent) -> Option<u64> {
        match event {
            MarketEvent::Snapshot(s) => Some(s.timestamp),
            MarketEvent::Update(u) => Some(u.timestamp),
            MarketEvent::Trade(t) => Some(t.timestamp),
            MarketEvent::Bar(b) => Some(b.close_time),
            MarketEvent::Arbitrage(a) => Some(a.timestamp),
            MarketEvent::Disconnect { .. } => Some(current_timestamp_us()),
        }
    }

    /// 獲取攝取統計
    pub fn metrics(&self) -> &IngestionMetrics {
        &self.metrics
    }

    /// 重置統計
    pub fn reset_metrics(&mut self) {
        self.metrics = IngestionMetrics::default();
    }

    /// 獲取當前背壓狀態
    pub fn backpressure_status(&self) -> BackpressureStatus {
        let utilization = self.producer.utilization();
        let is_under_pressure = utilization > 0.8; // 80% 以上認為有背壓
        let recommended_action = match &self.config.backpressure_policy {
            BackpressurePolicy::Adaptive {
                low_threshold,
                high_threshold,
            } => {
                if utilization <= *low_threshold {
                    "Normal operation - LastWins active".to_string()
                } else if utilization >= *high_threshold {
                    "High pressure - DropNew active".to_string()
                } else {
                    "Medium pressure - Adaptive switching".to_string()
                }
            }
            BackpressurePolicy::DropNew => "DropNew policy active".to_string(),
            BackpressurePolicy::LastWins => "LastWins policy active".to_string(),
            BackpressurePolicy::Block => "Block policy active".to_string(),
        };

        BackpressureStatus {
            utilization,
            is_under_pressure,
            events_dropped_total: self.metrics.events_dropped,
            recommended_action,
            queue_capacity: self.config.queue_capacity,
        }
    }
}

/// 事件消費者 (Consumer 端，engine 主循環使用)
pub struct EventConsumer {
    consumer: SpscConsumer<TrackedMarketEvent>,
    config: IngestionConfig,
    symbol_state: FxHashMap<Symbol, SymbolState>,
    flip_metrics: FlipMetrics,
    /// 引擎唤醒通知器（可选）
    engine_notify: Option<Arc<Notify>>,
}

#[derive(Debug)]
#[allow(dead_code)]
struct SymbolState {
    last_snapshot_time: u64,
    last_sequence: u64,
    events_since_flip: u32,
}

#[derive(Debug, Default)]
pub struct FlipMetrics {
    pub total_flips: u64,
    pub last_flip_time: u64,
    pub events_processed: u64,
}

impl EventConsumer {
    fn new(consumer: SpscConsumer<TrackedMarketEvent>, config: IngestionConfig) -> Self {
        Self {
            consumer,
            config,
            symbol_state: FxHashMap::default(),
            flip_metrics: FlipMetrics::default(),
            engine_notify: None,
        }
    }

    /// 设置引擎唤醒通知器
    pub fn set_engine_notify(&mut self, notify: Arc<Notify>) {
        self.engine_notify = Some(notify);
    }

    /// 消費事件，返回是否應該觸發快照發佈
    pub fn consume_events(&mut self, mut callback: impl FnMut(TrackedMarketEvent) -> bool) -> bool {
        let mut should_flip = false;
        let mut events_processed: u32 = 0;
        let utilization = self.consumer.utilization();
        let batch_limit: u32 = if utilization > 0.8 { 256 } else { 64 };

        // 批量消費事件
        while let Some(tracked_event) = self.consumer.recv() {
            self.flip_metrics.events_processed += 1;
            events_processed += 1;

            // 更新 symbol 狀態
            self.update_symbol_state(&tracked_event.event);

            // 調用回調處理事件
            let callback_wants_flip = callback(tracked_event);
            should_flip = should_flip || callback_wants_flip;

            // 檢查是否達到批處理限制
            if events_processed >= batch_limit {
                debug!(
                    "批處理達到限制，暫停消費 (limit={}, utilization={:.2})",
                    batch_limit, utilization
                );
                break;
            }
        }

        // 檢查 flip 策略
        if events_processed > 0 {
            should_flip = should_flip || self.should_flip(events_processed);
        }

        if should_flip {
            self.flip_metrics.total_flips += 1;
            self.flip_metrics.last_flip_time = current_timestamp_us();
        }

        // 如果处理了事件，唤醒引擎进行下一轮处理
        if events_processed > 0 {
            if let Some(notify) = &self.engine_notify {
                notify.notify_one();
            }
        }

        should_flip
    }

    /// 更新 symbol 狀態
    fn update_symbol_state(&mut self, event: &MarketEvent) {
        let (symbol, sequence) = match event {
            MarketEvent::Snapshot(s) => (s.symbol.clone(), s.sequence),
            MarketEvent::Update(u) => (u.symbol.clone(), u.sequence),
            MarketEvent::Trade(t) => (t.symbol.clone(), 0), // Trade 無序號
            MarketEvent::Bar(b) => (b.symbol.clone(), 0),
            MarketEvent::Arbitrage(a) => (a.symbol.clone(), 0),
            MarketEvent::Disconnect { .. } => return,
        };

        let state = self
            .symbol_state
            .entry(symbol)
            .or_insert_with(|| SymbolState {
                last_snapshot_time: current_timestamp_us(),
                last_sequence: 0,
                events_since_flip: 0,
            });

        state.last_sequence = sequence;
        state.events_since_flip += 1;
    }

    /// 檢查是否應該觸發快照發佈
    fn should_flip(&self, events_processed: u32) -> bool {
        let now = current_timestamp_us();

        match &self.config.flip_policy {
            FlipPolicy::OnUpdate => {
                // 每次有事件就觸發
                events_processed > 0
            }
            FlipPolicy::OnTimer(interval_us) => {
                // 定時觸發
                now.saturating_sub(self.flip_metrics.last_flip_time) >= *interval_us
            }
            FlipPolicy::OnUtilization(threshold) => {
                // 利用率觸發
                self.consumer.utilization() >= *threshold
            }
        }
    }

    /// 獲取消費統計
    pub fn flip_metrics(&self) -> &FlipMetrics {
        &self.flip_metrics
    }

    /// 獲取隊列利用率
    pub fn utilization(&self) -> f64 {
        self.consumer.utilization()
    }

    /// 檢查隊列是否為空
    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }
}

/// 獲取當前時間戳 (微秒)
fn current_timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::Symbol;
    use ports::{BookLevel, MarketEvent, MarketSnapshot, TrackedMarketEvent};

    #[test]
    fn test_ingestion_basic() {
        let config = IngestionConfig::default();
        let (mut ingester, mut consumer) = EventIngester::new(config);

        // 創建測試事件
        let snapshot = MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: current_timestamp_us(),
            bids: vec![BookLevel::new_unchecked(50000.0, 1.0)],
            asks: vec![BookLevel::new_unchecked(50001.0, 1.0)],
            sequence: 1,
            source_venue: None,
        };

        // 攝取事件
        assert!(ingester.ingest(MarketEvent::Snapshot(snapshot)).is_ok());

        // 消費事件
        let mut received_events: Vec<TrackedMarketEvent> = Vec::new();
        consumer.consume_events(|event| {
            received_events.push(event);
            false // 不觸發 flip
        });

        assert_eq!(received_events.len(), 1);
        assert!(matches!(received_events[0].event, MarketEvent::Snapshot(_)));
    }

    #[test]
    fn test_stale_event_rejection() {
        let mut config = IngestionConfig::default();
        config.stale_threshold_us = 1000; // 1ms

        let (mut ingester, _consumer) = EventIngester::new(config);

        // 創建陳舊事件 (時間戳為 0)
        let stale_snapshot = MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 0,
            bids: vec![],
            asks: vec![],
            sequence: 1,
            source_venue: None,
        };

        // 應該被拒絕但不返回錯誤
        assert!(ingester
            .ingest(MarketEvent::Snapshot(stale_snapshot))
            .is_ok());
        assert_eq!(ingester.metrics().events_stale, 1);
    }
}
