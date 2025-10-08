//! Prometheus 指標整合（可選）
//!
//! 為 HFT 系統提供分段延遲監控、隊列利用率、事件計數等關鍵指標

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use tracing::debug;

use prometheus::{Gauge, Histogram, HistogramOpts, IntCounter, Registry};

/// 全局指標註冊表
static METRICS_REGISTRY: OnceLock<MetricsRegistry> = OnceLock::new();

// HTTP 服务器模块
#[cfg(feature = "http-server")]
pub mod http_server;

/// HFT 系統指標註冊表
#[derive(Debug)]
pub struct MetricsRegistry {
    pub registry: Registry,

    // 分段延遲直方圖
    pub latency_ingestion: Histogram,
    pub latency_aggregation: Histogram,
    pub latency_strategy: Histogram,
    pub latency_risk: Histogram,
    pub latency_execution: Histogram,
    pub latency_end_to_end: Histogram,
    pub latency_order_ack: Histogram,
    pub latency_order_fill: Histogram,

    // 隊列利用率與計數
    pub queue_utilization: Gauge,
    pub events_processed: IntCounter,
    pub events_dropped: IntCounter,
    pub events_stale: IntCounter,

    // Staleness 指標
    pub staleness_histogram: Histogram,
    pub staleness_count: IntCounter,

    // 快照發佈指標
    pub snapshot_flips: IntCounter,
    pub snapshot_version: Gauge,

    // 執行指標
    pub orders_submitted: IntCounter,
    pub orders_filled: IntCounter,
    pub orders_rejected: IntCounter,

    // 對帳相關
    pub reconcile_runs: IntCounter,
    pub reconcile_exchange_only_found: IntCounter,
    pub reconcile_cancel_sent: IntCounter,
    pub reconcile_errors: IntCounter,

    // P3: 新增執行/快照相關統計
    pub intents_dropped: IntCounter,
    pub snapshot_publish_failed: IntCounter,

    // 引擎統計 Gauges（來自 EngineStatisticsExport 的當前快照）
    pub engine_cycle_count: Gauge,
    pub engine_exec_events_processed: Gauge,
    pub engine_orders_submitted: Gauge,
    pub engine_orders_ack: Gauge,
    pub engine_orders_filled: Gauge,
    pub engine_orders_rejected: Gauge,
    pub engine_orders_canceled: Gauge,

    // 本地就緒狀態跟蹤（不依賴 Prometheus 讀取，使 /readiness 更輕量）
    last_activity_micros: AtomicU64,
    last_queue_utilization_ppm: AtomicU64, // 以百萬分位儲存（ppm），避免 f64 原子
}

/// 引擎統計快照（由 engine 匯出，用於更新 gauges）
#[derive(Debug, Clone)]
pub struct EngineStatisticsExport {
    pub cycle_count: u64,
    pub execution_events_processed: u64,
    pub orders_submitted: u64,
    pub orders_ack: u64,
    pub orders_filled: u64,
    pub orders_rejected: u64,
    pub orders_canceled: u64,
}

