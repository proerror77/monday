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
        // Memory fence to ensure data write completes before head update is visible
        // Critical for ARM/weak memory models to prevent consumer seeing stale data
        std::sync::atomic::fence(Ordering::Release);
        self.head.store(next_head, Ordering::Release);
        Ok(())
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
    fn test_basic_push_pop_and_capacity() {
        let (producer, consumer) = spsc_ring_buffer(4); // 實際可用 3
        assert!(producer.send(1).is_ok());
        assert!(producer.send(2).is_ok());
        assert!(producer.send(3).is_ok());
        assert!(producer.send(4).is_err()); // 滿載

        assert_eq!(consumer.recv(), Some(1));
        assert_eq!(consumer.recv(), Some(2));
        assert_eq!(consumer.recv(), Some(3));
        assert_eq!(consumer.recv(), None);

        // 再次寫入
        assert!(producer.send(5).is_ok());
        assert_eq!(consumer.recv(), Some(5));
    }

    #[test]
    fn test_wraparound() {
        let (producer, consumer) = spsc_ring_buffer(4); // capacity=4, usable=3

        // Fill and empty multiple times to test wraparound
        for iteration in 0..3 {
            for i in 0..3 {
                let value = iteration * 3 + i + 1;
                assert!(producer.send(value).is_ok(),
                        "Failed to send {} in iteration {}", value, iteration);
            }

            for i in 0..3 {
                let expected = iteration * 3 + i + 1;
                assert_eq!(consumer.recv(), Some(expected),
                          "Failed to recv in iteration {}", iteration);
            }

            assert!(consumer.recv().is_none(), "Buffer should be empty after consuming all");
        }
    }

    #[test]
    fn test_utilization_metrics() {
        let (producer, consumer) = spsc_ring_buffer(8);

        assert_eq!(producer.utilization(), 0.0, "Empty buffer should have 0% utilization");

        // Fill to 50% of usable capacity (3 out of 7)
        for i in 0..3 {
            assert!(producer.send(i).is_ok());
        }
        let util = producer.utilization();
        assert!(util > 0.4 && util < 0.5, "Expected ~43% utilization, got {}", util);

        // Consume one
        assert_eq!(consumer.recv(), Some(0));
        let util_after = consumer.utilization();
        assert!(util_after > 0.25 && util_after < 0.35, "Expected ~29% utilization, got {}", util_after);
    }

    #[cfg(loom)]
    #[test]
    fn loom_concurrent_push_pop() {
        loom::model(|| {
            let (producer, consumer) = spsc_ring_buffer(4);

            let prod_handle = loom::thread::spawn({
                let producer = producer.clone();
                move || {
                    // Producer sends 3 items sequentially
                    for i in 1..=3 {
                        while producer.send(i).is_err() {
                            // Spin until buffer has space
                            loom::thread::yield_now();
                        }
                    }
                }
            });

            let cons_handle = loom::thread::spawn({
                let consumer = consumer.clone();
                move || {
                    let mut items = Vec::new();
                    // Consumer tries to receive items
                    loop {
                        if let Some(item) = consumer.recv() {
                            items.push(item);
                            if items.len() >= 3 {
                                break;
                            }
                        } else {
                            loom::thread::yield_now();
                        }
                    }
                    items
                }
            });

            prod_handle.join().unwrap();
            let received = cons_handle.join().unwrap();

            // Verify SPSC invariants: all sent items received in order
            assert_eq!(received, vec![1, 2, 3], "Items must be received in FIFO order");
        });
    }

    #[cfg(loom)]
    #[test]
    fn loom_release_acquire_semantics() {
        loom::model(|| {
            // Test that Release/Acquire semantics prevent stale reads
            let (producer, consumer) = spsc_ring_buffer(4);

            let prod_handle = loom::thread::spawn({
                let producer = producer.clone();
                move || {
                    // Producer sends item with data value 42
                    while producer.send(42).is_err() {
                        loom::thread::yield_now();
                    }
                }
            });

            let cons_handle = loom::thread::spawn({
                let consumer = consumer.clone();
                move || {
                    let mut received = None;
                    // Consumer polls until it gets the item
                    loop {
                        if let Some(item) = consumer.recv() {
                            received = Some(item);
                            break;
                        }
                        loom::thread::yield_now();
                    }
                    received
                }
            });

            prod_handle.join().unwrap();
            let result = cons_handle.join().unwrap();

            // The Release fence in try_push and Acquire load in try_pop
            // guarantee that the consumer sees the correct data
            assert_eq!(result, Some(42), "Consumer must see the value that was sent");
        });
    }

    #[cfg(loom)]
    #[test]
    fn loom_wraparound_with_concurrent_access() {
        loom::model(|| {
            let (producer, consumer) = spsc_ring_buffer(4); // capacity=4, usable=3

            let prod_handle = loom::thread::spawn({
                let producer = producer.clone();
                move || {
                    // Producer pushes 6 items (wraps around twice)
                    for i in 1..=6 {
                        while producer.send(i).is_err() {
                            loom::thread::yield_now();
                        }
                    }
                }
            });

            let cons_handle = loom::thread::spawn({
                let consumer = consumer.clone();
                move || {
                    let mut items = Vec::new();
                    // Consumer pops all items
                    loop {
                        if let Some(item) = consumer.recv() {
                            items.push(item);
                            if items.len() >= 6 {
                                break;
                            }
                        } else {
                            loom::thread::yield_now();
                        }
                    }
                    items
                }
            });

            prod_handle.join().unwrap();
            let received = cons_handle.join().unwrap();

            // Verify FIFO and wraparound correctness
            assert_eq!(received, vec![1, 2, 3, 4, 5, 6],
                      "Wraparound must maintain FIFO order");
        });
    }

    #[cfg(loom)]
    #[test]
    fn loom_capacity_boundary() {
        loom::model(|| {
            // Test that capacity boundary (tail == next_head) is correctly enforced
            let (producer, consumer) = spsc_ring_buffer(4); // usable=3

            let prod_handle = loom::thread::spawn({
                let producer = producer.clone();
                move || {
                    // Fill to capacity
                    assert!(producer.send(1).is_ok());
                    assert!(producer.send(2).is_ok());
                    assert!(producer.send(3).is_ok());

                    // 4th attempt should fail (buffer full)
                    let result = producer.send(4);
                    assert!(result.is_err(), "Buffer must refuse item when full");
                }
            });

            let cons_handle = loom::thread::spawn({
                let consumer = consumer.clone();
                move || {
                    // Let producer fill the buffer first
                    loom::thread::yield_now();

                    // Now make space by consuming one
                    let item = consumer.recv();
                    assert_eq!(item, Some(1), "Should receive first item");
                }
            });

            prod_handle.join().unwrap();
            cons_handle.join().unwrap();
        });
    }

    #[cfg(loom)]
    #[test]
    fn loom_empty_buffer_handling() {
        loom::model(|| {
            // Test that consuming from empty buffer returns None safely
            let (producer, consumer) = spsc_ring_buffer(4);

            let cons_handle = loom::thread::spawn({
                let consumer = consumer.clone();
                move || {
                    // Try to receive from empty buffer
                    for _ in 0..3 {
                        assert_eq!(consumer.recv(), None, "Empty buffer must return None");
                        loom::thread::yield_now();
                    }
                }
            });

            let prod_handle = loom::thread::spawn({
                let producer = producer.clone();
                move || {
                    loom::thread::yield_now();
                    // After consumer checks empty, producer sends
                    while producer.send(99).is_err() {
                        loom::thread::yield_now();
                    }
                }
            });

            cons_handle.join().unwrap();
            prod_handle.join().unwrap();
        });
    }
}
