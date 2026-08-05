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
use hft_core::{
    now_micros, AccountId, HftError, LatencyStage, OrderId, Price, ProductType, Quantity,
};
use hft_core::{Symbol, VenueId};
use ports::{
    AccountBalance, AccountExecutionAdmission, AccountExecutionEnvironment, AccountReadbackState,
    AssetInventoryCapability, AssetInventoryRecord, ExecutionClient, ExecutionEvent,
    ExecutionRouter, OpenOrder, OrderIntent, OrderIntentEnvelope,
};
use rustc_hash::FxHashMap;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, Duration, Instant};
use tracing::{debug, error, info, warn};

enum IndexedExecutionStreamItem {
    Event {
        client_idx: usize,
        result: Result<ExecutionEvent, HftError>,
    },
    Completed {
        client_idx: usize,
    },
}

type IndexedExecutionStream =
    Pin<Box<dyn futures::Stream<Item = IndexedExecutionStreamItem> + Send>>;

const STREAM_RECOVERY_RETRY_INTERVAL: Duration = Duration::from_secs(1);

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
    /// Most recent proven userspace write-return to synchronous-response span.
    pub recent_execution_latency_micros: Option<u64>,
}

#[derive(Debug, Clone)]
struct TrackedOrder {
    symbol: Symbol,
    strategy_id: String,
    venue: Option<VenueId>,
    account_id: Option<AccountId>,
    side: hft_core::Side,
    limit_price: Option<Price>,
    remaining_quantity: Quantity,
    processed_fill_ids: HashSet<String>,
}

