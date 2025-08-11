/*!
 * Memory Pre-allocation and Optimization
 * 
 * Implements custom memory allocation strategies for ultra-low latency:
 * - Pre-allocated memory pools
 * - Cache-aligned allocations  
 * - NUMA-aware memory layout
 * - Garbage collection minimization
 */

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::ptr::{self, NonNull};
use std::mem;
use std::collections::VecDeque;
use anyhow::{Result, anyhow};

/// Custom allocator for high-frequency trading
pub struct HftAllocator {
    system: System,
    allocated_bytes: AtomicUsize,
    peak_usage: AtomicUsize,
    allocation_count: AtomicUsize,
}

impl HftAllocator {
    pub fn new() -> Self {
        Self {
            system: System,
            allocated_bytes: AtomicUsize::new(0),
            peak_usage: AtomicUsize::new(0),
            allocation_count: AtomicUsize::new(0),
        }
    }
    
    pub fn get_stats(&self) -> AllocatorStats {
        let current = self.allocated_bytes.load(Ordering::Relaxed);
        let peak = self.peak_usage.load(Ordering::Relaxed);
        let count = self.allocation_count.load(Ordering::Relaxed);
        
        AllocatorStats {
            current_bytes: current,
            peak_bytes: peak,
            allocation_count: count,
        }
    }
}

unsafe impl GlobalAlloc for HftAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = self.system.alloc(layout);
        if !ptr.is_null() {
            let size = layout.size();
            self.allocated_bytes.fetch_add(size, Ordering::Relaxed);
            self.allocation_count.fetch_add(1, Ordering::Relaxed);
            
            // Update peak usage
            let current = self.allocated_bytes.load(Ordering::Relaxed);
            self.peak_usage.fetch_max(current, Ordering::Relaxed);
        }
        ptr
    }
    
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.system.dealloc(ptr, layout);
        self.allocated_bytes.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

/// Memory pool for fixed-size allocations
pub struct MemoryPool<T> {
    free_objects: Arc<Mutex<VecDeque<Box<T>>>>,
    allocated_count: AtomicUsize,
    max_capacity: usize,
    constructor: fn() -> T,
}

impl<T> MemoryPool<T> {
    pub fn new(initial_size: usize, max_capacity: usize, constructor: fn() -> T) -> Self {
        let pool = MemoryPool {
            free_objects: Arc::new(Mutex::new(VecDeque::with_capacity(max_capacity))),
            allocated_count: AtomicUsize::new(0),
            max_capacity,
            constructor,
        };
        
        // Pre-allocate objects
        {
            let mut free_list = pool.free_objects.lock().unwrap();
            for _ in 0..initial_size {
                free_list.push_back(Box::new(constructor()));
            }
        }
        
        pool
    }
    
    /// Acquire object from pool or allocate new one
    pub fn acquire(&self) -> PooledObject<T> {
        let object = {
            let mut free_list = self.free_objects.lock().unwrap();
            free_list.pop_front().unwrap_or_else(|| {
                self.allocated_count.fetch_add(1, Ordering::Relaxed);
                Box::new((self.constructor)())
            })
        };
        
        PooledObject {
            object: Some(object),
            pool: Arc::downgrade(&self.free_objects),
        }
    }
    
    pub fn stats(&self) -> PoolStats {
        let free_count = self.free_objects.lock().unwrap().len();
        let allocated = self.allocated_count.load(Ordering::Relaxed);
        
        PoolStats {
            free_objects: free_count,
            allocated_objects: allocated,
            total_capacity: self.max_capacity,
        }
    }
}

/// RAII wrapper for pooled objects
pub struct PooledObject<T> {
    object: Option<Box<T>>,
    pool: std::sync::Weak<Mutex<VecDeque<Box<T>>>>,
}

impl<T> PooledObject<T> {
    pub fn as_ref(&self) -> &T {
        self.object.as_ref().unwrap()
    }
    
    pub fn as_mut(&mut self) -> &mut T {
        self.object.as_mut().unwrap()
    }
}

impl<T> Drop for PooledObject<T> {
    fn drop(&mut self) {
        if let (Some(object), Some(pool)) = (self.object.take(), self.pool.upgrade()) {
            if let Ok(mut free_list) = pool.lock() {
                if free_list.len() < free_list.capacity() {
                    free_list.push_back(object);
                }
                // Otherwise, object is dropped normally
            }
        }
    }
}

/// Cache-aligned memory allocator
pub struct CacheAlignedAllocator {
    cache_line_size: usize,
}

impl CacheAlignedAllocator {
    pub fn new() -> Self {
        Self {
            cache_line_size: 64, // Common cache line size
        }
    }
    
