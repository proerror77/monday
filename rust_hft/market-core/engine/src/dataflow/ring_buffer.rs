//! 高性能SPSC (Single Producer Single Consumer) Ring Buffer（從 src/ 平移骨架）
use crossbeam_utils::CachePadded;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[repr(align(64))]
pub struct SpscRingBuffer<T> {
    buffer: Box<[Option<T>]>,
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    capacity: usize,
}

impl<T> SpscRingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        let actual_capacity = capacity.next_power_of_two();
        let mut buffer = Vec::with_capacity(actual_capacity);
        for _ in 0..actual_capacity {
            buffer.push(None);
        }
        Self {
            buffer: buffer.into_boxed_slice(),
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
            capacity: actual_capacity,
        }
    }
    pub fn try_push(&self, item: T) -> Result<(), T> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let next_head = (head + 1) & (self.capacity - 1);
        if next_head == tail {
            return Err(item);
        }
        unsafe {
            let slot = &mut *self.buffer.as_ptr().add(head).cast_mut();
            *slot = Some(item);
        }
        self.head.store(next_head, Ordering::Release);
        Ok(())
    }

    /// Force push - 强制写入策略，用于最新事件优先的场景
    ///
    /// # Last-Wins 语义
    ///
    /// - 如果队列未满：行为与 `try_push` 一致，直接插入
    /// - 如果队列已满：**丢弃最旧的元素**，为新元素腾出空间
    ///
    /// # 重要特性
    ///
    /// - **非 FIFO 语义**: 在满载情况下会跳过（丢弃）旧元素
    /// - **无阻塞**: 保证插入成功，适用于热路径
    /// - **最新优先**: 确保最新的数据能够被处理
    ///
    /// # 适用场景
    ///
    /// - 交易信号队列：最新信号比旧信号更重要
    /// - 市场数据流：最新价格比历史价格更关键
    /// - 告警事件：最新告警应该优先处理
    ///
    /// # 返回值
    ///
    /// 总是返回 `true`，表示插入成功
    ///
    /// # 示例
    ///
    /// ```
    /// // 创建容量为 3 的队列
    /// let (producer, consumer) = spsc_ring_buffer(4); // 实际容量 3 (power-of-2 - 1)
    ///
    /// // 填满队列
    /// assert!(producer.try_push(1).is_ok());
    /// assert!(producer.try_push(2).is_ok());
    /// assert!(producer.try_push(3).is_ok());
    ///
    /// // 队列已满，普通 push 失败
    /// assert!(producer.try_push(4).is_err());
    ///
    /// // force_push 总是成功，丢弃最旧元素
    /// assert!(producer.force_push(4));
    ///
    /// // 现在读取：跳过了 1，从 2 开始
    /// assert_eq!(consumer.recv(), Some(2));
    /// assert_eq!(consumer.recv(), Some(3));
    /// assert_eq!(consumer.recv(), Some(4));
    /// ```
    pub fn force_push(&self, item: T) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let next_head = (head + 1) & (self.capacity - 1);

        // 如果滿了，強制移動 tail 指針（丟棄最舊元素）
        if next_head == tail {
            // 先清理舊元素
            unsafe {
                let slot = &mut *self.buffer.as_ptr().add(tail).cast_mut();
                *slot = None; // 清理舊數據
            }

            // 移動 tail 指針，丟棄最舊元素
            let next_tail = (tail + 1) & (self.capacity - 1);
            self.tail.store(next_tail, Ordering::Release);
        }

        // 現在有空間了，插入新元素
        unsafe {
            let slot = &mut *self.buffer.as_ptr().add(head).cast_mut();
            *slot = Some(item);
        }
        self.head.store(next_head, Ordering::Release);
        true
    }
    pub fn try_pop(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        let item = unsafe {
            let slot = &mut *self.buffer.as_ptr().add(tail).cast_mut();
            slot.take()
        };
        let next_tail = (tail + 1) & (self.capacity - 1);
        self.tail.store(next_tail, Ordering::Release);
        item
    }
    pub fn utilization(&self) -> f64 {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        let used = if head >= tail {
            head - tail
        } else {
            self.capacity - (tail - head)
        };
        let max_usable = self.capacity - 1; // Ring buffer reserves one slot to distinguish full vs empty
        used as f64 / max_usable as f64
    }
    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) == self.tail.load(Ordering::Acquire)
    }
    pub fn is_full(&self) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let next_head = (head + 1) & (self.capacity - 1);
        next_head == tail
    }
}

unsafe impl<T: Send> Send for SpscRingBuffer<T> {}
unsafe impl<T: Send> Sync for SpscRingBuffer<T> {}

pub struct SpscProducer<T> {
    ring_buffer: Arc<SpscRingBuffer<T>>,
}
impl<T> SpscProducer<T> {
    pub fn send(&self, item: T) -> Result<(), T> {
        self.ring_buffer.try_push(item)
    }
    pub fn force_send(&self, item: T) -> bool {
        self.ring_buffer.force_push(item)
    }
    pub fn is_full(&self) -> bool {
        self.ring_buffer.is_full()
    }
    pub fn utilization(&self) -> f64 {
        self.ring_buffer.utilization()
    }
}
impl<T> Clone for SpscProducer<T> {
    fn clone(&self) -> Self {
        Self {
            ring_buffer: self.ring_buffer.clone(),
        }
    }
}

