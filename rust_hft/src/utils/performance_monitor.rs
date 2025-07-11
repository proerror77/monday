/*!
 * 性能监控系统 - 实时延迟追踪和指标收集
 * 
 * 功能：
 * - 延迟分布统计 (P50, P95, P99)
 * - 吞吐量监控
 * - 内存使用统计
 * - CPU使用率监控
 * - 系统健康检查
 */

use crate::core::types::*;
use anyhow::Result;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tracing::{info, warn, error};
use serde::{Serialize, Deserialize};

/// 性能监控器主结构
#[derive(Debug)]
pub struct PerformanceMonitor {
    /// 延迟统计
    latency_tracker: Arc<LatencyTracker>,
    
    /// 吞吐量统计
    throughput_tracker: Arc<ThroughputTracker>,
    
    /// 内存监控
    memory_tracker: Arc<MemoryTracker>,
    
    /// 系统监控
    system_monitor: Arc<SystemMonitor>,
    
    /// 监控配置
    config: MonitorConfig,
    
    /// 是否正在运行
    is_running: Arc<AtomicBool>,
}

/// 监控配置
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// 采样间隔 (毫秒)
    pub sample_interval_ms: u64,
    
    /// 历史数据保留时间 (秒)
    pub history_retention_seconds: u64,
    
    /// 延迟阈值告警 (微秒)
    pub latency_warning_threshold_us: u64,
    pub latency_critical_threshold_us: u64,
    
    /// 吞吐量阈值告警 (msg/s)
    pub throughput_warning_threshold: f64,
    
    /// 内存使用阈值告警 (MB)
    pub memory_warning_threshold_mb: u64,
    pub memory_critical_threshold_mb: u64,
    
    /// 是否启用详细日志
    pub enable_detailed_logging: bool,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            sample_interval_ms: 1000,           // 1秒采样间隔
            history_retention_seconds: 3600,    // 1小时历史数据
            latency_warning_threshold_us: 10000, // 10ms 警告
            latency_critical_threshold_us: 50000, // 50ms 严重
            throughput_warning_threshold: 100.0,  // 100 msg/s 警告
            memory_warning_threshold_mb: 500,     // 500MB 警告
            memory_critical_threshold_mb: 1000,   // 1GB 严重
            enable_detailed_logging: false,
        }
    }
}

/// 延迟追踪器
#[derive(Debug)]
pub struct LatencyTracker {
    /// 延迟样本 (微秒)
    samples: Mutex<VecDeque<u64>>,
    
    /// 总样本数
    total_samples: AtomicU64,
    
    /// 累计延迟
    total_latency_us: AtomicU64,
    
    /// 最小延迟
    min_latency_us: AtomicU64,
    
    /// 最大延迟
    max_latency_us: AtomicU64,
    
    /// 配置
    config: MonitorConfig,
}

impl LatencyTracker {
    pub fn new(config: MonitorConfig) -> Self {
        Self {
            samples: Mutex::new(VecDeque::new()),
            total_samples: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
            min_latency_us: AtomicU64::new(u64::MAX),
            max_latency_us: AtomicU64::new(0),
            config,
        }
    }
    
    /// 记录延迟样本
    pub fn record_latency(&self, latency_us: u64) {
        // 更新原子统计
        self.total_samples.fetch_add(1, Ordering::Relaxed);
        self.total_latency_us.fetch_add(latency_us, Ordering::Relaxed);
        
        // 更新最小延迟
        let mut min_latency = self.min_latency_us.load(Ordering::Relaxed);
        while latency_us < min_latency {
            match self.min_latency_us.compare_exchange_weak(
                min_latency,
                latency_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => min_latency = current,
            }
        }
        
        // 更新最大延迟
        let mut max_latency = self.max_latency_us.load(Ordering::Relaxed);
        while latency_us > max_latency {
            match self.max_latency_us.compare_exchange_weak(
                max_latency,
                latency_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => max_latency = current,
            }
        }
        
        // 添加到样本队列
        if let Ok(mut samples) = self.samples.lock() {
            samples.push_back(latency_us);
            
            // 清理过期样本
            let max_samples = (self.config.history_retention_seconds * 1000 / self.config.sample_interval_ms) as usize;
            while samples.len() > max_samples {
                samples.pop_front();
            }
        }
        
        // 检查告警阈值
        if latency_us > self.config.latency_critical_threshold_us {
            error!("⚠️ CRITICAL: Latency {}μs exceeds critical threshold ({}μs)", 
                   latency_us, self.config.latency_critical_threshold_us);
        } else if latency_us > self.config.latency_warning_threshold_us {
            warn!("⚠️ WARNING: Latency {}μs exceeds warning threshold ({}μs)", 
                  latency_us, self.config.latency_warning_threshold_us);
        }
    }
    
