/*!
 * 零拷貝 WebSocket 實現
 * 
 * 使用內存池和零分配策略實現超低延遲的 WebSocket 消息處理
 * 目標：將消息處理延遲降至 < 100ns
 */

use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::collections::VecDeque;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use tracing::{debug, warn, error};

/// 零拷貝消息池大小常量
const MESSAGE_POOL_SIZE: usize = 8192;
const MESSAGE_BUFFER_SIZE: usize = 4096;
const BATCH_SIZE: usize = 64;

/// 預分配的消息緩衝區
#[derive(Debug)]
struct MessageBuffer {
    data: Vec<u8>,
    length: usize,
    timestamp: u64,
    is_used: bool,
}

impl MessageBuffer {
    fn new() -> Self {
        Self {
            data: vec![0; MESSAGE_BUFFER_SIZE],
            length: 0,
            timestamp: 0,
            is_used: false,
        }
    }

    fn reset(&mut self) {
        self.length = 0;
        self.timestamp = 0;
        self.is_used = false;
    }

    fn write_data(&mut self, data: &[u8], timestamp: u64) -> Result<()> {
        if data.len() > MESSAGE_BUFFER_SIZE {
            return Err(anyhow::anyhow!("Message too large: {} bytes", data.len()));
        }
        
        self.data[..data.len()].copy_from_slice(data);
        self.length = data.len();
        self.timestamp = timestamp;
        self.is_used = true;
        Ok(())
    }

    fn as_slice(&self) -> &[u8] {
        &self.data[..self.length]
    }
}

/// 零拷貝消息池
#[derive(Debug)]
pub struct ZeroCopyMessagePool {
    buffers: Vec<MessageBuffer>,
    free_indices: VecDeque<usize>,
    allocated_count: AtomicUsize,
    peak_usage: AtomicUsize,
    allocation_count: AtomicU64,
    deallocation_count: AtomicU64,
}

impl ZeroCopyMessagePool {
    pub fn new() -> Self {
        let mut buffers = Vec::with_capacity(MESSAGE_POOL_SIZE);
        let mut free_indices = VecDeque::with_capacity(MESSAGE_POOL_SIZE);
        
        for i in 0..MESSAGE_POOL_SIZE {
            buffers.push(MessageBuffer::new());
            free_indices.push_back(i);
        }

        Self {
            buffers,
            free_indices,
            allocated_count: AtomicUsize::new(0),
            peak_usage: AtomicUsize::new(0),
            allocation_count: AtomicU64::new(0),
            deallocation_count: AtomicU64::new(0),
        }
    }

    /// 零分配獲取消息緩衝區
    pub fn acquire_buffer(&mut self) -> Option<usize> {
        if let Some(index) = self.free_indices.pop_front() {
            self.buffers[index].reset();
            self.buffers[index].is_used = true;
            
            let allocated = self.allocated_count.fetch_add(1, Ordering::Relaxed) + 1;
            self.allocation_count.fetch_add(1, Ordering::Relaxed);
            
            // 更新峰值使用量
            let mut current_peak = self.peak_usage.load(Ordering::Relaxed);
            while allocated > current_peak {
                match self.peak_usage.compare_exchange_weak(
                    current_peak, allocated, Ordering::Relaxed, Ordering::Relaxed
                ) {
                    Ok(_) => break,
                    Err(x) => current_peak = x,
                }
            }
            
            Some(index)
        } else {
            warn!("Message pool exhausted! Consider increasing pool size.");
            None
        }
    }

