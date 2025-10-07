//! 执行队列系统 - SPSC 无锁队列用于引擎与执行 worker 解耦
//!
//! 架构:
//! - 引擎 -> 执行队列 (OrderIntent) -> 执行 Worker
//! - 执行 Worker -> 回报队列 (ExecutionEvent) -> 引擎
//! - 双向队列确保任何网络 await 不持有引擎锁

use crate::dataflow::ring_buffer::{spsc_ring_buffer, SpscConsumer, SpscProducer};
use ports::{ExecutionEvent, OrderIntent};
use std::sync::Arc;
use tokio::sync::Notify;
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
    intent_producer: SpscProducer<OrderIntent>,
    /// 接收执行回报从执行 worker
    event_consumer: SpscConsumer<ExecutionEvent>,
    config: ExecutionQueueConfig,
    stats: QueueStats,
}

/// 执行 Worker 端的队列接口
pub struct WorkerQueues {
    /// 接收订单意图从引擎
    intent_consumer: SpscConsumer<OrderIntent>,
    /// 发送执行回报给引擎
    event_producer: SpscProducer<ExecutionEvent>,
    config: ExecutionQueueConfig,
    stats: QueueStats,
    /// 引擎唤醒通知器
    engine_notify: Option<Arc<Notify>>,
}

/// 队列统计
#[derive(Debug, Default)]
pub struct QueueStats {
    pub intents_sent: u64,
    pub intents_received: u64,
    pub events_sent: u64,
    pub events_received: u64,
    pub intent_queue_full_count: u64,
    pub event_queue_full_count: u64,
}

/// 创建引擎和 Worker 队列对
pub fn create_execution_queues(config: ExecutionQueueConfig) -> (EngineQueues, WorkerQueues) {
    let (intent_producer, intent_consumer) = spsc_ring_buffer(config.intent_queue_capacity);
    let (event_producer, event_consumer) = spsc_ring_buffer(config.event_queue_capacity);

    let engine_queues = EngineQueues {
        intent_producer,
        event_consumer,
        config: config.clone(),
        stats: QueueStats::default(),
    };

    let worker_queues = WorkerQueues {
        intent_consumer,
        event_producer,
        config,
        stats: QueueStats::default(),
        engine_notify: None,
    };

    (engine_queues, worker_queues)
}

impl EngineQueues {
    /// 发送订单意图到执行 worker (非阻塞)
    pub fn send_intent(&mut self, intent: OrderIntent) -> Result<(), OrderIntent> {
        match self.intent_producer.send(intent) {
            Ok(()) => {
                self.stats.intents_sent += 1;
                Ok(())
            }
            Err(intent) => {
                self.stats.intent_queue_full_count += 1;
                warn!(
                    "意图队列满载，丢弃订单: {} {}",
                    intent.symbol.0, intent.quantity.0
                );
                Err(intent)
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

        if !buffer.is_empty() {
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
}

impl WorkerQueues {
    /// 设置引擎唤醒通知器
    pub fn set_engine_notify(&mut self, notify: Arc<Notify>) {
        self.engine_notify = Some(notify);
    }

    /// 接收订单意图 (非阻塞批量)
    pub fn receive_intents(&mut self) -> Vec<OrderIntent> {
        let mut intents = Vec::new();
        let mut count = 0;

        while count < self.config.batch_size {
            match self.intent_consumer.recv() {
                Some(intent) => {
                    intents.push(intent);
                    self.stats.intents_received += 1;
                    count += 1;
                }
                None => break,
            }
        }

        if !intents.is_empty() {
            debug!("执行 Worker 接收到 {} 个订单意图", intents.len());
        }

        intents
    }

    /// 发送执行回报到引擎 (非阻塞)
    pub fn send_event(&mut self, event: ExecutionEvent) -> Result<(), ExecutionEvent> {
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
                warn!("回报队列满载，丢弃事件: {:?}", event);
                Err(event)
            }
        }
    }

    /// 强制发送回报 (使用 force_send)
    pub fn send_event_force(&mut self, event: ExecutionEvent) -> bool {
        if self.event_producer.force_send(event) {
            self.stats.events_sent += 1;
            // 唤醒引擎处理新的执行事件
            if let Some(notify) = &self.engine_notify {
                notify.notify_one();
            }
            true
        } else {
            self.stats.event_queue_full_count += 1;
            warn!("回报队列 force_send 失败");
            false
        }
    }

    /// 批量发送执行回报
    pub fn send_events_batch(&mut self, events: Vec<ExecutionEvent>) -> Vec<ExecutionEvent> {
        let mut failed = Vec::new();

        for event in events {
            if let Err(failed_event) = self.send_event(event) {
                failed.push(failed_event);
            }
        }

        failed
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
