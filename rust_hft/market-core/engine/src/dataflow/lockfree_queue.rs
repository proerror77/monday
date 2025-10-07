//! 無鎖MPMC隊列（從 src/ 平移骨架）
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::Arc;

struct Node<T> {
    data: MaybeUninit<T>,
    next: AtomicPtr<Node<T>>,
}
impl<T> Node<T> {
    fn new() -> Self {
        Self {
            data: MaybeUninit::uninit(),
            next: AtomicPtr::new(ptr::null_mut()),
        }
    }
    fn new_with_data(data: T) -> Self {
        Self {
            data: MaybeUninit::new(data),
            next: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

pub struct LockFreeQueue<T> {
    head: AtomicPtr<Node<T>>,
    tail: AtomicPtr<Node<T>>,
    size: AtomicUsize,
}
impl<T> LockFreeQueue<T> {
    pub fn new() -> Self {
        let dummy = Box::into_raw(Box::new(Node::new()));
        Self {
            head: AtomicPtr::new(dummy),
            tail: AtomicPtr::new(dummy),
            size: AtomicUsize::new(0),
        }
    }
    pub fn enqueue(&self, data: T) {
        let new_node = Box::into_raw(Box::new(Node::new_with_data(data)));
        loop {
            let tail = self.tail.load(Ordering::Acquire);
            let next = unsafe { (*tail).next.load(Ordering::Acquire) };
            if tail == self.tail.load(Ordering::Acquire) {
                if next.is_null() {
                    if unsafe {
                        (*tail)
                            .next
                            .compare_exchange_weak(
                                next,
                                new_node,
                                Ordering::Release,
                                Ordering::Relaxed,
                            )
                            .is_ok()
                    } {
                        let _ = self.tail.compare_exchange_weak(
                            tail,
                            new_node,
                            Ordering::Release,
                            Ordering::Relaxed,
                        );
                        break;
                    }
                } else {
                    let _ = self.tail.compare_exchange_weak(
                        tail,
                        next,
                        Ordering::Release,
                        Ordering::Relaxed,
                    );
                }
            }
        }
        self.size.fetch_add(1, Ordering::Relaxed);
    }
    pub fn dequeue(&self) -> Option<T> {
        loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail.load(Ordering::Acquire);
            let next = unsafe { (*head).next.load(Ordering::Acquire) };
            if head == self.head.load(Ordering::Acquire) {
                if head == tail {
                    if next.is_null() {
                        return None;
                    }
                    let _ = self.tail.compare_exchange_weak(
                        tail,
                        next,
                        Ordering::Release,
                        Ordering::Relaxed,
                    );
                } else {
                    if next.is_null() {
                        continue;
                    }
                    let data = unsafe { (*next).data.assume_init_read() };
                    if self
                        .head
                        .compare_exchange_weak(head, next, Ordering::Release, Ordering::Relaxed)
                        .is_ok()
                    {
                        unsafe {
                            let _ = Box::from_raw(head);
                        }
                        self.size.fetch_sub(1, Ordering::Relaxed);
                        return Some(data);
                    }
                }
            }
        }
    }
    pub fn len(&self) -> usize {
        self.size.load(Ordering::Relaxed)
    }
    pub fn is_empty(&self) -> bool {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        let next = unsafe { (*head).next.load(Ordering::Acquire) };
        head == tail && next.is_null()
    }
}

impl<T> Drop for LockFreeQueue<T> {
    fn drop(&mut self) {
        while self.dequeue().is_some() {}
        let head = self.head.load(Ordering::Relaxed);
        unsafe {
            let _ = Box::from_raw(head);
        }
    }
}

unsafe impl<T: Send> Send for LockFreeQueue<T> {}
unsafe impl<T: Send> Sync for LockFreeQueue<T> {}

pub struct BoundedLockFreeQueue<T> {
    inner: LockFreeQueue<T>,
    max_size: usize,
}
impl<T> BoundedLockFreeQueue<T> {
    pub fn new(max_size: usize) -> Self {
        Self {
            inner: LockFreeQueue::new(),
            max_size,
        }
    }
    pub fn try_enqueue(&self, data: T) -> Result<(), T> {
        if self.inner.len() >= self.max_size {
            Err(data)
        } else {
            self.inner.enqueue(data);
            Ok(())
        }
    }
    pub fn dequeue(&self) -> Option<T> {
        self.inner.dequeue()
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    pub fn is_full(&self) -> bool {
        self.inner.len() >= self.max_size
    }
    pub fn utilization(&self) -> f64 {
        self.inner.len() as f64 / self.max_size as f64
    }
}

#[derive(Clone)]
pub struct MpmcProducer<T> {
    queue: Arc<BoundedLockFreeQueue<T>>,
}
impl<T> MpmcProducer<T> {
    pub fn send(&self, data: T) -> Result<(), T> {
        self.queue.try_enqueue(data)
    }
    pub fn is_full(&self) -> bool {
        self.queue.is_full()
    }
    pub fn utilization(&self) -> f64 {
        self.queue.utilization()
    }
}

#[derive(Clone)]
pub struct MpmcConsumer<T> {
    queue: Arc<BoundedLockFreeQueue<T>>,
}
impl<T> MpmcConsumer<T> {
    pub fn recv(&self) -> Option<T> {
        self.queue.dequeue()
    }
    pub fn recv_batch(&self, output: &mut Vec<T>, max_items: usize) -> usize {
        let mut count = 0;
        while count < max_items {
            if let Some(item) = self.queue.dequeue() {
                output.push(item);
                count += 1;
            } else {
                break;
            }
        }
        count
    }
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    pub fn utilization(&self) -> f64 {
        self.queue.utilization()
    }
}

pub fn mpmc_queue<T>(max_size: Option<usize>) -> (MpmcProducer<T>, MpmcConsumer<T>) {
    let queue = if let Some(size) = max_size {
        Arc::new(BoundedLockFreeQueue::new(size))
    } else {
        Arc::new(BoundedLockFreeQueue::new(usize::MAX))
    };
    (
        MpmcProducer {
            queue: queue.clone(),
        },
        MpmcConsumer { queue },
    )
}
