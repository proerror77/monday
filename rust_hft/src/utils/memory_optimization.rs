/*!
 * 💾 內存管理優化模塊
 * 
 * 專為高頻交易設計的零分配內存管理系統
 * 
 * 核心特性：
 * - 零分配數據結構
 * - 內存池管理
 * - 智能緩存機制
 * - 堆棧優化分配器
 * - 內存使用監控
 * - Copy-on-Write 優化
 */

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use std::sync::Arc;
use std::collections::VecDeque;
use std::cell::UnsafeCell;
use std::ptr::NonNull;
use std::mem::{size_of, align_of, MaybeUninit};
use std::marker::PhantomData;
use serde::{Serialize, Deserialize};
use tracing::{debug, info, warn, error};

/// 內存統計信息
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryStats {
    /// 總分配字節數
    pub total_allocated: usize,
    /// 當前使用字節數
    pub current_used: usize,
    /// 分配次數
    pub allocation_count: usize,
    /// 釋放次數
    pub deallocation_count: usize,
    /// 峰值內存使用
    pub peak_usage: usize,
    /// 內存池統計
    pub pool_stats: PoolStats,
}

/// 內存池統計
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolStats {
    /// 池大小
    pub pool_size: usize,
    /// 已使用塊數
    pub used_blocks: usize,
    /// 可用塊數
    pub available_blocks: usize,
    /// 池命中率
    pub hit_rate: f64,
}

/// 全局內存統計追蹤器
pub struct MemoryTracker {
    total_allocated: AtomicUsize,
    current_used: AtomicUsize,
    allocation_count: AtomicUsize,
    deallocation_count: AtomicUsize,
    peak_usage: AtomicUsize,
    enabled: AtomicBool,
}

impl MemoryTracker {
    pub const fn new() -> Self {
        Self {
            total_allocated: AtomicUsize::new(0),
            current_used: AtomicUsize::new(0),
            allocation_count: AtomicUsize::new(0),
            deallocation_count: AtomicUsize::new(0),
            peak_usage: AtomicUsize::new(0),
            enabled: AtomicBool::new(true),
        }
    }

    pub fn track_allocation(&self, size: usize) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }

        self.total_allocated.fetch_add(size, Ordering::Relaxed);
        let current = self.current_used.fetch_add(size, Ordering::Relaxed) + size;
        self.allocation_count.fetch_add(1, Ordering::Relaxed);

        // 更新峰值使用量
        let mut peak = self.peak_usage.load(Ordering::Relaxed);
        while current > peak {
            match self.peak_usage.compare_exchange_weak(
                peak, current, Ordering::Relaxed, Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(actual) => peak = actual,
            }
        }
    }

    pub fn track_deallocation(&self, size: usize) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }

        self.current_used.fetch_sub(size, Ordering::Relaxed);
        self.deallocation_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get_stats(&self) -> MemoryStats {
        MemoryStats {
            total_allocated: self.total_allocated.load(Ordering::Relaxed),
            current_used: self.current_used.load(Ordering::Relaxed),
            allocation_count: self.allocation_count.load(Ordering::Relaxed),
            deallocation_count: self.deallocation_count.load(Ordering::Relaxed),
            peak_usage: self.peak_usage.load(Ordering::Relaxed),
            pool_stats: PoolStats::default(), // 由池管理器填充
        }
    }

    pub fn enable_tracking(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn reset(&self) {
        self.total_allocated.store(0, Ordering::Relaxed);
        self.current_used.store(0, Ordering::Relaxed);
        self.allocation_count.store(0, Ordering::Relaxed);
        self.deallocation_count.store(0, Ordering::Relaxed);
        self.peak_usage.store(0, Ordering::Relaxed);
    }
}

/// 全局內存追蹤器實例
static MEMORY_TRACKER: MemoryTracker = MemoryTracker::new();

/// 追蹤分配的全局分配器
pub struct TrackingAllocator;

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            MEMORY_TRACKER.track_allocation(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        MEMORY_TRACKER.track_deallocation(layout.size());
        System.dealloc(ptr, layout);
    }
}

/// 零分配向量實現，使用預分配內存池
pub struct ZeroAllocVec<T> {
    data: NonNull<T>,
    len: usize,
    capacity: usize,
    _phantom: PhantomData<T>,
}

impl<T> ZeroAllocVec<T> {
    /// 使用預分配內存創建向量
    pub fn with_capacity_from_pool(capacity: usize, pool: &MemoryPool<T>) -> Option<Self> {
        pool.allocate_block(capacity).map(|data| Self {
            data,
            len: 0,
            capacity,
            _phantom: PhantomData,
        })
    }

