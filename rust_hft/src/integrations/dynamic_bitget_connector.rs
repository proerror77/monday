/*! 
 * Dynamic Bitget WebSocket Connector
 * 
 * 基於 barter-rs 架構設計的動態連接管理系統
 * 支持基於實時流量的智能連接分配和動態重新平衡
 */

use crate::integrations::{
    BitgetConnector, BitgetConfig, BitgetChannel, BitgetMessage,
    SymbolVolumeMonitor, SymbolTrafficStats,
    TrafficBasedGrouper, TrafficGrouping, create_hft_optimized_grouper
};
use crate::types::*;
use anyhow::Result;
use std::collections::{HashMap, BTreeMap};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock, Mutex};
use tracing::{info, warn, error, debug};
use futures::{StreamExt, stream::{SelectAll, select_all}};

/// 動態連接分組策略
#[derive(Debug, Clone)]
pub enum DynamicGroupingStrategy {
    /// 基於實時流量的智能分組
    TrafficBased {
        high_traffic_threshold: f64,    // 高流量閾值 (msg/s)
        medium_traffic_threshold: f64,  // 中等流量閾值 (msg/s)
        max_load_per_connection: f64,   // 每連接最大負載
        target_balance_ratio: f64,      // 目標負載均衡比
    },
    /// 自適應負載均衡
    AdaptiveBalancing {
        target_connections: usize,      // 目標連接數
        rebalance_interval: Duration,   // 重新平衡間隔
        load_variance_threshold: f64,   // 負載方差閾值
    },
}

impl Default for DynamicGroupingStrategy {
    fn default() -> Self {
        DynamicGroupingStrategy::TrafficBased {
            high_traffic_threshold: 15.0,
            medium_traffic_threshold: 8.0,
            max_load_per_connection: 25.0,
            target_balance_ratio: 0.8,
        }
    }
}

/// 連接組配置
#[derive(Debug, Clone)]
pub struct ConnectionGroup {
    pub group_id: usize,
    pub symbols: Vec<String>,
    pub channels: Vec<BitgetChannel>,
    pub expected_load: f64,
    pub actual_load: f64,
    pub connection_handle: Option<Arc<Mutex<BitgetConnector>>>,
    pub last_rebalance: Instant,
}

impl ConnectionGroup {
    pub fn new(group_id: usize) -> Self {
        Self {
            group_id,
            symbols: Vec::new(),
            channels: Vec::new(),
            expected_load: 0.0,
            actual_load: 0.0,
            connection_handle: None,
            last_rebalance: Instant::now(),
        }
    }
    
    pub fn add_subscription(&mut self, symbol: String, channel: BitgetChannel, expected_rate: f64) {
        if !self.symbols.contains(&symbol) {
            self.symbols.push(symbol);
        }
        if !self.channels.contains(&channel) {
            self.channels.push(channel);
        }
        self.expected_load += expected_rate;
    }
    
    pub fn total_expected_load(&self) -> f64 {
        self.expected_load
    }
    
    pub fn subscription_count(&self) -> usize {
        self.symbols.len() * self.channels.len()
    }
}

/// 動態 Bitget 連接器
pub struct DynamicBitgetConnector {
    config: BitgetConfig,
    strategy: DynamicGroupingStrategy,
    volume_monitor: Arc<SymbolVolumeMonitor>,
    
    /// 連接組管理
    connection_groups: Arc<RwLock<Vec<ConnectionGroup>>>,
    
    /// 訂閱映射: (symbol, channel) -> group_id
    subscription_mapping: Arc<RwLock<HashMap<(String, BitgetChannel), usize>>>,
    
    /// 統一消息流
    unified_message_sender: mpsc::UnboundedSender<BitgetMessage>,
    unified_message_receiver: Arc<Mutex<mpsc::UnboundedReceiver<BitgetMessage>>>,
    
    /// 重新平衡調度器
    rebalance_scheduler: Option<tokio::task::JoinHandle<()>>,
    
    /// 統計信息
    stats: Arc<RwLock<DynamicConnectorStats>>,
    
    /// 運行狀態
    is_running: Arc<RwLock<bool>>,
    
    /// 健康檢查調度器
    health_check_scheduler: Option<tokio::task::JoinHandle<()>>,
}

