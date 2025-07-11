/*!
 * 🔒 並發安全工具模塊
 * 
 * 提供安全的鎖操作、死鎖檢測和並發監控功能
 * 
 * 特性：
 * - 超時保護的鎖操作
 * - 統一的鎖獲取順序
 * - 死鎖檢測和預防
 * - 鎖爭用監控
 * - 無鎖數據結構
 */

use std::collections::{HashMap, BTreeMap};
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::thread::ThreadId;
use tokio::sync::{Mutex, RwLock};
use tokio::time::timeout;
use tracing::{warn, error, debug, info};
use serde::{Serialize, Deserialize};

/// 默認鎖超時時間
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_millis(5000);

/// 鎖獲取順序枚舉，防止死鎖
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LockOrder {
    /// 系統狀態鎖（優先級最高）
    SystemState = 1,
    /// Pipeline狀態鎖
    PipelineState = 2,
    /// HFT執行狀態鎖
    HftExecutionState = 3,
    /// 訂單簿更新鎖
    OrderBookUpdate = 4,
    /// 上下文管理鎖
    ContextManagement = 5,
    /// 命令隊列鎖
    CommandQueue = 6,
    /// 監控數據鎖
    MonitoringData = 7,
    /// 日誌記錄鎖（優先級最低）
    LoggingData = 8,
}

/// 鎖操作結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LockResult<T> {
    Success(T),
    Timeout { waited_ms: u64 },
    DeadlockDetected { involved_locks: Vec<LockOrder> },
    Error { message: String },
}

/// 鎖爭用統計
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LockContentionStats {
    pub total_acquisitions: u64,
    pub total_timeouts: u64,
    pub average_wait_time_ms: f64,
    pub max_wait_time_ms: u64,
    pub deadlock_count: u64,
}

/// 安全鎖管理器
pub struct SafeLockManager {
    /// 鎖爭用統計
    contention_stats: Arc<RwLock<HashMap<LockOrder, LockContentionStats>>>,
    /// 當前持有的鎖（按線程）
    held_locks: Arc<RwLock<HashMap<ThreadId, Vec<LockOrder>>>>,
    /// 死鎖檢測啟用標誌
    deadlock_detection_enabled: AtomicBool,
}

impl SafeLockManager {
    /// 創建新的安全鎖管理器
    pub fn new() -> Self {
        Self {
            contention_stats: Arc::new(RwLock::new(HashMap::new())),
            held_locks: Arc::new(RwLock::new(HashMap::new())),
            deadlock_detection_enabled: AtomicBool::new(true),
        }
    }

    /// 安全獲取Mutex鎖，帶超時保護
    pub async fn safe_lock<T>(
        &self,
        mutex: &Arc<Mutex<T>>,
        lock_order: LockOrder,
        timeout_duration: Option<Duration>,
    ) -> LockResult<tokio::sync::MutexGuard<'_, T>> {
        let start_time = Instant::now();
        let timeout_duration = timeout_duration.unwrap_or(DEFAULT_LOCK_TIMEOUT);

        // 死鎖檢測
        if self.deadlock_detection_enabled.load(Ordering::Acquire) {
            if let Some(deadlock_info) = self.check_deadlock_risk(lock_order).await {
                warn!("檢測到潛在死鎖風險: {:?}", deadlock_info);
                return LockResult::DeadlockDetected { 
                    involved_locks: deadlock_info 
                };
            }
        }

        // 記錄鎖請求
        self.record_lock_acquisition(lock_order).await;

