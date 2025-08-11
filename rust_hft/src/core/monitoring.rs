//! 生產級監控和可觀測性系統
//! 
//! 實現Prometheus指標導出、health checks和服務發現

use metrics::{describe_counter, describe_gauge, describe_histogram, counter, gauge, histogram};
use metrics_exporter_prometheus::PrometheusBuilder;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error};
use std::collections::HashMap;

/// HFT系統核心指標定義
pub struct HftMetrics {
    initialized: bool,
}

impl HftMetrics {
    pub fn new() -> Self {
        // 註冊所有指標
        describe_counter!("hft_messages_processed_total", "處理的消息總數");
        describe_counter!("hft_orders_executed_total", "執行的訂單總數");
        describe_counter!("hft_data_written_total", "寫入的數據行數");
        describe_counter!("hft_errors_total", "錯誤總數");
        
        describe_gauge!("hft_websocket_connections", "WebSocket連接數");
        describe_gauge!("hft_queue_utilization_percent", "隊列使用率（%）");
        describe_gauge!("hft_spread_basis_points", "價差（基點）");
        describe_gauge!("hft_position_pnl_usd", "持倉盈虧（美元）");
        describe_gauge!("hft_risk_exposure_usd", "風險敞口（美元）");
        
        describe_histogram!("hft_orderbook_update_latency_microseconds", "訂單簿更新延遲（微秒）");
        describe_histogram!("hft_trade_execution_latency_microseconds", "交易執行延遲（微秒）");
        describe_histogram!("hft_end_to_end_latency_microseconds", "端到端延遲（微秒）");
        
        Self { initialized: true }
    }
    
    /// 記錄延遲指標
    pub fn record_latency(&self, metric_type: &str, latency_micros: f64) {
        match metric_type {
            "orderbook_update" => {
                histogram!("hft_orderbook_update_latency_microseconds").record(latency_micros);
            },
            "trade_execution" => {
                histogram!("hft_trade_execution_latency_microseconds").record(latency_micros);
            },
            "end_to_end" => {
                histogram!("hft_end_to_end_latency_microseconds").record(latency_micros);
            },
            _ => {}
        }
    }
    
    /// 增加計數器指標
    pub fn increment_counter(&self, metric_type: &str, value: u64) {
        match metric_type {
            "messages_processed" => {
                counter!("hft_messages_processed_total").increment(value);
            },
            "orders_executed" => {
                counter!("hft_orders_executed_total").increment(value);
            },
            "data_written" => {
                counter!("hft_data_written_total").increment(value);
            },
            "errors" => {
                counter!("hft_errors_total").increment(value);
            },
            _ => {}
        }
    }
    
    /// 更新業務指標
    pub fn update_business_metrics(&self, spread_bps: f64, pnl_usd: f64, exposure_usd: f64) {
        gauge!("hft_spread_basis_points").set(spread_bps);
        gauge!("hft_position_pnl_usd").set(pnl_usd);
        gauge!("hft_risk_exposure_usd").set(exposure_usd);
    }
    
    /// 更新系統指標
    pub fn update_system_metrics(&self, websocket_connections: u64, queue_utilization: f64) {
        gauge!("hft_websocket_connections").set(websocket_connections as f64);
        gauge!("hft_queue_utilization_percent").set(queue_utilization);
    }
}

impl Default for HftMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// 監控服務器
pub struct MonitoringServer {
    metrics: Arc<HftMetrics>,
    health_status: Arc<RwLock<HashMap<String, bool>>>,
    port: u16,
}

impl MonitoringServer {
    pub async fn new(port: u16) -> Result<Self, Box<dyn std::error::Error>> {
        // 初始化Prometheus導出器
        PrometheusBuilder::new()
            .with_http_listener(([0, 0, 0, 0], port))
            .install()
            .expect("Failed to install Prometheus recorder");
        
        info!("Prometheus監控服務啟動在端口 {}", port);
        
        let mut health_status = HashMap::new();
        health_status.insert("database".to_string(), false);
        health_status.insert("websocket".to_string(), false);
        health_status.insert("execution_engine".to_string(), false);
        
        Ok(Self {
            metrics: Arc::new(HftMetrics::new()),
            health_status: Arc::new(RwLock::new(health_status)),
            port,
        })
    }
    
    pub fn get_metrics(&self) -> Arc<HftMetrics> {
        self.metrics.clone()
    }
    
    /// 更新組件健康狀態
    pub async fn update_health(&self, component: &str, is_healthy: bool) {
        let mut status = self.health_status.write().await;
        status.insert(component.to_string(), is_healthy);
        
        if is_healthy {
            info!("組件 {} 狀態: 健康", component);
        } else {
            warn!("組件 {} 狀態: 不健康", component);
        }
    }
    
