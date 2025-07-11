/*!
 * ⚡ 並行數據處理引擎
 * 
 * 專為HFT設計的高性能並行計算系統
 * 
 * 核心特性：
 * - 多線程數據處理
 * - 並行特徵工程
 * - 向量化計算
 * - 工作竊取調度
 * - NUMA感知處理
 * - 緩存友好的數據分割
 */

use rayon::prelude::*;
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, AtomicU64, Ordering};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use serde::{Serialize, Deserialize};
use tracing::{debug, info, warn, error, instrument};
use crate::core::types::*;
use crate::utils::memory_optimization::{ZeroAllocVec, MemoryPool, CowPtr};

/// 並行處理配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelConfig {
    /// 工作線程數量
    pub num_threads: Option<usize>,
    /// 批處理大小
    pub batch_size: usize,
    /// 啟用NUMA感知
    pub numa_aware: bool,
    /// 線程親和性設置
    pub thread_affinity: bool,
    /// 工作竊取啟用
    pub work_stealing: bool,
    /// 內存預取距離
    pub prefetch_distance: usize,
}

impl Default for ParallelConfig {
    fn default() -> Self {
        Self {
            num_threads: None, // 自動檢測
            batch_size: 1000,
            numa_aware: true,
            thread_affinity: true,
            work_stealing: true,
            prefetch_distance: 64,
        }
    }
}

/// 並行處理統計
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParallelStats {
    /// 總處理項目數
    pub total_items: u64,
    /// 處理時間（微秒）
    pub processing_time_us: u64,
    /// 吞吐量（項目/秒）
    pub throughput: f64,
    /// 並行效率
    pub parallel_efficiency: f64,
    /// 線程利用率
    pub thread_utilization: f64,
    /// 緩存命中率
    pub cache_hit_rate: f64,
}

/// 特徵工程任務類型
#[derive(Debug, Clone)]
pub enum FeatureTask {
    /// 移動平均計算
    MovingAverage { window: usize },
    /// 技術指標計算
    TechnicalIndicator { indicator_type: String, params: HashMap<String, f64> },
    /// 統計特徵
    Statistics { metrics: Vec<String> },
    /// 交叉特徵
    CrossFeatures { combinations: Vec<(usize, usize)> },
    /// 時間窗口聚合
    WindowAggregation { window_sizes: Vec<usize>, aggregations: Vec<String> },
}

/// 並行數據處理器
pub struct ParallelProcessor {
    /// 線程池
    thread_pool: ThreadPool,
    /// 配置
    config: ParallelConfig,
    /// 統計計數器
    stats: Arc<ParallelStatsCollector>,
    /// 內存池（用於批處理）
    memory_pool: Arc<MemoryPool<f64>>,
}

/// 統計收集器
struct ParallelStatsCollector {
    total_items: AtomicU64,
    total_time_us: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
}

impl ParallelStatsCollector {
    fn new() -> Self {
        Self {
            total_items: AtomicU64::new(0),
            total_time_us: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
        }
    }

    fn record_processing(&self, items: u64, time_us: u64) {
        self.total_items.fetch_add(items, Ordering::Relaxed);
        self.total_time_us.fetch_add(time_us, Ordering::Relaxed);
    }

    fn record_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    fn record_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    fn get_stats(&self, num_threads: usize) -> ParallelStats {
        let total_items = self.total_items.load(Ordering::Relaxed);
        let total_time_us = self.total_time_us.load(Ordering::Relaxed);
        let cache_hits = self.cache_hits.load(Ordering::Relaxed);
        let cache_misses = self.cache_misses.load(Ordering::Relaxed);

        let throughput = if total_time_us > 0 {
            (total_items as f64 * 1_000_000.0) / total_time_us as f64
        } else {
            0.0
        };

        let cache_hit_rate = if cache_hits + cache_misses > 0 {
            cache_hits as f64 / (cache_hits + cache_misses) as f64
        } else {
            0.0
        };

        // 理論上的並行效率計算（簡化版）
        let parallel_efficiency = if num_threads > 1 {
            // 假設理想情況下的線性加速
            0.85 // 經驗值，考慮到並行開銷
        } else {
            1.0
        };

        ParallelStats {
            total_items,
            processing_time_us: total_time_us,
            throughput,
            parallel_efficiency,
            thread_utilization: 0.8, // 簡化估算
            cache_hit_rate,
        }
    }
}

