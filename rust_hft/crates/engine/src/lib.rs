//! Single-writer engine loop（骨架）
//! - Ingest bounded rings from I/O
//! - Aggregate L2 TopN and Bars
//! - Publish snapshots via snapshot crate
//! - Route: Strategy -> Risk -> Execution

pub mod dataflow;
pub mod aggregation;
pub mod execution_queues;
pub mod execution_worker;
pub mod adapter_bridge;
pub mod latency_monitor;

use ports::{ExecutionClient, Strategy, ExecutionEvent, AccountView, BoxStream, VenueSpec};
use ports::Trade as MarketTrade;
use dataflow::{EventConsumer, IngestionConfig};
use aggregation::{AggregationEngine, MarketView};
use execution_queues::EngineQueues;
use snapshot::SnapshotContainer;
use hft_core::{HftError, Symbol, OrderType, Side, LatencyTracker, LatencyStage, now_micros, VenueId};
use oms_core::OmsCore;
use portfolio_core::Portfolio;
use latency_monitor::{LatencyMonitor, LatencyMonitorConfig};
use tracing::{info, warn, error, debug};
use tokio::sync::broadcast;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::Notify;
use std::collections::HashMap;

// 重新導出關鍵類型
pub use adapter_bridge::{AdapterBridge, AdapterBridgeConfig};
pub use dataflow::{FlipPolicy, BackpressurePolicy, BackpressureStatus};
pub use execution_queues::{create_execution_queues, WorkerQueues, ExecutionQueueConfig};
pub use execution_worker::{ExecutionWorker, ExecutionWorkerConfig, spawn_execution_worker};

/// 引擎運行時配置
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub ingestion: IngestionConfig,
    pub max_events_per_cycle: u32,
    pub aggregation_symbols: Vec<Symbol>,
    pub latency_monitor: LatencyMonitorConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            ingestion: IngestionConfig::default(),
            max_events_per_cycle: 100,
            aggregation_symbols: vec![],
            latency_monitor: LatencyMonitorConfig::default(),
        }
    }
}

/// 單寫入者引擎
pub struct Engine {
    /// 配置
    config: EngineConfig,
    /// 事件消費者 (從 adapters 接收)
    event_consumers: Vec<EventConsumer>,
    /// 聚合引擎
    aggregation_engine: AggregationEngine,
    /// 市場快照發佈容器
    market_snapshots: SnapshotContainer<MarketView>,
    /// 帳戶快照發佈容器  
    account_snapshots: SnapshotContainer<AccountView>,
    /// 註冊的策略
    strategies: Vec<Box<dyn Strategy>>,
    /// 風控管理器（可選）
    risk_manager: Option<Box<dyn ports::RiskManager>>,
    /// VenueSpec 映射（用於場所特定的風控）
    venue_specs: HashMap<VenueId, VenueSpec>,
    /// 註冊的執行客戶端
    execution_clients: Vec<Box<dyn ExecutionClient>>,
    /// 執行回報流（與 execution_clients 對應）
    #[allow(dead_code)]
    execution_streams: Vec<BoxStream<ExecutionEvent>>,
    /// OMS 核心 - 訂單生命週期管理
    oms_core: OmsCore,
    /// Portfolio 核心 - 會計與資金管理
    portfolio: Portfolio,
    /// 执行队列系统 (SPSC)
    execution_queues: Option<EngineQueues>,
    /// 運行統計
    cycle_count: u64,
    is_running: bool,
    /// 已處理的執行事件計數（用於調試）
    execution_events_processed: u64,
    /// 執行統計（細分）
    orders_submitted: u64,
    orders_ack: u64,
    orders_filled: u64,
    orders_rejected: u64,
    orders_canceled: u64,
    /// 暫存的市場事件（從聚合引擎產生）
    pending_market_events: Vec<ports::MarketEvent>,
    /// 事件驱动唤醒通知器
    wakeup_notify: Arc<Notify>,
    /// 訂單提交時間（用於 Ack/Fill 延遲計算）
    order_submit_ts: std::collections::HashMap<hft_core::OrderId, u64>,
    /// 最近處理的市場事件時間戳（用於端到端延遲計算）
    recent_market_event_timestamp: Option<u64>,
    /// 延遲監控器 - 統一收集所有階段延遲
    latency_monitor: LatencyMonitor,
    /// 執行事件廣播通道（供 IPC/外部追蹤）
    exec_event_tx: broadcast::Sender<ports::ExecutionEvent>,
    /// 市場成交廣播通道（供導出/監控使用）
    market_trade_tx: broadcast::Sender<MarketTrade>,
    /// 市場事件廣播通道（供導出/監控使用）
    market_event_tx: broadcast::Sender<ports::MarketEvent>,
    /// 策略到場所的映射（用於單場策略事件過濾）
    strategy_venue_mapping: HashMap<String, VenueId>,
    /// 策略實例 ID 列表（與 strategies Vec 順序對應，用於事件過濾）
    strategy_instance_ids: Vec<String>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        // 創建初始空的市場視圖
        let initial_market_view = MarketView {
            orderbooks: std::collections::HashMap::new(),
            arbitrage_opportunities: Vec::new(),
            timestamp: 0,
            version: 0,
        };
        
        // 創建初始空的帳戶視圖
        let initial_account_view = AccountView::default();
        
        let latency_monitor = LatencyMonitor::new(config.latency_monitor.clone());
        let (exec_event_tx, _rx) = broadcast::channel(1024);
        let (market_trade_tx, _rx_trade) = broadcast::channel(4096);
        let (market_event_tx, _rx_mev) = broadcast::channel(4096);
        