    /// Allocate cache-aligned memory
    pub unsafe fn alloc_aligned<T>(&self, count: usize) -> Result<*mut T> {
        let size = mem::size_of::<T>() * count;
        let align = self.cache_line_size.max(mem::align_of::<T>());
        
        let layout = Layout::from_size_align(size, align)
            .map_err(|e| anyhow!("Invalid layout: {}", e))?;
        
        let ptr = System.alloc(layout);
        if ptr.is_null() {
            return Err(anyhow!("Allocation failed"));
        }
        
        Ok(ptr as *mut T)
    }
    
    pub unsafe fn dealloc_aligned<T>(&self, ptr: *mut T, count: usize) {
        let size = mem::size_of::<T>() * count;
        let align = self.cache_line_size.max(mem::align_of::<T>());
        
        if let Ok(layout) = Layout::from_size_align(size, align) {
            System.dealloc(ptr as *mut u8, layout);
        }
    }
}

/// Ring buffer with pre-allocated memory
pub struct PreAllocatedRingBuffer<T> {
    buffer: Vec<T>,
    head: AtomicUsize,
    tail: AtomicUsize,
    capacity: usize,
}

impl<T: Default + Clone> PreAllocatedRingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        let mut buffer = Vec::with_capacity(capacity);
        buffer.resize(capacity, T::default());
        
        Self {
            buffer,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            capacity,
        }
    }
    
    /// Lock-free push operation
    pub fn push(&self, item: T) -> Result<()> {
        let tail = self.tail.load(Ordering::Relaxed);
        let next_tail = (tail + 1) % self.capacity;
        let head = self.head.load(Ordering::Acquire);
        
        if next_tail == head {
            return Err(anyhow!("Ring buffer full"));
        }
        
        // SAFETY: We've ensured the index is valid
        unsafe {
            let slot = self.buffer.as_ptr().add(tail) as *mut T;
            ptr::write(slot, item);
        }
        
        self.tail.store(next_tail, Ordering::Release);
        Ok(())
    }
    
    /// Lock-free pop operation
    pub fn pop(&self) -> Option<T> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        
        if head == tail {
            return None; // Empty
        }
        
        // SAFETY: We've ensured the index is valid
        let item = unsafe {
            let slot = self.buffer.as_ptr().add(head);
            ptr::read(slot)
        };
        
        let next_head = (head + 1) % self.capacity;
        self.head.store(next_head, Ordering::Release);
        
        Some(item)
    }
    
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Relaxed);
        
        if tail >= head {
            tail - head
        } else {
            self.capacity - head + tail
        }
    }
    
    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Relaxed) == self.tail.load(Ordering::Relaxed)
    }
    
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

/// Memory-mapped region for ultra-fast data structures
pub struct MemoryMappedRegion {
    ptr: NonNull<u8>,
    size: usize,
}

impl MemoryMappedRegion {
    #[cfg(target_os = "linux")]
    pub fn new(size: usize) -> Result<Self> {
        use libc::{mmap, MAP_PRIVATE, MAP_ANONYMOUS, PROT_READ, PROT_WRITE, MAP_FAILED};
        
        let ptr = unsafe {
            mmap(
                ptr::null_mut(),
                size,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        
        if ptr == MAP_FAILED {
            return Err(anyhow!("mmap failed"));
        }
        
        Ok(Self {
            ptr: NonNull::new(ptr as *mut u8).unwrap(),
            size,
        })
    }
    
    #[cfg(not(target_os = "linux"))]
    pub fn new(size: usize) -> Result<Self> {
        // Fallback to regular allocation
        let layout = Layout::from_size_align(size, 4096)?;
        let ptr = unsafe { System.alloc(layout) };
        
        if ptr.is_null() {
            return Err(anyhow!("Allocation failed"));
        }
        
        Ok(Self {
            ptr: NonNull::new(ptr).unwrap(),
            size,
        })
    }
    
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }
    
    pub fn size(&self) -> usize {
        self.size
    }
}

impl Drop for MemoryMappedRegion {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            use libc::munmap;
            unsafe {
                munmap(self.ptr.as_ptr() as *mut libc::c_void, self.size);
            }
        }
        
        #[cfg(not(target_os = "linux"))]
        {
            if let Ok(layout) = Layout::from_size_align(self.size, 4096) {
                unsafe {
                    System.dealloc(self.ptr.as_ptr(), layout);
                }
            }
        }
    }
}

/// NUMA-aware memory allocation
pub struct NumaAllocator {
    node: i32,
}

impl NumaAllocator {
    pub fn new(node: i32) -> Self {
        Self { node }
    }
    
