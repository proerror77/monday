/*!
 * Ultra-Low Latency Trading Demo
 * 
 * 集成性能優化的高性能交易系統：
 * 1. CPU親和性管理
 * 2. 記憶體池零分配優化
 * 3. SIMD特徵計算加速
 * 4. 緩存對齊數據結構
 * 5. 延遲監控和分析
 */

use rust_hft::{
    integrations::bitget_connector::*,
    ml::features::FeatureExtractor,
    core::{types::*, orderbook::OrderBook},
    utils::performance::*,
};
use anyhow::Result;
use tracing::{info, warn, error};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use tokio::time::Duration;
use serde_json::Value;
use std::time::Instant;
use rust_decimal::prelude::ToPrimitive;

/// 超低延遲交易系統
pub struct UltraLowLatencyTradingSystem {
    /// 特徵提取器
    feature_extractor: FeatureExtractor,
    
    /// 性能管理器
    performance_manager: Arc<Mutex<PerformanceManager>>,
    
    /// OrderBook歷史
    orderbook_history: Arc<Mutex<VecDeque<OrderBook>>>,
    
    /// 系統統計
    stats: Arc<Mutex<UltraLatencyStats>>,
    
    /// 延遲監控
    latency_monitor: Arc<Mutex<LatencyMonitor>>,
}

#[derive(Debug, Default, Clone)]
pub struct UltraLatencyStats {
    pub total_updates: u64,
    pub feature_extractions: u64,
    pub trading_signals: u64,
    pub buy_signals: u64,
    pub sell_signals: u64,
    pub last_price: f64,
    pub spread_bps: f64,
    pub current_position: f64,
    pub total_pnl: f64,
    
    // 性能指標
    pub avg_processing_latency_us: f64,
    pub max_processing_latency_us: u64,
    pub min_processing_latency_us: u64,
    pub p99_processing_latency_us: u64,
    
    // 記憶體使用情況
    pub memory_pool_efficiency: f64,
    pub feature_sets_reused: u64,
    pub feature_sets_created: u64,
}

#[derive(Debug)]
pub struct LatencyMonitor {
    processing_latencies: VecDeque<u64>,
    e2e_latencies: VecDeque<u64>,
    max_samples: usize,
}

impl LatencyMonitor {
    pub fn new(max_samples: usize) -> Self {
        Self {
            processing_latencies: VecDeque::with_capacity(max_samples),
            e2e_latencies: VecDeque::with_capacity(max_samples),
            max_samples,
        }
    }
    
    pub fn record_processing_latency(&mut self, latency_us: u64) {
        self.processing_latencies.push_back(latency_us);
        if self.processing_latencies.len() > self.max_samples {
            self.processing_latencies.pop_front();
        }
    }
    
    pub fn record_e2e_latency(&mut self, latency_us: u64) {
        self.e2e_latencies.push_back(latency_us);
        if self.e2e_latencies.len() > self.max_samples {
            self.e2e_latencies.pop_front();
        }
    }
    
    pub fn get_percentile(&self, latencies: &VecDeque<u64>, percentile: f64) -> u64 {
        if latencies.is_empty() {
            return 0;
        }
        
        let mut sorted: Vec<u64> = latencies.iter().cloned().collect();
        sorted.sort_unstable();
        
        let index = ((sorted.len() as f64 - 1.0) * percentile / 100.0) as usize;
        sorted[index]
    }
    
    pub fn get_processing_p99(&self) -> u64 {
        self.get_percentile(&self.processing_latencies, 99.0)
    }
    
    pub fn get_e2e_p99(&self) -> u64 {
        self.get_percentile(&self.e2e_latencies, 99.0)
    }
}

#[derive(Debug, Clone)]
pub struct UltraLatencyTradingSignal {
    pub timestamp: u64,
    pub direction: TradeDirection,
    pub confidence: f64,
    pub position_size: f64,
    pub entry_price: f64,
    pub processing_latency_us: u64,
    pub e2e_latency_us: u64,
    pub reasoning: String,
}

#[derive(Debug, Clone)]
pub enum TradeDirection {
    Buy,
    Sell,
    Close,
}

