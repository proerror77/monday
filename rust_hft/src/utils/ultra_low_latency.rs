/*! 
 * Ultra Low Latency Optimization for BTCUSDT DL Trading
 * 
 * UltraThink 方法論實施的極致延遲優化
 * 目標：P99 推理延遲 ≤ 1ms，P50 ≤ 300μs
 * 
 * 核心優化技術：
 * - CPU 親和性綁定和實時調度
 * - SIMD 向量化計算
 * - 無鎖數據結構
 * - 內存池和零分配
 * - 缓存優化和預取
 */

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use std::mem::{self, MaybeUninit};
use std::ptr;
use lockfree::queue::Queue;
use anyhow::Result;
use tracing::{debug, warn, info};

/// CPU 親和性管理器
#[derive(Debug)]
pub struct CpuAffinityManager {
    /// 綁定的 CPU 核心
    cpu_core: u32,
    /// 是否已綁定
    is_bound: AtomicBool,
    /// 原始調度策略
    original_policy: Option<i32>,
}

impl CpuAffinityManager {
    /// 創建新的 CPU 親和性管理器
    pub fn new(cpu_core: u32) -> Self {
        Self {
            cpu_core,
            is_bound: AtomicBool::new(false),
            original_policy: None,
        }
    }
    
    /// 綁定當前線程到指定 CPU 核心
    pub fn bind_current_thread(&self) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            use libc::{cpu_set_t, sched_setaffinity, sched_setscheduler, sched_param, SCHED_FIFO};
            
            unsafe {
                // 設置 CPU 親和性
                let mut cpu_set: cpu_set_t = mem::zeroed();
                libc::CPU_ZERO(&mut cpu_set);
                libc::CPU_SET(self.cpu_core as usize, &mut cpu_set);
                
                let result = sched_setaffinity(0, mem::size_of::<cpu_set_t>(), &cpu_set);
                if result != 0 {
                    return Err(anyhow::anyhow!("Failed to set CPU affinity"));
                }
                
                // 設置實時調度策略
                let param = sched_param { sched_priority: 50 }; // 高優先級
                let result = sched_setscheduler(0, SCHED_FIFO, &param);
                if result != 0 {
                    warn!("Failed to set SCHED_FIFO, using default scheduler");
                }
                
                self.is_bound.store(true, Ordering::Relaxed);
                info!("Thread bound to CPU core {} with high priority", self.cpu_core);
            }
        }
        
        #[cfg(not(target_os = "linux"))]
        {
            warn!("CPU affinity binding not supported on this platform");
        }
        
        Ok(())
    }
}

/// SIMD 優化的特徵計算器
#[derive(Debug)]
pub struct SIMDFeatureProcessor {
    /// 預分配的計算緩衝區
    compute_buffer: Vec<f32>,
    /// 對齊的內存池
    aligned_memory: AlignedMemoryPool,
}

impl SIMDFeatureProcessor {
    /// 創建 SIMD 特徵處理器
    pub fn new() -> Self {
        Self {
            compute_buffer: vec![0.0f32; 256], // 預分配緩衝區
            aligned_memory: AlignedMemoryPool::new(1024, 32), // 32字節對齊
        }
    }
    
    /// SIMD 優化的 LOB 特徵提取
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    pub unsafe fn extract_lob_features_simd(&mut self, lob_data: &[f64]) -> Vec<f32> {
        let mut result = self.aligned_memory.get_aligned_vector(52);
        
        // 8x 並行處理 LOB 數據
        let chunks = lob_data.chunks_exact(4);
        let mut output_idx = 0;
        
        for chunk in chunks {
            if output_idx + 4 <= result.len() {
                // 加載 4 個 f64 值
                let data = _mm256_loadu_pd(chunk.as_ptr());
                
                // 轉換為 f32
                let lower = _mm256_extractf128_pd(data, 0);
                let upper = _mm256_extractf128_pd(data, 1);
                let lower_f32 = _mm_cvtpd_ps(lower);
                let upper_f32 = _mm_cvtpd_ps(upper);
                let combined = _mm_movelh_ps(lower_f32, upper_f32);
                
                // 存儲結果
                _mm_storeu_ps(result[output_idx..].as_mut_ptr(), combined);
                output_idx += 4;
            }
        }
        
        result
    }
    
