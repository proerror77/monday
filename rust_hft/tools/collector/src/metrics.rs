use anyhow::Result;
use hyper::{
    service::{make_service_fn, service_fn},
    Body, Request, Response, Server,
};
use once_cell::sync::Lazy;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Registry, TextEncoder,
};
use std::convert::Infallible;

pub static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);
#[cfg(feature = "use-adapters")]
pub static EVENTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let c = IntCounterVec::new(
        prometheus::Opts::new("collector_events_total", "Total events by type"),
        &["exchange", "type"],
    )
    .unwrap();
    REGISTRY.register(Box::new(c.clone())).ok();
    c
});
#[cfg(feature = "use-adapters")]
pub static ERRORS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let c = IntCounterVec::new(
        prometheus::Opts::new("collector_errors_total", "Total errors"),
        &["exchange"],
    )
    .unwrap();
    REGISTRY.register(Box::new(c.clone())).ok();
    c
});

/// 每个交易所的重连次数
pub static RECONNECTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let c = IntCounterVec::new(
        prometheus::Opts::new(
            "collector_reconnects_total",
            "Reconnect attempts by exchange",
        ),
        &["exchange"],
    )
    .unwrap();
    REGISTRY.register(Box::new(c.clone())).ok();
    c
});

/// 分类别错误计数（connect_failed/ws_error/process_error/flush_error/raw_flush_error/heartbeat_error）
pub static ERRORS_BY_KIND_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let c = IntCounterVec::new(
        prometheus::Opts::new("collector_error_kinds_total", "Errors by kind and exchange"),
        &["exchange", "kind"],
    )
    .unwrap();
    REGISTRY.register(Box::new(c.clone())).ok();
    c
});

/// 最近错误类别的时间戳（按类别记录最近发生时间，秒）
pub static LAST_ERROR_KIND: Lazy<IntGaugeVec> = Lazy::new(|| {
    let g = IntGaugeVec::new(
        prometheus::Opts::new(
            "collector_last_error_kind_seconds",
            "Unix seconds of last error occurrence by kind",
        ),
        &["exchange", "kind"],
    )
    .unwrap();
    REGISTRY.register(Box::new(g.clone())).ok();
    g
});

/// 批量落库耗时（秒），按表区分 unified/raw_ws
pub static BATCH_INSERT_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    let h = HistogramVec::new(
        HistogramOpts::new(
            "collector_batch_insert_seconds",
            "Batch insert duration in seconds by table",
        )
        // 常见批量落库应在 < 2s，设置几何 buckets
        .buckets(vec![0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0]),
        &["exchange", "table"],
    )
    .unwrap();
    REGISTRY.register(Box::new(h.clone())).ok();
    h
});

/// 插入失败的行数（按表统计）
pub static FAILED_INSERT_ROWS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    let c = IntCounterVec::new(
        prometheus::Opts::new(
            "collector_failed_insert_rows_total",
            "Failed rows to insert to table",
        ),
        &["table"],
    )
    .unwrap();
    REGISTRY.register(Box::new(c.clone())).ok();
    c
});

async fn metrics_handler(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    let path = req.uri().path();
    match path {
        "/health" | "/ready" => Ok(Response::new(Body::from("ok"))),
        "/metrics" | _ => {
            let encoder = TextEncoder::new();
            let metric_families = REGISTRY.gather();
            let mut buf = Vec::new();
            encoder.encode(&metric_families, &mut buf).ok();
            Ok(Response::builder()
                .status(200)
                .header("Content-Type", encoder.format_type())
                .body(Body::from(buf))
                .unwrap())
        }
    }
}

pub async fn start_metrics_server(addr: String) -> Result<()> {
    let make_svc =
        make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(metrics_handler)) });
    let server = Server::bind(&addr.parse().unwrap()).serve(make_svc);
    tokio::spawn(async move {
        if let Err(e) = server.await {
            eprintln!("metrics server error: {}", e);
        }
    });
    Ok(())
}