    /// 获取延迟统计
    pub fn get_stats(&self) -> LatencyStats {
        let total_samples = self.total_samples.load(Ordering::Relaxed);
        let total_latency = self.total_latency_us.load(Ordering::Relaxed);
        let min_latency = self.min_latency_us.load(Ordering::Relaxed);
        let max_latency = self.max_latency_us.load(Ordering::Relaxed);
        
        let avg_latency = if total_samples > 0 {
            total_latency / total_samples
        } else {
            0
        };
        
        // 计算百分位数
        let (p50, p95, p99) = self.calculate_percentiles();
        
        LatencyStats {
            total_samples,
            avg_latency_us: avg_latency,
            min_latency_us: if min_latency == u64::MAX { 0 } else { min_latency },
            max_latency_us: max_latency,
            p50_latency_us: p50,
            p95_latency_us: p95,
            p99_latency_us: p99,
        }
    }
    
    /// 计算延迟百分位数
    fn calculate_percentiles(&self) -> (u64, u64, u64) {
        if let Ok(samples) = self.samples.lock() {
            if samples.is_empty() {
                return (0, 0, 0);
            }
            
            let mut sorted_samples: Vec<u64> = samples.iter().cloned().collect();
            sorted_samples.sort_unstable();
            
            let len = sorted_samples.len();
            let p50_idx = (len * 50) / 100;
            let p95_idx = (len * 95) / 100;
            let p99_idx = (len * 99) / 100;
            
            let p50 = sorted_samples.get(p50_idx).cloned().unwrap_or(0);
            let p95 = sorted_samples.get(p95_idx).cloned().unwrap_or(0);
            let p99 = sorted_samples.get(p99_idx).cloned().unwrap_or(0);
            
            (p50, p95, p99)
        } else {
            (0, 0, 0)
        }
    }
}

/// 吞吐量追踪器
#[derive(Debug)]
pub struct ThroughputTracker {
    /// 消息计数器
    message_count: AtomicU64,
    
    /// 字节计数器
    byte_count: AtomicU64,
    
    /// 开始时间
    start_time: Instant,
    
    /// 最后统计时间
    last_stat_time: Mutex<Instant>,
    
    /// 历史吞吐量
    throughput_history: Mutex<VecDeque<ThroughputSample>>,
    
    /// 配置
    config: MonitorConfig,
}

#[derive(Debug, Clone)]
struct ThroughputSample {
    timestamp: Instant,
    messages_per_second: f64,
    bytes_per_second: f64,
}

impl ThroughputTracker {
    pub fn new(config: MonitorConfig) -> Self {
        Self {
            message_count: AtomicU64::new(0),
            byte_count: AtomicU64::new(0),
            start_time: Instant::now(),
            last_stat_time: Mutex::new(Instant::now()),
            throughput_history: Mutex::new(VecDeque::new()),
            config,
        }
    }
    
    /// 记录消息处理
    pub fn record_message(&self, byte_size: usize) {
        self.message_count.fetch_add(1, Ordering::Relaxed);
        self.byte_count.fetch_add(byte_size as u64, Ordering::Relaxed);
    }
    
