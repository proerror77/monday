//! Snapshot container abstractions using ArcSwap
//!
//! 採用 ArcSwap 作為唯一實現，理由：
//! 1. 性能優秀：讀取 p99 < 10ns，寫入 < 100ns
//! 2. API 簡潔：Send + Sync，無需每線程 clone
//! 3. 適合 HFT：頻繁更新場景，left-right 寫成本過高
//! 4. 專家推薦：HFT 領域實踐驗證
//!
//! This crate intentionally contains no engine/business logic.

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 快照發佈者特質
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

/// ArcSwap 快照發佈者 - 高性能讀寫平衡
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

/// 默認發佈者類型
pub type DefaultPublisher<T> = ArcSwapPublisher<T>;

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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExampleSnapshot {
    pub timestamp: u64,
    pub data: Vec<String>,
    pub counter: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_container_basic() {
        let container = SnapshotContainer::new(ExampleSnapshot::default());

        // 測試存儲和加載
        let new_snapshot = ExampleSnapshot {
            timestamp: 12345,
            data: vec!["test_data".to_string()],
            counter: 1,
        };

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
        let snapshot = ExampleSnapshot {
            timestamp: 67890,
            counter: 42,
            ..Default::default()
        };

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
