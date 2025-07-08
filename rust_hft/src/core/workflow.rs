/*!
 * 工作流執行器 - 統一複雜操作的執行流程
 * 
 * 提供標準化的工作流管理：
 * - 步驟定義和執行
 * - 錯誤處理和重試
 * - 進度追蹤和報告
 * - 可組合的工作流
 */

use anyhow::Result;
use async_trait::async_trait;
use tracing::{info, warn, error, debug};
use std::time::{Duration, Instant};
use serde::{Serialize, Deserialize};

/// 工作流步驟特徵
#[async_trait]
pub trait WorkflowStep: Send + Sync {
    /// 步驟名稱
    fn name(&self) -> &str;
    
    /// 步驟描述
    fn description(&self) -> &str {
        "No description provided"
    }
    
    /// 預估執行時間（秒）
    fn estimated_duration(&self) -> u64 {
        60
    }
    
    /// 是否可以跳過
    fn can_skip(&self) -> bool {
        false
    }
    
    /// 執行步驟
    async fn execute(&mut self) -> Result<StepResult>;
    
    /// 清理資源
    async fn cleanup(&mut self) -> Result<()> {
        Ok(())
    }
    
    /// 檢查前置條件
    async fn check_prerequisites(&self) -> Result<bool> {
        Ok(true)
    }
}

/// 步驟執行結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub success: bool,
    pub message: String,
    pub data: serde_json::Value,
    pub metrics: std::collections::HashMap<String, f64>,
    pub execution_time_ms: u64,
}

impl StepResult {
    pub fn success(message: &str) -> Self {
        Self {
            success: true,
            message: message.to_string(),
            data: serde_json::Value::Null,
            metrics: std::collections::HashMap::new(),
            execution_time_ms: 0,
        }
    }
    
    pub fn success_with_data(message: &str, data: serde_json::Value) -> Self {
        Self {
            success: true,
            message: message.to_string(),
            data,
            metrics: std::collections::HashMap::new(),
            execution_time_ms: 0,
        }
    }
    
    pub fn failure(message: &str) -> Self {
        Self {
            success: false,
            message: message.to_string(),
            data: serde_json::Value::Null,
            metrics: std::collections::HashMap::new(),
            execution_time_ms: 0,
        }
    }
    
    pub fn with_metric(mut self, key: &str, value: f64) -> Self {
        self.metrics.insert(key.to_string(), value);
        self
    }
    
    pub fn with_execution_time(mut self, duration: Duration) -> Self {
        self.execution_time_ms = duration.as_millis() as u64;
        self
    }
}

/// 工作流執行配置
#[derive(Debug, Clone)]
pub struct WorkflowConfig {
    /// 失敗時是否停止
    pub stop_on_failure: bool,
    /// 最大重試次數
    pub max_retries: usize,
    /// 重試間隔（秒）
    pub retry_delay_secs: u64,
    /// 是否並行執行（如果可能）
    pub parallel_execution: bool,
    /// 超時時間（秒）
    pub timeout_secs: Option<u64>,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            stop_on_failure: true,
            max_retries: 3,
            retry_delay_secs: 5,
            parallel_execution: false,
            timeout_secs: Some(3600), // 1小時
        }
    }
}

/// 工作流執行器
pub struct WorkflowExecutor {
    steps: Vec<Box<dyn WorkflowStep>>,
    config: WorkflowConfig,
    results: Vec<StepResult>,
    start_time: Option<Instant>,
}