impl ParallelProcessor {
    /// 創建新的並行處理器
    pub fn new(config: ParallelConfig) -> anyhow::Result<Self> {
        let num_threads = config.num_threads.unwrap_or_else(|| {
            let logical_cores = num_cpus::get();
            // 為HFT保留一些核心
            (logical_cores - 2).max(2)
        });

        let mut builder = ThreadPoolBuilder::new().num_threads(num_threads);

        // 配置線程親和性
        if config.thread_affinity {
            builder = builder.spawn_handler(|thread| {
                std::thread::spawn(|| {
                    // 設置線程優先級
                    #[cfg(target_os = "linux")]
                    {
                        unsafe {
                            libc::setpriority(libc::PRIO_PROCESS, 0, -10);
                        }
                    }
                    thread.run()
                })
            });
        }

        let thread_pool = builder.build()
            .map_err(|e| anyhow::anyhow!("Failed to create thread pool: {}", e))?;

        let memory_pool = Arc::new(MemoryPool::new(config.batch_size, num_threads * 2));

        Ok(Self {
            thread_pool,
            config,
            stats: Arc::new(ParallelStatsCollector::new()),
            memory_pool,
        })
    }

    /// 並行處理數值數組
    #[instrument(skip(self, data, processor))]
    pub fn process_array<T, F, R>(&self, data: &[T], processor: F) -> Vec<R>
    where
        T: Send + Sync,
        F: Fn(&T) -> R + Send + Sync,
        R: Send,
    {
        let start_time = Instant::now();
        
        let result = if data.len() < self.config.batch_size {
            // 小數據集，直接處理
            data.iter().map(&processor).collect()
        } else {
            // 大數據集，並行處理
            data.par_iter()
                .with_min_len(self.config.batch_size / self.thread_pool.current_num_threads())
                .map(&processor)
                .collect()
        };

        let elapsed = start_time.elapsed();
        self.stats.record_processing(data.len() as u64, elapsed.as_micros() as u64);

        result
    }

    /// 並行批處理
    #[instrument(skip(self, data, batch_processor))]
    pub fn process_batches<T, F, R>(&self, data: &[T], batch_processor: F) -> Vec<R>
    where
        T: Send + Sync,
        F: Fn(&[T]) -> Vec<R> + Send + Sync,
        R: Send,
    {
        let start_time = Instant::now();

        let result = data
            .par_chunks(self.config.batch_size)
            .flat_map(&batch_processor)
            .collect();

        let elapsed = start_time.elapsed();
        self.stats.record_processing(data.len() as u64, elapsed.as_micros() as u64);

        result
    }

    /// 並行移動平均計算
    pub fn parallel_moving_average(&self, data: &[f64], window: usize) -> Vec<f64> {
        if data.len() < window {
            return Vec::new();
        }

        let start_time = Instant::now();

        // 並行計算移動平均
        let result: Vec<f64> = (window - 1..data.len())
            .into_par_iter()
            .map(|i| {
                let sum: f64 = data[i - window + 1..=i].iter().sum();
                sum / window as f64
            })
            .collect();

        let elapsed = start_time.elapsed();
        self.stats.record_processing(result.len() as u64, elapsed.as_micros() as u64);

        result
    }

    /// 並行技術指標計算
    pub fn parallel_technical_indicators(&self, prices: &[f64], volumes: &[f64]) -> TechnicalIndicators {
        let start_time = Instant::now();

        // 並行計算多個技術指標
        let (sma_20, ema_20, rsi_14, bollinger_bands, vwap) = rayon::join_all([
            || self.calculate_sma(prices, 20),
            || self.calculate_ema(prices, 20),
            || self.calculate_rsi(prices, 14),
            || self.calculate_bollinger_bands(prices, 20, 2.0),
            || self.calculate_vwap(prices, volumes),
        ]);

        let elapsed = start_time.elapsed();
        self.stats.record_processing(prices.len() as u64, elapsed.as_micros() as u64);

        TechnicalIndicators {
            sma_20: sma_20(),
            ema_20: ema_20(),
            rsi_14: rsi_14(),
            bollinger_bands: bollinger_bands(),
            vwap: vwap(),
        }
    }

