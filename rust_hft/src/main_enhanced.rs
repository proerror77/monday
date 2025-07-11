/*!
 * Rust HFT - Enhanced High-Frequency Trading System
 * 
 * 增强功能：
 * - 性能监控和指标收集
 * - Graceful shutdown 管理
 * - 资源清理和状态保存
 * - 详细的系统健康监控
 */

use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tracing::{info, warn, error};
use tokio::sync::RwLock;

// Import from the new modular structure
use rust_hft::core::types::*;
use rust_hft::core::config::Config;
use rust_hft::core::shutdown::{
    ShutdownManager, ShutdownConfig, ShutdownComponent, 
    ComponentState, create_default_shutdown_manager
};
use rust_hft::utils::performance_monitor::{
    PerformanceMonitor, create_high_performance_config
};
use rust_hft::data::{network_run, processor_run};
use rust_hft::engine::{strategy_run, execution_run};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("rust_hft=info")
        .init();

    info!("🚀 Starting Enhanced Rust HFT System");

    // Load configuration
    let config = Config::load()?;
    info!("Configuration loaded for symbol: {}", config.symbol());

    // Initialize performance monitor
    let perf_config = create_high_performance_config();
    let performance_monitor = Arc::new(PerformanceMonitor::new(perf_config));
    
    // Start performance monitoring
    performance_monitor.start().await?;
    info!("📊 Performance monitoring started");

    // Initialize shutdown manager
    let shutdown_config = ShutdownConfig {
        graceful_timeout: Duration::from_secs(30),
        save_state: true,
        verbose_logging: true,
        ..ShutdownConfig::default()
    };
    let shutdown_manager = Arc::new(ShutdownManager::new(shutdown_config));
    
    // Start listening for shutdown signals
    shutdown_manager.listen_for_signals().await?;
    
    // Create shutdown signal receiver for main loop
    let mut shutdown_rx = shutdown_manager.subscribe();
    
    // Register system components for graceful shutdown
    register_system_components(&shutdown_manager, &performance_monitor).await?;

    // Check CPU cores
    let core_ids = core_affinity::get_core_ids()
        .ok_or_else(|| anyhow::anyhow!("Failed to get CPU core IDs"))?;
    
    if core_ids.len() < 4 {
        warn!("Only {} CPU cores available, recommend at least 4 for optimal performance", core_ids.len());
    }

    info!("Available CPU cores: {}", core_ids.len());

    // Create enhanced HFT system
    let system = EnhancedHftSystem::new(
        config, 
        core_ids, 
        Arc::clone(&performance_monitor), 
        Arc::clone(&shutdown_manager)
    ).await?;
    
    // Start the trading system
    let system_handle = system.start().await?;
    
    info!("✅ Enhanced HFT System fully operational");
    
    // Main loop - wait for shutdown signal
    tokio::select! {
        signal = shutdown_rx.recv() => {
            match signal {
                Ok(signal) => {
                    info!("📡 Received shutdown signal: {:?}", signal);
                    
                    // Request system shutdown
                    system.request_shutdown().await?;
                    
                    // Wait for system to stop
                    system_handle.await??;
                    
                    // Execute graceful shutdown
                    shutdown_manager.shutdown("User requested shutdown".to_string()).await?;
                }
                Err(e) => {
                    error!("Failed to receive shutdown signal: {}", e);
                }
            }
        }
        
        result = system_handle => {
            match result {
                Ok(Ok(())) => {
                    info!("🏁 HFT System completed normally");
                }
                Ok(Err(e)) => {
                    error!("❌ HFT System error: {}", e);
                    shutdown_manager.shutdown(format!("System error: {}", e)).await?;
                }
                Err(e) => {
                    error!("❌ System task join error: {}", e);
                    shutdown_manager.shutdown(format!("Task join error: {}", e)).await?;
                }
            }
        }
    }
    
    // Stop performance monitoring
    performance_monitor.stop();
    
    // Print final performance report
    print_final_performance_report(&performance_monitor).await?;
    
    info!("🏁 Enhanced Rust HFT System shutdown complete");
    Ok(())
}

/// Enhanced HFT System with monitoring and shutdown management
struct EnhancedHftSystem {
    config: Config,
    core_ids: Vec<core_affinity::CoreId>,
    performance_monitor: Arc<PerformanceMonitor>,
    shutdown_manager: Arc<ShutdownManager>,
    