impl WorkflowExecutor {
    /// 創建新的工作流執行器
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            config: WorkflowConfig::default(),
            results: Vec::new(),
            start_time: None,
        }
    }
    
    /// 使用自定義配置創建
    pub fn with_config(config: WorkflowConfig) -> Self {
        Self {
            steps: Vec::new(),
            config,
            results: Vec::new(),
            start_time: None,
        }
    }
    
    /// 添加步驟
    pub fn add_step(mut self, step: Box<dyn WorkflowStep>) -> Self {
        self.steps.push(step);
        self
    }
    
    /// 添加多個步驟
    pub fn add_steps(mut self, steps: Vec<Box<dyn WorkflowStep>>) -> Self {
        self.steps.extend(steps);
        self
    }
    
    /// 獲取步驟數量
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }
    
    /// 執行工作流
    pub async fn execute(&mut self) -> Result<WorkflowReport> {
        self.start_time = Some(Instant::now());
        self.results.clear();
        
        info!("🚀 Starting workflow with {} steps", self.steps.len());
        
        // 檢查前置條件
        for (i, step) in self.steps.iter().enumerate() {
            info!("📋 Checking prerequisites for step {}: {}", i + 1, step.name());
            if !step.check_prerequisites().await? {
                return Err(anyhow::anyhow!("Prerequisites not met for step: {}", step.name()));
            }
        }
        
        let total_estimated_time: u64 = self.steps.iter()
            .map(|s| s.estimated_duration())
            .sum();
        info!("⏱️  Estimated total execution time: {} minutes", total_estimated_time / 60);
        
        // 執行步驟
        let total_steps = self.steps.len();
        for i in 0..total_steps {
            let step_start = Instant::now();
            let step_name = self.steps[i].name().to_string();
            let step_description = self.steps[i].description().to_string();
            info!("🔄 Executing step {}/{}: {}", i + 1, total_steps, step_name);
            debug!("Step description: {}", step_description);
            
            let result = {
                let step = &mut self.steps[i];
                step.execute().await
            };
            let execution_time = step_start.elapsed();
            
            match result {
                Ok(mut step_result) => {
                    step_result = step_result.with_execution_time(execution_time);
                    info!("✅ Step {} completed in {:.2}s: {}", 
                          i + 1, execution_time.as_secs_f64(), step_result.message);
                    self.results.push(step_result);
                }
                Err(e) => {
                    let failure_result = StepResult::failure(&format!("Step failed: {}", e))
                        .with_execution_time(execution_time);
                    error!("❌ Step {} failed after {:.2}s: {}", 
                           i + 1, execution_time.as_secs_f64(), e);
                    self.results.push(failure_result);
                    
                    if self.config.stop_on_failure {
                        error!("🛑 Stopping workflow due to failure");
                        break;
                    }
                }
            }
            
            // 檢查超時
            if let Some(timeout) = self.config.timeout_secs {
                if self.start_time.unwrap().elapsed().as_secs() > timeout {
                    warn!("⏰ Workflow timeout reached");
                    break;
                }
            }
        }
        
        // 清理
        info!("🧹 Cleaning up workflow steps...");
        for step in self.steps.iter_mut() {
            if let Err(e) = step.cleanup().await {
                warn!("⚠️  Cleanup failed for step {}: {}", step.name(), e);
            }
        }
        
        let report = self.generate_report();
        info!("📊 Workflow completed with {} successful steps out of {}", 
              report.successful_steps, report.total_steps);
        
        Ok(report)
    }
    
    /// 執行單個步驟（帶重試）
    async fn execute_single_step(&self, step: &mut Box<dyn WorkflowStep>, step_number: usize) -> Result<StepResult> {
        let mut last_error = None;
        
        for attempt in 1..=self.config.max_retries + 1 {
            if attempt > 1 {
                info!("🔄 Retrying step {} (attempt {}/{})", 
                      step_number, attempt, self.config.max_retries + 1);
                tokio::time::sleep(Duration::from_secs(self.config.retry_delay_secs)).await;
            }
            
            match step.execute().await {
                Ok(result) => {
                    if result.success {
                        return Ok(result);
                    } else {
                        last_error = Some(anyhow::anyhow!("Step returned failure: {}", result.message));
                    }
                }
                Err(e) => {
                    last_error = Some(e);
                }
            }
            
            if attempt <= self.config.max_retries {
                warn!("⚠️  Step {} attempt {} failed, retrying...", step_number, attempt);
            }
        }
        
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Unknown error")))
    }
    
    /// 生成工作流報告
    fn generate_report(&self) -> WorkflowReport {
        let total_time = self.start_time.map(|t| t.elapsed()).unwrap_or_default();
        let successful_steps = self.results.iter().filter(|r| r.success).count();
        let failed_steps = self.results.iter().filter(|r| !r.success).count();
        
        let mut all_metrics = std::collections::HashMap::new();
        for result in &self.results {
            for (key, value) in &result.metrics {
                all_metrics.insert(key.clone(), *value);
            }
        }
        
        WorkflowReport {
            total_steps: self.steps.len(),
            successful_steps,
            failed_steps,
            total_execution_time: total_time,
            step_results: self.results.clone(),
            aggregated_metrics: all_metrics,
            success: failed_steps == 0,
        }
    }
    
    /// 獲取結果
    pub fn get_results(&self) -> &[StepResult] {
        &self.results
    }
}