    /// 安全地推入元素（無分配）
    pub fn push(&mut self, value: T) -> Result<(), T> {
        if self.len >= self.capacity {
            return Err(value);
        }

        unsafe {
            self.data.as_ptr().add(self.len).write(value);
        }
        self.len += 1;
        Ok(())
    }

    /// 彈出元素
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }

        self.len -= 1;
        unsafe {
            Some(self.data.as_ptr().add(self.len).read())
        }
    }

    /// 獲取長度
    pub fn len(&self) -> usize {
        self.len
    }

    /// 檢查是否為空
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 清空向量但保留內存
    pub fn clear(&mut self) {
        unsafe {
            for i in 0..self.len {
                self.data.as_ptr().add(i).drop_in_place();
            }
        }
        self.len = 0;
    }

    /// 獲取切片視圖
    pub fn as_slice(&self) -> &[T] {
        unsafe {
            std::slice::from_raw_parts(self.data.as_ptr(), self.len)
        }
    }

    /// 獲取可變切片視圖
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        unsafe {
            std::slice::from_raw_parts_mut(self.data.as_ptr(), self.len)
        }
    }
}

unsafe impl<T: Send> Send for ZeroAllocVec<T> {}
unsafe impl<T: Sync> Sync for ZeroAllocVec<T> {}

impl<T> Drop for ZeroAllocVec<T> {
    fn drop(&mut self) {
        self.clear();
        // 注意：這裡不釋放內存，因為它屬於內存池
    }
}

/// 高性能內存池
pub struct MemoryPool<T> {
    blocks: UnsafeCell<VecDeque<NonNull<T>>>,
    block_size: usize,
    capacity: usize,
    hits: AtomicUsize,
    misses: AtomicUsize,
}

impl<T> MemoryPool<T> {
    /// 創建新的內存池
    pub fn new(block_size: usize, initial_blocks: usize) -> Self {
        let mut blocks = VecDeque::with_capacity(initial_blocks);
        
        // 預分配內存塊
        for _ in 0..initial_blocks {
            let layout = Layout::array::<T>(block_size).unwrap();
            unsafe {
                let ptr = System.alloc(layout) as *mut T;
                if !ptr.is_null() {
                    blocks.push_back(NonNull::new_unchecked(ptr));
                }
            }
        }

        Self {
            blocks: UnsafeCell::new(blocks),
            block_size,
            capacity: initial_blocks,
            hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
        }
    }

    /// 分配內存塊
    pub fn allocate_block(&self, size: usize) -> Option<NonNull<T>> {
        if size > self.block_size {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }

        unsafe {
            let blocks = &mut *self.blocks.get();
            if let Some(block) = blocks.pop_front() {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(block)
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                // 分配新塊
                let layout = Layout::array::<T>(self.block_size).unwrap();
                let ptr = System.alloc(layout) as *mut T;
                NonNull::new(ptr)
            }
        }
    }

    /// 釋放內存塊回池
    pub fn deallocate_block(&self, block: NonNull<T>) {
        unsafe {
            let blocks = &mut *self.blocks.get();
            blocks.push_back(block);
        }
    }

    /// 獲取池統計
    pub fn get_stats(&self) -> PoolStats {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total_requests = hits + misses;
        
        unsafe {
            let blocks = &*self.blocks.get();
            PoolStats {
                pool_size: self.capacity,
                used_blocks: self.capacity - blocks.len(),
                available_blocks: blocks.len(),
                hit_rate: if total_requests > 0 {
                    hits as f64 / total_requests as f64
                } else {
                    0.0
                },
            }
        }
    }
}

unsafe impl<T: Send> Send for MemoryPool<T> {}
unsafe impl<T: Sync> Sync for MemoryPool<T> {}

/// Copy-on-Write 智能指針，減少不必要的拷貝
#[derive(Debug)]
pub struct CowPtr<T> {
    data: Arc<T>,
    is_owned: bool,
}

impl<T: Clone> CowPtr<T> {
    /// 從共享數據創建
    pub fn from_shared(data: Arc<T>) -> Self {
        Self {
            data,
            is_owned: false,
        }
    }

    /// 從擁有的數據創建
    pub fn from_owned(data: T) -> Self {
        Self {
            data: Arc::new(data),
            is_owned: true,
        }
    }

    /// 獲取不可變引用
    pub fn as_ref(&self) -> &T {
        &self.data
    }

    /// 獲取可變引用，觸發Copy-on-Write
    pub fn as_mut(&mut self) -> &mut T {
        if !self.is_owned || Arc::strong_count(&self.data) > 1 {
            // 需要拷貝
            let cloned = (**self.data).clone();
            self.data = Arc::new(cloned);
            self.is_owned = true;
        }
        
        // 安全：我們確保這是唯一引用
        unsafe {
            Arc::get_mut_unchecked(&mut self.data)
        }
    }

