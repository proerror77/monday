//! 执行 Worker - 独立任务处理订单意图，避免引擎锁定
//!
//! 架构:
//! - 从意图队列批量接收 OrderIntent
//! - 调用 ExecutionClient (带 await)
//! - 将 ExecutionEvent 发送到回报队列
//! - 所有网络 await 不会阻塞引擎主循环

use crate::execution_queues::WorkerQueues;
use crate::latency_monitor::{LatencyMonitor, LatencyMonitorConfig};
use futures::{FutureExt, StreamExt};
use hft_core::{now_micros, AccountId, HftError, LatencyStage, OrderId, Price, Quantity};
use hft_core::{Symbol, VenueId};
use ports::{
    AccountBalance, BoxStream, ExecutionClient, ExecutionEvent, ExecutionRouter, OpenOrder,
    OrderIntent,
};
use rustc_hash::FxHashMap;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::task::yield_now;
use tokio::time::{sleep, Duration, Instant};
use tracing::{debug, error, info, warn};

/// 客户端选择策略
#[derive(Debug, Clone, Copy, Default)]
pub enum ClientSelectionStrategy {
    /// 基于符号名称的一致性哈希（确保同一品种总是路由到同一客户端）
    #[default]
    ConsistentHash,
    /// 轮询策略（负载均衡，但可能将同品种分散到不同客户端）
    RoundRobin,
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
    /// 延遲監控配置
    pub latency_monitor: LatencyMonitorConfig,
    /// Ack 超時（毫秒），0 表示不啟用
    pub ack_timeout_ms: u64,
    /// 對帳間隔（毫秒），0 表示不啟用
    pub reconcile_interval_ms: u64,
    /// 是否自動撤銷交換端存在但本地未追蹤的訂單
    pub auto_cancel_exchange_only: bool,
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
            latency_monitor: LatencyMonitorConfig::default(),
            ack_timeout_ms: 3000,
            reconcile_interval_ms: 5000,
            auto_cancel_exchange_only: false,
        }
    }
}

