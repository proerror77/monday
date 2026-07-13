//! 执行 Worker - 独立任务处理订单意图，避免引擎锁定
//!
//! 架构:
//! - 从意图队列批量接收 OrderIntent
//! - 调用 ExecutionClient (带 await)
//! - 将 ExecutionEvent 发送到回报队列
//! - 所有网络 await 不会阻塞引擎主循环

use crate::execution_queues::WorkerQueues;
use crate::latency_monitor::{LatencyMonitor, LatencyMonitorConfig};
use futures::stream::SelectAll;
use futures::{FutureExt, StreamExt};
use hft_core::{now_micros, AccountId, HftError, LatencyStage, OrderId, Price, Quantity};
use hft_core::{Symbol, VenueId};
use ports::{
    AccountBalance, BoxStream, ExecutionClient, ExecutionEvent, ExecutionRouter, OpenOrder,
    OrderIntent, OrderIntentEnvelope,
};
use rustc_hash::FxHashMap;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
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
    /// 最近的執行延遲（用於實時監控）
    pub recent_execution_latency_micros: Option<u64>,
}

#[derive(Debug, Clone)]
struct TrackedOrder {
    symbol: Symbol,
    strategy_id: String,
    venue: Option<VenueId>,
    account_id: Option<AccountId>,
    remaining_quantity: Quantity,
    processed_fill_ids: HashSet<String>,
}

#[derive(Debug, Clone)]
struct PendingAck {
    symbol: Symbol,
    submitted_at: Instant,
    cancel_sent: bool,
}

/// 执行 Worker - 在独立 Tokio 任务中运行
pub struct ExecutionWorker {
    config: ExecutionWorkerConfig,
    queues: WorkerQueues,
    execution_clients: Vec<Box<dyn ExecutionClient>>,
    execution_streams: SelectAll<BoxStream<ExecutionEvent>>,
    stats: ExecutionWorkerStats,
    /// 订单 ID 到客户端索引的映射
    order_to_client: FxHashMap<OrderId, usize>,
    tracked_orders: FxHashMap<OrderId, TrackedOrder>,
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
    /// 等待 Ack 的訂單。超時後保持追蹤，直到收到交易所終態。
    pending_acks: FxHashMap<OrderId, PendingAck>,
    /// 上次對帳時間
    last_reconcile: Instant,
    /// 策略到客戶端索引的映射（同交易所多帳戶路由）
    strategy_to_client: Option<rustc_hash::FxHashMap<String, usize>>,
    /// 帳戶到客戶端索引的映射（恢復訂單與控制面路由）
    account_to_client: FxHashMap<AccountId, usize>,
    /// Emergency is sticky for the worker lifetime; restart is required to re-arm execution.
    accepting_intents: bool,
    emergency_latched: bool,
    intents_buf: Vec<OrderIntentEnvelope>,
    execution_events_buf: Vec<ExecutionEvent>,
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
        for (order_id, pending) in &mut self.pending_acks {
            if !pending.cancel_sent && now.duration_since(pending.submitted_at) > timeout {
                pending.cancel_sent = true;
                timed_out.push((order_id.clone(), pending.symbol.clone()));
            }
        }

        if !timed_out.is_empty() {
            self.accepting_intents = false;
            self.emergency_latched = true;
        }

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
                    if let Some(pending) = self.pending_acks.get_mut(order_id) {
                        pending.submitted_at = Instant::now();
                        pending.cancel_sent = false;
                    }
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
        let batch_size = config.batch_size;