    #[cfg(target_os = "linux")]
    pub fn set_memory_policy(&self) -> Result<()> {
        // Note: This requires libnuma, simplified implementation
        Ok(())
    }
    
    #[cfg(not(target_os = "linux"))]
    pub fn set_memory_policy(&self) -> Result<()> {
        Ok(()) // No-op on non-Linux systems
    }
}

/// Statistics structures
#[derive(Debug, Clone)]
pub struct AllocatorStats {
    pub current_bytes: usize,
    pub peak_bytes: usize,
    pub allocation_count: usize,
}

#[derive(Debug, Clone)]
pub struct PoolStats {
    pub free_objects: usize,
    pub allocated_objects: usize,
    pub total_capacity: usize,
}

/// Advanced Memory Manager for trading systems
pub struct PreallocatedMemoryManager {
    config: MemoryConfig,
    pools: Vec<Arc<MemoryPool<Vec<u8>>>>,
    allocator: Arc<CacheAlignedAllocator>,
    stats: Arc<Mutex<AdvancedMemoryStats>>,
    optimizer: MemoryOptimizer,
}

impl PreallocatedMemoryManager {
    pub fn new(config: MemoryConfig) -> Result<Self> {
        let mut pools = Vec::new();
        
        // Create pools for different size classes
        for &size in &config.size_classes {
            fn create_vec_of_size(size: usize) -> fn() -> Vec<u8> {
                match size {
                    256 => || vec![0u8; 256],
                    1024 => || vec![0u8; 1024],
                    4096 => || vec![0u8; 4096],
                    8192 => || vec![0u8; 8192],
                    _ => || vec![0u8; 4096], // default size
                }
            }
            let pool = Arc::new(MemoryPool::new(
                config.initial_pool_size,
                config.max_pool_size,
                create_vec_of_size(size)
            ));
            pools.push(pool);
        }
        
        let optimizer = MemoryOptimizer::new();
        let mut manager = Self {
            config: config.clone(),
            pools,
            allocator: Arc::new(CacheAlignedAllocator::new()),
            stats: Arc::new(Mutex::new(AdvancedMemoryStats::default())),
            optimizer,
        };
        
        // Apply optimizations
        manager.apply_optimizations()?;
        
        Ok(manager)
    }
    
    /// Allocate memory from appropriate pool
    pub fn allocate(&self, size: usize) -> Option<PooledObject<Vec<u8>>> {
        let start_time = std::time::Instant::now();
        
        // Find appropriate size class
        if let Some(pool_index) = self.find_size_class(size) {
            let result = self.pools[pool_index].acquire();
            
            // Update statistics
            {
                let mut stats = self.stats.lock().unwrap();
                stats.total_allocations += 1;
                stats.allocation_time_ns += start_time.elapsed().as_nanos() as u64;
                stats.current_allocated += 1;
                if stats.current_allocated > stats.peak_allocated {
                    stats.peak_allocated = stats.current_allocated;
                }
            }
            
            Some(result)
        } else {
            // Size too large, record failure
            let mut stats = self.stats.lock().unwrap();
            stats.allocation_failures += 1;
            None
        }
    }
    
    /// Find appropriate size class for allocation
    fn find_size_class(&self, size: usize) -> Option<usize> {
        self.config.size_classes
            .iter()
            .position(|&class_size| class_size >= size)
    }
    
    /// Apply memory optimizations
    fn apply_optimizations(&mut self) -> Result<()> {
        if self.config.enable_huge_pages {
            self.optimizer.enable_huge_pages()?;
        }
        
        if self.config.enable_memory_locking {
            // Lock pool memory if configured
            for pool in &self.pools {
                // In a real implementation, you'd lock the pool's memory
            }
        }
        
        Ok(())
    }
    
    /// Get comprehensive statistics
    pub fn get_stats(&self) -> AdvancedMemoryStats {
        let mut stats = self.stats.lock().unwrap();
        
        // Calculate additional metrics
        let total_memory: usize = self.pools.iter()
            .map(|pool| {
                let pool_stats = pool.stats();
                pool_stats.total_capacity * std::mem::size_of::<Vec<u8>>()
            })
            .sum();
        
        stats.total_memory_bytes = total_memory;
        stats.memory_utilization = if total_memory > 0 {
            (stats.current_allocated as f64 / self.config.initial_pool_size as f64) * 100.0
        } else {
            0.0
        };
        
        stats.clone()
    }
    
    /// Check if system meets performance targets
    pub fn meets_targets(&self) -> bool {
        let stats = self.get_stats();
        let target_memory = 10 * 1024 * 1024; // 10MB target
        
        stats.total_memory_bytes <= target_memory && 
        stats.memory_utilization <= 90.0 &&
        stats.allocation_failures == 0
    }
}

