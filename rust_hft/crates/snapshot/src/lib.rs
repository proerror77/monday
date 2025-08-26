//! Snapshot container abstractions
//! - Default: ArcSwap-based read-mostly snapshots
//! - Feature `snapshot-left-right`: switch to left-right (cost to writer)
//!
//! This crate intentionally contains no engine/business logic.

use std::sync::Arc;
use serde::{Deserialize, Serialize};

/// 快照發佈者特質 - 可替換 ArcSwap/left-right 實作
pub trait SnapshotPublisher<T>: Send + Sync {
    /// 存儲新快照
    fn store(&self, snapshot: Arc<T>);
    
    /// 加載當前快照
    fn load(&self) -> Arc<T>;
    
    /// 檢查是否有更新的快照
    fn is_updated(&self) -> bool;
}

/// 快照讀取者特質
pub trait SnapshotReader<T>: Send + Sync {
    /// 加載當前快照
    fn load(&self) -> Arc<T>;
}

#[cfg(not(feature = "snapshot-left-right"))]
mod arcswap_impl {
    use super::*;
    use arc_swap::ArcSwap;
    use std::sync::atomic::{AtomicBool, Ordering};
    
    /// ArcSwap 實作 - 默認高性能讀取
    pub struct ArcSwapPublisher<T> {
        inner: ArcSwap<T>,
        updated: AtomicBool,
    }
    
    impl<T> ArcSwapPublisher<T> {
        pub fn new(initial_value: T) -> Self {
            Self {
                inner: ArcSwap::new(Arc::new(initial_value)),
                updated: AtomicBool::new(false),
            }
        }
    }
    
    impl<T: Send + Sync> SnapshotPublisher<T> for ArcSwapPublisher<T> {
        fn store(&self, snapshot: Arc<T>) {
            self.inner.store(snapshot);
            self.updated.store(true, Ordering::Release);
        }
        
        fn load(&self) -> Arc<T> {
            self.inner.load_full()
        }
        
        fn is_updated(&self) -> bool {
            self.updated.swap(false, Ordering::Acquire)
        }
    }
    
    impl<T: Send + Sync> SnapshotReader<T> for ArcSwapPublisher<T> {
        fn load(&self) -> Arc<T> {
            self.inner.load_full()
        }
    }
    
    pub type DefaultPublisher<T> = ArcSwapPublisher<T>;
}

#[cfg(feature = "snapshot-left-right")]
mod leftright_impl {
    use super::*;
    use left_right::{Absorb, ReadHandle, WriteHandle};
    use std::sync::atomic::{AtomicBool, Ordering};
    
    /// Left-right 實作 - 極致讀性能但寫成本高
    pub struct LeftRightPublisher<T> {
        writer: WriteHandle<T, ()>,
        reader: ReadHandle<T>,
        updated: AtomicBool,
    }
    
    impl<T: Clone> LeftRightPublisher<T> {
        pub fn new(initial_value: T) -> Self {
            let (writer, reader) = left_right::new::<T, ()>();
            Self {
                writer,
                reader,
                updated: AtomicBool::new(false),
            }
        }
    }
    
    impl<T: Clone> SnapshotPublisher<T> for LeftRightPublisher<T> {
        fn store(&self, snapshot: Arc<T>) {
            // Left-right 需要 owned value，從 Arc 中 clone
            let value = (*snapshot).clone();
            self.writer.append(value);
            self.writer.publish();
            self.updated.store(true, Ordering::Release);
        }
        
        fn load(&self) -> Arc<T> {
            let guard = self.reader.enter().unwrap();
            Arc::new(guard.clone())
        }
        
        fn is_updated(&self) -> bool {
            self.updated.swap(false, Ordering::Acquire)
        }
    }
    
    impl<T: Clone> SnapshotReader<T> for LeftRightPublisher<T> {
        fn load(&self) -> Arc<T> {
            let guard = self.reader.enter().unwrap();
            Arc::new(guard.clone())
        }
    }
    