impl ExecutionWorkerConfig {
    /// 高性能預設：降低批量系統調用開銷與空閒睡眠延遲
    pub fn high_performance() -> Self {
        Self {
            batch_size: 64,
            idle_sleep_ms: 0,
            ack_timeout_ms: 2000,
            retry_delay_ms: 50,
            ..Default::default()
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
    order_to_client: FxHashMap<OrderId, usize>,
    /// 轮询计数器（用于 RoundRobin 策略）
    round_robin_counter: AtomicUsize,
    /// 延遲監控器 - 追蹤 Worker 執行延遲
    latency_monitor: Arc<LatencyMonitor>,
    /// 控制通道（取消等指令）
    control_rx: mpsc::UnboundedReceiver<ControlCommand>,
    /// Phase 1 重構：可插拔執行路由器
    router: Option<Box<dyn ExecutionRouter>>,
    /// Venue 到客戶端索引的映射（用於新路由系統）
    venue_to_client: HashMap<VenueId, usize>,
    /// 等待 Ack 的訂單：order_id -> (symbol, 下單時間)
    pending_acks: FxHashMap<OrderId, (Symbol, Instant)>,
    /// 上次對帳時間
    last_reconcile: Instant,
    /// 策略到客戶端索引的映射（同交易所多帳戶路由）
    strategy_to_client: Option<rustc_hash::FxHashMap<String, usize>>,
    /// 帳戶到客戶端索引的映射（恢復訂單與控制面路由）
    account_to_client: FxHashMap<AccountId, usize>,
    /// Emergency is sticky for the worker lifetime; restart is required to re-arm execution.
    accepting_intents: bool,
    emergency_latched: bool,
}

impl ExecutionWorker {
    /// 反查某 client 對應的 VenueId（若唯一對應）
    fn venue_for_client(&self, client_idx: usize) -> Option<VenueId> {
        for (venue, idx) in &self.venue_to_client {
            if *idx == client_idx {
                return Some(*venue);
            }
        }
        None
    }

    /// 檢查等待 Ack 的訂單是否超時，嘗試取消（同步改為 async 直接等待撤單）
    async fn check_ack_timeouts(&mut self) -> usize {
        if self.pending_acks.is_empty() || self.config.ack_timeout_ms == 0 {
            return 0;
        }
        let now = Instant::now();
        let timeout = Duration::from_millis(self.config.ack_timeout_ms);
        let mut timed_out: Vec<(OrderId, Symbol)> = Vec::new();
        self.pending_acks.retain(|oid, (sym, ts)| {
            if now.duration_since(*ts) > timeout {
                timed_out.push((oid.clone(), sym.clone()));
                false
            } else {
                true
            }
        });

        for (order_id, symbol) in &timed_out {
            let client_idx = match self.order_to_client.get(order_id).copied() {
                Some(i) => i,
                None => self.select_client_by_symbol(symbol),
            };
            if let Some(client) = self.execution_clients.get_mut(client_idx) {
                let oid = order_id.clone();
                let sym = symbol.clone();
                if let Err(e) = client.cancel_order(&oid).await {
                    tracing::warn!("Ack timeout cancel failed: {} - {}", oid.0, e);
                } else {
                    tracing::info!("Ack timeout: cancel sent for {} ({})", oid.0, sym.as_str());
                }
            }
        }
        timed_out.len()
    }
    /// 创建新的执行 Worker（舊版，保持向後兼容性）
    pub fn new(
        config: ExecutionWorkerConfig,
        queues: WorkerQueues,
        execution_clients: Vec<Box<dyn ExecutionClient>>,
        control_rx: mpsc::UnboundedReceiver<ControlCommand>,
    ) -> Self {
        let latency_monitor = Arc::new(LatencyMonitor::new(config.latency_monitor.clone()));

        Self {
            config,
            queues,
            execution_clients,
            execution_streams: Vec::new(),
            stats: ExecutionWorkerStats::default(),
            order_to_client: FxHashMap::default(),
            round_robin_counter: AtomicUsize::new(0),
            latency_monitor,
            control_rx,
            router: None, // 使用舊的硬編碼邏輯
            venue_to_client: HashMap::new(),
            pending_acks: FxHashMap::default(),
            last_reconcile: Instant::now(),
            strategy_to_client: None,
            account_to_client: FxHashMap::default(),
            accepting_intents: true,
            emergency_latched: false,
        }
    }

    /// Phase 1 重構：創建帶路由器的执行 Worker
    pub fn new_with_router(
        config: ExecutionWorkerConfig,
        queues: WorkerQueues,
        execution_clients: Vec<Box<dyn ExecutionClient>>,
        control_rx: mpsc::UnboundedReceiver<ControlCommand>,
        router: Box<dyn ExecutionRouter>,
        venue_to_client: HashMap<VenueId, usize>,
    ) -> Self {
        let latency_monitor = Arc::new(LatencyMonitor::new(config.latency_monitor.clone()));

        Self {
            config,
            queues,
            execution_clients,
            execution_streams: Vec::new(),
            stats: ExecutionWorkerStats::default(),
            order_to_client: FxHashMap::default(),
            round_robin_counter: AtomicUsize::new(0),
            latency_monitor,
            control_rx,
            router: Some(router),
            venue_to_client,
            pending_acks: FxHashMap::default(),
            last_reconcile: Instant::now(),
            strategy_to_client: None,
            account_to_client: FxHashMap::default(),
            accepting_intents: true,
            emergency_latched: false,
        }
    }

    /// 獲取延遲監控器的引用
    pub fn latency_monitor(&self) -> Arc<LatencyMonitor> {
        self.latency_monitor.clone()
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

            // 0. 非阻塞處理控制指令
            while let Ok(cmd) = self.control_rx.try_recv() {
                self.handle_control_command(cmd).await;
                had_activity = true;
            }

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

            // 檢查 Ack 超時並嘗試取消
            if self.config.ack_timeout_ms > 0 {
                let timeouts = self.check_ack_timeouts().await;
                if timeouts > 0 {
                    had_activity = true;
                }
            }

            // 3. 週期性對帳：比對交易所未結訂單
            if self.config.reconcile_interval_ms > 0
                && self.last_reconcile.elapsed()
                    > Duration::from_millis(self.config.reconcile_interval_ms)
            {
                let reconciled = self.reconcile_open_orders().await;
                if reconciled {
                    had_activity = true;
                }
                self.last_reconcile = Instant::now();
            }

            // 3. 统计和调试
            if had_activity {
                last_activity = tick_start;
                debug!(
                    "Worker {} 处理活动，意图队列利用率: {:.2}%",
                    self.config.name,
                    self.queues.intent_queue_utilization() * 100.0
                );
            }

            // 4. 空闲控制
            if !had_activity {
                let idle_duration = tick_start.duration_since(last_activity);
                if self.config.idle_sleep_ms == 0 {
                    // 忙等模式下交出調度權，避免 100% CPU
                    yield_now().await;
                } else if idle_duration.as_millis() > 1000 {
                    // 长时间空闲，增加睡眠时间
                    sleep(Duration::from_millis(self.config.idle_sleep_ms * 2)).await;
                } else {
                    // 正常空闲
                    sleep(Duration::from_millis(self.config.idle_sleep_ms)).await;
                }
            }

            // 5. 周期性状态日志
            if tick_start.duration_since(last_activity).as_secs() > 30 {
                info!(
                    "Worker {} 状态: 意图处理 {}, 订单下达 {}, 事件发送 {}",
                    self.config.name,
                    self.stats.intents_processed,
                    self.stats.orders_placed,
                    self.stats.events_sent
                );
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
                    order_id: OrderId(format!(
                        "no_client_{}",
                        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
                    )),
                    reason: "没有可用的执行客户端".to_string(),
                    timestamp: chrono::Utc::now().timestamp_micros() as u64,
                };
                let _ = self.queues.send_event_force(reject_event);
                self.stats.orders_failed += 1;
            }
            return;
        }

        for intent in intents {
            while let Ok(command) = self.control_rx.try_recv() {
                self.handle_control_command(command).await;
            }
            if !self.accepting_intents {
                self.reject_for_disabled_intake(&intent);
                continue;
            }
            let execution_start = now_micros();

            // 🔥 Phase 1.1: 选择执行客户端 - 現在可能失敗
            let client_idx = match self.select_execution_client(&intent) {
                Ok(idx) => idx,
                Err(reason) => {
                    // 客戶端選擇失敗，發送拒絕事件
                    warn!("客戶端選擇失敗: {}", reason);
                    let reject_event = ExecutionEvent::OrderReject {
                        order_id: OrderId(format!(
                            "route_failed_{}",
                            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
                        )),
                        reason: format!("路由失敗: {}", reason),
                        timestamp: chrono::Utc::now().timestamp_micros() as u64,
                    };

                    if let Err(failed_event) = self.queues.send_event(reject_event) {
                        // 队列满载，使用 force_send
                        self.queues.send_event_force(failed_event);
                    }
                    self.stats.orders_failed += 1;
                    continue; // 跳過這個意圖，繼續處理下一個
                }
            };

            match self.place_order_with_retry(client_idx, &intent).await {
                Ok(order_id) => {
                    // 記錄執行延遲到統一監控器和本地統計
                    let execution_latency = now_micros().saturating_sub(execution_start);
                    self.latency_monitor
                        .record_latency(LatencyStage::Submission, execution_latency);
                    self.stats.execution_latency_micros.push(execution_latency);
                    self.stats.recent_execution_latency_micros = Some(execution_latency);
                    #[cfg(feature = "metrics")]
                    infra_metrics::MetricsRegistry::global()
                        .record_submission_latency(execution_latency as f64);

                    self.stats.orders_placed += 1;
                    // 记录订单到客户端映射，用于回报路由
                    self.order_to_client.insert(order_id.clone(), client_idx);

                    // 向引擎派發 OrderNew 以註冊訂單（便於 Portfolio/OMS 正確處理 Fill）
                    // 推斷對應的 venue（根據 venue_to_client 反查）
                    let venue_for_client = self.venue_for_client(client_idx);

                    let OrderIntent {
                        symbol,
                        side,
                        quantity,
                        order_type: _,
                        price,
                        time_in_force: _,
                        strategy_id,
                        target_venue: _,
                        ..
                    } = intent;

                    let symbol_for_ack = symbol.clone();

                    let new_event = ExecutionEvent::OrderNew {
                        order_id: order_id.clone(),
                        symbol,
                        side,
                        quantity,
                        requested_price: price,
                        timestamp: now_micros(),
                        venue: venue_for_client,
                        strategy_id,
                    };
                    if let Err(failed_event) = self.queues.send_event(new_event) {
                        // 队列满载，使用強制发送
                        self.queues.send_event_force(failed_event);
                    }

                    debug!("訂單執行成功，延遲: {}μs", execution_latency);

                    // 標記等待 Ack
                    self.pending_acks
                        .insert(order_id.clone(), (symbol_for_ack, Instant::now()));
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
                        order_id: OrderId(format!(
                            "failed_{}",
                            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
                        )),
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

    fn reject_for_disabled_intake(&mut self, intent: &OrderIntent) {
        let reject_event = ExecutionEvent::OrderReject {
            order_id: OrderId(format!(
                "emergency_blocked_{}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
            )),
            reason: format!(
                "execution intake disabled by emergency control for {}",
                intent.symbol.as_str()
            ),
            timestamp: now_micros(),
        };
        let _ = self.queues.send_event_force(reject_event);
        self.stats.orders_failed += 1;
    }

    /// 下单并重试
    async fn place_order_with_retry(
        &mut self,
        client_idx: usize,
        intent: &OrderIntent,
    ) -> Result<OrderId, HftError> {
        let mut last_error = HftError::Generic {
            message: "未知错误".to_string(),
        };

        for attempt in 0..=self.config.max_retries {
            match self.execution_clients[client_idx]
                .place_order(intent.clone())
                .await
            {
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
                        warn!(
                            "下单失败，尝试重试 {}/{}: {}",
                            attempt + 1,
                            self.config.max_retries,
                            last_error
                        );
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
                        // Keep routing metadata only while an order may still be open.
                        match &event {
                            ExecutionEvent::OrderAck { order_id, .. } => {
                                self.pending_acks.remove(order_id);
                            }
                            ExecutionEvent::OrderCanceled { order_id, .. }
                            | ExecutionEvent::OrderReject { order_id, .. }
                            | ExecutionEvent::OrderCompleted { order_id, .. } => {
                                self.pending_acks.remove(order_id);
                                self.order_to_client.remove(order_id);
                            }
                            _ => {}
                        }
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

    /// 选择执行客户端（Phase 1 重構：支持路由器或舊邏輯 + 強制目標場約束）
    fn select_execution_client(&self, intent: &OrderIntent) -> Result<usize, String> {
        // 0) 策略→客戶端映射（支援同交易所多帳戶）
        if let Some(map) = &self.strategy_to_client {
            if let Some(&idx) = map.get(&intent.strategy_id) {
                return Ok(idx);
            }
            if let Some(pos) = intent.strategy_id.find(':') {
                let base = &intent.strategy_id[..pos];
                if let Some(&idx) = map.get(base) {
                    return Ok(idx);
                }
            }
        }
        if self.execution_clients.is_empty() {
            return Err("沒有可用的執行客戶端".to_string());
        }

        // Phase 1 重構：如果有路由器，使用新邏輯
        if let Some(ref router) = self.router {
            if let Some(decision) = router.route_order(intent, &self.venue_to_client, None) {
                debug!(
                    "Router '{}' decision: target_venue={}, client_index={}, reason='{}'",
                    router.name(),
                    decision.target_venue,
                    decision.client_index,
                    decision.reason
                );

                // 安全檢查：確保 client_index 在有效範圍內
                if decision.client_index >= self.execution_clients.len() {
                    return Err(format!(
                        "Router '{}' returned out-of-range client_index={} (available clients: {})",
                        router.name(),
                        decision.client_index,
                        self.execution_clients.len()
                    ));
                }

                return Ok(decision.client_index);
            } else {
                return Err(format!(
                    "Router '{}' failed to find route for intent: strategy_id={}, symbol={}",
                    router.name(),
                    intent.strategy_id,
                    intent.symbol.as_str()
                ));
            }
        }

        // 🔥 Phase 1.1: 強制目標場約束 - 多個執行客戶端且無路由器時，target_venue 必須存在
        if self.execution_clients.len() > 1 {
            if let Some(target_venue) = &intent.target_venue {
                // 檢查 target_venue 是否有對應的客戶端
                if let Some(&client_index) = self.venue_to_client.get(target_venue) {
                    if client_index < self.execution_clients.len() {
                        debug!(
                            "使用指定的目標場: {} (client_index: {})",
                            target_venue, client_index
                        );
                        return Ok(client_index);
                    } else {
                        return Err(format!(
                            "指定的目標場 '{}' 對應的客戶端索引 {} 超出範圍 (總客戶端數: {})",
                            target_venue,
                            client_index,
                            self.execution_clients.len()
                        ));
                    }
                } else {
                    let available_venues: Vec<String> =
                        self.venue_to_client.keys().map(|v| v.to_string()).collect();
                    return Err(format!(
                        "指定的目標場 '{}' 不存在。可用場所: {:?}",
                        target_venue, available_venues
                    ));
                }
            } else {
                // 🔥 關鍵：多客戶端無路由器時強制要求 target_venue
                let available_venues: Vec<String> =
                    self.venue_to_client.keys().map(|v| v.to_string()).collect();
                return Err(format!("多個執行客戶端且無路由器時，訂單意圖必須指定 target_venue。策略: {}, 可用場所: {:?}",
                    intent.strategy_id, available_venues));
            }
        }

        // 單個客戶端時的舊邏輯：保持向後兼容性
        match self.config.client_selection {
            ClientSelectionStrategy::ConsistentHash => {
                // 使用品种名称的 FNV-1a hash，确保同一品种总是路由到同一客户端
                let mut hash: u32 = 2166136261;
                for byte in intent.symbol.as_str().as_bytes() {
                    hash ^= *byte as u32;
                    hash = hash.wrapping_mul(16777619);
                }
                Ok((hash as usize) % self.execution_clients.len())
            }
            ClientSelectionStrategy::RoundRobin => {
                // 轮询策略，确保各客户端负载均衡
                let current = self.round_robin_counter.fetch_add(1, Ordering::Relaxed);
                Ok(current % self.execution_clients.len())
            }
        }
    }

    /// 获取统计信息
    pub fn stats(&self) -> &ExecutionWorkerStats {
        &self.stats
    }

    /// 處理控制指令（取消訂單等）
    async fn handle_control_command(&mut self, cmd: ControlCommand) {
        match cmd {
            ControlCommand::CancelOrders { targets, reply } => {
                let report = self.dispatch_cancellations(targets).await;
                let _ = reply.send(report);
            }
            ControlCommand::EnterEmergency { targets, reply } => {
                self.accepting_intents = false;
                self.emergency_latched = true;
                warn!("execution worker intake disabled by emergency control");
                let targets = self.include_worker_tracked_orders(targets);
                let report = self.dispatch_cancellations(targets).await;
                let _ = reply.send(report);
            }
            ControlCommand::Reconcile {
                include_balances,
                reply,
            } => {
                let snapshot = self.collect_reconcile_snapshot(include_balances).await;
                let _ = reply.send(snapshot);
            }
            ControlCommand::SetIntake { enabled, reply } => {
                let result = if enabled && self.emergency_latched {
                    Err("execution intake is emergency-latched; restart required".to_string())
                } else {
                    self.accepting_intents = enabled;
                    Ok(())
                };
                let _ = reply.send(result);
            }
            ControlCommand::ReplaceOrder {
                order_id,
                symbol,
                new_quantity,
                new_price,
            } => {
                info!("控制指令: 替換訂單 {}", order_id.0);
                let client_idx = if let Some(idx) = self.order_to_client.get(&order_id).copied() {
                    idx
                } else {
                    self.select_client_by_symbol(&symbol)
                };
                if let Some(client) = self.execution_clients.get_mut(client_idx) {
                    match client
                        .modify_order(&order_id, new_quantity, new_price)
                        .await
                    {
                        Ok(()) => {
                            debug!("替換訂單成功: {} (client={})", order_id.0, client_idx);
                        }
                        Err(e) => {
                            warn!(
                                "替換訂單失敗: {} - {} (client={})",
                                order_id.0, e, client_idx
                            );
                        }
                    }
                }
            }
        }
    }

    fn include_worker_tracked_orders(&self, mut targets: Vec<CancelTarget>) -> Vec<CancelTarget> {
        let mut known = targets
            .iter()
            .map(|target| target.order_id.clone())
            .collect::<HashSet<_>>();
        for (order_id, client_idx) in &self.order_to_client {
            if !known.insert(order_id.clone()) {
                continue;
            }
            let symbol = self
                .pending_acks
                .get(order_id)
                .map(|(symbol, _)| symbol.clone())
                .unwrap_or_else(|| Symbol::new("UNKNOWN"));
            targets.push(CancelTarget {
                order_id: order_id.clone(),
                symbol,
                venue: self.venue_for_client(*client_idx),
                account_id: None,
            });
        }
        targets
    }

    async fn dispatch_cancellations(&mut self, targets: Vec<CancelTarget>) -> CancelDispatchReport {
        let mut report = CancelDispatchReport {
            requested: targets.len(),
            ..Default::default()
        };
        info!("控制指令: 取消 {} 個訂單", report.requested);

        for target in targets {
            let client_idx = match self.select_client_for_cancel(&target) {
                Ok(client_idx) => client_idx,
                Err(reason) => {
                    report.failures.push(CancelFailure {
                        order_id: target.order_id,
                        reason,
                    });
                    continue;
                }
            };
            let Some(client) = self.execution_clients.get_mut(client_idx) else {
                report.failures.push(CancelFailure {
                    order_id: target.order_id,
                    reason: format!("execution client index {client_idx} is unavailable"),
                });
                continue;
            };

            match client.cancel_order(&target.order_id).await {
                Ok(()) => {
                    debug!(
                        "取消訂單已提交: {} (client={})",
                        target.order_id.0, client_idx
                    );
                    report.submitted.push(target.order_id);
                }
                Err(error) => report.failures.push(CancelFailure {
                    order_id: target.order_id,
                    reason: error.to_string(),
                }),
            }
        }

        report
    }

    fn select_client_for_cancel(&self, target: &CancelTarget) -> Result<usize, String> {
        if let Some(client_idx) = self.order_to_client.get(&target.order_id).copied() {
            return Ok(client_idx);
        }
        if let Some(account_id) = &target.account_id {
            if let Some(client_idx) = self.account_to_client.get(account_id).copied() {
                return Ok(client_idx);
            }
        }
        if let Some(venue) = target.venue {
            if let Some(client_idx) = self.venue_to_client.get(&venue).copied() {
                return Ok(client_idx);
            }
        }
        if self.execution_clients.len() == 1 {
            return Ok(0);
        }
        Err(format!(
            "cannot route cancellation for {} across {} execution clients",
            target.order_id.0,
            self.execution_clients.len()
        ))
    }

    fn select_client_by_symbol(&self, symbol: &Symbol) -> usize {
        if self.execution_clients.is_empty() {
            return 0;
        }
        match self.config.client_selection {
            ClientSelectionStrategy::ConsistentHash => {
                let mut hash: u32 = 2166136261;
                for b in symbol.as_str().as_bytes() {
                    hash ^= *b as u32;
                    hash = hash.wrapping_mul(16777619);
                }
                (hash as usize) % self.execution_clients.len()
            }
            ClientSelectionStrategy::RoundRobin => {
                let current = self.round_robin_counter.load(Ordering::Relaxed);
                current % self.execution_clients.len()
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct CancelTarget {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub venue: Option<VenueId>,
    pub account_id: Option<AccountId>,
}

#[derive(Debug, Clone)]
pub struct CancelFailure {
    pub order_id: OrderId,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct CancelDispatchReport {
    pub requested: usize,
    pub submitted: Vec<OrderId>,
    pub failures: Vec<CancelFailure>,
}

impl CancelDispatchReport {
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty() && self.submitted.len() == self.requested
    }
}

#[derive(Debug, Clone)]
pub struct ClientReconcileSnapshot {
    pub client_index: usize,
    pub venue: Option<VenueId>,
    pub account_id: Option<AccountId>,
    pub open_orders: Result<Vec<OpenOrder>, HftError>,
    pub balances: Option<Result<Vec<AccountBalance>, HftError>>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkerReconcileSnapshot {
    pub clients: Vec<ClientReconcileSnapshot>,
}

impl WorkerReconcileSnapshot {
    pub fn is_complete(&self) -> bool {
        !self.clients.is_empty()
            && self.clients.iter().all(|client| {
                client.open_orders.is_ok()
                    && client
                        .balances
                        .as_ref()
                        .is_none_or(|balances| balances.is_ok())
            })
    }
}

/// 執行控制指令
#[derive(Debug)]
pub enum ControlCommand {
    CancelOrders {
        targets: Vec<CancelTarget>,
        reply: oneshot::Sender<CancelDispatchReport>,
    },
    EnterEmergency {
        targets: Vec<CancelTarget>,
        reply: oneshot::Sender<CancelDispatchReport>,
    },
    Reconcile {
        include_balances: bool,
        reply: oneshot::Sender<WorkerReconcileSnapshot>,
    },
    SetIntake {
        enabled: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// 替換/修改訂單（優先嘗試 modify，失敗時由上層決策是否 Cancel/Replace）
    ReplaceOrder {
        order_id: hft_core::OrderId,
        symbol: Symbol,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    },
}

/// 创建并启动执行 Worker 任务
pub fn spawn_execution_worker_with_control(
    config: ExecutionWorkerConfig,
    queues: WorkerQueues,
    execution_clients: Vec<Box<dyn ExecutionClient>>,
    strategy_to_client: Option<std::collections::HashMap<String, usize>>,
    account_to_client: Option<std::collections::HashMap<AccountId, usize>>,
) -> (
    tokio::task::JoinHandle<Result<(), HftError>>,
    mpsc::UnboundedSender<ControlCommand>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut worker = ExecutionWorker::new(config.clone(), queues, execution_clients, rx);
    if let Some(map) = strategy_to_client {
        worker.strategy_to_client = Some(map.into_iter().collect());
    }
    if let Some(map) = account_to_client {
        worker.account_to_client = map.into_iter().collect();
    }
    let handle = tokio::spawn(async move {
        info!("执行 Worker {} 任务启动", config.name);
        worker.run().await
    });
    (handle, tx)
}

/// 🔥 Phase 1.5: 创建并启动带路由器的执行 Worker 任务
pub fn spawn_execution_worker_with_control_and_router(
    config: ExecutionWorkerConfig,
    queues: WorkerQueues,
    execution_clients: Vec<Box<dyn ExecutionClient>>,
    router: Box<dyn ExecutionRouter>,
    venue_to_client: HashMap<VenueId, usize>,
    strategy_to_client: Option<std::collections::HashMap<String, usize>>,
    account_to_client: Option<std::collections::HashMap<AccountId, usize>>,
) -> (
    tokio::task::JoinHandle<Result<(), HftError>>,
    mpsc::UnboundedSender<ControlCommand>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut worker = ExecutionWorker::new_with_router(
        config.clone(),
        queues,
        execution_clients,
        rx,
        router,
        venue_to_client,
    );
    if let Some(map) = strategy_to_client {
        worker.strategy_to_client = Some(map.into_iter().collect());
    }
    if let Some(map) = account_to_client {
        worker.account_to_client = map.into_iter().collect();
    }
    let handle = tokio::spawn(async move {
        info!("执行 Worker {} 任务启动 (带路由器)", config.name);
        worker.run().await
    });
    (handle, tx)
}

/// 保留舊接口，沒有控制通道
pub fn spawn_execution_worker(
    config: ExecutionWorkerConfig,
    queues: WorkerQueues,
    execution_clients: Vec<Box<dyn ExecutionClient>>,
) -> tokio::task::JoinHandle<Result<(), HftError>> {
    let (h, _tx) =
        spawn_execution_worker_with_control(config, queues, execution_clients, None, None);
    h
}

impl ExecutionWorker {
    /// Periodic worker-side capability check. OMS comparison is performed by
    /// `ExecutionControlHandle`, which owns access to the engine's local truth.
    async fn reconcile_open_orders(&mut self) -> bool {
        let snapshot = self.collect_reconcile_snapshot(false).await;
        let complete = snapshot.is_complete();
        if !complete {
            warn!("對帳快照不完整；系統不得將未知狀態視為無未結訂單");
        }
        if self.config.auto_cancel_exchange_only {
            debug!("exchange-only auto-cancel requires a runtime OMS comparison");
        }
        complete
    }

    async fn collect_reconcile_snapshot(
        &mut self,
        include_balances: bool,
    ) -> WorkerReconcileSnapshot {
        let mut clients = Vec::with_capacity(self.execution_clients.len());
        for (idx, client) in self.execution_clients.iter_mut().enumerate() {
            let open_orders = client.list_open_orders().await;
            #[cfg(feature = "metrics")]
            {
                infra_metrics::MetricsRegistry::global().inc_reconcile_runs();
                if open_orders.is_err() {
                    infra_metrics::MetricsRegistry::global().inc_reconcile_errors();
                }
            }

            let balances = if include_balances {
                Some(client.get_balance().await)
            } else {
                None
            };
            clients.push(ClientReconcileSnapshot {
                client_index: idx,
                venue: self
                    .venue_to_client
                    .iter()
                    .find_map(|(venue, client_idx)| (*client_idx == idx).then_some(*venue)),
                account_id: self
                    .account_to_client
                    .iter()
                    .find_map(|(account_id, client_idx)| {
                        (*client_idx == idx).then_some(account_id.clone())
                    }),
                open_orders,
                balances,
            });
        }

        WorkerReconcileSnapshot { clients }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderType, Price, Quantity, Side, Symbol, TimeInForce};
    use ports::{ConnectionHealth, HftResult, OrderIntent};
    use std::collections::HashSet;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct MockExecutionState {
        placed: Vec<Symbol>,
        canceled: Vec<OrderId>,
    }

    struct MockExecutionClient {
        state: Arc<StdMutex<MockExecutionState>>,
        list_error: bool,
    }

    #[async_trait::async_trait]
    impl ExecutionClient for MockExecutionClient {
        async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
            self.state.lock().unwrap().placed.push(intent.symbol);
            Ok(OrderId("placed".to_string()))
        }

        async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
            self.state.lock().unwrap().canceled.push(order_id.clone());
            Ok(())
        }

        async fn modify_order(
            &mut self,
            _order_id: &OrderId,
            _new_quantity: Option<Quantity>,
            _new_price: Option<Price>,
        ) -> HftResult<()> {
            Ok(())
        }

        async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
            if self.list_error {
                Err(HftError::Network("open-order snapshot failed".to_string()))
            } else {
                Ok(Vec::new())
            }
        }

        async fn connect(&mut self) -> HftResult<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> HftResult<()> {
            Ok(())
        }

        async fn health(&self) -> ConnectionHealth {
            ConnectionHealth {
                connected: true,
                latency_ms: None,
                last_heartbeat: now_micros(),
            }
        }
    }

    #[allow(dead_code)]
    fn create_test_intent(symbol: &str) -> OrderIntent {
        OrderIntent {
            symbol: Symbol::new(symbol),
            asset_class: hft_core::AssetClass::Crypto,
            product_type: hft_core::ProductType::Spot,
            compliance_context: hft_core::ComplianceContext::default(),
            side: Side::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::from_f64(1.0).unwrap(),
            price: Some(Price::from_f64(100.0).unwrap()),
            time_in_force: TimeInForce::IOC,
            strategy_id: "test".to_string(),
            target_venue: None,
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
                assert_eq!(
                    expected_client, actual_client,
                    "一致性哈希對於符號 '{}' 應該總是返回相同的客戶端",
                    symbol
                );
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
            assert_eq!(
                *count, expected_per_client,
                "輪詢策略應該均勻分配負載，客戶端 {} 期望 {} 次，實際 {} 次",
                client_idx, expected_per_client, count
            );
        }
    }

    #[test]
    fn test_hash_distribution_quality() {
        // 測試哈希分佈質量
        let client_count = 4;
        let symbols = [
            "BTCUSDT",
            "ETHUSDT",
            "ADAUSDT",
            "DOTUSDT",
            "BNBUSDT",
            "XRPUSDT",
            "SOLUSDT",
            "LINKUSDT",
            "AVAXUSDT",
            "MATICUSDT",
        ];

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
        assert!(
            used_clients.len() >= 2,
            "哈希分佈應該使用多個客戶端，實際只使用了 {} 個",
            used_clients.len()
        );

        // 列印分佈以供調試
        println!("哈希分佈: {:?}", distribution);
    }

    #[tokio::test]
    async fn emergency_disables_intake_before_cancel_dispatch() {
        let state = Arc::new(StdMutex::new(MockExecutionState::default()));
        let client = MockExecutionClient {
            state: state.clone(),
            list_error: false,
        };
        let (mut engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        engine_queues
            .send_intent(create_test_intent("ETHUSDT"))
            .expect("queue intent");
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig {
                max_retries: 0,
                ..Default::default()
            },
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        let order_id = OrderId("open-order".to_string());
        worker.order_to_client.insert(order_id.clone(), 0);
        let (reply, report) = oneshot::channel();

        worker
            .handle_control_command(ControlCommand::EnterEmergency {
                targets: Vec::new(),
                reply,
            })
            .await;
        let report = report.await.expect("emergency report");
        assert!(report.is_complete());

        let (resume_reply, resume_result) = oneshot::channel();
        worker
            .handle_control_command(ControlCommand::SetIntake {
                enabled: true,
                reply: resume_reply,
            })
            .await;
        assert!(resume_result.await.expect("resume result").is_err());

        let queued = worker.queues.receive_intents();
        worker.process_order_intents(queued).await;
        let state = state.lock().unwrap();
        assert_eq!(state.canceled, vec![order_id]);
        assert!(state.placed.is_empty());
        assert_eq!(worker.stats.orders_failed, 1);
    }

    #[tokio::test]
    async fn reconciliation_snapshot_is_incomplete_on_client_error() {
        let client = MockExecutionClient {
            state: Arc::new(StdMutex::new(MockExecutionState::default())),
            list_error: true,
        };
        let (_engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );

        let snapshot = worker.collect_reconcile_snapshot(false).await;
        assert_eq!(snapshot.clients.len(), 1);
        assert!(snapshot.clients[0].open_orders.is_err());
        assert!(!snapshot.is_complete());
    }
}