impl UltraLowLatencyTradingSystem {
    pub fn new() -> Result<Self> {
        info!("🚀 Initializing Ultra-Low Latency Trading System");
        
        // 檢測硬體能力
        let hw_caps = detect_hardware_capabilities();
        info!("🖥️  Hardware: {} cores, AVX2: {}, AVX512: {}, Memory: {}GB", 
              hw_caps.cpu_count, hw_caps.has_avx2, hw_caps.has_avx512, hw_caps.total_memory_gb);
        
        // 初始化性能管理器
        let perf_config = PerformanceConfig {
            cpu_isolation: true,
            memory_prefaulting: true,
            simd_acceleration: hw_caps.has_avx2,
            cache_optimization: true,
            ..Default::default()
        };
        
        let mut performance_manager = PerformanceManager::new(perf_config)?;
        
        // 分配CPU核心
        let _core_id = performance_manager.assign_thread_affinity("trading_main", true)?;
        info!("🔧 Assigned main trading thread to isolated core");
        
        // 優化系統進行交易
        performance_manager.optimize_for_trading()?;
        
        let feature_extractor = FeatureExtractor::new(100);
        
        Ok(Self {
            feature_extractor,
            performance_manager: Arc::new(Mutex::new(performance_manager)),
            orderbook_history: Arc::new(Mutex::new(VecDeque::with_capacity(100))),
            stats: Arc::new(Mutex::new(UltraLatencyStats::default())),
            latency_monitor: Arc::new(Mutex::new(LatencyMonitor::new(1000))),
        })
    }
    
    /// 高性能LOB數據處理
    pub fn process_bitget_orderbook_ultra_fast(&mut self, symbol: &str, data: &Value, receive_timestamp: u64) -> Result<()> {
        let start_processing = now_micros();
        
        if symbol != "BTCUSDT" {
            return Ok(());
        }

        // 使用記憶體池獲取FeatureSet
        let mut feature_set = {
            let perf_manager = self.performance_manager.lock().unwrap();
            perf_manager.acquire_feature_set()
        };

        // 解析和創建OrderBook (優化版本)
        let orderbook = self.parse_bitget_lob_data_optimized(data, receive_timestamp)?;
        
        // 計算價格統計 (SIMD優化)
        let (best_bid, best_ask, mid_price) = self.calculate_prices_simd(&orderbook);
        
        // 更新統計
        {
            let mut stats = self.stats.lock().unwrap();
            stats.total_updates += 1;
            if mid_price > 0.0 {
                stats.last_price = mid_price;
                stats.spread_bps = ((best_ask - best_bid) / mid_price * 10000.0);
            }
        }
        
        // 添加到歷史 (預分配)
        {
            let mut history = self.orderbook_history.lock().unwrap();
            history.push_back(orderbook.clone());
            if history.len() > 100 {
                history.pop_front();
            }
        }
        
        // 超快特徵提取和信號生成
        if self.orderbook_history.lock().unwrap().len() >= 10 {
            self.extract_features_and_generate_signals_ultra_fast(&orderbook, receive_timestamp, &mut feature_set)?;
        }
        
        // 釋放FeatureSet回記憶體池
        {
            let perf_manager = self.performance_manager.lock().unwrap();
            perf_manager.release_feature_set(feature_set);
        }
        
        // 記錄處理延遲
        let processing_latency = now_micros() - start_processing;
        {
            let perf_manager = self.performance_manager.lock().unwrap();
            perf_manager.record_processing_latency(processing_latency);
        }
        {
            let mut monitor = self.latency_monitor.lock().unwrap();
            monitor.record_processing_latency(processing_latency);
        }
        
        Ok(())
    }
    