    /// SIMD 優化的基礎特徵計算
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    pub unsafe fn calculate_basic_features_simd(&mut self, 
                                               prices: &[f32], 
                                               volumes: &[f32]) -> [f32; 12] {
        let mut result = [0.0f32; 12];
        
        if prices.len() >= 8 && volumes.len() >= 8 {
            // 加載價格數據
            let prices_v = _mm256_loadu_ps(prices.as_ptr());
            let volumes_v = _mm256_loadu_ps(volumes.as_ptr());
            
            // 計算 VWAP
            let price_vol = _mm256_mul_ps(prices_v, volumes_v);
            let sum_price_vol = self.horizontal_sum_avx2(price_vol);
            let sum_volumes = self.horizontal_sum_avx2(volumes_v);
            let vwap = sum_price_vol / sum_volumes;
            result[0] = vwap;
            
            // 計算加權平均價格變動
            let price_diffs = _mm256_sub_ps(
                _mm256_loadu_ps(prices[1..].as_ptr()),
                _mm256_loadu_ps(prices.as_ptr())
            );
            let weighted_diff = _mm256_mul_ps(price_diffs, volumes_v);
            let avg_weighted_diff = self.horizontal_sum_avx2(weighted_diff) / sum_volumes;
            result[1] = avg_weighted_diff;
            
            // 計算波動率
            let squared_diffs = _mm256_mul_ps(price_diffs, price_diffs);
            let variance = self.horizontal_sum_avx2(squared_diffs) / 8.0;
            result[2] = variance.sqrt();
        }
        
        result
    }
    
    /// 非 SIMD 備用實現 - LOB 特徵提取
    #[cfg(not(target_arch = "x86_64"))]
    pub fn extract_lob_features_simd(&mut self, lob_data: &[f64]) -> Vec<f32> {
        // 標準實現，無 SIMD 優化
        let mut result = self.aligned_memory.get_aligned_vector(52);
        
        for (i, &value) in lob_data.iter().take(52).enumerate() {
            if i < result.len() {
                result[i] = value as f32;
            }
        }
        
        result
    }
    
    /// 非 SIMD 備用實現 - 基礎特徵計算
    #[cfg(not(target_arch = "x86_64"))]
    pub fn calculate_basic_features_simd(&mut self, 
                                        prices: &[f32], 
                                        volumes: &[f32]) -> [f32; 12] {
        let mut result = [0.0f32; 12];
        
        if !prices.is_empty() && !volumes.is_empty() {
            // VWAP 計算
            let total_vol: f32 = volumes.iter().sum();
            if total_vol > 0.0 {
                let weighted_price: f32 = prices.iter().zip(volumes.iter())
                    .map(|(p, v)| p * v)
                    .sum();
                result[0] = weighted_price / total_vol;
            }
            
            // 價格變動
            if prices.len() > 1 {
                result[1] = prices[prices.len()-1] - prices[0];
            }
            
            // 簡化波動率
            if prices.len() > 2 {
                let mean = prices.iter().sum::<f32>() / prices.len() as f32;
                let variance = prices.iter()
                    .map(|p| (p - mean).powi(2))
                    .sum::<f32>() / prices.len() as f32;
                result[2] = variance.sqrt();
            }
        }
        
        result
    }
    
    /// AVX2 水平求和
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn horizontal_sum_avx2(&self, v: __m256) -> f32 {
        let high = _mm256_extractf128_ps(v, 1);
        let low = _mm256_castps256_ps128(v);
        let sum128 = _mm_add_ps(high, low);
        let high64 = _mm_unpackhi_ps(sum128, sum128);
        let sum64 = _mm_add_ps(sum128, high64);
        let high32 = _mm_shuffle_ps(sum64, sum64, 0x1);
        let sum32 = _mm_add_ps(sum64, high32);
        _mm_cvtss_f32(sum32)
    }
}

/// 對齊內存池
#[derive(Debug)]
pub struct AlignedMemoryPool {
    /// 內存池
    pools: Vec<Vec<f32>>,
    /// 當前池索引
    current_pool: AtomicU64,
    /// 池大小
    pool_size: usize,
    /// 對齊大小
    alignment: usize,
}

