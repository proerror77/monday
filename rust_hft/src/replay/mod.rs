/*!
 * 🔄 LOB Replay System - 历史订单簿回放系统
 * 
 * 核心功能：
 * - 从 ClickHouse 加载历史 LOB 数据
 * - 支持多倍速精确时间回放 (1x, 10x, 100x)
 * - 内存优化的流式处理，避免 OOM
 * - 为训练/回测提供高质量历史数据环境
 * 
 * 设计原则：
 * - 零拷贝数据流：避免不必要的内存分配
 * - 时间精确性：微秒级时序重现
 * - 可扩展性：支持任意时间范围和数据量
 * - 性能优化：SIMD 处理和预取优化
 */

use crate::core::{types::*, config::Config, error::*};
use crate::database::clickhouse_client::ClickHouseClient;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn, error, debug, instrument};

pub mod lob_replay_engine;
pub mod historical_data_loader;
pub mod time_travel_manager;
pub mod replay_validator;

pub use lob_replay_engine::*;
pub use historical_data_loader::*;
pub use time_travel_manager::*;
pub use replay_validator::*;

/// 回放配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayConfig {
    /// 回放速度倍数 (1.0 = 实时, 10.0 = 10倍速)
    pub speed_multiplier: f64,
    /// 开始时间戳 (微秒)
    pub start_timestamp_us: u64,
    /// 结束时间戳 (微秒)  
    pub end_timestamp_us: u64,
    /// 交易对列表
    pub symbols: Vec<String>,
    /// 最大内存使用 (MB)
    pub max_memory_mb: u64,
    /// 预加载缓冲区大小
    pub buffer_size: usize,
    /// 批次大小
    pub batch_size: usize,
    /// 是否启用数据验证
    pub enable_validation: bool,
    /// 输出格式
    pub output_format: ReplayOutputFormat,
}

/// 回放输出格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReplayOutputFormat {
    /// 原始 LOB 事件流
    RawLobEvents,
    /// 标准化的订单簿快照
    OrderBookSnapshots,
    /// 特征工程后的数据
    FeatureVectors,
    /// 自定义格式
    Custom(String),
}

impl Default for ReplayConfig {
    fn default() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
            
        Self {
            speed_multiplier: 1.0,
            start_timestamp_us: now - 3600_000_000, // 1小时前
            end_timestamp_us: now,
            symbols: vec!["BTCUSDT".to_string()],
            max_memory_mb: 2048, // 2GB
            buffer_size: 10000,
            batch_size: 1000,
            enable_validation: true,
            output_format: ReplayOutputFormat::RawLobEvents,
        }
    }
}

/// 回放状态
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayState {
    /// 未初始化
    Uninitialized,
    /// 正在加载数据
    Loading,
    /// 准备就绪
    Ready,
    /// 正在回放
    Replaying,
    /// 已暂停
    Paused,
    /// 已完成
    Completed,
    /// 发生错误
    Error(String),
}

/// 回放统计信息
#[derive(Debug, Clone, Default)]
pub struct ReplayStats {
    /// 总事件数
    pub total_events: u64,
    /// 已处理事件数
    pub processed_events: u64,
    /// 回放开始时间
    pub start_time: Option<Instant>,
    /// 回放进度 (0.0 - 1.0)
    pub progress: f64,
    /// 当前回放时间戳
    pub current_timestamp_us: u64,
    /// 实际耗时
    pub elapsed_time: Duration,
    /// 平均处理速度 (events/s)
    pub events_per_second: f64,
    /// 内存使用量 (MB)
    pub memory_usage_mb: f64,
    /// 数据加载时间
    pub load_time: Duration,
}

