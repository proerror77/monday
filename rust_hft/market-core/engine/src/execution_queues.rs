//! 执行队列系统 - SPSC 无锁队列用于引擎与执行 worker 解耦
//!
//! 架构:
//! - 引擎 -> 执行队列 (OrderIntent) -> 执行 Worker
//! - 执行 Worker -> 回报队列 (ExecutionEvent) -> 引擎
//! - 双向队列确保任何网络 await 不持有引擎锁

use crate::dataflow::ring_buffer::{spsc_ring_buffer, SpscConsumer, SpscProducer};
use hft_core::{AccountId, Timestamp};
use ports::{
    ExecutionEvent, ExecutionPriceProtection, OrderIntent, OrderIntentEnvelope,
    OrderIntentRejectReason,
};
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};
use tracing::{debug, warn};

/// 执行队列配置
#[derive(Debug, Clone)]
pub struct ExecutionQueueConfig {
    /// 意图队列容量 (power of 2)
    pub intent_queue_capacity: usize,
    /// 回报队列容量 (power of 2)
    pub event_queue_capacity: usize,
    /// 批处理大小
    pub batch_size: usize,
}

impl Default for ExecutionQueueConfig {
    fn default() -> Self {
        Self {
            intent_queue_capacity: 4096,
            event_queue_capacity: 8192,
            batch_size: 32,
        }
    }
}

/// 引擎端的队列接口
pub struct EngineQueues {
    /// 发送订单意图给执行 worker
    intent_producer: SpscProducer<OrderIntentEnvelope>,
    /// 接收执行回报从执行 worker
    event_consumer: SpscConsumer<ExecutionEvent>,
    config: ExecutionQueueConfig,
    stats: QueueStats,
    intent_notify: Arc<Notify>,
    event_space_notify: Arc<Notify>,
    applied_stream_tx: mpsc::UnboundedSender<u64>,
    applied_stream_notify: Arc<Notify>,
}

/// 执行 Worker 端的队列接口
pub struct WorkerQueues {
    /// 接收订单意图从引擎
    intent_consumer: SpscConsumer<OrderIntentEnvelope>,
    /// 发送执行回报给引擎
    event_producer: SpscProducer<ExecutionEvent>,
    config: ExecutionQueueConfig,
    stats: QueueStats,
    /// 引擎唤醒通知器
    engine_notify: Option<Arc<Notify>>,
    intent_notify: Arc<Notify>,
    event_space_notify: Arc<Notify>,
    applied_stream_rx: mpsc::UnboundedReceiver<u64>,
    applied_stream_notify: Arc<Notify>,
    market_reader: Option<Arc<dyn snapshot::SnapshotReader<crate::aggregation::MarketView>>>,
}

/// 队列统计
#[derive(Debug, Default, Clone, Copy)]
pub struct QueueStats {
    pub intents_sent: u64,
    pub intents_received: u64,
    pub events_sent: u64,
    pub events_received: u64,
    pub intent_queue_full_count: u64,
    pub event_queue_full_count: u64,
    pub intent_lifecycle_rejected_count: u64,
    pub intent_expired_count: u64,
    pub intent_stale_count: u64,
    pub intent_max_latency_count: u64,
    pub intent_order_notional_count: u64,
    pub intent_order_quantity_count: u64,
    pub intent_slippage_count: u64,
}

/// 帶生命週期 envelope 的意圖提交失敗原因。
#[derive(Debug, Clone)]
pub enum LifecycleIntentSubmitError {
    LifecycleRejected {
        envelope: Box<OrderIntentEnvelope>,
        reason: OrderIntentRejectReason,
    },
    QueueFull {
        envelope: Box<OrderIntentEnvelope>,
    },
}