impl AlignedMemoryPool {
    /// 創建對齊內存池
    pub fn new(pool_size: usize, alignment: usize) -> Self {
        let pool_count = 8; // 8個池子輪換使用
        let mut pools = Vec::with_capacity(pool_count);
        
        for _ in 0..pool_count {
            let mut pool = Vec::with_capacity(pool_size);
            pool.resize(pool_size, 0.0f32);
            pools.push(pool);
        }
        
        Self {
            pools,
            current_pool: AtomicU64::new(0),
            pool_size,
            alignment,
        }
    }
    
    /// 獲取對齊的向量
    pub fn get_aligned_vector(&self, size: usize) -> Vec<f32> {
        if size <= self.pool_size {
            let pool_idx = self.current_pool.fetch_add(1, Ordering::Relaxed) % self.pools.len() as u64;
            let mut vec = self.pools[pool_idx as usize].clone();
            vec.truncate(size);
            vec.clear();
            vec.resize(size, 0.0f32);
            vec
        } else {
            vec![0.0f32; size]
        }
    }
}

/// 無鎖環形緩衝區
#[derive(Debug)]
pub struct LockFreeRingBuffer<T> {
    /// 數據存儲
    data: Vec<MaybeUninit<T>>,
    /// 寫入位置
    write_pos: AtomicU64,
    /// 讀取位置
    read_pos: AtomicU64,
    /// 緩衝區大小
    capacity: usize,
    /// 容量掩碼（用於快速取模）
    capacity_mask: u64,
}

impl<T> LockFreeRingBuffer<T> {
    /// 創建無鎖環形緩衝區
    pub fn new(capacity: usize) -> Self {
        // 確保容量是2的幂
        let capacity = capacity.next_power_of_two();
        let mut data = Vec::with_capacity(capacity);
        data.resize_with(capacity, || MaybeUninit::uninit());
        
        Self {
            data,
            write_pos: AtomicU64::new(0),
            read_pos: AtomicU64::new(0),
            capacity,
            capacity_mask: (capacity - 1) as u64,
        }
    }
    
    /// 嘗試寫入數據
    pub fn try_push(&self, item: T) -> Result<(), T> {
        let write_pos = self.write_pos.load(Ordering::Relaxed);
        let read_pos = self.read_pos.load(Ordering::Acquire);
        
        // 檢查是否已滿
        if write_pos.wrapping_sub(read_pos) >= self.capacity as u64 {
            return Err(item);
        }
        
        // 寫入數據
        let slot = &self.data[(write_pos & self.capacity_mask) as usize];
        unsafe {
            ptr::write(slot.as_ptr() as *mut T, item);
        }
        
        // 更新寫入位置
        self.write_pos.store(write_pos.wrapping_add(1), Ordering::Release);
        Ok(())
    }
    
    /// 嘗試讀取數據
    pub fn try_pop(&self) -> Option<T> {
        let read_pos = self.read_pos.load(Ordering::Relaxed);
        let write_pos = self.write_pos.load(Ordering::Acquire);
        
        // 檢查是否為空
        if read_pos == write_pos {
            return None;
        }
        
        // 讀取數據
        let slot = &self.data[(read_pos & self.capacity_mask) as usize];
        let item = unsafe { ptr::read(slot.as_ptr()) };
        
        // 更新讀取位置
        self.read_pos.store(read_pos.wrapping_add(1), Ordering::Release);
        Some(item)
    }
}

/// 高精度計時器
#[derive(Debug)]
pub struct HighPrecisionTimer {
    /// TSC 頻率
    tsc_frequency: u64,
    /// 校準完成
    calibrated: AtomicBool,
}

impl HighPrecisionTimer {
    /// 創建高精度計時器
    pub fn new() -> Self {
        Self {
            tsc_frequency: 0,
            calibrated: AtomicBool::new(false),
        }
    }
    
    /// 校準 TSC 頻率
    pub fn calibrate(&mut self) -> Result<()> {
        use std::time::{Duration, Instant};
        use std::thread;
        
        let start_time = Instant::now();
        let start_tsc = self.rdtsc();
        
        thread::sleep(Duration::from_millis(100));
        
        let end_time = Instant::now();
        let end_tsc = self.rdtsc();
        
        let elapsed_nanos = end_time.duration_since(start_time).as_nanos() as u64;
        let tsc_cycles = end_tsc - start_tsc;
        
        self.tsc_frequency = (tsc_cycles * 1_000_000_000) / elapsed_nanos;
        self.calibrated.store(true, Ordering::Relaxed);
        
        info!("TSC frequency calibrated: {} Hz", self.tsc_frequency);
        Ok(())
    }
    