    /// 獲取整體系統健康狀態
    pub async fn get_overall_health(&self) -> bool {
        let status = self.health_status.read().await;
        status.values().all(|&healthy| healthy)
    }
    
    /// 記錄延遲指標
    pub fn record_latency(&self, metric_type: &str, latency_micros: f64) {
        self.metrics.record_latency(metric_type, latency_micros);
    }
    
    /// 增加計數器指標
    pub fn increment_counter(&self, metric_type: &str, value: u64) {
        self.metrics.increment_counter(metric_type, value);
    }
    
    /// 更新業務指標
    pub fn update_business_metrics(&self, spread_bps: f64, pnl_usd: f64, exposure_usd: f64) {
        self.metrics.update_business_metrics(spread_bps, pnl_usd, exposure_usd);
    }
    
    /// 啟動健康檢查HTTP服務
    pub async fn start_health_endpoint(&self) {
        use warp::Filter;
        
        let health_status = self.health_status.clone();
        let health_route = warp::path("health")
            .and(warp::get())
            .and_then(move || {
                let status = health_status.clone();
                async move {
                    let status_map = status.read().await;
                    let is_healthy = status_map.values().all(|&h| h);
                    
                    if is_healthy {
                        Ok::<_, warp::Rejection>(warp::reply::with_status("OK", warp::http::StatusCode::OK))
                    } else {
                        Ok::<_, warp::Rejection>(warp::reply::with_status("UNHEALTHY", warp::http::StatusCode::SERVICE_UNAVAILABLE))
                    }
                }
            });
        
        let readiness_route = warp::path("ready")
            .and(warp::get())
            .map(|| warp::reply::with_status("READY", warp::http::StatusCode::OK));
        
        let routes = health_route.or(readiness_route);
        
        info!("健康檢查HTTP服務啟動在端口 {}", self.port + 1);
        warp::serve(routes)
            .run(([0, 0, 0, 0], self.port + 1))
            .await;
    }
}

/// 性能告警規則
pub struct AlertRules {
    pub max_latency_micros: f64,
    pub max_error_rate: f64,
    pub min_throughput: f64,
}

impl Default for AlertRules {
    fn default() -> Self {
        Self {
            max_latency_micros: 25.0,  // 25微秒
            max_error_rate: 0.01,      // 1%錯誤率
            min_throughput: 1000.0,    // 1000 msg/sec
        }
    }
}

/// 告警檢查器
pub struct AlertChecker {
    rules: AlertRules,
    monitoring: Arc<MonitoringServer>,
}

impl AlertChecker {
    pub fn new(monitoring: Arc<MonitoringServer>, rules: AlertRules) -> Self {
        Self { rules, monitoring }
    }
    
    /// 檢查延遲告警
    pub async fn check_latency_alert(&self, current_p99_latency: f64) -> bool {
        if current_p99_latency > self.rules.max_latency_micros {
            error!("延遲告警: P99延遲 {:.2}μs 超過閾值 {:.2}μs", 
                   current_p99_latency, self.rules.max_latency_micros);
            
            self.monitoring.increment_counter("errors", 1);
            true
        } else {
            false
        }
    }
    
    /// 檢查系統健康並發送告警
    pub async fn run_health_checks(&self) -> Vec<String> {
        let mut alerts = Vec::new();
        
        // 檢查整體健康狀態
        if !self.monitoring.get_overall_health().await {
            alerts.push("系統組件不健康".to_string());
        }
        
        alerts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};
    
    #[tokio::test]
    async fn test_monitoring_server() {
        let server = MonitoringServer::new(19090).await.unwrap();
        let _metrics = server.get_metrics();
        
        // 測試指標記錄
        server.record_latency("orderbook_update", 15.0);
        server.increment_counter("messages_processed", 100);
        server.update_business_metrics(2.5, 1000.0, 5000.0);
        
        // 測試健康狀態更新
        server.update_health("database", true).await;
        server.update_health("websocket", true).await;
        server.update_health("execution_engine", false).await;
        
        assert!(!server.get_overall_health().await);
        
        server.update_health("execution_engine", true).await;
        assert!(server.get_overall_health().await);
    }
    
    #[tokio::test]
    async fn test_alert_checker() {
        let server = Arc::new(MonitoringServer::new(19091).await.unwrap());
        let rules = AlertRules::default();
        let checker = AlertChecker::new(server, rules);
        
        // 測試延遲告警
        let should_alert = checker.check_latency_alert(30.0).await;
        assert!(should_alert);
        
        let should_not_alert = checker.check_latency_alert(20.0).await;
        assert!(!should_not_alert);
    }
}