/// 创建引擎和 Worker 队列对
pub fn create_execution_queues(config: ExecutionQueueConfig) -> (EngineQueues, WorkerQueues) {
    let (intent_producer, intent_consumer) = spsc_ring_buffer(config.intent_queue_capacity);
    let (event_producer, event_consumer) = spsc_ring_buffer(config.event_queue_capacity);
    let intent_notify = Arc::new(Notify::new());
    let event_space_notify = Arc::new(Notify::new());
    let applied_stream_notify = Arc::new(Notify::new());
    let (applied_stream_tx, applied_stream_rx) = mpsc::unbounded_channel();

    let engine_queues = EngineQueues {
        intent_producer,
        event_consumer,
        config: config.clone(),
        stats: QueueStats::default(),
        intent_notify: Arc::clone(&intent_notify),
        event_space_notify: Arc::clone(&event_space_notify),
        applied_stream_tx,
        applied_stream_notify: Arc::clone(&applied_stream_notify),
    };

    let worker_queues = WorkerQueues {
        intent_consumer,
        event_producer,
        config,
        stats: QueueStats::default(),
        engine_notify: None,
        intent_notify,
        event_space_notify,
        applied_stream_rx,
        applied_stream_notify,
        market_reader: None,
    };

    (engine_queues, worker_queues)
}

impl EngineQueues {
    /// 发送订单意图到执行 worker (非阻塞)
    #[allow(clippy::result_large_err)] // Returning ownership avoids a heap allocation on the hot path.
    pub fn send_intent(
        &mut self,
        account_id: AccountId,
        intent: OrderIntent,
    ) -> Result<(), OrderIntent> {
        let now = hft_core::now_micros();
        let envelope = OrderIntentEnvelope::new(
            intent,
            ports::OrderIntentLifecycle::new(now, Timestamp::MAX),
        )
        .with_account_id(account_id);
        self.send_envelope(envelope)
            .map_err(OrderIntentEnvelope::into_inner)
    }

    /// Send a fully qualified intent without stripping lifecycle or idempotency metadata.
    #[allow(clippy::result_large_err)] // Returning ownership avoids a heap allocation on the hot path.
    pub fn send_envelope(
        &mut self,
        envelope: OrderIntentEnvelope,
    ) -> Result<(), OrderIntentEnvelope> {
        match self.intent_producer.send(envelope) {
            Ok(()) => {
                self.stats.intents_sent += 1;
                self.intent_notify.notify_one();
                Ok(())
            }
            Err(envelope) => {
                self.stats.intent_queue_full_count += 1;
                warn!(
                    "意图队列满载，丢弃订单: {} {}",
                    envelope.intent.symbol.as_str(),
                    envelope.intent.quantity.0
                );
                Err(envelope)
            }
        }
    }

    /// 生命週期 gate 後再提交訂單意圖到執行 worker。
    ///
    /// 這是 pre-execution gate：過期、來源 book 已過時、或超過策略允許本地
    /// 等待時間的 intent 不會進入執行隊列。
    pub fn send_lifecycle_intent(
        &mut self,
        envelope: OrderIntentEnvelope,
        now: Timestamp,
    ) -> Result<(), LifecycleIntentSubmitError> {
        self.send_lifecycle_intent_with_book_seq(envelope, now, None)
    }

    /// 帶最新 book sequence 的 pre-execution gate。
    pub fn send_lifecycle_intent_with_book_seq(
        &mut self,
        envelope: OrderIntentEnvelope,
        now: Timestamp,
        latest_book_seq: Option<u64>,
    ) -> Result<(), LifecycleIntentSubmitError> {
        self.send_lifecycle_intent_with_price_protection(
            envelope,
            now,
            latest_book_seq,
            ExecutionPriceProtection::CanonicalBook,
        )
    }