    /// 获取当前吞吐量统计
    pub fn get_stats(&self) -> ThroughputStats {
        let message_count = self.message_count.load(Ordering::Relaxed);
        let byte_count = self.byte_count.load(Ordering::Relaxed);
        let elapsed = self.start_time.elapsed().as_secs_f64();
        
        let messages_per_second = if elapsed > 0.0 { message_count as f64 / elapsed } else { 0.0 };
        let bytes_per_second = if elapsed > 0.0 { byte_count as f64 / elapsed } else { 0.0 };
        
        ThroughputStats {
            total_messages: message_count,
            total_bytes: byte_count,
            messages_per_second,
            bytes_per_second,
            elapsed_seconds: elapsed,
        }
    }
    
    /// 更新吞吐量历史
    pub fn update_history(&self) {
        let stats = self.get_stats();
        
        if let Ok(mut history) = self.throughput_history.lock() {
            let sample = ThroughputSample {
                timestamp: Instant::now(),
                messages_per_second: stats.messages_per_second,
                bytes_per_second: stats.bytes_per_second,
            };
            
            history.push_back(sample);
            
            // 清理过期样本
            let max_samples = (self.config.history_retention_seconds * 1000 / self.config.sample_interval_ms) as usize;
            while history.len() > max_samples {
                history.pop_front();
            }
        }
        
        // 检查告警阈值
        if stats.messages_per_second < self.config.throughput_warning_threshold {
            warn!("⚠️ WARNING: Low throughput {:.2} msg/s (threshold: {:.2} msg/s)", 
                  stats.messages_per_second, self.config.throughput_warning_threshold);
        }
    }
}

/// 内存监控器
#[derive(Debug)]
pub struct MemoryTracker {
    /// 内存使用历史
    memory_history: Mutex<VecDeque<MemorySample>>,
    
    /// 配置
    config: MonitorConfig,
}

#[derive(Debug, Clone)]
struct MemorySample {
    timestamp: Instant,
    heap_usage_mb: u64,
    resident_set_size_mb: u64,
    virtual_memory_mb: u64,
}

impl MemoryTracker {
    pub fn new(config: MonitorConfig) -> Self {
        Self {
            memory_history: Mutex::new(VecDeque::new()),
            config,
        }
    }
    
    /// 更新内存统计
    pub fn update_stats(&self) -> Result<()> {
        let memory_info = self.get_current_memory_info()?;
        
        if let Ok(mut history) = self.memory_history.lock() {
            let sample = MemorySample {
                timestamp: Instant::now(),
                heap_usage_mb: memory_info.heap_usage_mb,
                resident_set_size_mb: memory_info.resident_set_size_mb,
                virtual_memory_mb: memory_info.virtual_memory_mb,
            };
            
            history.push_back(sample);
            
            // 清理过期样本
            let max_samples = (self.config.history_retention_seconds * 1000 / self.config.sample_interval_ms) as usize;
            while history.len() > max_samples {
                history.pop_front();
            }
        }
        
        // 检查告警阈值
        if memory_info.heap_usage_mb > self.config.memory_critical_threshold_mb {
            error!("⚠️ CRITICAL: Memory usage {}MB exceeds critical threshold ({}MB)", 
                   memory_info.heap_usage_mb, self.config.memory_critical_threshold_mb);
        } else if memory_info.heap_usage_mb > self.config.memory_warning_threshold_mb {
            warn!("⚠️ WARNING: Memory usage {}MB exceeds warning threshold ({}MB)", 
                  memory_info.heap_usage_mb, self.config.memory_warning_threshold_mb);
        }
        
        Ok(())
    }
    