    // Communication channels
    raw_msg_tx: Sender<RawMessage>,
    raw_msg_rx: Receiver<RawMessage>,
    features_tx: Sender<FeatureSet>,
    features_rx: Receiver<FeatureSet>,
    signal_tx: Sender<TradingSignal>,
    signal_rx: Receiver<TradingSignal>,
}

impl EnhancedHftSystem {
    async fn new(
        config: Config,
        core_ids: Vec<core_affinity::CoreId>,
        performance_monitor: Arc<PerformanceMonitor>,
        shutdown_manager: Arc<ShutdownManager>,
    ) -> Result<Self> {
        // Create lock-free channels with performance monitoring
        let (raw_msg_tx, raw_msg_rx): (Sender<RawMessage>, Receiver<RawMessage>) = bounded(2048);
        let (features_tx, features_rx): (Sender<FeatureSet>, Receiver<FeatureSet>) = bounded(1024);
        let (signal_tx, signal_rx): (Sender<TradingSignal>, Receiver<TradingSignal>) = bounded(512);
        
        Ok(Self {
            config,
            core_ids,
            performance_monitor,
            shutdown_manager,
            raw_msg_tx,
            raw_msg_rx,
            features_tx,
            features_rx,
            signal_tx,
            signal_rx,
        })
    }
    
    async fn start(&self) -> Result<tokio::task::JoinHandle<Result<()>>> {
        info!("🔄 Starting enhanced HFT threads with monitoring...");
        
        // CPU core assignment
        let network_core = self.core_ids[0];
        let processor_core = self.core_ids[1];
        let strategy_core = self.core_ids[2];
        let execution_core = if self.core_ids.len() >= 4 { Some(self.core_ids[3]) } else { None };
        
        // Clone necessary data for threads
        let config = self.config.clone();
        let perf_monitor = Arc::clone(&self.performance_monitor);
        let shutdown_manager = Arc::clone(&self.shutdown_manager);
        let raw_msg_tx = self.raw_msg_tx.clone();
        let raw_msg_rx = self.raw_msg_rx.clone();
        let features_tx = self.features_tx.clone();
        let features_rx = self.features_rx.clone();
        let signal_tx = self.signal_tx.clone();
        let signal_rx = self.signal_rx.clone();
        
        let handle = tokio::spawn(async move {
            // Start network thread with monitoring
            let network_config = config.clone();
            let network_perf = Arc::clone(&perf_monitor);
            let network_shutdown = shutdown_manager.subscribe();
            let network_handle = thread::spawn(move || {
                Self::run_network_thread(
                    network_core, 
                    network_config, 
                    raw_msg_tx, 
                    network_perf,
                    network_shutdown
                )
            });

            // Start processor thread with monitoring
            let processor_config = config.clone();
            let processor_perf = Arc::clone(&perf_monitor);
            let processor_shutdown = shutdown_manager.subscribe();
            let processor_handle = thread::spawn(move || {
                Self::run_processor_thread(
                    processor_core, 
                    processor_config, 
                    raw_msg_rx, 
                    features_tx, 
                    processor_perf,
                    processor_shutdown
                )
            });

            // Start strategy thread with monitoring
            let strategy_config = config.clone();
            let strategy_perf = Arc::clone(&perf_monitor);
            let strategy_shutdown = shutdown_manager.subscribe();
            let strategy_handle = thread::spawn(move || {
                Self::run_strategy_thread(
                    strategy_core, 
                    strategy_config, 
                    features_rx, 
                    signal_tx, 
                    strategy_perf,
                    strategy_shutdown
                )
            });

            // Start execution thread with monitoring (if available)
            let execution_handle = if let Some(exec_core) = execution_core {
                let execution_config = config.clone();
                let execution_perf = Arc::clone(&perf_monitor);
                let execution_shutdown = shutdown_manager.subscribe();
                Some(thread::spawn(move || {
                    Self::run_execution_thread(
                        exec_core, 
                        execution_config, 
                        signal_rx, 
                        execution_perf,
                        execution_shutdown
                    )
                }))
            } else {
                warn!("Not enough cores for dedicated execution thread");
                None
            };

            info!("✅ All HFT threads started successfully");

            // Wait for threads to complete
            let mut thread_results = Vec::new();
            
            if let Err(e) = network_handle.join() {
                error!("Network thread panicked: {:?}", e);
                thread_results.push(Err(anyhow::anyhow!("Network thread panic")));
            } else {
                thread_results.push(Ok(()));
            }

            if let Err(e) = processor_handle.join() {
                error!("Processor thread panicked: {:?}", e);
                thread_results.push(Err(anyhow::anyhow!("Processor thread panic")));
            } else {
                thread_results.push(Ok(()));
            }

            if let Err(e) = strategy_handle.join() {
                error!("Strategy thread panicked: {:?}", e);
                thread_results.push(Err(anyhow::anyhow!("Strategy thread panic")));
            } else {
                thread_results.push(Ok(()));
            }

            if let Some(handle) = execution_handle {
                if let Err(e) = handle.join() {
                    error!("Execution thread panicked: {:?}", e);
                    thread_results.push(Err(anyhow::anyhow!("Execution thread panic")));
                } else {
                    thread_results.push(Ok(()));
                }
            }

            // Check if any threads failed
            let failed_threads = thread_results.iter().filter(|r| r.is_err()).count();
            if failed_threads > 0 {
                error!("❌ {} threads failed", failed_threads);
                Err(anyhow::anyhow!("{} threads failed", failed_threads))
            } else {
                info!("✅ All threads completed successfully");
                Ok(())
            }
        });
        
        Ok(handle)
    }
    
