/*!
 * Rust HFT - High-Frequency Trading System
 * 
 * Pure Rust implementation targeting sub-100μs latency
 * Multi-threaded architecture with CPU affinity for maximum performance
 */

use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};
use std::thread;
use tracing::{info, warn, error};

// Import from the new modular structure
use rust_hft::core::types::*;
use rust_hft::core::config::Config;
use rust_hft::data::{network_run, processor_run};
use rust_hft::engine::{strategy_run, execution_run};

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("rust_hft=debug")
        .init();

    info!("🚀 Starting Rust HFT System");

    // Load configuration
    let config = Config::load()?;
    info!("Configuration loaded: {:?}", config);

    // Check CPU cores
    let core_ids = core_affinity::get_core_ids()
        .ok_or_else(|| anyhow::anyhow!("Failed to get CPU core IDs"))?;
    
    if core_ids.len() < 4 {
        warn!("Only {} CPU cores available, recommend at least 4 for optimal performance", core_ids.len());
    }

    info!("Available CPU cores: {}", core_ids.len());

    // Create lock-free channels for inter-thread communication
    // Network -> Processor
    let (raw_msg_tx, raw_msg_rx): (Sender<RawMessage>, Receiver<RawMessage>) = bounded(2048);
    
    // Processor -> Strategy  
    let (features_tx, features_rx): (Sender<FeatureSet>, Receiver<FeatureSet>) = bounded(1024);
    
    // Strategy -> Execution
    let (signal_tx, signal_rx): (Sender<TradingSignal>, Receiver<TradingSignal>) = bounded(512);

    // CPU core assignment
    let network_core = core_ids[0];
    let processor_core = core_ids[1];
    let strategy_core = core_ids[2];
    let execution_core = if core_ids.len() >= 4 { Some(core_ids[3]) } else { None };

    // Start performance monitoring
    let start_time = std::time::Instant::now();

    // Spawn threads with CPU affinity
    info!("Spawning threads with CPU affinity...");

    // Network Thread (Core 0)
    let network_config = config.clone();
    let network_handle = thread::spawn(move || {
        // Pin to specific CPU core
        if !core_affinity::set_for_current(network_core) {
            error!("Failed to set CPU affinity for network thread");
        } else {
            info!("Network thread pinned to core {:?}", network_core);
        }

        // Run network layer
        if let Err(e) = network_run(network_config, raw_msg_tx) {
            error!("Network thread error: {}", e);
        }
    });

    // Processor Thread (Core 1)  
    let processor_config = config.clone();
    let processor_handle = thread::spawn(move || {
        if !core_affinity::set_for_current(processor_core) {
            error!("Failed to set CPU affinity for processor thread");
        } else {
            info!("Processor thread pinned to core {:?}", processor_core);
        }

        if let Err(e) = processor_run(processor_config, raw_msg_rx, features_tx) {
            error!("Processor thread error: {}", e);
        }
    });

    // Strategy Thread (Core 2)
    let strategy_config = config.clone();
    let strategy_handle = thread::spawn(move || {
        if !core_affinity::set_for_current(strategy_core) {
            error!("Failed to set CPU affinity for strategy thread");
        } else {
            info!("Strategy thread pinned to core {:?}", strategy_core);
        }

        if let Err(e) = strategy_run(strategy_config, features_rx, signal_tx) {
            error!("Strategy thread error: {}", e);
        }
    });

    // Execution Thread (Core 3, optional)
    let execution_handle = if let Some(exec_core) = execution_core {
        let execution_config = config.clone();
        Some(thread::spawn(move || {
            if !core_affinity::set_for_current(exec_core) {
                error!("Failed to set CPU affinity for execution thread");
            } else {
                info!("Execution thread pinned to core {:?}", exec_core);
            }

            if let Err(e) = execution_run(execution_config, signal_rx) {
                error!("Execution thread error: {}", e);
            }
        }))
    } else {
        warn!("Not enough cores for dedicated execution thread");
        None
    };

    info!("All threads started in {:.2}ms", start_time.elapsed().as_micros() as f64 / 1000.0);

    // Wait for threads to complete
    info!("System running... Press Ctrl+C to stop");

    // Join threads
    if let Err(e) = network_handle.join() {
        error!("Network thread panicked: {:?}", e);
    }

    if let Err(e) = processor_handle.join() {
        error!("Processor thread panicked: {:?}", e);
    }

    if let Err(e) = strategy_handle.join() {
        error!("Strategy thread panicked: {:?}", e);
    }

    if let Some(handle) = execution_handle {
        if let Err(e) = handle.join() {
            error!("Execution thread panicked: {:?}", e);
        }
    }

    info!("🏁 Rust HFT System shutdown complete");
    Ok(())
}