/// 工作流執行報告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowReport {
    pub total_steps: usize,
    pub successful_steps: usize,
    pub failed_steps: usize,
    pub total_execution_time: Duration,
    pub step_results: Vec<StepResult>,
    pub aggregated_metrics: std::collections::HashMap<String, f64>,
    pub success: bool,
}

impl WorkflowReport {
    /// 打印詳細報告
    pub fn print_detailed_report(&self) {
        info!("📋 === Workflow Execution Report ===");
        info!("Total Steps: {}", self.total_steps);
        info!("Successful: {}", self.successful_steps);
        info!("Failed: {}", self.failed_steps);
        info!("Total Time: {:.2} minutes", self.total_execution_time.as_secs_f64() / 60.0);
        info!("Overall Success: {}", if self.success { "✅" } else { "❌" });
        
        info!("📊 Step Details:");
        for (i, result) in self.step_results.iter().enumerate() {
            let status = if result.success { "✅" } else { "❌" };
            info!("  {}. {} {} ({:.2}s) - {}", 
                  i + 1, status, 
                  if result.success { "SUCCESS" } else { "FAILED" },
                  result.execution_time_ms as f64 / 1000.0,
                  result.message);
        }
        
        if !self.aggregated_metrics.is_empty() {
            info!("📈 Aggregated Metrics:");
            for (key, value) in &self.aggregated_metrics {
                info!("  {}: {:.4}", key, value);
            }
        }
        
        info!("=====================================");
    }
    
    /// 保存報告到文件
    pub fn save_to_file(&self, path: &str) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        info!("📄 Workflow report saved to: {}", path);
        Ok(())
    }
}

// === 常用工作流步驟實現 ===

/// 延遲步驟（用於測試和調試）
pub struct DelayStep {
    name: String,
    duration_secs: u64,
}

impl DelayStep {
    pub fn new(name: &str, duration_secs: u64) -> Box<dyn WorkflowStep> {
        Box::new(Self {
            name: name.to_string(),
            duration_secs,
        })
    }
}

#[async_trait]
impl WorkflowStep for DelayStep {
    fn name(&self) -> &str {
        &self.name
    }
    
    fn description(&self) -> &str {
        "Wait for specified duration"
    }
    
    fn estimated_duration(&self) -> u64 {
        self.duration_secs
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("⏳ Waiting for {} seconds...", self.duration_secs);
        tokio::time::sleep(Duration::from_secs(self.duration_secs)).await;
        Ok(StepResult::success(&format!("Waited {} seconds", self.duration_secs)))
    }
}

/// 條件步驟（根據條件決定是否執行）
pub struct ConditionalStep {
    name: String,
    condition: Box<dyn Fn() -> bool + Send + Sync>,
    inner_step: Option<Box<dyn WorkflowStep>>,
}

impl ConditionalStep {
    pub fn new<F>(name: &str, condition: F, inner_step: Box<dyn WorkflowStep>) -> Box<dyn WorkflowStep>
    where
        F: Fn() -> bool + Send + Sync + 'static,
    {
        Box::new(Self {
            name: name.to_string(),
            condition: Box::new(condition),
            inner_step: Some(inner_step),
        })
    }
}

#[async_trait]
impl WorkflowStep for ConditionalStep {
    fn name(&self) -> &str {
        &self.name
    }
    
    fn description(&self) -> &str {
        "Execute inner step if condition is met"
    }
    
    fn can_skip(&self) -> bool {
        true
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        if (self.condition)() {
            if let Some(ref mut inner) = self.inner_step {
                inner.execute().await
            } else {
                Ok(StepResult::success("No inner step to execute"))
            }
        } else {
            Ok(StepResult::success("Condition not met, skipping"))
        }
    }
}