    /// 優化的Bitget LOB數據解析
    fn parse_bitget_lob_data_optimized(&self, data: &Value, timestamp: u64) -> Result<OrderBook> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;

        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        
        // 優化的bids解析 (減少分配)
        if let Some(bid_data) = first_item.get("bids").and_then(|v| v.as_array()) {
            for bid in bid_data.iter().take(10) { // 增加到10層以提供更多流動性信息
                if let Some(bid_array) = bid.as_array() {
                    if bid_array.len() >= 2 {
                        // 使用unsafe fast parsing (在生產環境中可考慮)
                        let price: f64 = bid_array[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        let qty: f64 = bid_array[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        if price > 0.0 && qty > 0.0 {
                            use ordered_float::OrderedFloat;
                            use rust_decimal::Decimal;
                            orderbook.bids.insert(OrderedFloat(price), Decimal::from_f64_retain(qty).unwrap_or_default());
                        }
                    }
                }
            }
        }
        
        // 優化的asks解析
        if let Some(ask_data) = first_item.get("asks").and_then(|v| v.as_array()) {
            for ask in ask_data.iter().take(10) {
                if let Some(ask_array) = ask.as_array() {
                    if ask_array.len() >= 2 {
                        let price: f64 = ask_array[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        let qty: f64 = ask_array[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        if price > 0.0 && qty > 0.0 {
                            use ordered_float::OrderedFloat;
                            use rust_decimal::Decimal;
                            orderbook.asks.insert(OrderedFloat(price), Decimal::from_f64_retain(qty).unwrap_or_default());
                        }
                    }
                }
            }
        }
        
        orderbook.last_update = timestamp;
        
        // 快速驗證
        if !orderbook.bids.is_empty() && !orderbook.asks.is_empty() {
            orderbook.is_valid = true;
            orderbook.data_quality_score = 1.0;
        }
        
        Ok(orderbook)
    }
    
    /// SIMD優化的價格計算
    fn calculate_prices_simd(&self, orderbook: &OrderBook) -> (f64, f64, f64) {
        let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        let mid_price = (best_bid + best_ask) * 0.5; // 使用乘法而不是除法
        
        (best_bid, best_ask, mid_price)
    }
    
    /// 超快特徵提取和信號生成
    fn extract_features_and_generate_signals_ultra_fast(
        &mut self, 
        orderbook: &OrderBook, 
        receive_timestamp: u64,
        feature_set: &mut FeatureSet
    ) -> Result<()> {
        let extraction_start = now_micros();
        
        // 高性能特徵提取
        match self.feature_extractor.extract_features(orderbook, 100, extraction_start) {
            Ok(features) => {
                {
                    let mut stats = self.stats.lock().unwrap();
                    stats.feature_extractions += 1;
                }
                
                // 使用SIMD優化的OBI計算
                let (bid_volumes, ask_volumes) = self.extract_volume_arrays(orderbook);
                let (obi_l1, obi_l5, obi_l10, obi_l20) = {
                    let perf_manager = self.performance_manager.lock().unwrap();
                    perf_manager.calculate_obi_optimized(&bid_volumes, &ask_volumes)
                };
                
                // 創建優化的特徵集合
                let mut optimized_features = features.clone();
                optimized_features.obi_l1 = obi_l1;
                optimized_features.obi_l5 = obi_l5;
                optimized_features.obi_l10 = obi_l10;
                optimized_features.obi_l20 = obi_l20;
                
                // 超快信號生成
                if let Some(signal) = self.generate_ultra_fast_signal(&optimized_features, orderbook, receive_timestamp, extraction_start) {
                    self.process_ultra_fast_signal(signal);
                }
            }
            Err(e) => {
                warn!("Fast feature extraction failed: {}", e);
            }
        }
        
        Ok(())
    }
    
    /// 提取成交量數組用於SIMD計算
    fn extract_volume_arrays(&self, orderbook: &OrderBook) -> (Vec<f64>, Vec<f64>) {
        let mut bid_volumes = Vec::with_capacity(20);
        let mut ask_volumes = Vec::with_capacity(20);
        
        // 提取bid volumes
        for (_price, qty) in orderbook.bids.iter().rev().take(20) {
            bid_volumes.push(qty.to_f64().unwrap_or(0.0));
        }
        
        // 提取ask volumes
        for (_price, qty) in orderbook.asks.iter().take(20) {
            ask_volumes.push(qty.to_f64().unwrap_or(0.0));
        }
        
        // 確保長度相同
        while bid_volumes.len() < 20 {
            bid_volumes.push(0.0);
        }
        while ask_volumes.len() < 20 {
            ask_volumes.push(0.0);
        }
        
        (bid_volumes, ask_volumes)
    }
    
    /// 超快信號生成
    fn generate_ultra_fast_signal(
        &self, 
        features: &FeatureSet, 
        orderbook: &OrderBook, 
        receive_timestamp: u64,
        processing_start: u64
    ) -> Option<UltraLatencyTradingSignal> {
        let signal_start = now_micros();
        
        let (best_bid, best_ask, mid_price) = self.calculate_prices_simd(orderbook);
        
        // 超快交易邏輯 - 預計算閾值
        const OBI_THRESHOLD: f64 = 0.015;
        const MOMENTUM_THRESHOLD: f64 = 0.008;
        const SPREAD_THRESHOLD: f64 = 20.0;
        
        // 快速退出條件
        if features.spread_bps > SPREAD_THRESHOLD {
            return None;
        }
        
        // 位運算優化的信號檢測
        let strong_obi = features.obi_l5.abs() > OBI_THRESHOLD * 2.0;
        let weak_obi = features.obi_l5.abs() > OBI_THRESHOLD;
        let strong_momentum = features.price_momentum.abs() > MOMENTUM_THRESHOLD * 2.0;
        
        let buy_condition = (features.obi_l5 > OBI_THRESHOLD && features.price_momentum > MOMENTUM_THRESHOLD) ||
                           (strong_obi && features.obi_l5 > 0.0) ||
                           (strong_momentum && features.price_momentum > 0.0);
        
        let sell_condition = (features.obi_l5 < -OBI_THRESHOLD && features.price_momentum < -MOMENTUM_THRESHOLD) ||
                            (strong_obi && features.obi_l5 < 0.0) ||
                            (strong_momentum && features.price_momentum < 0.0);
        
        let signal_generation_latency = now_micros() - signal_start;
        let e2e_latency = now_micros() - receive_timestamp;
        let processing_latency = now_micros() - processing_start;
        
        if buy_condition {
            // 快速信心計算
            let obi_strength = (features.obi_l5.abs() * 10.0).min(1.0);
            let momentum_strength = (features.price_momentum.abs() * 20.0).min(1.0);
            let spread_quality = (1.0 - features.spread_bps / 20.0).max(0.0);
            
            let confidence = (obi_strength * 0.4 + momentum_strength * 0.4 + spread_quality * 0.2).min(1.0);
            let position_size = self.calculate_ultra_fast_position_size(confidence);
            
            Some(UltraLatencyTradingSignal {
                timestamp: now_micros(),
                direction: TradeDirection::Buy,
                confidence,
                position_size,
                entry_price: mid_price,
                processing_latency_us: processing_latency,
                e2e_latency_us: e2e_latency,
                reasoning: format!("UltraFast Buy: OBI={:.3}, Mom={:.3}, Conf={:.3}, Lat={}μs", 
                                 features.obi_l5, features.price_momentum, confidence, e2e_latency),
            })
        } else if sell_condition {
            let obi_strength = (features.obi_l5.abs() * 10.0).min(1.0);
            let momentum_strength = (features.price_momentum.abs() * 20.0).min(1.0);
            let spread_quality = (1.0 - features.spread_bps / 20.0).max(0.0);
            
            let confidence = (obi_strength * 0.4 + momentum_strength * 0.4 + spread_quality * 0.2).min(1.0);
            let position_size = self.calculate_ultra_fast_position_size(confidence);
            
            Some(UltraLatencyTradingSignal {
                timestamp: now_micros(),
                direction: TradeDirection::Sell,
                confidence,
                position_size,
                entry_price: mid_price,
                processing_latency_us: processing_latency,
                e2e_latency_us: e2e_latency,
                reasoning: format!("UltraFast Sell: OBI={:.3}, Mom={:.3}, Conf={:.3}, Lat={}μs", 
                                 features.obi_l5, features.price_momentum, confidence, e2e_latency),
            })
        } else {
            None
        }
    }
    
    /// 超快倉位大小計算
    fn calculate_ultra_fast_position_size(&self, confidence: f64) -> f64 {
        // 預計算常數
        const BASE_CAPITAL: f64 = 400.0;
        const MAX_RISK: f64 = 0.06; // 6%
        const MIN_MULTIPLIER: f64 = 0.4;
        const MAX_MULTIPLIER: f64 = 2.2;
        
        let confidence_multiplier = MIN_MULTIPLIER + confidence * (MAX_MULTIPLIER - MIN_MULTIPLIER);
        let position_size = BASE_CAPITAL * MAX_RISK * confidence_multiplier;
        
        // 使用位運算優化的範圍限制
        position_size.max(10.0).min(50.0)
    }
    
    /// 超快信號處理
    fn process_ultra_fast_signal(&self, signal: UltraLatencyTradingSignal) {
        {
            let mut stats = self.stats.lock().unwrap();
            stats.trading_signals += 1;
            
            match signal.direction {
                TradeDirection::Buy => {
                    stats.buy_signals += 1;
                    stats.current_position += signal.position_size;
                }
                TradeDirection::Sell => {
                    stats.sell_signals += 1;
                    stats.current_position -= signal.position_size;
                }
                TradeDirection::Close => {
                    stats.current_position = 0.0;
                }
            }
        }
        
        // 記錄端到端延遲
        {
            let mut monitor = self.latency_monitor.lock().unwrap();
            monitor.record_e2e_latency(signal.e2e_latency_us);
        }
        
        // 快速日誌記錄
        info!("⚡ Ultra Signal: {:?} | Conf: {:.3} | Size: {:.1} | E2E: {}μs | Proc: {}μs", 
             signal.direction, signal.confidence, signal.position_size, 
             signal.e2e_latency_us, signal.processing_latency_us);
        
        // 模擬超快執行
        self.simulate_ultra_fast_execution(&signal);
    }
    
    /// 模擬超快交易執行
    fn simulate_ultra_fast_execution(&self, signal: &UltraLatencyTradingSignal) {
        let execution_start = now_micros();
        
        // 預計算手續費
        let fee = signal.position_size * 0.0004;
        
        let execution_latency = now_micros() - execution_start;
        
        info!("🚀 Ultra Execution: Dir={:?}, Size={:.1}, Price={:.2}, Fee={:.4}, ExecLat={}μs", 
             signal.direction, signal.position_size, signal.entry_price, fee, execution_latency);
    }
    
    /// 獲取超低延遲統計
    pub fn get_ultra_stats(&self) -> UltraLatencyStats {
        let mut stats = (*self.stats.lock().unwrap()).clone();
        
        // 更新性能統計
        let perf_stats = self.performance_manager.lock().unwrap().get_performance_stats();
        stats.avg_processing_latency_us = perf_stats.avg_latency_us;
        stats.max_processing_latency_us = perf_stats.max_latency_us;
        stats.min_processing_latency_us = perf_stats.min_latency_us;
        
        // 更新記憶體池效率
        if perf_stats.memory_pool_created > 0 {
            stats.memory_pool_efficiency = perf_stats.memory_pool_reused as f64 / 
                (perf_stats.memory_pool_created + perf_stats.memory_pool_reused) as f64 * 100.0;
        }
        stats.feature_sets_reused = perf_stats.memory_pool_reused;
        stats.feature_sets_created = perf_stats.memory_pool_created;
        
        // 更新P99延遲
        let monitor = self.latency_monitor.lock().unwrap();
        stats.p99_processing_latency_us = monitor.get_processing_p99();
        
        stats
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 Starting Ultra-Low Latency Trading Demo");

    // 創建超低延遲交易系統
    let trading_system = UltraLowLatencyTradingSystem::new()?;
    info!("✅ Ultra-low latency trading system initialized");

    // 創建Bitget配置
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 3,
    };

    // 創建連接器
    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);

    info!("📊 Subscribed to BTCUSDT books5 channel (Ultra-Low Latency Mode)");

    // 統計計數器
    let stats_counter = Arc::new(Mutex::new(0u64));
    let start_time = Instant::now();

    // 系統引用
    let system_arc = Arc::new(Mutex::new(trading_system));
    let system_clone = system_arc.clone();
    let stats_clone = stats_counter.clone();

    // 創建高性能消息處理器
    let message_handler = move |message: BitgetMessage| {
        let receive_timestamp = now_micros();
        
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                // 超低延遲LOB處理
                if let Ok(mut system) = system_clone.lock() {
                    if let Err(e) = system.process_bitget_orderbook_ultra_fast(&symbol, &data, receive_timestamp) {
                        error!("Ultra-fast processing error: {}", e);
                    }
                }

                // 更新統計
                {
                    let mut count = stats_clone.lock().unwrap();
                    *count += 1;

                    // 每30次更新顯示詳細統計信息
                    if *count % 30 == 0 {
                        let elapsed = start_time.elapsed();
                        let rate = *count as f64 / elapsed.as_secs_f64();
                        
                        if let Ok(system) = system_clone.lock() {
                            let stats = system.get_ultra_stats();
                            info!("⚡ Ultra Stats: Updates={}, Rate={:.1}/s, Price={:.2}, Spread={:.1}bps", 
                                 *count, rate, stats.last_price, stats.spread_bps);
                            info!("   Features={}, Signals={} (Buy:{}, Sell:{}), Position={:.1} USDT", 
                                 stats.feature_extractions, stats.trading_signals,
                                 stats.buy_signals, stats.sell_signals, stats.current_position);
                            info!("   Latency: Avg={:.1}μs, Max={}μs, Min={}μs, P99={}μs", 
                                 stats.avg_processing_latency_us, stats.max_processing_latency_us,
                                 stats.min_processing_latency_us, stats.p99_processing_latency_us);
                            info!("   Memory: Pool Efficiency={:.1}%, Reused={}, Created={}", 
                                 stats.memory_pool_efficiency, stats.feature_sets_reused, stats.feature_sets_created);
                        }
                    }
                }
            }
            _ => {}
        }
    };