/// 回放事件
#[derive(Debug, Clone)]
pub enum ReplayEvent {
    /// LOB 更新事件
    LobUpdate {
        timestamp_us: u64,
        symbol: String,
        bids: Vec<(f64, f64)>, // (price, quantity)
        asks: Vec<(f64, f64)>,
        sequence: u64,
    },
    /// 交易事件
    Trade {
        timestamp_us: u64,
        symbol: String,
        price: f64,
        quantity: f64,
        side: String,
        trade_id: String,
    },
    /// Ticker 更新事件
    Ticker {
        timestamp_us: u64,
        symbol: String,
        last_price: f64,
        volume_24h: f64,
        change_24h: f64,
    },
    /// 回放控制事件
    Control(ReplayControlEvent),
}

/// 回放控制事件
#[derive(Debug, Clone)]
pub enum ReplayControlEvent {
    /// 开始回放
    Start,
    /// 暂停回放
    Pause,
    /// 恢复回放
    Resume,
    /// 停止回放
    Stop,
    /// 调整速度
    SetSpeed(f64),
    /// 跳转到时间点
    SeekTo(u64),
    /// 回放完成
    Completed,
    /// 发生错误
    Error(String),
}

/// 时间范围
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeRange {
    pub start_us: u64,
    pub end_us: u64,
}

impl TimeRange {
    pub fn new(start_us: u64, end_us: u64) -> Self {
        Self { start_us, end_us }
    }
    
    pub fn duration_us(&self) -> u64 {
        self.end_us.saturating_sub(self.start_us)
    }
    
    pub fn duration(&self) -> Duration {
        Duration::from_micros(self.duration_us())
    }
    
    pub fn contains(&self, timestamp_us: u64) -> bool {
        timestamp_us >= self.start_us && timestamp_us <= self.end_us
    }
    
    pub fn overlaps(&self, other: &TimeRange) -> bool {
        self.start_us <= other.end_us && self.end_us >= other.start_us
    }
}

/// 数据块
#[derive(Debug, Clone)]
pub struct DataChunk {
    /// 时间范围
    pub time_range: TimeRange,
    /// 事件列表
    pub events: Vec<ReplayEvent>,
    /// 数据大小 (字节)
    pub size_bytes: usize,
    /// 加载时间
    pub load_time: Duration,
}

impl DataChunk {
    pub fn new(time_range: TimeRange) -> Self {
        Self {
            time_range,
            events: Vec::new(),
            size_bytes: 0,
            load_time: Duration::ZERO,
        }
    }
    
    pub fn add_event(&mut self, event: ReplayEvent) {
        self.size_bytes += std::mem::size_of_val(&event);
        self.events.push(event);
    }
    
    pub fn event_count(&self) -> usize {
        self.events.len()
    }
    
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    
    /// 按时间戳排序事件
    pub fn sort_by_timestamp(&mut self) {
        self.events.sort_by_key(|event| {
            match event {
                ReplayEvent::LobUpdate { timestamp_us, .. } => *timestamp_us,
                ReplayEvent::Trade { timestamp_us, .. } => *timestamp_us,
                ReplayEvent::Ticker { timestamp_us, .. } => *timestamp_us,
                ReplayEvent::Control(_) => 0, // 控制事件优先级最高
            }
        });
    }
}

/// 错误类型
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("配置错误: {0}")]
    Config(String),
    
    #[error("数据加载错误: {0}")]
    DataLoad(String),
    
    #[error("时间范围错误: {0}")]
    TimeRange(String),
    
    #[error("内存不足: 需要 {required_mb}MB, 可用 {available_mb}MB")]
    OutOfMemory { required_mb: u64, available_mb: u64 },
    
    #[error("数据验证失败: {0}")]
    Validation(String),
    
    #[error("回放状态错误: {current_state:?}, 期望 {expected_state:?}")]
    InvalidState { current_state: ReplayState, expected_state: ReplayState },
    
    #[error("IO错误: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("数据库错误: {0}")]
    Database(String),
    
    #[error("并发错误: {0}")]
    Concurrency(String),
}

pub type ReplayResult<T> = Result<T, ReplayError>;

/// 工具函数
pub mod utils {
    use super::*;
    