    pub fn send_lifecycle_intent_with_price_protection(
        &mut self,
        envelope: OrderIntentEnvelope,
        now: Timestamp,
        latest_book_seq: Option<u64>,
        price_protection: ExecutionPriceProtection,
    ) -> Result<(), LifecycleIntentSubmitError> {
        let validation = match price_protection {
            ExecutionPriceProtection::CanonicalBook => {
                envelope.validate_cex_pre_execution(now, latest_book_seq)
            }
            ExecutionPriceProtection::VenueQuote => {
                envelope.validate_pre_execution(now, latest_book_seq)
            }
        };
        match validation {
            Ok(()) => self.send_envelope(envelope).map_err(|envelope| {
                LifecycleIntentSubmitError::QueueFull {
                    envelope: Box::new(envelope),
                }
            }),
            Err(reason) => {
                self.stats.intent_lifecycle_rejected_count += 1;
                match reason {
                    OrderIntentRejectReason::Expired { .. } => {
                        self.stats.intent_expired_count += 1;
                    }
                    OrderIntentRejectReason::SourceBookStale { .. }
                    | OrderIntentRejectReason::SourceBookUnavailable => {
                        self.stats.intent_stale_count += 1;
                    }
                    OrderIntentRejectReason::MaxLatencyExceeded { .. } => {
                        self.stats.intent_max_latency_count += 1;
                    }
                    OrderIntentRejectReason::InvalidMaxOrderNotional { .. }
                    | OrderIntentRejectReason::MissingMaxOrderNotional
                    | OrderIntentRejectReason::OrderNotionalUnpriceable { .. }
                    | OrderIntentRejectReason::MaxOrderNotionalExceeded { .. } => {
                        self.stats.intent_order_notional_count += 1;
                    }
                    OrderIntentRejectReason::InvalidMaxOrderQuantity { .. }
                    | OrderIntentRejectReason::MaxOrderQuantityExceeded { .. } => {
                        self.stats.intent_order_quantity_count += 1;
                    }
                    OrderIntentRejectReason::InvalidMaxSlippage { .. }
                    | OrderIntentRejectReason::MissingMaxSlippage
                    | OrderIntentRejectReason::MissingSlippageReference
                    | OrderIntentRejectReason::SlippageReferenceMismatch
                    | OrderIntentRejectReason::MissingSlippageReferenceLifetime
                    | OrderIntentRejectReason::SlippageReferenceExpired { .. }
                    | OrderIntentRejectReason::SlippageUnprotectedOrder
                    | OrderIntentRejectReason::MaxSlippageExceeded { .. } => {
                        self.stats.intent_slippage_count += 1;
                    }
                }
                Err(LifecycleIntentSubmitError::LifecycleRejected {
                    envelope: Box::new(envelope),
                    reason,
                })
            }
        }
    }

    /// 接收执行回报到提供的緩衝 (非阻塞批量)
    pub fn receive_events_into(&mut self, buffer: &mut Vec<ExecutionEvent>) {
        let mut count = 0;

        while count < self.config.batch_size {
            match self.event_consumer.recv() {
                Some(event) => {
                    buffer.push(event);
                    self.stats.events_received += 1;
                    count += 1;
                }
                None => break,
            }
        }

        if count > 0 {
            self.event_space_notify.notify_one();
            debug!("引擎接收到 {} 个执行回报", buffer.len());
        }
    }

    /// 检查意图队列利用率
    pub fn intent_queue_utilization(&self) -> f64 {
        self.intent_producer.utilization()
    }

    /// 检查回报队列利用率
    pub fn event_queue_utilization(&self) -> f64 {
        self.event_consumer.utilization()
    }

    /// 获取统计信息
    pub fn stats(&self) -> &QueueStats {
        &self.stats
    }

    /// Intent 隊列容量（監控用）
    pub fn intent_queue_capacity(&self) -> usize {
        self.config.intent_queue_capacity
    }

    /// Event 隊列容量（監控用）
    pub fn event_queue_capacity(&self) -> usize {
        self.config.event_queue_capacity
    }

    /// A stream synchronization marker is acknowledged only after the engine has applied it and
    /// every earlier FIFO execution report to OMS, portfolio, and risk state.
    pub fn acknowledge_applied_execution_stream(&self, stream_id: u64) {
        if self.applied_stream_tx.send(stream_id).is_ok() {
            self.applied_stream_notify.notify_one();
        } else {
            warn!(
                stream_id,
                "execution worker dropped before stream-application acknowledgement"
            );
        }
    }
}

impl WorkerQueues {
    pub fn set_market_reader(
        &mut self,
        reader: Arc<dyn snapshot::SnapshotReader<crate::aggregation::MarketView>>,
    ) {
        self.market_reader = Some(reader);
    }

    /// Reload the existing canonical snapshot immediately before adapter entry. Never trust
    /// only a quote that travelled with an intent through an asynchronous execution queue.
    pub fn validate_current_market_reference(
        &self,
        envelope: &OrderIntentEnvelope,
        now: Timestamp,
    ) -> Result<(), OrderIntentRejectReason> {
        self.validate_execution_market_reference(
            envelope,
            now,
            ExecutionPriceProtection::CanonicalBook,
        )
    }