impl MetricsRegistry {
    /// 初始化全局指標註冊表
    pub fn init() -> &'static Self {
        METRICS_REGISTRY.get_or_init(|| Self::create_with_prometheus())
    }

    /// 獲取全局指標註冊表
    pub fn global() -> &'static Self {
        Self::init()
    }

    fn create_with_prometheus() -> Self {
        let registry = Registry::new();

        // 延遲直方圖 - 使用微秒，覆蓋 1μs 到 10ms 範圍
        let latency_buckets = vec![
            1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0,
        ];

        let latency_ingestion = Histogram::with_opts(
            HistogramOpts::new(
                "hft_latency_ingestion_microseconds",
                "事件攝取階段延遲 (微秒)",
            )
            .buckets(latency_buckets.clone()),
        )
        .expect("創建攝取延遲直方圖失敗");

        let latency_aggregation = Histogram::with_opts(
            HistogramOpts::new(
                "hft_latency_aggregation_microseconds",
                "聚合處理階段延遲 (微秒)",
            )
            .buckets(latency_buckets.clone()),
        )
        .expect("創建聚合延遲直方圖失敗");

        let latency_strategy = Histogram::with_opts(
            HistogramOpts::new(
                "hft_latency_strategy_microseconds",
                "策略計算階段延遲 (微秒)",
            )
            .buckets(latency_buckets.clone()),
        )
        .expect("創建策略延遲直方圖失敗");

        let latency_risk = Histogram::with_opts(
            HistogramOpts::new("hft_latency_risk_microseconds", "風控檢查階段延遲 (微秒)")
                .buckets(latency_buckets.clone()),
        )
        .expect("創建風控延遲直方圖失敗");

        let latency_execution = Histogram::with_opts(
            HistogramOpts::new(
                "hft_latency_execution_microseconds",
                "執行提交階段延遲 (微秒)",
            )
            .buckets(latency_buckets.clone()),
        )
        .expect("創建執行延遲直方圖失敗");

        let latency_end_to_end = Histogram::with_opts(
            HistogramOpts::new("hft_latency_end_to_end_microseconds", "端到端總延遲 (微秒)")
                .buckets(latency_buckets),
        )
        .expect("創建端到端延遲直方圖失敗");

        // Ack/Fill 直方圖（微秒）
        let latency_order_ack = Histogram::with_opts(
            HistogramOpts::new(
                "hft_order_ack_latency_microseconds",
                "下單到 Ack 延遲 (微秒)",
            )
            .buckets(vec![
                1.0, 2.0, 5.0, 10.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0,
                50_000.0, 100_000.0, 500_000.0,
            ]),
        )
        .expect("創建 Ack 延遲直方圖失敗");

        let latency_order_fill = Histogram::with_opts(
            HistogramOpts::new(
                "hft_order_fill_latency_microseconds",
                "下單到 Fill 延遲 (微秒)",
            )
            .buckets(vec![
                1.0, 2.0, 5.0, 10.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0,
                50_000.0, 100_000.0, 500_000.0,
            ]),
        )
        .expect("創建 Fill 延遲直方圖失敗");

        // 隊列指標
        let queue_utilization =
            Gauge::new("hft_queue_utilization_ratio", "SPSC 隊列利用率 (0.0-1.0)")
                .expect("創建隊列利用率指標失敗");

        let events_processed = IntCounter::new("hft_events_processed_total", "已處理事件總數")
            .expect("創建處理事件計數器失敗");

        let events_dropped = IntCounter::new("hft_events_dropped_total", "已丟棄事件總數")
            .expect("創建丟棄事件計數器失敗");

        let events_stale = IntCounter::new("hft_events_stale_total", "過期事件總數")
            .expect("創建過期事件計數器失敗");

        // Staleness 指標 - 毫秒範圍
        let staleness_buckets = vec![
            1.0, 2.0, 3.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0,
        ];

        let staleness_histogram = Histogram::with_opts(
            HistogramOpts::new("hft_staleness_milliseconds", "數據陳舊度分佈 (毫秒)")
                .buckets(staleness_buckets),
        )
        .expect("創建陳舊度直方圖失敗");

        let staleness_count = IntCounter::new("hft_staleness_events_total", "陳舊事件總數")
            .expect("創建陳舊事件計數器失敗");

        // 快照指標
        let snapshot_flips = IntCounter::new("hft_snapshot_flips_total", "快照翻轉總次數")
            .expect("創建快照翻轉計數器失敗");

        let snapshot_version =
            Gauge::new("hft_snapshot_version", "當前快照版本號").expect("創建快照版本指標失敗");

        // 執行指標
        let orders_submitted = IntCounter::new("hft_orders_submitted_total", "已提交訂單總數")
            .expect("創建提交訂單計數器失敗");

        let orders_filled = IntCounter::new("hft_orders_filled_total", "已成交訂單總數")
            .expect("創建成交訂單計數器失敗");

        let orders_rejected = IntCounter::new("hft_orders_rejected_total", "已拒絕訂單總數")
            .expect("創建拒絕訂單計數器失敗");

        // 引擎統計 Gauges（當前值，不累加）
        let engine_cycle_count = Gauge::new("hft_engine_cycle_count", "引擎循環計數（當前快照）")
            .expect("創建 engine_cycle_count 失敗");
        let engine_exec_events_processed = Gauge::new(
            "hft_engine_exec_events_processed",
            "已處理執行事件數（當前快照）",
        )
        .expect("創建 engine_exec_events_processed 失敗");
        let engine_orders_submitted = Gauge::new(
            "hft_engine_orders_submitted",
            "已提交訂單數（當前快照）",
        )
        .expect("創建 engine_orders_submitted 失敗");
        let engine_orders_ack = Gauge::new("hft_engine_orders_ack", "已 Ack 訂單數（當前快照）")
            .expect("創建 engine_orders_ack 失敗");
        let engine_orders_filled = Gauge::new(
            "hft_engine_orders_filled",
            "已成交訂單數（當前快照）",
        )
        .expect("創建 engine_orders_filled 失敗");
        let engine_orders_rejected = Gauge::new(
            "hft_engine_orders_rejected",
            "已拒絕訂單數（當前快照）",
        )
        .expect("創建 engine_orders_rejected 失敗");
        let engine_orders_canceled = Gauge::new(
            "hft_engine_orders_canceled",
            "已撤銷訂單數（當前快照）",
        )
        .expect("創建 engine_orders_canceled 失敗");

        // 對帳指標
        let reconcile_runs = IntCounter::new("hft_reconcile_runs_total", "對帳執行次數")
            .expect("創建對帳次數指標失敗");
        let reconcile_exchange_only_found = IntCounter::new(
            "hft_reconcile_exchange_only_found_total",
            "發現交換端獨有未結訂單總數",
        )
        .expect("創建對帳交換端獨有指標失敗");
        let reconcile_cancel_sent =
            IntCounter::new("hft_reconcile_cancel_sent_total", "對帳過程中發送撤單數量")
                .expect("創建對帳撤單指標失敗");
        let reconcile_errors = IntCounter::new("hft_reconcile_errors_total", "對帳錯誤計數")
            .expect("創建對帳錯誤指標失敗");

        // 註冊所有指標到註冊表
        registry
            .register(Box::new(latency_ingestion.clone()))
            .expect("註冊攝取延遲指標失敗");
        registry
            .register(Box::new(latency_aggregation.clone()))
            .expect("註冊聚合延遲指標失敗");
        registry
            .register(Box::new(latency_strategy.clone()))
            .expect("註冊策略延遲指標失敗");
        registry
            .register(Box::new(latency_risk.clone()))
            .expect("註冊風控延遲指標失敗");
        registry
            .register(Box::new(latency_execution.clone()))
            .expect("註冊執行延遲指標失敗");
        registry
            .register(Box::new(latency_end_to_end.clone()))
            .expect("註冊端到端延遲指標失敗");
        registry
            .register(Box::new(latency_order_ack.clone()))
            .expect("註冊 Ack 延遲指標失敗");
        registry
            .register(Box::new(latency_order_fill.clone()))
            .expect("註冊 Fill 延遲指標失敗");

        registry
            .register(Box::new(queue_utilization.clone()))
            .expect("註冊隊列利用率指標失敗");
        registry
            .register(Box::new(events_processed.clone()))
            .expect("註冊處理事件指標失敗");
        registry
            .register(Box::new(events_dropped.clone()))
            .expect("註冊丟棄事件指標失敗");
        registry
            .register(Box::new(events_stale.clone()))
            .expect("註冊過期事件指標失敗");

        registry
            .register(Box::new(staleness_histogram.clone()))
            .expect("註冊陳舊度直方圖失敗");
        registry
            .register(Box::new(staleness_count.clone()))
            .expect("註冊陳舊事件指標失敗");

        registry
            .register(Box::new(snapshot_flips.clone()))
            .expect("註冊快照翻轉指標失敗");
        registry
            .register(Box::new(snapshot_version.clone()))
            .expect("註冊快照版本指標失敗");

        registry
            .register(Box::new(orders_submitted.clone()))
            .expect("註冊提交訂單指標失敗");
        registry
            .register(Box::new(orders_filled.clone()))
            .expect("註冊成交訂單指標失敗");
        registry
            .register(Box::new(orders_rejected.clone()))
            .expect("註冊拒絕訂單指標失敗");
        // 註冊引擎統計 Gauges
        registry
            .register(Box::new(engine_cycle_count.clone()))
            .expect("註冊 engine_cycle_count 失敗");
        registry
            .register(Box::new(engine_exec_events_processed.clone()))
            .expect("註冊 engine_exec_events_processed 失敗");
        registry
            .register(Box::new(engine_orders_submitted.clone()))
            .expect("註冊 engine_orders_submitted 失敗");
        registry
            .register(Box::new(engine_orders_ack.clone()))
            .expect("註冊 engine_orders_ack 失敗");
        registry
            .register(Box::new(engine_orders_filled.clone()))
            .expect("註冊 engine_orders_filled 失敗");
        registry
            .register(Box::new(engine_orders_rejected.clone()))
            .expect("註冊 engine_orders_rejected 失敗");
        registry
            .register(Box::new(engine_orders_canceled.clone()))
            .expect("註冊 engine_orders_canceled 失敗");
        registry
            .register(Box::new(reconcile_runs.clone()))
            .expect("註冊對帳次數指標失敗");
        registry
            .register(Box::new(reconcile_exchange_only_found.clone()))
            .expect("註冊對帳交換端獨有指標失敗");
        registry
            .register(Box::new(reconcile_cancel_sent.clone()))
            .expect("註冊對帳撤單指標失敗");
        registry
            .register(Box::new(reconcile_errors.clone()))
            .expect("註冊對帳錯誤指標失敗");

        // P3: 新增統計註冊
        let intents_dropped = IntCounter::new(
            "hft_intents_dropped_total",
            "因執行隊列滿載而丟棄的意圖總數",
        )
        .expect("創建 intents_dropped 計數器失敗");
        let snapshot_publish_failed = IntCounter::new(
            "hft_snapshot_publish_failed_total",
            "快照發佈失敗計數（理論上為 0，保留監控）",
        )
        .expect("創建 snapshot_publish_failed 計數器失敗");

        registry
            .register(Box::new(intents_dropped.clone()))
            .expect("註冊 intents_dropped 指標失敗");
        registry
            .register(Box::new(snapshot_publish_failed.clone()))
            .expect("註冊 snapshot_publish_failed 指標失敗");

        debug!("Prometheus 指標註冊完成");

        let now = now_micros();
        Self {
            registry,
            latency_ingestion,
            latency_aggregation,
            latency_strategy,
            latency_risk,
            latency_execution,
            latency_end_to_end,
            latency_order_ack,
            latency_order_fill,
            queue_utilization,
            events_processed,
            events_dropped,
            events_stale,
            staleness_histogram,
            staleness_count,
            snapshot_flips,
            snapshot_version,
            orders_submitted,
            orders_filled,
            orders_rejected,
            reconcile_runs,
            reconcile_exchange_only_found,
            reconcile_cancel_sent,
            reconcile_errors,
            intents_dropped,
            snapshot_publish_failed,
            engine_cycle_count,
            engine_exec_events_processed,
            engine_orders_submitted,
            engine_orders_ack,
            engine_orders_filled,
            engine_orders_rejected,
            engine_orders_canceled,
            last_activity_micros: AtomicU64::new(now),
            last_queue_utilization_ppm: AtomicU64::new(0),
        }
    }

    /// 記錄攝取階段延遲
    pub fn record_ingestion_latency(&self, latency_us: f64) {
        self.latency_ingestion.observe(latency_us);
        debug!("記錄攝取延遲: {:.2}μs", latency_us);
        self.note_activity();
    }

    /// 記錄聚合階段延遲
    pub fn record_aggregation_latency(&self, latency_us: f64) {
        self.latency_aggregation.observe(latency_us);
        debug!("記錄聚合延遲: {:.2}μs", latency_us);
        self.note_activity();
    }

    /// 記錄策略階段延遲
    pub fn record_strategy_latency(&self, latency_us: f64) {
        self.latency_strategy.observe(latency_us);
        debug!("記錄策略延遲: {:.2}μs", latency_us);
        self.note_activity();
    }

    /// 記錄風控階段延遲
    pub fn record_risk_latency(&self, latency_us: f64) {
        self.latency_risk.observe(latency_us);
        debug!("記錄風控延遲: {:.2}μs", latency_us);
        self.note_activity();
    }

    /// 記錄執行階段延遲
    pub fn record_execution_latency(&self, latency_us: f64) {
        self.latency_execution.observe(latency_us);
        debug!("記錄執行延遲: {:.2}μs", latency_us);
        self.note_activity();
    }

    /// 記錄下單→Ack 延遲
    pub fn record_order_ack_latency(&self, latency_us: f64) {
        self.latency_order_ack.observe(latency_us);
    }

    /// 記錄下單→Fill 延遲
    pub fn record_order_fill_latency(&self, latency_us: f64) {
        self.latency_order_fill.observe(latency_us);
    }

    /// 記錄端到端延遲
    pub fn record_end_to_end_latency(&self, latency_us: f64) {
        self.latency_end_to_end.observe(latency_us);
        debug!("記錄端到端延遲: {:.2}μs", latency_us);
        self.note_activity();
    }

    /// 更新隊列利用率
    pub fn update_queue_utilization(&self, ratio: f64) {
        self.queue_utilization.set(ratio);
        // ppm 儲存避免 f64 原子
        let ppm = (ratio.max(0.0).min(1.0) * 1_000_000.0) as u64;
        self.last_queue_utilization_ppm
            .store(ppm, Ordering::Relaxed);
        self.note_activity();
    }

    /// 增加處理事件計數
    pub fn inc_events_processed(&self) {
        self.events_processed.inc();
        self.note_activity();
    }

    /// 增加丟棄事件計數
    pub fn inc_events_dropped(&self) {
        self.events_dropped.inc();
    }

    /// 增加過期事件計數
    pub fn inc_events_stale(&self) {
        self.events_stale.inc();
    }

    /// 記錄數據陳舊度
    pub fn record_staleness(&self, staleness_ms: f64) {
        self.staleness_histogram.observe(staleness_ms);
        self.staleness_count.inc();
        self.note_activity();
    }

    /// 增加快照翻轉計數
    pub fn inc_snapshot_flips(&self) {
        self.snapshot_flips.inc();
        self.note_activity();
    }

    /// 更新快照版本號
    pub fn update_snapshot_version(&self, version: u64) {
        self.snapshot_version.set(version as f64);
        self.note_activity();
    }

    /// 增加提交訂單計數
    pub fn inc_orders_submitted(&self) {
        self.orders_submitted.inc();
        self.note_activity();
    }

    /// 增加成交訂單計數
    pub fn inc_orders_filled(&self) {
        self.orders_filled.inc();
        self.note_activity();
    }

    // 對帳相關
    pub fn inc_reconcile_runs(&self) {
        self.reconcile_runs.inc();
    }
    pub fn add_reconcile_exchange_only_found(&self, n: u64) {
        for _ in 0..n {
            self.reconcile_exchange_only_found.inc();
        }
    }
    pub fn add_reconcile_cancel_sent(&self, n: u64) {
        for _ in 0..n {
            self.reconcile_cancel_sent.inc();
        }
    }
    pub fn inc_reconcile_errors(&self) {
        self.reconcile_errors.inc();
    }

    /// 增加拒絕訂單計數
    pub fn inc_orders_rejected(&self) {
        self.orders_rejected.inc();
        self.note_activity();
    }

    // P3: 新增方法
    pub fn add_intents_dropped(&self, n: u64) {
        for _ in 0..n {
            self.intents_dropped.inc();
        }
    }
    pub fn inc_snapshot_publish_failed(&self) {
        self.snapshot_publish_failed.inc();
    }

    /// 獲取 Prometheus 註冊表（用於 HTTP 暴露）
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// 用於引擎將快照統計同步到 Prometheus（以 Gauge 型式）
    pub fn update_engine_statistics(&self, s: &EngineStatisticsExport) {
        self.engine_cycle_count.set(s.cycle_count as f64);
        self.engine_exec_events_processed
            .set(s.execution_events_processed as f64);
        self.engine_orders_submitted
            .set(s.orders_submitted as f64);
        self.engine_orders_ack.set(s.orders_ack as f64);
        self.engine_orders_filled.set(s.orders_filled as f64);
        self.engine_orders_rejected
            .set(s.orders_rejected as f64);
        self.engine_orders_canceled
            .set(s.orders_canceled as f64);
        self.note_activity();
    }

    /// 从 LatencyMonitor 批量更新指标
    pub fn update_from_latency_monitor(
        &self,
        latency_stats: &std::collections::HashMap<
            hft_core::latency::LatencyStage,
            hft_core::latency::LatencyStageStats,
        >,
    ) {
        use hft_core::latency::LatencyStage;

        for (stage, stats) in latency_stats {
            match stage {
                LatencyStage::Ingestion => {
                    // 更新所有样本到直方图
                    for _ in 0..stats.count {
                        self.latency_ingestion.observe(stats.mean_micros);
                    }
                }
                LatencyStage::Aggregation => {
                    for _ in 0..stats.count {
                        self.latency_aggregation.observe(stats.mean_micros);
                    }
                }
                LatencyStage::Strategy => {
                    for _ in 0..stats.count {
                        self.latency_strategy.observe(stats.mean_micros);
                    }
                }
                LatencyStage::Execution => {
                    for _ in 0..stats.count {
                        self.latency_execution.observe(stats.mean_micros);
                    }
                }
                LatencyStage::EndToEnd => {
                    for _ in 0..stats.count {
                        self.latency_end_to_end.observe(stats.mean_micros);
                    }
                }
            }
        }
        self.note_activity();
    }
}

