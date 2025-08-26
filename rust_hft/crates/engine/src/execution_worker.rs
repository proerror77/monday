//! 执行 Worker - 独立任务处理订单意图，避免引擎锁定
//!
//! 架构:
//! - 从意图队列批量接收 OrderIntent
//! - 调用 ExecutionClient (带 await) 
//! - 将 ExecutionEvent 发送到回报队列
//! - 所有网络 await 不会阻塞引擎主循环

use crate::execution_queues::WorkerQueues;
use ports::{ExecutionClient, OrderIntent, ExecutionEvent, BoxStream};
use hft_core::{HftError, OrderId, now_micros};
use tracing::{info, warn, error, debug};
use tokio::time::{sleep, Duration, Instant};
use futures::{StreamExt, FutureExt};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 客户端选择策略
#[derive(Debug, Clone, Copy)]
pub enum ClientSelectionStrategy {
    /// 基于符号名称的一致性哈希（确保同一品种总是路由到同一客户端）
    ConsistentHash,
    /// 轮询策略（负载均衡，但可能将同品种分散到不同客户端）
    RoundRobin,
}

impl Default for ClientSelectionStrategy {
    fn default() -> Self {
        Self::ConsistentHash
    }
}

/// 执行 Worker 配置
#[derive(Debug, Clone)]
pub struct ExecutionWorkerConfig {
    /// Worker 名称
    pub name: String,
    /// 批处理大小
    pub batch_size: usize,
    /// 空闲时的睡眠时间 (ms)
    pub idle_sleep_ms: u64,
    /// 最大重试次数
    pub max_retries: u32,
    /// 重试延迟 (ms)
    pub retry_delay_ms: u64,
    /// 客户端选择策略
    pub client_selection: ClientSelectionStrategy,
}

impl Default for ExecutionWorkerConfig {
    fn default() -> Self {
        Self {
            name: "execution_worker".to_string(),
            batch_size: 16,
            idle_sleep_ms: 1, // 1ms 空闲睡眠
            max_retries: 3,
            retry_delay_ms: 100,
            client_selection: ClientSelectionStrategy::default(),
        }
    }
}

/// 执行 Worker 统计
#[derive(Debug, Default)]
pub struct ExecutionWorkerStats {
    pub intents_processed: u64,
    pub orders_placed: u64,
    pub orders_failed: u64,
    pub events_sent: u64,
    pub queue_full_events: u64,
    pub retries_count: u64,
    /// 執行階段延遲統計（微秒）
    pub execution_latency_micros: Vec<u64>,
    /// 最近的執行延遲（用於實時監控）
    pub recent_execution_latency_micros: Option<u64>,
}

/// 执行 Worker - 在独立 Tokio 任务中运行
pub struct ExecutionWorker {
    config: ExecutionWorkerConfig,
    queues: WorkerQueues,
    execution_clients: Vec<Box<dyn ExecutionClient>>,
    execution_streams: Vec<BoxStream<ExecutionEvent>>,
    stats: ExecutionWorkerStats,
    /// 订单 ID 到客户端索引的映射
    order_to_client: HashMap<OrderId, usize>,
    /// 轮询计数器（用于 RoundRobin 策略）
    round_robin_counter: AtomicUsize,
}

impl ExecutionWorker {
    /// 创建新的执行 Worker
    pub fn new(
        config: ExecutionWorkerConfig,
        queues: WorkerQueues,
        execution_clients: Vec<Box<dyn ExecutionClient>>,
    ) -> Self {
        Self {
            config,
            queues,
            execution_clients,
            execution_streams: Vec::new(),
            stats: ExecutionWorkerStats::default(),
            order_to_client: HashMap::new(),
            round_robin_counter: AtomicUsize::new(0),
        }
    }
    
