/*!
 * 執行服務 (Execution Service)
 *
 * 專注於訂單執行和執行質量監控，包括執行路由、執行追蹤和成交分析。
 * 從 CompleteOMS 中分離出專門的執行管理職責。
 */

use anyhow::Result;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::time::Duration;
use tracing::info;

use crate::app::routing::{ExchangeRouter, RoutingResult};
use crate::app::services::order_service::OrderRequest;
use crate::domains::trading::{AccountId, ExecutionReport, OrderStatus, Price, Timestamp};

/// 執行狀態
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExecutionStatus {
    /// 等待執行
    Pending,

    /// 正在執行
    Executing,

    /// 部分執行
    PartiallyExecuted,

    /// 完全執行
    FullyExecuted,

    /// 執行失敗
    Failed,

    /// 已取消
    Cancelled,

    /// 執行超時
    Timeout,
}

/// 執行記錄
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    /// 執行ID
    pub execution_id: String,

    /// 關聯的訂單ID
    pub order_id: String,

    /// 賬戶ID
    pub account_id: AccountId,

    /// 交易所
    pub exchange: String,

    /// 交易對
    pub symbol: String,

    /// 執行狀態
    pub status: ExecutionStatus,

    /// 路由結果
    #[serde(skip)]
    pub routing_result: Option<RoutingResult>,

    /// 開始執行時間
    pub start_time: Timestamp,

    /// 結束執行時間
    pub end_time: Option<Timestamp>,

    /// 執行延遲 (微秒)
    pub execution_latency_us: Option<u64>,

    /// 預期價格
    pub expected_price: Option<Price>,

    /// 實際平均成交價格
    pub actual_avg_price: Option<Price>,

    /// 滑點 (基點)
    pub slippage_bps: f64,

    /// 執行質量評分 (0.0 - 1.0)
    pub execution_quality_score: f64,

    /// 執行回報列表
    pub execution_reports: Vec<ExecutionReport>,

    /// 錯誤信息
    pub error_message: Option<String>,

    /// 執行元數據
    pub metadata: HashMap<String, String>,
}

/// 執行配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// 執行超時時間 (毫秒)
    pub execution_timeout_ms: u64,

    /// 最大可接受滑點 (基點)
    pub max_acceptable_slippage_bps: f64,

    /// 執行質量評分閾值
    pub min_execution_quality_score: f64,

    /// 是否啟用智能路由
    pub enable_smart_routing: bool,

    /// 是否啟用執行質量監控
    pub enable_quality_monitoring: bool,

    /// 重試次數
    pub max_retry_attempts: u32,

    /// 重試延遲 (毫秒)
    pub retry_delay_ms: u64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            execution_timeout_ms: 30000,       // 30秒
            max_acceptable_slippage_bps: 10.0, // 10個基點
            min_execution_quality_score: 0.8,  // 80%
            enable_smart_routing: true,
            enable_quality_monitoring: true,
            max_retry_attempts: 3,
            retry_delay_ms: 1000, // 1秒
        }
    }
}

/// 執行統計信息
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionStats {
    /// 總執行次數
    pub total_executions: u64,

    /// 成功執行次數
    pub successful_executions: u64,

    /// 失敗執行次數
    pub failed_executions: u64,

    /// 平均執行延遲 (微秒)
    pub avg_execution_latency_us: f64,

    /// 平均滑點 (基點)
    pub avg_slippage_bps: f64,

    /// 平均執行質量評分
    pub avg_execution_quality: f64,

    /// 最佳執行延遲 (微秒)
    pub best_execution_latency_us: u64,

    /// 最差執行延遲 (微秒)
    pub worst_execution_latency_us: u64,

    /// 執行成功率
    pub execution_success_rate: f64,

    /// 按交易所分組的統計
    pub exchange_stats: HashMap<String, ExchangeExecutionStats>,
}