    async fn request_shutdown(&self) -> Result<()> {
        self.shutdown_manager.request_shutdown(rust_hft::core::shutdown::ShutdownSignal::Graceful).await;
        Ok(())
    }
    
    fn run_network_thread(
        core_id: core_affinity::CoreId,
        config: Config,
        raw_msg_tx: Sender<RawMessage>,
        perf_monitor: Arc<PerformanceMonitor>,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<rust_hft::core::shutdown::ShutdownSignal>,
    ) -> Result<()> {
        // Pin to CPU core
        if !core_affinity::set_for_current(core_id) {
            error!("Failed to set CPU affinity for network thread");
        } else {
            info!("Network thread pinned to core {:?}", core_id);
        }

        // Enhanced network processing with monitoring
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        info!("🛑 Network thread received shutdown signal");
                        break;
                    }
                    
                    result = Self::process_network_messages(&config, &raw_msg_tx, &perf_monitor) => {
                        if let Err(e) = result {
                            error!("Network processing error: {}", e);
                            break;
                        }
                    }
                }
            }
        });

        info!("🏁 Network thread shutdown complete");
        Ok(())
    }
    
    async fn process_network_messages(
        config: &Config,
        raw_msg_tx: &Sender<RawMessage>,
        perf_monitor: &PerformanceMonitor,
    ) -> Result<()> {
        // 这里会实现实际的网络消息处理
        // 目前使用模拟数据
        tokio::time::sleep(Duration::from_millis(10)).await;
        
        let start_time = std::time::Instant::now();
        let message = RawMessage {
            data: "simulated_market_data".to_string(),
            received_at: now_micros(),
            sequence: Some(1),
        };
        
        // Record performance metrics
        let processing_time = start_time.elapsed().as_micros() as u64;
        perf_monitor.record_latency(processing_time);
        perf_monitor.record_message(message.data.len());
        
        if let Err(_) = raw_msg_tx.try_send(message) {
            warn!("Raw message channel full, dropping message");
        }
        
        Ok(())
    }
    
    fn run_processor_thread(
        core_id: core_affinity::CoreId,
        config: Config,
        raw_msg_rx: Receiver<RawMessage>,
        features_tx: Sender<FeatureSet>,
        perf_monitor: Arc<PerformanceMonitor>,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<rust_hft::core::shutdown::ShutdownSignal>,
    ) -> Result<()> {
        if !core_affinity::set_for_current(core_id) {
            error!("Failed to set CPU affinity for processor thread");
        } else {
            info!("Processor thread pinned to core {:?}", core_id);
        }

        // Process messages with performance monitoring
        loop {
            // Check for shutdown signal (non-blocking)
            if let Ok(_) = shutdown_rx.try_recv() {
                info!("🛑 Processor thread received shutdown signal");
                break;
            }
            
            match raw_msg_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(message) => {
                    let start_time = std::time::Instant::now();
                    
                    // Process message and extract features
                    let features = Self::extract_features_from_message(&message, &config)?;
                    
                    let processing_time = start_time.elapsed().as_micros() as u64;
                    perf_monitor.record_latency(processing_time);
                    
                    if let Err(_) = features_tx.try_send(features) {
                        warn!("Features channel full, dropping features");
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    // Timeout is expected for shutdown checking
                    continue;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    info!("Raw message channel disconnected");
                    break;
                }
            }
        }

        info!("🏁 Processor thread shutdown complete");
        Ok(())
    }
    
    fn run_strategy_thread(
        core_id: core_affinity::CoreId,
        config: Config,
        features_rx: Receiver<FeatureSet>,
        signal_tx: Sender<TradingSignal>,
        perf_monitor: Arc<PerformanceMonitor>,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<rust_hft::core::shutdown::ShutdownSignal>,
    ) -> Result<()> {
        if !core_affinity::set_for_current(core_id) {
            error!("Failed to set CPU affinity for strategy thread");
        } else {
            info!("Strategy thread pinned to core {:?}", core_id);
        }

        loop {
            if let Ok(_) = shutdown_rx.try_recv() {
                info!("🛑 Strategy thread received shutdown signal");
                break;
            }
            
            match features_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(features) => {
                    let start_time = std::time::Instant::now();
                    
                    // Generate trading signal
                    let signal = Self::generate_trading_signal(&features, &config)?;
                    
                    let processing_time = start_time.elapsed().as_micros() as u64;
                    perf_monitor.record_latency(processing_time);
                    
                    if let Some(signal) = signal {
                        if let Err(_) = signal_tx.try_send(signal) {
                            warn!("Signal channel full, dropping signal");
                        }
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    continue;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    info!("Features channel disconnected");
                    break;
                }
            }
        }

        info!("🏁 Strategy thread shutdown complete");
        Ok(())
    }
    
    fn run_execution_thread(
        core_id: core_affinity::CoreId,
        config: Config,
        signal_rx: Receiver<TradingSignal>,
        perf_monitor: Arc<PerformanceMonitor>,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<rust_hft::core::shutdown::ShutdownSignal>,
    ) -> Result<()> {
        if !core_affinity::set_for_current(core_id) {
            error!("Failed to set CPU affinity for execution thread");
        } else {
            info!("Execution thread pinned to core {:?}", core_id);
        }

        loop {
            if let Ok(_) = shutdown_rx.try_recv() {
                info!("🛑 Execution thread received shutdown signal");
                break;
            }
            
            match signal_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(signal) => {
                    let start_time = std::time::Instant::now();
                    
                    // Execute trading signal
                    Self::execute_trading_signal(&signal, &config)?;
                    
                    let processing_time = start_time.elapsed().as_micros() as u64;
                    perf_monitor.record_latency(processing_time);
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    continue;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    info!("Signal channel disconnected");
                    break;
                }
            }
        }

        info!("🏁 Execution thread shutdown complete");
        Ok(())
    }
    
    fn extract_features_from_message(message: &RawMessage, config: &Config) -> Result<FeatureSet> {
        // 简化的特征提取实现
        Ok(FeatureSet::default_enhanced())
    }
    
    fn generate_trading_signal(features: &FeatureSet, config: &Config) -> Result<Option<TradingSignal>> {
        // 简化的信号生成实现
        if features.obi_l1.abs() > 0.1 {
            Ok(Some(TradingSignal {
                signal_type: if features.obi_l1 > 0.0 { SignalType::Buy } else { SignalType::Sell },
                confidence: 0.8,
                suggested_price: features.mid_price,
                suggested_quantity: "0.001".to_quantity(),
                timestamp: now_micros(),
                features_timestamp: features.timestamp,
                signal_latency_us: 100,
            }))
        } else {
            Ok(None)
        }
    }
    
    fn execute_trading_signal(signal: &TradingSignal, config: &Config) -> Result<()> {
        // 简化的信号执行实现
        info!("📈 Executing signal: {:?} with confidence {:.2}", 
              signal.signal_type, signal.confidence);
        Ok(())
    }
}

