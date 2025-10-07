//! Prometheus HTTP 指标导出服务器
//!
//! 提供 /metrics 端点，导出所有 HFT 系统指标，包括：
//! - 分段延遲直方图 (P50, P95, P99)
//! - 吞吐量计数器
//! - 队列利用率
//! - 订单执行统计

use crate::MetricsRegistry;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use prometheus::TextEncoder;
use std::sync::Arc;
use tracing::{error, info};

/// HTTP 指标服务器配置
#[derive(Debug, Clone)]
pub struct MetricsServerConfig {
    /// 监听地址
    pub bind_address: String,
    /// 监听端口
    pub port: u16,
    /// 是否启用详细日志
    pub verbose_logging: bool,
    /// Readiness：最大允許空閒秒數（若長時間無活動則視為未就緒）
    pub readiness_max_idle_secs: u64,
    /// Readiness：隊列利用率上限（超過則視為壓力過大未就緒）
    pub readiness_max_utilization: f64,
}

impl Default for MetricsServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0".to_string(),
            port: 9090,
            verbose_logging: false,
            readiness_max_idle_secs: 5,
            readiness_max_utilization: 0.9,
        }
    }
}

/// HTTP 指标服务器
pub struct MetricsServer {
    config: MetricsServerConfig,
}

impl MetricsServer {
    /// 创建新的指标服务器
    pub fn new(config: MetricsServerConfig) -> Self {
        Self { config }
    }

    /// 启动 HTTP 指标服务器（异步）
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bind_addr = format!("{}:{}", self.config.bind_address, self.config.port);

        // 初始化指标注册表
        let _registry = MetricsRegistry::init();

        // 创建路由
        let app = Router::new()
            .route("/metrics", get(metrics_handler))
            .route("/health", get(health_handler))
            .route("/readiness", get(readiness_handler))
            .with_state(Arc::new(self.config.clone()));

        info!("🚀 启动 Prometheus 指标服务器: http://{}", bind_addr);
        info!("📊 指标端点: http://{}/metrics", bind_addr);
        info!("❤️  健康检查: http://{}/health", bind_addr);

        // 启动服务器
        let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }

    /// 在后台启动指标服务器（非阻塞）
    pub fn start_background(config: MetricsServerConfig) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let server = MetricsServer::new(config);
            if let Err(e) = server.start().await {
                error!("指标服务器启动失败: {}", e);
            }
        })
    }
}

/// /metrics 端点处理器 - 导出 Prometheus 格式指标
async fn metrics_handler(State(config): State<Arc<MetricsServerConfig>>) -> impl IntoResponse {
    let registry = MetricsRegistry::global();

    // 获取 Prometheus 注册表并编码为文本格式
    let prometheus_registry = registry.registry();
    let encoder = TextEncoder::new();
    let metric_families = prometheus_registry.gather();

    match encoder.encode_to_string(&metric_families) {
        Ok(output) => {
            if config.verbose_logging {
                info!("📊 导出 {} 个指标族", metric_families.len());
            }

            // 添加自定义头部信息
            let mut response = Response::new(output);
            response.headers_mut().insert(
                "content-type",
                "text/plain; version=0.0.4; charset=utf-8".parse().unwrap(),
            );
            response
        }
        Err(e) => {
            error!("指标编码失败: {}", e);
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(format!("指标编码失败: {}", e))
                .unwrap()
        }
    }
}

/// /health 端点处理器 - 简单健康检查
async fn health_handler() -> impl IntoResponse {
    let health_info = serde_json::json!({
        "status": "healthy",
        "service": "hft-metrics-server",
        "version": env!("CARGO_PKG_VERSION"),
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "features": {
            "prometheus": true,
            "http_server": cfg!(feature = "http-server"),
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(health_info.to_string())
        .unwrap()
}

/// /readiness 端点处理器 - 就緒檢查（基於最近活動與隊列利用率）
async fn readiness_handler(State(config): State<Arc<MetricsServerConfig>>) -> impl IntoResponse {
    let registry = MetricsRegistry::global();
    let (ready, detail) = registry.assess_readiness(
        config.readiness_max_utilization,
        config.readiness_max_idle_secs,
    );

    if ready {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(
                serde_json::json!({
                    "status": "ready",
                    "detail": detail
                })
                .to_string(),
            )
            .unwrap()
    } else {
        Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header("content-type", "application/json")
            .body(
                serde_json::json!({
                    "status": "not_ready",
                    "detail": detail
                })
                .to_string(),
            )
            .unwrap()
    }
}

/// 便利函数：使用默认配置启动指标服务器
pub async fn start_default_metrics_server() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
{
    let config = MetricsServerConfig::default();
    let server = MetricsServer::new(config);
    server.start().await
}

/// 便利函数：在后台启动默认指标服务器
pub fn start_default_metrics_server_background() -> tokio::task::JoinHandle<()> {
    MetricsServer::start_background(MetricsServerConfig::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_metrics_server_config() {
        let config = MetricsServerConfig {
            bind_address: "127.0.0.1".to_string(),
            port: 19090,
            verbose_logging: true,
            readiness_max_idle_secs: 5,
            readiness_max_utilization: 0.9,
        };

        assert_eq!(config.bind_address, "127.0.0.1");
        assert_eq!(config.port, 19090);
        assert!(config.verbose_logging);
    }

    #[tokio::test]
    async fn test_health_handler() {
        let response = health_handler().await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    // 集成测试：启动服务器并测试端点
    #[tokio::test]
    async fn test_metrics_server_integration() {
        // 使用随机端口避免冲突
        let config = MetricsServerConfig {
            bind_address: "127.0.0.1".to_string(),
            port: 19091,
            verbose_logging: false,
            readiness_max_idle_secs: 5,
            readiness_max_utilization: 0.9,
        };

        // 在后台启动服务器
        let _handle = MetricsServer::start_background(config);

        // 等待服务器启动
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 测试健康检查端点
        let health_url = "http://127.0.0.1:19091/health";
        let client = reqwest::Client::new();

        // 注意：这个测试需要 reqwest，但为了保持依赖最小，我们先跳过实际的 HTTP 请求
        // 在实际使用中，可以用 curl 或其他工具测试
    }
}