impl MetricsRegistry {
    /// 更新最後活動時間（用於 readiness）
    fn note_activity(&self) {
        self.last_activity_micros
            .store(now_micros(), Ordering::Relaxed);
    }

    /// 取得最近的隊列利用率（0.0-1.0）
    pub fn queue_utilization_value(&self) -> f64 {
        self.last_queue_utilization_ppm.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }

    /// 就緒評估（簡易版）：
    /// - 最近活動間隔小於 max_idle_secs
    /// - 隊列利用率低於 max_utilization
    pub fn assess_readiness(
        &self,
        max_utilization: f64,
        max_idle_secs: u64,
    ) -> (bool, serde_json::Value) {
        let now = now_micros();
        let last = self.last_activity_micros.load(Ordering::Relaxed);
        let idle_secs = (now.saturating_sub(last)) as f64 / 1_000_000.0;
        let util = self.queue_utilization_value();
        let ready = idle_secs <= max_idle_secs as f64 && util <= max_utilization;
        (
            ready,
            serde_json::json!({
                "idle_secs": idle_secs,
                "queue_utilization": util,
                "max_idle_secs": max_idle_secs,
                "max_utilization": max_utilization,
            }),
        )
    }
}

/// 便利宏：記錄分段延遲
#[inline]
pub fn now_micros() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[macro_export]
macro_rules! record_latency {
    (ingestion, $start_us:expr) => {{
        let latency_us = ($crate::now_micros() - $start_us) as f64;
        $crate::MetricsRegistry::global().record_ingestion_latency(latency_us);
    }};
    (aggregation, $start_us:expr) => {{
        let latency_us = ($crate::now_micros() - $start_us) as f64;
        $crate::MetricsRegistry::global().record_aggregation_latency(latency_us);
    }};
    (strategy, $start_us:expr) => {{
        let latency_us = ($crate::now_micros() - $start_us) as f64;
        $crate::MetricsRegistry::global().record_strategy_latency(latency_us);
    }};
    (risk, $start_us:expr) => {{
        let latency_us = ($crate::now_micros() - $start_us) as f64;
        $crate::MetricsRegistry::global().record_risk_latency(latency_us);
    }};
    (execution, $start_us:expr) => {{
        let latency_us = ($crate::now_micros() - $start_us) as f64;
        $crate::MetricsRegistry::global().record_execution_latency(latency_us);
    }};
    (end_to_end, $start_us:expr) => {{
        let latency_us = ($crate::now_micros() - $start_us) as f64;
        $crate::MetricsRegistry::global().record_end_to_end_latency(latency_us);
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_registry_init() {
        let metrics = MetricsRegistry::global();

        // 測試基本指標記錄（無崩潰即可）
        metrics.record_ingestion_latency(10.0);
        metrics.record_aggregation_latency(20.0);
        metrics.record_strategy_latency(15.0);
        metrics.update_queue_utilization(0.75);
        metrics.inc_events_processed();
        metrics.inc_snapshot_flips();

        // 多次呼叫應該不會崩潰
        metrics.inc_events_processed();
        metrics.inc_events_processed();
    }

    #[test]
    fn test_record_latency_macro() {
        let start = now_micros();

        // 模擬少量延遲
        std::thread::sleep(std::time::Duration::from_micros(100));

        record_latency!(ingestion, start);
        record_latency!(aggregation, start);
        record_latency!(strategy, start);
    }
}