    /// 获取当前内存信息
    fn get_current_memory_info(&self) -> Result<MemoryInfo> {
        #[cfg(target_os = "linux")]
        {
            self.get_linux_memory_info()
        }
        
        #[cfg(target_os = "macos")]
        {
            self.get_macos_memory_info()
        }
        
        #[cfg(target_os = "windows")]
        {
            self.get_windows_memory_info()
        }
        
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            // Fallback for unsupported platforms
            Ok(MemoryInfo {
                heap_usage_mb: 0,
                resident_set_size_mb: 0,
                virtual_memory_mb: 0,
            })
        }
    }
    
    #[cfg(target_os = "linux")]
    fn get_linux_memory_info(&self) -> Result<MemoryInfo> {
        let statm_content = std::fs::read_to_string("/proc/self/statm")?;
        let parts: Vec<&str> = statm_content.split_whitespace().collect();
        
        if parts.len() >= 2 {
            let page_size = 4096; // Linux page size is typically 4KB
            let virtual_pages: u64 = parts[0].parse().unwrap_or(0);
            let resident_pages: u64 = parts[1].parse().unwrap_or(0);
            
            Ok(MemoryInfo {
                heap_usage_mb: (resident_pages * page_size) / 1024 / 1024,
                resident_set_size_mb: (resident_pages * page_size) / 1024 / 1024,
                virtual_memory_mb: (virtual_pages * page_size) / 1024 / 1024,
            })
        } else {
            Err(anyhow::anyhow!("Failed to parse /proc/self/statm"))
        }
    }
    
    #[cfg(target_os = "macos")]
    fn get_macos_memory_info(&self) -> Result<MemoryInfo> {
        use std::process::Command;
        
        let output = Command::new("ps")
            .args(&["-o", "rss,vsz", "-p"])
            .arg(std::process::id().to_string())
            .output()?;
        
        let output_str = String::from_utf8(output.stdout)?;
        let lines: Vec<&str> = output_str.trim().split('\n').collect();
        
        if lines.len() >= 2 {
            let parts: Vec<&str> = lines[1].split_whitespace().collect();
            if parts.len() >= 2 {
                let rss_kb: u64 = parts[0].parse().unwrap_or(0);
                let vsz_kb: u64 = parts[1].parse().unwrap_or(0);
                
                return Ok(MemoryInfo {
                    heap_usage_mb: rss_kb / 1024,
                    resident_set_size_mb: rss_kb / 1024,
                    virtual_memory_mb: vsz_kb / 1024,
                });
            }
        }
        
        Err(anyhow::anyhow!("Failed to get macOS memory info"))
    }
    
    #[cfg(target_os = "windows")]
    fn get_windows_memory_info(&self) -> Result<MemoryInfo> {
        // Windows memory info implementation would go here
        // For now, return dummy values
        Ok(MemoryInfo {
            heap_usage_mb: 100,
            resident_set_size_mb: 100,
            virtual_memory_mb: 200,
        })
    }
    
    /// 获取内存统计
    pub fn get_stats(&self) -> Result<MemoryStats> {
        let current_info = self.get_current_memory_info()?;
        
        // 计算历史平均值
        let (avg_heap, max_heap) = if let Ok(history) = self.memory_history.lock() {
            if history.is_empty() {
                (current_info.heap_usage_mb, current_info.heap_usage_mb)
            } else {
                let total_heap: u64 = history.iter().map(|s| s.heap_usage_mb).sum();
                let avg_heap = total_heap / history.len() as u64;
                let max_heap = history.iter().map(|s| s.heap_usage_mb).max().unwrap_or(0);
                (avg_heap, max_heap)
            }
        } else {
            (current_info.heap_usage_mb, current_info.heap_usage_mb)
        };
        
        Ok(MemoryStats {
            current_heap_mb: current_info.heap_usage_mb,
            current_rss_mb: current_info.resident_set_size_mb,
            current_virtual_mb: current_info.virtual_memory_mb,
            avg_heap_mb: avg_heap,
            max_heap_mb: max_heap,
        })
    }
}

/// 系统监控器
#[derive(Debug)]
pub struct SystemMonitor {
    /// CPU使用率历史
    cpu_history: Mutex<VecDeque<f64>>,
    
    /// 系统负载历史
    load_history: Mutex<VecDeque<f64>>,
    
    /// 配置
    config: MonitorConfig,
}