    pub fn validate_execution_market_reference(
        &self,
        envelope: &OrderIntentEnvelope,
        now: Timestamp,
        price_protection: ExecutionPriceProtection,
    ) -> Result<(), OrderIntentRejectReason> {
        let market = self.market_reader.as_ref().map(|reader| reader.load());
        if price_protection == ExecutionPriceProtection::CanonicalBook
            && envelope.lifecycle.source_book_seq.is_some()
            && envelope.source_book_identity.is_none()
        {
            return Err(OrderIntentRejectReason::SourceBookUnavailable);
        }
        let source_sequence = match &envelope.source_book_identity {
            Some(identity) => Some(
                market
                    .as_ref()
                    .and_then(|view| view.get_orderbook(identity))
                    .ok_or(OrderIntentRejectReason::SourceBookUnavailable)?
                    .sequence,
            ),
            None => None,
        };
        envelope.validate_pre_execution(now, source_sequence)?;
        if price_protection == ExecutionPriceProtection::VenueQuote {
            return Ok(());
        }
        envelope.validate_slippage_reference(now, envelope.price_reference.as_ref())?;
        let reference = market
            .as_ref()
            .and_then(|view| view.execution_price_reference(&envelope.intent));
        envelope.validate_slippage_reference(now, reference.as_ref())
    }

    /// 设置引擎唤醒通知器
    pub fn set_engine_notify(&mut self, notify: Arc<Notify>) {
        self.engine_notify = Some(notify);
    }

    /// Receive lifecycle-qualified order intents (non-blocking batch).
    pub fn receive_envelopes(&mut self) -> Vec<OrderIntentEnvelope> {
        let mut envelopes = Vec::with_capacity(self.config.batch_size);
        self.receive_envelopes_into(&mut envelopes);
        envelopes
    }

    pub fn receive_envelopes_into(&mut self, envelopes: &mut Vec<OrderIntentEnvelope>) {
        let mut count = 0;

        while count < self.config.batch_size {
            match self.intent_consumer.recv() {
                Some(envelope) => {
                    envelopes.push(envelope);
                    self.stats.intents_received += 1;
                    count += 1;
                }
                None => break,
            }
        }

        if count > 0 {
            debug!("执行 Worker 接收到 {} 个订单意图", count);
        }
    }

    /// Compatibility helper for tests and non-live callers.
    pub fn receive_intents(&mut self) -> Vec<OrderIntent> {
        self.receive_envelopes()
            .into_iter()
            .map(OrderIntentEnvelope::into_inner)
            .collect()
    }

    pub fn intent_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.intent_notify)
    }

    pub fn applied_stream_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.applied_stream_notify)
    }

    pub fn try_receive_applied_execution_stream(&mut self) -> Option<u64> {
        self.applied_stream_rx.try_recv().ok()
    }

    /// 发送执行回报到引擎 (非阻塞)
    pub fn send_event(&mut self, event: ExecutionEvent) -> Result<(), Box<ExecutionEvent>> {
        match self.event_producer.send(event) {
            Ok(()) => {
                self.stats.events_sent += 1;
                // 唤醒引擎处理新的执行事件
                if let Some(notify) = &self.engine_notify {
                    notify.notify_one();
                }
                Ok(())
            }
            Err(event) => {
                self.stats.event_queue_full_count += 1;
                warn!("回报队列满载，等待引擎释放空间: {:?}", event);
                Err(Box::new(event))
            }
        }
    }

    /// Execution reports are lossless: backpressure waits for the engine to drain the SPSC ring.
    pub async fn send_event_reliable(&mut self, mut event: ExecutionEvent) {
        loop {
            match self.send_event(event) {
                Ok(()) => return,
                Err(rejected) => {
                    event = *rejected;
                    self.event_space_notify.notified().await;
                }
            }
        }
    }

    /// 检查是否有待处理的意图
    pub fn has_pending_intents(&self) -> bool {
        !self.intent_consumer.is_empty()
    }

    /// 检查意图队列利用率
    pub fn intent_queue_utilization(&self) -> f64 {
        self.intent_consumer.utilization()
    }

    /// 检查回报队列利用率
    pub fn event_queue_utilization(&self) -> f64 {
        self.event_producer.utilization()
    }

    /// 获取统计信息
    pub fn stats(&self) -> &QueueStats {
        &self.stats
    }
}