    pub type DefaultPublisher<T> = LeftRightPublisher<T>;
    
    impl<T> Absorb<T> for T {
        fn absorb_first(&mut self, operation: T, _: &()) {
            *self = operation;
        }
        
        fn absorb_second(&mut self, operation: T, _: &()) {
            *self = operation;
        }
        
        fn sync_with(&mut self, first: &Self, _: &()) {
            *self = first.clone();
        }
    }
}

// 根據 feature 選擇實作
#[cfg(not(feature = "snapshot-left-right"))]
pub use arcswap_impl::*;

#[cfg(feature = "snapshot-left-right")]
pub use leftright_impl::*;

/// 便利方法：創建快照發佈者
pub fn create_publisher<T>(initial_value: T) -> DefaultPublisher<T> 
where
    T: Clone + Send + Sync + 'static,
{
    DefaultPublisher::new(initial_value)
}

/// 快照容器 - 包裝發佈者和讀取者
pub struct SnapshotContainer<T> {
    publisher: Arc<DefaultPublisher<T>>,
}

impl<T> SnapshotContainer<T>
where
    T: Clone + Send + Sync + 'static,
{
    pub fn new(initial_value: T) -> Self {
        let publisher = Arc::new(create_publisher(initial_value));
        Self { publisher }
    }
    
    /// 獲取發佈者
    pub fn publisher(&self) -> Arc<dyn SnapshotPublisher<T>> {
        self.publisher.clone()
    }
    
    /// 獲取讀取者
    pub fn reader(&self) -> Arc<dyn SnapshotReader<T>> {
        self.publisher.clone()
    }
    
    /// 直接存儲
    pub fn store(&self, snapshot: Arc<T>) {
        self.publisher.store(snapshot);
    }
    
    /// 直接加載
    pub fn load(&self) -> Arc<T> {
        SnapshotPublisher::load(self.publisher.as_ref())
    }
}

impl<T> Clone for SnapshotContainer<T> {
    fn clone(&self) -> Self {
        Self {
            publisher: self.publisher.clone(),
        }
    }
}

/// 測試數據結構 - 僅用於演示快照容器功能
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExampleSnapshot {
    pub timestamp: u64,
    pub data: Vec<String>,
    pub counter: usize,
}

impl Default for ExampleSnapshot {
    fn default() -> Self {
        Self {
            timestamp: 0,
            data: Vec::new(),
            counter: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_snapshot_container_basic() {
        let container = SnapshotContainer::new(ExampleSnapshot::default());
        
        // 測試存儲和加載
        let mut new_snapshot = ExampleSnapshot::default();
        new_snapshot.timestamp = 12345;
        new_snapshot.data.push("test_data".to_string());
        new_snapshot.counter = 1;
        
        container.store(Arc::new(new_snapshot.clone()));
        let loaded = container.load();
        
        assert_eq!(loaded.timestamp, 12345);
        assert_eq!(loaded.data.len(), 1);
        assert_eq!(loaded.data[0], "test_data");
        assert_eq!(loaded.counter, 1);
    }
    
    #[test]
    fn test_multiple_readers() {
        let container = SnapshotContainer::new(ExampleSnapshot::default());
        
        // 創建多個讀取者
        let reader1 = container.reader();
        let reader2 = container.reader();
        
        // 更新數據
        let mut snapshot = ExampleSnapshot::default();
        snapshot.timestamp = 67890;
        snapshot.counter = 42;
        
        container.store(Arc::new(snapshot));
        
        // 兩個讀取者都應該看到更新
        let view1 = reader1.load();
        let view2 = reader2.load();
        
        assert_eq!(view1.timestamp, 67890);
        assert_eq!(view2.timestamp, 67890);
        assert_eq!(view1.counter, 42);
        assert_eq!(view2.counter, 42);
    }
}