impl SystemMonitor {
    pub fn new(config: MonitorConfig) -> Self {
        Self {
            cpu_history: Mutex::new(VecDeque::new()),
            load_history: Mutex::new(VecDeque::new()),
            config,
        }
    }
    
    /// 更新系统统计
    pub fn update_stats(&self) -> Result<()> {
        let cpu_usage = self.get_cpu_usage()?;
        let load_average = self.get_load_average()?;
        
        if let Ok(mut cpu_history) = self.cpu_history.lock() {
            cpu_history.push_back(cpu_usage);
            let max_samples = (self.config.history_retention_seconds * 1000 / self.config.sample_interval_ms) as usize;
            while cpu_history.len() > max_samples {
                cpu_history.pop_front();
            }
        }
        
        if let Ok(mut load_history) = self.load_history.lock() {
            load_history.push_back(load_average);
            let max_samples = (self.config.history_retention_seconds * 1000 / self.config.sample_interval_ms) as usize;
            while load_history.len() > max_samples {
                load_history.pop_front();
            }
        }
        
        Ok(())
    }
    
    /// 获取CPU使用率
    fn get_cpu_usage(&self) -> Result<f64> {
        // 简化的CPU使用率获取
        // 实际实现会更复杂，需要读取 /proc/stat 或使用系统API
        Ok(25.0) // 返回模拟值
    }
    
    /// 获取系统负载平均值
    fn get_load_average(&self) -> Result<f64> {
        #[cfg(target_os = "linux")]
        {
            let loadavg_content = std::fs::read_to_string("/proc/loadavg")?;
            let parts: Vec<&str> = loadavg_content.split_whitespace().collect();
            if !parts.is_empty() {
                return Ok(parts[0].parse().unwrap_or(0.0));
            }
        }
        
        // Fallback
        Ok(1.0)
    }
    
