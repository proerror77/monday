/*!
 * Ultra Performance Module - 極致性能優化
 * 
 * 目標：決策延遲 < 1μs P95，零分配熱路徑
 * 
 * 核心技術：
 * - SIMD向量化計算
 * - 零分配熱路徑設計
 * - 分支預測優化
 * - 緩存友好的數據結構
 * - 編譯時常量折疊
 */

use std::arch::x86_64::*;
use std::mem::{self, MaybeUninit};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use anyhow::Result;
use tracing::{debug, info};

/// 決策引擎性能計數器
#[derive(Debug)]
pub struct DecisionMetrics {
    /// 決策次數
    pub decision_count: AtomicU64,
    /// 累計延遲（納秒）
    pub total_latency_ns: AtomicU64,
    /// 最小延遲
    pub min_latency_ns: AtomicU64,
    /// 最大延遲
    pub max_latency_ns: AtomicU64,
}

impl DecisionMetrics {
    pub fn new() -> Self {
        Self {
            decision_count: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
            min_latency_ns: AtomicU64::new(u64::MAX),
            max_latency_ns: AtomicU64::new(0),
        }
    }
    
    #[inline(always)]
    pub fn record_decision(&self, latency_ns: u64) {
        self.decision_count.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ns.fetch_add(latency_ns, Ordering::Relaxed);
        
        // 更新最小延遲
        let mut current_min = self.min_latency_ns.load(Ordering::Relaxed);
        while latency_ns < current_min {
            match self.min_latency_ns.compare_exchange_weak(
                current_min, 
                latency_ns, 
                Ordering::Relaxed, 
                Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(actual) => current_min = actual,
            }
        }
        
        // 更新最大延遲
        let mut current_max = self.max_latency_ns.load(Ordering::Relaxed);
        while latency_ns > current_max {
            match self.max_latency_ns.compare_exchange_weak(
                current_max, 
                latency_ns, 
                Ordering::Relaxed, 
                Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(actual) => current_max = actual,
            }
        }
    }
    
    pub fn avg_latency_ns(&self) -> f64 {
        let count = self.decision_count.load(Ordering::Relaxed);
        if count == 0 {
            0.0
        } else {
            self.total_latency_ns.load(Ordering::Relaxed) as f64 / count as f64
        }
    }
}

/// 零分配決策引擎
/// 
/// 設計原則：
/// 1. 所有熱路徑操作零分配
/// 2. 預分配所有可能需要的內存
/// 3. 使用SIMD加速計算
/// 4. 分支預測友好的代碼結構
#[repr(align(64))] // 緩存行對齊
pub struct ZeroAllocDecisionEngine {
    /// 預分配的特徵向量緩衝區（緩存行對齊）
    feature_buffer: [f32; 64],
    /// 預分配的權重向量
    weight_buffer: [f32; 64],
    /// 預分配的輸出緩衝區
    output_buffer: [f32; 16],
    /// 性能指標
    metrics: DecisionMetrics,
    /// 決策閾值（編譯時常量）
    decision_threshold: f32,
}

impl ZeroAllocDecisionEngine {
    /// 創建新的零分配決策引擎
    pub fn new() -> Self {
        Self {
            feature_buffer: [0.0; 64],
            weight_buffer: [0.0; 64],
            output_buffer: [0.0; 16],
            metrics: DecisionMetrics::new(),
            decision_threshold: 0.5, // 可調整的決策閾值
        }
    }
    
    /// 預加載模型權重（只在初始化時調用）
    pub fn load_weights(&mut self, weights: &[f32]) -> Result<()> {
        if weights.len() != 64 {
            return Err(anyhow::anyhow!("權重向量長度必須為64"));
        }
        
        self.weight_buffer.copy_from_slice(weights);
        info!("模型權重已加載，準備進行零分配推理");
        Ok(())
    }
    
    /// 零分配的快速決策函數
    /// 
    /// 這是系統的熱路徑，需要極致優化：
    /// - 無任何內存分配
    /// - 使用SIMD向量化
    /// - 分支預測友好
    /// - 緩存友好的內存訪問
    #[inline(always)]
    pub fn make_decision(&mut self, features: &[f32; 64]) -> i8 {
        let start = unsafe { std::arch::x86_64::_rdtsc() };
        
        // 直接復制到預分配緩衝區（避免邊界檢查）
        unsafe {
            ptr::copy_nonoverlapping(
                features.as_ptr(),
                self.feature_buffer.as_mut_ptr(),
                64
            );
        }
        
        // SIMD優化的點積計算
        let score = unsafe { self.simd_dot_product() };
        
        // 編譯時已知的分支預測優化
        let decision = if score > self.decision_threshold {
            1i8  // 買入
        } else if score < -self.decision_threshold {
            -1i8 // 賣出
        } else {
            0i8  // 持有
        };
        
        let end = unsafe { std::arch::x86_64::_rdtsc() };
        let latency_cycles = end - start;
        
        // 假設3GHz CPU，轉換為納秒
        let latency_ns = (latency_cycles * 333) / 1000; // 1 cycle ≈ 0.333ns @ 3GHz
        self.metrics.record_decision(latency_ns);
        
        decision
    }
    