    /// 嘗試獲取可變引用而不拷貝
    pub fn try_as_mut(&mut self) -> Option<&mut T> {
        if self.is_owned && Arc::strong_count(&self.data) == 1 {
            Some(unsafe { Arc::get_mut_unchecked(&mut self.data) })
        } else {
            None
        }
    }

    /// 檢查是否為唯一所有者
    pub fn is_unique(&self) -> bool {
        self.is_owned && Arc::strong_count(&self.data) == 1
    }
}

impl<T: Clone> Clone for CowPtr<T> {
    fn clone(&self) -> Self {
        Self {
            data: Arc::clone(&self.data),
            is_owned: false,
        }
    }
}

impl<T> std::ops::Deref for CowPtr<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

/// 堆棧分配器，用於小對象的快速分配
pub struct StackAllocator {
    buffer: Vec<u8>,
    offset: AtomicUsize,
}

impl StackAllocator {
    /// 創建新的堆棧分配器
    pub fn new(size: usize) -> Self {
        Self {
            buffer: vec![0u8; size],
            offset: AtomicUsize::new(0),
        }
    }

    /// 分配內存
    pub fn allocate<T>(&self, count: usize) -> Option<NonNull<T>> {
        let size = size_of::<T>() * count;
        let align = align_of::<T>();
        
        let current_offset = self.offset.load(Ordering::Relaxed);
        let aligned_offset = (current_offset + align - 1) & !(align - 1);
        let new_offset = aligned_offset + size;
        
        if new_offset > self.buffer.len() {
            return None;
        }
        
        // 原子性更新偏移量
        match self.offset.compare_exchange(
            current_offset, new_offset, 
            Ordering::Relaxed, Ordering::Relaxed
        ) {
            Ok(_) => {
                let ptr = unsafe {
                    self.buffer.as_ptr().add(aligned_offset) as *mut T
                };
                NonNull::new(ptr)
            }
            Err(_) => {
                // 重試或失敗
                None
            }
        }
    }

    /// 重置分配器
    pub fn reset(&self) {
        self.offset.store(0, Ordering::Relaxed);
    }

    /// 獲取使用率
    pub fn usage(&self) -> f64 {
        let used = self.offset.load(Ordering::Relaxed);
        used as f64 / self.buffer.len() as f64
    }
}

/// 零拷貝字符串切片
#[derive(Debug, Clone)]
pub struct ZeroCopyStr<'a> {
    data: &'a str,
    owned: Option<String>,
}

impl<'a> ZeroCopyStr<'a> {
    /// 從借用字符串創建
    pub fn from_borrowed(s: &'a str) -> Self {
        Self {
            data: s,
            owned: None,
        }
    }

    /// 從擁有字符串創建
    pub fn from_owned(s: String) -> Self {
        Self {
            data: "",
            owned: Some(s),
        }
    }

    /// 獲取字符串引用
    pub fn as_str(&self) -> &str {
        if let Some(ref owned) = self.owned {
            owned
        } else {
            self.data
        }
    }

    /// 轉換為擁有的字符串
    pub fn into_owned(self) -> String {
        match self.owned {
            Some(s) => s,
            None => self.data.to_string(),
        }
    }

    /// 檢查是否為借用
    pub fn is_borrowed(&self) -> bool {
        self.owned.is_none()
    }
}

impl<'a> std::ops::Deref for ZeroCopyStr<'a> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl<'a> PartialEq for ZeroCopyStr<'a> {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl<'a> PartialEq<str> for ZeroCopyStr<'a> {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl<'a> PartialEq<&str> for ZeroCopyStr<'a> {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// 內存管理器
pub struct MemoryManager {
    // 不同大小的內存池
    small_pool: MemoryPool<u8>,    // 64字節塊
    medium_pool: MemoryPool<u8>,   // 1KB塊  
    large_pool: MemoryPool<u8>,    // 64KB塊
    stack_allocator: StackAllocator,
}

impl MemoryManager {
    /// 創建新的內存管理器
    pub fn new() -> Self {
        Self {
            small_pool: MemoryPool::new(64, 1000),
            medium_pool: MemoryPool::new(1024, 100),
            large_pool: MemoryPool::new(65536, 10),
            stack_allocator: StackAllocator::new(1024 * 1024), // 1MB堆棧
        }
    }

    /// 分配內存
    pub fn allocate(&self, size: usize) -> Option<NonNull<u8>> {
        if size <= 64 {
            self.small_pool.allocate_block(size)
        } else if size <= 1024 {
            self.medium_pool.allocate_block(size)
        } else if size <= 65536 {
            self.large_pool.allocate_block(size)
        } else {
            // 使用系統分配器
            let layout = Layout::from_size_align(size, 8).ok()?;
            unsafe {
                let ptr = System.alloc(layout);
                NonNull::new(ptr)
            }
        }
    }