#[derive(Debug, Clone)]
struct PendingAck {
    symbol: Symbol,
    submitted_at: Instant,
    cancel_sent: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ExecutionSpanSnapshot {
    intent_to_risk_us: Option<u64>,
    risk_to_write_us: Option<u64>,
    userspace_write_us: Option<u64>,
    write_to_response_us: Option<u64>,
    write_to_private_ack_us: Option<u64>,
    write_to_private_report_us: Option<u64>,
    intent_to_private_report_us: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
struct ExecutionTimeline {
    client_idx: usize,
    intent_emitted_mono_us: Option<u64>,
    risk_completed_mono_us: Option<u64>,
    write_started_mono_us: Option<u64>,
    write_returned_mono_us: Option<u64>,
    response_received_mono_us: Option<u64>,
    private_ack_received_mono_us: Option<u64>,
    private_report_received_mono_us: Option<u64>,
}

impl ExecutionTimeline {
    fn from_lifecycle(lifecycle: &ports::OrderIntentLifecycle, client_idx: usize) -> Self {
        Self {
            client_idx,
            intent_emitted_mono_us: lifecycle.timing.intent_emitted_mono_us,
            risk_completed_mono_us: lifecycle.timing.risk_completed_mono_us,
            ..Self::default()
        }
    }

    fn apply_submission(&mut self, attempt: &ports::ExecutionSubmissionAttempt) {
        self.write_started_mono_us = attempt.userspace_write_started_mono_us;
        self.write_returned_mono_us = attempt.userspace_write_returned_mono_us;
        self.response_received_mono_us = attempt.response_received_mono_us;
    }

    fn apply_private(&mut self, kind: ports::PrivateOrderEventKind, received_mono_us: u64) {
        match kind {
            ports::PrivateOrderEventKind::Ack => {
                self.private_ack_received_mono_us = Some(received_mono_us)
            }
            ports::PrivateOrderEventKind::Report => {
                self.private_report_received_mono_us = Some(received_mono_us)
            }
        }
    }

    fn spans(&self) -> ExecutionSpanSnapshot {
        let elapsed = |start: Option<u64>, end: Option<u64>| {
            start
                .zip(end)
                .and_then(|(start, end)| end.checked_sub(start))
        };
        ExecutionSpanSnapshot {
            intent_to_risk_us: elapsed(self.intent_emitted_mono_us, self.risk_completed_mono_us),
            risk_to_write_us: elapsed(self.risk_completed_mono_us, self.write_started_mono_us),
            userspace_write_us: elapsed(self.write_started_mono_us, self.write_returned_mono_us),
            write_to_response_us: elapsed(
                self.write_returned_mono_us,
                self.response_received_mono_us,
            ),
            write_to_private_ack_us: elapsed(
                self.write_returned_mono_us,
                self.private_ack_received_mono_us,
            ),
            write_to_private_report_us: elapsed(
                self.write_returned_mono_us,
                self.private_report_received_mono_us,
            ),
            intent_to_private_report_us: elapsed(
                self.intent_emitted_mono_us,
                self.private_report_received_mono_us,
            ),
        }
    }
}

/// 执行 Worker - 在独立 Tokio 任务中运行
pub struct ExecutionWorker {
    config: ExecutionWorkerConfig,
    queues: WorkerQueues,
    execution_clients: Vec<Box<dyn ExecutionClient>>,
    execution_streams: SelectAll<IndexedExecutionStream>,
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
    execution_timelines: FxHashMap<OrderId, ExecutionTimeline>,
    /// 上次對帳時間
    last_reconcile: Instant,
    /// 策略到客戶端索引的映射（同交易所多帳戶路由）
    strategy_to_client: Option<rustc_hash::FxHashMap<String, usize>>,
    /// 帳戶到客戶端索引的映射（恢復訂單與控制面路由）
    account_to_client: FxHashMap<AccountId, usize>,
    /// Runtime-owned, externally read-back admission facts. Empty is a deliberate deny-all.
    account_admissions: FxHashMap<AccountId, AccountExecutionAdmission>,
    /// Environment independently bound to the selected runtime client.
    account_environments: FxHashMap<AccountId, AccountExecutionEnvironment>,
    /// Emergency is sticky for the worker lifetime; restart is required to re-arm execution.
    accepting_intents: bool,
    /// Operator authorization is independent from transient stream recovery. Automatic recovery
    /// may clear only its own latch and must never override an explicit SetIntake(false).
    operator_intake_enabled: bool,
    emergency_latched: bool,
    stream_recovery_pending: bool,
    client_connected: Vec<bool>,
    /// Per-client stream attach generation that must reach its matching tail marker before the
    /// client can be considered connected again. This prevents stale ready events already in the
    /// adapter backlog from reopening intake.
    client_stream_barriers: Vec<Option<u64>>,
    /// Highest globally unique stream generation observed for each client. A replacement stream
    /// announces its barrier out-of-band before reading the old consumer backlog, so older queued
    /// barriers must never supersede that newer generation.
    client_latest_stream_id: Vec<u64>,
    /// Matching synchronized markers remain fail-closed until the engine confirms that OMS,
    /// portfolio, and risk state have applied every preceding execution report.
    client_engine_ack_pending: Vec<Option<u64>>,
    recovery_intent_drain_required: bool,
    last_stream_recovery_attempt: Instant,
    intents_buf: Vec<OrderIntentEnvelope>,
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

    /// Refuse execution unless the runtime client binding and fresh external account proof agree.
    /// Tokenized securities remain outside this generic gate until #700 supplies its own
    /// product/compliance attestation policy.
    fn validate_account_admission(
        &self,
        account_id: &AccountId,
        intent: &OrderIntent,
        client_idx: usize,
        now: u64,
    ) -> Result<(), &'static str> {
        let admission = self
            .account_admissions
            .get(account_id)
            .ok_or("account has no current external admission")?;
        let selected_venue = self
            .venue_for_client(client_idx)
            .ok_or("selected execution client has no venue binding")?;
        let bound_environment = self
            .account_environments
            .get(account_id)
            .ok_or("account environment is not bound to an execution client")?;

        if &admission.account_id != account_id {
            return Err("account admission identity does not match intent account");
        }
        if intent
            .target_venue
            .is_some_and(|venue| venue != selected_venue)
        {
            return Err("intent venue does not match selected execution client");
        }
        if admission.venue != selected_venue {
            return Err("account admission venue does not match selected execution client");
        }
        if admission.environment != *bound_environment {
            return Err("account admission environment does not match execution client");
        }
        if admission.product_type != intent.product_type {
            return Err("account admission product scope does not match intent");
        }
        if intent.product_type != hft_core::ProductType::Spot {
            return Err("product requires a venue-specific account admission policy");
        }
        if !admission.ready {
            return Err("account admission is not ready");
        }
        if admission.kill_switch_active {
            return Err("account kill switch is active");
        }
        if admission.credential_reference.trim().is_empty() {
            return Err("account admission has no credential reference");
        }
        if admission.readback.state != AccountReadbackState::Enabled {
            return Err("external account readback is not enabled");
        }
        if admission
            .readback
            .regional_compliance_attestation_id
            .trim()
            .is_empty()
            || admission.readback.receipt_id.trim().is_empty()
            || admission.readback.evidence_digest.trim().is_empty()
        {
            return Err("account admission has incomplete external evidence");
        }
        if admission.readback.validated_at == 0
            || admission.readback.validated_at > now
            || admission.readback.valid_until < admission.readback.validated_at
            || now > admission.readback.valid_until
        {
            return Err("account external readback is stale or invalid");
        }
        if !admission.readback.capability.can_trade_crypto_spot {
            return Err("account capability does not permit crypto spot execution");
        }
        if admission.max_order_notional <= rust_decimal::Decimal::ZERO {
            return Err("account order-notional limit is not positive");
        }
        let price = intent
            .price
            .ok_or("account notional limit requires a priced intent")?;
        let notional = price
            .0
            .checked_mul(intent.quantity.0)
            .ok_or("account order notional overflowed")?;
        if notional > admission.max_order_notional {
            return Err("account order-notional limit exceeded");
        }
        let open_orders = self
            .tracked_orders
            .values()
            .filter(|order| order.account_id.as_ref() == Some(account_id))
            .count();
        if open_orders >= admission.max_open_orders {
            return Err("account open-order limit reached");
        }
        Ok(())
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
        let client_count = execution_clients.len();

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
            execution_timelines: FxHashMap::default(),
            last_reconcile: Instant::now(),
            strategy_to_client: None,
            account_to_client: FxHashMap::default(),
            account_admissions: FxHashMap::default(),
            account_environments: FxHashMap::default(),
            accepting_intents: true,
            operator_intake_enabled: true,
            emergency_latched: false,
            stream_recovery_pending: false,
            client_connected: vec![true; client_count],
            client_stream_barriers: vec![None; client_count],
            client_latest_stream_id: vec![0; client_count],
            client_engine_ack_pending: vec![None; client_count],
            recovery_intent_drain_required: false,
            last_stream_recovery_attempt: Instant::now(),
            intents_buf: Vec::with_capacity(batch_size),
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
        let client_count = execution_clients.len();

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
            execution_timelines: FxHashMap::default(),
            last_reconcile: Instant::now(),
            strategy_to_client: None,
            account_to_client: FxHashMap::default(),
            account_admissions: FxHashMap::default(),
            account_environments: FxHashMap::default(),
            accepting_intents: true,
            operator_intake_enabled: true,
            emergency_latched: false,
            stream_recovery_pending: false,
            client_connected: vec![true; client_count],
            client_stream_barriers: vec![None; client_count],
            client_latest_stream_id: vec![0; client_count],
            client_engine_ack_pending: vec![None; client_count],
            recovery_intent_drain_required: false,
            last_stream_recovery_attempt: Instant::now(),
            intents_buf: Vec::with_capacity(batch_size),
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
            let applied_streams = self.poll_applied_execution_streams().await;
            if applied_streams > 0 {
                had_activity = true;
            }
            if self.retry_stream_recovery_if_due().await {
                had_activity = true;
            }

            // Hitting the event batch limit does not prove that the private-event backlog is
            // drained. Defer new intents for one more tick so a queued disconnect/fill behind the
            // current item is observed before an order can be submitted.
            if events_received < self.execution_event_batch_limit() {
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
                let intent_notify = self.queues.intent_notify();
                let applied_stream_notify = self.queues.applied_stream_notify();
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
                    _ = applied_stream_notify.notified() => {
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
                    let indexed = stream
                        .map(move |result| IndexedExecutionStreamItem::Event {
                            client_idx: idx,
                            result,
                        })
                        .chain(futures::stream::once(async move {
                            IndexedExecutionStreamItem::Completed { client_idx: idx }
                        }));
                    self.execution_streams.push(Box::pin(indexed));
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
            self.drain_private_events_before_submission().await;
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
            if intent.product_type != hft_core::ProductType::Spot {
                let reject_event = ExecutionEvent::OrderReject {
                    order_id: OrderId(envelope.client_order_id.clone()),
                    reason: "product requires a venue-specific account admission policy".to_string(),
                    timestamp: now_micros(),
                };
                self.queues.send_event_reliable(reject_event).await;
                self.stats.orders_failed += 1;
                continue;
            }
            let account_id = if self.execution_clients[client_idx].is_simulated_execution() {
                envelope.account_id.clone()
            } else {
                let account_id = match envelope.account_id.clone() {
                    Some(account_id) => account_id,
                    None => {
                        let reject_event = ExecutionEvent::OrderReject {
                            order_id: OrderId(envelope.client_order_id.clone()),
                            reason: "missing canonical account identity".to_string(),
                            timestamp: now_micros(),
                        };
                        self.queues.send_event_reliable(reject_event).await;
                        self.stats.orders_failed += 1;
                        continue;
                    }
                };
                match self.account_to_client.get(&account_id) {
                    Some(expected_client_idx) if *expected_client_idx == client_idx => {}
                    Some(expected_client_idx) => {
                        warn!(
                            account_id = %account_id.0,
                            selected_client_idx = client_idx,
                            expected_client_idx = *expected_client_idx,
                            "account identity does not match selected execution client"
                        );
                        let reject_event = ExecutionEvent::OrderReject {
                            order_id: OrderId(envelope.client_order_id.clone()),
                            reason: "account identity does not match selected execution client"
                                .to_string(),
                            timestamp: now_micros(),
                        };
                        self.queues.send_event_reliable(reject_event).await;
                        self.stats.orders_failed += 1;
                        continue;
                    }
                    None => {
                        warn!(
                            account_id = %account_id.0,
                            "account identity is not bound to an execution client"
                        );
                        let reject_event = ExecutionEvent::OrderReject {
                            order_id: OrderId(envelope.client_order_id.clone()),
                            reason: "account identity is not bound to an execution client"
                                .to_string(),
                            timestamp: now_micros(),
                        };
                        self.queues.send_event_reliable(reject_event).await;
                        self.stats.orders_failed += 1;
                        continue;
                    }
                }
                if let Err(reason) =
                    self.validate_account_admission(&account_id, intent, client_idx, now_micros())
                {
                    warn!(
                        account_id = %account_id.0,
                        client_idx,
                        reason,
                        "account admission rejected execution intent"
                    );
                    let reject_event = ExecutionEvent::OrderReject {
                        order_id: OrderId(envelope.client_order_id.clone()),
                        reason: reason.to_string(),
                        timestamp: now_micros(),
                    };
                    self.queues.send_event_reliable(reject_event).await;
                    self.stats.orders_failed += 1;
                    continue;
                }
                Some(account_id)
            };
            let trace_id = OrderId(envelope.client_order_id.clone());
            self.execution_timelines.insert(
                trace_id.clone(),
                ExecutionTimeline::from_lifecycle(&envelope.lifecycle, client_idx),
            );

            let attempt = self.execution_clients[client_idx]
                .place_order_envelope_traced(&envelope)
                .await;
            let timeline = self
                .execution_timelines
                .get_mut(&trace_id)
                .expect("timeline inserted before submission");
            timeline.apply_submission(&attempt);
            let spans = timeline.spans();
            self.stats.recent_execution_latency_micros = spans.write_to_response_us;
            if let Some(end_to_end) = envelope
                .lifecycle
                .timing
                .intent_emitted_mono_us
                .zip(attempt.response_received_mono_us)
                .and_then(|(start, end)| end.checked_sub(start))
            {
                self.latency_monitor
                    .record_latency(LatencyStage::EndToEnd, end_to_end);
            }
            #[cfg(feature = "metrics")]
            Self::record_submission_timeline_metrics(*timeline);

            match attempt.outcome {
                Ok(order_id) => {
                    self.stats.orders_placed += 1;
                    if self.latch_duplicate_order_id(&order_id, client_idx) {
                        self.execution_timelines.remove(&trace_id);
                        self.stats.orders_failed += 1;
                        continue;
                    }
                    if order_id != trace_id {
                        let timeline = self
                            .execution_timelines
                            .remove(&trace_id)
                            .expect("timeline retained through successful submission");
                        self.execution_timelines.insert(order_id.clone(), timeline);
                    }
                    self.order_to_client.insert(order_id.clone(), client_idx);

                    let venue_for_client = intent
                        .target_venue
                        .or_else(|| self.venue_for_client(client_idx));
                    self.tracked_orders.insert(
                        order_id.clone(),
                        TrackedOrder {
                            symbol: intent.symbol.clone(),
                            strategy_id: intent.strategy_id.clone(),
                            venue: venue_for_client,
                            account_id: account_id.clone(),
                            side: intent.side,
                            limit_price: intent.price,
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
                        ..
                    } = envelope.intent;

                    let symbol_for_ack = symbol.clone();
                    let client_order_id = envelope.client_order_id.clone();

                    let new_event = ExecutionEvent::OrderNew {
                        order_id: order_id.clone(),
                        client_order_id: Some(client_order_id),
                        account_id,
                        symbol,
                        side,
                        quantity,
                        requested_price: price,
                        timestamp: now_micros(),
                        venue: venue_for_client,
                        strategy_id,
                    };
                    self.queues.send_event_reliable(new_event).await;

                    debug!(
                        write_to_response_us = ?spans.write_to_response_us,
                        "order submission response received"
                    );

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
                        if self.latch_duplicate_order_id(&order_id, client_idx) {
                            continue;
                        }
                        self.order_to_client.insert(order_id.clone(), client_idx);
                        self.tracked_orders.insert(
                            order_id.clone(),
                            TrackedOrder {
                                symbol: intent.symbol.clone(),
                                strategy_id: intent.strategy_id.clone(),
                                venue,
                                account_id: account_id.clone(),
                                side: intent.side,
                                limit_price: intent.price,
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
                                account_id,
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
                        self.execution_timelines.remove(&trace_id);
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

    /// Private reports outrank every new submission, including submissions already coalesced into
    /// the same queue batch. Drain to a momentary boundary before each order so a disconnect or
    /// reconciliation latch arriving during the previous HTTP await closes the remaining batch.
    async fn drain_private_events_before_submission(&mut self) {
        loop {
            while let Ok(command) = self.control_rx.try_recv() {
                self.handle_control_command(command).await;
            }
            let received = self.poll_execution_events().await;
            self.poll_applied_execution_streams().await;
            if received < self.execution_event_batch_limit() {
                break;
            }
        }
    }

    async fn reject_for_disabled_intake(&mut self, envelope: &OrderIntentEnvelope) {
        let reject_event = ExecutionEvent::OrderReject {
            order_id: OrderId(envelope.client_order_id.clone()),
            reason: format!(
                "execution intake disabled by control for {}",
                envelope.intent.symbol.as_str()
            ),
            timestamp: now_micros(),
        };
        self.queues.send_event_reliable(reject_event).await;
        self.stats.orders_failed += 1;
    }

    async fn reject_queued_intents_for_disabled_intake(&mut self) {
        let mut queued = Vec::new();
        loop {
            queued.clear();
            self.queues.receive_envelopes_into(&mut queued);
            if queued.is_empty() {
                break;
            }
            self.stats.intents_processed += queued.len() as u64;
            for envelope in queued.drain(..) {
                self.reject_for_disabled_intake(&envelope).await;
            }
        }
    }

    /// Reject one bounded queue batch while recovery stays fail-closed. Recovery must not spin in
    /// an unbounded drain loop because a continuously producing engine could otherwise prevent the
    /// worker from ever polling its private report stream again.
    async fn reject_one_queued_intent_batch_for_recovery(&mut self) -> bool {
        let mut queued = Vec::new();
        self.queues.receive_envelopes_into(&mut queued);
        self.stats.intents_processed += queued.len() as u64;
        for envelope in queued.drain(..) {
            self.reject_for_disabled_intake(&envelope).await;
        }
        !self.queues.has_pending_intents()
    }

    fn submission_outcome_may_be_unknown(error: &HftError) -> bool {
        !matches!(
            error,
            HftError::InvalidOrder(_)
                | HftError::SubmissionNotAttempted(_)
                | HftError::InsufficientBalance(_)
                | HftError::Risk(_)
                | HftError::Config(_)
                | HftError::Authentication(_)
                | HftError::RateLimit(_)
                | HftError::Exchange(_)
                | HftError::OrderNotFound(_)
        )
    }

    #[cfg(feature = "metrics")]
    fn record_submission_timeline_metrics(timeline: ExecutionTimeline) {
        let spans = timeline.spans();
        let metrics = infra_metrics::MetricsRegistry::global();
        if let Some(value) = spans.intent_to_risk_us {
            metrics.record_execution_intent_to_risk(value as f64);
        }
        if let Some(value) = spans.risk_to_write_us {
            metrics.record_execution_risk_to_write(value as f64);
        }
        if let Some(value) = spans.userspace_write_us {
            metrics.record_execution_userspace_write(value as f64);
        }
        if let Some(value) = spans.write_to_response_us {
            metrics.record_execution_write_to_response(value as f64);
        }
    }

    fn capture_private_timing(&mut self, client_idx: usize, event: &ExecutionEvent) -> bool {
        let ExecutionEvent::PrivateOrderTiming {
            order_id,
            kind,
            received_mono_us,
        } = event
        else {
            return false;
        };
        if let Some(timeline) = self.execution_timelines.get_mut(order_id) {
            if timeline.client_idx != client_idx {
                error!(
                    order_id = %order_id.0,
                    expected_client_idx = timeline.client_idx,
                    conflicting_client_idx = client_idx,
                    "discarded private timing from the wrong execution client"
                );
                self.emergency_latched = true;
                self.latch_stream_recovery();
                return true;
            }
            timeline.apply_private(*kind, *received_mono_us);
            #[cfg(feature = "metrics")]
            match kind {
                ports::PrivateOrderEventKind::Ack => {
                    if let Some(value) = timeline.spans().write_to_private_ack_us {
                        infra_metrics::MetricsRegistry::global()
                            .record_execution_write_to_private_ack(value as f64);
                    }
                }
                ports::PrivateOrderEventKind::Report => {
                    let spans = timeline.spans();
                    if let Some(value) = spans.write_to_private_report_us {
                        infra_metrics::MetricsRegistry::global()
                            .record_execution_write_to_private_report(value as f64);
                    }
                    if let Some(value) = spans.intent_to_private_report_us {
                        infra_metrics::MetricsRegistry::global()
                            .record_execution_intent_to_private_report(value as f64);
                    }
                }
            }
        }
        true
    }

    fn latch_duplicate_order_id(&mut self, order_id: &OrderId, client_idx: usize) -> bool {
        if !self.order_to_client.contains_key(order_id)
            && !self.tracked_orders.contains_key(order_id)
            && !self.pending_acks.contains_key(order_id)
        {
            return false;
        }

        error!(
            order_id = %order_id.0,
            existing_client_idx = ?self.order_to_client.get(order_id),
            duplicate_client_idx = client_idx,
            "duplicate execution order id; preserving the first lifecycle and latching intake"
        );
        self.accepting_intents = false;
        self.emergency_latched = true;
        true
    }

    /// 轮询执行回报流
    async fn poll_execution_events(&mut self) -> u32 {
        let mut events_count = 0;
        let batch_limit = self.execution_event_batch_limit();
        while events_count < batch_limit {
            match self.execution_streams.next().now_or_never() {
                Some(Some(IndexedExecutionStreamItem::Event {
                    client_idx,
                    result: Ok(event),
                })) => {
                    if self.capture_private_timing(client_idx, &event) {
                        events_count += 1;
                        continue;
                    }
                    if !self.private_order_event_matches_client(client_idx, &event) {
                        events_count += 1;
                        continue;
                    }
                    let requires_reconciliation =
                        self.event_requires_reconciliation(client_idx, &event);
                    self.update_connection_tracking(client_idx, &event);
                    self.update_execution_tracking(&event);
                    // Do not poll the adapter stream again until the downstream SPSC has accepted
                    // this event. For replayable adapter batches, that next poll is the delivery
                    // acknowledgement; cancellation during backpressure therefore replays rather
                    // than losing the in-flight event.
                    self.queues.send_event_reliable(event).await;
                    self.stats.events_sent += 1;
                    events_count += 1;
                    if requires_reconciliation {
                        self.attempt_stream_recovery().await;
                    }
                }
                Some(Some(IndexedExecutionStreamItem::Event {
                    client_idx,
                    result: Err(e),
                })) => {
                    warn!("执行回报流错误: {}", e);
                    self.latch_on_execution_stream_failure(client_idx).await;
                    break;
                }
                Some(Some(IndexedExecutionStreamItem::Completed { client_idx })) => {
                    self.handle_execution_stream_completion(client_idx).await;
                    break;
                }
                Some(None) => {
                    // Every production stream emits an explicit per-client Completed item before
                    // SelectAll removes it. Reaching aggregate exhaustion therefore needs no
                    // additional state transition.
                    break;
                }
                None => break,
            }
        }
        events_count
    }

    async fn handle_execution_stream_item(&mut self, item: Option<IndexedExecutionStreamItem>) {
        match item {
            Some(IndexedExecutionStreamItem::Event {
                client_idx,
                result: Ok(event),
            }) => {
                if self.capture_private_timing(client_idx, &event) {
                    return;
                }
                if !self.private_order_event_matches_client(client_idx, &event) {
                    return;
                }
                let requires_reconciliation =
                    self.event_requires_reconciliation(client_idx, &event);
                self.update_connection_tracking(client_idx, &event);
                self.update_execution_tracking(&event);
                self.queues.send_event_reliable(event).await;
                self.stats.events_sent += 1;
                if requires_reconciliation {
                    self.attempt_stream_recovery().await;
                }
            }
            Some(IndexedExecutionStreamItem::Event {
                client_idx,
                result: Err(error),
            }) => {
                warn!("执行回报流错误: {}", error);
                self.latch_on_execution_stream_failure(client_idx).await;
            }
            Some(IndexedExecutionStreamItem::Completed { client_idx }) => {
                self.handle_execution_stream_completion(client_idx).await;
            }
            None => {}
        }
    }

    async fn handle_execution_stream_completion(&mut self, client_idx: usize) {
        let Some(client) = self.execution_clients.get(client_idx) else {
            warn!(client_idx, "unknown execution report stream completed");
            self.latch_stream_recovery();
            return;
        };
        let explicitly_finite = client.execution_stream_may_complete();
        let client_healthy = client.health().await.connected;
        if explicitly_finite && client_healthy {
            warn!(
                client_idx,
                "explicitly finite execution report stream completed while client remains healthy"
            );
            return;
        }

        warn!(
            client_idx,
            explicitly_finite,
            "execution report stream ended without a healthy finite-stream contract"
        );
        self.latch_stream_recovery();
        if let Some(connected) = self.client_connected.get_mut(client_idx) {
            *connected = false;
        }
        self.queues
            .send_event_reliable(ExecutionEvent::ConnectionStatus {
                connected: false,
                timestamp: now_micros(),
            })
            .await;
    }

    fn update_connection_tracking(&mut self, client_idx: usize, event: &ExecutionEvent) {
        match event {
            ExecutionEvent::ConnectionStatus { connected, .. } => {
                let barrier_pending = self
                    .client_stream_barriers
                    .get(client_idx)
                    .is_some_and(Option::is_some);
                if !connected || !barrier_pending {
                    if let Some(state) = self.client_connected.get_mut(client_idx) {
                        *state = *connected;
                    }
                }
                if !connected {
                    self.latch_stream_recovery();
                }
            }
            ExecutionEvent::ExecutionStreamBarrier { stream_id, .. } => {
                let is_newer = self
                    .client_latest_stream_id
                    .get_mut(client_idx)
                    .is_some_and(|latest| {
                        if *stream_id > *latest {
                            *latest = *stream_id;
                            true
                        } else {
                            false
                        }
                    });
                if !is_newer {
                    warn!(
                        client_idx,
                        stream_id, "ignored stale execution-stream barrier"
                    );
                    return;
                }
                if let Some(barrier) = self.client_stream_barriers.get_mut(client_idx) {
                    *barrier = Some(*stream_id);
                }
                if let Some(pending) = self.client_engine_ack_pending.get_mut(client_idx) {
                    *pending = None;
                }
                if let Some(state) = self.client_connected.get_mut(client_idx) {
                    *state = false;
                }
                self.latch_stream_recovery();
            }
            ExecutionEvent::ExecutionStreamSynchronized {
                stream_id,
                connected,
                ..
            } => {
                let matches_current = self
                    .client_stream_barriers
                    .get(client_idx)
                    .is_some_and(|barrier| *barrier == Some(*stream_id));
                if matches_current {
                    if let Some(barrier) = self.client_stream_barriers.get_mut(client_idx) {
                        *barrier = None;
                    }
                    if let Some(pending) = self.client_engine_ack_pending.get_mut(client_idx) {
                        *pending = Some(*stream_id);
                    }
                    if let Some(state) = self.client_connected.get_mut(client_idx) {
                        *state = *connected;
                    }
                    self.latch_stream_recovery();
                } else {
                    warn!(
                        client_idx,
                        stream_id, "ignored stale execution-stream synchronization marker"
                    );
                }
            }
            ExecutionEvent::ReconciliationRequired { reason, .. } => {
                error!(
                    client_idx,
                    %reason,
                    "private execution events were missed; intake remains closed until a newer stream generation is engine-applied, otherwise restart is required"
                );
                if let Some(state) = self.client_connected.get_mut(client_idx) {
                    *state = false;
                }
                self.latch_stream_recovery();
            }
            _ => {}
        }
    }

    fn event_requires_reconciliation(&self, client_idx: usize, event: &ExecutionEvent) -> bool {
        match event {
            ExecutionEvent::ConnectionStatus {
                connected: true, ..
            } => {
                self.client_stream_barriers
                    .get(client_idx)
                    .is_some_and(Option::is_none)
                    && self
                        .client_engine_ack_pending
                        .get(client_idx)
                        .is_some_and(Option::is_none)
            }
            ExecutionEvent::ReconciliationRequired { .. } => true,
            _ => false,
        }
    }

    fn private_order_event_matches_client(
        &mut self,
        client_idx: usize,
        event: &ExecutionEvent,
    ) -> bool {
        let order_id = match event {
            ExecutionEvent::OrderAck { order_id, .. }
            | ExecutionEvent::Fill { order_id, .. }
            | ExecutionEvent::FeeCharged { order_id, .. }
            | ExecutionEvent::OrderReject { order_id, .. }
            | ExecutionEvent::OrderCompleted { order_id, .. }
            | ExecutionEvent::OrderCanceled { order_id, .. }
            | ExecutionEvent::OrderModified { order_id, .. }
            | ExecutionEvent::PrivateOrderTiming { order_id, .. } => order_id,
            _ => return true,
        };
        let Some(expected_client_idx) = self.order_to_client.get(order_id).copied() else {
            return true;
        };
        if expected_client_idx == client_idx {
            return true;
        }

        error!(
            order_id = %order_id.0,
            expected_client_idx,
            conflicting_client_idx = client_idx,
            "discarded private order event from the wrong execution client"
        );
        self.emergency_latched = true;
        self.latch_stream_recovery();
        false
    }

    fn latch_stream_recovery(&mut self) {
        self.accepting_intents = false;
        self.stream_recovery_pending = true;
        self.recovery_intent_drain_required = true;
    }

    fn execution_event_batch_limit(&self) -> u32 {
        self.config
            .batch_size
            .clamp(1, u32::MAX as usize)
            .try_into()
            .expect("execution event batch limit is clamped to u32")
    }

    async fn poll_applied_execution_streams(&mut self) -> u32 {
        let mut applied_count = 0;
        let mut should_reconcile = false;
        while let Some(stream_id) = self.queues.try_receive_applied_execution_stream() {
            applied_count += 1;
            let matched_client = self
                .client_engine_ack_pending
                .iter_mut()
                .enumerate()
                .find_map(|(client_idx, pending)| {
                    (*pending == Some(stream_id)).then(|| {
                        *pending = None;
                        client_idx
                    })
                });
            if let Some(client_idx) = matched_client {
                if let Some(client) = self.execution_clients.get(client_idx) {
                    client.acknowledge_execution_stream_applied(stream_id);
                }
                should_reconcile |= self
                    .client_connected
                    .get(client_idx)
                    .copied()
                    .unwrap_or(false);
            } else {
                warn!(
                    stream_id,
                    "ignored stale engine-applied execution-stream acknowledgement"
                );
            }
        }
        if should_reconcile {
            self.attempt_stream_recovery().await;
        }
        applied_count
    }

    fn stream_recovery_ready_for_reconcile(&self) -> bool {
        self.stream_recovery_pending
            && self.client_stream_barriers.iter().all(Option::is_none)
            && self.client_engine_ack_pending.iter().all(Option::is_none)
            && self.client_connected.iter().all(|connected| *connected)
    }

    async fn attempt_stream_recovery(&mut self) -> bool {
        if !self.stream_recovery_ready_for_reconcile() {
            return false;
        }
        self.last_stream_recovery_attempt = Instant::now();
        self.reconcile_open_orders().await
    }

    async fn retry_stream_recovery_if_due(&mut self) -> bool {
        if !self.stream_recovery_ready_for_reconcile()
            || self.last_stream_recovery_attempt.elapsed() < STREAM_RECOVERY_RETRY_INTERVAL
        {
            return false;
        }
        self.attempt_stream_recovery().await;
        true
    }

    fn update_execution_tracking(&mut self, event: &ExecutionEvent) {
        match event {
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
                    self.execution_timelines.remove(order_id);
                }
            }
            ExecutionEvent::OrderCanceled { order_id, .. }
            | ExecutionEvent::OrderReject { order_id, .. }
            | ExecutionEvent::OrderCompleted { order_id, .. } => {
                self.pending_acks.remove(order_id);
                self.order_to_client.remove(order_id);
                self.tracked_orders.remove(order_id);
                self.execution_timelines.remove(order_id);
            }
            _ => {}
        }
    }

    async fn latch_on_execution_stream_failure(&mut self, client_idx: usize) {
        self.latch_stream_recovery();
        if let Some(connected) = self.client_connected.get_mut(client_idx) {
            *connected = false;
        }
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
                include_positions,
                include_recent_fills,
                reply,
            } => {
                let snapshot = self
                    .collect_reconcile_snapshot(
                        include_balances,
                        include_positions,
                        include_recent_fills,
                    )
                    .await;
                let _ = reply.send(snapshot);
            }
            ControlCommand::SetIntake { enabled, reply } => {
                let result = if enabled && self.emergency_latched {
                    Err("execution intake is emergency-latched; restart required".to_string())
                } else if enabled && self.stream_recovery_pending {
                    Err("execution intake is waiting for private-stream reconciliation".to_string())
                } else {
                    self.operator_intake_enabled = enabled;
                    self.accepting_intents = enabled;
                    if !enabled {
                        // The acknowledgement is a real quiescence barrier: callers may start an
                        // authoritative snapshot only after every already-queued intent has been
                        // rejected under disabled intake.
                        self.reject_queued_intents_for_disabled_intake().await;
                    }
                    Ok(())
                };
                let _ = reply.send(result);
            }
            ControlCommand::ReplaceOrder {
                order_id,
                symbol,
                new_quantity,
                new_price,
                reply,
            } => {
                info!("控制指令: 替換訂單 {}", order_id.0);
                let replacement =
                    self.validate_replacement(&order_id, &symbol, new_quantity, new_price);
                let result = match replacement {
                    Ok(client_idx) => {
                        let Some(client) = self.execution_clients.get_mut(client_idx) else {
                            let _ = reply
                                .send(Err(format!("no execution client for order {}", order_id.0)));
                            return;
                        };
                        match client
                            .modify_order(&order_id, new_quantity, new_price)
                            .await
                        {
                            Ok(()) => {
                                if let Some(order) = self.tracked_orders.get_mut(&order_id) {
                                    if let Some(quantity) = new_quantity {
                                        order.remaining_quantity = quantity;
                                    }
                                    if let Some(price) = new_price {
                                        order.limit_price = Some(price);
                                    }
                                }
                                debug!("替換訂單成功: {} (client={})", order_id.0, client_idx);
                                Ok(())
                            }
                            Err(e) => {
                                let outcome_unknown = Self::submission_outcome_may_be_unknown(&e);
                                if outcome_unknown {
                                    self.accepting_intents = false;
                                    self.emergency_latched = true;
                                    self.reject_queued_intents_for_disabled_intake().await;
                                }
                                warn!(
                                    "替換訂單失敗: {} - {} (client={})",
                                    order_id.0, e, client_idx
                                );
                                if outcome_unknown {
                                    Err(format!(
                                        "order replacement outcome is unknown; execution intake latched and reconciliation required: {e}"
                                    ))
                                } else {
                                    Err(e.to_string())
                                }
                            }
                        }
                    }
                    Err(reason) => Err(reason),
                };
                let _ = reply.send(result);
            }
        }
    }

    fn validate_replacement(
        &self,
        order_id: &OrderId,
        symbol: &Symbol,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> Result<usize, String> {
        if !self.accepting_intents || self.emergency_latched || self.stream_recovery_pending {
            return Err(
                "execution intake is disabled; order replacement requires a healthy reviewed state"
                    .to_string(),
            );
        }
        let tracked = self.tracked_orders.get(order_id).ok_or_else(|| {
            format!(
                "order {} is not tracked by the execution worker",
                order_id.0
            )
        })?;
        if &tracked.symbol != symbol {
            return Err(format!(
                "replacement symbol mismatch for {}: expected {}, got {}",
                order_id.0,
                tracked.symbol.as_str(),
                symbol.as_str()
            ));
        }
        if let Some(quantity) = new_quantity {
            if quantity.0 <= rust_decimal::Decimal::ZERO {
                return Err("replacement quantity must be positive".to_string());
            }
            if quantity.0 > tracked.remaining_quantity.0 {
                return Err(format!(
                    "replacement quantity cannot increase from {} to {} without a new risk-reviewed intent",
                    tracked.remaining_quantity.0, quantity.0
                ));
            }
        }
        if let Some(price) = new_price {
            if price.0 <= rust_decimal::Decimal::ZERO {
                return Err("replacement price must be positive".to_string());
            }
            let current = tracked.limit_price.ok_or_else(|| {
                "order replacement cannot change price without a tracked limit price".to_string()
            })?;
            let increases_risk = match tracked.side {
                hft_core::Side::Buy => price.0 > current.0,
                hft_core::Side::Sell => price.0 < current.0,
            };
            if increases_risk {
                return Err(format!(
                    "replacement price {} is more aggressive than {}; cancel and submit a new risk-reviewed intent",
                    price.0, current.0
                ));
            }
        }

        self.select_client_for_cancel(&CancelTarget {
            order_id: order_id.clone(),
            symbol: symbol.clone(),
            venue: tracked.venue,
            account_id: tracked.account_id.clone(),
        })
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
                        outcome_unknown: false,
                    });
                    continue;
                }
            };
            let Some(client) = self.execution_clients.get_mut(client_idx) else {
                report.failures.push(CancelFailure {
                    order_id: target.order_id,
                    reason: format!("execution client index {client_idx} is unavailable"),
                    outcome_unknown: false,
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
                Err(error) => {
                    let outcome_unknown = Self::submission_outcome_may_be_unknown(&error);
                    if outcome_unknown {
                        self.accepting_intents = false;
                        self.emergency_latched = true;
                    }
                    report.failures.push(CancelFailure {
                        order_id: target.order_id,
                        reason: error.to_string(),
                        outcome_unknown,
                    });
                }
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
    pub outcome_unknown: bool,
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
    pub positions: Option<Result<Vec<ports::Position>, HftError>>,
    pub recent_fills: Option<Result<Vec<ports::AccountFill>, HftError>>,
    /// Capability and raw asset inventory are carried together so Spot assets cannot be
    /// mistaken for derivatives positions or accepted without an explicit venue declaration.
    pub asset_inventory_capability: Option<AssetInventoryCapability>,
    pub asset_inventory: Option<Result<Vec<AssetInventoryRecord>, HftError>>,
}

impl Default for ClientReconcileSnapshot {
    fn default() -> Self {
        Self {
            client_index: 0,
            venue: None,
            account_id: None,
            open_orders: Ok(Vec::new()),
            balances: None,
            positions: None,
            recent_fills: None,
            asset_inventory_capability: None,
            asset_inventory: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorkerReconcileSnapshot {
    pub clients: Vec<ClientReconcileSnapshot>,
}

impl WorkerReconcileSnapshot {
    pub(crate) fn account_id(&self) -> Option<AccountId> {
        let mut clients = self.clients.iter();
        let account_id = clients.next()?.account_id.as_ref()?;
        clients
            .all(|client| client.account_id.as_ref() == Some(account_id))
            .then(|| account_id.clone())
    }

    pub fn is_complete(&self) -> bool {
        !self.clients.is_empty()
            && self.clients.iter().all(|client| {
                client.open_orders.is_ok()
                    && client
                        .balances
                        .as_ref()
                        .is_none_or(|balances| balances.is_ok())
                    && client
                        .positions
                        .as_ref()
                        .is_none_or(|positions| positions.is_ok())
                    && client
                        .recent_fills
                        .as_ref()
                        .is_none_or(|fills| fills.is_ok())
            })
    }

    pub(crate) fn resolved_asset_inventory_capability(
        client: &ClientReconcileSnapshot,
    ) -> AssetInventoryCapability {
        client
            .asset_inventory_capability
            .unwrap_or(AssetInventoryCapability::Unsupported)
    }

    pub fn account_holdings_complete(&self) -> bool {
        !self.clients.is_empty()
            && self.clients.iter().all(|client| {
                match Self::resolved_asset_inventory_capability(client) {
                    AssetInventoryCapability::PositionSnapshotRequired => client
                        .positions
                        .as_ref()
                        .is_some_and(|result| result.is_ok()),
                    AssetInventoryCapability::AuthoritativeAssetInventory {
                        product_type: ProductType::Spot,
                    } => client.asset_inventory.as_ref().is_some_and(|result| {
                        result.as_ref().is_ok_and(|records| {
                            records.iter().all(|record| record.validate().is_ok())
                        })
                    }),
                    AssetInventoryCapability::AuthoritativeAssetInventory { .. }
                    | AssetInventoryCapability::Unsupported => false,
                }
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
        include_positions: bool,
        include_recent_fills: bool,
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
        reply: oneshot::Sender<Result<(), String>>,
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
    account_admissions: Option<std::collections::HashMap<AccountId, AccountExecutionAdmission>>,
    account_environments: Option<
        std::collections::HashMap<AccountId, AccountExecutionEnvironment>,
    >,
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
    if let Some(map) = account_admissions {
        worker.account_admissions = map.into_iter().collect();
    }
    if let Some(map) = account_environments {
        worker.account_environments = map.into_iter().collect();
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
    account_admissions: Option<std::collections::HashMap<AccountId, AccountExecutionAdmission>>,
    account_environments: Option<
        std::collections::HashMap<AccountId, AccountExecutionEnvironment>,
    >,
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
    if let Some(map) = account_admissions {
        worker.account_admissions = map.into_iter().collect();
    }
    if let Some(map) = account_environments {
        worker.account_environments = map.into_iter().collect();
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
        None,
        None,
    );
    h
}

impl ExecutionWorker {
    /// Periodic worker-side capability check. OMS comparison is performed by
    /// `ExecutionControlHandle`, which owns access to the engine's local truth.
    async fn reconcile_open_orders(&mut self) -> bool {
        let snapshot = self.collect_reconcile_snapshot(false, false, false).await;
        let complete = snapshot.is_complete();
        if !complete {
            warn!("對帳快照不完整；系統不得將未知狀態視為無未結訂單");
        }
        let clients_currently_healthy =
            !self.stream_recovery_pending || self.execution_clients_currently_healthy().await;
        if self.stream_recovery_pending
            && complete
            && clients_currently_healthy
            && self.client_connected.iter().all(|connected| *connected)
            && self.client_stream_barriers.iter().all(Option::is_none)
            && self.client_engine_ack_pending.iter().all(Option::is_none)
            && self.recovery_snapshot_matches(&snapshot)
        {
            if self.recovery_intent_drain_required {
                // Any intent queued before the engine-applied stream watermark was produced from
                // stale account state. Drain one bounded batch per pass so a continuous producer
                // cannot starve private-report polling; remain closed and retry until FIFO empty.
                if !self.reject_one_queued_intent_batch_for_recovery().await {
                    return complete;
                }
            }
            if self.execution_clients_currently_healthy().await {
                self.recovery_intent_drain_required = false;
                self.stream_recovery_pending = false;
                if self.operator_intake_enabled && !self.emergency_latched {
                    self.accepting_intents = true;
                    info!("private execution streams reconciled; execution intake restored");
                }
            }
        }
        complete
    }

    async fn execution_clients_currently_healthy(&mut self) -> bool {
        for client in &self.execution_clients {
            if !client.health().await.connected {
                return false;
            }
        }
        true
    }

    fn recovery_snapshot_matches(&self, snapshot: &WorkerReconcileSnapshot) -> bool {
        for client in &snapshot.clients {
            let Ok(exchange_orders) = &client.open_orders else {
                return false;
            };
            let local_orders = self
                .tracked_orders
                .iter()
                .filter(|(order_id, _)| {
                    self.order_to_client.get(*order_id).copied() == Some(client.client_index)
                })
                .collect::<Vec<_>>();
            if local_orders.len() != exchange_orders.len() {
                return false;
            }
            for (local_id, local) in local_orders {
                let Some(exchange) = exchange_orders.iter().find(|order| {
                    order.order_id == *local_id
                        || order.client_order_id.as_deref() == Some(local_id.0.as_str())
                }) else {
                    return false;
                };
                if exchange.remaining_quantity != local.remaining_quantity {
                    return false;
                }
            }
        }
        true
    }

    async fn collect_reconcile_snapshot(
        &mut self,
        include_balances: bool,
        include_positions: bool,
        include_recent_fills: bool,
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

            let inventory_capability = client.asset_inventory_capability();
            let balances = if include_balances {
                Some(client.get_balance().await)
            } else {
                None
            };
            let positions = if include_positions
                && matches!(
                    inventory_capability,
                    AssetInventoryCapability::PositionSnapshotRequired
                ) {
                Some(client.get_positions().await)
            } else {
                None
            };
            let asset_inventory = if include_balances
                && matches!(
                    inventory_capability,
                    AssetInventoryCapability::AuthoritativeAssetInventory {
                        product_type: ProductType::Spot,
                    }
                ) {
                Some(match balances.as_ref() {
                    Some(Ok(balances)) => client.asset_inventory_from_balances(balances),
                    Some(Err(_)) => Err(HftError::Execution(
                        "authoritative asset inventory is unavailable because the balance snapshot failed"
                            .to_string(),
                    )),
                    None => Err(HftError::Execution(
                        "authoritative asset inventory requires a balance snapshot".to_string(),
                    )),
                })
            } else {
                None
            };
            let recent_fills = if include_recent_fills && client.supports_recent_fills_snapshot() {
                Some(client.list_recent_fills().await)
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
                positions,
                recent_fills,
                asset_inventory_capability: Some(inventory_capability),
                asset_inventory,
            });
        }

        WorkerReconcileSnapshot { clients }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{
        AccountCapability, OrderType, Price, ProductType, Quantity, Side, Symbol, TimeInForce,
    };
    use ports::BoxStream;
    use ports::{AccountExternalReadback, ConnectionHealth, HftResult, OrderIntent};
    use std::collections::HashSet;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn reconciliation_account_scope_requires_one_complete_identity() {
        let client = |client_index, account_id: Option<&str>| ClientReconcileSnapshot {
            client_index,
            venue: None,
            account_id: account_id.map(|value| AccountId(value.to_string())),
            open_orders: Ok(Vec::new()),
            balances: None,
            positions: None,
            recent_fills: None,
            ..Default::default()
        };

        let one_account = WorkerReconcileSnapshot {
            clients: vec![client(0, Some("account-1")), client(1, Some("account-1"))],
        };
        assert_eq!(
            one_account.account_id(),
            Some(AccountId("account-1".to_string()))
        );
        assert!(WorkerReconcileSnapshot {
            clients: vec![client(0, Some("account-1")), client(1, Some("account-2"))],
        }
        .account_id()
        .is_none());
        assert!(WorkerReconcileSnapshot {
            clients: vec![client(0, Some("account-1")), client(1, None)],
        }
        .account_id()
        .is_none());
    }

    #[tokio::test]
    async fn downstream_backpressure_prevents_prefetching_the_next_adapter_event() {
        let queue_config = crate::ExecutionQueueConfig {
            intent_queue_capacity: 2,
            event_queue_capacity: 2,
            batch_size: 2,
        };
        let (_engine_queues, mut worker_queues) = crate::create_execution_queues(queue_config);
        worker_queues
            .send_event(ExecutionEvent::ConnectionStatus {
                connected: false,
                timestamp: 0,
            })
            .expect("fill the effective one-slot event ring");
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig {
                batch_size: 2,
                ..Default::default()
            },
            worker_queues,
            Vec::new(),
            control_rx,
        );
        let polls = Arc::new(AtomicUsize::new(0));
        let stream_polls = Arc::clone(&polls);
        let stream = futures::stream::unfold(0_u64, move |timestamp| {
            let stream_polls = Arc::clone(&stream_polls);
            async move {
                stream_polls.fetch_add(1, Ordering::AcqRel);
                Some((
                    IndexedExecutionStreamItem::Event {
                        client_idx: 0,
                        result: Ok::<_, HftError>(ExecutionEvent::OrderCanceled {
                            order_id: OrderId(format!("event-{timestamp}")),
                            timestamp,
                        }),
                    },
                    timestamp + 1,
                ))
            }
        });
        worker.execution_streams.push(Box::pin(stream));

        let poll_task = tokio::spawn(async move { worker.poll_execution_events().await });
        for _ in 0..100 {
            if polls.load(Ordering::Acquire) > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }

        assert_eq!(polls.load(Ordering::Acquire), 1);
        assert!(
            !poll_task.is_finished(),
            "first event must be backpressured"
        );
        poll_task.abort();
        let _ = poll_task.await;
    }

    #[test]
    fn explicit_exchange_rejections_are_known_but_transport_failures_are_ambiguous() {
        assert!(!ExecutionWorker::submission_outcome_may_be_unknown(
            &HftError::RateLimit("429".to_string())
        ));
        assert!(!ExecutionWorker::submission_outcome_may_be_unknown(
            &HftError::Exchange("invalid quantity".to_string())
        ));
        assert!(!ExecutionWorker::submission_outcome_may_be_unknown(
            &HftError::SubmissionNotAttempted("private stream recovering".to_string())
        ));
        assert!(ExecutionWorker::submission_outcome_may_be_unknown(
            &HftError::Network("connection reset".to_string())
        ));
        assert!(ExecutionWorker::submission_outcome_may_be_unknown(
            &HftError::Execution("accepted response was malformed".to_string())
        ));
        assert!(ExecutionWorker::submission_outcome_may_be_unknown(
            &HftError::Serialization("malformed response".to_string())
        ));
    }

    #[derive(Default)]
    struct MockExecutionState {
        placed: Vec<Symbol>,
        canceled: Vec<OrderId>,
        open_orders: Vec<OpenOrder>,
        balances: Vec<AccountBalance>,
        balance_reads: usize,
        spot_inventory: bool,
        healthy: Option<bool>,
        finite_stream: bool,
        disconnect_on_first_place: Option<mpsc::UnboundedSender<ExecutionEvent>>,
        modify_error: bool,
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
            let disconnect = {
                let mut state = self.state.lock().unwrap();
                state.placed.push(intent.symbol);
                (state.placed.len() == 1)
                    .then(|| state.disconnect_on_first_place.clone())
                    .flatten()
            };
            if let Some(disconnect) = disconnect {
                let _ = disconnect.send(ExecutionEvent::ConnectionStatus {
                    connected: false,
                    timestamp: now_micros(),
                });
            }
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
            if self.state.lock().unwrap().modify_error {
                Err(HftError::Execution(
                    "amend reconciliation required".to_string(),
                ))
            } else {
                Ok(())
            }
        }

        async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
            Ok(Box::pin(futures::stream::empty()))
        }

        fn execution_stream_may_complete(&self) -> bool {
            self.state.lock().unwrap().finite_stream
        }

        async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
            if self.list_error {
                Err(HftError::Network("open-order snapshot failed".to_string()))
            } else {
                Ok(self.state.lock().unwrap().open_orders.clone())
            }
        }

        fn asset_inventory_capability(&self) -> AssetInventoryCapability {
            if self.state.lock().unwrap().spot_inventory {
                AssetInventoryCapability::AuthoritativeAssetInventory {
                    product_type: hft_core::ProductType::Spot,
                }
            } else {
                AssetInventoryCapability::Unsupported
            }
        }

        async fn get_balance(&self) -> HftResult<Vec<AccountBalance>> {
            let mut state = self.state.lock().unwrap();
            state.balance_reads += 1;
            Ok(state.balances.clone())
        }

        async fn connect(&mut self) -> HftResult<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> HftResult<()> {
            Ok(())
        }

        async fn health(&self) -> ConnectionHealth {
            ConnectionHealth {
                connected: self.state.lock().unwrap().healthy.unwrap_or(true),
                latency_ms: None,
                last_heartbeat: now_micros(),
            }
        }
    }

    #[tokio::test]
    async fn stream_barrier_ignores_old_ready_until_matching_tail_after_fill_fee() {
        let client = MockExecutionClient {
            state: Arc::new(StdMutex::new(MockExecutionState {
                healthy: Some(true),
                ..Default::default()
            })),
            place_error: false,
            list_error: false,
            cancel_error: false,
        };
        let (mut engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig {
                batch_size: 1,
                ..Default::default()
            },
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        let fill_id = "startup-fill".to_string();
        let events = vec![
            ExecutionEvent::ExecutionStreamBarrier {
                stream_id: 2,
                timestamp: 1,
            },
            ExecutionEvent::ConnectionStatus {
                connected: true,
                timestamp: 2,
            },
            ExecutionEvent::ExecutionStreamSynchronized {
                stream_id: 1,
                connected: true,
                timestamp: 3,
            },
            ExecutionEvent::Fill {
                order_id: OrderId("logical-1".to_string()),
                price: Price::from_f64(0.5).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: 4,
                fill_id: fill_id.clone(),
            },
            ExecutionEvent::FeeCharged {
                order_id: OrderId("logical-1".to_string()),
                amount: rust_decimal::Decimal::new(1, 2),
                timestamp: 4,
                fill_id,
            },
            ExecutionEvent::ExecutionStreamSynchronized {
                stream_id: 2,
                connected: true,
                timestamp: 5,
            },
        ];
        worker
            .execution_streams
            .push(Box::pin(futures::stream::iter(events.into_iter().map(
                |event| IndexedExecutionStreamItem::Event {
                    client_idx: 0,
                    result: Ok::<_, HftError>(event),
                },
            ))));

        for _ in 0..5 {
            assert_eq!(worker.poll_execution_events().await, 1);
            assert!(!worker.accepting_intents);
            assert_eq!(worker.client_stream_barriers[0], Some(2));
            assert!(!worker.client_connected[0]);
        }

        assert_eq!(worker.poll_execution_events().await, 1);
        assert!(!worker.accepting_intents);
        assert!(worker.stream_recovery_pending);
        assert_eq!(worker.client_stream_barriers[0], None);
        assert_eq!(worker.client_engine_ack_pending[0], Some(2));

        for _ in 0..40 {
            engine_queues
                .send_intent(
                    AccountId("recovery-test".to_string()),
                    create_test_intent("BTCUSDT"),
                )
                .expect("queue stale recovery intent");
        }
        engine_queues.acknowledge_applied_execution_stream(2);
        assert_eq!(worker.poll_applied_execution_streams().await, 1);
        assert!(!worker.accepting_intents);
        assert!(worker.stream_recovery_pending);
        assert_eq!(worker.client_engine_ack_pending[0], None);
        assert!(worker.client_connected[0]);
        assert!(worker.queues.has_pending_intents());
        assert_eq!(worker.stats.orders_failed, 32);

        let mut remaining = worker.queues.receive_envelopes();
        worker.process_order_intents(&mut remaining).await;
        assert!(!worker.queues.has_pending_intents());
        assert_eq!(worker.stats.orders_failed, 40);
        assert!(worker.attempt_stream_recovery().await);
        assert!(worker.accepting_intents);
        assert!(!worker.stream_recovery_pending);
    }

    #[test]
    fn replacement_barrier_cannot_be_overwritten_by_an_older_stream_backlog() {
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

        worker.update_connection_tracking(
            0,
            &ExecutionEvent::ExecutionStreamBarrier {
                stream_id: 20,
                timestamp: 1,
            },
        );
        worker.update_connection_tracking(
            0,
            &ExecutionEvent::ExecutionStreamBarrier {
                stream_id: 10,
                timestamp: 2,
            },
        );
        worker.update_connection_tracking(
            0,
            &ExecutionEvent::ExecutionStreamSynchronized {
                stream_id: 10,
                connected: true,
                timestamp: 3,
            },
        );

        assert_eq!(worker.client_latest_stream_id[0], 20);
        assert_eq!(worker.client_stream_barriers[0], Some(20));
        assert_eq!(worker.client_engine_ack_pending[0], None);
        assert!(!worker.client_connected[0]);

        worker.update_connection_tracking(
            0,
            &ExecutionEvent::ExecutionStreamSynchronized {
                stream_id: 20,
                connected: true,
                timestamp: 4,
            },
        );
        assert_eq!(worker.client_stream_barriers[0], None);
        assert_eq!(worker.client_engine_ack_pending[0], Some(20));
        assert!(worker.client_connected[0]);
    }

    #[tokio::test]
    async fn synchronized_marker_retries_recovery_after_current_health_recovers() {
        let state = Arc::new(StdMutex::new(MockExecutionState {
            healthy: Some(false),
            ..Default::default()
        }));
        let client = MockExecutionClient {
            state: Arc::clone(&state),
            place_error: false,
            list_error: false,
            cancel_error: false,
        };
        let (engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig {
                batch_size: 1,
                ..Default::default()
            },
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        worker
            .execution_streams
            .push(Box::pin(futures::stream::iter(
                [
                    ExecutionEvent::ExecutionStreamBarrier {
                        stream_id: 7,
                        timestamp: 1,
                    },
                    ExecutionEvent::ExecutionStreamSynchronized {
                        stream_id: 7,
                        connected: true,
                        timestamp: 2,
                    },
                ]
                .into_iter()
                .map(|event| IndexedExecutionStreamItem::Event {
                    client_idx: 0,
                    result: Ok::<_, HftError>(event),
                }),
            )));

        assert_eq!(worker.poll_execution_events().await, 1);
        assert_eq!(worker.poll_execution_events().await, 1);
        assert_eq!(worker.client_engine_ack_pending[0], Some(7));
        engine_queues.acknowledge_applied_execution_stream(7);
        assert_eq!(worker.poll_applied_execution_streams().await, 1);
        assert!(!worker.accepting_intents);
        assert!(worker.stream_recovery_pending);
        assert_eq!(worker.client_stream_barriers[0], None);
        assert_eq!(worker.client_engine_ack_pending[0], None);

        state.lock().unwrap().healthy = Some(true);
        worker.last_stream_recovery_attempt = Instant::now() - STREAM_RECOVERY_RETRY_INTERVAL;
        assert!(worker.retry_stream_recovery_if_due().await);
        assert!(worker.accepting_intents);
        assert!(!worker.stream_recovery_pending);
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

    fn ready_spot_admission(account_id: AccountId, venue: VenueId) -> AccountExecutionAdmission {
        AccountExecutionAdmission {
            account_id,
            venue,
            product_type: ProductType::Spot,
            environment: AccountExecutionEnvironment::Testnet,
            credential_reference: "secret-ref:test-account".to_string(),
            readback: AccountExternalReadback {
                state: AccountReadbackState::Enabled,
                balances: vec![AccountBalance {
                    asset: "USDT".to_string(),
                    available: rust_decimal::Decimal::from(1_000),
                    frozen: rust_decimal::Decimal::ZERO,
                    total: rust_decimal::Decimal::from(1_000),
                    usd_value: Some(rust_decimal::Decimal::from(1_000)),
                }],
                capability: AccountCapability {
                    can_trade_crypto_spot: true,
                    can_trade_tokenized_securities: false,
                    can_trade_brokerage_equities: false,
                    jurisdiction: Some("test".to_string()),
                    kyc_level: Some("test".to_string()),
                },
                regional_compliance_attestation_id: "compliance-test-receipt".to_string(),
                receipt_id: "account-test-receipt".to_string(),
                evidence_digest: "sha256:test".to_string(),
                validated_at: 1,
                valid_until: u64::MAX,
            },
            max_order_notional: rust_decimal::Decimal::from(1_000_000),
            max_open_orders: 10,
            kill_switch_active: false,
            ready: true,
        }
    }

    fn bind_ready_spot_admission(
        worker: &mut ExecutionWorker,
        account_id: AccountId,
        client_idx: usize,
        venue: VenueId,
    ) {
        worker.venue_to_client.insert(venue, client_idx);
        worker.account_to_client.insert(account_id.clone(), client_idx);
        worker
            .account_environments
            .insert(account_id.clone(), AccountExecutionEnvironment::Testnet);
        worker
            .account_admissions
            .insert(account_id.clone(), ready_spot_admission(account_id, venue));
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
            .send_intent(
                AccountId("emergency-test".to_string()),
                create_test_intent("ETHUSDT"),
            )
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
                side: Side::Buy,
                limit_price: Some(Price::from_f64(100.0).unwrap()),
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
    fn replacement_policy_is_tracked_routable_and_non_increasing() {
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
        let order_id = OrderId("replace-order".to_string());
        let symbol = Symbol::new("123");
        worker.order_to_client.insert(order_id.clone(), 0);
        worker.tracked_orders.insert(
            order_id.clone(),
            TrackedOrder {
                symbol: symbol.clone(),
                strategy_id: "test".to_string(),
                venue: Some(VenueId::POLYMARKET),
                account_id: None,
                side: Side::Buy,
                limit_price: Some(Price::from_f64(0.5).unwrap()),
                remaining_quantity: Quantity::from_f64(10.0).unwrap(),
                processed_fill_ids: HashSet::new(),
            },
        );

        assert_eq!(
            worker
                .validate_replacement(
                    &order_id,
                    &symbol,
                    Some(Quantity::from_f64(8.0).unwrap()),
                    Some(Price::from_f64(0.4).unwrap()),
                )
                .expect("non-increasing replacement"),
            0
        );
        assert!(worker
            .validate_replacement(
                &order_id,
                &symbol,
                Some(Quantity::from_f64(11.0).unwrap()),
                None,
            )
            .expect_err("quantity increase")
            .contains("cannot increase"));
        assert!(worker
            .validate_replacement(
                &order_id,
                &symbol,
                None,
                Some(Price::from_f64(0.6).unwrap()),
            )
            .expect_err("more aggressive buy price")
            .contains("more aggressive"));

        worker.emergency_latched = true;
        assert!(worker
            .validate_replacement(
                &order_id,
                &symbol,
                None,
                Some(Price::from_f64(0.4).unwrap())
            )
            .expect_err("emergency replacement")
            .contains("disabled"));
    }

    #[tokio::test]
    async fn ambiguous_replacement_latches_intake_and_requires_reconciliation() {
        let state = Arc::new(StdMutex::new(MockExecutionState {
            modify_error: true,
            ..Default::default()
        }));
        let client = MockExecutionClient {
            state,
            place_error: false,
            list_error: false,
            cancel_error: false,
        };
        let (mut engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        engine_queues
            .send_intent(create_test_intent("BTCUSDT"))
            .expect("queue intent before ambiguous replacement");
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        let order_id = OrderId("replace-order".to_string());
        let symbol = Symbol::new("123");
        worker.order_to_client.insert(order_id.clone(), 0);
        worker.tracked_orders.insert(
            order_id.clone(),
            TrackedOrder {
                symbol: symbol.clone(),
                strategy_id: "test".to_string(),
                venue: Some(VenueId::MOCK),
                account_id: None,
                side: Side::Buy,
                limit_price: Some(Price::from_f64(0.5).unwrap()),
                remaining_quantity: Quantity::from_f64(10.0).unwrap(),
                processed_fill_ids: HashSet::new(),
            },
        );
        let (reply, result) = oneshot::channel();

        worker
            .handle_control_command(ControlCommand::ReplaceOrder {
                order_id: order_id.clone(),
                symbol,
                new_quantity: Some(Quantity::from_f64(8.0).unwrap()),
                new_price: Some(Price::from_f64(0.4).unwrap()),
                reply,
            })
            .await;

        assert!(result
            .await
            .expect("replacement result")
            .expect_err("ambiguous replacement")
            .contains("reconciliation required"));
        assert!(!worker.accepting_intents);
        assert!(worker.emergency_latched);
        assert!(worker.queues.receive_envelopes().is_empty());
        let mut events = Vec::new();
        engine_queues.receive_events_into(&mut events);
        assert!(matches!(
            events.as_slice(),
            [ExecutionEvent::OrderReject { .. }]
        ));
        let tracked = worker.tracked_orders.get(&order_id).unwrap();
        assert_eq!(
            tracked.remaining_quantity,
            Quantity::from_f64(10.0).unwrap()
        );
        assert_eq!(tracked.limit_price, Some(Price::from_f64(0.5).unwrap()));
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
                side: Side::Buy,
                limit_price: Some(Price::from_f64(100.0).unwrap()),
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
                side: Side::Buy,
                limit_price: Some(Price::from_f64(100.0).unwrap()),
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

        let snapshot = worker.collect_reconcile_snapshot(false, false, false).await;
        assert_eq!(snapshot.clients.len(), 1);
        assert!(snapshot.clients[0].open_orders.is_err());
        assert!(!snapshot.is_complete());
    }

    #[tokio::test]
    async fn spot_inventory_reuses_the_authoritative_balance_snapshot() {
        let state = Arc::new(StdMutex::new(MockExecutionState {
            balances: vec![AccountBalance {
                asset: "USDT".to_string(),
                available: rust_decimal::Decimal::from(90),
                frozen: rust_decimal::Decimal::from(10),
                total: rust_decimal::Decimal::from(100),
                usd_value: Some(rust_decimal::Decimal::from(100)),
            }],
            spot_inventory: true,
            ..Default::default()
        }));
        let client = MockExecutionClient {
            state: Arc::clone(&state),
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

        let snapshot = worker.collect_reconcile_snapshot(true, true, false).await;

        assert_eq!(state.lock().unwrap().balance_reads, 1);
        assert!(matches!(
            snapshot.clients[0].asset_inventory.as_ref(),
            Some(Ok(inventory))
                if inventory.len() == 1
                    && inventory[0].locked == rust_decimal::Decimal::from(10)
        ));
    }

    #[tokio::test]
    async fn stream_completion_requires_an_explicit_finite_contract_and_healthy_client() {
        let (_engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let live_client = MockExecutionClient {
            state: Arc::new(StdMutex::new(MockExecutionState::default())),
            place_error: false,
            list_error: false,
            cancel_error: false,
        };
        let mut live_worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(live_client)],
            control_rx,
        );
        live_worker.handle_execution_stream_completion(0).await;
        assert!(!live_worker.accepting_intents);
        assert!(live_worker.stream_recovery_pending);

        let (_engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let healthy_client = MockExecutionClient {
            state: Arc::new(StdMutex::new(MockExecutionState {
                finite_stream: true,
                ..Default::default()
            })),
            place_error: false,
            list_error: false,
            cancel_error: false,
        };
        let mut healthy_worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(healthy_client)],
            control_rx,
        );
        healthy_worker.handle_execution_stream_completion(0).await;
        assert!(healthy_worker.accepting_intents);
        assert!(!healthy_worker.stream_recovery_pending);

        let (_engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let disconnected_client = MockExecutionClient {
            state: Arc::new(StdMutex::new(MockExecutionState {
                healthy: Some(false),
                finite_stream: true,
                ..Default::default()
            })),
            place_error: false,
            list_error: false,
            cancel_error: false,
        };
        let mut disconnected_worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(disconnected_client)],
            control_rx,
        );
        disconnected_worker
            .handle_execution_stream_completion(0)
            .await;
        assert!(!disconnected_worker.accepting_intents);
        assert!(disconnected_worker.stream_recovery_pending);
    }

    #[tokio::test]
    async fn one_live_client_stream_completion_latches_a_multi_client_worker() {
        let clients = (0..2)
            .map(|_| {
                Box::new(MockExecutionClient {
                    state: Arc::new(StdMutex::new(MockExecutionState::default())),
                    place_error: false,
                    list_error: false,
                    cancel_error: false,
                }) as Box<dyn ExecutionClient>
            })
            .collect();
        let (_engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            clients,
            control_rx,
        );
        worker
            .execution_streams
            .push(Box::pin(futures::stream::iter([
                IndexedExecutionStreamItem::Completed { client_idx: 0 },
            ])));
        worker
            .execution_streams
            .push(Box::pin(futures::stream::pending::<
                IndexedExecutionStreamItem,
            >()));

        assert_eq!(worker.poll_execution_events().await, 0);
        assert!(!worker.accepting_intents);
        assert!(worker.stream_recovery_pending);
        assert!(!worker.client_connected[0]);
        assert!(worker.client_connected[1]);
        assert!(!worker.execution_streams.is_empty());
    }

    #[tokio::test]
    async fn disconnect_during_first_submission_rejects_the_rest_of_the_batch() {
        let (disconnect_tx, disconnect_rx) = mpsc::unbounded_channel();
        let state = Arc::new(StdMutex::new(MockExecutionState {
            disconnect_on_first_place: Some(disconnect_tx),
            ..Default::default()
        }));
        let client = MockExecutionClient {
            state: Arc::clone(&state),
            place_error: false,
            list_error: false,
            cancel_error: false,
        };
        let (mut engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let account_id = AccountId("disconnect-test".to_string());
        engine_queues
            .send_intent(account_id.clone(), create_test_intent("FIRST"))
            .expect("queue first intent");
        engine_queues
            .send_intent(account_id.clone(), create_test_intent("SECOND"))
            .expect("queue second intent");
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        bind_ready_spot_admission(&mut worker, account_id, 0, VenueId::BYBIT);
        worker
            .execution_streams
            .push(Box::pin(futures::stream::unfold(
                disconnect_rx,
                |mut receiver| async move {
                    receiver.recv().await.map(|event| {
                        (
                            IndexedExecutionStreamItem::Event {
                                client_idx: 0,
                                result: Ok(event),
                            },
                            receiver,
                        )
                    })
                },
            )));
        let mut intents = worker.queues.receive_envelopes();

        worker.process_order_intents(&mut intents).await;

        assert_eq!(state.lock().unwrap().placed.len(), 1);
        assert_eq!(worker.stats.orders_placed, 1);
        assert_eq!(worker.stats.orders_failed, 1);
        assert!(!worker.accepting_intents);
        assert!(worker.stream_recovery_pending);
    }

    #[tokio::test]
    async fn automatic_stream_recovery_does_not_override_operator_pause() {
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
        let (pause_tx, pause_rx) = oneshot::channel();
        worker
            .handle_control_command(ControlCommand::SetIntake {
                enabled: false,
                reply: pause_tx,
            })
            .await;
        assert!(pause_rx.await.expect("pause reply").is_ok());
        assert!(!worker.operator_intake_enabled);

        worker.latch_stream_recovery();
        assert!(worker.reconcile_open_orders().await);
        assert!(!worker.stream_recovery_pending);
        assert!(!worker.accepting_intents);

        let (resume_tx, resume_rx) = oneshot::channel();
        worker
            .handle_control_command(ControlCommand::SetIntake {
                enabled: true,
                reply: resume_tx,
            })
            .await;
        assert!(resume_rx.await.expect("resume reply").is_ok());
        assert!(worker.accepting_intents);
    }

    #[tokio::test]
    async fn disabling_intake_acknowledges_only_after_queued_intents_are_rejected() {
        let (mut engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        engine_queues
            .send_intent(
                AccountId("intake-test".to_string()),
                create_test_intent("BTCUSDT"),
            )
            .expect("queue intent before reconciliation barrier");
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            Vec::new(),
            control_rx,
        );
        let (reply, result) = oneshot::channel();

        worker
            .handle_control_command(ControlCommand::SetIntake {
                enabled: false,
                reply,
            })
            .await;

        assert_eq!(result.await.expect("intake barrier reply"), Ok(()));
        assert!(worker.queues.receive_envelopes().is_empty());
        assert!(!worker.accepting_intents);
        assert_eq!(worker.stats.intents_processed, 1);
        assert_eq!(worker.stats.orders_failed, 1);
        let mut events = Vec::new();
        engine_queues.receive_events_into(&mut events);
        assert!(matches!(
            events.as_slice(),
            [ExecutionEvent::OrderReject { .. }]
        ));
    }

    #[tokio::test]
    async fn private_stream_event_gap_restores_intake_after_exact_order_match() {
        let local_id = OrderId("client-1".to_string());
        let state = Arc::new(StdMutex::new(MockExecutionState {
            open_orders: vec![OpenOrder {
                order_id: OrderId("exchange-7".to_string()),
                client_order_id: Some(local_id.0.clone()),
                symbol: Symbol::new("BTCUSDT"),
                side: Side::Buy,
                order_type: OrderType::Limit,
                original_quantity: Quantity::from_f64(1.0).unwrap(),
                remaining_quantity: Quantity::from_f64(1.0).unwrap(),
                filled_quantity: Quantity::zero(),
                price: Some(Price::from_f64(100.0).unwrap()),
                status: ports::OrderStatus::Acknowledged,
                created_at: 1,
                updated_at: 1,
            }],
            ..Default::default()
        }));
        let client = MockExecutionClient {
            state,
            place_error: false,
            list_error: false,
            cancel_error: false,
        };
        let (engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        worker.order_to_client.insert(local_id.clone(), 0);
        worker.tracked_orders.insert(
            local_id,
            TrackedOrder {
                symbol: Symbol::new("BTCUSDT"),
                strategy_id: "test".to_string(),
                venue: Some(VenueId::MOCK),
                account_id: None,
                side: Side::Buy,
                limit_price: Some(Price::from_f64(100.0).unwrap()),
                remaining_quantity: Quantity::from_f64(1.0).unwrap(),
                processed_fill_ids: HashSet::new(),
            },
        );
        worker.update_connection_tracking(
            0,
            &ExecutionEvent::ReconciliationRequired {
                reason: "broadcast subscriber lagged".to_string(),
                timestamp: 1,
            },
        );
        assert!(!worker.client_connected[0]);
        assert!(!worker.accepting_intents);

        worker.update_connection_tracking(
            0,
            &ExecutionEvent::ExecutionStreamBarrier {
                stream_id: 17,
                timestamp: 2,
            },
        );
        worker.update_connection_tracking(
            0,
            &ExecutionEvent::ExecutionStreamSynchronized {
                stream_id: 17,
                connected: true,
                timestamp: 3,
            },
        );
        assert!(!worker.accepting_intents);
        assert_eq!(worker.client_engine_ack_pending[0], Some(17));

        engine_queues.acknowledge_applied_execution_stream(17);
        assert_eq!(worker.poll_applied_execution_streams().await, 1);
        assert!(worker.accepting_intents);
        assert!(!worker.stream_recovery_pending);
    }

    #[tokio::test]
    async fn private_stream_reconnect_stays_latched_when_local_order_is_missing() {
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
        let local_id = OrderId("client-missing".to_string());
        worker.order_to_client.insert(local_id.clone(), 0);
        worker.tracked_orders.insert(
            local_id,
            TrackedOrder {
                symbol: Symbol::new("BTCUSDT"),
                strategy_id: "test".to_string(),
                venue: Some(VenueId::MOCK),
                account_id: None,
                side: Side::Buy,
                limit_price: Some(Price::from_f64(100.0).unwrap()),
                remaining_quantity: Quantity::from_f64(1.0).unwrap(),
                processed_fill_ids: HashSet::new(),
            },
        );
        worker.update_connection_tracking(
            0,
            &ExecutionEvent::ConnectionStatus {
                connected: false,
                timestamp: 1,
            },
        );
        worker.update_connection_tracking(
            0,
            &ExecutionEvent::ConnectionStatus {
                connected: true,
                timestamp: 2,
            },
        );

        assert!(worker.reconcile_open_orders().await);
        assert!(!worker.accepting_intents);
        assert!(worker.stream_recovery_pending);
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
        let account_id = AccountId("ambiguous-test".to_string());
        engine_queues
            .send_intent(account_id.clone(), create_test_intent("BTCUSDT"))
            .expect("queue intent");
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        bind_ready_spot_admission(&mut worker, account_id, 0, VenueId::BYBIT);
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
    async fn missing_or_mismatched_account_is_rejected_before_submission() {
        let state = Arc::new(StdMutex::new(MockExecutionState::default()));
        let client = MockExecutionClient {
            state: Arc::clone(&state),
            place_error: false,
            list_error: false,
            cancel_error: false,
        };
        let (mut engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        engine_queues
            .send_envelope(ports::OrderIntentEnvelope::new(
                create_test_intent("MISSING"),
                ports::OrderIntentLifecycle::default(),
            ))
            .expect("queue missing-account intent");
        let mismatched_account = AccountId("mismatched-account".to_string());
        engine_queues
            .send_envelope(
                ports::OrderIntentEnvelope::new(
                    create_test_intent("MISMATCHED"),
                    ports::OrderIntentLifecycle::default(),
                )
                .with_account_id(mismatched_account.clone()),
            )
            .expect("queue mismatched-account intent");
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        worker.account_to_client = [(mismatched_account, 1usize)].into_iter().collect();
        let mut queued = worker.queues.receive_envelopes();

        worker.process_order_intents(&mut queued).await;

        assert!(state.lock().unwrap().placed.is_empty());
        let mut events = Vec::new();
        engine_queues.receive_events_into(&mut events);
        assert!(matches!(
            events.as_slice(),
            [
                ExecutionEvent::OrderReject { reason: missing, .. },
                ExecutionEvent::OrderReject { reason: mismatched, .. },
            ] if missing == "missing canonical account identity"
                && mismatched == "account identity does not match selected execution client"
        ));
    }

    #[tokio::test]
    async fn account_admission_requires_current_evidence_kill_clearance_and_limits() {
        let state = Arc::new(StdMutex::new(MockExecutionState::default()));
        let client = MockExecutionClient {
            state: Arc::clone(&state),
            place_error: false,
            list_error: false,
            cancel_error: false,
        };
        let (mut engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let missing = AccountId("missing-admission".to_string());
        let stale = AccountId("stale-admission".to_string());
        let disabled = AccountId("disabled-admission".to_string());
        let killed = AccountId("killed-admission".to_string());
        let limited = AccountId("limited-admission".to_string());
        let ready = AccountId("ready-admission".to_string());
        for (account_id, symbol) in [
            (&missing, "MISSING"),
            (&stale, "STALE"),
            (&disabled, "DISABLED"),
            (&killed, "KILLED"),
            (&limited, "LIMITED"),
            (&ready, "READY"),
        ] {
            engine_queues
                .send_intent(account_id.clone(), create_test_intent(symbol))
                .expect("queue account-gated intent");
        }
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            vec![Box::new(client)],
            control_rx,
        );
        worker.venue_to_client.insert(VenueId::BYBIT, 0);
        worker.account_to_client.insert(missing, 0);

        bind_ready_spot_admission(&mut worker, stale.clone(), 0, VenueId::BYBIT);
        worker
            .account_admissions
            .get_mut(&stale)
            .expect("stale admission")
            .readback
            .valid_until = now_micros().saturating_sub(1);

        bind_ready_spot_admission(&mut worker, disabled.clone(), 0, VenueId::BYBIT);
        worker
            .account_admissions
            .get_mut(&disabled)
            .expect("disabled admission")
            .readback
            .state = AccountReadbackState::Restricted;

        bind_ready_spot_admission(&mut worker, killed.clone(), 0, VenueId::BYBIT);
        worker
            .account_admissions
            .get_mut(&killed)
            .expect("killed admission")
            .kill_switch_active = true;

        bind_ready_spot_admission(&mut worker, limited.clone(), 0, VenueId::BYBIT);
        worker
            .account_admissions
            .get_mut(&limited)
            .expect("limited admission")
            .max_order_notional = rust_decimal::Decimal::from(99);

        bind_ready_spot_admission(&mut worker, ready, 0, VenueId::BYBIT);
        let mut queued = worker.queues.receive_envelopes();

        worker.process_order_intents(&mut queued).await;

        assert_eq!(state.lock().unwrap().placed, vec![Symbol::new("READY")]);
        let mut events = Vec::new();
        engine_queues.receive_events_into(&mut events);
        let rejects = events
            .iter()
            .filter_map(|event| match event {
                ExecutionEvent::OrderReject { reason, .. } => Some(reason.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rejects,
            vec![
                "account has no current external admission",
                "account external readback is stale or invalid",
                "external account readback is not enabled",
                "account kill switch is active",
                "account order-notional limit exceeded",
            ]
        );
    }

    #[test]
    fn private_ack_keeps_its_actual_ordering_around_sync_response() {
        let lifecycle = ports::OrderIntentLifecycle {
            timing: ports::ExecutionTiming {
                intent_emitted_mono_us: Some(10),
                risk_completed_mono_us: Some(20),
                ..Default::default()
            },
            ..ports::OrderIntentLifecycle::default()
        };
        let receipt = ports::ExecutionSubmissionAttempt {
            outcome: Ok(OrderId("client-42".to_string())),
            userspace_write_started_mono_us: Some(30),
            userspace_write_returned_mono_us: Some(35),
            response_received_mono_us: Some(50),
        };

        let mut before = ExecutionTimeline::from_lifecycle(&lifecycle, 0);
        before.apply_submission(&receipt);
        before.apply_private(ports::PrivateOrderEventKind::Ack, 40);
        assert_eq!(before.spans().write_to_response_us, Some(15));
        assert_eq!(before.spans().write_to_private_ack_us, Some(5));
        before.apply_private(ports::PrivateOrderEventKind::Report, 45);
        assert_eq!(before.spans().write_to_private_report_us, Some(10));
        assert_eq!(before.spans().intent_to_private_report_us, Some(35));

        let mut after = ExecutionTimeline::from_lifecycle(&lifecycle, 0);
        after.apply_submission(&receipt);
        after.apply_private(ports::PrivateOrderEventKind::Ack, 60);
        assert_eq!(after.spans().write_to_response_us, Some(15));
        assert_eq!(after.spans().write_to_private_ack_us, Some(25));
    }

    #[tokio::test]
    async fn ambiguous_cancel_latches_intake_and_reports_unknown_outcome() {
        let client = MockExecutionClient {
            state: Arc::new(StdMutex::new(MockExecutionState::default())),
            place_error: false,
            list_error: false,
            cancel_error: true,
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
        let order_id = OrderId("client-42".to_string());

        let report = worker
            .dispatch_cancellations(vec![CancelTarget {
                order_id: order_id.clone(),
                symbol: Symbol::new("BTCUSDT"),
                venue: None,
                account_id: None,
            }])
            .await;

        assert!(!worker.accepting_intents);
        assert!(worker.emergency_latched);
        assert!(matches!(
            report.failures.as_slice(),
            [CancelFailure {
                order_id: failed_id,
                outcome_unknown: true,
                ..
            }] if failed_id == &order_id
        ));
    }

    #[tokio::test]
    async fn duplicate_order_id_across_clients_latches_without_overwriting_first_order() {
        let first_state = Arc::new(StdMutex::new(MockExecutionState::default()));
        let second_state = Arc::new(StdMutex::new(MockExecutionState::default()));
        let clients = vec![
            Box::new(MockExecutionClient {
                state: first_state,
                place_error: false,
                list_error: false,
                cancel_error: false,
            }) as Box<dyn ExecutionClient>,
            Box::new(MockExecutionClient {
                state: second_state,
                place_error: false,
                list_error: false,
                cancel_error: false,
            }) as Box<dyn ExecutionClient>,
        ];
        let (mut engine_queues, worker_queues) =
            crate::create_execution_queues(crate::ExecutionQueueConfig::default());
        let mut first = create_test_intent("BTCUSDT");
        first.strategy_id = "binance".to_string();
        first.target_venue = Some(VenueId::BINANCE);
        let mut second = create_test_intent("ETHUSDT");
        second.strategy_id = "bitget".to_string();
        second.target_venue = Some(VenueId::BITGET);
        engine_queues
            .send_intent(AccountId("binance-main".to_string()), first)
            .expect("queue first intent");
        engine_queues
            .send_intent(AccountId("bitget-main".to_string()), second)
            .expect("queue second intent");
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let mut worker = ExecutionWorker::new(
            ExecutionWorkerConfig::default(),
            worker_queues,
            clients,
            control_rx,
        );
        bind_ready_spot_admission(
            &mut worker,
            AccountId("binance-main".to_string()),
            0,
            VenueId::BINANCE,
        );
        bind_ready_spot_admission(
            &mut worker,
            AccountId("bitget-main".to_string()),
            1,
            VenueId::BITGET,
        );
        let mut queued = worker.queues.receive_envelopes();

        worker.process_order_intents(&mut queued).await;

        let order_id = OrderId("placed".to_string());
        assert!(!worker.accepting_intents);
        assert!(worker.emergency_latched);
        assert_eq!(worker.order_to_client.get(&order_id), Some(&0));
        assert_eq!(
            worker
                .tracked_orders
                .get(&order_id)
                .map(|order| &order.symbol),
            Some(&Symbol::new("BTCUSDT"))
        );
        assert!(worker.pending_acks.contains_key(&order_id));
        assert_eq!(worker.execution_timelines.len(), 1);
        assert!(worker.execution_timelines.contains_key(&order_id));
        assert_eq!(
            worker
                .tracked_orders
                .get(&order_id)
                .and_then(|order| order.venue),
            Some(VenueId::BINANCE)
        );
        assert_eq!(
            worker
                .tracked_orders
                .get(&order_id)
                .and_then(|order| order.account_id.as_ref()),
            Some(&AccountId("binance-main".to_string()))
        );
        assert_eq!(worker.stats.orders_placed, 2);
        assert_eq!(worker.stats.orders_failed, 1);
        let mut events = Vec::new();
        engine_queues.receive_events_into(&mut events);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ExecutionEvent::OrderNew { .. }))
                .count(),
            1
        );
        assert!(matches!(
            events.as_slice(),
            [ExecutionEvent::OrderNew {
                account_id: Some(account_id),
                ..
            }] if account_id == &AccountId("binance-main".to_string())
        ));

        worker
            .execution_streams
            .push(Box::pin(futures::stream::iter(
                [
                    ExecutionEvent::Fill {
                        order_id: order_id.clone(),
                        price: Price::from_f64(100.0).unwrap(),
                        quantity: Quantity::from_f64(0.25).unwrap(),
                        timestamp: 1,
                        fill_id: "bitget-fill".to_string(),
                    },
                    ExecutionEvent::OrderCanceled {
                        order_id: order_id.clone(),
                        timestamp: 2,
                    },
                ]
                .into_iter()
                .map(|event| IndexedExecutionStreamItem::Event {
                    client_idx: 1,
                    result: Ok::<_, HftError>(event),
                }),
            )));

        assert_eq!(worker.poll_execution_events().await, 2);
        assert!(worker.emergency_latched);
        assert!(worker.stream_recovery_pending);
        assert_eq!(worker.order_to_client.get(&order_id), Some(&0));
        assert_eq!(
            worker
                .tracked_orders
                .get(&order_id)
                .map(|order| order.remaining_quantity),
            Some(Quantity::from_f64(1.0).unwrap())
        );
        assert!(worker.pending_acks.contains_key(&order_id));
        events.clear();
        engine_queues.receive_events_into(&mut events);
        assert!(events.is_empty());
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

    #[tokio::test]
    async fn private_recovery_latch_still_allows_authoritative_exchange_only_cancel() {
        let state = Arc::new(StdMutex::new(MockExecutionState {
            healthy: Some(false),
            ..Default::default()
        }));
        let client = MockExecutionClient {
            state: state.clone(),
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
        worker.accepting_intents = false;
        worker.stream_recovery_pending = true;
        worker.client_connected[0] = false;
        worker.venue_to_client.insert(VenueId::POLYMARKET, 0);
        let external_id = OrderId("external-venue-order".to_string());
        let (reply, report) = oneshot::channel();

        worker
            .handle_control_command(ControlCommand::CancelOrders {
                targets: vec![CancelTarget {
                    order_id: external_id.clone(),
                    symbol: Symbol::new("123"),
                    venue: Some(VenueId::POLYMARKET),
                    account_id: None,
                }],
                scope: CancelScope::Explicit,
                reply,
            })
            .await;

        assert!(report.await.expect("cancel report").is_complete());
        assert_eq!(state.lock().unwrap().canceled, vec![external_id]);
        assert!(!worker.accepting_intents);
        assert!(worker.stream_recovery_pending);
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
                side: Side::Buy,
                limit_price: Some(Price::from_f64(100.0).unwrap()),
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