/// 交易所執行統計
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExchangeExecutionStats {
    /// 執行次數
    pub execution_count: u64,

    /// 成功次數
    pub success_count: u64,

    /// 平均延遲 (微秒)
    pub avg_latency_us: f64,

    /// 平均滑點 (基點)
    pub avg_slippage_bps: f64,

    /// 平均質量評分
    pub avg_quality_score: f64,
}

/// 執行事件
#[derive(Debug, Clone)]
pub enum ExecutionEvent {
    /// 執行開始
    ExecutionStarted(ExecutionRecord),

    /// 執行更新
    ExecutionUpdated(ExecutionRecord),

    /// 執行完成
    ExecutionCompleted(ExecutionRecord),

    /// 執行失敗
    ExecutionFailed(ExecutionRecord, String),

    /// 執行回報接收
    ExecutionReportReceived(ExecutionReport),

    /// 執行質量告警
    QualityAlert(String, f64), // execution_id, quality_score
}

/// 執行服務 - 專注於訂單執行和執行質量監控
pub struct ExecutionService {
    /// 交易所路由器
    router: Arc<ExchangeRouter>,

    /// 執行記錄映射 - 使用 DashMap 提升執行追蹤性能
    executions: Arc<DashMap<String, ExecutionRecord>>,

    /// 執行事件發送器
    execution_events_sender: mpsc::UnboundedSender<ExecutionEvent>,

    /// 配置
    config: ExecutionConfig,

    /// 統計信息
    stats: Arc<RwLock<ExecutionStats>>,
}

impl ExecutionService {
    /// 創建新的執行服務
    pub fn new(router: Arc<ExchangeRouter>, config: ExecutionConfig) -> Self {
        let (execution_events_sender, _) = mpsc::unbounded_channel();

        Self {
            router,
            executions: Arc::new(DashMap::new()),
            execution_events_sender,
            config,
            stats: Arc::new(RwLock::new(ExecutionStats::default())),
        }
    }

    /// 創建執行服務並返回事件接收器
    pub fn new_with_receiver(
        router: Arc<ExchangeRouter>,
        config: ExecutionConfig,
    ) -> (Self, mpsc::UnboundedReceiver<ExecutionEvent>) {
        let (execution_events_sender, execution_events_receiver) = mpsc::unbounded_channel();

        let service = Self {
            router,
            executions: Arc::new(DashMap::new()),
            execution_events_sender,
            config,
            stats: Arc::new(RwLock::new(ExecutionStats::default())),
        };

        (service, execution_events_receiver)
    }

    /// 執行訂單
    pub async fn execute_order(&self, order_id: String, request: OrderRequest) -> Result<String> {
        let execution_id = self.generate_execution_id().await;

        // 1. 使用智能路由選擇最佳交易所
        let routing_result = if self.config.enable_smart_routing {
            Some(
                self.router
                    .route_for_account(&request.account_id, &request.symbol)
                    .await
                    .map_err(|e| anyhow::anyhow!("Routing failed: {}", e))?,
            )
        } else {
            None
        };

        // 2. 創建執行記錄
        let execution_record = ExecutionRecord {
            execution_id: execution_id.clone(),
            order_id: order_id.clone(),
            account_id: request.account_id.clone(),
            exchange: routing_result
                .as_ref()
                .map(|r| r.instance.config.name.clone())
                .unwrap_or_else(|| request.account_id.exchange.clone()),
            symbol: request.symbol.clone(),
            status: ExecutionStatus::Pending,
            routing_result: routing_result.clone(),
            start_time: crate::core::types::now_micros(),
            end_time: None,
            execution_latency_us: None,
            expected_price: request.price,
            actual_avg_price: None,
            slippage_bps: 0.0,
            execution_quality_score: 0.0,
            execution_reports: Vec::new(),
            error_message: None,
            metadata: HashMap::new(),
        };

        // 3. 保存執行記錄 - DashMap 無鎖插入，提升性能
        self.executions.insert(execution_id.clone(), execution_record.clone());

        // 4. 發送執行開始事件
        let _ = self
            .execution_events_sender
            .send(ExecutionEvent::ExecutionStarted(execution_record));

        // 5. 啟動執行監控任務
        self.start_execution_monitoring(execution_id.clone()).await;

        // 6. 更新統計
        self.stats.write().await.total_executions += 1;

        info!(
            "Execution started: {} for order {} on {}",
            execution_id,
            order_id,
            routing_result
                .as_ref()
                .map(|r| r.instance.config.name.as_str())
                .unwrap_or("unknown")
        );

        Ok(execution_id)
    }