pub struct SpscConsumer<T> {
    ring_buffer: Arc<SpscRingBuffer<T>>,
}
impl<T> SpscConsumer<T> {
    pub fn recv(&self) -> Option<T> {
        self.ring_buffer.try_pop()
    }
    pub fn is_empty(&self) -> bool {
        self.ring_buffer.is_empty()
    }
    pub fn utilization(&self) -> f64 {
        self.ring_buffer.utilization()
    }
}

pub fn spsc_ring_buffer<T>(capacity: usize) -> (SpscProducer<T>, SpscConsumer<T>) {
    let ring_buffer = Arc::new(SpscRingBuffer::new(capacity));
    (
        SpscProducer {
            ring_buffer: ring_buffer.clone(),
        },
        SpscConsumer { ring_buffer },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_force_push_basic() {
        let (producer, consumer) = spsc_ring_buffer(4); // 实际容量 3

        // 正常情况：队列未满
        assert!(producer.force_send(1));
        assert!(producer.force_send(2));
        assert!(producer.force_send(3));

        assert_eq!(consumer.recv(), Some(1));
        assert_eq!(consumer.recv(), Some(2));
        assert_eq!(consumer.recv(), Some(3));
    }

    #[test]
    fn test_force_push_overflow_last_wins() {
        let (producer, consumer) = spsc_ring_buffer(4); // 实际容量 3

        // 填满队列
        assert!(producer.send(1).is_ok());
        assert!(producer.send(2).is_ok());
        assert!(producer.send(3).is_ok());

        // 队列已满，普通 push 失败
        assert!(producer.send(4).is_err());

        // force_push 成功，丢弃最旧元素 (1)
        assert!(producer.force_send(4));

        // 验证 Last-Wins 语义：元素 1 被丢弃
        assert_eq!(consumer.recv(), Some(2));
        assert_eq!(consumer.recv(), Some(3));
        assert_eq!(consumer.recv(), Some(4));
        assert_eq!(consumer.recv(), None);
    }

    #[test]
    fn test_force_push_multiple_overflow() {
        let (producer, consumer) = spsc_ring_buffer(4); // 实际容量 3

        // 填满队列
        producer.send(1).unwrap();
        producer.send(2).unwrap();
        producer.send(3).unwrap();

        // 连续 force_push，每次都丢弃最旧元素
        assert!(producer.force_send(4)); // 丢弃 1
        assert!(producer.force_send(5)); // 丢弃 2
        assert!(producer.force_send(6)); // 丢弃 3

        // 验证：只保留最新的 3 个元素
        assert_eq!(consumer.recv(), Some(4));
        assert_eq!(consumer.recv(), Some(5));
        assert_eq!(consumer.recv(), Some(6));
        assert_eq!(consumer.recv(), None);
    }

    #[test]
    fn test_force_push_interleaved_with_consume() {
        let (producer, consumer) = spsc_ring_buffer(4);

        // 填满队列
        producer.send(1).unwrap();
        producer.send(2).unwrap();
        producer.send(3).unwrap();

        // 消费一个元素，腾出空间
        assert_eq!(consumer.recv(), Some(1));

        // 现在 force_push 不会丢弃元素
        assert!(producer.force_send(4));

        // 验证顺序保持
        assert_eq!(consumer.recv(), Some(2));
        assert_eq!(consumer.recv(), Some(3));
        assert_eq!(consumer.recv(), Some(4));
    }

    #[test]
    fn test_force_push_always_succeeds() {
        let (producer, _) = spsc_ring_buffer(4);

        // force_push 总是返回 true
        for i in 0..100 {
            assert!(producer.force_send(i));
        }
    }

    #[test]
    fn test_force_push_utilization() {
        let (producer, consumer) = spsc_ring_buffer(8); // 实际容量 7

        // 填满队列
        for i in 0..7 {
            producer.send(i).unwrap();
        }

        // 验证队列已满
        assert!(producer.is_full());
        assert!((producer.utilization() - 1.0).abs() < 0.01); // 允许浮点误差

        // force_push 保持队列满状态
        producer.force_send(100);
        assert!(producer.is_full());
        assert_eq!(producer.utilization(), 1.0);

        // 消费一个元素
        consumer.recv();
        assert!(!producer.is_full());
        assert!(producer.utilization() < 1.0);
    }

    #[test]
    fn test_force_push_ordering_guarantees() {
        let (producer, consumer) = spsc_ring_buffer(4);

        // 场景：快速生产，慢消费
        producer.send(1).unwrap();
        producer.send(2).unwrap();
        producer.send(3).unwrap();

        // force_push 新元素
        producer.force_send(4);
        producer.force_send(5);
        producer.force_send(6);

        // 验证最新的 3 个元素按顺序保留
        let mut results = Vec::new();
        while let Some(item) = consumer.recv() {
            results.push(item);
        }

        assert_eq!(results, vec![4, 5, 6]);
    }

    #[test]
    fn test_force_push_empty_queue() {
        let (producer, consumer) = spsc_ring_buffer(4);

        // 空队列上的 force_push
        assert!(producer.force_send(42));
        assert_eq!(consumer.recv(), Some(42));
    }
}
