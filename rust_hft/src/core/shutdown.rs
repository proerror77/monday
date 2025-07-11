/*!
 * Graceful Shutdown 管理器
 * 
 * 功能：
 * - 信号处理 (SIGINT, SIGTERM)
 * - 线程优雅关闭
 * - 资源清理
 * - 状态保存
 */

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::signal;
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn, error};
use serde::{Serialize, Deserialize};

/// Shutdown 管理器
#[derive(Debug)]
pub struct ShutdownManager {
    /// 是否正在关闭
    is_shutting_down: Arc<AtomicBool>,
    
    /// 关闭信号广播器
    shutdown_tx: broadcast::Sender<ShutdownSignal>,
    
    /// 注册的组件
    components: Arc<RwLock<Vec<Box<dyn ShutdownComponent>>>>,
    
    /// 关闭配置
    config: ShutdownConfig,
    
    /// 关闭开始时间
    shutdown_start_time: Arc<RwLock<Option<Instant>>>,
}

/// 关闭配置
#[derive(Debug, Clone)]
pub struct ShutdownConfig {
    /// 优雅关闭超时时间
    pub graceful_timeout: Duration,
    
    /// 强制关闭超时时间
    pub force_timeout: Duration,
    
    /// 保存状态到磁盘
    pub save_state: bool,
    
    /// 状态保存路径
    pub state_save_path: String,
    
    /// 启用详细日志
    pub verbose_logging: bool,
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            graceful_timeout: Duration::from_secs(30),
            force_timeout: Duration::from_secs(60),
            save_state: true,
            state_save_path: "data/shutdown_state.json".to_string(),
            verbose_logging: true,
        }
    }
}

/// 关闭信号类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSignal {
    /// 优雅关闭请求
    Graceful,
    /// 立即关闭请求
    Immediate,
    /// 强制关闭请求
    Force,
}

/// 关闭组件特征
pub trait ShutdownComponent: Send + Sync {
    /// 组件名称
    fn name(&self) -> &str;
    
    /// 优雅关闭
    async fn shutdown(&mut self) -> Result<()>;
    
    /// 保存状态
    async fn save_state(&self) -> Result<ComponentState>;
    
    /// 是否关键组件 (关键组件关闭失败会记录错误但不会阻止系统关闭)
    fn is_critical(&self) -> bool {
        false
    }
    
    /// 关闭超时时间
    fn shutdown_timeout(&self) -> Duration {
        Duration::from_secs(10)
    }
}

/// 组件状态
#[derive(Debug, Serialize, Deserialize)]
pub struct ComponentState {
    pub component_name: String,
    pub timestamp: u64,
    pub data: serde_json::Value,
}

/// 系统状态
#[derive(Debug, Serialize, Deserialize)]
pub struct SystemState {
    pub shutdown_timestamp: u64,
    pub shutdown_reason: String,
    pub component_states: Vec<ComponentState>,
}

impl ShutdownManager {
    /// 创建新的关闭管理器
    pub fn new(config: ShutdownConfig) -> Self {
        let (shutdown_tx, _) = broadcast::channel(16);
        
        Self {
            is_shutting_down: Arc::new(AtomicBool::new(false)),
            shutdown_tx,
            components: Arc::new(RwLock::new(Vec::new())),
            config,
            shutdown_start_time: Arc::new(RwLock::new(None)),
        }
    }
    
    /// 注册组件
    pub async fn register_component(&self, component: Box<dyn ShutdownComponent>) {
        let mut components = self.components.write().await;
        if self.config.verbose_logging {
            info!("🔧 Registered shutdown component: {}", component.name());
        }
        components.push(component);
    }
    
    /// 获取关闭信号接收器
    pub fn subscribe(&self) -> broadcast::Receiver<ShutdownSignal> {
        self.shutdown_tx.subscribe()
    }
    
    /// 检查是否正在关闭
    pub fn is_shutting_down(&self) -> bool {
        self.is_shutting_down.load(Ordering::Relaxed)
    }
    