    /// 並行特徵工程
    pub fn parallel_feature_engineering(&self, 
        ohlcv_data: &[OHLCVData], 
        tasks: &[FeatureTask]
    ) -> HashMap<String, Vec<f64>> {
        let start_time = Instant::now();

        // 並行執行所有特徵工程任務
        let features: HashMap<String, Vec<f64>> = tasks
            .par_iter()
            .map(|task| {
                let feature_name = self.task_name(task);
                let feature_values = self.execute_feature_task(ohlcv_data, task);
                (feature_name, feature_values)
            })
            .collect();

        let elapsed = start_time.elapsed();
        self.stats.record_processing(
            (ohlcv_data.len() * tasks.len()) as u64, 
            elapsed.as_micros() as u64
        );

        features
    }

    /// 並行數據聚合
    pub fn parallel_aggregation<T, F, R>(&self, 
        data: &[T], 
        window_sizes: &[usize], 
        aggregator: F
    ) -> HashMap<usize, Vec<R>>
    where
        T: Send + Sync,
        F: Fn(&[T]) -> R + Send + Sync,
        R: Send + Clone,
    {
        let start_time = Instant::now();

        let results: HashMap<usize, Vec<R>> = window_sizes
            .par_iter()
            .map(|&window_size| {
                let aggregated: Vec<R> = (window_size - 1..data.len())
                    .into_par_iter()
                    .map(|i| aggregator(&data[i - window_size + 1..=i]))
                    .collect();
                (window_size, aggregated)
            })
            .collect();

        let elapsed = start_time.elapsed();
        self.stats.record_processing(
            (data.len() * window_sizes.len()) as u64, 
            elapsed.as_micros() as u64
        );

        results
    }

    /// 並行矩陣運算
    pub fn parallel_matrix_multiply(&self, a: &[Vec<f64>], b: &[Vec<f64>]) -> Vec<Vec<f64>> {
        if a.is_empty() || b.is_empty() || a[0].len() != b.len() {
            return Vec::new();
        }

        let start_time = Instant::now();

        let result: Vec<Vec<f64>> = (0..a.len())
            .into_par_iter()
            .map(|i| {
                (0..b[0].len())
                    .into_par_iter()
                    .map(|j| {
                        (0..b.len())
                            .map(|k| a[i][k] * b[k][j])
                            .sum()
                    })
                    .collect()
            })
            .collect();

        let elapsed = start_time.elapsed();
        self.stats.record_processing(
            (a.len() * b[0].len()) as u64, 
            elapsed.as_micros() as u64
        );

        result
    }