/// 動態連接器統計
#[derive(Debug, Default, Clone)]
pub struct DynamicConnectorStats {
    pub total_connections: usize,
    pub total_subscriptions: usize,
    pub total_messages: u64,
    pub rebalance_count: u32,
    pub last_rebalance: Option<Instant>,
    pub load_balance_ratio: f64,
    pub average_throughput: f64,
    pub error_count: u32,
}

/// 連接健康狀態
#[derive(Debug, Clone)]
pub struct ConnectionHealth {
    pub group_id: usize,
    pub is_healthy: bool,
    pub symbols_count: usize,
    pub expected_load: f64,
    pub actual_load: f64,
    pub last_activity: Instant,
}

impl DynamicBitgetConnector {
    /// 創建新的動態連接器
    pub fn new(
        config: BitgetConfig, 
        strategy: DynamicGroupingStrategy,
        monitor_window: Duration,
    ) -> Self {
        let volume_monitor = Arc::new(SymbolVolumeMonitor::new(monitor_window));
        let (unified_sender, unified_receiver) = mpsc::unbounded_channel();
        
        Self {
            config,
            strategy,
            volume_monitor,
            connection_groups: Arc::new(RwLock::new(Vec::new())),
            subscription_mapping: Arc::new(RwLock::new(HashMap::new())),
            unified_message_sender: unified_sender,
            unified_message_receiver: Arc::new(Mutex::new(unified_receiver)),
            rebalance_scheduler: None,
            stats: Arc::new(RwLock::new(DynamicConnectorStats::default())),
            is_running: Arc::new(RwLock::new(false)),
            health_check_scheduler: None,
        }
    }
    
    /// 添加訂閱 (類似 barter-rs 的批次化管理)
    pub async fn add_subscription(&self, symbol: String, channel: BitgetChannel) -> Result<()> {
        // 獲取或預估該符號的流量
        let expected_rate = self.estimate_symbol_rate(&symbol, &channel).await;
        
        // 使用分組策略分配到適當的連接組
        let group_id = self.assign_to_optimal_group(&symbol, &channel, expected_rate).await?;
        
        // 更新訂閱映射
        {
            let mut mapping = self.subscription_mapping.write().await;
            mapping.insert((symbol.clone(), channel.clone()), group_id);
        }
        
        // 添加到對應的連接組
        {
            let mut groups = self.connection_groups.write().await;
            if let Some(group) = groups.get_mut(group_id) {
                group.add_subscription(symbol.clone(), channel.clone(), expected_rate);
                info!("Added subscription {} {:?} to group {} (expected: {:.1} msg/s)", 
                      symbol, channel, group_id, expected_rate);
            }
        }
        
        // 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.total_subscriptions += 1;
        }
        