    /// 启动 Worker 主循环
    pub async fn run(mut self) -> Result<(), HftError> {
        info!("启动执行 Worker: {}", self.config.name);
        
        // 连接所有执行客户端
        self.connect_execution_clients().await?;
        
        // 准备执行回报流
        self.prepare_execution_streams().await?;
        
        let mut last_activity = Instant::now();
        
        loop {
            let tick_start = Instant::now();
            let mut had_activity = false;
            
            // 1. 处理意图队列中的新订单
            let intents = self.queues.receive_intents();
            if !intents.is_empty() {
                self.process_order_intents(intents).await;
                had_activity = true;
            }
            
            // 2. 处理执行回报流
            let events_received = self.poll_execution_events().await;
            if events_received > 0 {
                had_activity = true;
            }
            
            // 3. 统计和调试
            if had_activity {
                last_activity = tick_start;
                debug!("Worker {} 处理活动，意图队列利用率: {:.2}%", 
                       self.config.name,
                       self.queues.intent_queue_utilization() * 100.0);
            }
            
            // 4. 空闲控制
            if !had_activity {
                let idle_duration = tick_start.duration_since(last_activity);
                if idle_duration.as_millis() > 1000 {
                    // 长时间空闲，增加睡眠时间
                    sleep(Duration::from_millis(self.config.idle_sleep_ms * 2)).await;
                } else {
                    // 正常空闲
                    sleep(Duration::from_millis(self.config.idle_sleep_ms)).await;
                }
            }
            
            // 5. 周期性状态日志
            if tick_start.duration_since(last_activity).as_secs() > 30 {
                info!("Worker {} 状态: 意图处理 {}, 订单下达 {}, 事件发送 {}",
                      self.config.name,
                      self.stats.intents_processed,
                      self.stats.orders_placed, 
                      self.stats.events_sent);
                last_activity = tick_start; // 重置避免频繁日志
            }
        }
    }
    
    /// 连接所有执行客户端
    async fn connect_execution_clients(&mut self) -> Result<(), HftError> {
        if self.execution_clients.is_empty() {
            info!("没有执行客户端需要连接");
            return Ok(());
        }
        
        for (idx, client) in self.execution_clients.iter_mut().enumerate() {
            match client.connect().await {
                Ok(()) => {
                    info!("执行客户端 {} 连接成功", idx);
                }
                Err(e) => {
                    error!("执行客户端 {} 连接失败: {}", idx, e);
                    return Err(e);
                }
            }
        }
        Ok(())
    }
    
    /// 准备执行回报流
    async fn prepare_execution_streams(&mut self) -> Result<(), HftError> {
        self.execution_streams.clear();
        if self.execution_clients.is_empty() {
            info!("没有执行客户端需要准备回报流");
            return Ok(());
        }
        
        for (idx, client) in self.execution_clients.iter().enumerate() {
            match client.execution_stream().await {
                Ok(stream) => {
                    self.execution_streams.push(stream);
                    debug!("执行客户端 {} 回报流准备完成", idx);
                }
                Err(e) => {
                    error!("执行客户端 {} 回报流准备失败: {}", idx, e);
                    return Err(e);
                }
            }
        }
        Ok(())
    }
    
