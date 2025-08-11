//! 高性能SPSC (Single Producer Single Consumer) Ring Buffer
//! 
//! 零拷貝、無鎖的環形緩衝區實現，針對HFT場景優化

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use crossbeam_utils::CachePadded;

/// SPSC Ring Buffer for zero-copy message passing
#[repr(align(64))] // Cache line alignment
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
    
    /// Producer: 嘗試推送單個元素
    pub fn try_push(&self, item: T) -> Result<(), T> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        
        let next_head = (head + 1) & (self.capacity - 1);
        if next_head == tail {
            return Err(item); // Buffer is full
        }
        
        unsafe {
            let slot = &mut *self.buffer.as_ptr().add(head).cast_mut();
            *slot = Some(item);
        }
        
        self.head.store(next_head, Ordering::Release);
        Ok(())
    }
    
    /// Consumer: 嘗試彈出單個元素
    pub fn try_pop(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        
        if tail == head {
            return None; // Buffer is empty
        }
        
        let item = unsafe {
            let slot = &mut *self.buffer.as_ptr().add(tail).cast_mut();
            slot.take()
        };
        
        let next_tail = (tail + 1) & (self.capacity - 1);
        self.tail.store(next_tail, Ordering::Release);
        
        item
    }
    
    /// 批次彈出多個元素（提高吞吐量）
    pub fn try_pop_batch(&self, output: &mut Vec<T>, max_items: usize) -> usize {
        let mut count = 0;
        
        while count < max_items {
            if let Some(item) = self.try_pop() {
                output.push(item);
                count += 1;
            } else {
                break;
            }
        }
        
        count
    }
    
    /// 獲取當前使用率（用於背壓控制）
    pub fn utilization(&self) -> f64 {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        
        let used = if head >= tail {
            head - tail
        } else {
            self.capacity - (tail - head)
        };
        
        used as f64 / self.capacity as f64
    }
    
    /// 檢查是否為空
    pub fn is_empty(&self) -> bool {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        head == tail
    }
    
    /// 檢查是否已滿
    pub fn is_full(&self) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let next_head = (head + 1) & (self.capacity - 1);
        next_head == tail
    }
}

unsafe impl<T: Send> Send for SpscRingBuffer<T> {}
unsafe impl<T: Send> Sync for SpscRingBuffer<T> {}

/// Producer handle for SPSC ring buffer
pub struct SpscProducer<T> {
    ring_buffer: Arc<SpscRingBuffer<T>>,
}

impl<T> SpscProducer<T> {
    pub fn send(&self, item: T) -> Result<(), T> {
        self.ring_buffer.try_push(item)
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

/// Consumer handle for SPSC ring buffer
pub struct SpscConsumer<T> {
    ring_buffer: Arc<SpscRingBuffer<T>>,
}

impl<T> SpscConsumer<T> {
    pub fn recv(&self) -> Option<T> {
        self.ring_buffer.try_pop()
    }
    
    pub fn recv_batch(&self, output: &mut Vec<T>, max_items: usize) -> usize {
        self.ring_buffer.try_pop_batch(output, max_items)
    }
    
    pub fn is_empty(&self) -> bool {
        self.ring_buffer.is_empty()
    }
    
    pub fn utilization(&self) -> f64 {
        self.ring_buffer.utilization()
    }
}

/// 創建SPSC ring buffer的生產者和消費者
pub fn spsc_ring_buffer<T>(capacity: usize) -> (SpscProducer<T>, SpscConsumer<T>) {
    let ring_buffer = Arc::new(SpscRingBuffer::new(capacity));
    
    let producer = SpscProducer {
        ring_buffer: ring_buffer.clone(),
    };
    
    let consumer = SpscConsumer {
        ring_buffer,
    };
    
    (producer, consumer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    
    #[test]
    fn test_single_threaded_operations() {
        let (producer, consumer) = spsc_ring_buffer::<i32>(8);
        
        // Test empty buffer
        assert!(consumer.is_empty());
        assert_eq!(consumer.recv(), None);
        
        // Test push/pop
        producer.send(42).unwrap();
        assert!(!consumer.is_empty());
        assert_eq!(consumer.recv(), Some(42));
        assert!(consumer.is_empty());
    }
    
    #[test]
    fn test_batch_operations() {
        let (producer, consumer) = spsc_ring_buffer::<i32>(16);
        
        // Send multiple items
        for i in 0..10 {
            producer.send(i).unwrap();
        }
        
        // Receive in batches
        let mut batch = Vec::new();
        let count = consumer.recv_batch(&mut batch, 5);
        assert_eq!(count, 5);
        assert_eq!(batch, vec![0, 1, 2, 3, 4]);
        
        batch.clear();
        let count = consumer.recv_batch(&mut batch, 10);
        assert_eq!(count, 5);
        assert_eq!(batch, vec![5, 6, 7, 8, 9]);
    }
    
    #[test]
    fn test_buffer_full_condition() {
        let (producer, consumer) = spsc_ring_buffer::<i32>(4);
        
        // Fill the buffer (capacity - 1 due to SPSC algorithm)
        for i in 0..3 {
            producer.send(i).unwrap();
        }
        
        // Buffer should now be full
        assert!(producer.is_full());
        assert!(producer.send(999).is_err());
        
        // Pop one item
        assert_eq!(consumer.recv(), Some(0));
        assert!(!producer.is_full());
        
        // Should be able to push again
        producer.send(999).unwrap();
    }
    
    #[test]
    fn test_multithreaded_producer_consumer() {
        let (producer, consumer) = spsc_ring_buffer::<i32>(1024);
        let num_items = 10000;
        
        let producer_handle = thread::spawn(move || {
            for i in 0..num_items {
                while producer.send(i).is_err() {
                    thread::yield_now();
                }
            }
        });
        
        let consumer_handle = thread::spawn(move || {
            let mut received = Vec::new();
            let mut batch = Vec::new();
            
            while received.len() < num_items {
                let count = consumer.recv_batch(&mut batch, 64);
                if count > 0 {
                    received.extend_from_slice(&batch);
                    batch.clear();
                } else {
                    thread::yield_now();
                }
            }
            received
        });
        
        producer_handle.join().unwrap();
        let received = consumer_handle.join().unwrap();
        
        assert_eq!(received.len(), num_items);
        for (i, &value) in received.iter().enumerate() {
            assert_eq!(value, i as i32);
        }
    }
    
    #[test]
    fn test_utilization_metric() {
        let (producer, consumer) = spsc_ring_buffer::<i32>(8);
        
        assert_eq!(producer.utilization(), 0.0);
        
        for i in 0..4 {
            producer.send(i).unwrap();
        }
        
        let utilization = producer.utilization();
        assert!(utilization > 0.0 && utilization <= 1.0);
        
        // Consume some items
        consumer.recv();
        consumer.recv();
        
        let new_utilization = consumer.utilization();
        assert!(new_utilization < utilization);
    }
}