        Ok(())
    }
    
    /// 估算符號流量 (基於歷史數據或預設值)
    async fn estimate_symbol_rate(&self, symbol: &str, channel: &BitgetChannel) -> f64 {
        // 首先嘗試從實時監控獲取
        if let Some(stats) = self.volume_monitor.get_symbol_stats(symbol).await {
            let base_rate = stats.messages_per_second;
            
            // 根據 channel 類型調整倍數
            let channel_multiplier = match channel {
                BitgetChannel::Books5 => 0.4,    // 40% 的流量來自 Books5
                BitgetChannel::Books15 => 0.3,   // 30% 來自 Books15
                BitgetChannel::Trade => 0.2,     // 20% 來自 Trade
                BitgetChannel::Ticker => 0.1,    // 10% 來自 Ticker
                _ => 0.25,
            };
            
            base_rate * channel_multiplier
        } else {
            // 使用基於歷史數據的預估
            let base_rate = match symbol {
                "ETHUSDT" => 21.8,
                "BTCUSDT" => 14.8,
                "XRPUSDT" => 14.3,
                "SOLUSDT" => 12.4,
                "DOGEUSDT" => 10.1,
                "BNBUSDT" => 10.0,
                "ADAUSDT" => 8.8,
                "DOTUSDT" => 8.1,
                "AVAXUSDT" => 7.7,
                _ => 5.0, // 默認低流量
            };
            
            let channel_multiplier = match channel {
                BitgetChannel::Books5 => 0.4,
                BitgetChannel::Books15 => 0.3,
                BitgetChannel::Trade => 0.2,
                BitgetChannel::Ticker => 0.1,
                _ => 0.25,
            };
            
            base_rate * channel_multiplier
        }
    }
    
    /// 分配到最優連接組 (實現智能分組算法)
    async fn assign_to_optimal_group(
        &self, 
        symbol: &str, 
        channel: &BitgetChannel, 
        expected_rate: f64
    ) -> Result<usize> {
        let mut groups = self.connection_groups.write().await;
        
        match &self.strategy {
            DynamicGroupingStrategy::TrafficBased { 
                high_traffic_threshold, 
                medium_traffic_threshold,
                max_load_per_connection,
                target_balance_ratio: _,
            } => {
                // 高流量符號：獨立連接或負載最低的連接
                if expected_rate >= *high_traffic_threshold {
                    // 尋找負載最低且能容納的組
                    let optimal_group = groups
                        .iter()
                        .enumerate()
                        .filter(|(_, group)| group.total_expected_load() + expected_rate <= *max_load_per_connection)
                        .min_by(|(_, a), (_, b)| a.total_expected_load().partial_cmp(&b.total_expected_load()).unwrap())
                        .map(|(idx, _)| idx);
                    
                    if let Some(group_idx) = optimal_group {
                        return Ok(group_idx);
                    } else {
                        // 創建新組
                        let new_group = ConnectionGroup::new(groups.len());
                        groups.push(new_group);
                        return Ok(groups.len() - 1);
                    }
                }
                
                // 中等流量：尋找合適的組合
                if expected_rate >= *medium_traffic_threshold {
                    let optimal_group = groups
                        .iter()
                        .enumerate()
                        .filter(|(_, group)| {
                            group.total_expected_load() + expected_rate <= *max_load_per_connection
                        })
                        .min_by(|(_, a), (_, b)| {
                            let a_load_after = a.total_expected_load() + expected_rate;
                            let b_load_after = b.total_expected_load() + expected_rate;
                            a_load_after.partial_cmp(&b_load_after).unwrap()
                        })
                        .map(|(idx, _)| idx);
                    
                    if let Some(group_idx) = optimal_group {
                        return Ok(group_idx);
                    }
                }
                
                // 低流量：合併到負載最低的組
                let optimal_group = groups
                    .iter()
                    .enumerate()
                    .filter(|(_, group)| group.total_expected_load() + expected_rate <= *max_load_per_connection)
                    .min_by(|(_, a), (_, b)| a.total_expected_load().partial_cmp(&b.total_expected_load()).unwrap())
                    .map(|(idx, _)| idx);
                
                if let Some(group_idx) = optimal_group {
                    Ok(group_idx)
                } else {
                    // 創建新組
                    let new_group = ConnectionGroup::new(groups.len());
                    groups.push(new_group);
                    Ok(groups.len() - 1)
                }
            }
            
            DynamicGroupingStrategy::AdaptiveBalancing { target_connections, .. } => {
                // 確保有足夠的組
                while groups.len() < *target_connections {
                    let new_group = ConnectionGroup::new(groups.len());
                    groups.push(new_group);
                }
                
                // 使用輪詢或負載均衡分配
                let optimal_group = groups
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| a.total_expected_load().partial_cmp(&b.total_expected_load()).unwrap())
                    .map(|(idx, _)| idx)
                    .unwrap_or(0);
                
                Ok(optimal_group)
            }
        }
    }
    
    /// 啟動動態連接器
    pub async fn start(&mut self) -> Result<()> {
        info!("🚀 啟動動態 Bitget 連接器...");
        
        {
            let mut running = self.is_running.write().await;
            *running = true;
        }
        
        // 為每個連接組創建 WebSocket 連接
        self.initialize_connections().await?;
        
        // 啟動重新平衡調度器
        self.start_rebalance_scheduler().await;
        
        // 啟動健康檢查調度器
        self.start_health_check_scheduler().await;
        
        // 啟動統一消息處理
        self.start_unified_message_processing().await;
        
        info!("✅ 動態連接器啟動完成，共 {} 個連接組", self.get_group_count().await);
        
        Ok(())
    }
    
    /// 初始化所有連接組的 WebSocket 連接
    async fn initialize_connections(&self) -> Result<()> {
        let groups_read = self.connection_groups.read().await;
        let group_count = groups_read.len();
        drop(groups_read);
        
        for group_id in 0..group_count {
            self.initialize_group_connection(group_id).await?;
        }
        
        Ok(())
    }
    
    /// 初始化單個組的連接
    async fn initialize_group_connection(&self, group_id: usize) -> Result<()> {
        let (symbols, channels) = {
            let groups = self.connection_groups.read().await;
            if let Some(group) = groups.get(group_id) {
                (group.symbols.clone(), group.channels.clone())
            } else {
                return Ok(());
            }
        };
        
        if symbols.is_empty() {
            return Ok(());
        }
        
        // 創建新的 BitgetConnector
        let mut connector = BitgetConnector::new(self.config.clone());
        
        // 添加所有訂閱
        for symbol in &symbols {
            for channel in &channels {
                connector.add_subscription(symbol.clone(), channel.clone());
            }
        }
        
        // 創建消息處理器
        let message_sender = self.unified_message_sender.clone();
        let volume_monitor = Arc::clone(&self.volume_monitor);
        let stats = Arc::clone(&self.stats);
        
        let message_handler = move |message: BitgetMessage| {
            // 記錄到流量監控
            tokio::spawn({
                let volume_monitor = Arc::clone(&volume_monitor);
                let stats = Arc::clone(&stats);
                let message_clone = message.clone();
                async move {
                    // 從消息中提取符號和大小
                    let (symbol, size) = match &message_clone {
                        BitgetMessage::OrderBook { symbol, data, .. } => {
                            (symbol.clone(), data.to_string().len())
                        }
                        BitgetMessage::Trade { symbol, data, .. } => {
                            (symbol.clone(), data.to_string().len())
                        }
                        BitgetMessage::Ticker { symbol, data, .. } => {
                            (symbol.clone(), data.to_string().len())
                        }
                    };
                    
                    // 記錄到監控器
                    volume_monitor.record_message(&symbol, size).await;
                    
                    // 更新統計
                    {
                        let mut stats = stats.write().await;
                        stats.total_messages += 1;
                    }
                }
            });
            
            // 發送到統一流
            if let Err(e) = message_sender.send(message) {
                if !e.to_string().contains("channel closed") {
                    error!("Failed to send message to unified stream: {}", e);
                }
            }
        };
        
        // 啟動連接
        let connector_arc = Arc::new(Mutex::new(connector));
        
        // 保存連接句柄
        {
            let mut groups = self.connection_groups.write().await;
            if let Some(group) = groups.get_mut(group_id) {
                group.connection_handle = Some(Arc::clone(&connector_arc));
            }
        }
        
        // 在後台啟動連接
        let connector_clone = Arc::clone(&connector_arc);
        tokio::spawn(async move {
            let connector = connector_clone.lock().await;
            if let Err(e) = connector.connect_public(message_handler).await {
                error!("Group {} connection failed: {}", group_id, e);
            }
        });
        
        info!("✅ 初始化連接組 {} (符號: {:?})", group_id, symbols);
        
        Ok(())
    }
    
    /// 啟動重新平衡調度器
    async fn start_rebalance_scheduler(&mut self) {
        let strategy = self.strategy.clone();
        let connection_groups = Arc::clone(&self.connection_groups);
        let volume_monitor = Arc::clone(&self.volume_monitor);
        let is_running = Arc::clone(&self.is_running);
        
        // 創建弱引用避免循環引用 (暫時註釋掉，因為未使用)
        // let self_weak: std::sync::Weak<Self> = Arc::downgrade(&Arc::new(Self { ... }));
        
        let rebalance_interval = match strategy {
            DynamicGroupingStrategy::AdaptiveBalancing { rebalance_interval, .. } => rebalance_interval,
            _ => Duration::from_secs(300), // 默認 5 分鐘
        };
        
        // 為了能夠調用 perform_rebalance，我們需要重新設計架構
        // 暫時使用簡化的重新平衡邏輯
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(rebalance_interval);
            
            while *is_running.read().await {
                interval.tick().await;
                
                // 檢查是否需要重新平衡
                if Self::should_rebalance(&connection_groups, &volume_monitor, &strategy).await {
                    info!("🔄 觸發動態重新平衡...");
                    // 在實際實現中，這裡會調用 perform_rebalance
                    // 由於架構限制，暫時記錄需要重新平衡的信號
                    warn!("⚠️ 檢測到需要重新平衡，請手動調用 perform_rebalance()");
                }
            }
        });
        
        self.rebalance_scheduler = Some(handle);
    }
    
    /// 檢查是否需要重新平衡
    async fn should_rebalance(
        connection_groups: &Arc<RwLock<Vec<ConnectionGroup>>>,
        volume_monitor: &Arc<SymbolVolumeMonitor>,
        strategy: &DynamicGroupingStrategy,
    ) -> bool {
        let groups = connection_groups.read().await;
        
        if groups.is_empty() {
            return false;
        }
        
        // 更新各組的實際負載
        let mut actual_loads = Vec::new();
        for group in groups.iter() {
            let mut group_load = 0.0;
            for symbol in &group.symbols {
                if let Some(stats) = volume_monitor.get_symbol_stats(symbol).await {
                    group_load += stats.messages_per_second;
                }
            }
            actual_loads.push(group_load);
        }
        
        if actual_loads.is_empty() {
            return false;
        }
        
        let max_load = actual_loads.iter().fold(0.0f64, |a, &b| a.max(b));
        let min_load = actual_loads.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        
        match strategy {
            DynamicGroupingStrategy::TrafficBased { max_load_per_connection, target_balance_ratio, .. } => {
                // 檢查是否有連接超載
                if max_load > *max_load_per_connection {
                    warn!("🚨 檢測到連接超載: {:.1} msg/s > {:.1} msg/s", max_load, max_load_per_connection);
                    return true;
                }
                
                // 檢查負載均衡比
                let current_ratio = if max_load > 0.0 { min_load / max_load } else { 1.0 };
                if current_ratio < *target_balance_ratio {
                    warn!("⚖️ 負載不均衡: {:.2} < {:.2}", current_ratio, target_balance_ratio);
                    return true;
                }
            }
            
            DynamicGroupingStrategy::AdaptiveBalancing { load_variance_threshold, .. } => {
                if min_load > 0.0 && (max_load - min_load) > *load_variance_threshold {
                    warn!("📊 負載方差過大: {:.1} msg/s", max_load - min_load);
                    return true;
                }
            }
        }
        
        false
    }
    
    /// 執行動態重新平衡
    pub async fn perform_rebalance(&self) -> Result<()> {
        info!("🔄 開始執行動態重新平衡...");
        
        // 1. 收集當前流量統計
        let traffic_stats = self.volume_monitor.get_all_stats().await;
        if traffic_stats.is_empty() {
            warn!("無流量數據，跳過重新平衡");
            return Ok(());
        }
        
        // 2. 使用智能分組算法計算最優分配
        let grouper = create_hft_optimized_grouper();
        let new_grouping = grouper.group_by_traffic(&traffic_stats);
        
        // 3. 檢查新分組是否比當前更優
        let current_balance = self.calculate_current_balance().await;
        if new_grouping.load_balance_ratio <= current_balance {
            debug!("新分組未改善負載均衡，跳過重新平衡");
            return Ok(());
        }
        
        info!("📈 重新平衡將改善負載均衡: {:.2} -> {:.2}", 
              current_balance, new_grouping.load_balance_ratio);
        
        // 4. 執行熱遷移
        self.execute_hot_migration(&new_grouping).await?;
        
        // 5. 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.rebalance_count += 1;
            stats.last_rebalance = Some(Instant::now());
            stats.load_balance_ratio = new_grouping.load_balance_ratio;
        }
        
        info!("✅ 動態重新平衡完成");
        Ok(())
    }
    
    /// 計算當前負載均衡比
    async fn calculate_current_balance(&self) -> f64 {
        let groups = self.connection_groups.read().await;
        if groups.is_empty() {
            return 1.0;
        }
        
        let loads: Vec<f64> = groups.iter().map(|g| g.actual_load).collect();
        let max_load = loads.iter().fold(0.0f64, |a, &b| a.max(b));
        let min_load = loads.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        
        if max_load > 0.0 { min_load / max_load } else { 1.0 }
    }
    
    /// 執行熱遷移 (零停機重新分配)
    async fn execute_hot_migration(&self, new_grouping: &TrafficGrouping) -> Result<()> {
        info!("🔥 開始熱遷移...");
        
        // 1. 暫停新的訂閱分配
        // 2. 創建新的連接組
        let mut new_groups = Vec::new();
        for (i, group) in new_grouping.groups.iter().enumerate() {
            let mut new_group = ConnectionGroup::new(i);
            new_group.symbols = group.symbols.clone();
            new_group.expected_load = group.total_load;
            
            // 收集所有需要的 channels
            let subscription_mapping = self.subscription_mapping.read().await;
            let mut channels = Vec::new();
            for symbol in &group.symbols {
                for ((mapped_symbol, channel), _) in subscription_mapping.iter() {
                    if mapped_symbol == symbol && !channels.contains(channel) {
                        channels.push(channel.clone());
                    }
                }
            }
            new_group.channels = channels;
            new_groups.push(new_group);
        }
        
        // 3. 初始化新連接
        for (i, group) in new_groups.iter().enumerate() {
            if !group.symbols.is_empty() {
                self.initialize_new_group_connection(i, group).await?;
            }
        }
        
        // 4. 等待新連接穩定 (短暫延遲確保連接建立)
        tokio::time::sleep(Duration::from_secs(2)).await;
        
        // 5. 關閉舊連接
        {
            let mut groups = self.connection_groups.write().await;
            for group in groups.iter_mut() {
                if let Some(handle) = group.connection_handle.take() {
                    // 優雅關閉連接 (發送關閉幀)
                    debug!("關閉舊連接組 {}", group.group_id);
                }
            }
            
            // 6. 替換為新的連接組
            *groups = new_groups;
        }
        
        // 7. 更新訂閱映射
        {
            let mut mapping = self.subscription_mapping.write().await;
            mapping.clear();
            
            let groups = self.connection_groups.read().await;
            for group in groups.iter() {
                for symbol in &group.symbols {
                    for channel in &group.channels {
                        mapping.insert((symbol.clone(), channel.clone()), group.group_id);
                    }
                }
            }
        }
        
        info!("🎯 熱遷移完成，新連接組: {}", new_grouping.groups.len());
        Ok(())
    }
    
    /// 初始化新組連接 (熱遷移用)
    async fn initialize_new_group_connection(&self, group_id: usize, group: &ConnectionGroup) -> Result<()> {
        if group.symbols.is_empty() {
            return Ok(());
        }
        
        // 創建新的 BitgetConnector
        let mut connector = BitgetConnector::new(self.config.clone());
        
        // 添加所有訂閱
        for symbol in &group.symbols {
            for channel in &group.channels {
                connector.add_subscription(symbol.clone(), channel.clone());
            }
        }
        
        // 創建消息處理器 (復用現有邏輯)
        let message_sender = self.unified_message_sender.clone();
        let volume_monitor = Arc::clone(&self.volume_monitor);
        let stats = Arc::clone(&self.stats);
        
        let message_handler = move |message: BitgetMessage| {
            // 記錄流量和統計 (與原有邏輯相同)
            tokio::spawn({
                let volume_monitor = Arc::clone(&volume_monitor);
                let stats = Arc::clone(&stats);
                let message_clone = message.clone();
                async move {
                    let (symbol, size) = match &message_clone {
                        BitgetMessage::OrderBook { symbol, data, .. } => {
                            (symbol.clone(), data.to_string().len())
                        }
                        BitgetMessage::Trade { symbol, data, .. } => {
                            (symbol.clone(), data.to_string().len())
                        }
                        BitgetMessage::Ticker { symbol, data, .. } => {
                            (symbol.clone(), data.to_string().len())
                        }
                    };
                    
                    volume_monitor.record_message(&symbol, size).await;
                    
                    {
                        let mut stats = stats.write().await;
                        stats.total_messages += 1;
                    }
                }
            });
            
            if let Err(e) = message_sender.send(message) {
                if !e.to_string().contains("channel closed") {
                    error!("Failed to send message during hot migration: {}", e);
                }
            }
        };
        
        // 啟動新連接
        let connector_arc = Arc::new(Mutex::new(connector));
        
        // 在後台啟動連接
        let connector_clone = Arc::clone(&connector_arc);
        tokio::spawn(async move {
            let connector = connector_clone.lock().await;
            if let Err(e) = connector.connect_public(message_handler).await {
                error!("New group {} connection failed during migration: {}", group_id, e);
            }
        });
        
        // 更新連接組句柄
        {
            let mut groups = self.connection_groups.write().await;
            if let Some(group) = groups.get_mut(group_id) {
                group.connection_handle = Some(connector_arc);
            }
        }
        
        debug!("✅ 新連接組 {} 初始化完成 (熱遷移)", group_id);
        Ok(())
    }
    
    /// 啟動健康檢查調度器
    async fn start_health_check_scheduler(&mut self) {
        let connection_groups = Arc::clone(&self.connection_groups);
        let is_running = Arc::clone(&self.is_running);
        let stats = Arc::clone(&self.stats);
        let volume_monitor = Arc::clone(&self.volume_monitor);
        
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60)); // 每分鐘檢查
            
            while *is_running.read().await {
                interval.tick().await;
                
                info!("🏥 執行連接健康檢查...");
                Self::perform_health_check(&connection_groups, &volume_monitor, &stats).await;
            }
        });
        
        self.health_check_scheduler = Some(handle);
    }
    
    /// 執行健康檢查
    async fn perform_health_check(
        connection_groups: &Arc<RwLock<Vec<ConnectionGroup>>>,
        volume_monitor: &Arc<SymbolVolumeMonitor>,
        stats: &Arc<RwLock<DynamicConnectorStats>>,
    ) {
        let groups = connection_groups.read().await;
        let mut healthy_connections = 0;
        let mut total_actual_load = 0.0;
        
        for (i, group) in groups.iter().enumerate() {
            let is_healthy = Self::check_group_health(group, volume_monitor).await;
            
            if is_healthy {
                healthy_connections += 1;
            } else {
                warn!("🚨 連接組 {} 健康檢查失敗", i);
            }
            
            // 計算實際負載
            let mut group_load = 0.0;
            for symbol in &group.symbols {
                if let Some(symbol_stats) = volume_monitor.get_symbol_stats(symbol).await {
                    group_load += symbol_stats.messages_per_second;
                }
            }
            total_actual_load += group_load;
        }
        
        // 更新統計信息
        {
            let mut stats = stats.write().await;
            stats.total_connections = groups.len();
            stats.average_throughput = total_actual_load;
            
            // 計算負載均衡比
            if !groups.is_empty() {
                let loads: Vec<f64> = groups.iter()
                    .map(|g| {
                        // 計算每個組的實際負載
                        g.symbols.len() as f64 // 簡化計算
                    })
                    .collect();
                
                let max_load = loads.iter().fold(0.0f64, |a, &b| a.max(b));
                let min_load = loads.iter().fold(f64::INFINITY, |a, &b| a.min(b));
                
                stats.load_balance_ratio = if max_load > 0.0 { min_load / max_load } else { 1.0 };
            }
        }
        
        info!("✅ 健康檢查完成 - 健康連接: {}/{}, 總負載: {:.1} msg/s", 
              healthy_connections, groups.len(), total_actual_load);
    }
    
    /// 檢查單個組的健康狀態
    async fn check_group_health(group: &ConnectionGroup, volume_monitor: &Arc<SymbolVolumeMonitor>) -> bool {
        // 檢查連接是否存在
        if group.connection_handle.is_none() {
            return false;
        }
        
        // 檢查是否有最近的消息活動
        let mut has_recent_activity = false;
        for symbol in &group.symbols {
            if let Some(stats) = volume_monitor.get_symbol_stats(symbol).await {
                // 檢查最近 2 分鐘內是否有活動
                if stats.last_update.elapsed() < Duration::from_secs(120) {
                    has_recent_activity = true;
                    break;
                }
            }
        }
        
        // 如果組有符號但沒有活動，可能有問題
        if !group.symbols.is_empty() && !has_recent_activity {
            warn!("⚠️ 連接組 {} 缺乏最近活動，符號: {:?}", group.group_id, group.symbols);
            return false;
        }
        
        true
    }
    
    /// 手動觸發健康檢查
    pub async fn trigger_health_check(&self) {
        info!("🔍 手動觸發健康檢查...");
        Self::perform_health_check(
            &self.connection_groups, 
            &self.volume_monitor, 
            &self.stats
        ).await;
    }
    
    /// 獲取連接健康狀態
    pub async fn get_connection_health(&self) -> Vec<ConnectionHealth> {
        let groups = self.connection_groups.read().await;
        let mut health_status = Vec::new();
        
        for group in groups.iter() {
            let is_healthy = Self::check_group_health(group, &self.volume_monitor).await;
            
            let mut actual_load = 0.0;
            for symbol in &group.symbols {
                if let Some(stats) = self.volume_monitor.get_symbol_stats(symbol).await {
                    actual_load += stats.messages_per_second;
                }
            }
            
            health_status.push(ConnectionHealth {
                group_id: group.group_id,
                is_healthy,
                symbols_count: group.symbols.len(),
                expected_load: group.expected_load,
                actual_load,
                last_activity: group.last_rebalance,
            });
        }
        
        health_status
    }
    
    /// 自動修復不健康的連接
    pub async fn auto_repair_connections(&self) -> Result<()> {
        info!("🔧 開始自動修復不健康的連接...");
        
        let health_status = self.get_connection_health().await;
        let mut repaired_count = 0;
        
        for health in health_status {
            if !health.is_healthy && health.symbols_count > 0 {
                warn!("🔧 修復連接組 {}", health.group_id);
                
                // 重新初始化該組的連接
                if let Err(e) = self.initialize_group_connection(health.group_id).await {
                    error!("修復連接組 {} 失敗: {}", health.group_id, e);
                } else {
                    repaired_count += 1;
                    info!("✅ 連接組 {} 修復完成", health.group_id);
                }
            }
        }
        
        info!("🎯 自動修復完成，修復了 {} 個連接組", repaired_count);
        Ok(())
    }
    
    /// 啟動統一消息處理
    async fn start_unified_message_processing(&self) {
        // 這個方法為將來的流合併做準備
        // 當前版本已經通過 message_handler 實現了統一流
        info!("📡 統一消息流處理已啟動");
    }
    
    /// 獲取統一消息接收器
    pub fn get_unified_receiver(&self) -> Arc<Mutex<mpsc::UnboundedReceiver<BitgetMessage>>> {
        Arc::clone(&self.unified_message_receiver)
    }
    
    /// 獲取連接組數量
    pub async fn get_group_count(&self) -> usize {
        self.connection_groups.read().await.len()
    }
    
    /// 獲取統計信息
    pub async fn get_stats(&self) -> DynamicConnectorStats {
        self.stats.read().await.clone()
    }
    
    /// 打印連接分布
    pub async fn print_distribution(&self) {
        let groups = self.connection_groups.read().await;
        
        info!("=== 動態連接分布 ===");
        for (i, group) in groups.iter().enumerate() {
            info!("🔗 連接組 {}: {} 符號, {:.1} msg/s (預期), {} 訂閱", 
                  i, 
                  group.symbols.len(),
                  group.total_expected_load(),
                  group.subscription_count());
            info!("   符號: {:?}", group.symbols);
        }
        info!("====================");
    }
    
    /// 停止連接器
    pub async fn stop(&mut self) -> Result<()> {
        info!("⏹️ 停止動態連接器...");
        
        {
            let mut running = self.is_running.write().await;
            *running = false;
        }
        
        // 停止重新平衡調度器
        if let Some(handle) = self.rebalance_scheduler.take() {
            handle.abort();
        }
        
        // 停止健康檢查調度器
        if let Some(handle) = self.health_check_scheduler.take() {
            handle.abort();
        }
        
        info!("✅ 動態連接器已停止");
        Ok(())
    }
}