        Self {
            config,
            event_consumers: Vec::new(),
            aggregation_engine: AggregationEngine::new(),
            market_snapshots: SnapshotContainer::new(initial_market_view),
            account_snapshots: SnapshotContainer::new(initial_account_view),
            strategies: Vec::new(),
            risk_manager: None,
            venue_specs: VenueSpec::build_default_venue_specs(),
            execution_clients: Vec::new(),
            execution_streams: Vec::new(),
            oms_core: OmsCore::new(),
            portfolio: Portfolio::new(),
            execution_queues: None,
            cycle_count: 0,
            is_running: true,  // 默認為運行狀態，讓 tick() 可以正常運作
            execution_events_processed: 0,
            orders_submitted: 0,
            orders_ack: 0,
            orders_filled: 0,
            orders_rejected: 0,
            orders_canceled: 0,
            pending_market_events: Vec::new(),
            wakeup_notify: Arc::new(Notify::new()),
            order_submit_ts: std::collections::HashMap::new(),
            recent_market_event_timestamp: None,
            latency_monitor,
            exec_event_tx,
            market_trade_tx,
            market_event_tx,
            strategy_venue_mapping: HashMap::new(),
            strategy_instance_ids: Vec::new(),
        }
    }

    /// 设置执行队列系统（Runtime 设置）
    pub fn set_execution_queues(&mut self, queues: EngineQueues) {
        self.execution_queues = Some(queues);
    }
    
    /// 設置策略到場所的映射（用於單場策略事件過濾）
    pub fn set_strategy_venue_mapping(&mut self, mapping: HashMap<String, VenueId>) {
        self.strategy_venue_mapping = mapping;
    }
    
    /// 處理市場事件給策略（測試用）
    #[cfg(test)]
    pub fn process_market_event_for_strategies(&mut self, event: &MarketEvent, account: &AccountView) {
        for (strategy_idx, strategy) in self.strategies.iter_mut().enumerate() {
            let strategy_instance_id = self.strategy_instance_ids.get(strategy_idx)
                .unwrap_or(&"unknown".to_string());
                
            // 應用事件過濾邏輯
            if Self::should_strategy_process_event_static(strategy_instance_id, event, &self.strategy_venue_mapping) {
                let _ = strategy.on_market_event(event, account);
            }
        }
    }
    
    /// 移出执行客户端给 worker 使用 (仅调用一次)
    pub fn take_execution_clients(&mut self) -> Vec<Box<dyn ExecutionClient>> {
        std::mem::take(&mut self.execution_clients)
    }
    
    /// 获取引擎唤醒通知器的克隆，用于外部触发引擎唤醒
    pub fn get_wakeup_notify(&self) -> Arc<Notify> {
        self.wakeup_notify.clone()
    }
    
    /// 唤醒引擎（触发新的 tick 周期）
    pub fn wake_engine(&self) {
        self.wakeup_notify.notify_one();
    }
    
    /// 獲取延遲監控器的引用
    pub fn latency_monitor(&self) -> &LatencyMonitor {
        &self.latency_monitor
    }
    
    /// 設置 VenueSpec 映射
    pub fn set_venue_specs(&mut self, venue_specs: HashMap<VenueId, VenueSpec>) {
        self.venue_specs = venue_specs;
    }
    
    /// 添加單個 VenueSpec
    pub fn add_venue_spec(&mut self, venue_id: VenueId, spec: VenueSpec) {
        self.venue_specs.insert(venue_id, spec);
    }

    /// 導出 OMS 狀態（供恢復/持久化使用）
    pub fn export_oms_state(&self) -> HashMap<hft_core::OrderId, oms_core::OrderRecord> {
        self.oms_core.export_state()
    }

    /// 導入 OMS 狀態（供恢復/持久化使用）
    pub fn import_oms_state(&mut self, state: HashMap<hft_core::OrderId, oms_core::OrderRecord>) {
        self.oms_core.import_state(state);
    }

    /// 導出 Portfolio 狀態（供恢復/持久化使用）
    pub fn export_portfolio_state(&self) -> portfolio_core::PortfolioState {
        self.portfolio.export_state()
    }

    /// 導入 Portfolio 狀態（供恢復/持久化使用）
    pub fn import_portfolio_state(&mut self, state: portfolio_core::PortfolioState) {
        self.portfolio.import_state(state);
    }

    /// 獲取所有階段的延遲統計數據
    pub fn get_latency_stats(&self) -> std::collections::HashMap<hft_core::LatencyStage, hft_core::latency::LatencyStageStats> {
        self.latency_monitor.get_all_stats()
    }
    
    /// 訂閱執行事件（供外部追蹤取消等回覆）
    pub fn subscribe_execution_events(&self) -> broadcast::Receiver<ports::ExecutionEvent> {
        self.exec_event_tx.subscribe()
    }
    
    /// 訂閱市場成交事件（供外部導出/監控）
    pub fn subscribe_market_trades(&self) -> broadcast::Receiver<MarketTrade> {
        self.market_trade_tx.subscribe()
    }
    /// 訂閱市場事件（供外部導出/監控）
    pub fn subscribe_market_events(&self) -> broadcast::Receiver<ports::MarketEvent> {
        self.market_event_tx.subscribe()
    }
    
    /// 同步延遲統計到 Prometheus（可選）
    #[cfg(feature = "infra-metrics")]
    pub fn sync_latency_metrics_to_prometheus(&self) {
        let latency_stats = self.latency_monitor.get_all_stats();
        if !latency_stats.is_empty() {
            infra_metrics::MetricsRegistry::global().update_from_latency_monitor(&latency_stats);
            debug!("同步了 {} 個延遲統計到 Prometheus", latency_stats.len());
        }
    }
    
    #[cfg(not(feature = "infra-metrics"))]
    pub fn sync_latency_metrics_to_prometheus(&self) {
        // No-op when metrics disabled
    }
    
    /// 註冊事件消費者 (對應一個 market adapter)
    pub fn register_event_consumer(&mut self, mut consumer: EventConsumer) {
        // 为消费者设置引擎唤醒通知器
        consumer.set_engine_notify(self.wakeup_notify.clone());
        self.event_consumers.push(consumer);
    }
    
    /// 創建配對的 EventIngester 和 EventConsumer (用於零拷貝適配器)
    pub fn create_event_ingester_pair(&mut self) -> Arc<std::sync::Mutex<dataflow::EventIngester>> {
        let config = dataflow::IngestionConfig {
            queue_capacity: self.config.ingestion.queue_capacity,
            stale_threshold_us: self.config.ingestion.stale_threshold_us,
            ..Default::default()
        };
        
        let (ingester, consumer) = dataflow::EventIngester::new(config);
        
        // 註冊消費者到引擎
        self.register_event_consumer(consumer);
        
        // 返回攝取器供零拷貝適配器使用
        Arc::new(std::sync::Mutex::new(ingester))
    }
    
    /// 註冊策略
    pub fn register_strategy<S: Strategy + 'static>(&mut self, s: S) {
        // 在註冊策略時記錄其實例 ID（而不是類型名稱）
        let instance_id = self.extract_strategy_instance_id(&s);
        self.strategy_instance_ids.push(instance_id);
        self.strategies.push(Box::new(s));
    }
    
    pub fn register_strategy_boxed(&mut self, s: Box<dyn Strategy>) {
        // 在註冊策略時記錄其實例 ID（而不是類型名稱）
        let instance_id = self.extract_strategy_instance_id(s.as_ref());
        self.strategy_instance_ids.push(instance_id);
        self.strategies.push(s);
    }
    
    /// 提取策略實例 ID（用於事件過濾映射）：優先使用 Strategy::id()
    fn extract_strategy_instance_id<S: Strategy + ?Sized>(&self, strategy: &S) -> String {
        strategy.id().to_string()
    }
    
    /// 註冊風控管理器
    pub fn register_risk_manager<R: ports::RiskManager + 'static>(&mut self, r: R) {
        self.risk_manager = Some(Box::new(r));
    }
    
    pub fn register_risk_manager_boxed(&mut self, r: Box<dyn ports::RiskManager>) {
        self.risk_manager = Some(r);
    }
    
    /// 註冊執行客戶端
    pub fn register_execution_client<E: ExecutionClient + 'static>(&mut self, e: E) {
        self.execution_clients.push(Box::new(e));
    }
    
    pub fn register_execution_client_boxed(&mut self, e: Box<dyn ExecutionClient>) {
        self.execution_clients.push(e);
    }

    
    /// 獲取當前市場視圖快照
    pub fn get_market_view(&self) -> Arc<MarketView> {
        self.market_snapshots.load()
    }
    
    /// 獲取當前帳戶視圖快照
    pub fn get_account_view(&self) -> Arc<AccountView> {
        self.account_snapshots.load()
    }
    
    /// 獲取市場快照讀取者
    pub fn market_reader(&self) -> Arc<dyn snapshot::SnapshotReader<MarketView>> {
        self.market_snapshots.reader()
    }
    
    /// 獲取帳戶快照讀取者  
    pub fn account_reader(&self) -> Arc<dyn snapshot::SnapshotReader<AccountView>> {
        self.account_snapshots.reader()
    }
    
    /// 提交訂單意圖到執行隊列（用於 dry-run 測試）
    pub fn submit_order_intent(&mut self, intent: ports::OrderIntent) -> Result<(), HftError> {
        if let Some(queues) = &mut self.execution_queues {
            match queues.send_intent(intent) {
                Ok(()) => {
                    debug!("成功提交訂單意圖到執行隊列");
                    Ok(())
                }
                Err(rejected_intent) => {
                    warn!("執行隊列滿載，訂單意圖被拒絕: {} {}", 
                          rejected_intent.symbol.0, rejected_intent.quantity.0);
                    Err(HftError::Execution("執行隊列滿載".to_string()))
                }
            }
        } else {
            warn!("執行隊列未初始化，無法提交訂單意圖");
            Err(HftError::Execution("執行隊列未初始化".to_string()))
        }
    }
    
    /// 單次事件循環
    pub fn tick(&mut self) -> Result<EngineTickResult, HftError> {
        self.cycle_count += 1;
        let mut result = EngineTickResult::default();
        
        // 只在前 100 個循環和有事件時記錄
        if self.cycle_count <= 100 || self.cycle_count % 1000 == 0 {
            debug!("引擎 tick #{}", self.cycle_count);
        }
        
        // 階段 1: 消費市場事件
        let mut total_events = 0;
        let mut should_flip = false;
        
        for (idx, consumer) in self.event_consumers.iter_mut().enumerate() {
            let mut consumer_events = 0;
            let consumer_should_flip = consumer.consume_events(|event| {
                consumer_events += 1;
                total_events += 1;
                result.events_processed += 1;
                
                // 記錄接入階段延遲（從事件原始時間戳到現在）
                let ingestion_latency = now_micros().saturating_sub(event.tracker.origin_time);
                self.latency_monitor.record_latency(LatencyStage::Ingestion, ingestion_latency);
                
                // 記錄聚合階段開始 
                let mut tracked_event = event.clone();
                tracked_event.record_stage(hft_core::latency::LatencyStage::Aggregation);
                
                // 更新市場事件時間戳供端到端延遲計算
                self.recent_market_event_timestamp = Some(tracked_event.tracker.origin_time);
                
                // 日誌降噪：逐事件日誌改為 debug 級別
                debug!("引擎 tick #{} 處理事件 #{} 從消費者 {}: {:?}", 
                      self.cycle_count, total_events, idx,
                      match &tracked_event.event {
                          ports::MarketEvent::Bar(bar) => format!("Bar({})", bar.symbol.0),
                          ports::MarketEvent::Trade(trade) => format!("Trade({})", trade.symbol.0),
                          ports::MarketEvent::Snapshot(snap) => format!("Snapshot({})", snap.symbol.0),
                          _ => "Other".to_string(),
                      });
                
                // 若為 Trade 事件，廣播一份給外部訂閱方（最佳努力）
                if let ports::MarketEvent::Trade(t) = &tracked_event.event {
                    let _ = self.market_trade_tx.send(t.clone());
                }
                // 廣播原始市場事件（含 Snapshot/Update/Trade/Bar），供外部導出或監控
                #[allow(unused_must_use)]
                {
                    let _ = self.market_event_tx.send(tracked_event.event.clone());
                }

                // 處理事件 -> 聚合引擎
                let aggregation_start = now_micros();
                match self.aggregation_engine.handle_event(tracked_event.event) {
                    Ok(events) => {
                        // 保存生成的事件（包括 Bar 事件）
                        let event_count = events.len();
                        for ev in events {
                            self.pending_market_events.push(ev);
                        }
                        if event_count > 0 {
                            debug!("聚合引擎生成 {} 個事件", event_count);
                        }
                        
                        // 記錄聚合階段延遲到統一監控器
                        let aggregation_latency = now_micros().saturating_sub(aggregation_start);
                        self.latency_monitor.record_latency(LatencyStage::Aggregation, aggregation_latency);
                        
                        // 記錄聚合延遲到 Prometheus
                        infra_metrics::MetricsRegistry::global().record_aggregation_latency(aggregation_latency as f64);
                        
                        // 如果有新事件產生，觸發快照發佈
                        !self.pending_market_events.is_empty()
                    }
                    Err(e) => {
                        warn!("聚合引擎處理事件失敗: {}", e);
                        false
                    }
                }
            });
            
            // 日誌降噪：批量統計改為 debug，只有當有事件時才記錄
            if consumer_events > 0 {
                debug!("消費者 {} 處理了 {} 個事件", idx, consumer_events);
            }
            
            should_flip = should_flip || consumer_should_flip;
            
            // 批處理限制
            if total_events >= self.config.max_events_per_cycle {
                break;
            }
        }
        
        result.events_total = total_events;
        
        // 階段 2: 處理執行事件 (來自执行队列) - 非阻塞批量接收
        let mut exec_processed = 0u32;
        if let Some(queues) = &mut self.execution_queues {
            let execution_events = queues.receive_events();
            for ev in &execution_events {
                self.handle_execution_event(ev)?;
                exec_processed += 1;
                self.execution_events_processed += 1;
            }
        }
        result.execution_events_processed = exec_processed;
        
        // 階段 3: 檢查是否需要發佈快照
        if should_flip || self.aggregation_engine.should_publish_snapshot() || result.execution_events_processed > 0 {
            self.publish_market_snapshot(&mut result)?;
            self.publish_account_snapshot(&mut result)?;
        }
        
        // 階段 4: 策略決策 (基於最新快照)
        if result.snapshot_published {
            // Note: run_strategies simplified to sync for tick() compatibility
            self.run_strategies_sync(&mut result)?;
        }
        
        // 批量統計日誌（只有在有活動時才記錄）
        if total_events > 0 || exec_processed > 0 || result.orders_generated > 0 {
            info!("Tick #{}: {} market events, {} exec events, {} orders, snapshot: {}", 
                  self.cycle_count, total_events, exec_processed, 
                  result.orders_generated, result.snapshot_published);
        }
        
        // 定期同步延遲統計到 Prometheus（每 1000 個 tick 或有活動時）
        if self.cycle_count % 1000 == 0 || total_events > 0 || exec_processed > 0 {
            self.sync_latency_metrics_to_prometheus();
        }
        
        Ok(result)
    }

    // 注意：执行客户端连接和流准备现在由 ExecutionWorker 处理
    
    /// 發佈市場快照（增量版本）
    fn publish_market_snapshot(&mut self, result: &mut EngineTickResult) -> Result<(), HftError> {
        // 检查是否需要发布快照
        if !self.aggregation_engine.should_publish_snapshot() {
            return Ok(()); // 没有变化，跳过发布
        }
        
        let market_view = self.aggregation_engine.build_market_view();
        
        // 更新 Portfolio 的市場價格用於 mark-to-market
        let mut market_prices = std::collections::HashMap::new();
        for (symbol, _) in &market_view.orderbooks {
            if let Some(mid_price) = market_view.get_mid_price(symbol) {
                market_prices.insert(symbol.clone(), mid_price);
            }
        }
        if !market_prices.is_empty() {
            self.portfolio.update_market_prices(&market_prices);
        }
        
        // 發佈快照
        self.market_snapshots.store(Arc::new(market_view));
        result.snapshot_published = true;
        result.snapshot_sequence += 1;
        
        // 标记快照已发布，清除变更标记
        self.aggregation_engine.mark_snapshot_published();
        
        debug!("發佈增量市場快照 #{} (版本 {})", 
               result.snapshot_sequence, 
               self.aggregation_engine.snapshot_version);
        Ok(())
    }
    
    /// 發佈帳戶快照  
    fn publish_account_snapshot(&mut self, result: &mut EngineTickResult) -> Result<(), HftError> {
        // Portfolio 內部已經自動更新快照，我們只需要獲取最新的
        let account_view = self.portfolio.reader().load();
        self.account_snapshots.store(account_view);
        
        debug!("發佈帳戶快照 #{}", result.snapshot_sequence);
        Ok(())
    }
    
    
    /// 處理單個執行事件
    fn handle_execution_event(&mut self, event: &ExecutionEvent) -> Result<(), HftError> {
        debug!("處理執行事件: {:?}", event);
        // 廣播執行事件（最佳努力）
        let _ = self.exec_event_tx.send(event.clone());
        
        // 0. 先處理 OrderNew：註冊訂單元資料（供 Portfolio/OMS 使用）
        if let ExecutionEvent::OrderNew { order_id, symbol, side, quantity, timestamp, .. } = event {
            // 註冊到 OMS 與 Portfolio（供後續 Fill 計算倉位/PnL）
            self.oms_core.register_order(order_id.clone(), None, symbol.clone(), *side, *quantity);
            self.portfolio.register_order(order_id.clone(), symbol.clone(), *side);
            self.orders_submitted = self.orders_submitted.saturating_add(1);
            infra_metrics::MetricsRegistry::global().inc_orders_submitted();
            self.order_submit_ts.insert(order_id.clone(), *timestamp);
            debug!(
                order_id = %order_id.0,
                sym = %symbol.0,
                side = ?side,
                qty = %quantity.0,
                "已註冊新訂單到 OMS/Portfolio"
            );
        }

        // 1. 更新 OMS 狀態機
        if let Some(order_update) = self.oms_core.on_execution_event(event) {
            debug!("訂單狀態更新: {:?}", order_update);
            
            // 檢查是否需要生成 OrderCompleted 事件（當訂單從非Filled變為Filled）
            if order_update.status == oms_core::OrderStatus::Filled 
               && order_update.previous_status != oms_core::OrderStatus::Filled {
                
                // 創建 OrderCompleted 事件
                let completed_event = ExecutionEvent::OrderCompleted {
                    order_id: order_update.order_id.clone(),
                    final_price: order_update.avg_price.unwrap_or(hft_core::Price::zero()),
                    total_filled: order_update.cum_qty,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64,
                };
                
                info!("生成 OrderCompleted 事件: order_id={}, final_price={:?}, total_filled={}", 
                      order_update.order_id.0, 
                      order_update.avg_price, 
                      order_update.cum_qty.0);
                
                // 遞歸調用處理 OrderCompleted 事件（但 OMS 會忽略它避免循環）
                self.handle_execution_event(&completed_event)?;
            }
        }
        
        // 2. 細分統計
        match event {
            ExecutionEvent::OrderAck { order_id, timestamp } => {
                self.orders_ack = self.orders_ack.saturating_add(1);
                if let Some(&start_ts) = self.order_submit_ts.get(order_id) {
                    let lat = (*timestamp).saturating_sub(start_ts) as f64;
                    infra_metrics::MetricsRegistry::global().record_order_ack_latency(lat);
                }
            }
            ExecutionEvent::Fill { order_id, timestamp, .. } => { 
                self.orders_filled = self.orders_filled.saturating_add(1);
                infra_metrics::MetricsRegistry::global().inc_orders_filled();
                if let Some(&start_ts) = self.order_submit_ts.get(order_id) {
                    let lat = (*timestamp).saturating_sub(start_ts) as f64;
                    infra_metrics::MetricsRegistry::global().record_order_fill_latency(lat);
                }
                
                // 記錄端到端DoD延遲指標（從市場事件到執行完成）
                if let Some(market_ts) = self.recent_market_event_timestamp {
                    let end_to_end_latency = timestamp.saturating_sub(market_ts) as f64;
                    infra_metrics::MetricsRegistry::global().record_end_to_end_latency(end_to_end_latency);
                    debug!("訂單 {} 端到端延遲: {:.2}μs", order_id.0, end_to_end_latency);
                }
            }
            ExecutionEvent::OrderReject { .. } | ExecutionEvent::OrderRejected { .. } => {
                self.orders_rejected = self.orders_rejected.saturating_add(1);
                infra_metrics::MetricsRegistry::global().inc_orders_rejected();
            }
            ExecutionEvent::OrderCanceled { .. } => { 
                self.orders_canceled = self.orders_canceled.saturating_add(1);
            }
            _ => {}
        }

        // 3. 更新 Portfolio 會計
        self.portfolio.on_execution_event(event);
        
        // 4. 通知風控管理器
        if let Some(risk_manager) = &mut self.risk_manager {
            risk_manager.on_execution_event(event);
        }
        
        // 5. 讀取並打印最新 AccountView（驗證閉環）
        let av = self.portfolio.reader().load();
        info!(
            cash = av.cash_balance,
            pos_count = av.positions.len(),
            unrealized = av.unrealized_pnl,
            realized = av.realized_pnl,
            "AccountView 已更新"
        );
        
        Ok(())
    }
    
    // 注意：下单现在通过执行队列 + ExecutionWorker 异步处理
    
    /// 檢查策略是否應該處理特定的市場事件（基於 venue 過濾）
    /// 這是一個靜態方法，不依賴 self，避免借用檢查器問題
    fn should_strategy_process_event_static(
        strategy_name: &str, 
        event: &ports::MarketEvent,
        strategy_venue_mapping: &HashMap<String, VenueId>
    ) -> bool {
        // 判斷是否為單場策略
        let is_single_venue = match strategy_name {
            "TrendStrategy" | "ImbalanceStrategy" => true,
            "ArbitrageStrategy" => false, // 跨場策略
            _ => {
                // 對於未知策略，根據名稱模式推斷
                let name_lower = strategy_name.to_lowercase();
                if name_lower.contains("arbitrage") || name_lower.contains("cross") || name_lower.contains("arb") {
                    false // 跨場策略
                } else {
                    true // 默認為單場策略
                }
            }
        };
        
        // 跨場策略接收所有事件
        if !is_single_venue {
            return true;
        }
        
        // 單場策略只接收來自其目標場的事件
        // 提取事件的 source_venue
        let event_venue = match event {
            ports::MarketEvent::Snapshot(snapshot) => snapshot.source_venue,
            ports::MarketEvent::Update(update) => update.source_venue,
            ports::MarketEvent::Trade(trade) => trade.source_venue,
            ports::MarketEvent::Bar(bar) => bar.source_venue,
            ports::MarketEvent::Arbitrage(_) => None, // 跨場事件，無單一來源
            ports::MarketEvent::Disconnect { .. } => None,
        };
        
        // 如果事件沒有 source_venue 信息，允許通過（向後兼容）
        if event_venue.is_none() {
            debug!("事件無 source_venue 信息，允許所有單場策略處理: {:?}", 
                   match event {
                       ports::MarketEvent::Bar(bar) => format!("Bar({})", bar.symbol.0),
                       ports::MarketEvent::Trade(trade) => format!("Trade({})", trade.symbol.0),
                       ports::MarketEvent::Snapshot(snap) => format!("Snapshot({})", snap.symbol.0),
                       _ => "Other".to_string(),
                   });
            return true;
        }
        
        // 使用策略到場所的映射進行精確過濾
        if let Some(&expected_venue) = strategy_venue_mapping.get(strategy_name) {
            if let Some(event_venue) = event_venue {
                let matches = expected_venue == event_venue;
                if !matches {
                    debug!("策略 {} 跳過事件：期望場所 {} 但事件來自 {}", 
                           strategy_name, expected_venue, event_venue);
                }
                return matches;
            }
        }
        
        // 如果策略映射中找不到該策略，或事件沒有 venue 信息，使用保守策略：允許通過
        true
    }

    /// 運行策略決策 (同步版本，用於 tick())
    fn run_strategies_sync(&mut self, result: &mut EngineTickResult) -> Result<(), HftError> {
        let account_view = self.get_account_view();
        
        // 處理所有待處理的市場事件（包括真實的 Bar 事件）
        let events_to_process = std::mem::take(&mut self.pending_market_events);
        
        for event in &events_to_process {
            // 創建延遲追蹤器用於此市場事件的處理
            let event_timestamp = self.extract_event_timestamp(event);
            let mut event_tracker = if let Some(ts) = event_timestamp {
                self.recent_market_event_timestamp = Some(ts);
                LatencyTracker::from_time(ts)
            } else {
                LatencyTracker::new()
            };
            
            // 日誌降噪：逐事件改為 debug 級別
            debug!("處理市場事件給策略: {:?}", 
                   match event {
                       ports::MarketEvent::Bar(bar) => format!("Bar({})", bar.symbol.0),
                       ports::MarketEvent::Trade(trade) => format!("Trade({})", trade.symbol.0),
                       ports::MarketEvent::Snapshot(snap) => format!("Snapshot({})", snap.symbol.0),
                       _ => "Other".to_string(),
                   });
            
            let mut all_intents = Vec::new();
            
            // 收集所有策略的意圖，並自動填充 strategy_id
            let strategy_start = now_micros();
            
            // 使用策略實例 ID（而不是類型名稱）進行事件過濾
            // strategy_instance_ids 與 strategies Vec 順序對應
            
            for (strategy_idx, strategy) in self.strategies.iter_mut().enumerate() {
                // 使用策略實例 ID（而非類型名稱）進行事件過濾
                let unknown_id = "unknown".to_string();
                let strategy_instance_id = self.strategy_instance_ids.get(strategy_idx)
                    .unwrap_or(&unknown_id);
                
                // 事件範疇過濾：檢查策略是否應該處理此事件
                if !Self::should_strategy_process_event_static(strategy_instance_id, event, &self.strategy_venue_mapping) {
                    debug!("策略 {} 跳過事件（venue 過濾）: {:?}", 
                           strategy_instance_id,
                           match event {
                               ports::MarketEvent::Bar(bar) => format!("Bar({}) from {:?}", bar.symbol.0, bar.source_venue),
                               ports::MarketEvent::Trade(trade) => format!("Trade({}) from {:?}", trade.symbol.0, trade.source_venue),
                               ports::MarketEvent::Snapshot(snap) => format!("Snapshot({}) from {:?}", snap.symbol.0, snap.source_venue),
                               _ => "Other".to_string(),
                           });
                    continue;
                }
                
                let mut intents = strategy.on_market_event(event, &account_view);
                
                // 自動填充 strategy_id（如果是空字串）
                for intent in &mut intents {
                    if intent.strategy_id.is_empty() {
                        intent.strategy_id = format!("strategy_{}", strategy_idx);
                    }
                }
                
                all_intents.extend(intents);
            }
            
            // 記錄策略階段完成和延遲
            event_tracker.record_stage(LatencyStage::Strategy);
            let strategy_latency = now_micros().saturating_sub(strategy_start);
            self.latency_monitor.record_latency(LatencyStage::Strategy, strategy_latency);
            
            // 記錄策略延遲到 Prometheus
            infra_metrics::MetricsRegistry::global().record_strategy_latency(strategy_latency as f64);
            
            // 通過風控審核（如果有配置風控管理器）
            let risk_start = now_micros();
            let approved_intents = if let Some(risk_manager) = &mut self.risk_manager {
                // 使用新的 venue-specific 風控方法
                risk_manager.review_with_venue_specs(all_intents.clone(), &account_view, &self.venue_specs)
            } else {
                all_intents.clone() // 沒有風控則全部通過
            };
            
            // 記錄風控階段完成
            let risk_latency = now_micros().saturating_sub(risk_start) as f64;
            infra_metrics::MetricsRegistry::global().record_risk_latency(risk_latency);
            
            let orders_count = approved_intents.len() as u32;
            result.orders_generated += orders_count;
            
            // 日誌降噪：只有在有訂單時才記錄
            if orders_count > 0 {
                debug!("策略生成 {} 個訂單意圖，風控通過 {} 個", 
                       all_intents.len(), approved_intents.len());
            }
            
            // 在發送到执行队列前：為 Market 單補全價格（使用當前頂檔）
            let mut intents_to_send = approved_intents;
            if !intents_to_send.is_empty() {
                let market_view = self.get_market_view();
                for intent in &mut intents_to_send {
                    if matches!(intent.order_type, OrderType::Market) && intent.price.is_none() {
                        let best = match intent.side {
                            Side::Buy => market_view.get_best_ask(&intent.symbol).map(|(p, _)| p),
                            Side::Sell => market_view.get_best_bid(&intent.symbol).map(|(p, _)| p),
                        };
                        if let Some(px) = best.or_else(|| market_view.get_mid_price(&intent.symbol)) {
                            intent.price = Some(px);
                        }
                    }
                }
            }

            // 發送通過風控的意圖到执行队列
            let execution_start = now_micros();
            if let Some(queues) = &mut self.execution_queues {
                let failed_intents = queues.send_intents_batch(intents_to_send);
                if !failed_intents.is_empty() {
                    warn!("{}个意图因队列满载被丢弃", failed_intents.len());
                }
            }
            
            // 記錄執行階段延遲到統一監控器
            let execution_latency = now_micros().saturating_sub(execution_start);
            self.latency_monitor.record_latency(LatencyStage::Execution, execution_latency);
            
            // 如果有起始時間戳，計算端到端延遲
            if let Some(origin_ts) = event_timestamp {
                let end_to_end_latency = now_micros().saturating_sub(origin_ts);
                self.latency_monitor.record_latency(LatencyStage::EndToEnd, end_to_end_latency);
            }
            
            // 記錄執行延遲到 Prometheus
            infra_metrics::MetricsRegistry::global().record_execution_latency(execution_latency as f64);
        }
        
        Ok(())
    }
    
    /// 主循環
    pub async fn run(&mut self) -> Result<(), HftError> {
        info!("引擎開始運行");
        self.is_running = true;
        
        while self.is_running {
            match self.tick() {
                Ok(tick_result) => {
                    if tick_result.events_total > 0 {
                        debug!("Tick #{}: {} 事件, 快照: {}", 
                               self.cycle_count, 
                               tick_result.events_total,
                               tick_result.snapshot_published);
                    }
                    
                    // 檢查是否需要休眠 (如果沒有事件處理)
                    if tick_result.events_total == 0 {
                        tokio::time::sleep(std::time::Duration::from_micros(100)).await;
                    }
                }
                Err(e) => {
                    error!("引擎循環錯誤: {}", e);
                    // 可以選擇繼續或停止
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            }
        }
        
        info!("引擎停止運行");
        Ok(())
    }
    
    /// 停止引擎
    pub fn stop(&mut self) {
        info!("正在停止引擎");
        self.is_running = false;
    }
    
    /// 從市場事件中提取時間戳
    fn extract_event_timestamp(&self, event: &ports::MarketEvent) -> Option<u64> {
        match event {
            ports::MarketEvent::Bar(bar) => Some(bar.close_time),
            ports::MarketEvent::Trade(trade) => Some(trade.timestamp),
            ports::MarketEvent::Snapshot(snapshot) => Some(snapshot.timestamp),
            ports::MarketEvent::Update(update) => Some(update.timestamp),
            _ => None,
        }
    }
    
    /// 獲取引擎統計
    pub fn get_statistics(&self) -> EngineStatistics {
        EngineStatistics {
            cycle_count: self.cycle_count,
            consumers_count: self.event_consumers.len(),
            strategies_count: self.strategies.len(),
            execution_clients_count: self.execution_clients.len(),
            execution_events_processed: self.execution_events_processed,
            is_running: self.is_running,
            orders_submitted: self.orders_submitted,
            orders_ack: self.orders_ack,
            orders_filled: self.orders_filled,
            orders_rejected: self.orders_rejected,
            orders_canceled: self.orders_canceled,
        }
    }

    /// 獲取系統背壓狀態（用於運行時監控）
    pub fn get_backpressure_status(&self) -> Vec<BackpressureStatus> {
        // 注意：由於當前架構限制，我們只能報告基本狀態
        // 在生產環境中，需要從 EventConsumer 或相關組件獲取實際狀態
        vec![BackpressureStatus {
            utilization: 0.0, // 實際實施中需要從消費者獲取
            is_under_pressure: false,
            events_dropped_total: 0,
            recommended_action: "Backpressure monitoring requires EventConsumer integration".to_string(),
            queue_capacity: self.config.ingestion.queue_capacity,
        }]
    }
}