    /// 将时间戳转换为人类可读格式
    pub fn timestamp_to_string(timestamp_us: u64) -> String {
        let timestamp_s = timestamp_us / 1_000_000;
        let dt = chrono::DateTime::from_timestamp(timestamp_s as i64, 0);
        match dt {
            Some(dt) => dt.format("%Y-%m-%d %H:%M:%S%.6f UTC").to_string(),
            None => format!("Invalid timestamp: {}", timestamp_us),
        }
    }
    
    /// 解析时间字符串为时间戳
    pub fn parse_timestamp(time_str: &str) -> ReplayResult<u64> {
        use chrono::prelude::*;
        
        let dt = chrono::DateTime::parse_from_rfc3339(time_str)
            .or_else(|_| {
                // 尝试其他格式
                let naive_dt = chrono::NaiveDateTime::parse_from_str(time_str, "%Y-%m-%d %H:%M:%S")?;
                Ok(chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(naive_dt, chrono::Utc))
            })
            .map_err(|e| ReplayError::Config(format!("时间解析失败: {}", e)))?;
            
        Ok(dt.timestamp_micros() as u64)
    }
    
    /// 计算内存使用量
    pub fn calculate_memory_usage() -> f64 {
        // 这里应该使用系统 API 获取实际内存使用
        // 暂时返回估算值
        #[cfg(target_os = "linux")]
        {
            if let Ok(content) = std::fs::read_to_string("/proc/self/status") {
                for line in content.lines() {
                    if line.starts_with("VmRSS:") {
                        if let Some(mem_str) = line.split_whitespace().nth(1) {
                            if let Ok(mem_kb) = mem_str.parse::<f64>() {
                                return mem_kb / 1024.0; // 转换为 MB
                            }
                        }
                    }
                }
            }
        }
        
        // 默认返回估算值
        256.0 // MB
    }
    
    /// 格式化字节大小
    pub fn format_bytes(bytes: usize) -> String {
        const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
        let mut size = bytes as f64;
        let mut unit_index = 0;
        
        while size >= 1024.0 && unit_index < UNITS.len() - 1 {
            size /= 1024.0;
            unit_index += 1;
        }
        
        format!("{:.2} {}", size, UNITS[unit_index])
    }
    
    /// 格式化持续时间
    pub fn format_duration(duration: Duration) -> String {
        let total_seconds = duration.as_secs();
        let hours = total_seconds / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;
        let millis = duration.subsec_millis();
        
        if hours > 0 {
            format!("{}h {}m {}s", hours, minutes, seconds)
        } else if minutes > 0 {
            format!("{}m {}s", minutes, seconds)
        } else {
            format!("{}.{:03}s", seconds, millis)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_time_range() {
        let range = TimeRange::new(1000, 2000);
        assert_eq!(range.duration_us(), 1000);
        assert!(range.contains(1500));
        assert!(!range.contains(500));
        assert!(!range.contains(2500));
    }
    
    #[test]
    fn test_time_range_overlap() {
        let range1 = TimeRange::new(1000, 2000);
        let range2 = TimeRange::new(1500, 2500);
        let range3 = TimeRange::new(3000, 4000);
        
        assert!(range1.overlaps(&range2));
        assert!(!range1.overlaps(&range3));
    }
    
    #[test]
    fn test_data_chunk() {
        let mut chunk = DataChunk::new(TimeRange::new(1000, 2000));
        assert!(chunk.is_empty());
        
        let event = ReplayEvent::LobUpdate {
            timestamp_us: 1500,
            symbol: "BTCUSDT".to_string(),
            bids: vec![(50000.0, 1.0)],
            asks: vec![(50001.0, 1.0)],
            sequence: 1,
        };
        
        chunk.add_event(event);
        assert!(!chunk.is_empty());
        assert_eq!(chunk.event_count(), 1);
    }
    
    #[test]
    fn test_utils() {
        use utils::*;
        
        // 测试字节格式化
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        
        // 测试持续时间格式化
        assert_eq!(format_duration(Duration::from_secs(65)), "1m 5s");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1.500s");
    }
}