    /// 處理執行回報
    pub async fn handle_execution_report(&self, report: ExecutionReport) -> Result<()> {
        // 1. 找到對應的執行記錄
        let execution_id = self
            .find_execution_by_order_id(&report.order_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Execution not found for order: {}", report.order_id))?;

        let mut execution_entry = self.executions.get_mut(&execution_id)
            .ok_or_else(|| anyhow::anyhow!("Execution record not found: {}", execution_id))?;
        let execution = execution_entry.value_mut();

        // 2. 更新執行記錄
        execution.execution_reports.push(report.clone());

        // 3. 根據執行回報更新執行狀態
        match report.status {
            OrderStatus::New | OrderStatus::Pending => {
                execution.status = ExecutionStatus::Executing;
            }
            OrderStatus::PartiallyFilled => {
                execution.status = ExecutionStatus::PartiallyExecuted;
                if let Some(avg_price) = report.average_price {
                    execution.actual_avg_price = Some(avg_price);
                }
            }
            OrderStatus::Filled => {
                execution.status = ExecutionStatus::FullyExecuted;
                execution.end_time = Some(crate::core::types::now_micros());
                if let Some(avg_price) = report.average_price {
                    execution.actual_avg_price = Some(avg_price);
                }

                // 計算執行指標
                self.calculate_execution_metrics(execution).await;

                // 更新統計
                self.update_execution_stats(execution).await;
            }
            OrderStatus::Cancelled => {
                execution.status = ExecutionStatus::Cancelled;
                execution.end_time = Some(crate::core::types::now_micros());
            }
            OrderStatus::Rejected => {
                execution.status = ExecutionStatus::Failed;
                execution.end_time = Some(crate::core::types::now_micros());
                execution.error_message = report.reject_reason.clone();

                // 更新失敗統計
                self.stats.write().await.failed_executions += 1;
            }
            OrderStatus::Expired => {
                execution.status = ExecutionStatus::Timeout;
                execution.end_time = Some(crate::core::types::now_micros());
            }
        }

        // 4. 發送執行事件
        let event = match execution.status {
            ExecutionStatus::FullyExecuted => ExecutionEvent::ExecutionCompleted(execution.clone()),
            ExecutionStatus::Failed => ExecutionEvent::ExecutionFailed(
                execution.clone(),
                execution.error_message.clone().unwrap_or_default(),
            ),
            _ => ExecutionEvent::ExecutionUpdated(execution.clone()),
        };

        let _ = self.execution_events_sender.send(event);
        let _ = self
            .execution_events_sender
            .send(ExecutionEvent::ExecutionReportReceived(report));

        Ok(())
    }

    /// 取消執行
    pub async fn cancel_execution(&self, execution_id: &str) -> Result<bool> {
        let mut execution_entry = self.executions.get_mut(execution_id)
            .ok_or_else(|| anyhow::anyhow!("Execution not found: {}", execution_id))?;
        let execution = execution_entry.value_mut();

        // 檢查是否可以取消
        if !matches!(
            execution.status,
            ExecutionStatus::Pending
                | ExecutionStatus::Executing
                | ExecutionStatus::PartiallyExecuted
        ) {
            return Err(anyhow::anyhow!(
                "Execution cannot be cancelled, current status: {:?}",
                execution.status
            ));
        }

        // 更新執行狀態
        execution.status = ExecutionStatus::Cancelled;
        execution.end_time = Some(crate::core::types::now_micros());

        // 發送取消事件
        let _ = self
            .execution_events_sender
            .send(ExecutionEvent::ExecutionUpdated(execution.clone()));

        info!("Execution cancelled: {}", execution_id);

        Ok(true)
    }

    /// 獲取執行記錄
    pub async fn get_execution(&self, execution_id: &str) -> Option<ExecutionRecord> {
        // DashMap 無鎖讀取，性能大幅提升
        self.executions.get(execution_id).map(|entry| entry.value().clone())
    }

    /// 獲取賬戶的執行記錄
    pub async fn get_executions_for_account(&self, account_id: &AccountId) -> Vec<ExecutionRecord> {
        // DashMap 並行迭代，無鎖查詢特定賬戶執行記錄
        self.executions
            .iter()
            .filter(|entry| &entry.value().account_id == account_id)
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// 獲取執行統計信息
    pub async fn get_stats(&self) -> ExecutionStats {
        self.stats.read().await.clone()
    }

    /// 獲取執行質量報告
    pub async fn get_execution_quality_report(
        &self,
        account_id: Option<&AccountId>,
        time_range_hours: u64,
    ) -> ExecutionQualityReport {
        let now = crate::core::types::now_micros();
        let time_threshold = now - (time_range_hours * 3600 * 1_000_000); // 轉換為微秒

        // DashMap 並行迭代，無鎖高性能篩選
        let filtered_executions: Vec<_> = self.executions
            .iter()
            .filter(|entry| entry.value().start_time >= time_threshold)
            .filter(|entry| account_id.map_or(true, |acc| &entry.value().account_id == acc))
            .filter(|entry| entry.value().status == ExecutionStatus::FullyExecuted)
            .map(|entry| entry.value().clone())
            .collect();

        if filtered_executions.is_empty() {
            return ExecutionQualityReport::default();
        }

        let total_count = filtered_executions.len() as f64;
        let avg_latency = filtered_executions
            .iter()
            .filter_map(|exec| exec.execution_latency_us)
            .map(|lat| lat as f64)
            .sum::<f64>()
            / total_count;

        let avg_slippage = filtered_executions
            .iter()
            .map(|exec| exec.slippage_bps)
            .sum::<f64>()
            / total_count;

        let avg_quality = filtered_executions
            .iter()
            .map(|exec| exec.execution_quality_score)
            .sum::<f64>()
            / total_count;

        ExecutionQualityReport {
            time_range_hours,
            total_executions: filtered_executions.len() as u64,
            avg_execution_latency_us: avg_latency,
            avg_slippage_bps: avg_slippage,
            avg_quality_score: avg_quality,
            quality_distribution: self.calculate_quality_distribution(&filtered_executions),
        }
    }

    // ====================== 內部輔助方法 ======================

    /// 生成執行ID
    async fn generate_execution_id(&self) -> String {
        let timestamp = crate::core::types::now_micros();
        let execution_count = self.stats.read().await.total_executions;
        format!("EXEC_{}_{}", timestamp, execution_count)
    }

    /// 根據訂單ID查找執行ID
    async fn find_execution_by_order_id(&self, order_id: &str) -> Option<String> {
        // DashMap 並行查找，性能優於單鎖HashMap
        for entry in self.executions.iter() {
            if entry.value().order_id == order_id {
                return Some(entry.key().clone());
            }
        }
        None
    }

    /// 啟動執行監控
    async fn start_execution_monitoring(&self, execution_id: String) {
        let executions = self.executions.clone();
        let timeout_ms = self.config.execution_timeout_ms;
        let events_sender = self.execution_events_sender.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;

            // DashMap 無鎖可變訪問，避免監控超時時的鎖競爭
            if let Some(mut execution_entry) = executions.get_mut(&execution_id) {
                let execution = execution_entry.value_mut();
                if matches!(
                    execution.status,
                    ExecutionStatus::Pending | ExecutionStatus::Executing
                ) {
                    execution.status = ExecutionStatus::Timeout;
                    execution.end_time = Some(crate::core::types::now_micros());

                    let _ = events_sender.send(ExecutionEvent::ExecutionFailed(
                        execution.clone(),
                        "Execution timeout".to_string(),
                    ));
                }
            }
        });
    }