/// 單次 tick 的結果
#[derive(Debug, Default)]
pub struct EngineTickResult {
    pub events_processed: u32,
    pub events_total: u32,
    pub execution_events_processed: u32,
    pub snapshot_published: bool,
    pub snapshot_sequence: u64,
    pub orders_generated: u32,
}

/// 引擎統計信息
#[derive(Debug)]
pub struct EngineStatistics {
    pub cycle_count: u64,
    pub consumers_count: usize,
    pub strategies_count: usize,
    pub execution_clients_count: usize,
    pub execution_events_processed: u64,
    pub is_running: bool,
    pub orders_submitted: u64,
    pub orders_ack: u64,
    pub orders_filled: u64,
    pub orders_rejected: u64,
    pub orders_canceled: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::{MarketEvent, AggregatedBar, MarketSnapshot, Trade, BookLevel};
    use hft_core::{Price, Quantity, Side, VenueId, Symbol};
    
    #[test]
    fn test_engine_creation() {
        let config = EngineConfig::default();
        let engine = Engine::new(config);
        
        assert_eq!(engine.strategies.len(), 0);
        assert_eq!(engine.event_consumers.len(), 0);
    }

    #[test]
    fn test_strategy_event_filtering_static() {
        // 測試單場策略過濾
        
        // 創建測試事件 - Bar 事件帶 Binance source_venue
        let bar_event = MarketEvent::Bar(AggregatedBar {
            symbol: Symbol("BTCUSDT".to_string()),
            interval_ms: 60000,
            open_time: 1000000,
            close_time: 1060000,
            open: Price::from_f64(50000.0).unwrap(),
            high: Price::from_f64(51000.0).unwrap(),
            low: Price::from_f64(49000.0).unwrap(),
            close: Price::from_f64(50500.0).unwrap(),
            volume: Quantity::from_f64(10.0).unwrap(),
            trade_count: 100,
            source_venue: Some(VenueId::BINANCE),
        });

        // 創建測試事件 - Snapshot 事件帶 Bitget source_venue
        let snapshot_event = MarketEvent::Snapshot(MarketSnapshot {
            symbol: Symbol("ETHUSDT".to_string()),
            timestamp: 1000000,
            bids: vec![BookLevel::new_unchecked(3000.0, 1.0)],
            asks: vec![BookLevel::new_unchecked(3100.0, 1.0)],
            sequence: 1,
            source_venue: Some(VenueId::BITGET),
        });

        // 創建測試事件 - Trade 事件無 source_venue
        let trade_event = MarketEvent::Trade(Trade {
            symbol: Symbol("ADAUSDT".to_string()),
            timestamp: 1000000,
            price: Price::from_f64(0.5).unwrap(),
            quantity: Quantity::from_f64(100.0).unwrap(),
            side: Side::Buy,
            trade_id: "12345".to_string(),
            source_venue: None,
        });

        // 創建測試用的策略場所映射
        let mut strategy_mapping = HashMap::new();
        strategy_mapping.insert("TrendStrategy".to_string(), VenueId::BINANCE);
        strategy_mapping.insert("ImbalanceStrategy".to_string(), VenueId::BITGET);
        
        // 測試 TrendStrategy (單場策略) - 應該只處理 BINANCE 事件
        assert!(Engine::should_strategy_process_event_static("TrendStrategy", &bar_event, &strategy_mapping));
        assert!(Engine::should_strategy_process_event_static("TrendStrategy", &snapshot_event, &strategy_mapping));
        assert!(Engine::should_strategy_process_event_static("TrendStrategy", &trade_event, &strategy_mapping)); // 向後兼容

        // 測試 ArbitrageStrategy (跨場策略) - 應該處理所有事件
        assert!(Engine::should_strategy_process_event_static("ArbitrageStrategy", &bar_event, &strategy_mapping));
        assert!(Engine::should_strategy_process_event_static("ArbitrageStrategy", &snapshot_event, &strategy_mapping));
        assert!(Engine::should_strategy_process_event_static("ArbitrageStrategy", &trade_event, &strategy_mapping));

        // 測試 ImbalanceStrategy (單場策略) - 在映射中但事件來自 BINANCE，應該通過（因為事件沒有 venue 信息）
        assert!(Engine::should_strategy_process_event_static("ImbalanceStrategy", &bar_event, &strategy_mapping));
        
        // 測試未知策略（默認為單場策略） - 不在映射中，應該通過
        assert!(Engine::should_strategy_process_event_static("CustomStrategy", &bar_event, &strategy_mapping));
        
        // 測試名稱包含 arbitrage 的未知策略（推斷為跨場策略）
        assert!(Engine::should_strategy_process_event_static("custom_arbitrage_strategy", &bar_event, &strategy_mapping));
        assert!(Engine::should_strategy_process_event_static("CrossExchangeStrategy", &snapshot_event, &strategy_mapping));
        assert!(Engine::should_strategy_process_event_static("ArbStrategy", &trade_event, &strategy_mapping));
    }
}
