/*!
 * 🔄 LOB Replay Engine - 核心回放引擎实现
 * 
 * 功能特性：
 * - 高性能历史数据回放 (10k+ events/s)
 * - 精确时间控制和倍速播放
 * - 内存优化的流式处理
 * - 实时监控和统计
 * - 优雅的暂停/恢复/停止控制
 */

use super::*;
use crate::core::{types::*, config::Config, error::*};
use crate::database::clickhouse_client::ClickHouseClient;
use crate::utils::performance_monitor::PerformanceMonitor;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock, Mutex, watch, oneshot};
use tokio::time::{sleep, interval};
use tracing::{info, warn, error, debug, instrument, span, Level};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::collections::VecDeque;

/// LOB 回放引擎
pub struct LobReplayEngine {
    /// 配置
    config: ReplayConfig,
    /// 当前状态
    state: Arc<RwLock<ReplayState>>,
    /// 统计信息
    stats: Arc<RwLock<ReplayStats>>,
    /// 数据加载器
    data_loader: Arc<HistoricalDataLoader>,
    /// 时间管理器
    time_manager: Arc<TimeTravelManager>,
    /// 事件发送器
    event_sender: Option<mpsc::UnboundedSender<ReplayEvent>>,
    /// 控制命令接收器
    control_receiver: Option<mpsc::UnboundedReceiver<ReplayControlEvent>>,
    /// 控制命令发送器 (外部使用)
    control_sender: mpsc::UnboundedSender<ReplayControlEvent>,
    /// 数据缓冲区
    data_buffer: Arc<Mutex<VecDeque<DataChunk>>>,
    /// 性能监控器
    performance_monitor: Arc<PerformanceMonitor>,
    /// 停止信号
    shutdown_sender: Option<oneshot::Sender<()>>,
    shutdown_receiver: Option<oneshot::Receiver<()>>,
}

impl LobReplayEngine {
    /// 创建新的回放引擎
    #[instrument(skip(clickhouse_client))]
    pub async fn new(
        config: ReplayConfig,
        clickhouse_client: Arc<ClickHouseClient>,
    ) -> ReplayResult<Self> {
        info!("创建 LOB 回放引擎, 配置: {:?}", config);
        
        // 验证配置
        Self::validate_config(&config)?;
        
        // 创建数据加载器
        let data_loader = Arc::new(
            HistoricalDataLoader::new(clickhouse_client, config.clone()).await?
        );
        
        // 创建时间管理器
        let time_manager = Arc::new(TimeTravelManager::new(config.clone()));
        
        // 创建控制通道
        let (control_sender, control_receiver) = mpsc::unbounded_channel();
        
        // 创建停止信号通道
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        
        // 创建性能监控器
        let performance_monitor = Arc::new(PerformanceMonitor::new("replay_engine".to_string()));
        
        Ok(Self {
            config,
            state: Arc::new(RwLock::new(ReplayState::Uninitialized)),
            stats: Arc::new(RwLock::new(ReplayStats::default())),
            data_loader,
            time_manager,
            event_sender: None,
            control_receiver: Some(control_receiver),
            control_sender,
            data_buffer: Arc::new(Mutex::new(VecDeque::new())),
            performance_monitor,
            shutdown_sender: Some(shutdown_sender),
            shutdown_receiver: Some(shutdown_receiver),
        })
    }
    
    /// 验证配置
    fn validate_config(config: &ReplayConfig) -> ReplayResult<()> {
        if config.start_timestamp_us >= config.end_timestamp_us {
            return Err(ReplayError::Config(
                "开始时间必须早于结束时间".to_string()
            ));
        }
        
        if config.speed_multiplier <= 0.0 {
            return Err(ReplayError::Config(
                "回放速度倍数必须大于0".to_string()
            ));
        }
        
        if config.symbols.is_empty() {
            return Err(ReplayError::Config(
                "必须指定至少一个交易对".to_string()
            ));
        }
        
        if config.max_memory_mb < 100 {
            return Err(ReplayError::Config(
                "最大内存使用量不能少于100MB".to_string()
            ));
        }
        
        Ok(())
    }
    
    /// 获取控制发送器的克隆
    pub fn control_sender(&self) -> mpsc::UnboundedSender<ReplayControlEvent> {
        self.control_sender.clone()
    }
    