    /// 計算執行指標
    async fn calculate_execution_metrics(&self, execution: &mut ExecutionRecord) {
        // 計算執行延遲
        if let Some(end_time) = execution.end_time {
            execution.execution_latency_us = Some(end_time - execution.start_time);
        }

        // 計算滑點
        if let (Some(expected), Some(actual)) =
            (execution.expected_price, execution.actual_avg_price)
        {
            let slippage = ((actual.0 - expected.0) / expected.0).abs() * 10000.0; // 轉換為基點
            execution.slippage_bps = slippage;
        }

        // 計算執行質量評分
        execution.execution_quality_score = self.calculate_quality_score(execution).await;

        // 質量告警
        if self.config.enable_quality_monitoring
            && execution.execution_quality_score < self.config.min_execution_quality_score
        {
            let _ = self
                .execution_events_sender
                .send(ExecutionEvent::QualityAlert(
                    execution.execution_id.clone(),
                    execution.execution_quality_score,
                ));
        }
    }

    /// 計算執行質量評分
    async fn calculate_quality_score(&self, execution: &ExecutionRecord) -> f64 {
        let mut score: f64 = 1.0;

        // 延遲因子 (延遲越低分數越高)
        if let Some(latency_us) = execution.execution_latency_us {
            let latency_factor = if latency_us < 1000 {
                // < 1ms
                1.0
            } else if latency_us < 10000 {
                // < 10ms
                0.9
            } else if latency_us < 100000 {
                // < 100ms
                0.8
            } else {
                0.5
            };
            score *= latency_factor;
        }

        // 滑點因子 (滑點越低分數越高)
        let slippage_factor = if execution.slippage_bps < 1.0 {
            1.0
        } else if execution.slippage_bps < 5.0 {
            0.9
        } else if execution.slippage_bps < 10.0 {
            0.8
        } else {
            0.5
        };
        score *= slippage_factor;

        score.max(0.0).min(1.0)
    }

