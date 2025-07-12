/*!
 * ⏰ Time Travel Manager - 精确时间控制和回溯管理
 * 
 * 核心功能：
 * - 虚拟时间管理：支持暂停、快进、倒退
 * - 时间同步：确保多组件时间一致性
 * - 速度控制：支持任意倍速回放 (0.1x - 1000x)
 * - 时间跳转：快速定位到指定时间点
 * - 时间标记：支持书签和快照功能
 * 
 * 设计原则：
 * - 高精度：微秒级时间控制
 * - 高性能：最小化时间计算开销
 * - 线程安全：支持多线程并发访问
 * - 可预测性：确定性的时间推进
 */

use super::*;
use crate::core::{types::*, config::Config, error::*};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::collections::HashMap;
use tokio::sync::{RwLock, watch, Notify};
use tokio::time::{sleep, interval};
use tracing::{info, warn, error, debug, instrument, span, Level};
use serde::{Deserialize, Serialize};

/// 时间管理器
pub struct TimeTravelManager {
    /// 内部状态
    state: Arc<RwLock<TimeState>>,
    /// 时间通知器
    time_notifier: Arc<Notify>,
    /// 时间广播器
    time_broadcaster: watch::Sender<VirtualTime>,
    /// 时间接收器
    _time_receiver: watch::Receiver<VirtualTime>,
    /// 时间标记存储
    bookmarks: Arc<Mutex<HashMap<String, TimeBookmark>>>,
    /// 配置
    config: ReplayConfig,
}

/// 时间状态
#[derive(Debug, Clone)]
struct TimeState {
    /// 当前虚拟时间 (微秒)
    current_virtual_time_us: u64,
    /// 开始时间
    start_time_us: u64,
    /// 结束时间
    end_time_us: u64,
    /// 实际开始时间
    real_start_time: Instant,
    /// 当前速度倍数
    speed_multiplier: f64,
    /// 时间状态
    time_status: TimeStatus,
    /// 上次更新时间
    last_update: Instant,
    /// 暂停时累积的虚拟时间
    paused_virtual_time_us: u64,
    /// 暂停开始时间
    pause_start: Option<Instant>,
}

/// 时间状态枚举
#[derive(Debug, Clone, PartialEq)]
pub enum TimeStatus {
    /// 未初始化
    Uninitialized,
    /// 运行中
    Running,
    /// 已暂停
    Paused,
    /// 已完成
    Completed,
    /// 正在跳转
    Seeking,
}

/// 虚拟时间
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct VirtualTime {
    /// 虚拟时间戳 (微秒)
    pub timestamp_us: u64,
    /// 速度倍数
    pub speed_multiplier: f64,
    /// 时间状态
    pub status: VirtualTimeStatus,
    /// 实际时间戳
    pub real_timestamp: u64,
}

/// 虚拟时间状态
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum VirtualTimeStatus {
    Running,
    Paused,
    Seeking,
    Completed,
}

/// 时间书签
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeBookmark {
    /// 书签名称
    pub name: String,
    /// 时间戳 (微秒)
    pub timestamp_us: u64,
    /// 描述
    pub description: Option<String>,
    /// 创建时间
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 关联的数据快照路径
    pub snapshot_path: Option<String>,
}

/// 时间统计信息
#[derive(Debug, Clone, Default)]
pub struct TimeStats {
    /// 总运行时间
    pub total_runtime: Duration,
    /// 虚拟时间覆盖范围
    pub virtual_time_range_us: u64,
    /// 平均速度倍数
    pub average_speed: f64,
    /// 暂停次数
    pub pause_count: u32,
    /// 跳转次数
    pub seek_count: u32,
    /// 书签数量
    pub bookmark_count: u32,
}