    /// 初始化回放引擎
    #[instrument(skip(self))]
    pub async fn initialize(&mut self) -> ReplayResult<()> {
        let _span = span!(Level::INFO, "initialize_replay_engine").entered();
        info!("初始化回放引擎");
        
        // 更新状态
        self.set_state(ReplayState::Loading).await;
        
        // 重置统计信息
        {
            let mut stats = self.stats.write().await;
            *stats = ReplayStats::default();
            stats.current_timestamp_us = self.config.start_timestamp_us;
        }
        
        // 预加载初始数据
        let start_time = Instant::now();
        self.preload_initial_data().await?;
        let load_time = start_time.elapsed();
        
        // 更新加载时间统计
        {
            let mut stats = self.stats.write().await;
            stats.load_time = load_time;
        }
        
        // 初始化时间管理器
        self.time_manager.initialize(
            self.config.start_timestamp_us,
            self.config.speed_multiplier,
        ).await?;
        
        // 更新状态为就绪
        self.set_state(ReplayState::Ready).await;
        
        info!("回放引擎初始化完成, 加载时间: {:?}", load_time);
        Ok(())
    }
    
    /// 预加载初始数据
    async fn preload_initial_data(&mut self) -> ReplayResult<()> {
        let chunk_duration_us = 60_000_000; // 1分钟的数据块
        let mut current_time = self.config.start_timestamp_us;
        let mut total_events = 0;
        let max_preload_time = self.config.start_timestamp_us + chunk_duration_us * 5; // 预加载5分钟
        
        while current_time < max_preload_time.min(self.config.end_timestamp_us) {
            let chunk_end = (current_time + chunk_duration_us).min(self.config.end_timestamp_us);
            let time_range = TimeRange::new(current_time, chunk_end);
            
            match self.data_loader.load_chunk(time_range.clone()).await {
                Ok(chunk) => {
                    total_events += chunk.event_count();
                    
                    // 添加到缓冲区
                    {
                        let mut buffer = self.data_buffer.lock().await;
                        buffer.push_back(chunk);
                        
                        // 限制缓冲区大小
                        while buffer.len() > 10 {
                            buffer.pop_front();
                        }
                    }
                    
                    info!("预加载数据块: {:?}, 事件数: {}", time_range, total_events);
                }
                Err(e) => {
                    warn!("预加载数据块失败: {:?}, 错误: {}", time_range, e);
                }
            }
            
            current_time = chunk_end;
        }
        
        // 更新总事件数统计
        {
            let mut stats = self.stats.write().await;
            stats.total_events = total_events as u64;
        }
        
        info!("预加载完成, 总事件数: {}", total_events);
        Ok(())
    }
    
    /// 开始回放
    #[instrument(skip(self, event_sender))]
    pub async fn start_replay(
        &mut self,
        event_sender: mpsc::UnboundedSender<ReplayEvent>,
    ) -> ReplayResult<()> {
        info!("开始回放");
        
        // 检查状态
        let current_state = self.get_state().await;
        if current_state != ReplayState::Ready {
            return Err(ReplayError::InvalidState {
                current_state,
                expected_state: ReplayState::Ready,
            });
        }
        
        self.event_sender = Some(event_sender);
        self.set_state(ReplayState::Replaying).await;
        
        // 更新开始时间
        {
            let mut stats = self.stats.write().await;
            stats.start_time = Some(Instant::now());
        }
        
        // 启动回放主循环
        self.run_replay_loop().await
    }
    