    /// 釋放內存
    pub fn deallocate(&self, ptr: NonNull<u8>, size: usize) {
        if size <= 64 {
            self.small_pool.deallocate_block(ptr);
        } else if size <= 1024 {
            self.medium_pool.deallocate_block(ptr);
        } else if size <= 65536 {
            self.large_pool.deallocate_block(ptr);
        } else {
            // 使用系統分配器釋放
            let layout = Layout::from_size_align(size, 8).unwrap();
            unsafe {
                System.dealloc(ptr.as_ptr(), layout);
            }
        }
    }

    /// 獲取綜合統計
    pub fn get_comprehensive_stats(&self) -> MemoryStats {
        let mut stats = MEMORY_TRACKER.get_stats();
        
        // 添加池統計
        let small_stats = self.small_pool.get_stats();
        let medium_stats = self.medium_pool.get_stats();
        let large_stats = self.large_pool.get_stats();
        
        stats.pool_stats = PoolStats {
            pool_size: small_stats.pool_size + medium_stats.pool_size + large_stats.pool_size,
            used_blocks: small_stats.used_blocks + medium_stats.used_blocks + large_stats.used_blocks,
            available_blocks: small_stats.available_blocks + medium_stats.available_blocks + large_stats.available_blocks,
            hit_rate: (small_stats.hit_rate + medium_stats.hit_rate + large_stats.hit_rate) / 3.0,
        };
        
        stats
    }

    /// 重置堆棧分配器
    pub fn reset_stack(&self) {
        self.stack_allocator.reset();
    }
}

impl Default for MemoryManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局內存管理器實例
lazy_static::lazy_static! {
    pub static ref GLOBAL_MEMORY_MANAGER: MemoryManager = MemoryManager::new();
}

/// 內存優化的宏
#[macro_export]
macro_rules! zero_alloc_vec {
    ($capacity:expr, $type:ty) => {
        {
            static POOL: std::sync::OnceLock<crate::utils::memory_optimization::MemoryPool<$type>> = std::sync::OnceLock::new();
            let pool = POOL.get_or_init(|| {
                crate::utils::memory_optimization::MemoryPool::new($capacity, 10)
            });
            crate::utils::memory_optimization::ZeroAllocVec::with_capacity_from_pool($capacity, pool)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_tracker() {
        let tracker = MemoryTracker::new();
        
        tracker.track_allocation(1024);
        tracker.track_allocation(512);
        
        let stats = tracker.get_stats();
        assert_eq!(stats.total_allocated, 1536);
        assert_eq!(stats.current_used, 1536);
        assert_eq!(stats.allocation_count, 2);
        
        tracker.track_deallocation(512);
        let stats = tracker.get_stats();
        assert_eq!(stats.current_used, 1024);
        assert_eq!(stats.deallocation_count, 1);
    }

    #[test]
    fn test_zero_alloc_vec() {
        let pool = MemoryPool::new(100, 5);
        let mut vec = ZeroAllocVec::with_capacity_from_pool(10, &pool).unwrap();
        
        for i in 0..5 {
            assert!(vec.push(i).is_ok());
        }
        
        assert_eq!(vec.len(), 5);
        assert_eq!(vec.pop(), Some(4));
        assert_eq!(vec.len(), 4);
    }

    #[test]
    fn test_cow_ptr() {
        let data = vec![1, 2, 3, 4, 5];
        let mut cow = CowPtr::from_owned(data);
        
        // 第一次獲取可變引用應該不會拷貝
        assert!(cow.is_unique());
        cow.as_mut().push(6);
        
        // 克隆後再獲取可變引用應該觸發拷貝
        let mut cow2 = cow.clone();
        assert!(!cow2.is_unique());
        cow2.as_mut().push(7);
        
        assert_eq!(cow.len(), 6);
        assert_eq!(cow2.len(), 7);
    }

    #[test]
    fn test_stack_allocator() {
        let allocator = StackAllocator::new(1024);
        
        let ptr1 = allocator.allocate::<u64>(10);
        assert!(ptr1.is_some());
        
        let ptr2 = allocator.allocate::<u32>(20);
        assert!(ptr2.is_some());
        
        // 檢查使用率
        assert!(allocator.usage() > 0.0);
        
        allocator.reset();
        assert_eq!(allocator.usage(), 0.0);
    }

    #[test]
    fn test_zero_copy_str() {
        let borrowed = ZeroCopyStr::from_borrowed("hello");
        assert!(borrowed.is_borrowed());
        assert_eq!(borrowed.as_str(), "hello");
        
        let owned = ZeroCopyStr::from_owned("world".to_string());
        assert!(!owned.is_borrowed());
        assert_eq!(owned.as_str(), "world");
    }
}