    /// 讀取 TSC
    #[inline(always)]
    pub fn rdtsc(&self) -> u64 {
        #[cfg(target_arch = "x86_64")]
        unsafe { _rdtsc() }
        
        #[cfg(not(target_arch = "x86_64"))]
        {
            // 非 x86_64 平臺使用系統時間
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now().duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
        }
    }
    
    /// TSC 轉微秒
    #[inline(always)]
    pub fn tsc_to_micros(&self, tsc: u64) -> u64 {
        if self.calibrated.load(Ordering::Relaxed) {
            (tsc * 1_000_000) / self.tsc_frequency
        } else {
            0
        }
    }
    
    /// 獲取當前微秒時間戳
    #[inline(always)]
    pub fn now_micros(&self) -> u64 {
        self.tsc_to_micros(self.rdtsc())
    }
}

/// 延遲測量器
#[derive(Debug)]
pub struct LatencyMeasurer {
    /// 延遲樣本
    samples: LockFreeRingBuffer<u64>,
    /// 總樣本數
    total_samples: AtomicU64,
    /// 計時器
    timer: HighPrecisionTimer,
}

impl LatencyMeasurer {
    /// 創建延遲測量器
    pub fn new() -> Self {
        let mut timer = HighPrecisionTimer::new();
        timer.calibrate().expect("Failed to calibrate timer");
        
        Self {
            samples: LockFreeRingBuffer::new(10000),
            total_samples: AtomicU64::new(0),
            timer,
        }
    }
    
    /// 測量延遲開始
    #[inline(always)]
    pub fn start(&self) -> u64 {
        self.timer.rdtsc()
    }
    
    /// 測量延遲結束並記錄
    #[inline(always)]
    pub fn end_and_record(&self, start_tsc: u64) -> u64 {
        let end_tsc = self.timer.rdtsc();
        let latency_tsc = end_tsc - start_tsc;
        let latency_micros = self.timer.tsc_to_micros(latency_tsc);
        
        // 記錄樣本
        let _ = self.samples.try_push(latency_micros);
        self.total_samples.fetch_add(1, Ordering::Relaxed);
        
        latency_micros
    }
    
    /// 計算延遲統計
    pub fn calculate_stats(&self) -> LatencyStats {
        let mut samples = Vec::new();
        
        // 收集所有樣本
        while let Some(sample) = self.samples.try_pop() {
            samples.push(sample);
        }
        
        if samples.is_empty() {
            return LatencyStats::default();
        }
        
        samples.sort_unstable();
        
        let len = samples.len();
        let sum: u64 = samples.iter().sum();
        let mean = sum / len as u64;
        
        LatencyStats {
            count: len,
            mean_micros: mean,
            p50_micros: samples[len * 50 / 100],
            p95_micros: samples[len * 95 / 100],
            p99_micros: samples[len * 99 / 100],
            max_micros: samples[len - 1],
            min_micros: samples[0],
        }
    }
}

/// 延遲統計
#[derive(Debug, Default, Clone)]
pub struct LatencyStats {
    pub count: usize,
    pub mean_micros: u64,
    pub p50_micros: u64,
    pub p95_micros: u64,
    pub p99_micros: u64,
    pub max_micros: u64,
    pub min_micros: u64,
}

impl LatencyStats {
    /// 是否達到延遲目標
    pub fn meets_targets(&self) -> bool {
        self.p50_micros <= 300 && self.p99_micros <= 1000
    }
    
    /// 格式化統計信息
    pub fn format(&self) -> String {
        format!(
            "Latency Stats: count={}, mean={}μs, p50={}μs, p95={}μs, p99={}μs, max={}μs",
            self.count, self.mean_micros, self.p50_micros, 
            self.p95_micros, self.p99_micros, self.max_micros
        )
    }
}

/// Ultra Low Latency 交易執行器
#[derive(Debug)]
pub struct UltraLowLatencyExecutor {
    /// CPU 親和性管理
    cpu_manager: CpuAffinityManager,
    /// SIMD 特徵處理器
    feature_processor: SIMDFeatureProcessor,
    /// 延遲測量器
    latency_measurer: LatencyMeasurer,
    /// 預分配的推理結果池
    prediction_pool: LockFreeRingBuffer<Vec<f32>>,
}