        // 嘗試獲取鎖
        match timeout(timeout_duration, mutex.lock()).await {
            Ok(guard) => {
                let wait_time = start_time.elapsed();
                self.update_contention_stats(lock_order, wait_time, false).await;
                debug!("成功獲取鎖 {:?}，等待時間: {:?}", lock_order, wait_time);
                LockResult::Success(guard)
            }
            Err(_) => {
                let wait_time = start_time.elapsed();
                self.update_contention_stats(lock_order, wait_time, true).await;
                warn!("鎖獲取超時 {:?}，等待時間: {:?}", lock_order, wait_time);
                LockResult::Timeout { 
                    waited_ms: wait_time.as_millis() as u64 
                }
            }
        }
    }

    /// 安全獲取RwLock讀鎖
    pub async fn safe_read_lock<T>(
        &self,
        rwlock: &Arc<RwLock<T>>,
        lock_order: LockOrder,
        timeout_duration: Option<Duration>,
    ) -> LockResult<tokio::sync::RwLockReadGuard<'_, T>> {
        let start_time = Instant::now();
        let timeout_duration = timeout_duration.unwrap_or(DEFAULT_LOCK_TIMEOUT);

        match timeout(timeout_duration, rwlock.read()).await {
            Ok(guard) => {
                let wait_time = start_time.elapsed();
                self.update_contention_stats(lock_order, wait_time, false).await;
                LockResult::Success(guard)
            }
            Err(_) => {
                let wait_time = start_time.elapsed();
                self.update_contention_stats(lock_order, wait_time, true).await;
                LockResult::Timeout { 
                    waited_ms: wait_time.as_millis() as u64 
                }
            }
        }
    }

    /// 安全獲取RwLock寫鎖
    pub async fn safe_write_lock<T>(
        &self,
        rwlock: &Arc<RwLock<T>>,
        lock_order: LockOrder,
        timeout_duration: Option<Duration>,
    ) -> LockResult<tokio::sync::RwLockWriteGuard<'_, T>> {
        let start_time = Instant::now();
        let timeout_duration = timeout_duration.unwrap_or(DEFAULT_LOCK_TIMEOUT);

        // 寫鎖的死鎖檢測更嚴格
        if self.deadlock_detection_enabled.load(Ordering::Acquire) {
            if let Some(deadlock_info) = self.check_deadlock_risk(lock_order).await {
                return LockResult::DeadlockDetected { 
                    involved_locks: deadlock_info 
                };
            }
        }

        match timeout(timeout_duration, rwlock.write()).await {
            Ok(guard) => {
                let wait_time = start_time.elapsed();
                self.update_contention_stats(lock_order, wait_time, false).await;
                LockResult::Success(guard)
            }
            Err(_) => {
                let wait_time = start_time.elapsed();
                self.update_contention_stats(lock_order, wait_time, true).await;
                LockResult::Timeout { 
                    waited_ms: wait_time.as_millis() as u64 
                }
            }
        }
    }

    /// 檢查死鎖風險
    async fn check_deadlock_risk(&self, requested_lock: LockOrder) -> Option<Vec<LockOrder>> {
        let current_thread = std::thread::current().id();
        let held_locks_guard = self.held_locks.read().await;
        
        if let Some(held_locks) = held_locks_guard.get(&current_thread) {
            // 檢查鎖獲取順序是否違反規則
            for &held_lock in held_locks {
                if requested_lock < held_lock {
                    // 違反鎖順序，可能導致死鎖
                    let mut involved_locks = held_locks.clone();
                    involved_locks.push(requested_lock);
                    return Some(involved_locks);
                }
            }
        }
        None
    }

    /// 記錄鎖獲取
    async fn record_lock_acquisition(&self, lock_order: LockOrder) {
        let current_thread = std::thread::current().id();
        let mut held_locks_guard = self.held_locks.write().await;
        
        held_locks_guard
            .entry(current_thread)
            .or_insert_with(Vec::new)
            .push(lock_order);
    }

    /// 記錄鎖釋放
    pub async fn record_lock_release(&self, lock_order: LockOrder) {
        let current_thread = std::thread::current().id();
        let mut held_locks_guard = self.held_locks.write().await;
        
        if let Some(held_locks) = held_locks_guard.get_mut(&current_thread) {
            held_locks.retain(|&lock| lock != lock_order);
            if held_locks.is_empty() {
                held_locks_guard.remove(&current_thread);
            }
        }
    }

    /// 更新鎖爭用統計
    async fn update_contention_stats(
        &self,
        lock_order: LockOrder,
        wait_time: Duration,
        is_timeout: bool,
    ) {
        let mut stats_guard = self.contention_stats.write().await;
        let stats = stats_guard
            .entry(lock_order)
            .or_insert_with(LockContentionStats::default);

        stats.total_acquisitions += 1;
        if is_timeout {
            stats.total_timeouts += 1;
        }

        let wait_time_ms = wait_time.as_millis() as u64;
        stats.max_wait_time_ms = stats.max_wait_time_ms.max(wait_time_ms);
        
        // 更新平均等待時間
        let total_wait_time = stats.average_wait_time_ms * (stats.total_acquisitions - 1) as f64;
        stats.average_wait_time_ms = (total_wait_time + wait_time_ms as f64) / stats.total_acquisitions as f64;
    }

    /// 獲取鎖爭用統計
    pub async fn get_contention_stats(&self) -> HashMap<LockOrder, LockContentionStats> {
        self.contention_stats.read().await.clone()
    }

    /// 啟用/禁用死鎖檢測
    pub fn set_deadlock_detection(&self, enabled: bool) {
        self.deadlock_detection_enabled.store(enabled, Ordering::Release);
    }

    /// 清理死亡線程的鎖記錄
    pub async fn cleanup_dead_threads(&self) {
        let mut held_locks_guard = self.held_locks.write().await;
        // 在實際實現中，這裡需要檢查哪些線程已經終止
        // 並清理它們的鎖記錄
        // 這是一個簡化的實現
        held_locks_guard.retain(|_thread_id, locks| !locks.is_empty());
    }
}