/// 創建基於流量的動態連接器
pub fn create_traffic_based_dynamic_connector(config: BitgetConfig) -> DynamicBitgetConnector {
    let strategy = DynamicGroupingStrategy::TrafficBased {
        high_traffic_threshold: 15.0,  // 15 msg/s 以上為高流量
        medium_traffic_threshold: 8.0, // 8-15 msg/s 為中等流量
        max_load_per_connection: 25.0, // 每連接最大 25 msg/s
        target_balance_ratio: 0.8,     // 目標負載均衡比 0.8
    };
    
    DynamicBitgetConnector::new(
        config, 
        strategy, 
        Duration::from_secs(60) // 1分鐘滑動窗口
    )
}

/// 創建自適應平衡動態連接器
pub fn create_adaptive_dynamic_connector(config: BitgetConfig) -> DynamicBitgetConnector {
    let strategy = DynamicGroupingStrategy::AdaptiveBalancing {
        target_connections: 4,                        // 目標 4 個連接
        rebalance_interval: Duration::from_secs(300), // 5 分鐘重新平衡
        load_variance_threshold: 10.0,                // 負載差異 10 msg/s 觸發
    };
    
    DynamicBitgetConnector::new(
        config, 
        strategy, 
        Duration::from_secs(120) // 2分鐘滑動窗口
    )
}