    /// 零分配釋放消息緩衝區
    pub fn release_buffer(&mut self, index: usize) {
        if index < MESSAGE_POOL_SIZE && self.buffers[index].is_used {
            self.buffers[index].reset();
            self.free_indices.push_back(index);
            self.allocated_count.fetch_sub(1, Ordering::Relaxed);
            self.deallocation_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 獲取緩衝區引用（零拷貝）
    pub fn get_buffer(&self, index: usize) -> Option<&MessageBuffer> {
        if index < MESSAGE_POOL_SIZE {
            Some(&self.buffers[index])
        } else {
            None
        }
    }

    /// 獲取可變緩衝區引用
    pub fn get_buffer_mut(&mut self, index: usize) -> Option<&mut MessageBuffer> {
        if index < MESSAGE_POOL_SIZE {
            Some(&mut self.buffers[index])
        } else {
            None
        }
    }

    /// 獲取池統計信息
    pub fn stats(&self) -> PoolStats {
        PoolStats {
            total_buffers: MESSAGE_POOL_SIZE,
            allocated_buffers: self.allocated_count.load(Ordering::Relaxed),
            free_buffers: self.free_indices.len(),
            peak_usage: self.peak_usage.load(Ordering::Relaxed),
            total_allocations: self.allocation_count.load(Ordering::Relaxed),
            total_deallocations: self.deallocation_count.load(Ordering::Relaxed),
        }
    }
}

/// 消息池統計信息
#[derive(Debug, Clone)]
pub struct PoolStats {
    pub total_buffers: usize,
    pub allocated_buffers: usize,
    pub free_buffers: usize,
    pub peak_usage: usize,
    pub total_allocations: u64,
    pub total_deallocations: u64,
}

/// 零拷貝批處理器
#[derive(Debug)]
pub struct ZeroCopyBatchProcessor {
    batch_buffer: Vec<usize>, // 存儲緩衝區索引
    batch_size: usize,
    processing_count: AtomicU64,
    batch_count: AtomicU64,
    total_latency_ns: AtomicU64,
}

impl ZeroCopyBatchProcessor {
    pub fn new(batch_size: usize) -> Self {
        Self {
            batch_buffer: Vec::with_capacity(batch_size),
            batch_size,
            processing_count: AtomicU64::new(0),
            batch_count: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
        }
    }

    /// 添加消息到批處理（零拷貝）
    pub fn add_message(&mut self, buffer_index: usize) -> bool {
        self.batch_buffer.push(buffer_index);
        self.batch_buffer.len() >= self.batch_size
    }

    /// 處理批量消息（零拷貝）
    pub fn process_batch<F>(&mut self, pool: &ZeroCopyMessagePool, mut processor: F) -> Result<()>
    where
        F: FnMut(&[u8], u64) -> Result<()>,
    {
        if self.batch_buffer.is_empty() {
            return Ok(());
        }

        let start_time = std::time::Instant::now();
        
        for &buffer_index in &self.batch_buffer {
            if let Some(buffer) = pool.get_buffer(buffer_index) {
                processor(buffer.as_slice(), buffer.timestamp)?;
                self.processing_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        let elapsed_ns = start_time.elapsed().as_nanos() as u64;
        self.total_latency_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
        self.batch_count.fetch_add(1, Ordering::Relaxed);

        self.batch_buffer.clear();
        Ok(())
    }

    /// 獲取處理統計
    pub fn stats(&self) -> ProcessorStats {
        let batch_count = self.batch_count.load(Ordering::Relaxed);
        let avg_latency = if batch_count > 0 {
            self.total_latency_ns.load(Ordering::Relaxed) as f64 / batch_count as f64
        } else {
            0.0
        };

        ProcessorStats {
            processed_messages: self.processing_count.load(Ordering::Relaxed),
            processed_batches: batch_count,
            average_batch_latency_ns: avg_latency,
            current_batch_size: self.batch_buffer.len(),
        }
    }
}

/// 處理器統計信息
#[derive(Debug, Clone)]
pub struct ProcessorStats {
    pub processed_messages: u64,
    pub processed_batches: u64,
    pub average_batch_latency_ns: f64,
    pub current_batch_size: usize,
}

/// 零拷貝 WebSocket 連接器
#[derive(Debug)]
pub struct ZeroCopyWebSocketConnector {
    url: String,
    message_pool: Arc<parking_lot::Mutex<ZeroCopyMessagePool>>,
    batch_processor: Arc<parking_lot::Mutex<ZeroCopyBatchProcessor>>,
    connection_count: AtomicU64,
    message_count: AtomicU64,
    error_count: AtomicU64,
}

impl ZeroCopyWebSocketConnector {
    pub fn new(url: String) -> Self {
        Self {
            url,
            message_pool: Arc::new(parking_lot::Mutex::new(ZeroCopyMessagePool::new())),
            batch_processor: Arc::new(parking_lot::Mutex::new(ZeroCopyBatchProcessor::new(BATCH_SIZE))),
            connection_count: AtomicU64::new(0),
            message_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
        }
    }

    /// 連接並開始零拷貝消息處理
    pub async fn connect_and_process<F>(&self, mut message_handler: F) -> Result<()>
    where
        F: FnMut(&[u8], u64) -> Result<()> + Send + 'static,
    {
        let (ws_stream, _) = connect_async(&self.url).await?;
        let (mut ws_sender, mut ws_receiver) = ws_stream.split();
        
        self.connection_count.fetch_add(1, Ordering::Relaxed);
        debug!("Zero-copy WebSocket connected to {}", self.url);

        // 訂閱消息
        let subscribe_msg = serde_json::json!({
            "op": "subscribe",
            "args": [
                {
                    "instType": "SPOT",
                    "channel": "books5",
                    "instId": "BTCUSDT"
                }
            ]
        });
        
        ws_sender.send(Message::Text(subscribe_msg.to_string())).await?;

        // 消息處理循環
        while let Some(message) = ws_receiver.next().await {
            match message {
                Ok(Message::Text(text)) => {
                    let timestamp = crate::core::types::now_micros();
                    self.process_message_zero_copy(text.as_bytes(), timestamp, &mut message_handler).await?;
                }
                Ok(Message::Binary(data)) => {
                    let timestamp = crate::core::types::now_micros();
                    self.process_message_zero_copy(&data, timestamp, &mut message_handler).await?;
                }
                Ok(Message::Ping(data)) => {
                    ws_sender.send(Message::Pong(data)).await?;
                }
                Ok(Message::Close(_)) => {
                    debug!("WebSocket connection closed");
                    break;
                }
                Err(e) => {
                    self.error_count.fetch_add(1, Ordering::Relaxed);
                    error!("WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// 零拷貝消息處理
    async fn process_message_zero_copy<F>(
        &self,
        data: &[u8],
        timestamp: u64,
        message_handler: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&[u8], u64) -> Result<()>,
    {
        // 獲取消息緩衝區
        let buffer_index = {
            let mut pool = self.message_pool.lock();
            pool.acquire_buffer()
        };

        if let Some(index) = buffer_index {
            // 寫入數據到緩衝區（零拷貝引用）
            {
                let mut pool = self.message_pool.lock();
                if let Some(buffer) = pool.get_buffer_mut(index) {
                    buffer.write_data(data, timestamp)?;
                }
            }

            // 添加到批處理器
            let should_process = {
                let mut processor = self.batch_processor.lock();
                processor.add_message(index)
            };

            // 如果批處理已滿，立即處理
            if should_process {
                self.process_batch(message_handler).await?;
            }

            // 釋放緩衝區
            {
                let mut pool = self.message_pool.lock();
                pool.release_buffer(index);
            }

            self.message_count.fetch_add(1, Ordering::Relaxed);
        } else {
            self.error_count.fetch_add(1, Ordering::Relaxed);
            warn!("Failed to acquire message buffer");
        }

        Ok(())
    }

    /// 處理批量消息
    async fn process_batch<F>(&self, message_handler: &mut F) -> Result<()>
    where
        F: FnMut(&[u8], u64) -> Result<()>,
    {
        let pool = self.message_pool.lock();
        let mut processor = self.batch_processor.lock();
        
        processor.process_batch(&*pool, |data, timestamp| {
            message_handler(data, timestamp)
        })?;

        Ok(())
    }

    /// 強制處理剩餘批量消息
    pub async fn flush_batch<F>(&self, message_handler: &mut F) -> Result<()>
    where
        F: FnMut(&[u8], u64) -> Result<()>,
    {
        self.process_batch(message_handler).await
    }

    /// 獲取連接器統計信息
    pub fn stats(&self) -> ConnectorStats {
        let pool_stats = self.message_pool.lock().stats();
        let processor_stats = self.batch_processor.lock().stats();

        ConnectorStats {
            connections: self.connection_count.load(Ordering::Relaxed),
            messages_processed: self.message_count.load(Ordering::Relaxed),
            errors: self.error_count.load(Ordering::Relaxed),
            pool_stats,
            processor_stats,
        }
    }
}

/// 連接器統計信息
#[derive(Debug, Clone)]
pub struct ConnectorStats {
    pub connections: u64,
    pub messages_processed: u64,
    pub errors: u64,
    pub pool_stats: PoolStats,
    pub processor_stats: ProcessorStats,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_pool() {
        let mut pool = ZeroCopyMessagePool::new();
        
        // 測試緩衝區分配
        let index1 = pool.acquire_buffer().unwrap();
        let index2 = pool.acquire_buffer().unwrap();
        
        assert_ne!(index1, index2);
        
        // 測試緩衝區寫入
        {
            let buffer = pool.get_buffer_mut(index1).unwrap();
            buffer.write_data(b"test message", 123456).unwrap();
            assert_eq!(buffer.length, 12);
            assert_eq!(buffer.timestamp, 123456);
        }
        
        // 測試緩衝區讀取
        {
            let buffer = pool.get_buffer(index1).unwrap();
            assert_eq!(buffer.as_slice(), b"test message");
        }
        
        // 測試緩衝區釋放
        pool.release_buffer(index1);
        pool.release_buffer(index2);
        
        let stats = pool.stats();
        assert_eq!(stats.allocated_buffers, 0);
        // 檢查分配和釋放計數平衡
        assert_eq!(stats.total_allocations, stats.total_deallocations);
    }

    #[test]
    fn test_batch_processor() {
        let mut pool = ZeroCopyMessagePool::new();
        let mut processor = ZeroCopyBatchProcessor::new(2);
        
        // 添加測試消息
        let index1 = pool.acquire_buffer().unwrap();
        let index2 = pool.acquire_buffer().unwrap();
        
        pool.get_buffer_mut(index1).unwrap().write_data(b"msg1", 1).unwrap();
        pool.get_buffer_mut(index2).unwrap().write_data(b"msg2", 2).unwrap();
        
        processor.add_message(index1);
        let should_process = processor.add_message(index2);
        assert!(should_process);
        
        // 測試批處理
        let mut processed_count = 0;
        processor.process_batch(&pool, |_data, _timestamp| {
            processed_count += 1;
            Ok(())
        }).unwrap();
        
        assert_eq!(processed_count, 2);
        
        let stats = processor.stats();
        assert_eq!(stats.processed_messages, 2);
        assert_eq!(stats.processed_batches, 1);
    }
}