        Self {
            config,
            queues,
            execution_clients,
            execution_streams: SelectAll::new(),
            stats: ExecutionWorkerStats::default(),
            order_to_client: FxHashMap::default(),
            tracked_orders: FxHashMap::default(),
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
            intents_buf: Vec::with_capacity(batch_size),
            execution_events_buf: Vec::with_capacity(batch_size),
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
        let batch_size = config.batch_size;

        Self {
            config,
            queues,
            execution_clients,
            execution_streams: SelectAll::new(),
            stats: ExecutionWorkerStats::default(),
            order_to_client: FxHashMap::default(),
            tracked_orders: FxHashMap::default(),
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
            intents_buf: Vec::with_capacity(batch_size),
            execution_events_buf: Vec::with_capacity(batch_size),
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

            // 1. Prioritize private execution events so a queued disconnect is observed before
            // accepting another order intent.
            let events_received = self.poll_execution_events().await;
            if events_received > 0 {
                had_activity = true;
            }

            // 2. 处理意图队列中的新订单
            let mut intents = std::mem::take(&mut self.intents_buf);
            intents.clear();
            self.queues.receive_envelopes_into(&mut intents);
            if !intents.is_empty() {
                self.process_order_intents(&mut intents).await;
                had_activity = true;
            }
            intents.clear();
            self.intents_buf = intents;

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
                let intent_notify = self.queues.intent_notify();
                let has_execution_stream = !self.execution_streams.is_empty();
                let maintenance_wait = Duration::from_millis(self.config.idle_sleep_ms.max(10));
                tokio::select! {
                    biased;
                    command = self.control_rx.recv() => {
                        if let Some(command) = command {
                            self.handle_control_command(command).await;
                            last_activity = Instant::now();
                        }
                    }
                    item = self.execution_streams.next(), if has_execution_stream => {
                        self.handle_execution_stream_item(item).await;
                        last_activity = Instant::now();
                    }
                    _ = intent_notify.notified() => {
                        last_activity = Instant::now();
                    }
                    _ = sleep(maintenance_wait) => {}
                }
            }

            // 5. 周期性状态日志
            if last_activity.elapsed().as_secs() > 30 {
                self.latency_monitor.report_if_due();
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

    /// Process lifecycle-qualified intents. A placement is submitted once; ambiguous failures latch
    /// intake until reconciliation/restart instead of risking a duplicate live order.
    async fn process_order_intents(&mut self, intents: &mut Vec<OrderIntentEnvelope>) {
        self.stats.intents_processed += intents.len() as u64;

        if self.execution_clients.is_empty() {
            for envelope in intents.drain(..) {
                let reject_event = ExecutionEvent::OrderReject {
                    order_id: OrderId(envelope.client_order_id),
                    reason: "没有可用的执行客户端".to_string(),
                    timestamp: now_micros(),
                };
                self.queues.send_event_reliable(reject_event).await;
                self.stats.orders_failed += 1;
            }
            return;
        }

        for envelope in intents.drain(..) {
            while let Ok(command) = self.control_rx.try_recv() {
                self.handle_control_command(command).await;
            }
            if let Err(reason) = envelope.validate_pre_execution(now_micros(), None) {
                let reject_event = ExecutionEvent::OrderReject {
                    order_id: OrderId(envelope.client_order_id.clone()),
                    reason: format!("execution lifecycle gate rejected intent: {reason:?}"),
                    timestamp: now_micros(),
                };
                self.queues.send_event_reliable(reject_event).await;
                self.stats.orders_failed += 1;
                continue;
            }
            let intent = &envelope.intent;
            if !self.accepting_intents {
                self.reject_for_disabled_intake(&envelope).await;
                continue;
            }
            let execution_start = now_micros();

            let client_idx = match self.select_execution_client(intent) {
                Ok(idx) => idx,
                Err(reason) => {
                    warn!("客戶端選擇失敗: {}", reason);
                    let reject_event = ExecutionEvent::OrderReject {
                        order_id: OrderId(envelope.client_order_id.clone()),
                        reason: format!("路由失敗: {}", reason),
                        timestamp: now_micros(),
                    };
                    self.queues.send_event_reliable(reject_event).await;
                    self.stats.orders_failed += 1;
                    continue;
                }
            };

            match self.execution_clients[client_idx]
                .place_order_envelope(&envelope)
                .await
            {
                Ok(order_id) => {
                    let submitted_at = now_micros();
                    let execution_latency = submitted_at.saturating_sub(execution_start);
                    self.latency_monitor
                        .record_latency(LatencyStage::Submission, execution_latency);
                    self.latency_monitor.record_latency(
                        LatencyStage::EndToEnd,
                        submitted_at.saturating_sub(envelope.lifecycle.created_ts),
                    );
                    self.stats.recent_execution_latency_micros = Some(execution_latency);
                    #[cfg(feature = "metrics")]
                    infra_metrics::MetricsRegistry::global()
                        .record_submission_latency(execution_latency as f64);

                    self.stats.orders_placed += 1;
                    self.order_to_client.insert(order_id.clone(), client_idx);

                    let venue_for_client = intent
                        .target_venue
                        .or_else(|| self.venue_for_client(client_idx));
                    let account_id =
                        self.account_to_client
                            .iter()
                            .find_map(|(account_id, index)| {
                                (*index == client_idx).then_some(account_id.clone())
                            });
                    self.tracked_orders.insert(
                        order_id.clone(),
                        TrackedOrder {
                            symbol: intent.symbol.clone(),
                            strategy_id: intent.strategy_id.clone(),
                            venue: venue_for_client,
                            account_id,
                            remaining_quantity: intent.quantity,
                            processed_fill_ids: HashSet::new(),
                        },
                    );

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
                    } = envelope.intent;

                    let symbol_for_ack = symbol.clone();
                    let client_order_id = envelope.client_order_id.clone();

                    let new_event = ExecutionEvent::OrderNew {
                        order_id: order_id.clone(),
                        client_order_id: Some(client_order_id),
                        symbol,
                        side,
                        quantity,
                        requested_price: price,
                        timestamp: now_micros(),
                        venue: venue_for_client,
                        strategy_id,
                    };
                    self.queues.send_event_reliable(new_event).await;

                    debug!("訂單執行成功，延遲: {}μs", execution_latency);

                    // 標記等待 Ack
                    self.pending_acks.insert(
                        order_id.clone(),
                        PendingAck {
                            symbol: symbol_for_ack,
                            submitted_at: Instant::now(),
                            cancel_sent: false,
                        },
                    );
                }
                Err(e) => {
                    let execution_latency = now_micros().saturating_sub(execution_start);
                    self.stats.recent_execution_latency_micros = Some(execution_latency);

                    self.stats.orders_failed += 1;
                    let outcome_unknown = Self::submission_outcome_may_be_unknown(&e);
                    if outcome_unknown {
                        self.accepting_intents = false;
                        self.emergency_latched = true;

                        // The venue may already own this order. Keep a provisional local record
                        // under the stable client id so private reports, reconciliation, and
                        // emergency cancellation cannot lose it.
                        let order_id = OrderId(envelope.client_order_id.clone());
                        let intent = &envelope.intent;
                        let venue = intent
                            .target_venue
                            .or_else(|| self.venue_for_client(client_idx));
                        let account_id =
                            self.account_to_client
                                .iter()
                                .find_map(|(account_id, index)| {
                                    (*index == client_idx).then_some(account_id.clone())
                                });
                        self.order_to_client.insert(order_id.clone(), client_idx);
                        self.tracked_orders.insert(
                            order_id.clone(),
                            TrackedOrder {
                                symbol: intent.symbol.clone(),
                                strategy_id: intent.strategy_id.clone(),
                                venue,
                                account_id,
                                remaining_quantity: intent.quantity,
                                processed_fill_ids: HashSet::new(),
                            },
                        );
                        self.pending_acks.insert(
                            order_id.clone(),
                            PendingAck {
                                symbol: intent.symbol.clone(),
                                submitted_at: Instant::now(),
                                cancel_sent: false,
                            },
                        );
                        self.queues
                            .send_event_reliable(ExecutionEvent::OrderNew {
                                order_id,
                                client_order_id: Some(envelope.client_order_id.clone()),
                                symbol: intent.symbol.clone(),
                                side: intent.side,
                                quantity: intent.quantity,
                                requested_price: intent.price,
                                timestamp: now_micros(),
                                venue,
                                strategy_id: intent.strategy_id.clone(),
                            })
                            .await;
                    }
                    warn!(
                        client_order_id = %envelope.client_order_id,
                        outcome_unknown,
                        "下单失败: {}",
                        e
                    );
                    if !outcome_unknown {
                        let reject_event = ExecutionEvent::OrderReject {
                            order_id: OrderId(envelope.client_order_id.clone()),
                            reason: format!("Worker 下单失败: {}", e),
                            timestamp: now_micros(),
                        };
                        self.queues.send_event_reliable(reject_event).await;
                    }
                }
            }
        }
    }

    async fn reject_for_disabled_intake(&mut self, envelope: &OrderIntentEnvelope) {
        let reject_event = ExecutionEvent::OrderReject {
            order_id: OrderId(envelope.client_order_id.clone()),
            reason: format!(
                "execution intake disabled by emergency control for {}",
                envelope.intent.symbol.as_str()
            ),
            timestamp: now_micros(),
        };
        self.queues.send_event_reliable(reject_event).await;
        self.stats.orders_failed += 1;
    }

    fn submission_outcome_may_be_unknown(error: &HftError) -> bool {
        !matches!(
            error,
            HftError::InvalidOrder(_)
                | HftError::InsufficientBalance(_)
                | HftError::Risk(_)
                | HftError::Config(_)
                | HftError::Authentication(_)
                | HftError::RateLimit(_)
                | HftError::Exchange(_)
                | HftError::OrderNotFound(_)
                | HftError::Parse(_)
                | HftError::Serialization(_)
        )
    }

    /// 轮询执行回报流
    async fn poll_execution_events(&mut self) -> u32 {
        let mut events_count = 0;
        let mut execution_events = std::mem::take(&mut self.execution_events_buf);
        execution_events.clear();
        let mut stream_failed = false;

        while execution_events.len() < self.config.batch_size {
            match self.execution_streams.next().now_or_never() {
                Some(Some(Ok(event))) => execution_events.push(event),
                Some(Some(Err(e))) => {
                    warn!("执行回报流错误: {}", e);
                    stream_failed = true;
                    break;
                }
                Some(None) => {
                    if !self.execution_streams.is_empty() {
                        continue;
                    }
                    debug!("所有执行回报流已结束");
                    stream_failed = true;
                    break;
                }
                None => break,
            }
        }

        if stream_failed {
            self.accepting_intents = false;
            self.emergency_latched = true;
            execution_events.push(ExecutionEvent::ConnectionStatus {
                connected: false,
                timestamp: now_micros(),
            });
        }

        for event in execution_events.drain(..) {
            self.update_execution_tracking(&event);
            self.queues.send_event_reliable(event).await;
            self.stats.events_sent += 1;
            events_count += 1;
        }
        self.execution_events_buf = execution_events;

        events_count
    }

    async fn handle_execution_stream_item(
        &mut self,
        item: Option<Result<ExecutionEvent, HftError>>,
    ) {
        match item {
            Some(Ok(event)) => {
                self.update_execution_tracking(&event);
                self.queues.send_event_reliable(event).await;
                self.stats.events_sent += 1;
            }
            Some(Err(error)) => {
                warn!("执行回报流错误: {}", error);
                self.latch_on_execution_stream_failure().await;
            }
            None => {
                warn!("所有执行回报流已结束");
                self.latch_on_execution_stream_failure().await;
            }
        }
    }

    fn update_execution_tracking(&mut self, event: &ExecutionEvent) {
        match event {
            ExecutionEvent::ConnectionStatus {
                connected: false, ..
            } => {
                self.accepting_intents = false;
                self.emergency_latched = true;
            }
            ExecutionEvent::OrderAck { order_id, .. } => {
                self.pending_acks.remove(order_id);
            }
            ExecutionEvent::Fill {
                order_id,
                quantity,
                fill_id,
                ..
            } => {
                let mut terminal = false;
                if let Some(order) = self.tracked_orders.get_mut(order_id) {
                    if !fill_id.is_empty() && !order.processed_fill_ids.insert(fill_id.clone()) {
                        return;
                    }
                    if quantity.0 >= order.remaining_quantity.0 {
                        if quantity.0 > order.remaining_quantity.0 {
                            warn!(
                                order_id = %order_id.0,
                                fill_quantity = %quantity.0,
                                remaining_quantity = %order.remaining_quantity.0,
                                "fill exceeds locally tracked remaining quantity; execution intake latched"
                            );
                            self.accepting_intents = false;
                            self.emergency_latched = true;
                        }
                        terminal = true;
                    } else {
                        order.remaining_quantity =
                            Quantity(order.remaining_quantity.0 - quantity.0);
                    }
                }
                if terminal {
                    self.pending_acks.remove(order_id);
                    self.order_to_client.remove(order_id);
                    self.tracked_orders.remove(order_id);
                }
            }
            ExecutionEvent::OrderCanceled { order_id, .. }
            | ExecutionEvent::OrderReject { order_id, .. }
            | ExecutionEvent::OrderCompleted { order_id, .. } => {
                self.pending_acks.remove(order_id);
                self.order_to_client.remove(order_id);
                self.tracked_orders.remove(order_id);
            }
            _ => {}
        }
    }

    async fn latch_on_execution_stream_failure(&mut self) {
        self.accepting_intents = false;
        self.emergency_latched = true;
        self.queues
            .send_event_reliable(ExecutionEvent::ConnectionStatus {
                connected: false,
                timestamp: now_micros(),
            })
            .await;
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
            ControlCommand::CancelOrders {
                targets,
                scope,
                reply,
            } => {
                let targets = self.include_worker_tracked_orders(targets, &scope);
                let report = self.dispatch_cancellations(targets).await;
                let _ = reply.send(report);
            }
            ControlCommand::EnterEmergency { targets, reply } => {
                self.accepting_intents = false;
                self.emergency_latched = true;
                warn!("execution worker intake disabled by emergency control");
                let targets = self.include_worker_tracked_orders(targets, &CancelScope::All);
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

    fn include_worker_tracked_orders(
        &self,
        mut targets: Vec<CancelTarget>,
        scope: &CancelScope,
    ) -> Vec<CancelTarget> {
        let mut known = targets
            .iter()
            .map(|target| target.order_id.clone())
            .collect::<HashSet<_>>();
        for (order_id, metadata) in &self.tracked_orders {
            if !scope.matches(metadata) {
                continue;
            }
            if !known.insert(order_id.clone()) {
                continue;
            }
            targets.push(CancelTarget {
                order_id: order_id.clone(),
                symbol: metadata.symbol.clone(),
                venue: metadata.venue,
                account_id: metadata.account_id.clone(),
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
pub enum CancelScope {
    Explicit,
    All,
    Filter {
        symbol: Option<Symbol>,
        venue: Option<VenueId>,
    },
    Strategy(String),
}

impl CancelScope {
    fn matches(&self, order: &TrackedOrder) -> bool {
        match self {
            Self::Explicit => false,
            Self::All => true,
            Self::Filter { symbol, venue } => {
                symbol.as_ref().is_none_or(|value| &order.symbol == value)
                    && venue.is_none_or(|value| order.venue == Some(value))
            }
            Self::Strategy(strategy_id) => order.strategy_id == *strategy_id,
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
        scope: CancelScope,
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
    venue_to_client: HashMap<VenueId, usize>,
    strategy_to_client: Option<std::collections::HashMap<String, usize>>,
    account_to_client: Option<std::collections::HashMap<AccountId, usize>>,
) -> (
    tokio::task::JoinHandle<Result<(), HftError>>,
    mpsc::UnboundedSender<ControlCommand>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut worker = ExecutionWorker::new(config.clone(), queues, execution_clients, rx);
    worker.venue_to_client = venue_to_client;
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
    let (h, _tx) = spawn_execution_worker_with_control(
        config,
        queues,
        execution_clients,
        HashMap::new(),
        None,
        None,
    );
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

    #[test]
    fn explicit_exchange_rejections_are_known_but_transport_failures_are_ambiguous() {
        assert!(!ExecutionWorker::submission_outcome_may_be_unknown(
            &HftError::RateLimit("429".to_string())
        ));
        assert!(!ExecutionWorker::submission_outcome_may_be_unknown(
            &HftError::Exchange("invalid quantity".to_string())
        ));
        assert!(ExecutionWorker::submission_outcome_may_be_unknown(
            &HftError::Network("connection reset".to_string())
        ));
        assert!(ExecutionWorker::submission_outcome_may_be_unknown(
            &HftError::Execution("accepted response was malformed".to_string())
        ));
    }

    #[derive(Default)]
    struct MockExecutionState {
        placed: Vec<Symbol>,
        canceled: Vec<OrderId>,
    }

    struct MockExecutionClient {
        state: Arc<StdMutex<MockExecutionState>>,
        place_error: bool,
        list_error: bool,
        cancel_error: bool,
    }

    #[async_trait::async_trait]
    impl ExecutionClient for MockExecutionClient {
        async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
            self.state.lock().unwrap().placed.push(intent.symbol);
            if self.place_error {
                Err(HftError::Network("submission outcome unknown".to_string()))
            } else {
                Ok(OrderId("placed".to_string()))
            }
        }

        async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
            self.state.lock().unwrap().canceled.push(order_id.clone());
            if self.cancel_error {
                Err(HftError::Network("cancel outcome unknown".to_string()))
            } else {
                Ok(())
            }
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
            place_error: false,
            list_error: false,
            cancel_error: false,
        };
        let (mut engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        engine_queues
            .send_intent(create_test_intent("ETHUSDT"))
            .expect("queue intent");
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        let order_id = OrderId("open-order".to_string());
        worker.order_to_client.insert(order_id.clone(), 0);
        worker.tracked_orders.insert(
            order_id.clone(),
            TrackedOrder {
                symbol: Symbol::new("ETHUSDT"),
                strategy_id: "test_strategy".to_string(),
                venue: Some(VenueId::MOCK),
                account_id: None,
                remaining_quantity: Quantity::from_f64(1.0).unwrap(),
                processed_fill_ids: HashSet::new(),
            },
        );
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

        let mut queued = worker.queues.receive_envelopes();
        worker.process_order_intents(&mut queued).await;
        let state = state.lock().unwrap();
        assert_eq!(state.canceled, vec![order_id]);
        assert!(state.placed.is_empty());
        assert_eq!(worker.stats.orders_failed, 1);
    }

    #[test]
    fn regular_cancel_scopes_include_only_matching_worker_tracked_orders() {
        let client = MockExecutionClient {
            state: Arc::new(StdMutex::new(MockExecutionState::default())),
            place_error: false,
            list_error: false,
            cancel_error: false,
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
        worker.tracked_orders.insert(
            OrderId("alpha-btc".to_string()),
            TrackedOrder {
                symbol: Symbol::new("BTCUSDT"),
                strategy_id: "alpha".to_string(),
                venue: Some(VenueId::MOCK),
                account_id: None,
                remaining_quantity: Quantity::from_f64(1.0).unwrap(),
                processed_fill_ids: HashSet::new(),
            },
        );
        worker.tracked_orders.insert(
            OrderId("beta-eth".to_string()),
            TrackedOrder {
                symbol: Symbol::new("ETHUSDT"),
                strategy_id: "beta".to_string(),
                venue: Some(VenueId::MOCK),
                account_id: None,
                remaining_quantity: Quantity::from_f64(1.0).unwrap(),
                processed_fill_ids: HashSet::new(),
            },
        );

        let all = worker.include_worker_tracked_orders(Vec::new(), &CancelScope::All);
        assert_eq!(all.len(), 2);

        let alpha = worker
            .include_worker_tracked_orders(Vec::new(), &CancelScope::Strategy("alpha".to_string()));
        assert_eq!(alpha.len(), 1);
        assert_eq!(alpha[0].order_id, OrderId("alpha-btc".to_string()));

        let eth = worker.include_worker_tracked_orders(
            Vec::new(),
            &CancelScope::Filter {
                symbol: Some(Symbol::new("ETHUSDT")),
                venue: Some(VenueId::MOCK),
            },
        );
        assert_eq!(eth.len(), 1);
        assert_eq!(eth[0].order_id, OrderId("beta-eth".to_string()));

        let explicit = worker.include_worker_tracked_orders(Vec::new(), &CancelScope::Explicit);
        assert!(explicit.is_empty());
    }

    #[tokio::test]
    async fn reconciliation_snapshot_is_incomplete_on_client_error() {
        let client = MockExecutionClient {
            state: Arc::new(StdMutex::new(MockExecutionState::default())),
            place_error: false,
            list_error: true,
            cancel_error: false,
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

    #[tokio::test]
    async fn ambiguous_submission_keeps_a_provisional_order_for_reconciliation() {
        let client = MockExecutionClient {
            state: Arc::new(StdMutex::new(MockExecutionState::default())),
            place_error: true,
            list_error: false,
            cancel_error: false,
        };
        let (mut engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        engine_queues
            .send_intent(create_test_intent("BTCUSDT"))
            .expect("queue intent");
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        let mut queued = worker.queues.receive_envelopes();
        let client_order_id = queued[0].client_order_id.clone();

        worker.process_order_intents(&mut queued).await;

        let provisional_id = OrderId(client_order_id.clone());
        assert!(!worker.accepting_intents);
        assert!(worker.emergency_latched);
        assert!(worker.tracked_orders.contains_key(&provisional_id));
        assert!(worker.pending_acks.contains_key(&provisional_id));
        let mut events = Vec::new();
        engine_queues.receive_events_into(&mut events);
        assert!(matches!(
            events.as_slice(),
            [ExecutionEvent::OrderNew {
                order_id,
                client_order_id: Some(event_client_id),
                ..
            }] if order_id == &provisional_id && event_client_id == &client_order_id
        ));
    }

    #[tokio::test]
    async fn ack_timeout_latches_intake_and_keeps_the_order_pending_when_cancel_fails() {
        let state = Arc::new(StdMutex::new(MockExecutionState::default()));
        let client = MockExecutionClient {
            state: state.clone(),
            place_error: false,
            list_error: false,
            cancel_error: true,
        };
        let (_engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig {
                ack_timeout_ms: 1,
                ..ExecutionWorkerConfig::default()
            },
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        let order_id = OrderId("unknown-order".to_string());
        worker.order_to_client.insert(order_id.clone(), 0);
        worker.pending_acks.insert(
            order_id.clone(),
            PendingAck {
                symbol: Symbol::new("BTCUSDT"),
                submitted_at: Instant::now() - Duration::from_millis(10),
                cancel_sent: false,
            },
        );

        assert_eq!(worker.check_ack_timeouts().await, 1);
        assert!(!worker.accepting_intents);
        assert!(worker.emergency_latched);
        assert!(worker.pending_acks.contains_key(&order_id));
        assert_eq!(state.lock().unwrap().canceled, vec![order_id]);
    }

    #[test]
    fn terminal_fill_removes_worker_order_tracking() {
        let client = MockExecutionClient {
            state: Arc::new(StdMutex::new(MockExecutionState::default())),
            place_error: false,
            list_error: false,
            cancel_error: false,
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
        let order_id = OrderId("filled-order".to_string());
        worker.order_to_client.insert(order_id.clone(), 0);
        worker.tracked_orders.insert(
            order_id.clone(),
            TrackedOrder {
                symbol: Symbol::new("BTCUSDT"),
                strategy_id: "test".to_string(),
                venue: Some(VenueId::MOCK),
                account_id: None,
                remaining_quantity: Quantity::from_f64(1.0).unwrap(),
                processed_fill_ids: HashSet::new(),
            },
        );

        worker.update_execution_tracking(&ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(1.0).unwrap(),
            timestamp: now_micros(),
            fill_id: "fill-1".to_string(),
        });

        assert!(!worker.tracked_orders.contains_key(&order_id));
        assert!(!worker.order_to_client.contains_key(&order_id));
    }
}