/// Register system components for graceful shutdown
async fn register_system_components(
    shutdown_manager: &ShutdownManager,
    performance_monitor: &PerformanceMonitor,
) -> Result<()> {
    // Register performance monitor component
    let perf_component = Box::new(PerformanceMonitorComponent::new(Arc::clone(performance_monitor)));
    shutdown_manager.register_component(perf_component).await;
    
    // Register other critical components as needed
    let network_component = Box::new(NetworkComponent::new());
    shutdown_manager.register_component(network_component).await;
    
    let strategy_component = Box::new(StrategyComponent::new());
    shutdown_manager.register_component(strategy_component).await;
    
    info!("🔧 Registered {} shutdown components", 3);
    
    Ok(())
}

/// Print final performance report
async fn print_final_performance_report(performance_monitor: &PerformanceMonitor) -> Result<()> {
    let report = performance_monitor.get_performance_report()?;
    
    info!("📊 ========== Final Performance Report ==========");
    info!("⏱️ Latency Statistics:");
    info!("  - Average: {}μs", report.latency.avg_latency_us);
    info!("  - P95: {}μs", report.latency.p95_latency_us);
    info!("  - P99: {}μs", report.latency.p99_latency_us);
    info!("  - Total samples: {}", report.latency.total_samples);
    
    info!("📈 Throughput Statistics:");
    info!("  - Total messages: {}", report.throughput.total_messages);
    info!("  - Messages/sec: {:.2}", report.throughput.messages_per_second);
    info!("  - Data processed: {:.2} MB", report.throughput.total_bytes as f64 / 1024.0 / 1024.0);
    
    info!("💾 Memory Statistics:");
    info!("  - Current heap: {} MB", report.memory.current_heap_mb);
    info!("  - Peak heap: {} MB", report.memory.max_heap_mb);
    info!("  - Average heap: {} MB", report.memory.avg_heap_mb);
    
    info!("🖥️ System Statistics:");
    info!("  - CPU usage: {:.1}%", report.system.cpu_usage_percent);
    info!("  - Load average: {:.2}", report.system.load_average_1min);
    
    // Performance assessment
    if report.latency.p99_latency_us < 1000 {
        info!("🚀 EXCELLENT: P99 latency < 1ms");
    } else if report.latency.p99_latency_us < 10000 {
        info!("✅ GOOD: P99 latency < 10ms");
    } else {
        warn!("⚠️ NEEDS IMPROVEMENT: P99 latency > 10ms");
    }
    
    info!("===============================================");
    
    Ok(())
}