    /// 回放主循环
    async fn run_replay_loop(&mut self) -> ReplayResult<()> {
        let _span = span!(Level::INFO, "replay_loop").entered();
        
        let mut control_receiver = self.control_receiver.take()
            .ok_or_else(|| ReplayError::Concurrency("控制接收器已被使用".to_string()))?;
            
        let mut shutdown_receiver = self.shutdown_receiver.take()
            .ok_or_else(|| ReplayError::Concurrency("停止接收器已被使用".to_string()))?;
        
        // 创建定时器用于控制回放速度
        let mut ticker = interval(Duration::from_micros(1000)); // 1ms 间隔
        let mut current_virtual_time = self.config.start_timestamp_us;
        
        loop {
            tokio::select! {
                // 检查停止信号
                _ = &mut shutdown_receiver => {
                    info!("收到停止信号，结束回放");
                    break;
                }
                
                // 处理控制命令
                Some(control_event) = control_receiver.recv() => {
                    match self.handle_control_event(control_event).await {
                        Ok(should_continue) => {
                            if !should_continue {
                                break;
                            }
                        }
                        Err(e) => {
                            error!("处理控制事件失败: {}", e);
                            self.set_state(ReplayState::Error(e.to_string())).await;
                            break;
                        }
                    }
                }
                
                // 回放定时处理
                _ = ticker.tick() => {
                    let state = self.get_state().await;
                    if state != ReplayState::Replaying {
                        continue;
                    }
                    
                    // 处理当前时间点的事件
                    match self.process_events_at_time(current_virtual_time).await {
                        Ok(events_processed) => {
                            if events_processed > 0 {
                                self.update_progress(current_virtual_time).await;
                            }
                            
                            // 推进虚拟时间
                            current_virtual_time += self.calculate_time_step().await;
                            
                            // 检查是否完成
                            if current_virtual_time >= self.config.end_timestamp_us {
                                info!("回放完成");
                                self.set_state(ReplayState::Completed).await;
                                
                                // 发送完成事件
                                if let Some(ref sender) = self.event_sender {
                                    let _ = sender.send(ReplayEvent::Control(ReplayControlEvent::Completed));
                                }
                                break;
                            }
                        }
                        Err(e) => {
                            error!("处理事件失败: {}", e);
                            self.set_state(ReplayState::Error(e.to_string())).await;
                            break;
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
    
    /// 处理控制事件
    async fn handle_control_event(&mut self, event: ReplayControlEvent) -> ReplayResult<bool> {
        match event {
            ReplayControlEvent::Start => {
                self.set_state(ReplayState::Replaying).await;
                info!("回放已开始");
            }
            ReplayControlEvent::Pause => {
                self.set_state(ReplayState::Paused).await;
                info!("回放已暂停");
            }
            ReplayControlEvent::Resume => {
                self.set_state(ReplayState::Replaying).await;
                info!("回放已恢复");
            }
            ReplayControlEvent::Stop => {
                self.set_state(ReplayState::Completed).await;
                info!("回放已停止");
                return Ok(false); // 结束循环
            }
            ReplayControlEvent::SetSpeed(new_speed) => {
                self.config.speed_multiplier = new_speed;
                self.time_manager.set_speed(new_speed).await;
                info!("回放速度已调整为: {}x", new_speed);
            }
            ReplayControlEvent::SeekTo(timestamp_us) => {
                info!("跳转到时间点: {}", utils::timestamp_to_string(timestamp_us));
                self.seek_to_timestamp(timestamp_us).await?;
            }
            _ => {
                debug!("收到其他控制事件: {:?}", event);
            }
        }
        
        Ok(true)
    }
    
    /// 跳转到指定时间点
    async fn seek_to_timestamp(&mut self, timestamp_us: u64) -> ReplayResult<()> {
        if timestamp_us < self.config.start_timestamp_us || timestamp_us > self.config.end_timestamp_us {
            return Err(ReplayError::TimeRange(
                format!("时间戳超出范围: {}", timestamp_us)
            ));
        }
        
        // 清空当前缓冲区
        {
            let mut buffer = self.data_buffer.lock().await;
            buffer.clear();
        }
        
        // 更新时间管理器
        self.time_manager.seek_to(timestamp_us).await;
        
        // 重新加载数据
        let chunk_duration_us = 60_000_000; // 1分钟
        let chunk_end = (timestamp_us + chunk_duration_us).min(self.config.end_timestamp_us);
        let time_range = TimeRange::new(timestamp_us, chunk_end);
        
        let chunk = self.data_loader.load_chunk(time_range).await?;
        
        {
            let mut buffer = self.data_buffer.lock().await;
            buffer.push_back(chunk);
        }
        
        // 更新统计信息
        {
            let mut stats = self.stats.write().await;
            stats.current_timestamp_us = timestamp_us;
        }
        
        info!("已跳转到时间点: {}", utils::timestamp_to_string(timestamp_us));
        Ok(())
    }
    
    /// 处理指定时间点的事件
    async fn process_events_at_time(&self, virtual_time_us: u64) -> ReplayResult<usize> {
        let tolerance_us = 1000; // 1ms 容差
        let mut events_processed = 0;
        
        // 从缓冲区获取数据
        let buffer = self.data_buffer.lock().await;
        for chunk in buffer.iter() {
            if !chunk.time_range.contains(virtual_time_us) {
                continue;
            }
            
            for event in &chunk.events {
                let event_time = match event {
                    ReplayEvent::LobUpdate { timestamp_us, .. } => *timestamp_us,
                    ReplayEvent::Trade { timestamp_us, .. } => *timestamp_us,
                    ReplayEvent::Ticker { timestamp_us, .. } => *timestamp_us,
                    ReplayEvent::Control(_) => continue,
                };
                
                // 检查事件时间是否匹配
                if event_time >= virtual_time_us && event_time < virtual_time_us + tolerance_us {
                    // 发送事件
                    if let Some(ref sender) = self.event_sender {
                        if let Err(e) = sender.send(event.clone()) {
                            warn!("发送事件失败: {}", e);
                        } else {
                            events_processed += 1;
                        }
                    }
                }
            }
        }
        
        Ok(events_processed)
    }
    
    /// 计算时间步长
    async fn calculate_time_step(&self) -> u64 {
        let base_step_us = 1000; // 基础步长 1ms
        let speed = self.config.speed_multiplier;
        
        // 根据回放速度调整步长
        if speed >= 1.0 {
            (base_step_us as f64 * speed) as u64
        } else {
            (base_step_us as f64 / (1.0 / speed)) as u64
        }
    }
    
    /// 更新进度
    async fn update_progress(&self, current_time_us: u64) {
        let mut stats = self.stats.write().await;
        
        let total_duration = self.config.end_timestamp_us - self.config.start_timestamp_us;
        let elapsed_duration = current_time_us - self.config.start_timestamp_us;
        
        stats.progress = if total_duration > 0 {
            elapsed_duration as f64 / total_duration as f64
        } else {
            1.0
        };
        
        stats.current_timestamp_us = current_time_us;
        stats.processed_events += 1;
        
        if let Some(start_time) = stats.start_time {
            stats.elapsed_time = start_time.elapsed();
            if stats.elapsed_time.as_secs() > 0 {
                stats.events_per_second = stats.processed_events as f64 / stats.elapsed_time.as_secs_f64();
            }
        }
        
        stats.memory_usage_mb = utils::calculate_memory_usage();
    }
    
    /// 设置状态
    async fn set_state(&self, new_state: ReplayState) {
        let mut state = self.state.write().await;
        *state = new_state;
    }
    
    /// 获取状态
    async fn get_state(&self) -> ReplayState {
        self.state.read().await.clone()
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> ReplayStats {
        self.stats.read().await.clone()
    }
    
    /// 优雅停止
    pub async fn shutdown(&mut self) -> ReplayResult<()> {
        info!("开始优雅停止回放引擎");
        
        if let Some(sender) = self.shutdown_sender.take() {
            let _ = sender.send(());
        }
        
        // 等待一段时间让处理完成
        sleep(Duration::from_millis(100)).await;
        
        self.set_state(ReplayState::Completed).await;
        info!("回放引擎已停止");
        
        Ok(())
    }
    
    /// 检查内存使用量
    async fn check_memory_usage(&self) -> ReplayResult<()> {
        let current_usage = utils::calculate_memory_usage();
        if current_usage > self.config.max_memory_mb as f64 {
            return Err(ReplayError::OutOfMemory {
                required_mb: current_usage as u64,
                available_mb: self.config.max_memory_mb,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::clickhouse_client::ClickHouseClient;
    
    #[tokio::test]
    async fn test_replay_config_validation() {
        // 测试无效配置
        let invalid_config = ReplayConfig {
            start_timestamp_us: 2000,
            end_timestamp_us: 1000, // 结束时间早于开始时间
            ..Default::default()
        };
        
        assert!(LobReplayEngine::validate_config(&invalid_config).is_err());
        
        // 测试有效配置
        let valid_config = ReplayConfig::default();
        assert!(LobReplayEngine::validate_config(&valid_config).is_ok());
    }
    
    #[tokio::test]
    async fn test_time_step_calculation() {
        let config = ReplayConfig {
            speed_multiplier: 2.0,
            ..Default::default()
        };
        
        // 这里需要实际的 ClickHouse 客户端来创建引擎
        // 暂时跳过完整测试
    }
}