/// 並行步驟（同時執行多個子步驟）
pub struct ParallelStep {
    name: String,
    sub_steps: Vec<Box<dyn WorkflowStep>>,
}

impl ParallelStep {
    pub fn new(name: &str, sub_steps: Vec<Box<dyn WorkflowStep>>) -> Box<dyn WorkflowStep> {
        Box::new(Self {
            name: name.to_string(),
            sub_steps,
        })
    }
}

#[async_trait]
impl WorkflowStep for ParallelStep {
    fn name(&self) -> &str {
        &self.name
    }
    
    fn description(&self) -> &str {
        "Execute multiple steps in parallel"
    }
    
    fn estimated_duration(&self) -> u64 {
        // 並行執行，所以取最大值
        self.sub_steps.iter()
            .map(|s| s.estimated_duration())
            .max()
            .unwrap_or(60)
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("🔀 Executing {} sub-steps in parallel", self.sub_steps.len());
        
        let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        
        // Note: 這是一個簡化實現，實際使用時需要更複雜的並行邏輯
        // 由於 WorkflowStep 特徵的限制，這裡只能順序執行
        let mut results = Vec::new();
        for step in self.sub_steps.iter_mut() {
            let result = step.execute().await?;
            results.push(result);
        }
        
        let all_success = results.iter().all(|r| r.success);
        let message = format!("Parallel execution completed: {} successes", 
                             results.iter().filter(|r| r.success).count());
        
        if all_success {
            Ok(StepResult::success(&message))
        } else {
            Ok(StepResult::failure(&message))
        }
    }
}

/// 便利函數：創建簡單的工作流
pub fn create_simple_workflow(steps: Vec<Box<dyn WorkflowStep>>) -> WorkflowExecutor {
    let mut executor = WorkflowExecutor::new();
    for step in steps {
        executor = executor.add_step(step);
    }
    executor
}

/// 便利函數：創建帶配置的工作流
pub fn create_workflow_with_config(
    steps: Vec<Box<dyn WorkflowStep>>, 
    config: WorkflowConfig
) -> WorkflowExecutor {
    let mut executor = WorkflowExecutor::with_config(config);
    for step in steps {
        executor = executor.add_step(step);
    }
    executor
}

#[cfg(test)]
mod tests {
    use super::*;
    
    struct TestStep {
        name: String,
        should_fail: bool,
    }
    
    impl TestStep {
        fn new(name: &str, should_fail: bool) -> Box<dyn WorkflowStep> {
            Box::new(Self {
                name: name.to_string(),
                should_fail,
            })
        }
    }
    
    #[async_trait]
    impl WorkflowStep for TestStep {
        fn name(&self) -> &str {
            &self.name
        }
        
        async fn execute(&mut self) -> Result<StepResult> {
            if self.should_fail {
                Err(anyhow::anyhow!("Test failure"))
            } else {
                Ok(StepResult::success("Test passed"))
            }
        }
    }
    
    #[tokio::test]
    async fn test_successful_workflow() {
        let mut executor = WorkflowExecutor::new()
            .add_step(TestStep::new("step1", false))
            .add_step(TestStep::new("step2", false));
        
        let report = executor.execute().await.unwrap();
        assert!(report.success);
        assert_eq!(report.successful_steps, 2);
        assert_eq!(report.failed_steps, 0);
    }
    
    #[tokio::test]
    async fn test_failing_workflow() {
        let mut executor = WorkflowExecutor::new()
            .add_step(TestStep::new("step1", false))
            .add_step(TestStep::new("step2", true));
        
        let report = executor.execute().await.unwrap();
        assert!(!report.success);
        assert_eq!(report.successful_steps, 1);
        assert_eq!(report.failed_steps, 1);
    }
    
    #[tokio::test]
    async fn test_delay_step() {
        let mut executor = WorkflowExecutor::new()
            .add_step(DelayStep::new("delay", 1));
        
        let start = Instant::now();
        let report = executor.execute().await.unwrap();
        let elapsed = start.elapsed();
        
        assert!(report.success);
        assert!(elapsed >= Duration::from_secs(1));
    }
}