// Shutdown component implementations
struct PerformanceMonitorComponent {
    performance_monitor: Arc<PerformanceMonitor>,
}

impl PerformanceMonitorComponent {
    fn new(performance_monitor: Arc<PerformanceMonitor>) -> Self {
        Self { performance_monitor }
    }
}

impl ShutdownComponent for PerformanceMonitorComponent {
    fn name(&self) -> &str {
        "PerformanceMonitor"
    }
    
    async fn shutdown(&mut self) -> Result<()> {
        self.performance_monitor.stop();
        info!("🛑 Performance monitor stopped");
        Ok(())
    }
    
    async fn save_state(&self) -> Result<ComponentState> {
        let report = self.performance_monitor.get_performance_report()?;
        Ok(ComponentState {
            component_name: "PerformanceMonitor".to_string(),
            timestamp: now_micros(),
            data: serde_json::to_value(report)?,
        })
    }
    
    fn is_critical(&self) -> bool {
        false
    }
}

struct NetworkComponent;
impl NetworkComponent {
    fn new() -> Self { Self }
}

impl ShutdownComponent for NetworkComponent {
    fn name(&self) -> &str { "Network" }
    async fn shutdown(&mut self) -> Result<()> {
        info!("🛑 Network component stopped");
        Ok(())
    }
    async fn save_state(&self) -> Result<ComponentState> {
        Ok(ComponentState {
            component_name: "Network".to_string(),
            timestamp: now_micros(),
            data: serde_json::json!({"status": "shutdown"}),
        })
    }
    fn is_critical(&self) -> bool { true }
}

struct StrategyComponent;
impl StrategyComponent {
    fn new() -> Self { Self }
}

impl ShutdownComponent for StrategyComponent {
    fn name(&self) -> &str { "Strategy" }
    async fn shutdown(&mut self) -> Result<()> {
        info!("🛑 Strategy component stopped");
        Ok(())
    }
    async fn save_state(&self) -> Result<ComponentState> {
        Ok(ComponentState {
            component_name: "Strategy".to_string(),
            timestamp: now_micros(),
            data: serde_json::json!({"status": "shutdown"}),
        })
    }
    fn is_critical(&self) -> bool { true }
}