    /// 获取系统统计
    pub fn get_stats(&self) -> SystemStats {
        let cpu_usage = self.get_cpu_usage().unwrap_or(0.0);
        let load_average = self.get_load_average().unwrap_or(0.0);
        
        SystemStats {
            cpu_usage_percent: cpu_usage,
            load_average_1min: load_average,
            uptime_seconds: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}

// 数据结构定义
use std::sync::atomic::AtomicBool;

#[derive(Debug, Serialize, Deserialize)]
pub struct LatencyStats {
    pub total_samples: u64,
    pub avg_latency_us: u64,
    pub min_latency_us: u64,
    pub max_latency_us: u64,
    pub p50_latency_us: u64,
    pub p95_latency_us: u64,
    pub p99_latency_us: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ThroughputStats {
    pub total_messages: u64,
    pub total_bytes: u64,
    pub messages_per_second: f64,
    pub bytes_per_second: f64,
    pub elapsed_seconds: f64,
}

#[derive(Debug)]
struct MemoryInfo {
    heap_usage_mb: u64,
    resident_set_size_mb: u64,
    virtual_memory_mb: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryStats {
    pub current_heap_mb: u64,
    pub current_rss_mb: u64,
    pub current_virtual_mb: u64,
    pub avg_heap_mb: u64,
    pub max_heap_mb: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SystemStats {
    pub cpu_usage_percent: f64,
    pub load_average_1min: f64,
    pub uptime_seconds: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PerformanceReport {
    pub timestamp: u64,
    pub latency: LatencyStats,
    pub throughput: ThroughputStats,
    pub memory: MemoryStats,
    pub system: SystemStats,
}

impl PerformanceMonitor {
    /// 创建新的性能监控器
    pub fn new(config: MonitorConfig) -> Self {
        Self {
            latency_tracker: Arc::new(LatencyTracker::new(config.clone())),
            throughput_tracker: Arc::new(ThroughputTracker::new(config.clone())),
            memory_tracker: Arc::new(MemoryTracker::new(config.clone())),
            system_monitor: Arc::new(SystemMonitor::new(config.clone())),
            config,
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }
    
    /// 启动性能监控
    pub async fn start(&self) -> Result<()> {
        self.is_running.store(true, Ordering::Relaxed);
        info!("🚀 Performance Monitor started");
        
        let latency_tracker = Arc::clone(&self.latency_tracker);
        let throughput_tracker = Arc::clone(&self.throughput_tracker);
        let memory_tracker = Arc::clone(&self.memory_tracker);
        let system_monitor = Arc::clone(&self.system_monitor);
        let is_running = Arc::clone(&self.is_running);
        let config = self.config.clone();
        
        // 启动监控循环
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(config.sample_interval_ms));
            
            while is_running.load(Ordering::Relaxed) {
                interval.tick().await;
                
                // 更新各项统计
                throughput_tracker.update_history();
                
                if let Err(e) = memory_tracker.update_stats() {
                    error!("Failed to update memory stats: {}", e);
                }
                
                if let Err(e) = system_monitor.update_stats() {
                    error!("Failed to update system stats: {}", e);
                }
                
                // 定期打印统计信息
                if config.enable_detailed_logging {
                    Self::log_detailed_stats(&latency_tracker, &throughput_tracker, &memory_tracker, &system_monitor);
                }
            }
        });
        
        Ok(())
    }
    
    /// 停止性能监控
    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
        info!("🛑 Performance Monitor stopped");
    }
    
    /// 记录延迟
    pub fn record_latency(&self, latency_us: u64) {
        self.latency_tracker.record_latency(latency_us);
    }
    
    /// 记录消息处理
    pub fn record_message(&self, byte_size: usize) {
        self.throughput_tracker.record_message(byte_size);
    }
    
    /// 获取性能报告
    pub fn get_performance_report(&self) -> Result<PerformanceReport> {
        let latency = self.latency_tracker.get_stats();
        let throughput = self.throughput_tracker.get_stats();
        let memory = self.memory_tracker.get_stats()?;
        let system = self.system_monitor.get_stats();
        
        Ok(PerformanceReport {
            timestamp: now_micros(),
            latency,
            throughput,
            memory,
            system,
        })
    }
    
    /// 打印详细统计信息
    fn log_detailed_stats(
        latency_tracker: &LatencyTracker,
        throughput_tracker: &ThroughputTracker,
        memory_tracker: &MemoryTracker,
        system_monitor: &SystemMonitor,
    ) {
        let latency = latency_tracker.get_stats();
        let throughput = throughput_tracker.get_stats();
        let memory = memory_tracker.get_stats().unwrap_or(MemoryStats {
            current_heap_mb: 0,
            current_rss_mb: 0,
            current_virtual_mb: 0,
            avg_heap_mb: 0,
            max_heap_mb: 0,
        });
        let system = system_monitor.get_stats();
        
        info!("📊 Performance Stats:");
        info!("  Latency: avg={}μs, p95={}μs, p99={}μs", 
              latency.avg_latency_us, latency.p95_latency_us, latency.p99_latency_us);
        info!("  Throughput: {:.2} msg/s, {:.2} MB/s", 
              throughput.messages_per_second, throughput.bytes_per_second / 1024.0 / 1024.0);
        info!("  Memory: current={}MB, avg={}MB, max={}MB", 
              memory.current_heap_mb, memory.avg_heap_mb, memory.max_heap_mb);
        info!("  System: CPU={:.1}%, Load={:.2}", 
              system.cpu_usage_percent, system.load_average_1min);
    }
}

/// 创建默认性能监控器
pub fn create_default_monitor() -> PerformanceMonitor {
    PerformanceMonitor::new(MonitorConfig::default())
}

/// 创建高性能监控器配置
pub fn create_high_performance_config() -> MonitorConfig {
    MonitorConfig {
        sample_interval_ms: 100,             // 100ms 高频采样
        latency_warning_threshold_us: 1000,  // 1ms 警告
        latency_critical_threshold_us: 5000, // 5ms 严重
        throughput_warning_threshold: 1000.0, // 1000 msg/s
        enable_detailed_logging: true,
        ..MonitorConfig::default()
    }
}