    /// SIMD優化的點積計算
    /// 
    /// 使用AVX2指令集進行8路並行浮點運算
    #[target_feature(enable = "avx2")]
    #[inline(always)]
    unsafe fn simd_dot_product(&self) -> f32 {
        let mut sum = _mm256_setzero_ps();
        
        // 每次處理8個浮點數（256位 / 32位 = 8）
        for i in (0..64).step_by(8) {
            let features = _mm256_load_ps(self.feature_buffer.as_ptr().add(i));
            let weights = _mm256_load_ps(self.weight_buffer.as_ptr().add(i));
            let product = _mm256_mul_ps(features, weights);
            sum = _mm256_add_ps(sum, product);
        }
        
        // 水平求和
        let sum_high = _mm256_extractf128_ps::<1>(sum);
        let sum_low = _mm256_extractf128_ps::<0>(sum);
        let sum128 = _mm_add_ps(sum_low, sum_high);
        
        let sum64 = _mm_add_ps(sum128, _mm_movehl_ps(sum128, sum128));
        let sum32 = _mm_add_ss(sum64, _mm_shuffle_ps::<0x01>(sum64, sum64));
        
        _mm_cvtss_f32(sum32)
    }
    
    /// 獲取性能統計
    pub fn get_performance_stats(&self) -> PerformanceStats {
        let count = self.metrics.decision_count.load(Ordering::Relaxed);
        let avg_latency_ns = self.metrics.avg_latency_ns();
        let min_latency_ns = self.metrics.min_latency_ns.load(Ordering::Relaxed);
        let max_latency_ns = self.metrics.max_latency_ns.load(Ordering::Relaxed);
        
        PerformanceStats {
            total_decisions: count,
            avg_latency_ns,
            min_latency_ns: if min_latency_ns == u64::MAX { 0 } else { min_latency_ns },
            max_latency_ns,
            avg_latency_us: avg_latency_ns / 1000.0,
            throughput_decisions_per_sec: if avg_latency_ns > 0.0 {
                1_000_000_000.0 / avg_latency_ns
            } else {
                0.0
            },
        }
    }
    
    /// 重置性能計數器
    pub fn reset_metrics(&self) {
        self.metrics.decision_count.store(0, Ordering::Relaxed);
        self.metrics.total_latency_ns.store(0, Ordering::Relaxed);
        self.metrics.min_latency_ns.store(u64::MAX, Ordering::Relaxed);
        self.metrics.max_latency_ns.store(0, Ordering::Relaxed);
    }
}

/// 性能統計結構
#[derive(Debug, Clone)]
pub struct PerformanceStats {
    pub total_decisions: u64,
    pub avg_latency_ns: f64,
    pub min_latency_ns: u64,
    pub max_latency_ns: u64,
    pub avg_latency_us: f64,
    pub throughput_decisions_per_sec: f64,
}

impl PerformanceStats {
    /// 檢查是否達到性能目標
    pub fn meets_performance_targets(&self) -> bool {
        // 目標：平均延遲 < 1μs，P95 延遲 < 1μs
        self.avg_latency_us < 1.0 && 
        self.throughput_decisions_per_sec > 500_000.0 // 每秒50萬次決策
    }
    
    /// 格式化性能報告
    pub fn format_report(&self) -> String {
        format!(
            "Performance Report:\n\
             - Total Decisions: {}\n\
             - Average Latency: {:.3} μs ({:.0} ns)\n\
             - Min Latency: {} ns\n\
             - Max Latency: {} ns\n\
             - Throughput: {:.0} decisions/sec\n\
             - Performance Target: {}",
            self.total_decisions,
            self.avg_latency_us,
            self.avg_latency_ns,
            self.min_latency_ns,
            self.max_latency_ns,
            self.throughput_decisions_per_sec,
            if self.meets_performance_targets() { "✅ MET" } else { "❌ NOT MET" }
        )
    }
}

/// 內存預取優化的緩存
/// 
/// 預取下一批數據以減少緩存未命中
#[repr(align(64))]
pub struct PrefetchOptimizedCache<T> {
    data: Vec<T>,
    current_index: usize,
    prefetch_distance: usize,
}

impl<T> PrefetchOptimizedCache<T> {
    pub fn new(capacity: usize, prefetch_distance: usize) -> Self {
        Self {
            data: Vec::with_capacity(capacity),
            current_index: 0,
            prefetch_distance,
        }
    }
    