    /// 开始监听关闭信号
    pub async fn listen_for_signals(&self) -> Result<()> {
        let shutdown_tx = self.shutdown_tx.clone();
        let is_shutting_down = Arc::clone(&self.is_shutting_down);
        let verbose = self.config.verbose_logging;
        
        tokio::spawn(async move {
            let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt()).unwrap();
            let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate()).unwrap();
            
            tokio::select! {
                _ = sigint.recv() => {
                    if verbose {
                        info!("📡 Received SIGINT (Ctrl+C)");
                    }
                    is_shutting_down.store(true, Ordering::Relaxed);
                    let _ = shutdown_tx.send(ShutdownSignal::Graceful);
                }
                _ = sigterm.recv() => {
                    if verbose {
                        info!("📡 Received SIGTERM");
                    }
                    is_shutting_down.store(true, Ordering::Relaxed);
                    let _ = shutdown_tx.send(ShutdownSignal::Graceful);
                }
            }
        });
        
        if self.config.verbose_logging {
            info!("👂 Listening for shutdown signals...");
        }
        
        Ok(())
    }
    
    /// 请求关闭
    pub async fn request_shutdown(&self, signal: ShutdownSignal) {
        if self.is_shutting_down.swap(true, Ordering::Relaxed) {
            warn!("⚠️ Shutdown already in progress");
            return;
        }
        
        *self.shutdown_start_time.write().await = Some(Instant::now());
        
        if self.config.verbose_logging {
            info!("🛑 Shutdown requested: {:?}", signal);
        }
        
        let _ = self.shutdown_tx.send(signal);
    }
    
    /// 执行关闭序列
    pub async fn shutdown(&self, reason: String) -> Result<()> {
        let start_time = Instant::now();
        info!("🛑 Starting graceful shutdown: {}", reason);
        
        // 保存系统状态
        let system_state = if self.config.save_state {
            Some(self.save_system_state(reason.clone()).await?)
        } else {
            None
        };
        
        // 关闭所有组件
        self.shutdown_components().await?;
        
        // 保存状态到磁盘
        if let Some(state) = system_state {
            self.save_state_to_disk(&state).await?;
        }
        
        let shutdown_duration = start_time.elapsed();
        info!("✅ Graceful shutdown completed in {:.2}s", shutdown_duration.as_secs_f64());
        
        Ok(())
    }
    
    /// 关闭所有组件
    async fn shutdown_components(&self) -> Result<()> {
        let mut components = self.components.write().await;
        let total_components = components.len();
        
        if total_components == 0 {
            info!("ℹ️ No components to shutdown");
            return Ok(());
        }
        
        info!("🔄 Shutting down {} components...", total_components);
        
        let mut shutdown_errors = Vec::new();
        let mut successful_shutdowns = 0;
        
        // 安全地并行关闭所有组件
        let mut shutdown_tasks = Vec::new();
        
        // 收集組件信息，避免unsafe操作
        let component_infos: Vec<_> = components.iter().map(|component| {
            (
                component.name().to_string(),
                component.is_critical(),
                component.shutdown_timeout(),
            )
        }).collect();
        
        // 創建共享組件引用用於安全並發訪問
        let components_arc = Arc::clone(&self.components);
        
        for (index, (component_name, is_critical, timeout)) in component_infos.into_iter().enumerate() {
            let components_ref = Arc::clone(&components_arc);
            
            let task = tokio::spawn(async move {
                let start_time = Instant::now();
                
                // 安全地獲取組件並調用shutdown
                let result = {
                    let components_guard = components_ref.read().await;
                    if let Some(component) = components_guard.get(index) {
                        tokio::time::timeout(timeout, component.shutdown()).await
                    } else {
                        return Err(format!("Component '{}' not found at index {}", component_name, index));
                    }
                };
                
                let elapsed = start_time.elapsed();
                
                match result {
                    Ok(Ok(())) => {
                        info!("✅ Component '{}' shutdown successfully in {:.2}s", 
                              component_name, elapsed.as_secs_f64());
                        Ok(())
                    }
                    Ok(Err(e)) => {
                        let error_msg = format!("Component '{}' shutdown failed: {}", component_name, e);
                        if is_critical {
                            error!("❌ CRITICAL: {}", error_msg);
                        } else {
                            warn!("⚠️ {}", error_msg);
                        }
                        Err(error_msg)
                    }
                    Err(_) => {
                        let error_msg = format!("Component '{}' shutdown timed out after {:.2}s", 
                                               component_name, timeout.as_secs_f64());
                        if is_critical {
                            error!("❌ CRITICAL: {}", error_msg);
                        } else {
                            warn!("⚠️ {}", error_msg);
                        }
                        Err(error_msg)
                    }
                }
            });
            
            shutdown_tasks.push(task);
        }
        
        // 等待所有关闭任务完成
        let shutdown_timeout = tokio::time::timeout(
            self.config.graceful_timeout,
            futures::future::join_all(shutdown_tasks)
        ).await;
        
        match shutdown_timeout {
            Ok(results) => {
                for result in results {
                    match result {
                        Ok(Ok(())) => successful_shutdowns += 1,
                        Ok(Err(error)) => shutdown_errors.push(error),
                        Err(e) => shutdown_errors.push(format!("Task join error: {}", e)),
                    }
                }
            }
            Err(_) => {
                error!("❌ Global shutdown timeout exceeded ({:.2}s)", 
                       self.config.graceful_timeout.as_secs_f64());
                return Err(anyhow::anyhow!("Shutdown timeout exceeded"));
            }
        }
        
        info!("📊 Shutdown summary: {}/{} components successful", 
              successful_shutdowns, total_components);
        
        if !shutdown_errors.is_empty() {
            warn!("⚠️ Shutdown errors occurred:");
            for error in &shutdown_errors {
                warn!("  - {}", error);
            }
        }
        
        Ok(())
    }
    
    /// 保存系统状态
    async fn save_system_state(&self, reason: String) -> Result<SystemState> {
        info!("💾 Saving system state...");
        
        let components = self.components.read().await;
        let mut component_states = Vec::new();
        
        for component in components.iter() {
            match component.save_state().await {
                Ok(state) => {
                    component_states.push(state);
                    if self.config.verbose_logging {
                        info!("💾 Saved state for component: {}", component.name());
                    }
                }
                Err(e) => {
                    warn!("⚠️ Failed to save state for component '{}': {}", component.name(), e);
                }
            }
        }
        
        let system_state = SystemState {
            shutdown_timestamp: crate::core::types::now_micros(),
            shutdown_reason: reason,
            component_states,
        };
        
        info!("✅ System state saved ({} components)", system_state.component_states.len());
        
        Ok(system_state)
    }
    
    /// 保存状态到磁盘
    async fn save_state_to_disk(&self, state: &SystemState) -> Result<()> {
        // 创建目录
        if let Some(parent) = std::path::Path::new(&self.config.state_save_path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        
        // 序列化状态
        let state_json = serde_json::to_string_pretty(state)?;
        
        // 写入文件
        tokio::fs::write(&self.config.state_save_path, state_json).await?;
        
        info!("💾 System state saved to: {}", self.config.state_save_path);
        
        Ok(())
    }
    
    /// 获取关闭统计
    pub async fn get_shutdown_stats(&self) -> ShutdownStats {
        let shutdown_start = self.shutdown_start_time.read().await;
        let components_count = self.components.read().await.len();
        
        ShutdownStats {
            is_shutting_down: self.is_shutting_down(),
            shutdown_start_time: *shutdown_start,
            registered_components: components_count,
            graceful_timeout_seconds: self.config.graceful_timeout.as_secs(),
        }
    }
}