impl TimeTravelManager {
    /// 创建新的时间管理器
    #[instrument(skip(config))]
    pub fn new(config: ReplayConfig) -> Self {
        info!("创建时间管理器");
        
        let initial_virtual_time = VirtualTime {
            timestamp_us: config.start_timestamp_us,
            speed_multiplier: config.speed_multiplier,
            status: VirtualTimeStatus::Paused,
            real_timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
        };
        
        let (time_broadcaster, time_receiver) = watch::channel(initial_virtual_time);
        
        let state = TimeState {
            current_virtual_time_us: config.start_timestamp_us,
            start_time_us: config.start_timestamp_us,
            end_time_us: config.end_timestamp_us,
            real_start_time: Instant::now(),
            speed_multiplier: config.speed_multiplier,
            time_status: TimeStatus::Uninitialized,
            last_update: Instant::now(),
            paused_virtual_time_us: 0,
            pause_start: None,
        };
        
        Self {
            state: Arc::new(RwLock::new(state)),
            time_notifier: Arc::new(Notify::new()),
            time_broadcaster,
            _time_receiver: time_receiver,
            bookmarks: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }
    
    /// 初始化时间管理器
    #[instrument(skip(self))]
    pub async fn initialize(
        &self,
        start_time_us: u64,
        speed_multiplier: f64,
    ) -> ReplayResult<()> {
        info!("初始化时间管理器: start={}, speed={}x", 
              utils::timestamp_to_string(start_time_us), speed_multiplier);
        
        let mut state = self.state.write().await;
        
        state.current_virtual_time_us = start_time_us;
        state.start_time_us = start_time_us;
        state.speed_multiplier = speed_multiplier;
        state.time_status = TimeStatus::Paused;
        state.real_start_time = Instant::now();
        state.last_update = Instant::now();
        state.paused_virtual_time_us = 0;
        state.pause_start = None;
        
        // 广播初始时间
        let virtual_time = VirtualTime {
            timestamp_us: start_time_us,
            speed_multiplier,
            status: VirtualTimeStatus::Paused,
            real_timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
        };
        
        let _ = self.time_broadcaster.send(virtual_time);
        self.time_notifier.notify_waiters();
        
        info!("时间管理器初始化完成");
        Ok(())
    }
    
    /// 开始时间推进
    #[instrument(skip(self))]
    pub async fn start(&self) -> ReplayResult<()> {
        info!("开始时间推进");
        
        let mut state = self.state.write().await;
        
        match state.time_status {
            TimeStatus::Uninitialized => {
                return Err(ReplayError::Config("时间管理器未初始化".to_string()));
            }
            TimeStatus::Running => {
                debug!("时间管理器已在运行");
                return Ok(());
            }
            _ => {}
        }
        
        // 如果从暂停状态恢复，需要调整时间基准
        if state.time_status == TimeStatus::Paused {
            if let Some(pause_start) = state.pause_start {
                let pause_duration = pause_start.elapsed();
                debug!("从暂停状态恢复，暂停时长: {:?}", pause_duration);
            }
        }
        
        state.time_status = TimeStatus::Running;
        state.last_update = Instant::now();
        state.pause_start = None;
        
        // 广播状态变化
        let virtual_time = VirtualTime {
            timestamp_us: state.current_virtual_time_us,
            speed_multiplier: state.speed_multiplier,
            status: VirtualTimeStatus::Running,
            real_timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
        };
        
        let _ = self.time_broadcaster.send(virtual_time);
        self.time_notifier.notify_waiters();
        
        info!("时间推进已开始");
        Ok(())
    }
    
    /// 暂停时间推进
    #[instrument(skip(self))]
    pub async fn pause(&self) -> ReplayResult<()> {
        info!("暂停时间推进");
        
        let mut state = self.state.write().await;
        
        if state.time_status != TimeStatus::Running {
            debug!("时间管理器未在运行状态");
            return Ok(());
        }
        
        // 更新当前虚拟时间
        self.update_virtual_time(&mut state);
        
        state.time_status = TimeStatus::Paused;
        state.pause_start = Some(Instant::now());
        
        // 广播状态变化
        let virtual_time = VirtualTime {
            timestamp_us: state.current_virtual_time_us,
            speed_multiplier: state.speed_multiplier,
            status: VirtualTimeStatus::Paused,
            real_timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
        };
        
        let _ = self.time_broadcaster.send(virtual_time);
        self.time_notifier.notify_waiters();
        
        info!("时间推进已暂停");
        Ok(())
    }
    
    /// 跳转到指定时间
    #[instrument(skip(self))]
    pub async fn seek_to(&self, timestamp_us: u64) -> ReplayResult<()> {
        info!("跳转到时间: {}", utils::timestamp_to_string(timestamp_us));
        
        let mut state = self.state.write().await;
        
        // 验证时间范围
        if timestamp_us < state.start_time_us || timestamp_us > state.end_time_us {
            return Err(ReplayError::TimeRange(format!(
                "时间戳超出范围: {} 不在 [{}, {}] 内",
                timestamp_us, state.start_time_us, state.end_time_us
            )));
        }
        
        let old_status = state.time_status.clone();
        state.time_status = TimeStatus::Seeking;
        
        // 更新虚拟时间
        state.current_virtual_time_us = timestamp_us;
        state.last_update = Instant::now();
        state.paused_virtual_time_us = 0;
        state.pause_start = None;
        
        // 广播跳转状态
        let virtual_time = VirtualTime {
            timestamp_us,
            speed_multiplier: state.speed_multiplier,
            status: VirtualTimeStatus::Seeking,
            real_timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
        };
        
        let _ = self.time_broadcaster.send(virtual_time);
        
        // 短暂延迟模拟跳转过程
        drop(state);
        sleep(Duration::from_millis(10)).await;
        
        // 恢复之前的状态
        let mut state = self.state.write().await;
        state.time_status = old_status;
        
        // 广播完成跳转
        let virtual_time = VirtualTime {
            timestamp_us,
            speed_multiplier: state.speed_multiplier,
            status: match state.time_status {
                TimeStatus::Running => VirtualTimeStatus::Running,
                TimeStatus::Paused => VirtualTimeStatus::Paused,
                _ => VirtualTimeStatus::Paused,
            },
            real_timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
        };
        
        let _ = self.time_broadcaster.send(virtual_time);
        self.time_notifier.notify_waiters();
        
        info!("跳转完成: {}", utils::timestamp_to_string(timestamp_us));
        Ok(())
    }
    
    /// 设置播放速度
    #[instrument(skip(self))]
    pub async fn set_speed(&self, speed_multiplier: f64) -> ReplayResult<()> {
        info!("设置播放速度: {}x", speed_multiplier);
        
        if speed_multiplier <= 0.0 {
            return Err(ReplayError::Config("速度倍数必须大于0".to_string()));
        }
        
        if speed_multiplier > 1000.0 {
            return Err(ReplayError::Config("速度倍数不能超过1000x".to_string()));
        }
        
        let mut state = self.state.write().await;
        
        // 如果正在运行，先更新当前虚拟时间
        if state.time_status == TimeStatus::Running {
            self.update_virtual_time(&mut state);
        }
        
        state.speed_multiplier = speed_multiplier;
        state.last_update = Instant::now();
        
        // 广播速度变化
        let virtual_time = VirtualTime {
            timestamp_us: state.current_virtual_time_us,
            speed_multiplier,
            status: match state.time_status {
                TimeStatus::Running => VirtualTimeStatus::Running,
                TimeStatus::Paused => VirtualTimeStatus::Paused,
                TimeStatus::Seeking => VirtualTimeStatus::Seeking,
                _ => VirtualTimeStatus::Paused,
            },
            real_timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
        };
        
        let _ = self.time_broadcaster.send(virtual_time);
        self.time_notifier.notify_waiters();
        
        info!("播放速度已设置为: {}x", speed_multiplier);
        Ok(())
    }
    
    /// 获取当前虚拟时间
    pub async fn current_time(&self) -> u64 {
        let mut state = self.state.write().await;
        
        if state.time_status == TimeStatus::Running {
            self.update_virtual_time(&mut state);
        }
        
        state.current_virtual_time_us
    }
    
    /// 获取时间接收器（用于订阅时间变化）
    pub fn subscribe_time_updates(&self) -> watch::Receiver<VirtualTime> {
        self.time_broadcaster.subscribe()
    }
    
    /// 等待时间变化通知
    pub async fn wait_for_time_change(&self) {
        self.time_notifier.notified().await;
    }
    
    /// 添加时间书签
    #[instrument(skip(self))]
    pub async fn add_bookmark(
        &self,
        name: String,
        timestamp_us: Option<u64>,
        description: Option<String>,
    ) -> ReplayResult<()> {
        let timestamp = match timestamp_us {
            Some(ts) => ts,
            None => self.current_time().await,
        };
        
        info!("添加时间书签: {} -> {}", name, utils::timestamp_to_string(timestamp));
        
        let bookmark = TimeBookmark {
            name: name.clone(),
            timestamp_us: timestamp,
            description,
            created_at: chrono::Utc::now(),
            snapshot_path: None,
        };
        
        let mut bookmarks = self.bookmarks.lock()
            .map_err(|e| ReplayError::Concurrency(format!("获取书签锁失败: {}", e)))?;
        
        bookmarks.insert(name, bookmark);
        
        Ok(())
    }
    
    /// 跳转到书签
    #[instrument(skip(self))]
    pub async fn seek_to_bookmark(&self, name: &str) -> ReplayResult<()> {
        let bookmarks = self.bookmarks.lock()
            .map_err(|e| ReplayError::Concurrency(format!("获取书签锁失败: {}", e)))?;
        
        let bookmark = bookmarks.get(name)
            .ok_or_else(|| ReplayError::Config(format!("未找到书签: {}", name)))?;
        
        let timestamp = bookmark.timestamp_us;
        drop(bookmarks);
        
        info!("跳转到书签: {} ({})", name, utils::timestamp_to_string(timestamp));
        self.seek_to(timestamp).await
    }
    
    /// 获取所有书签
    pub async fn get_bookmarks(&self) -> ReplayResult<Vec<TimeBookmark>> {
        let bookmarks = self.bookmarks.lock()
            .map_err(|e| ReplayError::Concurrency(format!("获取书签锁失败: {}", e)))?;
        
        Ok(bookmarks.values().cloned().collect())
    }
    
    /// 删除书签
    pub async fn remove_bookmark(&self, name: &str) -> ReplayResult<bool> {
        let mut bookmarks = self.bookmarks.lock()
            .map_err(|e| ReplayError::Concurrency(format!("获取书签锁失败: {}", e)))?;
        
        Ok(bookmarks.remove(name).is_some())
    }
    
    /// 获取时间状态
    pub async fn get_status(&self) -> TimeStatus {
        self.state.read().await.time_status.clone()
    }
    
    /// 获取时间统计信息
    pub async fn get_stats(&self) -> TimeStats {
        let state = self.state.read().await;
        let bookmarks = self.bookmarks.lock().unwrap_or_else(|_| {
            HashMap::new().into()
        });
        
        TimeStats {
            total_runtime: state.real_start_time.elapsed(),
            virtual_time_range_us: state.end_time_us - state.start_time_us,
            average_speed: state.speed_multiplier,
            pause_count: 0, // 这里需要额外的状态跟踪
            seek_count: 0,  // 这里需要额外的状态跟踪
            bookmark_count: bookmarks.len() as u32,
        }
    }
    
    /// 检查是否已完成
    pub async fn is_completed(&self) -> bool {
        let state = self.state.read().await;
        state.current_virtual_time_us >= state.end_time_us
    }
    
    /// 获取进度百分比
    pub async fn get_progress(&self) -> f64 {
        let state = self.state.read().await;
        let total_duration = state.end_time_us - state.start_time_us;
        let elapsed_duration = state.current_virtual_time_us - state.start_time_us;
        
        if total_duration > 0 {
            elapsed_duration as f64 / total_duration as f64
        } else {
            1.0
        }
    }
    
    /// 更新虚拟时间（内部方法）
    fn update_virtual_time(&self, state: &mut TimeState) {
        if state.time_status != TimeStatus::Running {
            return;
        }
        
        let now = Instant::now();
        let real_elapsed = now.duration_since(state.last_update);
        let virtual_elapsed_us = (real_elapsed.as_micros() as f64 * state.speed_multiplier) as u64;
        
        state.current_virtual_time_us += virtual_elapsed_us;
        state.last_update = now;
        
        // 检查是否已到达结束时间
        if state.current_virtual_time_us >= state.end_time_us {
            state.current_virtual_time_us = state.end_time_us;
            state.time_status = TimeStatus::Completed;
        }
    }
    
    /// 启动时间更新循环（供外部使用）
    pub async fn start_time_update_loop(&self) -> ReplayResult<()> {
        info!("启动时间更新循环");
        
        let mut interval = interval(Duration::from_millis(10)); // 100Hz 更新频率
        
        loop {
            interval.tick().await;
            
            let should_continue = {
                let mut state = self.state.write().await;
                
                if state.time_status == TimeStatus::Running {
                    self.update_virtual_time(&mut state);
                    
                    // 广播时间更新
                    let virtual_time = VirtualTime {
                        timestamp_us: state.current_virtual_time_us,
                        speed_multiplier: state.speed_multiplier,
                        status: if state.time_status == TimeStatus::Completed {
                            VirtualTimeStatus::Completed
                        } else {
                            VirtualTimeStatus::Running
                        },
                        real_timestamp: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_micros() as u64,
                    };
                    
                    let _ = self.time_broadcaster.send(virtual_time);
                    self.time_notifier.notify_waiters();
                }
                
                state.time_status != TimeStatus::Completed
            };
            
            if !should_continue {
                info!("时间更新循环结束");
                break;
            }
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};
    
    #[tokio::test]
    async fn test_time_manager_initialization() {
        let config = ReplayConfig::default();
        let manager = TimeTravelManager::new(config.clone());
        
        assert_eq!(manager.get_status().await, TimeStatus::Uninitialized);
        
        let result = manager.initialize(config.start_timestamp_us, 1.0).await;
        assert!(result.is_ok());
        
        assert_eq!(manager.get_status().await, TimeStatus::Paused);
        assert_eq!(manager.current_time().await, config.start_timestamp_us);
    }
    
    #[tokio::test]
    async fn test_time_progression() {
        let config = ReplayConfig::default();
        let manager = TimeTravelManager::new(config.clone());
        
        manager.initialize(config.start_timestamp_us, 10.0).await.unwrap();
        
        let initial_time = manager.current_time().await;
        
        manager.start().await.unwrap();
        sleep(Duration::from_millis(100)).await;
        
        let current_time = manager.current_time().await;
        assert!(current_time > initial_time);
        
        manager.pause().await.unwrap();
        let paused_time = manager.current_time().await;
        
        sleep(Duration::from_millis(100)).await;
        let still_paused_time = manager.current_time().await;
        assert_eq!(paused_time, still_paused_time);
    }
    
    #[tokio::test]
    async fn test_time_seeking() {
        let config = ReplayConfig::default();
        let manager = TimeTravelManager::new(config.clone());
        
        manager.initialize(config.start_timestamp_us, 1.0).await.unwrap();
        
        let target_time = config.start_timestamp_us + 1000000; // +1 second
        manager.seek_to(target_time).await.unwrap();
        
        assert_eq!(manager.current_time().await, target_time);
    }
    
    #[tokio::test]
    async fn test_speed_control() {
        let config = ReplayConfig::default();
        let manager = TimeTravelManager::new(config.clone());
        
        manager.initialize(config.start_timestamp_us, 1.0).await.unwrap();
        
        manager.set_speed(5.0).await.unwrap();
        manager.start().await.unwrap();
        
        let initial_time = manager.current_time().await;
        sleep(Duration::from_millis(100)).await;
        let final_time = manager.current_time().await;
        
        // 5倍速应该让虚拟时间推进得更快
        let virtual_elapsed = final_time - initial_time;
        assert!(virtual_elapsed > 400000); // 应该大于0.4秒
    }
    
    #[tokio::test]
    async fn test_bookmarks() {
        let config = ReplayConfig::default();
        let manager = TimeTravelManager::new(config.clone());
        
        manager.initialize(config.start_timestamp_us, 1.0).await.unwrap();
        
        let bookmark_time = config.start_timestamp_us + 500000; // +0.5 second
        manager.seek_to(bookmark_time).await.unwrap();
        
        manager.add_bookmark(
            "test_bookmark".to_string(),
            None,
            Some("测试书签".to_string())
        ).await.unwrap();
        
        let bookmarks = manager.get_bookmarks().await.unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].name, "test_bookmark");
        assert_eq!(bookmarks[0].timestamp_us, bookmark_time);
        
        // 跳转到其他时间
        manager.seek_to(config.start_timestamp_us).await.unwrap();
        assert_eq!(manager.current_time().await, config.start_timestamp_us);
        
        // 跳转回书签
        manager.seek_to_bookmark("test_bookmark").await.unwrap();
        assert_eq!(manager.current_time().await, bookmark_time);
    }
    
    #[tokio::test]
    async fn test_time_subscription() {
        let config = ReplayConfig::default();
        let manager = TimeTravelManager::new(config.clone());
        
        let mut time_receiver = manager.subscribe_time_updates();
        
        manager.initialize(config.start_timestamp_us, 1.0).await.unwrap();
        
        // 等待初始化广播
        let initial_time = time_receiver.changed().await;
        assert!(initial_time.is_ok());
        
        let time = *time_receiver.borrow();
        assert_eq!(time.timestamp_us, config.start_timestamp_us);
        assert_eq!(time.status, VirtualTimeStatus::Paused);
    }
}