    /// 处理订单意图批次
    async fn process_order_intents(&mut self, intents: Vec<OrderIntent>) {
        self.stats.intents_processed += intents.len() as u64;
        
        if self.execution_clients.is_empty() {
            // 没有执行客户端，直接发送失败事件
            for _intent in intents {
                let reject_event = ExecutionEvent::OrderReject {
                    order_id: OrderId(format!("no_client_{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0))),
                    reason: "没有可用的执行客户端".to_string(),
                    timestamp: chrono::Utc::now().timestamp_micros() as u64,
                };
                let _ = self.queues.send_event_force(reject_event);
                self.stats.orders_failed += 1;
            }
            return;
        }
        
        for intent in intents {
            let execution_start = now_micros();
            
            // 选择执行客户端
            let client_idx = self.select_execution_client(&intent);
            
            match self.place_order_with_retry(client_idx, intent).await {
                Ok(order_id) => {
                    // 記錄執行延遲
                    let execution_latency = now_micros().saturating_sub(execution_start);
                    self.stats.execution_latency_micros.push(execution_latency);
                    self.stats.recent_execution_latency_micros = Some(execution_latency);
                    
                    self.stats.orders_placed += 1;
                    // 记录订单到客户端映射，用于回报路由
                    self.order_to_client.insert(order_id, client_idx);
                    
                    debug!("訂單執行成功，延遲: {}μs", execution_latency);
                }
                Err(e) => {
                    // 記錄執行失敗延遲
                    let execution_latency = now_micros().saturating_sub(execution_start);
                    self.stats.execution_latency_micros.push(execution_latency);
                    self.stats.recent_execution_latency_micros = Some(execution_latency);
                    
                    self.stats.orders_failed += 1;
                    warn!("下单失败: {}", e);
                    
                    // 发送失败事件到引擎
                    let reject_event = ExecutionEvent::OrderReject {
                        order_id: OrderId(format!("failed_{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0))),
                        reason: format!("Worker 下单失败: {}", e),
                        timestamp: chrono::Utc::now().timestamp_micros() as u64,
                    };
                    
                    if let Err(failed_event) = self.queues.send_event(reject_event) {
                        // 队列满载，使用 force_send
                        self.queues.send_event_force(failed_event);
                    }
                }
            }
        }
    }
    
    /// 下单并重试
    async fn place_order_with_retry(&mut self, client_idx: usize, intent: OrderIntent) -> Result<OrderId, HftError> {
        let mut last_error = HftError::Generic { message: "未知错误".to_string() };
        
        for attempt in 0..=self.config.max_retries {
            match self.execution_clients[client_idx].place_order(intent.clone()).await {
                Ok(order_id) => {
                    if attempt > 0 {
                        self.stats.retries_count += attempt as u64;
                        info!("下单重试 {} 次后成功: {}", attempt, order_id.0);
                    }
                    return Ok(order_id);
                }
                Err(e) => {
                    last_error = e;
                    if attempt < self.config.max_retries {
                        warn!("下单失败，尝试重试 {}/{}: {}", 
                              attempt + 1, self.config.max_retries, last_error);
                        sleep(Duration::from_millis(self.config.retry_delay_ms)).await;
                    }
                }
            }
        }
        
        Err(last_error)
    }
    
    /// 轮询执行回报流
    async fn poll_execution_events(&mut self) -> u32 {
        let mut events_count = 0;
        
        for stream in &mut self.execution_streams {
            let mut batch_count = 0;
            
            // 批量处理回报，避免单个流阻塞
            while batch_count < self.config.batch_size {
                match stream.next().now_or_never() {
                    Some(Some(Ok(event))) => {
                        // 发送回报到引擎
                        match self.queues.send_event(event) {
                            Ok(()) => {
                                self.stats.events_sent += 1;
                                events_count += 1;
                                batch_count += 1;
                            }
                            Err(failed_event) => {
                                // 队列满载，尝试 force_send
                                if self.queues.send_event_force(failed_event) {
                                    self.stats.events_sent += 1;
                                    events_count += 1;
                                } else {
                                    self.stats.queue_full_events += 1;
                                    warn!("回报队列满载，事件丢失");
                                }
                                batch_count += 1;
                            }
                        }
                    }
                    Some(Some(Err(e))) => {
                        warn!("执行回报流错误: {}", e);
                        break;
                    }
                    Some(None) => {
                        debug!("执行回报流结束");
                        break;
                    }
                    None => break, // 没有可用事件
                }
            }
        }
        
        events_count
    }
    
    /// 选择执行客户端（优化版 - 支持多种策略）
    fn select_execution_client(&self, intent: &OrderIntent) -> usize {
        if self.execution_clients.is_empty() {
            return 0;
        }
        
        match self.config.client_selection {
            ClientSelectionStrategy::ConsistentHash => {
                // 使用品种名称的 FNV-1a hash，确保同一品种总是路由到同一客户端
                // 优点：相同品种的订单状态一致性，减少资金分散
                // FNV-1a 常量：offset_basis = 2166136261, prime = 16777619
                let mut hash: u32 = 2166136261;
                for byte in intent.symbol.0.as_bytes() {
                    hash ^= *byte as u32;
                    hash = hash.wrapping_mul(16777619);
                }
                (hash as usize) % self.execution_clients.len()
            }
            ClientSelectionStrategy::RoundRobin => {
                // 轮询策略，确保各客户端负载均衡
                // 优点：避免热点客户端，最大化并行度
                let current = self.round_robin_counter.fetch_add(1, Ordering::Relaxed);
                current % self.execution_clients.len()
            }
        }
    }
    
    /// 获取统计信息
    pub fn stats(&self) -> &ExecutionWorkerStats {
        &self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::OrderIntent;
    use hft_core::{Symbol, Side, OrderType, TimeInForce, Price, Quantity};
    use std::collections::HashSet;

    fn create_test_intent(symbol: &str) -> OrderIntent {
        OrderIntent {
            symbol: Symbol(symbol.to_string()),
            side: Side::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::from_f64(1.0).unwrap(),
            price: Some(Price::from_f64(100.0).unwrap()),
            time_in_force: TimeInForce::IOC,
            strategy_id: "test".to_string(),
        }
    }

    #[test]
    fn test_consistent_hash_selection() {
        let client_count = 3;
        let symbols = ["BTCUSDT", "ETHUSDT", "ADAUSDT", "DOTUSDT"];
        
        // 測試一致性哈希：相同的符號應該總是選擇相同的客戶端
        for symbol in &symbols {
            // 模擬一致性哈希計算
            let mut hash: u32 = 2166136261;
            for byte in symbol.as_bytes() {
                hash ^= *byte as u32;
                hash = hash.wrapping_mul(16777619);
            }
            let expected_client = (hash as usize) % client_count;
            
            // 多次計算應該得到相同結果
            for _ in 0..10 {
                let mut test_hash: u32 = 2166136261;
                for byte in symbol.as_bytes() {
                    test_hash ^= *byte as u32;
                    test_hash = test_hash.wrapping_mul(16777619);
                }
                let actual_client = (test_hash as usize) % client_count;
                assert_eq!(expected_client, actual_client, 
                    "一致性哈希對於符號 '{}' 應該總是返回相同的客戶端", symbol);
            }
        }
    }

    #[test]
    fn test_round_robin_distribution() {
        let client_count = 3;
        let rounds = 9; // 3 輪循環
        
        // 模擬輪詢分配
        let mut distribution = vec![0; client_count];
        for i in 0..rounds {
            let client_idx = i % client_count;
            distribution[client_idx] += 1;
        }
        
        // 驗證分配均勻性
        let expected_per_client = rounds / client_count;
        for (client_idx, count) in distribution.iter().enumerate() {
            assert_eq!(*count, expected_per_client,
                "輪詢策略應該均勻分配負載，客戶端 {} 期望 {} 次，實際 {} 次", 
                client_idx, expected_per_client, count);
        }
    }

    #[test] 
    fn test_hash_distribution_quality() {
        // 測試哈希分佈質量
        let client_count = 4;
        let symbols = ["BTCUSDT", "ETHUSDT", "ADAUSDT", "DOTUSDT", "BNBUSDT", 
                      "XRPUSDT", "SOLUSDT", "LINKUSDT", "AVAXUSDT", "MATICUSDT"];
        
        let mut distribution = vec![0; client_count];
        let mut used_clients = HashSet::new();
        
        for symbol in &symbols {
            let mut hash: u32 = 2166136261;
            for byte in symbol.as_bytes() {
                hash ^= *byte as u32;
                hash = hash.wrapping_mul(16777619);
            }
            let client_idx = (hash as usize) % client_count;
            distribution[client_idx] += 1;
            used_clients.insert(client_idx);
        }
        
        // 應該至少使用 2 個不同的客戶端（避免所有流量集中在單個客戶端）
        assert!(used_clients.len() >= 2, 
            "哈希分佈應該使用多個客戶端，實際只使用了 {} 個", used_clients.len());
        
        // 列印分佈以供調試
        println!("哈希分佈: {:?}", distribution);
    }
}

/// 创建并启动执行 Worker 任务
pub fn spawn_execution_worker(
    config: ExecutionWorkerConfig,
    queues: WorkerQueues,
    execution_clients: Vec<Box<dyn ExecutionClient>>,
) -> tokio::task::JoinHandle<Result<(), HftError>> {
    let worker = ExecutionWorker::new(config.clone(), queues, execution_clients);
    
    tokio::spawn(async move {
        info!("执行 Worker {} 任务启动", config.name);
        worker.run().await
    })
}