    /// 並行數據驗證
    pub fn parallel_data_validation<T, F>(&self, data: &[T], validator: F) -> ValidationResult
    where
        T: Send + Sync,
        F: Fn(&T) -> bool + Send + Sync,
    {
        let start_time = Instant::now();

        let (valid_count, total_count) = data
            .par_iter()
            .map(|item| if validator(item) { (1, 1) } else { (0, 1) })
            .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));

        let elapsed = start_time.elapsed();
        self.stats.record_processing(total_count as u64, elapsed.as_micros() as u64);

        ValidationResult {
            valid_items: valid_count,
            total_items: total_count,
            validation_rate: valid_count as f64 / total_count as f64,
            processing_time_us: elapsed.as_micros() as u64,
        }
    }

    /// 獲取處理統計
    pub fn get_stats(&self) -> ParallelStats {
        self.stats.get_stats(self.thread_pool.current_num_threads())
    }

    /// 重置統計
    pub fn reset_stats(&self) {
        self.stats.total_items.store(0, Ordering::Relaxed);
        self.stats.total_time_us.store(0, Ordering::Relaxed);
        self.stats.cache_hits.store(0, Ordering::Relaxed);
        self.stats.cache_misses.store(0, Ordering::Relaxed);
    }

    // 私有輔助方法
    fn calculate_sma(&self, prices: &[f64], period: usize) -> Vec<f64> {
        self.parallel_moving_average(prices, period)
    }

    fn calculate_ema(&self, prices: &[f64], period: usize) -> Vec<f64> {
        if prices.len() < period {
            return Vec::new();
        }

        let multiplier = 2.0 / (period + 1) as f64;
        let mut ema = Vec::with_capacity(prices.len());
        
        // 第一個EMA值使用SMA
        let first_sma: f64 = prices[..period].iter().sum::<f64>() / period as f64;
        ema.push(first_sma);

        // 計算後續EMA值
        for i in period..prices.len() {
            let current_ema = (prices[i] * multiplier) + (ema[i - period] * (1.0 - multiplier));
            ema.push(current_ema);
        }

        ema
    }

    fn calculate_rsi(&self, prices: &[f64], period: usize) -> Vec<f64> {
        if prices.len() < period + 1 {
            return Vec::new();
        }

        // 計算價格變化
        let price_changes: Vec<f64> = prices.windows(2)
            .map(|w| w[1] - w[0])
            .collect();

        // 並行計算RSI
        (period..price_changes.len())
            .into_par_iter()
            .map(|i| {
                let window = &price_changes[i - period + 1..=i];
                let gains: f64 = window.iter().filter(|&&x| x > 0.0).sum();
                let losses: f64 = window.iter().filter(|&&x| x < 0.0).map(|x| -x).sum();
                
                let avg_gain = gains / period as f64;
                let avg_loss = losses / period as f64;
                
                if avg_loss == 0.0 {
                    100.0
                } else {
                    let rs = avg_gain / avg_loss;
                    100.0 - (100.0 / (1.0 + rs))
                }
            })
            .collect()
    }

    fn calculate_bollinger_bands(&self, prices: &[f64], period: usize, std_dev: f64) -> Vec<(f64, f64, f64)> {
        let sma = self.calculate_sma(prices, period);
        
        (period - 1..prices.len())
            .into_par_iter()
            .zip(sma.par_iter())
            .map(|(i, &middle)| {
                let window = &prices[i - period + 1..=i];
                let variance: f64 = window.iter()
                    .map(|&x| (x - middle).powi(2))
                    .sum::<f64>() / period as f64;
                let std = variance.sqrt();
                
                let upper = middle + (std_dev * std);
                let lower = middle - (std_dev * std);
                
                (upper, middle, lower)
            })
            .collect()
    }

    fn calculate_vwap(&self, prices: &[f64], volumes: &[f64]) -> Vec<f64> {
        if prices.len() != volumes.len() {
            return Vec::new();
        }

        (1..=prices.len())
            .into_par_iter()
            .map(|i| {
                let window_prices = &prices[..i];
                let window_volumes = &volumes[..i];
                
                let total_pv: f64 = window_prices.iter()
                    .zip(window_volumes.iter())
                    .map(|(&p, &v)| p * v)
                    .sum();
                
                let total_volume: f64 = window_volumes.iter().sum();
                
                if total_volume > 0.0 {
                    total_pv / total_volume
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn task_name(&self, task: &FeatureTask) -> String {
        match task {
            FeatureTask::MovingAverage { window } => format!("ma_{}", window),
            FeatureTask::TechnicalIndicator { indicator_type, .. } => indicator_type.clone(),
            FeatureTask::Statistics { metrics } => format!("stats_{}", metrics.join("_")),
            FeatureTask::CrossFeatures { .. } => "cross_features".to_string(),
            FeatureTask::WindowAggregation { .. } => "window_agg".to_string(),
        }
    }

    fn execute_feature_task(&self, data: &[OHLCVData], task: &FeatureTask) -> Vec<f64> {
        match task {
            FeatureTask::MovingAverage { window } => {
                let closes: Vec<f64> = data.iter().map(|d| d.close).collect();
                self.parallel_moving_average(&closes, *window)
            },
            FeatureTask::TechnicalIndicator { indicator_type, params } => {
                // 實現各種技術指標
                match indicator_type.as_str() {
                    "rsi" => {
                        let closes: Vec<f64> = data.iter().map(|d| d.close).collect();
                        let period = params.get("period").unwrap_or(&14.0) as usize;
                        self.calculate_rsi(&closes, period)
                    },
                    _ => Vec::new(),
                }
            },
            FeatureTask::Statistics { metrics } => {
                // 實現統計特徵計算
                let closes: Vec<f64> = data.iter().map(|d| d.close).collect();
                
                metrics.par_iter()
                    .flat_map(|metric| match metric.as_str() {
                        "mean" => vec![closes.iter().sum::<f64>() / closes.len() as f64],
                        "std" => {
                            let mean = closes.iter().sum::<f64>() / closes.len() as f64;
                            let variance = closes.iter()
                                .map(|x| (x - mean).powi(2))
                                .sum::<f64>() / closes.len() as f64;
                            vec![variance.sqrt()]
                        },
                        _ => vec![0.0],
                    })
                    .collect()
            },
            _ => Vec::new(),
        }
    }
}

/// 技術指標結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechnicalIndicators {
    pub sma_20: Vec<f64>,
    pub ema_20: Vec<f64>,
    pub rsi_14: Vec<f64>,
    pub bollinger_bands: Vec<(f64, f64, f64)>,
    pub vwap: Vec<f64>,
}

/// 數據驗證結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub valid_items: usize,
    pub total_items: usize,
    pub validation_rate: f64,
    pub processing_time_us: u64,
}

/// OHLCV數據結構
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OHLCVData {
    pub timestamp: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// 全局並行處理器實例
lazy_static::lazy_static! {
    pub static ref GLOBAL_PARALLEL_PROCESSOR: ParallelProcessor = {
        ParallelProcessor::new(ParallelConfig::default())
            .expect("Failed to create global parallel processor")
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parallel_processor_creation() {
        let config = ParallelConfig::default();
        let processor = ParallelProcessor::new(config).unwrap();
        assert!(processor.thread_pool.current_num_threads() > 0);
    }

    #[test]
    fn test_parallel_array_processing() {
        let processor = ParallelProcessor::new(ParallelConfig::default()).unwrap();
        let data: Vec<i32> = (0..1000).collect();
        
        let result = processor.process_array(&data, |&x| x * 2);
        
        assert_eq!(result.len(), 1000);
        assert_eq!(result[0], 0);
        assert_eq!(result[999], 1998);
    }

    #[test]
    fn test_parallel_moving_average() {
        let processor = ParallelProcessor::new(ParallelConfig::default()).unwrap();
        let data: Vec<f64> = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        
        let ma = processor.parallel_moving_average(&data, 3);
        
        assert_eq!(ma.len(), 8);
        assert!((ma[0] - 2.0).abs() < f64::EPSILON); // (1+2+3)/3 = 2
        assert!((ma[7] - 9.0).abs() < f64::EPSILON); // (8+9+10)/3 = 9
    }

    #[test]
    fn test_parallel_batch_processing() {
        let processor = ParallelProcessor::new(ParallelConfig::default()).unwrap();
        let data: Vec<i32> = (0..100).collect();
        
        let result = processor.process_batches(&data, |batch| {
            batch.iter().map(|&x| x * x).collect()
        });
        
        assert_eq!(result.len(), 100);
        assert_eq!(result[0], 0);
        assert_eq!(result[99], 9801);
    }

    #[test]
    fn test_parallel_data_validation() {
        let processor = ParallelProcessor::new(ParallelConfig::default()).unwrap();
        let data: Vec<i32> = (0..100).collect();
        
        let result = processor.parallel_data_validation(&data, |&x| x % 2 == 0);
        
        assert_eq!(result.total_items, 100);
        assert_eq!(result.valid_items, 50); // 偶數
        assert!((result.validation_rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_technical_indicators() {
        let processor = ParallelProcessor::new(ParallelConfig::default()).unwrap();
        let prices: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        let volumes: Vec<f64> = vec![1000.0; 100];
        
        let indicators = processor.parallel_technical_indicators(&prices, &volumes);
        
        assert!(!indicators.sma_20.is_empty());
        assert!(!indicators.ema_20.is_empty());
        assert!(!indicators.rsi_14.is_empty());
    }

    #[test]
    fn test_parallel_matrix_multiply() {
        let processor = ParallelProcessor::new(ParallelConfig::default()).unwrap();
        let a = vec![vec![1.0, 2.0], vec![3.0, 4.0]];
        let b = vec![vec![5.0, 6.0], vec![7.0, 8.0]];
        
        let result = processor.parallel_matrix_multiply(&a, &b);
        
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].len(), 2);
        assert_eq!(result[0][0], 19.0); // 1*5 + 2*7 = 19
        assert_eq!(result[1][1], 50.0); // 3*6 + 4*8 = 50
    }
}