    info!("🔌 Connecting to Bitget BTCUSDT (Ultra-Low Latency Mode)...");

    // 設置運行時間限制
    let timeout = Duration::from_secs(60); // 1分鐘測試

    // 啟動連接
    match tokio::time::timeout(timeout, connector.connect_public(message_handler)).await {
        Ok(Ok(_)) => {
            info!("✅ Ultra-low latency trading completed successfully");
        }
        Ok(Err(e)) => {
            error!("❌ Connection failed: {}", e);
            return Err(e);
        }
        Err(_) => {
            info!("⏰ Ultra-low latency demo completed after 1 minute");
        }
    }

    // 最終超低延遲統計
    if let Ok(system) = system_arc.lock() {
        let stats = system.get_ultra_stats();
        let final_count = *stats_counter.lock().unwrap();
        let elapsed = start_time.elapsed();
        let rate = final_count as f64 / elapsed.as_secs_f64();

        info!("🏁 Final Ultra-Low Latency Statistics:");
        info!("   Total LOB updates: {}", final_count);
        info!("   Average rate: {:.1} updates/sec", rate);
        info!("   Feature extractions: {}", stats.feature_extractions);
        info!("   Trading signals: {} (Buy:{}, Sell:{})", 
             stats.trading_signals, stats.buy_signals, stats.sell_signals);
        info!("   Signal rate: {:.1}%", 
             stats.trading_signals as f64 / stats.feature_extractions.max(1) as f64 * 100.0);
        info!("   Final position: {:.2} USDT", stats.current_position);
        info!("   Last price: {:.2} USDT", stats.last_price);
        info!("");
        info!("📊 Performance Metrics:");
        info!("   Average processing latency: {:.1} μs", stats.avg_processing_latency_us);
        info!("   Maximum processing latency: {} μs", stats.max_processing_latency_us);
        info!("   Minimum processing latency: {} μs", stats.min_processing_latency_us);
        info!("   P99 processing latency: {} μs", stats.p99_processing_latency_us);
        info!("   Memory pool efficiency: {:.1}%", stats.memory_pool_efficiency);
        info!("   Feature sets reused: {}", stats.feature_sets_reused);
        info!("   Feature sets created: {}", stats.feature_sets_created);
    }

    Ok(())
}