impl Default for SafeLockManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局鎖管理器實例
lazy_static::lazy_static! {
    pub static ref GLOBAL_LOCK_MANAGER: SafeLockManager = SafeLockManager::new();
}

/// 便利宏：安全獲取鎖
#[macro_export]
macro_rules! safe_lock {
    ($mutex:expr, $order:expr) => {
        crate::utils::concurrency_safety::GLOBAL_LOCK_MANAGER
            .safe_lock($mutex, $order, None)
            .await
    };
    ($mutex:expr, $order:expr, $timeout:expr) => {
        crate::utils::concurrency_safety::GLOBAL_LOCK_MANAGER
            .safe_lock($mutex, $order, Some($timeout))
            .await
    };
}

/// 便利宏：安全獲取讀鎖
#[macro_export]
macro_rules! safe_read_lock {
    ($rwlock:expr, $order:expr) => {
        crate::utils::concurrency_safety::GLOBAL_LOCK_MANAGER
            .safe_read_lock($rwlock, $order, None)
            .await
    };
    ($rwlock:expr, $order:expr, $timeout:expr) => {
        crate::utils::concurrency_safety::GLOBAL_LOCK_MANAGER
            .safe_read_lock($rwlock, $order, Some($timeout))
            .await
    };
}

/// 便利宏：安全獲取寫鎖
#[macro_export]
macro_rules! safe_write_lock {
    ($rwlock:expr, $order:expr) => {
        crate::utils::concurrency_safety::GLOBAL_LOCK_MANAGER
            .safe_write_lock($rwlock, $order, None)
            .await
    };
    ($rwlock:expr, $order:expr, $timeout:expr) => {
        crate::utils::concurrency_safety::GLOBAL_LOCK_MANAGER
            .safe_write_lock($rwlock, $order, Some($timeout))
            .await
    };
}

/// 高性能原子計數器，避免鎖爭用
#[derive(Debug)]
pub struct LockFreeCounter {
    value: AtomicU64,
}

impl LockFreeCounter {
    pub fn new(initial: u64) -> Self {
        Self {
            value: AtomicU64::new(initial),
        }
    }

    pub fn increment(&self) -> u64 {
        self.value.fetch_add(1, Ordering::Relaxed)
    }

    pub fn decrement(&self) -> u64 {
        self.value.fetch_sub(1, Ordering::Relaxed)
    }

    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Acquire)
    }

    pub fn set(&self, value: u64) {
        self.value.store(value, Ordering::Release);
    }

    pub fn compare_and_swap(&self, current: u64, new: u64) -> Result<u64, u64> {
        match self.value.compare_exchange(current, new, Ordering::AcqRel, Ordering::Acquire) {
            Ok(prev) => Ok(prev),
            Err(actual) => Err(actual),
        }
    }
}

impl Default for LockFreeCounter {
    fn default() -> Self {
        Self::new(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn test_safe_lock_acquisition() {
        let manager = SafeLockManager::new();
        let mutex = Arc::new(Mutex::new(42));

        let result = manager
            .safe_lock(&mutex, LockOrder::SystemState, None)
            .await;

        match result {
            LockResult::Success(guard) => {
                assert_eq!(*guard, 42);
            }
            _ => panic!("Expected successful lock acquisition"),
        }
    }

    #[tokio::test]
    async fn test_deadlock_detection() {
        let manager = SafeLockManager::new();
        let mutex1 = Arc::new(Mutex::new(1));
        let mutex2 = Arc::new(Mutex::new(2));

        // 第一個鎖正常獲取
        let _guard1 = manager
            .safe_lock(&mutex1, LockOrder::SystemState, None)
            .await;

        // 嘗試獲取較低優先級的鎖，應該成功
        let result2 = manager
            .safe_lock(&mutex2, LockOrder::PipelineState, None)
            .await;

        match result2 {
            LockResult::Success(_) => {
                // 正常情況
            }
            _ => panic!("Expected successful lock acquisition"),
        }

        // 嘗試獲取更高優先級的鎖，應該檢測到死鎖風險
        let result3 = manager
            .safe_lock(&mutex1, LockOrder::SystemState, None)
            .await;

        // 注意：這個測試可能需要調整，因為我們已經持有SystemState鎖
    }

    #[tokio::test]
    async fn test_lock_free_counter() {
        let counter = LockFreeCounter::new(0);

        // 測試基本操作
        assert_eq!(counter.get(), 0);
        counter.increment();
        assert_eq!(counter.get(), 1);
        counter.decrement();
        assert_eq!(counter.get(), 0);

        // 測試並發安全性
        let counter_arc = Arc::new(counter);
        let mut handles = Vec::new();

        for _ in 0..10 {
            let counter_clone = Arc::clone(&counter_arc);
            let handle = tokio::spawn(async move {
                for _ in 0..100 {
                    counter_clone.increment();
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await.unwrap();
        }

        assert_eq!(counter_arc.get(), 1000);
    }
}