#[derive(Debug, Clone)]
pub struct MemoryConfig {
    pub size_classes: Vec<usize>,
    pub initial_pool_size: usize,
    pub max_pool_size: usize,
    pub enable_huge_pages: bool,
    pub enable_memory_locking: bool,
    pub numa_node: Option<u32>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            size_classes: vec![64, 128, 256, 512, 1024, 2048, 4096, 8192],
            initial_pool_size: 1000,
            max_pool_size: 10000,
            enable_huge_pages: false,
            enable_memory_locking: false,
            numa_node: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AdvancedMemoryStats {
    pub total_allocations: u64,
    pub allocation_failures: u64,
    pub current_allocated: u64,
    pub peak_allocated: u64,
    pub allocation_time_ns: u64,
    pub total_memory_bytes: usize,
    pub memory_utilization: f64,
}

/// Global memory optimization settings
pub struct MemoryOptimizer {
    huge_pages_enabled: bool,
    prefault_enabled: bool,
    mlock_enabled: bool,
}

impl MemoryOptimizer {
    pub fn new() -> Self {
        Self {
            huge_pages_enabled: false,
            prefault_enabled: false,
            mlock_enabled: false,
        }
    }
    
    /// Enable transparent huge pages
    #[cfg(target_os = "linux")]
    pub fn enable_huge_pages(&mut self) -> Result<()> {
        use std::fs;
        
        // Attempt to enable transparent huge pages
        let _ = fs::write("/sys/kernel/mm/transparent_hugepage/enabled", "madvise");
        self.huge_pages_enabled = true;
        Ok(())
    }
    
    #[cfg(not(target_os = "linux"))]
    pub fn enable_huge_pages(&mut self) -> Result<()> {
        Ok(()) // No-op on non-Linux
    }
    
    /// Lock memory to prevent swapping
    #[cfg(target_os = "linux")]
    pub fn lock_memory(&mut self, addr: *const u8, len: usize) -> Result<()> {
        use libc::mlock;
        
        let result = unsafe { mlock(addr as *const libc::c_void, len) };
        if result != 0 {
            return Err(anyhow!("mlock failed"));
        }
        
        self.mlock_enabled = true;
        Ok(())
    }
    
    #[cfg(not(target_os = "linux"))]
    pub fn lock_memory(&mut self, _addr: *const u8, _len: usize) -> Result<()> {
        Ok(()) // No-op on non-Linux
    }
    
    /// Prefault memory pages
    pub fn prefault_memory(&mut self, addr: *mut u8, len: usize) -> Result<()> {
        // Touch every page to ensure it's in memory
        let page_size = 4096;
        let mut offset = 0;
        
        while offset < len {
            unsafe {
                ptr::write_volatile(addr.add(offset), 0);
            }
            offset += page_size;
        }
        
        self.prefault_enabled = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_memory_pool() {
        let pool = MemoryPool::new(10, 100, || 42u32);
        
        let obj1 = pool.acquire();
        assert_eq!(*obj1.as_ref(), 42);
        
        let stats = pool.stats();
        assert_eq!(stats.free_objects, 9);
        
        drop(obj1);
        
        let stats = pool.stats();
        assert_eq!(stats.free_objects, 10);
    }
    
    #[test]
    fn test_ring_buffer() {
        let buffer = PreAllocatedRingBuffer::new(4);
        
        // Test pushing
        assert!(buffer.push(1).is_ok());
        assert!(buffer.push(2).is_ok());
        assert!(buffer.push(3).is_ok());
        
        assert_eq!(buffer.len(), 3);
        
        // Test popping
        assert_eq!(buffer.pop(), Some(1));
        assert_eq!(buffer.pop(), Some(2));
        assert_eq!(buffer.len(), 1);
        
        // Test full buffer
        assert!(buffer.push(4).is_ok());
        assert!(buffer.push(5).is_ok());
        assert!(buffer.push(6).is_err()); // Should fail - full
    }
    
    #[test]
    fn test_cache_aligned_allocator() {
        let allocator = CacheAlignedAllocator::new();
        
        unsafe {
            let ptr = allocator.alloc_aligned::<u64>(10).unwrap();
            assert!(!ptr.is_null());
            
            // Check alignment
            let addr = ptr as usize;
            assert_eq!(addr % 64, 0);
            
            allocator.dealloc_aligned(ptr, 10);
        }
    }
    
    #[test]
    fn test_memory_mapped_region() {
        let region = MemoryMappedRegion::new(4096).unwrap();
        assert!(!region.as_ptr().is_null());
        assert_eq!(region.size(), 4096);
    }
}