/// 关闭统计
#[derive(Debug)]
pub struct ShutdownStats {
    pub is_shutting_down: bool,
    pub shutdown_start_time: Option<Instant>,
    pub registered_components: usize,
    pub graceful_timeout_seconds: u64,
}

/// 创建默认关闭管理器
pub fn create_default_shutdown_manager() -> ShutdownManager {
    ShutdownManager::new(ShutdownConfig::default())
}

/// 创建快速关闭管理器 (用于开发和测试)
pub fn create_fast_shutdown_manager() -> ShutdownManager {
    let config = ShutdownConfig {
        graceful_timeout: Duration::from_secs(5),
        force_timeout: Duration::from_secs(10),
        save_state: false,
        verbose_logging: true,
        ..ShutdownConfig::default()
    };
    
    ShutdownManager::new(config)
}

// 示例组件实现
#[derive(Debug)]
pub struct ExampleComponent {
    name: String,
    is_critical: bool,
}

impl ExampleComponent {
    pub fn new(name: String, is_critical: bool) -> Self {
        Self { name, is_critical }
    }
}

impl ShutdownComponent for ExampleComponent {
    fn name(&self) -> &str {
        &self.name
    }
    
    async fn shutdown(&mut self) -> Result<()> {
        // 模拟关闭过程
        tokio::time::sleep(Duration::from_millis(100)).await;
        info!("🔧 Component '{}' shutdown completed", self.name);
        Ok(())
    }
    
    async fn save_state(&self) -> Result<ComponentState> {
        Ok(ComponentState {
            component_name: self.name.clone(),
            timestamp: crate::core::types::now_micros(),
            data: serde_json::json!({
                "is_critical": self.is_critical,
                "status": "shutdown"
            }),
        })
    }
    
    fn is_critical(&self) -> bool {
        self.is_critical
    }
    
    fn shutdown_timeout(&self) -> Duration {
        if self.is_critical {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(10)
        }
    }
}