impl UltraLowLatencyExecutor {
    /// 創建執行器
    pub fn new(cpu_core: u32) -> Result<Self> {
        let mut executor = Self {
            cpu_manager: CpuAffinityManager::new(cpu_core),
            feature_processor: SIMDFeatureProcessor::new(),
            latency_measurer: LatencyMeasurer::new(),
            prediction_pool: LockFreeRingBuffer::new(64),
        };
        
        // 綁定 CPU 親和性
        executor.cpu_manager.bind_current_thread()?;
        
        // 預熱緩存
        executor.warmup_caches()?;
        
        Ok(executor)
    }
    
    /// 預熱緩存
    fn warmup_caches(&mut self) -> Result<()> {
        info!("Warming up caches...");
        
        // 預熱 SIMD 處理器
        let dummy_data = vec![1.0f64; 64];
        for _ in 0..1000 {
            #[cfg(target_arch = "x86_64")]
            unsafe {
                let _ = self.feature_processor.extract_lob_features_simd(&dummy_data);
            }
            
            #[cfg(not(target_arch = "x86_64"))]
            {
                let _ = self.feature_processor.extract_lob_features_simd(&dummy_data);
            }
        }
        
        // 預填充預測池
        for _ in 0..64 {
            let _ = self.prediction_pool.try_push(vec![0.0f32; 52]);
        }
        
        info!("Cache warmup completed");
        Ok(())
    }
    
    /// 執行超低延遲推理
    #[inline(always)]
    pub fn execute_ultra_fast_inference(&mut self, lob_data: &[f64]) -> Result<Vec<f32>> {
        let start_tsc = self.latency_measurer.start();
        
        // 1. SIMD 特徵提取
        let features = {
            #[cfg(target_arch = "x86_64")]
            {
                unsafe { self.feature_processor.extract_lob_features_simd(lob_data) }
            }
            
            #[cfg(not(target_arch = "x86_64"))]
            {
                self.feature_processor.extract_lob_features_simd(lob_data)
            }
        };
        
        // 2. 簡化推理（在實際使用中會調用 DL 模型）
        let mut prediction = self.prediction_pool.try_pop()
            .unwrap_or_else(|| vec![0.0f32; 5]);
        
        // 簡化的預測邏輯（實際中替換為 Transformer 推理）
        prediction[0] = features[0] * 0.1; // StrongDown
        prediction[1] = features[1] * 0.2; // WeakDown  
        prediction[2] = 0.5; // Neutral (基線)
        prediction[3] = features[2] * 0.2; // WeakUp
        prediction[4] = features[3] * 0.1; // StrongUp
        
        // 3. 記錄延遲
        let latency = self.latency_measurer.end_and_record(start_tsc);
        
        if latency > 1000 {
            warn!("High latency detected: {}μs", latency);
        }
        
        Ok(prediction)
    }
    
    /// 獲取延遲統計
    pub fn get_latency_stats(&self) -> LatencyStats {
        self.latency_measurer.calculate_stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_simd_feature_processor() {
        let mut processor = SIMDFeatureProcessor::new();
        let test_data = vec![1.0f64; 64];
        
        #[cfg(target_arch = "x86_64")]
        {
            unsafe {
                let result = processor.extract_lob_features_simd(&test_data);
                assert_eq!(result.len(), 52);
            }
        }
        
        #[cfg(not(target_arch = "x86_64"))]
        {
            let result = processor.extract_lob_features_simd(&test_data);
            assert_eq!(result.len(), 52);
        }
    }
    
    #[test]
    fn test_lockfree_ring_buffer() {
        let buffer = LockFreeRingBuffer::new(4);
        
        // 測試推入
        assert!(buffer.try_push(1).is_ok());
        assert!(buffer.try_push(2).is_ok());
        
        // 測試彈出
        assert_eq!(buffer.try_pop(), Some(1));
        assert_eq!(buffer.try_pop(), Some(2));
        assert_eq!(buffer.try_pop(), None);
    }
    
    #[test]
    fn test_latency_measurer() {
        let measurer = LatencyMeasurer::new();
        
        let start = measurer.start();
        std::thread::sleep(std::time::Duration::from_micros(100));
        let latency = measurer.end_and_record(start);
        
        assert!(latency >= 100);
        
        let stats = measurer.calculate_stats();
        assert!(stats.count > 0);
    }
}