    /// 更新執行統計
    async fn update_execution_stats(&self, execution: &ExecutionRecord) {
        let mut stats = self.stats.write().await;

        stats.successful_executions += 1;

        if let Some(latency) = execution.execution_latency_us {
            let prev_total =
                stats.avg_execution_latency_us * (stats.successful_executions - 1) as f64;
            stats.avg_execution_latency_us =
                (prev_total + latency as f64) / stats.successful_executions as f64;

            stats.best_execution_latency_us = stats.best_execution_latency_us.min(latency);
            stats.worst_execution_latency_us = stats.worst_execution_latency_us.max(latency);
        }

        // 更新滑點統計
        let prev_slippage_total = stats.avg_slippage_bps * (stats.successful_executions - 1) as f64;
        stats.avg_slippage_bps =
            (prev_slippage_total + execution.slippage_bps) / stats.successful_executions as f64;

        // 更新質量統計
        let prev_quality_total =
            stats.avg_execution_quality * (stats.successful_executions - 1) as f64;
        stats.avg_execution_quality = (prev_quality_total + execution.execution_quality_score)
            / stats.successful_executions as f64;

        // 更新成功率
        stats.execution_success_rate =
            (stats.successful_executions as f64 / stats.total_executions as f64) * 100.0;

        // 更新交易所統計
        let exchange_stats = stats
            .exchange_stats
            .entry(execution.exchange.clone())
            .or_default();
        exchange_stats.execution_count += 1;
        exchange_stats.success_count += 1;

        if let Some(latency) = execution.execution_latency_us {
            let prev_total =
                exchange_stats.avg_latency_us * (exchange_stats.success_count - 1) as f64;
            exchange_stats.avg_latency_us =
                (prev_total + latency as f64) / exchange_stats.success_count as f64;
        }

        let prev_slippage =
            exchange_stats.avg_slippage_bps * (exchange_stats.success_count - 1) as f64;
        exchange_stats.avg_slippage_bps =
            (prev_slippage + execution.slippage_bps) / exchange_stats.success_count as f64;

        let prev_quality =
            exchange_stats.avg_quality_score * (exchange_stats.success_count - 1) as f64;
        exchange_stats.avg_quality_score = (prev_quality + execution.execution_quality_score)
            / exchange_stats.success_count as f64;
    }