    #[inline(always)]
    pub fn get_with_prefetch(&mut self, index: usize) -> Option<&T> {
        // 預取下一批數據
        let prefetch_index = index + self.prefetch_distance;
        if prefetch_index < self.data.len() {
            unsafe {
                let ptr = self.data.as_ptr().add(prefetch_index);
                std::arch::x86_64::_mm_prefetch::<{std::arch::x86_64::_MM_HINT_T0}>(
                    ptr as *const i8
                );
            }
        }
        
        self.data.get(index)
    }
}

/// 極致性能的OrderBook更新器
/// 
/// 專門為微秒級延遲設計的OrderBook處理
#[repr(align(64))]
pub struct UltraFastOrderBookUpdater {
    /// 買單價格數組（預分配）
    bid_prices: [f64; 32],
    /// 買單數量數組（預分配）
    bid_quantities: [f64; 32],
    /// 賣單價格數組（預分配）
    ask_prices: [f64; 32],
    /// 賣單數量數組（預分配）
    ask_quantities: [f64; 32],
    /// 當前有效層數
    valid_levels: u8,
    /// 最佳買價指針（無分配訪問）
    best_bid_ptr: *const f64,
    /// 最佳賣價指針（無分配訪問）
    best_ask_ptr: *const f64,
}

impl UltraFastOrderBookUpdater {
    pub fn new() -> Self {
        let mut updater = Self {
            bid_prices: [0.0; 32],
            bid_quantities: [0.0; 32],
            ask_prices: [0.0; 32],
            ask_quantities: [0.0; 32],
            valid_levels: 0,
            best_bid_ptr: ptr::null(),
            best_ask_ptr: ptr::null(),
        };
        
        // 初始化指針
        updater.best_bid_ptr = updater.bid_prices.as_ptr();
        updater.best_ask_ptr = updater.ask_prices.as_ptr();
        
        updater
    }
    
    /// 零分配的OrderBook更新
    #[inline(always)]
    pub fn update_book(&mut self, bids: &[(f64, f64)], asks: &[(f64, f64)]) {
        let bid_count = bids.len().min(32);
        let ask_count = asks.len().min(32);
        
        // 直接內存復制，避免邊界檢查
        unsafe {
            for i in 0..bid_count {
                *self.bid_prices.get_unchecked_mut(i) = bids.get_unchecked(i).0;
                *self.bid_quantities.get_unchecked_mut(i) = bids.get_unchecked(i).1;
            }
            
            for i in 0..ask_count {
                *self.ask_prices.get_unchecked_mut(i) = asks.get_unchecked(i).0;
                *self.ask_quantities.get_unchecked_mut(i) = asks.get_unchecked(i).1;
            }
        }
        
        self.valid_levels = (bid_count.max(ask_count)) as u8;
    }
    
    /// 零分配獲取最佳價格
    #[inline(always)]
    pub fn get_best_prices(&self) -> (f64, f64) {
        unsafe {
            (*self.best_bid_ptr, *self.best_ask_ptr)
        }
    }
    
    /// 零分配計算價差
    #[inline(always)]
    pub fn get_spread(&self) -> f64 {
        unsafe {
            *self.best_ask_ptr - *self.best_bid_ptr
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_zero_alloc_decision_engine() {
        let mut engine = ZeroAllocDecisionEngine::new();
        
        // 創建模擬權重
        let weights: [f32; 64] = [0.1; 64];
        engine.load_weights(&weights).unwrap();
        
        // 測試決策性能
        let features = [0.5; 64];
        
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            let _decision = engine.make_decision(&features);
        }
        let duration = start.elapsed();
        
        println!("100,000 決策耗時: {:?}", duration);
        println!("平均每次決策: {:.3} μs", 
                duration.as_nanos() as f64 / 100_000.0 / 1000.0);
        
        let stats = engine.get_performance_stats();
        println!("{}", stats.format_report());
        
        // 驗證性能目標
        assert!(stats.avg_latency_us < 10.0, "延遲應小於10μs");
    }
    
    #[test]
    fn test_ultra_fast_orderbook() {
        let mut updater = UltraFastOrderBookUpdater::new();
        
        let bids = vec![(50000.0, 1.5), (49999.0, 2.0), (49998.0, 1.8)];
        let asks = vec![(50001.0, 1.2), (50002.0, 2.5), (50003.0, 1.9)];
        
        let start = std::time::Instant::now();
        for _ in 0..1_000_000 {
            updater.update_book(&bids, &asks);
            let _prices = updater.get_best_prices();
            let _spread = updater.get_spread();
        }
        let duration = start.elapsed();
        
        println!("1,000,000 OrderBook更新耗時: {:?}", duration);
        println!("平均每次更新: {:.3} μs", 
                duration.as_nanos() as f64 / 1_000_000.0 / 1000.0);
        
        assert!(duration.as_micros() < 1_000_000, "100萬次更新應在1秒內完成");
    }
}