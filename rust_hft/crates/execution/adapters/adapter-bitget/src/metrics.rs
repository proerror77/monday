//! Bitget Adapter 指標定義
//! 
//! 用於監控執行適配器的各類錯誤和性能指標

use once_cell::sync::Lazy;
use prometheus::{Counter, CounterVec, HistogramVec, register_counter_vec, register_histogram_vec};

/// 成交回報解析失敗計數
pub static FILL_PARSE_ERRORS: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "bitget_adapter_fill_parse_errors_total",
        "Number of fill parsing errors",
        &["error_type"]
    ).unwrap()
});

/// 事件發送失敗計數
pub static EVENT_SEND_ERRORS: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "bitget_adapter_event_send_errors_total", 
        "Number of event send failures",
        &["event_type"]
    ).unwrap()
});

/// 幂等去重計數 (重複 fill_id)
pub static FILL_DEDUPLICATION: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "bitget_adapter_fill_dedup_total",
        "Number of deduplicated fill events", 
        &["action"] // "duplicate_detected", "cache_hit", "cache_miss"
    ).unwrap()
});

/// 端到端延遲直方圖
pub static END_TO_END_LATENCY: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "bitget_adapter_latency_microseconds",
        "End-to-end latency from exchange to adapter processing",
        &["stage"], // "parse", "send", "total"
        prometheus::exponential_buckets(1.0, 2.0, 20).unwrap() // 1µs 到 ~1ms
    ).unwrap()
});

/// 時間戳轉換延遲
pub static TIMESTAMP_CONVERSION_LATENCY: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "bitget_adapter_timestamp_conversion_microseconds",
        "Time taken to convert and validate timestamps",
        &["source"], // "exchange_ts", "local_ts", "unified_ts"
        prometheus::exponential_buckets(0.1, 2.0, 15).unwrap() // 0.1µs 到 ~3ms
    ).unwrap()
});

/// WebSocket 消息處理速率
pub static WS_MESSAGE_PROCESSING_RATE: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "bitget_adapter_ws_messages_total",
        "Total WebSocket messages processed",
        &["channel", "status"] // channel: "orders"/"fill", status: "success"/"error"
    ).unwrap()
});

/// 背壓事件計數
pub static BACKPRESSURE_EVENTS: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "bitget_adapter_backpressure_total",
        "Number of backpressure events",
        &["action"] // "queue_full", "event_dropped", "degraded_mode_entered"
    ).unwrap()
});