    /// 計算質量分佈
    fn calculate_quality_distribution(
        &self,
        executions: &[ExecutionRecord],
    ) -> HashMap<String, u64> {
        let mut distribution = HashMap::new();

        for execution in executions {
            let range = if execution.execution_quality_score >= 0.9 {
                "excellent"
            } else if execution.execution_quality_score >= 0.8 {
                "good"
            } else if execution.execution_quality_score >= 0.7 {
                "fair"
            } else {
                "poor"
            };

            *distribution.entry(range.to_string()).or_insert(0) += 1;
        }

        distribution
    }
}

/// 執行質量報告
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionQualityReport {
    /// 時間範圍 (小時)
    pub time_range_hours: u64,

    /// 總執行次數
    pub total_executions: u64,

    /// 平均執行延遲 (微秒)
    pub avg_execution_latency_us: f64,

    /// 平均滑點 (基點)
    pub avg_slippage_bps: f64,

    /// 平均質量評分
    pub avg_quality_score: f64,

    /// 質量分佈
    pub quality_distribution: HashMap<String, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::routing::{ExchangeRouter, RouterConfig};
    use crate::domains::trading::{OrderSide, OrderType, TimeInForce};
    use crate::exchanges::ExchangeManager;

    #[tokio::test]
    async fn test_execution_service_creation() {
        let exchange_manager = Arc::new(ExchangeManager::new());
        let router = Arc::new(ExchangeRouter::with_default_config(exchange_manager));
        let config = ExecutionConfig::default();

        let execution_service = ExecutionService::new(router, config);
        let stats = execution_service.get_stats().await;

        assert_eq!(stats.total_executions, 0);
        assert_eq!(stats.successful_executions, 0);
    }

    #[tokio::test]
    async fn test_execution_id_generation() {
        let exchange_manager = Arc::new(ExchangeManager::new());
        let router = Arc::new(ExchangeRouter::with_default_config(exchange_manager));
        let config = ExecutionConfig::default();
        let execution_service = ExecutionService::new(router, config);

        let execution_id = execution_service.generate_execution_id().await;
        assert!(execution_id.starts_with("EXEC_"));
    }

    #[tokio::test]
    async fn test_quality_score_calculation() {
        let exchange_manager = Arc::new(ExchangeManager::new());
        let router = Arc::new(ExchangeRouter::with_default_config(exchange_manager));
        let config = ExecutionConfig::default();
        let execution_service = ExecutionService::new(router, config);

        let mut execution = ExecutionRecord {
            execution_id: "test_exec".to_string(),
            order_id: "test_order".to_string(),
            account_id: AccountId::new("bitget", "main"),
            exchange: "bitget".to_string(),
            symbol: "BTCUSDT".to_string(),
            status: ExecutionStatus::FullyExecuted,
            routing_result: None,
            start_time: 0,
            end_time: Some(1000), // 1ms execution
            execution_latency_us: Some(1000),
            expected_price: Some(crate::domains::trading::ToPrice::to_price(50000.0)),
            actual_avg_price: Some(crate::domains::trading::ToPrice::to_price(50005.0)),
            slippage_bps: 1.0, // 1 basis point
            execution_quality_score: 0.0,
            execution_reports: Vec::new(),
            error_message: None,
            metadata: HashMap::new(),
        };

        let quality_score = execution_service.calculate_quality_score(&execution).await;

        // 低延遲 (1ms) 和低滑點 (1bp) 應該得到高質量評分
        assert!(quality_score > 0.8);
    }
}
